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

/**
 * The name to show for whoever wrote a room message.
 *
 * A room frame carries a handle only when the sender had one set at the time
 * it was written, so an older message - or one from a peer who set their name
 * afterwards - arrives with nothing. Falling straight through to the id then
 * showed a raw `VkR20VqcIw` next to every bubble even though the peer store
 * knew the person perfectly well.
 *
 * Matched on either spelling: room frames carry the base64 `public_id` while
 * peer rows are keyed by the hex `peer_id`, and the padded and un-padded forms
 * of a public_id are the same identity.
 */
fun List<Peer>.roomSenderName(senderId: String, carriedHandle: String): String {
    if (carriedHandle.isNotBlank()) return carriedHandle
    if (senderId.isNotBlank()) {
        val bare = senderId.trimEnd('=')
        val known = firstOrNull { it.identityPub.trimEnd('=') == bare || it.peerId == senderId }
        val name = known?.displayName?.ifBlank { known.handle }.orEmpty()
        if (name.isNotBlank()) return name
    }
    // Nothing better to show. Deliberately last, not the default.
    return senderId.take(10)
}

@Serializable
data class ChatMessage(
    val id: String = "",
    @SerialName("peer_id") val peerId: String = "",
    /**
     * Who wrote it, as their `public_id`.
     *
     * Distinct from [peerId], which is the *conversation* key - for a room
     * message that is the room, not a person. Reading the sender off [peerId]
     * gave every message in a room the same author.
     */
    val sender: String = "",
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
     * Parent node in the Space tree; empty or equal to [spaceId] means the room
     * sits directly under the server rather than inside another room.
     */
    @SerialName("parent_id") val parentId: String = "",
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

/**
 * A supernode we know, with the cluster it fronts for.
 *
 * `decodeList` swallows a decode failure and returns an empty list, so the
 * field names have to match the wire exactly - a mismatch here would show as
 * "no supernodes" rather than an error.
 */
@Serializable
data class SupernodeInfo(
    @SerialName("peer_id") val peerId: String = "",
    @SerialName("identity_pub") val identityPub: String = "",
    @SerialName("display_name") val displayName: String = "",
    @SerialName("cluster_members") val clusterMembers: List<String> = emptyList(),
)

/** Who we are, from `identity.info`. */
@Serializable
data class IdentityInfo(
    @SerialName("public_id") val publicId: String = "",
    @SerialName("peer_id") val peerId: String = "",
    val fingerprint: String = "",
    /** The name peers see for us; empty until one is set. */
    val handle: String = "",
)

/**
 * How the app is currently reaching the network.
 *
 * Mirrors the desktop session banner: the distinction between a direct peer
 * connection and a relayed one is something users act on, so it is surfaced
 * rather than collapsed into "connected".
 */
enum class ConnectionMode { OFFLINE, RELAY, DIRECT }
