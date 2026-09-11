package com.conquerd.client

import android.Manifest
import android.app.Notification
import android.app.Notification.Action
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.annotation.RequiresApi
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.serialization.json.put

private const val TAG = "CoreService"

/**
 * The foreground types this process may claim right now.
 *
 * Always includes `specialUse` (holding the transport open is why the service
 * exists). Adds microphone and camera only while a call needs them *and* their
 * permission is held — Android 14 throws if those types are claimed without
 * the matching runtime grant.
 *
 * `dataSync` is deliberately absent: Android 15 caps it at six hours and Play
 * only accepts it for short user-initiated transfers, not a standing session.
 */
internal fun coreForegroundTypes(
    microphoneActive: Boolean,
    cameraActive: Boolean,
    hasMicrophonePermission: Boolean,
    hasCameraPermission: Boolean,
): Int {
    var types = ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
    if (microphoneActive && hasMicrophonePermission) {
        types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
    }
    if (cameraActive && hasCameraPermission) {
        types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
    }
    return types
}

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

    /**
     * Watches for the device changing network.
     *
     * Owned by the service rather than the activity for the same reason the
     * core is: the phone is most likely to hop between Wi-Fi and cellular while
     * the app is in someone's pocket, which is exactly when no activity exists
     * to notice.
     */
    private val networkMonitor by lazy { NetworkMonitor(this, ConquerdCore.get(this)) }

    /**
     * Event pump for incoming calls. The ViewModel is gone when the activity
     * is; this scope lives with the session so a ring can still be posted.
     */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        networkMonitor.start()
        IncomingCallNotifier.ensureLifecycleObserver()
        scope.launch {
            ConquerdCore.get(this@CoreService).events.collect { event ->
                when (event.eventName()) {
                    "call_request" -> {
                        val peerId = event.stringOrEmpty("peer_id")
                        if (peerId.isEmpty()) return@collect
                        IncomingCallNotifier.show(
                            this@CoreService,
                            peerId,
                            peerId.take(12),
                        )
                    }
                    "call_accepted", "call_ended" ->
                        IncomingCallNotifier.cancel(this@CoreService)
                }
            }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_DISCONNECT) {
            Log.i(TAG, "disconnect requested from the notification")
            IncomingCallNotifier.cancel(this)
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }

        if (intent?.action == IncomingCallNotifier.ACTION_DECLINE) {
            val peerId = intent.getStringExtra(IncomingCallNotifier.EXTRA_PEER_ID)
            if (!peerId.isNullOrEmpty()) {
                IncomingCallNotifier.declined(this, peerId)
                scope.launch {
                    ConquerdCore.get(this@CoreService).command("call.reject") {
                        put("peer_id", peerId)
                    }
                }
            }
            return START_STICKY
        }

        createChannel()

        if (intent?.action == ACTION_SET_MEDIA) {
            microphoneActive = intent.getBooleanExtra(EXTRA_MICROPHONE, false)
            cameraActive = intent.getBooleanExtra(EXTRA_CAMERA, false)
        }

        if (!ConquerdCore.get(this).isRunning) {
            // Sticky restart after the process died, or a start with no
            // session. A notification that says we are connected when the
            // core is gone is a lie.
            stopSelf()
            return START_NOT_STICKY
        }

        startInForeground()

        // START_STICKY: if the system reclaims the *service* under memory
        // pressure while the process (and therefore the core) is still
        // alive, bring the notification back. Process death is the other
        // case and is rejected above because there is then no core to keep.
        return START_STICKY
    }

    @RequiresApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
    override fun onTimeout(startId: Int) {
        // API 34 short-service path. specialUse is not supposed to hit this,
        // but failing to stop here is a crash rather than a logged timeout.
        handleTimeout("unspecified")
    }

    @RequiresApi(Build.VERSION_CODES.VANILLA_ICE_CREAM)
    override fun onTimeout(startId: Int, fgsType: Int) {
        handleTimeout(fgsType.toString())
    }

    private fun handleTimeout(type: String) {
        Log.w(TAG, "foreground service timed out (type=$type); stopping")
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        super.onDestroy()
        scope.cancel()
        IncomingCallNotifier.cancel(this)
        networkMonitor.stop()
        ConquerdCore.get(this).stop()
    }

    private fun startInForeground() {
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val disconnect = PendingIntent.getService(
            this,
            1,
            Intent(this, CoreService::class.java).setAction(ACTION_DISCONNECT),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
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
            .addAction(
                Action.Builder(
                    Icon.createWithResource(this, R.drawable.ic_notification),
                    getString(R.string.service_disconnect),
                    disconnect,
                ).build(),
            )
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

    private fun activeServiceTypes(): Int = coreForegroundTypes(
        microphoneActive = microphoneActive,
        cameraActive = cameraActive,
        hasMicrophonePermission = hasPermission(Manifest.permission.RECORD_AUDIO),
        hasCameraPermission = hasPermission(Manifest.permission.CAMERA),
    )

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
        private const val ACTION_DISCONNECT = "com.conquerd.client.DISCONNECT"
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
