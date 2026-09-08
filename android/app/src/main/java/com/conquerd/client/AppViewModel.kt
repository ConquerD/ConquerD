package com.conquerd.client

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.put

/** Where the user is in the app. */
sealed interface Screen {
    data object Unlock : Screen
    data object Home : Screen
    data class Chat(val peer: Peer) : Screen
    data class RoomChat(val room: Room) : Screen
}

/** Which list the home screen is showing. */
enum class HomeTab { PEERS, ROOMS }

/** Phase of a direct call. */
enum class CallPhase { INCOMING, OUTGOING, ACTIVE }

data class CallState(
    val peerId: String,
    val peerLabel: String,
    val phase: CallPhase,
    val muted: Boolean = false,
)

data class AppState(
    val screen: Screen = Screen.Unlock,
    val busy: Boolean = false,
    val error: String? = null,
    val notice: String? = null,
    val identity: IdentityInfo = IdentityInfo(),
    val peers: List<Peer> = emptyList(),
    val rooms: List<Room> = emptyList(),
    val messages: List<ChatMessage> = emptyList(),
    val connectionMode: ConnectionMode = ConnectionMode.OFFLINE,
    /** Peers with a live session, so the list can show who is reachable now. */
    val onlinePeers: Set<String> = emptySet(),
    val inviteUrl: String? = null,
    val tab: HomeTab = HomeTab.PEERS,
    /** Live messages for the room currently open. Not persisted anywhere. */
    val roomMessages: List<RoomMessage> = emptyList(),
    /** Participants in the open room, by peer id. */
    val roomMembers: List<String> = emptyList(),
    /** True once the supernode has admitted us to the open room. */
    val roomJoined: Boolean = false,
    /** The one direct call in progress, if any. */
    val call: CallState? = null,
    /** Show rooms the user hid. Off by default, matching the desktop sidebar. */
    val showHiddenRooms: Boolean = false,
    /** True while capturing and sending audio into the open room. */
    val roomVoiceActive: Boolean = false,
    /** Local mute, shared by direct calls and room voice. */
    val muted: Boolean = false,
    /** True while the local camera is capturing and sending. */
    val videoActive: Boolean = false,
)

class AppViewModel(app: Application) : AndroidViewModel(app) {

    private val core = ConquerdCore.get(app)

    private val _state = MutableStateFlow(AppState())
    val state: StateFlow<AppState> = _state.asStateFlow()

    /**
     * An invite link received while the core was still locked.
     *
     * Tapping a `conquerd://` link is a normal way to open the app for the
     * first time, so the link routinely arrives before there is anything to
     * hand it to. Held here and replayed once unlock succeeds.
     */
    private var pendingInvite: String? = null

    val coreVersion: String get() = core.version()

    init {
        viewModelScope.launch {
            core.events.collect(::onCoreEvent)
        }
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────

    fun unlock(passphrase: String) {
        if (_state.value.busy) return
        _state.update { it.copy(busy = true, error = null) }

        viewModelScope.launch {
            val result = core.start(passphrase)
            result.onFailure { e ->
                // The overwhelmingly common cause is a wrong passphrase, and
                // the Rust error text says so; surface it rather than a
                // generic "could not start".
                _state.update {
                    it.copy(busy = false, error = e.message ?: "could not start the core")
                }
                return@launch
            }

            CoreService.start(getApplication())
            _state.update { it.copy(busy = false, screen = Screen.Home) }
            refreshIdentity()
            refreshPeers()
            refreshRooms()

            pendingInvite?.let { url ->
                pendingInvite = null
                acceptInvite(url)
            }
        }
    }

    fun lock() {
        CoreService.stop(getApplication())
        core.stop()
        _state.value = AppState()
    }

    // ── Reads ─────────────────────────────────────────────────────────────

    private suspend fun refreshIdentity() {
        val reply = core.command("identity.info")
        if (!reply.ok) return
        _state.update {
            it.copy(
                identity = IdentityInfo(
                    publicId = reply.stringOrEmpty("public_id"),
                    peerId = reply.stringOrEmpty("peer_id"),
                    fingerprint = reply.stringOrEmpty("fingerprint"),
                ),
            )
        }
    }

    fun refreshPeers() = viewModelScope.launch {
        val reply = core.command("peer.list")
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }
        // Supernodes are infrastructure, not people — the desktop client keeps
        // them out of the contact list too.
        val peers = reply.decodeList<Peer>(core, "peers").filterNot { it.isSupernode }
        _state.update { it.copy(peers = peers) }
    }

