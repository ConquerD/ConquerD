// SessionBanner.qml — Compact session status bar at the top of the window.
//
// Displays the current connection path, health, and voice state.
// Color-coded: green = direct, yellow = relay, red = offline/error.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Rectangle {
    id: root

    property string bannerText: ""
    property string connectionMode: "offline"  // "direct" | "relay" | "offline" | "error"

    readonly property color _dotColor: Theme.connectionModeColor(root.connectionMode)
    readonly property string _modeLabel: Theme.connectionModeLabel(root.connectionMode)

    color: Qt.tint(Theme.bg1, Theme.connectionModeTint(root.connectionMode))

    // 3px left accent bar
    Rectangle {
        id: accentBar
        anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
        width: 3
        color: root._dotColor
        Behavior on color { ColorAnimation { duration: Theme.animSlow } }
    }

    RowLayout {
        anchors {
            left: accentBar.right; leftMargin: Theme.spacingSm
            right: parent.right; rightMargin: Theme.spacingSm
            top: parent.top; bottom: parent.bottom
        }
        spacing: Theme.spacingXs

        // Status dot
        Rectangle {
            width: 7; height: 7; radius: 3.5
            color: root._dotColor
            Behavior on color { ColorAnimation { duration: Theme.animSlow } }
        }

        // Connection mode label
        Text {
            text: root._modeLabel
            color: root._dotColor
            font.pixelSize: Theme.fontSizeCaption
            font.bold: true
            Behavior on color { ColorAnimation { duration: Theme.animSlow } }
        }

        Text {
            text: "\u00B7"
            color: Theme.muted
            font.pixelSize: Theme.fontSizeCaption
            visible: root.bannerText !== ""
        }

        Text {
            Layout.fillWidth: true
            text: root.bannerText
            color: Theme.muted
            font.pixelSize: Theme.fontSizeCaption
            elide: Text.ElideRight
        }
    }

    Behavior on color { ColorAnimation { duration: Theme.animSlow } }
}
