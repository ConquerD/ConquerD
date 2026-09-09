package com.conquerd.client

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.ViewModelProvider
import com.conquerd.client.ui.AppRoot
import com.conquerd.client.ui.ConquerdTheme

class MainActivity : ComponentActivity() {

    private val viewModel: AppViewModel by lazy {
        ViewModelProvider(this)[AppViewModel::class.java]
    }

    /**
     * The foreground service notification is how the user knows the client is
     * connected, so ask for permission on first launch. A refusal is not
     * fatal — the service still runs, the notice is just silent.
     */
    private val requestNotifications =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        }

        setContent {
            // Collected here rather than inside the theme so a change repaints
            // the whole tree, including the system bars.
            val state by viewModel.state.collectAsState()
            ConquerdTheme(
                darkTheme = when (state.prefs.theme) {
                    AppSettings.THEME_DARK -> true
                    AppSettings.THEME_LIGHT -> false
                    else -> isSystemInDarkTheme()
                },
            ) {
                AppRoot(viewModel = viewModel)
            }
        }

        handleInviteIntent(intent)
    }

    /**
     * The activity is `singleTask`, so a second `conquerd://` link while the app
     * is already open arrives here instead of through `onCreate`.
     */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // Without this the activity keeps returning the intent it launched
        // with, so anything reading `getIntent()` later sees a stale link.
        setIntent(intent)
        handleInviteIntent(intent)
    }

    /**
     * Hand a `conquerd://` invite to the core.
     *
     * The link usually arrives before the identity is unlocked — tapping an
     * invite is a common way to open the app for the first time — so the view
     * model holds it until there is a core to give it to.
     */
    private fun handleInviteIntent(intent: Intent?) {
        if (intent?.action != Intent.ACTION_VIEW) return
        val uri = intent.data ?: return
        if (!uri.scheme.equals("conquerd", ignoreCase = true)) return

        viewModel.acceptInvite(uri.toString())
    }
}
