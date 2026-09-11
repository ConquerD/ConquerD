package com.conquerd.client

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log

/**
 * Store-facing legal URLs and the in-app report helper.
 *
 * Play wants a public policy URL, terms accepted before user-generated
 * content, and a way to report. There is no DoubleSlash backend to file
 * into, so a report is a document the user sends (GitHub or their mail app).
 */
object Legal {
    const val TERMS_VERSION = 1

    const val PRIVACY_URL =
        "https://github.com/ConquerD/DoubleSlash/blob/develop/PRIVACY.md"
    const val TERMS_URL =
        "https://github.com/ConquerD/DoubleSlash/blob/develop/TERMS.md"
    const val ISSUES_URL =
        "https://github.com/ConquerD/DoubleSlash/issues/new"
    const val CYBERTIP_URL = "https://report.cybertip.org/"

    fun openUrl(context: Context, url: String) {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url))
        runCatching { context.startActivity(intent) }
            .onFailure { Log.w(TAG, "could not open $url", it) }
    }

    /**
     * A chooser that sends a report the maintainers can actually receive.
     *
     * Body is plaintext: peer/room identifiers and what the user typed, never
     * message contents we do not have a right to copy out automatically.
     */
    fun shareReport(context: Context, subject: String, body: String) {
        val send = Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_SUBJECT, subject)
            putExtra(Intent.EXTRA_TEXT, body)
        }
        runCatching {
            context.startActivity(Intent.createChooser(send, "Send report"))
        }.onFailure { Log.w(TAG, "could not share report", it) }
    }

    fun reportSubject(target: String): String = "DoubleSlash report: $target"

    fun reportBody(
        kind: String,
        targetId: String,
        targetLabel: String,
        note: String,
    ): String = buildString {
        appendLine("This is a user report from the DoubleSlash Android client.")
        appendLine()
        appendLine("Kind: $kind")
        appendLine("Label: $targetLabel")
        appendLine("Id: $targetId")
        if (note.isNotBlank()) {
            appendLine()
            appendLine("User note:")
            appendLine(note.trim())
        }
        appendLine()
        appendLine("DoubleSlash cannot read end-to-end sealed content. This report")
        appendLine("is identifiers and the note above, not a copy of the messages.")
    }

    private const val TAG = "Legal"
}
