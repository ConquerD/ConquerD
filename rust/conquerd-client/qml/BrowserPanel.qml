// BrowserPanel.qml — Embedded node browser panel (4th sidebar tab content).
//
// A general-purpose Chromium browser embedded in the native client.
// Primary uses:
//   • Supernode portals via conquerd:// (served over the QUIC relay connection)
//   • Relay access portals that require a browser visit
//   • General web browsing at user discretion
//
// Security properties:
//   • Off-the-record profile: no cookies, cache, or history persist.
//   • conquerd:// portal mode (nodeMode: true): navigation is locked to the
//     conquerd:// scheme only. External links open in the system browser.
//   • Peer identity, crypto material, and AppBridge state are never
//     accessible from within the browser (no QWebChannel bridge in this panel).
//
// The panel is only shown when a conquerd:// portal is active (portalActive = true).
// A disclosure is presented in Settings → Privacy before first enabling.
//
// Public API:
//   BrowserPanel {
//       id: browserPanel
//       startUrl: "conquerd://abc123/"
//       nodeMode: true          // lock to conquerd:// only
//   }
//   // Later: browserPanel.navigateTo("conquerd://abc123/games/")

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    /// Set this to navigate to a URL from outside the panel (e.g., portal redirect).
    property string startUrl: ""

    /// When true, restricts navigation to conquerd:// only (node portal mode).
    /// When false, all URLs are permitted (general browser mode).
    property bool nodeMode: false

    /// Navigate to a URL from outside the panel.
    function navigateTo(url) {
        _webView.navigate(url)
        _addressBar.text = url
    }

    onStartUrlChanged: {
        if (startUrl !== "") navigateTo(startUrl)
    }

    // ── Toolbar ───────────────────────────────────────────────────────────
    Rectangle {
        id: toolbar
        anchors { top: parent.top; left: parent.left; right: parent.right }
        height: 40
        color: Theme.bg1

        RowLayout {
            anchors { fill: parent; leftMargin: 6; rightMargin: 6 }
            spacing: 4

            // Back button
            ToolButton {
                id: _backBtn
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/connect.svg"
                icon.width: 14
                icon.height: 14
                icon.color: _webView.loading ? Theme.muted : Theme.text
                flat: true
                implicitWidth: 32; implicitHeight: 30
                enabled: !_webView.loading
                onClicked: _webView.goBack()
                ToolTip.text: "Back"; ToolTip.visible: hovered
            }

            // Forward button
            ToolButton {
                id: _fwdBtn
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/send.svg"
                icon.width: 14
                icon.height: 14
                icon.color: _webView.loading ? Theme.muted : Theme.text
                flat: true
                implicitWidth: 32; implicitHeight: 30
                enabled: !_webView.loading
                onClicked: _webView.goForward()
                ToolTip.text: "Forward"; ToolTip.visible: hovered
            }

            // Reload / stop button
            ToolButton {
                id: _reloadBtn
                icon.source: _webView.loading
                    ? "qrc:/qt/qml/ConquerD/Client/icons/x-circle.svg"
                    : "qrc:/qt/qml/ConquerD/Client/icons/refresh.svg"
                icon.width: 14
                icon.height: 14
                icon.color: Theme.text
                flat: true
                implicitWidth: 32
                implicitHeight: 30
                onClicked: _webView.loading ? _webView.navigate(_webView.currentUrl) : _webView.reload()
                ToolTip.text: _webView.loading ? "Stop" : "Reload"; ToolTip.visible: hovered
            }

            // Address bar
            TextField {
                id: _addressBar
                Layout.fillWidth: true
                implicitHeight: 28
                font.pixelSize: 11
                color: Theme.text
                placeholderText: "Enter URL or search…"
                background: Rectangle {
                    color: Theme.bg0
                    border.color: _addressBar.activeFocus ? Theme.accent : Theme.bg3
                    border.width: 1
                    radius: 0
                }
                leftPadding: 8; rightPadding: 8
                text: _webView.currentUrl === "about:blank" ? "" : _webView.currentUrl
                // Navigate on Enter
                Keys.onReturnPressed: {
                    var input = _addressBar.text.trim()
                    if (input === "") return
                    // Auto-prepend https:// if no scheme is present
                    if (!input.match(/^[a-zA-Z][a-zA-Z0-9+\-.]*:\/\//)) {
                        // Looks like a URL (has a dot) → prepend https://
                        if (input.match(/^[^\s]+\.[^\s]+/))
                            input = "https://" + input
                        else
                            input = "https://www.google.com/search?q=" + encodeURIComponent(input)
                    }
                    _webView.navigate(input)
                }
            }

            // Open in system browser
            ToolButton {
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/globe.svg"
                icon.width: 14
                icon.height: 14
                icon.color: Theme.muted
                flat: true
                implicitWidth: 32; implicitHeight: 30
                ToolTip.text: "Open in system browser"
                ToolTip.visible: hovered
                // Hidden for conquerd:// URLs — they can only be served
                // by *this* client over its authenticated QUIC relay,
                // and Windows has the scheme registered to ConquerD.exe
                // so handing it off would just spawn a second instance.
                enabled: _webView.currentUrl !== "" &&
                         _webView.currentUrl !== "about:blank" &&
                         !_webView.currentUrl.startsWith("conquerd:")
                opacity: enabled ? 1.0 : 0.4
                onClicked: {
                    var u = _webView.currentUrl
                    if (u !== "" && u !== "about:blank" &&
                        !u.startsWith("conquerd:"))
                        Qt.openUrlExternally(u)
                }
            }
        }
    }

    // Keep address bar in sync with navigation
    Connections {
        target: _webView
        // currentUrl is a read-only alias, so we watch it via the view
        function onCurrentUrlChanged() {
            if (!_addressBar.activeFocus)
                _addressBar.text = _webView.currentUrl === "about:blank"
                                   ? "" : _webView.currentUrl
        }
    }

    // ── Privacy notice bar (first use reminder) ───────────────────────────
    Rectangle {
        id: _privacyBar
        anchors { top: toolbar.bottom; left: parent.left; right: parent.right }
        height: visible ? 30 : 0
        color: Theme.bg2
        visible: false   // set true on first navigateTo() until acknowledged

        RowLayout {
            anchors { fill: parent; leftMargin: 10; rightMargin: 6 }
            Image {
                source: "qrc:/qt/qml/ConquerD/Client/icons/lock.svg"
                sourceSize.width: 12
                sourceSize.height: 12
                Layout.preferredWidth: 12
                Layout.preferredHeight: 12
                fillMode: Image.PreserveAspectFit
            }
            Label {
                Layout.fillWidth: true
                text: "Off-the-record - no cookies or history are saved."
                color: Theme.muted
                font.pixelSize: 10
            }
            ToolButton {
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                icon.width: 12
                icon.height: 12
                icon.color: Theme.muted
                flat: true; implicitWidth: 24; implicitHeight: 24
                onClicked: _privacyBar.visible = false
            }
        }
    }

    // ── Browser view ──────────────────────────────────────────────────────
    ConquerdWebView {
        id: _webView
        anchors {
            top: _privacyBar.bottom
            left: parent.left; right: parent.right; bottom: parent.bottom
        }
        // In node-portal mode allow only conquerd://; otherwise allow all.
        allowConquerd: root.nodeMode
        allowAll: !root.nodeMode
        startUrl: root.startUrl !== "" ? root.startUrl : "about:blank"

        onCurrentUrlChanged: {
            // Show the privacy reminder bar whenever a new page loads
            if (_webView.currentUrl !== "about:blank" && _webView.currentUrl !== "")
                _privacyBar.visible = true
        }
    }
}
