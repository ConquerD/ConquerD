// PassphraseDialog.qml — Shown when the identity requires a passphrase and/or keyfile.

import QtQuick
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

    signal submitted(string passphrase, string filePath)

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
                text: root.isNew ? "Create your ConquerD identity"
                                 : "Unlock your ConquerD identity"
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
        root.submitted(pass, file)
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