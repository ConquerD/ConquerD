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

use std::collections::HashMap;
use std::io::{Read, Write};
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
/// Maximum accepted file size (10 MiB).
const MAX_TRANSFER_SIZE: usize = 10 * 1024 * 1024;
/// Minimum input size before compression is attempted.
const COMPRESSION_THRESHOLD: usize = 1024;
/// Delta wire-format magic (identical to Rust crypto crate).
const DELTA_MAGIC: &[u8; 4] = b"CDv1";

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

pub struct OutboundTransfer {
    pub transfer_id: String,
    pub peer_id: String,
    pub rel_path: String,
    /// Payload bytes (possibly compressed / delta-encoded).
    pub data: Vec<u8>,
    /// SHA-256 hex of the *original* uncompressed content.
    pub sha256: String,
    pub total_chunks: usize,
    pub chunks_sent: usize,
    pub state: TransferState,
    pub purpose: String,
    pub compressed: bool,
    pub is_delta: bool,
    pub created_at: f64,
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

pub struct InboundTransfer {
    pub transfer_id: String,
    pub peer_id: String,
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
    /// Received chunk data indexed by chunk index.
    chunks: HashMap<usize, Vec<u8>>,
}

impl InboundTransfer {
    #[allow(clippy::too_many_arguments)]
    fn new(
        transfer_id: String,
        peer_id: String,
        rel_path: String,
        expected_sha256: String,
        expected_size: usize,
        total_chunks: usize,
        purpose: String,
        compressed: bool,
        is_delta: bool,
        base_sha256: String,
    ) -> Self {
        Self {
            transfer_id,
            peer_id,
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
            chunks: HashMap::new(),
        }
    }

    /// Store a chunk; returns `true` if new, `false` if duplicate.
    fn store_chunk(&mut self, index: usize, data: Vec<u8>) -> bool {
        if index >= self.total_chunks {
            return false;
        }
        if self.chunks.contains_key(&index) {
            return false;
        }
        self.chunks.insert(index, data);
        true
    }

    fn chunks_received(&self) -> usize {
        self.chunks.len()
    }

    fn progress(&self) -> f64 {
        if self.total_chunks == 0 {
            return 0.0;
        }
        self.chunks_received() as f64 / self.total_chunks as f64
    }

    /// Reassemble all chunks in order.
    fn reassemble(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..self.total_chunks {
            if let Some(c) = self.chunks.get(&i) {
                out.extend_from_slice(c);
            }
        }
        out
    }
}

// ── Events emitted by FileTransferManager ────────────────────────────────────

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
        data: Vec<u8>,
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
        if data.len() > MAX_TRANSFER_SIZE {
            return Err(format!(
                "File too large ({} bytes, max {MAX_TRANSFER_SIZE})",
                data.len()
            ));
        }

        let transfer_id = Uuid::new_v4().simple().to_string()[..16].to_owned();
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

        let total_chunks = payload.len().div_ceil(CHUNK_SIZE).max(1);

        let mut xfer = OutboundTransfer {
            transfer_id: transfer_id.clone(),
            peer_id: peer_id.to_owned(),
            rel_path: rel_path.to_owned(),
            data: payload.clone(),
            sha256: original_sha.clone(),
            total_chunks,
            chunks_sent: 0,
            state: TransferState::Pending,
            purpose: purpose.to_owned(),
            compressed,
            is_delta,
            created_at: unix_now_f64(),
        };

        let mut events = Vec::new();

        // Build the OFFER message
        let mut offer_payload = serde_json::Map::new();
        offer_payload.insert("transfer_id".into(), Value::String(transfer_id.clone()));
        offer_payload.insert("rel_path".into(), Value::String(rel_path.to_owned()));
        offer_payload.insert("sha256".into(), Value::String(original_sha));
        offer_payload.insert("size".into(), Value::Number(original_size.into()));
        offer_payload.insert("payload_size".into(), Value::Number(payload.len().into()));
        offer_payload.insert("total_chunks".into(), Value::Number(total_chunks.into()));
        offer_payload.insert("purpose".into(), Value::String(purpose.to_owned()));
        offer_payload.insert("compressed".into(), Value::Bool(compressed));
        offer_payload.insert("is_delta".into(), Value::Bool(is_delta));
        offer_payload.insert("base_sha256".into(), Value::String(base_sha256));
        events.push(TransferEvent::SendMessage {
            peer_id: peer_id.to_owned(),
            message_type: MessageType::FileTransferOffer,
            payload: offer_payload,
        });

