// StatsPanel.qml — Connection statistics overlay.
//
// Displays RTT, packet loss, jitter, and relay vs direct status.
// Intended as a collapsible panel or HUD overlay.
//
// Usage:
//   StatsPanel {
//       id: statsPanel
//       visible: showStats
//   }
//
// Receives data via backend.connectionStats signal (JSON string with fields:
//   rtt_ms, packet_loss_pct, jitter_ms, relay, bandwidth_kbps)

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Rectangle {
    id: root

    color: Qt.rgba(0, 0, 0, 0.75)
    radius: 0
    width: 220
    height: contentCol.implicitHeight + 20

    property real rttMs: 0
    property real packetLossPct: 0
    property real jitterMs: 0
    property bool isRelay: false
    property real bandwidthKbps: 0

    // Rolling RTT history for the sparkline (last 30 samples).
    property var _rttHistory: []

    // Connect to backend signal from parent:
    //   Connections { target: backend
    //     function onConnectionStats(jsonStr) { statsPanel.applyStats(jsonStr) } }
    function applyStats(jsonStr) {
        try {
            var s = JSON.parse(jsonStr)
            root.rttMs          = s.rtt_ms         || 0
            root.packetLossPct  = s.packet_loss_pct || 0
            root.jitterMs       = s.jitter_ms       || 0
            root.isRelay        = s.relay            || false
            root.bandwidthKbps  = s.bandwidth_kbps  || 0

            // Keep a capped history array (JS arrays are value types when
            // re-assigned, so we must replace the whole property to trigger bindings).
            var h = root._rttHistory.slice()
            h.push(root.rttMs)
            if (h.length > 30) h = h.slice(h.length - 30)
            root._rttHistory = h
            sparkCanvas.requestPaint()
        } catch (e) {}
    }

    ColumnLayout {
        id: contentCol
        anchors { top: parent.top; left: parent.left; right: parent.right; margins: 10 }
        spacing: 4

        // Header row
        RowLayout {
            Layout.fillWidth: true
            Text {
                text: "Connection Stats"
                color: "white"
                font.pixelSize: 11
                font.bold: true
                opacity: 0.8
                Layout.fillWidth: true
            }
            Rectangle {
                width: 8; height: 8; radius: 4
                color: root.isRelay ? "#FFA500" : "#22C55E"
            }
            Text {
                text: root.isRelay ? "Relay" : "Direct"
                color: root.isRelay ? "#FFA500" : "#22C55E"
                font.pixelSize: 10
            }
        }

        Rectangle { Layout.fillWidth: true; height: 1; color: "#ffffff22" }

        // Stat rows
        StatRow { label: "RTT";       value: root.rttMs.toFixed(0) + " ms";     warn: root.rttMs > 200 }
        StatRow { label: "Packet loss"; value: root.packetLossPct.toFixed(1) + "%"; warn: root.packetLossPct > 2 }
        StatRow { label: "Jitter";    value: root.jitterMs.toFixed(0) + " ms";   warn: root.jitterMs > 30 }
        StatRow { label: "Bandwidth"; value: root.bandwidthKbps.toFixed(0) + " kbps"; warn: false }

        // ── Quality tier label ────────────────────────────────────────────
        RowLayout {
            Layout.fillWidth: true

            Text {
                text: "Quality"
                color: "#aaaaaa"
                font.pixelSize: 10
                Layout.fillWidth: true
            }
            Text {
                readonly property string _tier: {
                    // Excellent: RTT<80ms, loss<0.5%, jitter<15ms
                    if (root.rttMs < 80 && root.packetLossPct < 0.5 && root.jitterMs < 15)
                        return "Excellent"
                    // Good: RTT<150ms, loss<1.5%, jitter<25ms
                    if (root.rttMs < 150 && root.packetLossPct < 1.5 && root.jitterMs < 25)
                        return "Good"
                    // Fair: RTT<300ms, loss<4%, jitter<50ms
                    if (root.rttMs < 300 && root.packetLossPct < 4.0 && root.jitterMs < 50)
                        return "Fair"
                    return "Poor"
                }
                text: _tier
                color: _tier === "Excellent" ? "#22c55e"
                     : _tier === "Good"      ? "#84cc16"
                     : _tier === "Fair"      ? "#f97316"
                     : "#ef4444"
                font.pixelSize: 10
                font.bold: true
            }
        }

        // ── RTT sparkline ─────────────────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            height: 38
            color: "#00000044"
            radius: 0

            // "RTT" label
            Text {
                anchors { left: parent.left; top: parent.top; margins: 3 }
                text: "RTT"
                color: "#666666"
                font.pixelSize: 8
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
                    // Color: green < 150 ms, orange < 300 ms, red otherwise
                    var lineColor = maxVal > 300 ? "#ef4444"
                                  : maxVal > 150 ? "#f97316"
                                  : "#22c55e"

                    // Filled area under the curve
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

                    // Line
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

    // Internal row component
    component StatRow: RowLayout {
        required property string label
        required property string value
        required property bool warn

        Layout.fillWidth: true

        Text {
            text: parent.label
            color: "#aaaaaa"
            font.pixelSize: 10
            Layout.fillWidth: true
        }
        Text {
            text: parent.value
            color: parent.warn ? "#FFA500" : "white"
            font.pixelSize: 10
            font.bold: parent.warn
        }
    }
}
