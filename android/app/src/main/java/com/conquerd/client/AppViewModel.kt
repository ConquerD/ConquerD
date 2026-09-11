package com.conquerd.client

import android.Manifest
import android.app.Application
import android.content.pm.PackageManager
import androidx.core.content.ContextCompat
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import com.conquerd.client.ui.AvatarArt
import com.conquerd.client.ui.parseAvatarSvg
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.put

/** Where the user is in the app. */
sealed interface Screen {
    data object Unlock : Screen
    /** Shown after unlock until [Legal.TERMS_VERSION] has been accepted. */
    data object Terms : Screen
    data object Home : Screen
    data class Chat(val peer: Peer) : Screen
    data class RoomChat(val room: Room) : Screen
    data object Settings : Screen
    /** A supernode's in-app portal, rendered in a WebView. */
    data class Portal(val supernodeId: String, val label: String) : Screen
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
    /** True while a stored key is being tried, so the prompt does not flash. */
    val autoUnlocking: Boolean = false,
    /** True when a key is stored and this launch skipped the passphrase. */
    val stayUnlocked: Boolean = false,
    val error: String? = null,
    val notice: String? = null,
    val identity: IdentityInfo = IdentityInfo(),
    val peers: List<Peer> = emptyList(),
    /** Known supernodes — the hosts a new room can be created on. */
    val supernodes: List<Peer> = emptyList(),
    val rooms: List<Room> = emptyList(),
    val messages: List<ChatMessage> = emptyList(),
    val connectionMode: ConnectionMode = ConnectionMode.OFFLINE,
    /**
     * Peers the list shows as online — the union of [directPeers] and
     * [relayPresentPeers], never assigned directly.
     */
    val onlinePeers: Set<String> = emptySet(),
    /** Peers with a live direct QUIC session. */
    val directPeers: Set<String> = emptySet(),
    /**
     * Peers whose relayed presence announce is still fresh.
     *
     * Separate from [directPeers] because a relay-only pair — both behind
     * CGNAT — never gets a direct session, and the dot has to light anyway.
     * Kept apart so losing one source does not clear a peer the other still
     * vouches for.
     */
    val relayPresentPeers: Set<String> = emptySet(),
    val inviteUrl: String? = null,
    val tab: HomeTab = HomeTab.PEERS,
    /** Live messages for the room currently open. Not persisted anywhere. */
    val roomMessages: List<RoomMessage> = emptyList(),
    /**
     * Voice participants in the open room, by peer id.
     *
     * The voice rail only. A text-only subscriber never appears here, so this
     * must not be used for a "members" count - see [roomChatMembers].
     */
    val roomMembers: List<String> = emptyList(),
    /**
     * Everyone in the open room: voice participants plus text subscribers.
     *
     * This is the supernode's `chat_members` (its full key-group roster) and
     * is what the desktop's text member panel shows. The room header counts
     * this, not [roomMembers], or a peer reading the room without joining
     * voice is invisible in the count.
     */
    val roomChatMembers: List<String> = emptyList(),
    /** True once the supernode has admitted us to the open room. */
    val roomJoined: Boolean = false,
    /**
     * Voice rosters for every room we hear about, keyed `"supernodeId:roomId"`.
     *
     * Kept per node and unioned for display: a cluster hosts one logical room
     * on several members, so each node knows only the peers attached to it.
     */
    val roomVoiceRosters: Map<String, List<String>> = emptyMap(),
    /** Chat rosters (participants + text subscribers), keyed the same way. */
    val roomTextRosters: Map<String, List<String>> = emptyMap(),
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
    /** An inbound file offer waiting on accept or decline. */
    val fileOffer: FileOffer? = null,
    /** Live transfers by id, 0.0-1.0, for the progress line. */
    val transfers: Map<String, Float> = emptyMap(),
    /** The most recent completed download, offered for saving out. */
    val savedFile: SavedFile? = null,
    /** Device-local preferences. */
    val prefs: Prefs = Prefs(),
    /** Parsed identicons by peer id. Built once per peer, then reused. */
    val avatars: Map<String, AvatarArt> = emptyMap(),
    /** Known supernodes, for the management screen. */
    val supernodeInfo: List<SupernodeInfo> = emptyList(),
)

/**
 * Update one presence source and recompute [AppState.onlinePeers] from all of
 * them.
 *
 * The union is derived here rather than assigned at each call site so the dot
 * cannot drift from its sources — the bug this replaced was a direct
 * disconnect clearing a peer that the relay still reported as up.
 */
internal fun AppState.withPresence(
    direct: Set<String> = directPeers,
    relay: Set<String> = relayPresentPeers,
    connectionMode: ConnectionMode = this.connectionMode,
): AppState = copy(
    directPeers = direct,
    relayPresentPeers = relay,
    onlinePeers = direct + relay,
    connectionMode = connectionMode,
)

