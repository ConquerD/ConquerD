// PassphraseDialog.qml — Shown when the identity requires a passphrase and/or keyfile.

import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts
import QtQuick.Dialogs
import ConquerD.Client 1.0

Item {
    id: root
    anchors.fill: parent
    visible: false
    z: 100

    property bool isNew: false
    property string errorText: ""
    property string _selectedFilePath: ""

    // Name the actual store rather than saying "the OS keyring": the user can
    // only judge the trade-off if they know where the key lands, and it is the
    // place they would go to remove it by hand.
    readonly property string _keyringName:
        Qt.platform.os === "windows" ? "Windows Credential Manager"
        : Qt.platform.os === "osx"   ? "the macOS Keychain"
                                     : "your desktop keyring"

    signal submitted(string passphrase, string filePath, bool remember)

    FileDialog {
        id: filePickerDialog
        title: "Choose keyfile"
        onAccepted: {
            const s = filePickerDialog.selectedFile.toString()
            const path = decodeURIComponent(
                Qt.platform.os === "windows" ? s.slice(8) : s.slice(7)
            )
            root._selectedFilePath = path
        }
    }

    Rectangle {
        anchors.fill: parent
        color: Theme.overlayScrim
        opacity: 0.75
    }

    Rectangle {
        anchors.centerIn: parent
        width: 400
        height: column.implicitHeight + Theme.spacingXl * 2
        radius: Theme.radiusMd
        color: Theme.bg2
        border.color: Theme.accent
        border.width: 1

        ColumnLayout {
            id: column
            anchors {
                left: parent.left
                right: parent.right
                top: parent.top
                margins: Theme.spacingXl
            }
            spacing: Theme.spacingLg

            Text {
                Layout.fillWidth: true
                text: root.isNew ? "Create your DoubleSlash identity"
                                 : "Unlock your DoubleSlash identity"
                font.pixelSize: Theme.fontSizeDialog
                font.bold: true
                color: Theme.text
                horizontalAlignment: Text.AlignHCenter
            }

            Text {
                Layout.fillWidth: true
                text: root.isNew
                      ? "Protect your private key with a passphrase, a keyfile, or both.\nYou will need the same input every time you log in."
                      : "Enter your passphrase and/or keyfile to decrypt your private key."
                font.pixelSize: Theme.fontSizeBody
                color: Theme.muted
                wrapMode: Text.WordWrap
                horizontalAlignment: Text.AlignHCenter
            }

            Text {
                Layout.fillWidth: true
                visible: root.errorText !== ""
                text: root.errorText
                font.pixelSize: Theme.fontSizeBody
                color: Theme.danger
                wrapMode: Text.WordWrap
                horizontalAlignment: Text.AlignHCenter
            }

            StyledTextField {
                id: passphraseField
                Layout.fillWidth: true
                placeholderText: "Passphrase (optional with keyfile)"
                echoMode: TextInput.Password
                Keys.onReturnPressed: root._submit()
            }

            StyledTextField {
                id: confirmField
                Layout.fillWidth: true
                visible: root.isNew && passphraseField.text.length > 0
                placeholderText: "Confirm passphrase"
                echoMode: TextInput.Password
                Keys.onReturnPressed: root._submit()
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingSm

                StyledButton {
                    text: "Choose keyfile\u2026"
                    onClicked: filePickerDialog.open()
                }

                Text {
                    Layout.fillWidth: true
                    text: root._selectedFilePath !== ""
                          ? root._selectedFilePath.split(/[\\/]/).pop()
                          : "No keyfile selected"
                    color: root._selectedFilePath !== "" ? Theme.text : Theme.muted
                    font.pixelSize: Theme.fontSizeCaption
                    elide: Text.ElideMiddle
                }

                ToolButton {
                    visible: root._selectedFilePath !== ""
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/close.svg"
                    icon.width: 14
                    icon.height: 14
                    icon.color: Theme.muted
                    implicitWidth: Theme.controlHeight
                    implicitHeight: Theme.controlHeight
                    onClicked: root._selectedFilePath = ""
                    ToolTip.visible: hovered
                    ToolTip.text: "Clear keyfile"
                    ToolTip.delay: 400
                }
            }

            // ── Optional auto-unlock ──────────────────────────────────
            //
            // Off by default and never implied: leaving this alone keeps the
            // behaviour every existing user already has. The body text spells
            // out both sides rather than selling the convenience, because the
            // cost is real - the key sits in the OS store for anything running
            // as this user to read.
            ColumnLayout {
                Layout.fillWidth: true
                spacing: Theme.spacingXs

                CheckBox {
                    id: rememberBox
                    text: "Stay unlocked on this device (optional)"
                    checked: false
                    font.pixelSize: Theme.fontSizeBody
                }

                Text {
                    Layout.fillWidth: true
                    Layout.leftMargin: Theme.spacingLg
                    text: rememberBox.checked
                          ? "On: DoubleSlash opens without asking for this passphrase again on this device. Your key is stored in " + root._keyringName + ", so anyone who can use your account here - or any program running as you - can open your identity. Your passphrase itself is never stored."
                          : "Off: you type this passphrase every launch. Your identity file is useless to anyone who copies it without the passphrase."
                    font.pixelSize: Theme.fontSizeCaption
                    color: Theme.muted
                    wrapMode: Text.WordWrap
                }
            }

            StyledButton {
                Layout.fillWidth: true
                text: root.isNew ? "Create identity" : "Unlock"
                primary: true
                onClicked: root._submit()
            }
        }
    }

    function _submit() {
        const pass = passphraseField.text
        const file = root._selectedFilePath

        if (pass.length === 0 && file === "") {
            root.errorText = "Please enter a passphrase, choose a keyfile, or both."
            return
        }

        if (root.isNew && pass.length > 0 && pass !== confirmField.text) {
            root.errorText = "Passphrases do not match."
            return
        }

        root.errorText = ""
        root.submitted(pass, file, rememberBox.checked)
        passphraseField.text = ""
        confirmField.text = ""
        root._selectedFilePath = ""
    }

    onVisibleChanged: {
        if (visible) {
            passphraseField.forceActiveFocus()
        }
    }
}