//! Session lifecycle — everything `nativeStart` sets up and `nativeStop` tears down.
//!
//! A session owns the tokio runtime the client core runs on, the three
//! on-disk stores, and the OS thread that pumps core events into Kotlin.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use conquerd_client::call_controller::{CallCommand, CallController};
use conquerd_client::chat_store::ChatStore;
use conquerd_client::connection_manager::{ConnectionCommand, ConnectionEvent, ConnectionManager};
use conquerd_client::identity::{self, Identity};
use conquerd_client::peer_store::PeerStore;
use conquerd_client::room_store::RoomStore;
use conquerd_client::sfu_client::SfuClient;
use parking_lot::RwLock;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::event;
use crate::sink::EventSink;

/// A running client core.
pub struct Session {
    /// Dropped last, on `stop`: dropping the runtime shuts down every spawned
    /// task, which closes the event channel and lets the pump thread finish.
    runtime: Runtime,
    pub cmd_tx: mpsc::Sender<ConnectionCommand>,
    pub call_tx: mpsc::Sender<CallCommand>,
    pub identity: Arc<Identity>,
    /// The Argon2id-derived key that opened (or sealed) `identity.dat`.
    ///
    /// Held so `identity.export_key` can hand it to Kotlin for the Android
    /// Keystore when - and only when - the user has asked to stay unlocked.
    /// It is the file key, not the passphrase: it opens this one identity on
    /// this one device and is worthless anywhere else.
    pub identity_key: [u8; 32],
    pub peer_store: Arc<RwLock<PeerStore>>,
    pub chat_store: Arc<ChatStore>,
    pub room_store: Arc<RwLock<RoomStore>>,
    /// Cached because `Identity::public_id` allocates and the chat path reads
    /// it for every outbound message.
    pub my_public_id: String,
    /// The live local video capture, if any.
    ///
    /// One at a time: the camera is a single device, and `AndroidCamera::open`
    /// refuses a second capture rather than silently stealing frames from the
    /// first.
    pub video: Arc<RwLock<Option<conquerd_client::video::sender::VideoSender>>>,
    /// A clone of the event sink, so a capture that dies on its own can say so.
    pub sink: EventSink,
    /// Parent room for a create still in flight, keyed `supernode_id:room_name`.
    ///
    /// The supernode's `RoomCreated` reply carries the new room id but not the
    /// parent we asked for, so the intent has to be held between the request
    /// and the reply. Same approach as the desktop bridge's
    /// `pending_sub_room_parent`, and keyed the same way: a name is unique
    /// enough within one host for the moment a create is outstanding.
    pub pending_sub_room_parent: Arc<RwLock<HashMap<String, String>>>,
    /// Cluster rosters learned from `ClusterMembersUpdated`, keyed by the
    /// supernode that reported them.
    ///
    /// A cluster presents as one logical supernode, so the same room can be
    /// recorded under any member's id. Listing rooms without this shows one
    /// row per member instead of one per room, and misses hide state recorded
    /// against a sibling.
    pub cluster_members: Arc<RwLock<HashMap<String, Vec<String>>>>,
    pump: Option<std::thread::JoinHandle<()>>,
    call_pump: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// Unlock (or create) the identity, open the stores, and start the core.
    ///
    /// `home_dir` is the app's private storage directory — everything the
    /// client persists lives under it. An empty `passphrase` means an
    /// unencrypted identity, matching the desktop client's "press Enter for no
    /// passphrase" path.
    ///
    /// `stored_key` is a previously exported file key, unsealed from the
    /// Android Keystore by the Kotlin side when the user turned on staying
    /// unlocked. When present the passphrase is not consulted at all; when it
    /// no longer opens the file (identity replaced, key stale) start-up fails
    /// so the caller falls back to prompting rather than silently running on a
    /// different identity.
    pub fn start(
        home_dir: &str,
        passphrase: &str,
        keyfile_path: &str,
        stored_key: Option<[u8; 32]>,
        sink: EventSink,
    ) -> anyhow::Result<Self> {
        let key_dir = PathBuf::from(home_dir);
        std::fs::create_dir_all(&key_dir)?;

        // The stores resolve their own default paths through
        // `Identity::default_key_dir()`, which reads this. Android has no
        // meaningful HOME, so it must be set before any store is opened.
        std::env::set_var("CONQUERD_HOME", &key_dir);

        let pending_sub_room_parent: Arc<RwLock<HashMap<String, String>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let (identity, identity_key) =
            unlock_identity(&key_dir, passphrase, keyfile_path, stored_key)?;
        let identity = Arc::new(identity);
        let my_public_id = identity.public_id();
        info!(
            "identity unlocked: {} ({})",
            my_public_id,
            identity.peer_id()
        );

        let peer_store = Arc::new(RwLock::new(PeerStore::open(&identity, None)?));
        let chat_store = Arc::new(ChatStore::open(&identity, None)?);
        let room_store = Arc::new(RwLock::new(RoomStore::open(&identity, None)?));
        info!(
            "stores opened: {} peer(s), {} room definition(s)",
            peer_store.read().len(),
            room_store.read().list().len()
        );

        // A multi-thread runtime, as on the desktop: QUIC, the relay client,
        // and the audio pipeline all run concurrently and a current-thread
        // runtime would serialise them behind whichever one is blocking.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("conquerd-core")
            .build()?;

        let (cmd_tx, event_rx, cm_fut) =
            ConnectionManager::split(Arc::clone(&identity), Arc::clone(&peer_store));
        runtime.spawn(cm_fut);

        let (call_tx, call_events, call_fut) = CallController::split(Some(cmd_tx.clone()));
        runtime.spawn(call_fut);

        let (_sfu_tx, _sfu_events, sfu_fut) = SfuClient::split(Some(cmd_tx.clone()));
        runtime.spawn(sfu_fut);

        let cluster_members = Arc::new(RwLock::new(HashMap::new()));
        // The call controller has its own event channel, separate from the
        // connection manager's. Dropping it costs the real call state (the UI
        // is left guessing from signalling, and an answered call still reads
        // "calling"), the speaking indicators, and every capture error.
        let call_pump = spawn_call_event_pump(call_events, sink.clone())?;
        let sink_for_session = sink.clone();

        let pump = spawn_event_pump(
            event_rx,
            sink,
            Arc::clone(&chat_store),
            key_dir.clone(),
            Arc::clone(&room_store),
            Arc::clone(&pending_sub_room_parent),
            Arc::clone(&identity),
            cmd_tx.clone(),
            call_tx.clone(),
            Arc::clone(&cluster_members),
            my_public_id.clone(),
        )?;

        Ok(Self {
            runtime,
            cmd_tx,
            call_tx,
            identity,
            identity_key,
            peer_store,
            chat_store,
            room_store,
            my_public_id,
            pending_sub_room_parent,
            video: Arc::new(RwLock::new(None)),
            sink: sink_for_session,
            cluster_members,
            pump: Some(pump),
            call_pump: Some(call_pump),
        })
    }

