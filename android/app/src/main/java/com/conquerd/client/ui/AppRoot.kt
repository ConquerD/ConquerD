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
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.Phone
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.conquerd.client.AppViewModel
import com.conquerd.client.R
import com.conquerd.client.ChatMessage
import com.conquerd.client.AppState
import com.conquerd.client.CallPhase
import com.conquerd.client.CameraCapture
import com.conquerd.client.CallState
import com.conquerd.client.ConnectionMode
import com.conquerd.client.HomeTab
import com.conquerd.client.Room
import com.conquerd.client.RoomMessage
import com.conquerd.client.Peer
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
                    version = viewModel.coreVersion,
                    onUnlock = viewModel::unlock,
                )

                Screen.Home -> HomeScreen(viewModel = viewModel)

                is Screen.Chat -> ChatScreen(
                    peer = screen.peer,
                    messages = state.messages,
                    onBack = viewModel::closeChat,
                    onSend = viewModel::sendChat,
                    onCall = { viewModel.startCall(screen.peer) },
                )

                is Screen.RoomChat -> RoomChatScreen(
                    room = screen.room,
                    messages = state.roomMessages,
                    members = state.roomMembers,
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
                )
            }
        }
    }

    state.inviteUrl?.let { url ->
        InviteDialog(url = url, onDismiss = viewModel::dismissInvite)
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
    // binding afterwards would race that timeout.
    val requestCamera = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            CameraCapture.start(context)
            onToggleVideo(true)
        }
    }
    if (call.phase == CallPhase.INCOMING) {
        AlertDialog(
            onDismissRequest = { /* a ringing call needs an explicit answer */ },
            title = { Text("Incoming call") },
            text = { Text(call.peerLabel) },
            confirmButton = { TextButton(onClick = onAccept) { Text("Answer") } },
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
                            requestCamera.launch(Manifest.permission.CAMERA)
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
private fun UnlockScreen(busy: Boolean, version: String, onUnlock: (String) -> Unit) {
    var passphrase by remember { mutableStateOf("") }

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

        Text("ConquerD", style = MaterialTheme.typography.headlineLarge)
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
            keyboardActions = KeyboardActions(onGo = { onUnlock(passphrase) }),
            enabled = !busy,
            modifier = Modifier.fillMaxWidth(),
        )

        Spacer(Modifier.height(16.dp))

        Button(
            onClick = { onUnlock(passphrase) },
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
                // Decorative: the title beside it already names the screen.
                Image(
                    painter = painterResource(R.drawable.ic_logo),
                    contentDescription = null,
                    modifier = Modifier.padding(start = 12.dp).width(40.dp).height(18.dp),
                )
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
                )

                HomeTab.ROOMS -> RoomsList(
                    rooms = state.rooms,
                    showHidden = state.showHiddenRooms,
                    onOpenRoom = viewModel::openRoom,
                    onSetHidden = viewModel::setRoomHidden,
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
}

@Composable
private fun PeersList(
    state: AppState,
    onOpenPeer: (Peer) -> Unit,
    onCreateInvite: () -> Unit,
    onAcceptInvite: () -> Unit,
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
                onClick = { onOpenPeer(peer) },
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

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RoomsList(
    rooms: List<Room>,
    showHidden: Boolean,
    onOpenRoom: (Room) -> Unit,
    onSetHidden: (Room, Boolean) -> Unit,
) {
    // Hidden is per-profile local state, so the desktop's choices arrive with
    // the room list and are honoured here rather than re-derived.
    val visible = remember(rooms, showHidden) {
        rooms.filter { showHidden || !it.hidden }.sortedBy { it.roomName.lowercase() }
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
        items(visible, key = { it.key }) { room ->
            ListItem(
                headlineContent = { Text(room.roomName.ifBlank { room.roomId.take(12) }) },
                supportingContent = {
                    Text(
                        buildString {
                            append(room.roomType.ifBlank { "room" })
                            if (room.isCreator) append(" - yours")
                            if (room.spaceId.isNotBlank()) append(" - in a space")
                            if (room.hidden) append(" - hidden, long-press to restore")
                        },
                        style = MaterialTheme.typography.bodySmall,
                    )
                },
                // Long-press toggles. Purely local either way: the room stays
                // on the supernode and other members are unaffected.
                modifier = Modifier.combinedClickable(
                    onClick = { onOpenRoom(room) },
                    onLongClick = { onSetHidden(room, !room.hidden) },
                ),
            )
            HorizontalDivider()
        }
    }
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

@Composable
private fun PeerRow(peer: Peer, online: Boolean, onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(peer.label) },
        supportingContent = {
            Text(
                peer.peerId,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.bodySmall,
            )
        },
        leadingContent = {
            Box(
                Modifier
                    .size(10.dp)
                    .clip(CircleShape)
                    .background(
                        if (online) Color(0xFF16A34A) else MaterialTheme.colorScheme.outlineVariant,
                    ),
            )
        },
        modifier = Modifier.clickable(onClick = onClick),
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
) {
    var draft by remember { mutableStateOf("") }
    val listState = rememberLazyListState()

    // Ask at the point of use rather than on launch: a client that demands
    // the microphone before you have placed a call is asking for something it
    // cannot yet justify.
    val requestMic = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted -> if (granted) onCall() }

    // Follow the conversation as it grows, the way every chat app does.
    LaunchedEffect(messages.size) {
        if (messages.isNotEmpty()) listState.animateScrollToItem(messages.lastIndex)
    }

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
                IconButton(onClick = { requestMic.launch(Manifest.permission.RECORD_AUDIO) }) {
                    Icon(Icons.Filled.Phone, contentDescription = "Call")
                }
            },
        )

        LazyColumn(
            state = listState,
            modifier = Modifier.weight(1f).fillMaxWidth().padding(horizontal = 12.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            items(messages, key = { it.id }) { MessageBubble(it) }
        }

        Row(
            modifier = Modifier.fillMaxWidth().padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
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
                },
                enabled = draft.isNotBlank(),
            ) {
                Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send")
            }
        }
    }
}

