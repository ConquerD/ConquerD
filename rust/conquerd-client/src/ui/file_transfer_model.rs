//! FileTransferModel — QAbstractListModel of active file transfers.
//!
//! Tracks inbound offers (waiting for accept/reject) and active sends/receives.
//! Compiled only when the `qt-ui` Cargo feature is enabled.
//!
//! Roles exposed to QML:
//!   • `transferId`  — opaque UUID string
//!   • `peerId`      — remote peer public key (hex)
//!   • `relPath`     — relative file name / path
//!   • `progress`    — 0.0 – 1.0 (f64 cast from QVariant double)
//!   • `state`       — "pending" | "active" | "done" | "failed" | "rejected"
//!   • `isSelf`      — true if this client is the sender
//!   • `purpose`     — e.g. "file" or "image"

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};
use serde::Deserialize;

// Role constants
mod roles {
    pub const TRANSFER_ID: i32 = 256;
    pub const PEER_ID: i32 = 257;
    pub const REL_PATH: i32 = 258;
    pub const PROGRESS: i32 = 259;
    pub const STATE: i32 = 260;
    pub const IS_SELF: i32 = 261;
    pub const PURPOSE: i32 = 262;
}

// ---------------------------------------------------------------------------
// Internal data row
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TransferRow {
    pub transfer_id: String,
    pub peer_id: String,
    pub rel_path: String,
    #[serde(default)]
    pub progress: f64,
    #[serde(default = "default_state")]
    pub state: String,
    #[serde(default)]
    pub is_self: bool,
    #[serde(default = "default_purpose")]
    pub purpose: String,
    #[serde(default)]
    pub reason: String,
}

fn default_state() -> String {
    "pending".to_owned()
}
fn default_purpose() -> String {
    "file".to_owned()
}

// ---------------------------------------------------------------------------
// Rust state
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct FileTransferModelRust {
    rows: Vec<TransferRow>,
}

