//! File-transfer manager.
//!
//! Chunk encoding uses standard base64. Compression + delta are handled by
//! pure-Rust helpers implementing the conquerd transfer protocol.
//!
//! Wire compatibility
//! ------------------
//! * `CHUNK_SIZE` = 65 536 bytes.
//! * Delta format: `b"CDv1"` magic + zlib-compressed opcode stream.
//! * Compression: zlib level 6; only applied when output ≤ 90 % of input and
//!   input ≥ 1 024 bytes.
//! * SHA-256 hex digest of the **original uncompressed** file is the transfer
//!   identifier for integrity.
//!
//! Inline vs. streaming
//! --------------------
//! Files at or below [`INLINE_MAX`] take the original in-RAM path: compressed
//! and/or delta-encoded, chunked from a `Vec<u8>`, reassembled from a chunk map.
//!
//! Larger files (up to [`MAX_TRANSFER_SIZE`]) stream: the sender reads
//! [`CHUNK_SIZE`] at a time from the source file on demand, and the receiver
//! writes each chunk straight into a sparse `.part` file at its own offset,
//! so neither side ever holds the whole payload.
//!
//! **Streaming transfers are never compressed or delta-encoded.** That is not
//! an optimisation choice — it is what makes `payload_len == size`, and hence
//! what makes "chunk `i` lives at byte offset `i * CHUNK_SIZE`" true. Fixed
//! offsets are the only reason a receiver can write chunks out of order without
//! buffering them. Do not add compression to the streaming path without also
//! replacing the offset calculation.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::protocol::MessageType;

// ── Constants ────────────────────────────────────────────────────────────────

/// Chunk size (65 536 bytes = 64 KiB).
pub const CHUNK_SIZE: usize = 65_536;
/// Maximum accepted file size (250 MiB).
///
/// Anything above [`INLINE_MAX`] is carried by the streaming path, so this
/// ceiling bounds disk use and transfer duration, not resident memory.
pub const MAX_TRANSFER_SIZE: usize = 250 * 1024 * 1024;
/// Largest file carried entirely in memory (8 MiB).
///
/// At or below this a transfer may be compressed or delta-encoded and is held
/// as a `Vec<u8>`; above it the streaming path applies. This also caps every
/// *expansion* bound below (decompression bombs, delta output): only inline
/// transfers are ever compressed, so nothing can legitimately inflate past it.
pub const INLINE_MAX: usize = 8 * 1024 * 1024;
/// Minimum input size before compression is attempted.
const COMPRESSION_THRESHOLD: usize = 1024;
/// Delta wire-format magic (identical to Rust crypto crate).
const DELTA_MAGIC: &[u8; 4] = b"CDv1";
/// How long a completed/failed transfer lingers before eviction (seconds).
///
/// Long enough for the UI to render a terminal state, short enough that a
/// long-lived client does not accumulate transfer records forever.
const TRANSFER_RETAIN_SECS: f64 = 300.0;
/// Chunks emitted per pump turn for a streaming room transfer.
///
/// The QUIC relay signaling queue holds 512 frames, so a bounded slice per turn
/// leaves headroom for chat/control traffic and lets the pump notice
/// backpressure instead of dumping 4 000 frames in one go. At 50 turns/s this
/// still exceeds the 8 MB/s `room.file.v1` quota, so the quota — not this
/// number — is what actually paces the transfer.
pub const ROOM_FILE_CHUNK_BUDGET: usize = 8;
/// How long an outbound offer stays answerable by a late `SfuFileRequest`.
///
/// The supernode caches nothing, so this is the entire window in which a room
/// member can accept a file and have it re-streamed from the sender's disk.
pub const OFFER_TTL_SECS: f64 = 3600.0;

// ── Transfer state ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    Pending,
    Transferring,
    Verifying,
    Complete,
    Failed,
    Rejected,
    Cancelled,
}

/// Whether a transfer has reached a state it can never leave.
fn is_terminal(state: &TransferState) -> bool {
    matches!(
        state,
        TransferState::Complete
            | TransferState::Failed
            | TransferState::Rejected
            | TransferState::Cancelled
    )
}

impl TransferState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Transferring => "transferring",
            Self::Verifying => "verifying",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
        }
    }
}

// ── Outbound transfer ─────────────────────────────────────────────────────────

/// Where an outbound transfer's payload bytes come from.
///
/// [`Inline`](Self::Inline) holds the (possibly compressed / delta-encoded)
/// payload in memory — the original behaviour, kept for small files.
/// [`FilePath`](Self::FilePath) holds only a path, and chunks are read from it
/// on demand so a 250 MB send costs one [`CHUNK_SIZE`] buffer.
pub enum TransferSource {
    Inline(Vec<u8>),
    FilePath { path: PathBuf, len: usize },
}

impl TransferSource {
    /// Length of the payload that will be chunked.
    pub fn payload_len(&self) -> usize {
        match self {
            Self::Inline(d) => d.len(),
            Self::FilePath { len, .. } => *len,
        }
    }

    fn is_streaming(&self) -> bool {
        matches!(self, Self::FilePath { .. })
    }

    /// Read chunk `index` into a freshly allocated buffer.
    ///
    /// Returns `None` past the end, or on any read error (the caller fails the
    /// transfer — a source file that vanished mid-send cannot be recovered).
    fn read_chunk(&self, index: usize) -> Option<Vec<u8>> {
        let offset = index.checked_mul(CHUNK_SIZE)?;
        let total = self.payload_len();
        if offset >= total && !(total == 0 && index == 0) {
            return None;
        }
        let want = CHUNK_SIZE.min(total - offset);
        match self {
            Self::Inline(d) => Some(d[offset..offset + want].to_vec()),
            Self::FilePath { path, .. } => {
                let mut f = File::open(path).ok()?;
                f.seek(SeekFrom::Start(offset as u64)).ok()?;
                let mut buf = vec![0u8; want];
                f.read_exact(&mut buf).ok()?;
                Some(buf)
            }
        }
    }
}

pub struct OutboundTransfer {
    pub transfer_id: String,
    pub peer_id: String,
    pub rel_path: String,
    /// Payload source (in-memory bytes, or a path streamed on demand).
    pub source: TransferSource,
    /// SHA-256 hex of the *original* uncompressed content.
    pub sha256: String,
    pub total_chunks: usize,
    pub chunks_sent: usize,
    pub state: TransferState,
    pub purpose: String,
    pub compressed: bool,
    pub is_delta: bool,
    pub created_at: f64,
    /// Room transfers only: the peer this stream is being sent to.
    ///
    /// Empty for a broadcast (1:1, or an offer not yet requested). Set when a
    /// `SfuFileRequest` starts a per-requester stream, and written to the
    /// chunk/complete frames' `to` field so the supernode narrows delivery.
    pub to: String,
    /// Terminal-state timestamp, for eviction. `None` while still active.
    pub finished_at: Option<f64>,
}

impl OutboundTransfer {
    pub fn progress(&self) -> f64 {
        if self.total_chunks == 0 {
            return 0.0;
        }
        self.chunks_sent as f64 / self.total_chunks as f64
    }
}

// ── Inbound transfer ──────────────────────────────────────────────────────────

/// Where an inbound transfer's chunks accumulate.
///
/// [`Memory`](Self::Memory) is the original chunk map, used for inline
/// transfers (which may be compressed, so chunk offsets are not predictable
/// until the whole payload is reassembled).
///
/// [`PartFile`](Self::PartFile) writes each chunk directly to its final byte
/// offset in a sparse `.part` file. Correct **only** because streaming
/// transfers are never compressed, so `payload_len == size` and chunk `i`
/// belongs at `i * CHUNK_SIZE`.
pub enum TransferSink {
    Memory(HashMap<usize, Vec<u8>>),
    PartFile {
        file: File,
        path: PathBuf,
        /// Per-index arrival flags; length is `total_chunks`.
        received: Vec<bool>,
        count: usize,
    },
}

impl TransferSink {
    fn received_count(&self) -> usize {
        match self {
            Self::Memory(m) => m.len(),
            Self::PartFile { count, .. } => *count,
        }
    }
}

pub struct InboundTransfer {
    pub transfer_id: String,
    /// Wire peer that offered the file (1:1 peer, or room-file originator).
    pub peer_id: String,
    /// When non-empty, this is a room transfer keyed by room id.
    pub room_id: String,
    /// Supernode that hosts the room (empty for 1:1).
    pub supernode_id: String,
    pub rel_path: String,
    pub expected_sha256: String,
    pub expected_size: usize,
    pub total_chunks: usize,
    pub state: TransferState,
    pub purpose: String,
    pub compressed: bool,
    pub is_delta: bool,
    pub base_sha256: String,
    pub created_at: f64,
    /// Terminal-state timestamp, for eviction. `None` while still active.
    pub finished_at: Option<f64>,
    /// Where arriving chunks accumulate (memory map, or a sparse `.part` file).
    sink: TransferSink,
}

impl InboundTransfer {
    #[allow(clippy::too_many_arguments)]
    fn new(
        transfer_id: String,
        peer_id: String,
        room_id: String,
        supernode_id: String,
        rel_path: String,
        expected_sha256: String,
        expected_size: usize,
        total_chunks: usize,
        purpose: String,
        compressed: bool,
        is_delta: bool,
        base_sha256: String,
        sink: TransferSink,
    ) -> Self {
        Self {
            transfer_id,
            peer_id,
            room_id,
            supernode_id,
            rel_path,
            expected_sha256,
            expected_size,
            total_chunks,
            state: if purpose == "update" {
                TransferState::Transferring
            } else {
                TransferState::Pending
            },
            purpose,
            compressed,
            is_delta,
            base_sha256,
            created_at: unix_now_f64(),
            finished_at: None,
            sink,
        }
    }

    /// True when chunks land straight on disk rather than in a memory map.
    fn is_streaming(&self) -> bool {
        matches!(self.sink, TransferSink::PartFile { .. })
    }

