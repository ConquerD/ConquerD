// StatsPanel.qml — Connection statistics overlay.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Rectangle {
    id: root

    color: Theme.withAlpha(Theme.overlayScrim, 0.82)
    radius: Theme.radiusMd
    border.color: Theme.divider
    border.width: 1
    width: 220
    height: contentCol.implicitHeight + Theme.spacingLg * 2

    property real rttMs: 0
    property real packetLossPct: 0
    property real jitterMs: 0
    property bool isRelay: false
    property real bandwidthKbps: 0
    property var _rttHistory: []

    function applyStats(jsonStr) {
        try {
            var s = JSON.parse(jsonStr)
            root.rttMs          = s.rtt_ms         || 0
            root.packetLossPct  = s.packet_loss_pct || 0
            root.jitterMs       = s.jitter_ms       || 0
            root.isRelay        = s.relay            || false
            root.bandwidthKbps  = s.bandwidth_kbps  || 0

            var h = root._rttHistory.slice()
            h.push(root.rttMs)
            if (h.length > 30) h = h.slice(h.length - 30)
            root._rttHistory = h
            sparkCanvas.requestPaint()
        } catch (e) {}
    }

    ColumnLayout {
        id: contentCol
        anchors {
            top: parent.top
            left: parent.left
            right: parent.right
            margins: Theme.spacingMd
        }
        spacing: Theme.spacingXs

        RowLayout {
            Layout.fillWidth: true
            Text {
                text: "Connection Stats"
                color: Theme.text
                font.pixelSize: Theme.fontSizeCaption
                font.bold: true
                Layout.fillWidth: true
            }
            Rectangle {
                width: 8; height: 8; radius: 4
                color: root.isRelay ? Theme.warn : Theme.online
            }
            Text {
                text: root.isRelay ? "Relay" : "Direct"
                color: root.isRelay ? Theme.warn : Theme.online
                font.pixelSize: Theme.fontSizeCaption
            }
        }

        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.divider }

        StatRow { label: "RTT"; value: root.rttMs.toFixed(0) + " ms"; warn: root.rttMs > 200 }
        StatRow { label: "Packet loss"; value: root.packetLossPct.toFixed(1) + "%"; warn: root.packetLossPct > 2 }
        StatRow { label: "Jitter"; value: root.jitterMs.toFixed(0) + " ms"; warn: root.jitterMs > 30 }
        StatRow { label: "Bandwidth"; value: root.bandwidthKbps.toFixed(0) + " kbps"; warn: false }

        RowLayout {
            Layout.fillWidth: true

            Text {
                text: "Quality"
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                Layout.fillWidth: true
            }
            Text {
                readonly property string _tier: {
                    if (root.rttMs < 80 && root.packetLossPct < 0.5 && root.jitterMs < 15)
                        return "Excellent"
                    if (root.rttMs < 150 && root.packetLossPct < 1.5 && root.jitterMs < 25)
                        return "Good"
                    if (root.rttMs < 300 && root.packetLossPct < 4.0 && root.jitterMs < 50)
                        return "Fair"
                    return "Poor"
                }
                text: _tier
                color: Theme.qualityTierColor(_tier)
                font.pixelSize: Theme.fontSizeCaption
                font.bold: true
            }
        }

        Rectangle {
            Layout.fillWidth: true
            height: 38
            color: Theme.withAlpha(Theme.bg0, 0.55)
            radius: Theme.radiusSm

            Text {
                anchors { left: parent.left; top: parent.top; margins: Theme.spacingXs }
                text: "RTT"
                color: Theme.muted
                font.pixelSize: Theme.fontSizeMicro
            }

            Canvas {
                id: sparkCanvas
                anchors { fill: parent; margins: 2 }

                onPaint: {
                    var ctx = getContext("2d")
                    ctx.clearRect(0, 0, width, height)
                    var hist = root._rttHistory
                    if (hist.length < 2) return

                    var maxVal = Math.max(200, Math.max.apply(null, hist))
                    var step   = width / (hist.length - 1)
                    var pad    = 2
                    var lineColor = Theme.toHex(Theme.rttSparklineColor(maxVal))

                    ctx.beginPath()
                    for (var i = 0; i < hist.length; i++) {
                        var x = i * step
                        var y = height - pad - (hist[i] / maxVal) * (height - pad * 2)
                        if (i === 0) ctx.moveTo(x, y)
                        else         ctx.lineTo(x, y)
                    }
                    ctx.lineTo((hist.length - 1) * step, height)
                    ctx.lineTo(0, height)
                    ctx.closePath()
                    ctx.globalAlpha = 0.15
                    ctx.fillStyle   = lineColor
                    ctx.fill()

                    ctx.globalAlpha = 0.85
                    ctx.beginPath()
                    for (var j = 0; j < hist.length; j++) {
                        var px = j * step
                        var py = height - pad - (hist[j] / maxVal) * (height - pad * 2)
                        if (j === 0) ctx.moveTo(px, py)
                        else         ctx.lineTo(px, py)
                    }
                    ctx.strokeStyle = lineColor
                    ctx.lineWidth   = 1.5
                    ctx.lineJoin    = "round"
                    ctx.stroke()
                    ctx.globalAlpha = 1.0
                }
            }
        }
    }

    component StatRow: RowLayout {
        required property string label
        required property string value
        required property bool warn

        Layout.fillWidth: true

        Text {
            text: parent.label
            color: Theme.muted
            font.pixelSize: Theme.fontSizeCaption
            Layout.fillWidth: true
        }
        Text {
            text: parent.value
            color: parent.warn ? Theme.warn : Theme.text
            font.pixelSize: Theme.fontSizeCaption
            font.bold: parent.warn
        }
    }
}