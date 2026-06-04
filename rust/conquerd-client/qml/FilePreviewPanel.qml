// FilePreviewPanel.qml — Inline file preview using the secure ConquerdWebView.
//
// Renders received files in a local Chromium surface:
//   • Images, PDF, HTML, text, code files → loaded via file:// URL directly.
//   • Video files (mp4, webm, ogv)         → injected HTML5 <video> element.
//   • Audio files (mp3, wav, ogg, flac, aac) → injected HTML5 <audio> element.
//   • All other types                      → shows a "cannot preview" message.
//
// Navigation is restricted to file:// and data: URIs only — no outbound
// network requests are possible from within the preview panel.
//
// Usage:
//   FilePreviewPanel {
//       filePath: "/path/to/received_document.pdf"
//       onCloseRequested: panel.visible = false
//   }

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    signal closeRequested()

    /// Absolute path to the file to preview. Changing this reloads the view.
    property string filePath: ""

    // ── Extension → render mode mapping ──────────────────────────────────
    readonly property var _imageExts:
        [".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".svg", ".ico"]
    readonly property var _webExts:
        [".pdf", ".html", ".htm", ".txt", ".md",
         ".py", ".js", ".ts", ".json", ".xml", ".css", ".log"]
    readonly property var _videoExts: [".mp4", ".webm", ".ogv"]
    readonly property var _audioExts: [".mp3", ".wav", ".ogg", ".flac", ".aac"]

    function _ext(path) {
        var dot = path.lastIndexOf(".")
        return dot >= 0 ? path.substring(dot).toLowerCase() : ""
    }

    function _canPreview(path) {
        var e = _ext(path)
        return _imageExts.indexOf(e) >= 0 ||
               _webExts.indexOf(e) >= 0   ||
               _videoExts.indexOf(e) >= 0 ||
               _audioExts.indexOf(e) >= 0
    }

    // ── Background ────────────────────────────────────────────────────────
    Rectangle {
        anchors.fill: parent
        color: "#111214"
        radius: 0
        clip: true

        // ── Header bar ────────────────────────────────────────────────────
        Rectangle {
            id: headerBar
            anchors { top: parent.top; left: parent.left; right: parent.right }
            height: 36
            color: "#1E2124"

            RowLayout {
                anchors { fill: parent; leftMargin: 10; rightMargin: 6 }
                spacing: 6

                Label {
                    Layout.fillWidth: true
                    text: root.filePath.length > 0
                          ? root.filePath.replace(/\\/g, "/").split("/").pop()
                          : "File Preview"
                    color: "#DCDDDE"
                    font.pixelSize: 11
                    elide: Text.ElideMiddle
                }

                Button {
                    text: "⬡ Open"
                    flat: true
                    implicitHeight: 24
                    font.pixelSize: 10
                    Material.foreground: Theme.accent
                    ToolTip.text: "Open in system application"
                    ToolTip.visible: hovered
                    onClicked: Qt.openUrlExternally("file:///" + root.filePath.replace(/\\/g, "/"))
                }

                Button {
                    text: "✕"
                    flat: true
                    implicitHeight: 24
                    implicitWidth: 28
                    font.pixelSize: 12
                    Material.foreground: "#8E9297"
                    onClicked: root.closeRequested()
                }
            }
        }

        // ── Preview area ──────────────────────────────────────────────────
        Item {
            anchors {
                top: headerBar.bottom
                left: parent.left; right: parent.right; bottom: parent.bottom
            }

            // ConquerdWebView for previewable types
            ConquerdWebView {
                id: _webView
                anchors.fill: parent

                // No outbound network: only file:// and data: permitted
                allowAll: false
                allowedDomains: []  // empty → local content only

                visible: root._canPreview(root.filePath)
            }

            // "Cannot preview" fallback
            ColumnLayout {
                anchors.centerIn: parent
                visible: !root._canPreview(root.filePath) && root.filePath !== ""
                spacing: 12

                Label {
                    Layout.alignment: Qt.AlignHCenter
                    text: "📎"
                    font.pixelSize: 32
                }
                Label {
                    Layout.alignment: Qt.AlignHCenter
                    text: "No preview available"
                    color: "#8E9297"
                    font.pixelSize: 13
                }
                Label {
                    Layout.alignment: Qt.AlignHCenter
                    text: root.filePath.replace(/\\/g, "/").split("/").pop()
                    color: "#5D6A73"
                    font.pixelSize: 10
                }
                Button {
                    Layout.alignment: Qt.AlignHCenter
                    text: "Open in system application"
                    flat: true
                    Material.foreground: Theme.accent
                    onClicked: Qt.openUrlExternally("file:///" + root.filePath.replace(/\\/g, "/"))
                }
            }
        }
    }

    // ── Reload when filePath changes ──────────────────────────────────────
    onFilePathChanged: {
        if (filePath === "" || !_canPreview(filePath)) return
        var e = _ext(filePath)
        var encoded = encodeURIComponent(filePath.replace(/\\/g, "/"))

        if (_videoExts.indexOf(e) >= 0) {
            // Inject HTML5 <video> — avoids file:// cross-origin restrictions
            // on some platforms by using a data: URI with the source attribute
            // pointing to the local path via file://.
            _webView.navigate("data:text/html;charset=utf-8," + encodeURIComponent(
                "<!DOCTYPE html><html><body style='margin:0;background:#000;display:flex;" +
                "align-items:center;justify-content:center;height:100vh'>" +
                "<video controls autoplay style='max-width:100%;max-height:100vh' " +
                "src='file:///" + filePath.replace(/\\/g, "/") + "'>" +
                "</video></body></html>"
            ))
        } else if (_audioExts.indexOf(e) >= 0) {
            _webView.navigate("data:text/html;charset=utf-8," + encodeURIComponent(
                "<!DOCTYPE html><html><body style='margin:0;background:#111;display:flex;" +
                "align-items:center;justify-content:center;height:100vh'>" +
                "<audio controls autoplay style='width:90%' " +
                "src='file:///" + filePath.replace(/\\/g, "/") + "'>" +
                "</audio></body></html>"
            ))
        } else {
            // Images, PDF, HTML, text, code — load directly via file:// URL
            _webView.navigate("file:///" + filePath.replace(/\\/g, "/"))
        }
    }
}
