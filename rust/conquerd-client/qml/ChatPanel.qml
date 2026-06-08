import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    signal sendMessage(string peerId, string message)
    signal startCall(string peerId)
    signal sendFile(string peerId, string filePath)

    property string selectedPeerId: ""
    property string selectedPeerName: ""
    property var chatModel: null
    property var fileTransferModel: null
    property string typingPeerId: ""
    property bool peerIsTyping: false
    property bool youtubePreviewEnabled: true
    property var settingsModel: null
    property bool youtubeInlineAck: false
    property string _searchText: ""
    property bool _searchOpen: false
    property bool aiStreaming: false
    property string aiResponse: ""
    property string aiError: ""
    property int _aiSeq: 0
    property string _currentAiRequestId: ""
    property bool _statsPanelOpen: false

    Connections {
        target: backend
        function onConnectionStats(json) {
            try {
                var stats = JSON.parse(json)
                if (stats.peer_id !== root.selectedPeerId)
                    return
                connStatsPanel.applyStats(json)
                connStatsPanel.isRelay = backend.connection_mode === "relay"
            } catch (e) {}
        }
        function onOllamaChunk(requestId, text) {
            if (requestId === root._currentAiRequestId) root.aiResponse += text
        }
        function onOllamaDone(requestId) {
            if (requestId === root._currentAiRequestId) root.aiStreaming = false
        }
        function onOllamaError(requestId, error) {
            if (requestId === root._currentAiRequestId) {
                root.aiStreaming = false
                root.aiError = error
            }
        }
    }

    Timer {
        id: typingClearTimer
        interval: 4000
        onTriggered: {
            root.peerIsTyping = false
            root.typingPeerId = ""
        }
    }

    onSelectedPeerIdChanged: {
        root._statsPanelOpen = false
        connStatsPanel.applyStats(JSON.stringify({
            peer_id: root.selectedPeerId,
            rtt_ms: 0,
            packet_loss_pct: 0,
            jitter_ms: 0,
            relay: backend.connection_mode === "relay",
            bandwidth_kbps: 0
        }))
    }

    StatsPanel {
        id: connStatsPanel
        z: 60
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.margins: Theme.spacingMd
        anchors.topMargin: Theme.touchTarget + Theme.spacingMd + Theme.spacingXs
        visible: root._statsPanelOpen && root.selectedPeerId !== ""
    }

    MouseArea {
        z: 55
        anchors.fill: parent
        visible: root._statsPanelOpen
        onClicked: root._statsPanelOpen = false
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        Rectangle {
            Layout.fillWidth: true
            height: Theme.touchTarget + Theme.spacingXs
            color: Theme.bg2

            RowLayout {
                anchors.verticalCenter: parent.verticalCenter
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.leftMargin: Theme.spacingMd
                anchors.rightMargin: Theme.spacingMd
                spacing: Theme.spacingSm

                Avatar {
                    visible: root.selectedPeerId !== ""
                    peerId: root.selectedPeerId
                    size: 28
                    showRing: true
                    Layout.alignment: Qt.AlignVCenter
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 1

                    Text {
                        text: root.selectedPeerName || root.selectedPeerId || "No peer selected"
                        color: Theme.text
                        font.pixelSize: Theme.fontSizeTitle
                        font.bold: true
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }

                    Text {
                        text: root.selectedPeerId === ""
                            ? "Choose a peer to begin"
                            : "Private trusted-peer conversation"
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                }

                ConnectionStatsChip {
                    id: statsChip
                    Layout.alignment: Qt.AlignVCenter
                    peerId: root.selectedPeerId
                    rttMs: connStatsPanel.rttMs
                    packetLossPct: connStatsPanel.packetLossPct
                    isRelay: backend.connection_mode === "relay"
                    expanded: root._statsPanelOpen
                    onToggleExpanded: root._statsPanelOpen = !root._statsPanelOpen
                }

                ToolButton {
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/search.svg"
                    icon.width: 18
                    icon.height: 18
                    visible: root.selectedPeerId !== ""
                    flat: true
                    ToolTip.text: "Search messages"
                    ToolTip.visible: hovered
                    onClicked: {
                        root._searchOpen = !root._searchOpen
                        if (root._searchOpen) msgSearchField.forceActiveFocus()
                        else {
                            root._searchText = ""
                            msgSearchField.clear()
                        }
                    }
                }

                Button {
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/phone.svg"
                    icon.width: 18
                    icon.height: 18
                    visible: root.selectedPeerId !== ""
                    flat: true
                    onClicked: root.startCall(root.selectedPeerId)
                }
            }
        }

        Rectangle {
            Layout.fillWidth: true
            height: root._searchOpen ? Theme.controlHeight + Theme.spacingXs : 0
            clip: true
            color: Theme.bg3
            Behavior on height { NumberAnimation { duration: Theme.animFast } }

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: Theme.spacingMd
                anchors.rightMargin: Theme.spacingSm
                spacing: Theme.spacingXs

                StyledTextField {
                    id: msgSearchField
                    Layout.fillWidth: true
                    placeholderText: "Search messages..."
                    onTextChanged: root._searchText = text.toLowerCase()
                    Keys.onEscapePressed: {
                        root._searchOpen = false
                        root._searchText = ""
                        clear()
                    }
                }

                ToolButton {
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                    icon.width: 12
                    icon.height: 12
                    icon.color: Theme.muted
                    flat: true
                    onClicked: {
                        root._searchOpen = false
                        root._searchText = ""
                        msgSearchField.clear()
                    }
                }
            }
        }

        Shortcut {
            sequence: "Ctrl+F"
            enabled: root.selectedPeerId !== ""
            onActivated: {
                root._searchOpen = !root._searchOpen
                if (root._searchOpen) msgSearchField.forceActiveFocus()
                else {
                    root._searchText = ""
                    msgSearchField.clear()
                }
            }
        }

        ListView {
            id: msgList
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: root.chatModel
            verticalLayoutDirection: ListView.BottomToTop
            spacing: 2
            onCountChanged: Qt.callLater(function() { msgList.positionViewAtBeginning() })

            ColumnLayout {
                anchors.centerIn: parent
                width: Math.min(parent.width - 48, 260)
                spacing: 10
                visible: root.selectedPeerId === "" || msgList.count === 0

                Image {
                    source: root.selectedPeerId === ""
                        ? "qrc:/qt/qml/ConquerD/Client/icons/speech.svg"
                        : "qrc:/qt/qml/ConquerD/Client/icons/send.svg"
                    sourceSize.width: 40
                    sourceSize.height: 40
                    Layout.preferredWidth: 40
                    Layout.preferredHeight: 40
                    Layout.alignment: Qt.AlignHCenter
                    fillMode: Image.PreserveAspectFit
                    opacity: 0.72
                }

                Text {
                    text: root.selectedPeerId === "" ? "Select a peer" : "No messages yet"
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeTitle
                    font.bold: true
                    horizontalAlignment: Text.AlignHCenter
                    Layout.fillWidth: true
                }

                Text {
                    text: root.selectedPeerId === ""
                        ? "Choose a trusted peer from the left rail to open a private conversation."
                        : "Send a message or attach a file to start this conversation."
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeCaption
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                    lineHeight: 1.3
                    Layout.fillWidth: true
                }
            }

            delegate: ChatRichMessageDelegate {
                property bool searchMatch: root._searchText === "" ||
                    (model.body || "").toLowerCase().indexOf(root._searchText) !== -1
                visible: searchMatch
                height: searchMatch ? implicitHeight : 0
                clip: true
                msgId: model.msgId || ""
                sender: model.sender || ""
                body: model.body || ""
                kind: model.kind || "text"
                mine: !!model.mine
                timestamp: model.timestamp || 0
                status: model.status || "delivered"
                inlinePreviewEnabled: root.youtubePreviewEnabled
                inlinePreviewAck: root.youtubeInlineAck
                allowDelete: true
                onInlineAckAccepted: {
                    root.youtubeInlineAck = true
                    if (root.settingsModel) {
                        root.settingsModel.youtube_inline_ack = true
                        root.settingsModel.save()
                    }
                }
                onCopyRequested: (text) => backend.copyToClipboard(text)
                onDeleteRequested: (id) => backend.deleteMessage(id)
                onRetryRequested: (id) => backend.retryMessage(id)
            }
        }

        Column {
            id: transferList
            Layout.fillWidth: true
            Layout.margins: 8
            spacing: 4
            visible: fileTransferRepeater.count > 0

            Repeater {
                id: fileTransferRepeater
                model: root.fileTransferModel ? root.fileTransferModel : null

                delegate: Rectangle {
                    id: chip
                    required property string transferId
                    required property string peerId
                    required property string relPath
                    required property double progress
                    required property string state
                    required property bool isSelf

                    visible: chip.peerId === root.selectedPeerId
                    width: transferList.width
                    height: visible ? chipLayout.implicitHeight + 12 : 0
                    clip: true
                    color: Theme.bg2
                    radius: 0
                    border.color: Theme.bg3
                    border.width: 1

                    ColumnLayout {
                        id: chipLayout
                        anchors { left: parent.left; right: parent.right; margins: 10 }
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 4

                        RowLayout {
                            spacing: 6
                            Text {
                                text: chip.isSelf ? "up" : "down"
                                color: Theme.muted
                                font.pixelSize: Theme.fontSizeCaption
                            }
                            Text {
                                text: chip.relPath
                                color: Theme.text
                                font.pixelSize: Theme.fontSizeBody
                                elide: Text.ElideMiddle
                                Layout.fillWidth: true
                            }
                            ToolButton {
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/check.svg"
                                icon.width: 14
                                icon.height: 14
                                icon.color: Theme.online
                                visible: !chip.isSelf && chip.state === "pending"
                                implicitWidth: 28
                                implicitHeight: 28
                                ToolTip.text: "Accept"
                                ToolTip.visible: hovered
                                onClicked: backend.acceptFile(chip.transferId)
                            }
                            ToolButton {
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/x-circle.svg"
                                icon.width: 14
                                icon.height: 14
                                icon.color: Theme.danger
                                visible: chip.state === "pending" || chip.state === "active"
                                implicitWidth: 28
                                implicitHeight: 28
                                ToolTip.text: chip.isSelf ? "Cancel" : "Reject"
                                ToolTip.visible: hovered
                                onClicked: backend.rejectFile(chip.transferId)
                            }
                            ToolButton {
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                                icon.width: 12
                                icon.height: 12
                                icon.color: Theme.muted
                                visible: chip.state === "done" || chip.state === "failed"
                                implicitWidth: 24
                                implicitHeight: 24
                                ToolTip.text: "Dismiss"
                                ToolTip.visible: hovered
                                onClicked: root.fileTransferModel && root.fileTransferModel.removeTransfer(chip.transferId)
                            }
                        }

                        ProgressBar {
                            Layout.fillWidth: true
                            from: 0.0
                            to: 1.0
                            value: chip.progress
                            visible: chip.state === "active"
                        }

                        Text {
                            visible: chip.state === "done" || chip.state === "failed"
                            text: chip.state === "done" ? "Complete" : "Failed"
                            color: chip.state === "done" ? Theme.online : Theme.danger
                            font.pixelSize: Theme.fontSizeCaption
                        }
                    }
                }
            }
        }

        Item {
            Layout.fillWidth: true
            height: root.peerIsTyping && root.typingPeerId === root.selectedPeerId ? 22 : 0
            clip: true
            visible: height > 0
            Behavior on height { NumberAnimation { duration: 150 } }

            Text {
                anchors { left: parent.left; leftMargin: 14; verticalCenter: parent.verticalCenter }
                text: (root.selectedPeerName || root.typingPeerId) + " is typing..."
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                font.italic: true
            }
        }

        Rectangle {
            id: aiPanel
            Layout.fillWidth: true
            Layout.margins: Theme.spacingSm
            implicitHeight: aiContent.implicitHeight + 16
            visible: root.aiResponse !== "" || root.aiStreaming || root.aiError !== ""
            color: Theme.bg1
            radius: 0
            border.color: Theme.accent
            border.width: 1

            ColumnLayout {
                id: aiContent
                anchors { left: parent.left; right: parent.right; margins: 10 }
                anchors.verticalCenter: parent.verticalCenter
                spacing: 6

                RowLayout {
                    spacing: 6
                    Text {
                        text: "AI"
                        color: Theme.accent
                        font.pixelSize: Theme.fontSizeCaption
                        font.bold: true
                    }
                    Item { Layout.fillWidth: true }
                    StyledButton {
                        text: "Cancel"
                        visible: root.aiStreaming
                        compact: true
                        onClicked: {
                            backend.cancelOllama(root._currentAiRequestId)
                            root.aiStreaming = false
                        }
                    }
                    ToolButton {
                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                        icon.width: 12
                        icon.height: 12
                        icon.color: Theme.muted
                        visible: !root.aiStreaming
                        flat: true
                        onClicked: {
                            root.aiResponse = ""
                            root.aiError = ""
                            root._currentAiRequestId = ""
                        }
                    }
                }

                Text {
                    visible: root.aiError !== ""
                    text: "Error: " + root.aiError
                    color: Theme.danger
                    font.pixelSize: Theme.fontSizeBody
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }

                Text {
                    visible: root.aiResponse !== ""
                    text: root.aiResponse
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeBody
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }

                Text {
                    visible: root.aiStreaming && root.aiResponse === ""
                    text: "..."
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeBody
                    font.italic: true
                }
            }
        }

        RichChatComposer {
            Layout.margins: Theme.spacingSm
            targetName: root.selectedPeerName || root.selectedPeerId
            enabledForTarget: root.selectedPeerId !== ""
            fileTransferEnabled: true
            aiEnabled: backend.ollama_available
            aiStreaming: root.aiStreaming
            onComposing: function(active) {
                if (root.selectedPeerId !== "") backend.sendTyping(root.selectedPeerId, active)
            }
            onSendMessage: function(message) {
                root.sendMessage(root.selectedPeerId, message)
                if (root.selectedPeerId !== "") backend.sendTyping(root.selectedPeerId, false)
            }
            onSendFile: function(fileUrl) {
                if (root.selectedPeerId !== "") root.sendFile(root.selectedPeerId, fileUrl)
            }
            onAskAi: function(prompt) {
                root._aiSeq += 1
                root._currentAiRequestId = "chat-" + root._aiSeq
                root.aiResponse = ""
                root.aiError = ""
                root.aiStreaming = true
                var sysPrompt = root.selectedPeerName !== ""
                    ? "You are helping " + root.selectedPeerName + " in a secure peer-to-peer chat."
                    : "You are an assistant in a secure peer-to-peer chat application."
                backend.askOllama(root._currentAiRequestId, prompt, sysPrompt)
                if (root.selectedPeerId !== "") backend.sendTyping(root.selectedPeerId, false)
            }
        }
    }
}
