// SidebarItem.qml — Reusable sidebar navigation item.
//
// Usage:
//   SidebarItem {
//       icon: "qrc:/qt/qml/ConquerD/Client/icons/speech.svg"
//       label: "Chat"
//       badge: 3        // 0 = no badge
//       selected: true
//       onClicked: currentPage = "chat"
//   }

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

ItemDelegate {
    id: root

    property string icon: ""
    property string label: ""
    property int badge: 0
    property bool selected: false

    width: parent ? parent.width : 200
    height: 44
    padding: 0
    topPadding: 0
    bottomPadding: 0

    background: Rectangle {
        color: root.selected
            ? Qt.rgba(Theme.accent.r, Theme.accent.g, Theme.accent.b, 0.15)
            : (root.hovered ? Theme.bg2 : "transparent")
        radius: 0

        Rectangle {
            anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
            width: 3
            radius: 0
            color: root.selected ? Theme.accent : "transparent"
        }
    }

    contentItem: RowLayout {
        anchors { fill: parent; leftMargin: 12; rightMargin: 8 }
        spacing: 10

        Image {
            source: root.icon
            visible: root.icon !== ""
            sourceSize.width: 18
            sourceSize.height: 18
            Layout.preferredWidth: 18
            Layout.preferredHeight: 18
            fillMode: Image.PreserveAspectFit
            opacity: root.selected ? 1.0 : 0.72
        }

        Text {
            text: root.label
            color: root.selected ? Theme.text : Theme.muted
            font.pixelSize: Theme.fontSizeCaption
            font.bold: root.selected
            elide: Text.ElideRight
            Layout.fillWidth: true
        }

        // Badge
        Rectangle {
            visible: root.badge > 0
            width: Math.max(18, badgeLabel.width + 8)
            height: 18
            radius: 9
            color: Theme.accent

            Text {
                id: badgeLabel
                anchors.centerIn: parent
                text: root.badge > 99 ? "99+" : root.badge.toString()
                color: "white"
                font.pixelSize: 10
                font.bold: true
            }
        }
    }
}
