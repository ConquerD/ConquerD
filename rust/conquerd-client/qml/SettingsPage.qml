// SettingsPage.qml — User settings panel.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    // Settings model (loaded/saved from ~/.conquerd/settings.json)
    property var settings: null  // bound by MainWindow after creation

    // Selected settings section (0=Audio … 7=Diagnostics). Driven by MainWindow sidebar nav.
    property int currentTab: 0

    // Map a QML key-press event to the platform key string used by PTT polling.
    function pttKeyName(event) {
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

    StackLayout {
        anchors.fill: parent
        currentIndex: root.currentTab

        // ── Tab 0: Audio ──────────────────────────────────────────────────
        ScrollView { contentWidth: availableWidth; clip: true
        ColumnLayout { x: 20; y: 12; width: parent.availableWidth - 40; spacing: 12

        // ── Audio ─────────────────────────────────────────────────────────
        GroupBox {
            Layout.fillWidth: true
            topPadding: 28
            label: Label { text: "Audio"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
            background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

            ColumnLayout {
                anchors.left: parent.left
                anchors.right: parent.right
                spacing: 10

                // ── Input mode toggle ─────────────────────────────────────
                Label { text: "Input mode"; color: Theme.muted; font.pixelSize: 12 }

                RowLayout {
                    spacing: 0
                    readonly property bool pttOn: root.settings ? root.settings.push_to_talk : false
                    readonly property bool vadOn: root.settings ? root.settings.voice_activation : false

                    Rectangle {
                        width: 130; height: 30; radius: 0
                        color: parent.pttOn ? Theme.accent : Theme.bg3
                        border.color: parent.pttOn ? Theme.accent : Theme.divider
                        border.width: 1
                        Label {
                            anchors.centerIn: parent
                            text: "Push to Talk"
                            color: parent.parent.pttOn ? Theme.textInv : Theme.muted
                            font.pixelSize: 12
                            font.bold: parent.parent.pttOn
                        }
                        MouseArea {
                            anchors.fill: parent
                            onClicked: {
                                if (root.settings) {
                                    root.settings.push_to_talk = true
                                    root.settings.voice_activation = false
                                }
                            }
                        }
                    }
                    Rectangle {
                        width: 130; height: 30; radius: 0
                        color: parent.vadOn ? Theme.accent : Theme.bg3
                        border.color: parent.vadOn ? Theme.accent : Theme.divider
                        border.width: 1
                        Label {
                            anchors.centerIn: parent
                            text: "Voice Activation"
                            color: parent.parent.vadOn ? Theme.textInv : Theme.muted
                            font.pixelSize: 12
                            font.bold: parent.parent.vadOn
                        }
                        MouseArea {
                            anchors.fill: parent
                            onClicked: {
                                if (root.settings) {
                                    root.settings.push_to_talk = false
                                    root.settings.voice_activation = true
                                }
                            }
                        }
                    }
                }

                // ── PTT key capture ───────────────────────────────────────
                RowLayout {
                    visible: root.settings ? root.settings.push_to_talk : false
                    spacing: 8
                    Label { text: "PTT key"; color: Theme.muted }

                    FocusScope {
                        id: pttCapture
                        property bool capturing: false
                        property bool readyToCapture: false
                        width: 160; height: 28

                        // Brief debounce so the left-click that activates
                        // capture mode is not immediately captured as mouse1.
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
                        function cancel() {
                            capturing = false
                            readyToCapture = false
                        }

                        Keys.onPressed: function(event) {
                            if (!capturing || !readyToCapture) return
                            event.accepted = true
                            if (event.key === Qt.Key_Escape) { cancel(); return }
                            var s = root.pttKeyName(event)
                            finish(s)
                        }

                        Rectangle {
                            anchors.fill: parent; radius: 0
                            color: pttCapture.capturing ? Theme.accent : Theme.bg3
                            border.color: pttCapture.capturing ? Theme.accent : Theme.divider; border.width: 1

                            Label {
                                anchors.centerIn: parent
                                text: pttCapture.capturing
                                    ? "Press a key or click…"
                                    : (root.settings ? root.settings.ptt_key : "space")
                                color: pttCapture.capturing ? Theme.textInv : Theme.text
                                font.pixelSize: 12
                            }

                            MouseArea {
                                anchors.fill: parent
                                acceptedButtons: Qt.AllButtons
                                onPressed: function(mouse) {
                                    if (!pttCapture.capturing) {
                                        pttCapture.activate()
                                    } else if (pttCapture.readyToCapture) {
                                        var name = ""
                                        switch (mouse.button) {
                                            case Qt.LeftButton:    name = "mouse1"; break
                                            case Qt.RightButton:   name = "mouse2"; break
                                            case Qt.MiddleButton:  name = "mouse3"; break
                                            case Qt.BackButton:    name = "mouse4"; break
                                            case Qt.ForwardButton: name = "mouse5"; break
                                            default: break
                                        }
                                        pttCapture.finish(name)
                                    }
                                    mouse.accepted = true
                                }
                            }
                        }
                    }

                    Label {
                        text: "Click the box, then press a key or mouse button"
                        color: Theme.muted; font.pixelSize: 11
                    }
                }

                // ── Noise suppression ─────────────────────────────────────
                RowLayout {
                    spacing: 8
                    Label { text: "Noise suppression"; color: Theme.muted }
                    ComboBox {
                        id: noiseCombo
                        width: 150
                        model: ["Off", "Mild", "Moderate", "Aggressive", "Max"]
                        readonly property var _values: ["off", "mild", "moderate", "aggressive", "max"]
                        currentIndex: {
                            var v = root.settings ? root.settings.noise_strength : "moderate"
                            var i = _values.indexOf(v); return i >= 0 ? i : 2
                        }
                        onActivated: {
                            if (root.settings) {
                                var v = _values[currentIndex]
                                root.settings.noise_strength = v
                                root.settings.noise_suppression = (v !== "off")
                                if (typeof backend !== "undefined" && backend)
                                    backend.setNoiseStrength(v)
                            }
                        }
                    }
                }

                // ── Jitter buffer ─────────────────────────────────────────
                RowLayout {
                    spacing: 8
                    Label { text: "Jitter buffer depth"; color: Theme.muted }
                    SpinBox {
                        from: 1; to: 20
                        value: root.settings ? root.settings.jitter_buffer_depth : 3
                        onValueModified: {
                            if (root.settings) {
                                root.settings.jitter_buffer_depth = value
                                if (typeof backend !== "undefined" && backend)
                                    backend.setJitterDepth(value)
                            }
                        }
                    }
                    Label { text: "packets"; color: Theme.muted; font.pixelSize: 11 }
                }

                // ── Volume sliders ────────────────────────────────────────
                RowLayout {
                    Label { text: "Microphone volume"; color: Theme.muted }
                    Slider {
                        from: 0; to: 200; stepSize: 1
                        value: root.settings ? root.settings.input_volume : 100
                        onMoved: {
                            if (root.settings) {
                                root.settings.input_volume = value
                                if (typeof backend !== "undefined" && backend)
                                    backend.setInputVolume(value)
                            }
                        }
                        implicitWidth: 160
                    }
                    Label {
                        text: (root.settings ? root.settings.input_volume : 100) + "%"
                        color: Theme.muted; width: 36
                    }
                }

                RowLayout {
                    Label { text: "Speaker volume"; color: Theme.muted }
                    Slider {
                        from: 0; to: 200; stepSize: 1
                        value: root.settings ? root.settings.output_volume : 100
                        onMoved: {
                            if (root.settings) {
                                root.settings.output_volume = value
                                if (typeof backend !== "undefined" && backend)
                                    backend.setOutputVolume(value)
                            }
                        }
                        implicitWidth: 160
                    }
                    Label {
                        text: (root.settings ? root.settings.output_volume : 100) + "%"
                        color: Theme.muted; width: 36
                    }
                }

                // ── Audio device selection ────────────────────────────────
                // Populated once by calling backend.listAudioDevices() when the
                // Audio section first becomes visible.
                RowLayout {
                    spacing: 8
                    Label { text: "Input device"; color: Theme.muted; Layout.preferredWidth: 110 }
                    ComboBox {
                        id: inputDeviceCombo
                        Layout.fillWidth: true
                        model: ["Default"]
                        property bool _ready: false
                        // Restore saved selection once model is populated.
                        onModelChanged: {
                            if (!_ready || !root.settings) return
                            var saved = root.settings.audio_input_device
                            var idx = find(saved)
                            currentIndex = idx >= 0 ? idx : 0
                        }
                        onActivated: {
                            if (!root.settings) return
                            root.settings.audio_input_device = currentIndex === 0 ? "" : currentText
                            if (backend) backend.setAudioDevices(
                                root.settings.audio_input_device,
                                root.settings ? root.settings.audio_output_device : "")
                        }
                        contentItem: Text {
                            leftPadding: 8; text: inputDeviceCombo.displayText
                            color: Theme.text; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                        }
                        background: Rectangle {
                            color: Theme.bg3; radius: 0
                            border.color: inputDeviceCombo.activeFocus ? Theme.accent : Theme.divider; border.width: 1
                        }
                        popup: Popup {
                            width: inputDeviceCombo.width
                            contentItem: ListView {
                                clip: true; implicitHeight: Math.min(contentHeight, 200)
                                model: inputDeviceCombo.popup.visible ? inputDeviceCombo.delegateModel : null
                                ScrollIndicator.vertical: ScrollIndicator {}
                            }
                            background: Rectangle { color: Theme.bg3; radius: 0; border.color: Theme.divider; border.width: 1 }
                        }
                    }
                }

                RowLayout {
                    spacing: 8
                    Label { text: "Output device"; color: Theme.muted; Layout.preferredWidth: 110 }
                    ComboBox {
                        id: outputDeviceCombo
                        Layout.fillWidth: true
                        model: ["Default"]
                        property bool _ready: false
                        onModelChanged: {
                            if (!_ready || !root.settings) return
                            var saved = root.settings.audio_output_device
                            var idx = find(saved)
                            currentIndex = idx >= 0 ? idx : 0
                        }
                        onActivated: {
                            if (!root.settings) return
                            root.settings.audio_output_device = currentIndex === 0 ? "" : currentText
                            if (backend) backend.setAudioDevices(
                                root.settings ? root.settings.audio_input_device : "",
                                root.settings.audio_output_device)
                        }
                        contentItem: Text {
                            leftPadding: 8; text: outputDeviceCombo.displayText
                            color: Theme.text; font.pixelSize: 12; verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                        }
                        background: Rectangle {
                            color: Theme.bg3; radius: 0
                            border.color: outputDeviceCombo.activeFocus ? Theme.accent : Theme.divider; border.width: 1
                        }
                        popup: Popup {
                            width: outputDeviceCombo.width
                            contentItem: ListView {
                                clip: true; implicitHeight: Math.min(contentHeight, 200)
                                model: outputDeviceCombo.popup.visible ? outputDeviceCombo.delegateModel : null
                                ScrollIndicator.vertical: ScrollIndicator {}
                            }
                            background: Rectangle { color: Theme.bg3; radius: 0; border.color: Theme.divider; border.width: 1 }
                        }
                    }
                }

                // Populate device lists once per session.
                Component.onCompleted: {
                    if (!backend) return
                    var raw = backend.listAudioDevices()
                    try {
                        var devs = JSON.parse(raw)
                        var ins  = ["Default"].concat(devs.inputs  || [])
                        var outs = ["Default"].concat(devs.outputs || [])
                        inputDeviceCombo.model  = ins
                        outputDeviceCombo.model = outs
                    } catch (e) {}
                    inputDeviceCombo._ready  = true
                    outputDeviceCombo._ready = true
                    // Restore saved selections now that models are populated.
                    if (root.settings) {
                        var si = root.settings.audio_input_device
                        var ii = inputDeviceCombo.find(si)
                        inputDeviceCombo.currentIndex = ii >= 0 ? ii : 0
                        var so = root.settings.audio_output_device
                        var oi = outputDeviceCombo.find(so)
                        outputDeviceCombo.currentIndex = oi >= 0 ? oi : 0
                    }
                }

                // ── Mic / speaker test ────────────────────────────────────
                RowLayout {
                    spacing: 10
                    Label { text: "Mic test"; color: Theme.muted }

                    // Level bar
                    Rectangle {
                        width: 90; height: 10; radius: 0; color: Theme.bg3
                        Rectangle {
                            width: parent.width * (backend ? Math.min(backend.mic_level, 1.0) : 0)
                            height: parent.height; radius: 0
                            color: {
                                var lv = backend ? backend.mic_level : 0
                                return lv > 0.85 ? Theme.danger : lv > 0.55 ? Theme.warn : Theme.online
                            }
                            Behavior on width { NumberAnimation { duration: 40 } }
                        }
                    }

                    Button {
                        text: backend && backend.mic_test_active ? "Stop" : "Test Mic"
                        font.pixelSize: 12
                        onClicked: {
                            if (backend && backend.mic_test_active) backend.stopMicTest()
                            else if (backend) backend.startMicTest()
                        }
                    }

                    Button {
                        text: "Test Speaker"
                        font.pixelSize: 12
                        onClicked: if (backend) backend.testSpeaker()
                    }

                    Label {
                        text: "Use headphones to avoid feedback"
                        color: Theme.muted; font.pixelSize: 11
                    }
                }
            }
        }

        } } // end ColumnLayout + ScrollView (Audio)

        // ── Tab 1: Identity & Avatar ─────────────────────────────────────
        ScrollView {
            id: avatarTab
            contentWidth: availableWidth
            clip: true

            // ── Avatar config helpers (moved here from old Tab 8) ─────────
            function _defaultCfg() {
                return {
                    grid: 16, sat: 0.55, lig: 0.55, spread: 0.15,
                    bg_tint: true, bg_lig: 0.12, shade_mode: 3,
                    dual_hue: false, dual_hue_mode: "topbot",
                    islands: true, island_conn: 8, island_step: 0.62,
                    island_varsat: true, svg_crisp: true, svg_round_cells: false
                }
            }

            // Declarative binding — always a full config with defaults merged in.
            // Do NOT imperatively assign avatarTab._cfg elsewhere.
            property var _cfg: {
                if (!settings) return avatarTab._defaultCfg()
                var raw = settings.avatar_config_json
                try {
                    if (!raw || raw === "") return avatarTab._defaultCfg()
                    var d = avatarTab._defaultCfg()
                    var p = JSON.parse(raw)
                    for (var k in p) d[k] = p[k]
                    return d
                } catch(e) { return avatarTab._defaultCfg() }
            }
            function _getCfg(key, def) { return avatarTab._cfg.hasOwnProperty(key) ? avatarTab._cfg[key] : def }
            function _setCfg(key, val) {
                if (!settings) return
                var d = _defaultCfg()
                var c = avatarTab._cfg
                for (var k in c) d[k] = c[k]
                d[key] = val
                var j = JSON.stringify(d)
                settings.avatar_config_json = j
                settings.save()
                backend.setAvatarConfigJson(j)
                backend.broadcastAvatarConfigToAll(j)
            }

            RowLayout {
                id: identityAvatarRow
                width: parent.availableWidth - 40
                x: 20
                y: 12
                spacing: 24

                // ── Left: Identity ────────────────────────────────────────
                ColumnLayout {
                    Layout.preferredWidth: Math.round((identityAvatarRow.width - 24) * 0.25)
                    Layout.maximumWidth: Math.round((identityAvatarRow.width - 24) * 0.25)
                    Layout.fillWidth: false
                    Layout.alignment: Qt.AlignTop
                    spacing: 12

                    GroupBox {
                        Layout.fillWidth: true
                        topPadding: 28
                        label: Label { text: "Identity"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
                        background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

                        ColumnLayout {
                            anchors.left: parent.left
                            anchors.right: parent.right
                            spacing: 8

                            RowLayout {
                                spacing: 12

                                Avatar {
                                    peerId: backend ? backend.public_id : ""
                                    size: 48
                                    showRing: true
                                    configJson: settings ? settings.avatar_config_json : ""
                                    Layout.alignment: Qt.AlignVCenter
                                }

                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: 4

                                    Label { text: "Display name"; color: Theme.muted }
                                    TextField {
                                        Layout.fillWidth: true
                                        text: root.settings ? root.settings.local_handle : ""
                                        placeholderText: "Optional display name"
                                        color: Theme.text
                                        placeholderTextColor: Theme.muted
                                        background: Rectangle { color: Theme.bg3; radius: 0 }
                                        onEditingFinished: if (root.settings) root.settings.local_handle = text
                                    }
                                }
                            }

                            Label {
                                text: "Public ID: " + (backend.public_id || "(loading...)")
                                color: Theme.muted
                                font.pixelSize: 11
                                wrapMode: Text.WrapAnywhere
                                Layout.fillWidth: true
                            }

                            Button {
                                text: "Copy Invite Link"
                                icon.source: "qrc:/qt/qml/ConquerD/Client/icons/invite.svg"
                                icon.width: 16
                                icon.height: 16
                                onClicked: backend.copyInvite()
                            }
                        }
                    }
                }

                // ── Right: Avatar ─────────────────────────────────────────
                ColumnLayout {
                    Layout.fillWidth: true
                    Layout.minimumWidth: 0
                    Layout.alignment: Qt.AlignTop
                    spacing: 8

                    GroupBox {
                        Layout.fillWidth: true
                        topPadding: 28
                        label: Label { text: "Avatar"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
                        background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

                        ColumnLayout {
                            anchors.left: parent.left
                            anchors.right: parent.right
                            spacing: 10

                            // Preview + reset
                            RowLayout {
                                spacing: 12
                                Layout.fillWidth: true

                                Avatar {
                                    id: avatarPreview
                                    size: 72
                                    showRing: true
                                    peerId: backend.public_id
                                    configJson: settings ? settings.avatar_config_json : ""
                                    Layout.alignment: Qt.AlignVCenter
                                }

                                ColumnLayout {
                                    spacing: 2
                                    Layout.alignment: Qt.AlignVCenter

                                    Label { text: "Live preview"; color: Theme.muted; font.pixelSize: 10 }
                                    ToolButton {
                                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/undo.svg"
                                        icon.width: 14; icon.height: 14
                                        icon.color: Theme.muted
                                        flat: true; implicitHeight: 26
                                        ToolTip.text: "Reset avatar to defaults"
                                        ToolTip.visible: hovered
                                        onClicked: {
                                            if (!settings) return
                                            settings.avatar_config_json = ""
                                            settings.save()
                                            backend.setAvatarConfigJson("")
                                        }
                                    }
                                }
                            }

                            // Settings grid
                            GridLayout {
                                Layout.fillWidth: true
                                columns: 2
                                columnSpacing: 10
                                rowSpacing: 3

                                Label { text: "COLOUR"; color: Theme.accent; font.pixelSize: 10; font.bold: true
                                        Layout.columnSpan: 2; bottomPadding: 1 }

                                Label { text: "Grid size"; color: Theme.muted; font.pixelSize: 12 }
                                ComboBox {
                                    implicitHeight: 22; font.pixelSize: 11
                                    model: [8, 12, 16, 20, 24]
                                    currentIndex: {
                                        var g = avatarTab._getCfg("grid", 16)
                                        return [8,12,16,20,24].indexOf(g) < 0 ? 2 : [8,12,16,20,24].indexOf(g)
                                    }
                                    onActivated: avatarTab._setCfg("grid", parseInt(currentText))
                                }

                                Label { text: "Saturation"; color: Theme.muted; font.pixelSize: 12 }
                                Slider {
                                    Layout.fillWidth: true; from: 0.1; to: 1.0; stepSize: 0.01
                                    value: avatarTab._getCfg("sat", 0.55)
                                    onMoved: avatarTab._setCfg("sat", parseFloat(value.toFixed(2)))
                                }

                                Label { text: "Lightness"; color: Theme.muted; font.pixelSize: 12 }
                                Slider {
                                    Layout.fillWidth: true; from: 0.1; to: 0.9; stepSize: 0.01
                                    value: avatarTab._getCfg("lig", 0.55)
                                    onMoved: avatarTab._setCfg("lig", parseFloat(value.toFixed(2)))
                                }

                                Label { text: "Hue spread"; color: Theme.muted; font.pixelSize: 12 }
                                Slider {
                                    Layout.fillWidth: true; from: 0.0; to: 0.5; stepSize: 0.01
                                    value: avatarTab._getCfg("spread", 0.15)
                                    onMoved: avatarTab._setCfg("spread", parseFloat(value.toFixed(2)))
                                }

                                Label { text: "BG tint"; color: Theme.muted; font.pixelSize: 12 }
                                Switch {
                                    scale: 0.72; Layout.preferredHeight: 34
                                    checked: avatarTab._getCfg("bg_tint", true)
                                    onToggled: avatarTab._setCfg("bg_tint", checked)
                                }

                                Label { text: "BG lightness"; color: Theme.muted; font.pixelSize: 12 }
                                Slider {
                                    Layout.fillWidth: true; from: 0.0; to: 0.35; stepSize: 0.01
                                    value: avatarTab._getCfg("bg_lig", 0.12)
                                    onMoved: avatarTab._setCfg("bg_lig", parseFloat(value.toFixed(2)))
                                }

                                Label { text: "SHADING & STYLE"; color: Theme.accent; font.pixelSize: 10; font.bold: true
                                        Layout.columnSpan: 2; topPadding: 6; bottomPadding: 1 }

                                Label { text: "Shade mode"; color: Theme.muted; font.pixelSize: 12 }
                                ComboBox {
                                    implicitHeight: 22; font.pixelSize: 11
                                    model: ["flat", "gradient", "radial"]
                                    currentIndex: avatarTab._getCfg("shade_mode", 3) - 1
                                    onActivated: avatarTab._setCfg("shade_mode", currentIndex + 1)
                                }

                                Label { text: "Crisp edges"; color: Theme.muted; font.pixelSize: 12 }
                                Switch {
                                    scale: 0.72; Layout.preferredHeight: 34
                                    checked: avatarTab._getCfg("svg_crisp", true)
                                    onToggled: avatarTab._setCfg("svg_crisp", checked)
                                }

                                Label { text: "Rounded cells"; color: Theme.muted; font.pixelSize: 12 }
                                Switch {
                                    scale: 0.72; Layout.preferredHeight: 34
                                    checked: avatarTab._getCfg("svg_round_cells", false)
                                    onToggled: avatarTab._setCfg("svg_round_cells", checked)
                                }

                                Label { text: "DUAL HUE"; color: Theme.accent; font.pixelSize: 10; font.bold: true
                                        Layout.columnSpan: 2; topPadding: 6; bottomPadding: 1 }

                                Label { text: "Enable"; color: Theme.muted; font.pixelSize: 12 }
                                Switch {
                                    scale: 0.72; Layout.preferredHeight: 34
                                    checked: avatarTab._getCfg("dual_hue", false)
                                    onToggled: avatarTab._setCfg("dual_hue", checked)
                                }

                                Label { text: "Mode"; color: Theme.muted; font.pixelSize: 12 }
                                ComboBox {
                                    implicitHeight: 22; font.pixelSize: 11
                                    model: ["topbot", "leftright", "diagonal", "checker"]
                                    currentIndex: {
                                        var modes = ["topbot","leftright","diagonal","checker"]
                                        var idx = modes.indexOf(avatarTab._getCfg("dual_hue_mode", "topbot"))
                                        return idx < 0 ? 0 : idx
                                    }
                                    onActivated: avatarTab._setCfg("dual_hue_mode", currentText)
                                }

                                Label { text: "ISLANDS"; color: Theme.accent; font.pixelSize: 10; font.bold: true
                                        Layout.columnSpan: 2; topPadding: 6; bottomPadding: 1 }

                                Label { text: "Enable"; color: Theme.muted; font.pixelSize: 12 }
                                Switch {
                                    scale: 0.72; Layout.preferredHeight: 34
                                    checked: avatarTab._getCfg("islands", true)
                                    onToggled: avatarTab._setCfg("islands", checked)
                                }

                                Label { text: "Connectivity"; color: Theme.muted; font.pixelSize: 12 }
                                ComboBox {
                                    implicitHeight: 22; font.pixelSize: 11
                                    model: ["4-conn", "8-conn"]
                                    currentIndex: avatarTab._getCfg("island_conn", 8) === 8 ? 1 : 0
                                    onActivated: avatarTab._setCfg("island_conn", currentIndex === 1 ? 8 : 4)
                                }

                                Label { text: "Island step"; color: Theme.muted; font.pixelSize: 12 }
                                Slider {
                                    Layout.fillWidth: true; from: 0.1; to: 1.0; stepSize: 0.01
                                    value: avatarTab._getCfg("island_step", 0.62)
                                    onMoved: avatarTab._setCfg("island_step", parseFloat(value.toFixed(2)))
                                }

                                Label { text: "Varied sat"; color: Theme.muted; font.pixelSize: 12 }
                                Switch {
                                    scale: 0.72; Layout.preferredHeight: 34
                                    checked: avatarTab._getCfg("island_varsat", true)
                                    onToggled: avatarTab._setCfg("island_varsat", checked)
                                }
                            } // GridLayout
                        }
                    }
                }
            }
        } // end ScrollView (Identity & Avatar)

        // ── Tab 2: General ────────────────────────────────────────────────
        ScrollView { contentWidth: availableWidth; clip: true
        ColumnLayout { x: 20; y: 12; width: parent.availableWidth - 40; spacing: 12

        // ── General ───────────────────────────────────────────────────────
        GroupBox {
            Layout.fillWidth: true
            topPadding: 28
            label: Label { text: "General"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
            background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

            ColumnLayout {
                anchors.left: parent.left
                anchors.right: parent.right
                spacing: 8

                CheckBox {
                    text: "Enable notifications"
                    checked: root.settings ? root.settings.notifications_enabled : true
                    onCheckedChanged: if (root.settings) root.settings.notifications_enabled = checked
                }

                CheckBox {
                    text: "Auto-connect to known peers"
                    checked: root.settings ? root.settings.auto_connect : false
                    onCheckedChanged: if (root.settings) root.settings.auto_connect = checked
                }

                CheckBox {
                    text: "Start minimized"
                    checked: root.settings ? root.settings.start_minimized : false
                    onCheckedChanged: if (root.settings) root.settings.start_minimized = checked
                }

                CheckBox {
                    text: "Check for updates automatically"
                    checked: root.settings ? root.settings.update_check_enabled : true
                    onCheckedChanged: if (root.settings) root.settings.update_check_enabled = checked
                }

                CheckBox {
                    text: "Enable UPnP port mapping"
                    checked: root.settings ? root.settings.upnp_enabled : true
                    onCheckedChanged: if (root.settings) root.settings.upnp_enabled = checked
                }

                // ── Appearance ───────────────────────────────────────────
                RowLayout {
                    spacing: 8
                    Label { text: "Theme"; color: Theme.muted }
                    ComboBox {
                        id: themeCombo
                        width: 130
                        model: ["System", "Dark", "Light"]
                        readonly property var _values: ["system", "dark", "light"]
                        currentIndex: {
                            var v = root.settings ? root.settings.theme : "system"
                            var i = _values.indexOf(v); return i >= 0 ? i : 0
                        }
                        onActivated: if (root.settings) root.settings.theme = _values[currentIndex]
                    }
                }
            }
        }

        } } // end ColumnLayout + ScrollView (General)

        // ── Tab 3: AI Assistant ───────────────────────────────────────────
        ScrollView { contentWidth: availableWidth; clip: true
        ColumnLayout { x: 20; y: 12; width: parent.availableWidth - 40; spacing: 12

        // ── AI Assistant (Ollama) ─────────────────────────────────────────
        GroupBox {
            Layout.fillWidth: true
            topPadding: 28
            label: Label { text: "AI Assistant"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
            background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

            ColumnLayout {
                anchors.left: parent.left
                anchors.right: parent.right
                spacing: 8

                CheckBox {
                    id: ollamaEnabledCheck
                    text: "Enable AI assistant (Ollama)"
                    checked: root.settings ? root.settings.ollama_enabled : false
                    onCheckedChanged: if (root.settings) root.settings.ollama_enabled = checked
                }

                GridLayout {
                    columns: 2
                    columnSpacing: 8
                    rowSpacing: 6
                    enabled: ollamaEnabledCheck.checked
                    opacity: enabled ? 1.0 : 0.45

                    Label { text: "Base URL"; color: Theme.muted }
                    TextField {
                        id: ollamaUrlField
                        Layout.fillWidth: true
                        text: root.settings ? root.settings.ollama_base_url : "http://localhost:11434"
                        placeholderText: "http://localhost:11434"
                        color: Theme.text
                        placeholderTextColor: Theme.muted
                        background: Rectangle { color: Theme.bg3; radius: 0 }
                        onEditingFinished: {
                            if (root.settings) root.settings.ollama_base_url = text
                            // Re-fetch models whenever the URL changes
                            if (backend) {
                                ollamaModelStatus.text = "Loading models\u2026"
                                backend.fetchOllamaModels(text)
                            }
                        }
                    }

                    Label { text: "Model"; color: Theme.muted }
                    RowLayout {
                        spacing: 4
                        Layout.fillWidth: true
                        ComboBox {
                            id: ollamaModelCombo
                            Layout.fillWidth: true
                            editable: true
                            model: [root.settings ? root.settings.ollama_model : "llama3"]
                            Component.onCompleted: {
                                editText = root.settings ? root.settings.ollama_model : "llama3"
                            }
                            onEditTextChanged: if (root.settings) root.settings.ollama_model = editText
                            contentItem: TextField {
                                text: ollamaModelCombo.editText
                                onTextChanged: ollamaModelCombo.editText = text
                                color: Theme.text
                                background: Rectangle { color: Theme.bg3; radius: 0 }
                                leftPadding: 6
                            }
                            background: Rectangle { color: Theme.bg3; radius: 0 }
                        }
                        ToolButton {
                            icon.source: "qrc:/qt/qml/ConquerD/Client/icons/refresh.svg"
                            icon.width: 14
                            icon.height: 14
                            icon.color: Theme.muted
                            flat: true
                            implicitWidth: 32; implicitHeight: 32
                            ToolTip.text: "Fetch available models from Ollama"
                            ToolTip.visible: hovered
                            onClicked: {
                                ollamaModelStatus.text = "Loading models\u2026"
                                if (backend) backend.fetchOllamaModels(
                                    root.settings ? root.settings.ollama_base_url : "")
                            }
                        }
                    }

                    Item {}  // spacer for grid alignment
                    Label {
                        id: ollamaModelStatus
                        text: "Use refresh to load available models"
                        color: Theme.muted; font.pixelSize: 11
                        wrapMode: Text.Wrap; Layout.fillWidth: true
                    }

                    Label { text: "System Prompt"; color: Theme.muted; Layout.alignment: Qt.AlignTop }
                    ScrollView {
                        Layout.fillWidth: true
                        implicitHeight: 80
                        TextArea {
                            id: ollamaSystemPromptArea
                            wrapMode: TextArea.Wrap
                            text: root.settings ? root.settings.ollama_system_prompt
                                               : "You are a helpful assistant."
                            placeholderText: "You are a helpful assistant."
                            color: Theme.text
                            placeholderTextColor: Theme.muted
                            background: Rectangle { color: Theme.bg3; radius: 0 }
                            onEditingFinished: if (root.settings) root.settings.ollama_system_prompt = text
                        }
                    }

                    Item {}  // spacer
                    ColumnLayout {
                        spacing: 4
                        CheckBox {
                            text: "Auto-reply to direct peer messages"
                            checked: root.settings ? root.settings.ollama_auto_respond_direct : false
                            onCheckedChanged: if (root.settings) root.settings.ollama_auto_respond_direct = checked
                        }
                        CheckBox {
                            text: "Auto-reply to room chat messages"
                            checked: root.settings ? root.settings.ollama_auto_respond_room : false
                            onCheckedChanged: if (root.settings) root.settings.ollama_auto_respond_room = checked
                        }
                    }
                }

                Label {
                    text: "Ollama must be running locally. Responses are never sent off-device."
                    color: Theme.muted; font.pixelSize: 11
                    wrapMode: Text.Wrap; Layout.fillWidth: true
                }
            }

            // Auto-fetch model list when the settings page loads
            Component.onCompleted: {
                if (backend) backend.fetchOllamaModels(
                    root.settings ? root.settings.ollama_base_url : "")
            }

            // Handle async model-list result
            Connections {
                target: backend
                function onOllamaModelsReady(models, error) {
                    if (error !== "") {
                        ollamaModelStatus.text = "Error: " + error
                        return
                    }
                    var names = JSON.parse(models)
                    if (names.length === 0) {
                        ollamaModelStatus.text = "No models found"
                        return
                    }
                    var saved = ollamaModelCombo.editText
                    ollamaModelCombo.model = names
                    var idx = names.indexOf(saved)
                    if (idx >= 0) {
                        ollamaModelCombo.currentIndex = idx
                    } else {
                        ollamaModelCombo.editText = saved
                    }
                    ollamaModelStatus.text = "Found " + names.length
                        + (names.length === 1 ? " model" : " models")
                }
            }
        }

        } } // end ColumnLayout + ScrollView (AI)

        // ── Tab 4: Network ────────────────────────────────────────────────
        ScrollView { contentWidth: availableWidth; clip: true
        ColumnLayout { x: 20; y: 12; width: parent.availableWidth - 40; spacing: 12

        // ── Relay ─────────────────────────────────────────────────────────
        GroupBox {
            Layout.fillWidth: true
            topPadding: 28
            label: Label { text: "Relay"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
            background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

            ColumnLayout {
                anchors.left: parent.left
                anchors.right: parent.right
                spacing: 8

                CheckBox {
                    text: "Allow relay-gated connections"
                    checked: root.settings ? root.settings.relay_allow_gated : true
                    onCheckedChanged: if (root.settings) root.settings.relay_allow_gated = checked
                }

                CheckBox {
                    text: "Auto-renew relay tickets"
                    checked: root.settings ? root.settings.relay_auto_renew : true
                    onCheckedChanged: if (root.settings) root.settings.relay_auto_renew = checked
                }

                RowLayout {
                    spacing: 8
                    Label { text: "Relay port (0 = automatic)"; color: Theme.muted }
                    TextField {
                        text: root.settings ? root.settings.relay_port.toString() : "0"
                        inputMethodHints: Qt.ImhDigitsOnly
                        validator: IntValidator { bottom: 0; top: 65535 }
                        onEditingFinished: if (root.settings) root.settings.relay_port = parseInt(text) || 0
                        color: Theme.text
                        background: Rectangle { color: Theme.bg3; radius: 0 }
                        width: 70
                    }
                }
            }
        }

        } } // end ColumnLayout + ScrollView (Network)

        // ── Tab 5: Security ───────────────────────────────────────────────
        ScrollView { contentWidth: availableWidth; clip: true
        ColumnLayout { x: 20; y: 12; width: parent.availableWidth - 40; spacing: 12

        // ── Security ──────────────────────────────────────────────────────
        GroupBox {
            Layout.fillWidth: true
            topPadding: 28
            label: Label { text: "Security"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
            background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

            ColumnLayout {
                anchors.left: parent.left
                anchors.right: parent.right
                spacing: 8

                RowLayout {
                    spacing: 8
                    Label { text: "Build attestation"; color: Theme.muted }
                    ComboBox {
                        id: attCombo
                        width: 130
                        model: ["Off", "Warn", "Strict"]
                        readonly property var _values: ["off", "warn", "strict"]
                        currentIndex: {
                            var v = root.settings ? root.settings.attestation_policy : "warn"
                            var i = _values.indexOf(v); return i >= 0 ? i : 1
                        }
                        onActivated: if (root.settings) root.settings.attestation_policy = _values[currentIndex]
                    }
                }

                Label {
                    text: "Warn: challenge peers after handshake · Strict: deny relay for invalid attestation"
                    color: Theme.muted; font.pixelSize: 11
                    wrapMode: Text.Wrap; Layout.fillWidth: true
                }

                CheckBox {
                    text: "Show YouTube preview cards in chat"
                    checked: root.settings ? root.settings.youtube_preview_enabled : true
                    onCheckedChanged: if (root.settings) root.settings.youtube_preview_enabled = checked
                    ToolTip.text: "Displays a click-to-open card when a YouTube link is detected. No thumbnails are fetched."
                    ToolTip.visible: hovered
                }

            }
        }

        } } // end ColumnLayout + ScrollView (Security)

        // ── Tab 6: Privacy & Data ─────────────────────────────────────────
        ScrollView { contentWidth: availableWidth; clip: true
        ColumnLayout { x: 20; y: 12; width: parent.availableWidth - 40; spacing: 12

        // ── Privacy & Data ─────────────────────────────────────────────
        GroupBox {
            id: privacyBox
            Layout.fillWidth: true
            topPadding: 28
            label: Label { text: "Privacy & Data"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
            background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

            property int storedCount: 0
            property bool _confirmPurge: false
            property bool _confirmLock: false

            Component.onCompleted: if (backend) privacyBox.storedCount = backend.getStoredMessageCount()

            ColumnLayout {
                anchors.left: parent.left
                anchors.right: parent.right
                spacing: 10

                Label {
                    text: "Chat messages are stored encrypted on this device only."
                    color: Theme.muted; font.pixelSize: 11
                    wrapMode: Text.Wrap; Layout.fillWidth: true
                }

                Label {
                    text: "Stored messages: " + privacyBox.storedCount
                    color: Theme.muted
                }

                // ── Trim by age ──────────────────────────────────────
                RowLayout {
                    spacing: 8
                    Label { text: "Trim messages older than"; color: Theme.muted }
                    SpinBox {
                        id: trimAgeSpin
                        from: 1; to: 3650; value: 30
                        textFromValue: function(v) { return v + " days" }
                        valueFromText: function(t) { return parseInt(t) || 30 }
                        implicitWidth: 110
                    }
                    Button {
                        text: "Trim by Age"
                        font.pixelSize: 12
                        onClicked: {
                            if (backend) backend.trimMessagesByAge(trimAgeSpin.value)
                            privacyBox.storedCount = backend ? backend.getStoredMessageCount() : 0
                        }
                    }
                }

                // ── Trim by count ────────────────────────────────────
                RowLayout {
                    spacing: 8
                    Label { text: "Keep most recent"; color: Theme.muted }
                    SpinBox {
                        id: trimCountSpin
                        from: 1; to: 100000; value: 500
                        textFromValue: function(v) { return v + " msgs" }
                        valueFromText: function(t) { return parseInt(t) || 500 }
                        implicitWidth: 110
                    }
                    Label { text: "per conversation"; color: Theme.muted }
                    Button {
                        text: "Trim to Limit"
                        font.pixelSize: 12
                        onClicked: {
                            if (backend) backend.trimMessagesByCount(trimCountSpin.value)
                            privacyBox.storedCount = backend ? backend.getStoredMessageCount() : 0
                        }
                    }
                }

                // ── Danger zone ──────────────────────────────────────
                Label { text: "Danger Zone"; color: Theme.text; font.bold: true; font.pixelSize: 12; topPadding: 4 }

                ColumnLayout {
                    spacing: 6
                    visible: privacyBox._confirmPurge

                    Label {
                        text: "This will permanently delete ALL chat history. Are you sure?"
                        color: Theme.danger; wrapMode: Text.Wrap; Layout.fillWidth: true
                        font.pixelSize: 11
                    }
                    RowLayout {
                        spacing: 8
                        Button {
                            text: "Yes, delete everything"
                            font.pixelSize: 12
                            background: Rectangle { color: Theme.danger; radius: 0 }
                            contentItem: Text { text: parent.text; color: Theme.textInv; font: parent.font; horizontalAlignment: Text.AlignHCenter }
                            onClicked: {
                                if (backend) backend.purgeAllChatHistory()
                                privacyBox.storedCount = backend ? backend.getStoredMessageCount() : 0
                                privacyBox._confirmPurge = false
                            }
                        }
                        Button {
                            text: "Cancel"
                            font.pixelSize: 12
                            flat: true
                            onClicked: privacyBox._confirmPurge = false
                        }
                    }
                }

                Button {
                    visible: !privacyBox._confirmPurge
                    text: "Purge All Chat History"
                    font.pixelSize: 12
                    background: Rectangle { color: Theme.danger; radius: 0 }
                    contentItem: Text { text: parent.text; color: Theme.textInv; font: parent.font; horizontalAlignment: Text.AlignHCenter }
                    onClicked: privacyBox._confirmPurge = true
                }

                // ── Identity lock ────────────────────────────────────
                Label {
                    text: "Identity Lock"; color: Theme.text; font.bold: true; font.pixelSize: 12; topPadding: 4
                }
                Label {
                    text: "Removes the saved key from your OS keyring and quits. You will need your passphrase on next launch."
                    color: Theme.muted; font.pixelSize: 11
                    wrapMode: Text.Wrap; Layout.fillWidth: true
                }

                ColumnLayout {
                    spacing: 6
                    visible: privacyBox._confirmLock

                    Label {
                        text: "Lock identity and quit now?"
                        color: Theme.danger; font.pixelSize: 11
                    }
                    RowLayout {
                        spacing: 8
                        Button {
                            text: "Yes, lock & quit"
                            font.pixelSize: 12
                            background: Rectangle { color: Theme.danger; radius: 0 }
                            contentItem: Text { text: parent.text; color: Theme.textInv; font: parent.font; horizontalAlignment: Text.AlignHCenter }
                            onClicked: if (backend) backend.lockIdentityAndQuit()
                        }
                        Button {
                            text: "Cancel"
                            font.pixelSize: 12
                            flat: true
                            onClicked: privacyBox._confirmLock = false
                        }
                    }
                }

                Button {
                    visible: !privacyBox._confirmLock
                    text: "Lock Identity \u0026 Quit"
                    font.pixelSize: 12
                    background: Rectangle { color: Theme.danger; radius: 0 }
                    contentItem: Text { text: parent.text; color: Theme.textInv; font: parent.font; horizontalAlignment: Text.AlignHCenter }
                    onClicked: privacyBox._confirmLock = true
                }
            }
        }

        } } // end ColumnLayout + ScrollView (Privacy)

        // ── Tab 7: Diagnostics & About ────────────────────────────────────
        ScrollView { contentWidth: availableWidth; clip: true
        ColumnLayout { x: 20; y: 12; width: parent.availableWidth - 40; spacing: 12

        // ── Diagnostics ───────────────────────────────────────────────────
        GroupBox {
            id: diagnosticsBox
            Layout.fillWidth: true
            topPadding: 28
            label: Label { text: "Diagnostics"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
            background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

            property string _logText: ""

            // Auto-refresh log while this box is visible
            Timer {
                running: diagnosticsBox.visible
                interval: 2000
                repeat: true
                triggeredOnStart: true
                onTriggered: diagnosticsBox._logText = backend.getEventLogs()
            }

            ColumnLayout {
                anchors.left: parent.left
                anchors.right: parent.right
                spacing: 8

                ScrollView {
                    Layout.fillWidth: true
                    implicitHeight: 160
                    clip: true
                    background: Rectangle { color: Theme.bg1; radius: 0 }
                    ScrollBar.vertical.policy: ScrollBar.AlwaysOn

                    TextArea {
                        readOnly: true
                        text: diagnosticsBox._logText
                        color: Theme.muted
                        font.family: "Courier New, monospace"
                        font.pixelSize: 11
                        wrapMode: Text.WrapAnywhere
                        background: Item {}
                        padding: 6
                    }
                }

                RowLayout {
                    spacing: 8
                    ToolButton {
                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/refresh.svg"
                        icon.width: 14
                        icon.height: 14
                        icon.color: Theme.muted
                        flat: true
                        implicitWidth: 32
                        implicitHeight: 28
                        ToolTip.text: "Refresh logs"
                        ToolTip.visible: hovered
                        onClicked: diagnosticsBox._logText = backend.getEventLogs()
                    }
                    ToolButton {
                        icon.source: "qrc:/qt/qml/ConquerD/Client/icons/trash.svg"
                        icon.width: 14
                        icon.height: 14
                        icon.color: Theme.danger
                        flat: true
                        implicitWidth: 32
                        implicitHeight: 28
                        ToolTip.text: "Clear logs"
                        ToolTip.visible: hovered
                        onClicked: {
                            backend.clearEventLogs()
                            diagnosticsBox._logText = ""
                        }
                    }
                }
            }
        }

        // ── About ─────────────────────────────────────────────────────────
        GroupBox {
            Layout.fillWidth: true
            topPadding: 28
            label: Label { text: "About"; color: Theme.muted; font.bold: true; font.pixelSize: 11 }
            background: Rectangle { color: Theme.bg2; radius: Theme.radiusSm }

            ColumnLayout {
                anchors.left: parent.left
                anchors.right: parent.right
                Label { text: "ConquerD — Privacy-first peer connectivity"; color: Theme.muted }
                Label { text: "Version " + Qt.application.version; color: Theme.muted }

                // ── Desktop shortcuts (Windows) ───────────────────────────
                ColumnLayout {
                    spacing: 4
                    RowLayout {
                        spacing: 8
                        Button {
                            text: "Create Desktop Shortcuts"
                            font.pixelSize: 12
                            ToolTip.text: "Add ConquerD shortcuts to the Desktop and Start Menu"
                            ToolTip.visible: hovered
                            onClicked: {
                                backend.createDesktopShortcuts()
                                shortcutStatusLabel._hasShortcuts = backend.hasDesktopShortcuts()
                            }
                        }
                        Button {
                            text: "Remove Shortcuts"
                            font.pixelSize: 12
                            flat: true
                            ToolTip.text: "Remove Desktop and Start Menu shortcuts"
                            ToolTip.visible: hovered
                            onClicked: {
                                backend.removeDesktopShortcuts()
                                shortcutStatusLabel._hasShortcuts = backend.hasDesktopShortcuts()
                            }
                        }
                    }
                    Label {
                        id: shortcutStatusLabel
                        font.pixelSize: 11
                        property bool _hasShortcuts: backend ? backend.hasDesktopShortcuts() : false
                        color: _hasShortcuts ? Theme.online : Theme.muted
                        text: _hasShortcuts ? "Status: Shortcuts exist" : "Status: No shortcuts found"
                    }
                }
            }
        }

        } } // end ColumnLayout + ScrollView (Diagnostics & About)

    } // StackLayout
}

