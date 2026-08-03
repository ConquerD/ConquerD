//! Video codec identity, advertisement, and negotiation.
//!
//! Video is the only media channel where peers can legitimately disagree about
//! the bytes inside the payload. Opus is the one audio codec, so
//! `core.audio.opus` names it in the capability id and the question never
//! arises. Video cannot work that way: the encoder a peer can actually run
//! depends on its platform (Media Foundation H.264 is available on Windows and
//! nowhere else) and on how the binary was built.
//!
//! So the capability id is codec-*neutral* (`core.video.v1`, `room.video.sfu`)
//! and the codec set is advertised in `params.codecs`. Two peers intersect
//! their lists and [`negotiate`] picks the winner. A frame then carries its
//! codec on the wire (see the fragment header) so a receiver routes it to the
//! matching decoder rather than inferring one from the capability id.
//!
//! # Why the id must not name a codec
//!
//! An id like `core.video.vp8` forces a lie the moment the shipped encoder is
//! anything else, and negotiation would still not know what the bytes are:
//! [`CapabilityDescriptor::is_compatible_with`](crate::CapabilityDescriptor::is_compatible_with)
//! matches on id and major version only, so `vp8` vs `h264` peers would either
//! fail to negotiate video at all (different ids) or negotiate it and then feed
//! each other undecodable frames (same id, different bytes). Neither is
//! recoverable at runtime. The codec belongs in params and on the frame.

use serde::{Deserialize, Serialize};

/// A video codec a peer may be able to encode and/or decode.
///
/// The wire byte is part of the fragment header and therefore a protocol
/// constant: **never renumber an existing variant**. New codecs take the next
/// free byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    /// H.264 / AVC. On Windows this is Media Foundation's hardware-preferring
    /// encoder and decoder, using the codec licence held by the OS. There is
    /// deliberately no in-tree AVC implementation — see the video non-goals in
    /// `backlog.md`.
    H264,
    /// VP8. Royalty-free, and the path to non-Windows peers, but only present
    /// when the binary was built with a VP8 implementation linked in.
    Vp8,
    /// Compression-free I420 packing used by transport tests.
    ///
    /// Present on the wire so the test path exercises exactly the same framing
    /// and negotiation as a real codec. It is never advertised by a release
    /// build — [`advertised_codecs`] filters it out.
    Stub,
}

impl VideoCodec {
    /// Byte carried in the fragment header.
    pub const fn as_wire(self) -> u8 {
        match self {
            VideoCodec::H264 => 0x01,
            VideoCodec::Vp8 => 0x02,
            VideoCodec::Stub => 0xFF,
        }
    }

    /// Parse a fragment-header byte. `None` for an unknown codec, which a
    /// receiver must treat as "cannot decode" rather than guessing.
    pub const fn from_wire(b: u8) -> Option<Self> {
        match b {
            0x01 => Some(VideoCodec::H264),
            0x02 => Some(VideoCodec::Vp8),
            0xFF => Some(VideoCodec::Stub),
            _ => None,
        }
    }

    /// Name used in `params.codecs` and in logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            VideoCodec::H264 => "h264",
            VideoCodec::Vp8 => "vp8",
            VideoCodec::Stub => "stub",
        }
    }

    /// Parse a `params.codecs` entry. Unknown names are ignored by callers
    /// rather than rejected, so a newer peer advertising a codec we have never
    /// heard of degrades to "no mutual codec" instead of failing the whole
    /// capability exchange.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "h264" => Some(VideoCodec::H264),
            "vp8" => Some(VideoCodec::Vp8),
            "stub" => Some(VideoCodec::Stub),
            _ => None,
        }
    }
}

/// Preference order used by [`negotiate`], most preferred first.
///
/// H.264 outranks VP8 because where both are available H.264 is the hardware
/// path (Media Foundation will pick a GPU encoder when the machine has one),
/// and hardware encode is what keeps a 720p send off the CPU. The stub ranks
/// last so that a build which somehow advertises it still prefers any real
/// codec.
pub const PREFERENCE: [VideoCodec; 3] = [VideoCodec::H264, VideoCodec::Vp8, VideoCodec::Stub];

