//! Identity-derived avatar generator.
//!
//! Produces a deterministic, horizontally-symmetric identicon from a peer's
//! identity string. Grid size, colour, shading, dual-hue, and island-coloring
//! are all driven by [`AvatarConfig`] so the same wire config produces the
//! identical SVG on every client.
//!
//! This module is compiled unconditionally (no `qt-ui` gate) because
//! `AvatarConfig` is stored in `PeerRecord` and serialized to disk.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

// ---------------------------------------------------------------------------
// AvatarConfig serde default helpers (must match AvatarConfig::default())
// ---------------------------------------------------------------------------

fn def_grid() -> u8 {
    16
}
fn def_sat() -> f32 {
    0.55
}
fn def_lig() -> f32 {
    0.55
}
fn def_spread() -> f32 {
    0.15
}
fn def_true() -> bool {
    true
}
fn def_bg_lig() -> f32 {
    0.12
}
fn def_shade_mode() -> u8 {
    1
}
fn def_dual_hue_mode() -> String {
    "topbot".to_owned()
}
fn def_island_conn() -> u8 {
    8
}
fn def_island_step() -> f32 {
    0.62
}

// ---------------------------------------------------------------------------
// AvatarConfig
// ---------------------------------------------------------------------------

/// All user-configurable visual parameters for an avatar.
///
/// Serialised as JSON and transmitted to trusted peers after the Ed25519
/// handshake via `AVATAR_CONFIG` signaling messages so every client renders
/// an identical SVG.
///
/// Every field carries `#[serde(default = "...")]` so partial JSON (e.g. a
/// config that only contains a single changed key) deserialises successfully
/// instead of falling back to the whole-struct default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AvatarConfig {
    /// Grid side length in cells.  UI offers 8–32 in steps of 2.
    #[serde(default = "def_grid")]
    pub grid: u8,
    /// Foreground saturation 0–1.
    #[serde(default = "def_sat")]
    pub sat: f32,
    /// Base foreground lightness 0–1.
    #[serde(default = "def_lig")]
    pub lig: f32,
    /// Per-shade lightness step (spread ±).
    #[serde(default = "def_spread")]
    pub spread: f32,
    /// Fill background with an identity-derived tint.
    #[serde(default = "def_true")]
    pub bg_tint: bool,
    /// Background lightness when `bg_tint` is true.
    #[serde(default = "def_bg_lig")]
    pub bg_lig: f32,
    /// Number of lightness levels: 1 = flat, 2 = two levels, 3 = three.
    #[serde(default = "def_shade_mode")]
    pub shade_mode: u8,
    /// Enable dual-hue rendering.
    #[serde(default)]
    pub dual_hue: bool,
    /// Dual-hue split geometry: `"topbot"`, `"checker"`, or `"quad"`.
    #[serde(default = "def_dual_hue_mode")]
    pub dual_hue_mode: String,
    /// Enable island flood-fill coloring.
    #[serde(default = "def_true")]
    pub islands: bool,
    /// Island connectivity: 4 (cardinal) or 8 (diagonal).
    #[serde(default = "def_island_conn")]
    pub island_conn: u8,
    /// Hue step per island (golden ratio ≈ 0.618 recommended).
    #[serde(default = "def_island_step")]
    pub island_step: f32,
    /// Vary saturation slightly per island.
    #[serde(default = "def_true")]
    pub island_varsat: bool,
    /// Apply `shape-rendering="crispEdges"` to the SVG root element.
    #[serde(default = "def_true")]
    pub svg_crisp: bool,
    /// Round cell corners (rx = 0.25 cell units).
    #[serde(default)]
    pub svg_round_cells: bool,
}

impl Default for AvatarConfig {
    /// Full-featured default shown for trusted peers whose config has not yet
    /// been received.  All fields are user-configurable via the Avatar tab.
    fn default() -> Self {
        Self {
            grid: 16,
            sat: 0.55,
            lig: 0.55,
            spread: 0.15,
            bg_tint: true,
            bg_lig: 0.12,
            shade_mode: 1,
            dual_hue: false,
            dual_hue_mode: "topbot".to_owned(),
            islands: true,
            island_conn: 8,
            island_step: 0.62,
            island_varsat: true,
            svg_crisp: true,
            svg_round_cells: false,
        }
    }
}