    /// Every supernode id this profile might have filed a room under.
    ///
    /// The union of known supernodes and every cluster roster we have seen,
    /// because a room created against one member routinely comes back keyed to
    /// a sibling after a failover.
    pub fn known_supernode_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .peer_store
            .read()
            .supernodes()
            .iter()
            .map(|record| record.identity_pub.clone())
            .collect();

        for (host, members) in self.cluster_members.read().iter() {
            ids.push(host.clone());
            ids.extend(members.iter().cloned());
        }

        ids.sort();
        ids.dedup();
        ids
    }

    /// Queue a command for the connection manager.
    ///
    /// Returns `false` when the channel is full or closed. Callers surface
    /// that to the UI rather than retrying, so a wedged core shows up as a
    /// failed action instead of a silent no-op.
    pub fn send(&self, command: ConnectionCommand) -> bool {
        self.cmd_tx.try_send(command).is_ok()
    }

    /// Shut the core down and wait for the pump thread to finish.
    pub fn stop(mut self) {
        // Release the camera before anything else: the capture thread holds
        // the device, and a session that ends without stopping it leaves the
        // camera indicator lit on a phone with no call in progress.
        if let Some(video) = self.video.write().take() {
            video.stop();
        }

        // Ask the manager to close cleanly first, so peers see a disconnect
        // rather than a dropped socket.
        let _ = self.cmd_tx.try_send(ConnectionCommand::Shutdown);

        // Dropping the runtime cancels the remaining tasks and closes the
        // event channel, which is what ends the pump loop.
        drop(std::mem::replace(
            &mut self.runtime,
            match tokio::runtime::Builder::new_current_thread().build() {
                Ok(rt) => rt,
                Err(e) => {
                    // Nothing to swap in; leave the original runtime to be
                    // dropped with the struct instead of aborting the process.
                    warn!("could not build placeholder runtime: {e}");
                    return;
                }
            },
        ));

        for (name, handle) in [
            ("core event", self.pump.take()),
            ("call event", self.call_pump.take()),
        ] {
            if let Some(handle) = handle {
                if handle.join().is_err() {
                    error!("{name} pump thread panicked");
                }
            }
        }
        info!("session stopped");
    }
}

