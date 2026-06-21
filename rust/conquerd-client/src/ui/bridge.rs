//! Qt/QML bridge — the AppBridge QObject singleton that QML binds to.
//!
//! Compiled only when the `qt-ui` Cargo feature is enabled.
//!
//! Lifecycle:
//! 1. QML instantiates `AppBridge { id: backend }`.
//! 2. QML calls `backend.initializeBackend()` from `Component.onCompleted`.
//! 3. `initializeBackend` unlocks the identity, opens stores, starts a
//!    multi-thread tokio runtime on a dedicated OS thread, and spawns all
//!    ConquerD background tasks there.
//! 4. Background tasks post UI updates back to the Qt thread via
//!    `CxxQtThread::queue`.
//! 5. QML user actions call invokables which `try_send` on tokio channels.

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use parking_lot::RwLock;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use cxx_qt_lib::QString;

use crate::call_controller::CallCommand;
use crate::connection_manager::{ConnectionCommand, ConnectionEvent};
use crate::sfu_client::SfuCommand;

/// The main QObject singleton exposed to QML as `ConquerD.Client::AppBridge`.
#[cxx_qt::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i32, peer_count)]
        #[qproperty(bool, in_room)]
        /// True when local audio capture is active (direct call OR room voice).
        /// Distinguishes "chat-only room join" from "voice room join".
        #[qproperty(bool, voice_active)]
        #[qproperty(QString, session_banner)]
        #[qproperty(QString, call_state)]
        #[qproperty(QString, public_id)]
        /// Our embedded build ID (for reproducible build attestation).
        #[qproperty(QString, build_id)]
        #[qproperty(QString, invite_url)]
        /// One of "direct", "relay", "offline", "error" — drives SessionBanner colour.
        #[qproperty(QString, connection_mode)]
        /// Elapsed seconds for the current call (0 when idle).
        #[qproperty(i32, call_duration_secs)]
        /// Number of missed inbound calls since last cleared.
        #[qproperty(i32, missed_calls)]
        /// True when the x.ollama.v1 plugin is enabled and its task is running.
        #[qproperty(bool, ollama_available)]
        /// Normalized audio input level (0.0–1.0), updated each Opus frame.
        /// Non-zero only while a call or mic test is active.
        #[qproperty(f32, mic_level)]
        /// True while a microphone test is in progress.
        #[qproperty(bool, mic_test_active)]
        type AppBridge = super::AppBridgeRust;

        // ── Signals ───────────────────────────────────────────────────────

        // ── Signals ───────────────────────────────────────────────────────

        /// Emitted when the peer list changes. `peers_json` is a JSON array
        /// of `{peer_id, handle, online, in_call, blocked}` objects.
        #[qsignal]
        #[rust_name = "peers_updated"]
        fn peersUpdated(self: Pin<&mut AppBridge>, peers_json: QString);

        /// Emitted for each inbound chat message. `msg_json` is a single
        /// `{msg_id, sender, body, timestamp, kind, mine, status}` JSON object.
        #[qsignal]
        #[rust_name = "chat_message_received"]
        fn chatMessageReceived(self: Pin<&mut AppBridge>, msg_json: QString);

        /// Emitted when a locally-authored message delivery status changes.
        #[qsignal]
        #[rust_name = "message_status_changed"]
        fn messageStatusChanged(self: Pin<&mut AppBridge>, msg_id: QString, status: QString);

        /// Emitted when a peer is selected to load its full chat history.
        /// `msgs_json` is a JSON array of `{msg_id, sender, body, timestamp, kind, mine, status}`
        /// objects. Consumers should call `chatModel.setMessages()` to atomically
        /// replace the model contents.
        #[qsignal]
        #[rust_name = "chat_history_loaded"]
        fn chatHistoryLoaded(self: Pin<&mut AppBridge>, msgs_json: QString);

        /// Emitted when the room participant list changes. `json` is a JSON
        /// array of `{peer_id, handle, speaking, muted}` objects.
        #[qsignal]
        #[rust_name = "participants_updated"]
        fn participantsUpdated(self: Pin<&mut AppBridge>, json: QString);

        /// Emitted when a remote peer requests an audio call.
        #[qsignal]
        #[rust_name = "incoming_call"]
        fn incomingCall(self: Pin<&mut AppBridge>, peer_id: QString);

        /// Emitted when the available SFU room list updates. `rooms_json` is a
        /// JSON object `{supernode_id, rooms: [{room_id, name, kind, count}]}`.
        #[qsignal]
        #[rust_name = "sfu_rooms_updated"]
        fn sfuRoomsUpdated(self: Pin<&mut AppBridge>, rooms_json: QString);

        /// Emitted when a supernode connects or disconnects. `nodes_json` is a
        /// JSON array of `{node_id, connected, homepage_url, title, sfu_enabled}` patch objects.
        #[qsignal]
        #[rust_name = "nodes_updated"]
        fn nodesUpdated(self: Pin<&mut AppBridge>, nodes_json: QString);

        /// Emitted after a supernode is removed from the trusted peer store.
        #[qsignal]
        #[rust_name = "supernode_removed"]
        fn supernodeRemoved(self: Pin<&mut AppBridge>, node_id: QString);

        /// Full Rooms sidebar rebuild from the trusted peer store. `nodes_json`
        /// is a JSON array of `{node_id, connected, homepage_url, title, sfu_enabled}`.
        #[qsignal]
        #[rust_name = "rooms_sidebar_sync"]
        fn roomsSidebarSync(self: Pin<&mut AppBridge>, nodes_json: QString);

        /// Emitted when a supernode sends its homepage / portal info.
        /// Open `url` in the system browser to show the portal.
        #[qsignal]
        #[rust_name = "supernode_info_received"]
        fn supernodeInfoReceived(
            self: Pin<&mut AppBridge>,
            node_id: QString,
            url: QString,
            title: QString,
        );

        /// Emitted when the embedded browser panel should navigate to a
        /// `conquerd://` supernode portal. `url` is the full `conquerd://`
        /// URL to load; the panel sets `nodeMode: true` and calls `navigateTo`.
        #[qsignal]
        #[rust_name = "navigate_node_portal"]
        fn navigateNodePortal(self: Pin<&mut AppBridge>, supernode_id: QString, url: QString);

        /// Emitted when a supernode requires a portal visit before granting relay.
        /// The UI should open `portal_url` in the system browser and switch to
        /// the Nodes tab so the user understands what is happening.
        #[qsignal]
        #[rust_name = "relay_portal_required"]
        fn relayPortalRequired(
            self: Pin<&mut AppBridge>,
            supernode_id: QString,
            portal_url: QString,
        );

        // ── Invokables ────────────────────────────────────────────────────

        /// Bootstrap identity, stores, and background runtime.
        /// Call once from `Component.onCompleted` in QML.
        #[qinvokable]
        #[rust_name = "initialize_backend"]
        fn initializeBackend(self: Pin<&mut AppBridge>);

        /// Stop the current audio call.
        #[qinvokable]
        #[rust_name = "end_call"]
        fn endCall(self: Pin<&mut AppBridge>);

        /// Leave the current SFU voice room.
        #[qinvokable]
        #[rust_name = "leave_room"]
        fn leaveRoom(self: Pin<&mut AppBridge>);

        /// Send a text chat message to `peer_id`.
        #[qinvokable]
        #[rust_name = "send_chat"]
        fn sendChat(self: Pin<&mut AppBridge>, peer_id: &QString, message: &QString);

        /// Initiate an audio call to `peer_id`.
        #[qinvokable]
        #[rust_name = "start_call"]
        fn startCall(self: Pin<&mut AppBridge>, peer_id: &QString);

        /// Generate an invite link and copy it to the system clipboard.
        /// Also sets the `invite_url` property to the generated URL.
        #[qinvokable]
        #[rust_name = "copy_invite"]
        fn copyInvite(self: Pin<&mut AppBridge>);

        /// Accept an invite URL (conquerd://invite#…) or peer ID pasted by the user.
        #[qinvokable]
        #[rust_name = "paste_invite"]
        fn pasteInvite(self: Pin<&mut AppBridge>, url: &QString);

        /// Accept an incoming call from `peer_id`.
        #[qinvokable]
        #[rust_name = "accept_call"]
        fn acceptCall(self: Pin<&mut AppBridge>, peer_id: &QString);

        /// Reject / hang up an incoming call from `peer_id`.
        #[qinvokable]
        #[rust_name = "reject_call"]
        fn rejectCall(self: Pin<&mut AppBridge>, peer_id: &QString);

        /// Join an SFU room for text chat only (no audio pipeline started).
        #[qinvokable]
        #[rust_name = "join_room"]
        fn joinRoom(self: Pin<&mut AppBridge>, supernode_id: &QString, room_id: &QString);

        /// Validate a private-room invite token and join the SFU room.
        #[qinvokable]
        #[rust_name = "join_room_with_invite"]
        fn joinRoomWithInvite(
            self: Pin<&mut AppBridge>,
            supernode_id: &QString,
            room_id: &QString,
            invite_token: &QString,
        );

        /// Subscribe to an SFU room's text chat without joining voice or
        /// leaving the current voice room. Single-clicking a room in the sidebar
        /// calls this so the user can browse chat across rooms freely.
        #[qinvokable]
        #[rust_name = "subscribe_room_chat"]
        fn subscribeRoomChat(self: Pin<&mut AppBridge>, supernode_id: &QString, room_id: &QString);

        /// Join an SFU room for both text chat AND voice (starts audio pipeline).
        #[qinvokable]
        #[rust_name = "join_room_with_voice"]
        fn joinRoomWithVoice(self: Pin<&mut AppBridge>, supernode_id: &QString, room_id: &QString);

        /// Create and immediately join a new SFU voice room on a supernode.
        /// `room_type` is `"public"` or `"private"` (supernode wire shape).
        #[qinvokable]
        #[rust_name = "create_room"]
        fn createRoom(
            self: Pin<&mut AppBridge>,
            supernode_id: &QString,
            room_name: &QString,
            room_type: &QString,
        );

        /// Emitted when the supernode acknowledges a room we created.
        #[qsignal]
        #[rust_name = "room_created"]
        fn roomCreated(
            self: Pin<&mut AppBridge>,
            supernode_id: QString,
            room_id: QString,
            room_name: QString,
            room_type: QString,
            invite_token: QString,
        );

        /// Hide an SFU room from the local Rooms sidebar (not deleted on the supernode).
        #[qinvokable]
        #[rust_name = "remove_room"]
        fn removeRoom(self: Pin<&mut AppBridge>, supernode_id: &QString, room_id: &QString);

        /// Emitted when a room is hidden from the local Rooms sidebar.
        #[qsignal]
        #[rust_name = "room_removed"]
        fn roomRemoved(self: Pin<&mut AppBridge>, supernode_id: QString, room_id: QString);

        /// Register the conquerd:// URI scheme handler (Windows only, no-op elsewhere).
        #[qinvokable]
        #[rust_name = "register_uri_scheme"]
        fn registerUriScheme(self: Pin<&mut AppBridge>);

        /// Unregister the conquerd:// URI scheme handler (Windows only, no-op elsewhere).
        #[qinvokable]
        #[rust_name = "unregister_uri_scheme"]
        fn unregisterUriScheme(self: Pin<&mut AppBridge>);

        /// Open a supernode's in-app portal in the embedded browser panel.
        ///
        /// Requests a relay slot from the supernode (needed to open the QUIC
        /// connection) and then emits [`navigateNodePortal`] with the
        /// `conquerd://<supernode_id>/` URL so QML can load it.
        /// The relay request is fire-and-forget; the QUIC connection is set up
        /// asynchronously and the first page load will block until it is ready.
        #[qinvokable]
        #[rust_name = "open_node_portal"]
        fn openNodePortal(self: Pin<&mut AppBridge>, supernode_id: &QString);

        /// Apply a pending update (launch installer with --update-and-relaunch).
        #[qinvokable]
        #[rust_name = "apply_update"]
        fn applyUpdate(self: Pin<&mut AppBridge>);

        /// Mute or unmute the local microphone.
        #[qinvokable]
        #[rust_name = "set_muted"]
        fn setMuted(self: Pin<&mut AppBridge>, muted: bool);

        /// Attempt to unlock the identity using a text passphrase and/or a keyfile.
        /// Either `passphrase` or `file_path` may be empty; at least one must be set.
        /// `file_path` must be a local OS file path (not a URL).
        /// Called from QML after the user submits the passphrase dialog.
        #[qinvokable]
        #[rust_name = "unlock_with_passphrase_and_file"]
        fn unlockWithPassphraseAndFile(
            self: Pin<&mut AppBridge>,
            passphrase: &QString,
            file_path: &QString,
        );

        /// Emitted when the identity requires a passphrase to unlock.
        /// `is_new` is true when creating a new identity (no existing file).
        #[qsignal]
        #[rust_name = "passphrase_required"]
        fn passphraseRequired(self: Pin<&mut AppBridge>, is_new: bool);

        /// Emitted when a newer release is available. `tag` is the version
        /// string (e.g. "v1.2.0") and `url` is the GitHub release URL.
        #[qsignal]
        #[rust_name = "update_available"]
        fn updateAvailable(self: Pin<&mut AppBridge>, tag: QString, url: QString);

        /// Clear the unread message count (call when the user views chat).
        #[qinvokable]
        #[rust_name = "clear_unread"]
        fn clearUnread(self: Pin<&mut AppBridge>);

        /// Remove a peer from the peer store and disconnect them.
        #[qinvokable]
        #[rust_name = "remove_peer"]
        fn removePeer(self: Pin<&mut AppBridge>, peer_id: &QString);

        /// Remove a trusted supernode from the store and tear down its WS session.
        #[qinvokable]
        #[rust_name = "remove_supernode"]
        fn removeSupernode(self: Pin<&mut AppBridge>, node_id: &QString);

        /// Block a peer — prevents further inbound messages.
        #[qinvokable]
        #[rust_name = "block_peer"]
        fn blockPeer(self: Pin<&mut AppBridge>, peer_id: &QString);

        /// Unblock a previously blocked peer.
        #[qinvokable]
        #[rust_name = "unblock_peer"]
        fn unblockPeer(self: Pin<&mut AppBridge>, peer_id: &QString);

        /// Copy a peer's public ID to the system clipboard.
        #[qinvokable]
        #[rust_name = "copy_peer_id"]
        fn copyPeerId(self: Pin<&mut AppBridge>, peer_id: &QString);

        /// Load and emit chat history for the selected peer.
        #[qinvokable]
        #[rust_name = "select_peer"]
        fn selectPeer(self: Pin<&mut AppBridge>, peer_id: &QString);

        /// Load an older page of chat history for the selected peer.
        /// Emits `chatHistoryPrepended` with a JSON array (oldest-first within the page).
        #[qinvokable]
        #[rust_name = "load_more_history"]
        fn loadMoreHistory(self: Pin<&mut AppBridge>, peer_id: &QString, page: i32);

        /// Emitted when an older history page is loaded. `msgs_json` is a JSON array
        /// of message objects to prepend to the active conversation.
        #[qsignal]
        #[rust_name = "chat_history_prepended"]
        fn chatHistoryPrepended(self: Pin<&mut AppBridge>, msgs_json: QString);

        /// Send a typing indicator to a peer.
        #[qinvokable]
        #[rust_name = "send_typing"]
        fn sendTyping(self: Pin<&mut AppBridge>, peer_id: &QString, is_typing: bool);

        /// Send a text chat message to the current SFU room.
        #[qinvokable]
        #[rust_name = "send_room_chat"]
        fn sendRoomChat(self: Pin<&mut AppBridge>, body: &QString);

        /// Start PTT polling for the given key name (e.g. "space", "f1").
        /// Replaces any previously running PTT thread.
        #[qinvokable]
        #[rust_name = "enable_ptt"]
        fn enablePtt(self: Pin<&mut AppBridge>, key: &QString);

        /// Stop the PTT polling thread (call when PTT is disabled in settings).
        #[qinvokable]
        #[rust_name = "disable_ptt"]
        fn disablePtt(self: Pin<&mut AppBridge>);

        /// Switch the active audio pipeline between PTT (false) and
        /// voice-activation (true). Takes effect immediately when a call
        /// is in progress; otherwise the next `StartAudio` picks it up.
        #[qinvokable]
        #[rust_name = "set_voice_activation"]
        fn setVoiceActivation(self: Pin<&mut AppBridge>, enabled: bool);

        /// Update the jitter buffer depth (1–20 Opus frames = 20–400 ms).
        /// Takes effect immediately for the current and future calls.
        #[qinvokable]
        #[rust_name = "set_jitter_depth"]
        fn setJitterDepth(self: Pin<&mut AppBridge>, depth: i32);

        /// Emitted when a peer starts or stops typing.
        #[qsignal]
        #[rust_name = "typing_changed"]
        fn typingChanged(self: Pin<&mut AppBridge>, peer_id: QString, is_typing: bool);

        /// Emitted when a room text chat message arrives.
        #[qsignal]
        #[rust_name = "room_chat_received"]
        fn roomChatReceived(self: Pin<&mut AppBridge>, msg_json: QString);

        /// Emitted when a new peer is added via invite handshake.
        #[qsignal]
        #[rust_name = "peer_added"]
        fn peerAdded(self: Pin<&mut AppBridge>, peer_id: QString, handle: QString);

        /// Emitted when a remote peer sends a file offer.
        /// `json` = `{transfer_id, peer_id, rel_path, size, purpose}`.
        #[qsignal]
        #[rust_name = "file_offered"]
        fn fileOffered(self: Pin<&mut AppBridge>, json: QString);

        /// Emitted during an active file transfer with progress 0.0–1.0.
        #[qsignal]
        #[rust_name = "file_progress"]
        fn fileProgress(self: Pin<&mut AppBridge>, transfer_id: QString, progress: f64);

        /// Emitted when a file transfer is verified complete.
        /// The caller should move the file from the temp path to downloads.
        /// `json` = `{transfer_id, rel_path}` (data is saved to downloads dir).
        #[qsignal]
        #[rust_name = "file_complete"]
        fn fileComplete(self: Pin<&mut AppBridge>, json: QString);

        /// Emitted when a file transfer fails or is rejected.
        /// `json` = `{transfer_id, reason}`.
        #[qsignal]
        #[rust_name = "file_failed"]
        fn fileFailed(self: Pin<&mut AppBridge>, json: QString);

        // ── Ollama AI signals ─────────────────────────────────────────────

        /// Emitted for each streamed token chunk from Ollama.
        /// `request_id` lets QML correlate chunks to a specific query.
        #[qsignal]
        #[rust_name = "ollama_chunk"]
        fn ollamaChunk(self: Pin<&mut AppBridge>, request_id: QString, text: QString);

        /// Emitted when an Ollama query stream finishes successfully.
        #[qsignal]
        #[rust_name = "ollama_done"]
        fn ollamaDone(self: Pin<&mut AppBridge>, request_id: QString);

        /// Emitted when an Ollama query fails (HTTP error, timeout, etc.).
        #[qsignal]
        #[rust_name = "ollama_error"]
        fn ollamaError(self: Pin<&mut AppBridge>, request_id: QString, error: QString);

        /// Emitted when a `fetchOllamaModels` call completes.
        /// `models` is a JSON array of sorted model-name strings, e.g. `["llama3","mistral"]`.
        /// `error` is empty on success.
        #[qsignal]
        #[rust_name = "ollama_models_ready"]
        fn ollamaModelsReady(self: Pin<&mut AppBridge>, models: QString, error: QString);

        /// Emitted when a trusted peer's avatar config arrives or updates.
        /// QML Avatar components for `peer_id` should re-render.
        #[qsignal]
        #[rust_name = "avatar_config_updated"]
        fn avatarConfigUpdated(self: Pin<&mut AppBridge>, peer_id: QString);

        // ── Ollama AI invokables ──────────────────────────────────────────

        /// Send a prompt to the local Ollama instance.
        /// `request_id` is caller-chosen and echoed in every chunk/done/error.
        /// No-op when `ollama_available` is false.
        #[qinvokable]
        #[rust_name = "ask_ollama"]
        fn askOllama(
            self: Pin<&mut AppBridge>,
            request_id: &QString,
            prompt: &QString,
            system_prompt: &QString,
        );

        /// Cancel an in-flight Ollama query by `request_id`.
        #[qinvokable]
        #[rust_name = "cancel_ollama"]
        fn cancelOllama(self: Pin<&mut AppBridge>, request_id: &QString);

        /// Fetch the list of models available in the local Ollama instance.
        /// Pass `base_url` as an empty string to use the default (`http://localhost:11434`).
        /// Result is delivered asynchronously via `ollamaModelsReady`.
        #[qinvokable]
        #[rust_name = "fetch_ollama_models"]
        fn fetchOllamaModels(self: Pin<&mut AppBridge>, base_url: &QString);

        /// Accept an inbound file offer by transfer ID.
        #[qinvokable]
        #[rust_name = "accept_file"]
        fn acceptFile(self: Pin<&mut AppBridge>, transfer_id: &QString);

        /// Reject an inbound file offer by transfer ID.
        #[qinvokable]
        #[rust_name = "reject_file"]
        fn rejectFile(self: Pin<&mut AppBridge>, transfer_id: &QString);

        /// Send a file at `file_url` (a local file:// URI or absolute path) to `peer_id`.
        /// Reads the file synchronously then dispatches a SendFile command.
        #[qinvokable]
        #[rust_name = "send_file"]
        fn sendFile(self: Pin<&mut AppBridge>, peer_id: &QString, file_url: &QString);

        /// Send a file at `file_url` to the currently selected SFU room.
        #[qinvokable]
        #[rust_name = "send_room_file"]
        fn sendRoomFile(self: Pin<&mut AppBridge>, file_url: &QString);

        /// Generate an invite URL and return it as a QString.
        /// Does NOT copy to clipboard — call copyToClipboard separately if desired.
        #[qinvokable]
        #[rust_name = "generate_invite"]
        fn generateInvite(self: Pin<&mut AppBridge>) -> QString;

        /// Write `text` to the system clipboard.
        #[qinvokable]
        #[rust_name = "copy_to_clipboard"]
        fn copyToClipboard(self: Pin<&mut AppBridge>, text: &QString);

        /// Emitted when the unread count for a peer changes.
        #[qsignal]
        #[rust_name = "unread_changed"]
        fn unreadChanged(self: Pin<&mut AppBridge>, peer_id: QString, count: i32);

        /// Emitted when the last-message preview for a peer changes.
        #[qsignal]
        #[rust_name = "preview_changed"]
        fn previewChanged(self: Pin<&mut AppBridge>, peer_id: QString, text: QString);

        /// Emitted periodically (or on demand) with active session statistics.
        /// `json` = `{rtt_ms, packet_loss_pct, jitter_ms, relay, bandwidth_kbps}`.
        #[qsignal]
        #[rust_name = "connection_stats"]
        fn connectionStats(self: Pin<&mut AppBridge>, json: QString);

        /// Emitted when the local user's speaking state changes (VAD/PTT).
        /// `speaking` is `true` while voice activity is detected and `false`
        /// immediately when muted or after the VAD hold-off expires.
        #[qsignal]
        #[rust_name = "local_speaking_changed"]
        fn localSpeakingChanged(self: Pin<&mut AppBridge>, speaking: bool);

        /// Emitted when a remote room peer's speaking state changes.
        /// `speaking` is `true` when audio frames arrive from the peer and
        /// `false` after ~600 ms of silence.
        #[qsignal]
        #[rust_name = "peer_speaking_changed"]
        fn peerSpeakingChanged(self: Pin<&mut AppBridge>, peer_id: QString, speaking: bool);

        /// Emitted when a remote room peer's audio level changes.
        /// `level` is normalised RMS (0.0–1.0), emitted at ≤10 Hz per peer.
        #[qsignal]
        #[rust_name = "peer_level_changed"]
        fn peerLevelChanged(self: Pin<&mut AppBridge>, peer_id: QString, level: f32);

        /// Start a microphone test: starts audio capture and emits live level
        /// updates via the `mic_level` property.
        #[qinvokable]
        #[rust_name = "start_mic_test"]
        fn startMicTest(self: Pin<&mut AppBridge>);

        /// Stop the microphone test started by `startMicTest()`.
        #[qinvokable]
        #[rust_name = "stop_mic_test"]
        fn stopMicTest(self: Pin<&mut AppBridge>);

        /// Update the preferred audio capture / playback device names.
        /// Pass an empty string for either argument to mean "use system default".
        /// Takes effect on the next mic test or call start.
        #[qinvokable]
        #[rust_name = "set_audio_devices"]
        fn setAudioDevices(self: Pin<&mut AppBridge>, input: &QString, output: &QString);

        /// Play a short speaker test tone on the default output device.
        #[qinvokable]
        #[rust_name = "test_speaker"]
        fn testSpeaker(self: Pin<&mut AppBridge>);

        /// Set the noise gate suppression level. Accepted values (case-insensitive):
        /// "off", "mild", "moderate", "aggressive", "max".
        /// Takes effect immediately mid-call.
        #[qinvokable]
        #[rust_name = "set_noise_strength"]
        fn setNoiseStrength(self: Pin<&mut AppBridge>, level: &QString);

        /// Set the microphone input gain (0–200, where 100 = unity).
        /// Takes effect immediately mid-call.
        #[qinvokable]
        #[rust_name = "set_input_volume"]
        fn setInputVolume(self: Pin<&mut AppBridge>, pct: i32);

        /// Set the speaker output gain (0–200, where 100 = unity).
        /// Takes effect immediately mid-call.
        #[qinvokable]
        #[rust_name = "set_output_volume"]
        fn setOutputVolume(self: Pin<&mut AppBridge>, pct: i32);

        /// Set outgoing voice bitrate preset: "low", "balanced", "high", or "ultra".
        /// Takes effect immediately for direct and SFU room audio.
        #[qinvokable]
        #[rust_name = "set_voice_bitrate"]
        fn setVoiceBitrate(self: Pin<&mut AppBridge>, preset: &QString);

        /// Create Start Menu and Desktop `.lnk` shortcuts for this executable.
        /// Windows only — no-op on other platforms.
        #[qinvokable]
        #[rust_name = "create_desktop_shortcuts"]
        fn createDesktopShortcuts(self: Pin<&mut AppBridge>);

        /// Remove Start Menu and Desktop shortcuts created by
        /// `createDesktopShortcuts`.  Windows only — no-op on other platforms.
        #[qinvokable]
        #[rust_name = "remove_desktop_shortcuts"]
        fn removeDesktopShortcuts(self: Pin<&mut AppBridge>);

        /// Returns `true` if at least one ConquerD shortcut (Desktop or Start
        /// Menu) currently exists.  Windows only — always `false` elsewhere.
        #[qinvokable]
        #[rust_name = "has_desktop_shortcuts"]
        fn hasDesktopShortcuts(self: Pin<&mut AppBridge>) -> bool;

        /// Enumerate available CPAL audio devices.
        /// Returns a JSON object: `{"inputs": ["Default", ...], "outputs": ["Default", ...]}`.
        /// The string "Default" (index 0) means use the OS default; all other entries
        /// are device names that can be written to `SettingsModel::audio_input_device` /
        /// `audio_output_device`.
        #[qinvokable]
        #[rust_name = "list_audio_devices"]
        fn listAudioDevices(self: Pin<&mut AppBridge>) -> QString;

        /// Return a deterministic SVG identicon for `peer_id`.
        ///
        /// - If `config_json` is non-empty, use it directly (own-avatar preview).
        /// - Otherwise resolve trust tier from the peer store:
        ///   - Unknown / no handshake → `AvatarConfig::untrusted()` (8×8 flat)
        ///   - Known peer, no config yet → `AvatarConfig::default()` (16×16 full)
        ///   - Known peer with config → peer's exact config
        ///
        /// Returns a bare SVG string (not a data URI). QML wraps it with btoa.
        #[qinvokable]
        #[rust_name = "avatar_svg"]
        fn avatarSvg(
            self: Pin<&mut AppBridge>,
            peer_id: &QString,
            config_json: &QString,
        ) -> QString;

        /// Return the identity-derived background tint colour for `peer_id` as a
        /// `#rrggbb` hex string. Uses the same trust-tier / config lookup as
        /// `avatarSvg`. QML uses this to colour the resting-state avatar ring.
        #[qinvokable]
        #[rust_name = "avatar_tint_color"]
        fn avatarTintColor(
            self: Pin<&mut AppBridge>,
            peer_id: &QString,
            config_json: &QString,
        ) -> QString;

        /// Whether Qt should bilinear-filter the rasterized avatar (`Image.smooth`).
        /// Returns false when `svg_crisp` is enabled so cell edges stay sharp.
        #[qinvokable]
        #[rust_name = "avatar_image_smooth"]
        fn avatarImageSmooth(
            self: Pin<&mut AppBridge>,
            peer_id: &QString,
            config_json: &QString,
        ) -> bool;

        /// Broadcast the user's avatar config (as JSON) to a specific trusted peer.
        /// Silently does nothing if `config_json` is empty or peer is not trusted.
        #[qinvokable]
        #[rust_name = "broadcast_avatar_config"]
        fn broadcastAvatarConfig(
            self: Pin<&mut AppBridge>,
            peer_id: &QString,
            config_json: &QString,
        );

        /// Broadcast the user's avatar config to every currently-connected peer.
        /// Call after setAvatarConfigJson whenever the user changes their avatar.
        #[qinvokable]
        #[rust_name = "broadcast_avatar_config_to_all"]
        fn broadcastAvatarConfigToAll(self: Pin<&mut AppBridge>, config_json: &QString);

        /// Store `config_json` in the bridge so it is auto-broadcast to newly
        /// connected peers. Call whenever the user changes avatar settings.
        #[qinvokable]
        #[rust_name = "set_avatar_config_json"]
        fn setAvatarConfigJson(self: Pin<&mut AppBridge>, config_json: &QString);

        /// Re-emit previously received room chat messages as individual
        /// `roomChatReceived` signals so QML can repopulate after a room switch.
        /// History is session-scoped (not persisted to disk).
        #[qinvokable]
        #[rust_name = "load_room_chat_history"]
        fn loadRoomChatHistory(
            self: Pin<&mut AppBridge>,
            supernode_id: &QString,
            room_id: &QString,
        );

        /// Normalize a supernode sidebar id (hex `peer_id` or base64url
        /// `identity_pub`) to the canonical `identity_pub` used on the wire.
        /// Returns an empty string for ordinary peers.
        #[qinvokable]
        #[rust_name = "resolve_supernode_node_id"]
        fn resolveSupernodeNodeId(self: Pin<&mut AppBridge>, node_id: &QString) -> QString;

        /// True when `node_id` belongs to a trusted supernode in the peer store.
        #[qinvokable]
        #[rust_name = "is_known_supernode"]
        fn isKnownSupernode(self: Pin<&mut AppBridge>, node_id: &QString) -> bool;

        /// Delete a single chat message from the store by ID.
        /// Emits `messageDeleted(msg_id)` on success.
        #[qinvokable]
        #[rust_name = "delete_message"]
        fn deleteMessage(self: Pin<&mut AppBridge>, msg_id: &QString);

        /// Retry a failed locally-authored peer chat message by ID.
        #[qinvokable]
        #[rust_name = "retry_message"]
        fn retryMessage(self: Pin<&mut AppBridge>, msg_id: &QString);

        /// Delete all messages for a peer from the store.
        /// Emits `peerHistoryCleared(peer_id)` on success.
        #[qinvokable]
        #[rust_name = "clear_peer_history"]
        fn clearPeerHistory(self: Pin<&mut AppBridge>, peer_id: &QString);

        /// Return recent diagnostic log lines as a newline-delimited string.
        /// QML can call this on demand or poll with a timer.
        #[qinvokable]
        #[rust_name = "get_event_logs"]
        fn getEventLogs(self: Pin<&mut AppBridge>) -> QString;

        /// Clear the in-memory event log buffer.
        #[qinvokable]
        #[rust_name = "clear_event_logs"]
        fn clearEventLogs(self: Pin<&mut AppBridge>);

        // ── Privacy & Data invokables ─────────────────────────────────────

        /// Return the total number of chat messages stored on disk.
        #[qinvokable]
        #[rust_name = "get_stored_message_count"]
        fn getStoredMessageCount(self: Pin<&mut AppBridge>) -> i64;

        /// Delete all messages older than `days` days.
        #[qinvokable]
        #[rust_name = "trim_messages_by_age"]
        fn trimMessagesByAge(self: Pin<&mut AppBridge>, days: i32);

        /// For each conversation, keep only the most recent `keep` messages.
        #[qinvokable]
        #[rust_name = "trim_messages_by_count"]
        fn trimMessagesByCount(self: Pin<&mut AppBridge>, keep: i32);

        /// Delete all chat messages across every peer.
        #[qinvokable]
        #[rust_name = "purge_all_chat_history"]
        fn purgeAllChatHistory(self: Pin<&mut AppBridge>);

        /// Remove the identity AES key from the OS keyring, then quit.
        /// The user will be prompted for their passphrase on next launch.
        #[qinvokable]
        #[rust_name = "lock_identity_and_quit"]
        fn lockIdentityAndQuit(self: Pin<&mut AppBridge>);

        /// Emitted after a message is deleted so QML can remove it from the ChatModel.
        #[qsignal]
        #[rust_name = "message_deleted"]
        fn messageDeleted(self: Pin<&mut AppBridge>, msg_id: QString);

        /// Emitted after all messages for a peer are cleared.
        #[qsignal]
        #[rust_name = "peer_history_cleared"]
        fn peerHistoryCleared(self: Pin<&mut AppBridge>, peer_id: QString);
    }

    // Enable CxxQtThread so background tasks can post back to the Qt thread.
    impl cxx_qt::Threading for AppBridge {}
}

