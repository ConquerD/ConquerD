import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    signal leaveRoom()
    signal openAttachment(string path)

    property string roomName: "Room"
    property string roomId: ""
    property string supernodeId: ""
    property int participantCount: 0
    property var roomModel: null
    property var fileTransferModel: null
    property var settingsModel: null
    property bool youtubePreviewEnabled: true
    property bool youtubeInlineAck: false

    // Members sidebar (who is in this text room + their presence).
    property bool membersOpen: true

    onRoomModelChanged: {
        participantCount = roomModel ? roomModel.participantCount() : 0
    }

    Connections {
        target: root.roomModel
        ignoreUnknownSignals: true
        function onRowsInserted() { root.participantCount = root.roomModel ? root.roomModel.participantCount() : 0 }
        function onRowsRemoved()  { root.participantCount = root.roomModel ? root.roomModel.participantCount() : 0 }
        function onModelReset()   { root.participantCount = root.roomModel ? root.roomModel.participantCount() : 0 }
        function onRowsMoved()    { root.participantCount = root.roomModel ? root.roomModel.participantCount() : 0 }
    }

    function appendRoomChat(msgJson) {
        var msg = JSON.parse(msgJson)
        // Drop messages for other rooms (multi-room chat stays subscribed).
        var msgRoom = msg.room_id || ""
        var msgSn = msg.supernode_id || ""
        if (msgRoom !== "" && root.roomId !== "" && msgRoom !== root.roomId)
            return
        if (msgSn !== "" && root.supernodeId !== "" && msgSn !== root.supernodeId)
            return
        roomChatModel.append({
            "msgId": msg.msg_id || "",
            "sender": msg.sender || "",
            "senderPeerId": msg.sender_id || "",
            "body": msg.body || "",
            "timestamp": msg.timestamp || 0,
            "kind": msg.kind || "text",
            "mine": msg.mine || false,
            "status": msg.status || "delivered",
            "attachmentName": msg.attachment_name || "",
            "attachmentPath": msg.attachment_path || "",
            "sizeStr": msg.size_str || ""
        })
    }

    function switchToRoom(name, roomId, supernodeId) {
        var newName = name || "Room"
        var newSn = supernodeId || ""
        var newRid = roomId || ""
        var roomChanged = root.roomId !== newRid || root.supernodeId !== newSn
        if (roomChanged) {
            roomChatModel.clear()
            root.roomName = newName
            root.roomId = newRid
            root.supernodeId = newSn
            if (newRid !== "" && newSn !== "" && backend)
                backend.loadRoomChatHistory(newSn, newRid)
        } else {
            root.roomName = newName
            root.roomId = newRid || root.roomId
            root.supernodeId = newSn || root.supernodeId
        }
    }

    ListModel { id: roomChatModel }

    RowLayout {
        anchors.fill: parent
        spacing: 0

    ColumnLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        spacing: 0

        Rectangle {
            Layout.fillWidth: true
            height: Theme.touchTarget + Theme.spacingXs
            color: Theme.bg2

            RowLayout {
                anchors.fill: parent
                anchors.margins: Theme.spacingMd
                spacing: Theme.spacingSm

                Text {
                    text: root.roomName
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeTitle
                    font.bold: true
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                }

                // Members toggle — shows the room population and opens/closes
                // the presence sidebar.
                Rectangle {
                    Layout.alignment: Qt.AlignVCenter
                    implicitHeight: 28
                    implicitWidth: membersToggleRow.implicitWidth + Theme.spacingSm * 2
                    radius: Theme.radiusPill
                    color: root.membersOpen ? Theme.selectedFill()
                         : (membersToggleHover.hovered ? Theme.bg3 : "transparent")
                    visible: root.participantCount > 0

                    RowLayout {
                        id: membersToggleRow
                        anchors.centerIn: parent
                        spacing: Theme.spacingXs

                        Image {
                            source: "qrc:/qt/qml/ConquerD/Client/icons/peers.svg"
                            sourceSize.width: 15
                            sourceSize.height: 15
                            width: 15
                            height: 15
                            fillMode: Image.PreserveAspectFit
                            opacity: 0.85
                        }

                        Text {
                            text: root.participantCount
                            color: Theme.text
                            font.pixelSize: Theme.fontSizeCaption
                            font.bold: true
                        }
                    }

                    HoverHandler { id: membersToggleHover }
                    TapHandler { onTapped: root.membersOpen = !root.membersOpen }

                    ToolTip.text: root.membersOpen ? "Hide members" : "Show members"
                    ToolTip.visible: membersToggleHover.hovered
                }
            }
        }

        ListView {
            id: roomChat
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: roomChatModel
            spacing: 2
            onCountChanged: Qt.callLater(function() { roomChat.positionViewAtEnd() })

            EmptyState {
                anchors.centerIn: parent
                visible: roomChatModel.count === 0
                width: Math.min(parent.width - Theme.spacingXl, 200)
                iconSource: "qrc:/qt/qml/ConquerD/Client/icons/speech.svg"
                iconSize: 36
                title: "Room chat"
                subtitle: "Messages from room members appear here."
            }

            delegate: ChatRichMessageDelegate {
                msgId: model.msgId || ""
                sender: model.sender || ""
                senderPeerId: model.senderPeerId || ""
                body: model.body || ""
                kind: model.kind || "text"
                mine: !!model.mine
                timestamp: model.timestamp || 0
                status: model.status || "delivered"
                attachmentName: model.attachmentName || ""
                attachmentPath: model.attachmentPath || ""
                sizeStr: model.sizeStr || ""
                isRoom: true
                inlinePreviewEnabled: root.youtubePreviewEnabled
                inlinePreviewAck: root.youtubeInlineAck
                allowDelete: false
                onInlineAckAccepted: {
                    root.youtubeInlineAck = true
                    if (root.settingsModel) {
                        root.settingsModel.youtube_inline_ack = true
                        root.settingsModel.save()
                    }
                }
                onCopyRequested: (text) => backend.copyToClipboard(text)
                onOpenAttachmentRequested: (path) => root.openAttachment(path)
            }
        }

        Column {
            id: transferList
            Layout.fillWidth: true
            Layout.margins: 6
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
                    required property string purpose

                    visible: chip.peerId === root.roomId && chip.purpose === "room_file"
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
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                                icon.width: 12
                                icon.height: 12
                                icon.color: Theme.muted
                                visible: chip.state === "done" || chip.state === "failed"
                                flat: true
                                onClicked: root.fileTransferModel && root.fileTransferModel.removeTransfer(chip.transferId)
                            }
                        }

                        ProgressBar {
                            Layout.fillWidth: true
                            from: 0.0
                            to: 1.0
                            value: chip.progress
                            visible: chip.state === "active" || chip.state === "pending"
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

        RichChatComposer {
            Layout.margins: 6
            targetName: root.roomName
            enabledForTarget: root.roomName !== ""
            fileTransferEnabled: root.roomId !== ""
            fileTransferTooltip: "Attach file"
            onSendMessage: function(message) {
                backend.sendRoomChat(message)
            }
            onSendFile: function(fileUrl) {
                backend.sendRoomFile(fileUrl)
            }
        }
    }  // end chat ColumnLayout

        // ── Members sidebar ───────────────────────────────────────────────
        // Live roster of who is in this text room, grouped by presence.
        Rectangle {
            id: membersPanel
            Layout.fillHeight: true
            Layout.preferredWidth: root.membersOpen && root.participantCount > 0 ? 190 : 0
            visible: Layout.preferredWidth > 0
            clip: true
            color: Theme.bg1

            Behavior on Layout.preferredWidth {
                NumberAnimation { duration: Theme.animFast; easing.type: Easing.InOutQuad }
            }

            // Left separator
            Rectangle {
                anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
                width: 1
                color: Theme.divider
            }

            ColumnLayout {
                anchors { fill: parent; leftMargin: 1 }
                spacing: 0

                Rectangle {
                    Layout.fillWidth: true
                    height: Theme.touchTarget + Theme.spacingXs
                    color: Theme.bg2

                    Text {
                        anchors {
                            verticalCenter: parent.verticalCenter
                            left: parent.left
                            leftMargin: Theme.spacingMd
                        }
                        text: "Members (" + root.participantCount + ")"
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                        font.capitalization: Font.AllUppercase
                        font.letterSpacing: 1.2
                        font.bold: true
                    }
                }

                ListView {
                    id: membersList
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    model: root.roomModel

                    // Present members first, absent (offline) below.
                    section.property: "online"
                    section.criteria: ViewSection.FullString
                    section.delegate: Rectangle {
                        width: membersList.width
                        height: 20
                        color: "transparent"

                        Text {
                            anchors {
                                verticalCenter: parent.verticalCenter
                                left: parent.left
                                leftMargin: Theme.spacingMd
                            }
                            text: (section === "true" ? "Online" : "Offline")
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeMicro
                            font.capitalization: Font.AllUppercase
                            font.letterSpacing: 1.0
                            font.bold: true
                        }
                    }

                    delegate: Item {
                        id: memberRow
                        width: ListView.view ? ListView.view.width : 0
                        height: 44

                        required property string peerId
                        required property string handle
                        required property bool isSelf
                        required property bool online

                        RowLayout {
                            anchors {
                                fill: parent
                                leftMargin: Theme.spacingMd
                                rightMargin: Theme.spacingSm
                            }
                            spacing: Theme.spacingSm

                            Avatar {
                                peerId: memberRow.peerId
                                size: 28
                                showRing: true
                                ringColor: memberRow.online ? Theme.online : tintColor
                                Layout.alignment: Qt.AlignVCenter
                            }

                            Text {
                                text: memberRow.isSelf
                                    ? ((memberRow.handle || memberRow.peerId) + " (you)")
                                    : (memberRow.handle || memberRow.peerId)
                                color: Theme.text
                                font.pixelSize: Theme.fontSizeBody
                                elide: Text.ElideRight
                                Layout.fillWidth: true
                            }

                            // Presence dot
                            Rectangle {
                                width: 8
                                height: 8
                                radius: 4
                                color: memberRow.online ? Theme.online : Theme.muted
                                Layout.alignment: Qt.AlignVCenter
                            }
                        }
                    }

                    EmptyState {
                        anchors.centerIn: parent
                        visible: membersList.count === 0
                        width: Math.min(parent.width - Theme.spacingLg, 150)
                        iconSource: "qrc:/qt/qml/ConquerD/Client/icons/peers.svg"
                        iconSize: 28
                        title: "No one else here"
                        subtitle: "Members appear as they join."
                    }
                }
            }
        }
    }  // end RowLayout
}
