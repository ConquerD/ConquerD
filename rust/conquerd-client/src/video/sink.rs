//! Rust side of the QVideoSink registry.
//!
//! Wraps the C++ shim in `ui/video_sink_bridge.cpp`. Frames are pushed from the
//! decode thread; the shim builds the `QVideoFrame` there and hops it to the
//! GUI thread, dropping frames when the GUI thread falls behind.
//!
//! Compiled only when Qt Multimedia was found at build time — see the
//! `qt_multimedia` cfg set by `build.rs`. Without it the whole module is
//! replaced by no-ops so the rest of the video pipeline still builds and runs,
//! it simply has nowhere to draw.

use super::frame::RawFrame;

#[cfg(all(feature = "qt-ui", qt_multimedia))]
mod imp {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int};

    extern "C" {
        fn conquerd_register_video_singleton();
        fn conquerd_video_push_i420(
            peer_id: *const c_char,
            width: c_int,
            height: c_int,
            y: *const u8,
            u: *const u8,
            v: *const u8,
        );
        fn conquerd_video_clear(peer_id: *const c_char);
        fn conquerd_video_clear_all();
        fn conquerd_video_has_sink(peer_id: *const c_char) -> bool;
    }

    pub fn has_sink(peer_id: &str) -> bool {
        let Ok(id) = CString::new(peer_id) else {
            return false;
        };
        // SAFETY: read-only query against GUI-thread-owned state; the shim
        // treats an unknown id as "no sink".
        unsafe { conquerd_video_has_sink(id.as_ptr()) }
    }

    pub fn register_singleton() {
        // SAFETY: registers a process-wide singleton with the QML engine;
        // must be called before `engine.load()`.
        unsafe { conquerd_register_video_singleton() }
    }

    pub fn push_frame(peer_id: &str, frame: &super::RawFrame) {
        if !frame.is_consistent() {
            return;
        }
        let Ok(id) = CString::new(peer_id) else {
            return; // interior NUL — not a valid peer id
        };
        // SAFETY: plane lengths were validated by `is_consistent`, so the
        // shim's row-wise copy stays in bounds. The pointers are only read
        // during the call; the shim copies before returning.
        unsafe {
            conquerd_video_push_i420(
                id.as_ptr(),
                frame.width as c_int,
                frame.height as c_int,
                frame.y.as_ptr(),
                frame.u.as_ptr(),
                frame.v.as_ptr(),
            )
        }
    }

    pub fn clear_peer(peer_id: &str) {
        let Ok(id) = CString::new(peer_id) else {
            return;
        };
        // SAFETY: the shim marshals to the GUI thread before touching state.
        unsafe { conquerd_video_clear(id.as_ptr()) }
    }

    pub fn clear_all() {
        // SAFETY: the shim hops to the GUI thread before touching state.
        unsafe { conquerd_video_clear_all() }
    }
}

#[cfg(not(all(feature = "qt-ui", qt_multimedia)))]
mod imp {
    pub fn register_singleton() {}
    pub fn push_frame(_peer_id: &str, _frame: &super::RawFrame) {}
    pub fn clear_peer(_peer_id: &str) {}
    pub fn clear_all() {}
    /// Without a render surface nothing is ever watching, so the decode thread
    /// can skip the work entirely.
    pub fn has_sink(_peer_id: &str) -> bool {
        false
    }
}

/// Register the `VideoRegistry` QML singleton. Call before loading QML.
pub fn register_singleton() {
    imp::register_singleton();
}

/// Push a decoded frame to every sink bound to `peer_id`.
///
/// Cheap when nobody is watching: the shim's in-flight guard sheds the frame
/// before doing any allocation or copying.
pub fn push_frame(peer_id: &str, frame: &RawFrame) {
    imp::push_frame(peer_id, frame);
}

/// Blank every sink for a peer without unregistering them.
///
/// Used when the peer turns their camera off or leaves — the last frame must
/// not stick, but a later re-on should not require the QML tile to re-bind.
pub fn clear_peer(peer_id: &str) {
    imp::clear_peer(peer_id);
}

/// Blank every registered sink. Used when the local voice/video session ends.
pub fn clear_all() {
    imp::clear_all();
}

/// Whether anything is currently displaying this peer.
///
/// The decode thread checks this first: decoding for a peer whose tile is
/// closed is pure waste, and in a large room most peers are not on screen.
pub fn has_sink(peer_id: &str) -> bool {
    imp::has_sink(peer_id)
}

/// Whether this build can render video at all.
///
/// False when Qt Multimedia was absent at build time; the UI uses this to
/// explain the absence rather than showing a permanently black rectangle.
pub const fn rendering_available() -> bool {
    cfg!(all(feature = "qt-ui", qt_multimedia))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_rejects_inconsistent_frames_without_crashing() {
        // Guards the FFI precondition: the shim copies plane-by-plane using
        // the declared dimensions, so a frame whose planes do not match would
        // read out of bounds. `push_frame` must refuse it here.
        let mut bad = RawFrame::black(64, 48);
        bad.u.truncate(3);
        assert!(!bad.is_consistent());
        push_frame("peer", &bad); // must not panic or reach the shim
    }

    #[test]
    fn push_rejects_peer_ids_with_interior_nul() {
        let frame = RawFrame::black(16, 16);
        push_frame("bad\0id", &frame); // CString::new fails; must be a no-op
    }

    #[test]
    fn no_op_build_still_accepts_calls() {
        // Whether or not Multimedia is present, these must be safe to call so
        // callers never need to branch on availability.
        let frame = RawFrame::black(16, 16);
        push_frame("peer", &frame);
        clear_peer("peer");
        clear_all();
        let _ = rendering_available();
    }
}