// ---------------------------------------------------------------------------
// Rust-side state backing AppBridge
// ---------------------------------------------------------------------------

pub struct AppBridgeRust {
    // QML property backing fields
    peer_count: i32,
    in_room: bool,
    voice_active: bool,
    session_banner: QString,
    call_state: QString,
    public_id: QString,
    /// Build ID exposed as qproperty for QML (e.g. Settings or status display).
    build_id: QString,
    invite_url: QString,
    connection_mode: QString,
    call_duration_secs: i32,
    /// Number of missed inbound calls since last cleared. Mirrors the `missed_calls` qproperty.
    missed_calls: i32,

    // Channels to background tasks (populated during initialize_backend)
    conn_cmd_tx: Option<mpsc::Sender<ConnectionCommand>>,
    call_cmd_tx: Option<mpsc::Sender<CallCommand>>,
    sfu_cmd_tx: Option<mpsc::Sender<SfuCommand>>,
    updater_cmd_tx: Option<mpsc::Sender<crate::github_updater::UpdaterCommand>>,

    /// Our own peer_id (hex SHA-256 of public key) — used for peer_store lookups.
    my_peer_id: String,
    /// Our own public_id (base64url Ed25519 pubkey) — used as `sender` in signaling messages.
    my_public_id: String,
    /// Embedded build identifier (short git sha or CI-provided reproducible build ID).
    /// Peers exchange this via build attestation so you can verify they are on a
    /// reproducible build from the same source as you (or an official release).
    my_build_id: String,

    /// The Ed25519 identity — held after unlock so invite generation works.
    identity: Option<Arc<crate::identity::Identity>>,

    /// Pending update release info (set when updateAvailable is emitted).
    pending_release: Option<crate::github_updater::ReleaseInfo>,

    /// Keep the background OS thread (and the tokio Runtime inside it) alive.
    rt_thread: Option<std::thread::JoinHandle<()>>,

    /// Unread inbound chat message count (cleared by clearUnread()).
    unread_chat: u32,

    /// Peer store shared reference — used by removePeer/blockPeer invokables.
    peer_store: Option<Arc<RwLock<crate::peer_store::PeerStore>>>,

    /// Chat store shared reference — used by selectPeer to load history.
    chat_store: Option<Arc<crate::chat_store::ChatStore>>,

    /// Local room hide-list (sidebar removals do not touch the supernode).
    room_store: Option<Arc<RwLock<crate::room_store::RoomStore>>>,

    /// Currently selected peer (for per-peer chat loading).
    selected_peer_id: String,

    /// Current SFU room supernode ID (for sendRoomChat).
    current_supernode_id: String,

    /// Current SFU room ID (for sendRoomChat).
    current_room_id: String,

    /// Active SFU voice session (may differ from chat `current_*` after subscribe).
    voice_supernode_id: String,
    voice_room_id: String,

    /// PTT polling thread stop signal. Set `true` to stop the thread.
    ptt_stop: Option<Arc<std::sync::atomic::AtomicBool>>,

    /// PTT polling thread handle (kept alive as long as PTT is enabled).
    ptt_thread: Option<std::thread::JoinHandle<()>>,

    /// Command channel to the x.ollama.v1 plugin task (None when disabled).
    ollama_cmd_tx: Option<mpsc::Sender<crate::ollama_module::OllamaCommand>>,

    /// True when the Ollama plugin is running. Mirrors the `ollama_available` qproperty.
    ollama_available: bool,

    /// Normalized audio input level (0.0–1.0). Updated each Opus frame while
    /// a call or mic test is active. Mirrors the `mic_level` qproperty.
    mic_level: f32,

    /// True while a mic test is in progress. Mirrors the `mic_test_active` qproperty.
    mic_test_active: bool,

    /// Session-scoped room chat history.
    /// Key: room_id string.  Value: ordered list of message JSON strings
    /// (same format as the `roomChatReceived` signal payload).
    room_chat_history: std::collections::HashMap<String, Vec<String>>,

    /// Local cache of the current room's participant IDs, kept in sync with
    /// every SfuMembers / SfuPeerJoined / SfuPeerLeft event so that
    /// participants_updated always carries the FULL list (not a partial
    /// one-peer update that would wipe everyone else from the model).
    room_participant_ids: Vec<String>,

    /// Canonical peer-list keys (`PeerRecord::peer_id`) currently considered online.
    online_peer_ids: HashSet<String>,
    /// Canonical peer-list keys with an active voice session (room or direct call).
    in_call_peer_ids: HashSet<String>,
    /// Peers with a live direct QUIC session.
    direct_connected_peer_ids: HashSet<String>,
    /// Peers currently in the same SFU voice room as us.
    room_present_peer_ids: HashSet<String>,
    /// Remote peer id for an active direct P2P call (identity_pub or peer_id).
    active_direct_call_peer_id: String,

    /// Rolling in-memory diagnostic log buffer (max 300 entries).
    event_log: std::collections::VecDeque<String>,

    /// Set to true when an inbound call arrives; cleared on accept or end.
    /// Used to detect missed calls (CallEnded while flag is still set).
    has_incoming_call: bool,

    /// User's own avatar config as JSON (saved in settings, broadcast to peers).
    avatar_config_json: String,
}

fn request_invite_url(rust: &AppBridgeRust) -> Option<String> {
    let tx = rust.conn_cmd_tx.as_ref()?;
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    tx.try_send(ConnectionCommand::GenerateInvite { reply_tx })
        .ok()?;
    reply_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .ok()
        .flatten()
}

impl Default for AppBridgeRust {
    fn default() -> Self {
        Self {
            peer_count: 0,
            in_room: false,
            voice_active: false,
            session_banner: QString::default(),
            call_state: QString::from("idle"),
            public_id: QString::default(),
            build_id: QString::from(env!("CONQUERD_BUILD_ID")),
            invite_url: QString::default(),
            connection_mode: QString::from("offline"),
            call_duration_secs: 0,
            missed_calls: 0,
            has_incoming_call: false,
            conn_cmd_tx: None,
            call_cmd_tx: None,
            sfu_cmd_tx: None,
            updater_cmd_tx: None,
            my_peer_id: String::new(),
            my_public_id: String::new(),
            my_build_id: env!("CONQUERD_BUILD_ID").to_owned(),
            identity: None,
            pending_release: None,
            rt_thread: None,
            unread_chat: 0,
            peer_store: None,
            chat_store: None,
            room_store: None,
            selected_peer_id: String::new(),
            current_supernode_id: String::new(),
            current_room_id: String::new(),
            voice_supernode_id: String::new(),
            voice_room_id: String::new(),
            ptt_stop: None,
            ptt_thread: None,
            ollama_cmd_tx: None,
            ollama_available: false,
            mic_level: 0.0,
            mic_test_active: false,
            room_chat_history: std::collections::HashMap::new(),
            room_participant_ids: Vec::new(),
            online_peer_ids: HashSet::new(),
            in_call_peer_ids: HashSet::new(),
            direct_connected_peer_ids: HashSet::new(),
            room_present_peer_ids: HashSet::new(),
            active_direct_call_peer_id: String::new(),
            event_log: std::collections::VecDeque::with_capacity(300),
            avatar_config_json: String::new(),
        }
    }
}

impl AppBridgeRust {
    fn resolve_supernode_node_id_str(&self, id: &str) -> Option<String> {
        self.peer_store
            .as_ref()
            .and_then(|ps| ps.read().resolve_supernode_identity_pub(id))
    }
}

