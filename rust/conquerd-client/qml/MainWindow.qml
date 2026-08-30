// MainWindow.qml — Conquerd native client main window (Phase 3 scaffold)
//
// Hosts the navigation rail, chat panel, call panel, and session banner.
// Binds to AppBridge (exposed from Rust via cxx-qt) for live state.

import QtQuick
import QtQuick.Window
import QtQuick.Controls.Material
import QtQuick.Layouts
import Qt.labs.platform as Platform
import ConquerD.Client 1.0

ApplicationWindow {
    id: root
    title: ""
    width: 1100
    height: 700
    visible: false
    minimumWidth: 960
    minimumHeight: 640
    // CustomizeWindowHint hides Qt's default title-bar widgets. On Windows,
    // window_chrome.cpp re-applies WS_CAPTION|WS_THICKFRAME and handles
    // WM_NCCALCSIZE so DWM Aero Snap / drag-to-edge works with our QML
    // TitleBar (startSystemMove). Without that helper, CustomizeWindowHint
    // alone omits WS_CAPTION and snap is broken.
    flags: Qt.Window | Qt.CustomizeWindowHint

    Material.theme: Theme.isDark ? Material.Dark : Material.Light
    Material.accent: Material.Blue

    function applyThemePreference(value) {
        var useDark = true
        if (value === "light") {
            useDark = false
        } else if (value === "system") {
            useDark = Qt.styleHints.colorScheme === Qt.ColorScheme.Dark
        }
        Theme.isDark = useDark
        Material.theme = useDark ? Material.Dark : Material.Light
    }

    function showFilePreview(path) {
        if (!path || path === "") return
        // FilePreviewPanel requires the webengine feature. Fall back to the
        // system app when the panel module is not linked into this build.
        function openInSystem() {
            var n = ("" + path).replace(/\\/g, "/")
            if (n.charAt(0) !== "/") n = "/" + n
            Qt.openUrlExternally("file://" + n)
        }
        if (filePreviewLoader.item) {
            filePreviewLoader.item.filePath = path
            filePreviewLoader.visible = true
            return
        }
        filePreviewLoader.active = true
        filePreviewLoader.setSource(Qt.resolvedUrl("FilePreviewPanel.qml"), {
            "filePath": path
        })
        // setSource is async; check after a tick. On failure, open externally.
        Qt.callLater(function() {
            if (filePreviewLoader.status === Loader.Ready && filePreviewLoader.item) {
                filePreviewLoader.visible = true
            } else {
                filePreviewLoader.active = false
                filePreviewLoader.source = ""
                openInSystem()
            }
        })
    }

    function canonicalNodeId(nodeId) {
        if (!nodeId || nodeId === "") return ""
        if (!backend.isKnownSupernode(nodeId)) return ""
        var resolved = backend.resolveSupernodeNodeId(nodeId)
        if (resolved === "") resolved = nodeId
        // Fold every member of a cluster to its stable representative id so the
        // sidebar shows one logical node (one row, one avatar, one green dot,
        // one merged room list) regardless of which member currently hosts us.
        var rep = backend.clusterRepresentative(resolved)
        return rep !== "" ? rep : resolved
    }

    function pruneNonSupernodeEntries() {
        for (var j = nodeListModel.count - 1; j >= 0; j--) {
            if (!backend.isKnownSupernode(nodeListModel.get(j).node_id))
                nodeListModel.remove(j)
        }
    }

    function findNodeIndex(nodeId) {
        var canon = canonicalNodeId(nodeId)
        if (canon === "") return -1
        for (var j = 0; j < nodeListModel.count; j++) {
            var existing = nodeListModel.get(j).node_id
            if (existing === canon || canonicalNodeId(existing) === canon)
                return j
        }
        return -1
    }

    function findNodeIndexRaw(nodeId) {
        if (!nodeId || nodeId === "") return -1
        for (var j = 0; j < nodeListModel.count; j++) {
            if (nodeListModel.get(j).node_id === nodeId)
                return j
        }
        return -1
    }

    function supernodeHandleFor(nodeId) {
        var idx = findNodeIndex(nodeId)
        if (idx < 0) return ""
        var handle = nodeListModel.get(idx).title || ""
        return handle
    }

    // Per-supernode transport stats keyed by canonical node id (from connectionStats).
    property var nodeConnectionStats: ({})

    function upsertNodeConnectionStats(peerId, stats) {
        var canon = canonicalNodeId(peerId)
        if (canon === "") return
        var next = Object.assign({}, nodeConnectionStats)
        next[canon] = stats
        nodeConnectionStats = next
    }

    function clearNodeConnectionStats(nodeId) {
        var canon = canonicalNodeId(nodeId)
        if (canon === "" && nodeId) canon = nodeId
        if (canon === "" || !nodeConnectionStats[canon]) return
        var next = Object.assign({}, nodeConnectionStats)
        delete next[canon]
        nodeConnectionStats = next
    }

    function supernodeAvatarTooltip(nodeId, connected) {
        var id = canonicalNodeId(nodeId)
        if (id === "") id = nodeId || ""
        var lines = [id]
        var stats = nodeConnectionStats[id]
        if (stats && stats.rtt_ms > 0) {
            lines.push("Ping: " + Math.round(stats.rtt_ms) + " ms")
            lines.push("Packet loss: " + (stats.packet_loss_pct || 0).toFixed(1)
                + "% · Jitter: " + Math.round(stats.jitter_ms || 0) + " ms")
        } else if (connected) {
            lines.push("Ping: —")
            lines.push("Packet loss: — · Jitter: —")
        }
        lines.push("")
        lines.push("Left click — open portal")
        lines.push("Right click — options")
        return lines.join("\n")
    }

    function peerHandleFor(peerId) {
        if (!peerId || peerId === "") return ""
        for (var row = 0; row < peerModel.rowCount(); row++) {
            var idx = peerModel.index(row, 0)
            if (peerModel.data(idx, 256).toString() === peerId)
                return peerModel.data(idx, 257).toString()
        }
        if (peerId.length > 12)
            return peerId.substring(0, 12) + "…"
        return peerId
    }

    function activeCallPeerHandle() {
        var peerId = root.activeCallPeerId || chatPanel.selectedPeerId
        if (!peerId || peerId === "") return ""
        var handle = root.peerHandleFor(peerId)
        if (chatPanel.selectedPeerId === peerId && chatPanel.selectedPeerName !== "")
            return chatPanel.selectedPeerName
        return handle
    }

    function trackDirectCall(peerId) {
        root.activeCallPeerId = peerId || ""
    }

    /// Dial `peerId` 1:1, from wherever the user pressed call.
    ///
    /// `start_call` leaves any live voice room first — room audio and a direct
    /// call cannot both run. The expanded tiles and popout windows belong to
    /// that room's video, so collapse them here, the same housekeeping the
    /// Leave and End buttons do; the bridge cannot close QML windows.
    function beginDirectCall(peerId) {
        if (backend.voice_active && backend.in_room) {
            root.closeAllVideoPopouts()
            root.expandedVideoPeers = []
        }
        root.trackDirectCall(peerId)
        backend.startCall(peerId)
    }

    function refreshDirectCallModel() {
        directCallModel.clear()
        var remoteId = root.activeCallPeerId || chatPanel.selectedPeerId
        if (!remoteId || remoteId === "") return
        directCallModel.append({
            peerId: backend.public_id,
            handle: (settingsModel.local_handle && settingsModel.local_handle !== "")
                ? settingsModel.local_handle
                : backend.public_id,
            muted: voiceRail.muted,
            audioLevel: 0.0,
            isSelf: true
        })
        directCallModel.append({
            peerId: remoteId,
            handle: root.activeCallPeerHandle(),
            muted: false,
            audioLevel: 0.0,
            isSelf: false
        })
    }

    function numericRoomCount(value) {
        var n = Number(value)
        return isNaN(n) ? 0 : Math.max(0, n)
    }

    // Collapsed room subtrees, keyed `node_id:room_id`. Reassigned (not mutated)
    // on toggle so the `roomsTree` bindings re-evaluate.
    property var collapsedRooms: ({})

    function toggleRoomCollapse(nodeId, roomId) {
        var key = nodeId + ":" + roomId
        var next = {}
        for (var k in root.collapsedRooms)
            if (root.collapsedRooms.hasOwnProperty(k)) next[k] = root.collapsedRooms[k]
        if (next[key]) delete next[key]
        else next[key] = true
        root.collapsedRooms = next
    }

    // Flatten a node's room list into Space-tree order: DFS pre-order so each
    // parent is immediately followed by its descendants. Each emitted item gains:
    //   tree_depth   — nesting depth (0 = top-level)
    //   has_children — whether it has sub-rooms (shows the expand/collapse toggle)
    //   collapsed    — whether it is currently collapsed
    //   guide_cols   — per-column connector codes for the tree lines:
    //                  0 = blank, 1 = pass-through │, 2 = └ (last child), 3 = ├
    // A room is top-level when its `parent_id` is "" or points outside the list
    // (the Server node / a legacy room). Collapsed subtrees are traversed (to
    // mark them seen) but not emitted. Cycle- and self-parent-guarded.
    function roomTreeOrder(rooms, nodeId, collapsed) {
        if (!Array.isArray(rooms)) return []
        var byId = {}
        var i
        for (i = 0; i < rooms.length; i++)
            if (rooms[i] && rooms[i].room_id) byId[rooms[i].room_id] = rooms[i]

        var childrenOf = {}
        var roots = []
        for (i = 0; i < rooms.length; i++) {
            var r = rooms[i]
            if (!r || !r.room_id) continue
            var pid = r.parent_id || ""
            if (pid !== "" && pid !== r.room_id && byId.hasOwnProperty(pid)) {
                if (!childrenOf[pid]) childrenOf[pid] = []
                childrenOf[pid].push(r)
            } else {
                roots.push(r)
            }
        }

        // Explicit stack (avoids recursion); reverse-push keeps display order.
        // Each frame carries `isLast` (last among siblings, for └ vs ├),
        // `passLines` (ancestor vertical-line flags), and `visible` (false under
        // a collapsed ancestor — still traversed to mark seen, not emitted).
        var out = []
        var seen = {}
        var stack = []
        var s
        for (s = roots.length - 1; s >= 0; s--)
            stack.push({ room: roots[s], depth: 0, isLast: (s === roots.length - 1),
                         passLines: [], visible: true })
        var guard = 0
        while (stack.length > 0 && guard < 8192) {
            guard++
            var top = stack.pop()
            var rr = top.room
            if (seen[rr.room_id]) continue // guard against duplicate visits
            seen[rr.room_id] = true
            var kids = childrenOf[rr.room_id]
            var hasKids = !!(kids && kids.length)
            var isCollapsed = hasKids && collapsed
                && collapsed[nodeId + ":" + rr.room_id] === true
            if (top.visible) {
                var cols = []
                for (var k = 0; k < top.depth - 1; k++)
                    cols.push(top.passLines[k] ? 1 : 0)
                if (top.depth > 0) cols.push(top.isLast ? 2 : 3)
                var item = {}
                for (var kk in rr) if (rr.hasOwnProperty(kk)) item[kk] = rr[kk]
                item.tree_depth = top.depth
                item.has_children = hasKids
                item.collapsed = isCollapsed
                item.guide_cols = cols
                out.push(item)
            }
            if (hasKids) {
                var childVisible = top.visible && !isCollapsed
                var childPass = top.passLines.concat([!top.isLast])
                for (var c = kids.length - 1; c >= 0; c--)
                    stack.push({ room: kids[c], depth: top.depth + 1,
                                 isLast: (c === kids.length - 1),
                                 passLines: childPass, visible: childVisible })
            }
        }
        // Safety net: never hide a room. Anything genuinely unreachable (e.g. a
        // malformed parent cycle) is appended flat at the top level.
        for (i = 0; i < rooms.length; i++) {
            if (rooms[i] && rooms[i].room_id && !seen[rooms[i].room_id]) {
                seen[rooms[i].room_id] = true
                var orphan = {}
                for (var ok in rooms[i]) if (rooms[i].hasOwnProperty(ok)) orphan[ok] = rooms[i][ok]
                orphan.tree_depth = 0
                orphan.has_children = false
                orphan.collapsed = false
                orphan.guide_cols = []
                out.push(orphan)
            }
        }
        return out
    }

    function roomVoiceCount(room) {
        if (room.voice_count !== undefined && room.voice_count !== null)
            return root.numericRoomCount(room.voice_count)
        if (room.count !== undefined && room.count !== null)
            return root.numericRoomCount(room.count)
        if (room.member_count !== undefined && room.member_count !== null)
            return root.numericRoomCount(room.member_count)
        return 0
    }

    function roomHasVoiceCount(room) {
        return (room.voice_count !== undefined && room.voice_count !== null)
            || (room.count !== undefined && room.count !== null)
            || (room.member_count !== undefined && room.member_count !== null)
    }

    // Text-chat occupancy (voice participants + chat-only subscribers) —
    // distinct from roomVoiceCount, which is voice-only. Server sends this
    // alongside voice_count/member_count on a full room list. Incremental
    // voice-roster patches (SfuMembers/PeerJoined/PeerLeft — fired on every
    // voice join/leave, including your own SfuSubscribe when you click a
    // room) never carry it, and DO carry a real voice_count, so gating on
    // the shared `count_known` would treat those patches as authoritative
    // for chat_count too and zero it out. Use a dedicated
    // `chat_count_known` instead (see mergeRoomEntry).
    function roomChatCount(room) {
        if (room.chat_count !== undefined && room.chat_count !== null)
            return root.numericRoomCount(room.chat_count)
        return 0
    }

    function roomHasChatCount(room) {
        return room.chat_count !== undefined && room.chat_count !== null
    }

    function roomKnownPeers(room) {
        if (!room.known_peers || !Array.isArray(room.known_peers))
            return []
        var out = []
        for (var i = 0; i < room.known_peers.length; i++) {
            var name = String(room.known_peers[i] || "").trim()
            if (name !== "")
                out.push(name)
        }
        return out
    }

    function roomUnknownPeerCount(room, voiceCount, knownPeers) {
        if (room.unknown_peers !== undefined && room.unknown_peers !== null)
            return root.numericRoomCount(room.unknown_peers)
        return Math.max(0, voiceCount - knownPeers.length)
    }

    // Rebuild the Rooms sidebar from the trusted peer store, preserving
    // per-node room snapshots and live connected/sfu flags where possible.
    function syncRoomsSidebar(nodesJson) {
        try {
            var desired = JSON.parse(nodesJson || "[]")
            var keepIds = {}
            for (var d = 0; d < desired.length; d++) {
                if (desired[d].node_id)
                    keepIds[desired[d].node_id] = true
            }
            for (var j = nodeListModel.count - 1; j >= 0; j--) {
                if (!keepIds[nodeListModel.get(j).node_id])
                    nodeListModel.remove(j)
            }
            for (var i = 0; i < desired.length; i++) {
                var node = desired[i]
                var nid = node.node_id || ""
                if (nid === "") continue
                var idx = findNodeIndexRaw(nid)
                if (idx >= 0) {
                    if (node.title !== undefined && node.title !== "")
                        nodeListModel.setProperty(idx, "title", node.title)
                    if (node.homepage_url !== undefined && node.homepage_url !== "")
                        nodeListModel.setProperty(idx, "homepage_url", node.homepage_url)
                } else {
                    nodeListModel.append({
                        node_id: nid,
                        connected: node.connected || false,
                        homepage_url: node.homepage_url || "",
                        title: node.title || "",
                        sfu_enabled: node.sfu_enabled || false,
                        public_rooms_enabled: node.public_rooms_enabled || false,
                        rooms_json: "[]"
                    })
                }
            }
        } catch (e) {
            console.warn("syncRoomsSidebar parse error:", e)
        }
    }

    function mergeRoomEntry(existing, incoming) {
        var merged = {}
        for (var k in existing) {
            if (existing.hasOwnProperty(k))
                merged[k] = existing[k]
        }
        for (var j in incoming) {
            if (!incoming.hasOwnProperty(j))
                continue
            var v = incoming[j]
            if (v === undefined || v === null)
                continue
            if ((j === "name" || j === "room_name" || j === "kind" || j === "room_type") && v === "")
                continue
            if ((j === "name" || j === "room_name")
                    && existing[j]
                    && incoming.room_id
                    && v === incoming.room_id)
                continue
            if ((j === "kind" || j === "room_type")
                    && existing[j]
                    && v === "voice"
                    && incoming.room_id
                    && !incoming.creator_id
                    && incoming.is_default === undefined)
                continue
            if ((j === "voice_count" || j === "member_count" || j === "count"
                    || j === "known_peers" || j === "unknown_peers")
                    && incoming.count_known === false)
                continue
            if (j === "count_known" && v === false && existing.count_known === true)
                continue
            // chat_count has its own staleness flag — a voice-only patch
            // (incoming.count_known === true, since it has a real
            // voice_count) must not zero out the last known chat_count.
            if (j === "chat_count" && incoming.chat_count_known === false)
                continue
            if (j === "chat_count_known" && v === false && existing.chat_count_known === true)
                continue
            // Keep a known Space parent when a supernode-sourced update (which
            // has no tree metadata) would otherwise blank it out.
            if (j === "parent_id" && v === "" && existing.parent_id)
                continue
            merged[j] = v
        }
        return merged
    }

    // Union two room snapshots by room_id (used when deduping alias node rows).
    function mergeRoomLists(a, b) {
        var byId = {}
        for (var i = 0; i < a.length; i++)
            if (a[i].room_id) byId[a[i].room_id] = a[i]
        for (var j = 0; j < b.length; j++)
            if (b[j].room_id)
                byId[b[j].room_id] = byId.hasOwnProperty(b[j].room_id)
                    ? root.mergeRoomEntry(byId[b[j].room_id], b[j])
                    : b[j]
        var out = []
        for (var k in byId) {
            if (byId.hasOwnProperty(k))
                out.push(byId[k])
        }
        return out
    }

    // Merge duplicate sidebar entries created when the same supernode was
    // keyed once by hex peer_id and once by base64url identity_pub.
    function dedupeNodeList() {
        var canonToIdx = {}
        var toRemove = []
        for (var j = 0; j < nodeListModel.count; j++) {
            var entry = nodeListModel.get(j)
            var canon = canonicalNodeId(entry.node_id)
            if (canon === "") continue
            if (canonToIdx.hasOwnProperty(canon)) {
                var keep = canonToIdx[canon]
                var keepEntry = nodeListModel.get(keep)
                var keepRooms = []
                var dupRooms = []
                try { keepRooms = JSON.parse(keepEntry.rooms_json || "[]") } catch (e) {}
                try { dupRooms = JSON.parse(entry.rooms_json || "[]") } catch (e) {}
                if (dupRooms.length > 0) {
                    var mergedRooms = root.mergeRoomLists(keepRooms, dupRooms)
                    nodeListModel.setProperty(keep, "rooms_json", JSON.stringify(mergedRooms))
                }
                if (!keepEntry.connected && entry.connected)
                    nodeListModel.setProperty(keep, "connected", true)
                if (!keepEntry.sfu_enabled && entry.sfu_enabled)
                    nodeListModel.setProperty(keep, "sfu_enabled", true)
                if (!keepEntry.public_rooms_enabled && entry.public_rooms_enabled)
                    nodeListModel.setProperty(keep, "public_rooms_enabled", true)
                if (!keepEntry.title && entry.title)
                    nodeListModel.setProperty(keep, "title", entry.title)
                if (!keepEntry.homepage_url && entry.homepage_url)
                    nodeListModel.setProperty(keep, "homepage_url", entry.homepage_url)
                nodeListModel.setProperty(keep, "node_id", canon)
                toRemove.push(j)
            } else {
                canonToIdx[canon] = j
                if (entry.node_id !== canon)
                    nodeListModel.setProperty(j, "node_id", canon)
            }
        }
        toRemove.sort(function(a, b) { return b - a })
        for (var k = 0; k < toRemove.length; k++)
            nodeListModel.remove(toRemove[k])
        pruneNonSupernodeEntries()
    }

    function upsertSfuRoomGroup(supernodeId, rooms, replaceRooms) {
        var canon = canonicalNodeId(supernodeId)
        if (canon === "") return

        var normalized = []
        for (var i = 0; i < rooms.length; i++) {
            var r = rooms[i]
            var voiceCount = root.roomVoiceCount(r)
            var countKnown = root.roomHasVoiceCount(r)
            var knownPeers = root.roomKnownPeers(r)
            normalized.push({
                room_id: r.room_id || "",
                name: r.name || r.room_name || r.room_id || "Room",
                kind: r.kind || r.room_type || "voice",
                voice_count: voiceCount,
                chat_count: root.roomChatCount(r),
                chat_count_known: root.roomHasChatCount(r),
                known_peers: knownPeers,
                unknown_peers: root.roomUnknownPeerCount(r, voiceCount, knownPeers),
                count_known: countKnown,
                creator_id: r.creator_id || "",
                is_default: r.is_default === true || r.room_id === "default",
                // Space-tree parent (carried from the local store) for sidebar
                // indent. Supernode-sourced rooms omit it → "" → top-level.
                parent_id: r.parent_id || ""
            })
        }

        var nodeIdx = findNodeIndex(canon)
        if (nodeIdx >= 0) {
            var existing = []
            try { existing = JSON.parse(nodeListModel.get(nodeIdx).rooms_json || "[]") } catch (e) {}
            var merged = existing
            if (replaceRooms === true)
                merged = normalized
            else if (normalized.length > 0)
                merged = root.mergeRoomLists(existing, normalized)
            nodeListModel.setProperty(nodeIdx, "node_id", canon)
            nodeListModel.setProperty(nodeIdx, "rooms_json", JSON.stringify(merged))
        } else if (normalized.length > 0) {
            var roomsJson = JSON.stringify(normalized)
            nodeListModel.append({
                node_id: canon,
                connected: false,
                homepage_url: "",
                title: "",
                sfu_enabled: false,
                public_rooms_enabled: false,
                rooms_json: roomsJson
            })
        }
        dedupeNodeList()
    }

    // ── Custom frameless title bar with embedded logo + invite controls ─────
    TitleBar {
        id: customTitleBar
        z: 200
        anchors {
            top: updateBanner.visible ? updateBanner.bottom : parent.top
            left: parent.left
            right: parent.right
        }
        appWindow: root

        Image {
            id: logoImage
            Layout.preferredWidth: 48
            Layout.preferredHeight: 22
            Layout.alignment: Qt.AlignVCenter
            fillMode: Image.PreserveAspectFit
            source: "qrc:/qt/qml/ConquerD/Client/icons/logo.svg"
        }

        // Invite / peer-ID paste field
        TextField {
            id: inviteField
            Layout.preferredWidth: 220
            Layout.preferredHeight: Theme.controlHeight
            Layout.alignment: Qt.AlignVCenter
            implicitHeight: Theme.controlHeight
            placeholderText: "Paste invite\u2026"
            color: Theme.text
            placeholderTextColor: Theme.muted
            font.pixelSize: Theme.fontSizeBody
            leftPadding: Theme.spacingSm
            rightPadding: Theme.spacingSm
            background: Rectangle {
                color: Theme.bg3
                radius: Theme.radiusMd
                border.color: inviteField.activeFocus ? Theme.accent : Theme.bg3
                border.width: 1
                Behavior on border.color { ColorAnimation { duration: Theme.animFast } }
            }
            Keys.onReturnPressed: {
                if (text.trim().length > 0) {
                    backend.pasteInvite(text.trim())
                    text = ""
                }
            }
        }

        // Connect button (→)
        Rectangle {
            id: connectBtn
            signal clicked()
            property bool hovered: connectMouse.containsMouse
            property bool down: connectMouse.pressed
            enabled: inviteField.text.trim().length > 0
            implicitWidth: 36
            implicitHeight: inviteField.height > 0 ? inviteField.height : Theme.controlHeight
            Layout.preferredWidth: 36
            Layout.preferredHeight: inviteField.height > 0 ? inviteField.height : Theme.controlHeight
            Layout.minimumHeight: inviteField.height > 0 ? inviteField.height : Theme.controlHeight
            Layout.maximumHeight: inviteField.height > 0 ? inviteField.height : Theme.controlHeight
            Layout.alignment: Qt.AlignVCenter

            radius: Theme.radiusMd
            color: connectBtn.down
                ? Theme.selectedFill()
                : connectBtn.hovered
                    ? Theme.bg3
                    : Theme.bg2
            border.color: connectBtn.enabled ? Theme.divider : Theme.bg3
            border.width: 1

            Image {
                anchors.centerIn: parent
                source: "qrc:/qt/qml/ConquerD/Client/icons/invite-submit.svg"
                sourceSize.width: 32
                sourceSize.height: 32
                width: 32
                height: 32
                fillMode: Image.PreserveAspectFit
                opacity: connectBtn.enabled ? 1.0 : 0.45
            }
            ToolTip.text: "Connect to peer / accept invite"
            ToolTip.visible: connectBtn.hovered
            onClicked: {
                var u = inviteField.text.trim()
                if (u.length > 0) {
                    backend.pasteInvite(u)
                    inviteField.text = ""
                }
            }

            MouseArea {
                id: connectMouse
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: connectBtn.enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                onClicked: if (connectBtn.enabled) connectBtn.clicked()
            }
        }

        // New Invite button
        Rectangle {
            id: newInviteBtn
            signal clicked()
            property string text: "Invite"
            property bool hovered: newInviteMouse.containsMouse
            property bool down: newInviteMouse.pressed
            implicitHeight: inviteField.height > 0 ? inviteField.height : Theme.controlHeight
            implicitWidth: 82
            Layout.preferredWidth: 82
            Layout.preferredHeight: inviteField.height > 0 ? inviteField.height : Theme.controlHeight
            Layout.minimumHeight: inviteField.height > 0 ? inviteField.height : Theme.controlHeight
            Layout.maximumHeight: inviteField.height > 0 ? inviteField.height : Theme.controlHeight
            Layout.alignment: Qt.AlignVCenter

            radius: Theme.radiusMd
            color: newInviteBtn.down
                ? Qt.darker(Theme.accent, 1.15)
                : newInviteBtn.hovered
                    ? Qt.lighter(Theme.accent, 1.08)
                    : Theme.accent

            Row {
                anchors.centerIn: parent
                spacing: Theme.spacingXs

                Image {
                    source: "qrc:/qt/qml/ConquerD/Client/icons/invite.svg"
                    sourceSize.width: 30
                    sourceSize.height: 30
                    width: 30
                    height: 30
                    anchors.verticalCenter: parent.verticalCenter
                    fillMode: Image.PreserveAspectFit
                }

                Text {
                    text: newInviteBtn.text
                    color: Theme.textInv
                    font.pixelSize: Theme.fontSizeBody
                    font.weight: Font.DemiBold
                    anchors.verticalCenter: parent.verticalCenter
                    verticalAlignment: Text.AlignVCenter
                }
            }
            ToolTip.text: "Copy new invite link to clipboard (Ctrl+N)"
            ToolTip.visible: newInviteBtn.hovered
            onClicked: {
                backend.copyInvite()
                invitePopup.visible = true
            }
            MouseArea {
                id: newInviteMouse
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: newInviteBtn.clicked()
            }
            Shortcut {
                sequence: "Ctrl+N"
                onActivated: newInviteBtn.clicked()
            }
        }

        Item { Layout.fillWidth: true }

        // Own avatar — tooltip shows peer ID; click opens Avatar settings tab
        Avatar {
            Layout.alignment: Qt.AlignVCenter
            Layout.rightMargin: 4
            size: 28
            showRing: true
            peerId: backend.public_id
            configJson: settingsModel.avatar_config_json
            ToolTip.text: "Your peer ID: " + backend.public_id
            ToolTip.visible: ownAvatarHover.hovered
            ToolTip.delay: 500
            HoverHandler { id: ownAvatarHover }
            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                // 2 = Identity, where the avatar editor lives. Shifted from 1
                // when the Video section was inserted after Audio.
                onClicked: { navIndex = 2; settingsTab = settingsPage.tabIdentity }
            }
        }
    }

    // Active navigation index: 0=chat, 1=room, 2=settings
    property int navIndex: 0

    // Name of the room we are currently in voice for (set only by joinRoomWithVoice).
    // Kept separate from roomPanel.roomName so browsing chat rooms doesn't
    // change the voice-rail label.
    property string voiceRoomName: ""
    // Hosting supernode for the active voice room (paired with voiceRoomName).
    property string voiceSupernodeId: ""
    // Room id of the active voice room. Names are display strings and can
    // collide across nodes, so anything deciding "is voice active in *this*
    // room" must compare the id + supernode, not the name.
    property string voiceRoomId: ""
    // Remote peer for an active direct P2P voice call.
    property string activeCallPeerId: ""

    // Settings section index (0=Audio … 7=Diagnostics). Drives SettingsPage.currentTab.
    property int settingsTab: 0

    // ── Video expand / popout state ──────────────────────────────────────────

    /// Peers shown in the shared region above chat, in insertion order.
    property var expandedVideoPeers: []
    /// Peer ids currently sending video, as a set (`{peerId: true}`).
    ///
    /// Held here rather than read back off the participant model, because
    /// neither model can answer the question outside a delegate: `RoomModel` is
    /// a QAbstractListModel whose roles only reach delegates, and
    /// `directCallModel` is a plain ListModel with no videoActive field at all.
    /// Anything iterating peer *ids* (the video region does) therefore saw
    /// "not streaming" for everyone and rendered the camera-off placeholder
    /// permanently. A plain map is model-independent and, reassigned whole,
    /// re-triggers the bindings that depend on it.
    property var videoActivePeers: ({})

    /// Record a peer's camera state. Rebuilds the map so QML sees a new
    /// identity and re-evaluates; mutating in place would not notify.
    function setPeerVideoActive(peerId, active) {
        if (!peerId || peerId === "")
            return
        var next = {}
        for (var k in root.videoActivePeers)
            next[k] = true
        if (active)
            next[peerId] = true
        else
            delete next[peerId]
        root.videoActivePeers = next
    }

    /// Peers whose video stopped arriving while their camera is still on.
    /// Same shape and same reassign-whole rule as `videoActivePeers`.
    property var videoStalledPeers: ({})

    function setPeerVideoStalled(peerId, stalled) {
        if (!peerId || peerId === "")
            return
        var next = {}
        for (var k in root.videoStalledPeers)
            next[k] = true
        if (stalled)
            next[peerId] = true
        else
            delete next[peerId]
        root.videoStalledPeers = next
    }
    /// Fraction of the content area the region occupies (persisted).
    property real videoRegionRatio: 0.4
    /// Live popout windows keyed by peer id.
    property var videoPopouts: ({})

    /// Peers whose video is on screen right now — expanded tiles plus popouts.
    ///
    /// Two things are gated on this, for the same underlying reason:
    ///
    /// * **Shared application audio**, because that audio is one half of a
    ///   picture — playing it to someone who never opened the tile gives them a
    ///   noise with no context and no visible control to stop it.
    /// * **Which senders the supernode forwards video from**, because a tile
    ///   nobody opened is a stream nobody decodes, and in a room of 1080p
    ///   senders that is most of a member's downlink spent on nothing.
    ///
    /// Derived rather than maintained by hand. Both inputs are reassigned
    /// wholesale on every change, so one binding cannot miss an update the way
    /// the five call sites that mutate them could — and a missed *removal* is
    /// the failure that matters, since it leaves a closed tile audible and its
    /// stream still arriving.
    readonly property var watchedVideoPeers: {
        var out = root.expandedVideoPeers.slice()
        for (var pid in root.videoPopouts) {
            if (root.videoPopouts[pid] && out.indexOf(pid) === -1)
                out.push(pid)
        }
        return out
    }

    onWatchedVideoPeersChanged: root.publishWatchedVideoPeers()

    /// Push the watched set to the backend: what to play, and what to receive.
    ///
    /// Guarded because this runs during component completion, which can precede
    /// the backend object being constructed.
    ///
    /// Also called on completion with an empty set, which is deliberate and not
    /// a no-op: to the supernode "no subscription yet" means *forward
    /// everything*, so a member who opens no tiles only stops paying for the
    /// room's video once they have actually said so.
    function publishWatchedVideoPeers() {
        if (typeof backend === "undefined" || !backend)
            return
        var json = JSON.stringify(root.watchedVideoPeers)
        backend.setContentAudioViewers(json)
        backend.setVideoSubscriptions(json)
    }


    function isVideoExpanded(peerId) {
        return root.expandedVideoPeers.indexOf(peerId) !== -1
    }

    /// Toggle a peer in the shared region.
    ///
    /// Reassigns the array rather than mutating it: QML only re-evaluates
    /// bindings on assignment, so an in-place push would leave the grid stale.
    function toggleVideoExpanded(peerId) {
        if (!peerId)
            return
        var next = root.expandedVideoPeers.slice()
        var i = next.indexOf(peerId)
        if (i === -1)
            next.push(peerId)
        else
            next.splice(i, 1)
        root.expandedVideoPeers = next
    }

    function collapseVideo(peerId) {
        var next = root.expandedVideoPeers.slice()
        var i = next.indexOf(peerId)
        if (i !== -1) {
            next.splice(i, 1)
            root.expandedVideoPeers = next
        }
    }

    /// Move a peer from the shared region into its own window.
    function popoutVideo(peerId) {
        if (!peerId)
            return
        // Already popped out — raise the existing window instead of opening a
        // second one showing the same stream.
        if (root.videoPopouts[peerId]) {
            root.videoPopouts[peerId].raise()
            root.videoPopouts[peerId].requestActivate()
            return
        }
        var comp = Qt.createComponent(
            "qrc:/qt/qml/ConquerD/Client/qml/VideoPopoutWindow.qml")
        if (comp.status === Component.Error) {
            console.warn("[video] popout unavailable:", comp.errorString())
            return
        }
        var win = comp.createObject(null, {
            peerId: peerId,
            displayName: root.videoPeerName(peerId)
        })
        if (!win) {
            console.warn("[video] popout window could not be created")
            return
        }
        // Bound after creation, not passed in: createObject's property map sets
        // static values, and this one has to keep tracking the peer's camera.
        win.streaming = Qt.binding(() => root.videoActivePeers[peerId] === true)
        win.stalled = Qt.binding(() => root.videoStalledPeers[peerId] === true)
        var map = root.videoPopouts
        map[peerId] = win
        root.videoPopouts = map
        win.closed.connect(function() { root.forgetVideoPopout(peerId) })
        win.contentAudioChanged.connect(function(pid, muted, volume) {
            backend.setContentAudioPref(pid, muted, volume)
        })
        // Leaving it in the region too would decode the same stream into two
        // sinks for no benefit; popping out is a move, not a copy.
        root.collapseVideo(peerId)
    }

    function forgetVideoPopout(peerId) {
        var map = root.videoPopouts
        if (map[peerId]) {
            delete map[peerId]
            root.videoPopouts = map
        }
    }

    /// Close every popout. Called before quitting so no detached window
    /// outlives the main one and keeps the process alive.
    function closeAllVideoPopouts() {
        for (var pid in root.videoPopouts) {
            if (root.videoPopouts[pid])
                root.videoPopouts[pid].close()
        }
        root.videoPopouts = ({})
    }

    /// Resolve a peer's display name from the active voice roster.
    function videoPeerName(peerId) {
        var model = backend.voice_active && backend.in_room ? roomModel : directCallModel
        if (model && model.rowCount) {
            for (var i = 0; i < model.rowCount(); i++) {
                var row = model.get ? model.get(i) : null
                if (row && row.peerId === peerId)
                    return row.handle || peerId
            }
        }
        return peerId
    }

    /// Capture sources the share menu offers, as `[{ id, name }]`.
    ///
    /// The empty-id entry first is the "default camera" selection an empty
    /// `video_input_device` means — the same convention `SourceSpec` reads.
    property var shareCaptureSources: [{ id: "", name: qsTr("Default camera") }]

    /// Whether this build can encode video at all.
    ///
    /// A static platform fact (VP8 is vendored everywhere, H.264 is added on
    /// Windows), so it is resolved once. If it is ever false there is no point
    /// offering to share — and previously the attempt just failed in silence.
    property bool videoEncoderAvailable: true

    /// Real capture sources, excluding the synthetic "Default camera" entry
    /// that `shareCaptureSources` always leads with.
    readonly property int realCaptureSourceCount: root.shareCaptureSources.length - 1

    /// Why video cannot be shared right now, or "" when it can.
    ///
    /// Capture is re-enumerated every time the share menu opens, so a camera
    /// plugged in after launch is picked up without restarting.
    readonly property string videoUnavailableReason:
        !root.videoEncoderAvailable
            ? qsTr("Video unavailable on this platform — no encoder.")
            : (root.realCaptureSourceCount <= 0
                ? qsTr("No camera or screen detected.")
                : "")

    function refreshVideoEncoderAvailable() {
        try {
            var res = JSON.parse(backend.listVideoCodecs())
            root.videoEncoderAvailable = (res.codecs || []).length > 0
        } catch (e) {
            // Assume available: a failed probe must not lock the user out of a
            // feature that may well work.
            console.warn("could not list video codecs:", e)
            root.videoEncoderAvailable = true
        }
    }

    /// Re-enumerate capture sources for the share menu.
    ///
    /// Cameras, monitors and windows share one list because any of them can be
    /// either the main source or an inset over it.
    function refreshShareCaptureSources() {
        var out = [{ id: "", name: qsTr("Default camera") }]
        try {
            var res = JSON.parse(backend.listVideoDevices())
            var groups = [
                { items: res.cameras || [], prefix: "Camera — " },
                { items: res.screens || [], prefix: "" },
                { items: res.windows || [], prefix: "Window — " }
            ]
            for (var g = 0; g < groups.length; g++) {
                var items = groups[g].items
                for (var i = 0; i < items.length; i++) {
                    out.push({
                        id: items[i].id,
                        name: groups[g].prefix + (items[i].name || items[i].id)
                    })
                }
            }
        } catch (e) {
            // Leaves the default-camera entry standing: enumeration failing is
            // no reason to be unable to share at all.
            console.warn("share menu: could not list video devices:", e)
        }
        root.shareCaptureSources = out
    }

    // Debounced persist of the region ratio, mirroring the window-geometry
    // timer below: a drag emits a value per frame and each save rewrites the
    // settings file.
    Timer {
        id: videoRegionRatioSaveTimer
        interval: 600
        onTriggered: {
            if (settingsModel) {
                settingsModel.video_region_ratio = root.videoRegionRatio
                settingsModel.save()
            }
        }
    }

    // Auto-switch to room tab when voice join succeeds. Leaving voice must not
    // kick the user off the room text panel when a text room is still selected.
    Connections {
        target: backend
        function onIn_roomChanged() {
            if (backend.in_room) navIndex = 1
            else if (navIndex === 1 && !roomPanel.roomId) navIndex = 0
        }
    }

    // Rust backend singleton (injected by cxx-qt)
    AppBridge {
        id: backend
    }

    // Settings model
    SettingsModel {
        id: settingsModel
    }

    Connections {
        target: settingsModel
        function onThemeChanged() {
            applyThemePreference(settingsModel.theme)
        }
        // Keep the bridge's applied avatar config in lockstep with settings so
        // every self-avatar site (voice rail, own room messages, …) resolves to
        // the same config as the Settings preview — including after a profile
        // reload or reset, not just after an in-page avatar edit.
        function onAvatar_config_jsonChanged() {
            backend.setAvatarConfigJson(settingsModel.avatar_config_json)
        }
    }

    Connections {
        target: Qt.styleHints
        function onColorSchemeChanged() {
            if (settingsModel.theme === "system")
                applyThemePreference("system")
        }
    }

    // Live-data models
    PeerListModel     { id: peerModel }
    ChatModel         { id: chatModel }
    // Voice-rail participants (active voice room only).
    RoomModel         { id: roomModel }
    // Text-room members panel (selected room's chat recipients).
    RoomModel         { id: textRoomModel }
    FileTransferModel { id: fileTransferModel }

    // Sidebar: supernodes with grouped SFU rooms (nodesUpdated + sfuRoomsUpdated)
    ListModel { id: nodeListModel }

    Component.onCompleted: {
        settingsModel.load()
        applyThemePreference(settingsModel.theme)
        root.refreshVideoEncoderAvailable()

        // Announce the (empty) watched set. Not a no-op: the supernode treats
        // "never subscribed" as "forward everything", so this is what starts
        // the saving for a member who opens no tiles.
        root.publishWatchedVideoPeers()

        // ── Push saved avatar config into the bridge so avatarSvg() uses it ─
        backend.setAvatarConfigJson(settingsModel.avatar_config_json)

        // Restore the video region split, clamped to the same bounds the drag
        // handler enforces so a hand-edited settings file cannot wedge the
        // region open or shut.
        if (settingsModel.video_region_ratio > 0)
            root.videoRegionRatio = Math.max(0.2, Math.min(0.7, settingsModel.video_region_ratio))

        // Replay listener-local per-peer mute/volume into the mixer, so
        // choices made in an earlier session apply to peers we have not
        // interacted with yet this run.
        backend.applyPeerAudioPrefs(settingsModel.peer_audio_prefs_json)

        // Restore the last normal geometry. Ignore a position that no longer
        // overlaps a connected screen so monitor changes cannot strand us.
        root._restoringGeometry = true
        var savedWidth = settingsModel.window_width > 0
            ? Math.max(root.minimumWidth, settingsModel.window_width)
            : root.width
        var savedHeight = settingsModel.window_height > 0
            ? Math.max(root.minimumHeight, settingsModel.window_height)
            : root.height
        var restoreScreen = null
        if (settingsModel.window_position_saved) {
            for (var screenIndex = 0; screenIndex < Qt.application.screens.length; screenIndex++) {
                var candidate = Qt.application.screens[screenIndex]
                var overlapsX = settingsModel.window_x + savedWidth > candidate.virtualX
                    && settingsModel.window_x < candidate.virtualX + candidate.width
                var overlapsY = settingsModel.window_y + savedHeight > candidate.virtualY
                    && settingsModel.window_y < candidate.virtualY + candidate.height
                if (overlapsX && overlapsY) {
                    restoreScreen = candidate
                    break
                }
            }
        }
        if (restoreScreen) {
            root.width = Math.min(savedWidth, restoreScreen.width)
            root.height = Math.min(savedHeight, restoreScreen.height)
            root.x = Math.max(restoreScreen.virtualX,
                Math.min(settingsModel.window_x,
                    restoreScreen.virtualX + restoreScreen.width - root.width))
            root.y = Math.max(restoreScreen.virtualY,
                Math.min(settingsModel.window_y,
                    restoreScreen.virtualY + restoreScreen.height - root.height))
        } else {
            root.width = savedWidth
            root.height = savedHeight
        }
        root._windowWasMaximized = settingsModel.window_maximized
        root._restoringGeometry = false

        // ── Present the main window ───────────────────────────────────────
        // start_minimized only when the tray icon is available; otherwise
        // users would see a process with no visible UI.
        if (settingsModel.start_minimized && trayIcon.available) {
            root.showMinimized()
        } else {
            root.visible = true
            if (settingsModel.window_maximized)
                root.showMaximized()
            else
                root.show()
            root.raise()
            root.requestActivate()
        }

        backend.peersUpdated.connect(peerModel.setPeers)
        backend.chatMessageReceived.connect(function(msgJson) {
            try {
                var msg = JSON.parse(msgJson)
                if (!msg.peer_id || msg.peer_id === chatPanel.selectedPeerId)
                    chatModel.appendMessage(msgJson)
            } catch (e) {
                chatModel.appendMessage(msgJson)
            }
        })
        backend.chatHistoryLoaded.connect(function(json) {
            chatModel.setMessages(json)
            chatPanel._historyPage = 0
            chatPanel._hasMoreHistory = true
            chatPanel._loadingHistory = false
        })
        backend.chatHistoryPrepended.connect(chatPanel.onHistoryPrepended)
        backend.messageStatusChanged.connect(chatModel.updateMessageStatus)
        backend.participantsUpdated.connect(roomModel.setParticipants)
        backend.textMembersUpdated.connect(textRoomModel.setParticipants)
        backend.localSpeakingChanged.connect(function(speaking) {
            roomModel.updateParticipant(backend.public_id, speaking, false)
        })
        backend.peerSpeakingChanged.connect(function(peerId, speaking) {
            roomModel.updateParticipant(peerId, speaking, false)
        })
        backend.peerLevelChanged.connect(function(peerId, level) {
            roomModel.setAudioLevel(peerId, level)
        })
        backend.peerVideoStateChanged.connect(function(peerId, active) {
            roomModel.setVideoActive(peerId, active)
            // Drives the video region, which cannot read model roles.
            root.setPeerVideoActive(peerId, active)
            // A camera that just turned on or off cannot also be stalled; the
            // backend retracts its own report, but not before the tile would
            // have flashed the badge over a fresh stream.
            if (!active)
                root.setPeerVideoStalled(peerId, false)
        })
        backend.peerVideoStalledChanged.connect(function(peerId, stalled) {
            root.setPeerVideoStalled(peerId, stalled)
        })
        backend.cameraCaptureFailed.connect(function(reason) {
            // The toggle is already off by the time this arrives — the backend
            // turned it off, because the capture is gone either way.
            console.warn("[video] camera stopped:", reason)
        })
        // File transfer model wiring
        backend.fileOffered.connect(fileTransferModel.upsertTransfer)
        backend.fileProgress.connect(fileTransferModel.setProgress)
        backend.fileComplete.connect(function(json) {
            try {
                var o = JSON.parse(json)
                fileTransferModel.markComplete(o.transfer_id)
                var mid = "xfer-" + o.transfer_id
                if (o.saved_path)
                    chatModel.updateAttachment(mid, o.saved_path, o.size_str || "")
                if (o.saved_path)
                    roomPanel.updateAttachment(mid, o.saved_path, o.size_str || "")
            } catch(e) {}
        })
        backend.fileFailed.connect(function(json) {
            try {
                var o = JSON.parse(json)
                fileTransferModel.markFailed(o.transfer_id, o.reason || "failed")
            } catch(e) {}
        })
        // Wire peer list badge + preview + typing from bridge signals
        backend.unreadChanged.connect(peerModel.setPeerUnread)
        backend.previewChanged.connect(peerModel.setPeerPreview)
        backend.typingChanged.connect(function(peerId, isTyping) {
            peerModel.setTyping(peerId, isTyping)
        })
        backend.passphraseRequired.connect(function(isNew) {
            passphraseDialog.isNew = isNew
            passphraseDialog.errorText = backend.session_banner
            passphraseDialog.visible = true
        })
        backend.incomingCall.connect(function(peerId) {
            incomingCallDialog.show(peerId)
        })
        backend.updateAvailable.connect(function(tag, url) {
            updateBanner.tag = tag
            updateBanner.url = url
            updateBanner.visible = true
        })

        // Merge room list updates per supernode into grouped sidebar model.
        backend.sfuRoomsUpdated.connect(function(json) {
            try {
                var obj = JSON.parse(json)
                root.upsertSfuRoomGroup(obj.supernode_id || "", obj.rooms || [], obj.replace === true)
            } catch(e) { console.warn("sfuRoomsUpdated parse error:", e) }
        })

        backend.roomsSidebarSync.connect(root.syncRoomsSidebar)

        backend.connectionStats.connect(function(json) {
            try {
                var stats = JSON.parse(json)
                if (!stats.peer_id) return
                if (!backend.isKnownSupernode(stats.peer_id)) return
                root.upsertNodeConnectionStats(stats.peer_id, stats)
            } catch (e) {}
        })

        // Wire node connect/disconnect into nodeListModel (upsert by node_id)
        backend.nodesUpdated.connect(function(json) {
            try {
                var patches = JSON.parse(json)
                for (var i = 0; i < patches.length; i++) {
                    var p = patches[i]
                    var canon = root.canonicalNodeId(p.node_id || "")
                    if (canon === "") continue
                    var nodeIdx = root.findNodeIndex(canon)
                    if (nodeIdx >= 0) {
                        nodeListModel.setProperty(nodeIdx, "node_id", canon)
                        if (p.connected !== undefined && p.connected !== null) {
                            nodeListModel.setProperty(nodeIdx, "connected", p.connected)
                            if (!p.connected)
                                root.clearNodeConnectionStats(canon)
                        }
                        if (p.homepage_url !== undefined)
                            nodeListModel.setProperty(nodeIdx, "homepage_url", p.homepage_url)
                        if (p.title !== undefined)
                            nodeListModel.setProperty(nodeIdx, "title", p.title)
                        if (p.sfu_enabled !== undefined && p.sfu_enabled !== null)
                            nodeListModel.setProperty(nodeIdx, "sfu_enabled", p.sfu_enabled)
                        if (p.public_rooms_enabled !== undefined && p.public_rooms_enabled !== null)
                            nodeListModel.setProperty(nodeIdx, "public_rooms_enabled", p.public_rooms_enabled)
                    } else {
                        nodeListModel.append({
                            node_id:              canon,
                            connected:            p.connected || false,
                            homepage_url:         p.homepage_url || "",
                            title:                p.title || "",
                            sfu_enabled:          p.sfu_enabled || false,
                            public_rooms_enabled: p.public_rooms_enabled || false,
                            rooms_json:           "[]"
                        })
                    }
                }
                root.dedupeNodeList()
                root.pruneNonSupernodeEntries()
            } catch(e) { console.warn("nodesUpdated parse error:", e) }
        })

        // PTT: start polling thread if enabled in settings
        if (settingsModel.push_to_talk) {
            backend.enablePtt(settingsModel.ptt_key)
        }

        // Wire message deletion: remove from in-memory model when backend
        // confirms. Both panels are told — a message id is unique across the
        // store, so whichever holds it drops it and the other no-ops.
        backend.messageDeleted.connect(function(msgId) {
            chatModel.removeMessage(msgId)
            roomPanel.removeMessage(msgId)
        })
        // Wire peer history clear: wipe the in-memory model
        backend.peerHistoryCleared.connect(function(peerId) {
            if (chatPanel.selectedPeerId === peerId) {
                chatModel.clearMessages()
            }
        })

        backend.initializeBackend()
        if (!settingsModel.onboarding_complete)
            Qt.callLater(function() {
                if (backend.public_id && backend.public_id !== "")
                    onboardingWizard.open()
            })
        // Drop any stale non-supernode rows left from older builds.
        Qt.callLater(root.pruneNonSupernodeEntries)
    }

    // ── Passphrase dialog — shown when identity needs unlocking/creation ──
    OnboardingWizard {
        id: onboardingWizard
        anchors.centerIn: parent
        z: 120
        settingsModel: settingsModel
        appBackend: backend
    }

    PassphraseDialog {
        id: passphraseDialog
        onSubmitted: function(passphrase, filePath) {
            passphraseDialog.visible = false
            backend.unlockWithPassphraseAndFile(passphrase, filePath)
        }
    }

    // Listen for "Incorrect passphrase" banner to re-show dialog with error
    Connections {
        target: backend
        function onPublic_idChanged() {
            if (!settingsModel.onboarding_complete
                    && backend.public_id && backend.public_id !== ""
                    && !onboardingWizard.opened)
                onboardingWizard.open()
        }
        function onSession_bannerChanged() {
            const txt = backend.session_banner
            if (txt === "Incorrect passphrase \u2014 try again.") {
                passphraseDialog.errorText = txt
                passphraseDialog.visible = true
            }
        }
        function onTypingChanged(peerId, isTyping) {
            chatPanel.typingPeerId = isTyping ? peerId : ""
            chatPanel.peerIsTyping = isTyping
        }
        function onRoomChatReceived(msgJson) {
            // Bridge already filters to the selected text room; RoomPanel also
            // checks room_id so history loads / edge races cannot cross-paint.
            roomPanel.appendRoomChat(msgJson)
        }
        // Show a tray balloon when a message arrives while the window is not active.
        function onChatMessageReceived(msgJson) {
            if (!root.active && trayIcon.available) {
                try {
                    var msg = JSON.parse(msgJson)
                    if (!msg.mine) {
                        var sender = msg.sender || qsTr("ConquerD")
                        var body = (msg.body || qsTr("New message")).substring(0, 80)
                        trayIcon.showMessage(sender, body,
                                             Platform.SystemTrayIcon.Information,
                                             4000)
                    }
                } catch(e) {}
            }
        }
        // Show a tray balloon when a missed call is recorded.
        function onMissed_callsChanged() {
            if (backend.missed_calls > 0 && trayIcon.available) {
                trayIcon.showMessage(qsTr("ConquerD"),
                                     qsTr("Missed call"),
                                     Platform.SystemTrayIcon.Warning,
                                     5000)
            }
        }
        // Auto-update nodes list with portal info when supernode responds.
        // (The nodesUpdated signal already patches the ListModel; this handler
        //  is a no-op placeholder kept for future expansion.)
        function onSupernodeInfoReceived(nodeId, url, title) {
            // nodesUpdated already patched homepage_url + title in the model.
        }
        // When relay access requires a portal visit: open the browser,
        // switch to the portal view, and log an event.
        function onRelayPortalRequired(supernodeId, portalUrl) {
            if (portalUrl.startsWith("https://") || portalUrl.startsWith("http://")) {
                Qt.openUrlExternally(portalUrl)
                backend.logEvent("[relay] Portal visit required — opened browser: " + portalUrl)
            }
        }
        // Open a supernode's in-app portal: navigate the embedded browser
        // to the conquerd:// URL served over the QUIC relay connection.
        function onNavigateNodePortal(supernodeId, url) {
            console.log("[portal] onNavigateNodePortal sn=" + supernodeId + " url=" + url)
            browserPanel.portalActive = true   // ensure Loader fires
            browserPanel.nodeMode = true
            browserPanel.navigateTo(url)
            navIndex = 3
        }
        function onSupernodeRemoved(nodeId) {
            if (!nodeId || nodeId === "") return
            root.clearNodeConnectionStats(nodeId)
            // Peer store is already updated when this fires; canonicalNodeId()
            // would return "" because isKnownSupernode() is false.
            for (var j = nodeListModel.count - 1; j >= 0; j--) {
                if (nodeListModel.get(j).node_id === nodeId)
                    nodeListModel.remove(j)
            }
            root.pruneNonSupernodeEntries()
            if (roomPanel.supernodeId === nodeId)
                roomPanel.switchToRoom("", "", "")
        }
        function onRoomRemoved(supernodeId, roomId) {
            if (roomPanel.supernodeId === supernodeId && roomPanel.roomId === roomId)
                roomPanel.switchToRoom("", "", "")
            var nodeIdx = root.findNodeIndex(supernodeId)
            if (nodeIdx < 0) return
            var rooms = []
            try { rooms = JSON.parse(nodeListModel.get(nodeIdx).rooms_json || "[]") } catch (e) {}
            var filtered = []
            for (var i = 0; i < rooms.length; i++) {
                if (rooms[i].room_id !== roomId)
                    filtered.push(rooms[i])
            }
            nodeListModel.setProperty(nodeIdx, "rooms_json", JSON.stringify(filtered))
        }
        function onRoomCreated(supernodeId, roomId, roomName, roomType, inviteToken) {
            roomPanel.switchToRoom(roomName, roomId, supernodeId)
            backend.joinRoomWithVoice(supernodeId, roomId)
            root.voiceRoomName = roomName
            root.voiceSupernodeId = supernodeId
            root.voiceRoomId = roomId
            navIndex = 1
            if (roomType === "private" && inviteToken !== "") {
                // Prefer a self-contained invite URL (embeds the supernode
                // address) so recipients on any/no supernode can just paste it;
                // fall back to the bare token if the URL can't be built.
                var inviteUrl = backend.generateRoomInvite(supernodeId, roomId, roomName)
                backend.copyToClipboard(inviteUrl !== "" ? inviteUrl : inviteToken)
                if (trayIcon.available) {
                    trayIcon.showMessage(
                        qsTr("Private room created"),
                        inviteUrl !== ""
                            ? qsTr("Invite link copied to clipboard.")
                            : qsTr("Invite token copied to clipboard."),
                        Platform.SystemTrayIcon.Information,
                        5000)
                }
            }
        }
        function onRoomInviteReady(supernodeId, roomId, roomName) {
            roomPanel.switchToRoom(roomName, roomId, supernodeId)
            backend.joinRoomWithVoice(supernodeId, roomId)
            root.voiceRoomName = roomName
            root.voiceSupernodeId = supernodeId
            root.voiceRoomId = roomId
            navIndex = 1
        }
    }

    // ── Incoming call overlay ─────────────────────────────────────────────
    IncomingCallDialog {
        id: incomingCallDialog
        anchors.centerIn: parent
        z: 100
        onAccepted: function(peerId) {
            root.trackDirectCall(peerId)
            backend.acceptCall(peerId)
        }
        onRejected: function(peerId) { backend.rejectCall(peerId) }
    }

    // ── Join Room dialog ──────────────────────────────────────────────────
    JoinRoomDialog {
        id: joinRoomDialog
        anchors.centerIn: parent
        z: 100
        onJoinRequested: function(supernodeId, roomId, inviteToken) {
            roomPanel.switchToRoom(roomId, roomId, supernodeId)
            // Persist + validate the invite token before joinRoomWithVoice runs
            // join_room (which reads the token from the room store).
            if ((inviteToken || "").trim() !== "")
                backend.joinRoomWithInvite(supernodeId, roomId, inviteToken)
            backend.joinRoomWithVoice(supernodeId, roomId)
            root.voiceRoomName = roomId
            root.voiceSupernodeId = supernodeId
            root.voiceRoomId = roomId
            navIndex = 1
        }
    }

    CreateRoomDialog {
        id: createRoomDialog
        anchors.centerIn: parent
        z: 100
        nodeListModel: nodeListModel
    }

    // ── Update available banner ───────────────────────────────────────────
    Rectangle {
        id: updateBanner
        visible: false
        property string tag: ""
        property string url: ""
        z: 90
        anchors { top: customTitleBar.bottom; left: parent.left; right: parent.right }
        height: 36
        color: Theme.accent

        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 12
            anchors.rightMargin: 8
            spacing: 8

            Label {
                Layout.fillWidth: true
                text: "Update " + updateBanner.tag + " is available!"
                color: Theme.textInv
                font.pixelSize: Theme.fontSizeBody
            }
            Button {
                text: "Install"
                flat: true
                Material.foreground: Theme.textInv
                onClicked: {
                    backend.applyUpdate()
                    updateBanner.visible = false
                }
            }
            Button {
                text: "\u00D7"
                flat: true
                Material.foreground: Theme.textInv
                onClicked: updateBanner.visible = false
            }
        }
    }

    // ── Topbar removed: logo + invite field now live inside the TitleBar ─

    // ── Invite URL popup — shown after "New Invite" ───────────────────────
    Popup {
        id: invitePopup
        x: parent.width / 2 - width / 2
        y: customTitleBar.height + 8
        width: 460
        height: 100
        modal: false
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
        z: 200

        background: Rectangle {
            color: Theme.bg2
            radius: 0
            border.color: Theme.border
            border.width: 1
        }

        ColumnLayout {
            anchors.fill: parent
            anchors.margins: 12
            spacing: 8

            Label {
                text: "Invite link copied to clipboard:"
                color: Theme.text
                font.pixelSize: Theme.fontSizeCaption
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingXs

                StyledTextField {
                    Layout.fillWidth: true
                    text: backend.invite_url
                    readOnly: true
                }

                Button {
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/clipboard.svg"
                    icon.width: 30
                    icon.height: 30
                    icon.color: Theme.text
                    implicitHeight: 30
                    implicitWidth: 30
                    flat: true
                    Material.foreground: Theme.text
                    ToolTip.text: "Copy to clipboard"
                    ToolTip.visible: hovered
                    onClicked: {
                        // Copy exactly the link shown (peer or room invite),
                        // rather than minting a fresh peer invite.
                        backend.copyToClipboard(backend.invite_url)
                        invitePopup.visible = false
                    }
                }
            }
        }
    }

    // ── Synthetic participant model for direct P2P calls ─────────────────
    // Populated from bridge when call_state changes; cleared on idle.
    ListModel { id: directCallModel }

    Connections {
        target: backend
        function onCall_stateChanged() {
            var cs = backend.call_state
            if (cs === "connecting" || cs === "in_call") {
                root.refreshDirectCallModel()
            } else {
                root.activeCallPeerId = ""
                directCallModel.clear()
            }
        }
        function onCall_duration_secsChanged() {
            voiceRail.durationSecs = backend.call_duration_secs
        }
        function onConnection_modeChanged() {
            voiceRail.connectionMode = backend.connection_mode
        }
    }

    // ── Main body ─────────────────────────────────────────────────────────
    RowLayout {
        anchors {
            top: customTitleBar.bottom
            left: parent.left
            right: parent.right
            bottom: parent.bottom
        }
        spacing: 0

        // ── Left sidebar: Peers | Rooms tabs ─────────────────────────────────
        ColumnLayout {
            Layout.preferredWidth: Theme.sidebarWidth
            Layout.minimumWidth: Theme.sidebarWidth
            Layout.maximumWidth: Theme.sidebarWidth
            Layout.fillHeight: true
            clip: true
            spacing: 0

            // Tab bar (hidden while settings nav is active)
            TabBar {
                id: sidebarTabBar
                Layout.fillWidth: true
                Layout.preferredHeight: sidebarTabHeight
                implicitHeight: sidebarTabHeight
                visible: navIndex !== 2
                Material.accent: Material.Blue

                readonly property int sidebarTabHeight: Theme.controlHeight + 4

                TabButton {
                    text: "Peers"
                    font.pixelSize: Theme.fontSizeCaption
                    font.bold: sidebarTabBar.currentIndex === 0
                    implicitHeight: sidebarTabBar.sidebarTabHeight
                }
                TabButton {
                    text: "Rooms"
                    font.pixelSize: Theme.fontSizeCaption
                    font.bold: sidebarTabBar.currentIndex === 1
                    implicitHeight: sidebarTabBar.sidebarTabHeight
                }
            }

            // Tab content (hidden while settings nav is active)
            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                visible: navIndex !== 2
                currentIndex: sidebarTabBar.currentIndex

                // ── Tab 0: Peers ──────────────────────────────────────────
                PeerList {
                    id: peerList
                    peerCount: backend.peer_count
                    peerModel: peerModel
                    selectedPeerId: chatPanel.selectedPeerId
                    onPeerSelected: function(peerId, handle) {
                        chatPanel.selectedPeerId = peerId
                        chatPanel.selectedPeerName = handle
                        backend.selectPeer(peerId)
                        navIndex = 0
                        peerModel.setPeerUnread(peerId, 0)
                    }
                    onStartCallRequested: function(peerId) {
                        root.beginDirectCall(peerId)
                    }
                    onRemovePeerRequested: (peerId) => backend.removePeer(peerId)
                    onCopyPeerIdRequested: (peerId) => backend.copyPeerId(peerId)
                    onBlockPeerRequested: (peerId) => backend.blockPeer(peerId)
                    onUnblockPeerRequested: (peerId) => backend.unblockPeer(peerId)
                    onClearHistoryRequested: function(peerId) {
                        backend.clearPeerHistory(peerId)
                    }
                }

                // ── Tab 1: Rooms (supernodes + grouped rooms) ─────────────
                ColumnLayout {
                    spacing: 0

                    Menu {
                        id: roomContextMenu
                        property string targetSupernodeId: ""
                        property string targetRoomId: ""
                        property string targetRoomName: ""
                        property bool targetCanRemove: false

                        MenuItem {
                            text: qsTr("Join Voice Room")
                            onTriggered: {
                                roomPanel.switchToRoom(
                                    roomContextMenu.targetRoomName,
                                    roomContextMenu.targetRoomId,
                                    roomContextMenu.targetSupernodeId)
                                backend.joinRoomWithVoice(
                                    roomContextMenu.targetSupernodeId,
                                    roomContextMenu.targetRoomId)
                                root.voiceRoomName = roomContextMenu.targetRoomName
                                root.voiceSupernodeId = roomContextMenu.targetSupernodeId
                                root.voiceRoomId = roomContextMenu.targetRoomId
                                navIndex = 1
                            }
                        }
                        MenuItem {
                            text: qsTr("Copy Room Invite")
                            onTriggered: {
                                var url = backend.generateRoomInvite(
                                    roomContextMenu.targetSupernodeId,
                                    roomContextMenu.targetRoomId,
                                    roomContextMenu.targetRoomName)
                                if (url !== "") {
                                    backend.copyToClipboard(url)
                                    invitePopup.visible = true
                                } else if (trayIcon.available) {
                                    trayIcon.showMessage(
                                        qsTr("Room invite"),
                                        qsTr("Couldn't build the invite — connect to the room's supernode first."),
                                        Platform.SystemTrayIcon.Warning,
                                        5000)
                                }
                            }
                        }
                        // Per-contact invite: embeds an owner-signed SpaceGrant
                        // bound to the chosen peer, so a private room admits them
                        // durably by proof+grant (survives supernode restarts).
                        Menu {
                            id: inviteContactMenu
                            title: qsTr("Invite Contact to Room")
                            enabled: contactInviteInstantiator.count > 0
                            Instantiator {
                                id: contactInviteInstantiator
                                model: peerModel
                                delegate: MenuItem {
                                    text: (handle && handle !== "") ? handle : peerId
                                    onTriggered: {
                                        var url = backend.generateRoomInviteForPeer(
                                            roomContextMenu.targetSupernodeId,
                                            roomContextMenu.targetRoomId,
                                            roomContextMenu.targetRoomName,
                                            peerId)
                                        if (url !== "") {
                                            backend.copyToClipboard(url)
                                            invitePopup.visible = true
                                        } else if (trayIcon.available) {
                                            trayIcon.showMessage(
                                                qsTr("Room invite"),
                                                qsTr("Couldn't build the invite — you must own this room's Space and be connected to its supernode."),
                                                Platform.SystemTrayIcon.Warning,
                                                5000)
                                        }
                                    }
                                }
                                onObjectAdded: (index, object) => inviteContactMenu.insertItem(index, object)
                                onObjectRemoved: (index, object) => inviteContactMenu.removeItem(object)
                            }
                        }
                        MenuSeparator {}
                        MenuItem {
                            text: qsTr("Create Public Sub-room…")
                            onTriggered: createRoomDialog.openForParent(
                                roomContextMenu.targetSupernodeId,
                                "public",
                                roomContextMenu.targetRoomId,
                                roomContextMenu.targetRoomName)
                        }
                        MenuItem {
                            text: qsTr("Create Private Sub-room…")
                            onTriggered: createRoomDialog.openForParent(
                                roomContextMenu.targetSupernodeId,
                                "private",
                                roomContextMenu.targetRoomId,
                                roomContextMenu.targetRoomName)
                        }
                        MenuSeparator {
                            visible: roomContextMenu.targetCanRemove
                        }
                        MenuItem {
                            text: qsTr("Hide Room")
                            visible: roomContextMenu.targetCanRemove
                            onTriggered: backend.removeRoom(
                                roomContextMenu.targetSupernodeId,
                                roomContextMenu.targetRoomId)
                        }
                    }

                    Menu {
                        id: nodeContextMenu
                        property string targetNodeId: ""
                        property bool targetConnected: false
                        property bool targetSfuEnabled: false
                        property bool targetPublicRoomsEnabled: false

                        MenuItem {
                            text: qsTr("Create Public Room…")
                            enabled: nodeContextMenu.targetConnected && nodeContextMenu.targetSfuEnabled && nodeContextMenu.targetPublicRoomsEnabled
                            onTriggered: createRoomDialog.openForNode(
                                nodeContextMenu.targetNodeId, "public")
                        }
                        MenuItem {
                            text: qsTr("Create Private Room…")
                            enabled: nodeContextMenu.targetConnected && nodeContextMenu.targetSfuEnabled
                            onTriggered: createRoomDialog.openForNode(
                                nodeContextMenu.targetNodeId, "private")
                        }
                        MenuSeparator {}
                        MenuItem {
                            text: qsTr("Open Portal")
                            onTriggered: {
                                console.log("[portal] context menu node_id=" + nodeContextMenu.targetNodeId)
                                backend.openNodePortal(nodeContextMenu.targetNodeId)
                            }
                        }
                        MenuItem {
                            text: qsTr("Copy Node ID")
                            onTriggered: backend.copyToClipboard(nodeContextMenu.targetNodeId)
                        }
                        MenuSeparator {}
                        MenuItem {
                            text: qsTr("Remove Supernode")
                            onTriggered: backend.removeSupernode(nodeContextMenu.targetNodeId)
                        }
                    }

                    ListView {
                        id: roomsListView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: nodeListModel
                        clip: true
                        spacing: Theme.spacingXs

                        EmptyState {
                            anchors.centerIn: parent
                            visible: nodeListModel.count === 0
                            width: Math.min(parent.width - Theme.spacingXl, 170)
                            iconSource: "qrc:/qt/qml/ConquerD/Client/icons/globe.svg"
                            iconSize: 30
                            title: "No supernodes"
                            subtitle: "Connect to a supernode to browse rooms and portals."
                        }

                        delegate: Item {
                            id: roomGroup
                            required property string node_id
                            required property bool connected
                            required property bool sfu_enabled
                            required property bool public_rooms_enabled
                            required property string title
                            required property string rooms_json

                            readonly property var rooms: {
                                try {
                                    return JSON.parse(roomGroup.rooms_json || "[]")
                                } catch (e) {
                                    return []
                                }
                            }

                            // Rooms flattened into Space-tree order (parent →
                            // children, collapsed subtrees omitted), each with
                            // tree_depth / has_children / collapsed / guide_cols.
                            readonly property var roomsTree:
                                root.roomTreeOrder(roomGroup.rooms, roomGroup.node_id,
                                                   root.collapsedRooms)

                            readonly property real groupHeight:
                                Math.max(48, roomGroup.roomsTree.length * 48) + Theme.spacingSm

                            visible: backend.isKnownSupernode(roomGroup.node_id)
                            width: roomsListView.width
                            height: visible ? groupHeight : 0

                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: Theme.spacingMd
                                anchors.rightMargin: Theme.spacingSm
                                anchors.topMargin: Theme.spacingXs
                                anchors.bottomMargin: Theme.spacingXs
                                spacing: Theme.spacingSm

                                Item {
                                    Layout.alignment: Qt.AlignTop
                                    Layout.topMargin: 4
                                    width: 44
                                    height: 44

                                    Rectangle {
                                        anchors.fill: parent
                                        radius: Theme.radiusSm
                                        color: groupSnHover.hovered ? Theme.bg3 : "transparent"
                                        Behavior on color {
                                            ColorAnimation { duration: Theme.animNormal }
                                        }
                                    }

                                    Avatar {
                                        id: groupAvatar
                                        anchors.centerIn: parent
                                        peerId: roomGroup.node_id
                                        size: 36
                                        showRing: true
                                        ringColor: roomGroup.connected
                                            ? Theme.online
                                            : groupAvatar.tintColor
                                    }

                                    HoverHandler { id: groupSnHover }

                                    ToolTip {
                                        visible: groupSnHover.hovered
                                        text: root.supernodeAvatarTooltip(
                                            roomGroup.node_id, roomGroup.connected)
                                        delay: 300
                                        timeout: 5000
                                    }

                                    MouseArea {
                                        anchors.fill: parent
                                        acceptedButtons: Qt.LeftButton | Qt.RightButton
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: (mouse) => {
                                            if (mouse.button === Qt.RightButton) {
                                                nodeContextMenu.targetNodeId = roomGroup.node_id
                                                nodeContextMenu.targetConnected = roomGroup.connected
                                                nodeContextMenu.targetSfuEnabled = roomGroup.sfu_enabled
                                                nodeContextMenu.targetPublicRoomsEnabled = roomGroup.public_rooms_enabled
                                                nodeContextMenu.popup()
                                            } else {
                                                console.log("[portal] node avatar clicked node_id=" + roomGroup.node_id)
                                                backend.openNodePortal(roomGroup.node_id)
                                            }
                                        }
                                    }
                                }

                                Column {
                                    id: roomColumn
                                    Layout.fillWidth: true
                                    spacing: 0

                                    Label {
                                        visible: roomGroup.rooms.length === 0
                                        width: roomColumn.width
                                        height: 48
                                        verticalAlignment: Text.AlignVCenter
                                        text: "No rooms"
                                        color: Theme.muted
                                        font.pixelSize: Theme.fontSizeCaption
                                        leftPadding: Theme.spacingXs
                                    }

                                    Repeater {
                                        // Tree-ordered (parent → children) so the
                                        // list reads top-down as a proper tree.
                                        model: roomGroup.roomsTree

                                        delegate: ItemDelegate {
                                            id: roomDelegate
                                            required property string room_id
                                            required property string name
                                            required property string kind
                                            required property int voice_count
                                            required property int chat_count
                                            required property bool chat_count_known
                                            required property var known_peers
                                            required property int unknown_peers
                                            readonly property var knownPeers:
                                                Array.isArray(roomDelegate.known_peers)
                                                    ? roomDelegate.known_peers
                                                    : []
                                            // Derive Unknown from the same voice_count the badge shows
                                            // (minus the named/known peers) so the tooltip can never
                                            // disagree with the number on the bubble — named peers +
                                            // Unknown always sums to the badge count.
                                            readonly property int unknownPeers:
                                                Math.max(0, roomDelegate.voice_count - roomDelegate.knownPeers.length)
                                            required property string creator_id
                                            required property bool is_default
                                            // Space-tree metadata from roomTreeOrder.
                                            required property int tree_depth
                                            required property bool has_children
                                            required property bool collapsed
                                            required property var guide_cols

                                            // Width of one tree-guide column / indent step.
                                            readonly property int treeStep: Theme.spacingLg
                                            // Space-tree indent: one step (a "tab")
                                            // to the right per nesting level.
                                            readonly property int treeIndent:
                                                roomDelegate.tree_depth * roomDelegate.treeStep

                                            width: roomColumn.width
                                            height: 48

                                            readonly property bool canRemove:
                                                !roomDelegate.is_default
                                                && roomDelegate.room_id !== "default"

                                            readonly property bool roomSelected:
                                                roomPanel.supernodeId !== ""
                                                && roomPanel.supernodeId === roomGroup.node_id
                                                && roomPanel.roomId !== ""
                                                && roomPanel.roomId === roomDelegate.room_id

                                            background: Rectangle {
                                                color: roomDelegate.roomSelected
                                                    ? Theme.selectedFill()
                                                    : (roomDelegate.hovered ? Theme.bg3 : "transparent")
                                                Behavior on color {
                                                    ColorAnimation { duration: Theme.animNormal }
                                                }

                                                Rectangle {
                                                    visible: roomDelegate.roomSelected
                                                    width: 3
                                                    anchors {
                                                        left: parent.left
                                                        top: parent.top
                                                        bottom: parent.bottom
                                                    }
                                                    color: Theme.accent
                                                }
                                            }

                                            onClicked: {
                                                roomPanel.switchToRoom(
                                                    roomDelegate.name || roomDelegate.room_id,
                                                    roomDelegate.room_id,
                                                    roomGroup.node_id)
                                                backend.subscribeRoomChat(
                                                    roomGroup.node_id,
                                                    roomDelegate.room_id)
                                                navIndex = 1
                                            }
                                            onDoubleClicked: {
                                                roomPanel.switchToRoom(
                                                    roomDelegate.name || roomDelegate.room_id,
                                                    roomDelegate.room_id,
                                                    roomGroup.node_id)
                                                backend.joinRoomWithVoice(
                                                    roomGroup.node_id,
                                                    roomDelegate.room_id)
                                                root.voiceRoomName = roomDelegate.name || roomDelegate.room_id
                                                root.voiceSupernodeId = roomGroup.node_id
                                                root.voiceRoomId = roomDelegate.room_id
                                                navIndex = 1
                                            }

                                            MouseArea {
                                                anchors.fill: parent
                                                acceptedButtons: Qt.RightButton
                                                onClicked: (mouse) => {
                                                    if (mouse.button === Qt.RightButton) {
                                                        roomContextMenu.targetSupernodeId = roomGroup.node_id
                                                        roomContextMenu.targetRoomId = roomDelegate.room_id
                                                        roomContextMenu.targetRoomName =
                                                            roomDelegate.name || roomDelegate.room_id
                                                        roomContextMenu.targetCanRemove = roomDelegate.canRemove
                                                        roomContextMenu.popup()
                                                    }
                                                }
                                            }

                                            // Tree connector lines, drawn in the
                                            // indent gutter. One cell per depth
                                            // column; codes from `guide_cols`:
                                            // 1 = │ pass-through, 2 = └ (last),
                                            // 3 = ├ (has following sibling).
                                            Row {
                                                id: treeGuides
                                                anchors.left: parent.left
                                                anchors.top: parent.top
                                                anchors.bottom: parent.bottom
                                                width: roomDelegate.treeIndent
                                                visible: roomDelegate.tree_depth > 0

                                                Repeater {
                                                    model: roomDelegate.guide_cols
                                                    delegate: Item {
                                                        id: guideCell
                                                        property int code: modelData
                                                        width: roomDelegate.treeStep
                                                        height: treeGuides.height

                                                        // Vertical: full for │/├, top-half for └.
                                                        Rectangle {
                                                            width: 1
                                                            color: Theme.divider
                                                            x: Math.floor(guideCell.width / 2)
                                                            y: 0
                                                            height: (guideCell.code === 1 || guideCell.code === 3)
                                                                ? guideCell.height
                                                                : (guideCell.code === 2 ? guideCell.height / 2 : 0)
                                                            visible: guideCell.code !== 0
                                                        }
                                                        // Horizontal elbow into the row for ├ / └.
                                                        Rectangle {
                                                            height: 1
                                                            color: Theme.divider
                                                            x: Math.floor(guideCell.width / 2)
                                                            y: Math.floor(guideCell.height / 2)
                                                            width: (guideCell.code === 2 || guideCell.code === 3)
                                                                ? guideCell.width / 2
                                                                : 0
                                                            visible: guideCell.code === 2 || guideCell.code === 3
                                                        }
                                                    }
                                                }
                                            }

                                            ColumnLayout {
                                                anchors.verticalCenter: parent.verticalCenter
                                                anchors.left: parent.left
                                                anchors.right: parent.right
                                                anchors.leftMargin: roomDelegate.treeIndent
                                                spacing: Theme.spacingXs

                                                RowLayout {
                                                    Layout.fillWidth: true

                                                    // Expand/collapse toggle (only
                                                    // for rooms that have sub-rooms).
                                                    // An SVG caret rotated in place —
                                                    // right when collapsed, down when
                                                    // expanded — so it never depends on
                                                    // the UI font having a triangle char.
                                                    Item {
                                                        Layout.preferredWidth: 14
                                                        Layout.preferredHeight: 14
                                                        Layout.alignment: Qt.AlignVCenter

                                                        Image {
                                                            id: chevron
                                                            anchors.centerIn: parent
                                                            width: 10
                                                            height: 10
                                                            sourceSize.width: 20
                                                            sourceSize.height: 20
                                                            fillMode: Image.PreserveAspectFit
                                                            smooth: true
                                                            visible: roomDelegate.has_children
                                                            source: "qrc:/qt/qml/ConquerD/Client/icons/chevron.svg"
                                                            // collapsed → points right (0°);
                                                            // expanded → points down (90°).
                                                            rotation: roomDelegate.collapsed ? 0 : 90
                                                            Behavior on rotation {
                                                                NumberAnimation { duration: Theme.animNormal }
                                                            }
                                                        }

                                                        MouseArea {
                                                            anchors.fill: parent
                                                            anchors.margins: -3
                                                            enabled: roomDelegate.has_children
                                                            cursorShape: Qt.PointingHandCursor
                                                            onClicked: root.toggleRoomCollapse(
                                                                roomGroup.node_id, roomDelegate.room_id)
                                                        }
                                                    }

                                                    Label {
                                                        Layout.fillWidth: true
                                                        text: roomDelegate.name
                                                        color: Theme.text
                                                        font.pixelSize: Theme.fontSizeBody
                                                        font.bold: roomDelegate.roomSelected
                                                        elide: Text.ElideRight
                                                    }

                                                    Rectangle {
                                                        id: roomVoiceBubble
                                                        // Once the hosting supernode disconnects, `voice_count` is
                                                        // whatever we last heard — there is no live refresh path
                                                        // for a dead session (RequestRoomList would just target a
                                                        // closed connection), so treat the count as unknown rather
                                                        // than rendering a stale number as if it were live.
                                                        readonly property bool countIsStale: !roomGroup.connected
                                                        Layout.alignment: Qt.AlignVCenter
                                                        Layout.preferredWidth: voiceBubbleRow.implicitWidth + 10
                                                        Layout.preferredHeight: 22
                                                        radius: 11
                                                        color: (!roomVoiceBubble.countIsStale && roomDelegate.voice_count > 0)
                                                            ? Theme.semanticTint(Theme.online, 0.16)
                                                            : Theme.bg2
                                                        border.color: (!roomVoiceBubble.countIsStale && roomDelegate.voice_count > 0)
                                                            ? Theme.online
                                                            : Theme.divider
                                                        border.width: 1

                                                        Row {
                                                            id: voiceBubbleRow
                                                            anchors.centerIn: parent
                                                            spacing: 4

                                                            Image {
                                                                source: "qrc:/qt/qml/ConquerD/Client/icons/headphone.svg"
                                                                sourceSize.width: 12
                                                                sourceSize.height: 12
                                                                width: 12
                                                                height: 12
                                                                anchors.verticalCenter: parent.verticalCenter
                                                                fillMode: Image.PreserveAspectFit
                                                                opacity: (!roomVoiceBubble.countIsStale && roomDelegate.voice_count > 0) ? 1.0 : 0.55
                                                            }

                                                            Label {
                                                                text: roomVoiceBubble.countIsStale ? "\u2014" : roomDelegate.voice_count.toString()
                                                                color: (!roomVoiceBubble.countIsStale && roomDelegate.voice_count > 0) ? Theme.online : Theme.muted
                                                                font.pixelSize: Theme.fontSizeCaption
                                                                font.bold: !roomVoiceBubble.countIsStale && roomDelegate.voice_count > 0
                                                                anchors.verticalCenter: parent.verticalCenter
                                                            }
                                                        }

                                                        HoverHandler { id: roomStatsHover }

                                                        Popup {
                                                            id: roomVoicePopup
                                                            parent: roomVoiceBubble
                                                            visible: roomStatsHover.hovered
                                                            // TEMP DIAGNOSTIC — dump what the delegate actually
                                                            // received for this room. Remove after triage.
                                                            onVisibleChanged: if (visible) backend.logEvent(
                                                                "[qml-tooltip] room=" + roomDelegate.room_id
                                                                + " vc=" + roomDelegate.voice_count
                                                                + " kp_typeof=" + (typeof roomDelegate.known_peers)
                                                                + " kp_isArr=" + Array.isArray(roomDelegate.known_peers)
                                                                + " kp=" + JSON.stringify(roomDelegate.known_peers)
                                                                + " up_role=" + roomDelegate.unknown_peers
                                                                + " knownPeersLen=" + roomDelegate.knownPeers.length
                                                                + " unknownPeers=" + roomDelegate.unknownPeers)
                                                            modal: false
                                                            focus: false
                                                            closePolicy: Popup.NoAutoClose
                                                            padding: 10
                                                            width: 220
                                                            x: roomVoiceBubble.width + 6
                                                            y: Math.round((roomVoiceBubble.height - implicitHeight) / 2)

                                                            background: Rectangle {
                                                                color: Theme.bg2
                                                                radius: Theme.radiusMd
                                                                border.color: Theme.divider
                                                                border.width: 1
                                                            }

                                                            contentItem: Column {
                                                                spacing: Theme.spacingXs

                                                                Label {
                                                                    visible: roomVoiceBubble.countIsStale
                                                                    width: roomVoicePopup.availableWidth
                                                                    text: "Supernode offline \u2014 counts may be stale"
                                                                    color: Theme.muted
                                                                    font.italic: true
                                                                    font.pixelSize: Theme.fontSizeCaption
                                                                    wrapMode: Text.WordWrap
                                                                }

                                                                // Live but empty voice room — say so plainly rather
                                                                // than showing a permanent "Known: none / Unknown: 0".
                                                                Label {
                                                                    visible: !roomVoiceBubble.countIsStale
                                                                        && roomDelegate.voice_count === 0
                                                                    width: roomVoicePopup.availableWidth
                                                                    text: "No one in voice"
                                                                    color: Theme.muted
                                                                    font.pixelSize: Theme.fontSizeCaption
                                                                    wrapMode: Text.WordWrap
                                                                }

                                                                // Only render the roster breakdown when we have a live
                                                                // count with people in it — a stale ("—") badge has no
                                                                // trustworthy roster to describe.
                                                                Label {
                                                                    visible: !roomVoiceBubble.countIsStale
                                                                        && roomDelegate.voice_count > 0
                                                                    width: roomVoicePopup.availableWidth
                                                                    text: roomDelegate.knownPeers.length > 0
                                                                        ? "Known: " + roomDelegate.knownPeers.join(", ")
                                                                        : "Known: none"
                                                                    color: Theme.text
                                                                    font.pixelSize: Theme.fontSizeCaption
                                                                    wrapMode: Text.WordWrap
                                                                }

                                                                Label {
                                                                    visible: !roomVoiceBubble.countIsStale
                                                                        && roomDelegate.voice_count > 0
                                                                        && roomDelegate.unknownPeers > 0
                                                                    width: roomVoicePopup.availableWidth
                                                                    text: "Unknown: " + roomDelegate.unknownPeers
                                                                    color: Theme.muted
                                                                    font.pixelSize: Theme.fontSizeCaption
                                                                }
                                                            }
                                                        }
                                                    }

                                                    Rectangle {
                                                        id: roomChatBubble
                                                        // Text-chat occupancy (voice participants + chat-only
                                                        // subscribers) — distinct from roomVoiceBubble, which is
                                                        // voice-only. Stale when the supernode is offline (no live
                                                        // refresh path) OR we've never received a real chat_count
                                                        // yet (only voice-roster patches have landed so far) —
                                                        // show "—" rather than a possibly-wrong 0 in either case.
                                                        readonly property bool countIsStale:
                                                            !roomGroup.connected || !roomDelegate.chat_count_known
                                                        Layout.alignment: Qt.AlignVCenter
                                                        Layout.preferredWidth: chatBubbleRow.implicitWidth + 10
                                                        Layout.preferredHeight: 22
                                                        radius: 11
                                                        color: (!roomChatBubble.countIsStale && roomDelegate.chat_count > 0)
                                                            ? Theme.semanticTint(Theme.accent, 0.16)
                                                            : Theme.bg2
                                                        border.color: (!roomChatBubble.countIsStale && roomDelegate.chat_count > 0)
                                                            ? Theme.accent
                                                            : Theme.divider
                                                        border.width: 1

                                                        Row {
                                                            id: chatBubbleRow
                                                            anchors.centerIn: parent
                                                            spacing: 4

                                                            Image {
                                                                source: "qrc:/qt/qml/ConquerD/Client/icons/speech.svg"
                                                                sourceSize.width: 12
                                                                sourceSize.height: 12
                                                                width: 12
                                                                height: 12
                                                                anchors.verticalCenter: parent.verticalCenter
                                                                fillMode: Image.PreserveAspectFit
                                                                opacity: (!roomChatBubble.countIsStale && roomDelegate.chat_count > 0) ? 1.0 : 0.55
                                                            }

                                                            Label {
                                                                text: roomChatBubble.countIsStale ? "—" : roomDelegate.chat_count.toString()
                                                                color: (!roomChatBubble.countIsStale && roomDelegate.chat_count > 0) ? Theme.accent : Theme.muted
                                                                font.pixelSize: Theme.fontSizeCaption
                                                                font.bold: !roomChatBubble.countIsStale && roomDelegate.chat_count > 0
                                                                anchors.verticalCenter: parent.verticalCenter
                                                            }
                                                        }

                                                        HoverHandler { id: roomChatHover }

                                                        Popup {
                                                            id: roomChatPopup
                                                            parent: roomChatBubble
                                                            visible: roomChatHover.hovered
                                                            modal: false
                                                            focus: false
                                                            closePolicy: Popup.NoAutoClose
                                                            padding: 10
                                                            width: 180
                                                            x: roomChatBubble.width + 6
                                                            y: Math.round((roomChatBubble.height - implicitHeight) / 2)

                                                            background: Rectangle {
                                                                color: Theme.bg2
                                                                radius: Theme.radiusMd
                                                                border.color: Theme.divider
                                                                border.width: 1
                                                            }

                                                            contentItem: Label {
                                                                width: roomChatPopup.availableWidth
                                                                text: !roomGroup.connected
                                                                    ? "Supernode offline — counts may be stale"
                                                                    : (!roomDelegate.chat_count_known
                                                                        ? "Waiting for room list — count not yet known"
                                                                        : ("In room chat: " + roomDelegate.chat_count))
                                                                color: Theme.text
                                                                font.pixelSize: Theme.fontSizeCaption
                                                                wrapMode: Text.WordWrap
                                                            }
                                                        }
                                                    }
                                                }

                                                Label {
                                                    text: roomDelegate.kind
                                                    color: Theme.muted
                                                    font.pixelSize: Theme.fontSizeCaption
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Room action buttons
                    Rectangle {
                        Layout.fillWidth: true
                        height: Theme.touchTarget
                        color: Theme.bg0

                        StyledButton {
                            anchors.centerIn: parent
                            width: parent.width - Theme.spacingMd * 2
                            text: "Join Room"
                            onClicked: joinRoomDialog.show()
                        }
                    }
                }
            }                    // end sidebar StackLayout

            // ── Settings section navigation ──────────────────────────────────
            // Replaces the peer/rooms area when Settings is the active nav.
            SettingsSidebar {
                id: settingsSidebar
                visible: navIndex === 2
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: settingsTab
                dirty: settingsModel ? settingsModel.dirty : false
                onSectionActivated: (index) => settingsTab = index
                onSaveRequested: if (settingsModel) settingsModel.save()

                // Polled rather than pushed: settings are written from around a
                // hundred places, most of which save immediately and some of
                // which do not, so a flag each writer had to set would be wrong
                // the first time one was added. The model compares itself with
                // what is on disk instead, which cannot drift.
                //
                // Only while Settings is on screen — this is for the button.
                Timer {
                    interval: 300
                    repeat: true
                    running: settingsSidebar.visible && settingsModel !== null
                    triggeredOnStart: true
                    onTriggered: settingsModel.refreshDirty()
                }
            }

            Rectangle {
                Layout.fillWidth: true
                height: Theme.touchTarget
                color: Theme.bg0

                RowLayout {
                    anchors.fill: parent
                    anchors.margins: Theme.spacingXs
                    spacing: Theme.spacingXs

                    Repeater {
                        model: [
                            { icon: "qrc:/qt/qml/ConquerD/Client/icons/speech.svg", label: "Chat", index: 0 },
                            { icon: "qrc:/qt/qml/ConquerD/Client/icons/gear.svg", label: "Settings", index: 2 }
                        ]

                        delegate: Item {
                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            property bool active: navIndex === modelData.index

                            Rectangle {
                                anchors.fill: parent
                                color: active ? Theme.selectedFill() : (navMouse.containsMouse ? Theme.bg3 : "transparent")
                                Behavior on color { ColorAnimation { duration: Theme.animNormal } }

                                Rectangle {
                                    visible: active
                                    anchors.left: parent.left
                                    anchors.verticalCenter: parent.verticalCenter
                                    width: 3
                                    height: parent.height - Theme.spacingSm
                                    color: Theme.accent
                                }
                            }

                            Image {
                                anchors.centerIn: parent
                                source: modelData.icon
                                sourceSize.width: 20
                                sourceSize.height: 20
                                width: 20
                                height: 20
                                opacity: active ? 1.0 : 0.72
                            }

                            MouseArea {
                                id: navMouse
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                ToolTip.text: modelData.label
                                ToolTip.visible: containsMouse
                                onClicked: {
                                    navIndex = modelData.index
                                    if (modelData.index === 0) backend.clearUnread()
                                }
                            }
                        }
                    }
                }
            }
        }

        // Content area (chat / rooms / settings)
        // Using visibility-based switching rather than StackLayout so that
        // each child always has anchors.fill: parent — they get the correct
        // size at the first layout pass with no lazy-init timing dependency.
        Item {
            id: contentArea
            Layout.fillWidth: true
            Layout.minimumWidth: 200
            Layout.fillHeight: true

            // Shared video area. Only chat and room views yield space to it —
            // settings and the portal keep the full area, so the region never
            // overlaps a page that has nothing to do with a call.
            Loader {
                id: videoRegionLoader
                anchors { top: parent.top; left: parent.left; right: parent.right }
                z: 40
                // Loaded lazily: a build without Qt Multimedia has no
                // VideoRegion in the qrc at all, and MainWindow must still parse.
                active: root.expandedVideoPeers.length > 0
                source: "qrc:/qt/qml/ConquerD/Client/qml/VideoRegion.qml"

                readonly property bool showing:
                    active && (navIndex === 0 || navIndex === 1)
                visible: showing
                height: showing
                    ? Math.round(contentArea.height * root.videoRegionRatio)
                    : 0

                Behavior on height {
                    NumberAnimation { duration: Theme.animFast; easing.type: Easing.InOutQuad }
                }

                onLoaded: {
                    item.expandedPeers = Qt.binding(() => root.expandedVideoPeers)
                    item.videoActivePeers = Qt.binding(() => root.videoActivePeers)
                    item.videoStalledPeers = Qt.binding(() => root.videoStalledPeers)
                    item.participantModel = Qt.binding(() =>
                        backend.voice_active && backend.in_room ? roomModel : directCallModel)
                    item.heightRatio = Qt.binding(() => root.videoRegionRatio)
                    item.collapseRequested.connect(root.collapseVideo)
                    item.popoutRequested.connect(root.popoutVideo)
                    // `backend` is only in scope here, so the tile's request
                    // arrives as a signal and is applied at this level.
                    item.contentAudioChanged.connect(function(peerId, muted, volume) {
                        backend.setContentAudioPref(peerId, muted, volume)
                    })
                    item.ratioChanged.connect(function(r) {
                        root.videoRegionRatio = r
                        videoRegionRatioSaveTimer.restart()
                    })
                }
            }

            ChatPanel {
                id: chatPanel
                anchors {
                    top: videoRegionLoader.bottom
                    left: parent.left
                    right: parent.right
                    bottom: parent.bottom
                }
                visible: navIndex === 0
                chatModel: chatModel
                fileTransferModel: fileTransferModel
                settingsModel: settingsModel
                youtubePreviewEnabled: settingsModel ? settingsModel.youtube_preview_enabled : true
                youtubeInlineAck: settingsModel ? settingsModel.youtube_inline_ack : false
                // `call_state` moves only on the direct-call paths — room voice
                // never touches it — so this is a 1:1 with the selected peer and
                // not a room they happen to share.
                callActiveWithPeer: backend.call_state !== "idle"
                                    && root.activeCallPeerId !== ""
                                    && root.activeCallPeerId === chatPanel.selectedPeerId
                onSendMessage: (peerId, msg) => backend.sendChat(peerId, msg)
                onStartCall: function(peerId) {
                    root.beginDirectCall(peerId)
                }
                onSendFile: (peerId, fileUrl) => backend.sendFile(peerId, fileUrl)
                onOpenAttachment: (path) => root.showFilePreview(path)
                Component.onCompleted: chatPanel.onActiveFocusChanged.connect(function() {
                    if (chatPanel.activeFocus) backend.clearUnread()
                })
            }

            RoomPanel {
                id: roomPanel
                anchors {
                    top: videoRegionLoader.bottom
                    left: parent.left
                    right: parent.right
                    bottom: parent.bottom
                }
                visible: navIndex === 1
                // Text members only — never the active voice roster (voice rail
                // owns roomModel). Peers who share this room's chat space.
                roomModel: textRoomModel
                fileTransferModel: fileTransferModel
                settingsModel: settingsModel
                youtubePreviewEnabled: settingsModel ? settingsModel.youtube_preview_enabled : true
                youtubeInlineAck: settingsModel ? settingsModel.youtube_inline_ack : false
                // Compare id + supernode, not the display name: two nodes can
                // host rooms with the same name. Gated on voice_active so the
                // never-cleared voiceRoomId cannot report a stale match.
                voiceActiveHere: backend.voice_active && backend.in_room
                                 && root.voiceRoomId !== ""
                                 && root.voiceRoomId === roomPanel.roomId
                                 && root.voiceSupernodeId === roomPanel.supernodeId
                onLeaveRoom: {
                    root.closeAllVideoPopouts()
                    root.expandedVideoPeers = []
                    backend.leaveRoom()
                    navIndex = 0
                }
                onJoinVoiceRequested: {
                    // Same path as a sidebar double-click.
                    backend.joinRoomWithVoice(roomPanel.supernodeId, roomPanel.roomId)
                    root.voiceRoomName = roomPanel.roomName
                    root.voiceSupernodeId = roomPanel.supernodeId
                    root.voiceRoomId = roomPanel.roomId
                }
                onOpenAttachment: (path) => root.showFilePreview(path)
            }

            // Full-size local media / document preview (images, video, PDF, …).
            // Loaded lazily so non-webengine builds still parse MainWindow.qml;
            // showFilePreview() falls back to the system app when unavailable.
            Loader {
                id: filePreviewLoader
                anchors.fill: parent
                visible: false
                z: 50
                active: false
                onLoaded: {
                    if (item && item.closeRequested) {
                        item.closeRequested.connect(function() {
                            filePreviewLoader.visible = false
                            if (filePreviewLoader.item)
                                filePreviewLoader.item.filePath = ""
                        })
                    }
                }
            }

            SettingsPage {
                id: settingsPage
                anchors.fill: parent
                visible: navIndex === 2
                settings: settingsModel
                currentTab: settingsTab
                // React to PTT setting changes at runtime
                Connections {
                    target: settingsModel
                    function onPush_to_talkChanged() {
                        if (settingsModel.push_to_talk) {
                            backend.enablePtt(settingsModel.ptt_key)
                        } else {
                            backend.disablePtt()
                        }
                    }
                    function onPtt_keyChanged() {
                        if (settingsModel.push_to_talk) {
                            backend.enablePtt(settingsModel.ptt_key)
                        }
                    }
                    function onVoice_activationChanged() {
                        backend.setVoiceActivation(settingsModel.voice_activation)
                    }
                }
            }

            // ── Portal / Browser panel ────────────────────────────────────────
            // Occupies the full right content area (navIndex === 3).
            // Loaded lazily so QtWebEngine is not required at parse time.
            // The panel loads when a conquerd:// portal is active:
            //   portalActive = true (locked to the secure scheme over the QUIC relay).
            Item {
                id: browserPanel
                anchors.fill: parent
                visible: navIndex === 3

                property bool nodeMode: false
                property bool portalActive: false
                // URL buffered while the Loader is still instantiating.
                property string pendingUrl: ""

                function navigateTo(url) {
                    console.log("[portal] browserPanel.navigateTo url=" + url + " loaderItem=" + _bpLoader.item)
                    if (_bpLoader.item) {
                        _bpLoader.item.navigateTo(url)
                    } else {
                        pendingUrl = url
                    }
                }
                onNodeModeChanged: {
                    if (_bpLoader.item) _bpLoader.item.nodeMode = nodeMode
                }

                Loader {
                    id: _bpLoader
                    anchors.fill: parent
                    source: browserPanel.portalActive
                        ? Qt.resolvedUrl("BrowserPanel.qml")
                        : ""
                    onItemChanged: {
                        if (item) {
                            console.log("[portal] Loader item ready, flushing pending=" + browserPanel.pendingUrl)
                            item.nodeMode = browserPanel.nodeMode
                            if (browserPanel.pendingUrl !== "") {
                                item.navigateTo(browserPanel.pendingUrl)
                                browserPanel.pendingUrl = ""
                            }
                        }
                    }
                }
            }
        }  // end contentArea

        // ── Right voice rail (replaces floating overlay) ──────────────────
        VoiceRail {
            id: voiceRail
            Layout.fillHeight: true
            // Animate width in/out for smooth open/close
            // Show only when voice is actually active — not for chat-only room joins.
            Layout.preferredWidth: backend.voice_active ? 200 : 0
            visible: Layout.preferredWidth > 0
            clip: true

            Behavior on Layout.preferredWidth {
                NumberAnimation { duration: Theme.animFast; easing.type: Easing.InOutQuad }
            }

            // Always the active voice session only (never the selected text room).
            participantModel: backend.voice_active && backend.in_room
                ? roomModel
                : directCallModel
            contextName: backend.voice_active && backend.in_room
                ? root.voiceRoomName
                : (root.activeCallPeerHandle() || chatPanel.selectedPeerName || chatPanel.selectedPeerId || "Call")
            supernodeId: backend.voice_active && backend.in_room ? root.voiceSupernodeId : ""
            supernodeHandle: backend.voice_active && backend.in_room
                ? root.supernodeHandleFor(root.voiceSupernodeId)
                : ""
            callState: backend.call_state
            inRoom: backend.voice_active && backend.in_room
            connectionMode: backend.connection_mode
            durationSecs: backend.call_duration_secs

            onEndCallRequested: {
                // Collapse expand/popout UI before the session ends so tiles
                // unregister and we do not keep a detached window on a stream
                // that is about to stop.
                root.closeAllVideoPopouts()
                root.expandedVideoPeers = []
                if (backend.voice_active && backend.in_room) {
                    backend.leaveRoom()
                    // Stay on the room text panel when a text room is still selected.
                    if (!roomPanel.roomId)
                        navIndex = 0
                } else {
                    backend.endCall()
                }
            }
            onMuteToggled: (m) => backend.setMuted(m)

            videoOn: backend.video_active
            shareAudioOn: backend.content_audio_active

            videoSources: root.shareCaptureSources
            videoSourceId: settingsModel.video_input_device
            videoOverlaysJson: settingsModel.video_overlays_json
            contentAudioMode: settingsModel.content_audio_mode

            videoUnavailableReason: root.videoUnavailableReason
            videoEncoderMissing: !root.videoEncoderAvailable

            onShareOptionsOpened: root.refreshShareCaptureSources()
            // Written straight to settings, so the menu and Settings › Video
            // are two views of one choice rather than two choices that can
            // disagree. Not saved here: the settings page's Save button owns
            // that, exactly as the audio mode below has always worked.
            onVideoSourceSelected: (sourceId) => settingsModel.video_input_device = sourceId
            onVideoOverlaysEdited: (json) => settingsModel.video_overlays_json = json

            /// Start sharing video, and the audio that belongs with it.
            ///
            /// Order matters: video creates the session clock, and the content
            /// audio is timestamped against it. Starting audio first would
            /// stamp it against a clock that does not exist yet.
            onShareRequested: (audioMode) => {
                settingsModel.content_audio_mode = audioMode

                var got = backend.setVideoEnabled(
                    true,
                    settingsModel.video_input_device,
                    settingsModel.video_quality,
                    settingsModel.video_overlays_json,
                    settingsModel.videoEncoderJson())
                if (!got) {
                    console.warn("[video] could not start sharing")
                    return
                }
                // Reflect our own state on our own tile straight away; remote
                // members learn about it from the SfuVideoState announcement.
                // `public_id` is the exposed Q_PROPERTY — `my_public_id` is the
                // internal Rust field name and reads as undefined from QML.
                if (roomModel && roomModel.setVideoActive && backend.public_id)
                    roomModel.setVideoActive(backend.public_id, true)
                root.setPeerVideoActive(backend.public_id, true)

                if (audioMode === "off")
                    return
                // Audio failing is not a reason to abandon the video that is
                // already running — the platform may simply have no loopback.
                var audioOk = backend.setContentAudioEnabled(
                    true,
                    settingsModel.video_input_device,
                    audioMode)
                if (audioOk)
                    return
                // "auto" on a camera resolves to no audio by design — your
                // microphone already carries you — so it is not a failure and
                // must not be logged as one, or the real failures below stop
                // being worth reading.
                var dev = settingsModel.video_input_device
                var isScreen = dev.indexOf("window:") === 0
                    || dev.indexOf("monitor:") === 0
                if (audioMode === "auto" && !isScreen)
                    return
                console.warn("[content-audio] sharing video without audio: "
                    + "no capture endpoint for mode '" + audioMode + "'")
            }

            /// Stop both. Content audio cannot outlive the clock it is stamped
            /// against, so it is stopped first and explicitly.
            onStopShareRequested: {
                if (backend.content_audio_active)
                    backend.setContentAudioEnabled(
                        false,
                        settingsModel.video_input_device,
                        settingsModel.content_audio_mode)
                backend.setVideoEnabled(
                    false,
                    settingsModel.video_input_device,
                    settingsModel.video_quality,
                    settingsModel.video_overlays_json,
                    settingsModel.videoEncoderJson())
                if (roomModel && roomModel.setVideoActive && backend.public_id)
                    roomModel.setVideoActive(backend.public_id, false)
                root.setPeerVideoActive(backend.public_id, false)
            }

            expandedPeers: root.expandedVideoPeers
            onExpandVideoRequested: (pid) => root.toggleVideoExpanded(pid)
            onPopoutVideoRequested: (pid) => root.popoutVideo(pid)
        }
    }

    // ── System tray icon (port of client_desktop/taskbar_badge.py setup_tray) ─
    Platform.SystemTrayIcon {
        id: trayIcon
        visible: true
        icon.source: "qrc:/assets/conquerd.ico"
        tooltip: backend.session_banner.length > 0 ? backend.session_banner : "ConquerD"

        menu: Platform.Menu {
            Platform.MenuItem {
                text: qsTr("Show ConquerD")
                onTriggered: root.showFromTray()
            }
            Platform.MenuItem {
                text: qsTr("Mute microphone")
                checkable: true
                onTriggered: backend.setMuted(checked)
            }
            Platform.MenuSeparator { }
            Platform.MenuItem {
                text: qsTr("Quit")
                onTriggered: {
                    geometrySaveTimer.stop()
                    root.persistWindowGeometry()
                    Qt.quit()
                }
            }
        }

        onActivated: function(reason) {
            // On Windows, single-click = Trigger; double-click = DoubleClick.
            if (reason === Platform.SystemTrayIcon.Trigger
                || reason === Platform.SystemTrayIcon.DoubleClick) {
                if (root.visible) {
                    root.raise()
                    root.requestActivate()
                } else {
                    root.showFromTray()
                }
            }
        }
    }

    // Guard to suppress geometry saves during initial restore.
    property bool _restoringGeometry: false
    property bool _windowWasMaximized: false

    function showFromTray() {
        if (root._windowWasMaximized)
            root.showMaximized()
        else
            root.showNormal()
        root.raise()
        root.requestActivate()
    }

    function persistWindowGeometry() {
        if (root._restoringGeometry)
            return
        if (root.visibility === Window.Windowed) {
            settingsModel.window_x = root.x
            settingsModel.window_y = root.y
            settingsModel.window_width = root.width
            settingsModel.window_height = root.height
            settingsModel.window_position_saved = true
        }
        settingsModel.window_maximized = root._windowWasMaximized
        settingsModel.save()
    }

    // Debounce saves while the user moves or resizes the normal window.
    Timer {
        id: geometrySaveTimer
        interval: 600
        onTriggered: root.persistWindowGeometry()
    }
    onXChanged:      if (!root._restoringGeometry && root.visibility === Window.Windowed) geometrySaveTimer.restart()
    onYChanged:      if (!root._restoringGeometry && root.visibility === Window.Windowed) geometrySaveTimer.restart()
    onWidthChanged:  if (!root._restoringGeometry) geometrySaveTimer.restart()
    onHeightChanged: if (!root._restoringGeometry) geometrySaveTimer.restart()

    // True once we've explained (via a tray balloon) that the window was hidden
    // to the tray rather than closed — shown only on the first hide per session
    // so users aren't surprised by a process with no visible window.
    property bool _trayHintShown: false

    // Hide the window into the system tray and, the first time, tell the user
    // the app is still running and how to get it back.
    function hideToTray() {
        root.hide()
        if (!root._trayHintShown && trayIcon.available) {
            trayIcon.showMessage(
                qsTr("ConquerD is still running"),
                qsTr("The window was minimized to the tray. Click the tray icon to restore it, or use Quit to exit."),
                Platform.SystemTrayIcon.Information,
                5000)
            root._trayHintShown = true
        }
    }

    // Closing the window quits the application — unless "Minimize to tray" is
    // enabled and a tray icon is available, in which case the window is hidden
    // into the tray and ConquerD keeps running in the background. (Without the
    // setting, hiding on close surprised users who had no visible indication
    // the process was still alive.) The tray icon's Quit / Show items always
    // provide explicit control.
    onClosing: function(close) {
        geometrySaveTimer.stop()
        root.persistWindowGeometry()
        if (settingsModel.minimize_to_tray && trayIcon.available) {
            close.accepted = false
            hideToTray()
        } else {
            // Close popouts before quitting. They are separate top-level
            // windows, so leaving them open would both keep the process alive
            // and leave their HWNDs in the chrome tracking set during teardown.
            root.closeAllVideoPopouts()
            Qt.quit()
        }
    }

    // Minimizing the window also tucks it into the tray when the setting is on.
    onVisibilityChanged: function(visibility) {
        if (!root._restoringGeometry) {
            if (visibility === Window.Maximized)
                root._windowWasMaximized = true
            else if (visibility === Window.Windowed)
                root._windowWasMaximized = false
            if (visibility === Window.Maximized || visibility === Window.Windowed)
                geometrySaveTimer.restart()
        }
        if (visibility === Window.Minimized
                && settingsModel.minimize_to_tray
                && trayIcon.available) {
            hideToTray()
        }
    }

    // ── Global keyboard shortcuts ─────────────────────────────────────────
    Shortcut {
        sequence: "Ctrl+Q"
        context: Qt.ApplicationShortcut
        onActivated: Qt.quit()
    }
    Shortcut {
        sequence: "Ctrl+W"
        context: Qt.ApplicationShortcut
        onActivated: {
            if (trayIcon.available) root.hide()
            else Qt.quit()
        }
    }
    Shortcut {
        sequence: "Ctrl+,"
        context: Qt.ApplicationShortcut
        onActivated: navIndex = 2
    }
    Shortcut {
        sequence: "Ctrl+K"
        context: Qt.ApplicationShortcut
        onActivated: {
            navIndex = 0
            inviteField.forceActiveFocus()
            inviteField.selectAll()
        }
    }
}
