// VideoRegion.qml — shared video area above the chat panel.
//
// Every expanded peer shares this one region rather than getting its own
// space, so expanding a second peer splits the area instead of pushing chat
// further down. Cells are laid out on a near-square grid and each keeps 16:9.

import QtQuick
import QtQuick.Controls
import ConquerD.Client 1.0

Item {
    id: root

    /// Peer ids currently expanded, in insertion order.
    property var expandedPeers: []
    /// Model used to resolve display names.
    property var participantModel: null
    /// Peer ids currently sending video, as a set (`{peerId: true}`).
    ///
    /// Passed in rather than read off `participantModel`: role data on a
    /// QAbstractListModel is only reachable from a delegate, and this component
    /// iterates peer *ids*. Reading it from the model here silently yielded
    /// "not streaming" for every peer, so every tile showed the camera-off
    /// placeholder no matter what was actually arriving.
    property var videoActivePeers: ({})

    /// Fraction of the content area this region occupies, persisted by the
    /// caller. Clamped to keep chat usable at either extreme.
    property real heightRatio: 0.4
    readonly property real minRatio: 0.2
    readonly property real maxRatio: 0.7

    signal collapseRequested(string peerId)
    signal popoutRequested(string peerId)
    signal ratioChanged(real ratio)

    /// Near-square grid: 1 -> 1x1, 2 -> 2x1, 3-4 -> 2x2, 5-6 -> 3x2, 7-9 -> 3x3.
    readonly property int columns: Math.max(1, Math.ceil(Math.sqrt(grid.count)))
    readonly property int rows: Math.max(1, Math.ceil(grid.count / columns))

    /// Display name for a peer, falling back to the raw id.
    ///
    /// The two participant models answer this differently and neither is
    /// optional: `RoomModel` is a QAbstractListModel with no `get()`, so it
    /// exposes `handleFor()` for use outside a delegate, while
    /// `directCallModel` is a plain ListModel that has `get()` but no such
    /// method. Probing for the method is what keeps both working.
    function _handle(pid) {
        var m = root.participantModel
        if (!m)
            return pid
        if (m.handleFor) {
            var h = m.handleFor(pid)
            return (h && h.length > 0) ? h : pid
        }
        if (m.count !== undefined && m.get) {
            for (var i = 0; i < m.count; i++) {
                var row = m.get(i)
                if (row && row.peerId === pid)
                    return row.handle || pid
            }
        }
        return pid
    }

    Rectangle {
        anchors.fill: parent
        color: Theme.bg0

        Grid {
            id: grid
            anchors {
                fill: parent
                margins: Theme.spacingXs
                bottomMargin: Theme.spacingXs + dividerStrip.height
            }
            columns: root.columns
            spacing: Theme.spacingXs

            property int count: root.expandedPeers.length

            Repeater {
                model: root.expandedPeers

                VideoTile {
                    required property string modelData

                    // Split the grid evenly; the tile letterboxes internally so
                    // a cell that is not 16:9 shows bars rather than stretching.
                    width: Math.floor(
                        (grid.width - (root.columns - 1) * grid.spacing) / root.columns)
                    height: Math.floor(
                        (grid.height - (root.rows - 1) * grid.spacing) / root.rows)

                    peerId: modelData
                    displayName: root._handle(modelData)
                    streaming: root.videoActivePeers[modelData] === true

                    onCloseRequested: root.collapseRequested(modelData)
                    onPopoutRequested: root.popoutRequested(modelData)
                }
            }
        }

        // Drag divider along the bottom edge.
        Rectangle {
            id: dividerStrip
            anchors { left: parent.left; right: parent.right; bottom: parent.bottom }
            height: 6
            color: dragHandler.active || dividerHover.hovered
                ? Theme.accent
                : Theme.divider

            Behavior on color { ColorAnimation { duration: Theme.animMicro } }

            HoverHandler {
                id: dividerHover
                cursorShape: Qt.SizeVerCursor
            }

            DragHandler {
                id: dragHandler
                target: null
                yAxis.enabled: true
                xAxis.enabled: false
                cursorShape: Qt.SizeVerCursor

                onCentroidChanged: {
                    if (!active || !root.parent)
                        return
                    // Ratio of the *parent* (the content area), since that is
                    // what the region's height is a fraction of.
                    var pos = centroid.scenePosition.y
                        - root.parent.mapToItem(null, 0, 0).y
                    var ratio = pos / Math.max(1, root.parent.height)
                    ratio = Math.max(root.minRatio, Math.min(root.maxRatio, ratio))
                    root.heightRatio = ratio
                    root.ratioChanged(ratio)
                }
            }
        }

        // Bottom border so the region reads as separate from the chat below.
        Rectangle {
            anchors { left: parent.left; right: parent.right; bottom: parent.bottom }
            height: 1
            color: Theme.divider
        }
    }
}
