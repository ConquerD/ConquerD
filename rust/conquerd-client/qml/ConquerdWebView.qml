// ConquerdWebView.qml — Shared secure Chromium (QtWebEngine) wrapper.
//
// Security model:
//   • Always off-the-record: no persistent cookies, cache, localStorage,
//     IndexedDB, or history survive beyond the widget's lifetime.
//   • Navigation whitelist: only hosts whose suffix matches an entry in
//     allowedDomains are allowed.  file:// and data: URIs are always
//     permitted so local file previews and injected HTML work without an
//     entry in the list.
//   • allowAll: true bypasses the whitelist (used for the general browser
//     panel where the user controls navigation).  A privacy disclosure in
//     the settings page accompanies this mode.
//   • No QWebChannel bridge — this view has zero access to Rust/AppBridge
//     peer data or crypto state.
//
// Usage (embed use-cases):
//   ConquerdWebView {
//       anchors.fill: parent
//       allowedDomains: ["youtube.com", "googlevideo.com"]
//       startUrl: "https://www.youtube.com/embed/dQw4w9WgXcQ?autoplay=1"
//   }
//
// Usage (general browser panel):
//   ConquerdWebView {
//       anchors.fill: parent
//       allowAll: true
//       startUrl: "about:blank"
//   }

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import QtWebEngine
import ConquerD.Client 1.0

Item {
    id: root

    // ── Public API ────────────────────────────────────────────────────────

    /// Hostname suffixes to allow when allowAll is false.
    /// An empty list with allowAll:false permits only file:// and data: URIs.
    property var allowedDomains: []

    /// When true, all navigation is permitted (used by the browser panel).
    property bool allowAll: false

    /// When true, navigation to conquerd:// URLs is permitted.
    /// Set this for the node-portal panel to allow supernode portal pages
    /// while still blocking outbound https:// navigation.
    property bool allowConquerd: false

    /// Initial URL to load on component creation.
    property string startUrl: ""

    /// Expose the view's current URL to parent components.
    readonly property string currentUrl: _view.url.toString()

    /// Expose loading state for parent spinners.
    readonly property bool loading: _view.loading

    /// Expose page title.
    readonly property string pageTitle: _view.title

    /// Navigate to a new URL programmatically.
    function navigate(url) {
        console.log("[portal] ConquerdWebView.navigate url=" + url + " allowConquerd=" + root.allowConquerd + " allowAll=" + root.allowAll)
        _view.url = url
    }

    /// Reload the current page.
    function reload() {
        _view.reload()
    }

    function goBack()    { _view.goBack()    }
    function goForward() { _view.goForward() }

    // ── WebEngineView ─────────────────────────────────────────────────────
    WebEngineView {
        id: _view
        anchors.fill: parent

        Component.onCompleted: {
            if (root.startUrl !== "")
                _view.url = root.startUrl
        }

        onNavigationRequested: function(request) {
            var urlStr = request.url.toString()
            var colonIdx = urlStr.indexOf(":")
            var scheme = colonIdx > 0 ? urlStr.substring(0, colonIdx) : ""
            var host = ""
            if (urlStr.substring(colonIdx, colonIdx + 3) === "://") {
                var rest = urlStr.substring(colonIdx + 3)
                var slash = rest.indexOf("/")
                host = slash >= 0 ? rest.substring(0, slash) : rest
            }
            console.log("[portal] onNavigationRequested scheme=" + scheme + " host=" + host + " url=" + urlStr + " allowConquerd=" + root.allowConquerd)

            if (scheme === "file" || scheme === "data" ||
                scheme === "qrc"  || scheme === "about") {
                request.accept()
                return
            }

            if (scheme === "conquerd") {
                if (root.allowConquerd || root.allowAll) {
                    request.accept()
                } else {
                    request.reject()
                }
                return
            }

            if (root.allowConquerd && !root.allowAll) {
                request.reject()
                Qt.openUrlExternally(request.url)
                return
            }

            if (root.allowAll) {
                request.accept()
                return
            }

            for (var i = 0; i < root.allowedDomains.length; i++) {
                if (host === root.allowedDomains[i] ||
                    host.endsWith("." + root.allowedDomains[i])) {
                    request.accept()
                    return
                }
            }

            request.reject()
            Qt.openUrlExternally(request.url)
        }

        onContextMenuRequested: function(request) {
            request.accepted = true
        }

        onNewWindowRequested: function(request) {
            request.openIn(_view)
        }
    }

    // ── Loading overlay ───────────────────────────────────────────────────
    Rectangle {
        anchors.fill: parent
        color: Theme.bg0
        visible: _view.loading && !_errorVisible

        ColumnLayout {
            anchors.centerIn: parent
            spacing: Theme.spacingMd

            BusyIndicator {
                Layout.alignment: Qt.AlignHCenter
                running: parent.parent.visible
            }
            Label {
                Layout.alignment: Qt.AlignHCenter
                text: "Loading…"
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
            }
        }
    }

    // ── Error / blocked overlay ───────────────────────────────────────────
    property bool _errorVisible: false
    property string _errorUrl: ""

    Connections {
        target: _view
        function onLoadingChanged(loadRequest) {
            if (loadRequest.status === WebEngineLoadingInfo.LoadFailedStatus) {
                root._errorUrl = loadRequest.url.toString()
                root._errorVisible = true
            } else {
                root._errorVisible = false
            }
        }
    }

    Rectangle {
        anchors.fill: parent
        color: Theme.bg0
        visible: root._errorVisible

        ColumnLayout {
            anchors.centerIn: parent
            spacing: Theme.spacingMd
            width: Math.min(parent.width - Theme.spacingXl * 2, 300)

            Image {
                source: "qrc:/qt/qml/ConquerD/Client/icons/warning.svg"
                sourceSize.width: 28
                sourceSize.height: 28
                Layout.preferredWidth: 28
                Layout.preferredHeight: 28
                Layout.alignment: Qt.AlignHCenter
                fillMode: Image.PreserveAspectFit
            }
            Label {
                Layout.alignment: Qt.AlignHCenter
                text: "Could not load page"
                color: Theme.text
                font.pixelSize: Theme.fontSizeTitle
                font.bold: true
            }
            Label {
                Layout.fillWidth: true
                text: root._errorUrl
                visible: root._errorUrl !== "" &&
                         !root._errorUrl.startsWith("conquerd:")
                color: Theme.muted
                font.pixelSize: Theme.fontSizeMicro
                wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                elide: Text.ElideRight
                maximumLineCount: 2
            }
            StyledButton {
                Layout.alignment: Qt.AlignHCenter
                text: "Open in system browser"
                primary: true
                visible: root._errorUrl !== "" &&
                         !root._errorUrl.startsWith("conquerd:")
                onClicked: Qt.openUrlExternally(root._errorUrl)
            }
        }
    }
}