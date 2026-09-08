package com.conquerd.client

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.content.ContextCompat

/**
 * Keeps the client core alive while the app is backgrounded.
 *
 * Without this the process is killable the moment the last activity stops, and
 * a peer-to-peer client that dies when the screen turns off cannot hold a
 * session, receive a message, or take a call. A foreground service with a
 * visible notification is the only way Android grants that, and the
 * notification doubles as the honest disclosure that the app is connected.
 */
class CoreService : Service() {

    /** Whether a call currently needs the mic and/or the camera. */
    private var microphoneActive = false
    private var cameraActive = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        createChannel()

        if (intent?.action == ACTION_SET_MEDIA) {
            microphoneActive = intent.getBooleanExtra(EXTRA_MICROPHONE, false)
            cameraActive = intent.getBooleanExtra(EXTRA_CAMERA, false)
        }

        startInForeground()

        // START_STICKY: if the system reclaims the process under memory
        // pressure, bring the service back. The core re-reads its stores on
        // start, so a restart resumes rather than losing state.
        return START_STICKY
    }

    override fun onDestroy() {
        super.onDestroy()
        ConquerdCore.get(this).stop()
    }

    private fun startInForeground() {
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )

        val inCall = microphoneActive || cameraActive
        val notification: Notification = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle(
                getString(if (inCall) R.string.service_title_call else R.string.service_title),
            )
            .setContentText(
                getString(if (inCall) R.string.service_text_call else R.string.service_text),
            )
            .setSmallIcon(R.drawable.ic_notification)
            .setContentIntent(open)
            .setOngoing(true)
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            // From API 34 the type must be declared at start time, must match
            // the manifest, and — for microphone and camera — the matching
            // runtime permission must already be granted, or the system throws
            // instead of starting. So the type set is computed from what we
            // actually hold, not from what we would like.
            startForeground(NOTIFICATION_ID, notification, activeServiceTypes())
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    /**
     * The foreground service types this process may legitimately claim now.
     *
     * Always includes `dataSync` (holding the transport open is the baseline
     * reason this service exists); adds microphone and camera only while a
     * call needs them *and* their permission is held.
     */
    private fun activeServiceTypes(): Int {
        var types = ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
        if (microphoneActive && hasPermission(Manifest.permission.RECORD_AUDIO)) {
            types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        }
        if (cameraActive && hasPermission(Manifest.permission.CAMERA)) {
            types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
        }
        return types
    }

    private fun hasPermission(permission: String): Boolean =
        ContextCompat.checkSelfPermission(this, permission) == PackageManager.PERMISSION_GRANTED

    private fun createChannel() {
        val manager = getSystemService(NotificationManager::class.java) ?: return
        if (manager.getNotificationChannel(CHANNEL_ID) != null) return

        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                getString(R.string.channel_name),
                // Low: the connection notice is a persistent status line, not
                // something worth a sound or a heads-up every launch.
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = getString(R.string.channel_description) },
        )
    }

    companion object {
        private const val CHANNEL_ID = "conquerd_core"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_SET_MEDIA = "com.conquerd.client.SET_MEDIA"
        private const val EXTRA_MICROPHONE = "microphone"
        private const val EXTRA_CAMERA = "camera"

        fun start(context: Context) {
            context.startForegroundService(Intent(context, CoreService::class.java))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, CoreService::class.java))
        }

        /**
         * Tell the service a call started or ended, so it can claim (or drop)
         * the microphone and camera foreground types.
         *
         * Must be called *before* capture begins: claiming the type after the
         * fact does not retroactively make background capture legal, and
         * Android silently feeds a muted stream instead.
         */
        fun setMediaActive(context: Context, microphone: Boolean, camera: Boolean) {
            val intent = Intent(context, CoreService::class.java).apply {
                action = ACTION_SET_MEDIA
                putExtra(EXTRA_MICROPHONE, microphone)
                putExtra(EXTRA_CAMERA, camera)
            }
            context.startForegroundService(intent)
        }
    }
}
