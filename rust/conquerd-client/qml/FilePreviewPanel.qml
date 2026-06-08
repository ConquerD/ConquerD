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

    readonly property string _fileName: root.filePath.length > 0
        ? root.filePath.replace(/\\/g, "/").split("/").pop()
        : ""

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

    Rectangle {
        anchors.fill: parent
        color: Theme.bg0
        radius: 0
        clip: true

        Rectangle {
            id: headerBar
            anchors { top: parent.top; left: parent.left; right: parent.right }
            height: Theme.touchTarget
            color: Theme.bg1

            RowLayout {
                anchors {
                    fill: parent
                    leftMargin: Theme.spacingMd
                    rightMargin: Theme.spacingSm
                }
                spacing: Theme.spacingSm

                Label {
                    Layout.fillWidth: true
                    text: root._fileName || "File Preview"
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeCaption
                    font.bold: true
                    elide: Text.ElideMiddle
                }

                ToolButton {
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/globe.svg"
                    icon.width: 14
                    icon.height: 14
                    icon.color: Theme.accent
                    flat: true
                    implicitWidth: Theme.controlHeight
                    implicitHeight: Theme.controlHeight
                    ToolTip.text: "Open in system application"
                    ToolTip.visible: hovered
                    onClicked: Qt.openUrlExternally("file:///" + root.filePath.replace(/\\/g, "/"))
                }

                ToolButton {
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                    icon.width: 12
                    icon.height: 12
                    icon.color: Theme.muted
                    flat: true
                    implicitHeight: Theme.controlHeight
                    implicitWidth: Theme.controlHeight
                    ToolTip.text: "Close preview"
                    ToolTip.visible: hovered
                    onClicked: root.closeRequested()
                }
            }
        }

        Item {
            anchors {
                top: headerBar.bottom
                left: parent.left; right: parent.right; bottom: parent.bottom
            }

            ConquerdWebView {
                id: _webView
                anchors.fill: parent
                allowAll: false
                allowedDomains: []
                visible: root._canPreview(root.filePath)
            }

            ColumnLayout {
                anchors.centerIn: parent
                visible: !root._canPreview(root.filePath) && root.filePath !== ""
                spacing: Theme.spacingMd

                EmptyState {
                    Layout.alignment: Qt.AlignHCenter
                    iconSource: "qrc:/qt/qml/ConquerD/Client/icons/attach.svg"
                    iconSize: 32
                    title: "No preview available"
                    subtitle: root._fileName
                }

                StyledButton {
                    Layout.alignment: Qt.AlignHCenter
                    text: "Open in system application"
                    primary: true
                    onClicked: Qt.openUrlExternally("file:///" + root.filePath.replace(/\\/g, "/"))
                }
            }
        }
    }

    onFilePathChanged: {
        if (filePath === "" || !_canPreview(filePath)) return
        var e = _ext(filePath)

        if (_videoExts.indexOf(e) >= 0) {
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
            _webView.navigate("file:///" + filePath.replace(/\\/g, "/"))
        }
    }
}