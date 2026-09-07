package com.conquerd.client

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * Typed views over the JSON the core returns.
 *
 * Only the fields the UI actually renders are declared. The parser is
 * configured to ignore the rest, so the core can add fields to a record
 * without breaking an older app build.
 */

@Serializable
data class Peer(
    @SerialName("peer_id") val peerId: String = "",
    @SerialName("identity_pub") val identityPub: String = "",
    @SerialName("display_name") val displayName: String = "",
    val handle: String = "",
    val blocked: Boolean = false,
    val revoked: Boolean = false,
    @SerialName("is_supernode") val isSupernode: Boolean = false,
    @SerialName("last_seen_at") val lastSeenAt: Double = 0.0,
) {
    /** What to show in a list row when the peer never set a handle. */
    val label: String
        get() = displayName.ifBlank { handle.ifBlank { peerId.take(12) } }
}

@Serializable
data class ChatMessage(
    val id: String = "",
    @SerialName("peer_id") val peerId: String = "",
    val body: String = "",
    val timestamp: Double = 0.0,
    @SerialName("is_self") val isSelf: Boolean = false,
    val status: String = "",
    val kind: String = "text",
    @SerialName("status_note") val statusNote: String = "",
    @SerialName("sender_handle") val senderHandle: String = "",
)

@Serializable
data class Room(
    @SerialName("room_id") val roomId: String = "",
    @SerialName("room_name") val roomName: String = "",
    @SerialName("room_type") val roomType: String = "",
    @SerialName("supernode_id") val supernodeId: String = "",
    @SerialName("is_creator") val isCreator: Boolean = false,
    /**
     * Single-use admission token for a private room.
     *
     * Passed back on join when present; the core decides between a plain join
     * and an invite-gated one, because sending an empty token reads to the
     * supernode as a failed admission rather than an open join.
     */
    @SerialName("invite_token") val inviteToken: String = "",
    @SerialName("space_id") val spaceId: String = "",
    /**
     * Hidden from the sidebar on this profile.
     *
     * Local-only state that lives in the room store's tombstone list rather
     * than on the entry, so it has to be asked for explicitly - a room list
     * that ignores it shows every room the user has ever seen.
     */
    val hidden: Boolean = false,
) {
    /** A stable key for list rendering: room ids repeat across supernodes. */
    val key: String get() = "$supernodeId/$roomId"
}

/**
 * A message in a room.
 *
 * Rooms are ephemeral on the supernode and room chat is not written to the
 * local chat store, so these live only in memory for the duration of a visit.
 */
data class RoomMessage(
    val messageId: String,
    val senderId: String,
    val senderHandle: String,
    val body: String,
    val timestamp: Double,
    val isSelf: Boolean,
)

/** Who we are, from `identity.info`. */
@Serializable
data class IdentityInfo(
    @SerialName("public_id") val publicId: String = "",
    @SerialName("peer_id") val peerId: String = "",
    val fingerprint: String = "",
)

/**
 * How the app is currently reaching the network.
 *
 * Mirrors the desktop session banner: the distinction between a direct peer
 * connection and a relayed one is something users act on, so it is surfaced
 * rather than collapsed into "connected".
 */
enum class ConnectionMode { OFFLINE, RELAY, DIRECT }
