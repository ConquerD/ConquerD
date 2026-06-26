//! Pure-Rust acoustic echo canceller (experimental; gated by the `aec` feature).
//!
//! A normalized least-mean-squares (NLMS) adaptive FIR filter models the echo
//! path from the speaker (far-end / playback) to the microphone, then subtracts
//! the estimated echo from the captured near-end signal. A Geigel-style
//! double-talk detector freezes adaptation while the near-end user is also
//! talking, so the filter doesn't diverge.
//!
//! This is a lightweight, **dependency-free** canceller operating on 48 kHz
//! mono frames. It cancels linear echo whose bulk speaker→mic delay falls
//! within the filter's tap span ([`DEFAULT_TAPS`] ≈ 43 ms at 48 kHz). Quality
//! is modest next to a full WebRTC APM and real deployments need device-buffer
//! delay to land inside the tap span — it is a foundation, not a turnkey
//! solution, and ships **off by default**.
//!
//! Hot-path note: the per-sample estimate and weight update each iterate the
//! reference window as two contiguous runs (no per-tap modulo), so cost is
//! `O(taps)` per sample with cache-friendly access.

/// Default adaptive-filter length in taps (~43 ms at 48 kHz).
pub const DEFAULT_TAPS: usize = 2048;

/// NLMS step size (0 < mu < 2 for stability). 0.5 is a conservative balance of
/// convergence speed and steady-state error.
const MU: f32 = 0.5;
/// NLMS regularisation, avoids divide-by-zero on silent reference.
const EPS: f32 = 1e-6;
/// Geigel double-talk threshold: near-end is declared present when the mic
/// magnitude exceeds this multiple of the recent far-end envelope. Above ~1.0
/// so attenuated echo (always below the far-end peak) does not self-trigger.
const GEIGEL_C: f32 = 1.1;
/// Hold adaptation frozen for this many samples after a double-talk trigger
/// (~50 ms at 48 kHz) so brief gaps don't immediately re-enable adaptation.
const DT_HANGOVER: u32 = 2400;
/// Don't adapt when the far-end is essentially silent (nothing to cancel).
const REF_FLOOR: f32 = 1e-4;
/// Per-sample decay of the far-end envelope peak tracker.
const ENV_DECAY: f32 = 0.9995;

/// A mono NLMS acoustic echo canceller.
pub struct EchoCanceller {
    taps: usize,
    /// Adaptive FIR weights, index 0 = newest reference sample.
    weights: Vec<f32>,
    /// Circular reference history; `wp` holds the newest sample.
    history: Vec<f32>,
    wp: usize,
    /// Decaying peak tracker of the far-end magnitude (double-talk reference).
    ref_env: f32,
    /// Remaining samples to keep adaptation frozen after a double-talk trigger.
    dt_hold: u32,
}

impl EchoCanceller {
    /// Create a canceller with `taps` filter length (clamped to ≥ 1).
    pub fn new(taps: usize) -> Self {
        let taps = taps.max(1);
        Self {
            taps,
            weights: vec![0.0; taps],
            history: vec![0.0; taps],
            wp: 0,
            ref_env: 0.0,
            dt_hold: 0,
        }
    }