impl Drop for AppBridgeRust {
    fn drop(&mut self) {
        // Signal the PTT polling thread to exit before channels close.
        if let Some(ref flag) = self.ptt_stop {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // Dropping the cmd_tx senders propagates channel closure to the tokio
        // tasks (connection manager, call controller, etc.), which lets the
        // rt_thread's event loop exit via its `else => break` arm.
        // The JoinHandle in rt_thread is then dropped (detached), and the
        // caller (main.rs) follows up with process::exit(0) to guarantee
        // termination even if a task is blocked on I/O.
        drop(self.conn_cmd_tx.take());
        drop(self.call_cmd_tx.take());
        drop(self.sfu_cmd_tx.take());
        drop(self.updater_cmd_tx.take());
        drop(self.ollama_cmd_tx.take());
    }
}

fn room_chat_history_key(supernode_id: &str, room_id: &str) -> String {
    format!("{supernode_id}:{room_id}")
}

fn room_chat_store_peer_id(supernode_id: &str, room_id: &str) -> String {
    format!("room:{supernode_id}:{room_id}")
}

// ---------------------------------------------------------------------------
// Invokable implementations
// ---------------------------------------------------------------------------

impl ffi::AppBridge {
    fn initialize_backend(mut self: Pin<&mut Self>) {
        if self.rust().rt_thread.is_some() {
            warn!("initialize_backend called more than once — ignoring");
            return;
        }

        // ── Identity unlock ───────────────────────────────────────────────
        let key_dir = crate::identity::Identity::default_key_dir();
        let dat = key_dir.join(crate::identity::IDENTITY_FILENAME);
        let env_pass = std::env::var("CONQUERD_PASSPHRASE").unwrap_or_default();
        let env_file = std::env::var("CONQUERD_PASSPHRASE_FILE").unwrap_or_default();

        // Try passphrase/keyfile from env vars first
        if !env_pass.is_empty() || !env_file.is_empty() {
            match crate::crypto::build_passphrase_material(&env_pass, &env_file) {
                Ok(material) => {
                    match crate::identity::Identity::load_with_passphrase(&material, &key_dir) {
                        Ok(id) => {
                            self.continue_initialization(Arc::new(id));
                            return;
                        }
                        Err(e) => {
                            error!("Identity unlock with CONQUERD_PASSPHRASE/FILE failed: {e}");
                            // Fall through to ask user
                        }
                    }
                }
                Err(e) => {
                    error!("Invalid env passphrase/keyfile: {e}");
                }
            }
        }

        // Try keyring (no passphrase needed)
        if dat.exists() {
            if let Ok((id, _)) =
                crate::identity::Identity::load_with_keyring_or_passphrase(b"", &key_dir)
            {
                // Keyring succeeded
                self.continue_initialization(Arc::new(id));
                return;
            }
            // Keyring not available / stale — need passphrase from user
            info!("Identity locked — requesting passphrase from user");
            self.as_mut().passphrase_required(false);
            return;
        }

        // No identity file — ask user for a passphrase to create one
        info!("No identity found — requesting passphrase from user to create new identity");
        self.as_mut().passphrase_required(true);
    }

    fn unlock_with_passphrase_and_file(
        mut self: Pin<&mut Self>,
        passphrase: &QString,
        file_path: &QString,
    ) {
        let key_dir = crate::identity::Identity::default_key_dir();
        let dat = key_dir.join(crate::identity::IDENTITY_FILENAME);
        let text = passphrase.to_string();
        let path = file_path.to_string();

        let key_material = match crate::crypto::build_passphrase_material(&text, &path) {
            Ok(m) => m,
            Err(e) => {
                self.as_mut()
                    .set_session_banner(QString::from(e.to_string().as_str()));
                self.as_mut().passphrase_required(false);
                return;
            }
        };

        if dat.exists() {
            // Unlock existing identity
            match crate::identity::Identity::load_with_passphrase(&key_material, &key_dir) {
                Ok(id) => {
                    self.continue_initialization(Arc::new(id));
                }
                Err(e) => {
                    error!("Passphrase/keyfile incorrect: {e}");
                    self.as_mut()
                        .set_session_banner(QString::from("Incorrect passphrase — try again."));
                    self.as_mut().passphrase_required(false);
                }
            }
        } else {
            // Create new identity with this key material
            std::fs::create_dir_all(&key_dir).ok();
            let id = crate::identity::Identity::generate();
            if let Err(e) = id.save_encrypted(&key_material, &key_dir) {
                error!("Failed to save new identity: {e}");
                self.as_mut()
                    .set_session_banner(QString::from("Failed to create identity."));
                return;
            }
            self.continue_initialization(Arc::new(id));
        }
    }

    fn continue_initialization(mut self: Pin<&mut Self>, identity: Arc<crate::identity::Identity>) {
        if self.rust().rt_thread.is_some() {
            warn!("continue_initialization: already running");
            return;
        }

        self.as_mut()
            .rust_mut()
            .my_peer_id
            .clone_from(&identity.peer_id().to_owned());
        self.as_mut()
            .rust_mut()
            .my_public_id
            .clone_from(&identity.public_id().to_owned());
        self.as_mut()
            .set_public_id(QString::from(identity.public_id().as_str()));

        // Make our peer ID available to the conquerd:// portal bridge so
        // window.conquerd.ready resolves with the correct myPeerId value.
        #[cfg(feature = "webengine")]
        crate::ui::scheme::set_portal_peer_id(identity.public_id().as_str());

        info!(
            "AppBridge: identity {} ({})",
            identity.public_id(),
            identity.peer_id()
        );

        // ── Stores ────────────────────────────────────────────────────────
        let peer_store = match crate::peer_store::PeerStore::open(&identity, None) {
            Ok(s) => Arc::new(RwLock::new(s)),
            Err(e) => {
                error!("Peer store error: {e}");
                return;
            }
        };
        let _chat_store_arc = match crate::chat_store::ChatStore::open(&identity, None) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                error!("Chat store error: {e}");
                return;
            }
        };
        let chat_store = Arc::clone(&_chat_store_arc);
        let room_store = match crate::room_store::RoomStore::open(&identity, None) {
            Ok(s) => Arc::new(RwLock::new(s)),
            Err(e) => {
                error!("Room store error: {e}");
                return;
            }
        };

        // Re-promote supernodes that were demoted by an older repair pass but
        // are still referenced by saved room definitions (relay_hints intact).
        {
            let room_supernode_ids: Vec<String> = {
                let rs = room_store.read();
                let mut ids = std::collections::HashSet::new();
                for entry in rs.list() {
                    if !entry.supernode_id.is_empty() {
                        ids.insert(entry.supernode_id.clone());
                    }
                }
                ids.into_iter().collect()
            };
            if !room_supernode_ids.is_empty() {
                let mut store = peer_store.write();
                if store.restore_supernodes_referenced_by_ids(&room_supernode_ids) {
                    if let Err(e) = store.save() {
                        warn!("Failed to persist restored supernode flags: {e}");
                    }
                }
            }
            if let Err(e) = room_store
                .write()
                .normalize_supernode_ids(&peer_store.read())
            {
                warn!("RoomStore supernode id normalize failed: {e}");
            }
        }

        // ── Split subsystems ──────────────────────────────────────────────
        // Build a shared FeatureRegistry so plugin descriptors registered
        // by `PluginRuntime::start` are visible in the manager's
        // CAPABILITY_ANNOUNCE snapshot.
        let feature_registry = std::sync::Arc::new(conquerd_features::FeatureRegistry::new());
        if let Err(e) = conquerd_features::register_client_modules(&feature_registry) {
            error!("failed to seed feature registry: {e}");
        }

