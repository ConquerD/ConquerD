// SettingsDialog.qml — Multi-tab settings modal dialog.
//
// Tabs: General, Audio, Plugins, Privacy.
// Reads/writes to SettingsModel; calls settingsModel.save() on Apply/OK.
//
// Usage:
//   SettingsDialog {
//       id: settingsDialog
//       settingsModel: settingsModel      // SettingsModel singleton
//   }
//   // open:  settingsDialog.open()

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Dialog {
    id: root

    required property var settingsModel

    title: "Settings"
    modal: true
    width: 620
    height: 520
    closePolicy: Dialog.CloseOnEscape

    background: Rectangle {
        color: Theme.bg1
        radius: 0
        border.color: Theme.bg3
        border.width: 1
    }

    header: Rectangle {
        color: Theme.bg0
        height: 48
        radius: 0
        Rectangle {
            anchors { left: parent.left; right: parent.right; bottom: parent.bottom }
            height: 10; color: Theme.bg0
        }
        RowLayout {
            anchors { left: parent.left; right: parent.right; leftMargin: 16; rightMargin: 16 }
            height: parent.height
            Text {
                text: root.title
                color: Theme.text
                font.pixelSize: Theme.fontSizeBody
                font.bold: true
                Layout.fillWidth: true
            }
        }
    }

    contentItem: ColumnLayout {
        spacing: 0

        // Tab bar
        TabBar {
            id: tabBar
            Layout.fillWidth: true

            background: Rectangle { color: Theme.bg0 }

            TabButton {
                text: "General"
                contentItem: Text {
                    text: parent.text
                    color: tabBar.currentIndex === 0 ? Theme.text : Theme.muted
                    horizontalAlignment: Text.AlignHCenter
                    font.pixelSize: Theme.fontSizeCaption
                }
                background: Rectangle {
                    color: "transparent"
                    Rectangle {
                        anchors.bottom: parent.bottom
                        width: parent.width; height: 2
                        color: tabBar.currentIndex === 0 ? Theme.accent : "transparent"
                    }
                }
            }
            TabButton {
                text: "Audio"
                contentItem: Text {
                    text: parent.text
                    color: tabBar.currentIndex === 1 ? Theme.text : Theme.muted
                    horizontalAlignment: Text.AlignHCenter
                    font.pixelSize: Theme.fontSizeCaption
                }
                background: Rectangle {
                    color: "transparent"
                    Rectangle {
                        anchors.bottom: parent.bottom
                        width: parent.width; height: 2
                        color: tabBar.currentIndex === 1 ? Theme.accent : "transparent"
                    }
                }
            }
            TabButton {
                text: "Plugins"
                contentItem: Text {
                    text: parent.text
                    color: tabBar.currentIndex === 2 ? Theme.text : Theme.muted
                    horizontalAlignment: Text.AlignHCenter
                    font.pixelSize: Theme.fontSizeCaption
                }
                background: Rectangle {
                    color: "transparent"
                    Rectangle {
                        anchors.bottom: parent.bottom
                        width: parent.width; height: 2
                        color: tabBar.currentIndex === 2 ? Theme.accent : "transparent"
                    }
                }
            }
            TabButton {
                text: "Privacy"
                contentItem: Text {
                    text: parent.text
                    color: tabBar.currentIndex === 3 ? Theme.text : Theme.muted
                    horizontalAlignment: Text.AlignHCenter
                    font.pixelSize: Theme.fontSizeCaption
                }
                background: Rectangle {
                    color: "transparent"
                    Rectangle {
                        anchors.bottom: parent.bottom
                        width: parent.width; height: 2
                        color: tabBar.currentIndex === 3 ? Theme.accent : "transparent"
                    }
                }
            }
        }

        // Tab pages
        StackLayout {
            currentIndex: tabBar.currentIndex
            Layout.fillWidth: true
            Layout.fillHeight: true

            // ── General ───────────────────────────────────────────────────
            ScrollView {
                contentWidth: availableWidth
                clip: true
                ColumnLayout {
                    width: parent.width
                    spacing: 12

                    SettingsSection { title: "Connection" }

                    SettingsRow {
                        label: "Auto-connect on startup"
                        Switch {
                            checked: root.settingsModel ? root.settingsModel.auto_connect : false
                            onToggled: if (root.settingsModel) root.settingsModel.auto_connect = checked
                        }
                    }
                    SettingsRow {
                        label: "Start minimized"
                        Switch {
                            checked: root.settingsModel ? root.settingsModel.start_minimized : false
                            onToggled: if (root.settingsModel) root.settingsModel.start_minimized = checked
                        }
                    }

                    SettingsSection { title: "Appearance" }

                    SettingsRow {
                        label: "Theme"
                        ComboBox {
                            id: themeCombo
                            width: 140
                            model: ["System", "Dark", "Light"]
                            readonly property var _values: ["system", "dark", "light"]
                            currentIndex: {
                                var v = root.settingsModel ? root.settingsModel.theme : "system"
                                var idx = _values.indexOf(v)
                                return idx >= 0 ? idx : 0
                            }
                            onActivated: if (root.settingsModel) root.settingsModel.theme = _values[currentIndex]
                            contentItem: Text {
                                leftPadding: 8; text: themeCombo.displayText
                                color: Theme.text; font.pixelSize: Theme.fontSizeCaption
                                verticalAlignment: Text.AlignVCenter
                            }
                            background: Rectangle {
                                color: Theme.bg2; radius: 0
                                border.color: themeCombo.activeFocus ? Theme.accent : Theme.bg3; border.width: 1
                            }
                            popup: Popup {
                                width: themeCombo.width; implicitHeight: contentItem.implicitHeight
                                contentItem: ListView {
                                    clip: true; implicitHeight: contentHeight
                                    model: themeCombo.popup.visible ? themeCombo.delegateModel : null
                                    ScrollIndicator.vertical: ScrollIndicator {}
                                }
                                background: Rectangle { color: Theme.bg2; radius: 0; border.color: Theme.bg3; border.width: 1 }
                            }
                        }
                    }

                    SettingsSection { title: "Network" }

                    SettingsRow {
                        label: "Enable UPnP port mapping"
                        Switch {
                            checked: root.settingsModel ? root.settingsModel.upnp_enabled : true
                            onToggled: if (root.settingsModel) root.settingsModel.upnp_enabled = checked
                        }
                    }

                    SettingsSection { title: "Relay" }

                    SettingsRow {
                        label: "Allow relay-gated connections"
                        Switch {
                            checked: root.settingsModel ? root.settingsModel.relay_allow_gated : true
                            onToggled: if (root.settingsModel) root.settingsModel.relay_allow_gated = checked
                        }
                    }
                    SettingsRow {
                        label: "Auto-renew relay tickets"
                        Switch {
                            checked: root.settingsModel ? root.settingsModel.relay_auto_renew : true
                            onToggled: if (root.settingsModel) root.settingsModel.relay_auto_renew = checked
                        }
                    }

                    SettingsSection { title: "Relay Port" }

                    SettingsRow {
                        label: "Relay port (0 = automatic)"
                        TextField {
                            text: root.settingsModel ? root.settingsModel.relay_port.toString() : "0"
                            inputMethodHints: Qt.ImhDigitsOnly
                            validator: IntValidator { bottom: 0; top: 65535 }
                            onEditingFinished: if (root.settingsModel) root.settingsModel.relay_port = parseInt(text) || 0
                            background: Rectangle { color: Theme.bg2; radius: 0 }
                            color: Theme.text
                            font.pixelSize: Theme.fontSizeCaption
                            width: 80
                        }
                    }

                    SettingsSection { title: "Updates" }

                    SettingsRow {
                        label: "Check for updates automatically"
                        Switch {
                            checked: root.settingsModel ? root.settingsModel.update_check_enabled : true
                            onToggled: if (root.settingsModel) root.settingsModel.update_check_enabled = checked
                        }
                    }

                    Item { height: 12 }
                }
            }

            // ── Audio ─────────────────────────────────────────────────────
            ScrollView {
                contentWidth: availableWidth
                clip: true
                ColumnLayout {
                    id: audioTabLayout
                    width: parent.width
                    spacing: 12

                    // Convert a QML key-press event to a platform key string
                    // matching the format expected by start_ptt_polling().
                    function pttKeyToString(event) {
                        switch (event.key) {
                            case Qt.Key_Space:     return "space"
                            case Qt.Key_Control:   return "ctrl"
                            case Qt.Key_Shift:     return "shift"
                            case Qt.Key_Alt:       return "alt"
                            case Qt.Key_CapsLock:  return "capslock"
                            case Qt.Key_Tab:       return "tab"
                            case Qt.Key_Return:
                            case Qt.Key_Enter:     return "enter"
                            case Qt.Key_Backspace: return "backspace"
                            case Qt.Key_Delete:    return "delete"
                            case Qt.Key_Insert:    return "insert"
                            case Qt.Key_Home:      return "home"
                            case Qt.Key_End:       return "end"
                            case Qt.Key_PageUp:    return "pageup"
                            case Qt.Key_PageDown:  return "pagedown"
                            case Qt.Key_Left:      return "left"
                            case Qt.Key_Right:     return "right"
                            case Qt.Key_Up:        return "up"
                            case Qt.Key_Down:      return "down"
                            case Qt.Key_F1:  return "f1"
                            case Qt.Key_F2:  return "f2"
                            case Qt.Key_F3:  return "f3"
                            case Qt.Key_F4:  return "f4"
                            case Qt.Key_F5:  return "f5"
                            case Qt.Key_F6:  return "f6"
                            case Qt.Key_F7:  return "f7"
                            case Qt.Key_F8:  return "f8"
                            case Qt.Key_F9:  return "f9"
                            case Qt.Key_F10: return "f10"
                            case Qt.Key_F11: return "f11"
                            case Qt.Key_F12: return "f12"
                            default: {
                                var t = event.text.toLowerCase()
                                return (t.length === 1) ? t : ""
                            }
                        }
                    }

                    SettingsSection { title: "Input Mode" }

                    // Segmented voice-mode toggle: Push to Talk | Voice Activation
                    RowLayout {
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        spacing: 0

                        readonly property bool pttActive: root.settingsModel ? root.settingsModel.push_to_talk : false
                        readonly property bool vadActive: root.settingsModel ? root.settingsModel.voice_activation : false

                        Rectangle {
                            width: 148; height: 34
                            radius: 0
                            // Right corners squared to merge with sibling
                            layer.enabled: false
                            color: parent.pttActive ? Theme.accent : Theme.bg2
                            border.color: parent.pttActive ? Theme.accent : Theme.bg3
                            border.width: 1
                            Text {
                                anchors.centerIn: parent
                                text: "Push to Talk"
                                color: parent.parent.pttActive ? "white" : Theme.muted
                                font.pixelSize: Theme.fontSizeCaption
                                font.bold: parent.parent.pttActive
                            }
                            MouseArea {
                                anchors.fill: parent
                                onClicked: {
                                    if (root.settingsModel) {
                                        root.settingsModel.push_to_talk = true
                                        root.settingsModel.voice_activation = false
                                    }
                                }
                            }
                        }
                        Rectangle {
                            width: 148; height: 34
                            radius: 0
                            color: parent.vadActive ? Theme.accent : Theme.bg2
                            border.color: parent.vadActive ? Theme.accent : Theme.bg3
                            border.width: 1
                            Text {
                                anchors.centerIn: parent
                                text: "Voice Activation"
                                color: parent.parent.vadActive ? "white" : Theme.muted
                                font.pixelSize: Theme.fontSizeCaption
                                font.bold: parent.parent.vadActive
                            }
                            MouseArea {
                                anchors.fill: parent
                                onClicked: {
                                    if (root.settingsModel) {
                                        root.settingsModel.push_to_talk = false
                                        root.settingsModel.voice_activation = true
                                    }
                                }
                            }
                        }
                    }

                    // PTT key — visible only in PTT mode
                    SettingsRow {
                        label: "PTT key"
                        visible: root.settingsModel ? root.settingsModel.push_to_talk : false

                        FocusScope {
                            id: pttCapture
                            property bool capturing: false
                            width: 128; height: 32

                            Keys.onPressed: function(event) {
                                if (!capturing) return
                                event.accepted = true
                                if (event.key === Qt.Key_Escape) {
                                    capturing = false
                                    return
                                }
                                var s = audioTabLayout.pttKeyToString(event)
                                if (s !== "" && root.settingsModel) {
                                    root.settingsModel.ptt_key = s
                                }
                                capturing = false
                            }

                            Rectangle {
                                anchors.fill: parent
                                radius: 0
                                color: pttCapture.capturing ? Theme.accent : Theme.bg2
                                border.color: pttCapture.capturing ? Theme.accent : Theme.bg3
                                border.width: 1

                                Text {
                                    anchors.centerIn: parent
                                    text: pttCapture.capturing
                                        ? "Press a key…"
                                        : (root.settingsModel ? root.settingsModel.ptt_key : "space")
                                    color: pttCapture.capturing ? "white" : Theme.text
                                    font.pixelSize: Theme.fontSizeCaption
                                }

                                MouseArea {
                                    anchors.fill: parent
                                    onClicked: {
                                        pttCapture.capturing = true
                                        pttCapture.forceActiveFocus()
                                    }
                                }
                            }
                        }
                    }

                    SettingsSection { title: "Noise Suppression" }

                    SettingsRow {
                        label: "Suppression level"

                        ComboBox {
                            id: noiseCombo
                            width: 160
                            model: ["Off", "Mild", "Moderate", "Aggressive", "Max"]

                            readonly property var _values: ["off", "mild", "moderate", "aggressive", "max"]

                            currentIndex: {
                                var v = root.settingsModel ? root.settingsModel.noise_strength : "moderate"
                                var idx = _values.indexOf(v)
                                return idx >= 0 ? idx : 2
                            }

                            onActivated: {
                                if (root.settingsModel) {
                                    var v = _values[currentIndex]
                                    root.settingsModel.noise_strength = v
                                    root.settingsModel.noise_suppression = (v !== "off")
                                    if (typeof backend !== "undefined" && backend)
                                        backend.setNoiseStrength(v)
                                }
                            }

                            contentItem: Text {
                                leftPadding: 8
                                text: noiseCombo.displayText
                                color: Theme.text
                                font.pixelSize: Theme.fontSizeCaption
                                verticalAlignment: Text.AlignVCenter
                            }
                            background: Rectangle {
                                color: Theme.bg2
                                radius: 0
                                border.color: noiseCombo.activeFocus ? Theme.accent : Theme.bg3
                                border.width: 1
                            }
                            popup: Popup {
                                width: noiseCombo.width
                                implicitHeight: contentItem.implicitHeight
                                contentItem: ListView {
                                    clip: true
                                    implicitHeight: contentHeight
                                    model: noiseCombo.popup.visible ? noiseCombo.delegateModel : null
                                    ScrollIndicator.vertical: ScrollIndicator {}
                                }
                                background: Rectangle { color: Theme.bg2; radius: 0; border.color: Theme.bg3; border.width: 1 }
                            }
                        }

                        Text {
                            text: "Mild: subtle · Moderate: balanced · Aggressive/Max: may affect voice quality"
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeCaption - 1
                            wrapMode: Text.Wrap
                            Layout.fillWidth: true
                        }
                    }

                    SettingsSection { title: "Volume" }

                    SettingsRow {
                        label: "Microphone volume"
                        Slider {
                            from: 0; to: 200; stepSize: 1
                            value: root.settingsModel ? root.settingsModel.input_volume : 100
                            onMoved: {
                                if (root.settingsModel) {
                                    root.settingsModel.input_volume = value
                                    if (typeof backend !== "undefined" && backend)
                                        backend.setInputVolume(value)
                                }
                            }
                            width: 180
                        }
                    }
                    SettingsRow {
                        label: "Speaker volume"
                        Slider {
                            from: 0; to: 200; stepSize: 1
                            value: root.settingsModel ? root.settingsModel.output_volume : 100
                            onMoved: {
                                if (root.settingsModel) {
                                    root.settingsModel.output_volume = value
                                    if (typeof backend !== "undefined" && backend)
                                        backend.setOutputVolume(value)
                                }
                            }
                            width: 180
                        }
                    }

                    SettingsSection { title: "Jitter Buffer" }

                    SettingsRow {
                        label: "Depth (packets)"
                        Slider {
                            id: jitterSlider
                            from: 1; to: 10; stepSize: 1
                            value: root.settingsModel ? root.settingsModel.jitter_buffer_depth : 3
                            onMoved: {
                                if (root.settingsModel) {
                                    root.settingsModel.jitter_buffer_depth = value
                                    if (typeof backend !== "undefined" && backend)
                                        backend.setJitterDepth(value)
                                }
                            }
                            width: 160
                        }
                        Text {
                            text: (jitterSlider.value * 20) + " ms"
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeCaption - 1
                        }
                    }

                    SettingsSection { title: "Audio Test" }

                    SettingsRow {
                        label: "Microphone"
                        RowLayout {
                            spacing: 8
                            // Level meter bar
                            Rectangle {
                                width: 100; height: 10; radius: 0
                                color: Theme.bg3
                                Rectangle {
                                    width: parent.width * (backend ? Math.min(backend.mic_level, 1.0) : 0)
                                    height: parent.height; radius: 0
                                    color: {
                                        var lv = backend ? backend.mic_level : 0
                                        return lv > 0.85 ? "#e03428" : lv > 0.55 ? "#f0a020" : "#2ecc71"
                                    }
                                    Behavior on width { NumberAnimation { duration: 40 } }
                                }
                            }
                            Button {
                                text: backend && backend.mic_test_active ? "Stop" : "Test Mic"
                                font.pixelSize: Theme.fontSizeCaption
                                onClicked: {
                                    if (backend && backend.mic_test_active)
                                        backend.stopMicTest()
                                    else if (backend)
                                        backend.startMicTest()
                                }
                            }
                        }
                    }

                    SettingsRow {
                        label: "Speaker"
                        Button {
                            text: "Test Speaker"
                            font.pixelSize: Theme.fontSizeCaption
                            onClicked: if (backend) backend.testSpeaker()
                        }
                    }

                    Text {
                        Layout.leftMargin: 16
                        text: "Use headphones during mic test to avoid feedback."
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption - 1
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                        Layout.rightMargin: 16
                    }

                    SettingsSection { title: "Notifications" }

                    SettingsRow {
                        label: "Enable notifications"
                        Switch {
                            checked: root.settingsModel ? root.settingsModel.notifications_enabled : true
                            onToggled: if (root.settingsModel) root.settingsModel.notifications_enabled = checked
                        }
                    }

                    Item { height: 12 }
                }
            }

            // ── Plugins ───────────────────────────────────────────────────
            ScrollView {
                contentWidth: availableWidth
                clip: true
                ColumnLayout {
                    width: parent.width
                    spacing: 12

                    SettingsSection { title: "Ollama (Local AI)" }

                    SettingsRow {
                        label: "Enable Ollama integration"
                        Switch {
                            checked: root.settingsModel ? root.settingsModel.ollama_enabled : false
                            onToggled: if (root.settingsModel) root.settingsModel.ollama_enabled = checked
                        }
                    }
                    SettingsRow {
                        label: "Ollama base URL"
                        TextField {
                            text: root.settingsModel ? root.settingsModel.ollama_base_url : "http://localhost:11434"
                            placeholderText: "http://localhost:11434"
                            enabled: root.settingsModel && root.settingsModel.ollama_enabled
                            onEditingFinished: if (root.settingsModel) root.settingsModel.ollama_base_url = text
                            background: Rectangle {
                                color: Theme.bg2; radius: 0
                                border.color: parent.activeFocus ? Theme.accent : Theme.bg3
                                border.width: 1
                            }
                            color: Theme.text
                            font.pixelSize: Theme.fontSizeCaption
                            width: 260
                        }
                    }
                    SettingsRow {
                        label: "Model name"
                        TextField {
                            text: root.settingsModel ? root.settingsModel.ollama_model : "llama3"
                            placeholderText: "llama3"
                            enabled: root.settingsModel && root.settingsModel.ollama_enabled
                            onEditingFinished: if (root.settingsModel) root.settingsModel.ollama_model = text
                            background: Rectangle {
                                color: Theme.bg2; radius: 0
                                border.color: parent.activeFocus ? Theme.accent : Theme.bg3
                                border.width: 1
                            }
                            color: Theme.text
                            font.pixelSize: Theme.fontSizeCaption
                            width: 160
                        }
                    }

                    Text {
                        Layout.leftMargin: 16
                        text: "Ollama lets you send messages to a local AI model. It never contacts any remote server."
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption - 1
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                        Layout.rightMargin: 16
                    }

                    Item { height: 12 }
                }
            }

            // ── Privacy ───────────────────────────────────────────────────
            ScrollView {
                contentWidth: availableWidth
                clip: true
                ColumnLayout {
                    width: parent.width
                    spacing: 12

                    SettingsSection { title: "Build Attestation" }

                    SettingsRow {
                        label: "Attestation policy"
                        ComboBox {
                            id: attCombo
                            width: 140
                            model: ["Off", "Warn", "Strict"]
                            readonly property var _values: ["off", "warn", "strict"]
                            currentIndex: {
                                var v = root.settingsModel ? root.settingsModel.attestation_policy : "warn"
                                var idx = _values.indexOf(v)
                                return idx >= 0 ? idx : 1
                            }
                            onActivated: if (root.settingsModel) root.settingsModel.attestation_policy = _values[currentIndex]
                            contentItem: Text {
                                leftPadding: 8; text: attCombo.displayText
                                color: Theme.text; font.pixelSize: Theme.fontSizeCaption
                                verticalAlignment: Text.AlignVCenter
                            }
                            background: Rectangle {
                                color: Theme.bg2; radius: 0
                                border.color: attCombo.activeFocus ? Theme.accent : Theme.bg3; border.width: 1
                            }
                            popup: Popup {
                                width: attCombo.width; implicitHeight: contentItem.implicitHeight
                                contentItem: ListView {
                                    clip: true; implicitHeight: contentHeight
                                    model: attCombo.popup.visible ? attCombo.delegateModel : null
                                    ScrollIndicator.vertical: ScrollIndicator {}
                                }
                                background: Rectangle { color: Theme.bg2; radius: 0; border.color: Theme.bg3; border.width: 1 }
                            }
                        }
                    }

                    Text {
                        Layout.leftMargin: 16
                        text: "Off: disabled · Warn: challenge peers and log results · Strict: deny relay to unverified peers"
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption - 1
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                        Layout.rightMargin: 16
                    }

                    SettingsSection { title: "URI Scheme" }

                    RowLayout {
                        Layout.leftMargin: 16
                        spacing: 12
                        Text {
                            text: "Register conquerd:// handler"
                            color: Theme.text
                            font.pixelSize: Theme.fontSizeCaption
                        }
                        Button {
                            text: "Register"
                            onClicked: backend.registerUriScheme()
                        }
                        Button {
                            text: "Unregister"
                            onClicked: backend.unregisterUriScheme()
                        }
                    }

                    Item { height: 12 }
                }
            }
        }
    }

    footer: DialogButtonBox {
        standardButtons: Dialog.Apply | Dialog.Ok | Dialog.Cancel
        background: Rectangle {
            color: Theme.bg0
            radius: 0
            Rectangle {
                anchors { left: parent.left; right: parent.right; top: parent.top }
                height: 10; color: Theme.bg0
            }
        }
        onApplied: root.settingsModel && root.settingsModel.save()
        onAccepted: {
            if (root.settingsModel) root.settingsModel.save()
            root.close()
        }
        onRejected: root.close()
    }

    // ── Internal helper components ─────────────────────────────────────────

    component SettingsSection: Text {
        required property string title
        text: title
        color: Theme.muted
        font.pixelSize: Theme.fontSizeCaption
        font.bold: true
        Layout.leftMargin: 16
        Layout.topMargin: 8
        Layout.fillWidth: true
    }

    component SettingsRow: RowLayout {
        required property string label
        spacing: 12
        Layout.fillWidth: true
        Layout.leftMargin: 16
        Layout.rightMargin: 16

        Text {
            text: parent.label
            color: Theme.text
            font.pixelSize: Theme.fontSizeCaption
            Layout.fillWidth: true
        }
    }
}
