package com.conquerd.client

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

private const val TAG = "NetworkMonitor"

/**
 * Tells the core when the device moves between networks.
 *
 * A phone that walks off Wi-Fi onto cellular leaves every open socket bound to
 * a local address that no longer exists, and TCP does not report that: the
 * connection neither errors nor delivers. The core has a liveness deadline for
 * exactly this case, but it can only be generous — a supernode is allowed to be
 * quiet — so left to itself the client is offline for a minute and a half after
 * every network change. Android knows the moment it happens, so it says so and
 * the core re-dials immediately.
 *
 * Only a change of the *default* network counts. Capability and link-property
 * callbacks fire constantly — signal strength, metered flags, DNS updates — and
 * none of them invalidate a socket.
 */
class NetworkMonitor(context: Context, private val core: ConquerdCore) {

    private val appContext = context.applicationContext
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** Guards against a double `start()` leaking a second registration. */
    private var registered = false

    /**
     * The default network the core's sockets are on.
     *
     * Seeded from `activeNetwork` before registering, because Android replays
     * `onAvailable` for the current default the moment a callback is
     * registered. Without the seed that replay looks like a move and would tear
     * down the connection the app just finished opening.
     */
    private var current: Network? = null

    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            val previous = current
            current = network
            if (previous == network) return
            Log.i(TAG, "default network moved ($previous -> $network) — asking the core to re-dial")
            signal()
        }

        override fun onLost(network: Network) {
            // Clearing it means the next network to arrive counts as a move
            // even if it is the same one coming back, which is what we want:
            // the sockets did not survive the gap either way.
            if (current == network) {
                current = null
                Log.i(TAG, "default network lost")
            }
        }
    }

    fun start() {
        if (registered) return
        val cm = appContext.getSystemService(ConnectivityManager::class.java)
        if (cm == null) {
            Log.w(TAG, "no ConnectivityManager — network changes will not be reported")
            return
        }
        current = runCatching { cm.activeNetwork }.getOrNull()
        // Registration throws if the process has somehow exhausted its callback
        // quota. Losing the fast path is a degradation, not a reason to take
        // the service down with it — the core's liveness deadline still
        // recovers, just slowly.
        runCatching { cm.registerDefaultNetworkCallback(callback) }
            .onSuccess {
                registered = true
                Log.i(TAG, "watching the default network (currently $current)")
            }
            .onFailure { Log.w(TAG, "could not watch the default network", it) }
    }

    fun stop() {
        if (registered) {
            val cm = appContext.getSystemService(ConnectivityManager::class.java)
            runCatching { cm?.unregisterNetworkCallback(callback) }
            registered = false
        }
        scope.cancel()
    }

    /**
     * Fire-and-forget: the core replies, but there is nothing useful to do with
     * the answer and the callback thread must not block on it.
     */
    private fun signal() {
        scope.launch {
            val reply = core.command("net.changed")
            if (!reply.ok) {
                // Expected while the core is stopped or still starting up.
                Log.d(TAG, "net.changed not accepted: ${reply.errorText}")
            }
        }
    }
}
