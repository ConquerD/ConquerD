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

        // NOTE: no `profile:` binding on purpose.
        //
        // We deliberately use QWebEngineProfile::defaultProfile() (the
        // implicit profile when none is specified).  The conquerd://
        // URL scheme handler is installed on the default profile from
        // C++ (`conquerd_install_scheme_handler`), as are the
        // user-agent string, spell-check setting, and the
        // window.conquerd bridge script.
        //
        // The default profile in Qt 6 is off-the-record (no persistent
        // cookies, cache, history, localStorage, or IndexedDB), so the
        // privacy contract documented at the top of this file still
        // holds — but every WebEngineView in the process now shares
        // the profile that owns the conquerd:// handler.  Using an
        // inline `WebEngineProfile { ... }` here would silently
        // bypass the handler, causing Chromium to fall back to
        // QDesktopServices::openUrl() and (on Windows) spawn a fresh
        // ConquerD.exe via the OS-registered URI association.

        // Start loading the initial URL after the component is ready.
        Component.onCompleted: {
            if (root.startUrl !== "")
                _view.url = root.startUrl
        }

        // ── Navigation policy ─────────────────────────────────────────────
        onNavigationRequested: function(request) {
            // `request.url` is a QML `url` type whose `.scheme` / `.host`
            // JS properties are NOT exposed.  Convert to string and parse
            // manually.
            var urlStr = request.url.toString()
            var colonIdx = urlStr.indexOf(":")
            var scheme = colonIdx > 0 ? urlStr.substring(0, colonIdx) : ""
            // Best-effort host extraction (only valid for "scheme://host/…").
            var host = ""
            if (urlStr.substring(colonIdx, colonIdx + 3) === "://") {
                var rest = urlStr.substring(colonIdx + 3)
                var slash = rest.indexOf("/")
                host = slash >= 0 ? rest.substring(0, slash) : rest
            }
            console.log("[portal] onNavigationRequested scheme=" + scheme + " host=" + host + " url=" + urlStr + " allowConquerd=" + root.allowConquerd)

            // Always permit local content and injected HTML.
            if (scheme === "file" || scheme === "data" ||
                scheme === "qrc"  || scheme === "about") {
                request.accept()
                return
            }

            // conquerd:// — secure in-app portal scheme served by the
            // scheme handler over the QUIC relay connection.  Allowed
            // when allowConquerd (node portal panel) OR allowAll.
            if (scheme === "conquerd") {
                if (root.allowConquerd || root.allowAll) {
                    request.accept()
                } else {
                    request.reject()
                }
                return
            }

            // Node-portal panel: conquerd:// is the ONLY allowed scheme.
            // Any outbound http/https link opens in the system browser.
            if (root.allowConquerd && !root.allowAll) {
                request.reject()
                Qt.openUrlExternally(request.url)
                return
            }

            // General browser panel: permit everything.
            if (root.allowAll) {
                request.accept()
                return
            }

            // Whitelist check: allow if host ends with any allowed domain.
            for (var i = 0; i < root.allowedDomains.length; i++) {
                if (host === root.allowedDomains[i] ||
                    host.endsWith("." + root.allowedDomains[i])) {
                    request.accept()
                    return
                }
            }

            // Blocked — open in the system browser so the user isn't stranded.
            request.reject()
            Qt.openUrlExternally(request.url)
        }

        // Suppress context menu (no "Inspect Element", etc.)
        onContextMenuRequested: function(request) {
            request.accepted = true
        }

        // ── Popup / new-window policy ─────────────────────────────────────
        // Without this handler, QtWebEngine creates a fresh top-level
        // WebEngineView window for any `window.open()`, target="_blank"
        // link, middle-click, or framework-initiated popup.  We never
        // want that — route everything back into this same view (the
        // node-portal navigation policy above will then accept/reject
        // it based on scheme).
        onNewWindowRequested: function(request) {
            request.openIn(_view)
        }
    }

    // ── Loading overlay ───────────────────────────────────────────────────
    Rectangle {
        anchors.fill: parent
        color: "#1A1A1A"
        visible: _view.loading && !_errorVisible

        ColumnLayout {
            anchors.centerIn: parent
            spacing: 12

            BusyIndicator {
                Layout.alignment: Qt.AlignHCenter
                running: parent.parent.visible
            }
            Label {
                Layout.alignment: Qt.AlignHCenter
                text: "Loading…"
                color: "#8E9297"
                font.pixelSize: 11
            }
        }
    }

    // ── Error / blocked overlay ───────────────────────────────────────────
    property bool _errorVisible: false
    property string _errorUrl: ""

    Connections {
        target: _view
        function onLoadingChanged(loadRequest) {
            // LoadFailedStatus = 2 in QtWebEngine
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
        color: "#1A1A1A"
        visible: root._errorVisible

        ColumnLayout {
            anchors.centerIn: parent
            spacing: 12
            width: Math.min(parent.width - 32, 300)

            Label {
                Layout.alignment: Qt.AlignHCenter
                text: "⚠  Could not load page"
                color: "#DCDDDE"
                font.pixelSize: 13
                font.bold: true
            }
            Label {
                Layout.fillWidth: true
                text: root._errorUrl
                // Never hand a conquerd:// URL to the OS — Windows has
                // the scheme registered to ConquerD.exe, so doing so
                // would just spawn a second client instance.
                visible: root._errorUrl !== "" &&
                         !root._errorUrl.startsWith("conquerd:")
                color: "#8E9297"
                font.pixelSize: 10
                wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                elide: Text.ElideRight
                maximumLineCount: 2
            }
            Button {
                Layout.alignment: Qt.AlignHCenter
                text: "Open in system browser"
                flat: true
                Material.foreground: Theme.accent
                onClicked: Qt.openUrlExternally(root._errorUrl)
            }
        }
    }
}
