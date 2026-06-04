import QtQuick
import QtQuick.Controls.Material
import QtQuick.Dialogs
import QtQuick.Layouts

Rectangle {
    id: root

    signal sendMessage(string message)
    signal sendFile(string fileUrl)
    signal composing(bool active)
    signal askAi(string prompt)

    property string targetName: ""
    property bool enabledForTarget: true
    property bool fileTransferEnabled: true
    property string fileTransferTooltip: "Attach file"
    property bool aiEnabled: false
    property bool aiStreaming: false

    Layout.fillWidth: true
    implicitHeight: composerLayout.implicitHeight + 14
    color: Theme.bg3
    radius: 0

    function selectedText() {
        return composer.selectedText || ""
    }

    function wrapSelection(prefix, suffix) {
        var start = composer.selectionStart
        var end = composer.selectionEnd
        var before = composer.text.slice(0, start)
        var middle = composer.text.slice(start, end)
        var after = composer.text.slice(end)
        if (middle.length === 0) middle = "text"
        composer.text = before + prefix + middle + suffix + after
        composer.select(start + prefix.length, start + prefix.length + middle.length)
        composer.forceActiveFocus()
    }

    function insertLink() {
        var start = composer.selectionStart
        var end = composer.selectionEnd
        var label = composer.text.slice(start, end) || "link"
        var insert = label + " https://"
        composer.text = composer.text.slice(0, start) + insert + composer.text.slice(end)
        composer.cursorPosition = start + insert.length
        composer.forceActiveFocus()
    }

    function submit() {
        var value = composer.text.trim()
        if (value.length === 0 || !root.enabledForTarget) return
        root.sendMessage(value)
        composer.clear()
    }

    FileDialog {
        id: fileDialog
        title: "Attach File"
        onAccepted: root.sendFile(selectedFile.toString())
    }

    ColumnLayout {
        id: composerLayout
        anchors.fill: parent
        anchors.margins: 7
        spacing: 5

        RowLayout {
            Layout.fillWidth: true
            spacing: 2

            ToolButton {
                text: "B"
                enabled: root.enabledForTarget
                flat: true
                font.bold: true
                implicitWidth: 28
                implicitHeight: 24
                ToolTip.text: "Bold"
                ToolTip.visible: hovered
                onClicked: root.wrapSelection("**", "**")
            }
            ToolButton {
                text: "I"
                enabled: root.enabledForTarget
                flat: true
                font.italic: true
                implicitWidth: 28
                implicitHeight: 24
                ToolTip.text: "Italic"
                ToolTip.visible: hovered
                onClicked: root.wrapSelection("*", "*")
            }
            ToolButton {
                text: "U"
                enabled: root.enabledForTarget
                flat: true
                font.underline: true
                implicitWidth: 28
                implicitHeight: 24
                ToolTip.text: "Underline"
                ToolTip.visible: hovered
                onClicked: root.wrapSelection("__", "__")
            }
            ToolButton {
                text: "</>"
                enabled: root.enabledForTarget
                flat: true
                implicitWidth: 36
                implicitHeight: 24
                ToolTip.text: "Code"
                ToolTip.visible: hovered
                onClicked: root.wrapSelection("`", "`")
            }
            ToolButton {
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/chain.svg"
                icon.width: 14
                icon.height: 14
                icon.color: Theme.muted
                enabled: root.enabledForTarget
                flat: true
                implicitWidth: 28
                implicitHeight: 24
                ToolTip.text: "Link"
                ToolTip.visible: hovered
                onClicked: root.insertLink()
            }

            Item { Layout.fillWidth: true }

            ToolButton {
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/attach.svg"
                icon.width: 16
                icon.height: 16
                enabled: root.enabledForTarget && root.fileTransferEnabled
                flat: true
                implicitWidth: 32
                implicitHeight: 24
                ToolTip.text: root.fileTransferEnabled ? root.fileTransferTooltip : "Room file transfer is not available yet"
                ToolTip.visible: hovered
                onClicked: fileDialog.open()
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Item {
                Layout.fillWidth: true
                Layout.preferredHeight: Math.min(92, Math.max(34, composer.implicitHeight))

                TextArea {
                    id: composer
                    anchors.fill: parent
                    enabled: root.enabledForTarget
                    background: Item {}
                    color: Theme.text
                    wrapMode: TextEdit.Wrap
                    selectByMouse: true
                    onTextChanged: root.composing(text.length > 0)
                    Keys.onPressed: function(event) {
                        if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter)
                                && !(event.modifiers & Qt.ShiftModifier)) {
                            root.submit()
                            event.accepted = true
                        }
                    }
                }

                Text {
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    anchors.leftMargin: composer.leftPadding
                    visible: composer.text.length === 0
                    text: root.enabledForTarget ? "Message..." : "Select a chat"
                    color: Theme.muted
                    font.pixelSize: composer.font.pixelSize
                    elide: Text.ElideRight
                }
            }

            ToolButton {
                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/send.svg"
                icon.width: 16
                icon.height: 16
                icon.color: composer.text.trim() !== "" && root.enabledForTarget ? Theme.accent : Theme.muted
                enabled: composer.text.trim() !== "" && root.enabledForTarget
                flat: true
                implicitWidth: 32
                implicitHeight: 32
                ToolTip.text: "Send"
                ToolTip.visible: hovered
                onClicked: root.submit()
            }

            Button {
                text: root.aiStreaming ? "..." : "AI"
                visible: root.aiEnabled
                enabled: !root.aiStreaming
                    && composer.text.trim() !== ""
                    && root.enabledForTarget
                flat: true
                Material.foreground: Theme.accent
                ToolTip.text: "Ask local AI"
                ToolTip.visible: hovered
                onClicked: {
                    var value = composer.text.trim()
                    if (value.length === 0) return
                    root.askAi(value)
                    composer.clear()
                    root.composing(false)
                }
            }
        }
    }

    DropArea {
        anchors.fill: parent
        keys: ["text/uri-list"]
        enabled: root.enabledForTarget && root.fileTransferEnabled
        onDropped: function(drop) {
            if (drop.hasUrls && drop.urls.length > 0) {
                root.sendFile(drop.urls[0].toString())
            }
        }
    }
}
