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
    fun `updating one source never disturbs the other`() {
        val state = base
            .withPresence(direct = setOf("ada"))
            .withPresence(relay = setOf("bobert"))
        assertEquals(setOf("ada"), state.directPeers)
        assertEquals(setOf("bobert"), state.relayPresentPeers)
        assertEquals(setOf("ada", "bobert"), state.onlinePeers)
    }
}
