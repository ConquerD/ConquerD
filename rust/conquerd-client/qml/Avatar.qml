// Avatar.qml - Identity-derived symmetric identicon.

import QtQuick
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    // Peer identity string: base64url Ed25519 public key or any unique id.
    property string peerId: ""

    // When non-empty, passed verbatim to avatarSvg() and used instead of the
    // trust-tier lookup. Use this for the own-avatar live preview in Settings.
    property string configJson: ""

    property int size: 32
    property bool showRing: false
    property color ringColor: _tintHex
    property bool speaking: false
    property real audioLevel: 0.0
    property string _svgSource: ""
    property string _tintHex: Theme.toHex(Theme.muted)
    property bool _imageSmooth: true
    readonly property color tintColor: _tintHex
    readonly property bool _ringVisible: root.showRing || root.speaking
    readonly property int _ringWidth: {
        if (!_ringVisible) return 0
        return root.speaking ? Math.max(5, Math.round(root.size * 0.22))
                             : Math.max(4, Math.round(root.size * 0.17))
    }

    implicitWidth: size
    implicitHeight: size
    width: size
    height: size

    function _refresh() {
        if (!peerId) {
            _svgSource = ""
            _tintHex = Theme.toHex(Theme.muted)
            _imageSmooth = true
            return
        }
        var svgStr = backend.avatarSvg(peerId, configJson)
        _svgSource = "data:image/svg+xml;base64," + Qt.btoa(svgStr)
        _tintHex = backend.avatarTintColor(peerId, configJson)
        _imageSmooth = backend.avatarImageSmooth(peerId, configJson)
    }

    Component.onCompleted: _refresh()
    onPeerIdChanged: _refresh()
    onConfigJsonChanged: _refresh()

    Connections {
        target: backend
        function onAvatarConfigUpdated(peer_id) {
            if (peer_id === root.peerId) _refresh()
        }
    }

    Rectangle {
        id: statusRing
        anchors.fill: parent
        radius: width / 2
        color: "transparent"
        border.color: root.ringColor
        border.width: root.showRing && !root.speaking ? root._ringWidth : 0
        visible: border.width > 0

        Behavior on border.color {
            ColorAnimation { duration: Theme.animMicro }
        }
    }

    Rectangle {
        id: body
        anchors.centerIn: parent
        width: root.size - (root._ringWidth > 0 ? root._ringWidth * 2 : 0)
        height: width
        radius: width / 2
        color: "transparent"
        clip: true

        Image {
            id: img
            anchors.fill: parent
            source: root._svgSource
            sourceSize.width: root.size
            sourceSize.height: root.size
            fillMode: Image.Stretch
            smooth: root._imageSmooth
            mipmap: root._imageSmooth && root.size > 32
            visible: root._svgSource !== ""
        }
    }
}
