// ParticipantWidget.qml - Compact avatar tile with optimized activity ring.

import QtQuick
import QtQuick.Controls
import ConquerD.Client 1.0

Item {
    id: root

    property string peerId: ""
    property string displayName: "Unknown"
    property bool isMuted: false
    property real audioLevel: 0.0
    property bool isSelf: false

    readonly property real visualLevel: isMuted ? 0.0 : Math.max(0.0, Math.min(1.0, audioLevel))
    readonly property real quantizedLevel: Math.round(visualLevel * 32) / 32
    readonly property bool speaking: quantizedLevel > 0.05 && !isMuted
    readonly property color activityColor: {
        var lv = root.quantizedLevel
        if (lv < 0.55) {
            var t = lv / 0.55
            return Qt.rgba(
                (48 + (87 - 48) * t) / 255,
                (204 + (242 - 204) * t) / 255,
                (255 + (135 - 255) * t) / 255,
                1
            )
        }
        var f = (lv - 0.55) / 0.45
        return Qt.rgba(
            (87 + (254 - 87) * f) / 255,
            (242 + (231 - 242) * f) / 255,
            (135 + (92 - 135) * f) / 255,
            1
        )
    }

    // Persistent waveform history provided by VoiceRail; retained for API
    // compatibility with richer ring renderers.
    property var ringStore: null

    property bool showActivityRing: true
    property bool showSelfRing: true

    width: 80
    height: 80

    Avatar {
        anchors.centerIn: parent
        peerId: root.peerId
        size: 48
        showRing: true
    }

    // Property-bound rings are cheaper than Canvas repainting for live levels.
    Item {
        anchors.fill: parent
        visible: root.showActivityRing && (!root.isSelf || root.showSelfRing)

        Rectangle {
            anchors.centerIn: parent
            width: 50
            height: 50
            radius: width / 2
            color: "transparent"
            border.width: 5
            border.color: Theme.text
            opacity: 0.10
        }

        Rectangle {
            anchors.centerIn: parent
            width: 50 + root.quantizedLevel * 5
            height: width
            radius: width / 2
            color: "transparent"
            border.width: 5 + root.quantizedLevel * 2
            border.color: root.activityColor
            opacity: root.speaking ? Math.min(1.0, 0.36 + root.quantizedLevel * 0.64) : 0.0

            Behavior on width { NumberAnimation { duration: 70; easing.type: Easing.OutQuad } }
            Behavior on border.width { NumberAnimation { duration: 70; easing.type: Easing.OutQuad } }
            Behavior on border.color { ColorAnimation { duration: 90 } }
            Behavior on opacity { NumberAnimation { duration: 90; easing.type: Easing.OutQuad } }
        }
    }

    Rectangle {
        visible: root.isMuted
        anchors {
            right: parent.right
            bottom: parent.bottom
            margins: 4
        }
        width: 18
        height: 18
        radius: width / 2
        color: Theme.danger

        Image {
            anchors.centerIn: parent
            source: "qrc:/qt/qml/ConquerD/Client/icons/mic-off.svg"
            sourceSize.width: 12
            sourceSize.height: 12
            width: 12
            height: 12
            fillMode: Image.PreserveAspectFit
        }
    }

    Rectangle {
        visible: root.isSelf
        anchors {
            left: parent.left
            bottom: parent.bottom
            margins: 4
        }
        implicitWidth: youLabel.implicitWidth + 8
        height: 14
        radius: height / 2
        color: Theme.accent

        Text {
            id: youLabel
            anchors.centerIn: parent
            text: "you"
            color: Theme.textInv
            font.pixelSize: Theme.fontSizeCaption
            font.bold: true
        }
    }

    HoverHandler { id: hover }

    ToolTip {
        visible: hover.hovered
        text: (root.displayName && root.displayName !== "Unknown" && root.displayName !== "")
            ? root.displayName
            : root.peerId
        delay: 300
        timeout: 5000
    }
}
