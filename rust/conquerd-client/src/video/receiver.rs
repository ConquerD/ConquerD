//! Decode → render loop.
//!
//! Mirror of [`super::sender`]: one dedicated OS thread owning the decoders,
//! because `vpx_codec_decode`/Media Foundation calls block and are CPU-bound.
//! Running them on a tokio worker would stall unrelated futures, and running
//! them on the Qt GUI thread would drop frames of the UI.
//!
//! Decoders are per-sender and stateful — inter frames reference the sender's
//! own previous frames — so they can never be shared between peers.
//!
//! They are also per-*codec*. A room fans out frames from several senders who
//! need not agree on one codec, so which decoder a frame needs is a property of
//! the frame, not of the session: each frame carries its codec and the decoder
//! is built to match. A sender that changes codec gets a fresh decoder, since
//! the old one's reference frames are meaningless to the new format.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use conquerd_features::video_codec::VideoCodec;
use parking_lot::Mutex;
use tracing::{debug, info, warn};

use super::codec::VideoDecoder;
use super::frame::RawFrame;
use super::sink;

/// One inbound encoded frame awaiting decode.
pub struct InboundFrame {
    pub peer_id: String,
    pub encoded: Vec<u8>,
    pub keyframe: bool,
    /// Codec these bytes are in, from the frame's signed header.
    pub codec: VideoCodec,
    /// Sender's capture time. Drives the hold/drop decision once decoded; see
    /// [`crate::media_sync`].
    pub pts_us: u64,
}

/// Shared hold/drop state, so the audio playout can advance the anchors that
/// steer video. Cloned to whoever plays content audio.
pub type SharedPlayout = Arc<Mutex<crate::media_sync::VideoPlayout<RawFrame>>>;

/// A peer's decoder together with the codec it was built for, so a codec change
/// is detectable rather than silently feeding bytes to the wrong decoder.
struct PeerDecoder {
    codec: VideoCodec,
    decoder: Box<dyn VideoDecoder>,
}

/// How a peer's decoder is faring, as one entry rather than two maps.
///
/// The two counts are different diagnoses and escalate on different schedules
/// (see [`FAILURES_BEFORE_DECODER_REBUILD`] and
/// [`STARVED_BEFORE_DECODER_REBUILD`]), but they share every lifecycle: a peer
/// who leaves, is evicted, or changes codec must lose both. Keeping them in one
/// entry is what makes that automatic instead of a second `remove` each of the
/// five teardown paths has to remember.
#[derive(Default)]
struct DecodeHealth {
    /// Consecutive `Err` results — frames the decoder rejected.
    errors: u32,
    /// Consecutive `Ok(None)` results — frames it took without drawing.
    starved: u32,
}

/// How many undecoded frames to queue before shedding.
///
/// Small on purpose: video is real-time, so a backlog is worth less than the
/// latency it adds. Dropping and waiting for the next keyframe beats playing
/// half a second late.
const QUEUE_DEPTH: usize = 8;

/// Consecutive decode failures from one peer before asking for a keyframe.
///
/// A *failure* is an error, not a decoder that has yet to produce its first
/// picture — see [`STARVED_BEFORE_KEYFRAME_REQUEST`] for the latter, and
/// [`VideoDecoder::decode`](crate::video::codec::VideoDecoder::decode) for why
/// the two are different answers.
const FAILURES_BEFORE_KEYFRAME_REQUEST: u32 = 3;

/// Consecutive decode failures before the decoder itself is thrown away.
///
/// A keyframe repairs a decoder that lost its references. It does nothing for
/// one that has stopped working — a Media Foundation transform that no longer
/// requests input fails *every* call from then on, so a receiver that only ever
/// asks for keyframes sits on a frozen tile until the user leaves the room.
/// Dropping the decoder here means the next frame builds a fresh one, which is
/// the only recovery that does not need the session torn down.
///
/// Well above the keyframe threshold on purpose: rebuilding costs a decoder
/// (and, on the hardware path, GPU surfaces), so a keyframe gets several
/// chances to work before it comes to this.
const FAILURES_BEFORE_DECODER_REBUILD: u32 = 15;

/// Frames accepted without a picture coming back before a keyframe is asked for.
///
/// This is the "decoder is fine, it just has nothing to draw" path: it has not
/// seen a keyframe yet, so every inter frame it is handed refers to pictures it
/// does not have. A keyframe is exactly the fix, and one second of inter frames
/// at 30 fps is long enough to be sure that is the situation rather than a
/// decoder still filling its pipeline.
const STARVED_BEFORE_KEYFRAME_REQUEST: u32 = 30;

/// Frames accepted without a picture coming back before the decoder is replaced.
///
/// Distinct from [`FAILURES_BEFORE_DECODER_REBUILD`], and far larger, because
/// this counts a decoder that is *working* — accepting every frame without
/// complaint — and the honest reading of that is "not enough input yet", not
/// "broken". Rebuilding on a low count is actively harmful: a fresh decoder has
/// to fill its pipeline and wait for the next keyframe all over again, so a
/// threshold below the warm-up cost turns a slow start into a stream that never
/// produces a single frame. That is precisely what a 15-frame limit did against
/// Media Foundation's default ~30-frame buffering, on every call, in both
/// directions.
///
/// Ten seconds of frames leaves room for several sender keyframe intervals and
/// still recovers a genuinely wedged transform — one that swallows input and
/// returns nothing forever — within a bounded time.
const STARVED_BEFORE_DECODER_REBUILD: u32 = 300;

/// Minimum gap between keyframe requests for the same peer.
///
/// The connection manager rate-limits these too, but throttling at the source
/// is what makes re-asking safe: without it a wedged decoder would queue a
/// request per failed frame, thirty times a second, for the manager to throw
/// away.
const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