        let (conn_cmd_tx, conn_event_rx, conn_fut) =
            crate::connection_manager::ConnectionManager::split_with_registry(
                Arc::clone(&identity),
                Arc::clone(&peer_store),
                Arc::clone(&feature_registry),
            );
        let (call_cmd_tx, call_event_rx, call_fut) =
            crate::call_controller::CallController::split(Some(conn_cmd_tx.clone()));
        let (sfu_cmd_tx, _sfu_event_rx, sfu_fut) =
            crate::sfu_client::SfuClient::split(Some(conn_cmd_tx.clone()));
        let (_upnp_cmd_tx, _upnp_event_rx, upnp_fut) = crate::upnp::UPnPManager::split();
        let installer_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("conquerd-installer.exe")));
        let (updater_cmd_tx, updater_event_rx, updater_fut) = crate::github_updater::Updater::split(
            env!("CARGO_PKG_VERSION"),
            crate::github_updater::DEFAULT_REPO,
            installer_path,
        );

        // ── Plugin runtime: load enabled bespoke modules from settings ───
        // `PluginRuntime::start` does NOT spawn — it builds channels + futures
        // so we can store cmd_tx on the bridge before entering the tokio thread.
        let mut plugin_manager = crate::plugin_manager::PluginManager::new();
        {
            let s = read_plugin_settings();
            plugin_manager.load_from_settings(
                s.ollama_enabled,
                &s.ollama_base_url,
                &s.ollama_model,
            );
        }
        let started_plugins =
            crate::plugin_runtime::PluginRuntime::start(&plugin_manager, &feature_registry);

        // Destructure Ollama handles before any moves.
        let (maybe_ollama_cmd, ollama_event_rx, ollama_task) = match started_plugins.ollama {
            Some(h) => (Some(h.cmd_tx), Some(h.event_rx), Some(h.task)),
            None => (None, None, None),
        };
        let ollama_is_available = maybe_ollama_cmd.is_some();

        {
            let mut r = self.as_mut().rust_mut();
            r.conn_cmd_tx = Some(conn_cmd_tx.clone());
            r.call_cmd_tx = Some(call_cmd_tx);
            r.sfu_cmd_tx = Some(sfu_cmd_tx);
            r.updater_cmd_tx = Some(updater_cmd_tx.clone());
            r.identity = Some(Arc::clone(&identity));
            r.peer_store = Some(Arc::clone(&peer_store));
            r.chat_store = Some(Arc::clone(&chat_store));
            r.room_store = Some(Arc::clone(&room_store));
            r.ollama_cmd_tx = maybe_ollama_cmd;
            r.ollama_available = ollama_is_available;
        }
        if ollama_is_available {
            self.as_mut().set_ollama_available(true);
        }

        // ── PTT: start if enabled in persisted settings ───────────────────
        {
            let snap = read_settings_for_ptt();
            if snap.0 {
                // push_to_talk enabled — start polling thread
                let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let stop2 = Arc::clone(&stop);
                let (ptt_tx, ptt_rx) = std::sync::mpsc::sync_channel::<bool>(4);
                let call_tx = self.rust().call_cmd_tx.clone();
                std::thread::spawn(move || {
                    while let Ok(muted) = ptt_rx.recv() {
                        if let Some(ref tx) = call_tx {
                            let _ = tx.try_send(CallCommand::SetMuted(muted));
                        }
                    }
                });
                let handle = crate::platform::start_ptt_polling(snap.1, ptt_tx, stop2);
                let mut r = self.as_mut().rust_mut();
                r.ptt_stop = Some(stop);
                r.ptt_thread = Some(handle);
            }
        }

        // ── Apply persisted jitter depth to the call controller ───────────
        {
            let depth = read_jitter_depth_setting();
            if depth != 3 {
                if let Some(ref tx) = self.rust().call_cmd_tx {
                    let _ = tx.try_send(CallCommand::SetJitterDepth(depth));
                }
            }
        }

        // Apply persisted outgoing voice bitrate to the call controller.
        {
            let bitrate = read_voice_bitrate_setting();
            if let Some(ref tx) = self.rust().call_cmd_tx {
                let _ = tx.try_send(CallCommand::SetOutgoingBitrate(bitrate));
            }
        }

        self.as_mut()
            .set_session_banner(QString::from("Connecting\u{2026}"));

        // ── Emit initial peer list from store ─────────────────────────────
        {
            emit_peers_updated(self.as_mut());
            emit_rooms_sidebar_sync(self.as_mut());
            emit_local_rooms_for_all_supernodes(self.as_mut());
        }

        let qt_thread = self.qt_thread();

        let rt_thread = match std::thread::Builder::new()
            .name("conquerd-tokio".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        error!("failed to create conquerd tokio runtime: {e}");
                        return;
                    }
                };

                // Register the scheme handler callback so conquerd:// URL
                // fetches can be routed through the ConnectionManager.
                // Must be done before any conquerd:// URL is loaded; the
                // runtime handle lets the C++ handler thread call block_on.
                #[cfg(feature = "webengine")]
                crate::ui::scheme::register_fetch_callback(
                    conn_cmd_tx.clone(),
                    rt.handle().clone(),
                );

                rt.block_on(async move {
                    tokio::spawn(conn_fut);
                    tokio::spawn(call_fut);
                    tokio::spawn(sfu_fut);
                    tokio::spawn(upnp_fut);
                    tokio::spawn(updater_fut);

                    // Spawn Ollama task if the plugin is enabled.
                    if let Some(task) = ollama_task {
                        tokio::spawn(task);
                    }

                    crate::platform::register_uri_scheme();

                    let _ = updater_cmd_tx
                        .send(crate::github_updater::UpdaterCommand::Check)
                        .await;

                    // Drive connection events, updater events, and Ollama events.
                    let mut ev_rx = conn_event_rx;
                    let mut up_rx = updater_event_rx;
                    let mut ol_rx = ollama_event_rx;
                    let mut call_rx = call_event_rx;
                    let mut call_timer_stop: Option<oneshot::Sender<()>> = None;
                    loop {
                        tokio::select! {
                            Some(ev) = ev_rx.recv() => {
                                dispatch_event(&qt_thread, ev, &chat_store, &mut call_timer_stop);
                            }
                            Some(ev) = up_rx.recv() => {
                                dispatch_update_event(&qt_thread, ev);
                            }
                            Some(ev) = async {
                                match ol_rx.as_mut() {
                                    Some(rx) => rx.recv().await,
                                    None => std::future::pending().await,
                                }
                            } => {
                                dispatch_ollama_event(&qt_thread, ev);
                            }
                            Some(ev) = call_rx.recv() => {
                                dispatch_call_event(&qt_thread, ev);
                            }
                            else => break,
                        }
                    }
                    info!("AppBridge event loop exited");
                });
            }) {
            Ok(thread) => thread,
            Err(e) => {
                error!("failed to spawn conquerd-tokio thread: {e}");
                return;
            }
        };

        self.as_mut().rust_mut().rt_thread = Some(rt_thread);
    }

    fn end_call(mut self: Pin<&mut Self>) {
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::StopAudio);
        }
        {
            let active = self.rust().active_direct_call_peer_id.clone();
            if !active.is_empty() {
                let resolved = lookup_list_peer_id(self.rust(), &active);
                set_active_direct_call_presence(
                    &mut self.as_mut().rust_mut(),
                    &active,
                    false,
                    resolved,
                );
            }
        }
        self.as_mut().set_call_state(QString::from("idle"));
        self.as_mut().set_voice_active(false);
        emit_peers_updated(self.as_mut());
    }

    fn leave_room(mut self: Pin<&mut Self>) {
        let (prev_sn, prev_rid) = {
            let r = self.rust();
            (r.voice_supernode_id.clone(), r.voice_room_id.clone())
        };
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            if !prev_sn.is_empty() && !prev_rid.is_empty() {
                let _ = tx.try_send(ConnectionCommand::LeaveRoom {
                    supernode_id: prev_sn.clone(),
                    room_id: prev_rid,
                });
                let _ = tx.try_send(ConnectionCommand::RequestRoomList {
                    supernode_id: prev_sn,
                });
            }
        }
        // Clear room audio mode and stop audio in case we were in a voice room.
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::ClearRoomMode);
            let _ = tx.try_send(CallCommand::StopAudio);
        }
        {
            let mut r = self.as_mut().rust_mut();
            r.room_participant_ids.clear();
            r.voice_supernode_id.clear();
            r.voice_room_id.clear();
        }
        clear_room_member_presence(&mut self.as_mut().rust_mut());
        self.as_mut().set_in_room(false);
        self.as_mut().set_voice_active(false);
        emit_peers_updated(self.as_mut());
    }

    fn send_chat(mut self: Pin<&mut Self>, peer_id: &QString, message: &QString) {
        use crate::protocol::{MessageType, SignalingMessage};

        let pid = peer_id.to_string();
        let body = message.to_string();
        let message_id = uuid::Uuid::new_v4().to_string();
        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        // Extract everything we need while the immutable borrow is live.
        let (_sender_public, handle, chat_store_opt, outbound_msg) = {
            let r = self.rust();
            let sender_pub = r.my_public_id.clone();
            let handle = r
                .peer_store
                .as_ref()
                .and_then(|ps| {
                    let store = ps.read();
                    store.get(&r.my_peer_id).map(|rec| rec.display_name())
                })
                .unwrap_or_default();
            let cs = r.chat_store.clone();
            let mut msg = SignalingMessage::new(MessageType::ChatMessage, sender_pub.clone());
            msg.target = Some(pid.clone());
            msg.payload
                .insert("body".to_string(), serde_json::Value::String(body.clone()));
            msg.payload.insert(
                "message_id".to_string(),
                serde_json::Value::String(message_id.clone()),
            );
            msg.payload.insert(
                "sender_handle".to_string(),
                serde_json::Value::String(handle.clone()),
            );
            (sender_pub, handle, cs, msg)
        };

        // Send to the peer. If the command channel is missing, full, or
        // closed, mark the message failed immediately so it never lingers in
        // "sending" — the user can then retry it explicitly.
        let sent = match self.rust().conn_cmd_tx {
            Some(ref tx) => tx
                .try_send(ConnectionCommand::SendMessage(outbound_msg))
                .is_ok(),
            None => false,
        };
        let initial_status = if sent {
            crate::chat_store::MessageStatus::Sending
        } else {
            crate::chat_store::MessageStatus::Failed
        };

        // Persist outbound message so history replay shows it on both sides.
        let chat_msg = crate::chat_store::ChatMessage {
            id: message_id.clone(),
            peer_id: pid.clone(),
            sender: handle.clone(),
            recipient: pid.clone(),
            body: body.clone(),
            timestamp: now_ts,
            is_self: true,
            status: initial_status.clone(),
            kind: crate::chat_store::MessageKind::Text,
            attachment_name: String::new(),
            attachment_path: String::new(),
            size_str: String::new(),
            status_note: String::new(),
            sender_handle: handle.clone(),
        };
        if let Some(ref cs) = chat_store_opt {
            if let Err(e) = cs.insert(&chat_msg) {
                warn!("chat_store insert (outbound) error: {e}");
            }
        }

        // Local echo: emit immediately so the sender sees their own message.
        let echo_json = serde_json::json!({
            "msg_id": message_id,
            "peer_id": pid,
            "sender": handle,
            "body": body,
            "timestamp": now_ts as i64,
            "kind": "text",
            "mine": true,
            "status": initial_status.as_str(),
        })
        .to_string();
        if self.rust().selected_peer_id == pid {
            self.as_mut()
                .chat_message_received(QString::from(echo_json.as_str()));
        }
    }

    fn start_call(mut self: Pin<&mut Self>, peer_id: &QString) {
        use crate::protocol::{MessageType, SignalingMessage};

        let pid = peer_id.to_string();
        let sender = self.rust().my_public_id.clone();

        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let mut msg = SignalingMessage::new(MessageType::CallRequest, sender);
            msg.target = Some(pid.clone());
            let _ = tx.try_send(ConnectionCommand::SendMessage(msg));
        }
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::InitiatePeer {
                peer_id: pid.clone(),
                host: None,
                port: None,
            });
        }
        self.as_mut().set_call_state(QString::from("connecting"));
        self.as_mut().set_voice_active(true);
        {
            let resolved = lookup_list_peer_id(self.rust(), &pid);
            set_active_direct_call_presence(&mut self.as_mut().rust_mut(), &pid, true, resolved);
        }
        emit_peers_updated(self.as_mut());
    }

    fn copy_invite(mut self: Pin<&mut Self>) {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let Some(ref identity) = self.rust().identity else {
            warn!("copy_invite: identity not yet unlocked");
            return;
        };
        if let Some(url) = request_invite_url(self.rust()) {
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(url.clone())) {
                Ok(_) => info!("Invite link copied to clipboard: {url}"),
                Err(e) => warn!("Clipboard write failed: {e} - invite URL: {url}"),
            }
            self.as_mut().set_invite_url(QString::from(url.as_str()));
            return;
        }
        let peer_id = identity.peer_id().to_owned();
        let pub_key = identity.public_id().to_owned();

        // Build a minimal signed invite URL.
        // Format: conquerd://invite#<base64url(JSON)>
        let invite_id = uuid::Uuid::new_v4().to_string();
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + 900; // 15-minute TTL

        let payload = serde_json::json!({
            "inviter_peer_id": peer_id,
            "inviter_identity_pub": pub_key,
            "invite_id": invite_id,
            "expires_at": expires_at,
        });
        let encoded = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        let url = format!("conquerd://invite#{encoded}");

        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(url.clone())) {
            Ok(_) => info!("Invite link copied to clipboard: {url}"),
            Err(e) => warn!("Clipboard write failed: {e} — invite URL: {url}"),
        }
        self.as_mut().set_invite_url(QString::from(url.as_str()));
    }

    fn generate_invite(mut self: Pin<&mut Self>) -> QString {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let Some(ref identity) = self.rust().identity else {
            warn!("generate_invite: identity not yet unlocked");
            return QString::default();
        };
        if let Some(url) = request_invite_url(self.rust()) {
            self.as_mut().set_invite_url(QString::from(url.as_str()));
            return QString::from(url.as_str());
        }
        let peer_id = identity.peer_id().to_owned();
        let pub_key = identity.public_id().to_owned();

        let invite_id = uuid::Uuid::new_v4().to_string();
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + 900;

        let payload = serde_json::json!({
            "inviter_peer_id": peer_id,
            "inviter_identity_pub": pub_key,
            "invite_id": invite_id,
            "expires_at": expires_at,
        });
        let encoded = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        let url = format!("conquerd://invite#{encoded}");
        self.as_mut().set_invite_url(QString::from(url.as_str()));
        QString::from(url.as_str())
    }

    fn copy_to_clipboard(self: Pin<&mut Self>, text: &QString) {
        let s = text.to_string();
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(s.clone())) {
            Ok(_) => debug!("[bridge] Copied {} bytes to clipboard", s.len()),
            Err(e) => warn!("[bridge] Clipboard write failed: {e}"),
        }
    }

    fn start_mic_test(mut self: Pin<&mut Self>) {
        if let Some(ref tx) = self.rust().call_cmd_tx {
            // Push the most recently persisted device selection so the test
            // uses the same output device the user picked in Settings.
            let (input, output) = read_audio_device_settings();
            let _ =
                tx.try_send(crate::call_controller::CallCommand::SetAudioDevices { input, output });
            let _ = tx.try_send(crate::call_controller::CallCommand::StartMicTest);
        }
        self.as_mut().set_mic_test_active(true);
    }

    fn stop_mic_test(mut self: Pin<&mut Self>) {
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(crate::call_controller::CallCommand::StopMicTest);
        }
        self.as_mut().set_mic_test_active(false);
        self.as_mut().set_mic_level(0.0);
    }

    fn set_audio_devices(self: Pin<&mut Self>, input: &QString, output: &QString) {
        let in_s = input.to_string();
        let out_s = output.to_string();
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(crate::call_controller::CallCommand::SetAudioDevices {
                input: Some(in_s),
                output: Some(out_s),
            });
        }
    }

    fn test_speaker(self: Pin<&mut Self>) {
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(crate::call_controller::CallCommand::TestSpeaker);
        }
    }

    fn create_desktop_shortcuts(self: Pin<&mut Self>) {
        crate::platform::create_desktop_shortcuts();
    }

    fn remove_desktop_shortcuts(self: Pin<&mut Self>) {
        crate::platform::remove_desktop_shortcuts();
    }

    fn has_desktop_shortcuts(self: Pin<&mut Self>) -> bool {
        crate::platform::has_desktop_shortcuts()
    }

    fn list_audio_devices(self: Pin<&mut Self>) -> QString {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();

        let inputs: Vec<String> = std::iter::once("Default".to_string())
            .chain(
                host.input_devices()
                    .map(|it| it.filter_map(|d| d.name().ok()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            )
            .collect();

        let outputs: Vec<String> = std::iter::once("Default".to_string())
            .chain(
                host.output_devices()
                    .map(|it| it.filter_map(|d| d.name().ok()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            )
            .collect();

        let json = serde_json::json!({ "inputs": inputs, "outputs": outputs });
        QString::from(json.to_string().as_str())
    }

    fn load_room_chat_history(mut self: Pin<&mut Self>, supernode_id: &QString, room_id: &QString) {
        let Some(sn) = self
            .rust()
            .resolve_supernode_node_id_str(&supernode_id.to_string())
        else {
            return;
        };
        let rid = room_id.to_string();
        let key = room_chat_history_key(&sn, &rid);
        if let Some(ref cs) = self.rust().chat_store {
            let store_key = room_chat_store_peer_id(&sn, &rid);
            let msgs: Vec<serde_json::Value> = cs
                .get_history(&store_key, 0)
                .map(|rows| rows.iter().map(room_chat_message_to_json).collect())
                .unwrap_or_default();
            for msg in msgs {
                let json = msg.to_string();
                self.as_mut()
                    .room_chat_received(QString::from(json.as_str()));
            }
            return;
        }
        let msgs: Vec<String> = self
            .rust()
            .room_chat_history
            .get(&key)
            .cloned()
            .unwrap_or_default();
        for msg in msgs {
            self.as_mut()
                .room_chat_received(QString::from(msg.as_str()));
        }
    }

    fn resolve_supernode_node_id(self: Pin<&mut Self>, node_id: &QString) -> QString {
        let resolved = self
            .rust()
            .resolve_supernode_node_id_str(&node_id.to_string())
            .unwrap_or_default();
        QString::from(resolved.as_str())
    }

    fn is_known_supernode(self: Pin<&mut Self>, node_id: &QString) -> bool {
        let id = node_id.to_string();
        self.rust()
            .peer_store
            .as_ref()
            .map(|ps| ps.read().is_supernode_id(&id))
            .unwrap_or(false)
    }

    fn delete_message(mut self: Pin<&mut Self>, msg_id: &QString) {
        let id = msg_id.to_string();
        let cs_opt: Option<Arc<crate::chat_store::ChatStore>> =
            self.rust().chat_store.as_ref().map(Arc::clone);
        if let Some(cs) = cs_opt {
            if let Err(e) = cs.delete_message(&id) {
                warn!("delete_message: {e}");
                return;
            }
        }
        {
            let buf = &mut self.as_mut().rust_mut().event_log;
            if buf.len() >= 300 {
                buf.pop_front();
            }
            buf.push_back(format!("Message deleted: {id}"));
        }
        self.as_mut().message_deleted(QString::from(id.as_str()));
    }

    fn retry_message(mut self: Pin<&mut Self>, msg_id: &QString) {
        use crate::protocol::{MessageType, SignalingMessage};

        let id = msg_id.to_string();
        let Some(cs) = self.rust().chat_store.as_ref().map(Arc::clone) else {
            return;
        };
        let Ok(Some(msg)) = cs.get_by_id(&id) else {
            return;
        };
        if !msg.is_self
            || msg.peer_id.is_empty()
            || msg.kind != crate::chat_store::MessageKind::Text
        {
            return;
        }

        let mut outbound =
            SignalingMessage::new(MessageType::ChatMessage, self.rust().my_public_id.clone());
        outbound.target = Some(msg.peer_id.clone());
        outbound.payload.insert(
            "body".to_string(),
            serde_json::Value::String(msg.body.clone()),
        );
        outbound.payload.insert(
            "message_id".to_string(),
            serde_json::Value::String(msg.id.clone()),
        );
        outbound.payload.insert(
            "sender_handle".to_string(),
            serde_json::Value::String(msg.sender_handle.clone()),
        );

        let sent = match self.rust().conn_cmd_tx {
            Some(ref tx) => tx
                .try_send(ConnectionCommand::SendMessage(outbound))
                .is_ok(),
            None => false,
        };
        let status = if sent {
            crate::chat_store::MessageStatus::Sending
        } else {
            crate::chat_store::MessageStatus::Failed
        };
        if let Err(e) = cs.update_status_note(&id, status.clone(), "") {
            warn!("retry_message: status update failed: {e}");
        }
        self.as_mut()
            .message_status_changed(QString::from(id.as_str()), QString::from(status.as_str()));
    }

    fn clear_peer_history(mut self: Pin<&mut Self>, peer_id: &QString) {
        let pid = peer_id.to_string();
        let cs_opt: Option<Arc<crate::chat_store::ChatStore>> =
            self.rust().chat_store.as_ref().map(Arc::clone);
        if let Some(cs) = cs_opt {
            if let Err(e) = cs.clear_history(&pid) {
                warn!("clear_peer_history: {e}");
                return;
            }
        }
        {
            let buf = &mut self.as_mut().rust_mut().event_log;
            if buf.len() >= 300 {
                buf.pop_front();
            }
            buf.push_back(format!("Chat history cleared for peer {pid}"));
        }
        self.as_mut()
            .peer_history_cleared(QString::from(pid.as_str()));
    }

    fn get_event_logs(self: Pin<&mut Self>) -> QString {
        let text = self
            .rust()
            .event_log
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        QString::from(text.as_str())
    }

    fn clear_event_logs(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().event_log.clear();
    }

    fn get_stored_message_count(self: Pin<&mut Self>) -> i64 {
        if let Some(ref cs) = self.rust().chat_store {
            cs.total_count().unwrap_or(0) as i64
        } else {
            0
        }
    }

    fn trim_messages_by_age(self: Pin<&mut Self>, days: i32) {
        if let Some(ref cs) = self.rust().chat_store {
            match cs.trim_by_age(days) {
                Ok(n) => info!("Trimmed {n} messages older than {days} days"),
                Err(e) => warn!("trim_by_age failed: {e}"),
            }
        }
    }

    fn trim_messages_by_count(self: Pin<&mut Self>, keep: i32) {
        if let Some(ref cs) = self.rust().chat_store {
            match cs.trim_by_count(keep) {
                Ok(n) => info!("Trimmed {n} messages (kept {keep} per peer)"),
                Err(e) => warn!("trim_by_count failed: {e}"),
            }
        }
    }

    fn purge_all_chat_history(self: Pin<&mut Self>) {
        if let Some(ref cs) = self.rust().chat_store {
            match cs.purge_all() {
                Ok(n) => info!("Purged {n} messages"),
                Err(e) => warn!("purge_all failed: {e}"),
            }
        }
    }

    fn lock_identity_and_quit(self: Pin<&mut Self>) {
        if let Some(ref id) = self.rust().identity {
            crate::identity::keyring_delete_aes_key(&id.public_id());
            info!("Identity locked — keyring entry removed");
        }
        std::process::exit(0);
    }

    fn paste_invite(self: Pin<&mut Self>, url: &QString) {
        let url_str = url.to_string();
        if url_str.is_empty() {
            return;
        }
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::AcceptInvite {
                invite_url: url_str,
            });
        }
    }

    fn accept_call(mut self: Pin<&mut Self>, peer_id: &QString) {
        use crate::protocol::{MessageType, SignalingMessage};
        let pid = peer_id.to_string();
        // Notify the caller that we accepted.
        let sender = self.rust().my_public_id.clone();
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let mut msg = SignalingMessage::new(MessageType::CallAccept, sender);
            msg.target = Some(pid.clone());
            let _ = tx.try_send(ConnectionCommand::SendMessage(msg));
        }
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let va = read_voice_activation_setting();
            let _ = tx.try_send(CallCommand::StartAudio {
                voice_activation: va,
            });
            let _ = tx.try_send(CallCommand::InitiatePeer {
                peer_id: pid.clone(),
                host: None,
                port: None,
            });
        }
        self.as_mut().set_call_state(QString::from("in_call"));
        self.as_mut().set_voice_active(true);
        {
            let resolved = lookup_list_peer_id(self.rust(), &pid);
            set_active_direct_call_presence(&mut self.as_mut().rust_mut(), &pid, true, resolved);
        }
        emit_peers_updated(self.as_mut());
    }

    fn reject_call(self: Pin<&mut Self>, peer_id: &QString) {
        use crate::protocol::{MessageType, SignalingMessage};
        let pid = peer_id.to_string();
        let sender = self.rust().my_public_id.clone();
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let mut msg = SignalingMessage::new(MessageType::CallEnd, sender);
            msg.target = Some(pid);
            let _ = tx.try_send(ConnectionCommand::SendMessage(msg));
        }
    }

    fn join_room(mut self: Pin<&mut Self>, supernode_id: &QString, room_id: &QString) {
        let Some(sid) = self
            .rust()
            .resolve_supernode_node_id_str(&supernode_id.to_string())
        else {
            warn!(
                "[bridge] joinRoom: unknown supernode {}",
                supernode_id.to_string()
            );
            return;
        };
        let rid = room_id.to_string();

        // If we're already in a different room on the same/different supernode,
        // leave it first so the supernode stops sending chats from the old room.
        let prev_sn = self.rust().current_supernode_id.clone();
        let prev_rid = self.rust().current_room_id.clone();
        if !prev_rid.is_empty() && (prev_rid != rid || prev_sn != sid) {
            if let Some(ref tx) = self.rust().conn_cmd_tx {
                let _ = tx.try_send(ConnectionCommand::LeaveRoom {
                    supernode_id: prev_sn,
                    room_id: prev_rid.clone(),
                });
            }
        }

        let stored = self
            .rust()
            .room_store
            .as_ref()
            .and_then(|rs| rs.read().get(&sid, &rid).cloned());
        let room_type = stored
            .as_ref()
            .map(|e| e.room_type.clone())
            .unwrap_or_else(|| "public".to_owned());
        let use_invite = stored.as_ref().is_some_and(|e| {
            e.room_type == "private" && !e.is_creator && !e.invite_token.is_empty()
        });
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            if use_invite {
                let token = stored
                    .as_ref()
                    .map(|e| e.invite_token.clone())
                    .unwrap_or_default();
                let _ = tx.try_send(ConnectionCommand::JoinRoomWithInvite {
                    supernode_id: sid.clone(),
                    room_id: rid.clone(),
                    invite_token: token,
                });
            } else {
                let _ = tx.try_send(ConnectionCommand::JoinRoom {
                    supernode_id: sid.clone(),
                    room_id: rid.clone(),
                });
            }
        }
        {
            let mut r = self.as_mut().rust_mut();
            r.current_supernode_id = sid.clone();
            r.current_room_id = rid.clone();
        }
        remember_room_in_store(
            &self.rust().room_store,
            &sid,
            &rid,
            "",
            &room_type,
            "",
            false,
            "",
        );

        // Immediately seed room_participant_ids with just the local peer so
        // that any stale RoomPeerLeft closures already queued to the Qt thread
        // (from the previous room session) emit [self] rather than []. Without
        // this seed those closures re-emit the empty list that was left by
        // leave_room(), wiping the model before the authoritative SfuMembers
        // round-trip completes — the "join room, avatar missing" race.
        let my_public_id = self.rust().my_public_id.clone();
        let my_peer_id = self.rust().my_peer_id.clone();
        if !my_public_id.is_empty() {
            self.as_mut().rust_mut().room_participant_ids = vec![my_public_id.clone()];
            let json = if let Some(ps) = self.rust().peer_store.as_ref() {
                room_participants_json(
                    Some(&ps.read()),
                    std::slice::from_ref(&my_public_id),
                    &my_peer_id,
                    &my_public_id,
                )
            } else {
                room_participants_json(
                    None,
                    std::slice::from_ref(&my_public_id),
                    &my_peer_id,
                    &my_public_id,
                )
            };
            self.as_mut()
                .participants_updated(QString::from(json.as_str()));
        }

        self.as_mut().set_in_room(true);
    }

    fn join_room_with_invite(
        mut self: Pin<&mut Self>,
        supernode_id: &QString,
        room_id: &QString,
        invite_token: &QString,
    ) {
        let Some(sid) = self
            .rust()
            .resolve_supernode_node_id_str(&supernode_id.to_string())
        else {
            warn!(
                "[bridge] joinRoomWithInvite: unknown supernode {}",
                supernode_id.to_string()
            );
            return;
        };
        let rid = room_id.to_string();
        let token = invite_token.to_string();
        if sid.is_empty() || rid.is_empty() || token.is_empty() {
            return;
        }

        let prev_sn = self.rust().current_supernode_id.clone();
        let prev_rid = self.rust().current_room_id.clone();
        if !prev_rid.is_empty() && (prev_rid != rid || prev_sn != sid) {
            if let Some(ref tx) = self.rust().conn_cmd_tx {
                let _ = tx.try_send(ConnectionCommand::LeaveRoom {
                    supernode_id: prev_sn,
                    room_id: prev_rid.clone(),
                });
            }
        }

        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::JoinRoomWithInvite {
                supernode_id: sid.clone(),
                room_id: rid.clone(),
                invite_token: token,
            });
        }
        {
            let mut r = self.as_mut().rust_mut();
            r.current_supernode_id = sid.clone();
            r.current_room_id = rid.clone();
        }
        remember_room_in_store(
            &self.rust().room_store,
            &sid,
            &rid,
            "",
            "private",
            "",
            false,
            "",
        );

        let my_public_id = self.rust().my_public_id.clone();
        let my_peer_id = self.rust().my_peer_id.clone();
        if !my_public_id.is_empty() {
            self.as_mut().rust_mut().room_participant_ids = vec![my_public_id.clone()];
            let json = if let Some(ps) = self.rust().peer_store.as_ref() {
                room_participants_json(
                    Some(&ps.read()),
                    std::slice::from_ref(&my_public_id),
                    &my_peer_id,
                    &my_public_id,
                )
            } else {
                room_participants_json(
                    None,
                    std::slice::from_ref(&my_public_id),
                    &my_peer_id,
                    &my_public_id,
                )
            };
            self.as_mut()
                .participants_updated(QString::from(json.as_str()));
        }

        self.as_mut().set_in_room(true);
    }

    fn join_room_with_voice(mut self: Pin<&mut Self>, supernode_id: &QString, room_id: &QString) {
        let Some(new_sid) = self
            .rust()
            .resolve_supernode_node_id_str(&supernode_id.to_string())
        else {
            return;
        };
        let new_rid = room_id.to_string();
        if new_sid.is_empty() || new_rid.is_empty() {
            return;
        }

        // Voice may still be on a different supernode than the chat selection
        // (subscribe_room_chat updates `current_*` without leaving voice).
        let (prev_voice_sn, prev_voice_rid) = {
            let r = self.rust();
            (r.voice_supernode_id.clone(), r.voice_room_id.clone())
        };
        if self.rust().voice_active
            && !prev_voice_rid.is_empty()
            && (prev_voice_sn != new_sid || prev_voice_rid != new_rid)
        {
            if let Some(ref tx) = self.rust().conn_cmd_tx {
                let _ = tx.try_send(ConnectionCommand::LeaveRoom {
                    supernode_id: prev_voice_sn.clone(),
                    room_id: prev_voice_rid,
                });
                let _ = tx.try_send(ConnectionCommand::RequestRoomList {
                    supernode_id: prev_voice_sn,
                });
            }
        }

        // Join signaling for the new room (chat context + voice).
        self.as_mut().join_room(supernode_id, room_id);

        {
            let mut r = self.as_mut().rust_mut();
            r.voice_supernode_id = new_sid.clone();
            r.voice_room_id = new_rid.clone();
        }

        // Switch call controller to SFU room audio mode so outbound frames
        // are routed via the supernode WebSocket instead of direct QUIC.
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::SetRoomMode {
                supernode_id: new_sid,
                room_id: new_rid,
            });
        }
        // Then start the local audio pipeline.
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let va = read_voice_activation_setting();
            let _ = tx.try_send(CallCommand::StartAudio {
                voice_activation: va,
            });
        }
        self.as_mut().set_voice_active(true);
    }

    fn subscribe_room_chat(mut self: Pin<&mut Self>, supernode_id: &QString, room_id: &QString) {
        let Some(sid) = self
            .rust()
            .resolve_supernode_node_id_str(&supernode_id.to_string())
        else {
            return;
        };
        let rid = room_id.to_string();
        if sid.is_empty() || rid.is_empty() {
            return;
        }
        // Send SfuSubscribe so the supernode delivers chat messages to us
        // without making us a voice participant or leaving the current voice room.
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::SubscribeRoomChat {
                supernode_id: sid.clone(),
                room_id: rid.clone(),
            });
        }
        // Point send_room_chat at the newly-selected chat room.
        {
            let mut r = self.as_mut().rust_mut();
            r.current_supernode_id = sid.clone();
            r.current_room_id = rid.clone();
        }
        let room_type = match self.rust().room_store.as_ref() {
            Some(rs) => rs
                .read()
                .get(&sid, &rid)
                .map(|e| e.room_type.clone())
                .unwrap_or_else(|| "public".to_owned()),
            None => "public".to_owned(),
        };
        remember_room_in_store(
            &self.rust().room_store,
            &sid,
            &rid,
            "",
            &room_type,
            "",
            false,
            "",
        );
    }

    fn remove_room(mut self: Pin<&mut Self>, supernode_id: &QString, room_id: &QString) {
        let Some(sid) = self
            .rust()
            .resolve_supernode_node_id_str(&supernode_id.to_string())
        else {
            return;
        };
        let rid = room_id.to_string();
        if sid.is_empty() || rid.is_empty() || rid == "default" {
            return;
        }
        if let Some(ref rs) = self.rust().room_store {
            if let Err(e) = rs.write().hide_from_sidebar(&sid, &rid) {
                warn!("room_store hide_from_sidebar error: {e}");
            }
        }
        let in_this_room =
            self.rust().current_supernode_id == sid && self.rust().current_room_id == rid;
        if in_this_room {
            self.as_mut().leave_room();
            {
                let mut r = self.as_mut().rust_mut();
                r.current_supernode_id.clear();
                r.current_room_id.clear();
            }
            self.as_mut()
                .set_session_banner(QString::from("Offline \u{00b7} Room hidden"));
            self.as_mut().set_connection_mode(QString::from("offline"));
        }
        self.as_mut()
            .room_removed(QString::from(sid.as_str()), QString::from(rid.as_str()));
    }

    fn create_room(
        self: Pin<&mut Self>,
        supernode_id: &QString,
        room_name: &QString,
        room_type: &QString,
    ) {
        let Some(sid) = self
            .rust()
            .resolve_supernode_node_id_str(&supernode_id.to_string())
        else {
            return;
        };
        let name = room_name.to_string();
        let normalized = match room_type.to_string().trim().to_ascii_lowercase().as_str() {
            "private" => "private",
            _ => "public",
        };
        if name.trim().is_empty() {
            return;
        }
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::CreateRoom {
                supernode_id: sid,
                room_name: name,
                room_type: normalized.to_owned(),
                room_id: None,
                creator_id: None,
                materialize_only: false,
            });
        }
    }

    fn register_uri_scheme(self: Pin<&mut Self>) {
        match crate::uri_scheme::register() {
            Ok(true) => info!("[bridge] conquerd:// URI scheme registered"),
            Ok(false) => info!("[bridge] URI scheme registration not available on this platform"),
            Err(e) => warn!("[bridge] URI scheme register error: {e}"),
        }
    }

    fn unregister_uri_scheme(self: Pin<&mut Self>) {
        match crate::uri_scheme::unregister() {
            Ok(true) => info!("[bridge] conquerd:// URI scheme unregistered"),
            Ok(false) => {}
            Err(e) => warn!("[bridge] URI scheme unregister error: {e}"),
        }
    }

    fn open_node_portal(mut self: Pin<&mut Self>, supernode_id: &QString) {
        let Some(sn_id) = self
            .rust()
            .resolve_supernode_node_id_str(&supernode_id.to_string())
        else {
            warn!(
                "[bridge] openNodePortal: unknown supernode {}",
                supernode_id.to_string()
            );
            return;
        };
        info!("[bridge] open_node_portal called sn={}", sn_id);
        // Request a relay slot — the ConnectionManager will open the QUIC
        // relay connection on RelayGranted, making the scheme handler ready.
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::RequestRelay {
                supernode_id: sn_id.clone(),
            });
        }
        // Emit the navigate signal immediately so the panel is shown and
        // shows a "Loading…" spinner while the relay connects. The first
        // real page fetch will block in `conquerd_fetch_sync` until the
        // connection is established.
        //
        // Chromium lower-cases the authority of any `scheme://` URL, which
        // would destroy our case-sensitive base64url peer ID.  Register a
        // `{lowercase → original}` mapping before navigating so the scheme
        // handler can recover the canonical peer ID at fetch time.
        #[cfg(feature = "webengine")]
        crate::ui::scheme::register_portal_peer_id(&sn_id);
        let url = format!("conquerd://{}/", sn_id);
        self.as_mut()
            .navigate_node_portal(QString::from(sn_id.as_str()), QString::from(url.as_str()));
    }

    fn apply_update(self: Pin<&mut Self>) {
        if let Some(ref release) = self.rust().pending_release {
            if let Some(ref tx) = self.rust().updater_cmd_tx {
                let _ = tx.try_send(crate::github_updater::UpdaterCommand::ApplyUpdate(
                    release.clone(),
                ));
                info!("Applying update to {}", release.tag_name);
            }
        } else {
            warn!("apply_update: no pending release");
        }
    }

    fn set_muted(self: Pin<&mut Self>, muted: bool) {
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::SetMuted(muted));
        }
    }

    fn set_voice_activation(self: Pin<&mut Self>, enabled: bool) {
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::SetVoiceActivation(enabled));
            // VAD mode keeps the mic open; PTT mode mutes until the key is held.
            let _ = tx.try_send(CallCommand::SetMuted(!enabled));
        }
    }

    fn set_jitter_depth(self: Pin<&mut Self>, depth: i32) {
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::SetJitterDepth(depth.max(1) as usize));
        }
    }

    fn set_noise_strength(self: Pin<&mut Self>, level: &QString) {
        let idx = match level.to_string().to_lowercase().as_str() {
            "off" => 0u32,
            "mild" => 1,
            "moderate" => 2,
            "aggressive" => 3,
            "max" => 4,
            _ => 2, // default to moderate for unknown values
        };
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::SetNoiseStrength(idx));
        }
    }

    fn set_input_volume(self: Pin<&mut Self>, pct: i32) {
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::SetInputGain(pct.clamp(0, 200) as u32));
        }
    }

    fn set_output_volume(self: Pin<&mut Self>, pct: i32) {
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::SetOutputGain(pct.clamp(0, 200) as u32));
        }
    }

    fn set_voice_bitrate(self: Pin<&mut Self>, preset: &QString) {
        let bitrate = voice_bitrate_preset_to_bps(&preset.to_string());
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::SetOutgoingBitrate(bitrate));
        }
    }

    fn clear_unread(mut self: Pin<&mut Self>) {
        let global = self
            .rust()
            .chat_store
            .as_ref()
            .and_then(|cs| cs.total_unread_count().ok())
            .unwrap_or(0);
        self.as_mut().rust_mut().unread_chat = global as u32;
        if global == 0 {
            crate::platform::clear_taskbar_badge();
        } else {
            crate::platform::set_taskbar_badge(global as u32);
        }
    }

    fn avatar_svg(self: Pin<&mut Self>, peer_id: &QString, config_json: &QString) -> QString {
        use super::avatar::build_avatar_svg;

        let (id, config) = resolve_avatar_config(self.rust(), peer_id, config_json);
        QString::from(build_avatar_svg(&id, &config).as_str())
    }

    fn avatar_tint_color(
        self: Pin<&mut Self>,
        peer_id: &QString,
        config_json: &QString,
    ) -> QString {
        use super::avatar::avatar_tint_hex;

        let (id, config) = resolve_avatar_config(self.rust(), peer_id, config_json);
        QString::from(avatar_tint_hex(&id, &config).as_str())
    }

    fn avatar_image_smooth(self: Pin<&mut Self>, peer_id: &QString, config_json: &QString) -> bool {
        let (_, config) = resolve_avatar_config(self.rust(), peer_id, config_json);
        !config.svg_crisp
    }

    fn broadcast_avatar_config(self: Pin<&mut Self>, peer_id: &QString, config_json: &QString) {
        let pid = peer_id.to_string();
        let cfg = config_json.to_string();
        if cfg.is_empty() || pid.is_empty() {
            return;
        }
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::BroadcastAvatarConfig {
                peer_id: pid,
                config_json: cfg,
            });
        }
    }

    fn broadcast_avatar_config_to_all(self: Pin<&mut Self>, config_json: &QString) {
        let cfg = config_json.to_string();
        if cfg.is_empty() {
            return;
        }
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::BroadcastAvatarConfigToAll { config_json: cfg });
        }
    }

    fn set_avatar_config_json(mut self: Pin<&mut Self>, config_json: &QString) {
        self.as_mut().rust_mut().avatar_config_json = config_json.to_string();
        let pid = self.rust().public_id.clone();
        if !pid.is_empty() {
            self.as_mut().avatar_config_updated(pid);
        }
    }

    fn remove_peer(self: Pin<&mut Self>, peer_id: &QString) {
        let pid = peer_id.to_string();
        // Remove from in-memory + persisted peer store
        if let Some(ref ps) = self.rust().peer_store {
            let mut store = ps.write();
            if store.remove_by_any_id(&pid).is_some() {
                let _ = store.save();
            }
        }
        // Disconnect any active audio session
        if let Some(ref tx) = self.rust().call_cmd_tx {
            let _ = tx.try_send(CallCommand::RemovePeer {
                peer_id: pid.clone(),
            });
        }
        info!("Peer removed: {pid}");
    }

    fn remove_supernode(mut self: Pin<&mut Self>, node_id: &QString) {
        let id = node_id.to_string();
        let Some(canon) = self.rust().resolve_supernode_node_id_str(&id) else {
            warn!("remove_supernode: unknown node {id}");
            return;
        };

        {
            let ps = self.rust().peer_store.clone();
            let Some(ps) = ps else {
                warn!("remove_supernode: peer store unavailable");
                return;
            };
            let mut store = ps.write();
            if !store.is_supernode_id(&canon) {
                warn!("remove_supernode: not a supernode {canon}");
                return;
            }
            if store.remove_by_any_id(&canon).is_none() {
                return;
            }
            if let Err(e) = store.save() {
                warn!("remove_supernode: failed to persist peer store: {e}");
            }
        }

        let voice_on_removed = self.rust().voice_supernode_id == canon;
        if voice_on_removed {
            let leaving_voice = self.rust().voice_room_id.clone();
            if let Some(ref tx) = self.rust().conn_cmd_tx {
                if !leaving_voice.is_empty() {
                    let _ = tx.try_send(ConnectionCommand::LeaveRoom {
                        supernode_id: canon.clone(),
                        room_id: leaving_voice,
                    });
                }
            }
            if let Some(ref tx) = self.rust().call_cmd_tx {
                let _ = tx.try_send(CallCommand::ClearRoomMode);
                let _ = tx.try_send(CallCommand::StopAudio);
            }
            let mut r = self.as_mut().rust_mut();
            r.voice_supernode_id.clear();
            r.voice_room_id.clear();
            r.room_participant_ids.clear();
            self.as_mut().set_voice_active(false);
        }
        if self.rust().current_supernode_id == canon {
            let mut r = self.as_mut().rust_mut();
            r.current_supernode_id.clear();
            r.current_room_id.clear();
            self.as_mut().set_in_room(false);
        }

        {
            let prefix = format!("{canon}:");
            self.as_mut()
                .rust_mut()
                .room_chat_history
                .retain(|k, _| !k.starts_with(&prefix));
        }

        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::RemoveSupernode {
                supernode_id: canon.clone(),
            });
        }

        self.as_mut()
            .supernode_removed(QString::from(canon.as_str()));
        emit_rooms_sidebar_sync(self.as_mut());
        emit_local_rooms_for_all_supernodes(self.as_mut());
        info!("Supernode removed: {canon}");
    }

    fn enable_ptt(mut self: Pin<&mut Self>, key: &QString) {
        // Stop any existing PTT thread first
        if let Some(ref stop) = self.rust().ptt_stop {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let key_str = key.to_string();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let (ptt_tx, ptt_rx) = std::sync::mpsc::sync_channel::<bool>(4);
        let call_tx = self.rust().call_cmd_tx.clone();
        std::thread::spawn(move || {
            while let Ok(muted) = ptt_rx.recv() {
                if let Some(ref tx) = call_tx {
                    let _ = tx.try_send(CallCommand::SetMuted(muted));
                }
            }
        });
        let handle = crate::platform::start_ptt_polling(key_str, ptt_tx, stop2);
        let mut r = self.as_mut().rust_mut();
        r.ptt_stop = Some(stop);
        r.ptt_thread = Some(handle);
        info!("PTT enabled: key={}", key.to_string());
    }

    fn disable_ptt(mut self: Pin<&mut Self>) {
        if let Some(ref stop) = self.rust().ptt_stop {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let mut r = self.as_mut().rust_mut();
        r.ptt_stop = None;
        r.ptt_thread = None;
        info!("PTT disabled");
    }

    fn block_peer(self: Pin<&mut Self>, peer_id: &QString) {
        let pid = peer_id.to_string();
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::BlockPeer {
                peer_id: pid.clone(),
            });
        }
        info!("Blocking peer: {pid}");
    }

    fn unblock_peer(self: Pin<&mut Self>, peer_id: &QString) {
        let pid = peer_id.to_string();
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::UnblockPeer {
                peer_id: pid.clone(),
            });
        }
        info!("Unblocking peer: {pid}");
    }

    fn copy_peer_id(self: Pin<&mut Self>, peer_id: &QString) {
        let pid = peer_id.to_string();
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(pid.clone())) {
            Ok(_) => info!("Peer ID copied to clipboard"),
            Err(e) => warn!("Clipboard write failed for peer ID: {e}"),
        }
    }

    fn select_peer(mut self: Pin<&mut Self>, peer_id: &QString) {
        let pid = peer_id.to_string();
        self.as_mut().rust_mut().selected_peer_id = pid.clone();

        // Clone the Arc so the borrow on self ends before the for-loop below.
        let cs_opt: Option<Arc<crate::chat_store::ChatStore>> =
            self.rust().chat_store.as_ref().map(Arc::clone);

        if let Some(ref cs) = cs_opt {
            if let Err(e) = cs.mark_peer_read(&pid) {
                warn!("chat_store mark_peer_read error: {e}");
            }
            let global = cs.total_unread_count().unwrap_or(0);
            self.as_mut().rust_mut().unread_chat = global as u32;
            if global == 0 {
                crate::platform::clear_taskbar_badge();
            } else {
                crate::platform::set_taskbar_badge(global as u32);
            }
            self.as_mut().unread_changed(QString::from(pid.as_str()), 0);
        }

        // Build the full history JSON array then emit a single chatHistoryLoaded
        // signal. The QML side wires this to chatModel.setMessages() which does
        // an atomic beginResetModel/clear/endResetModel, preventing stale messages
        // from the previously-selected peer from persisting in the view.
        let msgs: Vec<serde_json::Value> = cs_opt
            .and_then(|cs| cs.get_history(&pid, 0).ok())
            .unwrap_or_default()
            .iter()
            .map(chat_message_to_json)
            .collect();

        let array_json = serde_json::Value::Array(msgs).to_string();
        self.as_mut()
            .chat_history_loaded(QString::from(array_json.as_str()));
    }

    fn load_more_history(mut self: Pin<&mut Self>, peer_id: &QString, page: i32) {
        let pid = peer_id.to_string();
        let page = page.max(0) as usize;
        let msgs: Vec<serde_json::Value> = self
            .rust()
            .chat_store
            .as_ref()
            .and_then(|cs| cs.get_history(&pid, page).ok())
            .unwrap_or_default()
            .iter()
            .map(chat_message_to_json)
            .collect();
        let array_json = serde_json::Value::Array(msgs).to_string();
        self.as_mut()
            .chat_history_prepended(QString::from(array_json.as_str()));
    }

    fn send_typing(self: Pin<&mut Self>, peer_id: &QString, is_typing: bool) {
        let pid = peer_id.to_string();
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::SendTyping {
                peer_id: pid,
                is_typing,
            });
        }
    }

    fn send_room_chat(mut self: Pin<&mut Self>, body: &QString) {
        let body_str = body.to_string();
        let message_id = uuid::Uuid::new_v4().to_string();
        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let (sn, rid, handle, sender_id, chat_store_opt) = {
            let r = self.rust();
            let sn = r.current_supernode_id.clone();
            let rid = r.current_room_id.clone();
            let handle = r
                .peer_store
                .as_ref()
                .and_then(|ps| {
                    let store = ps.read();
                    store.get(&r.my_peer_id).map(|rec| rec.display_name())
                })
                .unwrap_or_default();
            let sender_id = r.my_public_id.clone();
            let cs = r.chat_store.clone();
            (sn, rid, handle, sender_id, cs)
        };
        if sn.is_empty() || rid.is_empty() {
            return;
        }
        let sent = match self.rust().conn_cmd_tx {
            Some(ref tx) => tx
                .try_send(ConnectionCommand::SendSfuChat {
                    supernode_id: sn.clone(),
                    room_id: rid.clone(),
                    body: body_str.clone(),
                    sender_handle: handle.clone(),
                })
                .is_ok(),
            None => false,
        };
        let status = if sent { "sent" } else { "failed" };
        let message_status = if sent {
            crate::chat_store::MessageStatus::Sent
        } else {
            crate::chat_store::MessageStatus::Failed
        };

        let json = serde_json::json!({
            "msg_id": message_id.clone(),
            "sender": handle.clone(),
            "body": body_str.clone(),
            "timestamp": now_ts,
            "kind": "text",
            "mine": true,
            "is_room": true,
            "status": status,
        })
        .to_string();

        // Persist outbound message so loadRoomChatHistory can replay it after restart.
        if let Some(ref cs) = chat_store_opt {
            let store_key = room_chat_store_peer_id(&sn, &rid);
            let chat_msg = crate::chat_store::ChatMessage {
                id: message_id.clone(),
                peer_id: store_key,
                sender: sender_id,
                recipient: rid.clone(),
                body: body_str.clone(),
                timestamp: now_ts,
                is_self: true,
                status: message_status,
                kind: crate::chat_store::MessageKind::Text,
                attachment_name: String::new(),
                attachment_path: String::new(),
                size_str: String::new(),
                status_note: String::new(),
                sender_handle: handle.clone(),
            };
            if let Err(e) = cs.insert(&chat_msg) {
                warn!("chat_store insert (room outbound) error: {e}");
            }
        }
        if !rid.is_empty() && !sn.is_empty() {
            let key = room_chat_history_key(&sn, &rid);
            self.as_mut()
                .rust_mut()
                .room_chat_history
                .entry(key)
                .or_default()
                .push(json.clone());
        }
        self.as_mut()
            .room_chat_received(QString::from(json.as_str()));
    }

    fn accept_file(self: Pin<&mut Self>, transfer_id: &QString) {
        let tid = transfer_id.to_string();
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::AcceptFile { transfer_id: tid });
        }
    }

    fn reject_file(self: Pin<&mut Self>, transfer_id: &QString) {
        let tid = transfer_id.to_string();
        if let Some(ref tx) = self.rust().conn_cmd_tx {
            let _ = tx.try_send(ConnectionCommand::RejectFile { transfer_id: tid });
        }
    }

    fn send_file(self: Pin<&mut Self>, peer_id: &QString, file_url: &QString) {
        let pid = peer_id.to_string();
        let raw = file_url.to_string();
        // Accept both file:// URIs and plain absolute paths
        let path_str = if let Some(stripped) = raw.strip_prefix("file:///") {
            stripped.replace('/', std::path::MAIN_SEPARATOR_STR)
        } else if let Some(stripped) = raw.strip_prefix("file://") {
            stripped.replace('/', std::path::MAIN_SEPARATOR_STR)
        } else {
            raw.clone()
        };
        let path = std::path::Path::new(&path_str);
        let rel_path = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_owned();
        match std::fs::read(path) {
            Ok(data) => {
                if let Some(ref tx) = self.rust().conn_cmd_tx {
                    let _ = tx.try_send(ConnectionCommand::SendFile {
                        peer_id: pid,
                        rel_path,
                        data,
                        purpose: "file".to_owned(),
                    });
                }
            }
            Err(e) => warn!("sendFile: cannot read {:?}: {e}", path),
        }
    }

    fn send_room_file(self: Pin<&mut Self>, file_url: &QString) {
        let (sn, rid) = {
            let r = self.rust();
            (r.current_supernode_id.clone(), r.current_room_id.clone())
        };
        if sn.is_empty() || rid.is_empty() {
            return;
        }
        let raw = file_url.to_string();
        let path_str = if let Some(stripped) = raw.strip_prefix("file:///") {
            stripped.replace('/', std::path::MAIN_SEPARATOR_STR)
        } else if let Some(stripped) = raw.strip_prefix("file://") {
            stripped.replace('/', std::path::MAIN_SEPARATOR_STR)
        } else {
            raw.clone()
        };
        let path = std::path::Path::new(&path_str);
        let rel_path = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_owned();
        match std::fs::read(path) {
            Ok(data) => {
                if let Some(ref tx) = self.rust().conn_cmd_tx {
                    let _ = tx.try_send(ConnectionCommand::SendSfuFile {
                        supernode_id: sn,
                        room_id: rid,
                        rel_path,
                        data,
                        purpose: "room_file".to_owned(),
                    });
                }
            }
            Err(e) => warn!("sendRoomFile: cannot read {:?}: {e}", path),
        }
    }

    // ── Ollama invokables ────────────────────────────────────────────────

    fn ask_ollama(
        self: Pin<&mut Self>,
        request_id: &QString,
        prompt: &QString,
        system_prompt: &QString,
    ) {
        let rid = request_id.to_string();
        let pr = prompt.to_string();
        let sys = system_prompt.to_string();
        if let Some(ref tx) = self.rust().ollama_cmd_tx {
            let _ = tx.try_send(crate::ollama_module::OllamaCommand::Query {
                request_id: rid,
                prompt: pr,
                system_prompt: sys,
            });
        } else {
            warn!("ask_ollama: ollama plugin not available");
        }
    }

    fn cancel_ollama(self: Pin<&mut Self>, request_id: &QString) {
        let rid = request_id.to_string();
        if let Some(ref tx) = self.rust().ollama_cmd_tx {
            let _ = tx.try_send(crate::ollama_module::OllamaCommand::Cancel { request_id: rid });
        }
    }

    fn fetch_ollama_models(self: Pin<&mut Self>, base_url: &QString) {
        let url = {
            let s = base_url.to_string();
            if s.trim().is_empty() {
                crate::ollama_module::DEFAULT_BASE_URL.to_owned()
            } else {
                s
            }
        };

        // If the plugin task is running, route through its command channel so the
        // result arrives via the existing OllamaEvent dispatch path.
        if let Some(ref tx) = self.rust().ollama_cmd_tx {
            let _ = tx.try_send(crate::ollama_module::OllamaCommand::ListModels { base_url: url });
            return;
        }

        // Plugin disabled — do a standalone fetch and queue back via CxxQtThread.
        // Guard: QML child Component.onCompleted fires before MainWindow.onCompleted
        // calls initializeBackend(), so this can be reached before the Tokio runtime
        // exists.  Silently no-op; the settings page shows "Click ↻ to load models".
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let qt_thread = self.qt_thread();
        let client = reqwest::Client::new();
        tokio::spawn(async move {
            let (models_json, error) =
                match crate::ollama_module::fetch_model_list(&client, &url).await {
                    Ok(names) => (
                        serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_owned()),
                        String::new(),
                    ),
                    Err(e) => ("[]".to_owned(), e),
                };
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().ollama_models_ready(
                    QString::from(models_json.as_str()),
                    QString::from(error.as_str()),
                );
            });
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the effective [`AvatarConfig`] for `peer_id`, matching the trust-tier
/// rules used by `avatarSvg` / `avatarTintColor`.
fn resolve_avatar_config(
    bridge: &AppBridgeRust,
    peer_id: &QString,
    config_json: &QString,
) -> (String, super::avatar::AvatarConfig) {
    use super::avatar::{avatar_seed_id, AvatarConfig};

    let id = peer_id.to_string();
    let cfg_str = config_json.to_string();
    let seed_id = avatar_seed_id(&id, &bridge.my_public_id, &bridge.my_peer_id, |raw| {
        bridge.peer_store.as_ref().and_then(|ps| {
            let store = ps.read();
            store
                .get(raw)
                .or_else(|| store.get_by_identity(raw))
                .map(|rec| rec.identity_pub.clone())
                .filter(|pub_id| !pub_id.is_empty())
        })
    });

    let config = if !cfg_str.is_empty() {
        // Caller provided an explicit config (own-avatar preview).
        serde_json::from_str(&cfg_str).unwrap_or_default()
    } else if seed_id == bridge.my_public_id {
        // Empty configJson means "factory defaults" for the local user.
        // Do not fall back to AppBridgeRust::avatar_config_json here —
        // that field can lag behind SettingsModel during reset and would
        // keep rendering the previous custom avatar until the next edit.
        AvatarConfig::default()
    } else if let Some(ref ps) = bridge.peer_store {
        let store = ps.read();
        let rec = store
            .get(&id)
            .or_else(|| store.get_by_identity(&id))
            .or_else(|| store.get_by_identity(&seed_id));
        match rec {
            None => AvatarConfig::untrusted(),
            Some(rec) if rec.identity_pub.is_empty() => AvatarConfig::untrusted(),
            Some(rec) => rec.avatar_config.clone().unwrap_or_default(),
        }
    } else {
        AvatarConfig::untrusted()
    };

    (seed_id, config)
}

