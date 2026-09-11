package com.conquerd.client

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Person
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.os.Build
import android.provider.Settings
import androidx.core.app.NotificationCompat
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow

/**
 * The incoming-call notice shown when the activity is not in front.
 *
 * The in-app dialog only exists while [MainActivity] is visible. A peer-to-peer
 * client that is holding a session in [CoreService] must still be able to
 * ring: a `CallStyle` notification (or a high-importance fallback) plus a
 * full-screen intent on the lock screen. Answer opens the activity; decline
 * is handled by the service so the UI does not have to come up.
 */
object IncomingCallNotifier : DefaultLifecycleObserver {

    const val ACTION_SHOW = "com.conquerd.client.INCOMING_SHOW"
    const val ACTION_ANSWER = "com.conquerd.client.INCOMING_ANSWER"
    const val ACTION_DECLINE = "com.conquerd.client.INCOMING_DECLINE"
    const val EXTRA_PEER_ID = "peer_id"
    const val EXTRA_PEER_LABEL = "peer_label"

    private const val CHANNEL_ID = "conquerd_incoming_calls"
    private const val NOTIFICATION_ID = 2

    data class Ringing(val peerId: String, val peerLabel: String)

    @Volatile
    var ringing: Ringing? = null
        private set

    private val _cleared = MutableSharedFlow<String>(extraBufferCapacity = 4)
    val cleared: SharedFlow<String> = _cleared.asSharedFlow()

    @Volatile
    private var observingLifecycle = false

    fun ensureLifecycleObserver() {
        if (observingLifecycle) return
        observingLifecycle = true
        ProcessLifecycleOwner.get().lifecycle.addObserver(this)
    }

    override fun onStop(owner: LifecycleOwner) {
        val current = ringing ?: return
        // The in-app dialog went away with the activity. Re-post so the
        // ring is still reachable from the shade / lock screen.
        lastContext?.let { show(it, current.peerId, current.peerLabel, force = true) }
    }

    @Volatile
    private var lastContext: Context? = null

    fun show(context: Context, peerId: String, peerLabel: String, force: Boolean = false) {
        lastContext = context.applicationContext
        ensureLifecycleObserver()
        val previous = ringing
        val label = if (
            previous?.peerId == peerId &&
            previous.peerLabel != peerId.take(12) &&
            peerLabel == peerId.take(12)
        ) {
            previous.peerLabel
        } else {
            peerLabel
        }
        ringing = Ringing(peerId, label)
        if (!force && appIsResumed()) return

        val app = context.applicationContext
        val manager = app.getSystemService(NotificationManager::class.java) ?: return
        ensureChannel(app, manager)

        val show = activityIntent(app, ACTION_SHOW, peerId, label)
        val answer = activityIntent(app, ACTION_ANSWER, peerId, label)
        val decline = PendingIntent.getService(
            app,
            12,
            Intent(app, CoreService::class.java).apply {
                action = ACTION_DECLINE
                putExtra(EXTRA_PEER_ID, peerId)
                putExtra(EXTRA_PEER_LABEL, label)
            },
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )

        val notification = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val caller = Person.Builder()
                .setName(label)
                .setImportant(true)
                .build()
            Notification.Builder(app, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_notification)
                .setCategory(Notification.CATEGORY_CALL)
                .setOngoing(true)
                .setContentIntent(show)
                .setFullScreenIntent(show, true)
                .setStyle(
                    Notification.CallStyle.forIncomingCall(caller, decline, answer),
                )
                .build()
        } else {
            NotificationCompat.Builder(app, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(app.getString(R.string.incoming_call_title))
                .setContentText(label)
                .setCategory(NotificationCompat.CATEGORY_CALL)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .setOngoing(true)
                .setContentIntent(show)
                .setFullScreenIntent(show, true)
                .addAction(0, app.getString(R.string.incoming_call_decline), decline)
                .addAction(0, app.getString(R.string.incoming_call_answer), answer)
                .build()
        }

        manager.notify(NOTIFICATION_ID, notification)
    }

    fun cancel(context: Context) {
        context.applicationContext
            .getSystemService(NotificationManager::class.java)
            ?.cancel(NOTIFICATION_ID)
        ringing = null
    }

    /** Decline from the notification: drop the shade notice and tell the UI. */
    fun declined(context: Context, peerId: String) {
        cancel(context)
        _cleared.tryEmit(peerId)
    }

    private fun appIsResumed(): Boolean =
        ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)

    private fun activityIntent(
        context: Context,
        action: String,
        peerId: String,
        peerLabel: String,
    ): PendingIntent {
        val intent = Intent(context, MainActivity::class.java).apply {
            this.action = action
            putExtra(EXTRA_PEER_ID, peerId)
            putExtra(EXTRA_PEER_LABEL, peerLabel)
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or
                Intent.FLAG_ACTIVITY_CLEAR_TOP or
                Intent.FLAG_ACTIVITY_NEW_TASK
        }
        val request = if (action == ACTION_ANSWER) 11 else 10
        return PendingIntent.getActivity(
            context,
            request,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }

    private fun ensureChannel(context: Context, manager: NotificationManager) {
        if (manager.getNotificationChannel(CHANNEL_ID) != null) return
        val channel = NotificationChannel(
            CHANNEL_ID,
            context.getString(R.string.incoming_call_channel),
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = context.getString(R.string.incoming_call_channel_description)
            setSound(
                Settings.System.DEFAULT_RINGTONE_URI,
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build(),
            )
        }
        manager.createNotificationChannel(channel)
    }
}
