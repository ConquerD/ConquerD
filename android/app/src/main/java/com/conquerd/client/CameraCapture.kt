package com.conquerd.client

import android.content.Context
import android.util.Log
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import android.util.Size
import java.util.concurrent.Executors

private const val TAG = "ConquerdCamera"

/**
 * Feeds the native encoder from CameraX.
 *
 * The core's other capture backends own their device and pull frames; Android
 * cannot, because the camera lives behind CameraX and delivers buffers on its
 * own thread. So this pushes: every analyzed frame goes straight to
 * [NativeCore.nativeSubmitCameraFrame], which drops it if video is off.
 */
object CameraCapture {

    /**
     * Frames are handed to native code on this single thread.
     *
     * A dedicated executor rather than the main thread: packing and rotating
     * an I420 frame is real work, and doing it on the main thread would show
     * up directly as UI jank during a call.
     */
    private val executor = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "conquerd-camera").apply { priority = Thread.NORM_PRIORITY + 1 }
    }

    private var provider: ProcessCameraProvider? = null

    @Volatile
    var isBound: Boolean = false
        private set

    /**
     * Bind the camera and start delivering frames.
     *
     * Must be called before `video.start`: the native side waits for a first
     * frame to learn the capture dimensions and gives up after a few seconds.
     *
     * @param front which camera to bind; the front one for a call by default.
     */
    fun start(context: Context, front: Boolean = true) {
        // Bound to the *process* lifecycle, not the activity's. CameraX unbinds
        // when its owner stops, so binding to the activity meant the camera
        // died the moment the app was backgrounded or the screen turned off -
        // mid-call - and the capture thread reported "camera stopped
        // delivering frames". A call is expected to keep running there, which
        // is what the foreground service and its notification exist for.
        val lifecycleOwner: LifecycleOwner = ProcessLifecycleOwner.get()
        val future = ProcessCameraProvider.getInstance(context)
        future.addListener({
            val cameraProvider = runCatching { future.get() }.getOrElse { e ->
                Log.e(TAG, "camera provider unavailable", e)
                return@addListener
            }

            val analysis = ImageAnalysis.Builder()
                // The core wants I420; this is the only format CameraX
                // guarantees across devices, and it is what the packing code
                // in camera.rs is written against.
                .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
                // Drop rather than queue. A backed-up analyzer adds latency to
                // a live call, and the encoder only ever wants the newest
                // frame anyway.
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .setResolutionSelector(
                    ResolutionSelector.Builder()
                        .setResolutionStrategy(
                            // Near the core's "balanced" preset. CameraX picks
                            // the closest mode the sensor actually supports.
                            ResolutionStrategy(
                                Size(640, 480),
                                ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER,
                            ),
                        )
                        .build(),
                )
                .build()

            analysis.setAnalyzer(executor, ::submit)

            val selector = if (front) {
                CameraSelector.DEFAULT_FRONT_CAMERA
            } else {
                CameraSelector.DEFAULT_BACK_CAMERA
            }

            runCatching {
                // Unbind first: re-binding without it throws once the previous
                // use case is still attached, e.g. when switching cameras.
                cameraProvider.unbindAll()
                cameraProvider.bindToLifecycle(lifecycleOwner, selector, analysis)
                provider = cameraProvider
                isBound = true
                Log.i(TAG, "camera bound (${if (front) "front" else "back"})")
            }.onFailure { e ->
                isBound = false
                Log.e(TAG, "could not bind camera", e)
            }
        }, ContextCompat.getMainExecutor(context))
    }

    /** Release the camera. Safe to call when nothing is bound. */
    fun stop() {
        provider?.let { runCatching { it.unbindAll() } }
        provider = null
        isBound = false
        Log.i(TAG, "camera released")
    }

    /**
     * Hand one frame across to the encoder.
     *
     * The `ImageProxy` must stay open until the native call returns — it owns
     * the direct buffers being read — hence `use`, which closes it afterwards
     * and lets CameraX recycle the buffer.
     */
    private fun submit(image: ImageProxy) {
        image.use { frame ->
            if (frame.planes.size < 3) return@use

            val y = frame.planes[0]
            val u = frame.planes[1]
            val v = frame.planes[2]

            runCatching {
                NativeCore.nativeSubmitCameraFrame(
                    y.buffer,
                    y.rowStride,
                    u.buffer,
                    u.rowStride,
                    u.pixelStride,
                    v.buffer,
                    v.rowStride,
                    v.pixelStride,
                    frame.width,
                    frame.height,
                    // Sensors are mounted landscape, so this is almost always
                    // 90 in portrait. Rotation is applied natively.
                    frame.imageInfo.rotationDegrees,
                )
            }.onFailure { e ->
                // One log, not one per frame: at 30fps a persistent failure
                // would bury everything else in logcat.
                Log.e(TAG, "frame submission failed; stopping capture", e)
                stop()
            }
        }
    }
}
