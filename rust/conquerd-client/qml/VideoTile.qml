// VideoTile.qml — one peer's video surface.
//
// Deliberately one component with two hosts (the centre expand region and a
// popout window) rather than two similar ones: the sink lifecycle below is
// fiddly enough that duplicating it would mean duplicating the leak.
//
// `VideoOutput.videoSink` is read-only in Qt 6, so a sink cannot be handed to
// this item — the item hands *its* sink to the native registry instead, keyed
// by peer id. Several tiles may register for the same peer at once, which is
// what lets a stream show in the region and a popout simultaneously.

import QtQuick
import QtQuick.Controls
import QtMultimedia
import ConquerD.Native 1.0
import ConquerD.Client 1.0

Item {
    id: root

    /// Peer whose stream this tile shows.
    property string peerId: ""
    /// Display name for the label.
    property string displayName: ""
    /// Whether the peer is currently sending. Drives the placeholder.
    property bool streaming: false
    /// Show the name label and hover controls.
    property bool showChrome: true

    signal closeRequested()
    signal popoutRequested()

    // Tracks what we actually registered, which is not always `peerId`: when
    // peerId changes we must unregister the *previous* value, and by then the
    // property has already been updated.
    property string _bound: ""

    // Whether any frame has actually reached this sink.
    //
    // Separates "their camera is off" from "they say it is on but nothing is
    // arriving" — announced state and real frames are independent, since the
    // announcement is a signaling message and the frames are datagrams that can
    // be blocked, dropped, or never encoded. Collapsing both into one
    // placeholder makes a broken media path look like a peer who simply is not
    // sharing, which is the least debuggable failure this feature has.
    property bool _gotFrame: false

    onStreamingChanged: if (!root.streaming) root._gotFrame = false

    Connections {
        target: videoOutput.videoSink
        function onVideoFrameChanged(frame) { root._gotFrame = true }
    }

    function _bind(id) {
        if (id && id.length > 0)
            VideoRegistry.registerSink(id, videoOutput)
    }
    function _unbind(id) {
        if (id && id.length > 0)
            VideoRegistry.unregisterSink(id, videoOutput)
    }

    onPeerIdChanged: {
        _unbind(root._bound)
        root._bound = root.peerId
        root._gotFrame = false
        _bind(root._bound)
    }
    Component.onCompleted: {
        root._bound = root.peerId
        _bind(root._bound)
    }
    // Primary teardown path. The registry also holds QPointers so a destroyed
    // sink self-reaps, but relying on that alone would leak the peer's slot
    // until the next frame arrived.
    Component.onDestruction: _unbind(root._bound)

    Rectangle {
        anchors.fill: parent
        color: "black"
        radius: Theme.radiusSm
        clip: true

        VideoOutput {
            id: videoOutput
            anchors.fill: parent
            fillMode: VideoOutput.PreserveAspectFit
            // Only once a frame has landed — an empty VideoOutput paints
            // nothing, so showing it early hides the placeholder behind a
            // black rectangle and loses the explanation with it.
            visible: root.streaming && root._gotFrame
        }

        // Placeholder while no picture is on screen. Showing a black rectangle
        // instead would be indistinguishable from a broken decoder.
        Column {
            anchors.centerIn: parent
            spacing: Theme.spacingSm
            visible: !root.streaming || !root._gotFrame

            Avatar {
                anchors.horizontalCenter: parent.horizontalCenter
                peerId: root.peerId
                size: Math.max(32, Math.min(72, Math.round(root.height / 4)))
                showRing: false
            }

            Text {
                anchors.horizontalCenter: parent.horizontalCenter
                text: root.streaming ? qsTr("Waiting for video…") : qsTr("Camera off")
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
            }
        }

        // Name label
        Rectangle {
            visible: root.showChrome && root.displayName !== ""
            anchors {
                left: parent.left
                bottom: parent.bottom
                margins: Theme.spacingXs
            }
            width: nameLabel.implicitWidth + Theme.spacingSm
            height: nameLabel.implicitHeight + Theme.spacingXs
            radius: Theme.radiusSm
            color: Qt.rgba(0, 0, 0, 0.55)

            Text {
                id: nameLabel
                anchors.centerIn: parent
                text: root.displayName
                color: Theme.text
                font.pixelSize: Theme.fontSizeCaption
            }
        }

        // Tile controls.
        //
        // Anchored to the right edge so the row grows leftward: the close
        // button therefore keeps the same screen position whether or not the
        // hover-only popout button is present, instead of sliding out from
        // under the cursor as you reach for it.
        Row {
            visible: root.showChrome
            anchors {
                right: parent.right
                top: parent.top
                margins: Theme.spacingXs
            }
            spacing: Theme.spacingXs

            Rectangle {
                // Secondary action, so it stays on hover.
                visible: tileHover.hovered
                width: 26; height: 26; radius: Theme.radiusSm
                color: popoutHover.hovered ? Qt.rgba(0, 0, 0, 0.85) : Qt.rgba(0, 0, 0, 0.6)
                Image {
                    anchors.centerIn: parent
                    source: "qrc:/qt/qml/ConquerD/Client/icons/popout.svg"
                    sourceSize.width: 14; sourceSize.height: 14
                    width: 14; height: 14
                }
                HoverHandler { id: popoutHover }
                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.popoutRequested()
                }
                ToolTip.text: qsTr("Pop out to its own window")
                ToolTip.visible: popoutHover.hovered
            }

            // Always visible: closing a tile must not depend on discovering a
            // hover state, and it is the only way back out of the region once
            // the peer's camera goes off.
            Rectangle {
                id: closeButton
                width: 26; height: 26; radius: Theme.radiusSm
                color: closeHover.hovered ? Theme.danger : Qt.rgba(0, 0, 0, 0.6)
                Behavior on color { ColorAnimation { duration: Theme.animMicro } }

                Image {
                    anchors.centerIn: parent
                    source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                    sourceSize.width: 14; sourceSize.height: 14
                    width: 14; height: 14
                }
                HoverHandler { id: closeHover }
                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.closeRequested()
                }
                ToolTip.text: qsTr("Close video")
                ToolTip.visible: closeHover.hovered
            }
        }

        HoverHandler { id: tileHover }
    }
}
