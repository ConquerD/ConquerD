package com.conquerd.client

import android.content.Context

/**
 * The JNI surface of the Rust client core.
 *
 * Four methods rather than one per feature: everything the app does travels
 * over the JSON command/event channel that [nativeCommand] and [EventSink]
 * carry. The desktop client exposes close to a hundred bridge invokables, and
 * mirroring each as its own `external fun` would mean changing declarations on
 * both sides of the boundary every time one of them moved.
 */
object NativeCore {

    /** Receives core events as JSON. Called from a Rust-owned thread. */
    fun interface EventSink {
        fun onEvent(json: String)
    }

    init {
        System.loadLibrary("conquerd_android")
    }

    /** Core version string. Also the cheapest check that the library loaded. */
    external fun nativeVersion(): String

    /**
     * Unlock (or create) the identity in [homeDir], open the stores, and start
     * the core.
     *
     * @param homeDir app-private directory holding identity, peers, chat and rooms.
     * @param passphrase empty for an unencrypted identity.
     * @param storedKey base64url identity file key from [IdentityVault], or null
     *   to unlock with [passphrase]. When set the passphrase is not consulted,
     *   and a key that no longer opens the file fails the start so the caller
     *   can prompt instead.
     * @param context the application context. Handed to `ndk-context` so cpal's
     *   Oboe backend can open an audio device — without it the first attempt to
     *   start audio panics inside the native library.
     * @return an opaque handle, or 0. Throws [RuntimeException] on failure.
     */
    external fun nativeStart(
        homeDir: String,
        passphrase: String,
        storedKey: String?,
        context: Context,
        sink: EventSink,
    ): Long

    /**
     * Hand one captured frame to the encoder.
     *
     * Called on CameraX's analyzer thread. Cheap and non-blocking when video is
     * off, so the analyzer can stay bound across start/stop without
     * coordinating with the core. Planes are the `ImageProxy`'s direct buffers
     * and are only read for the duration of the call.
     */
    external fun nativeSubmitCameraFrame(
        y: java.nio.ByteBuffer,
        yRowStride: Int,
        u: java.nio.ByteBuffer,
        uRowStride: Int,
        uPixelStride: Int,
        v: java.nio.ByteBuffer,
        vRowStride: Int,
        vPixelStride: Int,
        width: Int,
        height: Int,
        rotationDegrees: Int,
    )

    /** Run one command, returning one JSON reply. Never throws. */
    external fun nativeCommand(handle: Long, json: String): String

    /** Shut the core down. Safe to call with 0. */
    external fun nativeStop(handle: Long)
}
