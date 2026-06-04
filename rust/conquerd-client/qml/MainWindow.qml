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
    minimumWidth: 700
    minimumHeight: 500
    // Qt.CustomizeWindowHint hides the native title bar while keeping
    // WS_THICKFRAME + WS_CAPTION so DWM provides Aero Snap, Snap Layouts
    // (Win11 hover-maximize), and Win+Arrow snapping.
    flags: Qt.Window | Qt.CustomizeWindowHint

    Material.theme: Material.Dark
    Material.accent: Material.Blue

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
            implicitHeight: 28
            placeholderText: "Paste invite or peer ID\u2026"
            color: "#DCDDDE"
            placeholderTextColor: "#72767D"
            leftPadding: 8
            rightPadding: 8
            background: Rectangle {
                color: "#383A40"
                radius: 0
                border.color: inviteField.activeFocus ? "#5865F2" : "transparent"
                border.width: 1
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
            Material.foreground: enabled ? "#DCDDDE" : "#72767D"
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
            icon.color: "#DCDDDE"
            implicitHeight: 28
            Layout.alignment: Qt.AlignVCenter
            flat: true
            Material.foreground: "#DCDDDE"
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

    // Live-data models
    PeerListModel     { id: peerModel }
    ChatModel         { id: chatModel }
    RoomModel         { id: roomModel }
    FileTransferModel { id: fileTransferModel }

    // Sidebar: available SFU rooms (updated by sfuRoomsUpdated signal)
    ListModel { id: sfuRoomListModel }

    // Sidebar: connected supernodes (updated by nodesUpdated signal)
    ListModel { id: nodeListModel }

    Component.onCompleted: {
        settingsModel.load()

        // ── Push saved avatar config into the bridge so avatarSvg() uses it ─
        backend.setAvatarConfigJson(settingsModel.avatar_config_json)

        // ── Restore window geometry ───────────────────────────────────────
        root._restoringGeometry = true
        if (settingsModel.window_width  > 0) root.width  = settingsModel.window_width
        if (settingsModel.window_height > 0) root.height = settingsModel.window_height
        root._restoringGeometry = false

        // ── Apply start-minimized preference ─────────────────────────────
        if (settingsModel.start_minimized) root.showMinimized()

        backend.peersUpdated.connect(peerModel.setPeers)
        backend.chatMessageReceived.connect(chatModel.appendMessage)
        backend.chatHistoryLoaded.connect(chatModel.setMessages)
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

        // Wire room list updates into sfuRoomListModel
        backend.sfuRoomsUpdated.connect(function(json) {
            try {
                var obj = JSON.parse(json)
                var rooms = obj.rooms || []
                sfuRoomListModel.clear()
                for (var i = 0; i < rooms.length; i++) {
                    var r = rooms[i]
                    sfuRoomListModel.append({
                        room_id:      r.room_id      || "",
                        name:         r.name         || r.room_id || "Room",
                        kind:         r.kind         || r.room_type || "voice",
                        count:        r.count        || r.member_count || 0,
                        supernode_id: obj.supernode_id || ""
                    })
                }
            } catch(e) { console.warn("sfuRoomsUpdated parse error:", e) }
        })

        // Wire node connect/disconnect into nodeListModel (upsert by node_id)
        backend.nodesUpdated.connect(function(json) {
            try {
                var patches = JSON.parse(json)
                for (var i = 0; i < patches.length; i++) {
                    var p = patches[i]
                    var found = false
                    for (var j = 0; j < nodeListModel.count; j++) {
                        if (nodeListModel.get(j).node_id === p.node_id) {
                            // Update only the fields present in the patch.
                            if (p.connected !== undefined && p.connected !== null)
                                nodeListModel.setProperty(j, "connected", p.connected)
                            if (p.homepage_url !== undefined)
                                nodeListModel.setProperty(j, "homepage_url", p.homepage_url)
                            if (p.title !== undefined)
                                nodeListModel.setProperty(j, "title", p.title)
                            found = true; break
                        }
                    }
                    if (!found) {
                        nodeListModel.append({
                            node_id:      p.node_id,
                            connected:    p.connected || false,
                            homepage_url: p.homepage_url || "",
                            title:        p.title || ""
                        })
                    }
                }
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
    }

    // ── Incoming call overlay ─────────────────────────────────────────────
    IncomingCallDialog {
        id: incomingCallDialog
        anchors.centerIn: parent
        z: 100
        onAccepted: function(peerId) { backend.acceptCall(peerId) }
        onRejected: function(peerId) { backend.rejectCall(peerId) }
    }

    // ── Join Room dialog ──────────────────────────────────────────────────
    JoinRoomDialog {
        id: joinRoomDialog
        anchors.centerIn: parent
        z: 100
        onJoinRequested: function(supernodeId, roomId) {
            roomPanel.switchToRoom(roomId, roomId)
            backend.joinRoom(supernodeId, roomId)
            navIndex = 1
        }
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
        color: "#5865F2"

        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 12
            anchors.rightMargin: 8
            spacing: 8

            Label {
                Layout.fillWidth: true
                text: "Update " + updateBanner.tag + " is available!"
                color: "#FFFFFF"
                font.pixelSize: 12
            }
            Button {
                text: "Install"
                flat: true
                Material.foreground: "#FFFFFF"
                onClicked: {
                    backend.applyUpdate()
                    updateBanner.visible = false
                }
            }
            Button {
                text: "\u00D7"
                flat: true
                Material.foreground: "#FFFFFF"
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
            color: "#2B2D31"
            radius: 0
            border.color: "#1E1F22"
            border.width: 1
        }

        ColumnLayout {
            anchors.fill: parent
            anchors.margins: 12
            spacing: 8

            Label {
                text: "Invite link copied to clipboard:"
                color: "#DCDDDE"
                font.pixelSize: 11
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: 6

                TextField {
                    Layout.fillWidth: true
                    implicitHeight: 28
                    text: backend.invite_url
                    readOnly: true
                    color: "#DCDDDE"
                    font.pixelSize: 10
                    background: Rectangle { color: "#383A40"; radius: 0 }
                }

                Button {
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/clipboard.svg"
                    icon.width: 16
                    icon.height: 16
                    icon.color: "#DCDDDE"
                    implicitHeight: 28
                    implicitWidth: 36
                    flat: true
                    Material.foreground: "#DCDDDE"
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
        height: 32
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
                directCallModel.clear()
                // Self entry
                directCallModel.append({
                    peerId: backend.public_id,
                    handle: "You",
                    muted: voiceRail.muted,
                    audioLevel: 0.0,
                    isSelf: true
                })
                // Remote peer entry (name from currently selected peer or "Peer")
                directCallModel.append({
                    peerId: chatPanel.selectedPeerId,
                    handle: chatPanel.selectedPeerId || "Peer",
                    muted: false,
                    audioLevel: 0.0,
                    isSelf: false
                })
            } else {
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

        // ── Left sidebar: Peers | Rooms | Nodes tabs ─────────────────────────
        ColumnLayout {
            Layout.preferredWidth: 220
            Layout.minimumWidth: 220
            Layout.maximumWidth: 220
            Layout.fillHeight: true
            clip: true
            spacing: 0

            // Tab bar (hidden while settings nav is active)
            TabBar {
                id: sidebarTabBar
                Layout.fillWidth: true
                implicitHeight: 36
                visible: navIndex !== 2
                Material.accent: Material.Blue

                TabButton {
                    text: "Peers"
                    font.pixelSize: 11
                    font.bold: sidebarTabBar.currentIndex === 0
                    implicitHeight: 36
                }
                TabButton {
                    text: "Rooms"
                    font.pixelSize: 11
                    font.bold: sidebarTabBar.currentIndex === 1
                    implicitHeight: 36
                }
                TabButton {
                    text: "Nodes"
                    font.pixelSize: 11
                    font.bold: sidebarTabBar.currentIndex === 2
                    implicitHeight: 36
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
                    onPeerSelected: function(peerId, handle) {
                        chatPanel.selectedPeerId = peerId
                        chatPanel.selectedPeerName = handle
                        backend.selectPeer(peerId)
                        navIndex = 0
                        backend.clearUnread()
                        // Clear the badge on the peer row immediately
                        peerModel.setPeerUnread(peerId, 0)
                    }
                    onStartCallRequested: (peerId) => backend.startCall(peerId)
                    onRemovePeerRequested: (peerId) => backend.removePeer(peerId)
                    onCopyPeerIdRequested: (peerId) => backend.copyPeerId(peerId)
                    onBlockPeerRequested: (peerId) => backend.blockPeer(peerId)
                    onUnblockPeerRequested: (peerId) => backend.unblockPeer(peerId)
                    onClearHistoryRequested: function(peerId) {
                        backend.clearPeerHistory(peerId)
                    }
                }

                // ── Tab 1: Rooms ──────────────────────────────────────────
                ColumnLayout {
                    spacing: 0

                    // Rooms list
                    ListView {
                        id: roomsListView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: sfuRoomListModel
                        clip: true

                        // Empty state
                        Label {
                            anchors.centerIn: parent
                            visible: sfuRoomListModel.count === 0
                            text: "Connect to a supernode\nto browse rooms."
                            horizontalAlignment: Text.AlignHCenter
                            color: Theme.muted
                            font.pixelSize: 11
                            wrapMode: Text.WordWrap
                        }

                        delegate: ItemDelegate {
                            width: roomsListView.width
                            height: 48
                            highlighted: backend.in_room && navIndex === 1
                            // Single click: subscribe to room chat (no voice join, no voice disruption)
                            onClicked: {
                                roomPanel.switchToRoom(model.name || model.room_id, model.room_id)
                                backend.subscribeRoomChat(model.supernode_id, model.room_id)
                                navIndex = 1
                            }
                            // Double click or right-click "Join Voice": join room with voice
                            onDoubleClicked: {
                                roomPanel.switchToRoom(model.name || model.room_id, model.room_id)
                                backend.joinRoomWithVoice(model.supernode_id, model.room_id)
                                root.voiceRoomName = model.name || model.room_id
                                navIndex = 1
                            }

                            // Right-click context menu
                            MouseArea {
                                anchors.fill: parent
                                acceptedButtons: Qt.RightButton
                                onClicked: (mouse) => {
                                    if (mouse.button === Qt.RightButton)
                                        roomContextMenu.popup()
                                }
                            }

                            Menu {
                                id: roomContextMenu
                                MenuItem {
                                    text: qsTr("Join Voice Room")
                                    onTriggered: {
                                        roomPanel.switchToRoom(model.name || model.room_id, model.room_id)
                                        backend.joinRoomWithVoice(model.supernode_id, model.room_id)
                                        root.voiceRoomName = model.name || model.room_id
                                        navIndex = 1
                                    }
                                }
                            }

                            ColumnLayout {
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.left: parent.left
                                anchors.leftMargin: 12
                                anchors.right: parent.right
                                anchors.rightMargin: 8
                                spacing: 2

                                Label {
                                    Layout.fillWidth: true
                                    text: model.name
                                    color: Theme.text
                                    font.pixelSize: 12
                                    elide: Text.ElideRight
                                }
                                Label {
                                    text: model.kind + (model.count > 0 ? " \u00B7 " + model.count : "")
                                    color: Theme.muted
                                    font.pixelSize: 10
                                }
                            }
                        }
                    }

                    // Room action buttons
                    Rectangle {
                        Layout.fillWidth: true
                        height: 44
                        color: Theme.bg0

                        RowLayout {
                            anchors.fill: parent
                            anchors.margins: 6
                            spacing: 6

                            Button {
                                Layout.fillWidth: true
                                text: "Join Room"
                                implicitHeight: 30
                                flat: true
                                Material.foreground: Theme.text
                                onClicked: joinRoomDialog.show()
                            }
                        }
                    }
                }

                // ── Tab 2: Nodes ──────────────────────────────────────────
                ColumnLayout {
                    spacing: 0

                    ListView {
                        id: nodesListView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: nodeListModel
                        clip: true

                        Label {
                            anchors.centerIn: parent
                            visible: nodeListModel.count === 0
                            text: "No supernodes connected."
                            horizontalAlignment: Text.AlignHCenter
                            color: Theme.muted
                            font.pixelSize: 11
                            wrapMode: Text.WordWrap
                        }

                        delegate: ItemDelegate {
                            id: nodeDelegate
                            width: nodesListView.width
                            height: model.homepage_url ? 60 : 48

                            RowLayout {
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.left: parent.left
                                anchors.leftMargin: 12
                                anchors.right: parent.right
                                anchors.rightMargin: 8
                                spacing: 8

                                // Status dot
                                Rectangle {
                                    width: 8; height: 8; radius: 4
                                    color: model.connected ? Theme.online : Theme.muted
                                }

                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: 2

                                    Label {
                                        Layout.fillWidth: true
                                        text: model.title ||
                                              (model.node_id.length > 16
                                               ? model.node_id.substring(0, 16) + "\u2026"
                                               : model.node_id)
                                        color: model.connected ? Theme.text : Theme.muted
                                        font.pixelSize: 11
                                        elide: Text.ElideRight
                                        ToolTip.text: model.node_id
                                        ToolTip.visible: hovered
                                    }
                                    Label {
                                        visible: !!model.homepage_url
                                        Layout.fillWidth: true
                                        text: model.homepage_url
                                        color: Theme.muted
                                        font.pixelSize: 9
                                        elide: Text.ElideRight
                                    }
                                }

                                // Open Portal button — always visible so you can
                                // click it even before the relay connects (the
                                // backend queues the request and shows a spinner).
                                ToolButton {
                                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/globe.svg"
                                    icon.width: 16
                                    icon.height: 16
                                    icon.color: model.connected ? Theme.accent : Theme.muted
                                    implicitWidth: 32
                                    implicitHeight: 32
                                    ToolTip.text: qsTr("Open supernode portal")
                                    ToolTip.visible: hovered
                                    onClicked: {
                                        console.log("[portal] Portal button clicked node_id=" + model.node_id)
                                        backend.openNodePortal(model.node_id)
                                    }
                                }
                            }

                            // Right-click context menu
                            MouseArea {
                                anchors.fill: parent
                                acceptedButtons: Qt.RightButton
                                onClicked: (mouse) => {
                                    if (mouse.button === Qt.RightButton) {
                                        nodeContextMenu.targetNodeId = model.node_id
                                        nodeContextMenu.popup()
                                    }
                                }
                            }

                            Menu {
                                id: nodeContextMenu
                                property string targetNodeId: ""
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
                            }
                        }
                    }
                }                // end Nodes ColumnLayout
            }                    // end sidebar StackLayout

            // ── Settings section navigation ──────────────────────────────────
            // Replaces the peer/room/nodes area when Settings is the active nav.
            ColumnLayout {
                visible: navIndex === 2
                Layout.fillWidth: true
                Layout.fillHeight: true
                spacing: 0

                Rectangle {
                    Layout.fillWidth: true
                    height: 36
                    color: "#2B2D31"
                    Label {
                        anchors.centerIn: parent
                        text: "Settings"
                        color: "#DCDDDE"
                        font.pixelSize: 12
                        font.bold: true
                    }
                }

                ListView {
                    id: settingsNavList
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    model: ["Audio", "Identity", "General", "AI",
                            "Network", "Security", "Privacy", "Diagnostics"]
                    delegate: ItemDelegate {
                        id: _sNavItem
                        width: settingsNavList.width
                        height: 36
                        highlighted: index === settingsTab
                        background: Rectangle {
                            color: _sNavItem.highlighted ? "#5865F2"
                                 : _sNavItem.hovered ? "#36393F"
                                 : "transparent"
                        }
                        contentItem: Label {
                            leftPadding: 16
                            text: modelData
                            color: _sNavItem.highlighted ? "#FFFFFF" : "#B9BBBE"
                            font.pixelSize: 12
                            font.bold: _sNavItem.highlighted
                            verticalAlignment: Text.AlignVCenter
                        }
                        onClicked: settingsTab = index
                    }
                }

                Rectangle {
                    Layout.fillWidth: true
                    height: 44
                    color: Theme.bg0
                    Button {
                        anchors.centerIn: parent
                        text: "Save Settings"
                        flat: true
                        font.pixelSize: 12
                        Material.foreground: Theme.accent
                        onClicked: if (settingsModel) settingsModel.save()
                    }
                }
            }

            // Bottom nav: Chat | Settings (Room join moved to Rooms tab)
            Rectangle {
                Layout.fillWidth: true
                height: 44
                color: Theme.bg0

                RowLayout {
                    anchors.fill: parent
                    anchors.margins: 4
                    spacing: 2

                    ToolButton {
                        Layout.fillWidth: true
                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/speech.svg"
                        icon.width: 20
                        icon.height: 20
                        ToolTip.text: "Chat"
                        ToolTip.visible: hovered
                        highlighted: navIndex === 0
                        onClicked: { navIndex = 0; backend.clearUnread() }
                    }
                    ToolButton {
                        Layout.fillWidth: true
                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/gear.svg"
                        icon.width: 20
                        icon.height: 20
                        ToolTip.text: "Settings"
                        ToolTip.visible: hovered
                        highlighted: navIndex === 2
                        onClicked: navIndex = 2
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
                onStartCall: (peerId) => backend.startCall(peerId)
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
                : (chatPanel.selectedPeerId || "")
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