    /// Store a chunk; returns `true` if new, `false` if duplicate or invalid.
    fn store_chunk(&mut self, index: usize, data: Vec<u8>) -> bool {
        if index >= self.total_chunks {
            return false;
        }
        match &mut self.sink {
            TransferSink::Memory(chunks) => {
                if chunks.contains_key(&index) {
                    return false;
                }
                chunks.insert(index, data);
                true
            }
            TransferSink::PartFile {
                file,
                received,
                count,
                ..
            } => {
                if received.get(index).copied().unwrap_or(true) {
                    return false; // duplicate, or out of range
                }
                // Fixed offset — valid only because streaming transfers are
                // never compressed (see the module docs).
                let offset = (index * CHUNK_SIZE) as u64;
                if file.seek(SeekFrom::Start(offset)).is_err() || file.write_all(&data).is_err() {
                    warn!(
                        "[file] write failed for chunk {index} of {}; dropping",
                        self.transfer_id
                    );
                    return false;
                }
                received[index] = true;
                *count += 1;
                true
            }
        }
    }

    fn chunks_received(&self) -> usize {
        self.sink.received_count()
    }

    fn progress(&self) -> f64 {
        if self.total_chunks == 0 {
            return 0.0;
        }
        self.chunks_received() as f64 / self.total_chunks as f64
    }

    /// Reassemble all chunks in order (inline transfers only).
    fn reassemble(&self) -> Vec<u8> {
        let TransferSink::Memory(chunks) = &self.sink else {
            return Vec::new();
        };
        // Pre-size: avoids the doubling reallocation that used to peak at ~1.5×
        // the payload while the chunk map still held the same bytes.
        let mut out = Vec::with_capacity(self.expected_size.min(INLINE_MAX));
        for i in 0..self.total_chunks {
            if let Some(c) = chunks.get(&i) {
                out.extend_from_slice(c);
            }
        }
        out
    }
}

// ── Events emitted by FileTransferManager ────────────────────────────────────

/// A completed transfer's verified content.
///
/// Inline transfers hand back bytes for the caller to write. Streaming
/// transfers were written to disk chunk-by-chunk and verified in place, so they
/// hand back a path — a 250 MB file must never cross the bounded event channel
/// as a `Vec<u8>`.
#[derive(Debug, Clone)]
pub enum TransferPayload {
    /// Verified bytes; the caller is responsible for saving them.
    Bytes(Vec<u8>),
    /// Already verified and saved at this absolute path.
    SavedAt { path: String, len: u64 },
}