    /// Process one 48 kHz mono frame in place: `mic` is the captured near-end
    /// signal (echo + voice), `far` is the matching far-end (played) reference.
    /// On return `mic` holds the echo-cancelled near-end signal. `far` may be
    /// shorter than `mic` (missing samples are treated as silence).
    pub fn process_frame(&mut self, mic: &mut [i16], far: &[f32]) {
        let taps = self.taps;
        for (n, m) in mic.iter_mut().enumerate() {
            let x = far.get(n).copied().unwrap_or(0.0);
            // Write the newest reference sample.
            self.history[self.wp] = x;
            let wp = self.wp;

            // FIR estimate of the echo, plus reference power for NLMS norm.
            // Two contiguous runs walk the window newest→oldest with no modulo.
            let mut y = 0.0f32;
            let mut norm = 0.0f32;
            let mut wi = 0usize;
            for k in (0..=wp).rev() {
                let xi = self.history[k];
                y += self.weights[wi] * xi;
                norm += xi * xi;
                wi += 1;
            }
            for k in (wp + 1..taps).rev() {
                let xi = self.history[k];
                y += self.weights[wi] * xi;
                norm += xi * xi;
                wi += 1;
            }

            let d = *m as f32 / 32768.0;
            let e = d - y; // echo-cancelled near-end

            // Update the far-end envelope, then run the double-talk detector.
            self.ref_env = self.ref_env.max(x.abs());
            if d.abs() > GEIGEL_C * self.ref_env {
                self.dt_hold = DT_HANGOVER;
            }

            // Adapt only with a live far-end and no double-talk.
            if self.dt_hold == 0 && self.ref_env > REF_FLOOR {
                let g = MU * e / (norm + EPS);
                let mut wi = 0usize;
                for k in (0..=wp).rev() {
                    self.weights[wi] += g * self.history[k];
                    wi += 1;
                }
                for k in (wp + 1..taps).rev() {
                    self.weights[wi] += g * self.history[k];
                    wi += 1;
                }
            }
            if self.dt_hold > 0 {
                self.dt_hold -= 1;
            }
            self.ref_env *= ENV_DECAY;

            *m = (e.clamp(-1.0, 1.0) * 32767.0) as i16;
            self.wp = (self.wp + 1) % taps;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic LCG white-ish noise in [-1, 1] for reproducible tests.
    fn noise(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }

    #[test]
    fn nlms_converges_and_cancels_synthetic_echo() {
        // Echo path: two delayed taps of the far-end signal, attenuated.
        // mic[n] = 0.6*far[n-10] + 0.3*far[n-25]  (no near-end talk).
        let taps = 256;
        let mut aec = EchoCanceller::new(taps);
        let mut seed = 0x1234_5678u32;

        let frame = 480usize; // 10 ms at 48 kHz
        let mut far_hist = vec![0.0f32; 64];
        let mut energy_in = 0.0f64;
        let mut energy_out = 0.0f64;
        let total_frames = 80; // ~0.8 s — well past convergence for 256 taps
        let measure_from = 60; // measure ERLE only after convergence

        for f in 0..total_frames {
            let mut far = vec![0.0f32; frame];
            let mut mic = vec![0i16; frame];
            for i in 0..frame {
                let x = noise(&mut seed);
                far[i] = x;
                // shift far history and compute echo from delayed taps
                far_hist.rotate_right(1);
                far_hist[0] = x;
                let echo = 0.6 * far_hist[10] + 0.3 * far_hist[25];
                mic[i] = (echo.clamp(-1.0, 1.0) * 32767.0) as i16;
            }
            let echo_copy = mic.clone();
            aec.process_frame(&mut mic, &far);
            if f >= measure_from {
                for i in 0..frame {
                    let d = echo_copy[i] as f64 / 32768.0;
                    let e = mic[i] as f64 / 32768.0;
                    energy_in += d * d;
                    energy_out += e * e;
                }
            }
        }

        // Echo Return Loss Enhancement: how much the canceller reduced echo.
        let erle_db = 10.0 * (energy_in / energy_out.max(1e-12)).log10();
        assert!(
            erle_db > 12.0,
            "AEC should cancel ≳12 dB of echo after convergence, got {erle_db:.1} dB"
        );
    }

    #[test]
    fn silent_far_end_passes_near_end_through() {
        // With no far-end reference, the canceller must not distort or adapt:
        // near-end audio passes through essentially unchanged.
        let mut aec = EchoCanceller::new(256);
        let mut seed = 99u32;
        let far = vec![0.0f32; 480];
        let mut mic = vec![0i16; 480];
        for s in mic.iter_mut() {
            *s = (noise(&mut seed) * 10000.0) as i16;
        }
        let before = mic.clone();
        aec.process_frame(&mut mic, &far);
        // No reference power → estimate is 0 → output ≈ input (within rounding).
        for (a, b) in before.iter().zip(mic.iter()) {
            assert!((*a - *b).abs() <= 1, "near-end altered with silent far-end");
        }
    }

    #[test]
    fn double_talk_freezes_adaptation() {
        // Loud near-end speech over quiet far-end must trigger the double-talk
        // hold so the filter doesn't adapt toward the near-end signal.
        let mut aec = EchoCanceller::new(64);
        let far = vec![0.05f32; 480]; // quiet far-end
        let mut mic = vec![16000i16; 480]; // loud near-end
        aec.process_frame(&mut mic, &far);
        assert!(aec.dt_hold > 0, "double-talk detector should have engaged");
        // Weights stay at zero because adaptation was frozen throughout.
        assert!(
            aec.weights.iter().all(|&w| w.abs() < 1e-6),
            "weights must not adapt during double-talk"
        );
    }
}