    fun refreshRooms() = viewModelScope.launch {
        val reply = core.command("room.list")
        if (!reply.ok) return@launch
        _state.update { it.copy(rooms = reply.decodeList<Room>(core, "rooms")) }
    }

    // ── Chat ──────────────────────────────────────────────────────────────

    fun openChat(peer: Peer) {
        _state.update { it.copy(screen = Screen.Chat(peer), messages = emptyList()) }
        viewModelScope.launch {
            loadHistory(peer.peerId)
            core.command("chat.mark_read") { put("peer_id", peer.peerId) }
        }
    }

    fun closeChat() {
        _state.update { it.copy(screen = Screen.Home, messages = emptyList()) }
    }

    private suspend fun loadHistory(peerId: String) {
        val reply = core.command("chat.history") { put("peer_id", peerId) }
        if (!reply.ok) return
        _state.update { it.copy(messages = reply.decodeList<ChatMessage>(core, "messages")) }
    }

    fun sendChat(body: String) {
        val peer = (_state.value.screen as? Screen.Chat)?.peer ?: return
        val trimmed = body.trim()
        if (trimmed.isEmpty()) return

        viewModelScope.launch {
            val reply = core.command("chat.send") {
                put("peer_id", peer.peerId)
                put("body", trimmed)
            }
            // The core persists the message either way — as `sending` when it
            // went out, `failed` when it did not — so reloading history shows
            // the true state rather than an optimistic echo.
            loadHistory(peer.peerId)
            if (!reply.ok) _state.update { it.copy(error = reply.errorText) }
        }
    }

    // ── Invites ───────────────────────────────────────────────────────────

    fun generateInvite() = viewModelScope.launch {
        _state.update { it.copy(busy = true) }
        val reply = core.command("invite.generate")
        _state.update {
            if (reply.ok) {
                it.copy(busy = false, inviteUrl = reply.string("invite_url"))
            } else {
                it.copy(busy = false, error = reply.errorText)
            }
        }
    }

    fun dismissInvite() = _state.update { it.copy(inviteUrl = null) }

    // ── Calls ─────────────────────────────────────────────────────────────

    /**
     * Place a call.
     *
     * The caller must already hold RECORD_AUDIO; the UI asks for it before
     * getting here, because the foreground service can only claim the
     * microphone type once the permission is actually granted.
     */
    fun startCall(peer: Peer) {
        // Claim the microphone service type first. Doing it after capture
        // starts does not retroactively legalise it - Android just feeds
        // silence once the app is no longer foreground.
        CoreService.setMediaActive(getApplication(), microphone = true, camera = false)

        _state.update {
            it.copy(call = CallState(peer.peerId, peer.label, CallPhase.OUTGOING))
        }

        viewModelScope.launch {
            val reply = core.command("call.start") { put("peer_id", peer.peerId) }
            if (!reply.ok) {
                _state.update { it.copy(call = null, error = reply.errorText) }
                CoreService.setMediaActive(getApplication(), microphone = false, camera = false)
            }
        }
    }

    fun acceptCall() {
        val call = _state.value.call ?: return
        CoreService.setMediaActive(getApplication(), microphone = true, camera = false)
        _state.update { it.copy(call = call.copy(phase = CallPhase.ACTIVE)) }

        viewModelScope.launch {
            val reply = core.command("call.accept") { put("peer_id", call.peerId) }
            if (!reply.ok) {
                _state.update { it.copy(call = null, error = reply.errorText) }
                CoreService.setMediaActive(getApplication(), microphone = false, camera = false)
            }
        }
    }

