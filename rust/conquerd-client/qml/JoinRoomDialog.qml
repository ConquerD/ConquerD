// JoinRoomDialog.qml — Dialog for joining an SFU voice room on a supernode.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root
    visible: false
    focus: visible
    width: 400
    height: 320

    signal joinRequested(string supernodeId, string roomId, string inviteToken)

    function show() {
        supernodeField.text = ""
        roomField.text = ""
        inviteTokenField.text = ""
        errorLabel.text = ""
        root.visible = true
        supernodeField.forceActiveFocus()
    }

    function dismiss() {
        root.visible = false
    }

    Keys.onEscapePressed: root.dismiss()

    Rectangle {
        anchors.fill: parent
        color: Theme.overlayScrim
        opacity: 0.72
    }

    Rectangle {
        anchors.centerIn: parent
        width: 380
        height: cardColumn.implicitHeight + Theme.spacingXl * 2
        color: Theme.bg2
        radius: Theme.radiusMd
        border.color: Theme.border
        border.width: 1

        ColumnLayout {
            id: cardColumn
            anchors {
                fill: parent
                margins: Theme.spacingXl
            }
            spacing: Theme.spacingMd

            Label {
                text: "Join Voice Room"
                font.pixelSize: Theme.fontSizeTitle
                font.bold: true
                color: Theme.text
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingSm

                Label {
                    text: "Supernode"
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeBody
                    Layout.preferredWidth: 90
                }
                StyledTextField {
                    id: supernodeField
                    Layout.fillWidth: true
                    placeholderText: "supernode_id or host:port"
                    Keys.onReturnPressed: roomField.forceActiveFocus()
                }
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingSm

                Label {
                    text: "Room"
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeBody
                    Layout.preferredWidth: 90
                }
                StyledTextField {
                    id: roomField
                    Layout.fillWidth: true
                    placeholderText: "room name or ID"
                    Keys.onReturnPressed: inviteTokenField.forceActiveFocus()
                }
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingSm

                Label {
                    text: "Invite"
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeBody
                    Layout.preferredWidth: 90
                }
                StyledTextField {
                    id: inviteTokenField
                    Layout.fillWidth: true
                    placeholderText: "optional token"
                    Keys.onReturnPressed: doJoin()
                }
            }

            Label {
                id: errorLabel
                text: ""
                color: Theme.danger
                font.pixelSize: Theme.fontSizeCaption
                visible: text.length > 0
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingSm

                StyledButton {
                    Layout.fillWidth: true
                    text: "Cancel"
                    onClicked: root.dismiss()
                }

                StyledButton {
                    Layout.fillWidth: true
                    text: "Join Room"
                    primary: true
                    onClicked: doJoin()
                }
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
        root.joinRequested(sn, room, inviteTokenField.text.trim())
    }
}
