package com.exergame.camera

import android.Manifest
import android.app.Activity
import android.graphics.Bitmap
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

private const val PERMISSION_ALIAS_CAMERA = "camera"
private const val PERMISSION_NAME = Manifest.permission.CAMERA

private const val JPEG_QUALITY = 80

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

@TauriPlugin(
    permissions = [
        Permission(strings = [PERMISSION_NAME], alias = PERMISSION_ALIAS_CAMERA)
    ]
)
class ExergameCameraPlugin(private val activity: Activity) : Plugin(activity) {
    companion object {
        init {
            // Load ONNX Runtime once, before the Rust side dlopens it by name.
            System.loadLibrary("onnxruntime")
        }
    }

    private var cameraProviderFuture: ListenableFuture<ProcessCameraProvider>? = null
    private var cameraProvider: ProcessCameraProvider? = null
    private var analysisUseCase: ImageAnalysis? = null
    private val analysisExecutor: ExecutorService = Executors.newSingleThreadExecutor()

    // The latest frame, continuously overwritten by the analyzer: the Rust
    // side polls it at its own pace and always gets the freshest picture.
    private var latestJpeg: ByteArray? = null
    private var latestWidth = 0
    private var latestHeight = 0

    @Command
    fun listCameras(invoke: Invoke) {
        try {
            val manager =
                activity.getSystemService(Activity.CAMERA_SERVICE) as android.hardware.camera2.CameraManager

            val cameras = JSArray()
            data class Entry(val id: String, val facing: Int, val base: String)

            val entries = mutableListOf<Entry>()
            for (id in manager.cameraIdList) {
                val characteristics = manager.getCameraCharacteristics(id)
                val facing =
                    characteristics.get(android.hardware.camera2.CameraCharacteristics.LENS_FACING)
                        ?: android.hardware.camera2.CameraMetadata.LENS_FACING_UNKNOWN

                val base = when (facing) {
                    android.hardware.camera2.CameraMetadata.LENS_FACING_BACK -> "Back camera"
                    android.hardware.camera2.CameraMetadata.LENS_FACING_FRONT -> "Front camera"
                    android.hardware.camera2.CameraMetadata.LENS_FACING_EXTERNAL -> "External camera"
                    else -> "Camera"
                }

                entries.add(Entry(id, facing, base))
            }

            // Front cameras first (selfie view), then back, then external and
            // unknown lenses.
            val priority = mapOf(
                android.hardware.camera2.CameraMetadata.LENS_FACING_FRONT to 0,
                android.hardware.camera2.CameraMetadata.LENS_FACING_BACK to 1,
                android.hardware.camera2.CameraMetadata.LENS_FACING_EXTERNAL to 2,
                android.hardware.camera2.CameraMetadata.LENS_FACING_UNKNOWN to 3
            )
            val sorted = entries.sortedBy { priority[it.facing] ?: 2 }

            // Number cameras that share the same facing ("Back camera 2").
            val totals = sorted.groupingBy { it.facing }.eachCount()
            val counters = HashMap<Int, Int>()

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
                val bitmap: Bitmap = image.toBitmap()
                val stream = ByteArrayOutputStream()
                bitmap.compress(Bitmap.CompressFormat.JPEG, JPEG_QUALITY, stream)

                latestWidth = bitmap.width
                latestHeight = bitmap.height
                latestJpeg = stream.toByteArray()
            } catch (e: Exception) {
                Logger.error("failed to process frame: ${e.message}")
            }
        }
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
        invoke.resolve(response)
    }

    @Command
    fun stopCapture(invoke: Invoke) {
        try {
            analysisUseCase?.let { analysis -> cameraProvider?.unbind(analysis) }
            cameraProvider?.unbindAll()
            analysisUseCase = null
            latestJpeg = null
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: e.toString())
        }
    }

    private var savedInvoke: Invoke? = null
}