/// Load the identity at `key_dir`, creating one on first launch.
fn unlock_identity(
    key_dir: &Path,
    passphrase: &str,
    keyfile_path: &str,
    stored_key: Option<[u8; 32]>,
) -> anyhow::Result<(Identity, [u8; 32])> {
    let exists = key_dir.join(identity::IDENTITY_FILENAME).exists();

    // A key from the Keystore skips Argon2id entirely - that is the whole
    // point of staying unlocked, and on a phone the 64 MiB hash is the slowest
    // thing in start-up.
    if let (true, Some(key)) = (exists, stored_key) {
        let identity = Identity::load_encrypted(&key, key_dir)
            .map_err(|e| anyhow::anyhow!("stored unlock key did not open the identity: {e}"))?;
        return Ok((identity, key));
    }

    // Text, keyfile, or both — `build_passphrase_material` concatenates the
    // passphrase with SHA-256 of the file, so the same identity opens on the
    // desktop with the same pair. An empty passphrase with no keyfile stays
    // legal: it is the unencrypted-identity path.
    let material = if keyfile_path.is_empty() {
        passphrase.as_bytes().to_vec()
    } else {
        conquerd_client::crypto::build_passphrase_material(passphrase, keyfile_path)
            .map_err(|e| anyhow::anyhow!("could not read the keyfile: {e}"))?
    };

    if exists {
        return Identity::load_with_passphrase_keyed(&material, key_dir)
            .map_err(|e| anyhow::anyhow!("could not unlock identity: {e}"));
    }

    info!("no identity found — generating one");
    let fresh = Identity::generate();
    let (_, key) = fresh
        .save_encrypted_keyed(&material, key_dir)
        .map_err(|e| anyhow::anyhow!("could not save new identity: {e}"))?;
    Ok((fresh, key))
}

