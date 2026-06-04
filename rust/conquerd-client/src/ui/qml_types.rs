//! QML model types — split into separate files so cxx-qt-build can process
//! one bridge module per file (its current requirement).
//!
//! The individual models now live in:
//!   ui/peer_list_model.rs, ui/chat_model.rs, ui/call_model.rs,
//!   ui/room_model.rs, ui/settings_model.rs
//!
//! This file is retained only to avoid breaking git history but is no longer
//! compiled (removed from ui/mod.rs).

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

// ---------------------------------------------------------------------------
// PeerListModel
// ---------------------------------------------------------------------------

/// Roles for PeerListModel.
#[allow(non_camel_case_types, dead_code)]
mod peer_roles {
    pub const PEER_ID: i32 = 256;
    pub const HANDLE: i32 = 257;
    pub const ONLINE: i32 = 258;
    pub const IN_CALL: i32 = 259;
    pub const BLOCKED: i32 = 260;
}

#[cxx_qt::bridge]
mod peer_list_ffi {
    unsafe extern "C++" {
        include!(<QAbstractListModel>);
        type QAbstractListModel;

        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;

        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;

        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;

        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        type PeerListModel = super::PeerListModelRust;

        #[cxx_override]
        #[rust_name = "row_count"]
        fn rowCount(&self, parent: &QModelIndex) -> i32;

        #[cxx_override]
        fn data(&self, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[rust_name = "role_names"]
        fn roleNames(&self) -> QHash_i32_QByteArray;

        /// Replace the entire peer list. QML re-renders automatically.
        #[qinvokable]
        #[rust_name = "set_peers"]
        fn setPeers(self: Pin<&mut Self>, peers: &QString);

        /// Mark a peer as online/offline.
        #[qinvokable]
        #[rust_name = "set_online"]
        fn setOnline(self: Pin<&mut Self>, peer_id: &QString, online: bool);

        #[inherit]
        #[rust_name = "begin_reset_model"]
        fn beginResetModel(self: Pin<&mut Self>);

        #[inherit]
        #[rust_name = "end_reset_model"]
        fn endResetModel(self: Pin<&mut Self>);

        #[inherit]
        #[rust_name = "data_changed"]
        fn dataChanged(
            self: Pin<&mut Self>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );
    }

    unsafe extern "C++" {
        include!("cxx-qt-lib/qlist.h");
        type QList_i32 = cxx_qt_lib::QList<i32>;
    }
}

#[derive(Default, Clone)]
pub struct PeerEntry {
    pub peer_id: String,
    pub handle: String,
    pub online: bool,
    pub in_call: bool,
    pub blocked: bool,
}

#[derive(Default)]
pub struct PeerListModelRust {
    peers: Vec<PeerEntry>,
}

impl peer_list_ffi::PeerListModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.peers.len() as i32
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(entry) = self.peers.get(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            r if r == peer_roles::PEER_ID => QString::from(entry.peer_id.as_str()).into(),
            r if r == peer_roles::HANDLE => QString::from(entry.handle.as_str()).into(),
            r if r == peer_roles::ONLINE => entry.online.into(),
            r if r == peer_roles::IN_CALL => entry.in_call.into(),
            r if r == peer_roles::BLOCKED => entry.blocked.into(),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut h = QHash::<QHashPair_i32_QByteArray>::default();
        h.insert(peer_roles::PEER_ID, QByteArray::from("peerId"));
        h.insert(peer_roles::HANDLE, QByteArray::from("handle"));
        h.insert(peer_roles::ONLINE, QByteArray::from("online"));
        h.insert(peer_roles::IN_CALL, QByteArray::from("inCall"));
        h.insert(peer_roles::BLOCKED, QByteArray::from("blocked"));
        h
    }

    /// Accept a JSON array of peer objects: `[{"peer_id":"…","handle":"…",…},…]`
    fn set_peers(mut self: Pin<&mut Self>, peers_json: &QString) {
        let json_str = peers_json.to_string();
        if let Ok(entries) = parse_peer_entries(&json_str) {
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().peers = entries;
            self.as_mut().end_reset_model();
        }
    }

