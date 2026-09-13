package com.conquerd.client.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

/**
 * Inline Accept / Ignore for a DoubleSlash invite pasted into a chat.
 *
 * Mirrors the desktop's `ChatRichMessageDelegate` embed. An invite arrives as
 * an ordinary chat message, so without this the recipient has to select a long
 * opaque URL out of a bubble and find somewhere to paste it — on a phone that
 * is the difference between an invite being used and being ignored.
 */

/** What an invite link connects you to, which is what the card should say. */
enum class InviteKind { PEER, ROOM, UNKNOWN }

/**
 * The first invite link in a chat body, or `null`.
 *
 * Invites are minted as `https://doubleslash.space/i#…` (peer) and `/r#…`
 * (room). The `d://` and `doubleslash://` hand-off forms are still recognised
 * so a link pasted from somewhere else is offered too rather than sitting
 * there as dead text. Kept deliberately in step with the desktop's regexes —
 * a link one client embeds and the other does not is worse than neither.
 */
fun findInviteUrl(body: String): String? {
    HTTPS_INVITE.find(body)?.let { return it.value }
    return SCHEME_INVITE.find(body)?.value
}

fun inviteKindOf(url: String): InviteKind {
    val u = url.lowercase()
    return when {
        u.contains("doubleslash.space/r") || u.contains("://room#") -> InviteKind.ROOM
        u.contains("doubleslash.space/i") || u.contains("://invite#") -> InviteKind.PEER
        else -> InviteKind.UNKNOWN
    }
}

/**
 * The message text with the invite link removed.
 *
 * The card already shows (an abbreviated form of) the URL, so leaving the raw
 * link in the bubble above it just repeats forty opaque characters. Returns
 * blank when the message was nothing but the link, which the caller uses to
 * skip the text entirely.
 */
fun bodyWithoutInvite(body: String, url: String): String =
    body.replace(url, "").replace(WHITESPACE, " ").trim()

/** Abbreviate the middle: the ends are what identify an invite by eye. */
private fun shorten(url: String): String =
    if (url.length <= 42) url else url.take(28) + "\u2026" + url.takeLast(10)

@Composable
fun InviteEmbed(
    url: String,
    mine: Boolean,
    onAccept: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    // Session-local, matching the desktop: accepting is idempotent on the core
    // side, but a card that stays live invites a second tap that looks like it
    // did nothing. Ignoring is a per-view dismissal, deliberately not
    // persisted — the message is still there to act on later.
    var accepted by remember(url) { mutableStateOf(false) }
    var ignored by remember(url) { mutableStateOf(false) }
    if (ignored) return

    val kind = inviteKindOf(url)
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.secondaryContainer,
        ),
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(10.dp)) {
            Text(
                title(kind, mine),
                style = MaterialTheme.typography.labelLarge,
            )
            Text(
                subtitle(kind, mine, accepted),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.width(6.dp))
            Text(
                shorten(url),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            // Nothing to accept on an invite we sent: the buttons are for the
            // recipient, and offering "Accept" on your own link would just
            // add you to your own trust list.
            if (!mine) {
                Row {
                    TextButton(
                        onClick = {
                            accepted = true
                            onAccept(url)
                        },
                        enabled = !accepted,
                    ) {
                        Text(if (accepted) "Accepting\u2026" else "Accept")
                    }
                    TextButton(onClick = { ignored = true }) { Text("Ignore") }
                }
            }
        }
    }
}

private fun title(kind: InviteKind, mine: Boolean): String = when (kind) {
    InviteKind.ROOM -> if (mine) "Room invite shared" else "Room invite"
    InviteKind.PEER -> if (mine) "Peer invite shared" else "Peer invite"
    InviteKind.UNKNOWN -> if (mine) "Invite shared" else "DoubleSlash invite"
}

private fun subtitle(kind: InviteKind, mine: Boolean, accepted: Boolean): String = when {
    accepted -> "Accepting\u2026"
    mine -> "They can Accept from their chat to connect."
    kind == InviteKind.ROOM -> "Join this room on a trusted supernode."
    kind == InviteKind.PEER -> "Add this peer to your trusted list."
    else -> "Open this invite to connect."
}

private val HTTPS_INVITE =
    Regex("""https?://(?:www\.)?doubleslash\.space/[ir]/?#[^\s<>"']+""", RegexOption.IGNORE_CASE)

private val SCHEME_INVITE =
    Regex("""(?:doubleslash|d)://[^\s<>"']+""", RegexOption.IGNORE_CASE)

private val WHITESPACE = Regex("""\s+""")