/// Read the persisted `audio_input_device` / `audio_output_device` strings
/// from the settings file.  Empty / missing fields map to `None` so the
/// CallController falls back to the host's default device.
fn read_audio_device_settings() -> (Option<String>, Option<String>) {
    let path = crate::ui::settings_model::settings_file();
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    if let Ok(txt) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            input = v
                .get("audio_input_device")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
            output = v
                .get("audio_output_device")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
        }
    }
    (input, output)
}

/// Read the persisted `voice_activation` flag from the settings file.
/// Falls back to `false` so calls default to PTT mode when the setting
/// is missing or unreadable.
fn read_voice_activation_setting() -> bool {
    let path = crate::ui::settings_model::settings_file();
    if let Ok(txt) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            return v
                .get("voice_activation")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
        }
    }
    false
}

fn voice_bitrate_preset_to_bps(preset: &str) -> u32 {
    match preset.trim().to_lowercase().as_str() {
        "low" => 32_000,
        "balanced" => 64_000,
        "high" => 96_000,
        "ultra" => 128_000,
        _ => crate::call_controller::DEFAULT_OUTGOING_BITRATE_BPS,
    }
}

/// Read the persisted outgoing voice bitrate preset from disk.
/// Defaults to Ultra (128 kbps) to preserve the current release behavior.
fn read_voice_bitrate_setting() -> u32 {
    let path = crate::ui::settings_model::settings_file();
    if let Ok(txt) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            return v
                .get("voice_bitrate")
                .and_then(|x| x.as_str())
                .map(voice_bitrate_preset_to_bps)
                .unwrap_or(crate::call_controller::DEFAULT_OUTGOING_BITRATE_BPS);
        }
    }
    crate::call_controller::DEFAULT_OUTGOING_BITRATE_BPS
}

