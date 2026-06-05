import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts

Item {
    id: root

    signal copyRequested(string body)
    signal deleteRequested(string msgId)
    signal retryRequested(string msgId)
    signal inlineAckAccepted()

    property string msgId: ""
    property string sender: ""
    property string body: ""
    property string kind: "text"
    property bool mine: false
    property real timestamp: 0
    property string status: "delivered"
    property bool isRoom: false
    property bool inlinePreviewEnabled: true
    property bool inlinePreviewAck: false
    property bool allowDelete: true

    width: ListView.view ? ListView.view.width : parent.width
    implicitHeight: column.implicitHeight + 10
    height: implicitHeight

    function escapeHtml(value) {
        return (value || "")
            .replace(/&/g, "&amp;")
            .replace(/</g, "&lt;")
            .replace(/>/g, "&gt;")
            .replace(/"/g, "&quot;")
    }

    function richText(value) {
        var text = escapeHtml(value)
        text = text.replace(/```([\s\S]*?)```/g, "<pre style='white-space:pre-wrap'>$1</pre>")
        text = text.replace(/`([^`\n]+)`/g, "<code>$1</code>")
        text = text.replace(/\*\*([^*\n]+)\*\*/g, "<b>$1</b>")
        text = text.replace(/\*([^*\n]+)\*/g, "<i>$1</i>")
        text = text.replace(/__([^_\n]+)__/g, "<u>$1</u>")
        text = text.replace(/~~([^~\n]+)~~/g, "<s>$1</s>")
        text = text.replace(/\n/g, "<br>")
        text = text.replace(/(https?:\/\/[^\s<>"]+)/g,
            '<a href="$1" style="color:' + (root.mine ? "#DCE0FF" : "#8EA7FF") + '">$1</a>')
        return text
    }

    function firstUrl(value) {
        var m = (value || "").match(/https?:\/\/[^\s<>"]+/)
        return m ? m[0] : ""
    }

    function youtubeId(value) {
        var b = value || ""
        var m = b.match(/youtu\.be\/([A-Za-z0-9_-]{11})/)
        if (m) return m[1]
        m = b.match(/youtube\.com\/watch\?.*v=([A-Za-z0-9_-]{11})/)
        if (m) return m[1]
        m = b.match(/youtube\.com\/shorts\/([A-Za-z0-9_-]{11})/)
        return m ? m[1] : ""
    }

    function vimeoId(value) {
        var m = (value || "").match(/vimeo\.com\/(?:video\/)?([0-9]+)/)
        return m ? m[1] : ""
    }

    function directVideoUrl(value) {
        var url = firstUrl(value)
        return /\.(mp4|webm|ogg)(\?|#|$)/i.test(url) ? url : ""
    }

    function previewKind() {
        if (youtubeId(root.body) !== "") return "youtube"
        if (vimeoId(root.body) !== "") return "vimeo"
        if (directVideoUrl(root.body) !== "") return "video"
        if (firstUrl(root.body) !== "") return "link"
        return ""
    }

    function previewTitle() {
        var k = previewKind()
        if (k === "youtube") return "YouTube"
        if (k === "vimeo") return "Vimeo"
        if (k === "video") return "Video"
        return "Link"
    }

    function previewUrl() {
        var yt = youtubeId(root.body)
        if (yt !== "") return "https://www.youtube.com/watch?v=" + yt
        var vm = vimeoId(root.body)
        if (vm !== "") return "https://vimeo.com/" + vm
        return firstUrl(root.body)
    }

    function inlineUrl() {
        var yt = youtubeId(root.body)
        if (yt !== "") return "https://www.youtube.com/embed/" + yt + "?autoplay=1&rel=0&modestbranding=1"
        var vm = vimeoId(root.body)
        if (vm !== "") return "https://player.vimeo.com/video/" + vm + "?autoplay=1"
        return directVideoUrl(root.body)
    }

    function statusText() {
        var s = root.status || "delivered"
        if (s === "sending") return "sending"
        if (s === "read") return "read"
        if (s === "failed") return "failed"
        if (s === "sent") return "sent"
        return "delivered"
    }

    Column {
        id: column
        anchors {
            right: root.mine ? parent.right : undefined
            left: root.mine ? undefined : parent.left
            rightMargin: root.mine ? 12 : 0
            leftMargin: root.mine ? 0 : 12
            top: parent.top
            topMargin: 5
        }
        width: Math.min(root.width * 0.75, 500)
        spacing: 4

        Text {
            visible: root.isRoom && !root.mine && root.sender !== ""
            text: root.sender
            color: Theme.muted
            font.pixelSize: 10
            elide: Text.ElideRight
            width: parent.width
        }

        Rectangle {
            id: bubble
            width: parent.width
            implicitHeight: bodyText.implicitHeight + 14
            radius: 0
            color: root.mine ? Theme.accent : Theme.bg2

            Text {
                id: bodyText
                anchors { fill: parent; margins: 7 }
                text: root.richText(root.body)
                textFormat: Text.RichText
                color: Theme.text
                font.pixelSize: 13
                wrapMode: Text.Wrap
                onLinkActivated: (link) => Qt.openUrlExternally(link)
            }

            MouseArea {
                anchors.fill: parent
                acceptedButtons: Qt.RightButton
                onClicked: function(mouse) {
                    if (mouse.button === Qt.RightButton) {
                        menu.popup()
                    }
                }
            }
        }

        Rectangle {
            id: preview
            property bool inline: false
            property string kindName: root.previewKind()
            visible: root.inlinePreviewEnabled && kindName !== ""
            width: parent.width
            height: visible ? (inline ? 185 : previewRow.implicitHeight + 14) : 0
            radius: 0
            color: Theme.bg1
            border.color: kindName === "youtube" ? "#FF0000" : Theme.bg3
            border.width: 1
            clip: true

            RowLayout {
                id: previewRow
                visible: !preview.inline
                anchors { left: parent.left; right: parent.right; margins: 8 }
                anchors.verticalCenter: parent.verticalCenter
                spacing: 8

                Rectangle {
                    Layout.preferredWidth: 34
                    Layout.preferredHeight: 24
                    radius: 0
                    color: preview.kindName === "youtube" ? Theme.danger : Theme.bg3
                    Image {
                        anchors.centerIn: parent
                        source: preview.kindName === "link"
                            ? "qrc:/qt/qml/ConquerD/Client/icons/globe.svg"
                            : "qrc:/qt/qml/ConquerD/Client/icons/play.svg"
                        sourceSize.width: 13
                        sourceSize.height: 13
                        width: 13
                        height: 13
                        fillMode: Image.PreserveAspectFit
                    }
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2
                    Label {
                        text: root.previewTitle()
                        color: Theme.text
                        font.pixelSize: 11
                        font.bold: true
                    }
                    Label {
                        text: root.previewUrl()
                        color: Theme.muted
                        font.pixelSize: 10
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                }

                ToolButton {
                    visible: root.inlineUrl() !== ""
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/play.svg"
                    icon.width: 14
                    icon.height: 14
                    icon.color: Theme.muted
                    implicitWidth: 28
                    implicitHeight: 24
                    padding: 4
                    ToolTip.text: "Inline"
                    ToolTip.visible: hovered
                    onClicked: {
                        if (root.inlinePreviewAck) {
                            preview.inline = true
                        } else {
                            disclosure._pendingPreview = preview
                            disclosure.open()
                        }
                    }
                }

                ToolButton {
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/globe.svg"
                    icon.width: 14
                    icon.height: 14
                    icon.color: Theme.muted
                    implicitWidth: 28
                    implicitHeight: 24
                    padding: 4
                    ToolTip.text: "Open"
                    ToolTip.visible: hovered
                    onClicked: Qt.openUrlExternally(root.previewUrl())
                }
            }

            Loader {
                visible: preview.inline
                anchors.fill: parent
                source: preview.inline ? Qt.resolvedUrl("ConquerdWebView.qml") : ""
                onLoaded: {
                    item.allowedDomains = [
                        "youtube.com", "youtu.be", "ytimg.com", "ggpht.com",
                        "googlevideo.com", "googleapis.com", "vimeo.com",
                        "player.vimeo.com", "vimeocdn.com"
                    ]
                    item.navigate(root.inlineUrl())
                }
            }
        }

        Row {
            anchors.right: root.mine ? parent.right : undefined
            spacing: 4

            Text {
                text: root.timestamp > 0
                    ? Qt.formatTime(new Date(root.timestamp * 1000), "hh:mm")
                    : ""
                color: Theme.muted
                font.pixelSize: 10
            }

            Text {
                visible: root.mine
                text: root.statusText()
                color: root.status === "failed" ? Theme.danger : Theme.muted
                font.pixelSize: 10
            }

            ToolButton {
                visible: root.mine && (root.status === "failed" || root.status === "sending")
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/refresh.svg"
                icon.width: 14
                icon.height: 14
                icon.color: Theme.muted
                implicitWidth: 22
                implicitHeight: 22
                padding: 4
                ToolTip.text: "Retry"
                ToolTip.visible: hovered
                onClicked: root.retryRequested(root.msgId)
            }

            ToolButton {
                visible: root.mine && root.msgId !== ""
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/trash.svg"
                icon.width: 14
                icon.height: 14
                icon.color: Theme.danger
                implicitWidth: 22
                implicitHeight: 22
                padding: 4
                ToolTip.text: "Delete"
                ToolTip.visible: hovered
                onClicked: root.deleteRequested(root.msgId)
            }
        }
    }

    Dialog {
        id: disclosure
        parent: Overlay.overlay
        anchors.centerIn: Overlay.overlay
        width: Math.min(root.width - 40, 420)
        title: "Inline Preview"
        modal: true
        standardButtons: Dialog.Ok | Dialog.Cancel

        property var _pendingPreview: null

        Label {
            width: parent.width
            wrapMode: Text.Wrap
            text: "Inline previews load the linked video provider directly in an off-the-record browser view. No data is sent to ConquerD servers."
            color: Theme.text
            font.pixelSize: 12
        }

        onAccepted: {
            root.inlinePreviewAck = true
            root.inlineAckAccepted()
            if (_pendingPreview) {
                _pendingPreview.inline = true
                _pendingPreview = null
            }
        }
        onRejected: { _pendingPreview = null }
    }

    Menu {
        id: menu
        MenuItem {
            text: "Copy Text"
            onTriggered: root.copyRequested(root.body)
        }
        MenuItem {
            text: "Delete Message"
            visible: root.allowDelete && root.msgId !== ""
            onTriggered: root.deleteRequested(root.msgId)
        }
    }
}