impl TransferPayload {
    /// Byte length of the received file, whichever form it took.
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes(b) => b.len() as u64,
            Self::SavedAt { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Events the manager produces; callers poll these to drive UI / bridge.
#[derive(Debug, Clone)]
pub enum TransferEvent {
    /// Inbound offer received; UI should prompt the user.
    Offered {
        transfer_id: String,
        peer_id: String,
        rel_path: String,
        size: usize,
        purpose: String,
    },
    /// Progress update (0.0–1.0).
    Progress { transfer_id: String, progress: f64 },
    /// Transfer verified and complete.
    Complete {
        transfer_id: String,
        /// Offer originator (1:1 peer, or room-file sender).
        peer_id: String,
        /// Non-empty when this was a room transfer.
        room_id: String,
        /// Supernode for room transfers (empty for 1:1).
        supernode_id: String,
        purpose: String,
        payload: TransferPayload,
        rel_path: String,
    },
    /// Transfer failed or rejected.
    Failed { transfer_id: String, reason: String },
    /// State changed (string tag from `TransferState::as_str`).
    StateChanged { transfer_id: String, state: String },
    /// The manager needs to send a signaling message outbound.
    SendMessage {
        peer_id: String,
        message_type: MessageType,
        payload: serde_json::Map<String, Value>,
    },
}

// ── FileTransferManager ───────────────────────────────────────────────────────

/// Offer metadata shared by the inline and streaming entry points.
///
/// Bundled into a struct purely to keep `insert_offer` under clippy's
/// argument-count lint; every field maps 1:1 to a `FileTransferOffer` payload
/// key.
struct OfferMeta {
    transfer_id: String,
    original_sha: String,
    original_size: usize,
    payload_len: usize,
    purpose: String,
    compressed: bool,
    is_delta: bool,
    base_sha256: String,
}

pub struct FileTransferManager {
    outbound: HashMap<String, OutboundTransfer>,
    inbound: HashMap<String, InboundTransfer>,
    /// Old file data keyed by rel_path, for delta application on receive.
    old_data_store: HashMap<String, Vec<u8>>,
}

impl FileTransferManager {
    pub fn new() -> Self {
        Self {
            outbound: HashMap::new(),
            inbound: HashMap::new(),
            old_data_store: HashMap::new(),
        }
    }

    /// Register old file content for delta computation / application.
    pub fn set_old_file_data(&mut self, rel_path: &str, data: Vec<u8>) {
        self.old_data_store.insert(rel_path.to_owned(), data);
    }

    /// Retrieve a clone of old file data by rel_path (for outbound delta hints).
    pub fn get_old_data(&self, rel_path: &str) -> Option<Vec<u8>> {
        self.old_data_store.get(rel_path).cloned()
    }

    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Drop finished transfers and abandon stale ones.
    ///
    /// Nothing used to be removed from either map, so every payload ever sent
    /// or received stayed resident for the process lifetime — survivable only
    /// because the old 10 MB cap kept each one small. Call this periodically.
    ///
    /// Returns the number of records dropped.
    pub fn gc(&mut self) -> usize {
        let now = unix_now_f64();
        let before = self.outbound.len() + self.inbound.len();

        // Stamp anything that reached a terminal state without recording when.
        // Doing it here rather than at each of the ~9 transition sites means a
        // future terminal path cannot silently become un-collectable.
        for x in self.outbound.values_mut() {
            if x.finished_at.is_none() && is_terminal(&x.state) {
                x.finished_at = Some(now);
            }
        }
        for x in self.inbound.values_mut() {
            if x.finished_at.is_none() && is_terminal(&x.state) {
                x.finished_at = Some(now);
            }
        }

        // Outbound offers stay answerable for OFFER_TTL_SECS so a room member
        // who accepts late still gets a re-stream; there are no payload bytes
        // held open, only a path.
        self.outbound.retain(|_, x| match x.finished_at {
            Some(t) => now - t < TRANSFER_RETAIN_SECS,
            None => now - x.created_at < OFFER_TTL_SECS,
        });

        // Inbound records own a `.part` file, so dropping one must delete it.
        let mut orphaned = Vec::new();
        self.inbound.retain(|_, x| {
            let keep = match x.finished_at {
                Some(t) => now - t < TRANSFER_RETAIN_SECS,
                None => now - x.created_at < OFFER_TTL_SECS,
            };
            if !keep {
                if let TransferSink::PartFile { path, .. } = &x.sink {
                    // A completed transfer already renamed its .part away, so
                    // this only fires for abandoned ones.
                    if path.exists() {
                        orphaned.push(path.clone());
                    }
                }
            }
            keep
        });
        for path in orphaned {
            if let Err(e) = std::fs::remove_file(&path) {
                debug!("[file] could not remove orphaned {}: {e}", path.display());
            }
        }

        before.saturating_sub(self.outbound.len() + self.inbound.len())
    }

    // ── On-demand room streaming ─────────────────────────────────────────

    /// A room member asked for `transfer_id` → start a stream addressed to them.
    ///
    /// The supernode caches nothing, so this is how a late acceptor gets the
    /// file: the offer is still on record (until [`OFFER_TTL_SECS`]) and the
    /// payload is re-read from the sender's disk. Returns the first slice of
    /// chunk events, or an empty vec if the offer is unknown or expired.
    pub fn start_stream_for(
        &mut self,
        transfer_id: &str,
        requester: &str,
        budget: usize,
    ) -> Vec<TransferEvent> {
        let Some(xfer) = self.outbound.get_mut(transfer_id) else {
            debug!("[file] request for unknown/expired transfer {transfer_id}; ignoring");
            return vec![];
        };
        // Restart from the top for this requester — each acceptor gets their
        // own pass over the source.
        xfer.chunks_sent = 0;
        xfer.to = requester.to_owned();
        xfer.state = TransferState::Transferring;
        xfer.finished_at = None;
        let (evs, _done) = next_chunk_events(xfer, budget);
        evs
    }

    /// Continue an in-flight outbound stream. Returns `(events, done)`.
    ///
    /// Does **not** mark the transfer complete: COMPLETE is only recorded after
    /// `dispatch_outbound` actually hands the frame to a transport. Marking
    /// complete here used to skip retry when quota dropped the COMPLETE.
    pub fn pump_stream(&mut self, transfer_id: &str, budget: usize) -> (Vec<TransferEvent>, bool) {
        let Some(xfer) = self.outbound.get_mut(transfer_id) else {
            return (vec![], true);
        };
        if xfer.state != TransferState::Transferring {
            return (vec![], true);
        }
        next_chunk_events(xfer, budget)
    }

    /// Route for an inbound offer: `(origin_peer, room_id, supernode_id)`.
    ///
    /// Needed to address the `SfuFileRequest` back at whoever advertised it.
    pub fn inbound_route(&self, transfer_id: &str) -> Option<(String, String, String)> {
        self.inbound
            .get(transfer_id)
            .map(|x| (x.peer_id.clone(), x.room_id.clone(), x.supernode_id.clone()))
    }

    /// Route for an outbound room transfer: `(room_id, supernode_id)`.
    ///
    /// Room transfers put the room id in `peer_id` (the UI keys chips by room),
    /// and the supernode is resolved by the caller when this is empty.
    pub fn outbound_route(&self, transfer_id: &str) -> Option<(String, String)> {
        self.outbound
            .get(transfer_id)
            .map(|x| (x.peer_id.clone(), String::new()))
    }

    /// Withdraw an outbound offer: no further request for it will be served.
    ///
    /// Because room files are pulled from the sender's own disk and the relay
    /// caches nothing, dropping this record is a real revocation — a peer who
    /// has not downloaded yet can no longer obtain the file from anyone. It
    /// cannot un-send bytes already delivered; peers who downloaded keep their
    /// copy. Any transfer still in flight stops (the pump has nothing to pump).
    ///
    /// Returns `true` if an offer was actually withdrawn.
    pub fn revoke_outbound(&mut self, transfer_id: &str) -> bool {
        self.outbound.remove(transfer_id).is_some()
    }

    /// True if we hold a still-serveable outbound offer for `transfer_id`.
    pub fn has_outbound(&self, transfer_id: &str) -> bool {
        self.outbound.contains_key(transfer_id)
    }

    /// True if we have an inbound transfer in flight (offer accepted or pending).
    pub fn has_inbound(&self, transfer_id: &str) -> bool {
        self.inbound.contains_key(transfer_id)
    }

    /// Drop a declined inbound offer, deleting any `.part` file it opened.
    pub fn discard_inbound(&mut self, transfer_id: &str) {
        if let Some(x) = self.inbound.remove(transfer_id) {
            if let TransferSink::PartFile { path, .. } = &x.sink {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// Transfer ids still streaming — including "all chunks counted locally
    /// but COMPLETE has not been handed to a transport yet", so a quota-blocked
    /// COMPLETE is retried instead of leaving the receiver hanging.
    pub fn active_outbound_streams(&self) -> Vec<String> {
        self.outbound
            .iter()
            .filter(|(_, x)| x.state == TransferState::Transferring)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Undo `n` locally-counted chunks that were not actually sent (quota /
    /// no path). The pump will emit them again.
    pub fn unsend_chunks(&mut self, transfer_id: &str, n: usize) {
        let Some(x) = self.outbound.get_mut(transfer_id) else {
            return;
        };
        x.chunks_sent = x.chunks_sent.saturating_sub(n);
        if x.state == TransferState::Complete {
            x.state = TransferState::Transferring;
            x.finished_at = None;
        }
    }

    /// COMPLETE frame was handed to a transport.
    pub fn mark_outbound_complete(&mut self, transfer_id: &str) {
        if let Some(x) = self.outbound.get_mut(transfer_id) {
            x.state = TransferState::Complete;
            x.finished_at = Some(unix_now_f64());
        }
    }

    // ── Outbound ─────────────────────────────────────────────────────────

    /// Create an outbound offer.  Returns `(transfer_id, events)`.
    pub fn offer_file(
        &mut self,
        peer_id: &str,
        rel_path: &str,
        data: Vec<u8>,
        purpose: &str,
        old_data: Option<&[u8]>,
        auto_push: bool,
    ) -> Result<(String, Vec<TransferEvent>), String> {
        self.offer_file_with_id(peer_id, rel_path, data, purpose, old_data, auto_push, None)
    }

    /// [`Self::offer_file`] with a caller-chosen transfer id. See
    /// [`Self::offer_file_from_path_with_id`] for why the UI picks it.
    #[allow(clippy::too_many_arguments)]
    pub fn offer_file_with_id(
        &mut self,
        peer_id: &str,
        rel_path: &str,
        data: Vec<u8>,
        purpose: &str,
        old_data: Option<&[u8]>,
        auto_push: bool,
        transfer_id: Option<&str>,
    ) -> Result<(String, Vec<TransferEvent>), String> {
        if data.len() > INLINE_MAX {
            // Callers with a file on disk should use `offer_file_from_path`;
            // this entry point materializes the payload and cannot serve a
            // 250 MB file without the RAM blow-up streaming exists to avoid.
            return Err(format!(
                "File too large for the in-memory path ({} bytes, max {INLINE_MAX}); \
                 use offer_file_from_path",
                data.len()
            ));
        }

        let transfer_id = transfer_id
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string()[..16].to_owned());
        let original_sha = sha256_hex(&data);
        let original_size = data.len();

        // Delta attempt
        let (payload, compressed, is_delta, base_sha256) = if let Some(old) = old_data {
            if let Some(delta) = create_delta(old, &data) {
                let base = sha256_hex(old);
                info!(
                    "Delta computed for {}: {} → {} bytes",
                    rel_path,
                    data.len(),
                    delta.len()
                );
                (delta, false, true, base)
            } else {
                let (c, flag) = compress_data(&data);
                (c, flag, false, String::new())
            }
        } else {
            let (c, flag) = compress_data(&data);
            (c, flag, false, String::new())
        };

        let payload_len = payload.len();
        Ok(self.insert_offer(
            peer_id,
            rel_path,
            TransferSource::Inline(payload),
            OfferMeta {
                transfer_id,
                original_sha,
                original_size,
                payload_len,
                purpose: purpose.to_owned(),
                compressed,
                is_delta,
                base_sha256,
            },
            auto_push,
        ))
    }

    /// Create an outbound offer for a file on disk, without reading it in.
    ///
    /// The payload is the file verbatim — no compression, no delta — so
    /// `payload_len == size` and chunk `i` sits at `i * CHUNK_SIZE`, which is
    /// what lets the receiver write chunks straight to their final offsets.
    /// Only the SHA-256 pass reads the file here, and it streams.
    pub fn offer_file_from_path(
        &mut self,
        peer_id: &str,
        rel_path: &str,
        path: &Path,
        purpose: &str,
        auto_push: bool,
    ) -> Result<(String, Vec<TransferEvent>), String> {
        self.offer_file_from_path_with_id(peer_id, rel_path, path, purpose, auto_push, None)
    }

    /// [`Self::offer_file_from_path`] with a caller-chosen transfer id.
    ///
    /// The UI supplies the id so the sender's own chat message can be keyed
    /// `xfer-{transfer_id}`, matching what receivers already use. Without that
    /// shared key there is no way to get from "the user deleted this message"
    /// back to the transfer it advertised, so sharing could never be revoked.
    pub fn offer_file_from_path_with_id(
        &mut self,
        peer_id: &str,
        rel_path: &str,
        path: &Path,
        purpose: &str,
        auto_push: bool,
        transfer_id: Option<&str>,
    ) -> Result<(String, Vec<TransferEvent>), String> {
        let len = std::fs::metadata(path)
            .map_err(|e| format!("Cannot stat {}: {e}", path.display()))?
            .len();
        if len > MAX_TRANSFER_SIZE as u64 {
            return Err(format!(
                "File too large ({len} bytes, max {MAX_TRANSFER_SIZE})"
            ));
        }
        let original_size = len as usize;
        let original_sha = sha256_hex_file(path)?;

        Ok(self.insert_offer(
            peer_id,
            rel_path,
            TransferSource::FilePath {
                path: path.to_path_buf(),
                len: original_size,
            },
            OfferMeta {
                transfer_id: transfer_id
                    .map(str::to_owned)
                    .unwrap_or_else(|| Uuid::new_v4().simple().to_string()[..16].to_owned()),
                original_sha,
                original_size,
                payload_len: original_size,
                purpose: purpose.to_owned(),
                compressed: false,
                is_delta: false,
                base_sha256: String::new(),
            },
            auto_push,
        ))
    }

    /// Record an outbound transfer and build its OFFER frame.
    fn insert_offer(
        &mut self,
        peer_id: &str,
        rel_path: &str,
        source: TransferSource,
        meta: OfferMeta,
        auto_push: bool,
    ) -> (String, Vec<TransferEvent>) {
        let total_chunks = meta.payload_len.div_ceil(CHUNK_SIZE).max(1);
        let transfer_id = meta.transfer_id;

        let mut xfer = OutboundTransfer {
            transfer_id: transfer_id.clone(),
            peer_id: peer_id.to_owned(),
            rel_path: rel_path.to_owned(),
            source,
            sha256: meta.original_sha.clone(),
            total_chunks,
            chunks_sent: 0,
            state: TransferState::Pending,
            purpose: meta.purpose.clone(),
            compressed: meta.compressed,
            is_delta: meta.is_delta,
            created_at: unix_now_f64(),
            to: String::new(),
            finished_at: None,
        };

        let mut events = Vec::new();

        // Build the OFFER message
        let mut offer_payload = serde_json::Map::new();
        offer_payload.insert("transfer_id".into(), Value::String(transfer_id.clone()));
        offer_payload.insert("rel_path".into(), Value::String(rel_path.to_owned()));
        offer_payload.insert("sha256".into(), Value::String(meta.original_sha));
        offer_payload.insert("size".into(), Value::Number(meta.original_size.into()));
        offer_payload.insert(
            "payload_size".into(),
            Value::Number(meta.payload_len.into()),
        );
        offer_payload.insert("total_chunks".into(), Value::Number(total_chunks.into()));
        offer_payload.insert("purpose".into(), Value::String(meta.purpose));
        offer_payload.insert("compressed".into(), Value::Bool(meta.compressed));
        offer_payload.insert("is_delta".into(), Value::Bool(meta.is_delta));
        offer_payload.insert("base_sha256".into(), Value::String(meta.base_sha256));
        events.push(TransferEvent::SendMessage {
            peer_id: peer_id.to_owned(),
            message_type: MessageType::FileTransferOffer,
            payload: offer_payload,
        });

        if auto_push {
            xfer.state = TransferState::Transferring;
            events.extend(build_chunk_events(&mut xfer));
        }

        self.outbound.insert(transfer_id.clone(), xfer);
        (transfer_id, events)
    }

    /// Peer accepted our offer → send chunks.
    ///
    /// An inline transfer is emitted whole (bounded by [`INLINE_MAX`]); a
    /// streaming one emits only its first slice here and is carried the rest of
    /// the way by [`Self::pump_stream`], so a large 1:1 send does not
    /// materialize thousands of frames in one turn either.
    pub fn on_transfer_accepted(&mut self, transfer_id: &str) -> Vec<TransferEvent> {
        let xfer = match self.outbound.get_mut(transfer_id) {
            Some(x) if x.state == TransferState::Pending => x,
            _ => return vec![],
        };
        xfer.state = TransferState::Transferring;
        let mut evs = vec![TransferEvent::StateChanged {
            transfer_id: transfer_id.to_owned(),
            state: "transferring".into(),
        }];
        if xfer.source.is_streaming() {
            let (chunk_evs, _done) = next_chunk_events(xfer, ROOM_FILE_CHUNK_BUDGET);
            evs.extend(chunk_evs);
        } else {
            evs.extend(build_chunk_events(xfer));
        }
        evs
    }

    pub fn on_transfer_rejected(&mut self, transfer_id: &str) -> Vec<TransferEvent> {
        if let Some(x) = self.outbound.get_mut(transfer_id) {
            x.state = TransferState::Rejected;
            return vec![
                TransferEvent::Failed {
                    transfer_id: transfer_id.to_owned(),
                    reason: "rejected by peer".into(),
                },
                TransferEvent::StateChanged {
                    transfer_id: transfer_id.to_owned(),
                    state: "rejected".into(),
                },
            ];
        }
        vec![]
    }

    pub fn on_transfer_ack(&mut self, transfer_id: &str) -> Vec<TransferEvent> {
        if let Some(x) = self.outbound.get_mut(transfer_id) {
            x.state = TransferState::Complete;
            return vec![TransferEvent::StateChanged {
                transfer_id: transfer_id.to_owned(),
                state: "complete".into(),
            }];
        }
        vec![]
    }

    pub fn on_transfer_error(&mut self, transfer_id: &str, reason: &str) -> Vec<TransferEvent> {
        if let Some(x) = self.outbound.get_mut(transfer_id) {
            x.state = TransferState::Failed;
            return vec![
                TransferEvent::Failed {
                    transfer_id: transfer_id.to_owned(),
                    reason: reason.to_owned(),
                },
                TransferEvent::StateChanged {
                    transfer_id: transfer_id.to_owned(),
                    state: "failed".into(),
                },
            ];
        }
        vec![]
    }

    // ── Inbound ──────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub fn on_offer_received(
        &mut self,
        peer_id: &str,
        transfer_id: &str,
        rel_path: &str,
        sha256: &str,
        size: usize,
        total_chunks: usize,
        purpose: &str,
        compressed: bool,
        is_delta: bool,
        base_sha256: &str,
    ) -> Vec<TransferEvent> {
        self.on_offer_received_with_room(
            peer_id,
            "",
            "",
            transfer_id,
            rel_path,
            sha256,
            size,
            total_chunks,
            purpose,
            compressed,
            is_delta,
            base_sha256,
        )
    }

    /// Like [`on_offer_received`] but records room/supernode context for SFU
    /// file transfers so completion can embed into the correct room chat.
    #[allow(clippy::too_many_arguments)]
    pub fn on_offer_received_with_room(
        &mut self,
        peer_id: &str,
        room_id: &str,
        supernode_id: &str,
        transfer_id: &str,
        rel_path: &str,
        sha256: &str,
        size: usize,
        total_chunks: usize,
        purpose: &str,
        compressed: bool,
        is_delta: bool,
        base_sha256: &str,
    ) -> Vec<TransferEvent> {
        if size > MAX_TRANSFER_SIZE {
            let mut p = serde_json::Map::new();
            p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
            p.insert("reason".into(), Value::String("file_too_large".into()));
            return vec![TransferEvent::SendMessage {
                peer_id: peer_id.to_owned(),
                message_type: MessageType::FileTransferReject,
                payload: p,
            }];
        }
        // Bound total_chunks: a peer claiming millions of chunks for a tiny
        // file is a protocol error (or a resource-exhaustion probe).
        //
        // This is a BOUND, not an equality. `size` is the original file size,
        // but the sender chunks the *payload* — `compress_data`'d or delta'd
        // (see `offer_file`) — which is normally much smaller. Equating the two
        // rejected every compressed or delta transfer with `invalid_chunk_count`
        // (e.g. a 2 785 432-byte file whose 1.5 MB deflated payload is 23 chunks,
        // not 43), and neither side surfaced it, so a send just silently never
        // appeared for the peer. One chunk of headroom covers zlib framing
        // overhead on incompressible input. Correctness does not rest on this
        // number: assembly still verifies `expected_size` and the sha256.
        let max_chunks = size.div_ceil(CHUNK_SIZE).max(1) + 1;
        if total_chunks == 0 || total_chunks > max_chunks {
            let mut p = serde_json::Map::new();
            p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
            p.insert("reason".into(), Value::String("invalid_chunk_count".into()));
            warn!(
                "Rejected transfer {transfer_id}: total_chunks {total_chunks} \
                 exceeds max {max_chunks} for size {size}"
            );
            return vec![TransferEvent::SendMessage {
                peer_id: peer_id.to_owned(),
                message_type: MessageType::FileTransferReject,
                payload: p,
            }];
        }

        // Pick the sink. Anything above INLINE_MAX streams to a sparse `.part`
        // file; a compressed or delta payload must stay in memory because its
        // chunk offsets do not map to file offsets.
        let sink = if size > INLINE_MAX && !compressed && !is_delta {
            match open_part_file(rel_path, transfer_id) {
                Ok((file, path)) => TransferSink::PartFile {
                    file,
                    path,
                    received: vec![false; total_chunks],
                    count: 0,
                },
                Err(e) => {
                    warn!("Rejected transfer {transfer_id}: cannot open .part file: {e}");
                    let mut p = serde_json::Map::new();
                    p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
                    p.insert("reason".into(), Value::String("storage_unavailable".into()));
                    return vec![TransferEvent::SendMessage {
                        peer_id: peer_id.to_owned(),
                        message_type: MessageType::FileTransferReject,
                        payload: p,
                    }];
                }
            }
        } else if size > INLINE_MAX {
            // Compressed/delta above the inline ceiling would have to be
            // buffered whole — exactly what the cap exists to prevent.
            warn!(
                "Rejected transfer {transfer_id}: compressed/delta payload of {size} bytes \
                 exceeds the in-memory ceiling {INLINE_MAX}"
            );
            let mut p = serde_json::Map::new();
            p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
            p.insert("reason".into(), Value::String("file_too_large".into()));
            return vec![TransferEvent::SendMessage {
                peer_id: peer_id.to_owned(),
                message_type: MessageType::FileTransferReject,
                payload: p,
            }];
        } else {
            TransferSink::Memory(HashMap::new())
        };

        let xfer = InboundTransfer::new(
            transfer_id.to_owned(),
            peer_id.to_owned(),
            room_id.to_owned(),
            supernode_id.to_owned(),
            rel_path.to_owned(),
            sha256.to_owned(),
            size,
            total_chunks,
            purpose.to_owned(),
            compressed,
            is_delta,
            base_sha256.to_owned(),
            sink,
        );

        // UI keys room transfers by room_id so chips filter with the room panel.
        let offered_peer = if room_id.is_empty() {
            peer_id.to_owned()
        } else {
            room_id.to_owned()
        };
        let mut evs = vec![TransferEvent::Offered {
            transfer_id: transfer_id.to_owned(),
            peer_id: offered_peer,
            rel_path: rel_path.to_owned(),
            size,
            purpose: purpose.to_owned(),
        }];

        if purpose == "update" {
            // Auto-accept for update transfers (push-mode).
            let mut p = serde_json::Map::new();
            p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
            evs.push(TransferEvent::SendMessage {
                peer_id: peer_id.to_owned(),
                message_type: MessageType::FileTransferAccept,
                payload: p,
            });
        }

        self.inbound.insert(transfer_id.to_owned(), xfer);
        evs
    }

    pub fn accept_transfer(&mut self, transfer_id: &str) -> Vec<TransferEvent> {
        let xfer = match self.inbound.get_mut(transfer_id) {
            Some(x) if x.state == TransferState::Pending => x,
            _ => return vec![],
        };
        xfer.state = TransferState::Transferring;
        let peer_id = xfer.peer_id.clone();
        let mut p = serde_json::Map::new();
        p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
        vec![
            TransferEvent::StateChanged {
                transfer_id: transfer_id.to_owned(),
                state: "transferring".into(),
            },
            TransferEvent::SendMessage {
                peer_id,
                message_type: MessageType::FileTransferAccept,
                payload: p,
            },
        ]
    }

    /// Move an inbound transfer into the receiving state without producing an
    /// outbound accept frame. Room file broadcasts (`SfuFile*`) are push-mode:
    /// the sender broadcasts chunks to every recipient, so there is no
    /// per-recipient accept/reject message on that wire path.
    pub fn accept_transfer_locally(&mut self, transfer_id: &str) -> Vec<TransferEvent> {
        let Some(xfer) = self.inbound.get_mut(transfer_id) else {
            return vec![];
        };
        if xfer.state != TransferState::Pending {
            return vec![];
        }
        xfer.state = TransferState::Transferring;
        vec![TransferEvent::StateChanged {
            transfer_id: transfer_id.to_owned(),
            state: "transferring".into(),
        }]
    }

    pub fn reject_transfer(&mut self, transfer_id: &str, reason: &str) -> Vec<TransferEvent> {
        let xfer = match self.inbound.get_mut(transfer_id) {
            Some(x) => x,
            None => return vec![],
        };
        xfer.state = TransferState::Rejected;
        let peer_id = xfer.peer_id.clone();
        let mut p = serde_json::Map::new();
        p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
        p.insert("reason".into(), Value::String(reason.to_owned()));
        vec![
            TransferEvent::StateChanged {
                transfer_id: transfer_id.to_owned(),
                state: "rejected".into(),
            },
            TransferEvent::SendMessage {
                peer_id,
                message_type: MessageType::FileTransferReject,
                payload: p,
            },
        ]
    }

    pub fn cancel_transfer(&mut self, transfer_id: &str) -> Vec<TransferEvent> {
        if let Some(xfer) = self.inbound.get_mut(transfer_id) {
            match xfer.state {
                TransferState::Pending => {
                    return self.reject_transfer(transfer_id, "cancelled");
                }
                TransferState::Transferring => {
                    xfer.state = TransferState::Cancelled;
                    let peer_id = xfer.peer_id.clone();
                    let mut p = serde_json::Map::new();
                    p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
                    p.insert("reason".into(), Value::String("cancelled".into()));
                    return vec![
                        TransferEvent::Failed {
                            transfer_id: transfer_id.to_owned(),
                            reason: "cancelled".into(),
                        },
                        TransferEvent::StateChanged {
                            transfer_id: transfer_id.to_owned(),
                            state: "cancelled".into(),
                        },
                        TransferEvent::SendMessage {
                            peer_id,
                            message_type: MessageType::FileTransferError,
                            payload: p,
                        },
                    ];
                }
                _ => {}
            }
        }
        if let Some(xfer) = self.outbound.get_mut(transfer_id) {
            if matches!(
                xfer.state,
                TransferState::Pending | TransferState::Transferring
            ) {
                xfer.state = TransferState::Cancelled;
                return vec![
                    TransferEvent::Failed {
                        transfer_id: transfer_id.to_owned(),
                        reason: "cancelled".into(),
                    },
                    TransferEvent::StateChanged {
                        transfer_id: transfer_id.to_owned(),
                        state: "cancelled".into(),
                    },
                ];
            }
        }
        vec![]
    }

    pub fn on_chunk_received(
        &mut self,
        transfer_id: &str,
        chunk_index: usize,
        data_b64: &str,
    ) -> Vec<TransferEvent> {
        let chunk = match B64.decode(data_b64) {
            Ok(b) => b,
            Err(_) => return self.fail_inbound(transfer_id, "invalid base64 chunk"),
        };
        self.on_chunk_bytes_received(transfer_id, chunk_index, chunk)
    }

    /// Store an already-decoded chunk.
    ///
    /// The room path decrypts to raw bytes, so it uses this directly rather
    /// than re-encoding to base64 just to have it decoded again.
    pub fn on_chunk_bytes_received(
        &mut self,
        transfer_id: &str,
        chunk_index: usize,
        chunk: Vec<u8>,
    ) -> Vec<TransferEvent> {
        let xfer = match self.inbound.get_mut(transfer_id) {
            Some(x) if x.state == TransferState::Transferring => x,
            _ => return vec![],
        };
        if chunk_index >= xfer.total_chunks {
            return self.fail_inbound(
                transfer_id,
                &format!("chunk_index {chunk_index} out of range"),
            );
        }
        let progress = if xfer.store_chunk(chunk_index, chunk) {
            xfer.progress()
        } else {
            return vec![];
        };
        vec![TransferEvent::Progress {
            transfer_id: transfer_id.to_owned(),
            progress,
        }]
    }

    pub fn on_complete_received(&mut self, transfer_id: &str) -> Vec<TransferEvent> {
        // --- Phase 0: completeness check + route by sink kind ---
        let streaming = {
            let xfer = match self.inbound.get(transfer_id) {
                Some(x) if x.state == TransferState::Transferring => x,
                _ => return vec![],
            };
            if xfer.chunks_received() != xfer.total_chunks {
                let reason = format!(
                    "missing chunks: got {}/{}",
                    xfer.chunks_received(),
                    xfer.total_chunks
                );
                return self.fail_inbound_with_error(transfer_id, &reason);
            }
            xfer.is_streaming()
        };

        // Streaming transfers are already on disk at their final offsets:
        // verify in place and rename, never materializing the payload.
        if streaming {
            return self.finish_streaming(transfer_id);
        }

        // --- Phase 1: validate and extract all data while holding the borrow ---
        let (assembled_raw, compressed, is_delta, rel_path_owned, expected_sha, expected_size) = {
            let xfer = match self.inbound.get_mut(transfer_id) {
                Some(x) if x.state == TransferState::Transferring => x,
                _ => return vec![],
            };

            if xfer.chunks_received() != xfer.total_chunks {
                let reason = format!(
                    "missing chunks: got {}/{}",
                    xfer.chunks_received(),
                    xfer.total_chunks
                );
                xfer.state = TransferState::Failed;
                let peer_id = xfer.peer_id.clone();
                warn!("Inbound transfer {transfer_id} failed: {reason}");
                let mut p = serde_json::Map::new();
                p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
                p.insert("reason".into(), Value::String(reason.clone()));
                return vec![
                    TransferEvent::Failed {
                        transfer_id: transfer_id.to_owned(),
                        reason,
                    },
                    TransferEvent::StateChanged {
                        transfer_id: transfer_id.to_owned(),
                        state: "failed".into(),
                    },
                    TransferEvent::SendMessage {
                        peer_id,
                        message_type: MessageType::FileTransferError,
                        payload: p,
                    },
                ];
            }

            xfer.state = TransferState::Verifying;
            (
                xfer.reassemble(),
                xfer.compressed,
                xfer.is_delta,
                xfer.rel_path.clone(),
                xfer.expected_sha256.clone(),
                xfer.expected_size,
            )
        };

        // --- Phase 2: decompress / apply delta (no live borrow of self.inbound) ---
        let mut assembled = assembled_raw;

        if compressed {
            match zlib_decompress(&assembled) {
                Ok(d) => assembled = d,
                Err(e) => {
                    return self.fail_inbound(transfer_id, &format!("decompression failed: {e}"))
                }
            }
        }

        if is_delta {
            let old_data = match self.old_data_store.get(&rel_path_owned).cloned() {
                Some(d) => d,
                None => {
                    return self.fail_inbound(
                        transfer_id,
                        &format!("delta requires old file data for {rel_path_owned}"),
                    )
                }
            };
            match apply_delta(&old_data, &assembled) {
                Ok(d) => assembled = d,
                Err(e) => {
                    return self.fail_inbound(transfer_id, &format!("delta apply failed: {e}"))
                }
            }
        }

        if assembled.len() != expected_size {
            let reason = format!("size mismatch: {} vs {}", assembled.len(), expected_size);
            return self.fail_inbound(transfer_id, &reason);
        }

        let actual_hash = sha256_hex(&assembled);
        if actual_hash != expected_sha {
            return self.fail_inbound(
                transfer_id,
                &format!(
                    "hash mismatch: {}… vs {}…",
                    &actual_hash[..16],
                    &expected_sha[..16]
                ),
            );
        }

        // --- Phase 3: mark complete ---
        let (peer_id, room_id, supernode_id, purpose) = match self.inbound.get_mut(transfer_id) {
            Some(xfer) => {
                xfer.state = TransferState::Complete;
                xfer.finished_at = Some(unix_now_f64());
                (
                    xfer.peer_id.clone(),
                    xfer.room_id.clone(),
                    xfer.supernode_id.clone(),
                    xfer.purpose.clone(),
                )
            }
            // Transfer was cancelled or completed concurrently between the
            // hash check and the state update — drop silently rather than
            // panicking. Without this guard a peer can crash the client by
            // racing FileTransferComplete with FileTransferCancel.
            None => {
                debug!("Transfer {transfer_id} disappeared before completion; ignoring");
                return Vec::new();
            }
        };

        info!(
            "Transfer {transfer_id} complete and verified ({rel_path_owned}, {} bytes)",
            assembled.len()
        );

        let mut p = serde_json::Map::new();
        p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));

        vec![
            TransferEvent::SendMessage {
                peer_id: peer_id.clone(),
                message_type: MessageType::FileTransferAck,
                payload: p,
            },
            TransferEvent::Complete {
                transfer_id: transfer_id.to_owned(),
                peer_id,
                room_id,
                supernode_id,
                purpose,
                payload: TransferPayload::Bytes(assembled),
                rel_path: rel_path_owned,
            },
            TransferEvent::StateChanged {
                transfer_id: transfer_id.to_owned(),
                state: "complete".into(),
            },
        ]
    }

    // ── Private helpers ───────────────────────────────────────────────────

    /// Verify a fully-received streaming transfer on disk and publish it.
    ///
    /// The `.part` file already holds every chunk at its final offset, so this
    /// truncates any tail slack, re-reads it once to hash, then renames it into
    /// place. Peak memory is one [`CHUNK_SIZE`] buffer regardless of file size.
    fn finish_streaming(&mut self, transfer_id: &str) -> Vec<TransferEvent> {
        let (part_path, expected_sha, expected_size, rel_path) = {
            let Some(xfer) = self.inbound.get_mut(transfer_id) else {
                return vec![];
            };
            let TransferSink::PartFile { file, path, .. } = &mut xfer.sink else {
                return vec![];
            };
            // The last chunk is short; the sparse file may be longer than the
            // payload if a write landed past it. Pin the exact length.
            if let Err(e) = file
                .set_len(xfer.expected_size as u64)
                .and_then(|()| file.flush())
            {
                let reason = format!("finalize failed: {e}");
                return self.fail_inbound(transfer_id, &reason);
            }
            (
                path.clone(),
                xfer.expected_sha256.clone(),
                xfer.expected_size,
                xfer.rel_path.clone(),
            )
        };

        match std::fs::metadata(&part_path).map(|m| m.len()) {
            Ok(len) if len == expected_size as u64 => {}
            Ok(len) => {
                let reason = format!("size mismatch: {len} vs {expected_size}");
                return self.fail_inbound(transfer_id, &reason);
            }
            Err(e) => {
                let reason = format!("cannot stat received file: {e}");
                return self.fail_inbound(transfer_id, &reason);
            }
        }

        let actual = match sha256_hex_file(&part_path) {
            Ok(h) => h,
            Err(e) => return self.fail_inbound(transfer_id, &e),
        };
        if actual != expected_sha {
            let reason = format!(
                "hash mismatch: {}… vs {}…",
                &actual[..16.min(actual.len())],
                &expected_sha[..16.min(expected_sha.len())]
            );
            return self.fail_inbound(transfer_id, &reason);
        }

        // Verified — publish under a non-colliding final name.
        let final_path = match promote_part_file(&part_path, &rel_path) {
            Ok(p) => p,
            Err(e) => {
                let reason = format!("could not save received file: {e}");
                return self.fail_inbound(transfer_id, &reason);
            }
        };

        let (peer_id, room_id, supernode_id, purpose) = match self.inbound.get_mut(transfer_id) {
            Some(xfer) => {
                xfer.state = TransferState::Complete;
                xfer.finished_at = Some(unix_now_f64());
                (
                    xfer.peer_id.clone(),
                    xfer.room_id.clone(),
                    xfer.supernode_id.clone(),
                    xfer.purpose.clone(),
                )
            }
            None => return vec![],
        };

        info!(
            "Transfer {transfer_id} complete and verified ({rel_path}, {expected_size} bytes, streamed)"
        );

        let mut p = serde_json::Map::new();
        p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));

        vec![
            TransferEvent::SendMessage {
                peer_id: peer_id.clone(),
                message_type: MessageType::FileTransferAck,
                payload: p,
            },
            TransferEvent::Complete {
                transfer_id: transfer_id.to_owned(),
                peer_id,
                room_id,
                supernode_id,
                purpose,
                payload: TransferPayload::SavedAt {
                    path: final_path.to_string_lossy().into_owned(),
                    len: expected_size as u64,
                },
                rel_path,
            },
            TransferEvent::StateChanged {
                transfer_id: transfer_id.to_owned(),
                state: "complete".into(),
            },
        ]
    }

    /// Fail an inbound transfer and tell the sender (used for missing chunks).
    fn fail_inbound_with_error(&mut self, transfer_id: &str, reason: &str) -> Vec<TransferEvent> {
        let Some(xfer) = self.inbound.get_mut(transfer_id) else {
            return vec![];
        };
        xfer.state = TransferState::Failed;
        xfer.finished_at = Some(unix_now_f64());
        let peer_id = xfer.peer_id.clone();
        warn!("Inbound transfer {transfer_id} failed: {reason}");
        let mut p = serde_json::Map::new();
        p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
        p.insert("reason".into(), Value::String(reason.to_owned()));
        vec![
            TransferEvent::Failed {
                transfer_id: transfer_id.to_owned(),
                reason: reason.to_owned(),
            },
            TransferEvent::StateChanged {
                transfer_id: transfer_id.to_owned(),
                state: "failed".into(),
            },
            TransferEvent::SendMessage {
                peer_id,
                message_type: MessageType::FileTransferError,
                payload: p,
            },
        ]
    }

    fn fail_inbound(&mut self, transfer_id: &str, reason: &str) -> Vec<TransferEvent> {
        if let Some(xfer) = self.inbound.get_mut(transfer_id) {
            xfer.state = TransferState::Failed;
            let peer_id = xfer.peer_id.clone();
            warn!("Inbound transfer {transfer_id} failed: {reason}");
            let mut p = serde_json::Map::new();
            p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
            p.insert("reason".into(), Value::String(reason.to_owned()));
            return vec![
                TransferEvent::Failed {
                    transfer_id: transfer_id.to_owned(),
                    reason: reason.to_owned(),
                },
                TransferEvent::StateChanged {
                    transfer_id: transfer_id.to_owned(),
                    state: "failed".into(),
                },
                TransferEvent::SendMessage {
                    peer_id,
                    message_type: MessageType::FileTransferError,
                    payload: p,
                },
            ];
        }
        vec![]
    }
}

// ── Build chunk events for an outbound transfer ───────────────────────────────

/// Emit at most `budget` chunk frames, resuming from `xfer.chunks_sent`.
///
/// Returns `(events, done)`. `done` is true once the COMPLETE frame has been
/// appended, i.e. the transfer has been fully emitted.
///
/// This is the pump the streaming path drives: the caller emits a bounded slice
/// per turn and stops when the transport signals backpressure, so a 250 MB file
/// never materializes 4 000 base64 frames at once.
fn next_chunk_events(xfer: &mut OutboundTransfer, budget: usize) -> (Vec<TransferEvent>, bool) {
    // Two pushes per chunk (frame + progress) plus the trailing COMPLETE.
    let mut evs = Vec::with_capacity(budget * 2 + 1);
    let mut emitted = 0usize;

    while xfer.chunks_sent < xfer.total_chunks && emitted < budget {
        let i = xfer.chunks_sent;
        let Some(chunk) = xfer.source.read_chunk(i) else {
            warn!(
                "[file] source read failed at chunk {i} for {}; aborting stream",
                xfer.transfer_id
            );
            xfer.state = TransferState::Failed;
            xfer.finished_at = Some(unix_now_f64());
            evs.push(TransferEvent::Failed {
                transfer_id: xfer.transfer_id.clone(),
                reason: "source file unreadable".into(),
            });
            return (evs, true);
        };

        let mut p = serde_json::Map::new();
        p.insert(
            "transfer_id".into(),
            Value::String(xfer.transfer_id.clone()),
        );
        p.insert("chunk_index".into(), Value::Number(i.into()));
        p.insert("data".into(), Value::String(B64.encode(&chunk)));
        if !xfer.to.is_empty() {
            p.insert("to".into(), Value::String(xfer.to.clone()));
        }
        evs.push(TransferEvent::SendMessage {
            peer_id: xfer.peer_id.clone(),
            message_type: MessageType::FileTransferChunk,
            payload: p,
        });

        xfer.chunks_sent += 1;
        emitted += 1;

        // Throttle progress: one event per chunk floods the bounded event
        // channel, which drops on overflow — and `Complete` rides the same
        // channel. Report on ~1 % boundaries and always on the last chunk.
        if progress_is_reportable(xfer.chunks_sent, xfer.total_chunks) {
            evs.push(TransferEvent::Progress {
                transfer_id: xfer.transfer_id.clone(),
                progress: xfer.chunks_sent as f64 / xfer.total_chunks as f64,
            });
        }
    }

    let done = xfer.chunks_sent >= xfer.total_chunks;
    if done {
        let mut p = serde_json::Map::new();
        p.insert(
            "transfer_id".into(),
            Value::String(xfer.transfer_id.clone()),
        );
        if !xfer.to.is_empty() {
            p.insert("to".into(), Value::String(xfer.to.clone()));
        }
        evs.push(TransferEvent::SendMessage {
            peer_id: xfer.peer_id.clone(),
            message_type: MessageType::FileTransferComplete,
            payload: p,
        });
    }
    (evs, done)
}

/// Emit every remaining chunk in one go (inline transfers only).
///
/// Bounded by [`INLINE_MAX`], so this is at most ~128 frames.
fn build_chunk_events(xfer: &mut OutboundTransfer) -> Vec<TransferEvent> {
    let (evs, _) = next_chunk_events(xfer, xfer.total_chunks);
    evs
}

/// Whether chunk `sent` of `total` crosses a ~1 % progress boundary.
fn progress_is_reportable(sent: usize, total: usize) -> bool {
    if total == 0 {
        return false;
    }
    if sent >= total {
        return true;
    }
    let step = (total / 100).max(1);
    sent.is_multiple_of(step)
}

// ── Received-file storage ─────────────────────────────────────────────────────

/// Directory received files land in. Mirrors `ui::bridge::save_received_file`.
pub fn download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
}