    fun rejectCall() {
        val call = _state.value.call ?: return
        _state.update { it.copy(call = null) }
        viewModelScope.launch { core.command("call.reject") { put("peer_id", call.peerId) } }
    }

    fun endCall() {
        val call = _state.value.call ?: return
        _state.update { it.copy(call = null) }
        viewModelScope.launch {
            core.command("call.end") { put("peer_id", call.peerId) }
            CoreService.setMediaActive(getApplication(), microphone = false, camera = false)
        }
    }

    fun toggleMute() = setMuted(!_state.value.muted)

    // ── Rooms ─────────────────────────────────────────────────────────────

    fun selectTab(tab: HomeTab) = _state.update { it.copy(tab = tab) }

    fun toggleShowHiddenRooms() =
        _state.update { it.copy(showHiddenRooms = !it.showHiddenRooms) }

    /**
     * Show or hide a room in this device's list.
     *
     * Local only - the room stays on the supernode and other members are
     * unaffected. Hiding is per profile, so the desktop keeps its own view.
     */
    fun setRoomHidden(room: Room, hidden: Boolean) = viewModelScope.launch {
        val reply = core.command(if (hidden) "room.hide" else "room.unhide") {
            put("supernode_id", room.supernodeId)
            put("room_id", room.roomId)
        }
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }

