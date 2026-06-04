// IncomingCallDialog.qml — Shown when a peer requests a voice call.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Rectangle {
    id: root
    visible: false
    color: "#2F3136"
    radius: 0
    width: 320
    height: 220

    // Provided by the parent when a call comes in
    property string callPeerId: ""

    // Emitted when the user accepts/rejects
    signal accepted(string peerId)
    signal rejected(string peerId)

    // Auto-dismiss if no action within 30 seconds
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

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 20
        spacing: 12

        Avatar {
            Layout.alignment: Qt.AlignHCenter
            peerId: root.callPeerId
            size: 56
            showRing: true
        }

        Label {
            Layout.fillWidth: true
            text: "Incoming call"
            font.pixelSize: 15
            font.bold: true
            color: "#FFFFFF"
            horizontalAlignment: Text.AlignHCenter
        }

        Label {
            Layout.fillWidth: true
            text: root.callPeerId || "Unknown"
            font.pixelSize: 13
            color: "#B9BBBE"
            elide: Text.ElideMiddle
            horizontalAlignment: Text.AlignHCenter
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 12

            Button {
                Layout.fillWidth: true
                text: "\u274C  Decline"
                Material.background: "#ED4245"
                onClicked: root.dismiss(false)
            }

            Button {
                Layout.fillWidth: true
                text: "\u2705  Accept"
                Material.background: "#57F287"
                Material.foreground: "#000000"
                onClicked: root.dismiss(true)
            }
        }
    }
}
