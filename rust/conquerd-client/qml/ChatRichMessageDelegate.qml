import QtQuick
import QtQuick.Controls
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
    signal transferAcceptRequested(string transferId)
    signal transferRejectRequested(string transferId)

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
    /// Live FileTransferModel so progress/accept live in this bubble.
    property var fileTransferModel: null

    readonly property bool isAttachment: root.kind === "image"
        || root.kind === "video"
        || root.kind === "file"
        || root.attachmentPath !== ""

    readonly property string transferId: {
        var id = root.msgId || ""
        return id.indexOf("xfer-") === 0 ? id.substring(5) : ""
    }
    property string xferState: ""
    property real xferProgress: 0
    readonly property bool xferLive: root.xferState === "pending" || root.xferState === "active"
    readonly property bool mediaPreviewReady: (root.kind === "image" || root.kind === "video")
        && root.attachmentPath !== ""
        && !root.xferLive

    function refreshTransfer() {
        if (!root.fileTransferModel || root.transferId === "") {
            root.xferState = ""
            root.xferProgress = 0
            return
        }
        root.xferState = root.fileTransferModel.stateFor(root.transferId)
        root.xferProgress = root.fileTransferModel.progressFor(root.transferId)
    }

    onTransferIdChanged: root.refreshTransfer()
    onFileTransferModelChanged: root.refreshTransfer()
    Component.onCompleted: root.refreshTransfer()

    Connections {
        target: root.fileTransferModel
        ignoreUnknownSignals: true
        function onModelReset() { root.refreshTransfer() }
        function onRowsInserted() { root.refreshTransfer() }
        function onRowsRemoved() { root.refreshTransfer() }
        function onDataChanged() { root.refreshTransfer() }
    }

    /// Session-local state for invite embeds in this message (not persisted).
    property bool inviteIgnored: false
    property bool inviteAccepted: false

    readonly property string inviteUrl: root.conquerdInviteUrl(root.body)
    readonly property bool hasInvite: root.inviteUrl !== ""
    readonly property string inviteKind: root.conquerdInviteKind(root.inviteUrl)
    readonly property string bodyWithoutInvite: root.stripInviteUrl(root.body, root.inviteUrl)
    readonly property bool showInviteEmbed: root.hasInvite && !root.inviteIgnored

    // ListView reuses delegates — clear per-message invite UI state on rebind.
    onMsgIdChanged: {
        inviteIgnored = false
        inviteAccepted = false
    }
    onBodyChanged: {
        inviteIgnored = false
        inviteAccepted = false
    }

    function fileUrlFromPath(p) {
        if (!p || p === "") return ""
        if (p.indexOf("file:") === 0) return p
        var n = p.replace(/\\/g, "/")
        if (n.charAt(0) !== "/") n = "/" + n
        return "file://" + n
    }

    /// First `conquerd://…` token in a chat body (peer, room, or legacy bare).
    function conquerdInviteUrl(value) {
        var m = (value || "").match(/conquerd:\/\/[^\s<>"']+/i)
        return m ? m[0] : ""
    }

    function conquerdInviteKind(url) {
        var u = (url || "").toLowerCase()
        if (u === "")
            return ""
        if (u.indexOf("conquerd://room#") === 0 || u.indexOf("://room#") >= 0)
            return "room"
        if (u.indexOf("conquerd://invite#") === 0 || u.indexOf("://invite#") >= 0)
            return "peer"
        return "invite"
    }

    function stripInviteUrl(value, url) {
        if (!url || url === "")
            return value || ""
        return (value || "").split(url).join("").replace(/\s+/g, " ").trim()
    }

    function inviteTitle() {
        if (root.inviteKind === "room")
            return root.mine ? "Room invite shared" : "Room invite"
        if (root.inviteKind === "peer")
            return root.mine ? "Peer invite shared" : "Peer invite"
        return root.mine ? "Invite shared" : "ConquerD invite"
    }

    function inviteSubtitle() {
        if (root.inviteAccepted)
            return "Accepting…"
        if (root.mine)
            return "They can Accept from their chat to connect."
        if (root.inviteKind === "room")
            return "Join this room on a trusted supernode."
        if (root.inviteKind === "peer")
            return "Add this peer to your trusted list."
        return "Open this invite to connect."
    }

    function inviteUrlShort() {
        var u = root.inviteUrl
        if (u.length <= 42)
            return u
        return u.substring(0, 28) + "…" + u.substring(u.length - 10)
    }

    function acceptInvite() {
        if (!root.hasInvite || root.inviteAccepted)
            return
        root.inviteAccepted = true
        if (typeof backend !== "undefined" && backend && backend.pasteInvite)
            backend.pasteInvite(root.inviteUrl)
    }

    function ignoreInvite() {
        root.inviteIgnored = true
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
        // Linkify conquerd:// invites so a leftover URL still routes in-app
        // (Accept embed is preferred when the full message is an invite).
        text = text.replace(/(conquerd:\/\/[^\s<>"]+)/gi,
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

    /// Plain text suitable for clipboard (full message, not rich HTML).
    function copyableText() {
        if (root.body && root.body !== "")
            return root.body
        if (root.attachmentName && root.attachmentName !== "")
            return root.attachmentName
        return ""
    }

    function copyEntireMessage() {
        var t = root.copyableText()
        if (t !== "")
            root.copyRequested(t)
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
                    visible: root.kind === "image" && root.mediaPreviewReady
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
                    visible: root.kind === "video" && root.mediaPreviewReady
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

                // File / in-progress transfer card. Images and videos switch
                // to their embeds once the bytes are on disk; this card holds
                // progress and accept/reject in the message flow.
                Rectangle {
                    id: fileChip
                    visible: root.isAttachment && !root.mediaPreviewReady
                    width: parent.width
                    height: visible ? fileChipCol.implicitHeight + 12 : 0
                    radius: Theme.radiusSm
                    color: root.mine ? Qt.rgba(0, 0, 0, 0.12) : Theme.bg1
                    border.color: Theme.bg3
                    border.width: 1

                    ColumnLayout {
                        id: fileChipCol
                        anchors {
                            left: parent.left
                            right: parent.right
                            top: parent.top
                            margins: 8
                        }
                        spacing: 6

                        RowLayout {
                            Layout.fillWidth: true
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
                                    visible: fileChip.statusLine !== ""
                                    text: fileChip.statusLine
                                    color: root.xferState === "failed"
                                           ? Theme.danger
                                           : (root.mine ? Theme.textInv : Theme.muted)
                                    opacity: 0.8
                                    font.pixelSize: Theme.fontSizeCaption
                                    elide: Text.ElideRight
                                    Layout.fillWidth: true
                                }
                            }
                            ToolButton {
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/check.svg"
                                icon.width: 14
                                icon.height: 14
                                icon.color: Theme.online
                                implicitWidth: 28
                                implicitHeight: 24
                                flat: true
                                visible: !root.mine && root.xferState === "pending"
                                ToolTip.text: root.isRoom ? qsTr("Download") : qsTr("Accept")
                                ToolTip.visible: hovered
                                onClicked: {
                                    root.xferState = "active"
                                    root.xferProgress = Math.max(root.xferProgress, 0.01)
                                    root.transferAcceptRequested(root.transferId)
                                }
                            }
                            ToolButton {
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/x-circle.svg"
                                icon.width: 14
                                icon.height: 14
                                icon.color: Theme.danger
                                implicitWidth: 28
                                implicitHeight: 24
                                flat: true
                                visible: root.xferState === "pending"
                                         || (root.xferState === "active" && !root.isRoom)
                                ToolTip.text: root.mine ? qsTr("Cancel") : qsTr("Decline")
                                ToolTip.visible: hovered
                                onClicked: root.transferRejectRequested(root.transferId)
                            }
                            ToolButton {
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/folder.svg"
                                icon.width: 14
                                icon.height: 14
                                icon.color: root.mine ? Theme.textInv : Theme.muted
                                implicitWidth: 28
                                implicitHeight: 24
                                flat: true
                                visible: root.attachmentPath !== "" && !root.xferLive
                                ToolTip.text: qsTr("Open")
                                ToolTip.visible: hovered
                                onClicked: root.openAttachmentRequested(root.attachmentPath)
                            }
                        }

                        ProgressBar {
                            Layout.fillWidth: true
                            from: 0.0
                            to: 1.0
                            value: root.xferProgress
                            visible: root.xferState === "active"
                        }
                    }

                    readonly property string statusLine: {
                        if (root.xferState === "pending")
                            return root.mine
                                   ? qsTr("Waiting for them to accept")
                                   : (root.sizeStr !== ""
                                      ? root.sizeStr + " · " + qsTr("Offered")
                                      : qsTr("Offered"))
                        if (root.xferState === "active") {
                            var pct = Math.round(root.xferProgress * 100)
                            return (root.sizeStr !== "" ? root.sizeStr + " · " : "") + pct + "%"
                        }
                        if (root.xferState === "failed")
                            return qsTr("Failed")
                        if (root.xferState === "done")
                            return root.sizeStr !== "" ? root.sizeStr : qsTr("Complete")
                        return root.sizeStr
                    }
                }

                // ConquerD invite embed — Accept / Ignore for inbound links.
                Rectangle {
                    id: inviteEmbed
                    visible: root.showInviteEmbed
                    width: parent.width
                    height: visible ? inviteEmbedCol.implicitHeight + 16 : 0
                    radius: Theme.radiusSm
                    color: root.mine ? Qt.rgba(0, 0, 0, 0.14) : Theme.bg1
                    border.color: root.mine ? Qt.rgba(255, 255, 255, 0.12) : Theme.bg3
                    border.width: 1
                    clip: true

                    ColumnLayout {
                        id: inviteEmbedCol
                        anchors {
                            left: parent.left
                            right: parent.right
                            top: parent.top
                            margins: 10
                        }
                        spacing: 8

                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 10

                            Rectangle {
                                Layout.preferredWidth: 36
                                Layout.preferredHeight: 36
                                radius: Theme.radiusSm
                                color: root.mine ? Qt.rgba(255, 255, 255, 0.12) : Theme.bg2
                                border.color: root.mine ? Qt.rgba(255, 255, 255, 0.08) : Theme.bg3
                                border.width: 1

                                Image {
                                    anchors.centerIn: parent
                                    source: root.inviteKind === "room"
                                        ? "qrc:/qt/qml/ConquerD/Client/icons/handshake.svg"
                                        : "qrc:/qt/qml/ConquerD/Client/icons/invite.svg"
                                    sourceSize.width: 18
                                    sourceSize.height: 18
                                    width: 18
                                    height: 18
                                    fillMode: Image.PreserveAspectFit
                                    opacity: 0.95
                                }
                            }

                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 2

                                Text {
                                    text: root.inviteTitle()
                                    color: root.mine ? Theme.textInv : Theme.text
                                    font.pixelSize: Theme.fontSizeBody
                                    font.bold: true
                                    elide: Text.ElideRight
                                    Layout.fillWidth: true
                                }
                                Text {
                                    text: root.inviteSubtitle()
                                    color: root.mine ? Theme.textInv : Theme.muted
                                    opacity: root.mine ? 0.8 : 1.0
                                    font.pixelSize: Theme.fontSizeCaption
                                    wrapMode: Text.WordWrap
                                    Layout.fillWidth: true
                                }
                                Text {
                                    text: root.inviteUrlShort()
                                    color: root.mine ? Theme.textInv : Theme.muted
                                    opacity: 0.65
                                    font.pixelSize: Theme.fontSizeMicro
                                    font.family: "Consolas, monospace"
                                    elide: Text.ElideMiddle
                                    Layout.fillWidth: true
                                }
                            }
                        }

                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 8

                            // Inbound: Accept / Ignore. Outbound: Copy only.
                            StyledButton {
                                visible: !root.mine
                                enabled: !root.inviteAccepted
                                text: root.inviteAccepted ? "Accepted" : "Accept"
                                primary: true
                                compact: true
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/check.svg"
                                onClicked: root.acceptInvite()
                            }
                            StyledButton {
                                visible: !root.mine && !root.inviteAccepted
                                text: "Ignore"
                                compact: true
                                flat: true
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                                onClicked: root.ignoreInvite()
                            }
                            StyledButton {
                                visible: root.mine || root.inviteAccepted
                                text: "Copy link"
                                compact: true
                                flat: true
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/clipboard.svg"
                                onClicked: root.copyRequested(root.inviteUrl)
                            }
                            Item { Layout.fillWidth: true }
                        }
                    }
                }

                // After Ignore: quiet status line (session-only).
                Text {
                    visible: root.hasInvite && root.inviteIgnored && !root.mine
                    width: parent.width
                    text: "Invite ignored"
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeCaption
                    font.italic: true
                }

                // Read-only selectable body so users can drag-select and Ctrl+C.
                TextEdit {
                    id: bodyText
                    // Hide when media/file embeds take over, or when the body is
                    // only a conquerd:// invite (the invite card / ignored line
                    // replaces the raw URL).
                    visible: {
                        if (root.kind === "image" || root.kind === "video" || root.kind === "file")
                            return false
                        if (root.hasInvite && root.bodyWithoutInvite === "")
                            return false
                        return true
                    }
                    width: parent.width
                    height: visible ? contentHeight : 0
                    readOnly: true
                    selectByMouse: true
                    selectByKeyboard: true
                    activeFocusOnPress: true
                    cursorVisible: false
                    text: root.richText(
                        root.hasInvite && root.bodyWithoutInvite !== ""
                            ? root.bodyWithoutInvite
                            : root.body
                    )
                    textFormat: TextEdit.RichText
                    color: root.mine ? Theme.textInv : Theme.text
                    selectedTextColor: root.mine ? Theme.accent : Theme.textInv
                    selectionColor: root.mine ? Qt.rgba(255, 255, 255, 0.35) : Theme.accent
                    font.pixelSize: Theme.fontSizeBody
                    wrapMode: TextEdit.Wrap
                    onLinkActivated: (link) => {
                        // Route conquerd:// through the invite path instead of
                        // the system browser (which cannot open the scheme).
                        if ((link || "").toLowerCase().indexOf("conquerd://") === 0) {
                            if (typeof backend !== "undefined" && backend && backend.pasteInvite)
                                backend.pasteInvite(link)
                            return
                        }
                        Qt.openUrlExternally(link)
                    }

                    // Right-click opens our message menu without blocking
                    // left-drag selection (TapHandler composes with TextEdit).
                    TapHandler {
                        acceptedButtons: Qt.RightButton
                        onTapped: menu.popup()
                    }
                }
            }

            MouseArea {
                anchors.fill: parent
                acceptedButtons: Qt.RightButton
                // Sit behind bodyText / controls so selection and buttons win;
                // still catch right-click on empty bubble chrome (attachments, padding).
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
                anchors.verticalCenter: parent.verticalCenter
            }

            Text {
                visible: root.mine
                text: root.statusText()
                color: root.status === "failed" ? Theme.danger : Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                anchors.verticalCenter: parent.verticalCenter
            }

            ToolButton {
                visible: root.copyableText() !== ""
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/clipboard.svg"
                icon.width: 14
                icon.height: 14
                icon.color: Theme.muted
                implicitWidth: 22
                implicitHeight: 22
                padding: 4
                ToolTip.text: "Copy message"
                ToolTip.visible: hovered
                onClicked: root.copyEntireMessage()
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
            text: "Copy Selection"
            enabled: bodyText.visible && bodyText.selectedText !== ""
            onTriggered: root.copyRequested(bodyText.selectedText)
        }
        MenuItem {
            text: "Copy Message"
            enabled: root.copyableText() !== ""
            onTriggered: root.copyEntireMessage()
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
