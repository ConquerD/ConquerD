// MainWindow.qml — Conquerd native client main window (Phase 3 scaffold)
//
// Hosts the navigation rail, chat panel, call panel, and session banner.
// Binds to AppBridge (exposed from Rust via cxx-qt) for live state.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Qt.labs.platform as Platform
import ConquerD.Client 1.0

ApplicationWindow {
    id: root
    title: ""
    width: 1100
    height: 700
    visible: true
    minimumWidth: 960
    minimumHeight: 640
    // Qt.CustomizeWindowHint hides the native title bar while keeping
    // WS_THICKFRAME + WS_CAPTION so DWM provides Aero Snap, Snap Layouts
    // (Win11 hover-maximize), and Win+Arrow snapping.
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

    function canonicalNodeId(nodeId) {
        if (!nodeId || nodeId === "") return ""
        if (!backend.isKnownSupernode(nodeId)) return ""
        var resolved = backend.resolveSupernodeNodeId(nodeId)
        return resolved !== "" ? resolved : nodeId
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

    function refreshDirectCallModel() {
        directCallModel.clear()
        var remoteId = root.activeCallPeerId || chatPanel.selectedPeerId
        if (!remoteId || remoteId === "") return
        directCallModel.append({
            peerId: backend.public_id,
            handle: "You",
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
                        rooms_json: "[]"
                    })
                }
            }
        } catch (e) {
            console.warn("syncRoomsSidebar parse error:", e)
        }
    }

    // Union two room snapshots by room_id (used when deduping alias node rows).
    function mergeRoomLists(a, b) {
        var byId = {}
        for (var i = 0; i < a.length; i++)
            if (a[i].room_id) byId[a[i].room_id] = a[i]
        for (var j = 0; j < b.length; j++)
            if (b[j].room_id) byId[b[j].room_id] = b[j]
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

    function upsertSfuRoomGroup(supernodeId, rooms) {
        var canon = canonicalNodeId(supernodeId)
        if (canon === "") return

        var normalized = []
        for (var i = 0; i < rooms.length; i++) {
            var r = rooms[i]
            normalized.push({
                room_id: r.room_id || "",
                name: r.name || r.room_name || r.room_id || "Room",
                kind: r.kind || r.room_type || "voice",
                count: r.count || r.member_count || 0,
                creator_id: r.creator_id || "",
                is_default: r.is_default === true || r.room_id === "default"
            })
        }

        var nodeIdx = findNodeIndex(canon)
        if (nodeIdx >= 0) {
            var existing = []
            try { existing = JSON.parse(nodeListModel.get(nodeIdx).rooms_json || "[]") } catch (e) {}
            var merged = normalized.length > 0
                ? root.mergeRoomLists(existing, normalized)
                : existing
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

        // Logo — ConquerD "D" chevron SVG as inline data URI
        Image {
            id: logoImage
            Layout.preferredWidth: 48
            Layout.preferredHeight: 22
            Layout.alignment: Qt.AlignVCenter
            fillMode: Image.PreserveAspectFit
            source: "data:image/svg+xml;utf8,<svg viewBox='0 0 122 56' xmlns='http://www.w3.org/2000/svg'><path fill-rule='evenodd' clip-rule='evenodd' d='M0,0 L36,0 L48,28 L36,56 L0,56 Z M11,11 L28,11 L37,28 L28,45 L11,45 Z' fill='%23FF2B40'/><rect x='58' y='17' width='8' height='8' rx='1' fill='%23FF2B40'/><rect x='58' y='41' width='8' height='8' rx='1' fill='%23FF2B40'/><polygon points='76,56 87,56 105,0 94,0' fill='%23FF2B40'/><polygon points='92,56 103,56 121,0 110,0' fill='%23FF2B40'/></svg>"
        }

        // Invite / peer-ID paste field
        TextField {
            id: inviteField
            Layout.preferredWidth: 220
            Layout.alignment: Qt.AlignVCenter
            implicitHeight: Theme.controlHeight
            placeholderText: "Paste invite or peer ID\u2026"
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
        Button {
            id: connectBtn
            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/invite.svg"
            implicitWidth: 36
            implicitHeight: 28
            Layout.alignment: Qt.AlignVCenter
            enabled: inviteField.text.trim().length > 0
            flat: true
            Material.foreground: enabled ? Theme.text : Theme.muted
            ToolTip.text: "Connect to peer / accept invite"
            ToolTip.visible: hovered
            onClicked: {
                var u = inviteField.text.trim()
                if (u.length > 0) {
                    backend.pasteInvite(u)
                    inviteField.text = ""
                }
            }
        }

        // New Invite button
        Button {
            id: newInviteBtn
            text: "Invite"
            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/invite.svg"
            icon.width: 16
            icon.height: 16
            icon.color: Theme.text
            implicitHeight: 28
            Layout.alignment: Qt.AlignVCenter
            flat: true
            Material.foreground: Theme.text
            ToolTip.text: "Copy new invite link to clipboard (Ctrl+N)"
            ToolTip.visible: hovered
            onClicked: {
                backend.copyInvite()
                invitePopup.visible = true
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
                onClicked: { navIndex = 2; settingsTab = 1 }
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
    // Remote peer for an active direct P2P voice call.
    property string activeCallPeerId: ""

    // Settings section index (0=Audio … 7=Diagnostics). Drives SettingsPage.currentTab.
    property int settingsTab: 0

    // Auto-switch to room tab when joinRoom succeeds
    Connections {
        target: backend
        function onIn_roomChanged() {
            if (backend.in_room) navIndex = 1
            else if (navIndex === 1) navIndex = 0
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
    RoomModel         { id: roomModel }
    FileTransferModel { id: fileTransferModel }

    // Sidebar: supernodes with grouped SFU rooms (nodesUpdated + sfuRoomsUpdated)
    ListModel { id: nodeListModel }

    Component.onCompleted: {
        settingsModel.load()
        applyThemePreference(settingsModel.theme)

        // ── Push saved avatar config into the bridge so avatarSvg() uses it ─
        backend.setAvatarConfigJson(settingsModel.avatar_config_json)

        // ── Restore window geometry (clamp to minimum usable size) ───────
        root._restoringGeometry = true
        if (settingsModel.window_width > 0)
            root.width = Math.max(root.minimumWidth, settingsModel.window_width)
        if (settingsModel.window_height > 0)
            root.height = Math.max(root.minimumHeight, settingsModel.window_height)
        root._restoringGeometry = false

        // ── Present the main window ───────────────────────────────────────
        // start_minimized only when the tray icon is available; otherwise
        // users would see a process with no visible UI.
        if (settingsModel.start_minimized && trayIcon.available) {
            root.showMinimized()
        } else {
            root.visible = true
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
        backend.localSpeakingChanged.connect(function(speaking) {
            roomModel.updateParticipant(backend.public_id, speaking, false)
        })
        backend.peerSpeakingChanged.connect(function(peerId, speaking) {
            roomModel.updateParticipant(peerId, speaking, false)
        })
        backend.peerLevelChanged.connect(function(peerId, level) {
            roomModel.setAudioLevel(peerId, level)
        })
        // File transfer model wiring
        backend.fileOffered.connect(fileTransferModel.upsertTransfer)
        backend.fileProgress.connect(fileTransferModel.setProgress)
        backend.fileComplete.connect(function(json) {
            try { fileTransferModel.markComplete(JSON.parse(json).transfer_id) } catch(e) {}
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
                root.upsertSfuRoomGroup(obj.supernode_id || "", obj.rooms || [])
            } catch(e) { console.warn("sfuRoomsUpdated parse error:", e) }
        })

        backend.roomsSidebarSync.connect(root.syncRoomsSidebar)

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
                        if (p.connected !== undefined && p.connected !== null)
                            nodeListModel.setProperty(nodeIdx, "connected", p.connected)
                        if (p.homepage_url !== undefined)
                            nodeListModel.setProperty(nodeIdx, "homepage_url", p.homepage_url)
                        if (p.title !== undefined)
                            nodeListModel.setProperty(nodeIdx, "title", p.title)
                        if (p.sfu_enabled !== undefined && p.sfu_enabled !== null)
                            nodeListModel.setProperty(nodeIdx, "sfu_enabled", p.sfu_enabled)
                    } else {
                        nodeListModel.append({
                            node_id:      canon,
                            connected:    p.connected || false,
                            homepage_url: p.homepage_url || "",
                            title:        p.title || "",
                            sfu_enabled:  p.sfu_enabled || false,
                            rooms_json:   "[]"
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

        // Wire message deletion: remove from in-memory model when backend confirms
        backend.messageDeleted.connect(function(msgId) {
            chatModel.removeMessage(msgId)
        })
        // Wire peer history clear: wipe the in-memory model
        backend.peerHistoryCleared.connect(function(peerId) {
            if (chatPanel.selectedPeerId === peerId) {
                chatModel.clearMessages()
            }
        })

        backend.initializeBackend()
        // Drop any stale non-supernode rows left from older builds.
        Qt.callLater(root.pruneNonSupernodeEntries)
    }

    // ── Passphrase dialog — shown when identity needs unlocking/creation ──
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
            navIndex = 1
            if (roomType === "private" && inviteToken !== "") {
                backend.copyToClipboard(inviteToken)
                if (trayIcon.available) {
                    trayIcon.showMessage(
                        qsTr("Private room created"),
                        qsTr("Invite token copied to clipboard."),
                        Platform.SystemTrayIcon.Information,
                        5000)
                }
            }
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
        onJoinRequested: function(supernodeId, roomId) {
            roomPanel.switchToRoom(roomId, roomId, supernodeId)
            backend.joinRoom(supernodeId, roomId)
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
                    icon.width: 16
                    icon.height: 16
                    icon.color: Theme.text
                    implicitHeight: 28
                    implicitWidth: 36
                    flat: true
                    Material.foreground: Theme.text
                    ToolTip.text: "Copy to clipboard"
                    ToolTip.visible: hovered
                    onClicked: {
                        backend.copyInvite()
                        invitePopup.visible = false
                    }
                }
            }
        }
    }

    // ── Session banner below topbar ───────────────────────────────────────
    SessionBanner {
        id: banner
        anchors {
            top: customTitleBar.bottom
            left: parent.left
            right: parent.right
        }
        height: Theme.bannerHeight
        bannerText: backend.session_banner
        connectionMode: backend.connection_mode
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

    // ── Main body below banner ────────────────────────────────────────────
    RowLayout {
        anchors {
            top: banner.bottom
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
                        root.trackDirectCall(peerId)
                        backend.startCall(peerId)
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
                                navIndex = 1
                            }
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

                        MenuItem {
                            text: qsTr("Create Public Room…")
                            enabled: nodeContextMenu.targetConnected && nodeContextMenu.targetSfuEnabled
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
                            required property string rooms_json

                            readonly property var rooms: {
                                try {
                                    return JSON.parse(roomGroup.rooms_json || "[]")
                                } catch (e) {
                                    return []
                                }
                            }

                            readonly property real groupHeight:
                                Math.max(48, roomGroup.rooms.length * 48) + Theme.spacingSm

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
                                        text: roomGroup.node_id
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
                                        model: roomGroup.rooms

                                        delegate: ItemDelegate {
                                            id: roomDelegate
                                            required property string room_id
                                            required property string name
                                            required property string kind
                                            required property int count
                                            required property string creator_id
                                            required property bool is_default

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

                                            ColumnLayout {
                                                anchors.verticalCenter: parent.verticalCenter
                                                anchors.left: parent.left
                                                anchors.right: parent.right
                                                spacing: Theme.spacingXs

                                                Label {
                                                    Layout.fillWidth: true
                                                    text: roomDelegate.name
                                                    color: Theme.text
                                                    font.pixelSize: Theme.fontSizeBody
                                                    font.bold: roomDelegate.roomSelected
                                                    elide: Text.ElideRight
                                                }
                                                Label {
                                                    text: roomDelegate.kind
                                                        + (roomDelegate.count > 0
                                                           ? " \u00B7 " + roomDelegate.count
                                                           : "")
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
                visible: navIndex === 2
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: settingsTab
                onSectionActivated: (index) => settingsTab = index
                onSaveRequested: if (settingsModel) settingsModel.save()
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

            ChatPanel {
                id: chatPanel
                anchors.fill: parent
                visible: navIndex === 0
                chatModel: chatModel
                fileTransferModel: fileTransferModel
                settingsModel: settingsModel
                youtubePreviewEnabled: settingsModel ? settingsModel.youtube_preview_enabled : true
                youtubeInlineAck: settingsModel ? settingsModel.youtube_inline_ack : false
                onSendMessage: (peerId, msg) => backend.sendChat(peerId, msg)
                onStartCall: function(peerId) {
                    root.trackDirectCall(peerId)
                    backend.startCall(peerId)
                }
                onSendFile: (peerId, fileUrl) => backend.sendFile(peerId, fileUrl)
                Component.onCompleted: chatPanel.onActiveFocusChanged.connect(function() {
                    if (chatPanel.activeFocus) backend.clearUnread()
                })
            }

            RoomPanel {
                id: roomPanel
                anchors.fill: parent
                visible: navIndex === 1
                roomModel: roomModel
                fileTransferModel: fileTransferModel
                settingsModel: settingsModel
                youtubePreviewEnabled: settingsModel ? settingsModel.youtube_preview_enabled : true
                youtubeInlineAck: settingsModel ? settingsModel.youtube_inline_ack : false
                onLeaveRoom: {
                    backend.leaveRoom()
                    navIndex = 0
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
                NumberAnimation { duration: 180; easing.type: Easing.InOutQuad }
            }

            participantModel: backend.in_room ? roomModel : directCallModel
            contextName: backend.in_room
                ? root.voiceRoomName
                : (root.activeCallPeerHandle() || chatPanel.selectedPeerName || chatPanel.selectedPeerId || "Call")
            supernodeId: backend.in_room ? root.voiceSupernodeId : ""
            supernodeHandle: backend.in_room
                ? root.supernodeHandleFor(root.voiceSupernodeId)
                : ""
            callState: backend.call_state
            inRoom: backend.in_room
            connectionMode: backend.connection_mode
            durationSecs: backend.call_duration_secs

            onEndCallRequested: {
                if (backend.in_room) {
                    backend.leaveRoom()
                    navIndex = 0
                } else {
                    backend.endCall()
                }
            }
            onMuteToggled: (m) => backend.setMuted(m)
        }
    }

    // ── System tray icon (port of client_desktop/taskbar_badge.py setup_tray) ─
    Platform.SystemTrayIcon {
        id: trayIcon
        visible: true
        icon.source: "qrc:/icons/conquerd.png"
        tooltip: backend.session_banner.length > 0 ? backend.session_banner : "ConquerD"

        menu: Platform.Menu {
            Platform.MenuItem {
                text: qsTr("Show ConquerD")
                onTriggered: {
                    root.show()
                    root.raise()
                    root.requestActivate()
                }
            }
            Platform.MenuItem {
                text: qsTr("Mute microphone")
                checkable: true
                onTriggered: backend.setMuted(checked)
            }
            Platform.MenuSeparator { }
            Platform.MenuItem {
                text: qsTr("Quit")
                onTriggered: Qt.quit()
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
                    root.show()
                }
            }
        }
    }

    // Guard to suppress geometry saves during initial restore.
    property bool _restoringGeometry: false

    // Debounce timer: save geometry 600ms after the last resize.
    Timer {
        id: geometrySaveTimer
        interval: 600
        onTriggered: {
            settingsModel.window_width  = root.width
            settingsModel.window_height = root.height
            settingsModel.save()
        }
    }
    onWidthChanged:  if (!root._restoringGeometry) geometrySaveTimer.restart()
    onHeightChanged: if (!root._restoringGeometry) geometrySaveTimer.restart()

    // Closing the window quits the application. (Previously this hid the
    // window into the system tray and left ConquerD.exe running in the
    // background, which surprised users who had no visible indication
    // the process was still alive.) Use the tray icon's Quit / Show items
    // for explicit background operation.
    onClosing: function(close) {
        Qt.quit()
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