/// Read the persisted local display handle from disk without going through QObject.
fn read_local_handle() -> String {
    let path = crate::ui::settings_model::settings_file();
    if let Ok(txt) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(handle) = v.get("local_handle").and_then(|x| x.as_str()) {
                return handle.to_owned();
            }
        }
    }
    String::new()
}

/// Read push-to-talk settings from disk without going through the QObject.
/// Returns `(ptt_enabled, ptt_key)`.
fn read_settings_for_ptt() -> (bool, String) {
    let path = crate::ui::settings_model::settings_file();
    if let Ok(txt) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            let enabled = v
                .get("push_to_talk")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let key = v
                .get("ptt_key")
                .and_then(|x| x.as_str())
                .unwrap_or("space")
                .to_owned();
            return (enabled, key);
        }
    }
    (false, "space".to_owned())
}

/// Snapshot of plugin-related fields read from the persisted settings file.
struct PluginSettings {
    ollama_enabled: bool,
    ollama_base_url: String,
    ollama_model: String,
}

/// Read plugin settings from the on-disk settings file. Falls back to
/// safe defaults when the file is missing or fields are absent.
fn read_plugin_settings() -> PluginSettings {
    let path = crate::ui::settings_model::settings_file();
    if let Ok(txt) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            return PluginSettings {
                ollama_enabled: v
                    .get("ollama_enabled")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false),
                ollama_base_url: v
                    .get("ollama_base_url")
                    .and_then(|x| x.as_str())
                    .unwrap_or(crate::ollama_module::DEFAULT_BASE_URL)
                    .to_owned(),
                ollama_model: v
                    .get("ollama_model")
                    .and_then(|x| x.as_str())
                    .unwrap_or(crate::ollama_module::DEFAULT_MODEL)
                    .to_owned(),
            };
        }
    }
    PluginSettings {
        ollama_enabled: false,
        ollama_base_url: crate::ollama_module::DEFAULT_BASE_URL.to_owned(),
        ollama_model: crate::ollama_module::DEFAULT_MODEL.to_owned(),
    }
}

/// Read the persisted `jitter_buffer_depth` (in Opus frames) from the settings
/// file.  Defaults to 3 (60 ms) when the field is absent or unreadable.
fn read_jitter_depth_setting() -> usize {
    let path = crate::ui::settings_model::settings_file();
    if let Ok(txt) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            return v
                .get("jitter_buffer_depth")
                .and_then(|x| x.as_i64())
                .map(|n| (n as usize).clamp(1, 20))
                .unwrap_or(3);
        }
    }
    3
}

// ---------------------------------------------------------------------------
// Background → Qt event dispatcher
// ---------------------------------------------------------------------------

/// Save received file data to the user's Downloads directory.
/// Returns the saved path on success (as a string).
fn save_received_file(rel_path: &str, data: &[u8]) -> Option<String> {
    use std::path::Path;
    let downloads = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir);
    let file_name = Path::new(rel_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "received_file".to_owned());
    let dest = downloads.join(&file_name);
    match std::fs::write(&dest, data) {
        Ok(()) => {
            info!("Received file saved to {}", dest.display());
            Some(dest.to_string_lossy().into_owned())
        }
        Err(e) => {
            warn!("Failed to save received file '{}': {e}", dest.display());
            None
        }
    }
}

fn chat_message_to_json(msg: &crate::chat_store::ChatMessage) -> serde_json::Value {
    serde_json::json!({
        "msg_id": msg.id,
        "peer_id": msg.peer_id,
        "sender": msg.sender_handle,
        "body": msg.body,
        "timestamp": msg.timestamp,
        "kind": msg.kind.as_str(),
        "mine": msg.is_self,
        "status": msg.status.as_str(),
    })
}

fn room_chat_message_to_json(msg: &crate::chat_store::ChatMessage) -> serde_json::Value {
    serde_json::json!({
        "msg_id": msg.id,
        "sender": msg.sender_handle,
        "body": msg.body,
        "timestamp": msg.timestamp,
        "kind": msg.kind.as_str(),
        "mine": msg.is_self,
        "is_room": true,
        "status": msg.status.as_str(),
    })
}

fn peer_row_json_with_presence(
    record: &crate::peer_store::PeerRecord,
    online_peer_ids: &HashSet<String>,
    in_call_peer_ids: &HashSet<String>,
) -> serde_json::Value {
    serde_json::json!({
        "peer_id": record.peer_id,
        "handle": record.handle,
        "online": online_peer_ids.contains(&record.peer_id),
        "in_call": in_call_peer_ids.contains(&record.peer_id),
        "blocked": record.blocked,
    })
}

fn resolve_list_peer_id(store: &crate::peer_store::PeerStore, id: &str) -> Option<String> {
    if store.get(id).is_some() {
        return Some(id.to_owned());
    }
    store
        .get_by_identity(id)
        .map(|record| record.peer_id.clone())
}

fn lookup_list_peer_id(bridge: &AppBridgeRust, id: &str) -> Option<String> {
    let ps = bridge.peer_store.as_ref()?;
    let store = ps.read();
    resolve_list_peer_id(&store, id)
}

fn resolved_room_member_pids(bridge: &AppBridgeRust, member_ids: &[String]) -> Vec<String> {
    let Some(ps) = bridge.peer_store.as_ref() else {
        return Vec::new();
    };
    let store = ps.read();
    let my_id = bridge.my_public_id.as_str();
    member_ids
        .iter()
        .filter(|id| id.as_str() != my_id)
        .filter_map(|id| resolve_list_peer_id(&store, id))
        .collect()
}

fn emit_peers_updated(mut bridge: Pin<&mut ffi::AppBridge>) {
    let online = bridge.rust().online_peer_ids.clone();
    let in_call = bridge.rust().in_call_peer_ids.clone();
    let json = bridge.rust().peer_store.as_ref().map(|ps| {
        let store = ps.read();
        serde_json::to_string(
            &store
                .list_non_supernode_peers()
                .iter()
                .map(|p| peer_row_json_with_presence(p, &online, &in_call))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_owned())
    });
    if let Some(json) = json {
        bridge.as_mut().peers_updated(QString::from(json.as_str()));
    }
}

fn mark_peer_online(rust: &mut AppBridgeRust, pid: &str, online: bool) {
    if online {
        rust.online_peer_ids.insert(pid.to_owned());
    } else {
        rust.online_peer_ids.remove(pid);
    }
}

fn mark_peer_in_call(rust: &mut AppBridgeRust, pid: &str, in_call: bool) {
    if in_call {
        rust.in_call_peer_ids.insert(pid.to_owned());
    } else {
        rust.in_call_peer_ids.remove(pid);
    }
}

fn mark_direct_connected(rust: &mut AppBridgeRust, pid: &str, connected: bool) {
    if connected {
        rust.direct_connected_peer_ids.insert(pid.to_owned());
        rust.online_peer_ids.insert(pid.to_owned());
    } else {
        rust.direct_connected_peer_ids.remove(pid);
        if !rust.room_present_peer_ids.contains(pid) {
            rust.online_peer_ids.remove(pid);
        }
    }
}

fn clear_room_member_presence(rust: &mut AppBridgeRust) {
    for pid in rust.room_present_peer_ids.drain() {
        rust.in_call_peer_ids.remove(&pid);
        if !rust.direct_connected_peer_ids.contains(&pid) {
            rust.online_peer_ids.remove(&pid);
        }
    }
}

fn apply_room_member_presence(rust: &mut AppBridgeRust, member_pids: &[String]) {
    clear_room_member_presence(rust);
    for pid in member_pids {
        rust.room_present_peer_ids.insert(pid.clone());
        rust.online_peer_ids.insert(pid.clone());
        rust.in_call_peer_ids.insert(pid.clone());
    }
}

