//! Central tracing setup with a runtime-reloadable level filter.
//!
//! The verbosity is seeded at startup from the persisted `debug_logging`
//! setting (so verbose logs are captured from the very first line) and can be
//! flipped live from the Settings toggle via [`set_debug_logging`]. An explicit
//! `RUST_LOG` always wins, so power users can still override either way.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, reload, EnvFilter, Registry};

static RELOAD: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();

/// Path of the current-session log file (`~/.conquerd/logs/conquerd-client.log`).
/// The GUI build has no console, so a file is the only way to capture logs.
pub fn log_file_path() -> PathBuf {
    crate::identity::Identity::default_key_dir()
        .join("logs")
        .join("conquerd-client.log")
}

/// A cloneable `MakeWriter` over a shared log file handle.
#[derive(Clone)]
struct FileWriter(Arc<Mutex<std::fs::File>>);

struct FileWriterGuard(Arc<Mutex<std::fs::File>>);

impl Write for FileWriterGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().map_or(Ok(buf.len()), |mut f| f.write(buf))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().map_or(Ok(()), |mut f| f.flush())
    }
}

impl<'a> MakeWriter<'a> for FileWriter {
    type Writer = FileWriterGuard;
    fn make_writer(&'a self) -> Self::Writer {
        FileWriterGuard(self.0.clone())
    }
}

/// Open (truncate) the per-session log file. Returns `None` if it can't be
/// created — logging then falls back to stdout only.
fn open_log_file() -> Option<FileWriter> {
    let path = log_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .ok()?;
    Some(FileWriter(Arc::new(Mutex::new(file))))
}

/// Build the active filter. `RUST_LOG` (if set) takes precedence over the
/// toggle; otherwise `debug` selects between verbose and normal presets.
fn filter_for(debug: bool) -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    if debug {
        // Verbose across the app's own crates; keep third-party at warn.
        EnvFilter::new("conquerd_client=debug,conquerd_opus=debug,warn")
    } else {
        EnvFilter::new("conquerd_client=info,warn")
    }
}

/// Initialise logging exactly once, at process start. `debug` is the persisted
/// `debug_logging` setting.
pub fn init(debug: bool) {
    let (filter, handle) = reload::Layer::new(filter_for(debug));
    // A file layer (so the console-less GUI build can be diagnosed) plus a
    // stdout layer (useful for `--features console` and headless runs). Both
    // are gated by the shared reloadable filter.
    let file_layer = open_log_file().map(|w| fmt::layer().with_ansi(false).with_writer(w));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .with(file_layer)
        .init();
    let _ = RELOAD.set(handle);
}

/// Flip the debug level at runtime (from the Settings toggle). No-op before
/// [`init`] or when `RUST_LOG` pins the filter.
pub fn set_debug_logging(debug: bool) {
    if std::env::var_os("RUST_LOG").is_some() {
        return;
    }
    if let Some(handle) = RELOAD.get() {
        let _ = handle.reload(filter_for(debug));
    }
}

/// Read the persisted `debug_logging` flag directly from `settings.json`
/// (before the Qt UI — and thus `SettingsModel` — has loaded), so the initial
/// filter matches the user's choice. Defaults to `false` on any error.
pub fn load_debug_logging_setting() -> bool {
    let path = crate::identity::Identity::default_key_dir().join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("debug_logging").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}
