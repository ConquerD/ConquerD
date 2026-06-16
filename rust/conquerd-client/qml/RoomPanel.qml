import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    signal leaveRoom()

    property string roomName: "Room"
    property string roomId: ""
    property string supernodeId: ""
    property int participantCount: 0
    property var roomModel: null
    property var fileTransferModel: null
    property var settingsModel: null
    property bool youtubePreviewEnabled: true
    property bool youtubeInlineAck: false

    onRoomModelChanged: {
        participantCount = roomModel ? roomModel.participantCount() : 0
    }

    Connections {
        target: root.roomModel
        ignoreUnknownSignals: true
        function onRowsInserted() { root.participantCount = root.roomModel ? root.roomModel.participantCount() : 0 }
        function onRowsRemoved()  { root.participantCount = root.roomModel ? root.roomModel.participantCount() : 0 }
        function onModelReset()   { root.participantCount = 0 }
        function onRowsMoved()    { root.participantCount = root.roomModel ? root.roomModel.participantCount() : 0 }
    }

    function appendRoomChat(msgJson) {
        var msg = JSON.parse(msgJson)
        roomChatModel.append({
            "msgId": msg.msg_id || "",
            "sender": msg.sender || "",
            "body": msg.body || "",
            "timestamp": msg.timestamp || 0,
            "kind": msg.kind || "text",
            "mine": msg.mine || false,
            "status": msg.status || "delivered"
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

    ColumnLayout {
        anchors.fill: parent
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

                Text {
                    text: root.participantCount > 0 ? root.participantCount + " in room" : ""
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeCaption
                    visible: root.participantCount > 0
                }
            }
        }

        ListView {
            id: roomChat
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: roomChatModel
            verticalLayoutDirection: ListView.BottomToTop
            spacing: 2
            onCountChanged: Qt.callLater(function() { roomChat.positionViewAtBeginning() })

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
                body: model.body || ""
                kind: model.kind || "text"
                mine: !!model.mine
                timestamp: model.timestamp || 0
                status: model.status || "delivered"
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
    }
}