/// Gap between keyframe requests once a stream has been quiet long enough to
/// report — see [`STALL_REPORT_AFTER`].
///
/// By then the fast retry has failed for ten seconds and is unlikely to start
/// working on the eleventh; an outage that lasts minutes should not cost a
/// signalling message a second for its whole duration. Recovery does not depend
/// on the request either way, since senders emit a keyframe every few seconds
/// regardless — this only shortens the wait when it works.
const STALLED_KEYFRAME_INTERVAL: Duration = Duration::from_secs(5);

/// How long a peer that was delivering frames may go quiet before we ask for a
/// keyframe.
///
/// A stream that simply stops — a relay reconnect, a shed burst, a frame the
/// group key could not open — produces no decode error at all, so nothing in
/// the failure path above ever fires. Asking here is cheap and is the only
/// prompt a sender gets that its frames are not landing.
const STALL_KEYFRAME_AFTER: Duration = Duration::from_secs(2);

/// How long a peer stays quiet before the UI is told the tile is stale.
///
/// Long enough that a bad few seconds of network does not flag every call, and
/// short enough that a user staring at a frozen picture is told why. The tile
/// keeps its last frame either way — this only distinguishes "still live" from
/// "this picture is old".
const STALL_REPORT_AFTER: Duration = Duration::from_secs(10);

/// How many peers may be decoded at once.
///
/// Decoding is the expensive part of receiving — a decoder holds reference
/// frames and, on the hardware path, GPU surfaces. Eight simultaneous 640×360
/// streams is enough to saturate a modest machine and starve the audio
/// pipeline, which is the failure users actually notice.
///
/// **This bounds CPU and memory, not bandwidth.** The supernode fans room video
/// to every member regardless of what we decode, so capping here does not
/// reduce what arrives on the wire. Cutting inbound bandwidth needs a
/// subscription protocol (a receiver telling the SFU which senders it wants),
/// which this version does not have.
pub const MAX_DECODED_STREAMS: usize = 4;

/// How long a decoded stream must go quiet before its slot can be taken.
///
/// Without a grace period a full slate would thrash: every frame from an
/// unadmitted peer would evict someone, so nine peers would decode nothing but
/// keyframes forever. Requiring real idleness means the cap converges on
/// whoever is actually streaming.
const STREAM_IDLE_EVICT: Duration = Duration::from_secs(3);

/// How long the decode thread parks waiting for the next frame before it
/// re-checks the stop flag and drains any pending forgets.
///
/// Short enough that leave/peer-left teardown feels immediate; long enough that
/// the loop is not a busy-spin when the room is quiet.
const IDLE_POLL: Duration = Duration::from_millis(50);

/// Commands that are not media frames. Separate from the frame queue so a full
/// decode backlog cannot block lifecycle cleanup (peer left, camera off).
enum Control {
    /// Drop the decoder + sink for one peer.
    Forget(String),
    /// Drop every decoder and clear every sink binding.
    ForgetAll,
}