fn set_active_direct_call_presence(
    rust: &mut AppBridgeRust,
    peer_id: &str,
    active: bool,
    resolved_pid: Option<String>,
) {
    if active {
        rust.active_direct_call_peer_id = peer_id.to_owned();
        if let Some(pid) = resolved_pid {
            mark_peer_online(rust, &pid, true);
            mark_peer_in_call(rust, &pid, true);
        }
    } else if rust.active_direct_call_peer_id == peer_id {
        rust.active_direct_call_peer_id.clear();
        if let Some(pid) = resolved_pid {
            mark_peer_in_call(rust, &pid, false);
            if !rust.direct_connected_peer_ids.contains(&pid)
                && !rust.room_present_peer_ids.contains(&pid)
            {
                rust.online_peer_ids.remove(&pid);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn remember_room_in_store(
    room_store: &Option<Arc<RwLock<crate::room_store::RoomStore>>>,
    supernode_id: &str,
    room_id: &str,
    room_name: &str,
    room_type: &str,
    creator_id: &str,
    is_creator: bool,
    invite_token: &str,
) {
    let Some(rs) = room_store else {
        return;
    };
    if supernode_id.is_empty() || room_id.is_empty() || room_id == "default" {
        return;
    }
    let entry = crate::room_store::RoomEntry::new(room_id, room_name)
        .with_type(if room_type.is_empty() {
            "public"
        } else {
            room_type
        })
        .with_supernode(supernode_id)
        .with_creator(creator_id, is_creator)
        .with_invite_token(invite_token);
    if let Err(e) = rs.write().upsert(entry) {
        warn!("room_store upsert error: {e}");
    }
}

fn local_rooms_json_for_supernode(
    room_store: &crate::room_store::RoomStore,
    peer_store: &crate::peer_store::PeerStore,
    supernode_id: &str,
) -> serde_json::Value {
    serde_json::Value::Array(
        room_store
            .list_for_supernode_resolved(peer_store, supernode_id)
            .iter()
            .filter(|e| !room_store.is_hidden_from_sidebar(supernode_id, &e.room_id))
            .map(|e| {
                serde_json::json!({
                    "room_id": e.room_id,
                    "room_name": e.room_name,
                    "room_type": e.room_type,
                    "creator_id": e.creator_id,
                })
            })
            .collect(),
    )
}

fn room_count_from_json(room: &serde_json::Value) -> u64 {
    room.get("member_count")
        .or_else(|| room.get("voice_count"))
        .or_else(|| room.get("count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

fn room_has_count(room: &serde_json::Value) -> bool {
    room.get("member_count")
        .or_else(|| room.get("voice_count"))
        .or_else(|| room.get("count"))
        .and_then(|v| v.as_u64())
        .is_some()
}

fn merge_room_entry(existing: &mut serde_json::Value, incoming: &serde_json::Value) {
    let old_count = room_count_from_json(existing);
    let mut merged = incoming.clone();
    if !room_has_count(incoming) {
        if let Some(obj) = merged.as_object_mut() {
            obj.insert(
                "member_count".to_owned(),
                serde_json::Value::Number(serde_json::Number::from(old_count)),
            );
            if let Some(ids) = existing.get("participant_ids") {
                obj.insert("participant_ids".to_owned(), ids.clone());
            }
        }
    }
    *existing = merged;
}

fn active_voice_room_scope(bridge: &AppBridgeRust) -> (String, String) {
    if !bridge.voice_room_id.is_empty() {
        return (
            bridge.voice_supernode_id.clone(),
            bridge.voice_room_id.clone(),
        );
    }
    (
        bridge.current_supernode_id.clone(),
        bridge.current_room_id.clone(),
    )
}

fn is_active_voice_room(bridge: &AppBridgeRust, supernode_id: &str, room_id: &str) -> bool {
    let (sn, rid) = active_voice_room_scope(bridge);
    !rid.is_empty() && sn == supernode_id && rid == room_id
}

fn merge_room_list_values(
    local: &serde_json::Value,
    remote: &serde_json::Value,
) -> serde_json::Value {
    let mut by_id: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    for source in [local, remote] {
        let Some(arr) = source.as_array() else {
            continue;
        };
        for room in arr {
            let Some(room_id) = room.get("room_id").and_then(|v| v.as_str()) else {
                continue;
            };
            if room_id.is_empty() {
                continue;
            }
            by_id
                .entry(room_id.to_owned())
                .and_modify(|existing| merge_room_entry(existing, room))
                .or_insert_with(|| room.clone());
        }
    }
    let mut merged: Vec<serde_json::Value> = by_id.into_values().collect();
    merged.sort_by(|a, b| {
        let an = a
            .get("room_name")
            .or_else(|| a.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let bn = b
            .get("room_name")
            .or_else(|| b.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        an.cmp(bn)
    });
    serde_json::Value::Array(merged)
}

fn sync_saved_rooms_from_list(
    room_store: &mut crate::room_store::RoomStore,
    supernode_id: &str,
    rooms: &serde_json::Value,
) {
    let Some(arr) = rooms.as_array() else {
        return;
    };
    for room in arr {
        let room_id = room
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if room_id.is_empty() {
            continue;
        }
        let existing = room_store.get(supernode_id, room_id);
        let room_name = room
            .get("name")
            .or_else(|| room.get("room_name"))
            .and_then(|v| v.as_str())
            .filter(|n| !n.is_empty() && *n != room_id)
            .map(str::to_owned)
            .or_else(|| {
                existing
                    .filter(|e| !e.room_name.is_empty() && e.room_name != room_id)
                    .map(|e| e.room_name.clone())
            })
            .unwrap_or_else(|| room_id.to_owned());
        let room_type = room
            .get("room_type")
            .and_then(|v| v.as_str())
            .unwrap_or("public");
        let creator_id = room
            .get("creator_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_creator = existing.map(|e| e.is_creator).unwrap_or(false);
        let invite_token = existing.map(|e| e.invite_token.as_str()).unwrap_or("");
        let entry = crate::room_store::RoomEntry::new(room_id, room_name)
            .with_type(room_type)
            .with_supernode(supernode_id)
            .with_creator(
                if creator_id.is_empty() {
                    existing.map(|e| e.creator_id.as_str()).unwrap_or("")
                } else {
                    creator_id
                },
                is_creator,
            )
            .with_invite_token(invite_token);
        if let Err(e) = room_store.upsert(entry) {
            warn!("room_store sync from list error: {e}");
        }
    }
}

fn replay_saved_rooms_on_supernode_connect(
    room_store: &crate::room_store::RoomStore,
    peer_store: &crate::peer_store::PeerStore,
    conn_cmd_tx: &mpsc::Sender<ConnectionCommand>,
    supernode_id: &str,
    my_public_id: &str,
) {
    for entry in room_store.list_for_supernode_resolved(peer_store, supernode_id) {
        if entry.room_id == "default" {
            continue;
        }
        if room_store.is_hidden_from_sidebar(supernode_id, &entry.room_id) {
            continue;
        }
        let creator_id = if entry.creator_id.is_empty() {
            my_public_id.to_owned()
        } else {
            entry.creator_id.clone()
        };
        let _ = conn_cmd_tx.try_send(ConnectionCommand::CreateRoom {
            supernode_id: supernode_id.to_owned(),
            room_name: entry.room_name.clone(),
            room_type: entry.room_type.clone(),
            room_id: Some(entry.room_id.clone()),
            creator_id: Some(creator_id),
            materialize_only: true,
        });
    }
}

fn filter_sfu_rooms_for_sidebar(
    room_store: &crate::room_store::RoomStore,
    supernode_id: &str,
    rooms: &serde_json::Value,
) -> serde_json::Value {
    let Some(arr) = rooms.as_array() else {
        return rooms.clone();
    };
    let filtered: Vec<serde_json::Value> = arr
        .iter()
        .filter(|r| {
            let room_id = r
                .get("room_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            !room_id.is_empty() && !room_store.is_hidden_from_sidebar(supernode_id, room_id)
        })
        .cloned()
        .collect();
    serde_json::Value::Array(filtered)
}

fn room_member_display_name(
    peer_store: &crate::peer_store::PeerStore,
    my_public_id: &str,
    peer_id: &str,
) -> Option<String> {
    if peer_id == my_public_id {
        return Some("You".to_owned());
    }
    peer_store
        .get(peer_id)
        .or_else(|| peer_store.get_by_identity(peer_id))
        .filter(|rec| !rec.is_supernode)
        .map(|rec| rec.display_name())
}

fn enrich_room_voice_participants(
    rooms: serde_json::Value,
    peer_store: Option<&crate::peer_store::PeerStore>,
    my_public_id: &str,
) -> serde_json::Value {
    let Some(arr) = rooms.as_array() else {
        return rooms;
    };
    let enriched = arr
        .iter()
        .map(|room| {
            let mut obj = room.as_object().cloned().unwrap_or_default();
            let participant_ids: Vec<String> = room
                .get("participant_ids")
                .or_else(|| room.get("participants"))
                .and_then(|v| v.as_array())
                .map(|ids| {
                    ids.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();

            let voice_count = if !participant_ids.is_empty() {
                participant_ids.len()
            } else {
                room.get("member_count")
                    .or_else(|| room.get("voice_count"))
                    .or_else(|| room.get("count"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize
            };

            let mut known = Vec::new();
            if let Some(store) = peer_store {
                for id in &participant_ids {
                    if let Some(name) = room_member_display_name(store, my_public_id, id) {
                        if !known.iter().any(|existing| existing == &name) {
                            known.push(name);
                        }
                    }
                }
            }
            known.sort();

            let unknown = voice_count.saturating_sub(known.len());
            obj.insert(
                "known_peers".to_owned(),
                serde_json::Value::Array(
                    known.into_iter().map(serde_json::Value::String).collect(),
                ),
            );
            obj.insert(
                "unknown_peers".to_owned(),
                serde_json::Value::Number(serde_json::Number::from(unknown as u64)),
            );
            obj.insert(
                "voice_count".to_owned(),
                serde_json::Value::Number(serde_json::Number::from(voice_count as u64)),
            );
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::Value::Array(enriched)
}

fn room_voice_sidebar_patch(
    bridge: &AppBridgeRust,
    supernode_id: &str,
    room_id: &str,
    member_ids: &[String],
) -> Option<String> {
    if supernode_id.is_empty() || room_id.is_empty() {
        return None;
    }

    let mut room = serde_json::json!({
        "room_id": room_id,
        "participant_ids": member_ids,
        "member_count": member_ids.len(),
    });

    if let (Some(rs), Some(ps)) = (bridge.room_store.as_ref(), bridge.peer_store.as_ref()) {
        let store = rs.read();
        let peer_store = ps.read();
        if let Some(entry) = store.get(supernode_id, room_id) {
            room["room_name"] = serde_json::Value::String(entry.room_name.clone());
            room["room_type"] = serde_json::Value::String(entry.room_type.clone());
            room["creator_id"] = serde_json::Value::String(entry.creator_id.clone());
            room["is_default"] = serde_json::Value::Bool(entry.room_id == "default");
        }
        let rooms = enrich_room_voice_participants(
            serde_json::Value::Array(vec![room]),
            Some(&peer_store),
            bridge.my_public_id.as_str(),
        );
        return Some(
            serde_json::json!({
                "supernode_id": supernode_id,
                "rooms": rooms,
                "replace": false,
            })
            .to_string(),
        );
    }

    let rooms = enrich_room_voice_participants(
        serde_json::Value::Array(vec![room]),
        None,
        bridge.my_public_id.as_str(),
    );
    Some(
        serde_json::json!({
            "supernode_id": supernode_id,
            "rooms": rooms,
            "replace": false,
        })
        .to_string(),
    )
}

fn room_participant_label(
    peer_store: Option<&crate::peer_store::PeerStore>,
    peer_id: &str,
    my_peer_id: &str,
    my_public_id: &str,
) -> String {
    if peer_id == my_public_id || peer_id == my_peer_id {
        let local = read_local_handle();
        if !local.is_empty() {
            return local;
        }
        if let Some(store) = peer_store {
            if let Some(rec) = store.get(my_peer_id) {
                let name = rec.display_name();
                if !name.is_empty() {
                    return name;
                }
            }
        }
    } else if let Some(store) = peer_store {
        if let Some(rec) = store
            .get(peer_id)
            .or_else(|| store.get_by_identity(peer_id))
        {
            return rec.display_name();
        }
    }
    if peer_id.len() > 12 {
        format!("{}…", &peer_id[..12])
    } else {
        peer_id.to_string()
    }
}

fn room_participants_json(
    peer_store: Option<&crate::peer_store::PeerStore>,
    ids: &[String],
    my_peer_id: &str,
    my_public_id: &str,
) -> String {
    serde_json::to_string(
        &ids.iter()
            .map(|id| {
                serde_json::json!({
                    "peer_id": id,
                    "handle": room_participant_label(peer_store, id, my_peer_id, my_public_id),
                    "speaking": false,
                    "muted": false,
                    "is_self": id == my_public_id || id == my_peer_id,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_owned())
}

fn rooms_sidebar_json(store: &crate::peer_store::PeerStore) -> String {
    serde_json::to_string(
        &store
            .supernodes()
            .iter()
            .map(|p| {
                serde_json::json!({
                    "node_id": p.identity_pub,
                    "connected": false,
                    "homepage_url": "",
                    "title": p.handle,
                    "sfu_enabled": false,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_owned())
}

fn emit_rooms_sidebar_sync(mut bridge: Pin<&mut ffi::AppBridge>) {
    let json = bridge
        .rust()
        .peer_store
        .as_ref()
        .map(|ps| rooms_sidebar_json(&ps.read()));
    if let Some(json) = json {
        bridge
            .as_mut()
            .rooms_sidebar_sync(QString::from(json.as_str()));
    }
}

fn emit_local_rooms_for_all_supernodes(mut bridge: Pin<&mut ffi::AppBridge>) {
    let (peer_store, room_store) = (
        bridge.rust().peer_store.clone(),
        bridge.rust().room_store.clone(),
    );
    let (Some(peer_store), Some(room_store)) = (peer_store, room_store) else {
        return;
    };
    let supernode_ids: Vec<String> = peer_store
        .read()
        .supernodes()
        .iter()
        .map(|p| p.identity_pub.clone())
        .collect();
    for supernode_id in supernode_ids {
        let ps = peer_store.read();
        let rs = room_store.read();
        let local = local_rooms_json_for_supernode(&rs, &ps, &supernode_id);
        drop(rs);
        drop(ps);
        if local.as_array().is_some_and(|a| a.is_empty()) {
            continue;
        }
        let wrapped = serde_json::json!({
            "supernode_id": supernode_id,
            "rooms": local,
            "replace": false,
        })
        .to_string();
        bridge
            .as_mut()
            .sfu_rooms_updated(QString::from(wrapped.as_str()));
    }
}

fn dispatch_event(
    qt_thread: &cxx_qt::CxxQtThread<ffi::AppBridge>,
    ev: ConnectionEvent,
    chat_store: &Arc<crate::chat_store::ChatStore>,
    call_timer_stop: &mut Option<oneshot::Sender<()>>,
) {
    match ev {
        ConnectionEvent::PeerConnected(peer_id) => {
            let banner = format!("Direct \u{00b7} {peer_id}");
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let n = *bridge.peer_count() + 1;
                bridge.as_mut().set_peer_count(n);
                bridge
                    .as_mut()
                    .set_session_banner(QString::from(banner.as_str()));
                bridge.as_mut().set_connection_mode(QString::from("direct"));
                if let Some(pid) = lookup_list_peer_id(bridge.rust(), &peer_id) {
                    mark_direct_connected(&mut bridge.as_mut().rust_mut(), &pid, true);
                }
                emit_peers_updated(bridge.as_mut());
                // Auto-broadcast own avatar config to the newly connected peer.
                let cfg = bridge.rust().avatar_config_json.clone();
                let pid = peer_id.clone();
                if !cfg.is_empty() {
                    if let Some(ref tx) = bridge.rust().conn_cmd_tx {
                        let _ = tx.try_send(ConnectionCommand::BroadcastAvatarConfig {
                            peer_id: pid,
                            config_json: cfg,
                        });
                    }
                }
            });
        }
        ConnectionEvent::PeerDisconnected(peer_id) => {
            let banner = format!("Offline \u{00b7} {peer_id}");
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let n = (*bridge.peer_count() - 1).max(0);
                bridge.as_mut().set_peer_count(n);
                bridge
                    .as_mut()
                    .set_session_banner(QString::from(banner.as_str()));
                bridge
                    .as_mut()
                    .set_connection_mode(QString::from("offline"));
                if let Some(pid) = lookup_list_peer_id(bridge.rust(), &peer_id) {
                    mark_direct_connected(&mut bridge.as_mut().rust_mut(), &pid, false);
                }
                emit_peers_updated(bridge.as_mut());
            });
        }
        ConnectionEvent::ChatMessage {
            peer_id,
            message_id,
            body,
            timestamp,
            sender_handle,
        } => {
            // Idempotency: the same message can legitimately arrive more than
            // once — e.g. relayed via a supernode *and* delivered directly, or
            // a duplicate relay. Rows are keyed by message_id, so if we've
            // already stored it, skip persist/notify/display to avoid showing
            // it twice or double-counting unread.
            if chat_store
                .get_by_id(&message_id)
                .map(|m| m.is_some())
                .unwrap_or(false)
            {
                return;
            }
            // Persist to chat store (best-effort)
            let msg = crate::chat_store::ChatMessage {
                id: message_id.clone(),
                peer_id: peer_id.clone(),
                sender: sender_handle.clone(),
                recipient: String::new(),
                body: body.clone(),
                timestamp,
                is_self: false,
                status: crate::chat_store::MessageStatus::Delivered,
                kind: crate::chat_store::MessageKind::Text,
                attachment_name: String::new(),
                attachment_path: String::new(),
                size_str: String::new(),
                status_note: String::new(),
                sender_handle: sender_handle.clone(),
            };
            if let Err(e) = chat_store.insert(&msg) {
                warn!("chat_store insert error: {e}");
            }
            let preview = if body.chars().count() > 80 {
                let truncated: String = body.chars().take(79).collect();
                format!("{truncated}\u{2026}")
            } else {
                body.clone()
            };

            let peer_id_clone = peer_id.clone();
            let preview_clone = preview.clone();
            let message_id_clone = message_id.clone();
            let sender_handle_clone = sender_handle.clone();
            let body_clone = body.clone();
            let chat_store_for_read = Arc::clone(chat_store);
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let is_viewing = bridge.rust().selected_peer_id == peer_id_clone;
                if is_viewing {
                    if let Err(e) = chat_store_for_read.mark_peer_read(&peer_id_clone) {
                        warn!("chat_store mark_peer_read (live) error: {e}");
                    }
                }

                let peer_unread = chat_store_for_read
                    .unread_count(&peer_id_clone)
                    .unwrap_or(0) as i32;
                let global = chat_store_for_read.total_unread_count().unwrap_or(0) as u32;
                bridge.as_mut().rust_mut().unread_chat = global;
                if global == 0 {
                    crate::platform::clear_taskbar_badge();
                } else {
                    crate::platform::set_taskbar_badge(global);
                }

                let status = if is_viewing { "read" } else { "delivered" };
                let json = serde_json::json!({
                    "msg_id": message_id_clone,
                    "peer_id": peer_id_clone,
                    "sender": sender_handle_clone,
                    "body": body_clone,
                    "timestamp": timestamp,
                    "kind": "text",
                    "mine": false,
                    "status": status,
                })
                .to_string();
                bridge
                    .as_mut()
                    .chat_message_received(QString::from(json.as_str()));

                bridge
                    .as_mut()
                    .unread_changed(QString::from(peer_id_clone.as_str()), peer_unread);
                let preview_text = if preview_clone.chars().count() > 60 {
                    let truncated: String = preview_clone.chars().take(59).collect();
                    format!("{truncated}\u{2026}")
                } else {
                    preview_clone.clone()
                };
                bridge.as_mut().preview_changed(
                    QString::from(peer_id_clone.as_str()),
                    QString::from(preview_text.as_str()),
                );
            });
        }
        ConnectionEvent::ChatAck {
            peer_id: _,
            message_id,
        } => {
            if let Err(e) =
                chat_store.update_status(&message_id, crate::chat_store::MessageStatus::Delivered)
            {
                warn!("chat_store update_status delivered error: {e}");
            }
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().message_status_changed(
                    QString::from(message_id.as_str()),
                    QString::from("delivered"),
                );
            });
        }
        ConnectionEvent::ChatSendFailed {
            peer_id: _,
            message_id,
            reason,
        } => {
            if let Err(e) = chat_store.update_status_note(
                &message_id,
                crate::chat_store::MessageStatus::Failed,
                &reason,
            ) {
                warn!("chat_store update_status failed error: {e}");
            }
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().message_status_changed(
                    QString::from(message_id.as_str()),
                    QString::from("failed"),
                );
            });
        }
        ConnectionEvent::CallRequest { peer_id } => {
            crate::platform::play_ringtone();
            crate::platform::show_notification("Incoming call", &peer_id);
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().rust_mut().has_incoming_call = true;
                bridge
                    .as_mut()
                    .incoming_call(QString::from(peer_id.as_str()));
            });
        }
        ConnectionEvent::CallAccepted { peer_id } => {
            // Start a per-second call duration timer.
            call_timer_stop.take(); // cancel any previous timer
            let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
            *call_timer_stop = Some(stop_tx);
            let qt = qt_thread.clone();
            tokio::spawn(async move {
                let mut secs: i32 = 0;
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                            secs += 1;
                            let _ = qt.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                                bridge.as_mut().set_call_duration_secs(secs);
                            });
                        }
                        _ = &mut stop_rx => break,
                    }
                }
            });
            let banner = format!("In call \u{00b7} {peer_id}");
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().rust_mut().has_incoming_call = false;
                bridge.as_mut().set_call_state(QString::from("in_call"));
                bridge
                    .as_mut()
                    .set_session_banner(QString::from(banner.as_str()));
                {
                    let resolved = lookup_list_peer_id(bridge.rust(), &peer_id);
                    set_active_direct_call_presence(
                        &mut bridge.as_mut().rust_mut(),
                        &peer_id,
                        true,
                        resolved,
                    );
                }
                emit_peers_updated(bridge.as_mut());
            });
        }
        ConnectionEvent::CallEnded { .. } => {
            call_timer_stop.take(); // stop the duration timer
            let _ = qt_thread.queue(|mut bridge: Pin<&mut ffi::AppBridge>| {
                if bridge.rust().has_incoming_call {
                    bridge.as_mut().rust_mut().has_incoming_call = false;
                    let mc = bridge.rust().missed_calls + 1;
                    bridge.as_mut().set_missed_calls(mc);
                }
                {
                    let active = bridge.rust().active_direct_call_peer_id.clone();
                    if !active.is_empty() {
                        let resolved = lookup_list_peer_id(bridge.rust(), &active);
                        set_active_direct_call_presence(
                            &mut bridge.as_mut().rust_mut(),
                            &active,
                            false,
                            resolved,
                        );
                    }
                }
                bridge.as_mut().set_call_state(QString::from("idle"));
                bridge.as_mut().set_call_duration_secs(0);
                emit_peers_updated(bridge);
            });
        }
        ConnectionEvent::SessionStateUpdate(state) => {
            let path_str = match state.chat_path {
                crate::session_state::ChatPath::Direct => "direct",
                crate::session_state::ChatPath::Relay => "relay",
                crate::session_state::ChatPath::None => "offline",
            };
            let mode = path_str.to_owned();
            let banner = format!("{path_str} \u{00b7} {}", state.peer_id);
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .set_session_banner(QString::from(banner.as_str()));
                bridge
                    .as_mut()
                    .set_connection_mode(QString::from(mode.as_str()));
            });
        }
        ConnectionEvent::SupernodeConnected(id) => {
            let banner = format!("Connected via supernode \u{00b7} {id}");
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .set_session_banner(QString::from(banner.as_str()));
                let Some(canon) = bridge.rust().resolve_supernode_node_id_str(&id) else {
                    return;
                };
                let node_json =
                    serde_json::json!([{ "node_id": canon, "connected": true }]).to_string();
                bridge
                    .as_mut()
                    .nodes_updated(QString::from(node_json.as_str()));
                let my_public_id = bridge.rust().my_public_id.clone();
                if let (Some(rs), Some(ps), Some(tx)) = (
                    bridge.rust().room_store.as_ref(),
                    bridge.rust().peer_store.as_ref(),
                    bridge.rust().conn_cmd_tx.as_ref(),
                ) {
                    replay_saved_rooms_on_supernode_connect(
                        &rs.read(),
                        &ps.read(),
                        tx,
                        &canon,
                        &my_public_id,
                    );
                }
            });
        }
        ConnectionEvent::SupernodeDisconnected(id) => {
            let banner = format!("Supernode disconnected \u{00b7} {id}");
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .set_session_banner(QString::from(banner.as_str()));
                let Some(canon) = bridge.rust().resolve_supernode_node_id_str(&id) else {
                    return;
                };
                let node_json =
                    serde_json::json!([{ "node_id": canon, "connected": false }]).to_string();
                bridge
                    .as_mut()
                    .nodes_updated(QString::from(node_json.as_str()));

                // If the lost supernode was hosting the active room, perform a
                // full local leave (SfuClient + CallController + in-room state)
                // so the client does not remain stuck targeting a dead host
                // for audio/chat while other supernodes or direct peers are
                // still usable. This mirrors explicit remove_room / leave paths
                // and the multi-supernode isolation requirement.
                if bridge.rust().current_supernode_id == canon {
                    bridge.as_mut().leave_room();
                    {
                        let mut r = bridge.as_mut().rust_mut();
                        r.current_supernode_id.clear();
                        r.current_room_id.clear();
                    }
                    bridge
                        .as_mut()
                        .set_session_banner(QString::from(banner.as_str()));
                    bridge
                        .as_mut()
                        .set_connection_mode(QString::from("offline"));
                }
            });
        }
        ConnectionEvent::SupernodeInfoReceived {
            supernode_id,
            homepage_url,
            title,
            wt_url,
            cert_fingerprint,
            sfu_enabled,
        } => {
            // Cache the WebTransport base URL and cert fingerprint so
            // /_conquerd/ctx.json can expose them to game pages loaded via
            // conquerd://.  The fingerprint lets games use
            // `serverCertificateHashes` — no CA cert needed.
            #[cfg(feature = "webengine")]
            {
                if !wt_url.is_empty() {
                    crate::ui::scheme::set_supernode_wt_url(&supernode_id, &wt_url);
                }
                if !cert_fingerprint.is_empty() {
                    crate::ui::scheme::set_supernode_cert_fingerprint(
                        &supernode_id,
                        &cert_fingerprint,
                    );
                }
            }
            #[cfg(not(feature = "webengine"))]
            let _ = (&wt_url, &cert_fingerprint);
            let url_q = homepage_url.clone();
            let title_q = title.clone();
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let Some(canon) = bridge.rust().resolve_supernode_node_id_str(&supernode_id) else {
                    return;
                };
                let node_json = serde_json::json!([{
                    "node_id": canon,
                    "homepage_url": homepage_url,
                    "title": title,
                    "sfu_enabled": sfu_enabled,
                }])
                .to_string();
                bridge
                    .as_mut()
                    .nodes_updated(QString::from(node_json.as_str()));
                bridge.as_mut().supernode_info_received(
                    QString::from(canon.as_str()),
                    QString::from(url_q.as_str()),
                    QString::from(title_q.as_str()),
                );
            });
        }
        ConnectionEvent::RoomCreated {
            supernode_id,
            room_id,
            room_name,
            room_type,
            invite_token,
        } => {
            let banner = format!("Room \u{00b7} {room_name}");
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let Some(canon) = bridge.rust().resolve_supernode_node_id_str(&supernode_id) else {
                    return;
                };
                let my_public_id = bridge.rust().my_public_id.clone();
                remember_room_in_store(
                    &bridge.rust().room_store,
                    &canon,
                    &room_id,
                    &room_name,
                    &room_type,
                    &my_public_id,
                    true,
                    &invite_token,
                );
                bridge
                    .as_mut()
                    .set_session_banner(QString::from(banner.as_str()));
                bridge.as_mut().set_connection_mode(QString::from("room"));
                {
                    let mut r = bridge.as_mut().rust_mut();
                    r.current_supernode_id = canon.clone();
                    r.current_room_id = room_id.clone();
                }
                bridge.as_mut().room_created(
                    QString::from(canon.as_str()),
                    QString::from(room_id.as_str()),
                    QString::from(room_name.as_str()),
                    QString::from(room_type.as_str()),
                    QString::from(invite_token.as_str()),
                );
            });
        }
        ConnectionEvent::RelayPaymentRequired {
            supernode_id,
            portal_url,
        } => {
            info!(
                "[relay] Portal required — opening browser for {}",
                &supernode_id[..8.min(supernode_id.len())]
            );
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().relay_portal_required(
                    QString::from(supernode_id.as_str()),
                    QString::from(portal_url.as_str()),
                );
            });
        }
        ConnectionEvent::RelayGranted {
            relay_host,
            relay_port,
            ..
        } => {
            let banner = format!("Relay \u{00b7} {relay_host}:{relay_port}");
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .set_session_banner(QString::from(banner.as_str()));
                bridge.as_mut().set_connection_mode(QString::from("relay"));
            });
        }
        ConnectionEvent::TypingIndicator { peer_id, is_typing } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .typing_changed(QString::from(peer_id.as_str()), is_typing);
            });
        }
        ConnectionEvent::HandleUpdated { peer_id, handle: _ } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let is_supernode = bridge
                    .rust()
                    .peer_store
                    .as_ref()
                    .map(|ps| ps.read().is_supernode_id(&peer_id))
                    .unwrap_or(false);
                if !is_supernode {
                    emit_peers_updated(bridge.as_mut());
                }
                if bridge.rust().in_room && !bridge.rust().room_participant_ids.is_empty() {
                    let my_public_id = bridge.rust().my_public_id.clone();
                    let my_peer_id = bridge.rust().my_peer_id.clone();
                    let ids = bridge.rust().room_participant_ids.clone();
                    let json = if let Some(ps) = bridge.rust().peer_store.as_ref() {
                        room_participants_json(Some(&ps.read()), &ids, &my_peer_id, &my_public_id)
                    } else {
                        room_participants_json(None, &ids, &my_peer_id, &my_public_id)
                    };
                    bridge
                        .as_mut()
                        .participants_updated(QString::from(json.as_str()));
                }
            });
        }
        ConnectionEvent::AvatarConfigUpdated { peer_id } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .avatar_config_updated(QString::from(peer_id.as_str()));
            });
        }
        ConnectionEvent::RoomMembersChanged {
            supernode_id,
            room_id,
            members,
        } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let Some(canon) = bridge.rust().resolve_supernode_node_id_str(&supernode_id) else {
                    return;
                };
                let voice_active =
                    is_active_voice_room(bridge.rust(), canon.as_str(), room_id.as_str());

                if voice_active {
                    // Replace the participant cache with the authoritative snapshot.
                    bridge.as_mut().rust_mut().room_participant_ids = members.clone();

                    // Initiate QUIC audio sessions for all non-self room members so
                    // direct peer audio works when hole-punch or same-LAN applies.
                    let my_public_id = bridge.rust().my_public_id.clone();
                    let my_peer_id = bridge.rust().my_peer_id.clone();
                    if let Some(ref tx) = bridge.rust().call_cmd_tx {
                        for peer_id in &members {
                            if peer_id != &my_public_id {
                                let _ = tx.try_send(CallCommand::InitiatePeer {
                                    peer_id: peer_id.clone(),
                                    host: None,
                                    port: None,
                                });
                            }
                        }
                    }

                    {
                        let pids = resolved_room_member_pids(bridge.rust(), &members);
                        apply_room_member_presence(&mut bridge.as_mut().rust_mut(), &pids);
                    }
                    let json = if let Some(ps) = bridge.rust().peer_store.as_ref() {
                        room_participants_json(
                            Some(&ps.read()),
                            &members,
                            &my_peer_id,
                            &my_public_id,
                        )
                    } else {
                        room_participants_json(None, &members, &my_peer_id, &my_public_id)
                    };
                    bridge
                        .as_mut()
                        .participants_updated(QString::from(json.as_str()));
                }

                if let Some(patch) =
                    room_voice_sidebar_patch(bridge.rust(), &canon, &room_id, &members)
                {
                    bridge
                        .as_mut()
                        .sfu_rooms_updated(QString::from(patch.as_str()));
                }
                emit_peers_updated(bridge.as_mut());
            });
        }
        ConnectionEvent::RoomPeerJoined {
            supernode_id,
            room_id,
            peer_id,
        } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let Some(canon) = bridge.rust().resolve_supernode_node_id_str(&supernode_id) else {
                    return;
                };
                let voice_active =
                    is_active_voice_room(bridge.rust(), canon.as_str(), room_id.as_str());

                if voice_active {
                    // Broadcast own avatar config to the newly-joined room peer so
                    // they can render our identicon without waiting for a direct
                    // QUIC PeerConnected event (which may never fire in SFU rooms).
                    let cfg = bridge.rust().avatar_config_json.clone();
                    if !cfg.is_empty() {
                        if let Some(ref tx) = bridge.rust().conn_cmd_tx {
                            let _ = tx.try_send(ConnectionCommand::BroadcastAvatarConfig {
                                peer_id: peer_id.clone(),
                                config_json: cfg,
                            });
                        }
                    }

                    // Add to cache if not already present (idempotent).
                    if !bridge.rust().room_participant_ids.contains(&peer_id) {
                        bridge
                            .as_mut()
                            .rust_mut()
                            .room_participant_ids
                            .push(peer_id);
                    }

                    // Re-emit the full participant list so the model is never partial.
                    let my_public_id = bridge.rust().my_public_id.clone();
                    let my_peer_id = bridge.rust().my_peer_id.clone();
                    let ids = bridge.rust().room_participant_ids.clone();
                    {
                        let pids = resolved_room_member_pids(bridge.rust(), &ids);
                        apply_room_member_presence(&mut bridge.as_mut().rust_mut(), &pids);
                    }
                    let json = if let Some(ps) = bridge.rust().peer_store.as_ref() {
                        room_participants_json(Some(&ps.read()), &ids, &my_peer_id, &my_public_id)
                    } else {
                        room_participants_json(None, &ids, &my_peer_id, &my_public_id)
                    };
                    bridge
                        .as_mut()
                        .participants_updated(QString::from(json.as_str()));

                    if let Some(patch) =
                        room_voice_sidebar_patch(bridge.rust(), &canon, &room_id, &ids)
                    {
                        bridge
                            .as_mut()
                            .sfu_rooms_updated(QString::from(patch.as_str()));
                    }
                }
                emit_peers_updated(bridge.as_mut());
            });
        }
        ConnectionEvent::RoomPeerLeft {
            supernode_id,
            room_id,
            peer_id,
        } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let Some(canon) = bridge.rust().resolve_supernode_node_id_str(&supernode_id) else {
                    return;
                };
                let voice_active =
                    is_active_voice_room(bridge.rust(), canon.as_str(), room_id.as_str());

                if voice_active {
                    bridge
                        .as_mut()
                        .rust_mut()
                        .room_participant_ids
                        .retain(|id| id != &peer_id);

                    // Re-emit the full participant list after removal.
                    let my_public_id = bridge.rust().my_public_id.clone();
                    let my_peer_id = bridge.rust().my_peer_id.clone();
                    let ids = bridge.rust().room_participant_ids.clone();
                    {
                        let pids = resolved_room_member_pids(bridge.rust(), &ids);
                        apply_room_member_presence(&mut bridge.as_mut().rust_mut(), &pids);
                    }
                    let json = if let Some(ps) = bridge.rust().peer_store.as_ref() {
                        room_participants_json(Some(&ps.read()), &ids, &my_peer_id, &my_public_id)
                    } else {
                        room_participants_json(None, &ids, &my_peer_id, &my_public_id)
                    };
                    bridge
                        .as_mut()
                        .participants_updated(QString::from(json.as_str()));

                    if let Some(patch) =
                        room_voice_sidebar_patch(bridge.rust(), &canon, &room_id, &ids)
                    {
                        bridge
                            .as_mut()
                            .sfu_rooms_updated(QString::from(patch.as_str()));
                    }
                }
                emit_peers_updated(bridge.as_mut());
            });
        }
        ConnectionEvent::SfuAudioReceived { peer_id, opus_data } => {
            // Forward relayed room audio to the call controller's inbound pipeline.
            let _ = qt_thread.queue(move |bridge: Pin<&mut ffi::AppBridge>| {
                if let Some(ref tx) = bridge.rust().call_cmd_tx {
                    let _ = tx.try_send(CallCommand::RoomAudioInbound { peer_id, opus_data });
                }
            });
        }
        ConnectionEvent::DirectAudioReceived { peer_id, opus_data } => {
            // Forward direct 1:1 QUIC audio to the call controller.
            let _ = qt_thread.queue(move |bridge: Pin<&mut ffi::AppBridge>| {
                if let Some(ref tx) = bridge.rust().call_cmd_tx {
                    let _ = tx.try_send(CallCommand::DirectAudioInbound { peer_id, opus_data });
                }
            });
        }
        ConnectionEvent::RoomChatMessage {
            supernode_id,
            room_id,
            sender_id,
            sender_handle,
            body,
            timestamp,
        } => {
            let message_id = uuid::Uuid::new_v4().to_string();
            let json = serde_json::json!({
                "msg_id": message_id.clone(),
                "sender": sender_handle.clone(),
                "body": body.clone(),
                "timestamp": timestamp,
                "kind": "text",
                "mine": false,
                "is_room": true,
                "status": "delivered",
            })
            .to_string();
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let Some(sn) = bridge.rust().resolve_supernode_node_id_str(&supernode_id) else {
                    return;
                };
                if let Some(ref cs) = bridge.rust().chat_store {
                    let store_key = room_chat_store_peer_id(&sn, &room_id);
                    let chat_msg = crate::chat_store::ChatMessage {
                        id: message_id.clone(),
                        peer_id: store_key,
                        sender: sender_id.clone(),
                        recipient: room_id.clone(),
                        body: body.clone(),
                        timestamp,
                        is_self: false,
                        status: crate::chat_store::MessageStatus::Delivered,
                        kind: crate::chat_store::MessageKind::Text,
                        attachment_name: String::new(),
                        attachment_path: String::new(),
                        size_str: String::new(),
                        status_note: String::new(),
                        sender_handle: sender_handle.clone(),
                    };
                    if let Err(e) = cs.insert(&chat_msg) {
                        warn!("chat_store insert (room inbound) error: {e}");
                    }
                }
                let key = room_chat_history_key(&sn, &room_id);
                // Persist in session-scoped history so switchToRoom can replay.
                bridge
                    .as_mut()
                    .rust_mut()
                    .room_chat_history
                    .entry(key)
                    .or_default()
                    .push(json.clone());
                bridge
                    .as_mut()
                    .room_chat_received(QString::from(json.as_str()));
            });
        }
        // Log capability announces; no UI update needed yet.
        ConnectionEvent::CapabilityAnnounced { peer_id, caps_json } => {
            info!(
                "Capabilities from {}: {}",
                &peer_id[..8.min(peer_id.len())],
                caps_json
            );
        }
        // Endpoint update — re-trigger peer list refresh (presence change).
        ConnectionEvent::EndpointUpdated { peer_id, .. } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                if bridge
                    .rust()
                    .peer_store
                    .as_ref()
                    .map(|ps| ps.read().is_supernode_id(&peer_id))
                    .unwrap_or(false)
                {
                    return;
                }
                if let Some(pid) = lookup_list_peer_id(bridge.rust(), &peer_id) {
                    mark_peer_online(&mut bridge.as_mut().rust_mut(), &pid, true);
                }
                emit_peers_updated(bridge.as_mut());
            });
        }
        // Room list received from supernode — forward to QML.
        ConnectionEvent::RoomListReceived {
            supernode_id,
            rooms_json,
        } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let Some(canon) = bridge.rust().resolve_supernode_node_id_str(&supernode_id) else {
                    return;
                };
                let remote = serde_json::from_str::<serde_json::Value>(&rooms_json)
                    .unwrap_or(serde_json::Value::Array(vec![]));
                let rooms = if let (Some(rs), Some(ps)) = (
                    bridge.rust().room_store.as_ref(),
                    bridge.rust().peer_store.as_ref(),
                ) {
                    let mut store = rs.write();
                    sync_saved_rooms_from_list(&mut store, &canon, &remote);
                    let peer_store = ps.read();
                    let local = local_rooms_json_for_supernode(&store, &peer_store, &canon);
                    let merged = merge_room_list_values(&local, &remote);
                    let filtered = filter_sfu_rooms_for_sidebar(&store, &canon, &merged);
                    enrich_room_voice_participants(
                        filtered,
                        Some(&peer_store),
                        bridge.rust().my_public_id.as_str(),
                    )
                } else {
                    enrich_room_voice_participants(
                        remote,
                        None,
                        bridge.rust().my_public_id.as_str(),
                    )
                };
                let wrapped = serde_json::json!({
                    "supernode_id": canon,
                    "rooms": rooms,
                    "replace": true,
                })
                .to_string();
                bridge
                    .as_mut()
                    .sfu_rooms_updated(QString::from(wrapped.as_str()));
            });
        }
        // Presence update — reflect in peer list.
        ConnectionEvent::PresenceUpdated { peer_id, status } => {
            let online = status != "offline";
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                if bridge
                    .rust()
                    .peer_store
                    .as_ref()
                    .map(|ps| ps.read().is_supernode_id(&peer_id))
                    .unwrap_or(false)
                {
                    return;
                }
                if let Some(pid) = lookup_list_peer_id(bridge.rust(), &peer_id) {
                    mark_peer_online(&mut bridge.as_mut().rust_mut(), &pid, online);
                }
                emit_peers_updated(bridge.as_mut());
            });
        }
        // Invite accepted — new peer added.
        ConnectionEvent::InviteAccepted { peer_id, handle } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let is_supernode = bridge
                    .rust()
                    .peer_store
                    .as_ref()
                    .map(|ps| ps.read().is_supernode_id(&peer_id))
                    .unwrap_or(false);
                if is_supernode {
                    emit_rooms_sidebar_sync(bridge.as_mut());
                    emit_local_rooms_for_all_supernodes(bridge.as_mut());
                    return;
                }
                if let Some(pid) = lookup_list_peer_id(bridge.rust(), &peer_id) {
                    mark_peer_online(&mut bridge.as_mut().rust_mut(), &pid, true);
                }
                emit_peers_updated(bridge.as_mut());
                bridge.as_mut().peer_added(
                    QString::from(peer_id.as_str()),
                    QString::from(handle.as_str()),
                );
            });
        }
        // File transfer events — forward to QML.
        ConnectionEvent::InviteFailed { reason } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                let banner = format!("Invite failed: {reason}");
                bridge
                    .as_mut()
                    .set_session_banner(QString::from(banner.as_str()));
                bridge.as_mut().set_connection_mode(QString::from("error"));
            });
        }
        ConnectionEvent::FileOffered {
            transfer_id,
            peer_id,
            rel_path,
            size,
            purpose,
            is_self,
        } => {
            let json = serde_json::json!({
                "transfer_id": transfer_id,
                "peer_id": peer_id,
                "rel_path": rel_path,
                "size": size,
                "purpose": purpose,
                "is_self": is_self,
            })
            .to_string();
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().file_offered(QString::from(json.as_str()));
            });
        }
        ConnectionEvent::FileProgress {
            transfer_id,
            progress,
        } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .file_progress(QString::from(transfer_id.as_str()), progress);
            });
        }
        ConnectionEvent::FileComplete {
            transfer_id,
            data,
            rel_path,
        } => {
            // Save the received file to the user's downloads folder.
            let saved_path = save_received_file(&rel_path, &data);
            let json = serde_json::json!({
                "transfer_id": transfer_id,
                "rel_path": rel_path,
                "saved_path": saved_path.unwrap_or_default(),
            })
            .to_string();
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().file_complete(QString::from(json.as_str()));
            });
        }
        ConnectionEvent::FileFailed {
            transfer_id,
            reason,
        } => {
            let json = serde_json::json!({
                "transfer_id": transfer_id,
                "reason": reason,
            })
            .to_string();
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().file_failed(QString::from(json.as_str()));
            });
        }
        ConnectionEvent::ConnectionStats { json, .. } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .connection_stats(QString::from(json.as_str()));
            });
        }
        _ => {}
    }
}

