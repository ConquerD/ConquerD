// ConquerD Supernode — sfu.rs
// SFU room management: room lifecycle, participant tracking, room types.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::crypto::generate_nonce_hex;

/// Max participants per SFU room.
pub const MAX_ROOM_SIZE: usize = 32;
/// Default room ID (always-present lobby for voice + chat).
pub const DEFAULT_ROOM_ID: &str = "default";
/// Display name for [`DEFAULT_ROOM_ID`] — created on every SFU-enabled node at startup.
pub const DEFAULT_ROOM_NAME: &str = "Public Voice/Chat Room";
/// Remove user-created SFU rooms after this many seconds with no voice or chat subscribers.
pub const IDLE_ROOM_GC_SECS: f64 = 900.0;

/// Maximum number of simultaneous talkers the SFU forwards per room. Audio
/// frames from speakers beyond this cap are dropped server-side (the receiver
/// fills the brief gap with Opus PLC), so each member's inbound stream count —
/// and therefore decode load and bandwidth — stays bounded no matter how large
/// the room is. Rooms with at most this many concurrent talkers (the common
/// case) are unaffected: every active speaker is forwarded.
pub const MAX_ACTIVE_SPEAKERS: usize = 5;

/// Half-life (seconds of silence) of a speaker's activity score. A speaker who
/// keeps sending frames accumulates a high score; one who pauses decays toward
/// zero, so the active set tracks *sustained* talkers rather than brief blips.
const SPEAKER_SCORE_HALF_LIFE_SECS: f64 = 2.0;

/// Drop a speaker from activity tracking (and free its active slot) after this
/// long with no audio frames. Matches the client's per-peer silence timeout.
const SPEAKER_SILENCE_SECS: f64 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoomType {
    Public,
    Private,
}

/// Normalize a client-supplied invite-policy string to a known value.
/// Anything other than exactly `"members"` (including absent/empty/garbled
/// input) normalizes to the safe `"owner"` default.
pub fn normalize_invite_policy(policy: &str) -> String {
    if policy == "members" {
        "members".to_owned()
    } else {
        "owner".to_owned()
    }
}

/// Decaying activity score for one speaker, used to pick the room's top-K
/// active talkers without the supernode ever decoding the (opaque) Opus audio.
#[derive(Debug, Clone, Copy)]
struct SpeakerScore {
    /// Accumulated, time-decayed activity (one unit per forwarded-frame attempt).
    score: f64,
    /// Wall-clock seconds of the last audio frame seen from this speaker.
    last_active: f64,
}

impl SpeakerScore {
    /// Score decayed forward to `now` (does not mutate).
    fn effective(&self, now: f64) -> f64 {
        let dt = (now - self.last_active).max(0.0);
        self.score * 0.5f64.powf(dt / SPEAKER_SCORE_HALF_LIFE_SECS)
    }
}

/// A single SFU room.
#[derive(Debug, Clone)]
pub struct SFURoom {
    pub room_id: String,
    pub room_name: String,
    pub room_type: RoomType,
    pub creator_id: String,
    /// Invite-mint policy: `"owner"` (only `creator_id` may mint — the safe
    /// default) or `"members"` (any current participant, chat subscriber,
    /// already-`allow`ed peer, or the creator may mint). Normalized at
    /// creation time via [`normalize_invite_policy`]; unknown/absent values
    /// fall back to `"owner"`.
    pub invite_policy: String,
    /// identity_pub → participant index
    participants: HashMap<String, u8>,
    /// Active-speaker tracking: sender → decaying activity score.
    speaker_scores: HashMap<String, SpeakerScore>,
    /// Currently committed active-speaker set (≤ [`MAX_ACTIVE_SPEAKERS`]).
    /// Kept sticky across frames to avoid per-frame flapping; re-evaluated on
    /// each inbound frame against [`Self::speaker_scores`].
    active_speakers: Vec<String>,
    /// Text-chat subscribers (not voice-joined)
    subscribers: std::collections::HashSet<String>,
    /// Allowed peers for private rooms
    allowed: std::collections::HashSet<String>,
    /// Invite tokens: token → InviteToken
    invite_tokens: HashMap<String, InviteToken>,
    #[allow(dead_code)]
    pub created_at: f64,
    /// When the room last had zero voice participants and zero chat subscribers.
    /// `None` while the room is in use. Used for idle GC of peer-materialized rooms.
    empty_since: Option<f64>,
    next_index: u8,
}

#[derive(Debug, Clone)]
struct InviteToken {
    #[allow(dead_code)]
    created_by: String,
    uses: u32,
    max_uses: u32,
}

impl SFURoom {
    pub fn new(room_id: &str, room_name: &str, room_type: RoomType, creator_id: &str) -> Self {
        Self {
            room_id: room_id.to_string(),
            room_name: room_name.to_string(),
            room_type,
            creator_id: creator_id.to_string(),
            invite_policy: "owner".to_string(),
            participants: HashMap::new(),
            speaker_scores: HashMap::new(),
            active_speakers: Vec::new(),
            subscribers: std::collections::HashSet::new(),
            allowed: std::collections::HashSet::new(),
            invite_tokens: HashMap::new(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
            empty_since: None,
            next_index: 1,
        }
    }

    fn now_secs() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    }

    pub fn is_unused(&self) -> bool {
        self.participants.is_empty() && self.subscribers.is_empty()
    }

    pub(crate) fn mark_in_use(&mut self) {
        self.empty_since = None;
    }