        if auto_push {
            xfer.state = TransferState::Transferring;
            events.extend(build_chunk_events(&xfer));
        }

        self.outbound.insert(transfer_id.clone(), xfer);
        Ok((transfer_id, events))
    }

    /// Peer accepted our offer → send chunks.
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
        evs.extend(build_chunk_events(xfer));
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
        // Reject inconsistent total_chunks: a peer claiming millions of chunks
        // for a tiny file is a protocol error (or a resource-exhaustion probe).
        let expected_chunks = size.div_ceil(CHUNK_SIZE).max(1);
        if total_chunks != expected_chunks {
            let mut p = serde_json::Map::new();
            p.insert("transfer_id".into(), Value::String(transfer_id.to_owned()));
            p.insert("reason".into(), Value::String("invalid_chunk_count".into()));
            warn!(
                "Rejected transfer {transfer_id}: total_chunks {total_chunks} \
                 != expected {expected_chunks} for size {size}"
            );
            return vec![TransferEvent::SendMessage {
                peer_id: peer_id.to_owned(),
                message_type: MessageType::FileTransferReject,
                payload: p,
            }];
        }

        let xfer = InboundTransfer::new(
            transfer_id.to_owned(),
            peer_id.to_owned(),
            rel_path.to_owned(),
            sha256.to_owned(),
            size,
            total_chunks,
            purpose.to_owned(),
            compressed,
            is_delta,
            base_sha256.to_owned(),
        );

        let mut evs = vec![TransferEvent::Offered {
            transfer_id: transfer_id.to_owned(),
            peer_id: peer_id.to_owned(),
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
        let xfer = match self.inbound.get_mut(transfer_id) {
            Some(x) if x.state == TransferState::Transferring => x,
            _ => return vec![],
        };
        let chunk = match B64.decode(data_b64) {
            Ok(b) => b,
            Err(_) => return self.fail_inbound(transfer_id, "invalid base64 chunk"),
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
        let peer_id = match self.inbound.get_mut(transfer_id) {
            Some(xfer) => {
                xfer.state = TransferState::Complete;
                xfer.peer_id.clone()
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
                peer_id,
                message_type: MessageType::FileTransferAck,
                payload: p,
            },
            TransferEvent::Complete {
                transfer_id: transfer_id.to_owned(),
                data: assembled,
                rel_path: rel_path_owned,
            },
            TransferEvent::StateChanged {
                transfer_id: transfer_id.to_owned(),
                state: "complete".into(),
            },
        ]
    }

    // ── Private helpers ───────────────────────────────────────────────────

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

fn build_chunk_events(xfer: &OutboundTransfer) -> Vec<TransferEvent> {
    let mut evs = Vec::with_capacity(xfer.total_chunks + 1);
    for i in 0..xfer.total_chunks {
        let offset = i * CHUNK_SIZE;
        let end = (offset + CHUNK_SIZE).min(xfer.data.len());
        let chunk = &xfer.data[offset..end];
        let encoded = B64.encode(chunk);

        let mut p = serde_json::Map::new();
        p.insert(
            "transfer_id".into(),
            Value::String(xfer.transfer_id.clone()),
        );
        p.insert("chunk_index".into(), Value::Number(i.into()));
        p.insert("data".into(), Value::String(encoded));

        evs.push(TransferEvent::SendMessage {
            peer_id: xfer.peer_id.clone(),
            message_type: MessageType::FileTransferChunk,
            payload: p,
        });
        evs.push(TransferEvent::Progress {
            transfer_id: xfer.transfer_id.clone(),
            progress: (i + 1) as f64 / xfer.total_chunks as f64,
        });
    }
    // COMPLETE frame
    let mut p = serde_json::Map::new();
    p.insert(
        "transfer_id".into(),
        Value::String(xfer.transfer_id.clone()),
    );
    evs.push(TransferEvent::SendMessage {
        peer_id: xfer.peer_id.clone(),
        message_type: MessageType::FileTransferComplete,
        payload: p,
    });
    evs
}

// ── Crypto helpers (wire-compatible with conquerd-crypto::transfer) ───────────

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex::encode(digest)
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
                data: completed,
                rel_path,
                ..
            } if completed == &data && rel_path == "note.txt"
        )));
    }
}
