//! Local video capture: CameraX frames in, encoded VP8 out.
//!
//! The core already owns the hard parts — capture pacing, encoding, adaptive
//! bitrate, fragmentation. This wires the Android camera into
//! [`VideoSender`] and points the encoded output at either a direct peer or a
//! room.

use conquerd_client::connection_manager::ConnectionCommand;
use conquerd_client::video::codec::{make_encoder, EncoderParams};
use conquerd_client::video::sender::{CaptureLayout, FrameSink, Quality, SourceSpec, VideoSender};
use conquerd_features::video_codec::VideoCodec;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::sink::EventSink;

/// The codec Android encodes with.
///
/// VP8 unconditionally: it is the only codec every platform in this project
/// can both encode and decode, and Android has no equivalent of the OS-held
/// H.264 licence that makes Media Foundation usable on Windows.
const ANDROID_CODEC: VideoCodec = VideoCodec::Vp8;

/// Where encoded frames go.
enum Target {
    /// A direct 1:1 call.
    Peer(String),
    /// The room the local user is in. The supernode fans it out.
    Room,
}

/// Adapts the core's `FrameSink` onto the connection manager's command channel.
struct CommandSink {
    tx: mpsc::Sender<ConnectionCommand>,
    target: Target,
}

impl FrameSink for CommandSink {
    fn send(&mut self, encoded: Vec<u8>, keyframe: bool, pts_us: u64) {
        // `try_send`, never `blocking_send`: this runs on the capture thread,
        // and blocking it would stall the camera rather than the network. A
        // dropped frame on a saturated link is the correct outcome — the
        // encoder will be asked for a keyframe if the far end falls apart.
        let command = match &self.target {
            Target::Peer(peer_id) => ConnectionCommand::SendVideoFrame {
                peer_id: peer_id.clone(),
                encoded,
                keyframe,
                codec: ANDROID_CODEC,
                pts_us,
            },
            Target::Room => ConnectionCommand::SendRoomVideo {
                encoded,
                keyframe,
                codec: ANDROID_CODEC,
                pts_us,
            },
        };

        if self.tx.try_send(command).is_err() {
            // Not warn-per-frame: on a congested link this fires continuously
            // and would bury the log the way the E2E path already does.
            tracing::trace!("[video] dropped an encoded frame; command channel full");
        }
    }
}

/// Start capturing and sending local video.
///
/// `peer_id` sends to one peer; `None` sends into the current room. The camera
/// itself is driven by CameraX on the Kotlin side — this only opens the
/// consumer end, and [`VideoSender`] blocks until the first frame arrives.
pub fn start(
    tx: mpsc::Sender<ConnectionCommand>,
    peer_id: Option<String>,
    device_id: &str,
    quality_preset: &str,
    slot: Arc<RwLock<Option<VideoSender>>>,
    sink: EventSink,
) -> anyhow::Result<VideoSender> {
    let quality = Quality::from_name(quality_preset);
    let layout = CaptureLayout::single(SourceSpec::from_device_id(device_id));

    // The encoder is sized from the quality preset rather than the camera,
    // because it has to be built before the first frame exists. The capture
    // loop scales frames to match.
    let encoder = make_encoder(
        ANDROID_CODEC,
        EncoderParams {
            width: quality.width,
            height: quality.height,
            bitrate_bps: quality.bitrate_bps,
            fps: quality.fps,
            keyframe_interval_secs: quality.keyframe_interval_secs,
        },
    )?;

    let target = match &peer_id {
        Some(id) => Target::Peer(id.clone()),
        None => Target::Room,
    };
    let frame_sink = CommandSink {
        tx: tx.clone(),
        target,
    };

    info!(
        "[video] starting Android capture ({}x{} @ {}fps, {} kbps) -> {}",
        quality.width,
        quality.height,
        quality.fps,
        quality.bitrate_bps / 1000,
        peer_id.as_deref().unwrap_or("room"),
    );

    Ok(VideoSender::start(
        layout,
        quality,
        encoder,
        frame_sink,
        None,
        None,
        move |end| {
            // A capture can die on its own: CameraX unbinds when the activity
            // backgrounds, and the camera can be revoked outright. Clearing
            // the slot is what makes video restartable - leaving a dead
            // `VideoSender` in it means every later `video.start` is refused
            // as "already running", with no way back short of restarting the
            // app.
            warn!("[video] capture ended: {end:?}");
            *slot.write() = None;

            // Tell the UI, or the button stays on "Stop video" for a capture
            // that is no longer running.
            let payload = serde_json::json!({
                "event": "video_ended",
                "reason": format!("{end:?}"),
            });
            match sink.attach() {
                Ok(mut guard) => sink.emit(&mut guard, &payload.to_string()),
                Err(e) => warn!("[video] could not report capture end: {e}"),
            }
        },
    ))
}
