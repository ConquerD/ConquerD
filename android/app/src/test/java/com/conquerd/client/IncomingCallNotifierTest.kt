package com.conquerd.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

class IncomingCallNotifierTest {

    @Test
    fun `incoming-call intent actions stay distinct from the session service`() {
        // The shade uses these strings as PendingIntent actions. Colliding
        // with DISCONNECT would hang up the session instead of one call.
        assertEquals("com.conquerd.client.INCOMING_SHOW", IncomingCallNotifier.ACTION_SHOW)
        assertEquals("com.conquerd.client.INCOMING_ANSWER", IncomingCallNotifier.ACTION_ANSWER)
        assertEquals("com.conquerd.client.INCOMING_DECLINE", IncomingCallNotifier.ACTION_DECLINE)
        assertNotEquals(IncomingCallNotifier.ACTION_DECLINE, IncomingCallNotifier.ACTION_ANSWER)
    }
}