/**
 * Cluster-wide headcount for one room across every node that reported it.
 *
 * Keys are `"supernodeId:roomId"`; the supernode id is base64url and carries
 * no `':'`, so everything after the first one is the room id. A peer that is
 * multi-homed appears in two nodes' rosters, so this unions ids rather than
 * summing counts - summing would report two people where there is one.
 */
internal fun Map<String, List<String>>.roomHeadcount(roomId: String): Int =
    entries.asSequence()
        .filter { it.key.substringAfter(':', "") == roomId }
        .flatMap { it.value.asSequence() }
        .toSet()
        .size

/** An inbound offer: nothing arrives until it is accepted. */
data class FileOffer(
    val transferId: String,
    val peerId: String,
    val name: String,
    val size: Long,
    /**
     * True for a room offer, which is an advertisement rather than a push.
     *
     * Accepting one asks the originator to start sending; accepting a 1:1
     * offer just lets bytes already on their way through. Different commands,
     * so the difference has to survive into the dialog.
     */
    val isRoom: Boolean = false,
)

/** A finished download sitting in app storage. */
data class SavedFile(val name: String, val path: String)

/** The device-local preferences, mirrored into state so the UI recomposes. */
data class Prefs(
    val frontCamera: Boolean = true,
    val voiceActivation: Boolean = true,
    val theme: String = AppSettings.THEME_SYSTEM,
    val inputGain: Int = 100,
    val outputGain: Int = 100,
    val noiseStrength: Int = 2,
    val voiceBitrate: Int = 32_000,
)


class AppViewModel(app: Application) : AndroidViewModel(app) {

    private val core = ConquerdCore.get(app)

    /** Optional Keystore-backed "stay unlocked" storage. Empty until opted in. */
    private val vault = IdentityVault(app)

