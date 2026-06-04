//! PeerListModel — QAbstractListModel backing the peer navigation rail.
//!
//! Compiled only when the `qt-ui` Cargo feature is enabled.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

mod peer_roles {
    pub const PEER_ID: i32 = 256;
    pub const HANDLE: i32 = 257;
    pub const ONLINE: i32 = 258;
    pub const IN_CALL: i32 = 259;
    pub const BLOCKED: i32 = 260;
    pub const UNREAD_COUNT: i32 = 261;
    pub const LAST_PREVIEW: i32 = 262;
    pub const IS_TYPING: i32 = 263;
}

#[cxx_qt::bridge]
pub mod ffi {
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
        #[qml_element]
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

        /// Replace the entire peer list (JSON array). QML re-renders automatically.
        #[qinvokable]
        #[rust_name = "set_peers"]
        fn setPeers(self: Pin<&mut Self>, peers: &QString);

        /// Mark a peer as online/offline.
        #[qinvokable]
        #[rust_name = "set_online"]
        fn setOnline(self: Pin<&mut Self>, peer_id: &QString, online: bool);

        /// Update unread message count for a peer.
        #[qinvokable]
        #[rust_name = "set_peer_unread"]
        fn setPeerUnread(self: Pin<&mut Self>, peer_id: &QString, count: i32);

        /// Update the last-message preview snippet for a peer.
        #[qinvokable]
        #[rust_name = "set_peer_preview"]
        fn setPeerPreview(self: Pin<&mut Self>, peer_id: &QString, text: &QString);

        /// Set or clear the typing indicator for a peer.
        #[qinvokable]
        #[rust_name = "set_typing"]
        fn setTyping(self: Pin<&mut Self>, peer_id: &QString, is_typing: bool);

        #[inherit]
        #[rust_name = "begin_reset_model"]
        fn beginResetModel(self: Pin<&mut Self>);

        #[inherit]
        #[rust_name = "end_reset_model"]
        fn endResetModel(self: Pin<&mut Self>);

        /// Emit dataChanged for a single-row update so QML refreshes only
        /// the affected delegate (and preserves selection/scroll state)
        /// instead of tearing down and rebuilding the whole list.
        #[inherit]
        #[rust_name = "data_changed"]
        fn dataChanged(
            self: Pin<&mut Self>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );

        #[inherit]
        fn index(&self, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;
    }
}

pub use ffi::PeerListModel;

#[derive(Default, Clone)]
pub struct PeerEntry {
    pub peer_id: String,
    pub handle: String,
    pub online: bool,
    pub in_call: bool,
    pub blocked: bool,
    pub unread_count: i32,
    pub last_preview: String,
    pub is_typing: bool,
}

#[derive(Default)]
pub struct PeerListModelRust {
    peers: Vec<PeerEntry>,
}

impl ffi::PeerListModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.peers.len() as i32
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(entry) = self.peers.get(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            r if r == peer_roles::PEER_ID => (&QString::from(entry.peer_id.as_str())).into(),
            r if r == peer_roles::HANDLE => (&QString::from(entry.handle.as_str())).into(),
            r if r == peer_roles::ONLINE => QVariant::from(&entry.online),
            r if r == peer_roles::IN_CALL => QVariant::from(&entry.in_call),
            r if r == peer_roles::BLOCKED => QVariant::from(&entry.blocked),
            r if r == peer_roles::UNREAD_COUNT => QVariant::from(&entry.unread_count),
            r if r == peer_roles::LAST_PREVIEW => {
                (&QString::from(entry.last_preview.as_str())).into()
            }
            r if r == peer_roles::IS_TYPING => QVariant::from(&entry.is_typing),
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
        h.insert(peer_roles::UNREAD_COUNT, QByteArray::from("unreadCount"));
        h.insert(peer_roles::LAST_PREVIEW, QByteArray::from("lastPreview"));
        h.insert(peer_roles::IS_TYPING, QByteArray::from("isTyping"));
        h
    }

    fn set_peers(mut self: Pin<&mut Self>, peers_json: &QString) {
        if let Ok(entries) = parse_peer_entries(&peers_json.to_string()) {
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().peers = entries;
            self.as_mut().end_reset_model();
        }
    }

    fn set_online(mut self: Pin<&mut Self>, peer_id: &QString, online: bool) {
        let id = peer_id.to_string();
        if let Some(idx) = self.rust().peers.iter().position(|p| p.peer_id == id) {
            self.as_mut().rust_mut().peers[idx].online = online;
            emit_row_changed(self.as_mut(), idx as i32, peer_roles::ONLINE);
        }
    }

    fn set_peer_unread(mut self: Pin<&mut Self>, peer_id: &QString, count: i32) {
        let id = peer_id.to_string();
        if let Some(idx) = self.rust().peers.iter().position(|p| p.peer_id == id) {
            self.as_mut().rust_mut().peers[idx].unread_count = count.max(0);
            emit_row_changed(self.as_mut(), idx as i32, peer_roles::UNREAD_COUNT);
        }
    }

    fn set_peer_preview(mut self: Pin<&mut Self>, peer_id: &QString, text: &QString) {
        let id = peer_id.to_string();
        let txt = text.to_string();
        if let Some(idx) = self.rust().peers.iter().position(|p| p.peer_id == id) {
            self.as_mut().rust_mut().peers[idx].last_preview = txt;
            emit_row_changed(self.as_mut(), idx as i32, peer_roles::LAST_PREVIEW);
        }
    }

    fn set_typing(mut self: Pin<&mut Self>, peer_id: &QString, is_typing: bool) {
        let id = peer_id.to_string();
        if let Some(idx) = self.rust().peers.iter().position(|p| p.peer_id == id) {
            self.as_mut().rust_mut().peers[idx].is_typing = is_typing;
            emit_row_changed(self.as_mut(), idx as i32, peer_roles::IS_TYPING);
        }
    }
}

/// Helper: emit `dataChanged(row..row)` for a single role so QML refreshes
/// only the affected delegate. Replaces the old begin_reset_model /
/// end_reset_model pattern which caused selection loss and frame drops
/// when many peers updated at once.
fn emit_row_changed(model: Pin<&mut ffi::PeerListModel>, row: i32, role: i32) {
    let parent = QModelIndex::default();
    let tl = model.as_ref().index(row, 0, &parent);
    let br = tl.clone();
    let mut roles = cxx_qt_lib::QList::<i32>::default();
    roles.append(role);
    model.data_changed(&tl, &br, &roles);
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
        #[serde(default)]
        unread_count: i32,
        #[serde(default)]
        last_preview: String,
        #[serde(default)]
        is_typing: bool,
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
            unread_count: r.unread_count,
            last_preview: r.last_preview,
            is_typing: r.is_typing,
        })
        .collect())
}
