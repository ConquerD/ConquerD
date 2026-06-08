// IncomingCallDialog.qml — Shown when a peer requests a voice call.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root
    visible: false
    width: 320
    height: 240

    property string callPeerId: ""

    signal accepted(string peerId)
    signal rejected(string peerId)

    Timer {
        id: dismissTimer
        interval: 30000
        running: root.visible
        onTriggered: root.dismiss(false)
    }

    function show(peerId) {
        root.callPeerId = peerId
        root.visible = true
    }

    function dismiss(wasAccepted) {
        root.visible = false
        dismissTimer.stop()
        if (wasAccepted) {
            root.accepted(root.callPeerId)
        } else {
            root.rejected(root.callPeerId)
        }
        root.callPeerId = ""
    }

    Rectangle {
        anchors.fill: parent
        color: Theme.overlayScrim
        opacity: 0.72
    }

    Rectangle {
        anchors.centerIn: parent
        width: 320
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

            Avatar {
                Layout.alignment: Qt.AlignHCenter
                peerId: root.callPeerId
                size: 56
                showRing: true
            }

            Label {
                Layout.fillWidth: true
                text: "Incoming call"
                font.pixelSize: Theme.fontSizeTitle
                font.bold: true
                color: Theme.text
                horizontalAlignment: Text.AlignHCenter
            }

            Label {
                Layout.fillWidth: true
                text: root.callPeerId || "Unknown"
                font.pixelSize: Theme.fontSizeBody
                color: Theme.muted
                elide: Text.ElideMiddle
                horizontalAlignment: Text.AlignHCenter
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingMd

                StyledButton {
                    Layout.fillWidth: true
                    text: "Decline"
                    danger: true
                    onClicked: root.dismiss(false)
                }

                StyledButton {
                    Layout.fillWidth: true
                    text: "Accept"
                    success: true
                    onClicked: root.dismiss(true)
                }
            }
        }
    }
}