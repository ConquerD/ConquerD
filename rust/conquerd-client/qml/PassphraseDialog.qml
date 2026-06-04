// PassphraseDialog.qml — Shown when the identity requires a passphrase and/or keyfile.
//
// Covers the full window with a modal overlay. Supports three modes:
//   text-only, keyfile-only, or both combined (any combination).
// The combined key material feeds into Argon2id on the Rust side.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import QtQuick.Dialogs

Item {
    id: root
    anchors.fill: parent
    visible: false
    z: 100  // Always on top

    // Set to true when creating a new identity (no existing file)
    property bool isNew: false
    // Banner text forwarded from backend (e.g. "Incorrect passphrase")
    property string errorText: ""
    // Currently selected keyfile OS path (empty = no keyfile)
    property string _selectedFilePath: ""

    // Emits both the text passphrase and the keyfile path (either may be empty).
    signal submitted(string passphrase, string filePath)

    // ── Native file picker ────────────────────────────────────────────────
    FileDialog {
        id: filePickerDialog
        title: "Choose keyfile"
        onAccepted: {
            // Convert Qt file:// URL to a local OS path.
            // file:///C:/foo  (Windows)  -> slice 8 chars -> C:/foo
            // file:///home/x  (Unix)     -> slice 7 chars -> /home/x
            const s = filePickerDialog.selectedFile.toString()
            const path = decodeURIComponent(
                Qt.platform.os === "windows" ? s.slice(8) : s.slice(7)
            )
            root._selectedFilePath = path
        }
    }

    // Semi-transparent dark backdrop
    Rectangle {
        anchors.fill: parent
        color: "#000000"
        opacity: 0.75
    }

    // Centered card
    Rectangle {
        anchors.centerIn: parent
        width: 400
        height: column.implicitHeight + 48
        radius: 0
        color: "#2B2D31"
        border.color: "#5865F2"
        border.width: 1

        ColumnLayout {
            id: column
            anchors {
                left: parent.left
                right: parent.right
                top: parent.top
                margins: 24
            }
            spacing: 16

            Text {
                Layout.fillWidth: true
                text: root.isNew ? "Create your ConquerD identity"
                                 : "Unlock your ConquerD identity"
                font.pixelSize: 18
                font.bold: true
                color: "#DCDDDE"
                horizontalAlignment: Text.AlignHCenter
            }

            Text {
                Layout.fillWidth: true
                text: root.isNew
                      ? "Protect your private key with a passphrase, a keyfile, or both.\nYou will need the same input every time you log in."
                      : "Enter your passphrase and/or keyfile to decrypt your private key."
                font.pixelSize: 13
                color: "#B9BBBE"
                wrapMode: Text.WordWrap
                horizontalAlignment: Text.AlignHCenter
            }

            // Error text (shown after wrong passphrase/keyfile)
            Text {
                Layout.fillWidth: true
                visible: root.errorText !== ""
                text: root.errorText
                font.pixelSize: 13
                color: "#ED4245"
                wrapMode: Text.WordWrap
                horizontalAlignment: Text.AlignHCenter
            }

            // ── Text passphrase ───────────────────────────────────────────
            TextField {
                id: passphraseField
                Layout.fillWidth: true
                placeholderText: "Passphrase (optional with keyfile)"
                echoMode: TextInput.Password
                Material.accent: Material.Blue
                background: Rectangle {
                    radius: 0
                    color: "#383A40"
                    border.color: passphraseField.activeFocus ? "#5865F2" : "#4F545C"
                }
                color: "#DCDDDE"
                Keys.onReturnPressed: root._submit()
            }

            // Confirm field — only when creating AND text passphrase is non-empty
            TextField {
                id: confirmField
                Layout.fillWidth: true
                visible: root.isNew && passphraseField.text.length > 0
                placeholderText: "Confirm passphrase"
                echoMode: TextInput.Password
                Material.accent: Material.Blue
                background: Rectangle {
                    radius: 0
                    color: "#383A40"
                    border.color: confirmField.activeFocus ? "#5865F2" : "#4F545C"
                }
                color: "#DCDDDE"
                Keys.onReturnPressed: root._submit()
            }

            // ── Keyfile row ───────────────────────────────────────────────
            RowLayout {
                Layout.fillWidth: true
                spacing: 8

                Button {
                    text: "Choose keyfile\u2026"
                    Material.background: "#383A40"
                    onClicked: filePickerDialog.open()
                }

                Text {
                    Layout.fillWidth: true
                    text: root._selectedFilePath !== ""
                          ? root._selectedFilePath.split(/[\\/]/).pop()
                          : "No keyfile selected"
                    color: root._selectedFilePath !== "" ? "#DCDDDE" : "#72767D"
                    font.pixelSize: 12
                    elide: Text.ElideMiddle
                }

                // Clear keyfile button
                ToolButton {
                    visible: root._selectedFilePath !== ""
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                    icon.width: 14
                    icon.height: 14
                    icon.color: Theme.muted
                    implicitWidth: 24
                    implicitHeight: 24
                    onClicked: root._selectedFilePath = ""
                    ToolTip.visible: hovered
                    ToolTip.text: "Clear keyfile"
                    ToolTip.delay: 400
                }
            }

            Button {
                Layout.fillWidth: true
                text: root.isNew ? "Create identity" : "Unlock"
                Material.background: Material.Blue
                onClicked: root._submit()
            }
        }
    }

    function _submit() {
        const pass = passphraseField.text
        const file = root._selectedFilePath

        // At least one input must be set
        if (pass.length === 0 && file === "") {
            root.errorText = "Please enter a passphrase, choose a keyfile, or both."
            return
        }

        // For new identities, require passphrase confirmation when text is used
        if (root.isNew && pass.length > 0 && pass !== confirmField.text) {
            root.errorText = "Passphrases do not match."
            return
        }

        root.errorText = ""
        root.submitted(pass, file)
        passphraseField.text = ""
        confirmField.text = ""
        root._selectedFilePath = ""
    }

    // Focus passphrase field whenever dialog becomes visible
    onVisibleChanged: {
        if (visible) {
            passphraseField.forceActiveFocus()
        }
    }
}
