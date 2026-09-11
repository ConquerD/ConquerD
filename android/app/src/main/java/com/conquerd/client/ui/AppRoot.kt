package com.conquerd.client.ui

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.interaction.DragInteraction
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.AddCircle
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.Phone
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.conquerd.client.AppViewModel
import com.conquerd.client.roomHeadcount
import com.conquerd.client.R
import com.conquerd.client.ChatMessage
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import com.conquerd.client.AppState
import com.conquerd.client.CallPhase
import com.conquerd.client.CameraCapture
import com.conquerd.client.CallState
import com.conquerd.client.AppSettings
import com.conquerd.client.Legal
import com.conquerd.client.FileOffer
import com.conquerd.client.SavedFile
import com.conquerd.client.SupernodeInfo
import com.conquerd.client.ConnectionMode
import com.conquerd.client.HomeTab
import com.conquerd.client.Room
import com.conquerd.client.RoomMessage
import com.conquerd.client.Peer
import com.conquerd.client.roomSenderName
import com.conquerd.client.Screen
import java.text.DateFormat
import java.util.Date

@Composable
fun AppRoot(viewModel: AppViewModel) {
    val state by viewModel.state.collectAsState()
    val snackbars = remember { SnackbarHostState() }

    // Errors and notices are transient; showing them in a snackbar keeps them
    // out of the layout so a failed send does not shift the message list.
    LaunchedEffect(state.error, state.notice) {
        val message = state.error ?: state.notice ?: return@LaunchedEffect
        snackbars.showSnackbar(message)
        viewModel.dismissError()
    }

    // Hold the screen awake while the camera is live.
    //
    // CameraX unbinds when its lifecycle owner stops, and locking the phone
    // stops it even under `ProcessLifecycleOwner` - so a lock kills the
    // capture mid-call. Android deliberately restricts camera access from the
    // lock screen, so the answer is to not let it lock while streaming rather
    // than to try to keep capturing behind it.
    //
    // Deliberately video-only: an audio call is expected to keep running with
    // the screen off, and pinning the display on for one would waste
    // significant battery for no benefit.
    val view = LocalView.current
    DisposableEffect(state.videoActive) {
        view.keepScreenOn = state.videoActive
        onDispose { view.keepScreenOn = false }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbars) },
    ) { padding ->
        Surface(modifier = Modifier.padding(padding).fillMaxSize()) {
            when (val screen = state.screen) {
                Screen.Unlock -> UnlockScreen(
                    busy = state.busy,
                    autoUnlocking = state.autoUnlocking,
                    version = viewModel.coreVersion,
                    onUnlock = viewModel::unlock,
                )

                Screen.Terms -> TermsScreen(
                    onAccept = viewModel::acceptTerms,
                    onDecline = viewModel::declineTerms,
                )

                Screen.Home -> HomeScreen(viewModel = viewModel)

                is Screen.Portal -> PortalScreen(
                    supernodeId = screen.supernodeId,
                    label = screen.label,
                    myPeerId = state.identity.publicId,
                    core = viewModel.portalCore(),
                    onBack = viewModel::closePortal,
                )

                Screen.Settings -> SettingsScreen(
                    state = state,
                    onBack = viewModel::closeSettings,
                    onSetHandle = viewModel::setHandle,
                    onSetFrontCamera = viewModel::setFrontCamera,
                    onSetVoiceActivation = viewModel::setVoiceActivation,
                    onSetTheme = viewModel::setTheme,
                    onSetAvatarConfig = viewModel::setAvatarConfig,
                    onPreviewAvatar = viewModel::previewAvatar,
                    onPurgeHistory = viewModel::purgeChatHistory,
                    onTrimHistory = viewModel::trimChatHistory,
                    onSetInputGain = viewModel::setInputGain,
                    onSetOutputGain = viewModel::setOutputGain,
                    onSetNoiseStrength = viewModel::setNoiseStrength,
                    onSetVoiceBitrate = viewModel::setVoiceBitrate,
                    onRemoveSupernode = viewModel::removeSupernode,
                    onOpenPortal = viewModel::openPortal,
                )

                is Screen.Chat -> ChatScreen(
                    peer = screen.peer,
                    messages = state.messages,
                    onBack = viewModel::closeChat,
                    onSend = viewModel::sendChat,
                    onCall = { viewModel.startCall(screen.peer) },
                    onRetry = { viewModel.retryMessage(it.id) },
                    onDelete = { viewModel.deleteMessage(it.id) },
                    transfers = state.transfers,
                    onSendFile = viewModel::sendFile,
                )

                is Screen.RoomChat -> RoomChatScreen(
                    room = screen.room,
                    messages = state.roomMessages,
                    avatars = state.avatars,
                    peers = state.peers,
                    members = state.roomMembers,
                    chatMembers = state.roomChatMembers,
                    joined = state.roomJoined,
                    voiceActive = state.roomVoiceActive,
                    muted = state.muted,
                    videoActive = state.videoActive,
                    onBack = viewModel::closeRoom,
                    onSend = viewModel::sendRoomChat,
                    onJoinVoice = viewModel::joinRoomVoice,
                    onLeaveVoice = viewModel::leaveRoomVoice,
                    onToggleMute = viewModel::toggleMute,
                    // No peer id: the supernode fans room video out to every
                    // participant, rather than it being addressed to one.
                    onToggleVideo = { wanted ->
                        if (wanted) viewModel.startVideo(null) else viewModel.stopVideo(null)
                    },
                    transfers = state.transfers,
                    onSendFile = viewModel::sendRoomFile,
                    onShare = viewModel::generateRoomInvite,
                )
            }
        }
    }

    state.inviteUrl?.let { url ->
        InviteDialog(url = url, onDismiss = viewModel::dismissInvite)
    }

    // A file offer is a question, not a notification: nothing is transferred
    // until it is answered, so it interrupts rather than waiting in a list.
    state.fileOffer?.let { offer ->
        FileOfferDialog(
            offer = offer,
            onAccept = viewModel::acceptFileOffer,
            onReject = viewModel::rejectFileOffer,
        )
    }

    state.savedFile?.let { saved ->
        SaveFilePrompt(
            saved = saved,
            onSave = viewModel::exportSavedFile,
            onDismiss = viewModel::dismissSavedFile,
        )
    }

    state.call?.let { call ->
        CallOverlay(
            call = call,
            videoActive = state.videoActive,
            onAccept = viewModel::acceptCall,
            onReject = viewModel::rejectCall,
            onEnd = viewModel::endCall,
            onToggleMute = viewModel::toggleMute,
            onToggleVideo = { wanted ->
                if (wanted) viewModel.startVideo(call.peerId) else viewModel.stopVideo(call.peerId)
            },
        )
    }

    if (state.screen is Screen.Home) {
        NotificationPermissionPrompt(AppSettings(LocalContext.current))
    }
}

/**
 * Incoming calls take over the screen; calls already in progress sit in a bar
 * so the rest of the app stays usable during them.
 */
@Composable
private fun CallOverlay(
    call: CallState,
    videoActive: Boolean,
    onAccept: () -> Unit,
    onReject: () -> Unit,
    onEnd: () -> Unit,
    onToggleMute: () -> Unit,
    onToggleVideo: (Boolean) -> Unit,
) {
    val context = LocalContext.current

    // CameraX has to be bound before the core asks for video: the native side
    // waits a few seconds for a first frame to learn the capture size, so
    // binding afterwards would race that timeout. The in-app explanation
    // runs first: video continues with the screen off.
    val requestCamera = rememberExplainedPermission(
        permission = Manifest.permission.CAMERA,
        title = CameraRationaleTitle,
        body = CameraRationaleBody,
        onGranted = {
            CameraCapture.start(context)
            onToggleVideo(true)
        },
    )
    val requestMicToAnswer = rememberExplainedPermission(
        permission = Manifest.permission.RECORD_AUDIO,
        title = MicRationaleTitle,
        body = MicRationaleBody,
        onGranted = onAccept,
    )
    if (call.phase == CallPhase.INCOMING) {
        AlertDialog(
            onDismissRequest = { /* a ringing call needs an explicit answer */ },
            title = { Text("Incoming call") },
            text = { Text(call.peerLabel) },
            confirmButton = { TextButton(onClick = { requestMicToAnswer() }) { Text("Answer") } },
            dismissButton = { TextButton(onClick = onReject) { Text("Decline") } },
        )
        return
    }

    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.BottomCenter) {
        Card(
            modifier = Modifier.fillMaxWidth().padding(12.dp),
            colors = CardDefaults.cardColors(
                containerColor = MaterialTheme.colorScheme.primaryContainer,
            ),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text(call.peerLabel, style = MaterialTheme.typography.titleSmall)
                    Text(
                        if (call.phase == CallPhase.OUTGOING) "Calling..." else "In call",
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
                TextButton(onClick = onToggleMute) {
                    Text(if (call.muted) "Unmute" else "Mute")
                }
                TextButton(
                    onClick = {
                        if (videoActive) {
                            onToggleVideo(false)
                            CameraCapture.stop()
                        } else {
                            requestCamera()
                        }
                    },
                ) {
                    Text(if (videoActive) "Stop video" else "Video")
                }
                TextButton(onClick = onEnd) { Text("End") }
            }
        }
    }
}

// ── Unlock ─────────────────────────────────────────────────────────────────

