package com.conquerd.client

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Log
import java.io.File

/**
 * Moving files between the Storage Access Framework and the core.
 *
 * The core streams a file from a path for the whole transfer, which can be
 * minutes. A SAF `content://` uri does not survive that: the permission is
 * scoped to the picker result and the provider behind it may be a cloud
 * document with no bytes on this device at all. So a file being sent is copied
 * into the app sandbox first, and a file being saved is copied back out.
 *
 * The copy is the price of the SAF, not an oversight — there is no reliable
 * filesystem path for a picked document on modern Android.
 */
object FileStaging {

    private const val TAG = "FileStaging"

    /** A file copied into the sandbox, ready to hand to the core. */
    data class Staged(val path: String, val displayName: String, val size: Long)

    /**
     * Copy the picked document into cache storage.
     *
     * Returns null when the document cannot be read — a revoked permission or
     * a cloud file with nothing to download.
     */
    fun stageForSend(context: Context, uri: Uri): Staged? {
        val name = displayName(context, uri) ?: "file"
        val outDir = File(context.cacheDir, "outbound").apply { mkdirs() }

        // Namespaced by time: picking two files with the same name in one
        // session must not have the second overwrite the first mid-transfer.
        val out = File(outDir, "${System.currentTimeMillis()}-${name.replace('/', '_')}")

        return runCatching {
            context.contentResolver.openInputStream(uri)!!.use { input ->
                out.outputStream().use { output -> input.copyTo(output) }
            }
            Staged(out.absolutePath, name, out.length())
        }.getOrElse { e ->
            Log.w(TAG, "could not stage $uri for sending", e)
            out.delete()
            null
        }
    }

    /**
     * Copy a received file out to wherever the user chose to save it.
     *
     * The received file stays in app storage either way: this is a copy out,
     * not a move, so a failed save does not lose the download.
     */
    fun exportTo(context: Context, sourcePath: String, destination: Uri): Boolean =
        runCatching {
            context.contentResolver.openOutputStream(destination)!!.use { output ->
                File(sourcePath).inputStream().use { input -> input.copyTo(output) }
            }
            true
        }.getOrElse { e ->
            Log.w(TAG, "could not save $sourcePath to $destination", e)
            false
        }

    /** The document's own name, which is not derivable from the uri. */
    private fun displayName(context: Context, uri: Uri): String? = runCatching {
        context.contentResolver.query(uri, null, null, null, null)?.use { cursor ->
            val column = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
            if (column >= 0 && cursor.moveToFirst()) cursor.getString(column) else null
        }
    }.getOrNull()
}