/// Pick the best codec both sides support.
///
/// **An empty `remote` means "no mutual codec", not "unknown".** Callers that
/// may not have heard a peer's capability announce yet must distinguish the two
/// themselves — conflating them turns a missing announce into a camera that
/// silently never starts, which is much harder to diagnose than a frame the
/// receiver drops.
///
/// Preference is **ours**, not the remote's, and the order is fixed rather than
/// negotiated: two peers running this function against the same pair of lists
/// must reach the same answer without exchanging another message. Returns
/// `None` when the intersection is empty — the caller must then not send video,
/// rather than sending something the peer cannot decode.
pub fn negotiate(local: &[VideoCodec], remote: &[VideoCodec]) -> Option<VideoCodec> {
    PREFERENCE
        .iter()
        .copied()
        .find(|c| local.contains(c) && remote.contains(c))
}

/// Read the `video_codec` setting into a preference.
///
/// `"auto"` (and anything unrecognised, including an empty string) means "no
/// preference" — let [`negotiate`] decide. A name that parses is a request to
/// send in that codec when it is possible to do so.
pub fn preference_from_setting(setting: &str) -> Option<VideoCodec> {
    match setting.trim() {
        "" | "auto" => None,
        // The stub is a transport-test fixture and is never advertised, so it
        // must not be selectable from settings either.
        "stub" => None,
        other => VideoCodec::parse(other),
    }
}

/// [`negotiate`], but honour the sender's own codec preference first.
///
/// # Why asymmetry is safe here
///
/// [`negotiate`] documents that both peers must reach the same answer from the
/// same pair of lists, and a user preference deliberately breaks that: A can
/// prefer VP8 while B prefers H.264. That is fine because the two directions are
/// independent. Each frame carries its codec in the fragment header, and a
/// receiver builds a decoder per (sender, codec) — so A sending VP8 while B
/// sends H.264 is two working streams, not a mismatch. What must never happen is
/// sending a codec the *receiver* cannot decode, which is why `preferred` is
/// only honoured when it is in both lists.
///
/// Falls back to plain [`negotiate`] when the preference is absent or not
/// mutually supported: a preference the peer cannot decode is a reason to pick
/// something else, never a reason to send nothing.
pub fn negotiate_preferring(
    local: &[VideoCodec],
    remote: &[VideoCodec],
    preferred: Option<VideoCodec>,
) -> Option<VideoCodec> {
    if let Some(want) = preferred {
        if local.contains(&want) && remote.contains(&want) {
            return Some(want);
        }
    }
    negotiate(local, remote)
}

/// Our own most-preferred codec out of `available`, honouring `preferred`.
///
/// The one-sided counterpart to [`negotiate_preferring`], for a room send where
/// there is no single remote list to intersect with. A preference this build
/// cannot encode is ignored rather than fatal.
pub fn best_available(
    available: &[VideoCodec],
    preferred: Option<VideoCodec>,
) -> Option<VideoCodec> {
    if let Some(want) = preferred {
        if available.contains(&want) {
            return Some(want);
        }
    }
    PREFERENCE.iter().copied().find(|c| available.contains(c))
}

/// Filter a locally-available codec set down to what should be advertised.
///
/// Drops [`VideoCodec::Stub`]: it is a transport-test fixture, and advertising
/// it would let a peer negotiate uncompressed I420 over a real link.
pub fn advertised_codecs(available: &[VideoCodec]) -> Vec<VideoCodec> {
    available
        .iter()
        .copied()
        .filter(|c| *c != VideoCodec::Stub)
        .collect()
}

/// Render a codec set for `params.codecs`.
pub fn codec_names(codecs: &[VideoCodec]) -> Vec<String> {
    codecs.iter().map(|c| c.as_str().to_owned()).collect()
}