    pub(crate) fn mark_unused_if_empty(&mut self) {
        if self.is_unused() {
            self.empty_since.get_or_insert_with(Self::now_secs);
        }
    }

    pub fn is_public(&self) -> bool {
        self.room_type == RoomType::Public
    }

    pub fn is_peer_allowed(&self, peer_id: &str) -> bool {
        self.is_public()
            || self.allowed.contains(peer_id)
            || self.creator_id == peer_id
            || self.participants.contains_key(peer_id)
    }

    /// Whether `peer_id` counts as a "member" of this room for the
    /// `"members"` invite-policy widening: the creator, an already-`allow`ed
    /// peer, a voice participant, or a text-chat subscriber. Deliberately
    /// distinct from [`Self::is_peer_allowed`] (which also admits anyone to a
    /// *public* room) — invite-minting eligibility is about actual membership,
    /// not room visibility.
    pub fn is_invite_eligible_member(&self, peer_id: &str) -> bool {
        self.creator_id == peer_id
            || self.allowed.contains(peer_id)
            || self.participants.contains_key(peer_id)
            || self.subscribers.contains(peer_id)
    }

    #[allow(dead_code)]
    pub fn allow_peer(&mut self, peer_id: &str) {
        self.allowed.insert(peer_id.to_string());
    }

    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }

    pub fn is_full(&self) -> bool {
        self.participants.len() >= MAX_ROOM_SIZE
    }

    pub fn participant_ids(&self) -> Vec<String> {
        self.participants.keys().cloned().collect()
    }

    pub fn add_participant(&mut self, peer_id: &str) -> bool {
        // Idempotent: if the peer is already a voice participant (common
        // reconnect race where the old WS hasn't been cleaned up yet), return
        // true so the caller still sends a fresh SfuMembers snapshot back to
        // the joiner.  This prevents the "don't see myself in the room" bug.
        if self.participants.contains_key(peer_id) {
            return true;
        }
        if self.is_full() {
            return false;
        }
        let idx = self.next_index;
        self.next_index = self.next_index.wrapping_add(1).max(1);
        self.participants.insert(peer_id.to_string(), idx);
        // Promoted to full participant — remove from text-only subscribers.
        self.subscribers.remove(peer_id);
        self.mark_in_use();
        true
    }

    pub fn remove_participant(&mut self, peer_id: &str) -> bool {
        let removed = self.participants.remove(peer_id).is_some();
        if removed {
            self.mark_unused_if_empty();
            self.speaker_scores.remove(peer_id);
            self.active_speakers.retain(|id| id != peer_id);
        }
        removed
    }

    /// Record an inbound audio frame from `sender` and decide whether the SFU
    /// should forward it this tick. Returns `false` only when the room already
    /// has [`MAX_ACTIVE_SPEAKERS`] louder/longer-sustained talkers — i.e. the
    /// frame is shed server-side to bound per-receiver fan-out.
    ///
    /// The decision is energy-free: it ranks speakers by a decaying count of
    /// recent frames (Opus DTX means a silent mic sends nothing, so "frames
    /// arriving" is a good proxy for "currently talking"). A committed active
    /// set provides hysteresis; a new talker only displaces an active one once
    /// it strictly out-scores the weakest current member.
    pub fn note_audio_should_forward(&mut self, sender: &str, now: f64) -> bool {
        // Decay + bump the sender's score.
        {
            let e = self
                .speaker_scores
                .entry(sender.to_owned())
                .or_insert(SpeakerScore {
                    score: 0.0,
                    last_active: now,
                });
            let dt = (now - e.last_active).max(0.0);
            e.score = e.score * 0.5f64.powf(dt / SPEAKER_SCORE_HALF_LIFE_SECS) + 1.0;
            e.last_active = now;
        }

        // Drop long-silent speakers from tracking and the active set so their
        // slots free up for whoever talks next.
        self.speaker_scores
            .retain(|_, s| now - s.last_active <= SPEAKER_SILENCE_SECS);
        let scores = &self.speaker_scores;
        self.active_speakers.retain(|id| scores.contains_key(id));

        // Already an active speaker → forward.
        if self.active_speakers.iter().any(|id| id == sender) {
            return true;
        }
        // Free slot → admit.
        if self.active_speakers.len() < MAX_ACTIVE_SPEAKERS {
            self.active_speakers.push(sender.to_owned());
            return true;
        }
        // Full: displace the weakest active speaker iff this sender now strictly
        // out-scores them (hysteresis prevents flapping between equal talkers).
        let sender_eff = self
            .speaker_scores
            .get(sender)
            .map(|s| s.effective(now))
            .unwrap_or(0.0);
        let mut weakest_idx = 0usize;
        let mut weakest_eff = f64::MAX;
        for (i, id) in self.active_speakers.iter().enumerate() {
            let eff = self
                .speaker_scores
                .get(id)
                .map(|s| s.effective(now))
                .unwrap_or(0.0);
            if eff < weakest_eff {
                weakest_eff = eff;
                weakest_idx = i;
            }
        }
        if sender_eff > weakest_eff {
            self.active_speakers[weakest_idx] = sender.to_owned();
            true
        } else {
            false
        }
    }

    /// Subscribe a peer to text chat (without voice join).
    pub fn subscribe(&mut self, peer_id: &str) {
        // No-op if already a voice participant (they already receive chat).
        if !self.participants.contains_key(peer_id) {
            self.subscribers.insert(peer_id.to_string());
            self.mark_in_use();
        }
    }

    /// Unsubscribe a peer from text chat.
    pub fn unsubscribe(&mut self, peer_id: &str) {
        if self.subscribers.remove(peer_id) {
            self.mark_unused_if_empty();
        }
    }

    /// Remove a peer from both participants and subscribers.
    pub fn remove_peer_entirely(&mut self, peer_id: &str) -> bool {
        let was_participant = self.participants.remove(peer_id).is_some();
        let was_subscriber = self.subscribers.remove(peer_id);
        if was_participant {
            self.speaker_scores.remove(peer_id);
            self.active_speakers.retain(|id| id != peer_id);
        }
        if was_participant || was_subscriber {
            self.mark_unused_if_empty();
        }
        was_participant || was_subscriber
    }

    /// All peers who should receive text chat: participants + subscribers.
    pub fn chat_recipient_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.participants.keys().cloned().collect();
        for sub in &self.subscribers {
            ids.push(sub.clone());
        }
        ids
    }

    pub fn generate_invite_token(&mut self, created_by: &str, max_uses: u32) -> String {
        let token = generate_nonce_hex(16);
        self.invite_tokens.insert(
            token.clone(),
            InviteToken {
                created_by: created_by.to_string(),
                uses: 0,
                max_uses,
            },
        );
        token
    }

    /// Re-seed a client-held invite credential after idle-GC / rematerialize.
    ///
    /// Room invite maps are in-memory only; GC wipes them. Clients keep the
    /// original token in their encrypted `RoomStore` and re-present it on
    /// materialize so returning members can rejoin without a fresh share.
    /// `max_uses = 0` means unlimited re-entry uses (the room definition is the
    /// trust root; rotate by minting a new invite / new room).
    ///
    /// Idempotent: re-registering the same token refreshes `created_by` and
    /// resets use counts so a previously single-use token becomes durable.
    pub fn reregister_invite_token(&mut self, token: &str, created_by: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        self.invite_tokens.insert(
            token.to_string(),
            InviteToken {
                created_by: created_by.to_string(),
                uses: 0,
                // 0 = unlimited — post-GC rejoin must not burn the credential.
                max_uses: 0,
            },
        );
        true
    }

    pub fn validate_and_consume_token(&mut self, token: &str, peer_id: &str) -> bool {
        let Some(it) = self.invite_tokens.get_mut(token) else {
            return false;
        };
        // max_uses == 0 means unlimited (durable re-entry / re-seeded token).
        if it.max_uses > 0 && it.uses >= it.max_uses {
            return false;
        }
        if it.max_uses > 0 {
            it.uses += 1;
        }
        self.allowed.insert(peer_id.to_string());
        if it.max_uses > 0 && it.uses >= it.max_uses {
            self.invite_tokens.remove(token);
        }
        true
    }
}

