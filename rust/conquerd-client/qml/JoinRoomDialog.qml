// JoinRoomDialog.qml — Dialog for joining an SFU voice room on a supernode.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts

Rectangle {
    id: root
    visible: false
    color: "#2F3136"
    radius: 0
    width: 380
    height: 210

    signal joinRequested(string supernodeId, string roomId)

    function show() {
        supernodeField.text = ""
        roomField.text = ""
        errorLabel.text = ""
        root.visible = true
        supernodeField.forceActiveFocus()
    }

    function dismiss() {
        root.visible = false
    }

    // Keyboard: Escape to cancel
    Keys.onEscapePressed: root.dismiss()

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 20
        spacing: 12

        Label {
            text: "Join Voice Room"
            font.pixelSize: 15
            font.bold: true
            color: "#FFFFFF"
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8
            Label {
                text: "Supernode:"
                color: "#B9BBBE"
                Layout.preferredWidth: 90
            }
            TextField {
                id: supernodeField
                Layout.fillWidth: true
                placeholderText: "supernode_id or host:port"
                color: "#DCDDDE"
                background: Rectangle { color: "#383A40"; radius: 0 }
                Keys.onReturnPressed: roomField.forceActiveFocus()
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8
            Label {
                text: "Room:"
                color: "#B9BBBE"
                Layout.preferredWidth: 90
            }
            TextField {
                id: roomField
                Layout.fillWidth: true
                placeholderText: "room name or ID"
                color: "#DCDDDE"
                background: Rectangle { color: "#383A40"; radius: 0 }
                Keys.onReturnPressed: doJoin()
            }
        }

        Label {
            id: errorLabel
            text: ""
            color: "#ED4245"
            font.pixelSize: 11
            visible: text.length > 0
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Button {
                Layout.fillWidth: true
                text: "Cancel"
                flat: true
                onClicked: root.dismiss()
            }

            Button {
                Layout.fillWidth: true
                text: "Join Room"
                Material.background: Material.Blue
                onClicked: doJoin()
            }
        }
    }

    function doJoin() {
        const sn = supernodeField.text.trim()
        const room = roomField.text.trim()
        if (sn.length === 0) {
            errorLabel.text = "Supernode ID is required."
            return
        }
        if (room.length === 0) {
            errorLabel.text = "Room name is required."
            return
        }
        root.visible = false
        root.joinRequested(sn, room)
    }
}
