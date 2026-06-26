// TitleBar.qml — Custom frameless window title bar.
//
// Usage: add to ApplicationWindow with flags Qt.FramelessWindowHint.
// Provides drag-to-move, double-click-to-maximize, and min/max/close buttons.
// Wire backend.minimizeWindow / maximizeWindow / closeWindow invokables in
// bridge.rs (or call Window methods directly).

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    // Height consumed by the title bar (taller to host topbar widgets)
    implicitHeight: Theme.titleBarHeight

    // Parent window reference (set by parent ApplicationWindow)
    property var appWindow: null

    // Default children go into the centre slot between the logo and the
    // window control buttons. Use Layout.* attached properties for sizing.
    default property alias contentChildren: contentSlot.data

    // Whether the window is currently maximized
    readonly property bool isMaximized: appWindow
        ? (appWindow.visibility === Window.Maximized || appWindow.visibility === Window.FullScreen)
        : false

    // Drag-to-move region
    DragHandler {
        id: dragHandler
        target: null
        onActiveChanged: {
            if (active && appWindow) appWindow.startSystemMove()
        }
    }

    // Double-click to toggle maximize
    TapHandler {
        onDoubleTapped: {
            if (!appWindow) return
            if (root.isMaximized)
                appWindow.showNormal()
            else
                appWindow.showMaximized()
        }
    }

    // Background
    Rectangle {
        anchors.fill: parent
        color: Theme.bg0
    }

    RowLayout {
        anchors.fill: parent
        spacing: 0

        // Spacer fills remaining width so window controls stay right-aligned
        RowLayout {
            id: contentSlot
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.leftMargin: Theme.spacingSm
            Layout.rightMargin: Theme.spacingSm
            spacing: Theme.spacingSm
        }

        // ── Window control buttons ─────────────────────────────────────────

        // Minimize
        TitleBarButton {
            iconSource: "qrc:/qt/qml/ConquerD/Client/icons/wm-minimize.svg"
            onClicked: appWindow && appWindow.showMinimized()
        }

        // Maximize / Restore
        TitleBarButton {
            iconSource: root.isMaximized
                ? "qrc:/qt/qml/ConquerD/Client/icons/wm-restore.svg"
                : "qrc:/qt/qml/ConquerD/Client/icons/wm-maximize.svg"
            onClicked: {
                if (!appWindow) return
                if (root.isMaximized) appWindow.showNormal()
                else                  appWindow.showMaximized()
            }
        }

        // Close
        TitleBarButton {
            id: closeBtn
            iconSource: "qrc:/qt/qml/ConquerD/Client/icons/wm-close.svg"
            hoverColor: Theme.danger
            onClicked: appWindow && appWindow.close()
        }
    }

    // ── Internal button component ──────────────────────────────────────────
    component TitleBarButton: Rectangle {
        id: btnRoot
        property string iconSource: ""
        property color hoverColor: Theme.bg3
        signal clicked()

        width: 46; height: parent.height
        color: hoverArea.containsMouse ? hoverColor : "transparent"

        Behavior on color { ColorAnimation { duration: Theme.animMicro } }

        Image {
            anchors.centerIn: parent
            source: btnRoot.iconSource
            width: 12; height: 12
            fillMode: Image.PreserveAspectFit
        }

        MouseArea {
            id: hoverArea
            anchors.fill: parent
            hoverEnabled: true
            onClicked: btnRoot.clicked()
        }
    }
}
