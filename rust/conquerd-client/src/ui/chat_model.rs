//! ChatModel — QAbstractListModel of messages for one peer conversation.
//!
//! Compiled only when the `qt-ui` Cargo feature is enabled.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

mod chat_roles {
    pub const MSG_ID: i32 = 256;
    pub const SENDER: i32 = 257;
    pub const BODY: i32 = 258;
    pub const TIMESTAMP: i32 = 259;
    pub const KIND: i32 = 260;
    pub const MINE: i32 = 261;
    pub const STATUS: i32 = 262;
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
        type ChatModel = super::ChatModelRust;

        #[cxx_override]
        #[rust_name = "row_count"]
        fn rowCount(&self, parent: &QModelIndex) -> i32;

        #[cxx_override]
        fn data(&self, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[rust_name = "role_names"]
        fn roleNames(&self) -> QHash_i32_QByteArray;

        /// Replace all messages for the active conversation (JSON array).
        #[qinvokable]
        #[rust_name = "set_messages"]
        fn setMessages(self: Pin<&mut Self>, messages_json: &QString);

        /// Append a single new message (JSON object).
        #[qinvokable]
        #[rust_name = "append_message"]
        fn appendMessage(self: Pin<&mut Self>, message_json: &QString);

        /// Update the delivery status of a message by ID.
        /// `status` is one of: "sending", "sent", "delivered", "read", "failed".
        #[qinvokable]
        #[rust_name = "update_message_status"]
        fn updateMessageStatus(self: Pin<&mut Self>, msg_id: &QString, status: &QString);

        /// Remove a single message from the model by its ID.
        #[qinvokable]
        #[rust_name = "remove_message"]
        fn removeMessage(self: Pin<&mut Self>, msg_id: &QString);

        /// Clear all messages from the model.
        #[qinvokable]
        #[rust_name = "clear_messages"]
        fn clearMessages(self: Pin<&mut Self>);

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

        #[inherit]
        #[rust_name = "begin_remove_rows"]
        fn beginRemoveRows(self: Pin<&mut Self>, parent: &QModelIndex, first: i32, last: i32);

        #[inherit]
        #[rust_name = "end_remove_rows"]
        fn endRemoveRows(self: Pin<&mut Self>);

        /// Emit dataChanged for a single-row update so QML refreshes only
        /// the affected delegate (preserves selection / scroll position)
        /// instead of resetting the whole conversation list.
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

pub use ffi::ChatModel;

#[derive(Default, Clone)]
pub struct ChatEntry {
    pub msg_id: String,
    pub sender: String,
    pub body: String,
    pub timestamp: f64,
    pub kind: String,
    pub mine: bool,
    pub status: String,
}

#[derive(Default)]
pub struct ChatModelRust {
    messages: Vec<ChatEntry>,
}

impl ffi::ChatModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.messages.len() as i32
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(e) = self.messages.get(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            r if r == chat_roles::MSG_ID => (&QString::from(e.msg_id.as_str())).into(),
            r if r == chat_roles::SENDER => (&QString::from(e.sender.as_str())).into(),
            r if r == chat_roles::BODY => (&QString::from(e.body.as_str())).into(),
            r if r == chat_roles::TIMESTAMP => (&e.timestamp).into(),
            r if r == chat_roles::KIND => (&QString::from(e.kind.as_str())).into(),
            r if r == chat_roles::MINE => QVariant::from(&e.mine),
            r if r == chat_roles::STATUS => (&QString::from(e.status.as_str())).into(),
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
        h.insert(chat_roles::STATUS, QByteArray::from("status"));
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

    fn update_message_status(mut self: Pin<&mut Self>, msg_id: &QString, status: &QString) {
        let id = msg_id.to_string();
        let st = status.to_string();
        if let Some(idx) = self.rust().messages.iter().position(|m| m.msg_id == id) {
            self.as_mut().rust_mut().messages[idx].status = st;
            let parent = QModelIndex::default();
            let tl = self.as_ref().index(idx as i32, 0, &parent);
            let br = tl.clone();
            let mut roles = cxx_qt_lib::QList::<i32>::default();
            roles.append(chat_roles::STATUS);
            self.as_mut().data_changed(&tl, &br, &roles);
        }
    }

    fn remove_message(mut self: Pin<&mut Self>, msg_id: &QString) {
        let id = msg_id.to_string();
        if let Some(idx) = self.rust().messages.iter().position(|m| m.msg_id == id) {
            let parent = QModelIndex::default();
            self.as_mut()
                .begin_remove_rows(&parent, idx as i32, idx as i32);
            self.as_mut().rust_mut().messages.remove(idx);
            self.as_mut().end_remove_rows();
        }
    }

    fn clear_messages(mut self: Pin<&mut Self>) {
        self.as_mut().begin_reset_model();
        self.as_mut().rust_mut().messages.clear();
        self.as_mut().end_reset_model();
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
        #[serde(default)]
        status: String,
    }
    fn default_kind() -> String {
        "text".into()
    }
    let rows: Vec<Row> = serde_json::from_str(json)?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let status = if r.status.is_empty() {
                if r.mine {
                    "sending".to_owned()
                } else {
                    "delivered".to_owned()
                }
            } else {
                r.status
            };
            ChatEntry {
                msg_id: r.msg_id,
                sender: r.sender,
                body: r.body,
                timestamp: r.timestamp,
                kind: r.kind,
                mine: r.mine,
                status,
            }
        })
        .collect())
}

fn parse_chat_entry(json: &str) -> Result<ChatEntry, serde_json::Error> {
    parse_chat_entries(&format!("[{json}]")).map(|mut v| v.remove(0))
}
