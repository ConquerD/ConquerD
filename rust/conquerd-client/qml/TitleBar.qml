// TitleBar.qml — Custom window title bar (client-side chrome).
//
// Usage: add to ApplicationWindow with flags Qt.Window | Qt.CustomizeWindowHint.
// Provides drag-to-move (startSystemMove → Aero Snap on Windows), double-click
// maximize, and min/max/close buttons.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import QtQuick.Window
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

    // Drag-to-move. Prefer startSystemMove so Windows engages Aero Snap /
    // Snap Assist when the cursor hits a screen edge. DragHandler alone can
    // miss the system move modal loop if a child steals the grab; the
    // empty filler Item in contentSlot is the primary drag surface, and
    // this handler covers the rest of the bar that is not a control.
    DragHandler {
        id: dragHandler
        target: null
        acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchScreen
        // Do not CanTakeOverFromItems — that steals drags from TextField
        // selection and buttons. Empty filler + non-interactive chrome still
        // activate this handler for startSystemMove / Aero Snap.
        onActiveChanged: {
            if (!active || !appWindow)
                return
            // startSystemMove engages the system move modal loop so Windows
            // can snap to edges / corners (requires snap-friendly frame
            // styles from window_chrome.cpp).
            appWindow.startSystemMove()
        }
    }

    // Double-click to toggle maximize
    TapHandler {
        acceptedButtons: Qt.LeftButton
        gesturePolicy: TapHandler.DragThreshold
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
