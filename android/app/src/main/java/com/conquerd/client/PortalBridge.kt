package com.conquerd.client

import android.webkit.JavascriptInterface
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.util.Log
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.put
import java.io.File
import java.io.FileInputStream

/**
 * The native half of `window.conquerd`, for portal pages in a WebView.
 *
 * A `conquerd://` page is not fetchable by a browser: the supernode serves it
 * over the identity QUIC relay through `web.host.app.v1`, so every request the
 * WebView makes has to be intercepted and answered by the core. That is what
 * [interceptRequest] does; [PortalApi] is the JS-visible object the shim drives
 * once the page is running.
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
        if (!isPortalUrl(url)) return null

        val path = url.path.orEmpty().ifEmpty { "/index.html" }

        val reply = runBlocking {
            core.command("portal.fetch") {
                put("supernode_id", supernodeOf(url))
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
        val mime = contentType.substringBefore(';').trim()

        // The bridge is injected into the HTML rather than evaluated after
        // load. Pages test for `window.conquerd` while they parse, so anything
        // that runs on page-finished is far too late — the page has already
        // decided it is in the wrong browser. The desktop gets this for free
        // with QWebEngineScript's DocumentCreation injection point; WebView's
        // WebViewClient has no equivalent, but every byte of the document
        // passes through here, so the script can simply lead it.
        val body = if (mime == "text/html") {
            injectBridge(file.readText()).byteInputStream()
        } else {
            FileInputStream(file)
        }

        return WebResourceResponse(
            mime,
            contentType.substringAfter("charset=", "utf-8").trim(),
            reply.number("status").toInt().takeIf { it in 100..599 } ?: 200,
            "OK",
            emptyMap(),
            body,
        )
    }

    /**
     * Whether this is a request the portal answers.
     *
     * Both spellings stay live: [PORTAL_ORIGIN] is what the WebView loads and
     * what every relative URL in a page resolves against, while `d://` and
     * `conquerd://` still appear in hand-off links written by pages and by
     * the desktop client.
     */
    private fun isPortalUrl(url: android.net.Uri): Boolean =
        url.scheme == SCHEME ||
            url.scheme == SCHEME_LEGACY ||
            (url.scheme.equals("https", true) && url.host == PORTAL_HOST)

    /**
     * Which supernode a portal URL is asking about.
     *
     * A `d://` URL carries it as the authority. [PORTAL_ORIGIN] does not - it
     * is one fixed host - so those resolve to the supernode this bridge was
     * opened for, which is also the only one it is allowed to reach.
     */
    private fun supernodeOf(url: android.net.Uri): String =
        if (url.host == PORTAL_HOST) supernodeId else url.host ?: supernodeId

    /** Put the bridge script ahead of anything the document might run. */
    private fun injectBridge(html: String): String {
        val tag = "<script>${bootstrapJs()}</script>"

        // After <head> when there is one, so the document still parses as the
        // author wrote it; otherwise lead the document.
        val head = HEAD_OPEN.find(html)
        return when {
            head != null -> html.substring(0, head.range.last + 1) +
                tag +
                html.substring(head.range.last + 1)
            else -> tag + html
        }
    }

    private fun errorResponse(message: String) = WebResourceResponse(
        "text/plain",
        "utf-8",
        502,
        "Portal error",
        emptyMap(),
        message.byteInputStream(),
    )

    /**
     * The shim that becomes `window.conquerd`.
     *
     * Deliberately the same surface the desktop injects in `scheme.cpp`: a
     * frozen `{supernodeId, ready}` whose promise resolves to the API object.
     * A page written against the desktop must not have to care which client it
     * is running in, so the names match exactly — `closeChannel`, not `close`.
     */
    private fun bootstrapJs(): String {
        // Quoted through the JSON encoder rather than by hand: this lands
        // inside a JS string literal, and an id carrying a quote would
        // otherwise close the literal and run as code.
        val sn = JsonPrimitive(supernodeId).toString()
        return """
        (function () {
          if (window.conquerd) return;
          var raw = window.__conquerdNative;
          if (!raw) return;
          var sn = $sn;
          var parse = function (s) {
            try { return JSON.parse(s); } catch (e) { return { ok: false, error: 'bad reply' }; }
          };
          var api = Object.freeze({
            myPeerId: raw.myPeerId(),
            version: raw.version(),
            nativeTransport: true,
            supernodeId: sn,
            openChannel: function (room) {
              return Promise.resolve(parse(raw.openChannel(room || 'default')));
            },
            sendDatagramB64: function (b64) {
              return Promise.resolve(parse(raw.sendDatagramB64(b64)));
            },
            pollDatagrams: function () {
              return Promise.resolve(parse(raw.pollDatagrams()));
            },
            closeChannel: function () {
              return Promise.resolve(parse(raw.closeChannel()));
            },
            fetch: function (path, opts) {
              // The page's own origin, not one built out of `sn`: the portal
              // is served from a fixed host and the supernode is implied by
              // which portal is open, not spelled in the URL.
              var origin = window.location.origin;
              var url = path.charAt(0) === '/' ? origin + path : origin + '/' + path;
              return window.fetch(url, opts);
            }
          });

          // Route page fetches of conquerd:// through the bridge. Chromium
          // refuses the scheme outright, so without this a page's own API
          // calls fail with "URL scheme conquerd is not supported" while its
          // documents and assets - which take the interceptor path - load fine.
          var nativeFetch = window.fetch ? window.fetch.bind(window) : null;
          window.fetch = function (input, opts) {
            var href;
            try {
              href = new URL(
                typeof input === 'string' ? input : (input && input.url) || '',
                window.location.href
              ).href;
            } catch (e) {
              href = '';
            }

            if (href.indexOf('conquerd:') !== 0 && href.indexOf('d://') !== 0) {
              if (!nativeFetch) return Promise.reject(new Error('fetch unavailable'));
              return nativeFetch(input, opts);
            }

            return new Promise(function (resolve, reject) {
              var res = parse(raw.fetchB64(href));
              if (!res || res.ok === false) {
                reject(new TypeError(res && res.error ? res.error : 'portal fetch failed'));
                return;
              }
              var binary = atob(res.body || '');
              var bytes = new Uint8Array(binary.length);
              for (var i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
              resolve(new Response(bytes, {
                status: res.status || 200,
                headers: { 'Content-Type': res.content_type || 'text/plain' }
              }));
            });
          };
          Object.defineProperty(window, 'conquerd', {
            configurable: false,
            writable: false,
            value: Object.freeze({ supernodeId: sn, ready: Promise.resolve(api) })
          });
        })();
        """.trimIndent()
    }

    /** The object exposed to page JS, wrapped by the shim above. */
    inner class PortalApi {

        /** The identity the page sees — the base64url public id, as on desktop. */
        @JavascriptInterface
        fun myPeerId(): String = myPeerId

        @JavascriptInterface
        fun version(): String = core.version()

        /**
         * Fetch a `conquerd://` URL on behalf of page script.
         *
         * `shouldInterceptRequest` only sees document and subresource loads.
         * Chromium rejects `fetch()` and XHR on an unregistered scheme before
         * the interceptor runs — "URL scheme conquerd is not supported" — so
         * script-initiated requests have to come back through here instead.
         * Qt WebEngine avoids this by registering the scheme properly, which
         * WebView has no equivalent of.
         *
         * Method and body are dropped, matching the desktop: `scheme.cpp` never
         * reads the request method and `FetchWebApp` carries neither, so every
         * portal request is a GET with a query string on both clients.
         */
        @JavascriptInterface
        fun fetchB64(rawUrl: String): String {
            val url = runCatching { android.net.Uri.parse(rawUrl) }.getOrNull()
                ?: return failure("that is not a URL")
            if (!isPortalUrl(url)) return failure("not a portal URL")

            val reply = runBlocking {
                core.command("portal.fetch") {
                    put("supernode_id", supernodeOf(url))
                    put("path", url.path.orEmpty().ifEmpty { "/" })
                    url.query?.let { put("query", it) }
                }
            }
            if (!reply.ok) return failure(reply.errorText ?: "portal fetch failed")

            val file = File(reply.stringOrEmpty("path"))
            if (!file.exists()) return failure("portal response missing")
            if (file.length() > MAX_INLINE_BODY) {
                // Page APIs return small JSON. Anything this large is a document
                // or asset, which loads through the interceptor without ever
                // passing the bytes through JavaScript.
                return failure("response too large for a page fetch")
            }

            val body = android.util.Base64.encodeToString(
                file.readBytes(),
                android.util.Base64.NO_WRAP,
            )
            return buildString {
                append("{\"ok\":true,\"status\":")
                append(reply.number("status").toInt().takeIf { it in 100..599 } ?: 200)
                append(",\"content_type\":")
                append(jsonString(reply.stringOrEmpty("content_type").ifBlank { "text/plain" }))
                append(",\"body\":")
                append(jsonString(body))
                append("}")
            }
        }

        private fun failure(message: String) =
            "{\"ok\":false,\"error\":${jsonString(message)}}"

        private fun jsonString(value: String) =
            kotlinx.serialization.json.JsonPrimitive(value).toString()

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
        fun closeChannel(): String = call("portal.close") {
            put("supernode_id", supernodeId)
        }

        private fun call(
            name: String,
            build: kotlinx.serialization.json.JsonObjectBuilder.() -> Unit,
        ): String = runBlocking { core.command(name, build) }.toString()
    }

    companion object {
        const val SCHEME = "d"
        const val SCHEME_LEGACY = "conquerd"

        /**
         * The origin a portal is served from inside the WebView.
         *
         * Not `d://`, and that is the whole point. WebView has no equivalent
         * of `QWebEngineUrlScheme::registerScheme`, so `d://` there is a
         * non-standard scheme: Chromium gives it an opaque origin and refuses
         * it outright for anything script-initiated. [PortalApi.fetchB64]
         * papers over `fetch` and XHR by monkey-patching them, but an
         * `<script type="module">` and the `import` graph under it are fetched
         * by Chromium's module loader, which no page script can reach - so on
         * `d://` every module in the portal silently never runs. The document
         * and its stylesheets take the interceptor path and appear, which is
         * why the result looks like a page that loaded and then did nothing.
         *
         * `https` is a registered, standard, secure scheme, so modules,
         * workers and storage all behave. Requests never leave the device:
         * [interceptRequest] answers this host and [PortalScreen] blocks
         * everything else. The `.invalid` TLD is reserved by RFC 2606 and can
         * never resolve, so a request that somehow escaped the interceptor
         * fails closed rather than reaching a real server.
         */
        const val PORTAL_HOST = "portal.doubleslash.invalid"

        /** Where [PortalScreen] points the WebView. */
        const val PORTAL_ORIGIN = "https://$PORTAL_HOST"
        private const val TAG = "PortalBridge"
        private val HEAD_OPEN = Regex("<head[^>]*>", RegexOption.IGNORE_CASE)

        /**
         * Ceiling on a body handed to page script.
         *
         * Documents and assets never come this way - they load through the
         * interceptor as a stream - so this only bounds page API responses,
         * which are small.
         */
        private const val MAX_INLINE_BODY = 4L * 1024 * 1024
    }
}