        val name = room.roomName.ifBlank { "room" }
        _state.update {
            it.copy(
                notice = if (hidden) "Hid $name" else "Restored $name",
                // Keep revealed rooms on screen after un-hiding one, so a
                // sweep of several does not close the list out from under you.
                showHiddenRooms = it.showHiddenRooms,
            )
        }
        refreshRooms()
    }

    /**
     * Open a room: join it, then subscribe to its chat.
     *
     * Order matters — the supernode only forwards room chat to peers it has
     * admitted, so subscribing first would silently receive nothing.
     */
    fun openRoom(room: Room) {
        _state.update {
            it.copy(
                screen = Screen.RoomChat(room),
                roomMessages = emptyList(),
                roomMembers = emptyList(),
                roomJoined = false,
            )
        }

        viewModelScope.launch {
            // History first: it is local, so it paints immediately instead of
            // leaving the room blank until the supernode admits us.
            loadRoomHistory(room)

            val join = core.command("room.join") {
                put("supernode_id", room.supernodeId)
                put("room_id", room.roomId)
                // Empty means "public join"; the core picks the command.
                put("invite_token", room.inviteToken)
            }
            if (!join.ok) {
                _state.update { it.copy(error = join.errorText) }
                return@launch
            }
            core.command("room.chat.subscribe") {
                put("supernode_id", room.supernodeId)
                put("room_id", room.roomId)
            }
        }
    }

    /**
     * Load a room's stored history.
     *
     * Room chat *is* persisted — under the same `room:<supernode>:<room>`
     * conversation key the desktop writes — so a room opened on the phone
     * shows what was said on the desktop and vice versa.
     */
    private suspend fun loadRoomHistory(room: Room) {
        // Room id only: it is a hash over the creator's key and the room name,
        // so it identifies the room on whichever supernode is hosting it.
        val reply = core.command("room.history") { put("room_id", room.roomId) }
        if (!reply.ok) return

        val history = reply.decodeList<ChatMessage>(core, "messages").map {
            RoomMessage(
                messageId = it.id,
                senderId = it.peerId,
                senderHandle = it.senderHandle,
                body = it.body,
                timestamp = it.timestamp,
                isSelf = it.isSelf,
            )
        }
        _state.update { it.copy(roomMessages = history) }
    }

    fun closeRoom() {
        val room = (_state.value.screen as? Screen.RoomChat)?.room
        val wasInVoice = _state.value.roomVoiceActive
        _state.update {
            it.copy(
                screen = Screen.Home,
                roomMessages = emptyList(),
                roomMembers = emptyList(),
                roomJoined = false,
                roomVoiceActive = false,
            )
        }
        if (room == null) return

        viewModelScope.launch {
            // Stop capture first: leaving the room while still in room mode
            // would keep the microphone live for a room we are no longer in.
            if (wasInVoice) {
                core.command("room.voice.leave")
                CoreService.setMediaActive(getApplication(), microphone = false, camera = false)
            }
            core.command("room.chat.unsubscribe") {
                put("supernode_id", room.supernodeId)
                put("room_id", room.roomId)
            }
            core.command("room.leave") {
                put("supernode_id", room.supernodeId)
                put("room_id", room.roomId)
            }
        }
    }

    /**
     * Start sending voice into the open room.
     *
     * Joining a room's chat and joining its voice are separate: room mode
     * redirects outbound Opus through the supernode instead of to individual
     * peers, and has to be set before capture starts.
     */
    fun joinRoomVoice() {
        val room = (_state.value.screen as? Screen.RoomChat)?.room ?: return
        CoreService.setMediaActive(getApplication(), microphone = true, camera = false)

        viewModelScope.launch {
            val reply = core.command("room.voice.join") {
                put("supernode_id", room.supernodeId)
                put("room_id", room.roomId)
            }
            if (reply.ok) {
                _state.update { it.copy(roomVoiceActive = true, muted = false) }
            } else {
                _state.update { it.copy(error = reply.errorText) }
                CoreService.setMediaActive(getApplication(), microphone = false, camera = false)
            }
        }
    }

    fun leaveRoomVoice() {
        _state.update { it.copy(roomVoiceActive = false) }
        viewModelScope.launch {
            core.command("room.voice.leave")
            CoreService.setMediaActive(getApplication(), microphone = false, camera = false)
        }
    }

    /**
     * Start sending local video.
     *
     * The caller must already have bound CameraX — the native side waits a few
     * seconds for a first frame to learn the capture size and fails if none
     * arrives, so binding after this would race that timeout.
     *
     * `peerId` targets a direct call; `null` sends into the current room.
     */
    fun startVideo(peerId: String?) = viewModelScope.launch {
        CoreService.setMediaActive(
            getApplication(),
            microphone = _state.value.call != null || _state.value.roomVoiceActive,
            camera = true,
        )

        val reply = core.command("video.start") {
            if (peerId != null) put("peer_id", peerId)
        }
        if (reply.ok) {
            _state.update { it.copy(videoActive = true) }
        } else {
            _state.update { it.copy(error = reply.errorText) }
            CoreService.setMediaActive(
                getApplication(),
                microphone = _state.value.call != null || _state.value.roomVoiceActive,
                camera = false,
            )
        }
    }

    /** Stop local video. Idempotent; the core accepts "off" when already off. */
    fun stopVideo(peerId: String?) = viewModelScope.launch {
        _state.update { it.copy(videoActive = false) }
        core.command("video.stop") {
            if (peerId != null) put("peer_id", peerId)
        }
        CoreService.setMediaActive(
            getApplication(),
            microphone = _state.value.call != null || _state.value.roomVoiceActive,
            camera = false,
        )
    }

    /** Mute the microphone. Applies to a direct call or room voice alike. */
    fun setMuted(muted: Boolean) {
        _state.update { it.copy(muted = muted, call = it.call?.copy(muted = muted)) }
        viewModelScope.launch { core.command("audio.set_muted") { put("muted", muted) } }
    }

    fun sendRoomChat(body: String) {
        val room = (_state.value.screen as? Screen.RoomChat)?.room ?: return
        val trimmed = body.trim()
        if (trimmed.isEmpty()) return

        viewModelScope.launch {
            val reply = core.command("room.chat.send") {
                put("supernode_id", room.supernodeId)
                put("room_id", room.roomId)
                put("body", trimmed)
            }
            if (!reply.ok) _state.update { it.copy(error = reply.errorText) }
        }
    }

    /**
     * True when an event belongs to the room currently on screen.
     *
     * Matched on `room_id` alone, deliberately. Room frames ride whichever
     * multi-homed cluster session wins the race, so the `supernode_id` on the
     * event is usually a roster-learned sibling rather than the node the room
     * is listed against. Comparing it would drop every sibling-delivered
     * message — which looks exactly like "I can send but never receive".
     * Room ids are unique across the cluster; the core's own
     * `list_for_cluster_members` dedupes on `room_id` alone for the same
     * reason.
     */
    private fun isOpenRoom(event: JsonObject): Boolean {
        val room = (_state.value.screen as? Screen.RoomChat)?.room ?: return false
        return event.stringOrEmpty("room_id") == room.roomId
    }

    fun acceptInvite(url: String) = viewModelScope.launch {
        val trimmed = url.trim()
        if (trimmed.isEmpty()) return@launch

        if (!core.isRunning) {
            pendingInvite = trimmed
            _state.update { it.copy(notice = "Invite saved — unlock to accept it") }
            return@launch
        }

        val reply = core.command("invite.accept") { put("invite_url", trimmed) }
        if (!reply.ok) _state.update { it.copy(error = reply.errorText) }
    }

    fun dismissError() = _state.update { it.copy(error = null, notice = null) }

    // ── Events ────────────────────────────────────────────────────────────

    private fun onCoreEvent(event: JsonObject) {
        when (event.eventName()) {
            "peer_connected" -> {
                val id = event.stringOrEmpty("peer_id")
                _state.update {
                    it.copy(
                        onlinePeers = it.onlinePeers + id,
                        // Any live peer session means we are reachable; the
                        // relay/direct distinction refines it below.
                        connectionMode = maxOf(it.connectionMode, ConnectionMode.RELAY),
                    )
                }
            }

            "peer_disconnected" -> {
                val id = event.stringOrEmpty("peer_id")
                _state.update { it.copy(onlinePeers = it.onlinePeers - id) }
            }

            "session_state" -> {
                val direct = event.stringOrEmpty("chat_path") == "direct"
                _state.update {
                    it.copy(
                        connectionMode = if (direct) ConnectionMode.DIRECT else it.connectionMode,
                    )
                }
            }

            "supernode_connected" ->
                _state.update {
                    it.copy(connectionMode = maxOf(it.connectionMode, ConnectionMode.RELAY))
                }

            "supernode_disconnected" ->
                _state.update { it.copy(connectionMode = ConnectionMode.OFFLINE) }

            // Chat is already persisted by the core before this arrives, so
            // reloading is enough — there is no separate in-memory append that
            // could disagree with the store.
            "chat_message", "chat_ack", "chat_send_failed" -> {
                val peerId = event.stringOrEmpty("peer_id")
                val open = (_state.value.screen as? Screen.Chat)?.peer
                if (open != null && open.peerId == peerId) {
                    viewModelScope.launch {
                        loadHistory(peerId)
                        core.command("chat.mark_read") { put("peer_id", peerId) }
                    }
                }
            }

            "call_request" -> {
                val peerId = event.stringOrEmpty("peer_id")
                // Prefer the stored handle over the raw id - an incoming call
                // screen showing 44 characters of base64 tells nobody who is
                // calling.
                val label = _state.value.peers
                    .firstOrNull { it.peerId == peerId }?.label
                    ?: peerId.take(12)
                _state.update {
                    // A second inbound call while one is up is not a feature
                    // yet; keep the first rather than silently switching.
                    if (it.call != null) it
                    else it.copy(call = CallState(peerId, label, CallPhase.INCOMING))
                }
            }

            "call_accepted" -> {
                _state.update { s ->
                    s.call?.let { s.copy(call = it.copy(phase = CallPhase.ACTIVE)) } ?: s
                }
            }

            "call_ended" -> {
                // Stop the camera too: a call that ends with video still
                // running leaves the capture thread holding the device and the
                // camera indicator lit with nothing to send to.
                if (_state.value.videoActive) {
                    viewModelScope.launch { core.command("video.stop") }
                }
                _state.update { it.copy(call = null, videoActive = false) }
                CoreService.setMediaActive(getApplication(), microphone = false, camera = false)
            }

            // A capture that stopped on its own - the camera was revoked, or
            // CameraX unbound. The core has already released its side; the UI
            // has to stop claiming video is live.
            "video_ended" -> {
                _state.update {
                    if (!it.videoActive) it
                    else it.copy(
                        videoActive = false,
                        notice = "Camera stopped: ${event.stringOrEmpty("reason")}",
                    )
                }
                CameraCapture.stop()
                CoreService.setMediaActive(
                    getApplication(),
                    microphone = _state.value.call != null || _state.value.roomVoiceActive,
                    camera = false,
                )
            }

            "invite_accepted" -> {
                _state.update { it.copy(notice = "Peer added") }
                refreshPeers()
            }

            "invite_failed" ->
                _state.update { it.copy(error = event.string("reason") ?: "invite failed") }

            "handle_updated", "presence_updated" -> refreshPeers()

            "room_created", "room_invite_ready" -> refreshRooms()

            "room_chat_message" -> {
                if (!isOpenRoom(event)) return@onCoreEvent
                val sender = event.stringOrEmpty("sender_id")
                val message = RoomMessage(
                    messageId = event.stringOrEmpty("message_id"),
                    senderId = sender,
                    senderHandle = event.stringOrEmpty("sender_handle"),
                    body = event.stringOrEmpty("body"),
                    timestamp = event.number("timestamp"),
                    isSelf = sender.sameIdentityAs(_state.value.identity.publicId),
                )
                _state.update {
                    // The supernode can legitimately deliver a room frame more
                    // than once when we are attached to several cluster
                    // members, so fold on message id rather than appending.
                    if (it.roomMessages.any { existing -> existing.messageId == message.messageId }) {
                        it
                    } else {
                        it.copy(roomMessages = it.roomMessages + message)
                    }
                }
            }

            "room_members_changed" -> {
                if (!isOpenRoom(event)) return@onCoreEvent
                val members = event.stringList("members")
                // Membership arriving at all means the supernode admitted us.
                _state.update { it.copy(roomMembers = members, roomJoined = true) }
            }

            "room_join_rejected" -> {
                if (!isOpenRoom(event)) return@onCoreEvent
                _state.update {
                    it.copy(error = event.string("reason") ?: "the room refused the join")
                }
            }

            "room_failed_over" -> {
                // The cluster presents as one supernode, so a failover is a
                // move rather than a leave: re-point the open room at the
                // sibling that took it over instead of tearing the view down.
                val open = (_state.value.screen as? Screen.RoomChat)?.room ?: return@onCoreEvent
                if (event.stringOrEmpty("room_id") != open.roomId) return@onCoreEvent
                val moved = open.copy(supernodeId = event.stringOrEmpty("supernode_id"))
                _state.update {
                    it.copy(screen = Screen.RoomChat(moved), notice = "Room moved to another node")
                }
            }
        }
    }

    /**
     * Compare two identity ids ignoring base64 padding.
     *
     * Ids reach the client from two directions with different encodings — the
     * relay path uses URL-safe base64 *without* padding while SFU and
     * signaling use the padded form — so a byte comparison misses matches
     * intermittently depending on which path delivered the frame.
     */
    private fun String.sameIdentityAs(other: String): Boolean =
        trimEnd('=') == other.trimEnd('=')

    override fun onCleared() {
        super.onCleared()
        // Deliberately not stopping the core: the foreground service owns its
        // lifetime so a rotation or a backgrounded app does not drop sessions.
    }
}
