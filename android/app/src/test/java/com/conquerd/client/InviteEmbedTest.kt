package com.conquerd.client

import com.conquerd.client.ui.InviteKind
import com.conquerd.client.ui.bodyWithoutInvite
import com.conquerd.client.ui.findInviteUrl
import com.conquerd.client.ui.inviteKindOf
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Detection rules for the inline invite embed.
 *
 * These mirror the desktop's `ChatRichMessageDelegate` regexes, and the point
 * of pinning them is that the two must agree: a link one client turns into an
 * Accept button and the other leaves as dead text is worse than neither doing
 * it, because the sender has no way to tell which happened.
 */
class InviteEmbedTest {

    @Test
    fun finds_https_peer_and_room_invites() {
        val peer = "https://doubleslash.space/i#abc123"
        val room = "https://doubleslash.space/r#xyz789"
        assertEquals(peer, findInviteUrl("join me $peer please"))
        assertEquals(room, findInviteUrl(room))
        assertEquals(InviteKind.PEER, inviteKindOf(peer))
        assertEquals(InviteKind.ROOM, inviteKindOf(room))
    }

    /** Hand-off forms pasted from elsewhere still have to be offered. */
    @Test
    fun finds_scheme_invites() {
        assertEquals("d://invite#tok", findInviteUrl("here: d://invite#tok"))
        assertEquals(InviteKind.PEER, inviteKindOf("d://invite#tok"))
        assertEquals(InviteKind.ROOM, inviteKindOf("doubleslash://room#tok"))
    }

    /** An ordinary link must not sprout an Accept button. */
    @Test
    fun ignores_unrelated_links() {
        assertNull(findInviteUrl("see https://example.com/i#notours"))
        assertNull(findInviteUrl("no links here at all"))
    }

    /**
     * The bare domain is not an invite: the fragment carries the token, and
     * without one there is nothing to accept.
     */
    @Test
    fun requires_a_fragment() {
        assertNull(findInviteUrl("https://doubleslash.space/i"))
        assertNull(findInviteUrl("https://doubleslash.space/i#"))
    }

    @Test
    fun strips_the_link_from_the_message_text() {
        val url = "https://doubleslash.space/i#abc123"
        assertEquals("join me please", bodyWithoutInvite("join me $url please", url))
        // A message that was only a link leaves nothing worth a bubble.
        assertEquals("", bodyWithoutInvite(url, url))
    }

    /** Trailing punctuation in prose must not be swallowed into the token. */
    @Test
    fun stops_at_whitespace_and_quotes() {
        val body = "\"https://doubleslash.space/i#abc\" and more"
        assertEquals("https://doubleslash.space/i#abc", findInviteUrl(body))
    }
}
