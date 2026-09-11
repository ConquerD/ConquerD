package com.conquerd.client

import android.content.pm.ServiceInfo
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The foreground-service type mask is what Play and Android 14+ both review.
 *
 * These lock in two claims: the standing session is `specialUse` (not the
 * 6-hour `dataSync` bucket), and microphone/camera bits are only added when
 * a call is active *and* the matching permission is held.
 */
class CoreServiceTypesTest {

    @Test
    fun `an idle session is specialUse only`() {
        val types = coreForegroundTypes(
            microphoneActive = false,
            cameraActive = false,
            hasMicrophonePermission = false,
            hasCameraPermission = false,
        )
        assertEquals(ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE, types)
        assertEquals(
            "dataSync is a transfer type, not a standing P2P session",
            0,
            types and ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
        )
    }

    @Test
    fun `microphone is claimed only when a call is active and the permission is held`() {
        val noPermission = coreForegroundTypes(
            microphoneActive = true,
            cameraActive = false,
            hasMicrophonePermission = false,
            hasCameraPermission = false,
        )
        assertEquals(ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE, noPermission)

        val withPermission = coreForegroundTypes(
            microphoneActive = true,
            cameraActive = false,
            hasMicrophonePermission = true,
            hasCameraPermission = false,
        )
        assertTrue(
            withPermission and ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE != 0,
        )
        assertTrue(
            withPermission and ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE != 0,
        )
        assertEquals(0, withPermission and ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA)
    }

    @Test
    fun `camera is claimed only when video is active and the permission is held`() {
        val grantedIdle = coreForegroundTypes(
            microphoneActive = false,
            cameraActive = false,
            hasMicrophonePermission = true,
            hasCameraPermission = true,
        )
        assertEquals(
            "a grant without an active call must not claim capture types",
            ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
            grantedIdle,
        )

        val video = coreForegroundTypes(
            microphoneActive = true,
            cameraActive = true,
            hasMicrophonePermission = true,
            hasCameraPermission = true,
        )
        assertNotEquals(0, video and ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
        assertNotEquals(0, video and ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA)
        assertNotEquals(0, video and ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        assertEquals(0, video and ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
    }
}
