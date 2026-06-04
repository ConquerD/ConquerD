// ParticipantWidget.qml - Compact avatar tile with activity ring.

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

    readonly property bool speaking: audioLevel > 0.05 && !isMuted

    // Persistent waveform history provided by VoiceRail; survives RoomModel
    // resets so the one-minute heat map accumulates correctly.
    property var ringStore: null

    // When false the inner activity ring Canvas is hidden.
    property bool showActivityRing: true
    // When false the speaking indicators are suppressed for the local
    // participant (`isSelf`).
    property bool showSelfRing: true

    width: 80
    height: 80

    // Avatar (SVG identicon).
    Avatar {
        anchors.centerIn: parent
        peerId: root.peerId
        size: 48
        showRing: true
    }

    // ── Activity ring ──────────────────────────────────────────────
    // Drawn AFTER (on top of) the Avatar so the ring is always visible at the
    // avatar edge regardless of z-ordering with the histograph. Replaces the
    // static-colour rectangle glow with a level-responsive heat-map arc.
    Canvas {
        id: activityRingCanvas
        anchors.fill: parent
        // Suppress for self when showSelfRing is false.
        visible: root.showActivityRing && (!root.isSelf || root.showSelfRing)

        // Ring geometry: outside the avatar tint ring (radius 17 + ~6px ring)
        // so the talking ring sits visually beyond it.
        readonly property real _mid: width * 0.310   // ≈24.8 at 80 px
        readonly property real _w:   width * 0.065   //  ≈5.2 px wide

        onPaint: {
            var ctx = getContext("2d")
            ctx.clearRect(0, 0, width, height)
            var cx = width / 2, cy = height / 2
            var mid = _mid, w = _w
            var lv = Math.max(0, Math.min(1, root.audioLevel))

            // Ghost ring — always drawn so the ring never disappears
            ctx.beginPath()
            ctx.arc(cx, cy, mid, 0, Math.PI * 2)
            ctx.strokeStyle = "rgba(255,255,255,0.10)"
            ctx.lineWidth = w
            ctx.stroke()

            if (lv > 0.02) {
                // Heat-map colour: cool blue → green → hot yellow
                var t = lv < 0.55 ? lv / 0.55 : 1.0
                var a = lv < 0.55 ? [48, 204, 255] : [87, 242, 135]
                var b = lv < 0.55 ? [87, 242, 135] : [254, 231, 92]
                var f = lv < 0.55 ? lv / 0.55 : (lv - 0.55) / 0.45
                var cr = Math.round(a[0] + (b[0] - a[0]) * f)
                var cg = Math.round(a[1] + (b[1] - a[1]) * f)
                var cb = Math.round(a[2] + (b[2] - a[2]) * f)
                var alpha = Math.min(1, 0.40 + lv * 0.60)
                ctx.save()
                if (lv > 0.30) {
                    ctx.shadowColor = "rgba(" + cr + "," + cg + "," + cb + "," + (lv * 0.70).toFixed(2) + ")"
                    ctx.shadowBlur  = 10 * lv
                }
                ctx.beginPath()
                ctx.arc(cx, cy, mid, 0, Math.PI * 2)
                ctx.strokeStyle = "rgba(" + cr + "," + cg + "," + cb + "," + alpha.toFixed(3) + ")"
                ctx.lineWidth = w
                ctx.stroke()
                ctx.restore()
            }
        }

        Component.onCompleted: requestPaint()
        onVisibleChanged: if (visible) requestPaint()

        Connections {
            target: root
            function onAudioLevelChanged() { activityRingCanvas.requestPaint() }
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
        radius: 9
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
        radius: 7
        color: Theme.accent

        Text {
            id: youLabel
            anchors.centerIn: parent
            text: "you"
            color: "#ffffff"
            font.pixelSize: 8
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
