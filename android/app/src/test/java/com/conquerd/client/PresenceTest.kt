package com.conquerd.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Covers the peer dot's two independent sources.
 *
 * The bug this replaced was a peer dot that meant "do I have a direct QUIC
 * session" rather than "is this peer up": remote on cellular, a pair behind
 * CGNAT never gets a direct session, so chat flowed over the relay while the
 * list showed the peer offline. These assert the union that fixed it, and in
 * particular that losing one source does not clear a peer the other still
 * vouches for.
 */
class PresenceTest {

    private val base = AppState()

    @Test
    fun `a relayed announce alone brings a peer online`() {
        val state = base.withPresence(relay = setOf("bobert"))
        assertTrue("bobert" in state.onlinePeers)
        assertFalse("a relay announce is not a direct session", "bobert" in state.directPeers)
    }

    @Test
    fun `a direct session alone brings a peer online`() {
        val state = base.withPresence(direct = setOf("bobert"))
        assertTrue("bobert" in state.onlinePeers)
    }

    @Test
    fun `losing the direct session keeps a peer with a fresh announce online`() {
        val connected = base.withPresence(direct = setOf("bobert"), relay = setOf("bobert"))
        assertTrue("bobert" in connected.onlinePeers)

        // Walking out of Wi-Fi range ends the direct session; the relay still
        // carries chat and still reports the peer, so the dot must stay lit.
        val dropped = connected.withPresence(direct = emptySet())
        assertTrue("bobert" in dropped.onlinePeers)
    }

    @Test
    fun `a peer goes offline only when every source has released it`() {
        val both = base.withPresence(direct = setOf("bobert"), relay = setOf("bobert"))
        val noRelay = both.withPresence(relay = emptySet())
        assertTrue("still directly connected", "bobert" in noRelay.onlinePeers)

        val neither = noRelay.withPresence(direct = emptySet())
        assertFalse("bobert" in neither.onlinePeers)
    }

    @Test
    fun `sources stay independent across peers`() {
        val state = base.withPresence(direct = setOf("ada"), relay = setOf("bobert"))
        assertEquals(setOf("ada", "bobert"), state.onlinePeers)

        val gone = state.withPresence(relay = emptySet())
        assertEquals(setOf("ada"), gone.onlinePeers)
    }

    @Test
    fun `a room headcount unions the nodes that reported it`() {
        // A cluster hosts one logical room on several members, and two peers
        // routinely subscribe on different ones - so no single node sees them
        // both. The badge unions instead of trusting one node.
        val rosters = mapOf(
            "nodeA:room1" to listOf("desktop"),
            "nodeB:room1" to listOf("phone"),
        )
        assertEquals(2, rosters.roomHeadcount("room1"))
    }

    @Test
    fun `a multi-homed peer is counted once`() {
        // The same peer legitimately appears in two nodes' rosters; summing
        // counts would report two people where there is one.
        val rosters = mapOf(
            "nodeA:room1" to listOf("phone"),
            "nodeB:room1" to listOf("phone"),
        )
        assertEquals(1, rosters.roomHeadcount("room1"))
    }

    @Test
    fun `headcounts do not bleed between rooms`() {
        val rosters = mapOf(
            "nodeA:room1" to listOf("a"),
            "nodeA:room2" to listOf("a", "b"),
        )
        assertEquals(1, rosters.roomHeadcount("room1"))
        assertEquals(2, rosters.roomHeadcount("room2"))
        assertEquals(0, rosters.roomHeadcount("room3"))
    }

    @Test
    fun `updating one source never disturbs the other`() {
        val state = base
            .withPresence(direct = setOf("ada"))
            .withPresence(relay = setOf("bobert"))
        assertEquals(setOf("ada"), state.directPeers)
        assertEquals(setOf("bobert"), state.relayPresentPeers)
        assertEquals(setOf("ada", "bobert"), state.onlinePeers)
    }
}