    /** Device-local preferences; the display name lives on the peer record. */
    private val settings = AppSettings(app)

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
        viewModelScope.launch {
            core.stopped.collect {
                // A reconnect can land between emit and collect; don't wipe a
                // session that is already running again.
                if (!core.isRunning) resetUiToLocked()
            }
        }
        viewModelScope.launch {
            IncomingCallNotifier.cleared.collect { peerId ->
                _state.update { s ->
                    if (s.call?.peerId == peerId && s.call.phase == CallPhase.INCOMING) {
                        s.copy(call = null)
                    } else {
                        s
                    }
                }
            }
        }
        _state.update { it.copy(prefs = prefsSnapshot()) }
        attemptAutoUnlock()
    }

    // ── Settings ──────────────────────────────────────────────────────────

    fun openSettings() {
        _state.update { it.copy(screen = Screen.Settings) }
        refreshSupernodes()
    }

    fun closeSettings() = _state.update { it.copy(screen = Screen.Home) }

    /** Open a supernode's portal. */
    fun openPortal(node: SupernodeInfo) = _state.update {
        it.copy(screen = Screen.Portal(node.peerId, node.displayName))
    }

    /**
     * Leave the portal, closing any game channel it opened.
     *
     * The page cannot be relied on to close it: a WebView torn down mid-frame
     * never runs its unload handler, and the supernode would keep the lobby
     * slot until it timed out.
     */
    fun closePortal() {
        val portal = _state.value.screen as? Screen.Portal
        _state.update { it.copy(screen = Screen.Settings) }
        portal?.let { open ->
            viewModelScope.launch {
                core.command("portal.close") { put("supernode_id", open.supernodeId) }
            }
        }
    }

    /** The core handle the portal bridge issues its commands through. */
    fun portalCore(): ConquerdCore = core

    /**
     * Set the name peers see, and tell them.
     *
     * Peers cache the handle, so a rename that is not announced leaves
     * everyone else showing the old one — the core broadcasts for us.
     */
    fun setHandle(handle: String) = viewModelScope.launch {
        val reply = core.command("identity.set_handle") { put("handle", handle.trim()) }
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }
        refreshIdentity()
        _state.update { it.copy(notice = "Name updated.") }
    }

    fun setFrontCamera(front: Boolean) {
        settings.frontCamera = front
        _state.update { it.copy(prefs = it.prefs.copy(frontCamera = front)) }
    }

    fun setVoiceActivation(enabled: Boolean) {
        settings.voiceActivation = enabled
        _state.update { it.copy(prefs = it.prefs.copy(voiceActivation = enabled)) }
    }

    /**
     * Push the stored audio tuning at the core.
     *
     * Called on every change and again when a call starts: the call controller
     * does not persist anything, so a setting made between calls would
     * otherwise be forgotten by the next one.
     */
    fun applyAudioTuning() = viewModelScope.launch {
        core.command("audio.tune") {
            put("input_gain", settings.inputGain)
            put("output_gain", settings.outputGain)
            put("noise_strength", settings.noiseStrength)
            put("noise_suppression", settings.noiseStrength > 0)
            put("bitrate_bps", settings.voiceBitrate)
            put("voice_activation", settings.voiceActivation)
        }
    }

    fun setInputGain(value: Int) {
        settings.inputGain = value
        _state.update { it.copy(prefs = it.prefs.copy(inputGain = settings.inputGain)) }
        applyAudioTuning()
    }

    fun setOutputGain(value: Int) {
        settings.outputGain = value
        _state.update { it.copy(prefs = it.prefs.copy(outputGain = settings.outputGain)) }
        applyAudioTuning()
    }

    fun setNoiseStrength(value: Int) {
        settings.noiseStrength = value
        _state.update { it.copy(prefs = it.prefs.copy(noiseStrength = settings.noiseStrength)) }
        applyAudioTuning()
    }

    fun setVoiceBitrate(value: Int) {
        settings.voiceBitrate = value
        _state.update { it.copy(prefs = it.prefs.copy(voiceBitrate = settings.voiceBitrate)) }
        applyAudioTuning()
    }

    // ── Supernodes ───────────────────────────────────────────────

    fun refreshSupernodes() = viewModelScope.launch {
        val reply = core.command("supernode.list")
        if (!reply.ok) return@launch
        _state.update {
            it.copy(supernodeInfo = reply.decodeList<SupernodeInfo>(core, "supernodes"))
        }
    }

    /**
     * Forget a supernode.
     *
     * Rooms hosted there stay in the store, so re-adding it later finds them
     * again — the same choice the desktop makes.
     */
    fun removeSupernode(nodeId: String) = viewModelScope.launch {
        val reply = core.command("supernode.remove") { put("node_id", nodeId) }
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }
        _state.update { it.copy(notice = "Supernode removed.") }
        refreshSupernodes()
        refreshPeers()
    }

    fun setTheme(theme: String) {
        settings.theme = theme
        _state.update { it.copy(prefs = it.prefs.copy(theme = theme)) }
    }

    /**
     * Open the identity with a stored key, when the user has asked for that.
     *
     * A stale key is not an error worth showing: the identity may have been
     * replaced, or the Keystore entry dropped. Either way the stored key is
     * forgotten and the passphrase screen appears as though it had never been
     * set - which is also what happens on a device where nothing is stored.
     */
    private fun attemptAutoUnlock() {
        val stored = vault.load() ?: return
        _state.update { it.copy(autoUnlocking = true, busy = true) }

        viewModelScope.launch {
            val result = core.start(passphrase = "", storedKey = stored)
            if (result.isFailure) {
                vault.clear()
                _state.update {
                    it.copy(autoUnlocking = false, busy = false, stayUnlocked = false)
                }
                return@launch
            }
            _state.update { it.copy(autoUnlocking = false, stayUnlocked = true) }
            onCoreStarted()
        }
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /**
     * Unlock with a typed passphrase.
     *
     * [stayUnlocked] is the user's explicit choice on the unlock screen. False
     * also *clears* a key stored earlier, so unticking the box is a way to turn
     * the feature off rather than only declining to renew it.
     */
    fun unlock(
        passphrase: String,
        stayUnlocked: Boolean = false,
        keyfile: android.net.Uri? = null,
    ) {
        if (_state.value.busy) return
        _state.update { it.copy(busy = true, error = null) }

        viewModelScope.launch {
            // The core reads the keyfile itself, repeatedly, so it needs a
            // sandbox path rather than the picker's uri - same constraint as
            // sending a file.
            val stagedKeyfile = keyfile?.let { uri ->
                withContext(Dispatchers.IO) { FileStaging.stageForSend(getApplication(), uri) }
            }
            if (keyfile != null && stagedKeyfile == null) {
                _state.update { it.copy(busy = false, error = "Could not read that keyfile.") }
                return@launch
            }

            val result = core.start(passphrase, stagedKeyfile?.path.orEmpty())
            result.onFailure { e ->
                // The overwhelmingly common cause is a wrong passphrase, and
                // the Rust error text says so; surface it rather than a
                // generic "could not start".
                _state.update {
                    it.copy(busy = false, error = e.message ?: "could not start the core")
                }
                return@launch
            }

            val remembered = if (stayUnlocked) rememberIdentityKey() else {
                vault.clear()
                false
            }
            _state.update { it.copy(stayUnlocked = remembered) }
            onCoreStarted()
        }
    }

    /**
     * Ask the core for the identity file key and seal it in the Keystore.
     *
     * Returns whether it will actually survive the next launch, so the UI can
     * say "you will still be asked" instead of quietly promising otherwise.
     */
    private suspend fun rememberIdentityKey(): Boolean {
        val reply = core.command("identity.export_key")
        val key = reply.stringOrEmpty("key")
        if (!reply.ok || key.isBlank()) {
            _state.update { it.copy(notice = "Could not stay unlocked — you will be asked again next time.") }
            return false
        }
        if (!vault.store(key)) {
            _state.update { it.copy(notice = "This device would not store the key — you will be asked again next time.") }
            return false
        }
        return true
    }

    /** Shared tail of both unlock paths. */
    private suspend fun onCoreStarted() {
        CoreService.start(getApplication())
        applyAudioTuning()
        val next = if (settings.acceptedTermsVersion >= Legal.TERMS_VERSION) {
            Screen.Home
        } else {
            Screen.Terms
        }
        _state.update { it.copy(busy = false, screen = next) }
        refreshIdentity()
        refreshPeers()
        refreshRooms()

        // Invites wait until terms are accepted so a first-run tap cannot
        // land in a room before the policy gate.
        if (next == Screen.Home) {
            pendingInvite?.let { url ->
                pendingInvite = null
                acceptInvite(url)
            }
        }
    }

    fun acceptTerms() {
        settings.acceptedTermsVersion = Legal.TERMS_VERSION
        _state.update { it.copy(screen = Screen.Home) }
        pendingInvite?.let { url ->
            pendingInvite = null
            acceptInvite(url)
        }
    }

    fun declineTerms() = lock()

    fun lock() {
        CoreService.stop(getApplication())
        core.stop()
        resetUiToLocked()
    }

    /** Device-local prefs as they stand now, not the defaults on a fresh [AppState]. */
    private fun prefsSnapshot(): Prefs = Prefs(
        frontCamera = settings.frontCamera,
        voiceActivation = settings.voiceActivation,
        theme = settings.theme,
        inputGain = settings.inputGain,
        outputGain = settings.outputGain,
        noiseStrength = settings.noiseStrength,
        voiceBitrate = settings.voiceBitrate,
    )

    /**
     * Return to the unlock screen after the core has stopped.
     *
     * Called from [lock] and from the notification Disconnect / FGS-timeout
     * path, which stop the core without going through the ViewModel. Theme
     * and audio prefs live in [AppSettings], so they have to be copied back
     * onto a fresh [AppState] or a disconnect would also reset the theme.
     */
    private fun resetUiToLocked() {
        IncomingCallNotifier.cancel(getApplication())
        CameraCapture.stop()
        _state.value = AppState(prefs = prefsSnapshot())
    }

    /**
     * Lock and forget the stored key, so the next launch asks again.
     *
     * The thing to reach for when handing the phone to someone else. Mirrors
     * the desktop's "Lock Identity & Quit". Forgetting the key is part of
     * locking on purpose: a lock that a relaunch undoes is not a lock.
     */
    fun lockAndForget() {
        vault.clear()
        lock()
    }

    /**
     * Turn staying unlocked on or off while the core is already running.
     *
     * Turning it on works at any time because the core can re-export the file
     * key on demand - the user does not have to lock and retype a passphrase
     * just to change their mind.
     */
    fun setStayUnlocked(enabled: Boolean) {
        if (!enabled) {
            vault.clear()
            _state.update { it.copy(stayUnlocked = false, notice = "Stored key forgotten.") }
            return
        }

        viewModelScope.launch {
            val remembered = rememberIdentityKey()
            _state.update {
                if (remembered) {
                    it.copy(stayUnlocked = true, notice = "This device will stay unlocked.")
                } else {
                    it.copy(stayUnlocked = false)
                }
            }
        }
    }

    // ── Reads ─────────────────────────────────────────────────────────────

    /** Our own peer id, once the core has reported it. */
    private fun identityPeerId(): String = _state.value.identity.peerId

    private suspend fun refreshIdentity() {
        val reply = core.command("identity.info")
        if (!reply.ok) return
        _state.update {
            it.copy(
                identity = IdentityInfo(
                    publicId = reply.stringOrEmpty("public_id"),
                    peerId = reply.stringOrEmpty("peer_id"),
                    fingerprint = reply.stringOrEmpty("fingerprint"),
                    handle = reply.stringOrEmpty("handle"),
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
        val all = reply.decodeList<Peer>(core, "peers")
        _state.update {
            it.copy(
                peers = all.filterNot { p -> p.isSupernode },
                // Kept rather than discarded: creating a room needs a host to
                // create it on, and this is the only list of them we have.
                supernodes = all.filter { p -> p.isSupernode },
            )
        }
        refreshAvatars(all.filterNot { it.isSupernode }.map { it.peerId } + identityPeerId())
    }

    /**
     * Forget a peer: drop the record and any call in progress with them.
     *
     * Local only, and not a revocation - they keep whatever they already have,
     * and an old invite of theirs would let them back in. It is the same
     * operation as the desktop's "Remove Peer".
     */
    fun removePeer(peerId: String) = viewModelScope.launch {
        val reply = core.command("peer.remove") { put("peer_id", peerId) }
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }
        _state.update { it.copy(notice = "Peer removed.") }
        refreshPeers()
    }

    /**
     * Delete one message from local history.
     *
     * Local only - the peer keeps their copy. The desktop behaves the same
     * way; there is no "delete for everyone" in the protocol.
     */
    fun deleteMessage(messageId: String) = viewModelScope.launch {
        val reply = core.command("chat.delete") { put("message_id", messageId) }
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }
        _state.update { it.copy(messages = it.messages.filterNot { m -> m.id == messageId }) }
    }

    /** Re-send a failed message, keeping its id so the peer can deduplicate. */
    fun retryMessage(messageId: String) = viewModelScope.launch {
        val reply = core.command("chat.retry") { put("message_id", messageId) }
        if (!reply.ok) {
            _state.update {
                it.copy(error = reply.errorText ?: "still could not send")
            }
        }
        (_state.value.screen as? Screen.Chat)?.let { loadHistory(it.peer.peerId) }
    }

    // ── History ─────────────────────────────────────────────────────

    /**
     * Delete every message on this device.
     *
     * Local only: peers keep their copies, and the protocol has no delete for
     * everyone. Worth saying plainly in the UI, because "delete all messages"
     * reads like it reaches further than it does.
     */
    fun purgeChatHistory() = viewModelScope.launch {
        val reply = core.command("chat.purge_all")
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }
        _state.update {
            it.copy(
                messages = emptyList(),
                notice = "Deleted ${reply.number("removed").toInt()} messages.",
            )
        }
    }

    /** Drop messages older than [days], keeping the rest. */
    fun trimChatHistory(days: Int) = viewModelScope.launch {
        val reply = core.command("chat.trim") { put("days", days) }
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }
        _state.update {
            it.copy(notice = "Removed ${reply.number("removed").toInt()} older messages.")
        }
    }

    // ── Avatar ─────────────────────────────────────────────────────

    /** Render an avatar for a config that has not been saved yet. */
    suspend fun previewAvatar(configJson: String): AvatarArt? {
        val reply = core.command("avatar.svg") {
            put("peer_id", _state.value.identity.peerId)
            put("config", configJson)
        }
        return if (reply.ok) parseAvatarSvg(reply.stringOrEmpty("svg")) else null
    }

    /**
     * Save our avatar config and tell peers.
     *
     * Peers cache the config, so an unannounced change leaves them rendering
     * the old avatar - the core broadcasts for us.
     */
    fun setAvatarConfig(configJson: String) = viewModelScope.launch {
        val reply = core.command("avatar.set_config") { put("config", configJson) }
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }
        // Drop the cached art for ourselves so the next read re-renders it.
        _state.update {
            it.copy(avatars = it.avatars - it.identity.peerId, notice = "Avatar updated.")
        }
        refreshAvatars(listOf(_state.value.identity.peerId))
    }

    // ── Files ─────────────────────────────────────────────────────────────

    /**
     * Offer a file to the peer whose chat is open.
     *
     * Staging copies the picked document into the sandbox first — see
     * [FileStaging] for why a `content://` uri cannot be handed to the core.
     */
    fun sendFile(uri: android.net.Uri) {
        val peer = (_state.value.screen as? Screen.Chat)?.peer ?: return

        viewModelScope.launch {
            val staged = withContext(Dispatchers.IO) {
                FileStaging.stageForSend(getApplication(), uri)
            }
            if (staged == null) {
                _state.update { it.copy(error = "Could not read that file.") }
                return@launch
            }

            val reply = core.command("file.send") {
                put("peer_id", peer.peerId)
                put("path", staged.path)
                put("rel_path", staged.displayName)
            }
            if (!reply.ok) {
                _state.update { it.copy(error = reply.errorText) }
                return@launch
            }
            loadHistory(peer.peerId)
        }
    }

    /**
     * Advertise a file to the room that is open.
     *
     * Nothing is sent yet: room files are advertised and pulled, so members
     * decide individually whether to spend the bandwidth.
     */
    fun sendRoomFile(uri: android.net.Uri) {
        val room = (_state.value.screen as? Screen.RoomChat)?.room ?: return

        viewModelScope.launch {
            val staged = withContext(Dispatchers.IO) {
                FileStaging.stageForSend(getApplication(), uri)
            }
            if (staged == null) {
                _state.update { it.copy(error = "Could not read that file.") }
                return@launch
            }

            val reply = core.command("file.send_room") {
                put("supernode_id", room.supernodeId)
                put("room_id", room.roomId)
                put("path", staged.path)
                put("rel_path", staged.displayName)
            }
            if (!reply.ok) {
                _state.update { it.copy(error = reply.errorText) }
                return@launch
            }
            _state.update { it.copy(notice = "Shared ${staged.displayName} with the room.") }
        }
    }

    /** Build a shareable link for the open room. */
    fun generateRoomInvite() {
        val room = (_state.value.screen as? Screen.RoomChat)?.room ?: return

        viewModelScope.launch {
            val reply = core.command("room.invite") {
                put("supernode_id", room.supernodeId)
                put("room_id", room.roomId)
            }
            if (!reply.ok) {
                _state.update { it.copy(error = reply.errorText) }
                return@launch
            }
            _state.update { it.copy(inviteUrl = reply.stringOrEmpty("invite_url")) }
        }
    }

    /** Accept the pending offer, which is what actually starts the download. */
    fun acceptFileOffer() = respondToOffer(accept = true)

    /** Decline the pending offer. Nothing is sent; the sender simply waits. */
    fun rejectFileOffer() = respondToOffer(accept = false)

    private fun respondToOffer(accept: Boolean) {
        val offer = _state.value.fileOffer ?: return
        _state.update { it.copy(fileOffer = null) }

        val command = when {
            offer.isRoom && accept -> "file.accept_room"
            offer.isRoom -> "file.decline_room"
            accept -> "file.accept"
            else -> "file.reject"
        }

        viewModelScope.launch {
            val reply = core.command(command) { put("transfer_id", offer.transferId) }
            if (!reply.ok) _state.update { it.copy(error = reply.errorText) }
        }
    }

    /** Copy the last completed download to wherever the user picked. */
    fun exportSavedFile(destination: android.net.Uri) {
        val saved = _state.value.savedFile ?: return
        viewModelScope.launch {
            val ok = withContext(Dispatchers.IO) {
                FileStaging.exportTo(getApplication(), saved.path, destination)
            }
            _state.update {
                if (ok) {
                    it.copy(notice = "Saved ${saved.name}.", savedFile = null)
                } else {
                    it.copy(error = "Could not save ${saved.name}.")
                }
            }
        }
    }

    fun dismissSavedFile() = _state.update { it.copy(savedFile = null) }

    /** Block or unblock a peer, mirroring the desktop's context menu. */
    fun setPeerBlocked(peerId: String, blocked: Boolean) = viewModelScope.launch {
        val reply = core.command(if (blocked) "peer.block" else "peer.unblock") {
            put("peer_id", peerId)
        }
        if (!reply.ok) {
            _state.update { it.copy(error = reply.errorText) }
            return@launch
        }
        _state.update { it.copy(notice = if (blocked) "Peer blocked." else "Peer unblocked.") }
        refreshPeers()
    }

    /**
     * Ask a supernode to host a new room.
     *
     * Nothing is stored locally here. The room is persisted when the supernode
     * answers with `RoomCreated`, which also adopts it into the Space tree —
     * so a create that never lands leaves no phantom room in the list.
     */
    fun createRoom(
        supernodeId: String,
        name: String,
        isPrivate: Boolean,
        parentRoomId: String = "",
    ) = viewModelScope.launch {
            val trimmed = name.trim()
            if (trimmed.isEmpty()) return@launch

            val reply = core.command("room.create") {
                put("supernode_id", supernodeId)
                put("room_name", trimmed)
                put("room_type", if (isPrivate) "private" else "public")
                if (parentRoomId.isNotBlank()) put("parent_room_id", parentRoomId)
            }
            if (!reply.ok) {
                _state.update { it.copy(error = reply.errorText) }
                return@launch
            }
            _state.update { it.copy(notice = "Creating \"$trimmed\"...") }
        }

    /**
     * Fetch identicons for peers we do not have one for yet.
     *
     * An avatar is a pure function of the peer's identity and their advertised
     * config, so it is fetched once and kept. Missing ones are fetched rather
     * than the whole set re-requested, because the common case after a refresh
     * is that nothing changed.
     */
    private fun refreshAvatars(ids: List<String>) = viewModelScope.launch {
        val have = _state.value.avatars
        val wanted = ids.filter { it.isNotBlank() && it !in have }
        if (wanted.isEmpty()) return@launch

        val fetched = mutableMapOf<String, AvatarArt>()
        wanted.forEach { id ->
            val reply = core.command("avatar.svg") { put("peer_id", id) }
            if (!reply.ok) return@forEach
            parseAvatarSvg(reply.stringOrEmpty("svg"))?.let { fetched[id] = it }
        }
        if (fetched.isNotEmpty()) {
            _state.update { it.copy(avatars = it.avatars + fetched) }
        }
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

    /**
     * Answer, decline, or surface a ringing call from the incoming-call
     * notification / full-screen intent.
     *
     * The ViewModel may have been recreated (activity was gone while the
     * service held the session), so the extras re-seed [CallState] first.
     */
    fun handleIncomingCallIntent(intent: android.content.Intent?) {
        val action = intent?.action ?: return
        if (action != IncomingCallNotifier.ACTION_SHOW &&
            action != IncomingCallNotifier.ACTION_ANSWER &&
            action != IncomingCallNotifier.ACTION_DECLINE
        ) {
            return
        }
        val peerId = intent.getStringExtra(IncomingCallNotifier.EXTRA_PEER_ID) ?: return
        val label = intent.getStringExtra(IncomingCallNotifier.EXTRA_PEER_LABEL)
            ?: peerId.take(12)

        if (_state.value.call == null) {
            _state.update {
                it.copy(call = CallState(peerId, label, CallPhase.INCOMING))
            }
        }

        when (action) {
            IncomingCallNotifier.ACTION_ANSWER -> {
                IncomingCallNotifier.cancel(getApplication())
                if (hasMicrophonePermission()) acceptCall()
                // Otherwise leave the overlay up so the mic disclosure can run.
            }
            IncomingCallNotifier.ACTION_DECLINE -> rejectCall()
            IncomingCallNotifier.ACTION_SHOW -> { /* overlay is enough */ }
        }
    }

    private fun hasMicrophonePermission(): Boolean =
        ContextCompat.checkSelfPermission(
            getApplication(),
            Manifest.permission.RECORD_AUDIO,
        ) == PackageManager.PERMISSION_GRANTED

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
            val reply = core.command("call.start") {
                put("peer_id", peer.peerId)
                put("voice_activation", settings.voiceActivation)
            }
            if (!reply.ok) {
                _state.update { it.copy(call = null, error = reply.errorText) }
                CoreService.setMediaActive(getApplication(), microphone = false, camera = false)
            }
        }
    }

    fun acceptCall() {
        val call = _state.value.call ?: return
        IncomingCallNotifier.cancel(getApplication())
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
        IncomingCallNotifier.cancel(getApplication())
        _state.update { it.copy(call = null) }
        viewModelScope.launch { core.command("call.reject") { put("peer_id", call.peerId) } }
    }

    fun endCall() {
        val call = _state.value.call ?: return
        IncomingCallNotifier.cancel(getApplication())
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
                roomChatMembers = emptyList(),
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
                // `sender`, not `peerId`: for a room message the conversation
                // key is the room itself, so reading the author off it gave
                // every message in the room the same sender.
                senderId = it.sender,
                senderHandle = it.senderHandle,
                body = it.body,
                timestamp = it.timestamp,
                isSelf = it.isSelf,
            )
        }
        _state.update { it.copy(roomMessages = history) }
        refreshAvatars(history.map { msg -> msg.senderId }.distinct())
    }

    fun closeRoom() {
        val room = (_state.value.screen as? Screen.RoomChat)?.room
        val wasInVoice = _state.value.roomVoiceActive
        _state.update {
            it.copy(
                screen = Screen.Home,
                roomMessages = emptyList(),
                roomMembers = emptyList(),
                roomChatMembers = emptyList(),
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
            // Deliberately no room.chat.unsubscribe here. Closing the view
            // is not leaving the room, and unsubscribing on the way out is
            // what removed us from the roster and let the remaining member
            // rotate the group key without us. The desktop keeps every room
            // it can see subscribed regardless of which one is selected;
            // leaving for real goes through hiding or removing the room.
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
            put("device_id", settings.cameraDeviceId)
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
            // Same contract as sendChat: the core persists the message either
            // way, so reloading shows its true state. Nothing arrives to
            // prompt this on its own - the supernode skips the author when it
            // fans a room frame out, so a sender never receives its own
            // message back.
            loadRoomHistory(room)
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
                    it.withPresence(
                        direct = it.directPeers + id,
                        // Any live peer session means we are reachable; the
                        // relay/direct distinction refines it below.
                        connectionMode = maxOf(it.connectionMode, ConnectionMode.RELAY),
                    )
                }
            }

            "peer_disconnected" -> {
                val id = event.stringOrEmpty("peer_id")
                // Only the direct session ended. A fresh relayed announce still
                // means the peer is up, so the dot is left to the union.
                _state.update { it.withPresence(direct = it.directPeers - id) }
            }

            "session_state" -> {
                val direct = event.stringOrEmpty("chat_path") == "direct"
                _state.update {
                    it.copy(
                        connectionMode = if (direct) ConnectionMode.DIRECT else it.connectionMode,
                    )
                }
            }

            "supernode_connected" -> {
                _state.update {
                    it.copy(connectionMode = maxOf(it.connectionMode, ConnectionMode.RELAY))
                }
                // Rejoin every room we hold on this node, not just the one on
                // screen. Membership is not a view state: being a member of
                // only the open room meant every other member saw us leave the
                // moment we backed out, and a room we were the last one in got
                // its group key rotated to an epoch we were never offered.
                // The desktop and headless clients have always done this on
                // connect.
                val id = event.stringOrEmpty("supernode_id")
                if (id.isNotEmpty()) {
                    viewModelScope.launch {
                        core.command("room.resubscribe_all") { put("supernode_id", id) }
                        refreshRooms()
                    }
                }
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
                if (_state.value.call?.peerId == peerId) {
                    IncomingCallNotifier.show(getApplication(), peerId, label)
                }
            }

            "call_accepted" -> {
                IncomingCallNotifier.cancel(getApplication())
                _state.update { s ->
                    s.call?.let { s.copy(call = it.copy(phase = CallPhase.ACTIVE)) } ?: s
                }
            }

            "call_ended" -> {
                IncomingCallNotifier.cancel(getApplication())
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

            // Carries the canonical peer id (the core resolves the sender's
            // identity key to it), so it matches the ids the peer list uses.
            "presence_updated" -> {
                val id = event.stringOrEmpty("peer_id")
                val present = event.stringOrEmpty("status") != "offline"
                _state.update {
                    it.withPresence(
                        relay = if (present) {
                            it.relayPresentPeers + id
                        } else {
                            it.relayPresentPeers - id
                        },
                    )
                }
            }

            "handle_updated" -> refreshPeers()

            "room_created", "room_invite_ready" -> refreshRooms()

            // Our own offers echo back as events too; only an inbound one is
            // a question for the user.
            "file_offered" -> {
                if (!event.isSelfEvent()) {
                    _state.update {
                        it.copy(
                            fileOffer = FileOffer(
                                transferId = event.stringOrEmpty("transfer_id"),
                                peerId = event.stringOrEmpty("peer_id"),
                                name = event.stringOrEmpty("rel_path"),
                                size = event.number("size").toLong(),
                                // Room offers carry the hosting supernode; 1:1
                                // offers leave it empty.
                                isRoom = event.stringOrEmpty("supernode_id").isNotBlank(),
                            ),
                        )
                    }
                }
            }

            "file_progress" -> {
                val id = event.stringOrEmpty("transfer_id")
                val progress = event.number("progress").toFloat()
                _state.update { it.copy(transfers = it.transfers + (id to progress)) }
            }

            "file_complete" -> {
                val id = event.stringOrEmpty("transfer_id")
                val path = event.stringOrEmpty("path")
                val name = event.stringOrEmpty("rel_path")
                _state.update {
                    it.copy(
                        transfers = it.transfers - id,
                        // Only a received file can be saved out; our own
                        // completed upload is already on this device.
                        savedFile = if (path.isNotBlank()) SavedFile(name, path) else it.savedFile,
                    )
                }
                (_state.value.screen as? Screen.Chat)?.let { chat ->
                    viewModelScope.launch { loadHistory(chat.peer.peerId) }
                }
            }

            "file_failed" -> {
                val id = event.stringOrEmpty("transfer_id")
                _state.update {
                    it.copy(
                        transfers = it.transfers - id,
                        error = event.stringOrEmpty("reason").ifBlank { "transfer failed" },
                    )
                }
            }

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
                refreshAvatars(listOf(sender))
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
                val members = event.stringList("members")
                val chatMembers = event.stringList("chat_members")
                // Record every room's roster, not just the open one. Rosters
                // arrive for everything we subscribe to, and discarding the
                // rest is why the room list could only show a count after you
                // opened the room.
                val key = event.stringOrEmpty("supernode_id") +
                    ":" + event.stringOrEmpty("room_id")
                _state.update {
                    it.copy(
                        roomVoiceRosters = it.roomVoiceRosters + (key to members),
                        roomTextRosters = it.roomTextRosters + (key to chatMembers),
                    )
                }
                if (!isOpenRoom(event)) return@onCoreEvent
                // Membership arriving at all means the supernode admitted us.
                _state.update {
                    it.copy(
                        roomMembers = members,
                        roomChatMembers = chatMembers,
                        roomJoined = true,
                    )
                }
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
