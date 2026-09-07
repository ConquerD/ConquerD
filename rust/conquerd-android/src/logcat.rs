//! Route `tracing` output to logcat.
//!
//! Android gives a process no stdout or stderr that anyone can read, so the
//! default `tracing_subscriber` writer sends every log line into the void.
//! This is a `MakeWriter` over liblog's `__android_log_write`, which is where
//! `adb logcat` reads from.

use std::ffi::CString;
use std::io;
use std::os::raw::{c_char, c_int};

/// liblog priority for `ANDROID_LOG_INFO`. Every line goes out at INFO and
/// carries its real level in the formatted text, because the level a
/// `tracing` event was recorded at is not visible to a `MakeWriter`.
const ANDROID_LOG_INFO: c_int = 4;

/// The tag `adb logcat -s` filters on.
const TAG: &str = "ConquerD";

#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// A writer that emits one logcat entry per `write` call.
///
/// `tracing_subscriber`'s formatter writes a whole event before flushing, so
/// entries line up with events rather than being split mid-message.
pub struct LogcatWriter;

impl io::Write for LogcatWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // liblog takes a NUL-terminated C string, so interior NULs — which a
        // formatted log line should never contain — mean we drop the write
        // rather than truncate it silently at the first NUL.
        let text = String::from_utf8_lossy(buf);
        let Ok(c_text) = CString::new(text.trim_end().as_bytes()) else {
            return Ok(buf.len());
        };
        let Ok(c_tag) = CString::new(TAG) else {
            return Ok(buf.len());
        };

        // SAFETY: both pointers come from `CString`s that outlive the call,
        // and liblog only reads through them.
        unsafe {
            __android_log_write(ANDROID_LOG_INFO, c_tag.as_ptr(), c_text.as_ptr());
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogcatWriter {
    type Writer = LogcatWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogcatWriter
    }
}
