package com.exergame.camera

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.SystemClock
import android.util.Log
import org.tensorflow.lite.Interpreter
import org.tensorflow.lite.gpu.GpuDelegate
import java.nio.channels.FileChannel
import kotlin.math.roundToInt

// MoveNet SinglePose Lightning: 192×192 input, 17 keypoints (COCO-17 order).
private const val MODEL_ASSET = "movenet-lightning.tflite"
private const val INPUT_SIZE = 192
private const val LOG_TAG = "ExergameCamera"

private val KEYPOINT_NAMES = arrayOf(
    "nose", "leftEye", "rightEye", "leftEar", "rightEar",
    "leftShoulder", "rightShoulder", "leftElbow", "rightElbow",
    "leftWrist", "rightWrist", "leftHip", "rightHip",
    "leftKnee", "rightKnee", "leftAnkle", "rightAnkle"
)

class NamedKeypoint(val name: String, val x: Double, val y: Double, val score: Double)

class MovenetResult(
    val keypoints: List<NamedKeypoint>,
    val inferenceMs: Double,
    val preprocessMs: Double,
    val provider: String
)

/**
 * Body-pose detection with MoveNet running through LiteRT.
 *
 * The GPU delegate is tried first (OpenCL works on Mali, Adreno and
 * Immortalis GPUs), with an automatic CPU fallback when its kernels cannot
 * be compiled on the device. The interpreter is created once and reused;
 * `detect()` is meant to be called from a single thread.
 */
class MovenetDetector(context: Context) {
    private val appContext = context.applicationContext
    private var interpreter: Interpreter? = null
    private var gpuDelegate: GpuDelegate? = null
    private var providerKind = "CPU"

    val provider: String
        get() = providerKind

    // Loads the model and creates the interpreter (idempotent).
    fun ensureLoaded() {
        if (interpreter != null) {
            return
        }

        val afd = appContext.assets.openFd(MODEL_ASSET)
        val channel = java.io.FileInputStream(afd.fileDescriptor).channel
        val model = channel.map(FileChannel.MapMode.READ_ONLY, afd.startOffset, afd.declaredLength)

        try {
            val delegate = GpuDelegate()
            val options = Interpreter.Options().addDelegate(delegate)
            val itp = Interpreter(model, options)
            runWarmup(itp)

            interpreter = itp
            gpuDelegate = delegate
            providerKind = "GPU"
        } catch (e: Throwable) {
            // Throwable: a missing delegate class is an Error, not an
            // Exception — it must not crash the app, only trigger the CPU
            // fallback.
            Log.e(LOG_TAG, "GPU delegate failed, falling back to CPU: ${e.message}")
            val itp = Interpreter(model, Interpreter.Options())
            runWarmup(itp)

            interpreter = itp
            providerKind = "CPU"
        }
    }

    // Recognizes the pose on the given bitmap and measures the timings.
    fun detect(bitmap: Bitmap): MovenetResult {
        ensureLoaded()
        val itp = interpreter
            ?: throw IllegalStateException("MoveNet interpreter is not initialized")

        val preprocessStart = SystemClock.uptimeMillis()
        val (input, padX, padY) = preprocess(itp, bitmap)
        val preprocessMs = SystemClock.uptimeMillis() - preprocessStart

        val inferenceStart = SystemClock.uptimeMillis()
        val output = Array(1) { Array(1) { Array(17) { FloatArray(3) } } }
        itp.run(input, output)
        val inferenceMs = SystemClock.uptimeMillis() - inferenceStart

        val keypoints = decodeKeypoints(
            output[0][0],
            padX,
            padY,
            bitmap.width,
            bitmap.height
        )

        return MovenetResult(
            keypoints,
            (inferenceMs + preprocessMs).toDouble(),
            preprocessMs.toDouble(),
            providerKind
        )
    }

    // A throwaway inference: compiles the GPU kernels (or surfaces a CPU
    // fallback signal) before real frames arrive.
    private fun runWarmup(itp: Interpreter) {
        val input = buildInput(itp, IntArray(INPUT_SIZE * INPUT_SIZE))
        val output = Array(1) { Array(1) { Array(17) { FloatArray(3) } } }
        itp.run(input, output)
    }

