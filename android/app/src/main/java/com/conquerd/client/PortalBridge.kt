package com.conquerd.client

import android.webkit.JavascriptInterface
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.util.Log
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.put
import java.io.File
import java.io.FileInputStream

/**
 * The native half of `window.conquerd`, for portal pages in a WebView.
 *
 * A `conquerd://` page is not fetchable by a browser: the supernode serves it
 * over the identity QUIC relay through `web.host.app.v1`, so every request the
 * WebView makes has to be intercepted and answered by the core. That is what
 * [interceptRequest] does; [PortalApi] is the JS-visible object the web SDK
 * drives once the page is running.
 *
 * The SDK polls for inbound datagrams rather than being pushed to, which is
 * why there is no callback into JS here — the core buffers frames and
 * `pollDatagrams` drains them.
 */
class PortalBridge(
    private val core: ConquerdCore,
    private val supernodeId: String,
    private val myPeerId: String,
) {

    /**
     * Answer one WebView request out of the portal.
     *
     * Runs on WebView's own worker thread, so blocking here is correct — the
     * alternative is returning null and letting Chromium try the network,
     * which cannot reach a `conquerd://` host at all.
     */
    fun interceptRequest(request: WebResourceRequest): WebResourceResponse? {
        val url = request.url
        if (url.scheme != SCHEME) return null

        // The host is the supernode; everything after it is the page path.
        val path = url.path.orEmpty().ifEmpty { "/index.html" }

        val reply = runBlocking {
            core.command("portal.fetch") {
                put("supernode_id", url.host ?: supernodeId)
                put("path", path)
                url.query?.let { put("query", it) }
            }
        }

        if (!reply.ok) {
            Log.w(TAG, "portal fetch failed for $path: ${reply.errorText}")
            return errorResponse(reply.errorText ?: "portal unavailable")
        }

        val file = File(reply.stringOrEmpty("path"))
        if (!file.exists()) return errorResponse("portal response missing")

        val contentType = reply.stringOrEmpty("content_type").ifBlank { "text/html" }
        return WebResourceResponse(
            contentType.substringBefore(';').trim(),
            contentType.substringAfter("charset=", "utf-8").trim(),
            reply.number("status").toInt().takeIf { it in 100..599 } ?: 200,
            "OK",
            emptyMap(),
            FileInputStream(file),
        )
    }

    private fun errorResponse(message: String) = WebResourceResponse(
        "text/plain",
        "utf-8",
        502,
        "Portal error",
        emptyMap(),
        message.byteInputStream(),
    )

    /** The object exposed to page JS as `window.conquerd`'s backing API. */
    inner class PortalApi {

        /** The identity the page sees — the base64url public id, as on desktop. */
        @JavascriptInterface
        fun myPeerId(): String = myPeerId

        /**
         * Join a game lobby. Returns a JSON reply the shim turns into a promise.
         *
         * Every call returns JSON rather than throwing: an exception across the
         * JavascriptInterface boundary reaches the page as a bare "Error",
         * losing whatever the core said went wrong.
         */
        @JavascriptInterface
        fun openChannel(room: String): String = call("portal.open") {
            put("supernode_id", supernodeId)
            put("room", room)
        }

        @JavascriptInterface
        fun sendDatagramB64(payloadB64: String): String = call("portal.send") {
            put("supernode_id", supernodeId)
            put("payload", payloadB64)
        }

        @JavascriptInterface
        fun pollDatagrams(): String = call("portal.poll") {}

        @JavascriptInterface
        fun close(): String = call("portal.close") {
            put("supernode_id", supernodeId)
        }

        private fun call(
            name: String,
            build: kotlinx.serialization.json.JsonObjectBuilder.() -> Unit,
        ): String = runBlocking { core.command(name, build) }.toString()
    }

    companion object {
        const val SCHEME = "conquerd"
        private const val TAG = "PortalBridge"

        /**
         * The shim that turns the injected object into the API the SDK expects.
         *
         * `addJavascriptInterface` can only pass strings, so the promises,
         * JSON parsing and the `ready` handshake the web SDK waits on are built
         * here in JS over the string calls above.
         */
        val BOOTSTRAP_JS = """
            (function () {
              if (window.conquerd && window.conquerd.ready) return;
              const raw = window.__conquerdNative;
              if (!raw) return;
              const parse = (s) => { try { return JSON.parse(s); } catch (e) { return { ok: false, error: 'bad reply' }; } };
              const api = {
                myPeerId: raw.myPeerId(),
                openChannel: (room) => Promise.resolve(parse(raw.openChannel(room || 'default'))),
                sendDatagramB64: (b64) => Promise.resolve(parse(raw.sendDatagramB64(b64))),
                pollDatagrams: () => Promise.resolve(parse(raw.pollDatagrams())),
                close: () => Promise.resolve(parse(raw.close())),
              };
              window.conquerd = { ready: Promise.resolve(api), ...api };
            })();
        """.trimIndent()
    }
}
