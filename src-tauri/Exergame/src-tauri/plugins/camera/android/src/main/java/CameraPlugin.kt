package com.exergame.camera

import android.Manifest
import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Matrix
import android.os.Build
import android.util.Base64
import android.util.Size
import androidx.camera.camera2.interop.Camera2CameraInfo
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import app.tauri.Logger
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.google.common.util.concurrent.ListenableFuture
import java.io.ByteArrayOutputStream
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import kotlin.math.roundToInt

private const val PERMISSION_ALIAS_CAMERA = "camera"
private const val PERMISSION_NAME = Manifest.permission.CAMERA

private const val JPEG_QUALITY = 80

// MoveNet needs only a 192×192 input: cap the analysis bitmap so the
// detection preprocessing stays cheap at any camera resolution.
private const val ANALYSIS_MAX_WIDTH = 640

@InvokeArg
class StartCaptureArgs {
    var cameraId: String? = null
    var width: Int = 0
    var height: Int = 0
}

@InvokeArg
class ListResolutionsArgs {
    var cameraId: String? = null
}

private data class CameraEntry(val id: String, val facing: Int, val base: String)

@TauriPlugin(
    permissions = [
        Permission(strings = [PERMISSION_NAME], alias = PERMISSION_ALIAS_CAMERA)
    ]
)
class ExergameCameraPlugin(private val activity: Activity) : Plugin(activity) {
    private var cameraProviderFuture: ListenableFuture<ProcessCameraProvider>? = null
    private var cameraProvider: ProcessCameraProvider? = null
    private var analysisUseCase: ImageAnalysis? = null
    private val analysisExecutor: ExecutorService = Executors.newSingleThreadExecutor()

    // Body-pose detection (MoveNet via LiteRT, GPU delegate when available).
    private val detector = MovenetDetector(activity)

    // The latest frame and pose, continuously overwritten by the analyzer: the
    // Rust side polls at its own pace and always gets the freshest data.
    private var latestJpeg: ByteArray? = null
    private var latestWidth = 0
    private var latestHeight = 0
    private var latestKeypoints: JSArray? = null
    private var latestInferenceMs = 0.0
    private var latestPreprocessMs = 0.0
    private var providerKind = "CPU"

    @Command
    fun listCameras(invoke: Invoke) {
        try {
            val manager =
                activity.getSystemService(Activity.CAMERA_SERVICE) as android.hardware.camera2.CameraManager

            val entries = mutableListOf<CameraEntry>()
            for (id in manager.cameraIdList) {
                val characteristics = manager.getCameraCharacteristics(id)
                val facing =
                    characteristics.get(android.hardware.camera2.CameraCharacteristics.LENS_FACING)
                        ?: -1

                val base = when (facing) {
                    android.hardware.camera2.CameraMetadata.LENS_FACING_FRONT -> "Front camera"
                    android.hardware.camera2.CameraMetadata.LENS_FACING_BACK -> "Back camera"
                    android.hardware.camera2.CameraMetadata.LENS_FACING_EXTERNAL -> "External camera"
                    else -> "Camera"
                }

                entries.add(CameraEntry(id, facing, base))
            }

            // Front cameras first (selfie view), then back, then external and
            // unknown lenses.
            val priority = mapOf(
                android.hardware.camera2.CameraMetadata.LENS_FACING_FRONT to 0,
                android.hardware.camera2.CameraMetadata.LENS_FACING_BACK to 1,
                android.hardware.camera2.CameraMetadata.LENS_FACING_EXTERNAL to 2
            )
            val sorted = entries.sortedBy { priority[it.facing] ?: 3 }

            // Number cameras that share the same facing ("Back camera 2").
            val totals = sorted.groupingBy { it.facing }.eachCount()
            val counters = HashMap<Int, Int>()

            val cameras = JSArray()
            for (entry in sorted) {
                val seen = (counters[entry.facing] ?: 0) + 1
                counters[entry.facing] = seen
                val name = if ((totals[entry.facing] ?: 0) > 1) "${entry.base} $seen" else entry.base

                val camera = JSObject()
                camera.put("id", entry.id)
                camera.put("name", name)
                cameras.put(camera)
            }

            val response = JSObject()
            response.put("cameras", cameras)
            invoke.resolve(response)
        } catch (e: Exception) {
            invoke.reject(e.message ?: e.toString())
        }
    }