/// Handle to the decode thread.
pub struct VideoReceiver {
    /// Shared with the caller so content-audio playout can advance anchors.
    playout: SharedPlayout,
    tx: mpsc::SyncSender<InboundFrame>,
    control_tx: mpsc::Sender<Control>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl VideoReceiver {
    /// Start the decode thread.
    ///
    /// `request_keyframe` is invoked (from the decode thread) with the peer id
    /// whose stream cannot be decoded, so the caller can send a keyframe
    /// request. Throttled here as well as by the caller — see
    /// [`KEYFRAME_REQUEST_INTERVAL`].
    ///
    /// `report_stall` is invoked with `(peer_id, stalled)` when a stream that
    /// was arriving goes quiet for [`STALL_REPORT_AFTER`], and again with
    /// `false` when frames resume or the peer is forgotten. Edges only: it is
    /// never called twice with the same state for one peer.
    ///
    /// `has_sink` answers "is anything displaying this peer?". Injected rather
    /// than called directly so the loop can be exercised without a Qt render
    /// surface; production passes [`sink::has_sink`].
    pub fn start<F, D, S>(
        mut make_decoder: F,
        mut request_keyframe: D,
        mut report_stall: S,
        has_sink: fn(&str) -> bool,
    ) -> Self
    where
        F: FnMut(VideoCodec) -> Option<Box<dyn VideoDecoder>> + Send + 'static,
        D: FnMut(&str) + Send + 'static,
        S: FnMut(&str, bool) + Send + 'static,
    {
        let (tx, rx) = mpsc::sync_channel::<InboundFrame>(QUEUE_DEPTH);
        let (control_tx, control_rx) = mpsc::channel::<Control>();
        let playout: SharedPlayout = Arc::new(Mutex::new(crate::media_sync::VideoPlayout::new()));
        let playout_t = Arc::clone(&playout);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);

        let handle = std::thread::Builder::new()
            .name("conquerd-video-decode".into())
            .spawn(move || {
                let mut decoders: HashMap<String, PeerDecoder> = HashMap::new();
                let mut health: HashMap<String, DecodeHealth> = HashMap::new();
                // When each decoded peer last delivered a frame, for the cap.
                let mut last_frame: HashMap<String, std::time::Instant> = HashMap::new();
                // When we last asked each peer for a keyframe, for the throttle.
                let mut keyframe_asked: HashMap<String, std::time::Instant> = HashMap::new();
                // Peers currently reported to the UI as stalled, so the report
                // fires on edges rather than once per sweep.
                let mut stalled: HashSet<String> = HashSet::new();
                info!("[video] decode thread started");

                while !stop_t.load(Ordering::Relaxed) {
                    // Lifecycle first: a peer who just left must not keep a
                    // decoder (and its HW surfaces) alive until the next frame
                    // arrives — which may never happen.
                    drain_control(
                        &control_rx,
                        &mut decoders,
                        &mut health,
                        &mut last_frame,
                        &mut keyframe_asked,
                        &mut stalled,
                        &mut report_stall,
                    );

                    // Runs on the idle tick as well as after every frame, which
                    // is the whole point: a stream that stopped produces no
                    // frames to hang the check off, so a loop that only looked
                    // at arrivals would never notice it went away.
                    sweep_stalls(
                        &last_frame,
                        &mut stalled,
                        &mut keyframe_asked,
                        &mut request_keyframe,
                        &mut report_stall,
                        has_sink,
                        std::time::Instant::now(),
                    );

                    let item = match rx.recv_timeout(IDLE_POLL) {
                        Ok(item) => item,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };

                    // Don't spend CPU decoding for a peer nobody is watching.
                    // Cheap to check and it is the common case in a large room.
                    if !has_sink(&item.peer_id) {
                        continue;
                    }

                    // Bound how many streams are decoded at once. Checked
                    // before the decoder is created, so a rejected peer costs
                    // nothing beyond the frame already received.
                    let now = std::time::Instant::now();
                    match admit(&item.peer_id, &last_frame, now) {
                        Admission::Decode => {}
                        Admission::Evict(stale) => {
                            debug!(
                                "[video] stream cap reached; dropping idle decoder for {}",
                                &stale[..8.min(stale.len())]
                            );
                            decoders.remove(&stale);
                            health.remove(&stale);
                            last_frame.remove(&stale);
                            keyframe_asked.remove(&stale);
                            // Clear the report with the state behind it: an
                            // evicted peer leaves `last_frame`, so the sweep
                            // would never revisit it and the tile would wear a
                            // stalled badge for the rest of the session.
                            if stalled.remove(&stale) {
                                report_stall(&stale, false);
                            }
                        }
                        Admission::Reject => continue,
                    }
                    last_frame.insert(item.peer_id.clone(), now);
                    // Arrival is the recovery signal, not a successful decode:
                    // the picture may still need a keyframe or a rebuilt
                    // decoder, but the stream itself is demonstrably back and
                    // both of those are handled below.
                    if stalled.remove(&item.peer_id) {
                        info!(
                            "[video] frames resumed from {}",
                            &item.peer_id[..8.min(item.peer_id.len())]
                        );
                        report_stall(&item.peer_id, false);
                    }

                    // A decoder built for a different codec cannot be reused:
                    // drop it so the entry below rebuilds. This is the path a
                    // mid-session renegotiation takes.
                    if decoders
                        .get(&item.peer_id)
                        .is_some_and(|d| d.codec != item.codec)
                    {
                        debug!(
                            "[video] {} switched codec to {}; rebuilding decoder",
                            &item.peer_id[..8.min(item.peer_id.len())],
                            item.codec.as_str()
                        );
                        decoders.remove(&item.peer_id);
                        health.remove(&item.peer_id);
                    }

                    let entry = match decoders.entry(item.peer_id.clone()) {
                        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::hash_map::Entry::Vacant(e) => {
                            let Some(d) = make_decoder(item.codec) else {
                                // Not a transient failure: this build has no
                                // decoder for this codec, so every frame from
                                // this sender will land here until they change
                                // codec. Warn with the codec named so the cause
                                // is diagnosable from a log alone.
                                warn!(
                                    "[video] no {} decoder in this build; dropping frame from {}",
                                    item.codec.as_str(),
                                    &item.peer_id[..8.min(item.peer_id.len())]
                                );
                                // Release the slot claimed above. Leaving it
                                // held would let peers we cannot decode crowd
                                // out peers we can.
                                last_frame.remove(&item.peer_id);
                                continue;
                            };
                            e.insert(PeerDecoder {
                                codec: item.codec,
                                decoder: d,
                            })
                        }
                    };

                    // Bound to a local so the `decoders` borrow ends here: the
                    // failure arm below has to be able to drop the very
                    // decoder this call came from.
                    let decoded = entry.decoder.decode(&item.encoded);
                    match decoded {
                        Ok(Some(frame)) => {
                            health.remove(&item.peer_id);
                            // Through the playout rather than straight to the
                            // sink: with content audio present this holds the
                            // frame until the audio timeline reaches it, and
                            // without it the playout returns the frame
                            // immediately, which is the free-run path.
                            let due = playout_t.lock().push(
                                &item.peer_id,
                                crate::media_sync::QueuedFrame {
                                    pts_us: item.pts_us,
                                    payload: frame,
                                },
                                std::time::Instant::now(),
                            );
                            for f in due {
                                sink::push_frame(&item.peer_id, &f.payload);
                            }
                        }
                        // Accepted, but nothing to draw yet: warming up, or
                        // waiting for the keyframe that lets it start. Neither
                        // is an error, so the failure counters above stay
                        // untouched — this has its own, much longer leash.
                        Ok(None) => {
                            let n = {
                                let h = health.entry(item.peer_id.clone()).or_default();
                                h.errors = 0;
                                h.starved += 1;
                                h.starved
                            };
                            if n >= STARVED_BEFORE_KEYFRAME_REQUEST
                                && request_keyframe_throttled(
                                    &mut request_keyframe,
                                    &mut keyframe_asked,
                                    &item.peer_id,
                                    now,
                                    KEYFRAME_REQUEST_INTERVAL,
                                )
                            {
                                debug!(
                                    "[video] {n} frames from {} with no picture out; requesting keyframe",
                                    &item.peer_id[..8.min(item.peer_id.len())]
                                );
                            }
                            if n >= STARVED_BEFORE_DECODER_REBUILD {
                                // See STARVED_BEFORE_DECODER_REBUILD: ten
                                // seconds of accepted frames and not one
                                // picture is no longer a warm-up.
                                warn!(
                                    "[video] rebuilding {} decoder for {}: {n} frames accepted, none decoded",
                                    item.codec.as_str(),
                                    &item.peer_id[..8.min(item.peer_id.len())]
                                );
                                decoders.remove(&item.peer_id);
                                health.remove(&item.peer_id);
                            }
                        }
                        Err(e) => {
                            let n = {
                                let h = health.entry(item.peer_id.clone()).or_default();
                                h.starved = 0;
                                h.errors += 1;
                                h.errors
                            };
                            if n >= FAILURES_BEFORE_KEYFRAME_REQUEST {
                                // Past the threshold, not merely at it, and
                                // throttled rather than fired once: a single
                                // request can itself be lost, and a receiver
                                // that asked exactly once then gave up stays
                                // frozen until the sender's periodic keyframe
                                // comes round — or forever, when the stream is
                                // already past what a keyframe can fix.
                                if request_keyframe_throttled(
                                    &mut request_keyframe,
                                    &mut keyframe_asked,
                                    &item.peer_id,
                                    now,
                                    KEYFRAME_REQUEST_INTERVAL,
                                ) {
                                    debug!(
                                        "[video] {n} decode failures from {}; requesting keyframe: {e}",
                                        &item.peer_id[..8.min(item.peer_id.len())]
                                    );
                                }
                            }
                            if n >= FAILURES_BEFORE_DECODER_REBUILD {
                                // See FAILURES_BEFORE_DECODER_REBUILD: at this
                                // point the decoder, not the stream, is what is
                                // broken. Dropping both entries makes the next
                                // frame build a fresh decoder and start the
                                // count over.
                                warn!(
                                    "[video] rebuilding {} decoder for {} after {n} failures: {e}",
                                    item.codec.as_str(),
                                    &item.peer_id[..8.min(item.peer_id.len())]
                                );
                                decoders.remove(&item.peer_id);
                                health.remove(&item.peer_id);
                            }
                        }
                    }
                }
                // Final drain so a forget issued during shutdown still lands.
                drain_control(
                    &control_rx,
                    &mut decoders,
                    &mut health,
                    &mut last_frame,
                    &mut keyframe_asked,
                    &mut stalled,
                    &mut report_stall,
                );
                decoders.clear();
                health.clear();
                last_frame.clear();
                keyframe_asked.clear();
                stalled.clear();
                info!("[video] decode thread stopped");
            })
            .ok();

        Self {
            playout,
            tx,
            control_tx,
            stop,
            handle,
        }
    }