@Composable
private fun UnlockScreen(
    busy: Boolean,
    autoUnlocking: Boolean,
    version: String,
    onUnlock: (String, Boolean, android.net.Uri?) -> Unit,
) {
    var passphrase by remember { mutableStateOf("") }
    var stayUnlocked by remember { mutableStateOf(false) }
    var keyfile by remember { mutableStateOf<android.net.Uri?>(null) }

    val pickKeyfile = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri -> keyfile = uri }

    // A stored key is being tried: asking for a passphrase we are about to not
    // need would flash a prompt on every launch, which is the thing the user
    // turned this on to avoid.
    if (autoUnlocking) {
        Column(
            modifier = Modifier.fillMaxSize().padding(24.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Image(
                painter = painterResource(R.drawable.ic_logo),
                contentDescription = null,
                modifier = Modifier.width(122.dp).height(56.dp),
            )
            Spacer(Modifier.height(24.dp))
            CircularProgressIndicator(strokeWidth = 2.dp)
        }
        return
    }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        // Optical centre rather than Arrangement.Center: the block is roughly
        // 350dp in an 850dp viewport, and splitting that slack evenly leaves a
        // quarter of the screen empty above the mark. Weighting the gap 1:2
        // lifts it to where the eye expects a sign-in screen to sit, and keeps
        // the field high enough that the IME does not shove the layout when it
        // opens.
        Spacer(Modifier.weight(1f))

        Image(
            painter = painterResource(R.drawable.ic_logo),
            contentDescription = null,
            modifier = Modifier.width(122.dp).height(56.dp),
        )
        Spacer(Modifier.height(16.dp))

        Text("DoubleSlash", style = MaterialTheme.typography.headlineLarge)
        Spacer(Modifier.height(8.dp))
        Text(
            "Your identity never leaves this device.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(32.dp))

        OutlinedTextField(
            value = passphrase,
            onValueChange = { passphrase = it },
            label = { Text("Passphrase") },
            supportingText = { Text("Leave empty for an unencrypted identity.") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Go),
            keyboardActions = KeyboardActions(
                onGo = { onUnlock(passphrase, stayUnlocked, keyfile) },
            ),
            enabled = !busy,
            modifier = Modifier.fillMaxWidth(),
        )

        Spacer(Modifier.height(8.dp))

        // A keyfile can stand in for a passphrase or strengthen one: the core
        // hashes the file and appends it to the typed text. Matching the
        // desktop matters here - an identity created with both only opens
        // with both.
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(
                onClick = { pickKeyfile.launch(arrayOf("*/*")) },
                enabled = !busy,
            ) { Text(if (keyfile == null) "Use a keyfile" else "Change keyfile") }

            if (keyfile != null) {
                TextButton(onClick = { keyfile = null }, enabled = !busy) { Text("Clear") }
            }
        }
        if (keyfile != null) {
            Text(
                keyfile?.lastPathSegment ?: "keyfile selected",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth(),
            )
        }

        Spacer(Modifier.height(8.dp))

        // ── Optional auto-unlock ──────────────────────────────────────────
        //
        // Off unless the user turns it on, and the caption states both sides
        // rather than selling the convenience: the cost - anyone who can use
        // this phone can open the identity - is the part that is easy to miss.
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable(enabled = !busy) { stayUnlocked = !stayUnlocked },
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Checkbox(
                checked = stayUnlocked,
                onCheckedChange = { stayUnlocked = it },
                enabled = !busy,
            )
            Column(Modifier.padding(start = 4.dp)) {
                Text("Stay unlocked on this device", style = MaterialTheme.typography.bodyMedium)
                Text(
                    if (stayUnlocked) {
                        "DoubleSlash will open without this passphrase. Your key is kept in " +
                            "the Android Keystore, so anyone who can use this phone can open " +
                            "your identity. Your passphrase itself is never stored."
                    } else {
                        "You will type this passphrase every launch. Your identity stays " +
                            "unreadable to anyone who gets the phone's files."
                    },
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        Spacer(Modifier.height(16.dp))

        Button(
            onClick = { onUnlock(passphrase, stayUnlocked, keyfile) },
            enabled = !busy,
            modifier = Modifier.fillMaxWidth(),
        ) {
            if (busy) {
                CircularProgressIndicator(
                    modifier = Modifier.size(18.dp),
                    strokeWidth = 2.dp,
                    color = MaterialTheme.colorScheme.onPrimary,
                )
            } else {
                Text("Unlock")
            }
        }

        Spacer(Modifier.height(16.dp))
        val context = LocalContext.current
        TextButton(onClick = { Legal.openUrl(context, Legal.PRIVACY_URL) }) {
            Text("Privacy policy")
        }

        Spacer(Modifier.height(24.dp))
        Text(
            "core $version",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.weight(2f))
    }
}

// ── Home ───────────────────────────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun HomeScreen(viewModel: AppViewModel) {
    val state by viewModel.state.collectAsState()
    var showAccept by remember { mutableStateOf(false) }
    var showAppMenu by remember { mutableStateOf(false) }
    var confirmLock by remember { mutableStateOf(false) }
    var confirmRemovePeer by remember { mutableStateOf<Peer?>(null) }
    var reportPeer by remember { mutableStateOf<Peer?>(null) }
    var reportRoom by remember { mutableStateOf<Room?>(null) }
    var showCreateRoom by remember { mutableStateOf(false) }

    Column(Modifier.fillMaxSize()) {
        TopAppBar(
            // The Scaffold above already pays the status-bar inset for this
            // content, and an M3 top bar applies its own by default - which
            // insets the bar twice and leaves a status-bar-height band of dead
            // space above the title. These bars live inside the Scaffold body
            // rather than its topBar slot, so the inset is not theirs to add.
            windowInsets = WindowInsets(0, 0, 0, 0),
            title = { Text(if (state.tab == HomeTab.PEERS) "Peers" else "Rooms") },
            navigationIcon = {
                // The mark doubles as the app menu, the way a desktop app's
                // logo opens its application menu. It is the only affordance
                // in the bar that is not about the current tab, so app-wide
                // things - what this build is, who I am, signing out - belong
                // behind it rather than competing with Refresh and Add.
                Box {
                    IconButton(onClick = { showAppMenu = true }) {
                        Image(
                            painter = painterResource(R.drawable.ic_logo),
                            contentDescription = "App menu",
                            modifier = Modifier.width(36.dp).height(17.dp),
                        )
                    }
                    AppMenu(
                        expanded = showAppMenu,
                        state = state,
                        version = viewModel.coreVersion,
                        onDismiss = { showAppMenu = false },
                        onSetStayUnlocked = viewModel::setStayUnlocked,
                        onRequestLock = {
                            showAppMenu = false
                            confirmLock = true
                        },
                        onOpenSettings = viewModel::openSettings,
                    )
                }
            },
            actions = {
                IconButton(
                    onClick = {
                        if (state.tab == HomeTab.PEERS) {
                            viewModel.refreshPeers()
                        } else {
                            viewModel.refreshRooms()
                        }
                    },
                ) {
                    Icon(Icons.Filled.Refresh, contentDescription = "Refresh")
                }
                if (state.tab == HomeTab.PEERS) {
                    IconButton(onClick = { viewModel.generateInvite() }) {
                        Icon(Icons.Filled.Add, contentDescription = "Create invite")
                    }
                } else {
                    IconButton(
                        onClick = { showCreateRoom = true },
                        enabled = state.supernodes.isNotEmpty(),
                    ) {
                        Icon(Icons.Filled.Add, contentDescription = "Create room")
                    }
                    val hiddenCount = state.rooms.count { it.hidden }
                    if (hiddenCount > 0) {
                        TextButton(onClick = { viewModel.toggleShowHiddenRooms() }) {
                            Text(
                                if (state.showHiddenRooms) "Hide $hiddenCount" else "Show $hiddenCount",
                            )
                        }
                    }
                }
            },
        )

        ConnectionBanner(state.connectionMode)

        Box(Modifier.weight(1f)) {
            when (state.tab) {
                HomeTab.PEERS -> PeersList(
                    state = state,
                    onOpenPeer = viewModel::openChat,
                    onCreateInvite = { viewModel.generateInvite() },
                    onAcceptInvite = { showAccept = true },
                    onSetBlocked = { peer, blocked ->
                        viewModel.setPeerBlocked(peer.peerId, blocked)
                    },
                    onRemove = { peer -> confirmRemovePeer = peer },
                    onReport = { peer -> reportPeer = peer },
                )

                HomeTab.ROOMS -> RoomsList(
                    rooms = state.rooms,
                    showHidden = state.showHiddenRooms,
                    voiceRosters = state.roomVoiceRosters,
                    textRosters = state.roomTextRosters,
                    onOpenRoom = viewModel::openRoom,
                    onSetHidden = viewModel::setRoomHidden,
                    onReport = { room -> reportRoom = room },
                )
            }
        }

        NavigationBar(windowInsets = WindowInsets(0, 0, 0, 0)) {
            NavigationBarItem(
                selected = state.tab == HomeTab.PEERS,
                onClick = { viewModel.selectTab(HomeTab.PEERS) },
                icon = { Icon(Icons.Filled.Person, contentDescription = null) },
                label = { Text("Peers") },
            )
            NavigationBarItem(
                selected = state.tab == HomeTab.ROOMS,
                onClick = { viewModel.selectTab(HomeTab.ROOMS) },
                icon = { Icon(Icons.AutoMirrored.Filled.List, contentDescription = null) },
                label = { Text("Rooms") },
            )
        }
    }

    if (showAccept) {
        AcceptInviteDialog(
            onDismiss = { showAccept = false },
            onAccept = {
                viewModel.acceptInvite(it)
                showAccept = false
            },
        )
    }

    confirmRemovePeer?.let { peer ->
        RemovePeerDialog(
            peer = peer,
            onDismiss = { confirmRemovePeer = null },
            onConfirm = {
                confirmRemovePeer = null
                viewModel.removePeer(peer.peerId)
            },
        )
    }

    reportPeer?.let { peer ->
        ReportDialog(
            kind = "peer",
            targetId = peer.peerId,
            targetLabel = peer.label,
            onBlock = { viewModel.setPeerBlocked(peer.peerId, true) },
            onDismiss = { reportPeer = null },
        )
    }

    reportRoom?.let { room ->
        ReportDialog(
            kind = "room",
            targetId = "${room.supernodeId}:${room.roomId}",
            targetLabel = room.roomName.ifBlank { room.roomId.take(12) },
            onBlock = null,
            onDismiss = { reportRoom = null },
        )
    }

    if (showCreateRoom) {
        CreateRoomDialog(
            supernodes = state.supernodes,
            rooms = state.rooms,
            onDismiss = { showCreateRoom = false },
            onCreate = { supernodeId, name, isPrivate, parentRoomId ->
                showCreateRoom = false
                viewModel.createRoom(supernodeId, name, isPrivate, parentRoomId)
            },
        )
    }

    if (confirmLock) {
        LockIdentityDialog(
            stayUnlocked = state.stayUnlocked,
            onDismiss = { confirmLock = false },
            onConfirm = {
                confirmLock = false
                viewModel.lockAndForget()
            },
        )
    }
}