    // Fits the frame into the model input (192×192) with letterboxing and
    // builds the input tensor for the model's input dtype.
    private fun preprocess(itp: Interpreter, bitmap: Bitmap): Triple<Any, Int, Int> {
        val scale = minOf(
            INPUT_SIZE.toFloat() / bitmap.width,
            INPUT_SIZE.toFloat() / bitmap.height
        )
        val scaledW = (bitmap.width * scale).roundToInt().coerceIn(1, INPUT_SIZE)
        val scaledH = (bitmap.height * scale).roundToInt().coerceIn(1, INPUT_SIZE)
        val padX = (INPUT_SIZE - scaledW) / 2
        val padY = (INPUT_SIZE - scaledH) / 2

        val scaled = Bitmap.createScaledBitmap(bitmap, scaledW, scaledH, true)
        val canvasBitmap = Bitmap.createBitmap(INPUT_SIZE, INPUT_SIZE, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(canvasBitmap)
        canvas.drawColor(android.graphics.Color.BLACK)
        canvas.drawBitmap(scaled, padX.toFloat(), padY.toFloat(), null)

        val pixels = IntArray(INPUT_SIZE * INPUT_SIZE)
        canvasBitmap.getPixels(pixels, 0, INPUT_SIZE, 0, 0, INPUT_SIZE, INPUT_SIZE)

        val input = buildInput(itp, pixels)
        return Triple(input, padX, padY)
    }

    // Builds the input tensor for the model's input dtype (MoveNet expects
    // int32 pixel values 0..255; handled generically for uint8/float32 too).
    private fun buildInput(itp: Interpreter, pixels: IntArray): Any {
        val plane = Array(INPUT_SIZE) { Array(INPUT_SIZE) { IntArray(3) } }
        for (y in 0 until INPUT_SIZE) {
            for (x in 0 until INPUT_SIZE) {
                val p = pixels[y * INPUT_SIZE + x]
                plane[y][x][0] = (p shr 16) and 0xFF
                plane[y][x][1] = (p shr 8) and 0xFF
                plane[y][x][2] = p and 0xFF
            }
        }

        val dtype = itp.getInputTensor(0).dataType()
        return when (dtype) {
            org.tensorflow.lite.DataType.UINT8 -> toUint8Tensor(plane)
            org.tensorflow.lite.DataType.FLOAT32 -> toFloatTensor(plane)
            else -> Array(1) { plane }
        }
    }

    private fun toInt32Tensor(plane: Array<Array<IntArray>>): Any = Array(1) { plane }

    private fun toUint8Tensor(plane: Array<Array<IntArray>>): Any =
        Array(1) { _ ->
            Array(INPUT_SIZE) { y ->
                Array(INPUT_SIZE) { x ->
                    ByteArray(3).also {
                        it[0] = plane[y][x][0].toByte()
                        it[1] = plane[y][x][1].toByte()
                        it[2] = plane[y][x][2].toByte()
                    }
                }
            }
        }

    private fun toFloatTensor(plane: Array<Array<IntArray>>): Any =
        Array(1) { _ ->
            Array(INPUT_SIZE) { y ->
                Array(INPUT_SIZE) { x ->
                    FloatArray(3).also {
                        it[0] = plane[y][x][0].toFloat()
                        it[1] = plane[y][x][1].toFloat()
                        it[2] = plane[y][x][2].toFloat()
                    }
                }
            }
        }

    private fun decodeKeypoints(
        points: Array<FloatArray>,
        padX: Int,
        padY: Int,
        srcWidth: Int,
        srcHeight: Int
    ): List<NamedKeypoint> {
        val scaleX = srcWidth.toFloat() / INPUT_SIZE
        val scaleY = srcHeight.toFloat() / INPUT_SIZE

        val keypoints = mutableListOf<NamedKeypoint>()
        for (i in KEYPOINT_NAMES.indices) {
            val yNorm = points[i][0]
            val xNorm = points[i][1]
            val score = points[i][2]

            // Map the model coordinates back to the source frame.
            val x = ((xNorm * INPUT_SIZE - padX) / scaleX).toDouble()
            val y = ((yNorm * INPUT_SIZE - padY) / scaleY).toDouble()

            keypoints.add(NamedKeypoint(KEYPOINT_NAMES[i], x, y, score.toDouble()))
        }

        return keypoints
    }
}