/// Start the thread that forwards core events to Kotlin.
///
/// This is a plain OS thread rather than a tokio task on purpose. Delivering
/// an event means calling into the JVM, which requires the calling thread to
/// stay attached — and tokio moves tasks between worker threads freely, so a
/// task would have to attach and detach around every single event.
#[allow(clippy::too_many_arguments)]
fn spawn_event_pump(
    mut event_rx: mpsc::Receiver<ConnectionEvent>,
    sink: EventSink,
    chat_store: Arc<ChatStore>,
    home_dir: PathBuf,
    room_store: Arc<RwLock<RoomStore>>,
    pending_sub_room_parent: Arc<RwLock<HashMap<String, String>>>,
    identity: Arc<Identity>,
    cmd_tx: mpsc::Sender<ConnectionCommand>,
    call_tx: mpsc::Sender<CallCommand>,
    cluster_members: Arc<RwLock<HashMap<String, Vec<String>>>>,
    my_public_id: String,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("conquerd-events".to_owned())
        .spawn(move || {
            let mut guard = match sink.attach() {
                Ok(g) => g,
                Err(e) => {
                    error!("could not attach event thread to the JVM: {e}");
                    return;
                }
            };

            while let Some(ev) = event_rx.blocking_recv() {
                route_media(&call_tx, &ev);
                persist_if_chat(&chat_store, &ev);
                persist_if_room_chat(&chat_store, &my_public_id, &ev);
                persist_if_room_created(
                    &room_store,
                    &pending_sub_room_parent,
                    &identity,
                    &cmd_tx,
                    &my_public_id,
                    &ev,
                );

                if let ConnectionEvent::ClusterMembersUpdated {
                    supernode_id,
                    members,
                } = &ev
                {
                    cluster_members
                        .write()
                        .insert(supernode_id.clone(), members.clone());
                }

                let saved_file = persist_if_file_complete(&chat_store, &home_dir, &ev);

                let Some(mut payload) = event::to_json(&ev) else {
                    continue;
                };
                // Where the file landed is decided here, not in the core, so
                // it is stamped on after rendering rather than inside it.
                if let Some(path) = saved_file {
                    payload["path"] = serde_json::Value::String(path);
                }
                match serde_json::to_string(&payload) {
                    Ok(json) => sink.emit(&mut guard, &json),
                    Err(e) => warn!("could not encode event: {e}"),
                }
            }

            info!("event pump finished");
        })
}

/// Forward call-controller events to Kotlin.
///
/// A second thread rather than merging into the core pump: each needs its own
/// permanent JVM attachment, and merging two typed tokio channels would mean a
/// select loop inside a thread that is deliberately blocking.
fn spawn_call_event_pump(
    mut call_events: mpsc::Receiver<conquerd_client::call_controller::CallEvent>,
    sink: EventSink,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("conquerd-call-events".to_owned())
        .spawn(move || {
            let mut guard = match sink.attach() {
                Ok(g) => g,
                Err(e) => {
                    error!("could not attach call-event thread to the JVM: {e}");
                    return;
                }
            };

            while let Some(ev) = call_events.blocking_recv() {
                let Some(payload) = event::call_event_to_json(&ev) else {
                    continue;
                };
                match serde_json::to_string(&payload) {
                    Ok(json) => sink.emit(&mut guard, &json),
                    Err(e) => warn!("could not encode call event: {e}"),
                }
            }

            info!("call event pump finished");
        })
}

/// Hand real-time media to the audio pipeline.
///
/// These events never reach Kotlin - they arrive hundreds of times a second
/// and the UI has no use for the bytes - but they still have to go
/// *somewhere*. Filtering them out of the JSON without routing them here is
/// what makes a call connect and stay silent: every inbound frame is dropped.
fn route_media(call_tx: &mpsc::Sender<CallCommand>, event: &ConnectionEvent) {
    // `try_send` rather than blocking: the pump must keep draining the event
    // channel. A full audio queue means playout is already behind, and a
    // dropped frame there is concealed by the jitter buffer, whereas a stalled
    // pump would freeze chat and presence with it.
    let command = match event {
        ConnectionEvent::DirectAudioReceived { peer_id, opus_data } => {
            CallCommand::DirectAudioInbound {
                peer_id: peer_id.clone(),
                opus_data: opus_data.clone(),
            }
        }
        ConnectionEvent::SfuAudioReceived { peer_id, opus_data } => CallCommand::RoomAudioInbound {
            peer_id: peer_id.clone(),
            opus_data: opus_data.clone(),
        },
        _ => return,
    };
    let _ = call_tx.try_send(command);
}