/// Manages all SFU rooms.
pub struct SFURoomManager {
    rooms: HashMap<String, SFURoom>,
}

impl SFURoomManager {
    pub fn new() -> Self {
        let mut mgr = Self {
            rooms: HashMap::new(),
        };
        // Built-in lobby (never idle-GC'd); peers cannot disable via room policy.
        let mut default_room =
            SFURoom::new(DEFAULT_ROOM_ID, DEFAULT_ROOM_NAME, RoomType::Public, "");
        default_room.empty_since = None;
        mgr.rooms.insert(DEFAULT_ROOM_ID.to_string(), default_room);
        mgr
    }

    /// Create or return an existing peer-materialized room, with the safe
    /// `"owner"` invite policy. Returns `(room, created_new)`.
    pub fn create_room(
        &mut self,
        room_id: Option<&str>,
        room_name: &str,
        room_type: RoomType,
        creator_id: &str,
    ) -> Option<(&SFURoom, bool)> {
        self.create_room_with_policy(room_id, room_name, room_type, creator_id, "owner")
    }

    /// Create or return an existing peer-materialized room with an explicit
    /// invite policy (`"owner"` or `"members"`; anything else normalizes to
    /// the safe `"owner"` default — see [`normalize_invite_policy`]). Returns
    /// `(room, created_new)`. An already-existing room's policy is not
    /// overwritten by a later create/materialize call.
    pub fn create_room_with_policy(
        &mut self,
        room_id: Option<&str>,
        room_name: &str,
        room_type: RoomType,
        creator_id: &str,
        invite_policy: &str,
    ) -> Option<(&SFURoom, bool)> {
        let id = room_id
            .map(String::from)
            .unwrap_or_else(|| crate::crypto::derive_room_id(creator_id, room_name));
        if self.rooms.contains_key(&id) {
            return self.rooms.get(&id).map(|r| (r, false));
        }
        let mut room = SFURoom::new(&id, room_name, room_type, creator_id);
        room.invite_policy = normalize_invite_policy(invite_policy);
        room.mark_unused_if_empty();
        info!("Materialized SFU room: {} ({})", room_name, &id);
        self.rooms.insert(id.clone(), room);
        self.rooms.get(&id).map(|r| (r, true))
    }

    /// Drop user-created rooms that have been unused for at least `idle_secs`.
    pub fn gc_idle_rooms(&mut self, now: f64, idle_secs: f64) -> Vec<String> {
        let mut removed = Vec::new();
        self.rooms.retain(|room_id, room| {
            if *room_id == DEFAULT_ROOM_ID || room.creator_id.is_empty() {
                return true;
            }
            let Some(empty_since) = room.empty_since else {
                return true;
            };
            if now - empty_since < idle_secs {
                return true;
            }
            debug!("GC idle SFU room: {}", room_id);
            removed.push(room_id.clone());
            false
        });
        removed
    }

