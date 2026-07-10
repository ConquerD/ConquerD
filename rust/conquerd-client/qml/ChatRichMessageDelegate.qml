import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    signal copyRequested(string body)
    signal deleteRequested(string msgId)
    signal retryRequested(string msgId)
    signal inlineAckAccepted()
    signal openAttachmentRequested(string path)

    property string msgId: ""
    property string sender: ""
    /// Ed25519 public id for avatar lookup (room chat).
    property string senderPeerId: ""
    property string body: ""
    property string kind: "text"
    property bool mine: false
    property real timestamp: 0
    property string status: "delivered"
    property bool isRoom: false
    property bool inlinePreviewEnabled: true
    property bool inlinePreviewAck: false
    property bool allowDelete: true
    property string attachmentName: ""
    property string attachmentPath: ""
    property string sizeStr: ""

    readonly property bool isAttachment: root.kind === "image"
        || root.kind === "video"
        || root.kind === "file"
        || root.attachmentPath !== ""

    function fileUrlFromPath(p) {
        if (!p || p === "") return ""
        if (p.indexOf("file:") === 0) return p
        var n = p.replace(/\\/g, "/")
        if (n.charAt(0) !== "/") n = "/" + n
        return "file://" + n
    }

    readonly property string avatarPeerId: root.mine
        ? (backend.public_id || "")
        : (root.senderPeerId || "")

    width: ListView.view ? ListView.view.width : parent.width
    implicitHeight: messageRow.implicitHeight + 10
    height: implicitHeight

    function senderDisplayName() {
        if (root.sender && root.sender !== "" && root.sender !== root.senderPeerId)
            return root.sender
        var id = root.senderPeerId || ""
        if (id !== "") {
            var resolved = backend.peerDisplayName(id)
            if (resolved && resolved !== "" && resolved !== id)
                return resolved
        }
        if (root.sender && root.sender !== "")
            return root.sender
        if (id.length > 12)
            return id.substring(0, 12) + "…"
        return id
    }

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
            '<a href="$1" style="color:' + Theme.toHex(root.mine ? Theme.linkMine : Theme.linkPeer) + '">$1</a>')
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
        if (s === "sending") return "Sending…"
        if (s === "sent") return "Sent"
        if (s === "delivered") return "Delivered"
        if (s === "read") return "Read"
        if (s === "failed") return "Failed"
        return s
    }

    function timestampText() {
        if (root.timestamp <= 0) return ""
        return Qt.formatDateTime(new Date(root.timestamp * 1000), "MMM d, yyyy hh:mm")
    }

    property string dateSeparator: ""

    Row {
        id: messageRow
        anchors {
            right: root.mine ? parent.right : undefined
            left: root.mine ? undefined : parent.left
            rightMargin: root.mine ? 12 : 0
            leftMargin: root.mine ? 0 : 12
            top: parent.top
            topMargin: 5
        }
        spacing: root.isRoom ? 8 : 0

        Avatar {
            visible: root.isRoom && root.avatarPeerId !== ""
            peerId: root.avatarPeerId
            size: 32
            showRing: true
            anchors.top: parent.top
            anchors.topMargin: 2

            HoverHandler { id: avatarHover }
            ToolTip.visible: avatarHover.hovered && root.senderDisplayName() !== ""
            ToolTip.text: root.senderDisplayName()
            ToolTip.delay: 400
        }

        Column {
        id: column
        width: Math.min(root.width * (root.isRoom ? 0.72 : 0.75), 500)
        spacing: 4

        Text {
            visible: root.dateSeparator !== ""
            text: root.dateSeparator
            color: Theme.muted
            font.pixelSize: Theme.fontSizeCaption
            font.bold: true
            horizontalAlignment: Text.AlignHCenter
            width: parent.width
            topPadding: 4
            bottomPadding: 2
        }

        Text {
            visible: root.isRoom && !root.mine && root.senderDisplayName() !== ""
            text: root.senderDisplayName()
            color: Theme.muted
            font.pixelSize: Theme.fontSizeCaption
            font.bold: true
            elide: Text.ElideRight
            width: parent.width
        }

        Rectangle {
            id: bubble
            width: parent.width
            implicitHeight: bubbleCol.implicitHeight + Theme.spacingMd
            radius: Theme.radiusMd
            color: root.mine ? Theme.accent : Theme.bg2
            clip: true

            Column {
                id: bubbleCol
                anchors {
                    left: parent.left
                    right: parent.right
                    top: parent.top
                    margins: Theme.spacingSm
                }
                spacing: 6

                // Inline image embed (local file after transfer).
                Item {
                    id: imageEmbed
                    visible: root.kind === "image" && root.attachmentPath !== ""
                    width: parent.width
                    height: visible ? Math.min(220, Math.max(120, img.implicitHeight || 160)) : 0

                    Image {
                        id: img
                        anchors.fill: parent
                        source: imageEmbed.visible ? root.fileUrlFromPath(root.attachmentPath) : ""
                        fillMode: Image.PreserveAspectFit
                        asynchronous: true
                        cache: true
                        smooth: true
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        acceptedButtons: Qt.LeftButton | Qt.RightButton
                        onClicked: function(mouse) {
                            if (mouse.button === Qt.RightButton)
                                menu.popup()
                            else
                                root.openAttachmentRequested(root.attachmentPath)
                        }
                    }
                }

                // Inline video card — open full preview on click.
                Rectangle {
                    id: videoEmbed
                    visible: root.kind === "video" && root.attachmentPath !== ""
                    width: parent.width
                    height: visible ? 140 : 0
                    radius: Theme.radiusSm
                    color: Theme.bg0
                    border.color: Theme.bg3
                    border.width: 1

                    Column {
                        anchors.centerIn: parent
                        spacing: 8
                        Image {
                            anchors.horizontalCenter: parent.horizontalCenter
                            source: "qrc:/qt/qml/ConquerD/Client/icons/play.svg"
                            sourceSize.width: 28
                            sourceSize.height: 28
                            width: 28
                            height: 28
                        }
                        Text {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: root.attachmentName || "Video"
                            color: Theme.text
                            font.pixelSize: Theme.fontSizeCaption
                            elide: Text.ElideMiddle
                            width: videoEmbed.width - 24
                            horizontalAlignment: Text.AlignHCenter
                        }
                        Text {
                            anchors.horizontalCenter: parent.horizontalCenter
                            visible: root.sizeStr !== ""
                            text: root.sizeStr
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeCaption
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        acceptedButtons: Qt.LeftButton | Qt.RightButton
                        onClicked: function(mouse) {
                            if (mouse.button === Qt.RightButton)
                                menu.popup()
                            else
                                root.openAttachmentRequested(root.attachmentPath)
                        }
                    }
                }

                // Generic file attachment chip.
                Rectangle {
                    id: fileChip
                    visible: root.kind === "file" && root.attachmentPath !== ""
                    width: parent.width
                    height: visible ? fileChipRow.implicitHeight + 12 : 0
                    radius: Theme.radiusSm
                    color: root.mine ? Qt.rgba(0, 0, 0, 0.12) : Theme.bg1
                    border.color: Theme.bg3
                    border.width: 1

                    RowLayout {
                        id: fileChipRow
                        anchors {
                            left: parent.left
                            right: parent.right
                            verticalCenter: parent.verticalCenter
                            margins: 8
                        }
                        spacing: 8

                        Image {
                            source: "qrc:/qt/qml/ConquerD/Client/icons/attach.svg"
                            sourceSize.width: 16
                            sourceSize.height: 16
                            width: 16
                            height: 16
                            Layout.alignment: Qt.AlignVCenter
                        }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 1
                            Text {
                                text: root.attachmentName || root.body
                                color: root.mine ? Theme.textInv : Theme.text
                                font.pixelSize: Theme.fontSizeBody
                                elide: Text.ElideMiddle
                                Layout.fillWidth: true
                            }
                            Text {
                                visible: root.sizeStr !== ""
                                text: root.sizeStr
                                color: root.mine ? Theme.textInv : Theme.muted
                                opacity: 0.75
                                font.pixelSize: Theme.fontSizeCaption
                            }
                        }
                        ToolButton {
                            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/folder.svg"
                            icon.width: 14
                            icon.height: 14
                            icon.color: root.mine ? Theme.textInv : Theme.muted
                            implicitWidth: 28
                            implicitHeight: 24
                            flat: true
                            ToolTip.text: "Open"
                            ToolTip.visible: hovered
                            onClicked: root.openAttachmentRequested(root.attachmentPath)
                        }
                    }
                }

                Text {
                    id: bodyText
                    // Hide the text label when a media/file embed is showing;
                    // keep it for plain text and for attachments missing a path.
                    visible: root.attachmentPath === "" ||
                             (root.kind !== "image" && root.kind !== "video" && root.kind !== "file")
                    width: parent.width
                    text: root.richText(root.body)
                    textFormat: Text.RichText
                    color: root.mine ? Theme.textInv : Theme.text
                    font.pixelSize: Theme.fontSizeBody
                    wrapMode: Text.Wrap
                    onLinkActivated: (link) => Qt.openUrlExternally(link)
                }
            }

            MouseArea {
                anchors.fill: parent
                acceptedButtons: Qt.RightButton
                z: -1
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
            border.color: kindName === "youtube" ? Theme.danger : Theme.bg3
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
                        font.pixelSize: Theme.fontSizeCaption
                        font.bold: true
                    }
                    Label {
                        text: root.previewUrl()
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
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
                text: root.timestampText()
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
            }

            Text {
                visible: root.mine
                text: root.statusText()
                color: root.status === "failed" ? Theme.danger : Theme.muted
                font.pixelSize: Theme.fontSizeCaption
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
            font.pixelSize: Theme.fontSizeBody
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
            text: "Open Attachment"
            visible: root.attachmentPath !== ""
            onTriggered: root.openAttachmentRequested(root.attachmentPath)
        }
        MenuItem {
            text: "Open in System App"
            visible: root.attachmentPath !== ""
            onTriggered: Qt.openUrlExternally(root.fileUrlFromPath(root.attachmentPath))
        }
        MenuItem {
            text: "Delete Message"
            visible: root.allowDelete && root.msgId !== ""
            onTriggered: root.deleteRequested(root.msgId)
        }
    }
}
