// OnboardingWizard.qml - first-run setup for the native client.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Dialog {
    id: root

    required property var settingsModel
    required property var appBackend

    property int step: 0
    readonly property int stepCount: 5
    readonly property bool isWindows: Qt.platform.os === "windows"
    property string generatedInvite: ""
    property bool copiedPublicId: false
    property bool copiedInvite: false
    property string setupStatus: ""

    title: "Welcome to ConquerD"
    modal: true
    closePolicy: Dialog.NoAutoClose
    width: 680
    height: 580
    padding: 0

    function publicIdText() {
        return root.appBackend && root.appBackend.public_id ? String(root.appBackend.public_id) : ""
    }

    function shortPublicId() {
        var id = root.publicIdText()
        if (id.length <= 18) return id
        return id.substring(0, 10) + "..." + id.substring(id.length - 8)
    }

    function identityPreview() {
        var id = root.publicIdText()
        if (id.length === 0) return "Identity loading"
        var clean = id.replace(/[^A-Za-z0-9]/g, "")
        var max = Math.min(clean.length, 32)
        var parts = []
        for (var i = 0; i < max; i += 4)
            parts.push(clean.substring(i, Math.min(i + 4, max)))
        return parts.join(" ")
    }

    function saveHandle() {
        if (!root.settingsModel) return
        root.settingsModel.local_handle = handleField.text.trim()
        root.settingsModel.save()
    }

    function saveConnectivity() {
        if (!root.settingsModel) return
        var selectedPort = parseInt(p2pPortField.text, 10)
        if (isNaN(selectedPort) || selectedPort < 1 || selectedPort > 65535)
            selectedPort = 61045
        root.settingsModel.direct_p2p_enabled = directP2pOption.checked
        root.settingsModel.direct_p2p_port = selectedPort
        root.settingsModel.save()
        if (root.appBackend)
            root.appBackend.configureDirectP2p(directP2pOption.checked, selectedPort)
    }

    function finishWizard() {
        root.saveHandle()
        root.saveConnectivity()

        var invite = incomingInviteField.text.trim()
        if (invite.length > 0 && root.appBackend) {
            root.appBackend.pasteInvite(invite)
        }

        if (root.isWindows && root.appBackend) {
            var actions = []
            if (uriOption.checked) {
                root.appBackend.registerUriScheme()
                actions.push("links")
            }
            if (shortcutOption.checked) {
                root.appBackend.createDesktopShortcuts()
                actions.push("shortcuts")
            }
            root.setupStatus = actions.length > 0 ? "Applied " + actions.join(" and ") : ""
        }

        if (root.settingsModel) {
            root.settingsModel.onboarding_complete = true
            root.settingsModel.save()
        }

        root.close()
    }

    background: Rectangle {
        color: Theme.bg1
        radius: 0
        border.color: Theme.bg3
        border.width: 1
    }

    contentItem: ColumnLayout {
        spacing: 0

        Rectangle {
            Layout.fillWidth: true
            height: 72
            color: Theme.bg0

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: Theme.spacingLg
                anchors.rightMargin: Theme.spacingLg
                spacing: Theme.spacingMd

                Image {
                    source: "qrc:/qt/qml/ConquerD/Client/icons/logo.svg"
                    sourceSize.width: 36
                    sourceSize.height: 36
                    Layout.preferredWidth: 36
                    Layout.preferredHeight: 36
                    fillMode: Image.PreserveAspectFit
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2

                    Text {
                        text: "Set Up ConquerD"
                        color: Theme.text
                        font.pixelSize: Theme.fontSizeTitle
                        font.bold: true
                    }

                    Text {
                        text: root.step === 0 ? "Choose your local display name"
                            : root.step === 1 ? "Review this device identity"
                            : root.step === 2 ? "Choose direct or supernode connectivity"
                            : root.step === 3 ? "Connect now or create an invite"
                            : "Finish local integration"
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                    }
                }

                Text {
                    text: (root.step + 1) + " / " + root.stepCount
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeCaption
                }
            }
        }

        StackLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            currentIndex: root.step

            // Step 0: Welcome and display name.
            ColumnLayout {
                spacing: Theme.spacingMd
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.margins: Theme.spacingXl

                Item { Layout.fillHeight: true }

                Image {
                    source: "qrc:/qt/qml/ConquerD/Client/icons/lock.svg"
                    sourceSize.width: 52
                    sourceSize.height: 52
                    Layout.preferredWidth: 52
                    Layout.preferredHeight: 52
                    fillMode: Image.PreserveAspectFit
                    Layout.alignment: Qt.AlignHCenter
                }

                Text {
                    text: "Private by default"
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeBody + 5
                    font.bold: true
                    Layout.alignment: Qt.AlignHCenter
                }

                Text {
                    text: "Your identity stays on this device. Peers are added with signed invite links."
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeBody
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                    Layout.maximumWidth: 460
                    Layout.alignment: Qt.AlignHCenter
                }

                ColumnLayout {
                    Layout.alignment: Qt.AlignHCenter
                    Layout.preferredWidth: 320
                    spacing: Theme.spacingXs

                    Text {
                        text: "Display name"
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                        font.bold: true
                    }

                    StyledTextField {
                        id: handleField
                        Layout.fillWidth: true
                        placeholderText: "Enter your display name"
                        maximumLength: 64
                        text: root.settingsModel ? root.settingsModel.local_handle : ""
                        onTextChanged: if (root.settingsModel) root.settingsModel.local_handle = text
                        Keys.onReturnPressed: if (text.trim().length > 0) nextBtn.clicked()
                    }
                }

                Item { Layout.fillHeight: true }
            }

            // Step 1: Identity.
            ColumnLayout {
                spacing: Theme.spacingMd
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.margins: Theme.spacingXl

                Text {
                    text: "Your Identity"
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeBody + 4
                    font.bold: true
                }

                Text {
                    text: "This public ID is what other trusted peers see when you connect."
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeBody
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }

                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 156
                    color: Theme.bg2
                    border.color: Theme.bg3
                    border.width: 1

                    RowLayout {
                        anchors.fill: parent
                        anchors.margins: Theme.spacingMd
                        spacing: Theme.spacingMd

                        Avatar {
                            peerId: root.publicIdText()
                            size: 86
                            showRing: true
                            Layout.alignment: Qt.AlignVCenter
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: Theme.spacingSm

                            Text {
                                text: root.shortPublicId()
                                color: Theme.text
                                font.pixelSize: Theme.fontSizeBody
                                font.bold: true
                                elide: Text.ElideMiddle
                                Layout.fillWidth: true
                            }

                            Text {
                                text: root.identityPreview()
                                color: Theme.accent
                                font.family: "Consolas"
                                font.pixelSize: Theme.fontSizeCaption
                                wrapMode: Text.WrapAnywhere
                                Layout.fillWidth: true
                            }

                            TextArea {
                                text: root.publicIdText()
                                readOnly: true
                                selectByMouse: true
                                wrapMode: Text.WrapAnywhere
                                color: Theme.text
                                font.family: "Consolas"
                                font.pixelSize: Theme.fontSizeCaption
                                Layout.fillWidth: true
                                Layout.preferredHeight: 54
                                background: Rectangle {
                                    color: Theme.bg0
                                    border.color: Theme.bg3
                                    border.width: 1
                                }
                            }
                        }

                        StyledButton {
                            text: root.copiedPublicId ? "Copied" : "Copy"
                            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/clipboard.svg"
                            success: root.copiedPublicId
                            Layout.alignment: Qt.AlignTop
                            onClicked: {
                                if (root.appBackend) root.appBackend.copyToClipboard(root.publicIdText())
                                root.copiedPublicId = true
                            }
                        }
                    }
                }

                Item { Layout.fillHeight: true }
            }

            // Step 2: Direct P2P connectivity.
            ColumnLayout {
                spacing: Theme.spacingMd
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.margins: Theme.spacingXl

                Text {
                    text: "Direct P2P Connectivity"
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeBody + 4
                    font.bold: true
                }

                Text {
                    text: "Choose how this device should accept peer connections. Supernodes are optional and never act as identity authorities."
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeBody
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }

                ButtonGroup { id: connectivityGroup }

                RadioButton {
                    id: directP2pOption
                    text: "Enable direct peer-to-peer connections"
                    checked: root.settingsModel ? root.settingsModel.direct_p2p_enabled : true
                    ButtonGroup.group: connectivityGroup
                    font.pixelSize: Theme.fontSizeBody
                    Material.foreground: Theme.text
                }

                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: directP2pOption.checked ? 190 : 0
                    visible: directP2pOption.checked
                    color: Theme.bg2
                    border.color: Theme.bg3
                    border.width: 1

                    ColumnLayout {
                        anchors.fill: parent
                        anchors.margins: Theme.spacingMd
                        spacing: Theme.spacingSm

                        RowLayout {
                            Layout.fillWidth: true
                            spacing: Theme.spacingSm

                            Text {
                                text: "Preferred UDP port"
                                color: Theme.text
                                font.pixelSize: Theme.fontSizeBody
                                font.bold: true
                            }

                            Item { Layout.fillWidth: true }

                            StyledTextField {
                                id: p2pPortField
                                Layout.preferredWidth: 110
                                text: root.settingsModel ? String(root.settingsModel.direct_p2p_port) : "61045"
                                horizontalAlignment: TextInput.AlignHCenter
                                validator: IntValidator { bottom: 1; top: 65535 }
                                inputMethodHints: Qt.ImhDigitsOnly
                            }
                        }

                        Text {
                            text: "ConquerD reuses this port when available and tries the next port if another local client already uses it."
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeCaption
                            wrapMode: Text.WordWrap
                            Layout.fillWidth: true
                        }

                        Text {
                            text: "Firewall: allow ConquerD, or allow inbound and outbound UDP on this port. Prefer Private networks on a trusted LAN."
                            color: Theme.text
                            font.pixelSize: Theme.fontSizeCaption
                            wrapMode: Text.WordWrap
                            Layout.fillWidth: true
                        }

                        Text {
                            text: "Internet P2P: forward the same UDP port on your router to this computer and reserve its LAN address. Carrier-grade NAT may still require a supernode."
                            color: Theme.warn
                            font.pixelSize: Theme.fontSizeCaption
                            wrapMode: Text.WordWrap
                            Layout.fillWidth: true
                        }
                    }
                }

                RadioButton {
                    id: supernodeOption
                    text: "I plan to connect through a supernode — skip port setup"
                    checked: root.settingsModel ? !root.settingsModel.direct_p2p_enabled : false
                    ButtonGroup.group: connectivityGroup
                    font.pixelSize: Theme.fontSizeBody
                    Material.foreground: Theme.text
                }

                Text {
                    visible: supernodeOption.checked
                    text: "No router or inbound-firewall changes are needed for the onboarding flow. You can enable direct P2P later."
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeCaption
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }

                Item { Layout.fillHeight: true }
            }

            // Step 3: Get connected.
            ColumnLayout {
                spacing: Theme.spacingMd
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.margins: Theme.spacingXl

                Text {
                    text: "Get Connected"
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeBody + 4
                    font.bold: true
                }

                Text {
                    text: "Paste an invite from a peer or supernode, or create one to send out."
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeBody
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: Theme.spacingXs

                    Text {
                        text: "Invite to accept"
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                        font.bold: true
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingSm

                        StyledTextField {
                            id: incomingInviteField
                            Layout.fillWidth: true
                            placeholderText: "conquerd://invite#..."
                            Keys.onReturnPressed: if (text.trim().length > 0 && root.appBackend) root.appBackend.pasteInvite(text.trim())
                        }

                        StyledButton {
                            text: "Accept"
                            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/connect.svg"
                            enabled: incomingInviteField.text.trim().length > 0
                            onClicked: {
                                if (root.appBackend) root.appBackend.pasteInvite(incomingInviteField.text.trim())
                                incomingInviteField.text = ""
                            }
                        }
                    }
                }

                Rectangle {
                    Layout.fillWidth: true
                    height: 1
                    color: Theme.bg3
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: Theme.spacingXs

                    Text {
                        text: "Invite to share"
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                        font.bold: true
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingSm

                        Rectangle {
                            Layout.fillWidth: true
                            Layout.preferredHeight: Theme.touchTarget
                            color: Theme.bg2
                            border.color: Theme.bg3
                            border.width: 1

                            Text {
                                anchors.fill: parent
                                anchors.leftMargin: Theme.spacingSm
                                anchors.rightMargin: Theme.spacingSm
                                verticalAlignment: Text.AlignVCenter
                                color: root.generatedInvite !== "" ? Theme.text : Theme.muted
                                font.pixelSize: Theme.fontSizeCaption
                                text: root.generatedInvite !== "" ? root.generatedInvite : "Generate an invite link"
                                elide: Text.ElideMiddle
                            }
                        }

                        StyledButton {
                            text: "Generate"
                            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/invite.svg"
                            onClicked: {
                                if (root.appBackend) {
                                    root.generatedInvite = root.appBackend.generateInvite()
                                    root.copiedInvite = false
                                }
                            }
                        }

                        StyledButton {
                            text: root.copiedInvite ? "Copied" : "Copy"
                            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/clipboard.svg"
                            enabled: root.generatedInvite !== ""
                            success: root.copiedInvite
                            onClicked: {
                                if (root.appBackend) root.appBackend.copyToClipboard(root.generatedInvite)
                                root.copiedInvite = true
                            }
                        }
                    }
                }

                Item { Layout.fillHeight: true }
            }

            // Step 4: System integration.
            ColumnLayout {
                spacing: Theme.spacingMd
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.margins: Theme.spacingXl

                Text {
                    text: "System Integration"
                    color: Theme.text
                    font.pixelSize: Theme.fontSizeBody + 4
                    font.bold: true
                }

                Text {
                    text: root.isWindows
                        ? "These local Windows settings can be changed later."
                        : "System integration is currently Windows-only."
                    color: Theme.muted
                    font.pixelSize: Theme.fontSizeBody
                    wrapMode: Text.WordWrap
                    Layout.fillWidth: true
                }

                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 132
                    color: Theme.bg2
                    border.color: Theme.bg3
                    border.width: 1

                    ColumnLayout {
                        anchors.fill: parent
                        anchors.margins: Theme.spacingMd
                        spacing: Theme.spacingSm

                        CheckBox {
                            id: uriOption
                            text: "Open conquerd:// links in ConquerD"
                            checked: root.isWindows
                            enabled: root.isWindows
                            font.pixelSize: Theme.fontSizeBody
                            Material.foreground: enabled ? Theme.text : Theme.muted
                        }

                        CheckBox {
                            id: shortcutOption
                            text: "Create Desktop and Start Menu shortcuts"
                            checked: root.isWindows
                            enabled: root.isWindows
                            font.pixelSize: Theme.fontSizeBody
                            Material.foreground: enabled ? Theme.text : Theme.muted
                        }

                        Text {
                            text: root.setupStatus
                            visible: root.setupStatus !== ""
                            color: Theme.online
                            font.pixelSize: Theme.fontSizeCaption
                        }
                    }
                }

                Item { Layout.fillHeight: true }
            }
        }
    }

    footer: ColumnLayout {
        spacing: Theme.spacingSm

        Row {
            Layout.alignment: Qt.AlignHCenter
            spacing: Theme.spacingSm

            Repeater {
                model: root.stepCount

                Rectangle {
                    width: index === root.step ? 22 : 8
                    height: 8
                    radius: 0
                    color: index === root.step ? Theme.accent : Theme.bg3

                    Behavior on width { NumberAnimation { duration: Theme.animFast } }
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: Theme.spacingLg
            Layout.rightMargin: Theme.spacingLg
            Layout.bottomMargin: Theme.spacingMd
            spacing: Theme.spacingSm

            StyledButton {
                text: "Back"
                visible: root.step > 0
                onClicked: root.step = Math.max(0, root.step - 1)
            }

            Item { Layout.fillWidth: true }

            StyledButton {
                id: nextBtn
                text: root.step === root.stepCount - 1 ? "Finish" : "Next"
                primary: true
                enabled: root.step !== 0 || handleField.text.trim().length > 0
                onClicked: {
                    if (root.step === 0) root.saveHandle()
                    if (root.step === 2) root.saveConnectivity()
                    if (root.step < root.stepCount - 1) {
                        root.step += 1
                    } else {
                        root.finishWizard()
                    }
                }
            }
        }
    }
}