    /// Join a peer to a room. Returns (success, member_list).
    pub fn join_room(&mut self, peer_id: &str, room_id: &str) -> (bool, Vec<String>) {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return (false, vec![]);
        };
        if !room.is_peer_allowed(peer_id) {
            return (false, vec![]);
        }
        let ok = room.add_participant(peer_id);
        let members = room.participant_ids();
        (ok, members)
    }

    /// Leave a room. Returns remaining member list.
    pub fn leave_room(&mut self, peer_id: &str, room_id: &str) -> Vec<String> {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return vec![];
        };
        room.remove_participant(peer_id);
        room.mark_unused_if_empty();
        let members = room.participant_ids();
        // GC anonymous rooms (non-default, no creator) when empty
        if room.is_unused() && room.creator_id.is_empty() && room_id != DEFAULT_ROOM_ID {
            debug!("GC-ing empty anonymous room: {}", room_id);
            self.rooms.remove(room_id);
        }
        members
    }

    /// Remove peer from ALL rooms (participants and subscribers).
    pub fn remove_peer_from_all(&mut self, peer_id: &str) -> Vec<(String, Vec<String>)> {
        let mut results = vec![];
        let room_ids: Vec<String> = self
            .rooms
            .iter()
            .filter(|(_, r)| {
                r.participants.contains_key(peer_id) || r.subscribers.contains(peer_id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for room_id in room_ids {
            if let Some(room) = self.rooms.get_mut(&room_id) {
                let was_present = room.remove_peer_entirely(peer_id);
                if was_present {
                    let members = room.participant_ids();
                    // GC anonymous rooms when empty
                    if room.is_unused() && room.creator_id.is_empty() && room_id != DEFAULT_ROOM_ID
                    {
                        debug!("GC-ing empty anonymous room: {}", room_id);
                        self.rooms.remove(&room_id);
                    }
                    results.push((room_id, members));
                }
            }
        }
        results
    }

    pub fn get_room(&self, room_id: &str) -> Option<&SFURoom> {
        self.rooms.get(room_id)
    }

    /// Active-speaker gate for an inbound audio frame. Records `sender`'s
    /// activity and returns the voice recipients (all room participants) when
    /// the frame should be forwarded, or `None` when `sender` is over the
    /// room's active-speaker cap and the frame should be dropped server-side.
    /// `now` is wall-clock seconds (see [`Self::audio_forward_targets_now`]).
    pub fn audio_forward_targets(
        &mut self,
        room_id: &str,
        sender: &str,
        now: f64,
    ) -> Option<Vec<String>> {
        let room = self.rooms.get_mut(room_id)?;
        if room.note_audio_should_forward(sender, now) {
            Some(room.participant_ids())
        } else {
            None
        }
    }

    /// [`Self::audio_forward_targets`] using the current wall clock.
    pub fn audio_forward_targets_now(
        &mut self,
        room_id: &str,
        sender: &str,
    ) -> Option<Vec<String>> {
        self.audio_forward_targets(room_id, sender, SFURoom::now_secs())
    }

    /// Room ids with at least one local participant or text subscriber. Used to
    /// advertise this node's interests to cluster peers for replication routing.
    pub fn subscribed_room_ids(&self) -> Vec<String> {
        self.rooms
            .iter()
            .filter(|(_, r)| !r.is_unused())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Descriptors for **durable** rooms — those with a creator (client-owned,
    /// not anonymous GC-able rooms) and not the always-present `default` lobby.
    /// Gossiped to cluster peers so any member can materialize them and accept a
    /// failed-over join, independent of whether the client pre-seeded that node.
    /// Returns `(room_id, room_name, room_type, creator_id, invite_policy)`.
    pub fn durable_room_descriptors(&self) -> Vec<(String, String, RoomType, String, String)> {
        self.rooms
            .values()
            .filter(|r| r.room_id != DEFAULT_ROOM_ID && !r.creator_id.is_empty())
            .map(|r| {
                (
                    r.room_id.clone(),
                    r.room_name.clone(),
                    r.room_type,
                    r.creator_id.clone(),
                    r.invite_policy.clone(),
                )
            })
            .collect()
    }

    /// Get all peers who should receive text chat (participants + subscribers).
    pub fn get_chat_recipients(&self, room_id: &str) -> Vec<String> {
        self.rooms
            .get(room_id)
            .map(|r| r.chat_recipient_ids())
            .unwrap_or_default()
    }

    /// True when `peer_id` is a voice participant or text-only subscriber in
    /// `room_id` — the minimum bar for accepting an outbound `SfuChat`.
    pub fn is_chat_sender(&self, room_id: &str, peer_id: &str) -> bool {
        self.rooms.get(room_id).is_some_and(|r| {
            r.participants.contains_key(peer_id) || r.subscribers.contains(peer_id)
        })
    }

    /// Subscribe a peer to a room's text chat without voice join.
    pub fn subscribe(&mut self, peer_id: &str, room_id: &str) -> bool {
        if let Some(room) = self.rooms.get_mut(room_id) {
            if room.is_peer_allowed(peer_id) {
                room.subscribe(peer_id);
                return true;
            }
        }
        false
    }

    /// Unsubscribe a peer from a room's text chat.
    pub fn unsubscribe(&mut self, peer_id: &str, room_id: &str) {
        if let Some(room) = self.rooms.get_mut(room_id) {
            room.unsubscribe(peer_id);
        }
    }

    /// Get rooms visible to a peer: all public + private rooms they're in.
    pub fn get_rooms_for_peer(&self, peer_id: &str) -> Vec<serde_json::Value> {
        self.rooms
            .values()
            .filter(|r| r.is_public() || r.is_peer_allowed(peer_id))
            .map(|r| {
                serde_json::json!({
                    "room_id": r.room_id,
                    "name": r.room_name,
                    "member_count": r.participant_count(),
                    "participant_ids": r.participant_ids(),
                    "room_type": r.room_type,
                    "creator_id": r.creator_id,
                    "is_default": r.room_id == DEFAULT_ROOM_ID,
                })
            })
            .collect()
    }

    /// Validate and consume a room invite token.
    pub fn validate_room_invite(&mut self, room_id: &str, token: &str, peer_id: &str) -> bool {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return false;
        };
        room.validate_and_consume_token(token, peer_id)
    }

    /// Allow a peer to join an existing private room.
    pub fn allow_peer(&mut self, room_id: &str, peer_id: &str) -> bool {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return false;
        };
        room.allow_peer(peer_id);
        true
    }

    /// Bind a room to its cryptographically-proven Space owner by adopting
    /// `owner_pub` as `creator_id` when the room currently has none. A Space
    /// room re-materialized after a restart/idle-GC comes back with an empty
    /// creator (in-memory SFU state is lost), which strips the owner's ability
    /// to mint invites or self-admit. A verified inclusion proof re-establishes
    /// `owner_pub` as the owner, so adopting it here restores that authority
    /// and is durable across restarts (re-applied on every proof-bearing join).
    /// No-op if `owner_pub` is empty or the room already has a creator.
    pub fn adopt_creator_if_empty(&mut self, room_id: &str, owner_pub: &str) -> bool {
        if owner_pub.is_empty() {
            return false;
        }
        match self.rooms.get_mut(room_id) {
            Some(room) if room.creator_id.is_empty() => {
                room.creator_id = owner_pub.to_string();
                true
            }
            _ => false,
        }
    }

    /// Generate an invite token for a room.
    ///
    /// New shareable invites remain single-use (`max_uses = 1`). Returning
    /// members rejoin via the `allowed` set (or a client re-seeded multi-use
    /// token after idle GC — see [`SFURoom::reregister_invite_token`]).
    pub fn generate_invite_token(&mut self, room_id: &str, created_by: &str) -> Option<String> {
        self.rooms
            .get_mut(room_id)
            .map(|r| r.generate_invite_token(created_by, 1))
    }

    /// Re-seed a client-held invite token into an existing room (post-GC
    /// materialize). Returns `false` if the room is missing or the token is
    /// empty.
    pub fn reregister_invite_token(
        &mut self,
        room_id: &str,
        token: &str,
        created_by: &str,
    ) -> bool {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return false;
        };
        room.reregister_invite_token(token, created_by)
    }

    /// Generate an invite token if `requester` is authorized under the room's
    /// [`SFURoom::invite_policy`]: the creator always qualifies; under
    /// `"members"`, any current participant, chat subscriber, or already-
    /// `allow`ed peer also qualifies (see [`SFURoom::is_invite_eligible_member`]).
    ///
    /// This closes the unchecked-minting hole where any authenticated peer
    /// could mint a valid token for any room. Rooms with no creator (the
    /// default/anonymous rooms) never mint through this path regardless of
    /// policy — there is no owner to have set one.
    pub fn generate_invite_token_checked(&mut self, room_id: &str, requester: &str) -> InviteMint {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return InviteMint::RoomNotFound;
        };
        if room.creator_id.is_empty() {
            return InviteMint::NotAuthorized;
        }
        let authorized = room.creator_id == requester
            || (room.invite_policy == "members" && room.is_invite_eligible_member(requester));
        if !authorized {
            return InviteMint::NotAuthorized;
        }
        InviteMint::Ok(room.generate_invite_token(requester, 1))
    }

    /// Stats snapshot.
    pub(crate) fn stats(&self) -> SFUStats {
        SFUStats {
            rooms_total: self.rooms.len(),
            participants_total: self.rooms.values().map(|r| r.participant_count()).sum(),
            rooms: self
                .rooms
                .values()
                .map(|r| SFURoomStats {
                    room_id: r.room_id.clone(),
                    name: r.room_name.clone(),
                    room_type: r.room_type,
                    participants: r.participant_count(),
                })
                .collect(),
        }
    }
}

