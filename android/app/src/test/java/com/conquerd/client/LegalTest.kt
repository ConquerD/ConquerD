package com.conquerd.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LegalTest {

    @Test
    fun `a report body carries the target id and not a guessed message`() {
        val body = Legal.reportBody(
            kind = "peer",
            targetId = "abc123",
            targetLabel = "Bobert",
            note = "unsolicited files",
        )
        assertTrue(body.contains("Kind: peer"))
        assertTrue(body.contains("Id: abc123"))
        assertTrue(body.contains("unsolicited files"))
        assertTrue(body.contains("not a copy of the messages"))
    }

    @Test
    fun `an empty note omits the user-note block`() {
        val body = Legal.reportBody("room", "n:r", "General", "")
        assertFalse(body.contains("User note:"))
    }

    @Test
    fun `terms version is a positive integer`() {
        // A zero here would skip the accept gate: acceptedTermsVersion
        // defaults to 0, and Home is shown when accepted >= TERMS_VERSION.
        assertTrue(Legal.TERMS_VERSION > 0)
        assertEquals(
            "https://github.com/ConquerD/DoubleSlash/blob/develop/PRIVACY.md",
            Legal.PRIVACY_URL,
        )
    }
}