impl AvatarConfig {
    /// Minimal config used for peers who have not yet completed an Ed25519
    /// handshake (e.g. strangers seen only in a supernode room).
    ///
    /// Intentionally simple (8×8, flat hue) so visual complexity signals
    /// trust level to the local user.  Not user-configurable.
    pub fn untrusted() -> Self {
        Self {
            grid: 8,
            sat: 0.55,
            lig: 0.55,
            spread: 0.15,
            bg_tint: false,
            bg_lig: 0.12,
            shade_mode: 1,
            dual_hue: false,
            dual_hue_mode: "topbot".to_owned(),
            islands: false,
            island_conn: 4,
            island_step: 0.62,
            island_varsat: false,
            svg_crisp: true,
            svg_round_cells: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Pattern computation
// ---------------------------------------------------------------------------

/// Computed pattern data for one peer + config combination.
#[derive(Debug, Clone)]
pub struct AvatarPattern {
    pub grid: usize,
    pub hue: f32,
    pub hue2: f32,
    /// Row-major `grid × grid` foreground cells.
    pub cells: Vec<bool>,
    /// Per-cell shade level (0-indexed, max = `config.shade_mode - 1`).
    pub shades: Vec<u8>,
}

/// Normalize any peer identifier to the base64url `identity_pub` avatar seed.
///
/// Settings, SFU rooms, and peer avatars all hash `identity_pub`. The Peers
/// rail passes hex `peer_id`; resolve it via `resolve_identity_pub` when known.
pub fn avatar_seed_id(
    raw_id: &str,
    my_public_id: &str,
    my_peer_id: &str,
    resolve_identity_pub: impl Fn(&str) -> Option<String>,
) -> String {
    if raw_id == my_public_id || raw_id == my_peer_id {
        return my_public_id.to_owned();
    }
    if let Some(identity_pub) = resolve_identity_pub(raw_id) {
        if !identity_pub.is_empty() {
            return identity_pub;
        }
    }
    raw_id.to_owned()
}

/// Derive a deterministic pattern from `peer_id` and `config`.
///
/// `peer_id` is the avatar seed (base64url `identity_pub` for known peers).
///
/// - Cells: SHA-512(peer_id) → `ceil(grid/2) × grid` left half, mirrored.
///   Center column is not double-mirrored for odd `grid` sizes.
/// - Hue:  SHA-256("hue:"  ‖ peer_id) → bytes[0..1] as u16 % 360 / 360.
/// - Hue2: SHA-256("hue2:" ‖ peer_id) → same formula.
/// - Shades: SHA-512("shade:" ‖ peer_id) → same bit-extraction, mod shade_mode.
pub fn pattern_for_peer(peer_id: &str, config: &AvatarConfig) -> AvatarPattern {
    let pb = peer_id.as_bytes();
    let grid = config.grid as usize;
    let half_cols = grid.div_ceil(2); // ceil(grid/2)
    let total = grid * grid;

    // Pattern bits from SHA-512(peer_id).
    let pat_dig = Sha512::digest(pb);

    // Shade bits from SHA-512("shade:" || peer_id).
    let mut shade_h = Sha512::new();
    shade_h.update(b"shade:");
    shade_h.update(pb);
    let shade_dig = shade_h.finalize();

    // Hue from SHA-256("hue:" || peer_id).
    let mut hue_h = Sha256::new();
    hue_h.update(b"hue:");
    hue_h.update(pb);
    let hue_dig = hue_h.finalize();

    // Hue2 from SHA-256("hue2:" || peer_id).
    let mut hue2_h = Sha256::new();
    hue2_h.update(b"hue2:");
    hue2_h.update(pb);
    let hue2_dig = hue2_h.finalize();

    let hue_u16 = ((hue_dig[0] as u16) << 8) | (hue_dig[1] as u16);
    let hue2_u16 = ((hue2_dig[0] as u16) << 8) | (hue2_dig[1] as u16);
    let hue = (hue_u16 % 360) as f32 / 360.0;
    let hue2 = (hue2_u16 % 360) as f32 / 360.0;

    let mut cells = vec![false; total];
    let mut shades = vec![0u8; total];
    let shade_mod = config.shade_mode.max(1) as u32;

    for row in 0..grid {
        for col in 0..half_cols {
            let bi = row * half_cols + col;
            let byte_p = pat_dig[bi / 8];
            let byte_s = shade_dig[bi / 8];
            let on = ((byte_p >> (7 - (bi % 8))) & 1) == 1;
            let sh = (((byte_s >> (7 - (bi % 8))) & 1) as u32 % shade_mod) as u8;
            let mir = grid - 1 - col;
            cells[row * grid + col] = on;
            shades[row * grid + col] = sh;
            if mir != col {
                cells[row * grid + mir] = on;
                shades[row * grid + mir] = sh;
            }
        }
    }

    AvatarPattern {
        grid,
        hue,
        hue2,
        cells,
        shades,
    }
}

// ---------------------------------------------------------------------------
// Island flood-fill (connected-component labeling)
// ---------------------------------------------------------------------------

/// Labels each foreground cell with an island index (1-based; 0 = background).
/// Connectivity: 4 (cardinal) or 8 (cardinal + diagonal).
fn find_islands(cells: &[bool], grid: usize, conn: u8) -> (Vec<i32>, usize) {
    let total = grid * grid;
    let mut labels = vec![0i32; total];
    let mut next_label = 1usize;

    let dirs4: &[(i32, i32)] = &[(-1, 0), (1, 0), (0, -1), (0, 1)];
    let dirs8: &[(i32, i32)] = &[
        (-1, -1),
        (-1, 0),
        (-1, 1),
        (0, -1),
        (0, 1),
        (1, -1),
        (1, 0),
        (1, 1),
    ];
    let dirs: &[(i32, i32)] = if conn >= 8 { dirs8 } else { dirs4 };

    for start in 0..total {
        if !cells[start] || labels[start] != 0 {
            continue;
        }
        let label = next_label as i32;
        next_label += 1;
        labels[start] = label;
        let mut queue = vec![start];
        let mut qi = 0;
        while qi < queue.len() {
            let cur = queue[qi];
            qi += 1;
            let cr = (cur / grid) as i32;
            let cc = (cur % grid) as i32;
            for &(dr, dc) in dirs {
                let nr = cr + dr;
                let nc = cc + dc;
                if nr < 0 || nr >= grid as i32 || nc < 0 || nc >= grid as i32 {
                    continue;
                }
                let ni = nr as usize * grid + nc as usize;
                if cells[ni] && labels[ni] == 0 {
                    labels[ni] = label;
                    queue.push(ni);
                }
            }
        }
    }

    (labels, next_label - 1)
}

// ---------------------------------------------------------------------------
// SVG generator — direct port of avatar_lab.html buildAvatarSvg()
// ---------------------------------------------------------------------------

/// Generate a complete SVG string for `peer_id` using `config`.
///
/// Uses `viewBox="0 0 {grid} {grid}"` so every cell is 1×1 SVG unit and Qt's
/// SVG renderer scales the image to whatever `size` the QML `Image` requests.
pub fn build_avatar_svg(peer_id: &str, config: &AvatarConfig) -> String {
    let pat = pattern_for_peer(peer_id, config);
    let grid = pat.grid;
    let half = grid / 2;

    // Island labels (if enabled).
    let (island_labels, island_count) = if config.islands {
        find_islands(&pat.cells, grid, config.island_conn)
    } else {
        (vec![0i32; grid * grid], 0)
    };

    // Per-island hue + sat (golden-ratio stepping from base hue).
    let mut island_hues = vec![0.0f32; island_count + 1];
    let mut island_sats = vec![config.sat; island_count + 1];
    for k in 0..island_count {
        island_hues[k + 1] = (pat.hue + k as f32 * config.island_step) % 1.0;
        if config.island_varsat {
            let raw = config.sat + (((k as f32 + 1.0) * config.island_step) % 1.0 - 0.5) * 0.3;
            island_sats[k + 1] = raw.clamp(0.15, 1.0);
        }
    }

    // Block-shift for checker/quad dual-hue mode (matches lab JS).
    let block_shift: u32 = if grid >= 24 {
        2
    } else if grid >= 10 {
        1
    } else {
        0
    };

    // Background colour.
    let bg_fill = if config.bg_tint {
        hsl_str(pat.hue, config.sat * 0.40, config.bg_lig)
    } else {
        "#2B2D31".to_owned()
    };

    // Clip-path border-radius: 18% of grid units (matches canvas `width * 0.18`).
    let corner_r = grid as f32 * 0.18;

    // SVG root attributes.
    let sr = if config.svg_crisp {
        "crispEdges"
    } else {
        "auto"
    };
    // Slight overshoot only when crisp: fills sub-pixel gaps without blurring.
    let cell_w = if config.svg_crisp { 1.01 } else { 1.0 };
    let rx_cell = if config.svg_round_cells { "0.25" } else { "0" };

    // Pre-allocate a generous buffer.
    let cap = 128 + grid * grid * 80;
    let mut svg = String::with_capacity(cap);

    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{grid}" height="{grid}" viewBox="0 0 {grid} {grid}" shape-rendering="{sr}"><defs><clipPath id="ac"><rect width="{grid}" height="{grid}" rx="{corner_r:.2}" ry="{corner_r:.2}"/></clipPath></defs><g clip-path="url(#ac)"><rect width="{grid}" height="{grid}" fill="{bg_fill}"/>"#,
    ));

    for row in 0..grid {
        for col in 0..grid {
            let i = row * grid + col;
            if !pat.cells[i] {
                continue;
            }

            // --- Hue selection ---
            let mut hue = pat.hue;
            if config.dual_hue {
                match config.dual_hue_mode.as_str() {
                    "topbot" => {
                        if row >= half { hue = pat.hue2; }
                    }
                    "checker" => {
                        if (((row >> block_shift) + (col >> block_shift)) & 1) == 1 {
                            hue = pat.hue2;
                        }
                    }
                    _ /* quad */ => {
                        if (row < half) != (col < half) { hue = pat.hue2; }
                    }
                }
            }
            let mut sat_used = config.sat;
            if config.islands {
                let lbl = island_labels[i] as usize;
                hue = island_hues[lbl];
                sat_used = island_sats[lbl];
            }

            // --- Lightness selection ---
            let sh = pat.shades[i];
            let lig = match config.shade_mode {
                2 => {
                    if sh != 0 {
                        config.lig + config.spread
                    } else {
                        config.lig - config.spread * 0.5
                    }
                }
                3 => {
                    if sh == 0 && row < half {
                        config.lig - config.spread
                    } else if sh != 0 && row < half {
                        config.lig + config.spread
                    } else {
                        config.lig + config.spread * 0.35
                    }
                }
                _ => config.lig,
            };
            let lig = lig.clamp(0.05, 0.95);

            let fill = hsl_str(hue, sat_used, lig);
            svg.push_str(&format!(
                r#"<rect x="{col}" y="{row}" width="{cell_w:.2}" height="{cell_w:.2}" rx="{rx_cell}" ry="{rx_cell}" fill="{fill}"/>"#,
            ));
        }
    }

    svg.push_str("</g></svg>");
    svg
}

/// Convert HSL (each 0–1) to an RGB hex colour string (`#rrggbb`).
///
/// Qt's SVG renderer (QSvgRenderer / SVG Tiny 1.2) does not support the
/// CSS `hsl()` notation, so we must emit plain hex values.
fn hsl_str(hue: f32, sat: f32, lig: f32) -> String {
    // Standard HSL → RGB algorithm.
    let (r, g, b) = hsl_to_rgb(hue, sat, lig);
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Return the identity-derived background tint hex colour for `peer_id`.
///
/// This matches exactly the `bg_fill` value computed inside `build_avatar_svg`:
/// `hsl(hue, sat*0.40, bg_lig)`. When `bg_tint` is false the avatar uses
/// `#2B2D31`; this function returns that value in the same case.
pub fn avatar_tint_hex(peer_id: &str, config: &AvatarConfig) -> String {
    if !config.bg_tint {
        return "#2B2D31".to_owned();
    }
    let pat = pattern_for_peer(peer_id, config);
    hsl_str(pat.hue, config.sat * 0.40, config.bg_lig)
}

fn hsl_to_rgb(hue: f32, sat: f32, lig: f32) -> (u8, u8, u8) {
    let h = hue.fract().abs(); // keep in [0,1)
    let s = sat.clamp(0.0, 1.0);
    let l = lig.clamp(0.0, 1.0);

    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;

    let r = hue_to_component(p, q, h + 1.0 / 3.0);
    let g = hue_to_component(p, q, h);
    let b = hue_to_component(p, q, h - 1.0 / 3.0);

    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn hue_to_component(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

// ---------------------------------------------------------------------------
// Legacy pattern_to_json — kept for unit tests and test-vector tooling only.
// ---------------------------------------------------------------------------

/// Encode a pattern as JSON for test-vector verification.
pub fn pattern_to_json(p: &AvatarPattern) -> String {
    let n = p.cells.len();
    let mut s = String::with_capacity(64 + n * 6);
    s.push_str(&format!(
        r#"{{"hue":{:.6},"hue2":{:.6},"grid":{},"cells":["#,
        p.hue, p.hue2, p.grid
    ));
    for (i, c) in p.cells.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(if *c { "true" } else { "false" });
    }
    s.push_str(r#"],"shades":["#);
    for (i, sh) in p.shades.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&sh.to_string());
    }
    s.push_str("]}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> AvatarConfig {
        AvatarConfig::default()
    }
    fn untrusted_cfg() -> AvatarConfig {
        AvatarConfig::untrusted()
    }

    #[test]
    fn avatar_seed_id_maps_hex_peer_id_to_identity_pub() {
        let hex = "4a6a66375f593a81b6e155c8db2aee144d9bfb7729219a95fc959a3b6e6250f6";
        let b64 = "LLgFfmkf9zL3mKUbLNJ-5722nCtF6gRr-PXZ7Z5_6DU=";
        let lookup = |id: &str| {
            if id == b64 || id == hex {
                Some(b64.to_owned())
            } else {
                None
            }
        };
        assert_eq!(avatar_seed_id(b64, b64, hex, lookup), b64);
        assert_eq!(avatar_seed_id(hex, b64, hex, lookup), b64);
        assert_eq!(
            avatar_seed_id(hex, b64, "other_local_peer_id", |_| None),
            hex
        );
    }

    #[test]
    fn hex_peer_id_and_public_id_produce_same_avatar_pattern() {
        let hex = "4a6a66375f593a81b6e155c8db2aee144d9bfb7729219a95fc959a3b6e6250f6";
        let b64 = "LLgFfmkf9zL3mKUbLNJ-5722nCtF6gRr-PXZ7Z5_6DU=";
        let cfg = default_cfg();
        let from_b64 = pattern_for_peer(b64, &cfg);
        let from_hex = pattern_for_peer(
            &avatar_seed_id(hex, b64, hex, |_| Some(b64.to_owned())),
            &cfg,
        );
        assert_eq!(from_hex.hue, from_b64.hue);
        assert_eq!(from_hex.cells, from_b64.cells);
    }

    #[test]
    fn is_deterministic() {
        let cfg = default_cfg();
        let a = pattern_for_peer("alice", &cfg);
        let b = pattern_for_peer("alice", &cfg);
        assert_eq!(a.cells, b.cells);
        assert_eq!(a.hue, b.hue);
    }

    #[test]
    fn distinct_ids_produce_distinct_patterns() {
        let cfg = default_cfg();
        let a = pattern_for_peer("alice", &cfg);
        let b = pattern_for_peer("bob", &cfg);
        assert!(a.cells != b.cells || (a.hue - b.hue).abs() > f32::EPSILON);
    }

    #[test]
    fn pattern_is_horizontally_symmetric() {
        let cfg = default_cfg();
        let p = pattern_for_peer("symmetry-check", &cfg);
        let grid = p.grid;
        for row in 0..grid {
            for col in 0..(grid / 2) {
                assert_eq!(
                    p.cells[row * grid + col],
                    p.cells[row * grid + (grid - 1 - col)],
                    "row {row} col {col} not mirrored"
                );
            }
        }
    }

    #[test]
    fn hue_in_unit_range() {
        let cfg = default_cfg();
        for id in &["", "x", "a-much-longer-peer-id-of-some-length", "🦀"] {
            let p = pattern_for_peer(id, &cfg);
            assert!(p.hue >= 0.0 && p.hue < 1.0, "hue out of range for {id:?}");
            assert!(
                p.hue2 >= 0.0 && p.hue2 < 1.0,
                "hue2 out of range for {id:?}"
            );
        }
    }

    #[test]
    fn hue_and_hue2_are_independent() {
        let cfg = default_cfg();
        let p = pattern_for_peer("hue-independence", &cfg);
        assert!((p.hue - p.hue2).abs() > 0.001 || p.cells.iter().filter(|&&c| c).count() == 0);
    }

    #[test]
    fn empty_and_short_inputs_dont_panic() {
        let cfg = default_cfg();
        let _ = pattern_for_peer("", &cfg);
        let _ = pattern_for_peer("a", &cfg);
    }

    #[test]
    fn grid_16_produces_256_cells() {
        let cfg = AvatarConfig {
            grid: 16,
            ..default_cfg()
        };
        let p = pattern_for_peer("grid16", &cfg);
        assert_eq!(p.grid, 16);
        assert_eq!(p.cells.len(), 256);
        assert_eq!(p.shades.len(), 256);
    }

    #[test]
    fn grid_5_odd_symmetry() {
        let cfg = AvatarConfig {
            grid: 5,
            ..default_cfg()
        };
        let p = pattern_for_peer("odd-grid", &cfg);
        let g = p.grid;
        for row in 0..g {
            for col in 0..(g / 2) {
                assert_eq!(
                    p.cells[row * g + col],
                    p.cells[row * g + (g - 1 - col)],
                    "row {row} col {col} not mirrored in 5×5"
                );
            }
        }
    }

    #[test]
    fn untrusted_config_grid_is_8() {
        let cfg = untrusted_cfg();
        assert_eq!(cfg.grid, 8);
        assert_eq!(cfg.shade_mode, 1);
        assert!(!cfg.islands);
        assert!(!cfg.bg_tint);
    }

    #[test]
    fn default_config_matches_settings_ui_contract() {
        let cfg = AvatarConfig::default();
        assert_eq!(cfg.grid, 16);
        assert!((cfg.sat - 0.55).abs() < f32::EPSILON);
        assert!((cfg.lig - 0.55).abs() < f32::EPSILON);
        assert!((cfg.spread - 0.15).abs() < f32::EPSILON);
        assert!(cfg.bg_tint);
        assert!((cfg.bg_lig - 0.12).abs() < f32::EPSILON);
        assert_eq!(cfg.shade_mode, 1);
        assert!(!cfg.dual_hue);
        assert_eq!(cfg.dual_hue_mode, "topbot");
        assert!(cfg.islands);
        assert_eq!(cfg.island_conn, 8);
        assert!((cfg.island_step - 0.62).abs() < f32::EPSILON);
        assert!(cfg.island_varsat);
        assert!(cfg.svg_crisp);
        assert!(!cfg.svg_round_cells);
    }

    #[test]
    fn partial_json_deserializes_with_field_defaults() {
        let cfg: AvatarConfig = serde_json::from_str(r#"{"grid":8,"svg_crisp":false}"#).unwrap();
        assert_eq!(cfg.grid, 8);
        assert!(!cfg.svg_crisp);
        assert!((cfg.sat - 0.55).abs() < f32::EPSILON);
        assert!(cfg.islands);
    }

    #[test]
    fn avatar_settings_change_svg_output() {
        let peer = "settings-matrix";
        let base = default_cfg();

        let grid8 = build_avatar_svg(
            peer,
            &AvatarConfig {
                grid: 8,
                ..base.clone()
            },
        );
        let grid32 = build_avatar_svg(
            peer,
            &AvatarConfig {
                grid: 32,
                ..base.clone()
            },
        );
        assert!(grid8.contains(r#"viewBox="0 0 8 8""#));
        assert!(grid32.contains(r#"viewBox="0 0 32 32""#));

        let tinted = build_avatar_svg(peer, &base);
        let flat_bg = build_avatar_svg(
            peer,
            &AvatarConfig {
                bg_tint: false,
                ..base.clone()
            },
        );
        assert!(flat_bg.contains("fill=\"#2B2D31\""));
        assert_ne!(tinted, flat_bg);

        let rounded = build_avatar_svg(
            peer,
            &AvatarConfig {
                svg_round_cells: true,
                ..base.clone()
            },
        );
        let square = build_avatar_svg(
            peer,
            &AvatarConfig {
                svg_round_cells: false,
                ..base.clone()
            },
        );
        assert!(rounded.contains(r#"rx="0.25" ry="0.25""#));
        assert!(!square.contains(r#"rx="0.25" ry="0.25""#));

        let dual = build_avatar_svg(
            peer,
            &AvatarConfig {
                dual_hue: true,
                dual_hue_mode: "topbot".to_owned(),
                islands: false,
                shade_mode: 1,
                ..base.clone()
            },
        );
        let single = build_avatar_svg(
            peer,
            &AvatarConfig {
                dual_hue: false,
                islands: false,
                shade_mode: 1,
                ..base.clone()
            },
        );
        assert_ne!(dual, single);

        let islands_on = build_avatar_svg(
            peer,
            &AvatarConfig {
                islands: true,
                island_conn: 4,
                shade_mode: 1,
                dual_hue: false,
                ..base.clone()
            },
        );
        let islands_off = build_avatar_svg(
            peer,
            &AvatarConfig {
                islands: false,
                shade_mode: 1,
                dual_hue: false,
                ..base.clone()
            },
        );
        assert_ne!(islands_on, islands_off);

        let shade1 = build_avatar_svg(
            peer,
            &AvatarConfig {
                shade_mode: 1,
                islands: false,
                dual_hue: false,
                ..base.clone()
            },
        );
        let shade3 = build_avatar_svg(
            peer,
            &AvatarConfig {
                shade_mode: 3,
                islands: false,
                dual_hue: false,
                ..base.clone()
            },
        );
        assert_ne!(shade1, shade3);

        let low_sat = build_avatar_svg(
            peer,
            &AvatarConfig {
                sat: 0.15,
                islands: false,
                shade_mode: 1,
                dual_hue: false,
                ..base.clone()
            },
        );
        let high_sat = build_avatar_svg(
            peer,
            &AvatarConfig {
                sat: 0.95,
                islands: false,
                shade_mode: 1,
                dual_hue: false,
                ..base.clone()
            },
        );
        assert_ne!(low_sat, high_sat);
    }

    #[test]
    fn avatar_tint_hex_tracks_bg_tint_setting() {
        let peer = "tint-peer";
        let cfg = default_cfg();
        let tinted = avatar_tint_hex(peer, &cfg);
        let flat = avatar_tint_hex(
            peer,
            &AvatarConfig {
                bg_tint: false,
                ..cfg
            },
        );
        assert_eq!(flat, "#2B2D31");
        assert_ne!(tinted, flat);
    }

    #[test]
    fn svg_crisp_edges_affects_rendering_hints() {
        let peer = "crisp-test";
        let crisp = build_avatar_svg(
            peer,
            &AvatarConfig {
                svg_crisp: true,
                ..default_cfg()
            },
        );
        let soft = build_avatar_svg(
            peer,
            &AvatarConfig {
                svg_crisp: false,
                ..default_cfg()
            },
        );
        assert!(crisp.contains(r#"shape-rendering="crispEdges""#));
        assert!(soft.contains(r#"shape-rendering="auto""#));
        assert!(crisp.contains(r#"width="1.01" height="1.01""#));
        assert!(!soft.contains(r#"width="1.01" height="1.01""#));
    }

    #[test]
    fn svg_output_is_valid() {
        let cfg = default_cfg();
        let svg = build_avatar_svg("svg-test", &cfg);
        assert!(svg.starts_with("<svg "), "svg should start with <svg");
        assert!(svg.ends_with("</svg>"), "svg should end with </svg>");
        assert!(
            svg.contains("viewBox=\"0 0 16 16\""),
            "viewBox should match grid"
        );
    }

    #[test]
    fn svg_untrusted_has_grid_8() {
        let cfg = untrusted_cfg();
        let svg = build_avatar_svg("stranger", &cfg);
        assert!(svg.contains("viewBox=\"0 0 8 8\""));
    }

    #[test]
    fn svg_islands_produces_multiple_fills() {
        let cfg = AvatarConfig {
            islands: true,
            island_conn: 4,
            bg_tint: false,
            shade_mode: 1,
            dual_hue: false,
            ..default_cfg()
        };
        let svg = build_avatar_svg("island-peer", &cfg);
        let fill_count: std::collections::HashSet<_> = svg
            .split("fill=\"")
            .skip(1)
            .map(|s| s.split('"').next().unwrap_or(""))
            .filter(|fill| fill.starts_with('#') && *fill != "#2B2D31")
            .collect();
        assert!(
            !svg.contains("fill=\"hsl("),
            "Qt-compatible SVG output should use hex colours, not CSS hsl()"
        );
        assert!(fill_count.len() >= 1);
    }

    #[test]
    fn pattern_to_json_shape() {
        let cfg = AvatarConfig {
            grid: 8,
            ..default_cfg()
        };
        let p = pattern_for_peer("json-test", &cfg);
        let s = pattern_to_json(&p);
        assert!(s.contains("\"hue\":"));
        assert!(s.contains("\"hue2\":"));
        assert!(s.contains("\"grid\":8"));
        assert!(s.contains("\"cells\":["));
        assert!(s.contains("\"shades\":["));
    }
}
