// SettingsPage.qml - token-driven settings content panel.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    property var settings: null
    property int currentTab: 0

    readonly property var avatarGridSizes: [8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 32]
    readonly property var avatarDualHueModes: ["topbot", "checker", "quad"]

    function pttKeyName(event) {
        switch (event.key) {
            case Qt.Key_Space: return "space"
            case Qt.Key_Control: return "ctrl"
            case Qt.Key_Shift: return "shift"
            case Qt.Key_Alt: return "alt"
            case Qt.Key_CapsLock: return "capslock"
            case Qt.Key_Tab: return "tab"
            case Qt.Key_Return:
            case Qt.Key_Enter: return "enter"
            case Qt.Key_Backspace: return "backspace"
            case Qt.Key_Delete: return "delete"
            case Qt.Key_Insert: return "insert"
            case Qt.Key_Home: return "home"
            case Qt.Key_End: return "end"
            case Qt.Key_PageUp: return "pageup"
            case Qt.Key_PageDown: return "pagedown"
            case Qt.Key_Left: return "left"
            case Qt.Key_Right: return "right"
            case Qt.Key_Up: return "up"
            case Qt.Key_Down: return "down"
            case Qt.Key_F1: return "f1"
            case Qt.Key_F2: return "f2"
            case Qt.Key_F3: return "f3"
            case Qt.Key_F4: return "f4"
            case Qt.Key_F5: return "f5"
            case Qt.Key_F6: return "f6"
            case Qt.Key_F7: return "f7"
            case Qt.Key_F8: return "f8"
            case Qt.Key_F9: return "f9"
            case Qt.Key_F10: return "f10"
            case Qt.Key_F11: return "f11"
            case Qt.Key_F12: return "f12"
            default: {
                var t = event.text.toLowerCase()
                return t.length === 1 ? t : ""
            }
        }
    }

    function indexOf(values, value, fallback) {
        var idx = values.indexOf(value)
        return idx >= 0 ? idx : fallback
    }

    function setTheme(value) {
        if (!settings) return
        settings.theme = value
        if (value === "dark") {
            Theme.isDark = true
        } else if (value === "light") {
            Theme.isDark = false
        } else if (value === "system") {
            Theme.isDark = Qt.styleHints.colorScheme === Qt.ColorScheme.Dark
        }
    }

    function defaultAvatarConfig() {
        return {
            grid: 16, sat: 0.55, lig: 0.55, spread: 0.15,
            bg_tint: true, bg_lig: 0.12, shade_mode: 1,
            dual_hue: false, dual_hue_mode: "topbot",
            islands: true, island_conn: 8, island_step: 0.62,
            island_varsat: true, svg_crisp: true, svg_round_cells: false
        }
    }

    function avatarConfig() {
        var d = defaultAvatarConfig()
        if (!settings || !settings.avatar_config_json) return d
        try {
            var parsed = JSON.parse(settings.avatar_config_json)
            for (var k in parsed) d[k] = parsed[k]
        } catch (e) {}
        return d
    }

    function avatarValue(key, fallback) {
        var cfg = avatarConfig()
        return cfg.hasOwnProperty(key) ? cfg[key] : fallback
    }

    function setAvatarValue(key, value) {
        if (!settings) return
        var cfg = avatarConfig()
        cfg[key] = value
        var json = JSON.stringify(cfg)
        settings.avatar_config_json = json
        settings.save()
        if (backend) {
            backend.setAvatarConfigJson(json)
            backend.broadcastAvatarConfigToAll(json)
        }
    }

    // Sliders only emit onMoved while dragging; clicks jump value without it.
    function commitAvatarSlider(key, sliderValue) {
        root.setAvatarValue(key, parseFloat(sliderValue.toFixed(2)))
    }

    Component {
        id: galleryComponent
        ComponentGallery {}
    }

    Rectangle {
        anchors.fill: parent
        color: Theme.bg1
    }

    StackLayout {
        anchors.fill: parent
        currentIndex: Math.max(0, Math.min(root.currentTab, 7))

        ScrollView {
            contentWidth: availableWidth
            clip: true

            ColumnLayout {
                width: Math.max(0, root.width - Theme.spacingXl * 2)
                x: Theme.spacingXl
                y: Theme.spacingLg
                spacing: Theme.spacingLg

                SettingsSectionHeader { title: "Audio" }

                SettingsCard {
                    title: "Input"
                    subtitle: "Voice capture, keying, and noise handling."

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingSm

                        StyledButton {
                            text: "Push to Talk"
                            primary: root.settings ? root.settings.push_to_talk : false
                            onClicked: {
                                if (!root.settings) return
                                root.settings.push_to_talk = true
                                root.settings.voice_activation = false
                            }
                        }

                        StyledButton {
                            text: "Voice Activation"
                            primary: root.settings ? root.settings.voice_activation : false
                            onClicked: {
                                if (!root.settings) return
                                root.settings.push_to_talk = false
                                root.settings.voice_activation = true
                            }
                        }
                    }

                    RowLayout {
                        visible: root.settings ? root.settings.push_to_talk : false
                        Layout.fillWidth: true
                        spacing: Theme.spacingMd

                        Label {
                            Layout.preferredWidth: 140
                            text: "PTT key"
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeBody
                        }

                        FocusScope {
                            id: pttCapture
                            property bool capturing: false
                            property bool readyToCapture: false
                            Layout.preferredWidth: 180
                            height: Theme.controlHeight

                            Timer {
                                id: captureReady
                                interval: 150
                                onTriggered: pttCapture.readyToCapture = true
                            }

                            function activate() {
                                capturing = true
                                readyToCapture = false
                                captureReady.start()
                                forceActiveFocus()
                            }

                            function finish(name) {
                                if (name !== "" && root.settings) root.settings.ptt_key = name
                                capturing = false
                                readyToCapture = false
                            }

                            Keys.onPressed: function(event) {
                                if (!capturing || !readyToCapture) return
                                event.accepted = true
                                if (event.key === Qt.Key_Escape) {
                                    capturing = false
                                    readyToCapture = false
                                    return
                                }
                                finish(root.pttKeyName(event))
                            }

                            Rectangle {
                                anchors.fill: parent
                                radius: Theme.radiusMd
                                color: pttCapture.capturing ? Theme.accent : Theme.bg3
                                border.color: pttCapture.activeFocus ? Theme.accent : Theme.bg3
                                border.width: 1

                                Label {
                                    anchors.centerIn: parent
                                    text: pttCapture.capturing
                                        ? "Press a key or click"
                                        : (root.settings ? root.settings.ptt_key : "space")
                                    color: pttCapture.capturing ? Theme.textInv : Theme.text
                                    font.pixelSize: Theme.fontSizeBody
                                }

                                MouseArea {
                                    anchors.fill: parent
                                    acceptedButtons: Qt.AllButtons
                                    onPressed: function(mouse) {
                                        if (!pttCapture.capturing) {
                                            pttCapture.activate()
                                        } else if (pttCapture.readyToCapture) {
                                            var name = ""
                                            if (mouse.button === Qt.LeftButton) name = "mouse1"
                                            else if (mouse.button === Qt.RightButton) name = "mouse2"
                                            else if (mouse.button === Qt.MiddleButton) name = "mouse3"
                                            else if (mouse.button === Qt.BackButton) name = "mouse4"
                                            else if (mouse.button === Qt.ForwardButton) name = "mouse5"
                                            pttCapture.finish(name)
                                        }
                                        mouse.accepted = true
                                    }
                                }
                            }
                        }

                        Label {
                            Layout.fillWidth: true
                            text: "Click the field, then press a key or mouse button."
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeCaption
                            wrapMode: Text.WordWrap
                        }
                    }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: Theme.spacingXl
                        rowSpacing: Theme.spacingMd

                        Label { text: "Noise suppression"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: noiseCombo
                            Layout.fillWidth: true
                            model: ["Off", "Mild", "Moderate", "Aggressive", "Max"]
                            property var values: ["off", "mild", "moderate", "aggressive", "max"]
                            currentIndex: root.indexOf(values, root.settings ? root.settings.noise_strength : "moderate", 2)
                            onActivated: {
                                if (!root.settings) return
                                var value = values[currentIndex]
                                root.settings.noise_strength = value
                                root.settings.noise_suppression = value !== "off"
                                if (backend) backend.setNoiseStrength(value)
                            }
                        }

                        Label { text: "Outgoing bitrate"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: bitrateCombo
                            Layout.fillWidth: true
                            model: ["Low (32 kbps)", "Balanced (64 kbps)", "High (96 kbps)", "Ultra (128 kbps)"]
                            property var values: ["low", "balanced", "high", "ultra"]
                            currentIndex: root.indexOf(values, root.settings ? root.settings.voice_bitrate : "ultra", 3)
                            onActivated: {
                                if (!root.settings) return
                                var value = values[currentIndex]
                                root.settings.voice_bitrate = value
                                if (backend) backend.setVoiceBitrate(value)
                            }
                        }

                        Label { text: "Jitter buffer"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        RowLayout {
                            Layout.fillWidth: true
                            SpinBox {
                                from: 1
                                to: 20
                                value: root.settings ? root.settings.jitter_buffer_depth : 3
                                onValueModified: {
                                    if (!root.settings) return
                                    root.settings.jitter_buffer_depth = value
                                    if (backend) backend.setJitterDepth(value)
                                }
                            }
                            Label { text: "packets"; color: Theme.muted; font.pixelSize: Theme.fontSizeCaption }
                        }
                    }
                }

                SettingsCard {
                    title: "Levels and Devices"

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 3
                        columnSpacing: Theme.spacingLg
                        rowSpacing: Theme.spacingMd

                        Label { text: "Microphone"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Slider {
                            Layout.fillWidth: true
                            from: 0
                            to: 200
                            stepSize: 1
                            value: root.settings ? root.settings.input_volume : 100
                            onMoved: {
                                if (!root.settings) return
                                root.settings.input_volume = Math.round(value)
                                if (backend) backend.setInputVolume(root.settings.input_volume)
                            }
                        }
                        Label { text: (root.settings ? root.settings.input_volume : 100) + "%"; color: Theme.muted; Layout.preferredWidth: 44 }

                        Label { text: "Speaker"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Slider {
                            Layout.fillWidth: true
                            from: 0
                            to: 200
                            stepSize: 1
                            value: root.settings ? root.settings.output_volume : 100
                            onMoved: {
                                if (!root.settings) return
                                root.settings.output_volume = Math.round(value)
                                if (backend) backend.setOutputVolume(root.settings.output_volume)
                            }
                        }
                        Label { text: (root.settings ? root.settings.output_volume : 100) + "%"; color: Theme.muted; Layout.preferredWidth: 44 }

                        Label { text: "Input device"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: inputDeviceCombo
                            Layout.columnSpan: 2
                            Layout.fillWidth: true
                            model: ["Default"]
                            property bool ready: false
                            onActivated: {
                                if (!root.settings) return
                                root.settings.audio_input_device = currentIndex === 0 ? "" : currentText
                                if (backend) backend.setAudioDevices(root.settings.audio_input_device, root.settings.audio_output_device)
                            }
                        }

                        Label { text: "Output device"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: outputDeviceCombo
                            Layout.columnSpan: 2
                            Layout.fillWidth: true
                            model: ["Default"]
                            property bool ready: false
                            onActivated: {
                                if (!root.settings) return
                                root.settings.audio_output_device = currentIndex === 0 ? "" : currentText
                                if (backend) backend.setAudioDevices(root.settings.audio_input_device, root.settings.audio_output_device)
                            }
                        }
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingMd

                        Label { text: "Mic test"; color: Theme.muted; font.pixelSize: Theme.fontSizeBody }
                        Rectangle {
                            Layout.preferredWidth: 120
                            height: 10
                            radius: Theme.radiusSm
                            color: Theme.bg3
                            Rectangle {
                                width: parent.width * (backend ? Math.min(backend.mic_level, 1.0) : 0)
                                height: parent.height
                                radius: Theme.radiusSm
                                color: {
                                    var level = backend ? backend.mic_level : 0
                                    return level > 0.85 ? Theme.danger : level > 0.55 ? Theme.warn : Theme.online
                                }
                                Behavior on width { NumberAnimation { duration: 60 } }
                            }
                        }
                        StyledButton {
                            text: backend && backend.mic_test_active ? "Stop" : "Test Mic"
                            onClicked: {
                                if (backend && backend.mic_test_active) backend.stopMicTest()
                                else if (backend) backend.startMicTest()
                            }
                        }
                        StyledButton {
                            text: "Test Speaker"
                            onClicked: if (backend) backend.testSpeaker()
                        }
                        Item { Layout.fillWidth: true }
                    }

                    Component.onCompleted: {
                        if (!backend) return
                        try {
                            var devices = JSON.parse(backend.listAudioDevices())
                            inputDeviceCombo.model = ["Default"].concat(devices.inputs || [])
                            outputDeviceCombo.model = ["Default"].concat(devices.outputs || [])
                        } catch (e) {}
                        if (root.settings) {
                            var inputIndex = inputDeviceCombo.find(root.settings.audio_input_device)
                            inputDeviceCombo.currentIndex = inputIndex >= 0 ? inputIndex : 0
                            var outputIndex = outputDeviceCombo.find(root.settings.audio_output_device)
                            outputDeviceCombo.currentIndex = outputIndex >= 0 ? outputIndex : 0
                        }
                    }
                }
            }
        }

        ScrollView {
            contentWidth: availableWidth
            clip: true

            ColumnLayout {
                width: Math.max(0, root.width - Theme.spacingXl * 2)
                x: Theme.spacingXl
                y: Theme.spacingLg
                spacing: Theme.spacingLg

                SettingsSectionHeader { title: "Identity" }

                SettingsCard {
                    title: "Local Profile"
                    subtitle: "Only trusted peers receive the profile data you broadcast."

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingMd

                        Avatar {
                            peerId: backend ? backend.public_id : ""
                            size: 56
                            showRing: true
                            configJson: root.settings ? root.settings.avatar_config_json : ""
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: Theme.spacingXs

                            Label { text: "Display name"; color: Theme.muted; font.pixelSize: Theme.fontSizeCaption }
                            TextField {
                                Layout.fillWidth: true
                                text: root.settings ? root.settings.local_handle : ""
                                placeholderText: "Optional display name"
                                color: Theme.text
                                placeholderTextColor: Theme.muted
                                background: Rectangle { color: Theme.bg3; radius: Theme.radiusMd; border.color: activeFocus ? Theme.accent : Theme.bg3; border.width: 1 }
                                onEditingFinished: {
                                    if (!root.settings) return
                                    root.settings.local_handle = text
                                    root.settings.save()
                                    // Push the new name to already-connected peers.
                                    if (backend)
                                        backend.broadcastHandleToAll(text.trim())
                                }
                            }
                        }
                    }

                    Label {
                        Layout.fillWidth: true
                        text: "Public ID: " + (backend ? backend.public_id : "(loading...)")
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                        wrapMode: Text.WrapAnywhere
                    }

                    StyledButton {
                        text: "Copy Invite Link"
                        primary: true
                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/invite.svg"
                        onClicked: if (backend) backend.copyInvite()
                    }
                }

                SettingsSectionHeader { title: "Avatar" }

                SettingsCard {
                    title: "Generated Avatar"
                    subtitle: "Deterministic SVG settings are stored locally and shared through the avatar capability path."

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingLg

                        Avatar {
                            size: 84
                            showRing: true
                            peerId: backend ? backend.public_id : ""
                            configJson: root.settings ? root.settings.avatar_config_json : ""
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: Theme.spacingSm

                            StyledButton {
                                text: "Reset Avatar"
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/undo.svg"
                                onClicked: {
                                    if (!root.settings || !backend) return
                                    var defaultJson = JSON.stringify(root.defaultAvatarConfig())
                                    backend.setAvatarConfigJson("")
                                    root.settings.avatar_config_json = ""
                                    root.settings.save()
                                    // Local empty JSON = factory defaults; peers need the
                                    // explicit payload because send_avatar_config skips "".
                                    backend.broadcastAvatarConfigToAll(defaultJson)
                                }
                            }
                        }
                    }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: Theme.spacingXl
                        rowSpacing: Theme.spacingMd

                        Label { text: "Grid size"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            Layout.fillWidth: true
                            model: root.avatarGridSizes
                            currentIndex: root.indexOf(root.avatarGridSizes, root.avatarValue("grid", 16), 4)
                            onActivated: root.setAvatarValue("grid", parseInt(currentText))
                        }

                        Label { text: "Saturation"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Slider {
                            Layout.fillWidth: true
                            from: 0.1; to: 1.0; stepSize: 0.01
                            value: root.avatarValue("sat", 0.55)
                            onMoved: root.commitAvatarSlider("sat", value)
                            onPressedChanged: if (!pressed) root.commitAvatarSlider("sat", value)
                        }

                        Label { text: "Lightness"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Slider {
                            Layout.fillWidth: true
                            from: 0.1; to: 0.9; stepSize: 0.01
                            value: root.avatarValue("lig", 0.55)
                            onMoved: root.commitAvatarSlider("lig", value)
                            onPressedChanged: if (!pressed) root.commitAvatarSlider("lig", value)
                        }

                        Label { text: "Hue spread"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Slider {
                            Layout.fillWidth: true
                            from: 0.0; to: 0.5; stepSize: 0.01
                            value: root.avatarValue("spread", 0.15)
                            onMoved: root.commitAvatarSlider("spread", value)
                            onPressedChanged: if (!pressed) root.commitAvatarSlider("spread", value)
                        }

                        Label { text: "Background tint"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Switch {
                            checked: root.avatarValue("bg_tint", true)
                            onToggled: root.setAvatarValue("bg_tint", checked)
                        }

                        Label {
                            text: "Background lightness"
                            color: Theme.muted
                            Layout.alignment: Qt.AlignRight
                            visible: root.avatarValue("bg_tint", true)
                        }
                        Slider {
                            Layout.fillWidth: true
                            visible: root.avatarValue("bg_tint", true)
                            enabled: root.avatarValue("bg_tint", true)
                            from: 0.0; to: 0.4; stepSize: 0.01
                            value: root.avatarValue("bg_lig", 0.12)
                            onMoved: root.commitAvatarSlider("bg_lig", value)
                            onPressedChanged: if (!pressed) root.commitAvatarSlider("bg_lig", value)
                        }

                        Label { text: "Shade levels"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            Layout.fillWidth: true
                            model: ["flat", "2-level", "3-level"]
                            currentIndex: Math.max(0, root.avatarValue("shade_mode", 1) - 1)
                            onActivated: root.setAvatarValue("shade_mode", currentIndex + 1)
                        }

                        Label { text: "Dual hue"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Switch {
                            checked: root.avatarValue("dual_hue", false)
                            onToggled: root.setAvatarValue("dual_hue", checked)
                        }

                        Label {
                            text: "Dual hue mode"
                            color: Theme.muted
                            Layout.alignment: Qt.AlignRight
                            visible: root.avatarValue("dual_hue", false)
                        }
                        ComboBox {
                            Layout.fillWidth: true
                            visible: root.avatarValue("dual_hue", false)
                            enabled: root.avatarValue("dual_hue", false)
                            model: root.avatarDualHueModes
                            currentIndex: root.indexOf(
                                root.avatarDualHueModes,
                                root.avatarValue("dual_hue_mode", "topbot"),
                                0)
                            onActivated: root.setAvatarValue("dual_hue_mode", currentText)
                        }

                        Label { text: "Islands"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Switch {
                            checked: root.avatarValue("islands", true)
                            onToggled: root.setAvatarValue("islands", checked)
                        }

                        Label {
                            text: "Island connectivity"
                            color: Theme.muted
                            Layout.alignment: Qt.AlignRight
                            visible: root.avatarValue("islands", true)
                        }
                        ComboBox {
                            Layout.fillWidth: true
                            visible: root.avatarValue("islands", true)
                            enabled: root.avatarValue("islands", true)
                            model: [4, 8]
                            currentIndex: root.indexOf([4, 8], root.avatarValue("island_conn", 8), 1)
                            onActivated: root.setAvatarValue("island_conn", parseInt(currentText))
                        }

                        Label {
                            text: "Island hue step"
                            color: Theme.muted
                            Layout.alignment: Qt.AlignRight
                            visible: root.avatarValue("islands", true)
                        }
                        Slider {
                            Layout.fillWidth: true
                            visible: root.avatarValue("islands", true)
                            enabled: root.avatarValue("islands", true)
                            from: 0.1; to: 1.0; stepSize: 0.01
                            value: root.avatarValue("island_step", 0.62)
                            onMoved: root.commitAvatarSlider("island_step", value)
                            onPressedChanged: if (!pressed) root.commitAvatarSlider("island_step", value)
                        }

                        Label {
                            text: "Island saturation variance"
                            color: Theme.muted
                            Layout.alignment: Qt.AlignRight
                            visible: root.avatarValue("islands", true)
                        }
                        Switch {
                            visible: root.avatarValue("islands", true)
                            enabled: root.avatarValue("islands", true)
                            checked: root.avatarValue("island_varsat", true)
                            onToggled: root.setAvatarValue("island_varsat", checked)
                        }

                        Label { text: "Crisp edges"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Switch {
                            checked: root.avatarValue("svg_crisp", true)
                            onToggled: root.setAvatarValue("svg_crisp", checked)
                        }

                        Label { text: "Rounded cells"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        Switch {
                            checked: root.avatarValue("svg_round_cells", false)
                            onToggled: root.setAvatarValue("svg_round_cells", checked)
                        }
                    }
                }
            }
        }

        ScrollView {
            contentWidth: availableWidth
            clip: true

            ColumnLayout {
                width: Math.max(0, root.width - Theme.spacingXl * 2)
                x: Theme.spacingXl
                y: Theme.spacingLg
                spacing: Theme.spacingLg

                SettingsSectionHeader { title: "General" }

                SettingsCard {
                    title: "Application"

                    SettingSwitch { title: "Enable notifications"; checked: root.settings ? root.settings.notifications_enabled : true; onChanged: if (root.settings) root.settings.notifications_enabled = checked }
                    SettingSwitch { title: "Auto-connect to known peers"; checked: root.settings ? root.settings.auto_connect : false; onChanged: if (root.settings) root.settings.auto_connect = checked }
                    SettingSwitch { title: "Start minimized"; checked: root.settings ? root.settings.start_minimized : false; onChanged: if (root.settings) root.settings.start_minimized = checked }
                    SettingSwitch { title: "Minimize to tray on close"; description: "Keep ConquerD running in the system tray instead of quitting when the window is closed or minimized"; checked: root.settings ? root.settings.minimize_to_tray : false; onChanged: if (root.settings) root.settings.minimize_to_tray = checked }
                    SettingSwitch { title: "Check for updates automatically"; checked: root.settings ? root.settings.update_check_enabled : true; onChanged: if (root.settings) root.settings.update_check_enabled = checked }
                    SettingSwitch { title: "Enable UPnP port mapping"; checked: root.settings ? root.settings.upnp_enabled : true; onChanged: if (root.settings) root.settings.upnp_enabled = checked }
                    SettingSwitch { title: "Verbose debug logging"; description: "Write detailed diagnostic logs for troubleshooting. Applies immediately; a RUST_LOG environment variable overrides this."; checked: root.settings ? root.settings.debug_logging : false; onChanged: if (root.settings) root.settings.debug_logging = checked }

                    Rectangle { Layout.fillWidth: true; height: 1; color: Theme.bg3 }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: Theme.spacingXl
                        Label { text: "Theme"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            Layout.fillWidth: true
                            model: ["System", "Dark", "Light"]
                            property var values: ["system", "dark", "light"]
                            currentIndex: root.indexOf(values, root.settings ? root.settings.theme : "dark", 1)
                            onActivated: root.setTheme(values[currentIndex])
                        }
                    }
                }
            }
        }

        ScrollView {
            contentWidth: availableWidth
            clip: true

            ColumnLayout {
                width: Math.max(0, root.width - Theme.spacingXl * 2)
                x: Theme.spacingXl
                y: Theme.spacingLg
                spacing: Theme.spacingLg

                SettingsSectionHeader { title: "AI" }

                SettingsCard {
                    id: ollamaCard
                    title: "Ollama Assistant"
                    subtitle: "Local-only assistant. Enable + Save, then restart once. Auto-reply posts Ollama answers into chat when a peer messages you."

                    // Stable ListModel — assigning a JS array to ComboBox.model is
                    // unreliable with editable Material combos on some Qt 6 builds.
                    ListModel { id: ollamaModelList }

                    function persistOllamaSettings() {
                        if (root.settings)
                            root.settings.save()
                    }

                    function refreshOllamaModels() {
                        if (typeof backend === "undefined" || !backend) {
                            ollamaModelStatus.text = "Backend not ready"
                            ollamaModelStatus.color = Theme.danger
                            return
                        }
                        ollamaModelStatus.text = "Loading models..."
                        ollamaModelStatus.color = Theme.muted
                        var url = root.settings ? root.settings.ollama_base_url : "http://127.0.0.1:11434"
                        backend.fetchOllamaModels(url || "")
                    }

                    function applyOllamaModels(modelsJson, errorText) {
                        var err = (errorText === undefined || errorText === null) ? "" : ("" + errorText)
                        if (err !== "") {
                            ollamaModelStatus.text = "Error: " + err
                            ollamaModelStatus.color = Theme.danger
                            return
                        }
                        var names = []
                        try {
                            names = JSON.parse("" + modelsJson)
                        } catch (e) {
                            ollamaModelStatus.text = "Error: bad model list from backend"
                            ollamaModelStatus.color = Theme.danger
                            return
                        }
                        if (!names || names.length === 0) {
                            ollamaModelStatus.text = "No models found — is Ollama running? Try: ollama list"
                            ollamaModelStatus.color = Theme.muted
                            return
                        }

                        var saved = root.settings ? root.settings.ollama_model : ollamaModelCombo.editText
                        ollamaModelList.clear()
                        var idx = -1
                        for (var i = 0; i < names.length; i++) {
                            ollamaModelList.append({ name: names[i] })
                            if (names[i] === saved)
                                idx = i
                        }
                        if (idx >= 0) {
                            ollamaModelCombo.currentIndex = idx
                        } else {
                            // Keep a custom / previously-saved name even if not installed.
                            ollamaModelCombo.editText = saved || names[0]
                        }
                        ollamaModelStatus.text = "Found " + names.length + (names.length === 1 ? " model" : " models")
                        ollamaModelStatus.color = Theme.muted
                    }

                    SettingSwitch {
                        id: ollamaEnabledSwitch
                        title: "Enable AI assistant"
                        description: backend && backend.ollama_available
                                     ? "Plugin running — chat AI is available."
                                     : "Uses your local Ollama server. Restart ConquerD after enabling so chat AI starts."
                        checked: root.settings ? root.settings.ollama_enabled : false
                        onChanged: {
                            if (!root.settings) return
                            root.settings.ollama_enabled = checked
                            persistOllamaSettings()
                            if (checked)
                                refreshOllamaModels()
                        }
                    }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: Theme.spacingXl
                        rowSpacing: Theme.spacingMd
                        enabled: ollamaEnabledSwitch.checked
                        opacity: enabled ? 1.0 : 0.5

                        Label { text: "Base URL"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        TextField {
                            Layout.fillWidth: true
                            text: root.settings ? root.settings.ollama_base_url : "http://127.0.0.1:11434"
                            placeholderText: "http://127.0.0.1:11434"
                            color: Theme.text
                            placeholderTextColor: Theme.muted
                            background: Rectangle { color: Theme.bg3; radius: Theme.radiusMd; border.color: activeFocus ? Theme.accent : Theme.bg3; border.width: 1 }
                            onEditingFinished: {
                                if (root.settings) root.settings.ollama_base_url = text
                                persistOllamaSettings()
                                refreshOllamaModels()
                            }
                        }

                        Label { text: "Model"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        RowLayout {
                            Layout.fillWidth: true
                            ComboBox {
                                id: ollamaModelCombo
                                Layout.fillWidth: true
                                editable: true
                                model: ollamaModelList
                                textRole: "name"
                                valueRole: "name"
                                Component.onCompleted: {
                                    var initial = root.settings ? root.settings.ollama_model : "llama3"
                                    if (ollamaModelList.count === 0)
                                        ollamaModelList.append({ name: initial })
                                    editText = initial
                                }
                                onActivated: {
                                    if (!root.settings) return
                                    var name = ollamaModelList.get(currentIndex).name
                                    if (root.settings.ollama_model !== name) {
                                        root.settings.ollama_model = name
                                        persistOllamaSettings()
                                    }
                                }
                                onEditTextChanged: {
                                    if (!root.settings) return
                                    if (root.settings.ollama_model === editText) return
                                    root.settings.ollama_model = editText
                                    persistOllamaSettings()
                                }
                            }
                            ToolButton {
                                implicitWidth: Theme.controlHeight
                                implicitHeight: Theme.controlHeight
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/refresh.svg"
                                icon.width: 16
                                icon.height: 16
                                icon.color: Theme.muted
                                ToolTip.text: "Fetch available models from Ollama"
                                ToolTip.visible: hovered
                                background: Rectangle {
                                    radius: Theme.radiusSm
                                    color: parent.hovered ? Theme.bg3 : "transparent"
                                }
                                onClicked: refreshOllamaModels()
                            }
                        }

                        Item {}
                        Label {
                            id: ollamaModelStatus
                            Layout.fillWidth: true
                            text: "Use refresh to load available models"
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeCaption
                            wrapMode: Text.WordWrap
                        }

                        Label { text: "System prompt"; color: Theme.muted; Layout.alignment: Qt.AlignRight | Qt.AlignTop }
                        TextArea {
                            Layout.fillWidth: true
                            implicitHeight: 96
                            wrapMode: TextArea.Wrap
                            text: root.settings ? root.settings.ollama_system_prompt : "You are a helpful assistant."
                            color: Theme.text
                            placeholderTextColor: Theme.muted
                            background: Rectangle { color: Theme.bg3; radius: Theme.radiusMd; border.color: activeFocus ? Theme.accent : Theme.bg3; border.width: 1 }
                            onActiveFocusChanged: {
                                if (!activeFocus && root.settings) {
                                    root.settings.ollama_system_prompt = text
                                    persistOllamaSettings()
                                }
                            }
                        }

                        Item {}
                        ColumnLayout {
                            Layout.fillWidth: true
                            SettingSwitch {
                                title: "Auto-reply to direct messages"
                                checked: root.settings ? root.settings.ollama_auto_respond_direct : false
                                onChanged: {
                                    if (!root.settings) return
                                    root.settings.ollama_auto_respond_direct = checked
                                    persistOllamaSettings()
                                }
                            }
                            SettingSwitch {
                                title: "Auto-reply to room chat"
                                checked: root.settings ? root.settings.ollama_auto_respond_room : false
                                onChanged: {
                                    if (!root.settings) return
                                    root.settings.ollama_auto_respond_room = checked
                                    persistOllamaSettings()
                                }
                            }
                        }
                    }

                    // Primary delivery path: qproperty change (snake_case), which
                    // matches how other cxx-qt settings bindings work in this app.
                    Connections {
                        target: backend
                        function onOllama_models_jsonChanged() {
                            ollamaCard.applyOllamaModels(backend.ollama_models_json, backend.ollama_models_error)
                        }
                        function onOllamaModelsReady(models, error) {
                            ollamaCard.applyOllamaModels(models, error)
                        }
                    }

                    Connections {
                        target: root
                        function onCurrentTabChanged() {
                            if (root.currentTab === 3)
                                ollamaCard.refreshOllamaModels()
                        }
                    }

                    Component.onCompleted: Qt.callLater(function() {
                        if (root.currentTab === 3)
                            ollamaCard.refreshOllamaModels()
                    })
                }
            }
        }

        ScrollView {
            contentWidth: availableWidth
            clip: true

            ColumnLayout {
                width: Math.max(0, root.width - Theme.spacingXl * 2)
                x: Theme.spacingXl
                y: Theme.spacingLg
                spacing: Theme.spacingLg

                SettingsSectionHeader { title: "Network" }

                SettingsCard {
                    title: "Direct P2P"
                    subtitle: "Listen for trusted peers without requiring a supernode."

                    SettingSwitch {
                        title: "Enable direct peer-to-peer listener"
                        checked: root.settings ? root.settings.direct_p2p_enabled : true
                        onChanged: {
                            if (!root.settings) return
                            root.settings.direct_p2p_enabled = checked
                            root.settings.save()
                            if (backend)
                                backend.configureDirectP2p(checked, root.settings.direct_p2p_port)
                        }
                    }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: Theme.spacingXl

                        Label { text: "UDP listener port"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        TextField {
                            Layout.preferredWidth: 120
                            enabled: root.settings ? root.settings.direct_p2p_enabled : true
                            text: root.settings ? root.settings.direct_p2p_port.toString() : "61045"
                            inputMethodHints: Qt.ImhDigitsOnly
                            validator: IntValidator { bottom: 1; top: 65535 }
                            color: enabled ? Theme.text : Theme.muted
                            background: Rectangle { color: Theme.bg3; radius: Theme.radiusMd; border.color: activeFocus ? Theme.accent : Theme.bg3; border.width: 1 }
                            onEditingFinished: {
                                if (!root.settings) return
                                var port = parseInt(text, 10)
                                if (isNaN(port) || port < 1 || port > 65535) port = 61045
                                root.settings.direct_p2p_port = port
                                root.settings.save()
                                if (backend)
                                    backend.configureDirectP2p(root.settings.direct_p2p_enabled, port)
                            }
                        }
                    }

                    Label {
                        Layout.fillWidth: true
                        text: "Allow UDP through the host firewall. For internet P2P, forward the same UDP port to this device; otherwise use a supernode."
                        color: Theme.muted
                        wrapMode: Text.WordWrap
                        font.pixelSize: Theme.fontSizeCaption
                    }
                }

                SettingsCard {
                    title: "Relay"
                    subtitle: "Transport assistance stays optional and client-owned."

                    SettingSwitch { title: "Allow relay-gated connections"; checked: root.settings ? root.settings.relay_allow_gated : true; onChanged: if (root.settings) root.settings.relay_allow_gated = checked }
                    SettingSwitch { title: "Auto-renew relay tickets"; checked: root.settings ? root.settings.relay_auto_renew : true; onChanged: if (root.settings) root.settings.relay_auto_renew = checked }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: Theme.spacingXl

                        Label { text: "Relay port"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        TextField {
                            Layout.preferredWidth: 120
                            text: root.settings ? root.settings.relay_port.toString() : "0"
                            inputMethodHints: Qt.ImhDigitsOnly
                            validator: IntValidator { bottom: 0; top: 65535 }
                            color: Theme.text
                            background: Rectangle { color: Theme.bg3; radius: Theme.radiusMd; border.color: activeFocus ? Theme.accent : Theme.bg3; border.width: 1 }
                            onEditingFinished: if (root.settings) root.settings.relay_port = parseInt(text) || 0
                        }
                    }
                }
            }
        }

        ScrollView {
            contentWidth: availableWidth
            clip: true

            ColumnLayout {
                width: Math.max(0, root.width - Theme.spacingXl * 2)
                x: Theme.spacingXl
                y: Theme.spacingLg
                spacing: Theme.spacingLg

                SettingsSectionHeader { title: "Security" }

                SettingsCard {
                    title: "Trust Controls"

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: Theme.spacingXl

                        Label { text: "Build attestation"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            Layout.fillWidth: true
                            model: ["Off", "Warn", "Strict"]
                            property var values: ["off", "warn", "strict"]
                            currentIndex: root.indexOf(values, root.settings ? root.settings.attestation_policy : "warn", 1)
                            onActivated: if (root.settings) root.settings.attestation_policy = values[currentIndex]
                        }
                    }

                    Label {
                        Layout.fillWidth: true
                        text: "Warn challenges peers after handshake. Strict denies relay for invalid attestation."
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                        wrapMode: Text.WordWrap
                    }

                    SettingSwitch {
                        title: "Show YouTube preview cards in chat"
                        description: "Preview cards are click-to-open and do not fetch thumbnails."
                        checked: root.settings ? root.settings.youtube_preview_enabled : true
                        onChanged: if (root.settings) root.settings.youtube_preview_enabled = checked
                    }
                }
            }
        }

        ScrollView {
            contentWidth: availableWidth
            clip: true

            ColumnLayout {
                width: Math.max(0, root.width - Theme.spacingXl * 2)
                x: Theme.spacingXl
                y: Theme.spacingLg
                spacing: Theme.spacingLg

                SettingsSectionHeader { title: "Privacy" }

                SettingsCard {
                    id: privacyBox
                    title: "Privacy and Data"
                    subtitle: "Chat messages are stored encrypted on this device only."
                    property int storedCount: 0
                    property bool confirmPurge: false
                    property bool confirmLock: false

                    Component.onCompleted: storedCount = backend ? backend.getStoredMessageCount() : 0

                    Label {
                        text: "Stored messages: " + privacyBox.storedCount
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeBody
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingMd

                        Label { text: "Trim messages older than"; color: Theme.muted }
                        SpinBox { id: trimAgeSpin; from: 1; to: 3650; value: 30; textFromValue: function(v) { return v + " days" }; valueFromText: function(t) { return parseInt(t) || 30 } }
                        StyledButton {
                            text: "Trim by Age"
                            onClicked: {
                                if (backend) backend.trimMessagesByAge(trimAgeSpin.value)
                                privacyBox.storedCount = backend ? backend.getStoredMessageCount() : 0
                            }
                        }
                        Item { Layout.fillWidth: true }
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingMd

                        Label { text: "Keep most recent"; color: Theme.muted }
                        SpinBox { id: trimCountSpin; from: 1; to: 100000; value: 500; textFromValue: function(v) { return v + " msgs" }; valueFromText: function(t) { return parseInt(t) || 500 } }
                        Label { text: "per conversation"; color: Theme.muted }
                        StyledButton {
                            text: "Trim to Limit"
                            onClicked: {
                                if (backend) backend.trimMessagesByCount(trimCountSpin.value)
                                privacyBox.storedCount = backend ? backend.getStoredMessageCount() : 0
                            }
                        }
                        Item { Layout.fillWidth: true }
                    }
                }

                SettingsCard {
                    title: "Danger Zone"
                    subtitle: "Destructive actions require a second click."

                    ColumnLayout {
                        visible: privacyBox.confirmPurge
                        Layout.fillWidth: true
                        spacing: Theme.spacingSm

                        Label { Layout.fillWidth: true; text: "This permanently deletes all chat history."; color: Theme.danger; wrapMode: Text.WordWrap }
                        RowLayout {
                            StyledButton {
                                text: "Delete Everything"
                                danger: true
                                onClicked: {
                                    if (backend) backend.purgeAllChatHistory()
                                    privacyBox.storedCount = backend ? backend.getStoredMessageCount() : 0
                                    privacyBox.confirmPurge = false
                                }
                            }
                            StyledButton { text: "Cancel"; onClicked: privacyBox.confirmPurge = false }
                        }
                    }

                    StyledButton {
                        visible: !privacyBox.confirmPurge
                        text: "Purge All Chat History"
                        danger: true
                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/trash.svg"
                        onClicked: privacyBox.confirmPurge = true
                    }

                    Rectangle { Layout.fillWidth: true; height: 1; color: Theme.bg3 }

                    Label {
                        Layout.fillWidth: true
                        text: "Identity Lock removes the saved key from your OS keyring and quits. You will need your passphrase on next launch."
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                        wrapMode: Text.WordWrap
                    }

                    ColumnLayout {
                        visible: privacyBox.confirmLock
                        Layout.fillWidth: true
                        spacing: Theme.spacingSm

                        Label { text: "Lock identity and quit now?"; color: Theme.danger }
                        RowLayout {
                            StyledButton { text: "Lock and Quit"; danger: true; onClicked: if (backend) backend.lockIdentityAndQuit() }
                            StyledButton { text: "Cancel"; onClicked: privacyBox.confirmLock = false }
                        }
                    }

                    StyledButton {
                        visible: !privacyBox.confirmLock
                        text: "Lock Identity and Quit"
                        danger: true
                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/lock.svg"
                        onClicked: privacyBox.confirmLock = true
                    }
                }
            }
        }

        ScrollView {
            contentWidth: availableWidth
            clip: true

            ColumnLayout {
                width: Math.max(0, root.width - Theme.spacingXl * 2)
                x: Theme.spacingXl
                y: Theme.spacingLg
                spacing: Theme.spacingLg

                SettingsSectionHeader { title: "Diagnostics" }

                SettingsCard {
                    id: diagnosticsBox
                    title: "Event Logs"
                    property string logText: ""

                    Timer {
                        running: diagnosticsBox.visible
                        interval: 2000
                        repeat: true
                        triggeredOnStart: true
                        onTriggered: diagnosticsBox.logText = backend ? backend.getEventLogs() : ""
                    }

                    ScrollView {
                        Layout.fillWidth: true
                        implicitHeight: 220
                        clip: true
                        background: Rectangle { color: Theme.bg1; radius: Theme.radiusMd }
                        ScrollBar.vertical.policy: ScrollBar.AlwaysOn

                        TextArea {
                            readOnly: true
                            text: diagnosticsBox.logText
                            color: Theme.muted
                            font.family: "Courier New"
                            font.pixelSize: Theme.fontSizeCaption
                            wrapMode: Text.WrapAnywhere
                            background: Item {}
                            padding: Theme.spacingSm
                        }
                    }

                    RowLayout {
                        spacing: Theme.spacingSm
                        StyledButton {
                            text: "Refresh"
                            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/refresh.svg"
                            onClicked: diagnosticsBox.logText = backend ? backend.getEventLogs() : ""
                        }
                        StyledButton {
                            text: "Clear"
                            danger: true
                            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/trash.svg"
                            onClicked: {
                                if (backend) backend.clearEventLogs()
                                diagnosticsBox.logText = ""
                            }
                        }
                    }
                }

                SettingsCard {
                    id: galleryCard
                    title: "Component Gallery"
                    subtitle: "Visual inventory for design-system QA."

                    SettingSwitch {
                        id: galleryToggle
                        title: "Show component gallery"
                        description: "Preview buttons, inputs, and palette tokens."
                        checked: false
                    }

                    Loader {
                        Layout.fillWidth: true
                        active: galleryToggle.checked
                        sourceComponent: galleryComponent
                    }
                }

                SettingsCard {
                    title: "About"

                    Label { text: "ConquerD - Privacy-first peer connectivity"; color: Theme.muted }
                    Label { text: "Version " + Qt.application.version; color: Theme.muted }

                    RowLayout {
                        spacing: Theme.spacingSm
                        StyledButton {
                            text: "Create Shortcuts"
                            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/plus.svg"
                            onClicked: {
                                if (!backend) return
                                backend.createDesktopShortcuts()
                                shortcutStatus.hasShortcuts = backend.hasDesktopShortcuts()
                            }
                        }
                        StyledButton {
                            text: "Remove Shortcuts"
                            onClicked: {
                                if (!backend) return
                                backend.removeDesktopShortcuts()
                                shortcutStatus.hasShortcuts = backend.hasDesktopShortcuts()
                            }
                        }
                    }

                    Label {
                        id: shortcutStatus
                        property bool hasShortcuts: backend ? backend.hasDesktopShortcuts() : false
                        text: hasShortcuts ? "Status: Shortcuts exist" : "Status: No shortcuts found"
                        color: hasShortcuts ? Theme.online : Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                    }
                }
            }
        }
    }
}
