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
//     Portal mode hides the toolbar and off-the-record notice for a full-bleed view.
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
        if (!root.nodeMode)
            _addressBar.text = url
    }

    onStartUrlChanged: {
        if (startUrl !== "") navigateTo(startUrl)
    }

    // ── Toolbar (omitted in supernode portal mode) ──────────────────────
    Rectangle {
        id: toolbar
        anchors { top: parent.top; left: parent.left; right: parent.right }
        height: root.nodeMode ? 0 : Theme.touchTarget
        visible: !root.nodeMode
        color: Theme.bg1

        RowLayout {
            anchors {
                fill: parent
                leftMargin: Theme.spacingSm
                rightMargin: Theme.spacingSm
            }
            spacing: Theme.spacingXs

            ToolButton {
                id: _backBtn
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/connect.svg"
                icon.width: 14
                icon.height: 14
                icon.color: _webView.loading ? Theme.muted : Theme.text
                flat: true
                implicitWidth: Theme.controlHeight
                implicitHeight: Theme.controlHeight
                enabled: !_webView.loading
                onClicked: _webView.goBack()
                ToolTip.text: "Back"
                ToolTip.visible: hovered
            }

            ToolButton {
                id: _fwdBtn
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/send.svg"
                icon.width: 14
                icon.height: 14
                icon.color: _webView.loading ? Theme.muted : Theme.text
                flat: true
                implicitWidth: Theme.controlHeight
                implicitHeight: Theme.controlHeight
                enabled: !_webView.loading
                onClicked: _webView.goForward()
                ToolTip.text: "Forward"
                ToolTip.visible: hovered
            }

            ToolButton {
                id: _reloadBtn
                icon.source: _webView.loading
                    ? "qrc:/qt/qml/ConquerD/Client/icons/x-circle.svg"
                    : "qrc:/qt/qml/ConquerD/Client/icons/refresh.svg"
                icon.width: 14
                icon.height: 14
                icon.color: Theme.text
                flat: true
                implicitWidth: Theme.controlHeight
                implicitHeight: Theme.controlHeight
                onClicked: _webView.loading ? _webView.navigate(_webView.currentUrl) : _webView.reload()
                ToolTip.text: _webView.loading ? "Stop" : "Reload"
                ToolTip.visible: hovered
            }

            StyledTextField {
                id: _addressBar
                visible: !root.nodeMode
                Layout.fillWidth: true
                placeholderText: "Enter URL or search…"
                font.pixelSize: Theme.fontSizeCaption
                text: _webView.currentUrl === "about:blank" ? "" : _webView.currentUrl
                Keys.onReturnPressed: {
                    var input = _addressBar.text.trim()
                    if (input === "") return
                    if (!input.match(/^[a-zA-Z][a-zA-Z0-9+\-.]*:\/\//)) {
                        if (input.match(/^[^\s]+\.[^\s]+/))
                            input = "https://" + input
                        else
                            input = "https://www.google.com/search?q=" + encodeURIComponent(input)
                    }
                    _webView.navigate(input)
                }
            }

            ToolButton {
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/globe.svg"
                icon.width: 14
                icon.height: 14
                icon.color: Theme.muted
                flat: true
                implicitWidth: Theme.controlHeight
                implicitHeight: Theme.controlHeight
                ToolTip.text: "Open in system browser"
                ToolTip.visible: hovered
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

    Connections {
        target: _webView
        function onCurrentUrlChanged() {
            if (root.nodeMode || _addressBar.activeFocus)
                return
            _addressBar.text = _webView.currentUrl === "about:blank"
                               ? "" : _webView.currentUrl
        }
    }

    // ── Privacy notice bar (general browser mode only) ────────────────────
    Rectangle {
        id: _privacyBar
        anchors { top: toolbar.bottom; left: parent.left; right: parent.right }
        height: visible ? Theme.bannerHeight : 0
        color: Theme.bg2
        visible: false

        RowLayout {
            anchors {
                fill: parent
                leftMargin: Theme.spacingMd
                rightMargin: Theme.spacingSm
            }
            spacing: Theme.spacingSm

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
                text: "Off-the-record — no cookies or history are saved."
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
            }
            ToolButton {
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                icon.width: 12
                icon.height: 12
                icon.color: Theme.muted
                flat: true
                implicitWidth: Theme.controlHeight
                implicitHeight: Theme.controlHeight
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
        allowConquerd: root.nodeMode
        allowAll: !root.nodeMode
        startUrl: root.startUrl !== "" ? root.startUrl : "about:blank"

        onCurrentUrlChanged: {
            if (root.nodeMode)
                return
            if (_webView.currentUrl !== "about:blank" && _webView.currentUrl !== "")
                _privacyBar.visible = true
        }
    }
}