/// Basename of `rel_path`, or a safe fallback.
///
/// Taking only the file name is the path-traversal defence: a sender cannot
/// steer bytes outside the download directory with `../` or an absolute path.
/// Both `/` and `\` are separators here, regardless of host OS — offers arrive
/// from every platform, and `Path::file_name` would leave a Windows path
/// intact on Unix.
pub fn safe_file_name(rel_path: &str) -> String {
    rel_path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .map(str::to_string)
        .filter(|n| !n.is_empty() && n != "." && n != "..")
        .unwrap_or_else(|| "received_file".to_owned())
}

/// Create the sparse `.part` file a streaming transfer writes into.
///
/// Keyed by `transfer_id` so two concurrent offers of the same filename cannot
/// scribble over each other.
fn open_part_file(rel_path: &str, transfer_id: &str) -> std::io::Result<(File, PathBuf)> {
    let dir = download_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.{transfer_id}.part", safe_file_name(rel_path)));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)?;
    Ok((file, path))
}

/// Rename a verified `.part` file to its final, non-colliding name.
fn promote_part_file(part: &Path, rel_path: &str) -> std::io::Result<PathBuf> {
    let dir = part
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(download_dir);
    let dest = unique_dest_path(&dir, &safe_file_name(rel_path));
    std::fs::rename(part, &dest)?;
    Ok(dest)
}