/// Persist inbound room chat, so history survives leaving the room.
fn persist_if_room_chat(chat_store: &ChatStore, my_public_id: &str, event: &ConnectionEvent) {
    use conquerd_client::chat_store::{ChatMessage, MessageKind, MessageStatus};

    let ConnectionEvent::RoomChatMessage {
        supernode_id: _,
        room_id,
        sender_id,
        sender_handle,
        body,
        timestamp,
        message_id,
    } = event
    else {
        return;
    };

    let message_id = if message_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        message_id.clone()
    };

    // Multi-homing means the same frame is delivered once per attached cluster
    // member, so the id check is what keeps one message from being stored
    // three or four times.
    if chat_store
        .get_by_id(&message_id)
        .map(|found| found.is_some())
        .unwrap_or(false)
    {
        return;
    }

    let is_self = sender_id.trim_end_matches('=') == my_public_id.trim_end_matches('=');

    let msg = ChatMessage {
        id: message_id,
        // Keyed on the room alone: a room_id is already a hash over the
        // creator's public id and the room name, so it is the room's identity
        // on every supernode that ever hosts it.
        peer_id: conquerd_client::chat_store::room_conversation_id(room_id),
        sender: sender_id.clone(),
        recipient: String::new(),
        body: body.clone(),
        timestamp: *timestamp,
        is_self,
        status: MessageStatus::Delivered,
        kind: MessageKind::Text,
        attachment_name: String::new(),
        attachment_path: String::new(),
        size_str: String::new(),
        status_note: String::new(),
        sender_handle: sender_handle.clone(),
    };
    if let Err(e) = chat_store.insert(&msg) {
        warn!("could not persist room chat: {e}");
    }
}

/// The one-line label a file shows as in chat, matching the desktop's icons.
pub(crate) fn attachment_label(
    kind: &conquerd_client::chat_store::MessageKind,
    name: &str,
) -> String {
    use conquerd_client::chat_store::MessageKind;
    match kind {
        MessageKind::Image => format!("🖼 {name}"),
        MessageKind::Video => format!("🎬 {name}"),
        _ => format!("📎 {name}"),
    }
}

/// Save a completed download and put it in the chat history.
///
/// Returns the absolute path the file ended up at, which the pump stamps onto
/// the outgoing event — the core hands the UI a payload it cannot serialise,
/// so without this Kotlin would be told a transfer finished and never told
/// where it landed.
///
/// Small files arrive as bytes and are written here; a streamed file was
/// already written and verified by the transfer manager, and is left where it
/// is rather than copied a second time.
fn persist_if_file_complete(
    chat_store: &ChatStore,
    home_dir: &Path,
    event: &ConnectionEvent,
) -> Option<String> {
    use conquerd_client::chat_store::{ChatMessage, MessageStatus};
    use conquerd_client::file_transfer::TransferPayload;

    let ConnectionEvent::FileComplete {
        transfer_id,
        peer_id,
        room_id,
        rel_path,
        payload,
        ..
    } = event
    else {
        return None;
    };

    let saved_path = match payload {
        TransferPayload::SavedAt { path, .. } => path.clone(),
        TransferPayload::Bytes(bytes) => {
            let dir = home_dir.join("received");
            if let Err(e) = std::fs::create_dir_all(&dir) {
                warn!("could not create the received-files directory: {e}");
                return None;
            }
            // Prefix with the transfer id: two peers sending "photo.jpg" must
            // not overwrite each other, and the id is already unique.
            let safe_name = rel_path.replace(['/', '\\'], "_");
            let path = dir.join(format!("{transfer_id}-{safe_name}"));
            if let Err(e) = std::fs::write(&path, bytes) {
                warn!("could not save the received file: {e}");
                return None;
            }
            path.to_string_lossy().into_owned()
        }
    };

    let byte_len = std::fs::metadata(&saved_path).map(|m| m.len()).unwrap_or(0);
    let kind = conquerd_client::chat_store::message_kind_for_path(rel_path);

    // Room files belong to the room conversation, 1:1 files to the peer's.
    let conversation = if room_id.is_empty() {
        peer_id.clone()
    } else {
        room_id.clone()
    };

    let record = ChatMessage {
        id: format!("xfer-{transfer_id}"),
        peer_id: conversation.clone(),
        sender: peer_id.clone(),
        recipient: String::new(),
        body: attachment_label(&kind, rel_path),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
        is_self: false,
        status: MessageStatus::Delivered,
        kind,
        attachment_name: rel_path.clone(),
        attachment_path: saved_path.clone(),
        size_str: conquerd_client::chat_store::format_byte_size(byte_len),
        status_note: String::new(),
        sender_handle: String::new(),
    };
    if let Err(e) = chat_store.upsert(&record) {
        warn!("could not record the received file in history: {e}");
    }

    Some(saved_path)
}

