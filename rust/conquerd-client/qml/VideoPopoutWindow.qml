// VideoPopoutWindow.qml — one peer's video in a detached OS window.
//
// The first dynamically-created top-level window in this app, so a few things
// that MainWindow gets for free have to be done explicitly here:
//
//  * It is not an engine root object, so `qml_startup.cpp` never sees it and
//    the frameless chrome must be requested by hand (Windows only).
//  * `ApplicationWindow.onClosing -> Qt.quit()` is not inherited, which is
//    what we want — closing a popout must not quit the app.
//  * Geometry is restored against the *current* screen set, so unplugging a
//    monitor cannot strand the window off-screen.

import QtQuick
import QtQuick.Layouts
import QtQuick.Window
import ConquerD.Client 1.0
import ConquerD.Native 1.0

Window {
    id: popWin

    /// Peer whose stream this window shows.
    property string peerId: ""
    /// Display name for the title bar.
    property string displayName: ""
    /// Whether that peer is currently sending. Drives the placeholder, so a
    /// peer switching their camera off shows "Camera off" here rather than
    /// leaving their last frame frozen on screen.
    property bool streaming: false

    signal closed()
    /// Shared-audio level/mute changed for this popout's peer.
    signal contentAudioChanged(string peerId, bool muted, int volume)

    title: (displayName || peerId) + " — ConquerD"
    width: 640
    height: 400
    minimumWidth: 240
    minimumHeight: 180
    color: Theme.bg0
    visible: true

    // Native decorations everywhere except Windows, where the custom chrome
    // below reproduces them. Copying MainWindow's frameless flags without that
    // chrome would leave a window with no resize borders and no snap.
    flags: Qt.platform.os === "windows"
        ? (Qt.Window | Qt.CustomizeWindowHint)
        : Qt.Window

    TitleBar {
        id: popTitleBar
        appWindow: popWin
        anchors { top: parent.top; left: parent.left; right: parent.right }
        z: 200

        Text {
            text: popWin.displayName || popWin.peerId
            color: Theme.text
            font.pixelSize: Theme.fontSizeBody
            font.bold: true
            elide: Text.ElideRight
            Layout.fillWidth: true
            Layout.leftMargin: Theme.spacingSm
        }
    }

    Loader {
        anchors {
            top: popTitleBar.bottom
            left: parent.left
            right: parent.right
            bottom: parent.bottom
        }
        // Lazy for the same reason as the region: without Qt Multimedia there
        // is no VideoTile in the qrc, and this file must still parse.
        source: "qrc:/qt/qml/ConquerD/Client/qml/VideoTile.qml"
        onLoaded: {
            item.peerId = Qt.binding(() => popWin.peerId)
            item.displayName = ""      // the title bar already names the peer
            item.showChrome = false    // no popout/collapse buttons in a popout
            // ...but the shared-audio control is still wanted: a popped-out
            // video is exactly where someone turns a loud stream down.
            item.showAudioControl = true
            item.streaming = Qt.binding(() => popWin.streaming)
            item.contentAudioChanged.connect(function(muted, volume) {
                popWin.contentAudioChanged(popWin.peerId, muted, volume)
            })
            // The tile pushed its starting state from Component.onCompleted,
            // which ran before the connection above existed, so that first push
            // went nowhere. Repeat it now that there is something listening.
            item.applyContentAudio()
        }
    }

    Component.onCompleted: {
        popWin.restoreGeometry()
        if (Qt.platform.os === "windows")
            WindowChrome.enable(popWin)
    }

    Component.onDestruction: {
        if (Qt.platform.os === "windows")
            WindowChrome.disable(popWin)
    }

    onClosing: {
        popWin.persistGeometry()
        // Deliberately not Qt.quit(): a popout closing must never take the app
        // with it, even when the main window is hidden to tray.
        popWin.closed()
    }

    onXChanged: geometrySaveTimer.restart()
    onYChanged: geometrySaveTimer.restart()
    onWidthChanged: geometrySaveTimer.restart()
    onHeightChanged: geometrySaveTimer.restart()

    Timer {
        id: geometrySaveTimer
        interval: 600
        onTriggered: popWin.persistGeometry()
    }

    function persistGeometry() {
        if (!settingsModel || !popWin.peerId)
            return
        var all = {}
        try {
            all = JSON.parse(settingsModel.video_popout_geometry_json || "{}")
        } catch (e) {
            all = {}
        }
        all[popWin.peerId] = {
            x: popWin.x, y: popWin.y, w: popWin.width, h: popWin.height
        }
        settingsModel.video_popout_geometry_json = JSON.stringify(all)
        settingsModel.save()
    }

    function restoreGeometry() {
        if (!settingsModel || !popWin.peerId)
            return
        var all
        try {
            all = JSON.parse(settingsModel.video_popout_geometry_json || "{}")
        } catch (e) {
            return
        }
        var g = all[popWin.peerId]
        if (!g)
            return

        // Only accept a saved rect that still lands on a connected screen.
        // MainWindow only ever persisted width/height, so this hazard is new:
        // without the check, unplugging a monitor puts the window somewhere
        // the user cannot reach it.
        var visibleOnSome = false
        for (var i = 0; i < Qt.application.screens.length; i++) {
            var s = Qt.application.screens[i]
            var overlapsX = g.x + g.w > s.virtualX && g.x < s.virtualX + s.width
            var overlapsY = g.y + g.h > s.virtualY && g.y < s.virtualY + s.height
            if (overlapsX && overlapsY) {
                visibleOnSome = true
                break
            }
        }
        popWin.width = Math.max(popWin.minimumWidth, g.w || popWin.width)
        popWin.height = Math.max(popWin.minimumHeight, g.h || popWin.height)
        if (visibleOnSome) {
            popWin.x = g.x
            popWin.y = g.y
        }
        // Otherwise leave the default centred position.
    }
}
