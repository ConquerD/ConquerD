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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use conquerd_features::video_codec::VideoCodec;
use tracing::{debug, info, warn};

use super::codec::VideoDecoder;
use super::sink;

/// One inbound encoded frame awaiting decode.
pub struct InboundFrame {
    pub peer_id: String,
    pub encoded: Vec<u8>,
    pub keyframe: bool,
    /// Codec these bytes are in, from the frame's signed header.
    pub codec: VideoCodec,
}

/// A peer's decoder together with the codec it was built for, so a codec change
/// is detectable rather than silently feeding bytes to the wrong decoder.
struct PeerDecoder {
    codec: VideoCodec,
    decoder: Box<dyn VideoDecoder>,
}

/// How many undecoded frames to queue before shedding.
///
/// Small on purpose: video is real-time, so a backlog is worth less than the
/// latency it adds. Dropping and waiting for the next keyframe beats playing
/// half a second late.
const QUEUE_DEPTH: usize = 8;

/// Consecutive decode failures from one peer before asking for a keyframe.
const FAILURES_BEFORE_KEYFRAME_REQUEST: u32 = 3;

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
    /// request. It is rate-limited by the caller, not here.
    pub fn start<F, D>(mut make_decoder: F, mut request_keyframe: D) -> Self
    where
        F: FnMut(VideoCodec) -> Option<Box<dyn VideoDecoder>> + Send + 'static,
        D: FnMut(&str) + Send + 'static,
    {
        let (tx, rx) = mpsc::sync_channel::<InboundFrame>(QUEUE_DEPTH);
        let (control_tx, control_rx) = mpsc::channel::<Control>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);

        let handle = std::thread::Builder::new()
            .name("conquerd-video-decode".into())
            .spawn(move || {
                let mut decoders: HashMap<String, PeerDecoder> = HashMap::new();
                let mut failures: HashMap<String, u32> = HashMap::new();
                // When each decoded peer last delivered a frame, for the cap.
                let mut last_frame: HashMap<String, std::time::Instant> = HashMap::new();
                info!("[video] decode thread started");

                while !stop_t.load(Ordering::Relaxed) {
                    // Lifecycle first: a peer who just left must not keep a
                    // decoder (and its HW surfaces) alive until the next frame
                    // arrives — which may never happen.
                    drain_control(&control_rx, &mut decoders, &mut failures, &mut last_frame);

                    let item = match rx.recv_timeout(IDLE_POLL) {
                        Ok(item) => item,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };

                    // Don't spend CPU decoding for a peer nobody is watching.
                    // Cheap to check and it is the common case in a large room.
                    if !sink::has_sink(&item.peer_id) {
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
                            failures.remove(&stale);
                            last_frame.remove(&stale);
                        }
                        Admission::Reject => continue,
                    }
                    last_frame.insert(item.peer_id.clone(), now);

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
                        failures.remove(&item.peer_id);
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

                    match entry.decoder.decode(&item.encoded) {
                        Ok(frame) => {
                            failures.remove(&item.peer_id);
                            sink::push_frame(&item.peer_id, &frame);
                        }
                        Err(e) => {
                            let n = failures.entry(item.peer_id.clone()).or_insert(0);
                            *n += 1;
                            if *n == FAILURES_BEFORE_KEYFRAME_REQUEST {
                                // Exactly at the threshold, not past it: the
                                // caller rate-limits, but not re-firing every
                                // frame keeps the request path quiet.
                                debug!(
                                    "[video] {} decode failures from {}; requesting keyframe: {e}",
                                    n,
                                    &item.peer_id[..8.min(item.peer_id.len())]
                                );
                                request_keyframe(&item.peer_id);
                            }
                        }
                    }
                }
                // Final drain so a forget issued during shutdown still lands.
                drain_control(&control_rx, &mut decoders, &mut failures, &mut last_frame);
                decoders.clear();
                failures.clear();
                last_frame.clear();
                info!("[video] decode thread stopped");
            })
            .ok();

        Self {
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
    pub fn submit(&self, peer_id: &str, encoded: Vec<u8>, keyframe: bool, codec: VideoCodec) {
        let item = InboundFrame {
            peer_id: peer_id.to_owned(),
            encoded,
            keyframe,
            codec,
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
    pub fn forget(&self, peer_id: &str) {
        // Immediate UI blank — do not wait for the decode thread.
        sink::clear_peer(peer_id);
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

fn drain_control(
    control_rx: &mpsc::Receiver<Control>,
    decoders: &mut HashMap<String, PeerDecoder>,
    failures: &mut HashMap<String, u32>,
    last_frame: &mut HashMap<String, std::time::Instant>,
) {
    while let Ok(cmd) = control_rx.try_recv() {
        match cmd {
            Control::Forget(id) => {
                decoders.remove(&id);
                failures.remove(&id);
                // Free the stream slot too, or a peer who left would keep
                // holding it against the cap until the idle timer expired.
                last_frame.remove(&id);
                sink::clear_peer(&id);
            }
            Control::ForgetAll => {
                decoders.clear();
                failures.clear();
                last_frame.clear();
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

    struct CountingDecoder(Arc<std::sync::atomic::AtomicU32>);

    impl VideoDecoder for CountingDecoder {
        fn decode(&mut self, encoded: &[u8]) -> anyhow::Result<RawFrame> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if encoded.first() == Some(&0xFF) {
                anyhow::bail!("simulated decode failure");
            }
            Ok(RawFrame::black(16, 16))
        }
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
        );

        let started = std::time::Instant::now();
        for _ in 0..(QUEUE_DEPTH * 4) {
            rx.submit("peer", vec![1, 2, 3], false, VideoCodec::Stub);
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "submit blocked; it must shed instead"
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
        );
        rx.submit("peer", vec![1], false, VideoCodec::Stub);
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

    #[test]
    fn forget_does_not_block_on_full_frame_queue() {
        let rx = VideoReceiver::start(
            |_codec| {
                Some(Box::new(CountingDecoder(Arc::new(
                    std::sync::atomic::AtomicU32::new(0),
                ))))
            },
            |_| {},
        );
        // Fill the frame queue so a coupled control path would block.
        for _ in 0..(QUEUE_DEPTH * 2) {
            rx.submit("peer-a", vec![1], false, VideoCodec::Stub);
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