    /// Queue an encoded frame for decoding.
    ///
    /// Never blocks: a full queue means the decoder is behind, and the newest
    /// frame is worth less than the latency that waiting would add.
    pub fn submit(
        &self,
        peer_id: &str,
        encoded: Vec<u8>,
        keyframe: bool,
        codec: VideoCodec,
        pts_us: u64,
    ) {
        let item = InboundFrame {
            peer_id: peer_id.to_owned(),
            encoded,
            keyframe,
            codec,
            pts_us,
        };
        match self.tx.try_send(item) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                debug!("[video] decode queue full; dropping frame");
            }
            Err(TrySendError::Disconnected(_)) => {
                debug!("[video] decode thread gone; dropping frame");
            }
        }
    }

    /// Forget a peer's decoder and sink bindings, e.g. when they leave the room
    /// or turn their camera off.
    ///
    /// The sink is cleared immediately so the tile blanks without waiting for
    /// the decode thread; the decoder is dropped on that thread so COM/MF
    /// objects stay on the apartment that created them.
    /// Handle to the shared hold/drop state.
    ///
    /// The content-audio playout calls `note_audio_played` on this each time it
    /// plays a frame — including concealed ones — which is what advances the
    /// timeline video is held against.
    pub fn playout(&self) -> SharedPlayout {
        Arc::clone(&self.playout)
    }

    pub fn forget(&self, peer_id: &str) {
        // Immediate UI blank — do not wait for the decode thread.
        sink::clear_peer(peer_id);
        self.playout.lock().forget(peer_id);
        let _ = self.control_tx.send(Control::Forget(peer_id.to_owned()));
    }

    /// Drop every decoder and clear every sink binding.
    ///
    /// Used on leave-room / end-call so a subsequent join starts clean rather
    /// than reusing stale decoder state from a prior session.
    pub fn forget_all(&self) {
        let _ = self.control_tx.send(Control::ForgetAll);
    }
}

/// What to do with a frame from `peer` given who is currently being decoded.
#[derive(Debug, PartialEq, Eq)]
enum Admission {
    /// Decode it; `peer` already holds a slot or one was free.
    Decode,
    /// Decode it after dropping this peer's decoder, whose stream went quiet.
    Evict(String),
    /// Drop the frame — the slate is full of active streams.
    Reject,
}

/// Decide whether a frame may be decoded.
///
/// Pure so the policy can be tested without a decode thread. `active` maps a
/// peer to when it last delivered a frame.
fn admit(
    peer: &str,
    active: &HashMap<String, std::time::Instant>,
    now: std::time::Instant,
) -> Admission {
    // Already decoding this peer: never re-evaluate. Dropping a live stream
    // mid-flight would cost a keyframe to recover for no benefit.
    if active.contains_key(peer) {
        return Admission::Decode;
    }
    if active.len() < MAX_DECODED_STREAMS {
        return Admission::Decode;
    }
    // Full. Take the slot only from a stream that has genuinely gone quiet,
    // and only the quietest one.
    let stalest = active
        .iter()
        .max_by_key(|(_, seen)| now.saturating_duration_since(**seen))
        .filter(|(_, seen)| now.saturating_duration_since(**seen) >= STREAM_IDLE_EVICT);
    match stalest {
        Some((id, _)) => Admission::Evict(id.clone()),
        None => Admission::Reject,
    }
}

/// Ask `peer` for a keyframe unless one was asked for too recently.
///
/// Returns whether the request actually went out, so callers can log the
/// request rather than the intent.
fn request_keyframe_throttled(
    request: &mut impl FnMut(&str),
    asked: &mut HashMap<String, std::time::Instant>,
    peer: &str,
    now: std::time::Instant,
    interval: Duration,
) -> bool {
    let due = asked
        .get(peer)
        .is_none_or(|t| now.saturating_duration_since(*t) >= interval);
    if !due {
        return false;
    }
    asked.insert(peer.to_owned(), now);
    request(peer);
    true
}

