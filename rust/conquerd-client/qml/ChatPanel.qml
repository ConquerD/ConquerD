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

    Connections {
        target: backend
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

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        Rectangle {
            Layout.fillWidth: true
            height: 48
            color: Theme.bg2

            RowLayout {
                anchors.verticalCenter: parent.verticalCenter
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.leftMargin: 12
                anchors.rightMargin: 12
                spacing: 8

                Avatar {
                    visible: root.selectedPeerId !== ""
                    peerId: root.selectedPeerId
                    size: 28
                    showRing: true
                    Layout.alignment: Qt.AlignVCenter
                }

                Text {
                    text: root.selectedPeerName || root.selectedPeerId || "No peer selected"
                    color: Theme.text
                    font.pixelSize: 14
                    font.bold: true
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                }

                ToolButton {
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/search.svg"
                    icon.width: 18
                    icon.height: 18
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
            height: root._searchOpen ? 38 : 0
            clip: true
            color: Theme.bg3
            Behavior on height { NumberAnimation { duration: 150 } }

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 10
                anchors.rightMargin: 6
                spacing: 4

                TextField {
                    id: msgSearchField
                    Layout.fillWidth: true
                    placeholderText: "Search messages..."
                    background: Item {}
                    color: Theme.text
                    font.pixelSize: 13
                    onTextChanged: root._searchText = text.toLowerCase()
                    Keys.onEscapePressed: {
                        root._searchOpen = false
                        root._searchText = ""
                        clear()
                    }
                }

                ToolButton {
                    text: "X"
                    flat: true
                    font.pixelSize: 13
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
                                font.pixelSize: 11
                            }
                            Text {
                                text: chip.relPath
                                color: Theme.text
                                font.pixelSize: 12
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
                            font.pixelSize: 11
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
                font.pixelSize: 11
                font.italic: true
            }
        }

        Rectangle {
            id: aiPanel
            Layout.fillWidth: true
            Layout.margins: 8
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
                        font.pixelSize: 11
                        font.bold: true
                    }
                    Item { Layout.fillWidth: true }
                    Button {
                        text: "Cancel"
                        visible: root.aiStreaming
                        flat: true
                        font.pixelSize: 11
                        onClicked: {
                            backend.cancelOllama(root._currentAiRequestId)
                            root.aiStreaming = false
                        }
                    }
                    Button {
                        text: "X"
                        visible: !root.aiStreaming
                        flat: true
                        font.pixelSize: 11
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
                    font.pixelSize: 12
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }

                Text {
                    visible: root.aiResponse !== ""
                    text: root.aiResponse
                    color: Theme.text
                    font.pixelSize: 13
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }

                Text {
                    visible: root.aiStreaming && root.aiResponse === ""
                    text: "..."
                    color: Theme.muted
                    font.pixelSize: 13
                    font.italic: true
                }
            }
        }

        RichChatComposer {
            Layout.margins: 8
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