fn dispatch_update_event(
    qt_thread: &cxx_qt::CxxQtThread<ffi::AppBridge>,
    ev: crate::github_updater::UpdateEvent,
) {
    use crate::github_updater::UpdateEvent;
    match ev {
        UpdateEvent::UpdateAvailable(release) => {
            let tag = release.tag_name.clone();
            let url = release.html_url.clone();
            info!("Update available: {tag}");
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().rust_mut().pending_release =
                    Some(crate::github_updater::ReleaseInfo {
                        tag_name: tag.clone(),
                        name: None,
                        body: None,
                        html_url: url.clone(),
                    });
                bridge
                    .as_mut()
                    .update_available(QString::from(tag.as_str()), QString::from(url.as_str()));
            });
        }
        UpdateEvent::CheckError(e) => warn!("Update check error: {e}"),
        UpdateEvent::InstallerStarted => info!("Installer launched"),
        UpdateEvent::InstallerError(e) => warn!("Installer error: {e}"),
        UpdateEvent::AlreadyLatest => info!("Already on latest version"),
    }
}

fn dispatch_ollama_event(
    qt_thread: &cxx_qt::CxxQtThread<ffi::AppBridge>,
    ev: crate::ollama_module::OllamaEvent,
) {
    use crate::ollama_module::OllamaEvent;
    match ev {
        OllamaEvent::Chunk(chunk) => {
            let rid = chunk.request_id.clone();
            let text = chunk.text.clone();
            let done = chunk.done;
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                if !text.is_empty() {
                    bridge
                        .as_mut()
                        .ollama_chunk(QString::from(rid.as_str()), QString::from(text.as_str()));
                }
                if done {
                    bridge.as_mut().ollama_done(QString::from(rid.as_str()));
                }
            });
        }
        OllamaEvent::Error {
            request_id,
            message,
        } => {
            warn!("[ollama] error for {request_id}: {message}");
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().ollama_error(
                    QString::from(request_id.as_str()),
                    QString::from(message.as_str()),
                );
            });
        }
        OllamaEvent::Models { models, error } => {
            let models_json = serde_json::to_string(&models).unwrap_or_else(|_| "[]".to_owned());
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().ollama_models_ready(
                    QString::from(models_json.as_str()),
                    QString::from(error.as_str()),
                );
            });
        }
    }
}

fn dispatch_call_event(
    qt_thread: &cxx_qt::CxxQtThread<ffi::AppBridge>,
    ev: crate::call_controller::CallEvent,
) {
    use crate::call_controller::{CallEvent, CallState};
    match ev {
        CallEvent::LocalSpeakingChanged(speaking) => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().local_speaking_changed(speaking);
            });
        }
        CallEvent::LocalLevelChanged(level) => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge.as_mut().set_mic_level(level);
            });
        }
        CallEvent::StateChanged(state) => {
            // When transitioning back to idle, reset mic test state and level.
            if state == CallState::Idle {
                let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                    bridge.as_mut().set_mic_level(0.0);
                    bridge.as_mut().set_mic_test_active(false);
                });
            }
        }
        CallEvent::RemoteSpeakingChanged { peer_id, speaking } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .peer_speaking_changed(QString::from(peer_id.as_str()), speaking);
            });
        }
        CallEvent::RemoteLevelChanged { peer_id, level } => {
            let _ = qt_thread.queue(move |mut bridge: Pin<&mut ffi::AppBridge>| {
                bridge
                    .as_mut()
                    .peer_level_changed(QString::from(peer_id.as_str()), level);
            });
        }
        _ => {}
    }
}