/// Notice streams that have gone quiet, ask for a keyframe, and eventually tell
/// the UI the picture it is showing is stale.
///
/// Only peers something is actually displaying are chased: a tile that was
/// closed mid-stream leaves its entry behind (nothing forgets a peer merely for
/// being off screen), and pestering a sender for keyframes nobody will draw is
/// pure cost.
///
/// Pure enough to test without a decode thread — it touches only the maps
/// handed to it and the callbacks.
fn sweep_stalls(
    last_frame: &HashMap<String, std::time::Instant>,
    stalled: &mut HashSet<String>,
    keyframe_asked: &mut HashMap<String, std::time::Instant>,
    request_keyframe: &mut impl FnMut(&str),
    report_stall: &mut impl FnMut(&str, bool),
    has_sink: fn(&str) -> bool,
    now: std::time::Instant,
) {
    for (peer, seen) in last_frame {
        if !has_sink(peer) {
            continue;
        }
        let quiet = now.saturating_duration_since(*seen);
        if quiet < STALL_KEYFRAME_AFTER {
            continue;
        }
        // A stream that stopped arriving never fails to decode, so this is the
        // only place a keyframe gets asked for on its behalf. Chased hard for
        // the first few seconds, then slowly — see STALLED_KEYFRAME_INTERVAL.
        let interval = if quiet >= STALL_REPORT_AFTER {
            STALLED_KEYFRAME_INTERVAL
        } else {
            KEYFRAME_REQUEST_INTERVAL
        };
        request_keyframe_throttled(request_keyframe, keyframe_asked, peer, now, interval);

        if quiet >= STALL_REPORT_AFTER && stalled.insert(peer.clone()) {
            warn!(
                "[video] no frames from {} for {}s; marking the tile stale",
                &peer[..8.min(peer.len())],
                quiet.as_secs()
            );
            report_stall(peer, true);
        }
    }
}

fn drain_control(
    control_rx: &mpsc::Receiver<Control>,
    decoders: &mut HashMap<String, PeerDecoder>,
    health: &mut HashMap<String, DecodeHealth>,
    last_frame: &mut HashMap<String, std::time::Instant>,
    keyframe_asked: &mut HashMap<String, std::time::Instant>,
    stalled: &mut HashSet<String>,
    report_stall: &mut impl FnMut(&str, bool),
) {
    while let Ok(cmd) = control_rx.try_recv() {
        match cmd {
            Control::Forget(id) => {
                decoders.remove(&id);
                health.remove(&id);
                // Free the stream slot too, or a peer who left would keep
                // holding it against the cap until the idle timer expired.
                last_frame.remove(&id);
                keyframe_asked.remove(&id);
                // A forgotten peer leaves `last_frame`, so the sweep can never
                // retract the report itself; leaving it set would badge the
                // tile of whoever occupies that id next.
                if stalled.remove(&id) {
                    report_stall(&id, false);
                }
                sink::clear_peer(&id);
            }
            Control::ForgetAll => {
                decoders.clear();
                health.clear();
                last_frame.clear();
                keyframe_asked.clear();
                for id in stalled.drain() {
                    report_stall(&id, false);
                }
                // Blank every tile, including peers that never produced a
                // decoder (e.g. indicator-only, or frames still in reassembly).
                sink::clear_all();
            }
        }
    }
}

