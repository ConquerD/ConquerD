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

    // Derive visual state from connectionMode
    readonly property color _dotColor: {
        switch (root.connectionMode) {
            case "direct":  return Theme.online
            case "relay":   return Theme.warn
            case "error":   return Theme.danger
            default:        return Theme.muted      // offline
        }
    }
    readonly property string _modeLabel: {
        switch (root.connectionMode) {
            case "direct":  return "Direct"
            case "relay":   return "Relay"
            case "error":   return "Error"
            default:        return "Offline"
        }
    }
    readonly property color _tintColor: {
        switch (root.connectionMode) {
            case "direct":  return Qt.rgba(0.23, 0.65, 0.36, 0.06)  // green tint
            case "relay":   return Qt.rgba(0.98, 0.65, 0.10, 0.06)  // yellow tint
            case "error":   return Qt.rgba(1.00, 0.17, 0.25, 0.06)  // red tint
            default:        return Qt.rgba(0,    0,    0,    0)
        }
    }

    color: Qt.tint(Theme.bg1, root._tintColor)

    // 3px left accent bar
    Rectangle {
        id: accentBar
        anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
        width: 3
        color: root._dotColor
        Behavior on color { ColorAnimation { duration: 300 } }
    }

    RowLayout {
        anchors {
            left: accentBar.right; leftMargin: 8
            right: parent.right; rightMargin: 8
            top: parent.top; bottom: parent.bottom
        }
        spacing: 6

        // Status dot
        Rectangle {
            width: 7; height: 7; radius: 3.5
            color: root._dotColor
            Behavior on color { ColorAnimation { duration: 300 } }
        }

        // Connection mode label
        Text {
            text: root._modeLabel
            color: root._dotColor
            font.pixelSize: 11
            font.bold: true
            Behavior on color { ColorAnimation { duration: 300 } }
        }

        // Separator dot
        Text {
            text: "\u00B7"
            color: Theme.muted
            font.pixelSize: 11
            visible: root.bannerText !== ""
        }

        // Banner text (scrolling elide)
        Text {
            Layout.fillWidth: true
            text: root.bannerText
            color: Theme.muted
            font.pixelSize: 11
            elide: Text.ElideRight
        }
    }

    Behavior on color { ColorAnimation { duration: 300 } }
}