@Composable
private fun MessageBubble(message: ChatMessage) {
    val alignment = if (message.isSelf) Alignment.End else Alignment.Start
    val container = if (message.isSelf) {
        MaterialTheme.colorScheme.primaryContainer
    } else {
        MaterialTheme.colorScheme.surfaceVariant
    }

    Column(Modifier.fillMaxWidth(), horizontalAlignment = alignment) {
        Card(colors = CardDefaults.cardColors(containerColor = container)) {
            Text(message.body, modifier = Modifier.padding(10.dp))
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
    members: List<String>,
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
) {
    var draft by remember { mutableStateOf("") }
    val listState = rememberLazyListState()
    val context = LocalContext.current

    val requestMic = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted -> if (granted) onJoinVoice() }

    // CameraX must be bound before the core asks for video — the native side
    // waits for a first frame to learn the capture size.
    val requestCamera = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            CameraCapture.start(context)
            onToggleVideo(true)
        }
    }

    LaunchedEffect(messages.size) {
        if (messages.isNotEmpty()) listState.animateScrollToItem(messages.lastIndex)
    }

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
                            "${members.size} " + if (members.size == 1) "member" else "members"
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
                IconButton(
                    enabled = joined,
                    onClick = {
                        if (voiceActive) {
                            onLeaveVoice()
                        } else {
                            requestMic.launch(Manifest.permission.RECORD_AUDIO)
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
                        requestCamera.launch(Manifest.permission.CAMERA)
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
            LazyColumn(
                state = listState,
                modifier = Modifier.weight(1f).fillMaxWidth().padding(horizontal = 12.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                items(messages, key = { it.messageId }) { RoomMessageBubble(it) }
            }
        }

        Row(
            modifier = Modifier.fillMaxWidth().padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
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
private fun RoomMessageBubble(message: RoomMessage) {
    val alignment = if (message.isSelf) Alignment.End else Alignment.Start
    val container = if (message.isSelf) {
        MaterialTheme.colorScheme.primaryContainer
    } else {
        MaterialTheme.colorScheme.surfaceVariant
    }

    Column(Modifier.fillMaxWidth(), horizontalAlignment = alignment) {
        // Unlike a 1:1 chat, a room has many senders, so each message has to
        // say who wrote it.
        if (!message.isSelf) {
            Text(
                message.senderHandle.ifBlank { message.senderId.take(10) },
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
                label = { Text("conquerd:// link") },
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
