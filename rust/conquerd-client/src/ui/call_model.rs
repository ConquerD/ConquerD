//! CallModel — singleton QObject tracking the live call state.
//!
//! Compiled only when the `qt-ui` Cargo feature is enabled.

use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, state)]
        #[qproperty(QString, peer_id)]
        #[qproperty(bool, muted)]
        #[qproperty(i32, duration_secs)]
        type CallModel = super::CallModelRust;
    }
}

pub use ffi::CallModel;

pub struct CallModelRust {
    /// "idle" | "connecting" | "in_call" | "ending"
    state: QString,
    peer_id: QString,
    muted: bool,
    duration_secs: i32,
}

impl Default for CallModelRust {
    fn default() -> Self {
        Self {
            state: QString::from("idle"),
            peer_id: QString::default(),
            muted: false,
            duration_secs: 0,
        }
    }
}
