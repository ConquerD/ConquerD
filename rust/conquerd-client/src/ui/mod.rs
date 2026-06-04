//! Qt/QML UI layer — compiled only when the `qt-ui` feature is enabled.
//!
//! Contains:
//! - `bridge.rs`           — cxx-qt AppBridge QObject (singleton the QML layer binds to)
//! - `peer_list_model.rs`  — PeerListModel (QAbstractListModel of trusted peers)
//! - `chat_model.rs`       — ChatModel (QAbstractListModel of messages per peer)
//! - `call_model.rs`       — CallModel (live call state QObject singleton)
//! - `room_model.rs`       — RoomModel (QAbstractListModel of voice-room participants)
//! - `settings_model.rs`   — SettingsModel (writable settings QObject singleton)
//!
//! # Activating the Qt UI
//!
//! 1. Install Qt6 (e.g. via the Qt installer or `winget install Qt.Qt.6.7.3.MSVC2022`).
//! 2. Set `QTDIR` or `CMAKE_PREFIX_PATH` to the Qt6 installation root.
//! 3. Ensure `cmake` is on `PATH`.
//! 4. Build with:
//!    ```sh
//!    cargo build -p conquerd-client --features qt-ui
//!    ```

#[cfg(feature = "qt-ui")]
pub mod bridge;

#[cfg(feature = "qt-ui")]
pub mod peer_list_model;

#[cfg(feature = "qt-ui")]
pub mod chat_model;

#[cfg(feature = "qt-ui")]
pub mod call_model;

#[cfg(feature = "qt-ui")]
pub mod room_model;

#[cfg(feature = "qt-ui")]
pub mod settings_model;

#[cfg(feature = "qt-ui")]
pub mod file_transfer_model;

#[cfg(feature = "qt-ui")]
pub mod avatar;

#[cfg(feature = "webengine")]
pub mod scheme;
