import QtQuick
import QtQuick.Controls.Material
import ConquerD.Client 1.0

Rectangle {
    id: root

    property string peerId: ""
    property real rttMs: 0
    property real packetLossPct: 0
    property bool isRelay: false
    property bool expanded: false

    signal toggleExpanded()

    readonly property bool hasData: root.rttMs > 0
    readonly property color qualityColor: {
        if (root.rttMs <= 0) return Theme.muted
        if (root.packetLossPct > 4 || root.rttMs > 300) return Theme.danger
        if (root.packetLossPct > 1.5 || root.rttMs > 150) return Theme.warn
        return Theme.online
    }

    implicitWidth: chipRow.implicitWidth + Theme.spacingSm * 2
    implicitHeight: Theme.controlHeight
    radius: Theme.radiusSm
    color: Theme.semanticTint(qualityColor, 0.14)
    border.color: Theme.semanticTint(qualityColor, 0.35)
    border.width: 1
    visible: root.peerId !== "" && root.hasData

    Behavior on color { ColorAnimation { duration: Theme.animNormal } }
    Behavior on border.color { ColorAnimation { duration: Theme.animNormal } }

    Row {
        id: chipRow
        anchors.centerIn: parent
        spacing: Theme.spacingXs

        Rectangle {
            width: 6
            height: 6
            radius: Theme.radiusPill
            anchors.verticalCenter: parent.verticalCenter
            color: root.qualityColor
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: root.rttMs > 0 ? Math.round(root.rttMs) + " ms" : "—"
            color: Theme.text
            font.pixelSize: Theme.fontSizeCaption
            font.bold: true
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: root.isRelay
            text: "relay"
            color: Theme.warn
            font.pixelSize: Theme.fontSizeMicro
        }
    }

    MouseArea {
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        ToolTip.text: "Connection stats"
        ToolTip.visible: containsMouse
        onClicked: root.toggleExpanded()
    }
}