    fn set_online(mut self: Pin<&mut Self>, peer_id: &QString, online: bool) {
        let id = peer_id.to_string();
        if let Some(idx) = self.rust().peers.iter().position(|p| p.peer_id == id) {
            self.as_mut().rust_mut().peers[idx].online = online;
            // emit dataChanged for that row
            let tl = self.as_ref().index(idx as i32, 0, &QModelIndex::default());
            let br = tl.clone();
            let mut roles = cxx_qt_lib::QList::<i32>::default();
            roles.append(peer_roles::ONLINE);
            self.as_mut().data_changed(&tl, &br, &roles);
        }
    }
}

fn parse_peer_entries(json: &str) -> Result<Vec<PeerEntry>, serde_json::Error> {
    #[derive(serde::Deserialize)]
    struct Row {
        peer_id: String,
        #[serde(default)]
        handle: String,
        #[serde(default)]
        online: bool,
        #[serde(default)]
        in_call: bool,
        #[serde(default)]
        blocked: bool,
    }
    let rows: Vec<Row> = serde_json::from_str(json)?;
    Ok(rows
        .into_iter()
        .map(|r| PeerEntry {
            peer_id: r.peer_id,
            handle: r.handle,
            online: r.online,
            in_call: r.in_call,
            blocked: r.blocked,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// ChatModel
// ---------------------------------------------------------------------------

mod chat_roles {
    pub const MSG_ID: i32 = 256;
    pub const SENDER: i32 = 257;
    pub const BODY: i32 = 258;
    pub const TIMESTAMP: i32 = 259;
    pub const KIND: i32 = 260;
    pub const MINE: i32 = 261;
}

#[cxx_qt::bridge]
mod chat_model_ffi {
    unsafe extern "C++" {
        include!(<QAbstractListModel>);
        type QAbstractListModel;

        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;

        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;

        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;

        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;

        include!("cxx-qt-lib/qlist.h");
        type QList_i32 = cxx_qt_lib::QList<i32>;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        type ChatModel = super::ChatModelRust;

        #[cxx_override]
        #[rust_name = "row_count"]
        fn rowCount(&self, parent: &QModelIndex) -> i32;

        #[cxx_override]
        fn data(&self, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[rust_name = "role_names"]
        fn roleNames(&self) -> QHash_i32_QByteArray;

        /// Replace all messages for a peer (JSON array).
        #[qinvokable]
        #[rust_name = "set_messages"]
        fn setMessages(self: Pin<&mut Self>, messages_json: &QString);

        /// Append a single new message (JSON object).
        #[qinvokable]
        #[rust_name = "append_message"]
        fn appendMessage(self: Pin<&mut Self>, message_json: &QString);

        #[inherit]
        #[rust_name = "begin_reset_model"]
        fn beginResetModel(self: Pin<&mut Self>);

        #[inherit]
        #[rust_name = "end_reset_model"]
        fn endResetModel(self: Pin<&mut Self>);

        #[inherit]
        #[rust_name = "begin_insert_rows"]
        fn beginInsertRows(self: Pin<&mut Self>, parent: &QModelIndex, first: i32, last: i32);

        #[inherit]
        #[rust_name = "end_insert_rows"]
        fn endInsertRows(self: Pin<&mut Self>);
    }
}

#[derive(Default, Clone)]
pub struct ChatEntry {
    pub msg_id: String,
    pub sender: String,
    pub body: String,
    pub timestamp: f64,
    pub kind: String,
    pub mine: bool,
}

#[derive(Default)]
pub struct ChatModelRust {
    messages: Vec<ChatEntry>,
}

impl chat_model_ffi::ChatModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.messages.len() as i32
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(e) = self.messages.get(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            r if r == chat_roles::MSG_ID => QString::from(e.msg_id.as_str()).into(),
            r if r == chat_roles::SENDER => QString::from(e.sender.as_str()).into(),
            r if r == chat_roles::BODY => QString::from(e.body.as_str()).into(),
            r if r == chat_roles::TIMESTAMP => e.timestamp.into(),
            r if r == chat_roles::KIND => QString::from(e.kind.as_str()).into(),
            r if r == chat_roles::MINE => e.mine.into(),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut h = QHash::<QHashPair_i32_QByteArray>::default();
        h.insert(chat_roles::MSG_ID, QByteArray::from("msgId"));
        h.insert(chat_roles::SENDER, QByteArray::from("sender"));
        h.insert(chat_roles::BODY, QByteArray::from("body"));
        h.insert(chat_roles::TIMESTAMP, QByteArray::from("timestamp"));
        h.insert(chat_roles::KIND, QByteArray::from("kind"));
        h.insert(chat_roles::MINE, QByteArray::from("mine"));
        h
    }

    fn set_messages(mut self: Pin<&mut Self>, json: &QString) {
        if let Ok(entries) = parse_chat_entries(&json.to_string()) {
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().messages = entries;
            self.as_mut().end_reset_model();
        }
    }

    fn append_message(mut self: Pin<&mut Self>, json: &QString) {
        if let Ok(entry) = parse_chat_entry(&json.to_string()) {
            let row = self.messages.len() as i32;
            let parent = QModelIndex::default();
            self.as_mut().begin_insert_rows(&parent, row, row);
            self.as_mut().rust_mut().messages.push(entry);
            self.as_mut().end_insert_rows();
        }
    }
}

fn parse_chat_entries(json: &str) -> Result<Vec<ChatEntry>, serde_json::Error> {
    #[derive(serde::Deserialize)]
    struct Row {
        #[serde(default)]
        msg_id: String,
        #[serde(default)]
        sender: String,
        body: String,
        #[serde(default)]
        timestamp: f64,
        #[serde(default = "default_kind")]
        kind: String,
        #[serde(default)]
        mine: bool,
    }
    fn default_kind() -> String { "text".into() }
    let rows: Vec<Row> = serde_json::from_str(json)?;
    Ok(rows.into_iter().map(|r| ChatEntry {
        msg_id: r.msg_id,
        sender: r.sender,
        body: r.body,
        timestamp: r.timestamp,
        kind: r.kind,
        mine: r.mine,
    }).collect())
}

fn parse_chat_entry(json: &str) -> Result<ChatEntry, serde_json::Error> {
    parse_chat_entries(&format!("[{json}]")).map(|mut v| v.remove(0))
}

// ---------------------------------------------------------------------------
// CallModel — singleton QObject tracking live call state
// ---------------------------------------------------------------------------

#[cxx_qt::bridge]
mod call_model_ffi {
    unsafe extern "RustQt" {
        #[qobject]
        #[qproperty(QString, state)]
        #[qproperty(QString, peer_id)]
        #[qproperty(bool, muted)]
        #[qproperty(i32, duration_secs)]
        type CallModel = super::CallModelRust;

        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }
}

#[derive(Default)]
pub struct CallModelRust {
    state: QString,      // "idle" | "connecting" | "active" | "ending"
    peer_id: QString,
    muted: bool,
    duration_secs: i32,
}

// ---------------------------------------------------------------------------
// RoomModel
// ---------------------------------------------------------------------------

mod room_roles {
    pub const PEER_ID: i32 = 256;
    pub const HANDLE: i32 = 257;
    pub const SPEAKING: i32 = 258;
    pub const MUTED: i32 = 259;
}

#[cxx_qt::bridge]
mod room_model_ffi {
    unsafe extern "C++" {
        include!(<QAbstractListModel>);
        type QAbstractListModel;

        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;

        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;

        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;

        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;

        include!("cxx-qt-lib/qlist.h");
        type QList_i32 = cxx_qt_lib::QList<i32>;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        type RoomModel = super::RoomModelRust;

        #[cxx_override]
        #[rust_name = "row_count"]
        fn rowCount(&self, parent: &QModelIndex) -> i32;

        #[cxx_override]
        fn data(&self, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[rust_name = "role_names"]
        fn roleNames(&self) -> QHash_i32_QByteArray;

        /// Replace participant list (JSON array).
        #[qinvokable]
        #[rust_name = "set_participants"]
        fn setParticipants(self: Pin<&mut Self>, json: &QString);

        /// Update speaking/muted state for one participant.
        #[qinvokable]
        #[rust_name = "update_participant"]
        fn updateParticipant(self: Pin<&mut Self>, peer_id: &QString, speaking: bool, muted: bool);

        #[inherit]
        #[rust_name = "begin_reset_model"]
        fn beginResetModel(self: Pin<&mut Self>);

        #[inherit]
        #[rust_name = "end_reset_model"]
        fn endResetModel(self: Pin<&mut Self>);

        #[inherit]
        #[rust_name = "data_changed"]
        fn dataChanged(
            self: Pin<&mut Self>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );
    }
}

#[derive(Default, Clone)]
pub struct RoomParticipant {
    pub peer_id: String,
    pub handle: String,
    pub speaking: bool,
    pub muted: bool,
}

#[derive(Default)]
pub struct RoomModelRust {
    participants: Vec<RoomParticipant>,
}

impl room_model_ffi::RoomModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.participants.len() as i32
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(p) = self.participants.get(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            r if r == room_roles::PEER_ID => QString::from(p.peer_id.as_str()).into(),
            r if r == room_roles::HANDLE => QString::from(p.handle.as_str()).into(),
            r if r == room_roles::SPEAKING => p.speaking.into(),
            r if r == room_roles::MUTED => p.muted.into(),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut h = QHash::<QHashPair_i32_QByteArray>::default();
        h.insert(room_roles::PEER_ID, QByteArray::from("peerId"));
        h.insert(room_roles::HANDLE, QByteArray::from("handle"));
        h.insert(room_roles::SPEAKING, QByteArray::from("speaking"));
        h.insert(room_roles::MUTED, QByteArray::from("muted"));
        h
    }

    fn set_participants(mut self: Pin<&mut Self>, json: &QString) {
        #[derive(serde::Deserialize)]
        struct Row {
            peer_id: String,
            #[serde(default)]
            handle: String,
            #[serde(default)]
            speaking: bool,
            #[serde(default)]
            muted: bool,
        }
        if let Ok(rows) = serde_json::from_str::<Vec<Row>>(&json.to_string()) {
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().participants = rows
                .into_iter()
                .map(|r| RoomParticipant {
                    peer_id: r.peer_id,
                    handle: r.handle,
                    speaking: r.speaking,
                    muted: r.muted,
                })
                .collect();
            self.as_mut().end_reset_model();
        }
    }

    fn update_participant(mut self: Pin<&mut Self>, peer_id: &QString, speaking: bool, muted: bool) {
        let id = peer_id.to_string();
        if let Some(idx) = self.rust().participants.iter().position(|p| p.peer_id == id) {
            {
                let p = &mut self.as_mut().rust_mut().participants[idx];
                p.speaking = speaking;
                p.muted = muted;
            }
            let tl = self.as_ref().index(idx as i32, 0, &QModelIndex::default());
            let br = tl.clone();
            let mut roles = cxx_qt_lib::QList::<i32>::default();
            roles.append(room_roles::SPEAKING);
            roles.append(room_roles::MUTED);
            self.as_mut().data_changed(&tl, &br, &roles);
        }
    }
}

// ---------------------------------------------------------------------------
// SettingsModel — writable settings singleton
// ---------------------------------------------------------------------------

#[cxx_qt::bridge]
mod settings_model_ffi {
    unsafe extern "RustQt" {
        #[qobject]
        #[qproperty(bool, notifications_enabled)]
        #[qproperty(bool, auto_connect)]
        #[qproperty(bool, start_minimized)]
        #[qproperty(bool, push_to_talk)]
        #[qproperty(QString, audio_input_device)]
        #[qproperty(QString, audio_output_device)]
        #[qproperty(i32, relay_port)]
        type SettingsModel = super::SettingsModelRust;

        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;

        /// Persist settings to disk (JSON). Path is configured on the Rust side.
        #[qinvokable]
        fn save(self: Pin<&mut Self>);

        /// Load settings from disk.
        #[qinvokable]
        fn load(self: Pin<&mut Self>);
    }
}

#[derive(Default)]
pub struct SettingsModelRust {
    notifications_enabled: bool,
    auto_connect: bool,
    start_minimized: bool,
    push_to_talk: bool,
    audio_input_device: QString,
    audio_output_device: QString,
    relay_port: i32,
}

impl settings_model_ffi::SettingsModel {
    fn save(self: Pin<&mut Self>) {
        // TODO: serialize to JSON and write to disk via the settings module
    }

    fn load(mut self: Pin<&mut Self>) {
        // TODO: read from disk and update properties
        let _ = self.as_mut();
    }
}