@Composable
private fun PeersList(
    state: AppState,
    onOpenPeer: (Peer) -> Unit,
    onCreateInvite: () -> Unit,
    onAcceptInvite: () -> Unit,
    onSetBlocked: (Peer, Boolean) -> Unit,
    onRemove: (Peer) -> Unit,
    onReport: (Peer) -> Unit,
) {
    if (state.peers.isEmpty()) {
        EmptyPeers(onCreateInvite = onCreateInvite, onAcceptInvite = onAcceptInvite)
        return
    }

    LazyColumn(Modifier.fillMaxSize()) {
        items(state.peers, key = { it.peerId }) { peer ->
            PeerRow(
                peer = peer,
                online = peer.peerId in state.onlinePeers,
                avatar = state.avatars[peer.peerId],
                onClick = { onOpenPeer(peer) },
                onSetBlocked = { blocked -> onSetBlocked(peer, blocked) },
                onRemove = { onRemove(peer) },
                onReport = { onReport(peer) },
            )
            HorizontalDivider()
        }
        item {
            TextButton(
                onClick = onAcceptInvite,
                modifier = Modifier.fillMaxWidth().padding(16.dp),
            ) { Text("Accept an invite") }
        }
    }
}

/**
 * The two room-occupancy pills, matching the desktop sidebar's bubbles.
 *
 * Voice and text are separate populations - a peer reading a room over text
 * never appears in the voice roster - so one number cannot stand for both.
 * A room no node has reported on yet shows nothing rather than a
 * possibly-wrong zero, which is the desktop's "-" placeholder in spirit.
 */
@Composable
private fun RoomCountBadges(voice: Int, text: Int) {
    if (voice == 0 && text == 0) return
    Row(
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (voice > 0) {
            CountPill(Icons.Filled.Phone, "in voice", voice, MaterialTheme.colorScheme.primary)
        }
        if (text > 0) {
            CountPill(Icons.Filled.Person, "in room", text, MaterialTheme.colorScheme.secondary)
        }
    }
}