/// Persist a room the supernode just created for us.
///
/// Nothing else on Android writes to the room store — the rooms a phone lists
/// were put there by a desktop client and travelled with the identity — so
/// without this a room created here would show up once and vanish on restart.
///
/// Mirrors `bridge.rs`'s `RoomCreated` handler: skip rooms we already know
/// (a cluster replay or rematerialize re-announces them), persist the entry,
/// then adopt it into the Space tree and announce the signed root. The Space
/// half matters even though this client has no Space UI: a room outside the
/// tree cannot be admitted to by proof, so a phone-created room would behave
/// differently from a desktop-created one.
fn persist_if_room_created(
    room_store: &Arc<RwLock<RoomStore>>,
    pending_sub_room_parent: &Arc<RwLock<HashMap<String, String>>>,
    identity: &Arc<Identity>,
    cmd_tx: &mpsc::Sender<ConnectionCommand>,
    my_public_id: &str,
    event: &ConnectionEvent,
) {
    let ConnectionEvent::RoomCreated {
        supernode_id,
        room_id,
        room_name,
        room_type,
        invite_token,
    } = event
    else {
        return;
    };

    if supernode_id.is_empty() || room_id.is_empty() || room_id == "default" {
        return;
    }

    {
        let store = room_store.read();
        if store.get(supernode_id, room_id).is_some() {
            info!("ignoring RoomCreated for a room already in the store");
            return;
        }
    }

    let entry = conquerd_client::room_store::RoomEntry::new(room_id, room_name)
        .with_type(if room_type.is_empty() {
            "public"
        } else {
            room_type
        })
        .with_supernode(supernode_id)
        .with_creator(my_public_id, true)
        .with_invite_token(invite_token)
        .with_invite_policy("owner");

    if let Err(e) = room_store.write().upsert(entry) {
        warn!("could not persist the created room: {e}");
        return;
    }

    let issued_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // The parent we asked for, if this create came from "inside" another
    // room. Removed rather than read: the intent belongs to one create.
    let parent_node_id = pending_sub_room_parent
        .write()
        .remove(&format!("{supernode_id}:{room_name}"))
        .unwrap_or_default();

    // Best-effort, exactly as on the desktop: a signing or persist hiccup
    // must not lose the room that was already created server-side.
    let adopted = room_store.write().adopt_room_into_space(
        my_public_id,
        supernode_id,
        room_id,
        room_name,
        room_type,
        &parent_node_id,
        issued_at,
        |b| identity.sign(b),
    );
    match adopted {
        Ok(root) => match serde_json::to_string(&root) {
            Ok(root_json) => {
                let _ = cmd_tx.try_send(ConnectionCommand::AnnounceSpaceRoot {
                    supernode_id: supernode_id.clone(),
                    root_json,
                });
            }
            Err(e) => warn!("could not encode the new space root: {e}"),
        },
        Err(e) => warn!("could not adopt the room into the space tree: {e}"),
    }
}