    @Command
    fun listResolutions(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(ListResolutionsArgs::class.java)
            val cameraId = args.cameraId
                ?: throw Exception("cameraId is required")

            val manager =
                activity.getSystemService(Activity.CAMERA_SERVICE) as android.hardware.camera2.CameraManager
            val characteristics = manager.getCameraCharacteristics(cameraId)
            val map = characteristics.get(android.hardware.camera2.CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)

            val resolutions = JSArray()
            if (map != null) {
                val seen = HashSet<String>()
                for (size in map.getOutputSizes(android.graphics.ImageFormat.YUV_420_888)) {
                    val label = "${size.width}x${size.height}"
                    if (seen.add(label)) {
                        resolutions.put(label)
                    }
                }
            }

            val response = JSObject()
            response.put("resolutions", resolutions)
            invoke.resolve(response)
        } catch (e: Exception) {
            invoke.reject(e.message ?: e.toString())
        }
    }

    @Command
    fun startCapture(invoke: Invoke) {
        val args = invoke.parseArgs(StartCaptureArgs::class.java)

        if (getPermissionState(PERMISSION_ALIAS_CAMERA) != PermissionState.GRANTED) {
            // Ask the user for the camera permission and retry afterwards.
            savedInvoke = invoke
            requestPermissionForAlias(PERMISSION_ALIAS_CAMERA, invoke, "cameraPermissionCallback")
            return
        }

        startWithPermission(args, invoke)
    }

    @PermissionCallback
    fun cameraPermissionCallback(invoke: Invoke) {
        val granted = getPermissionState(PERMISSION_ALIAS_CAMERA) === PermissionState.GRANTED
        val args = invoke.parseArgs(StartCaptureArgs::class.java)

        if (granted) {
            startWithPermission(args, invoke)
        } else {
            invoke.reject("Camera permission was denied")
        }
    }

    private fun startWithPermission(args: StartCaptureArgs, invoke: Invoke) {
        try {
            // Prepare the LiteRT interpreter before the camera starts, so the
            // first analyzed frame already has keypoints.
            detector.ensureLoaded()

            val future = ProcessCameraProvider.getInstance(activity)
            cameraProviderFuture = future

            future.addListener({
                try {
                    val provider = future.get()
                    cameraProvider = provider

                    val selector = if (args.cameraId != null) {
                        CameraSelector.Builder()
                            .addCameraFilter { infos ->
                                infos.filter { info ->
                                    Camera2CameraInfo.from(info).getCameraId() == args.cameraId
                                }
                            }
                            .build()
                    } else {
                        CameraSelector.DEFAULT_BACK_CAMERA
                    }

                    val builder = ImageAnalysis.Builder()
                    if (args.width > 0 && args.height > 0) {
                        builder.setResolutionSelector(
                            ResolutionSelector.Builder()
                                .setResolutionStrategy(
                                    ResolutionStrategy(
                                        Size(args.width, args.height),
                                        ResolutionStrategy.FALLBACK_RULE_CLOSEST_LOWER_THEN_HIGHER
                                    )
                                )
                                .build()
                        )
                    }

                    val analysis = builder
                        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                        .build()
                    analysis.setAnalyzer(analysisExecutor, ::analyze)
                    analysisUseCase = analysis

                    val owner = activity as LifecycleOwner
                    provider.unbindAll()
                    provider.bindToLifecycle(owner, selector, analysis)

                    invoke.resolve()
                } catch (e: Exception) {
                    invoke.reject(e.message ?: e.toString())
                }
            }, ContextCompat.getMainExecutor(activity))
        } catch (e: Exception) {
            invoke.reject(e.message ?: e.toString())
        }
    }

    private fun analyze(image: ImageProxy) {
        image.use {
            try {
                val bitmap: Bitmap = rotateUpright(image.toBitmap(), image.imageInfo.rotationDegrees)

                // JPEG for the video preview (full resolution).
                val stream = ByteArrayOutputStream()
                bitmap.compress(Bitmap.CompressFormat.JPEG, JPEG_QUALITY, stream)
                latestWidth = bitmap.width
                latestHeight = bitmap.height
                latestJpeg = stream.toByteArray()

                // Body-pose detection (MoveNet via LiteRT, GPU delegate when
                // available) on a capped copy: the detection preprocessing
                // stays cheap at high camera resolutions. The keypoints come
                // back normalized to the frame (0..1).
                val analysisBitmap = downscaleForDetection(bitmap)
                val result = detector.detect(analysisBitmap)

                latestKeypoints = keypointsToJsArray(result.keypoints)
                latestInferenceMs = result.inferenceMs
                latestPreprocessMs = result.preprocessMs
                providerKind = result.provider
            } catch (e: Exception) {
                Logger.error("frame processing failed: ${e.message}")
            }
        }
    }

    // The analyzer receives frames in sensor orientation (landscape buffers
    // even in a portrait app) and `toBitmap()` does not apply
    // `imageInfo.rotationDegrees`, so without this step the preview arrives
    // rotated by 90°. Rotate the bitmap upright; the JPEG, the reported
    // dimensions and the keypoint coordinates all follow the rotated bitmap,
    // keeping them consistent with each other.
    private fun rotateUpright(bitmap: Bitmap, rotationDegrees: Int): Bitmap {
        if (rotationDegrees == 0) {
            return bitmap
        }

        val matrix = Matrix()
        matrix.postRotate(rotationDegrees.toFloat())
        val rotated = Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
        if (rotated != bitmap) {
            bitmap.recycle()
        }
        return rotated
    }

    // Downscale the frame for detection (MoveNet needs only 192×192): the
    // keypoints are mapped back to the full frame afterwards.
    private fun downscaleForDetection(bitmap: Bitmap): Bitmap {
        if (bitmap.width <= ANALYSIS_MAX_WIDTH) {
            return bitmap
        }

        val height = (bitmap.height.toDouble() * ANALYSIS_MAX_WIDTH / bitmap.width)
            .roundToInt()
            .coerceAtLeast(1)
        return Bitmap.createScaledBitmap(bitmap, ANALYSIS_MAX_WIDTH, height, true)
    }

    private fun keypointsToJsArray(keypoints: List<NamedKeypoint>): JSArray {
        val arr = JSArray()
        for (keypoint in keypoints) {
            val obj = JSObject()
            obj.put("name", keypoint.name)
            obj.put("x", keypoint.x)
            obj.put("y", keypoint.y)
            obj.put("score", keypoint.score)
            arr.put(obj)
        }
        return arr
    }

    private fun keypointsToJsArray(keypoints: List<NamedKeypoint>): JSArray {
        val arr = JSArray()
        for (keypoint in keypoints) {
            val obj = JSObject()
            obj.put("name", keypoint.name)
            obj.put("x", keypoint.x)
            obj.put("y", keypoint.y)
            obj.put("score", keypoint.score)
            arr.put(obj)
        }
        return arr
    }

    @Command
    fun readFrame(invoke: Invoke) {
        val jpeg = latestJpeg
        if (jpeg == null || latestWidth == 0 || latestHeight == 0) {
            invoke.resolve()
            return
        }

        val encoded = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Base64.encodeToString(jpeg, Base64.NO_WRAP)
        } else {
            @Suppress("DEPRECATION")
            Base64.encodeToString(jpeg, Base64.DEFAULT)
        }

        val response = JSObject()
        response.put("width", latestWidth)
        response.put("height", latestHeight)
        response.put("jpeg", encoded)
        response.put("keypoints", latestKeypoints ?: JSArray())
        response.put("inferenceMs", latestInferenceMs)
        response.put("preprocessMs", latestPreprocessMs)
        response.put("provider", providerKind)
        invoke.resolve(response)
    }

    @Command
    fun stopCapture(invoke: Invoke) {
        try {
            analysisUseCase?.let { analysis -> cameraProvider?.unbind(analysis) }
            cameraProvider?.unbindAll()
            analysisUseCase = null
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: e.toString())
        }
    }

    private var savedInvoke: Invoke? = null
}
