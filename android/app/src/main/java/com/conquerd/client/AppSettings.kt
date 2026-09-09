package com.conquerd.client

import android.content.Context

/**
 * Preferences that live on this device only.
 *
 * The desktop persists about sixty settings through its Qt settings model,
 * which is behind the `qt-ui` feature and unavailable here. Rather than port
 * that, this holds the handful that mean something on a phone — the rest of
 * the desktop's list is device pickers, window geometry and tray behaviour
 * that a phone either decides for itself or does not have.
 *
 * The display name is deliberately *not* here: peers keep their own copy of
 * it, so it belongs on the identity's own peer record where every outbound
 * message already reads it from, not in a local preferences file.
 */
class AppSettings(context: Context) {

    private val prefs =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** Which camera a video call opens with. */
    var frontCamera: Boolean
        get() = prefs.getBoolean(KEY_FRONT_CAMERA, true)
        set(value) = prefs.edit().putBoolean(KEY_FRONT_CAMERA, value).apply()

    /**
     * Whether the mic gates on speech rather than staying open.
     *
     * On by default, matching the core: an always-open mic on a phone in a
     * pocket is both a battery cost and a privacy one.
     */
    var voiceActivation: Boolean
        get() = prefs.getBoolean(KEY_VOICE_ACTIVATION, true)
        set(value) = prefs.edit().putBoolean(KEY_VOICE_ACTIVATION, value).apply()

    /** Microphone gain, 0-200 where 100 is unity. */
    var inputGain: Int
        get() = prefs.getInt(KEY_INPUT_GAIN, 100)
        set(value) = prefs.edit().putInt(KEY_INPUT_GAIN, value.coerceIn(0, 200)).apply()

    /** Speaker gain, 0-200 where 100 is unity. */
    var outputGain: Int
        get() = prefs.getInt(KEY_OUTPUT_GAIN, 100)
        set(value) = prefs.edit().putInt(KEY_OUTPUT_GAIN, value.coerceIn(0, 200)).apply()

    /** Noise gate: 0 off, 1 mild, 2 moderate, 3 aggressive, 4 max. */
    var noiseStrength: Int
        get() = prefs.getInt(KEY_NOISE_STRENGTH, 2)
        set(value) = prefs.edit().putInt(KEY_NOISE_STRENGTH, value.coerceIn(0, 4)).apply()

    /**
     * Outgoing Opus bitrate ceiling in bits per second.
     *
     * A ceiling, not a target: the core lowers the live rate under packet
     * loss regardless of what is set here.
     */
    var voiceBitrate: Int
        get() = prefs.getInt(KEY_VOICE_BITRATE, 32_000)
        set(value) = prefs.edit().putInt(KEY_VOICE_BITRATE, value.coerceIn(8_000, 128_000)).apply()

    /** "system", "light" or "dark". */
    var theme: String
        get() = prefs.getString(KEY_THEME, THEME_SYSTEM) ?: THEME_SYSTEM
        set(value) = prefs.edit().putString(KEY_THEME, value).apply()

    /** The camera id the core expects for the current preference. */
    val cameraDeviceId: String
        get() = if (frontCamera) "android:front" else "android:back"

    companion object {
        const val THEME_SYSTEM = "system"
        const val THEME_LIGHT = "light"
        const val THEME_DARK = "dark"

        private const val PREFS_NAME = "app_settings"
        private const val KEY_FRONT_CAMERA = "front_camera"
        private const val KEY_VOICE_ACTIVATION = "voice_activation"
        private const val KEY_THEME = "theme"
        private const val KEY_INPUT_GAIN = "input_gain"
        private const val KEY_OUTPUT_GAIN = "output_gain"
        private const val KEY_NOISE_STRENGTH = "noise_strength"
        private const val KEY_VOICE_BITRATE = "voice_bitrate"
    }
}
