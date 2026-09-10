package com.conquerd.client

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Covers the JSON layer between the core and the UI.
 *
 * These are the pure parts of the boundary — everything that does not need a
 * running core or an Android context — and they encode claims the rest of the
 * app relies on: that a reply is only "ok" when the core said so, that an
 * unknown field cannot break an older build, and that the display fallbacks
 * never surface a raw 44-character id.
 */
class CoreModelsTest {

    private val json = Json { ignoreUnknownKeys = true; isLenient = true }

    private fun obj(raw: String): JsonObject = json.parseToJsonElement(raw).jsonObject

    // ── Reply helpers ──────────────────────────────────────────────────────

    @Test
    fun `ok is true only for a boolean true`() {
        assertTrue(obj("""{"ok":true}""").ok)
        assertFalse(obj("""{"ok":false}""").ok)
    }

    @Test
    fun `a reply with no ok field is not treated as success`() {
        // A malformed or truncated reply must fail closed. Reading a missing
        // field as success would make every parse error look like it worked.
        assertFalse(obj("""{}""").ok)
    }

    @Test
    fun `a non-boolean ok does not throw`() {
        // The core always sends a boolean, but a strict accessor here would
        // turn a wire change into a crash rather than a failed command.
        assertFalse(obj("""{"ok":"yes"}""").ok)
        assertFalse(obj("""{"ok":1}""").ok)
    }

    @Test
    fun `error text is read from a failed reply`() {
        assertEquals("no running session", obj("""{"ok":false,"error":"no running session"}""").errorText)
        assertNull(obj("""{"ok":true}""").errorText)
    }

    @Test
    fun `string reads only strings`() {
        val reply = obj("""{"a":"x","b":3}""")
        assertEquals("x", reply.string("a"))
        assertNull(reply.string("b"))
        assertEquals("", reply.stringOrEmpty("missing"))
    }

    @Test
    fun `number falls back to zero rather than throwing`() {
        assertEquals(1.5, obj("""{"t":1.5}""").number("t"), 0.0001)
        assertEquals(0.0, obj("""{}""").number("t"), 0.0001)
    }

    @Test
    fun `string list reads arrays and tolerates absence`() {
        assertEquals(listOf("a", "b"), obj("""{"m":["a","b"]}""").stringList("m"))
        assertEquals(emptyList<String>(), obj("""{}""").stringList("m"))
    }

    @Test
    fun `event name is empty for a command reply`() {
        assertEquals("chat_message", obj("""{"event":"chat_message"}""").eventName())
        assertEquals("", obj("""{"ok":true}""").eventName())
    }

    // ── Models ─────────────────────────────────────────────────────────────

    @Test
    fun `an unknown field does not break decoding`() {
        // Forward compatibility: a core that gained a field must not stop an
        // older app build from reading the rest.
        val peer = json.decodeFromString<Peer>(
            """{"peer_id":"abc","display_name":"Bobert","brand_new_field":42}""",
        )
        assertEquals("abc", peer.peerId)
        assertEquals("Bobert", peer.displayName)
    }

    @Test
    fun `peer label prefers display name then handle then a short id`() {
        val full = Peer(peerId = "0123456789abcdef", displayName = "Bobert", handle = "bob")
        assertEquals("Bobert", full.label)

        val handleOnly = full.copy(displayName = "")
        assertEquals("bob", handleOnly.label)

        // Never the full id: these are 44 characters and unreadable in a row.
        val idOnly = full.copy(displayName = "", handle = "")
        assertEquals("0123456789ab", idOnly.label)
        assertEquals(12, idOnly.label.length)
    }

    @Test
    fun `room key distinguishes the same room id on different supernodes`() {
        val a = Room(roomId = "r1", supernodeId = "node-a")
        val b = Room(roomId = "r1", supernodeId = "node-b")
        assertFalse("list keys must not collide", a.key == b.key)
    }

    @Test
    fun `room hidden defaults to false when the core omits it`() {
        // Hidden is a separate tombstone list on the core side; a room list
        // that omits the flag must not make everything vanish.
        val room = json.decodeFromString<Room>("""{"room_id":"r1","room_name":"General"}""")
        assertFalse(room.hidden)
    }

    @Test
    fun `chat message decodes the fields the UI renders`() {
        val message = json.decodeFromString<ChatMessage>(
            """{"id":"m1","peer_id":"p","body":"hi","timestamp":1.5,"is_self":true,"status":"failed","status_note":"not delivered"}""",
        )
        assertEquals("m1", message.id)
        assertTrue(message.isSelf)
        assertEquals("failed", message.status)
        assertEquals("not delivered", message.statusNote)
    }

    @Test
    fun `a room roster keeps voice participants and chat members apart`() {
        // `members` is the voice rail; `chat_members` is the supernode's full
        // key-group roster (participants + text subscribers). Counting the
        // former as "members" hides every peer reading the room without
        // joining voice - which is what made the header disagree with the
        // desktop's member panel.
        val event = obj(
            """{"event":"room_members_changed","room_id":"r1",
                "members":["me"],"chat_members":["me","bobert"]}""",
        )
        assertEquals(listOf("me"), event.stringList("members"))
        assertEquals(listOf("me", "bobert"), event.stringList("chat_members"))
    }

    @Test
    fun `a roster with nobody in voice still has chat members`() {
        // The all-text case: an empty voice rail must not read as an empty room.
        val event = obj(
            """{"event":"room_members_changed","room_id":"r1",
                "members":[],"chat_members":["me","bobert"]}""",
        )
        assertTrue(event.stringList("members").isEmpty())
        assertEquals(2, event.stringList("chat_members").size)
    }
}
