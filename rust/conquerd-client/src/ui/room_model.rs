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
    pub const ONLINE: i32 = 262;
    /// Muted by *this listener* only — distinct from [`MUTED`], which is the
    /// participant's own microphone state and is visible to everyone.
    pub const LOCAL_MUTED: i32 = 263;
    /// This listener's playback volume for the participant (0â€“200, 100 = unity).
    pub const LOCAL_VOLUME: i32 = 264;
    /// Whether the participant is currently sending video.
    pub const VIDEO_ACTIVE: i32 = 265;
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

        /// Mirror this listener's local mute/volume preference for a peer.
        #[qinvokable]
        #[rust_name = "set_local_audio"]
        fn setLocalAudio(self: Pin<&mut Self>, peer_id: &QString, muted: bool, volume: i32);

        /// Flag whether a peer is currently sending video.
        #[qinvokable]
        #[rust_name = "set_video_active"]
        fn setVideoActive(self: Pin<&mut Self>, peer_id: &QString, active: bool);

        /// Display handle for one peer, or an empty string if unknown.
        ///
        /// Exists because a `QAbstractListModel` exposes its roles only to
        /// delegates — QML code outside a delegate (the video region iterates
        /// expanded peer ids, not model rows) has no way to read them. Without
        /// this, such code falls back to showing the raw 44-character peer id.
        #[qinvokable]
        #[rust_name = "handle_for"]
        fn handleFor(&self, peer_id: &QString) -> QString;

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

        #[inherit]
        fn index(&self, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;
    }
}

pub use ffi::RoomModel;

/// serde default for the `online` field: entries in a room roster are
/// live-present members unless a producer explicitly says otherwise.
fn default_online() -> bool {
    true
}

/// serde default for `local_volume`: unity, so a producer that omits the field
/// never accidentally silences a participant.
fn default_volume() -> i32 {
    100
}

#[derive(Clone)]
pub struct RoomParticipant {
    pub peer_id: String,
    pub handle: String,
    pub speaking: bool,
    pub muted: bool,
    pub audio_level: f32,
    pub is_self: bool,
    /// Presence in the room. Room roster entries are live-present members, so
    /// this is normally `true`; it exists so the members list can render an
    /// online/offline indicator without inventing UI-only state.
    pub online: bool,
    /// Muted by this listener only. Never sent to the peer.
    pub local_muted: bool,
    /// This listener's playback volume for this peer (100 = unity).
    pub local_volume: i32,
    /// Whether this peer is currently sending video, which drives the voice
    /// rail's streaming indicator.
    pub video_active: bool,
}

impl Default for RoomParticipant {
    fn default() -> Self {
        Self {
            peer_id: String::new(),
            handle: String::new(),
            speaking: false,
            muted: false,
            audio_level: 0.0,
            is_self: false,
            online: true,
            local_muted: false,
            local_volume: 100,
            video_active: false,
        }
    }
}

#[derive(Default)]
pub struct RoomModelRust {
    participants: Vec<RoomParticipant>,
}

