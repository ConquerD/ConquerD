import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    signal leaveRoom()
    signal openAttachment(string path)
    // Start voice for the room currently shown. MainWindow owns the
    // joinRoomWithVoice call and the voice-rail bookkeeping.
    signal joinVoiceRequested()

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

    // True when voice is already live for *this* room, so the header's Join
    // Voice control hides instead of re-joining what you are already in.
    property bool voiceActiveHere: false

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
        var incomingId = msg.msg_id || ""
        if (incomingId !== "") {
            for (var i = 0; i < roomChatModel.count; i++) {
                if (roomChatModel.get(i).msgId === incomingId)
                    return
            }
        }
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

    /// Drop a message from the visible room history.
    ///
    /// Driven by the backend's `messageDeleted` confirmation, so the row only
    /// disappears once it is actually gone from the store — otherwise a failed
    /// delete would leave the UI and the history disagreeing.
    function removeMessage(msgId) {
        if (!msgId)
            return
        for (var i = 0; i < roomChatModel.count; i++) {
            if (roomChatModel.get(i).msgId === msgId) {
                roomChatModel.remove(i)
                return
            }
        }
    }

    function updateAttachment(msgId, path, sizeStr) {
        if (!msgId)
            return
        for (var i = 0; i < roomChatModel.count; i++) {
            if (roomChatModel.get(i).msgId === msgId) {
                roomChatModel.setProperty(i, "attachmentPath", path || "")
                if (sizeStr)
                    roomChatModel.setProperty(i, "sizeStr", sizeStr)
                return
            }
        }
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

                // Join Voice — the in-panel equivalent of double-clicking the
                // room in the sidebar. Without it, browsing a room's chat gave
                // you no way to start talking except going back to the sidebar.
                // Hidden once voice is live here (you are already in), and
                // while no room is actually open.
                Rectangle {
                    id: joinVoiceButton
                    Layout.alignment: Qt.AlignVCenter
                    implicitHeight: 28
                    implicitWidth: joinVoiceRow.implicitWidth + Theme.spacingSm * 2
                    radius: Theme.radiusPill
                    color: joinVoiceHover.hovered ? Theme.bg3 : "transparent"
                    border.color: Theme.bg3
                    border.width: 1
                    visible: !root.voiceActiveHere && root.roomId !== "" && root.supernodeId !== ""

                    RowLayout {
                        id: joinVoiceRow
                        anchors.centerIn: parent
                        spacing: Theme.spacingXs

                        Image {
                            source: "qrc:/qt/qml/ConquerD/Client/icons/phone.svg"
                            sourceSize.width: 14
                            sourceSize.height: 14
                            width: 14
                            height: 14
                            fillMode: Image.PreserveAspectFit
                            opacity: 0.85
                        }

                        Text {
                            text: qsTr("Join Voice")
                            color: Theme.text
                            font.pixelSize: Theme.fontSizeCaption
                        }
                    }

                    HoverHandler { id: joinVoiceHover }
                    TapHandler { onTapped: root.joinVoiceRequested() }

                    ToolTip.text: qsTr("Join this room's voice channel")
                    ToolTip.visible: joinVoiceHover.hovered
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
                fileTransferModel: root.fileTransferModel
                isRoom: true
                inlinePreviewEnabled: root.youtubePreviewEnabled
                inlinePreviewAck: root.youtubeInlineAck
                // Room history is client-side only — the supernode persists no
                // messages — so this copy is entirely the user's to trim,
                // exactly as in 1:1 chat. Deliberately not limited to your own
                // messages: deleting someone else's only removes it from *your*
                // local history, it does not reach them. For a file you sent,
                // deleting also revokes the share (see `delete_message`).
                allowDelete: true
                onDeleteRequested: (id) => backend.deleteMessage(id)
                onInlineAckAccepted: {
                    root.youtubeInlineAck = true
                    if (root.settingsModel) {
                        root.settingsModel.youtube_inline_ack = true
                        root.settingsModel.save()
                    }
                }
                onCopyRequested: (text) => backend.copyToClipboard(text)
                onOpenAttachmentRequested: (path) => root.openAttachment(path)
                onTransferAcceptRequested: (id) => backend.acceptRoomFile(id)
                onTransferRejectRequested: (id) => backend.declineRoomFile(id)
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
