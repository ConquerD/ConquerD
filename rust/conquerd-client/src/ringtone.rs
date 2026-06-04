//! Ringtone — plays a simple sine-wave ring for incoming calls.
//!
//! Generates an in-memory WAV (440 Hz + 480 Hz, 2-tone ring pattern) and
//! hands it to the OS audio player on a background thread so it never blocks
//! the Qt main thread.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// WAV generation helpers
// ---------------------------------------------------------------------------

const SAMPLE_RATE: u32 = 16_000;
const AMPLITUDE: f32 = 0.4;

fn generate_tone(freq: f32, duration_ms: u32) -> Vec<i16> {
    let n_samples = (SAMPLE_RATE * duration_ms / 1000) as usize;
    (0..n_samples)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            let v = AMPLITUDE * (2.0 * std::f32::consts::PI * freq * t).sin();
            (v * 32_767.0) as i16
        })
        .collect()
}

fn generate_silence(duration_ms: u32) -> Vec<i16> {
    vec![0i16; (SAMPLE_RATE * duration_ms / 1000) as usize]
}

/// Build a single ring-cycle WAV (440 Hz, 480 Hz, with silence).
fn build_ring_wav() -> Vec<u8> {
    let mut pcm: Vec<i16> = Vec::new();
    pcm.extend(generate_tone(440.0, 400));
    pcm.extend(generate_silence(100));
    pcm.extend(generate_tone(480.0, 400));
    pcm.extend(generate_silence(1500));

    let data_len = (pcm.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
                                                 // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in &pcm {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

// ---------------------------------------------------------------------------
// Ringtone controller
// ---------------------------------------------------------------------------

/// Controls a looping ring for incoming calls.
///
/// `start()` spawns a background thread that plays the ring repeatedly until
/// `stop()` is called.
pub struct Ringtone {
    playing: Arc<AtomicBool>,
}

impl Ringtone {
    pub fn new() -> Self {
        Self {
            playing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start the ring (no-op if already playing).
    pub fn start(&self) {
        if self.playing.swap(true, Ordering::SeqCst) {
            return; // already ringing
        }
        let flag = Arc::clone(&self.playing);
        let wav = build_ring_wav();
        std::thread::spawn(move || {
            play_loop(wav, flag);
        });
        debug!("[ringtone] started");
    }

    /// Stop the ring.
    pub fn stop(&self) {
        self.playing.store(false, Ordering::SeqCst);
        debug!("[ringtone] stopped");
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
}

impl Default for Ringtone {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Platform playback
// ---------------------------------------------------------------------------

fn play_loop(wav: Vec<u8>, flag: Arc<AtomicBool>) {
    while flag.load(Ordering::Relaxed) {
        if !play_wav_once(&wav) {
            break; // player not available; stop silently
        }
    }
    flag.store(false, Ordering::Relaxed);
}

fn play_wav_once(wav: &[u8]) -> bool {
    #[cfg(target_os = "windows")]
    {
        play_wav_windows(wav)
    }
    #[cfg(target_os = "linux")]
    {
        play_via_cmd("aplay", wav)
    }
    #[cfg(target_os = "macos")]
    {
        play_via_cmd("afplay", wav)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = wav;
        warn!("[ringtone] unsupported platform — audio disabled");
        false
    }
}

#[cfg(target_os = "windows")]
fn play_wav_windows(wav: &[u8]) -> bool {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    // Write to a temp file so winsound / PlaySound can read it.
    let tmp = std::env::temp_dir().join("conquerd_ring.wav");
    if std::fs::write(&tmp, wav).is_err() {
        warn!("[ringtone] failed to write temp WAV");
        return false;
    }

    // Use PowerShell Media.SoundPlayer as the most reliable Win32 audio path
    // that is guaranteed present on any Win10+ installation.
    let path = tmp.to_string_lossy().into_owned();
    match std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "$p = New-Object Media.SoundPlayer '{}'; $p.PlaySync()",
                path.replace('\'', "''")
            ),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
    {
        Ok(_) => true,
        Err(e) => {
            warn!("[ringtone] PowerShell SoundPlayer failed: {e}");
            false
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn play_via_cmd(cmd: &str, wav: &[u8]) -> bool {
    use std::io::Write;
    let tmp = std::env::temp_dir().join("conquerd_ring.wav");
    if std::fs::write(&tmp, wav).is_err() {
        return false;
    }
    std::process::Command::new(cmd)
        .arg(tmp.to_str().unwrap_or(""))
        .status()
        .is_ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_starts_with_riff() {
        let wav = build_ring_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
    }

    #[test]
    fn wav_data_length_correct() {
        let wav = build_ring_wav();
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(wav.len() as u32, 44 + data_len);
    }

    #[test]
    fn ringtone_start_stop() {
        let rt = Ringtone::new();
        // Stop without starting — should not panic
        rt.stop();
        // is_playing after stop
        assert!(!rt.is_playing());
    }
}
