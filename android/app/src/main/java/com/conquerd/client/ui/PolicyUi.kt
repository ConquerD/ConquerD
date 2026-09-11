package com.conquerd.client.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import com.conquerd.client.AppSettings
import com.conquerd.client.Legal

/**
 * Ask for a dangerous permission only after an in-app explanation.
 *
 * Play treats mic/camera used while the screen is off as background capture,
 * which needs a disclosure *before* the system dialog. If the grant is
 * already held the explanation is skipped.
 */
@Composable
fun rememberExplainedPermission(
    permission: String,
    title: String,
    body: String,
    onGranted: () -> Unit,
): () -> Unit {
    val context = LocalContext.current
    var show by remember { mutableStateOf(false) }
    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted -> if (granted) onGranted() }

    if (show) {
        AlertDialog(
            onDismissRequest = { show = false },
            title = { Text(title) },
            text = { Text(body) },
            confirmButton = {
                TextButton(onClick = {
                    show = false
                    launcher.launch(permission)
                }) { Text("Continue") }
            },
            dismissButton = {
                TextButton(onClick = { show = false }) { Text("Not now") }
            },
        )
    }

    return {
        val held = ContextCompat.checkSelfPermission(context, permission) ==
            PackageManager.PERMISSION_GRANTED
        if (held) onGranted() else show = true
    }
}

val MicRationaleTitle = "Microphone for calls"
val MicRationaleBody =
    "DoubleSlash uses the microphone for voice calls and room voice. If you " +
        "leave the app or turn the screen off during a call, the microphone " +
        "stays on until you hang up. A notification is shown the whole time."

val CameraRationaleTitle = "Camera for video"
val CameraRationaleBody =
    "DoubleSlash uses the camera only when you turn video on. If you leave " +
        "the app or turn the screen off during that call, the camera stays on " +
        "until you stop video or hang up. A notification is shown the whole time."

/**
 * One-shot explanation for the persistent connection notification.
 *
 * Asked on the first Home visit after terms, not in Activity.onCreate: a
 * permission with no feature in front of it is what Play objects to.
 */
@Composable
fun NotificationPermissionPrompt(settings: AppSettings) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
    if (settings.notificationRationaleShown) return

    val context = LocalContext.current
    var show by remember { mutableStateOf(false) }
    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { settings.notificationRationaleShown = true }

    LaunchedEffect(Unit) {
        val held = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        if (held) {
            settings.notificationRationaleShown = true
        } else {
            show = true
        }
    }

    if (!show) return

    AlertDialog(
        onDismissRequest = {
            show = false
            settings.notificationRationaleShown = true
        },
        title = { Text("Stay connected in the background") },
        text = {
            Text(
                "DoubleSlash shows a persistent notification while you are " +
                    "signed in so peer sessions stay open when the app is in " +
                    "the background. You can Disconnect from that notification " +
                    "at any time. Without it the notice is silent, and the " +
                    "session may still run.",
            )
        },
        confirmButton = {
            TextButton(onClick = {
                show = false
                launcher.launch(Manifest.permission.POST_NOTIFICATIONS)
            }) { Text("Continue") }
        },
        dismissButton = {
            TextButton(onClick = {
                show = false
                settings.notificationRationaleShown = true
            }) { Text("Not now") }
        },
    )
}

@Composable
fun TermsScreen(
    onAccept: () -> Unit,
    onDecline: () -> Unit,
) {
    val context = LocalContext.current
    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
    ) {
        Text("Before you send anything", style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(12.dp))
        Text(
            "DoubleSlash is invite-only. There is no central moderator. You " +
                "are responsible for who you invite and what you send.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(16.dp))
        Text(
            "You may not use DoubleSlash for child sexual abuse material, " +
                "exploitation of minors, illegal content, malware, or " +
                "non-consensual intimate imagery.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(16.dp))
        Text(
            "Conversations are end-to-end sealed, so the project cannot read " +
                "or take down a message on someone else's device. Block a peer " +
                "from their long-press menu. Report from that same menu — it " +
                "sends identifiers, not message bodies.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(16.dp))
        Text(
            "DoubleSlash is not directed at children under 13.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(24.dp))
        TextButton(onClick = { Legal.openUrl(context, Legal.TERMS_URL) }) {
            Text("Read the full terms")
        }
        TextButton(onClick = { Legal.openUrl(context, Legal.PRIVACY_URL) }) {
            Text("Privacy policy")
        }
        Spacer(Modifier.height(16.dp))
        Button(onClick = onAccept, modifier = Modifier.fillMaxWidth()) {
            Text("I agree")
        }
        TextButton(onClick = onDecline, modifier = Modifier.fillMaxWidth()) {
            Text("Decline and lock")
        }
    }
}

@Composable
fun ReportDialog(
    kind: String,
    targetId: String,
    targetLabel: String,
    onBlock: (() -> Unit)?,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    var note by remember { mutableStateOf("") }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Report $kind") },
        text = {
            Column {
                Text(
                    "DoubleSlash cannot read sealed messages. This sends the " +
                        "id and your note through an app you choose (mail, " +
                        "GitHub, …). For child sexual abuse material, also " +
                        "report to the authorities in your country.",
                    style = MaterialTheme.typography.bodySmall,
                )
                Spacer(Modifier.height(12.dp))
                Text(
                    targetLabel,
                    style = MaterialTheme.typography.labelMedium,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = note,
                    onValueChange = { note = it },
                    label = { Text("What happened") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 3,
                )
            }
        },
        confirmButton = {
            TextButton(onClick = {
                Legal.shareReport(
                    context,
                    Legal.reportSubject(targetLabel),
                    Legal.reportBody(kind, targetId, targetLabel, note),
                )
                onDismiss()
            }) { Text("Send report") }
        },
        dismissButton = {
            Column {
                if (onBlock != null) {
                    TextButton(onClick = {
                        onBlock()
                        onDismiss()
                    }) { Text("Block") }
                }
                TextButton(onClick = {
                    Legal.openUrl(context, Legal.CYBERTIP_URL)
                }) { Text("NCMEC CyberTip") }
                TextButton(onClick = onDismiss) { Text("Cancel") }
            }
        },
    )
}
