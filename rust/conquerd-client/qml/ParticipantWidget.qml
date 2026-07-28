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
    property var ringStore: null

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

    property bool showActivityRing: true
    property bool showSelfRing: true
    /// When true, show a name pill on the avatar (room voice rail).
    property bool showNameBubbles: false

    /// Whether this peer is currently sending video. Drives the indicator only —
    /// video itself never renders in the 200 px rail, but in the expand region
    /// or a popout window.
    property bool videoActive: false
    /// Muted by *this listener*, as opposed to `isMuted` which is the peer's own
    /// microphone state and is visible to everyone.
    property bool locallyMuted: false

    /// Right-click: open the shared peer menu.
    signal contextMenuRequested()
    /// Clicking the video badge expands this peer — the primary action.
    signal expandVideoRequested()

    readonly property string peerLabelText: {
        if (root.displayName && root.displayName !== "Unknown" && root.displayName !== "")
            return root.displayName
        if (root.peerId.length > 12)
            return root.peerId.substring(0, 12) + "…"
        return root.peerId
    }

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

            Behavior on width { NumberAnimation { duration: Theme.animMicro; easing.type: Easing.OutQuad } }
            Behavior on border.width { NumberAnimation { duration: Theme.animMicro; easing.type: Easing.OutQuad } }
            Behavior on border.color { ColorAnimation { duration: Theme.animMicro } }
            Behavior on opacity { NumberAnimation { duration: Theme.animMicro; easing.type: Easing.OutQuad } }
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
        visible: root.showNameBubbles && root.peerLabelText !== ""
        anchors {
            horizontalCenter: parent.horizontalCenter
            bottom: parent.bottom
            bottomMargin: 2
        }
        implicitWidth: Math.min(peerNameLabel.implicitWidth + 8, 72)
        width: implicitWidth
        height: 14
        radius: height / 2
        color: Theme.accent

        Text {
            id: peerNameLabel
            anchors.centerIn: parent
            width: parent.width - 4
            text: root.peerLabelText
            color: Theme.textInv
            font.pixelSize: Theme.fontSizeCaption
            font.bold: true
            elide: Text.ElideRight
            horizontalAlignment: Text.AlignHCenter
        }
    }

    // ── Streaming-video indicator ─────────────────────────────────────────
    //
    // Top-right is the only free corner: the muted badge owns bottom-right and
    // the name pill owns bottom-centre. Deliberately mirrors the muted badge's
    // size and shape so the two read as one family.
    Rectangle {
        id: videoBadge
        visible: root.videoActive
        anchors {
            right: parent.right
            top: parent.top
            margins: 4
        }
        width: 18
        height: 18
        radius: width / 2
        color: Theme.accent

        Image {
            anchors.centerIn: parent
            source: "qrc:/qt/qml/ConquerD/Client/icons/video.svg"
            sourceSize.width: 12
            sourceSize.height: 12
            width: 12
            height: 12
            fillMode: Image.PreserveAspectFit
        }

        TapHandler {
            acceptedButtons: Qt.LeftButton
            onTapped: root.expandVideoRequested()
        }
        HoverHandler { id: videoHover; cursorShape: Qt.PointingHandCursor }

        ToolTip.text: qsTr("Streaming video — click to expand")
        ToolTip.visible: videoHover.hovered
        ToolTip.delay: 300
    }

    // ── Locally-muted marker ──────────────────────────────────────────────
    //
    // Distinct from the peer's own mute badge: this one says "I muted them",
    // which only this listener can see. Drawn as a ring rather than another
    // badge so the two are not confusable at a glance.
    Rectangle {
        visible: root.locallyMuted
        anchors.centerIn: parent
        width: 54
        height: 54
        radius: width / 2
        color: "transparent"
        border.width: 2
        border.color: Theme.danger
        opacity: 0.75
    }

    HoverHandler { id: hover }

    // TapHandler rather than a MouseArea: a full-cover MouseArea would swallow
    // hover events and kill the tooltip below.
    TapHandler {
        acceptedButtons: Qt.RightButton
        onTapped: root.contextMenuRequested()
    }

    ToolTip {
        visible: hover.hovered
        text: {
            var who = (root.displayName && root.displayName !== "Unknown" && root.displayName !== "")
                ? root.displayName
                : root.peerId
            return root.locallyMuted ? who + qsTr(" (muted for you)") : who
        }
        delay: 300
        timeout: 5000
    }
}
