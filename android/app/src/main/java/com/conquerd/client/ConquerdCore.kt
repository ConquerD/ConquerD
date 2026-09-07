package com.conquerd.client

import android.content.Context
import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

private const val TAG = "ConquerdCore"

/**
 * The app's single handle on the Rust client core.
 *
 * One instance per process. The core owns its own threads and stores, so
 * starting a second one against the same directory would have two SQLite
 * writers and two QUIC endpoints claiming the same identity.
 */
class ConquerdCore private constructor(context: Context) : NativeCore.EventSink {

    private val appContext = context.applicationContext

    /**
     * Where the core keeps identity, peers, chat and rooms.
     *
     * `filesDir` rather than external storage: this is key material and
     * message history, and app-private storage is the only location Android
     * keeps out of reach of other apps without extra permissions.
     */
    private val homeDir: String get() = appContext.filesDir.resolve("conquerd").absolutePath

    @Volatile
    private var handle: Long = 0L

    /** Guards start/stop so two callers cannot race a second core into life. */
    private val lifecycleLock = Any()

    val isRunning: Boolean get() = handle != 0L

    private val _events = MutableSharedFlow<JsonObject>(
        replay = 0,
        extraBufferCapacity = 512,
        // Events arrive from a Rust thread that must not block on a slow
        // collector; dropping the oldest keeps the core running and costs a
        // stale UI update rather than a stalled connection.
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    /** Core events, as they arrive. */
    val events: SharedFlow<JsonObject> = _events.asSharedFlow()

    val json: Json = Json {
        ignoreUnknownKeys = true
        isLenient = true
        encodeDefaults = true
    }

    /** Core version — safe to call before [start]. */
    fun version(): String = runCatching { NativeCore.nativeVersion() }.getOrDefault("unknown")

    /**
     * Start the core, creating the identity on first launch.
     *
     * @param passphrase empty means an unencrypted identity.
     */
    suspend fun start(passphrase: String): Result<Unit> = withContext(Dispatchers.IO) {
        synchronized(lifecycleLock) {
            if (isRunning) return@withContext Result.success(Unit)
            runCatching {
                appContext.filesDir.resolve("conquerd").mkdirs()
                val started =
                    NativeCore.nativeStart(homeDir, passphrase, appContext, this@ConquerdCore)
                check(started != 0L) { "the core returned no handle" }
                Log.i(TAG, "core started, version ${version()}")
                // Publish the handle last: until it is set `isRunning` is
                // false, so a racing command fails cleanly instead of calling
                // into a half-built core.
                handle = started
            }
        }
    }

    /** Stop the core. Idempotent. */
    fun stop() {
        synchronized(lifecycleLock) {
            val current = handle
            if (current == 0L) return
            // Clear first: a command racing this must fail against a zero
            // handle rather than call into a core that is tearing down.
            handle = 0L
            NativeCore.nativeStop(current)
            Log.i(TAG, "core stopped")
        }
    }

    /**
     * Run one command and parse the reply.
     *
     * Commands are cheap but not free — some of them touch SQLite — so this
     * always hops to the IO dispatcher rather than trusting callers to.
     */
    suspend fun command(request: JsonObject): JsonObject = withContext(Dispatchers.IO) {
        val current = handle
        if (current == 0L) return@withContext failure("the core is not running")

        val reply = runCatching { NativeCore.nativeCommand(current, request.toString()) }
            .getOrElse { e ->
                Log.e(TAG, "command failed", e)
                return@withContext failure(e.message ?: "native call failed")
            }

        runCatching { json.parseToJsonElement(reply).jsonObject }
            .getOrElse { failure("core returned unparseable JSON") }
    }

    /** Convenience for a command with no arguments beyond its name. */
    suspend fun command(name: String): JsonObject = command(buildJsonObject { put("cmd", name) })

    /** Build and run a command with arguments. */
    suspend fun command(
        name: String,
        build: kotlinx.serialization.json.JsonObjectBuilder.() -> Unit,
    ): JsonObject = command(
        buildJsonObject {
            put("cmd", name)
            build()
        },
    )

    override fun onEvent(json: String) {
        // Called from the Rust event pump. Parsing here rather than in each
        // collector keeps the malformed-payload case in one place.
        val parsed = runCatching { this.json.parseToJsonElement(json).jsonObject }.getOrNull()
        if (parsed == null) {
            Log.w(TAG, "dropped unparseable event")
            return
        }
        if (!_events.tryEmit(parsed)) {
            Log.w(TAG, "event buffer full — dropped ${parsed.eventName()}")
        }
    }

    companion object {
        @Volatile
        private var instance: ConquerdCore? = null

        fun get(context: Context): ConquerdCore =
            instance ?: synchronized(this) {
                instance ?: ConquerdCore(context).also { instance = it }
            }

        private fun failure(reason: String): JsonObject = buildJsonObject {
            put("ok", false)
            put("error", reason)
        }
    }
}

// ── Reply helpers ──────────────────────────────────────────────────────────

/** True when the core reported success. */
val JsonObject.ok: Boolean
    get() = (this["ok"] as? JsonPrimitive)?.booleanOrNull ?: false

/** The error text of a failed reply, or null. */
val JsonObject.errorText: String?
    get() = (this["error"] as? JsonPrimitive)?.contentOrNull

/** The `event` discriminator, or empty for a command reply. */
fun JsonObject.eventName(): String = (this["event"] as? JsonPrimitive)?.contentOrNull.orEmpty()

/** Read a string field, or null. */
fun JsonObject.string(key: String): String? = (this[key] as? JsonPrimitive)?.contentOrNull

/** Read a string field, or empty. */
fun JsonObject.stringOrEmpty(key: String): String = string(key).orEmpty()

/** Read a numeric field, or 0.0. */
fun JsonObject.number(key: String): Double =
    (this[key] as? JsonPrimitive)?.contentOrNull?.toDoubleOrNull() ?: 0.0

/** Read an array-of-strings field, or empty. */
fun JsonObject.stringList(key: String): List<String> =
    (this[key] as? JsonArray)?.mapNotNull { (it as? JsonPrimitive)?.contentOrNull }.orEmpty()

/**
 * Decode a list field into typed models.
 *
 * A decode failure yields an empty list rather than throwing: a core that
 * gained a field the app cannot read should degrade to an empty screen, not
 * crash the process.
 */
inline fun <reified T> JsonObject.decodeList(core: ConquerdCore, key: String): List<T> {
    val array = this[key] ?: return emptyList()
    return runCatching { core.json.decodeFromJsonElement<List<T>>(array) }.getOrDefault(emptyList())
}