/// Read a peer's `params.codecs` array.
///
/// Unknown names are skipped. A missing or malformed `codecs` field yields an
/// empty set, which means "no mutual codec" — the honest reading of a peer that
/// did not tell us what it speaks.
pub fn codecs_from_params(params: &serde_json::Value) -> Vec<VideoCodec> {
    params
        .get("codecs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter_map(VideoCodec::parse)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wire_bytes_round_trip() {
        for c in PREFERENCE {
            assert_eq!(VideoCodec::from_wire(c.as_wire()), Some(c));
        }
    }

    /// GOLDEN: these bytes are on the wire. Renumbering silently makes every
    /// peer running an older build decode the wrong codec.
    #[test]
    fn wire_bytes_are_frozen() {
        assert_eq!(VideoCodec::H264.as_wire(), 0x01);
        assert_eq!(VideoCodec::Vp8.as_wire(), 0x02);
        assert_eq!(VideoCodec::Stub.as_wire(), 0xFF);
    }

    #[test]
    fn unknown_wire_byte_is_not_guessed() {
        assert_eq!(VideoCodec::from_wire(0x00), None);
        assert_eq!(VideoCodec::from_wire(0x03), None);
        assert_eq!(VideoCodec::from_wire(0x7F), None);
    }

    #[test]
    fn names_round_trip() {
        for c in PREFERENCE {
            assert_eq!(VideoCodec::parse(c.as_str()), Some(c));
        }
        assert_eq!(VideoCodec::parse("av1"), None);
        assert_eq!(VideoCodec::parse(""), None);
    }

    #[test]
    fn negotiate_prefers_h264_when_both_available() {
        let both = [VideoCodec::H264, VideoCodec::Vp8];
        assert_eq!(negotiate(&both, &both), Some(VideoCodec::H264));
    }

    #[test]
    fn negotiate_falls_back_to_the_only_mutual_codec() {
        let windows = [VideoCodec::H264, VideoCodec::Vp8];
        let linux = [VideoCodec::Vp8];
        assert_eq!(negotiate(&windows, &linux), Some(VideoCodec::Vp8));
    }

    #[test]
    fn negotiate_is_symmetric() {
        let a = [VideoCodec::H264, VideoCodec::Vp8];
        let b = [VideoCodec::Vp8];
        assert_eq!(negotiate(&a, &b), negotiate(&b, &a));
    }

    /// Documents the trap deliberately: an empty remote list is *not* the same
    /// question as "which codec do we share", and callers must not treat a peer
    /// they have not heard from as one they cannot talk to.
    #[test]
    fn an_unheard_peer_is_indistinguishable_from_an_incompatible_one_here() {
        let local = [VideoCodec::H264, VideoCodec::Vp8];
        assert_eq!(negotiate(&local, &[]), None);
        assert_eq!(negotiate(&local, &[VideoCodec::Stub]), None);
    }

    #[test]
    fn no_mutual_codec_yields_none() {
        assert_eq!(negotiate(&[VideoCodec::H264], &[VideoCodec::Vp8]), None);
        assert_eq!(negotiate(&[], &[VideoCodec::Vp8]), None);
        assert_eq!(negotiate(&[VideoCodec::H264], &[]), None);
    }

    #[test]
    fn a_preference_wins_when_both_peers_have_it() {
        let both = [VideoCodec::H264, VideoCodec::Vp8];
        assert_eq!(
            negotiate_preferring(&both, &both, Some(VideoCodec::Vp8)),
            Some(VideoCodec::Vp8),
            "a user who asked for VP8 must get VP8, not the default preference"
        );
    }

    /// A preference the receiver cannot decode must fall back, not send bytes
    /// nobody can read and not refuse to send at all.
    #[test]
    fn a_preference_the_peer_lacks_falls_back_to_negotiation() {
        let windows = [VideoCodec::H264, VideoCodec::Vp8];
        let linux = [VideoCodec::Vp8];
        assert_eq!(
            negotiate_preferring(&windows, &linux, Some(VideoCodec::H264)),
            Some(VideoCodec::Vp8)
        );
    }

    /// Likewise for a preference this build cannot *encode* — a settings file
    /// carried over from a Windows machine to a Linux one.
    #[test]
    fn a_preference_this_build_lacks_falls_back_to_negotiation() {
        let linux = [VideoCodec::Vp8];
        assert_eq!(
            negotiate_preferring(&linux, &linux, Some(VideoCodec::H264)),
            Some(VideoCodec::Vp8)
        );
    }

    #[test]
    fn no_preference_is_plain_negotiation() {
        let both = [VideoCodec::H264, VideoCodec::Vp8];
        assert_eq!(
            negotiate_preferring(&both, &both, None),
            negotiate(&both, &both)
        );
    }

    /// A preference cannot conjure a mutual codec that does not exist.
    #[test]
    fn a_preference_does_not_override_an_empty_intersection() {
        assert_eq!(
            negotiate_preferring(
                &[VideoCodec::H264],
                &[VideoCodec::Vp8],
                Some(VideoCodec::H264)
            ),
            None
        );
    }

    #[test]
    fn best_available_honours_a_preference_and_falls_back_in_order() {
        let both = [VideoCodec::H264, VideoCodec::Vp8];
        assert_eq!(
            best_available(&both, Some(VideoCodec::Vp8)),
            Some(VideoCodec::Vp8)
        );
        assert_eq!(best_available(&both, None), Some(VideoCodec::H264));
        // Unencodable preference: ignored rather than fatal.
        assert_eq!(
            best_available(&[VideoCodec::Vp8], Some(VideoCodec::H264)),
            Some(VideoCodec::Vp8)
        );
        assert_eq!(best_available(&[], Some(VideoCodec::Vp8)), None);
    }

    #[test]
    fn the_codec_setting_parses_to_a_preference() {
        assert_eq!(preference_from_setting("h264"), Some(VideoCodec::H264));
        assert_eq!(preference_from_setting("vp8"), Some(VideoCodec::Vp8));
        assert_eq!(preference_from_setting(" vp8 "), Some(VideoCodec::Vp8));
    }

    /// "auto" is the default, and an unset or nonsense setting must read the
    /// same way — as "let negotiation decide", never as a hard failure.
    #[test]
    fn an_absent_or_unknown_codec_setting_means_auto() {
        for s in ["", "auto", "   ", "av1", "H264"] {
            assert_eq!(preference_from_setting(s), None, "{s:?} must mean auto");
        }
    }

    /// The stub is never advertised, so it must not be reachable by writing it
    /// into a settings file either.
    #[test]
    fn the_stub_cannot_be_selected_from_settings() {
        assert_eq!(preference_from_setting("stub"), None);
    }

    #[test]
    fn stub_is_never_advertised() {
        let available = [VideoCodec::H264, VideoCodec::Stub];
        assert_eq!(advertised_codecs(&available), vec![VideoCodec::H264]);
        assert!(advertised_codecs(&[VideoCodec::Stub]).is_empty());
    }

    #[test]
    fn params_codecs_are_parsed() {
        let p = json!({ "codecs": ["h264", "vp8"] });
        assert_eq!(
            codecs_from_params(&p),
            vec![VideoCodec::H264, VideoCodec::Vp8]
        );
    }

    #[test]
    fn unknown_codec_names_are_skipped_not_fatal() {
        let p = json!({ "codecs": ["av1", "vp8", "h265"] });
        assert_eq!(codecs_from_params(&p), vec![VideoCodec::Vp8]);
    }

    /// A peer that never told us what it speaks must read as "no mutual
    /// codec", not as "assume the default".
    #[test]
    fn missing_or_malformed_codecs_field_is_empty() {
        assert!(codecs_from_params(&json!({})).is_empty());
        assert!(codecs_from_params(&json!({ "codecs": "h264" })).is_empty());
        assert!(codecs_from_params(&json!({ "codecs": [] })).is_empty());
        assert!(codecs_from_params(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn codec_names_render_for_params() {
        assert_eq!(
            codec_names(&[VideoCodec::H264, VideoCodec::Vp8]),
            vec!["h264".to_owned(), "vp8".to_owned()]
        );
    }
}