impl ffi::RoomModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.participants.len() as i32
    }

    fn handle_for(&self, peer_id: &QString) -> QString {
        let wanted = peer_id.to_string();
        self.participants
            .iter()
            .find(|p| p.peer_id == wanted)
            .map(|p| QString::from(p.handle.as_str()))
            .unwrap_or_default()
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
            r if r == room_roles::ONLINE => QVariant::from(&p.online),
            r if r == room_roles::LOCAL_MUTED => QVariant::from(&p.local_muted),
            r if r == room_roles::LOCAL_VOLUME => QVariant::from(&p.local_volume),
            r if r == room_roles::VIDEO_ACTIVE => QVariant::from(&p.video_active),
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
        h.insert(room_roles::ONLINE, QByteArray::from("online"));
        h.insert(room_roles::LOCAL_MUTED, QByteArray::from("localMuted"));
        h.insert(room_roles::LOCAL_VOLUME, QByteArray::from("localVolume"));
        h.insert(room_roles::VIDEO_ACTIVE, QByteArray::from("videoActive"));
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
            #[serde(default = "default_online")]
            online: bool,
            // These three are listener-local UI state rather than roster
            // facts, so a producer that omits them gets sensible defaults
            // instead of silently muting or hiding someone.
            #[serde(default)]
            local_muted: bool,
            #[serde(default = "default_volume")]
            local_volume: i32,
            #[serde(default)]
            video_active: bool,
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
                    online: r.online,
                    local_muted: r.local_muted,
                    local_volume: r.local_volume,
                    video_active: r.video_active,
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
            let p = &mut self.as_mut().rust_mut().participants[idx];
            let mut roles = vec![room_roles::SPEAKING, room_roles::MUTED];
            p.speaking = speaking;
            p.muted = muted;
            // Derive a synthetic audio level from speaking state when no VAD value
            // is available; real VAD values arrive via setParticipants JSON.
            if speaking && p.audio_level < 0.1 {
                p.audio_level = 0.7;
                roles.push(room_roles::AUDIO_LEVEL);
            } else if !speaking {
                p.audio_level = 0.0;
                roles.push(room_roles::AUDIO_LEVEL);
            }
            emit_row_changed(self.as_mut(), idx as i32, &roles);
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
            let next = level.clamp(0.0, 1.0);
            if (self.rust().participants[idx].audio_level - next).abs() < 0.005 {
                return;
            }
            self.as_mut().rust_mut().participants[idx].audio_level = next;
            emit_row_changed(self.as_mut(), idx as i32, &[room_roles::AUDIO_LEVEL]);
        }
    }

    /// Record this listener's mute/volume preference for one participant.
    ///
    /// Purely a UI mirror: the audio path is driven separately by
    /// `CallCommand::SetPeerMuted` / `SetPeerVolume`, so this only keeps the
    /// checkmark and slider in the context menu consistent.
    fn set_local_audio(mut self: Pin<&mut Self>, peer_id: &QString, muted: bool, volume: i32) {
        let id = peer_id.to_string();
        if let Some(idx) = self
            .rust()
            .participants
            .iter()
            .position(|p| p.peer_id == id)
        {
            let volume = volume.clamp(0, 200);
            let current = &self.rust().participants[idx];
            if current.local_muted == muted && current.local_volume == volume {
                return;
            }
            {
                let p = &mut self.as_mut().rust_mut().participants[idx];
                p.local_muted = muted;
                p.local_volume = volume;
            }
            emit_row_changed(
                self.as_mut(),
                idx as i32,
                &[room_roles::LOCAL_MUTED, room_roles::LOCAL_VOLUME],
            );
        }
    }

    /// Flag whether a participant is sending video, driving the voice-rail
    /// indicator. Deduped because the underlying signal is state, not an event.
    fn set_video_active(mut self: Pin<&mut Self>, peer_id: &QString, active: bool) {
        let id = peer_id.to_string();
        if let Some(idx) = self
            .rust()
            .participants
            .iter()
            .position(|p| p.peer_id == id)
        {
            if self.rust().participants[idx].video_active == active {
                return;
            }
            self.as_mut().rust_mut().participants[idx].video_active = active;
            emit_row_changed(self.as_mut(), idx as i32, &[room_roles::VIDEO_ACTIVE]);
        }
    }
}

fn emit_row_changed(model: Pin<&mut ffi::RoomModel>, row: i32, changed_roles: &[i32]) {
    let parent = QModelIndex::default();
    let tl = model.as_ref().index(row, 0, &parent);
    let br = tl.clone();
    let mut roles = cxx_qt_lib::QList::<i32>::default();
    for role in changed_roles {
        roles.append(*role);
    }
    model.data_changed(&tl, &br, &roles);
}
