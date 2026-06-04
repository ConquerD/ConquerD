//! RoomModel — QAbstractListModel of voice-room participants.
//!
//! Compiled only when the `qt-ui` Cargo feature is enabled.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

mod room_roles {
    pub const PEER_ID: i32 = 256;
    pub const HANDLE: i32 = 257;
    pub const SPEAKING: i32 = 258;
    pub const MUTED: i32 = 259;
    pub const AUDIO_LEVEL: i32 = 260;
    pub const IS_SELF: i32 = 261;
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
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
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

        /// Replace the full participant list (JSON array).
        #[qinvokable]
        #[rust_name = "set_participants"]
        fn setParticipants(self: Pin<&mut Self>, json: &QString);

        /// Update speaking/muted state for one participant.
        #[qinvokable]
        #[rust_name = "update_participant"]
        fn updateParticipant(self: Pin<&mut Self>, peer_id: &QString, speaking: bool, muted: bool);

        /// Returns the number of participants — callable from QML as model.participantCount()
        #[qinvokable]
        #[rust_name = "participant_count"]
        fn participantCount(&self) -> i32;

        /// Update the normalised audio level (0.0–1.0) for one participant.
        /// Called by the per-peer RMS pipeline at ≤10 Hz per peer.
        #[qinvokable]
        #[rust_name = "set_audio_level"]
        fn setAudioLevel(self: Pin<&mut Self>, peer_id: &QString, level: f32);

        #[inherit]
        #[rust_name = "begin_reset_model"]
        fn beginResetModel(self: Pin<&mut Self>);

        #[inherit]
        #[rust_name = "end_reset_model"]
        fn endResetModel(self: Pin<&mut Self>);
    }
}

pub use ffi::RoomModel;

#[derive(Default, Clone)]
pub struct RoomParticipant {
    pub peer_id: String,
    pub handle: String,
    pub speaking: bool,
    pub muted: bool,
    pub audio_level: f32,
    pub is_self: bool,
}

#[derive(Default)]
pub struct RoomModelRust {
    participants: Vec<RoomParticipant>,
}

impl ffi::RoomModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.participants.len() as i32
    }

    fn participant_count(&self) -> i32 {
        self.participants.len() as i32
    }
    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(p) = self.participants.get(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            r if r == room_roles::PEER_ID => (&QString::from(p.peer_id.as_str())).into(),
            r if r == room_roles::HANDLE => (&QString::from(p.handle.as_str())).into(),
            r if r == room_roles::SPEAKING => QVariant::from(&p.speaking),
            r if r == room_roles::MUTED => QVariant::from(&p.muted),
            r if r == room_roles::AUDIO_LEVEL => (&p.audio_level).into(),
            r if r == room_roles::IS_SELF => QVariant::from(&p.is_self),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut h = QHash::<QHashPair_i32_QByteArray>::default();
        h.insert(room_roles::PEER_ID, QByteArray::from("peerId"));
        h.insert(room_roles::HANDLE, QByteArray::from("handle"));
        h.insert(room_roles::SPEAKING, QByteArray::from("speaking"));
        h.insert(room_roles::MUTED, QByteArray::from("muted"));
        h.insert(room_roles::AUDIO_LEVEL, QByteArray::from("audioLevel"));
        h.insert(room_roles::IS_SELF, QByteArray::from("isSelf"));
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
            #[serde(default)]
            audio_level: f32,
            #[serde(default)]
            is_self: bool,
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
                    audio_level: r.audio_level,
                    is_self: r.is_self,
                })
                .collect();
            self.as_mut().end_reset_model();
        }
    }

    fn update_participant(
        mut self: Pin<&mut Self>,
        peer_id: &QString,
        speaking: bool,
        muted: bool,
    ) {
        let id = peer_id.to_string();
        if let Some(idx) = self
            .rust()
            .participants
            .iter()
            .position(|p| p.peer_id == id)
        {
            self.as_mut().begin_reset_model();
            let p = &mut self.as_mut().rust_mut().participants[idx];
            p.speaking = speaking;
            p.muted = muted;
            // Derive a synthetic audio level from speaking state when no VAD value
            // is available; real VAD values arrive via setParticipants JSON.
            if speaking && p.audio_level < 0.1 {
                p.audio_level = 0.7;
            } else if !speaking {
                p.audio_level = 0.0;
            }
            self.as_mut().end_reset_model();
        }
    }

    fn set_audio_level(mut self: Pin<&mut Self>, peer_id: &QString, level: f32) {
        let id = peer_id.to_string();
        if let Some(idx) = self
            .rust()
            .participants
            .iter()
            .position(|p| p.peer_id == id)
        {
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().participants[idx].audio_level = level.clamp(0.0, 1.0);
            self.as_mut().end_reset_model();
        }
    }
}