/// First free `name`, `name (1)`, `name (2)`… in `dir`.
///
/// Received files used to overwrite an existing file of the same name silently;
/// two people sending `clip.mp4` would clobber each other.
pub fn unique_dest_path(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.to_owned());
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 1..10_000u32 {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    // Pathological: fall back to the plain name rather than looping forever.
    dir.join(file_name)
}

// ── Crypto helpers (wire-compatible with conquerd-crypto::transfer) ───────────

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex::encode(digest)
}

/// SHA-256 of a file, read in [`CHUNK_SIZE`] blocks so a 250 MB file costs one
/// chunk buffer rather than 250 MB.
fn sha256_hex_file(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("Read failed on {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Adaptive zlib compression.  Returns `(output, was_compressed)`.
fn compress_data(data: &[u8]) -> (Vec<u8>, bool) {
    if data.len() < COMPRESSION_THRESHOLD {
        return (data.to_vec(), false);
    }
    match zlib_compress(data, 6) {
        Ok(compressed) if compressed.len() < (data.len() * 9 / 10) => (compressed, true),
        Ok(_) | Err(_) => (data.to_vec(), false),
    }
}

fn zlib_compress(data: &[u8], level: u32) -> Result<Vec<u8>, String> {
    let mut enc = ZlibEncoder::new(
        Vec::with_capacity(data.len() / 2 + 64),
        Compression::new(level),
    );
    enc.write_all(data)
        .map_err(|e| format!("zlib write: {e}"))?;
    enc.finish().map_err(|e| format!("zlib finish: {e}"))
}

fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut dec = ZlibDecoder::new(data);
    let mut out = Vec::with_capacity(data.len().min(MAX_TRANSFER_SIZE));
    // Cap decompression output to prevent decompression-bomb DoS: a crafted
    // zlib stream can otherwise expand to gigabytes from a tiny payload.
    use std::io::Read as _;
    dec.by_ref()
        .take((MAX_TRANSFER_SIZE + 1) as u64)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() > MAX_TRANSFER_SIZE {
        return Err(format!(
            "decompressed output exceeds {MAX_TRANSFER_SIZE} bytes"
        ));
    }
    Ok(out)
}

/// Compute a binary delta (CDv1 format).  Returns `None` if delta is not
/// smaller than compressed full content.
fn create_delta(old: &[u8], new: &[u8]) -> Option<Vec<u8>> {
    let opcodes = diff_opcodes(old, new);
    let compressed = zlib_compress(&opcodes, 6).ok()?;
    let mut delta = Vec::with_capacity(DELTA_MAGIC.len() + compressed.len());
    delta.extend_from_slice(DELTA_MAGIC);
    delta.extend_from_slice(&compressed);

    let full_compressed = zlib_compress(new, 6).ok()?;
    if delta.len() < full_compressed.len() {
        Some(delta)
    } else {
        None
    }
}

/// Apply a CDv1 delta to `old`, producing `new`.
fn apply_delta(old: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    if delta.len() < DELTA_MAGIC.len() || &delta[..4] != DELTA_MAGIC {
        return Err("Invalid delta magic".into());
    }
    let raw = zlib_decompress(&delta[4..])?;
    if raw.is_empty() {
        return Err("Empty delta opcode stream".into());
    }
    let mut out = Vec::with_capacity(old.len());
    let mut off = 0usize;
    // Cap reconstructed output: a malicious peer could otherwise craft a
    // small delta whose COPY opcodes expand to gigabytes (denial of service
    // via memory exhaustion).
    let max_out = MAX_TRANSFER_SIZE;
    while off < raw.len() {
        let tag = raw[off];
        off += 1;
        match tag {
            0 => {
                if off + 8 > raw.len() {
                    return Err("truncated COPY".into());
                }
                let src_arr: [u8; 4] = raw[off..off + 4]
                    .try_into()
                    .map_err(|_| "truncated COPY src".to_string())?;
                let len_arr: [u8; 4] = raw[off + 4..off + 8]
                    .try_into()
                    .map_err(|_| "truncated COPY len".to_string())?;
                let src = u32::from_be_bytes(src_arr) as usize;
                let len = u32::from_be_bytes(len_arr) as usize;
                off += 8;
                if src.saturating_add(len) > old.len() {
                    return Err("COPY out of range".into());
                }
                if out.len().saturating_add(len) > max_out {
                    return Err("delta output exceeds MAX_TRANSFER_SIZE".into());
                }
                out.extend_from_slice(&old[src..src + len]);
            }
            1 => {
                if off + 4 > raw.len() {
                    return Err("truncated INSERT".into());
                }
                let len_arr: [u8; 4] = raw[off..off + 4]
                    .try_into()
                    .map_err(|_| "truncated INSERT len".to_string())?;
                let len = u32::from_be_bytes(len_arr) as usize;
                off += 4;
                if off + len > raw.len() {
                    return Err("INSERT past end".into());
                }
                if out.len().saturating_add(len) > max_out {
                    return Err("delta output exceeds MAX_TRANSFER_SIZE".into());
                }
                out.extend_from_slice(&raw[off..off + len]);
                off += len;
            }
            other => return Err(format!("Unknown opcode: {other}")),
        }
    }
    Ok(out)
}

const WINDOW: usize = 16;

fn diff_opcodes(old: &[u8], new: &[u8]) -> Vec<u8> {
    if old == new {
        let mut buf = Vec::with_capacity(9);
        write_copy(&mut buf, 0, new.len() as u32);
        return buf;
    }
    // Build window index
    let mut idx: HashMap<&[u8], u32> = HashMap::new();
    if old.len() >= WINDOW {
        for i in 0..=(old.len() - WINDOW) {
            idx.entry(&old[i..i + WINDOW]).or_insert(i as u32);
        }
    }
    let mut buf = Vec::with_capacity(new.len() / 4 + 32);
    let mut pending_start = 0usize;
    let mut j = 0usize;
    while j + WINDOW <= new.len() {
        let key = &new[j..j + WINDOW];
        if let Some(&src) = idx.get(key) {
            let src = src as usize;
            let mlen = extend_match(old, src, new, j);
            if mlen >= WINDOW {
                if pending_start < j {
                    write_insert(&mut buf, &new[pending_start..j]);
                }
                write_copy(&mut buf, src as u32, mlen as u32);
                j += mlen;
                pending_start = j;
                continue;
            }
        }
        j += 1;
    }
    if pending_start < new.len() {
        write_insert(&mut buf, &new[pending_start..]);
    }
    buf
}

fn extend_match(old: &[u8], src: usize, new: &[u8], dst: usize) -> usize {
    let max = (old.len() - src).min(new.len() - dst);
    let mut n = 0;
    while n < max && old[src + n] == new[dst + n] {
        n += 1;
    }
    n
}

fn write_copy(buf: &mut Vec<u8>, src: u32, len: u32) {
    buf.push(0);
    buf.extend_from_slice(&src.to_be_bytes());
    buf.extend_from_slice(&len.to_be_bytes());
}

fn write_insert(buf: &mut Vec<u8>, seg: &[u8]) {
    if seg.is_empty() {
        return;
    }
    buf.push(1);
    buf.extend_from_slice(&(seg.len() as u32).to_be_bytes());
    buf.extend_from_slice(seg);
}

fn unix_now_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reject_reason(evs: &[TransferEvent]) -> Option<String> {
        evs.iter().find_map(|ev| match ev {
            TransferEvent::SendMessage {
                message_type: MessageType::FileTransferReject,
                payload,
                ..
            } => Some(
                payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            ),
            _ => None,
        })
    }

    /// The sender chunks the *compressed* payload, so `total_chunks` is smaller
    /// than `size / CHUNK_SIZE` implies. Equating the two rejected every
    /// compressed (and every delta) transfer as `invalid_chunk_count`, and
    /// neither side surfaced it — the peer simply never saw a pending transfer.
    #[test]
    fn compressed_offer_with_fewer_chunks_than_size_is_accepted() {
        let mut sender = FileTransferManager::new();
        // Several chunks long uncompressed, but highly compressible.
        let data = vec![b'a'; 4 * CHUNK_SIZE];
        let (transfer_id, events) = sender
            .offer_file("peer-1", "big.txt", data.clone(), "file", None, false)
            .expect("offer should be created");

        let offer = events
            .iter()
            .find_map(|ev| match ev {
                TransferEvent::SendMessage {
                    message_type: MessageType::FileTransferOffer,
                    payload,
                    ..
                } => Some(payload),
                _ => None,
            })
            .expect("offer event");

        let size = offer.get("size").and_then(Value::as_u64).unwrap() as usize;
        let total_chunks = offer.get("total_chunks").and_then(Value::as_u64).unwrap() as usize;
        assert_eq!(size, data.len(), "size is the ORIGINAL length");
        assert!(
            total_chunks < size.div_ceil(CHUNK_SIZE),
            "compression should shrink the chunk count: {total_chunks} vs size {size}"
        );

        let mut receiver = FileTransferManager::new();
        let evs = receiver.on_offer_received(
            "sender-peer",
            &transfer_id,
            "big.txt",
            offer.get("sha256").and_then(Value::as_str).unwrap(),
            size,
            total_chunks,
            "file",
            offer.get("compressed").and_then(Value::as_bool).unwrap(),
            offer.get("is_delta").and_then(Value::as_bool).unwrap(),
            "",
        );
        assert_eq!(
            reject_reason(&evs),
            None,
            "compressed offer must not be rejected"
        );
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, TransferEvent::Offered { .. })));
    }

    /// The bound still refuses an absurd chunk count (resource-exhaustion probe).
    #[test]
    fn absurd_chunk_count_is_still_rejected() {
        let mut receiver = FileTransferManager::new();
        let evs = receiver.on_offer_received(
            "sender-peer",
            "t1",
            "small.txt",
            &sha256_hex(b"hi"),
            2,
            100_000,
            "file",
            false,
            false,
            "",
        );
        assert_eq!(reject_reason(&evs).as_deref(), Some("invalid_chunk_count"));
    }

    #[test]
    fn local_accept_supports_push_mode_room_transfer() {
        let mut sender = FileTransferManager::new();
        let data = b"room broadcast file payload".repeat(4);
        let (transfer_id, outbound_events) = sender
            .offer_file("room-1", "note.txt", data.clone(), "room_file", None, true)
            .expect("offer should be created");

        let mut receiver = FileTransferManager::new();
        let offer_payload = outbound_events
            .iter()
            .find_map(|ev| match ev {
                TransferEvent::SendMessage {
                    message_type: MessageType::FileTransferOffer,
                    payload,
                    ..
                } => Some(payload),
                _ => None,
            })
            .expect("offer event");

        let mut evs = receiver.on_offer_received(
            "sender-peer",
            offer_payload
                .get("transfer_id")
                .and_then(Value::as_str)
                .unwrap(),
            offer_payload
                .get("rel_path")
                .and_then(Value::as_str)
                .unwrap(),
            offer_payload.get("sha256").and_then(Value::as_str).unwrap(),
            offer_payload.get("size").and_then(Value::as_u64).unwrap() as usize,
            offer_payload
                .get("total_chunks")
                .and_then(Value::as_u64)
                .unwrap() as usize,
            offer_payload
                .get("purpose")
                .and_then(Value::as_str)
                .unwrap(),
            offer_payload
                .get("compressed")
                .and_then(Value::as_bool)
                .unwrap(),
            offer_payload
                .get("is_delta")
                .and_then(Value::as_bool)
                .unwrap(),
            offer_payload
                .get("base_sha256")
                .and_then(Value::as_str)
                .unwrap(),
        );
        evs.extend(receiver.accept_transfer_locally(&transfer_id));
        assert!(evs
            .iter()
            .any(|ev| matches!(ev, TransferEvent::Offered { .. })));
        assert!(evs.iter().any(|ev| matches!(
            ev,
            TransferEvent::StateChanged { state, .. } if state == "transferring"
        )));

        for ev in &outbound_events {
            if let TransferEvent::SendMessage {
                message_type: MessageType::FileTransferChunk,
                payload,
                ..
            } = ev
            {
                let idx = payload.get("chunk_index").and_then(Value::as_u64).unwrap() as usize;
                let chunk = payload.get("data").and_then(Value::as_str).unwrap();
                receiver.on_chunk_received(&transfer_id, idx, chunk);
            }
        }

        let complete = receiver.on_complete_received(&transfer_id);
        assert!(complete.iter().any(|ev| matches!(
            ev,
            TransferEvent::Complete {
                payload: TransferPayload::Bytes(completed),
                rel_path,
                ..
            } if completed == &data && rel_path == "note.txt"
        )));
    }

    /// A file above `INLINE_MAX` streams: chunks are read from the source on
    /// demand and written straight to their final offsets in a `.part` file,
    /// which is verified and renamed on completion. Nothing on either side ever
    /// holds the whole payload.
    #[test]
    fn streaming_transfer_round_trips_via_disk() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("clip.bin");
        // Just over INLINE_MAX so the streaming path is selected, with varied
        // bytes so a mis-ordered chunk would break the hash.
        let size = INLINE_MAX + 3 * CHUNK_SIZE + 1234;
        {
            let mut f = std::fs::File::create(&src_path).unwrap();
            let block: Vec<u8> = (0..CHUNK_SIZE).map(|i| (i % 251) as u8).collect();
            let mut written = 0usize;
            while written < size {
                let n = block.len().min(size - written);
                f.write_all(&block[..n]).unwrap();
                written += n;
            }
        }
        let expected_sha = sha256_hex_file(&src_path).unwrap();

        let mut sender = FileTransferManager::new();
        let (transfer_id, offer_events) = sender
            .offer_file_from_path("room-1", "clip.bin", &src_path, "room_file", false)
            .expect("offer should be created");

        let offer = offer_events
            .iter()
            .find_map(|ev| match ev {
                TransferEvent::SendMessage {
                    message_type: MessageType::FileTransferOffer,
                    payload,
                    ..
                } => Some(payload),
                _ => None,
            })
            .expect("offer event");

        // Advertisement only: an unrequested offer must not emit any chunk.
        assert!(
            !offer_events.iter().any(|ev| matches!(
                ev,
                TransferEvent::SendMessage {
                    message_type: MessageType::FileTransferChunk,
                    ..
                }
            )),
            "room offers are advertisements — no chunk until requested"
        );
        assert_eq!(
            offer.get("size").and_then(Value::as_u64).unwrap(),
            size as u64
        );
        assert!(!offer.get("compressed").and_then(Value::as_bool).unwrap());
        let total_chunks = offer.get("total_chunks").and_then(Value::as_u64).unwrap() as usize;
        assert_eq!(total_chunks, size.div_ceil(CHUNK_SIZE));

        let mut receiver = FileTransferManager::new();
        let evs = receiver.on_offer_received_with_room(
            "sender-peer",
            "room-1",
            "SN-A",
            &transfer_id,
            "clip.bin",
            &expected_sha,
            size,
            total_chunks,
            "room_file",
            false,
            false,
            "",
        );
        assert_eq!(
            reject_reason(&evs),
            None,
            "streaming offer must be accepted"
        );
        receiver.accept_transfer_locally(&transfer_id);

        // The receiver's accept becomes an SfuFileRequest; that is what starts
        // the stream. Then drain a slice at a time as the pump does, delivering
        // out of order to prove offsets (not arrival order) place the bytes.
        let mut chunks: Vec<(usize, String)> = Vec::new();
        let first = sender.start_stream_for(&transfer_id, "receiver-peer", ROOM_FILE_CHUNK_BUDGET);
        let mut done = false;
        for ev in first {
            if let TransferEvent::SendMessage {
                message_type: MessageType::FileTransferChunk,
                payload,
                ..
            } = ev
            {
                let idx = payload.get("chunk_index").and_then(Value::as_u64).unwrap() as usize;
                // Every frame must name the requester so the supernode can
                // narrow delivery instead of broadcasting to the whole room.
                assert_eq!(
                    payload.get("to").and_then(Value::as_str),
                    Some("receiver-peer")
                );
                chunks.push((
                    idx,
                    payload
                        .get("data")
                        .and_then(Value::as_str)
                        .unwrap()
                        .to_owned(),
                ));
            }
        }
        while !done {
            let (evs, d) = sender.pump_stream(&transfer_id, ROOM_FILE_CHUNK_BUDGET);
            done = d;
            for ev in evs {
                if let TransferEvent::SendMessage {
                    message_type: MessageType::FileTransferChunk,
                    payload,
                    ..
                } = ev
                {
                    let idx = payload.get("chunk_index").and_then(Value::as_u64).unwrap() as usize;
                    let data = payload
                        .get("data")
                        .and_then(Value::as_str)
                        .unwrap()
                        .to_owned();
                    chunks.push((idx, data));
                }
            }
        }
        assert_eq!(chunks.len(), total_chunks);
        chunks.reverse();
        for (idx, data) in &chunks {
            receiver.on_chunk_received(&transfer_id, *idx, data);
        }

        let complete = receiver.on_complete_received(&transfer_id);
        let saved = complete
            .iter()
            .find_map(|ev| match ev {
                TransferEvent::Complete {
                    payload: TransferPayload::SavedAt { path, len },
                    ..
                } => Some((path.clone(), *len)),
                _ => None,
            })
            .expect("streamed transfer completes with a saved path");
        assert_eq!(saved.1, size as u64);
        let saved_path = PathBuf::from(&saved.0);
        assert_eq!(sha256_hex_file(&saved_path).unwrap(), expected_sha);
        assert!(
            !saved_path.to_string_lossy().ends_with(".part"),
            "the .part file must be renamed on success"
        );
        let _ = std::fs::remove_file(&saved_path);
    }

    /// Finished transfers used to stay in both maps forever, holding their full
    /// payloads for the process lifetime.
    #[test]
    fn gc_evicts_finished_transfers() {
        let mut mgr = FileTransferManager::new();
        let data = b"payload".repeat(64);
        let (transfer_id, _) = mgr
            .offer_file("peer-1", "note.txt", data, "file", None, true)
            .unwrap();
        // Auto-push ran to completion, so the record is terminal.
        mgr.outbound.get_mut(&transfer_id).unwrap().state = TransferState::Complete;
        mgr.outbound.get_mut(&transfer_id).unwrap().finished_at =
            Some(unix_now_f64() - TRANSFER_RETAIN_SECS - 1.0);
        assert_eq!(mgr.gc(), 1);
        assert!(mgr.outbound.is_empty());
    }

    #[test]
    fn unique_dest_path_does_not_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let first = unique_dest_path(dir.path(), "clip.mp4");
        assert_eq!(first.file_name().unwrap(), "clip.mp4");
        std::fs::write(&first, b"x").unwrap();
        let second = unique_dest_path(dir.path(), "clip.mp4");
        assert_eq!(second.file_name().unwrap(), "clip (1).mp4");
    }

    /// Quota used to drop chunk frames after `chunks_sent` had already moved,
    /// so the receiver stalled with holes. Rewinding must make those chunks
    /// eligible for the next pump.
    #[test]
    fn unsend_chunks_lets_pump_retry() {
        let mut mgr = FileTransferManager::new();
        let data = b"hello world".repeat(80);
        let (tid, _) = mgr
            .offer_file("peer-1", "a.bin", data.to_vec(), "file", None, false)
            .unwrap();
        let _ = mgr.on_transfer_accepted(&tid);
        let sent = mgr.outbound.get(&tid).unwrap().chunks_sent;
        assert!(sent > 0);
        mgr.unsend_chunks(&tid, sent);
        assert_eq!(mgr.outbound.get(&tid).unwrap().chunks_sent, 0);
        assert!(mgr.active_outbound_streams().contains(&tid));
        let (evs, _) = mgr.pump_stream(&tid, 8);
        assert!(evs.iter().any(|e| matches!(
            e,
            TransferEvent::SendMessage {
                message_type: MessageType::FileTransferChunk,
                ..
            }
        )));
    }

    /// Deleting a file message withdraws the share: because the relay caches
    /// nothing and the sender holds the only copy, a peer who has not
    /// downloaded yet can no longer get the file from anyone.
    #[test]
    fn revoking_an_offer_refuses_later_requests() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("secret.bin");
        std::fs::write(&src, vec![1u8; 4096]).unwrap();

        let mut sender = FileTransferManager::new();
        let (transfer_id, _) = sender
            .offer_file_from_path_with_id(
                "room-1",
                "secret.bin",
                &src,
                "room_file",
                false,
                Some("tid-abc"),
            )
            .unwrap();
        assert_eq!(transfer_id, "tid-abc", "caller-chosen id must be honoured");
        assert!(sender.has_outbound(&transfer_id));

        // A request before revocation streams normally.
        assert!(!sender
            .start_stream_for(&transfer_id, "early-peer", ROOM_FILE_CHUNK_BUDGET)
            .is_empty());

        assert!(sender.revoke_outbound(&transfer_id));
        assert!(!sender.has_outbound(&transfer_id));
        // A later acceptor gets nothing at all.
        assert!(sender
            .start_stream_for(&transfer_id, "late-peer", ROOM_FILE_CHUNK_BUDGET)
            .is_empty());
        // Revoking twice is harmless and reports that nothing was withdrawn.
        assert!(!sender.revoke_outbound(&transfer_id));
    }

    /// Declining must delete the `.part` file, not leave it in Downloads.
    #[test]
    fn discarding_an_inbound_transfer_removes_its_part_file() {
        let mut receiver = FileTransferManager::new();
        let size = INLINE_MAX + CHUNK_SIZE;
        let evs = receiver.on_offer_received_with_room(
            "sender-peer",
            "room-1",
            "SN-A",
            "tid-part",
            "big.bin",
            &"0".repeat(64),
            size,
            size.div_ceil(CHUNK_SIZE),
            "room_file",
            false,
            false,
            "",
        );
        assert_eq!(reject_reason(&evs), None);
        let part = download_dir().join("big.bin.tid-part.part");
        assert!(part.exists(), "streaming offer opens a .part file");
        receiver.discard_inbound("tid-part");
        assert!(!part.exists(), "declining must not leave a .part behind");
    }

    /// Path traversal must not escape the download directory.
    ///
    /// Windows-separator cases must hold on Unix too: `Path::file_name` does
    /// not treat `\` as a separator there, which is how this test (and the
    /// defence) used to fail on Linux/macOS CI.
    #[test]
    fn safe_file_name_strips_directories() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("/etc/shadow"), "shadow");
        assert_eq!(safe_file_name("C:\\Windows\\system32\\a.dll"), "a.dll");
        assert_eq!(safe_file_name("..\\..\\windows\\a.dll"), "a.dll");
        assert_eq!(safe_file_name("foo/bar\\baz.txt"), "baz.txt");
        assert_eq!(safe_file_name(""), "received_file");
        assert_eq!(safe_file_name("///"), "received_file");
    }
}