@Composable
private fun CountPill(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    description: String,
    count: Int,
    tint: Color,
) {
    Surface(
        shape = RoundedCornerShape(11.dp),
        color = tint.copy(alpha = 0.16f),
        contentColor = tint,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(3.dp),
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
        ) {
            Icon(icon, contentDescription = description, modifier = Modifier.size(13.dp))
            Text("$count", style = MaterialTheme.typography.labelSmall)
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RoomsList(
    rooms: List<Room>,
    showHidden: Boolean,
    /** Per-node voice rosters, unioned per room for the badge. */
    voiceRosters: Map<String, List<String>>,
    /** Per-node chat rosters (voice plus text subscribers), same shape. */
    textRosters: Map<String, List<String>>,
    onOpenRoom: (Room) -> Unit,
    onSetHidden: (Room, Boolean) -> Unit,
    onReport: (Room) -> Unit,
) {
    // Hidden is per-profile local state, so the desktop's choices arrive with
    // the room list and are honoured here rather than re-derived.
    val visible = remember(rooms, showHidden) {
        layOutSpaceTree(rooms.filter { showHidden || !it.hidden })
    }

    if (visible.isEmpty()) {
        Column(
            modifier = Modifier.fillMaxSize().padding(32.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text(
                if (rooms.isEmpty()) "No rooms yet" else "All rooms are hidden",
                style = MaterialTheme.typography.titleMedium,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                if (rooms.isEmpty()) {
                    "Rooms you create or are invited to appear here."
                } else {
                    "Use Show above to reveal them."
                },
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        return
    }

    LazyColumn(Modifier.fillMaxSize()) {
        items(visible, key = { it.room.key }) { node ->
            val room = node.room
            var menuOpen by remember(room.key) { mutableStateOf(false) }
            Box {
            ListItem(
                headlineContent = { Text(room.roomName.ifBlank { room.roomId.take(12) }) },
                supportingContent = {
                    Text(
                        buildString {
                            append(room.roomType.ifBlank { "room" })
                            if (room.isCreator) append(" - yours")
                            if (room.spaceId.isNotBlank()) append(" - in a space")
                            if (room.hidden) append(" - hidden, long-press for options")
                        },
                        style = MaterialTheme.typography.bodySmall,
                    )
                },
                trailingContent = {
                    RoomCountBadges(
                        voice = voiceRosters.roomHeadcount(room.roomId),
                        text = textRosters.roomHeadcount(room.roomId),
                    )
                },
                // Depth is an indent rather than a drawn tree: nesting is
                // rarely more than two deep, and an indent reads as "inside
                // that one" without spending phone width on connectors.
                modifier = Modifier
                    .padding(start = (node.depth * 20).dp)
                    .combinedClickable(
                        onClick = { onOpenRoom(room) },
                        onLongClick = { menuOpen = true },
                    ),
            )
            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                DropdownMenuItem(
                    text = { Text(if (room.hidden) "Show in list" else "Hide from list") },
                    onClick = {
                        onSetHidden(room, !room.hidden)
                        menuOpen = false
                    },
                )
                DropdownMenuItem(
                    text = { Text("Report") },
                    onClick = {
                        onReport(room)
                        menuOpen = false
                    },
                )
            }
            }
            HorizontalDivider()
        }
    }
}

/** A room placed in the Space tree, with how deep it sits. */
private data class RoomNode(val room: Room, val depth: Int)

/**
 * Order rooms parent-before-child and record each one's depth.
 *
 * The core hands back a flat list carrying `space_id` and `parent_id`; the
 * nesting is only implied. Rooms whose parent is absent from the list are
 * treated as top-level rather than dropped — a sub-room can outlive the parent
 * in the local store when the parent was hidden or never synced, and a room
 * you cannot see is worse than one shown at the wrong depth.
 */
private fun layOutSpaceTree(rooms: List<Room>): List<RoomNode> {
    val byRoomId = rooms.associateBy { it.roomId }
    val children = rooms.groupBy { room ->
        val parent = room.parentId
        // Empty, self-referential, pointing at the space itself, or naming a
        // room we do not have all mean "top level".
        if (parent.isBlank() ||
            parent == room.roomId ||
            parent == room.spaceId ||
            parent !in byRoomId
        ) {
            ""
        } else {
            parent
        }
    }

    val ordered = mutableListOf<RoomNode>()
    val seen = mutableSetOf<String>()

    fun walk(parentId: String, depth: Int) {
        children[parentId]
            .orEmpty()
            .sortedBy { it.roomName.lowercase() }
            .forEach { room ->
                // A parent cycle would otherwise recurse forever; the store is
                // not supposed to contain one, but this list must not hang.
                if (!seen.add(room.roomId)) return@forEach
                ordered += RoomNode(room, depth)
                walk(room.roomId, depth + 1)
            }
    }

    walk("", 0)

    // Anything a cycle excluded still gets shown, flat.
    rooms.filter { it.roomId !in seen }
        .sortedBy { it.roomName.lowercase() }
        .forEach { ordered += RoomNode(it, 0) }

    return ordered
}

@Composable
private fun ConnectionBanner(mode: ConnectionMode) {
    val (label, color) = when (mode) {
        ConnectionMode.DIRECT -> "Direct" to Color(0xFF16A34A)
        ConnectionMode.RELAY -> "Relayed" to Color(0xFFCA8A04)
        ConnectionMode.OFFLINE -> "Offline" to MaterialTheme.colorScheme.onSurfaceVariant
    }

    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(8.dp).clip(CircleShape).background(color))
        Spacer(Modifier.width(8.dp))
        Text(label, style = MaterialTheme.typography.labelMedium, color = color)
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PeerRow(
    peer: Peer,
    online: Boolean,
    avatar: AvatarArt?,
    onClick: () -> Unit,
    onSetBlocked: (Boolean) -> Unit,
    onRemove: () -> Unit,
    onReport: () -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current

    Box {
    ListItem(
        headlineContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(peer.label)
                // Blocked is otherwise invisible: the row looks identical to a
                // peer who simply is not online, which is the wrong story.
                if (peer.blocked) {
                    Spacer(Modifier.width(6.dp))
                    Text(
                        "blocked",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        },
        supportingContent = {
            Text(
                peer.peerId,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.bodySmall,
            )
        },
        leadingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(
                    Modifier
                        .size(10.dp)
                        .clip(CircleShape)
                        .background(
                            if (online) {
                                Color(0xFF16A34A)
                            } else {
                                MaterialTheme.colorScheme.outlineVariant
                            },
                        ),
                )
                Spacer(Modifier.width(10.dp))
                // Until the identicon arrives the row keeps its shape with a
                // blank of the same size, so the list does not jump.
                if (avatar != null) {
                    Avatar(avatar, Modifier.size(36.dp))
                } else {
                    Box(
                        Modifier
                            .size(36.dp)
                            .clip(RoundedCornerShape(percent = 18))
                            .background(MaterialTheme.colorScheme.surfaceVariant),
                    )
                }
            }
        },
        // Long-press for the peer actions, the same gesture rooms already use
        // for hide/unhide - one idiom for "more, on this row".
        modifier = Modifier.combinedClickable(
            onClick = onClick,
            onLongClick = { menuOpen = true },
        ),
    )

    DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
        DropdownMenuItem(
            text = { Text("Copy peer ID") },
            onClick = {
                clipboard.setText(androidx.compose.ui.text.AnnotatedString(peer.peerId))
                menuOpen = false
            },
        )
        DropdownMenuItem(
            text = { Text(if (peer.blocked) "Unblock" else "Block") },
            onClick = {
                onSetBlocked(!peer.blocked)
                menuOpen = false
            },
        )
        DropdownMenuItem(
            text = { Text("Report") },
            onClick = {
                onReport()
                menuOpen = false
            },
        )
        HorizontalDivider()
        DropdownMenuItem(
            text = { Text("Remove peer", color = MaterialTheme.colorScheme.error) },
            onClick = {
                onRemove()
                menuOpen = false
            },
        )
    }
    }
}

/**
 * Create a room on one of the supernodes we know.
 *
 * The host picker only appears when there is a choice to make — with one
 * supernode, asking which is noise.
 */
@Composable
private fun CreateRoomDialog(
    supernodes: List<Peer>,
    rooms: List<Room>,
    onDismiss: () -> Unit,
    onCreate: (String, String, Boolean, String) -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var isPrivate by remember { mutableStateOf(false) }
    var hostIndex by remember { mutableStateOf(0) }
    var hostMenuOpen by remember { mutableStateOf(false) }
    var parent by remember { mutableStateOf<Room?>(null) }
    var parentMenuOpen by remember { mutableStateOf(false) }

    val host = supernodes.getOrNull(hostIndex)

    // Only rooms on the chosen host can be a parent: the Space tree belongs to
    // one supernode, so nesting across hosts is not a thing to offer.
    val candidates = remember(rooms, host) {
        rooms.filter { host != null && it.supernodeId == host.peerId }
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("New room") },
        text = {
            Column {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text("Room name") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )

                Spacer(Modifier.height(12.dp))

                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { isPrivate = !isPrivate },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Checkbox(checked = isPrivate, onCheckedChange = { isPrivate = it })
                    Column(Modifier.padding(start = 4.dp)) {
                        Text("Private", style = MaterialTheme.typography.bodyMedium)
                        Text(
                            if (isPrivate) {
                                "Only people you invite can join."
                            } else {
                                "Anyone on this supernode can find and join it."
                            },
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }

                if (candidates.isNotEmpty()) {
                    Spacer(Modifier.height(12.dp))
                    Box {
                        TextButton(onClick = { parentMenuOpen = true }) {
                            Text("Inside: ${parent?.roomName ?: "nothing"}")
                        }
                        DropdownMenu(
                            expanded = parentMenuOpen,
                            onDismissRequest = { parentMenuOpen = false },
                        ) {
                            DropdownMenuItem(
                                text = { Text("Nothing - top level") },
                                onClick = {
                                    parent = null
                                    parentMenuOpen = false
                                },
                            )
                            candidates.forEach { room ->
                                DropdownMenuItem(
                                    text = { Text(room.roomName.ifBlank { room.roomId.take(12) }) },
                                    onClick = {
                                        parent = room
                                        parentMenuOpen = false
                                    },
                                )
                            }
                        }
                    }
                }

                if (supernodes.size > 1) {
                    Spacer(Modifier.height(12.dp))
                    Box {
                        TextButton(onClick = { hostMenuOpen = true }) {
                            Text("Host: ${host?.label ?: "choose"}")
                        }
                        DropdownMenu(
                            expanded = hostMenuOpen,
                            onDismissRequest = { hostMenuOpen = false },
                        ) {
                            supernodes.forEachIndexed { index, node ->
                                DropdownMenuItem(
                                    text = { Text(node.label) },
                                    onClick = {
                                        hostIndex = index
                                        hostMenuOpen = false
                                    },
                                )
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    host?.let { onCreate(it.peerId, name, isPrivate, parent?.roomId.orEmpty()) }
                },
                enabled = name.isNotBlank() && host != null,
            ) { Text("Create") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/**
 * Confirm before forgetting a peer.
 *
 * Removal drops the record and ends any call with them. It is recoverable -
 * a fresh invite puts them back - but not from inside this screen, so it is
 * worth one tap of friction.
 */
@Composable
private fun RemovePeerDialog(peer: Peer, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Remove ${peer.label}?") },
        text = {
            Text(
                "This forgets the peer on this device and ends any call with them. " +
                    "Your chat history stays. You will need a new invite to reach " +
                    "them again.",
            )
        },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text("Remove", color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun EmptyPeers(onCreateInvite: () -> Unit, onAcceptInvite: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("No peers yet", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(8.dp))
        Text(
            "Trust is established by exchanging an invite. Send one, or paste one you were given.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        Button(onClick = onCreateInvite) { Text("Create an invite") }
        Spacer(Modifier.height(8.dp))
        TextButton(onClick = onAcceptInvite) { Text("Accept an invite") }
    }
}

// ── Chat ───────────────────────────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChatScreen(
    peer: Peer,
    messages: List<ChatMessage>,
    onBack: () -> Unit,
    onSend: (String) -> Unit,
    onCall: () -> Unit,
    onRetry: (ChatMessage) -> Unit,
    onDelete: (ChatMessage) -> Unit,
    transfers: Map<String, Float>,
    onSendFile: (android.net.Uri) -> Unit,
) {
    var draft by remember { mutableStateOf("") }
    val listState = rememberLazyListState()
    val pinned = rememberPinnedToLatest(
        listState = listState,
        conversationKey = peer.peerId,
        itemCount = messages.size,
    )

    // OpenDocument rather than GetContent: it returns a durable uri we can
    // read from for the length of a copy, which GetContent does not promise.
    val pickFile = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri ->
        uri?.let {
            onSendFile(it)
            pinned.jumpToLatest()
        }
    }

    // Ask at the point of use rather than on launch: a client that demands
    // the microphone before you have placed a call is asking for something it
    // cannot yet justify. The in-app copy runs first because a call keeps
    // the mic on with the screen off.
    val requestMic = rememberExplainedPermission(
        permission = Manifest.permission.RECORD_AUDIO,
        title = MicRationaleTitle,
        body = MicRationaleBody,
        onGranted = onCall,
    )

    Column(Modifier.fillMaxSize().imePadding()) {
        TopAppBar(
            // The Scaffold above already pays the status-bar inset for this
            // content, and an M3 top bar applies its own by default - which
            // insets the bar twice and leaves a status-bar-height band of dead
            // space above the title. These bars live inside the Scaffold body
            // rather than its topBar slot, so the inset is not theirs to add.
            windowInsets = WindowInsets(0, 0, 0, 0),
            title = { Text(peer.label) },
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                }
            },
            actions = {
                IconButton(onClick = { requestMic() }) {
                    Icon(Icons.Filled.Phone, contentDescription = "Call")
                }
            },
        )

        Box(Modifier.weight(1f).fillMaxWidth()) {
            LazyColumn(
                state = listState,
                modifier = Modifier.fillMaxSize().padding(horizontal = 12.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                items(messages, key = { it.id }) { message ->
                    MessageBubble(
                        message = message,
                        onRetry = { onRetry(message) },
                        onDelete = { onDelete(message) },
                    )
                }
            }

            JumpToCurrentButton(visible = pinned.scrolledAway.value, onClick = pinned.jumpToLatest)
        }

        // One bar per transfer in flight, sending or receiving. A file moving
        // is the only thing here slow enough to need one, so it takes space
        // only while it is happening.
        transfers.forEach { (_, progress) ->
            LinearProgressIndicator(
                progress = { progress },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp),
            )
        }

        Row(
            modifier = Modifier.fillMaxWidth().padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = { pickFile.launch(arrayOf("*/*")) }) {
                Icon(Icons.Filled.AddCircle, contentDescription = "Attach a file")
            }
            OutlinedTextField(
                value = draft,
                onValueChange = { draft = it },
                placeholder = { Text("Message") },
                modifier = Modifier.weight(1f),
                maxLines = 4,
            )
            Spacer(Modifier.width(8.dp))
            IconButton(
                onClick = {
                    onSend(draft)
                    draft = ""
                    pinned.jumpToLatest()
                },
                enabled = draft.isNotBlank(),
            ) {
                Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send")
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun MessageBubble(
    message: ChatMessage,
    onRetry: () -> Unit,
    onDelete: () -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    val alignment = if (message.isSelf) Alignment.End else Alignment.Start
    val container = if (message.isSelf) {
        MaterialTheme.colorScheme.primaryContainer
    } else {
        MaterialTheme.colorScheme.surfaceVariant
    }

    Column(Modifier.fillMaxWidth(), horizontalAlignment = alignment) {
        Box {
            Card(
                colors = CardDefaults.cardColors(containerColor = container),
                modifier = Modifier.combinedClickable(
                    onClick = {},
                    onLongClick = { menuOpen = true },
                ),
            ) {
                Text(message.body, modifier = Modifier.padding(10.dp))
            }

            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                DropdownMenuItem(
                    text = { Text("Copy") },
                    onClick = {
                        clipboard.setText(
                            androidx.compose.ui.text.AnnotatedString(message.body),
                        )
                        menuOpen = false
                    },
                )
                // Retry only where it can do something: a failed message of
                // our own. Offering it on a delivered one invites a duplicate.
                if (message.isSelf && message.status == "failed") {
                    DropdownMenuItem(
                        text = { Text("Try again") },
                        onClick = {
                            onRetry()
                            menuOpen = false
                        },
                    )
                }
                DropdownMenuItem(
                    text = { Text("Delete", color = MaterialTheme.colorScheme.error) },
                    onClick = {
                        onDelete()
                        menuOpen = false
                    },
                )
            }
        }
        // A failed send is the one status worth spending a line on — the rest
        // (sending, sent, delivered) resolve on their own within a second.
        val note = if (message.status == "failed") {
            message.statusNote.ifBlank { "not delivered" }
        } else {
            formatTime(message.timestamp)
        }
        Text(
            note,
            style = MaterialTheme.typography.labelSmall,
            color = if (message.status == "failed") {
                MaterialTheme.colorScheme.error
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
            modifier = Modifier.padding(horizontal = 4.dp),
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RoomChatScreen(
    room: Room,
    messages: List<RoomMessage>,
    avatars: Map<String, AvatarArt>,
    peers: List<Peer>,
    /** Voice participants - drives the "in voice" bar only. */
    members: List<String>,
    /** Everyone in the room, voice or text - drives the header count. */
    chatMembers: List<String>,
    joined: Boolean,
    voiceActive: Boolean,
    muted: Boolean,
    videoActive: Boolean,
    onBack: () -> Unit,
    onSend: (String) -> Unit,
    onJoinVoice: () -> Unit,
    onLeaveVoice: () -> Unit,
    onToggleMute: () -> Unit,
    onToggleVideo: (Boolean) -> Unit,
    transfers: Map<String, Float>,
    onSendFile: (android.net.Uri) -> Unit,
    onShare: () -> Unit,
) {
    var draft by remember { mutableStateOf("") }
    val listState = rememberLazyListState()
    val pinned = rememberPinnedToLatest(
        listState = listState,
        conversationKey = room.key,
        itemCount = messages.size,
    )

    val pickFile = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri ->
        uri?.let {
            onSendFile(it)
            pinned.jumpToLatest()
        }
    }
    val context = LocalContext.current

    val requestMic = rememberExplainedPermission(
        permission = Manifest.permission.RECORD_AUDIO,
        title = MicRationaleTitle,
        body = MicRationaleBody,
        onGranted = onJoinVoice,
    )

    // CameraX must be bound before the core asks for video — the native side
    // waits for a first frame to learn the capture size.
    val requestCamera = rememberExplainedPermission(
        permission = Manifest.permission.CAMERA,
        title = CameraRationaleTitle,
        body = CameraRationaleBody,
        onGranted = {
            CameraCapture.start(context)
            onToggleVideo(true)
        },
    )

    Column(Modifier.fillMaxSize().imePadding()) {
        TopAppBar(
            // The Scaffold above already pays the status-bar inset for this
            // content, and an M3 top bar applies its own by default - which
            // insets the bar twice and leaves a status-bar-height band of dead
            // space above the title. These bars live inside the Scaffold body
            // rather than its topBar slot, so the inset is not theirs to add.
            windowInsets = WindowInsets(0, 0, 0, 0),
            title = {
                Column {
                    Text(room.roomName.ifBlank { room.roomId.take(12) })
                    Text(
                        if (joined) {
                            "${chatMembers.size} " +
                                if (chatMembers.size == 1) "member" else "members"
                        } else {
                            "joining..."
                        },
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            },
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Leave room")
                }
            },
            actions = {
                IconButton(onClick = onShare) {
                    Icon(Icons.Filled.Share, contentDescription = "Share this room")
                }
                IconButton(
                    enabled = joined,
                    onClick = {
                        if (voiceActive) {
                            onLeaveVoice()
                        } else {
                            requestMic()
                        }
                    },
                ) {
                    Icon(
                        Icons.Filled.Phone,
                        contentDescription = if (voiceActive) "Leave voice" else "Join voice",
                        tint = if (voiceActive) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            LocalContentColor.current
                        },
                    )
                }
            },
        )

        if (voiceActive) {
            VoiceRail(
                members = members,
                muted = muted,
                videoActive = videoActive,
                onToggleMute = onToggleMute,
                onToggleVideo = {
                    if (videoActive) {
                        onToggleVideo(false)
                        CameraCapture.stop()
                    } else {
                        requestCamera()
                    }
                },
                onLeave = onLeaveVoice,
            )
        }

        if (messages.isEmpty()) {
            Column(
                modifier = Modifier.weight(1f).fillMaxWidth().padding(32.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    // Worth stating plainly rather than showing a blank pane:
                    // rooms are ephemeral on the supernode and room chat is
                    // never written to the local store, so there is no history
                    // to load - only what arrives from now on.
                    if (joined) {
                        "Messages appear from now on. Room chat is not stored on this device."
                    } else {
                        "Waiting for the room to admit you."
                    },
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
        } else {
            Box(Modifier.weight(1f).fillMaxWidth()) {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize().padding(horizontal = 12.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    items(messages, key = { it.messageId }) {
                        RoomMessageBubble(
                            message = it,
                            avatar = avatars[it.senderId],
                            senderName = peers.roomSenderName(it.senderId, it.senderHandle),
                        )
                    }
                }

                JumpToCurrentButton(
                    visible = pinned.scrolledAway.value,
                    onClick = pinned.jumpToLatest,
                )
            }
        }

        Row(
            modifier = Modifier.fillMaxWidth().padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(
                onClick = { pickFile.launch(arrayOf("*/*")) },
                enabled = joined,
            ) {
                Icon(Icons.Filled.AddCircle, contentDescription = "Share a file")
            }
            OutlinedTextField(
                value = draft,
                onValueChange = { draft = it },
                placeholder = { Text(if (joined) "Message" else "Joining...") },
                enabled = joined,
                modifier = Modifier.weight(1f),
                maxLines = 4,
            )
            Spacer(Modifier.width(8.dp))
            IconButton(
                onClick = {
                    onSend(draft)
                    draft = ""
                    pinned.jumpToLatest()
                },
                enabled = joined && draft.isNotBlank(),
            ) {
                Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send")
            }
        }
    }
}

/**
 * The in-room voice bar: who is present, and the two controls that matter.
 *
 * Participants come from the room roster rather than from audio activity —
 * the core does not surface per-peer speaking state to this layer, so showing
 * a speaking indicator here would be decoration rather than information.
 */
@Composable
private fun VoiceRail(
    members: List<String>,
    muted: Boolean,
    videoActive: Boolean,
    onToggleMute: () -> Unit,
    onToggleVideo: () -> Unit,
    onLeave: () -> Unit,
) {
    Surface(
        color = MaterialTheme.colorScheme.primaryContainer,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(8.dp).clip(CircleShape).background(Color(0xFF16A34A)))
                Spacer(Modifier.width(8.dp))
                Text(
                    "In voice - ${members.size} " +
                        if (members.size == 1) "participant" else "participants",
                    style = MaterialTheme.typography.labelLarge,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onToggleMute) {
                    Text(if (muted) "Unmute" else "Mute")
                }
                TextButton(onClick = onToggleVideo) {
                    Text(if (videoActive) "Stop video" else "Video")
                }
                TextButton(onClick = onLeave) { Text("Leave") }
            }

            if (members.isNotEmpty()) {
                Spacer(Modifier.height(4.dp))
                Text(
                    members.joinToString(", ") { it.take(10) },
                    style = MaterialTheme.typography.labelSmall,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun RoomMessageBubble(message: RoomMessage, avatar: AvatarArt?, senderName: String) {
    val alignment = if (message.isSelf) Alignment.End else Alignment.Start
    val container = if (message.isSelf) {
        MaterialTheme.colorScheme.primaryContainer
    } else {
        MaterialTheme.colorScheme.surfaceVariant
    }

    // Rooms only, matching the desktop: in a 1:1 chat the same two faces beside
    // every line are noise, but in a room the face is how you tell speakers
    // apart at a glance, ahead of reading the name.
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = if (message.isSelf) Arrangement.End else Arrangement.Start,
        verticalAlignment = Alignment.Top,
    ) {
        if (!message.isSelf) {
            RoomAvatarSlot(avatar)
            Spacer(Modifier.width(8.dp))
        }
        Column(horizontalAlignment = alignment) {
            // Unlike a 1:1 chat, a room has many senders, so each message has to
            // say who wrote it.
            if (!message.isSelf) {
                Text(
                    senderName,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(horizontal = 4.dp),
                )
            }
            Card(colors = CardDefaults.cardColors(containerColor = container)) {
                Text(message.body, modifier = Modifier.padding(10.dp))
            }
            Text(
                formatTime(message.timestamp),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 4.dp),
            )
        }
        if (message.isSelf) {
            Spacer(Modifier.width(8.dp))
            RoomAvatarSlot(avatar)
        }
    }
}

/**
 * One avatar beside a room message, holding its space while the art loads.
 *
 * Avatars arrive asynchronously - the core renders the SVG on request - so an
 * absent one reserves the same box rather than collapsing, which would shuffle
 * every bubble sideways the moment it appeared.
 */
@Composable
private fun RoomAvatarSlot(avatar: AvatarArt?) {
    if (avatar != null) {
        Avatar(avatar, Modifier.size(ROOM_AVATAR_SIZE))
    } else {
        Spacer(Modifier.size(ROOM_AVATAR_SIZE))
    }
}

/** Matches the desktop's 32px room avatar. */
private val ROOM_AVATAR_SIZE = 32.dp

// ── App menu ───────────────────────────────────────────────────────────────

/**
 * What this build is, who I am here, and the app-wide switches.
 *
 * Deliberately short: a phone menu that lists everything is a menu nobody
 * reads. Identity and version are here because they are what you need when
 * something is wrong and someone asks you what you are running.
 */
@Composable
private fun AppMenu(
    expanded: Boolean,
    state: AppState,
    version: String,
    onDismiss: () -> Unit,
    onSetStayUnlocked: (Boolean) -> Unit,
    onRequestLock: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current

    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Image(
                    painter = painterResource(R.drawable.ic_logo),
                    contentDescription = null,
                    modifier = Modifier.width(44.dp).height(20.dp),
                )
                Spacer(Modifier.width(8.dp))
                Text("DoubleSlash", style = MaterialTheme.typography.titleSmall)
            }
            Spacer(Modifier.height(4.dp))
            Text(
                "core $version",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (state.identity.fingerprint.isNotBlank()) {
                Text(
                    state.identity.fingerprint,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        HorizontalDivider()

        DropdownMenuItem(
            text = { Text("Settings") },
            leadingIcon = { Icon(Icons.Filled.Settings, contentDescription = null) },
            onClick = {
                onOpenSettings()
                onDismiss()
            },
        )

        DropdownMenuItem(
            text = { Text("Copy my peer ID") },
            leadingIcon = { Icon(Icons.Filled.Person, contentDescription = null) },
            enabled = state.identity.publicId.isNotBlank(),
            onClick = {
                clipboard.setText(
                    androidx.compose.ui.text.AnnotatedString(state.identity.publicId),
                )
                onDismiss()
            },
        )

        HorizontalDivider()

        // The same choice as the unlock screen, reachable after the fact:
        // changing your mind should not require locking yourself out first.
        DropdownMenuItem(
            text = {
                Column {
                    Text("Stay unlocked on this device")
                    Text(
                        if (state.stayUnlocked) {
                            "On — opens without your passphrase"
                        } else {
                            "Off — asks for your passphrase each launch"
                        },
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            },
            trailingIcon = {
                Switch(checked = state.stayUnlocked, onCheckedChange = null)
            },
            onClick = { onSetStayUnlocked(!state.stayUnlocked) },
        )

        HorizontalDivider()

        DropdownMenuItem(
            text = { Text("Lock identity", color = MaterialTheme.colorScheme.error) },
            leadingIcon = {
                Icon(
                    Icons.Filled.Lock,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.error,
                )
            },
            onClick = onRequestLock,
        )
    }
}

/**
 * Confirm before locking.
 *
 * Locking ends the session, drops the connection to every peer, and - when a
 * key is stored - forgets it, so the way back in is the passphrase. That is
 * too much to hang off one stray tap in a top bar.
 */
@Composable
private fun LockIdentityDialog(
    stayUnlocked: Boolean,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Lock identity?") },
        text = {
            Text(
                if (stayUnlocked) {
                    "This signs out, disconnects your peers, and forgets the key kept " +
                        "on this device. You will need your passphrase to open DoubleSlash again."
                } else {
                    "This signs out and disconnects your peers. You will need your " +
                        "passphrase to open DoubleSlash again."
                },
            )
        },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text("Lock", color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

// ── Portal ───────────────────────────────────────────────────────

/**
 * A supernode's in-app portal.
 *
 * The WebView cannot load `conquerd://` itself - there is no such network
 * protocol - so every request is intercepted and answered by the core over the
 * identity QUIC relay. JavaScript is on because that is the entire point of
 * the portal; what makes it defensible is that the pages come from a supernode
 * the user has already trusted enough to relay their traffic, over an
 * authenticated channel, and the WebView is given no file or content access.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PortalScreen(
    supernodeId: String,
    label: String,
    myPeerId: String,
    core: com.conquerd.client.ConquerdCore,
    onBack: () -> Unit,
) {
    val bridge = remember(supernodeId, myPeerId) {
        com.conquerd.client.PortalBridge(core, supernodeId, myPeerId)
    }

    Column(Modifier.fillMaxSize()) {
        TopAppBar(
            windowInsets = WindowInsets(0, 0, 0, 0),
            title = { Text(label) },
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Leave portal")
                }
            },
        )

        AndroidView(
            modifier = Modifier.fillMaxSize(),
            factory = { context ->
                android.webkit.WebView(context).apply {
                    settings.javaScriptEnabled = true
                    settings.domStorageEnabled = true
                    // No local file or content-provider reach: a portal page is
                    // remote code, and the only thing it may touch is the
                    // bridge below.
                    settings.allowFileAccess = false
                    settings.allowContentAccess = false
                    settings.mixedContentMode =
                        android.webkit.WebSettings.MIXED_CONTENT_NEVER_ALLOW

                    addJavascriptInterface(bridge.PortalApi(), "__conquerdNative")

                    webViewClient = object : android.webkit.WebViewClient() {
                        override fun shouldInterceptRequest(
                            view: android.webkit.WebView,
                            request: android.webkit.WebResourceRequest,
                        ): android.webkit.WebResourceResponse? {
                            val intercepted = bridge.interceptRequest(request)
                            if (intercepted != null) return intercepted
                            // Anything that is not d:// or conquerd:// would
                            // otherwise hit the network with the JS bridge
                            // still attached.
                            return android.webkit.WebResourceResponse(
                                "text/plain",
                                "utf-8",
                                403,
                                "Blocked",
                                emptyMap(),
                                ByteArray(0).inputStream(),
                            )
                        }

                        override fun shouldOverrideUrlLoading(
                            view: android.webkit.WebView,
                            request: android.webkit.WebResourceRequest,
                        ): Boolean {
                            val scheme = request.url.scheme.orEmpty()
                            if (scheme.equals("d", true) ||
                                scheme.equals("conquerd", true)
                            ) {
                                return false
                            }
                            Legal.openUrl(view.context, request.url.toString())
                            return true
                        }
                    }

                    loadUrl("d://$supernodeId/index.html")
                }
            },
        )
    }
}

// ── Settings ──────────────────────────────────────────────────────────────

/**
 * The handful of settings that mean something on a phone.
 *
 * The desktop persists about sixty; most of the rest are device pickers,
 * window geometry and tray behaviour that a phone either decides for itself
 * or does not have. Each control here changes what the next call or launch
 * actually does.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SettingsScreen(
    state: AppState,
    onBack: () -> Unit,
    onSetHandle: (String) -> Unit,
    onSetFrontCamera: (Boolean) -> Unit,
    onSetVoiceActivation: (Boolean) -> Unit,
    onSetTheme: (String) -> Unit,
    onSetAvatarConfig: (String) -> Unit,
    onPreviewAvatar: suspend (String) -> AvatarArt?,
    onPurgeHistory: () -> Unit,
    onTrimHistory: (Int) -> Unit,
    onSetInputGain: (Int) -> Unit,
    onSetOutputGain: (Int) -> Unit,
    onSetNoiseStrength: (Int) -> Unit,
    onSetVoiceBitrate: (Int) -> Unit,
    onRemoveSupernode: (String) -> Unit,
    onOpenPortal: (SupernodeInfo) -> Unit,
) {
    var handle by remember(state.identity.handle) { mutableStateOf(state.identity.handle) }
    var showAvatarEditor by remember { mutableStateOf(false) }
    var confirmPurge by remember { mutableStateOf(false) }
    var confirmRemoveNode by remember { mutableStateOf<SupernodeInfo?>(null) }

    Column(Modifier.fillMaxSize()) {
        TopAppBar(
            windowInsets = WindowInsets(0, 0, 0, 0),
            title = { Text("Settings") },
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                }
            },
        )

        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
        ) {
            Text("Your name", style = MaterialTheme.typography.titleSmall)
            Spacer(Modifier.height(8.dp))
            OutlinedTextField(
                value = handle,
                onValueChange = { handle = it },
                placeholder = { Text("Not set") },
                supportingText = {
                    Text("What peers see instead of your ID. Changing it tells them.")
                },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(8.dp))
            Button(
                onClick = { onSetHandle(handle) },
                enabled = handle.trim() != state.identity.handle,
            ) { Text("Save name") }

            Spacer(Modifier.height(24.dp))
            HorizontalDivider()
            Spacer(Modifier.height(16.dp))

            SettingSwitch(
                title = "Front camera",
                subtitle = if (state.prefs.frontCamera) {
                    "Video calls open with the selfie camera"
                } else {
                    "Video calls open with the rear camera"
                },
                checked = state.prefs.frontCamera,
                onCheckedChange = onSetFrontCamera,
            )

            SettingSwitch(
                title = "Voice activation",
                subtitle = if (state.prefs.voiceActivation) {
                    "The mic opens when you speak"
                } else {
                    "The mic stays open for the whole call"
                },
                checked = state.prefs.voiceActivation,
                onCheckedChange = onSetVoiceActivation,
            )

            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            Spacer(Modifier.height(16.dp))

            Text("Your avatar", style = MaterialTheme.typography.titleSmall)
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                state.avatars[state.identity.peerId]?.let { art ->
                    Avatar(art, Modifier.size(56.dp))
                    Spacer(Modifier.width(12.dp))
                }
                Column(Modifier.weight(1f)) {
                    Text(
                        "Drawn from your identity, so it is the same everywhere.",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                TextButton(onClick = { showAvatarEditor = true }) { Text("Edit") }
            }

            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            Spacer(Modifier.height(16.dp))

            Text("Chat history", style = MaterialTheme.typography.titleSmall)
            Spacer(Modifier.height(4.dp))
            Text(
                "Stored on this device only. Deleting does not remove anything " +
                    "from your peers - they keep their own copies.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(8.dp))
            Row {
                TextButton(onClick = { onTrimHistory(30) }) { Text("Trim over 30 days") }
                TextButton(onClick = { confirmPurge = true }) {
                    Text("Delete all", color = MaterialTheme.colorScheme.error)
                }
            }

            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            Spacer(Modifier.height(16.dp))

            Text("Voice", style = MaterialTheme.typography.titleSmall)
            Spacer(Modifier.height(8.dp))

            TuningSlider(
                label = "Microphone",
                value = state.prefs.inputGain.toFloat(),
                range = 0f..200f,
                display = "${state.prefs.inputGain}%",
                onChange = { onSetInputGain(it.toInt()) },
            )
            TuningSlider(
                label = "Speaker",
                value = state.prefs.outputGain.toFloat(),
                range = 0f..200f,
                display = "${state.prefs.outputGain}%",
                onChange = { onSetOutputGain(it.toInt()) },
            )
            TuningSlider(
                label = "Noise gate",
                value = state.prefs.noiseStrength.toFloat(),
                range = 0f..4f,
                steps = 3,
                display = when (state.prefs.noiseStrength) {
                    0 -> "off"
                    1 -> "mild"
                    2 -> "moderate"
                    3 -> "aggressive"
                    else -> "max"
                },
                onChange = { onSetNoiseStrength(it.toInt()) },
            )
            TuningSlider(
                label = "Voice quality",
                value = state.prefs.voiceBitrate.toFloat(),
                range = 8_000f..128_000f,
                // A ceiling, not a promise: the core drops below it under loss.
                display = "${state.prefs.voiceBitrate / 1000} kbps max",
                onChange = { onSetVoiceBitrate((it / 1000).toInt() * 1000) },
            )

            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            Spacer(Modifier.height(16.dp))

            Text("Supernodes", style = MaterialTheme.typography.titleSmall)
            Spacer(Modifier.height(4.dp))
            Text(
                "The servers that relay your traffic and host rooms. Added by " +
                    "accepting an invite.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(8.dp))

            if (state.supernodeInfo.isEmpty()) {
                Text(
                    "None yet.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            state.supernodeInfo.forEach { node ->
                Row(
                    modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(node.displayName, style = MaterialTheme.typography.bodyLarge)
                        Text(
                            // A cluster presents as one node; saying how many
                            // members it has is what distinguishes "one server"
                            // from "three that fail over".
                            if (node.clusterMembers.size > 1) {
                                "cluster of ${node.clusterMembers.size}"
                            } else {
                                node.peerId.take(16)
                            },
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    TextButton(onClick = { onOpenPortal(node) }) { Text("Portal") }
                    TextButton(onClick = { confirmRemoveNode = node }) {
                        Text("Remove", color = MaterialTheme.colorScheme.error)
                    }
                }
            }

            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            Spacer(Modifier.height(16.dp))

            Text("Theme", style = MaterialTheme.typography.titleSmall)
            Spacer(Modifier.height(8.dp))
            listOf(
                AppSettings.THEME_SYSTEM to "Follow the system",
                AppSettings.THEME_LIGHT to "Light",
                AppSettings.THEME_DARK to "Dark",
            ).forEach { (value, label) ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { onSetTheme(value) }
                        .padding(vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    RadioButton(
                        selected = state.prefs.theme == value,
                        onClick = { onSetTheme(value) },
                    )
                    Text(label, Modifier.padding(start = 8.dp))
                }
            }

            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            Spacer(Modifier.height(16.dp))

            Text("Legal", style = MaterialTheme.typography.titleSmall)
            Spacer(Modifier.height(4.dp))
            val legalContext = LocalContext.current
            TextButton(onClick = { Legal.openUrl(legalContext, Legal.PRIVACY_URL) }) {
                Text("Privacy policy")
            }
            TextButton(onClick = { Legal.openUrl(legalContext, Legal.TERMS_URL) }) {
                Text("Terms of use")
            }

            Spacer(Modifier.height(24.dp))
            Text(
                "Fingerprint ${state.identity.fingerprint.take(23)}",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }

    if (showAvatarEditor) {
        AvatarEditorDialog(
            onPreview = onPreviewAvatar,
            onDismiss = { showAvatarEditor = false },
            onSave = { json ->
                showAvatarEditor = false
                onSetAvatarConfig(json)
            },
        )
    }

    confirmRemoveNode?.let { node ->
        AlertDialog(
            onDismissRequest = { confirmRemoveNode = null },
            title = { Text("Remove ${node.displayName}?") },
            text = {
                Text(
                    "You will stop relaying through it and lose access to the " +
                        "rooms it hosts. Rooms stay in your list, so re-adding " +
                        "it later finds them again.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmRemoveNode = null
                    onRemoveSupernode(node.peerId)
                }) { Text("Remove", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = {
                TextButton(onClick = { confirmRemoveNode = null }) { Text("Cancel") }
            },
        )
    }

    if (confirmPurge) {
        AlertDialog(
            onDismissRequest = { confirmPurge = false },
            title = { Text("Delete all messages?") },
            text = {
                Text(
                    "Every message on this device is removed. Your peers keep " +
                        "their copies, and this cannot be undone.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmPurge = false
                    onPurgeHistory()
                }) { Text("Delete all", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = {
                TextButton(onClick = { confirmPurge = false }) { Text("Cancel") }
            },
        )
    }
}

/**
 * Edit the identicon.
 *
 * A handful of the core's knobs, not all of them: grid size and the three
 * colour switches are what visibly change the picture, and the rest
 * (island connectivity, hue stepping, background lightness) are refinements
 * that need a bigger screen to be worth exposing.
 *
 * The preview is rendered by the core through `avatar.svg`, so what is shown
 * is exactly what peers will draw.
 */
@Composable
private fun AvatarEditorDialog(
    onPreview: suspend (String) -> AvatarArt?,
    onDismiss: () -> Unit,
    onSave: (String) -> Unit,
) {
    var grid by remember { mutableStateOf(16f) }
    var shadeMode by remember { mutableStateOf(1f) }
    var dualHue by remember { mutableStateOf(false) }
    var islands by remember { mutableStateOf(true) }
    var preview by remember { mutableStateOf<AvatarArt?>(null) }

    // Grid must be even: the pattern is mirrored about the vertical centre.
    val configJson = remember(grid, shadeMode, dualHue, islands) {
        val evenGrid = (grid.toInt() / 2) * 2
        "{\"grid\":$evenGrid,\"shade_mode\":${shadeMode.toInt()}," +
            "\"dual_hue\":$dualHue,\"islands\":$islands}"
    }

    LaunchedEffect(configJson) { preview = onPreview(configJson) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Your avatar") },
        text = {
            Column {
                preview?.let { art ->
                    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                        Avatar(art, Modifier.size(96.dp))
                    }
                    Spacer(Modifier.height(16.dp))
                }

                Text("Detail", style = MaterialTheme.typography.labelMedium)
                Slider(
                    value = grid,
                    onValueChange = { grid = it },
                    valueRange = 8f..32f,
                    steps = 11,
                )

                Text("Shading", style = MaterialTheme.typography.labelMedium)
                Slider(
                    value = shadeMode,
                    onValueChange = { shadeMode = it },
                    valueRange = 1f..3f,
                    steps = 1,
                )

                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { dualHue = !dualHue },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Checkbox(checked = dualHue, onCheckedChange = { dualHue = it })
                    Text("Two colours")
                }
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { islands = !islands },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Checkbox(checked = islands, onCheckedChange = { islands = it })
                    Text("Colour each shape")
                }
            }
        },
        confirmButton = { TextButton(onClick = { onSave(configJson) }) { Text("Save") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** A labelled slider that shows the value it is about to set. */
@Composable
private fun TuningSlider(
    label: String,
    value: Float,
    range: ClosedFloatingPointRange<Float>,
    display: String,
    onChange: (Float) -> Unit,
    steps: Int = 0,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(label, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
        Text(
            display,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    Slider(
        value = value.coerceIn(range.start, range.endInclusive),
        onValueChange = onChange,
        valueRange = range,
        steps = steps,
    )
}

@Composable
private fun SettingSwitch(
    title: String,
    subtitle: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable { onCheckedChange(!checked) }
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            Text(
                subtitle,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(checked = checked, onCheckedChange = onCheckedChange)
    }
}

// ── Files ──────────────────────────────────────────────────────────────────

/**
 * Ask before downloading. Declining sends nothing back — the offer simply
 * goes unanswered, which is what the protocol does too.
 */
@Composable
private fun FileOfferDialog(
    offer: FileOffer,
    onAccept: () -> Unit,
    onReject: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onReject,
        title = { Text(if (offer.isRoom) "File shared in a room" else "Incoming file") },
        text = {
            Column {
                Text(offer.name, style = MaterialTheme.typography.bodyLarge)
                Spacer(Modifier.height(4.dp))
                Text(
                    formatSize(offer.size),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        confirmButton = { TextButton(onClick = onAccept) { Text("Accept") } },
        dismissButton = { TextButton(onClick = onReject) { Text("Decline") } },
    )
}

/**
 * Offer to copy a finished download somewhere the user can reach.
 *
 * The file is already saved in app storage; this is the copy out to their own
 * documents, so dismissing loses nothing but the shortcut.
 */
@Composable
private fun SaveFilePrompt(
    saved: SavedFile,
    onSave: (android.net.Uri) -> Unit,
    onDismiss: () -> Unit,
) {
    val createDocument = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("*/*"),
    ) { uri -> if (uri != null) onSave(uri) else onDismiss() }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("File received") },
        text = { Text("${saved.name} finished downloading. Save a copy?") },
        confirmButton = {
            TextButton(onClick = { createDocument.launch(saved.name) }) { Text("Save") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Not now") } },
    )
}

/** Byte counts the way the desktop's `format_byte_size` renders them. */
private fun formatSize(bytes: Long): String = when {
    bytes >= 1024L * 1024 * 1024 -> "%.1f GB".format(bytes / (1024.0 * 1024 * 1024))
    bytes >= 1024L * 1024 -> "%.1f MB".format(bytes / (1024.0 * 1024))
    bytes >= 1024L -> "%.1f KB".format(bytes / 1024.0)
    else -> "$bytes bytes"
}

// ── Dialogs ────────────────────────────────────────────────────────────────

@Composable
private fun InviteDialog(url: String, onDismiss: () -> Unit) {
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Invite created") },
        text = {
            Column {
                Text(
                    "Single use. Send it over a channel you already trust.",
                    style = MaterialTheme.typography.bodySmall,
                )
                Spacer(Modifier.height(12.dp))
                Text(url, style = MaterialTheme.typography.bodySmall)
            }
        },
        confirmButton = {
            TextButton(onClick = {
                clipboard.setText(androidx.compose.ui.text.AnnotatedString(url))
                onDismiss()
            }) { Text("Copy") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Close") } },
    )
}

@Composable
private fun AcceptInviteDialog(onDismiss: () -> Unit, onAccept: (String) -> Unit) {
    var url by remember { mutableStateOf("") }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Accept an invite") },
        text = {
            OutlinedTextField(
                value = url,
                onValueChange = { url = it },
                label = { Text("Invite link") },
                singleLine = false,
                maxLines = 4,
            )
        },
        confirmButton = {
            TextButton(onClick = { onAccept(url) }, enabled = url.isNotBlank()) { Text("Accept") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

// ── Small helpers ──────────────────────────────────────────────────────────

private fun formatTime(epochSeconds: Double): String =
    DateFormat.getTimeInstance(DateFormat.SHORT).format(Date((epochSeconds * 1000).toLong()))

// ── Jump to current ────────────────────────────────────────────────────────

/**
 * How far from the newest message counts as "away", in list items.
 *
 * Small deliberately. The button exists so that reading back never hides an
 * arriving message, and a threshold much larger than this lets several land
 * unseen before anything says so.
 */
private const val JUMP_TO_CURRENT_AFTER_ITEMS = 3

/**
 * True while [listState] is scrolled far enough from the newest message that
 * an arriving message would land unseen below the fold.
 *
 * `derivedStateOf` rather than a plain read: scroll position changes every
 * frame while a finger is down, and recomposing the whole chat screen on each
 * of those would cost far more than the one boolean anyone actually reads.
 */
@Composable
private fun rememberScrolledAwayFromLatest(listState: LazyListState): State<Boolean> =
    remember(listState) {
        derivedStateOf {
            val info = listState.layoutInfo
            val last = info.visibleItemsInfo.lastOrNull() ?: return@derivedStateOf false
            info.totalItemsCount - 1 - last.index >= JUMP_TO_CURRENT_AFTER_ITEMS
        }
    }

/**
 * Keeps [listState] sitting on the newest message, and reports whether the
 * reader has moved off it - the one boolean [JumpToCurrentButton] needs.
 *
 * Three separate things move a chat list, and each wants different handling:
 *
 *  - **History arriving.** The screen opens with an empty list and the store
 *    answers a moment later, so the first populated frame is the one that
 *    decides where the reader starts. It gets the instant `scrollToItem`,
 *    because a launched effect runs before that frame's measure pass: an
 *    animated scroll would size itself from the layout it can still see, which
 *    is the empty one, and settle back at the top of the history.
 *  - **A message arriving.** Followed only while the reader is on the end.
 *    Snatching the view down mid-sentence is the thing the jump button exists
 *    to prevent.
 *  - **The viewport shrinking**, which on a phone means the keyboard. A
 *    LazyColumn holds its *top* anchor across a resize, so without this the
 *    newest message slides below the fold by exactly the keyboard's height.
 *
 * Whether to follow is remembered rather than measured when it is needed,
 * because the resize changes the very geometry a measured answer would be read
 * from: once the keyboard is up, a list that was pinned to the end looks
 * identical to one the reader had deliberately scrolled away from.
 */
/** What a chat list needs to stay on its newest message. */
private class PinnedToLatest(
    /** True while the reader has moved off the newest message. */
    val scrolledAway: State<Boolean>,
    /**
     * Put the reader back on the newest message and resume following it.
     *
     * The jump button's action, and also what sending does: posting a message
     * is as plain a statement that you want to see the end of the conversation
     * as tapping the button is, wherever you had scrolled to before.
     */
    val jumpToLatest: () -> Unit,
)

@Composable
private fun rememberPinnedToLatest(
    listState: LazyListState,
    conversationKey: String,
    itemCount: Int,
): PinnedToLatest {
    val scrolledAway = rememberScrolledAwayFromLatest(listState)
    val scope = rememberCoroutineScope()

    // Both reset per conversation: a newly opened room or peer starts on its
    // own newest message, wherever the last one was left.
    var anchored by remember(conversationKey) { mutableStateOf(false) }
    var following by remember(conversationKey) { mutableStateOf(true) }

    // The two halves below each move `following` one way only, which is what
    // keeps a reading taken mid-resize from doing damage: the keyboard's inset
    // animates over many frames, and during those the list is genuinely
    // clipped, so anything that could disarm on geometry alone would disarm
    // exactly when the pinning is needed.

    // Coming to rest on the end re-arms - that covers the jump button and a
    // drag back down to the bottom without either having to say so, and a
    // stray re-arm only pins a list that wanted pinning anyway.
    LaunchedEffect(listState, conversationKey) {
        snapshotFlow { listState.isScrollInProgress }.collect { moving ->
            if (!moving && !scrolledAway.value) following = true
        }
    }

    // Only the reader's own gesture disarms. Programmatic scrolls raise no
    // drag interaction, so this cannot be tripped by our own pinning.
    LaunchedEffect(listState, conversationKey) {
        listState.interactionSource.interactions.collect { interaction ->
            if (interaction !is DragInteraction.Stop &&
                interaction !is DragInteraction.Cancel
            ) {
                return@collect
            }
            // A fling carries on well past the finger, so where the reader
            // meant to leave the list is only knowable once it comes to rest.
            snapshotFlow { listState.isScrollInProgress }.first { !it }
            if (scrolledAway.value) following = false
        }
    }

    LaunchedEffect(listState, conversationKey, itemCount) {
        if (itemCount == 0) return@LaunchedEffect
        if (!anchored) {
            listState.scrollToItem(itemCount - 1)
            anchored = true
        } else if (following) {
            // Wait for the arrival to actually be measured before animating to
            // it. This effect runs ahead of the frame's measure pass, so the
            // list is still the old one: already at the bottom of it, with the
            // new message not laid out yet. `animateScrollToItem` asks for a
            // distance, gets nothing back because there is nowhere left to
            // scroll, and takes that as its cue to give up - leaving the view
            // one message short of the end, which is exactly the scroll the
            // reader then has to do by hand.
            snapshotFlow { listState.layoutInfo.totalItemsCount }.first { it >= itemCount }
            listState.animateScrollToItem(itemCount - 1)
        }
    }

    // The keyboard, and anything else that resizes the list. `drop(1)` skips
    // the initial measurement, which is the anchoring effect's job, not a
    // resize. Instant rather than animated for every frame of it: the inset
    // animates over a good fraction of a second, and a scroll animation
    // racing that one would visibly lag behind the keyboard on the way up.
    LaunchedEffect(listState, conversationKey) {
        snapshotFlow { listState.layoutInfo.viewportSize.height }
            .drop(1)
            .collect {
                val last = listState.layoutInfo.totalItemsCount - 1
                if (following && last >= 0) listState.scrollToItem(last)
            }
    }

    return PinnedToLatest(
        // Mid-drag the measured answer is the honest one - the button should
        // show as soon as the end leaves the screen, not when the finger comes
        // up. Keyed on the conversation as well as the list: a new key hands
        // `following` a fresh state object, and a lambda remembered only
        // against `listState` would go on reading the outgoing one.
        scrolledAway = remember(listState, conversationKey) {
            derivedStateOf { scrolledAway.value || !following }
        },
        jumpToLatest = {
            following = true
            scope.launch {
                val last = listState.layoutInfo.totalItemsCount - 1
                if (last >= 0) listState.animateScrollToItem(last)
            }
        },
    )
}

/**
 * The affordance back to the newest message, shown over the bottom of a chat
 * list while the reader has scrolled away from it.
 *
 * Pairs with suppressing auto-scroll: a list that keeps snapping to the end
 * needs no such button, and one that stops snapping without offering it
 * strands the reader.
 */
@Composable
private fun BoxScope.JumpToCurrentButton(visible: Boolean, onClick: () -> Unit) {
    AnimatedVisibility(
        visible = visible,
        enter = fadeIn(),
        exit = fadeOut(),
        modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 12.dp),
    ) {
        FilledTonalButton(onClick = onClick) {
            Icon(Icons.Filled.KeyboardArrowDown, contentDescription = null)
            Spacer(Modifier.width(6.dp))
            Text("Jump to current")
        }
    }
}
