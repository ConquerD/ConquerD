// CallPanel.qml — Floating call overlay shown during active voice calls.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Rectangle {
    id: root
    width: 240
    height: 80
    radius: 0
    color: Theme.bg2
    border.color: Theme.accent
    border.width: 1

    signal endCall()
    signal muteToggled(bool muted)

    property string callState: "idle"
    property bool muted: false

    RowLayout {
        anchors.fill: parent
        anchors.margins: 12
        spacing: 8

        // Call state indicator
        Rectangle {
            width: 10
            height: 10
            radius: 5
              color: root.callState === "in_call" ? Theme.online
                  : root.callState === "connecting" ? Theme.warn
                  : Theme.danger
        }

        Text {
            text: root.callState === "in_call" ? "In Call"
                : root.callState === "connecting" ? "Connecting…"
                : root.callState
            color: Theme.textInv
            font.pixelSize: 13
            Layout.fillWidth: true
        }

        // Mute toggle
        Button {
            icon.source: root.muted ? "qrc:/qt/qml/ConquerD/Client/icons/mic-off.svg" : "qrc:/qt/qml/ConquerD/Client/icons/mic.svg"
            icon.width: 18
            icon.height: 18
            flat: true
            implicitWidth: 32
            implicitHeight: 32
            Material.foreground: root.muted ? Theme.danger : Theme.online
            onClicked: {
                root.muted = !root.muted
                root.muteToggled(root.muted)
            }
        }

        ToolButton {
            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/x-circle.svg"
            icon.width: 16
            icon.height: 16
            icon.color: Theme.danger
            flat: true
            implicitWidth: 32
            implicitHeight: 32
            ToolTip.text: "End call"
            ToolTip.visible: hovered
            onClicked: root.endCall()
        }
    }
}