impl Drop for VideoReceiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Dropping the frame sender unblocks a parked `recv_timeout` once the
        // idle poll expires; without closing it the join below would wait for
        // a frame that will never arrive only after IDLE_POLL — still fine,
        // but closing is snappier.
        let (dead, _) = mpsc::sync_channel::<InboundFrame>(1);
        let _ = std::mem::replace(&mut self.tx, dead);
        // Drop control sender so the thread's final drain sees Disconnected.
        let (dead_ctrl, _) = mpsc::channel::<Control>();
        let _ = std::mem::replace(&mut self.control_tx, dead_ctrl);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::frame::RawFrame;

    /// Test decoder: counts submissions, and lets a frame's first byte pick
    /// which of the three outcomes it produces.
    ///
    /// `0xFF` fails, `0xNO` (see [`NO_PICTURE`]) is accepted without a picture,
    /// anything else decodes. The middle case is the one worth having a marker
    /// for: it is what a real decoder returns while it warms up or waits for a
    /// keyframe, and treating it as a failure is what wedged the stream.
    struct CountingDecoder(Arc<std::sync::atomic::AtomicU32>);

    /// First byte marking a frame the test decoder accepts without decoding.
    const NO_PICTURE: u8 = 0xFE;

    impl VideoDecoder for CountingDecoder {
        fn decode(&mut self, encoded: &[u8]) -> anyhow::Result<Option<RawFrame>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if encoded.first() == Some(&0xFF) {
                anyhow::bail!("simulated decode failure");
            }
            if encoded.first() == Some(&NO_PICTURE) {
                return Ok(None);
            }
            Ok(Some(RawFrame::black(16, 16)))
        }
    }

    /// Sink probe for tests: everyone is on screen.
    ///
    /// A test build has no Qt render surface, so the real `sink::has_sink`
    /// answers "nobody is watching" for every peer — with it, the decode loop
    /// would skip every frame and none of these tests would exercise anything.
    fn watched(_peer: &str) -> bool {
        true
    }

    /// The opposite: nothing is displayed, which is what a closed tile looks
    /// like to the sweep.
    fn unwatched(_peer: &str) -> bool {
        false
    }

    #[test]
    fn submitting_to_a_full_queue_does_not_block() {
        // The property that matters: a wedged decoder must never back-pressure
        // into the network thread.
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = Arc::clone(&calls);
        let rx = VideoReceiver::start(
            move |_codec| Some(Box::new(CountingDecoder(Arc::clone(&c)))),
            |_| {},
            |_, _| {},
            watched,
        );

        let started = std::time::Instant::now();
        for _ in 0..(QUEUE_DEPTH * 4) {
            rx.submit("peer", vec![1, 2, 3], false, VideoCodec::Stub, 0);
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "submit blocked; it must shed instead"
        );
    }

    /// The playout handle must be the *same* state the decode thread uses, or
    /// anchors set by the audio side would steer a queue nobody reads and video
    /// would free-run forever while looking correctly wired.
    #[test]
    fn the_exposed_playout_is_the_one_the_decode_thread_uses() {
        let rx = VideoReceiver::start(
            |_codec| {
                Some(Box::new(CountingDecoder(Arc::new(
                    std::sync::atomic::AtomicU32::new(0),
                ))))
            },
            |_| {},
            |_, _| {},
            watched,
        );

        let handle = rx.playout();
        handle
            .lock()
            .note_audio_played("peer", 1_000_000, std::time::Instant::now());
        // Reading through a second handle must observe the same anchor.
        assert!(rx
            .playout()
            .lock()
            .audio_now_us("peer", std::time::Instant::now())
            .is_some());
    }

    /// Forgetting a peer must clear their sync state too. Leaving an anchor
    /// behind would steer the next stream from that peer against a timeline
    /// belonging to the previous one.
    #[test]
    fn forget_clears_sync_state_as_well_as_the_decoder() {
        let rx = VideoReceiver::start(
            |_codec| {
                Some(Box::new(CountingDecoder(Arc::new(
                    std::sync::atomic::AtomicU32::new(0),
                ))))
            },
            |_| {},
            |_, _| {},
            watched,
        );
        let now = std::time::Instant::now();
        rx.playout()
            .lock()
            .note_audio_played("peer", 1_000_000, now);
        assert!(rx.playout().lock().audio_now_us("peer", now).is_some());

        rx.forget("peer");
        assert!(
            rx.playout().lock().audio_now_us("peer", now).is_none(),
            "a stale anchor survived the peer being forgotten"
        );
    }

    #[test]
    fn receiver_shuts_down_cleanly() {
        // Drop must not hang: the thread parks on recv_timeout, so Drop has to
        // close the channel and set stop before joining.
        let rx = VideoReceiver::start(
            |_codec| {
                Some(Box::new(CountingDecoder(Arc::new(
                    std::sync::atomic::AtomicU32::new(0),
                ))))
            },
            |_| {},
            |_, _| {},
            watched,
        );
        rx.submit("peer", vec![1], false, VideoCodec::Stub, 0);
        let started = std::time::Instant::now();
        drop(rx);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "Drop hung waiting for the decode thread"
        );
    }

    // ── Inbound stream cap ──────────────────────────────────────────────────

    /// Build an `active` map where each peer last delivered `age` ago.
    fn active_since(
        now: std::time::Instant,
        ages: &[(&str, Duration)],
    ) -> HashMap<String, std::time::Instant> {
        ages.iter()
            .map(|(id, age)| ((*id).to_owned(), now - *age))
            .collect()
    }

    #[test]
    fn peers_are_admitted_until_the_cap_is_reached() {
        let now = std::time::Instant::now();
        let mut active = HashMap::new();
        for i in 0..MAX_DECODED_STREAMS {
            let id = format!("peer-{i}");
            assert_eq!(admit(&id, &active, now), Admission::Decode);
            active.insert(id, now);
        }
        assert_eq!(active.len(), MAX_DECODED_STREAMS);
        assert_eq!(admit("one-too-many", &active, now), Admission::Reject);
    }

    /// The property that keeps the cap from being worse than no cap: a full
    /// slate of live streams must reject newcomers outright rather than
    /// evicting someone, which would thrash every stream into constant
    /// keyframe recovery.
    #[test]
    fn a_full_slate_of_active_streams_rejects_rather_than_thrashing() {
        let now = std::time::Instant::now();
        let active = active_since(
            now,
            &[
                ("a", Duration::from_millis(10)),
                ("b", Duration::from_millis(20)),
                ("c", Duration::from_millis(30)),
                ("d", Duration::from_millis(40)),
            ],
        );
        assert_eq!(active.len(), MAX_DECODED_STREAMS, "precondition");
        assert_eq!(admit("e", &active, now), Admission::Reject);
    }

    #[test]
    fn a_stream_that_went_quiet_yields_its_slot_to_a_newcomer() {
        let now = std::time::Instant::now();
        let active = active_since(
            now,
            &[
                ("a", Duration::from_millis(10)),
                ("b", STREAM_IDLE_EVICT + Duration::from_secs(1)),
                ("c", Duration::from_millis(30)),
                ("d", Duration::from_millis(40)),
            ],
        );
        assert_eq!(admit("e", &active, now), Admission::Evict("b".into()));
    }

    /// When several are idle, the *stalest* loses its slot — evicting a
    /// merely-lagging stream over a long-dead one would drop the wrong picture.
    #[test]
    fn the_stalest_stream_is_the_one_evicted() {
        let now = std::time::Instant::now();
        let active = active_since(
            now,
            &[
                ("a", STREAM_IDLE_EVICT + Duration::from_secs(1)),
                ("b", STREAM_IDLE_EVICT + Duration::from_secs(9)),
                ("c", STREAM_IDLE_EVICT + Duration::from_secs(4)),
                ("d", Duration::from_millis(5)),
            ],
        );
        assert_eq!(admit("e", &active, now), Admission::Evict("b".into()));
    }

    /// An already-decoding peer must never be re-evaluated: dropping a live
    /// decoder mid-stream costs a keyframe to recover, for nothing.
    #[test]
    fn an_established_stream_is_never_displaced_by_its_own_frames() {
        let now = std::time::Instant::now();
        let active = active_since(
            now,
            &[
                ("a", Duration::from_millis(10)),
                ("b", Duration::from_millis(20)),
                ("c", Duration::from_millis(30)),
                ("d", STREAM_IDLE_EVICT + Duration::from_secs(5)),
            ],
        );
        // Even though `d` is evictable, its own frame must be decoded, not
        // treated as a newcomer competing for a slot.
        assert_eq!(admit("d", &active, now), Admission::Decode);
    }

    // ── Recovery from a wedged decoder ──────────────────────────────────────

    /// The failure this exists for: a Media Foundation transform that stops
    /// requesting input fails *every* subsequent call, so a receiver that only
    /// ever asks for keyframes shows a frozen tile until the user leaves the
    /// room. The decoder must be thrown away and rebuilt.
    #[test]
    fn a_permanently_failing_decoder_is_rebuilt() {
        let built = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let b = Arc::clone(&built);
        let rx = VideoReceiver::start(
            move |_codec| {
                b.fetch_add(1, Ordering::SeqCst);
                Some(Box::new(CountingDecoder(Arc::new(
                    std::sync::atomic::AtomicU32::new(0),
                ))))
            },
            |_| {},
            |_, _| {},
            watched,
        );

        // 0xFF makes CountingDecoder fail, so every frame wedges it.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while built.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            rx.submit("peer", vec![0xFF], false, VideoCodec::Stub, 0);
            // Paced under the queue depth so frames are not simply shed: the
            // rebuild is counted in decode *attempts*, not submissions.
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            built.load(Ordering::SeqCst) >= 2,
            "decoder was never rebuilt after sustained failure"
        );
    }

    /// The bug this guards, which shipped and broke every call: a decoder that
    /// accepts frames without producing a picture yet is *warming up*, not
    /// broken. Media Foundation's H.264 decoder buffers around thirty
    /// submissions before its first output by default, so a rebuild at fifteen
    /// destroyed it mid-warm-up, every time, and the replacement started the
    /// same wait over — video that never produced a single frame while frames
    /// arrived at full rate.
    ///
    /// Well past `FAILURES_BEFORE_DECODER_REBUILD` on purpose: the point is
    /// that the *error* threshold must not govern this path at all.
    #[test]
    fn a_decoder_that_is_only_warming_up_is_not_rebuilt() {
        let built = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let decoded = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let b = Arc::clone(&built);
        let d = Arc::clone(&decoded);
        let rx = VideoReceiver::start(
            move |_codec| {
                b.fetch_add(1, Ordering::SeqCst);
                Some(Box::new(CountingDecoder(Arc::clone(&d))))
            },
            |_| {},
            |_, _| {},
            watched,
        );

        let submissions = (FAILURES_BEFORE_DECODER_REBUILD * 4) as usize;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while (decoded.load(Ordering::SeqCst) as usize) < submissions
            && std::time::Instant::now() < deadline
        {
            rx.submit("peer", vec![NO_PICTURE], false, VideoCodec::Stub, 0);
            // Paced under the queue depth, as above, so these are decode
            // attempts rather than frames shed before they reach the decoder.
            std::thread::sleep(Duration::from_millis(2));
        }

        assert!(
            decoded.load(Ordering::SeqCst) as usize >= submissions,
            "the decode loop never got through the submissions"
        );
        assert_eq!(
            built.load(Ordering::SeqCst),
            1,
            "a decoder that is merely waiting for a keyframe was thrown away"
        );
    }

    /// Asking once and giving up is what left a frozen tile in the first place:
    /// the request itself can be lost. It must repeat — but not per frame.
    #[test]
    fn keyframe_requests_repeat_but_are_throttled() {
        let mut asked: HashMap<String, std::time::Instant> = HashMap::new();
        let mut fired = 0u32;
        let mut request = |_: &str| fired += 1;
        let t0 = std::time::Instant::now();

        assert!(request_keyframe_throttled(
            &mut request,
            &mut asked,
            "alice",
            t0,
            KEYFRAME_REQUEST_INTERVAL
        ));
        // Everything inside the interval is swallowed.
        for ms in [1, 10, 100, 999] {
            assert!(!request_keyframe_throttled(
                &mut request,
                &mut asked,
                "alice",
                t0 + Duration::from_millis(ms),
                KEYFRAME_REQUEST_INTERVAL
            ));
        }
        // Past it, we ask again.
        assert!(request_keyframe_throttled(
            &mut request,
            &mut asked,
            "alice",
            t0 + KEYFRAME_REQUEST_INTERVAL,
            KEYFRAME_REQUEST_INTERVAL
        ));
        assert_eq!(fired, 2);
    }

    /// One peer's throttle must not silence another's — they fail independently.
    #[test]
    fn the_keyframe_throttle_is_per_peer() {
        let mut asked: HashMap<String, std::time::Instant> = HashMap::new();
        let mut request = |_: &str| {};
        let now = std::time::Instant::now();
        assert!(request_keyframe_throttled(
            &mut request,
            &mut asked,
            "alice",
            now,
            KEYFRAME_REQUEST_INTERVAL
        ));
        assert!(request_keyframe_throttled(
            &mut request,
            &mut asked,
            "bob",
            now,
            KEYFRAME_REQUEST_INTERVAL
        ));
    }

    // ── Stall watchdog ──────────────────────────────────────────────────────

    fn seen_at(
        now: std::time::Instant,
        ages: &[(&str, Duration)],
    ) -> HashMap<String, std::time::Instant> {
        ages.iter()
            .map(|(id, age)| ((*id).to_owned(), now - *age))
            .collect()
    }

    /// A stream that simply stops produces no decode error at all, so nothing
    /// in the failure path fires. The sweep is the only thing that notices.
    #[test]
    fn a_quiet_stream_is_chased_with_a_keyframe_then_reported() {
        let now = std::time::Instant::now();
        let mut stalled = HashSet::new();
        let mut asked = HashMap::new();
        let mut requested: Vec<String> = Vec::new();
        let mut reports: Vec<(String, bool)> = Vec::new();

        // Quiet long enough to chase, not long enough to report.
        let last = seen_at(now, &[("alice", STALL_KEYFRAME_AFTER)]);
        sweep_stalls(
            &last,
            &mut stalled,
            &mut asked,
            &mut |p: &str| requested.push(p.to_owned()),
            &mut |p: &str, s: bool| reports.push((p.to_owned(), s)),
            watched,
            now,
        );
        assert_eq!(requested, vec!["alice".to_owned()]);
        assert!(reports.is_empty(), "reported stale far too early");

        // Long enough to report.
        let last = seen_at(now, &[("alice", STALL_REPORT_AFTER)]);
        sweep_stalls(
            &last,
            &mut stalled,
            &mut asked,
            &mut |p: &str| requested.push(p.to_owned()),
            &mut |p: &str, s: bool| reports.push((p.to_owned(), s)),
            watched,
            now,
        );
        assert_eq!(reports, vec![("alice".to_owned(), true)]);
    }

    /// A live stream must never be flagged — the whole feature is worthless if
    /// it cries wolf during a normal call.
    #[test]
    fn a_live_stream_is_left_alone() {
        let now = std::time::Instant::now();
        let last = seen_at(now, &[("alice", Duration::from_millis(33))]);
        let mut stalled = HashSet::new();
        let mut asked = HashMap::new();
        let mut requests = 0u32;
        let mut reports = 0u32;
        sweep_stalls(
            &last,
            &mut stalled,
            &mut asked,
            &mut |_: &str| requests += 1,
            &mut |_: &str, _: bool| reports += 1,
            watched,
            now,
        );
        assert_eq!((requests, reports), (0, 0));
        assert!(stalled.is_empty());
    }

    /// Reported on the edge only: a sweep every 50 ms must not re-signal the
    /// same stall two hundred times before anything changes.
    #[test]
    fn a_continuing_stall_is_reported_once() {
        let now = std::time::Instant::now();
        let last = seen_at(now, &[("alice", STALL_REPORT_AFTER * 3)]);
        let mut stalled = HashSet::new();
        let mut asked = HashMap::new();
        let mut reports = 0u32;
        for tick in 0..10u32 {
            sweep_stalls(
                &last,
                &mut stalled,
                &mut asked,
                &mut |_: &str| {},
                &mut |_: &str, _: bool| reports += 1,
                watched,
                now + Duration::from_millis(50 * u64::from(tick)),
            );
        }
        assert_eq!(reports, 1, "the stall was re-reported on every sweep");
    }

    /// Past the reporting threshold the chase slows down: a multi-minute
    /// outage must not cost a signalling message a second for its duration.
    #[test]
    fn a_long_outage_is_chased_slowly() {
        let t0 = std::time::Instant::now();
        let last = seen_at(t0, &[("alice", STALL_REPORT_AFTER)]);
        let mut stalled = HashSet::new();
        let mut asked = HashMap::new();
        let mut requests = 0u32;
        // One sweep a second for four seconds — all inside the slow interval.
        for sec in 0..4u64 {
            sweep_stalls(
                &last,
                &mut stalled,
                &mut asked,
                &mut |_: &str| requests += 1,
                &mut |_: &str, _: bool| {},
                watched,
                t0 + Duration::from_secs(sec),
            );
        }
        assert_eq!(
            requests, 1,
            "the backed-off chase fired again inside its own interval"
        );
    }

    /// A tile the user closed leaves its entry behind — nothing forgets a peer
    /// merely for being off screen. Chasing keyframes for a picture nobody is
    /// drawing costs the sender bitrate for nothing.
    #[test]
    fn a_stream_nobody_is_watching_is_not_chased() {
        let now = std::time::Instant::now();
        let last = seen_at(now, &[("alice", STALL_REPORT_AFTER * 2)]);
        let mut stalled = HashSet::new();
        let mut asked = HashMap::new();
        let mut requests = 0u32;
        let mut reports = 0u32;
        sweep_stalls(
            &last,
            &mut stalled,
            &mut asked,
            &mut |_: &str| requests += 1,
            &mut |_: &str, _: bool| reports += 1,
            unwatched,
            now,
        );
        assert_eq!((requests, reports), (0, 0));
    }

    /// Forgetting a stalled peer must retract the report: they leave
    /// `last_frame`, so the sweep can never do it, and a stale badge would
    /// follow the id into its next session.
    #[test]
    fn forgetting_a_stalled_peer_retracts_the_report() {
        let (tx, rx) = mpsc::channel::<Control>();
        let mut decoders = HashMap::new();
        let mut health = HashMap::new();
        let mut last_frame = HashMap::new();
        let mut asked = HashMap::new();
        let mut stalled: HashSet<String> = HashSet::new();
        stalled.insert("alice".to_owned());
        stalled.insert("bob".to_owned());
        let mut reports: Vec<(String, bool)> = Vec::new();

        tx.send(Control::Forget("alice".to_owned())).unwrap();
        drain_control(
            &rx,
            &mut decoders,
            &mut health,
            &mut last_frame,
            &mut asked,
            &mut stalled,
            &mut |p: &str, s: bool| reports.push((p.to_owned(), s)),
        );
        assert_eq!(reports, vec![("alice".to_owned(), false)]);
        assert!(stalled.contains("bob"), "bob's stall was cleared too");

        // Leaving the room clears everyone.
        tx.send(Control::ForgetAll).unwrap();
        drain_control(
            &rx,
            &mut decoders,
            &mut health,
            &mut last_frame,
            &mut asked,
            &mut stalled,
            &mut |p: &str, s: bool| reports.push((p.to_owned(), s)),
        );
        assert!(stalled.is_empty());
        assert!(reports.contains(&("bob".to_owned(), false)));
    }

    #[test]
    fn forget_does_not_block_on_full_frame_queue() {
        let rx = VideoReceiver::start(
            |_codec| {
                Some(Box::new(CountingDecoder(Arc::new(
                    std::sync::atomic::AtomicU32::new(0),
                ))))
            },
            |_| {},
            |_, _| {},
            watched,
        );
        // Fill the frame queue so a coupled control path would block.
        for _ in 0..(QUEUE_DEPTH * 2) {
            rx.submit("peer-a", vec![1], false, VideoCodec::Stub, 0);
        }
        let started = std::time::Instant::now();
        rx.forget("peer-a");
        rx.forget_all();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "forget must not wait on the frame queue"
        );
    }
}