// ---------------------------------------------------------------------------
// cxx-qt bridge
// ---------------------------------------------------------------------------

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
        type FileTransferModel = super::FileTransferModelRust;

        // ── QAbstractListModel overrides ─────────────────────────────────
        #[cxx_override]
        #[rust_name = "row_count"]
        fn rowCount(&self, parent: &QModelIndex) -> i32;

        #[cxx_override]
        fn data(&self, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[rust_name = "role_names"]
        fn roleNames(&self) -> QHash_i32_QByteArray;

        // ── Invokables called from bridge.rs via bridge signals ───────────

        /// Upsert a transfer entry from a JSON object (fired on fileOffered).
        #[qinvokable]
        #[rust_name = "upsert_transfer"]
        fn upsertTransfer(self: Pin<&mut Self>, json: &QString);

        /// Update the progress of an existing transfer (fired on fileProgress).
        #[qinvokable]
        #[rust_name = "set_progress"]
        fn setProgress(self: Pin<&mut Self>, transfer_id: &QString, progress: f64);

        /// Mark a transfer as complete (fired on fileComplete).
        #[qinvokable]
        #[rust_name = "mark_complete"]
        fn markComplete(self: Pin<&mut Self>, transfer_id: &QString);

        /// Mark a transfer as failed/rejected (fired on fileFailed).
        #[qinvokable]
        #[rust_name = "mark_failed"]
        fn markFailed(self: Pin<&mut Self>, transfer_id: &QString, reason: &QString);

        /// Remove a transfer entry (e.g. after it has been dismissed by the user).
        #[qinvokable]
        #[rust_name = "remove_transfer"]
        fn removeTransfer(self: Pin<&mut Self>, transfer_id: &QString);

        /// Live state for a transfer shown inside a chat bubble, or empty.
        #[qinvokable]
        #[rust_name = "state_for"]
        fn stateFor(&self, transfer_id: &QString) -> QString;

        /// Progress 0–1 for a transfer shown inside a chat bubble.
        #[qinvokable]
        #[rust_name = "progress_for"]
        fn progressFor(&self, transfer_id: &QString) -> f64;

        /// Fail reason for a transfer shown inside a chat bubble, or empty.
        #[qinvokable]
        #[rust_name = "reason_for"]
        fn reasonFor(&self, transfer_id: &QString) -> QString;

        // ── Qt model lifecycle ────────────────────────────────────────────
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

use ffi::FileTransferModel;

// ---------------------------------------------------------------------------
// QAbstractListModel implementation
// ---------------------------------------------------------------------------

impl ffi::FileTransferModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.rust().rows.len() as i32
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = index.row() as usize;
        let rows = &self.rust().rows;
        if row >= rows.len() {
            return QVariant::default();
        }
        let r = &rows[row];
        match role {
            roles::TRANSFER_ID => QVariant::from(&QString::from(&*r.transfer_id)),
            roles::PEER_ID => QVariant::from(&QString::from(&*r.peer_id)),
            roles::REL_PATH => QVariant::from(&QString::from(&*r.rel_path)),
            roles::PROGRESS => QVariant::from(&r.progress),
            roles::STATE => QVariant::from(&QString::from(&*r.state)),
            roles::IS_SELF => QVariant::from(&r.is_self),
            roles::PURPOSE => QVariant::from(&QString::from(&*r.purpose)),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut map = QHash::<QHashPair_i32_QByteArray>::default();
        map.insert(roles::TRANSFER_ID, QByteArray::from("transferId"));
        map.insert(roles::PEER_ID, QByteArray::from("peerId"));
        map.insert(roles::REL_PATH, QByteArray::from("relPath"));
        map.insert(roles::PROGRESS, QByteArray::from("progress"));
        map.insert(roles::STATE, QByteArray::from("state"));
        map.insert(roles::IS_SELF, QByteArray::from("isSelf"));
        map.insert(roles::PURPOSE, QByteArray::from("purpose"));
        map
    }

    fn upsert_transfer(mut self: Pin<&mut Self>, json: &QString) {
        let s = json.to_string();
        let Ok(row) = serde_json::from_str::<TransferRow>(&s) else {
            return;
        };
        let tid = row.transfer_id.clone();
        // Check if it already exists
        let exists = self.rust().rows.iter().any(|r| r.transfer_id == tid);
        if exists {
            // Update via full reset (correct for small lists)
            self.as_mut().begin_reset_model();
            if let Some(r) = self
                .as_mut()
                .rust_mut()
                .rows
                .iter_mut()
                .find(|r| r.transfer_id == tid)
            {
                *r = row;
            }
            self.as_mut().end_reset_model();
        } else {
            let new_pos = self.rust().rows.len() as i32;
            self.as_mut()
                .begin_insert_rows(&QModelIndex::default(), new_pos, new_pos);
            self.as_mut().rust_mut().rows.push(row);
            self.as_mut().end_insert_rows();
        }
    }

    fn set_progress(mut self: Pin<&mut Self>, transfer_id: &QString, progress: f64) {
        let tid = transfer_id.to_string();
        let changed = {
            let rows = &mut self.as_mut().rust_mut().rows;
            if let Some(r) = rows.iter_mut().find(|r| r.transfer_id == tid) {
                r.progress = progress;
                if r.state == "pending" {
                    r.state = "active".to_owned();
                }
                true
            } else {
                false
            }
        };
        if changed {
            // Emit a lightweight reset so the view refreshes
            self.as_mut().begin_reset_model();
            self.as_mut().end_reset_model();
        }
    }

    fn mark_complete(mut self: Pin<&mut Self>, transfer_id: &QString) {
        let tid = transfer_id.to_string();
        let changed = {
            let rows = &mut self.as_mut().rust_mut().rows;
            if let Some(r) = rows.iter_mut().find(|r| r.transfer_id == tid) {
                r.progress = 1.0;
                r.state = "done".to_owned();
                true
            } else {
                false
            }
        };
        if changed {
            self.as_mut().begin_reset_model();
            self.as_mut().end_reset_model();
        }
    }

    fn mark_failed(mut self: Pin<&mut Self>, transfer_id: &QString, reason: &QString) {
        let tid = transfer_id.to_string();
        let why = reason.to_string();
        let changed = {
            let rows = &mut self.as_mut().rust_mut().rows;
            if let Some(r) = rows.iter_mut().find(|r| r.transfer_id == tid) {
                r.state = "failed".to_owned();
                r.reason = why;
                true
            } else {
                false
            }
        };
        if changed {
            self.as_mut().begin_reset_model();
            self.as_mut().end_reset_model();
        }
    }

    fn remove_transfer(mut self: Pin<&mut Self>, transfer_id: &QString) {
        let tid = transfer_id.to_string();
        let idx = self.rust().rows.iter().position(|r| r.transfer_id == tid);
        if let Some(i) = idx {
            self.as_mut().begin_reset_model();
            self.as_mut().rust_mut().rows.remove(i);
            self.as_mut().end_reset_model();
        }
    }

    fn state_for(&self, transfer_id: &QString) -> QString {
        let tid = transfer_id.to_string();
        self.rust()
            .rows
            .iter()
            .find(|r| r.transfer_id == tid)
            .map(|r| QString::from(r.state.as_str()))
            .unwrap_or_default()
    }

    fn progress_for(&self, transfer_id: &QString) -> f64 {
        let tid = transfer_id.to_string();
        self.rust()
            .rows
            .iter()
            .find(|r| r.transfer_id == tid)
            .map(|r| r.progress)
            .unwrap_or(0.0)
    }

    fn reason_for(&self, transfer_id: &QString) -> QString {
        let tid = transfer_id.to_string();
        self.rust()
            .rows
            .iter()
            .find(|r| r.transfer_id == tid)
            .map(|r| QString::from(r.reason.as_str()))
            .unwrap_or_default()
    }
}
