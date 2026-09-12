package com.conquerd.client

import android.content.Context
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Build
import android.util.Log

/**
 * Output routing for voice sessions (calls and room voice).
 *
 * Android decides where a stream plays from its `AudioAttributes.usage`, not
 * from anything the app asks for afterwards. The native core opens its Oboe
 * streams with `Usage::VoiceCommunication` (see the vendored cpal fork in
 * `rust/patches/cpal`); this class owns the other half — putting the device in
 * `MODE_IN_COMMUNICATION` and selecting which endpoint that call route lands
 * on. Without *both*, a stream stays on the media route: always the
 * loudspeaker, never the earpiece, and with no platform echo canceller
 * attached to capture.
 *
 * Routing is only meaningful while a session is live, so [activate] and
 * [release] bracket it and the mode is restored on the way out — leaving the
 * device in `MODE_IN_COMMUNICATION` would duck every other app's audio.
 */
class AudioRouter(context: Context) {

    private val audio =
        context.applicationContext.getSystemService(Context.AUDIO_SERVICE) as AudioManager

    /** Mode to restore in [release]; non-null only while a session is active. */
    private var previousMode: Int? = null

    /** Last requested speakerphone preference, re-applied when routes change. */
    private var speakerphone: Boolean = false

    /**
     * Enter communication mode and apply [speakerphone].
     *
     * Idempotent: calling it twice keeps the first saved mode, so a room join
     * arriving on top of a call does not save `MODE_IN_COMMUNICATION` as the
     * thing to restore later.
     */
    fun activate(speakerphone: Boolean) {
        this.speakerphone = speakerphone
        if (previousMode == null) {
            previousMode = audio.mode
            audio.mode = AudioManager.MODE_IN_COMMUNICATION
        }
        apply()
    }

    /** Switch between earpiece and loudspeaker mid-session. */
    fun setSpeakerphone(on: Boolean) {
        speakerphone = on
        if (previousMode != null) apply()
    }

    /**
     * True when a wired or Bluetooth headset is attached.
     *
     * The UI uses this to explain why the toggle is not in charge: a headset
     * outranks the preference in [apply], because someone who plugged one in
     * did not mean "play this out loud".
     */
    fun headsetAttached(): Boolean = externalHeadset() != null

    /** Leave communication mode and hand routing back to the system. */
    fun release() {
        val mode = previousMode ?: return
        previousMode = null
        runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                audio.clearCommunicationDevice()
            } else {
                @Suppress("DEPRECATION")
                audio.isSpeakerphoneOn = false
            }
            audio.mode = mode
        }.onFailure { Log.w(TAG, "releasing audio route failed", it) }
    }

    /**
     * Point the call route at the endpoint the user should hear.
     *
     * A headset, when present, always wins; otherwise the preference chooses
     * between loudspeaker and earpiece. On a tablet with no earpiece the
     * lookup finds nothing and the system default stands rather than the call
     * going silent.
     */
    private fun apply() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
            // Pre-31 has no device selection, only the speakerphone flag; the
            // platform already prefers a headset over the earpiece for it.
            @Suppress("DEPRECATION")
            runCatching { audio.isSpeakerphoneOn = speakerphone }
                .onFailure { Log.w(TAG, "setting speakerphone failed", it) }
            return
        }

        val headset = externalHeadset()
        val wanted = when {
            headset != null -> headset
            speakerphone -> deviceOfType(AudioDeviceInfo.TYPE_BUILTIN_SPEAKER)
            else -> deviceOfType(AudioDeviceInfo.TYPE_BUILTIN_EARPIECE)
                ?: deviceOfType(AudioDeviceInfo.TYPE_BUILTIN_SPEAKER)
        }
        if (wanted == null) {
            Log.w(TAG, "no communication device available; leaving system default")
            return
        }
        runCatching { audio.setCommunicationDevice(wanted) }
            .onSuccess { Log.i(TAG, "route -> ${wanted.type} (speakerphone=$speakerphone)") }
            .onFailure { Log.w(TAG, "setCommunicationDevice failed", it) }
    }

    /** The attached headset, if any, preferring wired over Bluetooth. */
    private fun externalHeadset(): AudioDeviceInfo? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return null
        val devices = audio.availableCommunicationDevices
        // Wired first: if both are connected, the one physically plugged in is
        // the more deliberate choice.
        for (type in WIRED_TYPES) {
            devices.firstOrNull { it.type == type }?.let { return it }
        }
        return devices.firstOrNull { it.type == AudioDeviceInfo.TYPE_BLUETOOTH_SCO }
    }

    private fun deviceOfType(type: Int): AudioDeviceInfo? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return null
        return audio.availableCommunicationDevices.firstOrNull { it.type == type }
    }

    private companion object {
        const val TAG = "ConquerdAudioRoute"

        /**
         * Wired output types. USB headsets report as either the headset or the
         * generic device type depending on how the descriptor is written, so
         * both are treated as wired.
         */
        val WIRED_TYPES = intArrayOf(
            AudioDeviceInfo.TYPE_WIRED_HEADSET,
            AudioDeviceInfo.TYPE_USB_HEADSET,
            AudioDeviceInfo.TYPE_USB_DEVICE,
        )
    }
}
