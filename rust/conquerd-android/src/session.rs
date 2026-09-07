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
    pub peer_store: Arc<RwLock<PeerStore>>,
    pub chat_store: Arc<ChatStore>,
    pub room_store: Arc<RwLock<RoomStore>>,
    /// Cached because `Identity::public_id` allocates and the chat path reads
    /// it for every outbound message.
    pub my_public_id: String,
    /// Cluster rosters learned from `ClusterMembersUpdated`, keyed by the
    /// supernode that reported them.
    ///
    /// A cluster presents as one logical supernode, so the same room can be
    /// recorded under any member's id. Listing rooms without this shows one
    /// row per member instead of one per room, and misses hide state recorded
    /// against a sibling.
    pub cluster_members: Arc<RwLock<HashMap<String, Vec<String>>>>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// Unlock (or create) the identity, open the stores, and start the core.
    ///
    /// `home_dir` is the app's private storage directory — everything the
    /// client persists lives under it. An empty `passphrase` means an
    /// unencrypted identity, matching the desktop client's "press Enter for no
    /// passphrase" path.
    pub fn start(home_dir: &str, passphrase: &str, sink: EventSink) -> anyhow::Result<Self> {
        let key_dir = PathBuf::from(home_dir);
        std::fs::create_dir_all(&key_dir)?;

        // The stores resolve their own default paths through
        // `Identity::default_key_dir()`, which reads this. Android has no
        // meaningful HOME, so it must be set before any store is opened.
        std::env::set_var("CONQUERD_HOME", &key_dir);

        let identity = Arc::new(unlock_identity(&key_dir, passphrase)?);
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

        let (call_tx, _call_events, call_fut) = CallController::split(Some(cmd_tx.clone()));
        runtime.spawn(call_fut);

        let (_sfu_tx, _sfu_events, sfu_fut) = SfuClient::split(Some(cmd_tx.clone()));
        runtime.spawn(sfu_fut);

        let cluster_members = Arc::new(RwLock::new(HashMap::new()));
        let pump = spawn_event_pump(
            event_rx,
            sink,
            Arc::clone(&chat_store),
            call_tx.clone(),
            Arc::clone(&cluster_members),
            my_public_id.clone(),
        )?;

        Ok(Self {
            runtime,
            cmd_tx,
            call_tx,
            identity,
            peer_store,
            chat_store,
            room_store,
            my_public_id,
            cluster_members,
            pump: Some(pump),
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

        if let Some(pump) = self.pump.take() {
            if pump.join().is_err() {
                error!("event pump thread panicked");
            }
        }
        info!("session stopped");
    }
}

/// Load the identity at `key_dir`, creating one on first launch.
fn unlock_identity(key_dir: &Path, passphrase: &str) -> anyhow::Result<Identity> {
    if key_dir.join(identity::IDENTITY_FILENAME).exists() {
        return Identity::load_with_passphrase(passphrase.as_bytes(), key_dir)
            .map_err(|e| anyhow::anyhow!("could not unlock identity: {e}"));
    }

    info!("no identity found — generating one");
    let fresh = Identity::generate();
    fresh
        .save_encrypted(passphrase.as_bytes(), key_dir)
        .map_err(|e| anyhow::anyhow!("could not save new identity: {e}"))?;
    Ok(fresh)
}

/// Start the thread that forwards core events to Kotlin.
///
/// This is a plain OS thread rather than a tokio task on purpose. Delivering
/// an event means calling into the JVM, which requires the calling thread to
/// stay attached — and tokio moves tasks between worker threads freely, so a
/// task would have to attach and detach around every single event.
fn spawn_event_pump(
    mut event_rx: mpsc::Receiver<ConnectionEvent>,
    sink: EventSink,
    chat_store: Arc<ChatStore>,
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

                if let ConnectionEvent::ClusterMembersUpdated {
                    supernode_id,
                    members,
                } = &ev
                {
                    cluster_members
                        .write()
                        .insert(supernode_id.clone(), members.clone());
                }

                let Some(payload) = event::to_json(&ev) else {
                    continue;
                };
                match serde_json::to_string(&payload) {
                    Ok(json) => sink.emit(&mut guard, &json),
                    Err(e) => warn!("could not encode event: {e}"),
                }
            }

            info!("event pump finished");
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