/// Persist inbound chat to the local history before the UI hears about it.
///
/// Order matters: the UI reloads history from the store on the back of these
/// events, so writing after emitting would race a fast reader into showing a
/// message that is not yet saved.
fn persist_if_chat(chat_store: &ChatStore, event: &ConnectionEvent) {
    use conquerd_client::chat_store::{ChatMessage, MessageKind, MessageStatus};

    match event {
        ConnectionEvent::ChatMessage {
            peer_id,
            message_id,
            body,
            timestamp,
            sender_handle,
        } => {
            let msg = ChatMessage {
                id: message_id.clone(),
                peer_id: peer_id.clone(),
                sender: peer_id.clone(),
                recipient: String::new(),
                body: body.clone(),
                timestamp: *timestamp,
                is_self: false,
                status: MessageStatus::Delivered,
                kind: MessageKind::Text,
                attachment_name: String::new(),
                attachment_path: String::new(),
                size_str: String::new(),
                status_note: String::new(),
                sender_handle: sender_handle.clone(),
            };
            if let Err(e) = chat_store.upsert(&msg) {
                warn!("could not persist inbound chat: {e}");
            }
        }
        ConnectionEvent::ChatAck {
            message_id,
            peer_id: _,
        } => {
            if let Err(e) = chat_store.update_status(message_id, MessageStatus::Delivered) {
                warn!("could not record chat ack: {e}");
            }
        }
        ConnectionEvent::ChatSendFailed {
            message_id, reason, ..
        } => {
            if let Err(e) = chat_store.update_status_note(message_id, MessageStatus::Failed, reason)
            {
                warn!("could not record chat failure: {e}");
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conquerd_client::call_controller::CallCommand;

    /// The regression this exists for: filtering media out of the event JSON
    /// without routing it here made calls connect, signal correctly, and stay
    /// completely silent - every inbound frame was dropped, with no error
    /// anywhere. A wiring bug, not a logic bug, so only a wiring test catches
    /// it.
    #[test]
    fn inbound_direct_audio_reaches_the_call_controller() {
        let (tx, mut rx) = mpsc::channel(8);
        route_media(
            &tx,
            &ConnectionEvent::DirectAudioReceived {
                peer_id: "peer-a".to_owned(),
                opus_data: vec![1, 2, 3],
            },
        );

        match rx.try_recv() {
            Ok(CallCommand::DirectAudioInbound { peer_id, opus_data }) => {
                assert_eq!(peer_id, "peer-a");
                assert_eq!(opus_data, vec![1, 2, 3]);
            }
            _ => panic!("direct audio must be handed to the call controller"),
        }
    }

    #[test]
    fn inbound_room_audio_reaches_the_call_controller() {
        let (tx, mut rx) = mpsc::channel(8);
        route_media(
            &tx,
            &ConnectionEvent::SfuAudioReceived {
                peer_id: "peer-b".to_owned(),
                opus_data: vec![9],
            },
        );

        match rx.try_recv() {
            Ok(CallCommand::RoomAudioInbound { peer_id, opus_data }) => {
                assert_eq!(peer_id, "peer-b");
                assert_eq!(opus_data, vec![9]);
            }
            _ => panic!("room audio must be handed to the call controller"),
        }
    }

    #[test]
    fn non_media_events_are_not_routed_to_audio() {
        let (tx, mut rx) = mpsc::channel(8);
        route_media(&tx, &ConnectionEvent::PeerConnected("peer".to_owned()));
        assert!(
            rx.try_recv().is_err(),
            "only media belongs on the call-controller channel",
        );
    }

    /// The pump must keep draining the event channel even when playout is
    /// behind. A dropped frame is concealed by the jitter buffer; a stalled
    /// pump would freeze chat and presence along with the audio.
    #[test]
    fn a_full_audio_queue_drops_rather_than_blocking() {
        let (tx, _rx) = mpsc::channel(1);
        for _ in 0..8 {
            route_media(
                &tx,
                &ConnectionEvent::SfuAudioReceived {
                    peer_id: "peer".to_owned(),
                    opus_data: vec![0],
                },
            );
        }
        // Reaching here without blocking is the assertion.
    }
}