/// Outcome of an owner-checked invite-mint request
/// ([`SFURoomManager::generate_invite_token_checked`]).
#[derive(Debug, PartialEq, Eq)]
pub enum InviteMint {
    /// Minted successfully; carries the token.
    Ok(String),
    /// No such room on this node.
    RoomNotFound,
    /// The requester is not authorized to mint for this room (owner-only).
    NotAuthorized,
}

#[derive(Debug, Clone, Serialize)]
pub struct SFUStats {
    pub rooms_total: usize,
    pub participants_total: usize,
    pub rooms: Vec<SFURoomStats>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SFURoomStats {
    pub room_id: String,
    pub name: String,
    pub room_type: RoomType,
    pub participants: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_descriptors_skip_default_and_anonymous_rooms() {
        let mut mgr = SFURoomManager::new();
        // A client-owned room — durable, should be advertised to the cluster.
        mgr.create_room_with_policy(
            Some("owned"),
            "We Gamin?",
            RoomType::Private,
            "OWNER",
            "members",
        )
        .expect("room");
        // The always-present default lobby has no creator — not advertised.
        mgr.create_room(Some(DEFAULT_ROOM_ID), "Lobby", RoomType::Public, "");
        // An anonymous (creatorless, GC-able) room — not advertised.
        mgr.create_room(Some("anon"), "Anon", RoomType::Public, "");

        let descs = mgr.durable_room_descriptors();
        assert_eq!(descs.len(), 1, "only the client-owned room is durable");
        let (id, name, rtype, creator, policy) = &descs[0];
        assert_eq!(id, "owned");
        assert_eq!(name, "We Gamin?");
        assert_eq!(*rtype, RoomType::Private);
        assert_eq!(creator, "OWNER");
        assert_eq!(policy, "members");
    }

    #[test]
    fn is_chat_sender_requires_membership() {
        let mut mgr = SFURoomManager::new();
        mgr.create_room(Some("r1"), "Room", RoomType::Public, "creator")
            .expect("room");
        mgr.join_room("talker", "r1");
        assert!(mgr.is_chat_sender("r1", "talker"));
        assert!(!mgr.is_chat_sender("r1", "outsider"));
        assert!(mgr.subscribe("listener", "r1"));
        assert!(mgr.is_chat_sender("r1", "listener"));
    }

    #[test]
    fn invite_mint_is_owner_only() {
        let mut mgr = SFURoomManager::new();
        mgr.create_room(Some("priv"), "Private", RoomType::Private, "owner-pub")
            .expect("room");

        // The creator mints successfully.
        match mgr.generate_invite_token_checked("priv", "owner-pub") {
            InviteMint::Ok(tok) => assert!(!tok.is_empty()),
            other => panic!("owner should mint, got {other:?}"),
        }

        // A non-creator is refused (the §6.1 hole).
        assert_eq!(
            mgr.generate_invite_token_checked("priv", "someone-else"),
            InviteMint::NotAuthorized
        );

        // Unknown room is reported distinctly.
        assert_eq!(
            mgr.generate_invite_token_checked("nope", "owner-pub"),
            InviteMint::RoomNotFound
        );
    }

    #[test]
    fn invite_mint_rejects_creatorless_rooms() {
        let mut mgr = SFURoomManager::new();
        // The default room has no creator — nobody may mint for it.
        assert_eq!(
            mgr.generate_invite_token_checked(DEFAULT_ROOM_ID, "anyone"),
            InviteMint::NotAuthorized
        );
        // Anonymous room (empty creator) likewise.
        mgr.create_room(Some("anon"), "Anon", RoomType::Public, "")
            .expect("room");
        assert_eq!(
            mgr.generate_invite_token_checked("anon", "anyone"),
            InviteMint::NotAuthorized
        );
    }

    #[test]
    fn invite_mint_members_policy_allows_participants_and_subscribers() {
        let mut mgr = SFURoomManager::new();
        mgr.create_room_with_policy(
            Some("priv"),
            "Private",
            RoomType::Private,
            "owner-pub",
            "members",
        )
        .expect("room");

        // Owner still mints under "members".
        match mgr.generate_invite_token_checked("priv", "owner-pub") {
            InviteMint::Ok(tok) => assert!(!tok.is_empty()),
            other => panic!("owner should mint, got {other:?}"),
        }

        // A voice participant qualifies under "members".
        mgr.allow_peer("priv", "talker");
        mgr.join_room("talker", "priv");
        match mgr.generate_invite_token_checked("priv", "talker") {
            InviteMint::Ok(tok) => assert!(!tok.is_empty()),
            other => panic!("participant should mint under members policy, got {other:?}"),
        }

        // A chat-only subscriber also qualifies.
        mgr.allow_peer("priv", "listener");
        mgr.subscribe("listener", "priv");
        match mgr.generate_invite_token_checked("priv", "listener") {
            InviteMint::Ok(tok) => assert!(!tok.is_empty()),
            other => panic!("subscriber should mint under members policy, got {other:?}"),
        }

        // A total outsider still cannot mint.
        assert_eq!(
            mgr.generate_invite_token_checked("priv", "outsider"),
            InviteMint::NotAuthorized
        );
    }

    #[test]
    fn invite_mint_owner_policy_still_rejects_non_creator_members() {
        let mut mgr = SFURoomManager::new();
        // Default/omitted policy normalizes to "owner" via `create_room`.
        mgr.create_room(Some("priv"), "Private", RoomType::Private, "owner-pub")
            .expect("room");
        mgr.allow_peer("priv", "talker");
        mgr.join_room("talker", "priv");
        // Even a joined participant cannot mint under the "owner" policy.
        assert_eq!(
            mgr.generate_invite_token_checked("priv", "talker"),
            InviteMint::NotAuthorized
        );
    }

    #[test]
    fn normalize_invite_policy_defaults_unknown_values_to_owner() {
        assert_eq!(normalize_invite_policy("members"), "members");
        assert_eq!(normalize_invite_policy("owner"), "owner");
        assert_eq!(normalize_invite_policy(""), "owner");
        assert_eq!(normalize_invite_policy("garbage"), "owner");
    }

    #[test]
    fn test_room_lifecycle() {
        let mut mgr = SFURoomManager::new();
        assert!(mgr.get_room(DEFAULT_ROOM_ID).is_some());

        mgr.create_room(Some("test"), "Test Room", RoomType::Public, "creator1")
            .expect("room");
        let (ok, members) = mgr.join_room("peer1", "test");
        assert!(ok);
        assert_eq!(members.len(), 1);

        let (ok2, members2) = mgr.join_room("peer2", "test");
        assert!(ok2);
        assert_eq!(members2.len(), 2);

        let remaining = mgr.leave_room("peer1", "test");
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn room_list_includes_voice_participant_ids() {
        let mut mgr = SFURoomManager::new();
        mgr.create_room(Some("test"), "Test Room", RoomType::Public, "creator")
            .expect("room");
        mgr.join_room("peer1", "test");
        mgr.join_room("peer2", "test");

        let rooms = mgr.get_rooms_for_peer("peer1");
        let room = rooms
            .iter()
            .find(|r| r.get("room_id").and_then(|v| v.as_str()) == Some("test"))
            .expect("test room in list");
        let mut ids: Vec<&str> = room
            .get("participant_ids")
            .and_then(|v| v.as_array())
            .expect("participant_ids array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["peer1", "peer2"]);
    }

    #[test]
    fn test_private_room() {
        let mut mgr = SFURoomManager::new();
        mgr.create_room(Some("priv"), "Private", RoomType::Private, "creator");

        // Unauthorized peer can't join
        let (ok, _) = mgr.join_room("stranger", "priv");
        assert!(!ok);

        // Creator can join
        let (ok, _) = mgr.join_room("creator", "priv");
        assert!(ok);

        // Generate invite token
        let token = mgr.generate_invite_token("priv", "creator").unwrap();
        assert!(mgr.validate_room_invite("priv", &token, "friend"));

        let (ok, _) = mgr.join_room("friend", "priv");
        assert!(ok);
    }

    #[test]
    fn materializing_peer_can_join_new_private_room() {
        let mut mgr = SFURoomManager::new();
        let (_, created) = mgr
            .create_room(Some("saved"), "Saved Private", RoomType::Private, "creator")
            .unwrap();
        assert!(created);
        assert!(mgr.allow_peer("saved", "friend"));

        let (ok, members) = mgr.join_room("friend", "saved");
        assert!(ok);
        assert_eq!(members, vec!["friend".to_string()]);
        let rooms = mgr.get_rooms_for_peer("friend");
        let room = rooms
            .iter()
            .find(|r| r.get("room_id").and_then(|v| v.as_str()) == Some("saved"))
            .expect("saved private room visible");
        assert_eq!(room.get("member_count").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn gc_idle_user_room() {
        let mut mgr = SFURoomManager::new();
        let now = SFURoom::now_secs();
        mgr.create_room(Some("idle"), "Idle", RoomType::Public, "creator");
        let removed = mgr.gc_idle_rooms(now + IDLE_ROOM_GC_SECS + 1.0, IDLE_ROOM_GC_SECS);
        assert_eq!(removed, vec!["idle".to_string()]);
        assert!(mgr.get_room("idle").is_none());
        assert!(mgr.get_room(DEFAULT_ROOM_ID).is_some());
    }

    /// After idle GC the invite map is gone. Clients re-seed their stored
    /// invite token on rematerialize; that credential must admit returning
    /// members (including multi-use re-entry) without a fresh share.
    #[test]
    fn reregister_invite_token_survives_gc_and_readmits_member() {
        let mut mgr = SFURoomManager::new();
        mgr.create_room(Some("priv"), "Private", RoomType::Private, "creator");
        let token = mgr.generate_invite_token("priv", "creator").unwrap();
        assert!(mgr.validate_room_invite("priv", &token, "friend"));
        // Single-use mint: second consume fails while the room still exists.
        assert!(!mgr.validate_room_invite("priv", &token, "other"));
        // Friend is allowed and can join.
        assert!(mgr.join_room("friend", "priv").0);

        // Idle-GC the empty room (friend leaves first so empty_since is set).
        mgr.leave_room("friend", "priv");
        let now = SFURoom::now_secs();
        // Force empty_since into the past via GC clock.
        let removed = mgr.gc_idle_rooms(now + IDLE_ROOM_GC_SECS + 1.0, IDLE_ROOM_GC_SECS);
        assert!(removed.contains(&"priv".to_string()));
        assert!(mgr.get_room("priv").is_none());

        // Rematerialize (as after client RoomStore replay).
        let (_, created) = mgr
            .create_room(Some("priv"), "Private", RoomType::Private, "creator")
            .unwrap();
        assert!(created);
        // Creator is always allowed; friend is not until re-seed.
        assert!(!mgr.join_room("friend", "priv").0);
        assert!(mgr.reregister_invite_token("priv", &token, "creator"));
        assert!(mgr.allow_peer("priv", "friend"));
        // Multi-use re-seeded token admits again (and a third time).
        assert!(mgr.validate_room_invite("priv", &token, "friend"));
        assert!(mgr.validate_room_invite("priv", &token, "friend"));
        assert!(mgr.join_room("friend", "priv").0);
    }

    #[test]
    fn reregister_empty_token_is_rejected() {
        let mut mgr = SFURoomManager::new();
        mgr.create_room(Some("priv"), "Private", RoomType::Private, "creator");
        assert!(!mgr.reregister_invite_token("priv", "", "creator"));
        assert!(!mgr.reregister_invite_token("missing", "tok", "creator"));
    }

    #[test]
    fn test_room_gc() {
        let mut mgr = SFURoomManager::new();
        // Anonymous room (no creator)
        mgr.rooms.insert(
            "anon".to_string(),
            SFURoom::new("anon", "Anon", RoomType::Public, ""),
        );
        mgr.join_room("p1", "anon");
        mgr.leave_room("p1", "anon");
        // Should be GC'd
        assert!(mgr.get_room("anon").is_none());
    }

    // ── Active-speaker forwarding ───────────────────────────────────────────

    fn room_with(participants: usize) -> SFURoomManager {
        let mut mgr = SFURoomManager::new();
        mgr.create_room(Some("r"), "R", RoomType::Public, "creator");
        for i in 0..participants {
            mgr.join_room(&format!("p{i}"), "r");
        }
        mgr
    }

    #[test]
    fn active_speaker_under_cap_forwards_everyone() {
        // A room with no more concurrent talkers than the cap must forward
        // every speaker's frames — the common small-room case is unaffected.
        let mut mgr = room_with(MAX_ACTIVE_SPEAKERS);
        let mut now = 0.0_f64;
        for _ in 0..20 {
            for i in 0..MAX_ACTIVE_SPEAKERS {
                assert!(
                    mgr.audio_forward_targets("r", &format!("p{i}"), now)
                        .is_some(),
                    "speaker p{i} within the cap should always be forwarded"
                );
                now += 0.001;
            }
            now += 0.02; // next 20 ms frame
        }
    }

    #[test]
    fn active_speaker_caps_extra_concurrent_talker() {
        // Cap distinct simultaneous talkers: once MAX_ACTIVE_SPEAKERS are
        // established, an additional concurrent talker is shed server-side.
        let mut mgr = room_with(MAX_ACTIVE_SPEAKERS + 1);
        let mut now = 0.0_f64;
        for _ in 0..30 {
            for i in 0..MAX_ACTIVE_SPEAKERS {
                let _ = mgr.audio_forward_targets("r", &format!("p{i}"), now);
                now += 0.001;
            }
            now += 0.02;
        }
        let extra = format!("p{MAX_ACTIVE_SPEAKERS}");
        assert!(
            mgr.audio_forward_targets("r", &extra, now).is_none(),
            "a talker beyond the cap, against an established active set, is dropped"
        );
    }

    #[test]
    fn active_speaker_silent_member_frees_slot() {
        // When an active speaker goes silent past the timeout, its slot frees
        // and a previously-capped talker is admitted.
        let mut mgr = room_with(MAX_ACTIVE_SPEAKERS + 1);
        let mut now = 0.0_f64;
        // Warm up p0..p_{K-1} as the active set.
        for _ in 0..30 {
            for i in 0..MAX_ACTIVE_SPEAKERS {
                let _ = mgr.audio_forward_targets("r", &format!("p{i}"), now);
                now += 0.001;
            }
            now += 0.02;
        }
        // p0 falls silent; p1..p_{K-1} keep talking and p_K starts. After p0's
        // silence exceeds the timeout its slot is freed and p_K is admitted.
        let newcomer = format!("p{MAX_ACTIVE_SPEAKERS}");
        for _ in 0..60 {
            for i in 1..MAX_ACTIVE_SPEAKERS {
                let _ = mgr.audio_forward_targets("r", &format!("p{i}"), now);
                now += 0.001;
            }
            let _ = mgr.audio_forward_targets("r", &newcomer, now);
            now += 0.021;
        }
        assert!(
            mgr.audio_forward_targets("r", &newcomer, now).is_some(),
            "after a silent member is pruned, the freed slot admits a new talker"
        );
    }

    #[test]
    fn active_speaker_sustained_talker_displaces_one_shot() {
        // A sustained new talker displaces a one-shot active speaker even
        // before the silence timeout (pure score-based displacement).
        let mut mgr = room_with(MAX_ACTIVE_SPEAKERS + 1);
        let mut now = 0.0_f64;
        // Each of p0..p_{K-1} sends a single frame → active set full, score 1.
        for i in 0..MAX_ACTIVE_SPEAKERS {
            let _ = mgr.audio_forward_targets("r", &format!("p{i}"), now);
            now += 0.001;
        }
        // p_K hammers frames; within a few it out-scores the one-shot members.
        let newcomer = format!("p{MAX_ACTIVE_SPEAKERS}");
        let mut forwarded = false;
        for _ in 0..20 {
            if mgr.audio_forward_targets("r", &newcomer, now).is_some() {
                forwarded = true;
                break;
            }
            now += 0.001;
        }
        assert!(
            forwarded,
            "a sustained talker displaces a one-shot active speaker"
        );
        assert!(
            now < SPEAKER_SILENCE_SECS,
            "displacement happened within the silence window, not via pruning"
        );
    }

    #[test]
    fn active_speaker_state_cleared_on_leave() {
        // Leaving the room must drop the peer from speaker tracking so a stale
        // entry can't hold an active slot.
        let mut mgr = room_with(1);
        assert!(mgr.audio_forward_targets("r", "p0", 0.0).is_some());
        let room = mgr.get_room("r").unwrap();
        assert!(room.speaker_scores.contains_key("p0"));
        mgr.leave_room("p0", "r");
        // Room "r" has a creator so it survives the leave; re-join and confirm
        // the rejoined peer starts with clean speaker tracking.
        mgr.join_room("p0", "r");
        let room = mgr.get_room("r").unwrap();
        assert!(!room.speaker_scores.contains_key("p0"));
        assert!(room.active_speakers.is_empty());
    }
}
