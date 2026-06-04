// OnboardingWizard.qml — Multi-step onboarding dialog.
//
// Steps:
//   0: Welcome
//   1: Identity (enter a display handle)
//   2: Invite  (generate first invite link)
//   3: Done
//
// Usage:
//   OnboardingWizard {
//       id: wizard
//       settingsModel: settingsModel
//   }
//   // Open on first launch: wizard.open()

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Dialog {
    id: root

    required property var settingsModel

    title: "Welcome to ConquerD"
    modal: true
    closePolicy: Dialog.NoAutoClose   // force user through wizard
    width: 520
    height: 400

    background: Rectangle {
        color: Theme.bg1
        radius: 0
        border.color: Theme.bg3
        border.width: 1
    }

    property int step: 0
    property string generatedInvite: ""
    // Track whether at least one setup action has been taken on step 3.
    property bool _setupDone: false

    contentItem: StackLayout {
        currentIndex: root.step

        // ── Step 0: Welcome ───────────────────────────────────────────────
        ColumnLayout {
            spacing: 20

            Item { Layout.fillHeight: true }

            Image {
                source: "qrc:/qt/qml/ConquerD/Client/icons/lock.svg"
                sourceSize.width: 48
                sourceSize.height: 48
                width: 48
                height: 48
                fillMode: Image.PreserveAspectFit
                Layout.alignment: Qt.AlignHCenter
            }
            Text {
                text: "Welcome to ConquerD"
                color: Theme.text
                font.pixelSize: Theme.fontSizeBody + 4
                font.bold: true
                Layout.alignment: Qt.AlignHCenter
            }
            Text {
                text: "A private, peer-to-peer voice and chat client.\n"
                    + "No servers, no accounts, no tracking.\n\n"
                    + "Let's get you set up in three steps."
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.Wrap
                Layout.alignment: Qt.AlignHCenter
                Layout.fillWidth: true
            }

            Item { Layout.fillHeight: true }
        }

        // ── Step 1: Identity ──────────────────────────────────────────────
        ColumnLayout {
            spacing: 16

            Item { Layout.fillHeight: true }

            Text {
                text: "Your Identity"
                color: Theme.text
                font.pixelSize: Theme.fontSizeBody + 2
                font.bold: true
                Layout.alignment: Qt.AlignHCenter
            }
            Text {
                text: "Choose a display name. This is shown to people you invite.\n"
                    + "You can change it later in Settings."
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.Wrap
                Layout.alignment: Qt.AlignHCenter
                Layout.fillWidth: true
            }

            TextField {
                id: handleField
                placeholderText: "e.g. Alice"
                Layout.alignment: Qt.AlignHCenter
                width: 260
                color: Theme.text
                font.pixelSize: Theme.fontSizeCaption
                background: Rectangle {
                    color: Theme.bg2; radius: 0
                    border.color: handleField.activeFocus ? Theme.accent : Theme.bg3
                    border.width: 1
                }
                onTextChanged: if (root.settingsModel) root.settingsModel.local_handle = text
            }

            Item { Layout.fillHeight: true }
        }

        // ── Step 2: Invite ────────────────────────────────────────────────
        ColumnLayout {
            spacing: 16

            Item { Layout.fillHeight: true }

            Text {
                text: "Invite Someone"
                color: Theme.text
                font.pixelSize: Theme.fontSizeBody + 2
                font.bold: true
                Layout.alignment: Qt.AlignHCenter
            }
            Text {
                text: "Share this invite link with a trusted contact.\n"
                    + "It expires after one use."
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.Wrap
                Layout.alignment: Qt.AlignHCenter
                Layout.fillWidth: true
            }

            Rectangle {
                color: Theme.bg2
                radius: 0
                border.color: Theme.bg3
                border.width: 1
                height: 44
                Layout.fillWidth: true
                Layout.leftMargin: 20
                Layout.rightMargin: 20

                Text {
                    id: inviteText
                    anchors { fill: parent; leftMargin: 10; rightMargin: 10 }
                    verticalAlignment: Text.AlignVCenter
                    color: root.generatedInvite !== "" ? Theme.text : Theme.muted
                    font.pixelSize: Theme.fontSizeCaption
                    text: root.generatedInvite !== ""
                        ? root.generatedInvite
                        : "Press Generate to create an invite…"
                    elide: Text.ElideMiddle
                }
            }

            RowLayout {
                Layout.alignment: Qt.AlignHCenter
                spacing: 12

                Button {
                    text: "Generate"
                    onClicked: {
                        root.generatedInvite = backend.generateInvite()
                    }
                }
                Button {
                    text: "Copy"
                    icon.source: "qrc:/qt/qml/ConquerD/Client/icons/clipboard.svg"
                    icon.width: 16
                    icon.height: 16
                    enabled: root.generatedInvite !== ""
                    onClicked: {
                        backend.copyToClipboard(root.generatedInvite)
                    }
                }
            }

            Item { Layout.fillHeight: true }
        }

        // ── Step 3: System Integration ────────────────────────────────────
        ColumnLayout {
            spacing: 16

            Item { Layout.fillHeight: true }

            Text {
                text: "System Integration"
                color: Theme.text
                font.pixelSize: Theme.fontSizeBody + 2
                font.bold: true
                Layout.alignment: Qt.AlignHCenter
            }
            Text {
                text: "Optional — you can skip this step and do it later in Settings."
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.Wrap
                Layout.alignment: Qt.AlignHCenter
                Layout.fillWidth: true
            }

            GridLayout {
                columns: 2
                columnSpacing: 12
                rowSpacing: 10
                Layout.alignment: Qt.AlignHCenter

                Button {
                    text: "Register conquerd:// links"
                    Layout.preferredWidth: 220
                    onClicked: {
                        if (backend) backend.registerUriScheme()
                        root._setupDone = true
                        uriLabel.text = "✓ Registered"
                    }
                }
                Text {
                    id: uriLabel
                    text: "Open invite links automatically in ConquerD"
                    color: Theme.muted
                    font.pixelSize: 11
                    wrapMode: Text.Wrap
                    Layout.preferredWidth: 200
                }

                Button {
                    text: "Create desktop shortcuts"
                    Layout.preferredWidth: 220
                    onClicked: {
                        if (backend) backend.createDesktopShortcuts()
                        root._setupDone = true
                        shortcutLabel.text = "✓ Created"
                    }
                }
                Text {
                    id: shortcutLabel
                    text: "Add ConquerD to Desktop and Start Menu"
                    color: Theme.muted
                    font.pixelSize: 11
                    wrapMode: Text.Wrap
                    Layout.preferredWidth: 200
                }
            }

            Item { Layout.fillHeight: true }
        }

        // ── Step 4: Done ──────────────────────────────────────────────────
        ColumnLayout {
            spacing: 20

            Item { Layout.fillHeight: true }

            Text {
                text: "✓"
                font.pixelSize: 48
                color: Theme.online
                Layout.alignment: Qt.AlignHCenter
            }
            Text {
                text: "You're ready!"
                color: Theme.text
                font.pixelSize: Theme.fontSizeBody + 4
                font.bold: true
                Layout.alignment: Qt.AlignHCenter
            }
            Text {
                text: "Share the invite link and wait for your contact to connect.\n"
                    + "ConquerD will notify you when they accept."
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.Wrap
                Layout.alignment: Qt.AlignHCenter
                Layout.fillWidth: true
            }

            Item { Layout.fillHeight: true }
        }
    }

    // ── Progress dots ─────────────────────────────────────────────────────────
    footer: ColumnLayout {
        spacing: 8

        // Step dots
        Row {
            Layout.alignment: Qt.AlignHCenter
            spacing: 8

            Repeater {
                model: 5
                Rectangle {
                    width: index === root.step ? 16 : 8; height: 8
                    radius: 0
                    color: index === root.step ? Theme.accent : Theme.bg3
                    Behavior on width { NumberAnimation { duration: 150 } }
                }
            }
        }

        // Navigation buttons
        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            Layout.bottomMargin: 8
            spacing: 8

            Button {
                text: "Back"
                visible: root.step > 0 && root.step < 4
                onClicked: root.step = Math.max(0, root.step - 1)
                flat: true
            }

            Item { Layout.fillWidth: true }

            Button {
                id: nextBtn
                text: root.step === 4 ? "Finish" : root.step === 3 ? "Skip" : "Next"
                enabled: root.step !== 1 || (handleField.text.trim().length > 0)
                highlighted: true
                onClicked: {
                    if (root.step < 4) {
                        if (root.step === 1 && root.settingsModel) {
                            root.settingsModel.save()
                        }
                        root.step += 1
                    } else {
                        root.close()
                    }
                }
            }
        }
    }
}
