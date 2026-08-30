// SettingsPage.qml - token-driven settings content panel.

import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Item {
    id: root

    property var settings: null
    property int currentTab: 0

    // Section indices, in the order SettingsSidebar lists them and the order
    // the StackLayout below declares its pages. Named rather than inlined
    // because inserting a section shifts every later index — when Video was
    // added after Audio, bare `=== 3` comparisons silently started matching
    // Network instead of AI.
    readonly property int tabAudio: 0
    readonly property int tabVideo: 1
    readonly property int tabIdentity: 2
    readonly property int tabGeneral: 3
    readonly property int tabAi: 4
    readonly property int tabNetwork: 5
    readonly property int tabSecurity: 6
    readonly property int tabPrivacy: 7
    readonly property int tabDiagnostics: 8

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

    // Nothing on this page writes the file itself; every control changes the
    // property and lets the Save button say so. Applying live is separate — a
    // slider that only took effect after Save would be unusable — so this still
    // updates the avatar everywhere it is shown.
    function setAvatarValue(key, value) {
        if (!settings) return
        var cfg = avatarConfig()
        cfg[key] = value
        var json = JSON.stringify(cfg)
        settings.avatar_config_json = json
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
        currentIndex: Math.max(0, Math.min(root.currentTab, root.tabDiagnostics))

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

                SettingsSectionHeader { title: "Video" }

                SettingsCard {
                    id: cameraCard
                    title: "Camera"
                    subtitle: "Used when you turn video on during a call."

                    // Cameras are chosen by *name* but reopened by *id* (a
                    // device symbolic link), so the ids are kept alongside the
                    // combo's display model. Storing the name would break the
                    // moment two identical webcams are attached.
                    property var cameraIds: []
                    /// Display names, index-aligned with `cameraIds`.
                    property var cameraNames: []
                    /// Id the empty "Default camera" selection actually resolves
                    /// to, so the overlay picker can avoid offering the very
                    /// device the main source is about to open.
                    property string firstCameraId: ""

                    /// Whether anything is currently feeding the preview surface.
                    ///
                    /// Either capture will: a call sends the same pre-encode
                    /// frames it shows locally, and the preview below is that
                    /// same surface. So while sharing is on the preview simply
                    /// shows the outgoing stream rather than opening the device
                    /// a second time — which would fail anyway.
                    readonly property bool previewLive: backend
                        ? (backend.video_active || backend.video_preview_active)
                        : false

                    /// Whether this build can encode video at all. Resolved once
                    /// on load — VP8 is vendored on every platform, so a false
                    /// here means something is genuinely wrong with the build.
                    property bool encoderAvailable: true

                    /// Why Preview cannot start, or "" when it can.
                    readonly property string previewBlockedReason:
                        !cameraCard.encoderAvailable
                            ? qsTr("Video unavailable on this platform — no encoder.")
                            : (cameraCombo.count <= 1
                                ? qsTr("No camera or capture source detected.")
                                : "")

                    function refreshEncoderAvailable() {
                        if (!backend) return
                        try {
                            var res = JSON.parse(backend.listVideoCodecs())
                            cameraCard.encoderAvailable = (res.codecs || []).length > 0
                        } catch (e) {
                            // Assume available: a failed probe must not lock the
                            // user out of a feature that may well work.
                            console.warn("settings: could not list video codecs:", e)
                            cameraCard.encoderAvailable = true
                        }
                    }

                    function startPreview() {
                        if (!backend || !root.settings) return
                        backend.setVideoPreviewEnabled(
                            true,
                            root.settings.video_input_device,
                            root.settings.video_quality,
                            root.settings.video_overlays_json,
                            root.settings.videoEncoderJson())
                    }

                    function stopPreview() {
                        // Only ever stops the preview capture — a call's camera
                        // is not this button's to turn off.
                        if (backend) backend.setVideoPreviewEnabled(false, "", "", "", "")
                    }

                    function applyCameraSettings() {
                        if (!root.settings || !backend) return
                        // Restart whichever capture is running so a device,
                        // quality, or layout change takes effect immediately
                        // rather than at the next call: the running thread still
                        // holds the old devices, so nothing else can open them.
                        if (backend.video_active) {
                            backend.setVideoEnabled(
                                true,
                                root.settings.video_input_device,
                                root.settings.video_quality,
                                root.settings.video_overlays_json,
                                root.settings.videoEncoderJson())
                        } else if (backend.video_preview_active) {
                            cameraCard.startPreview()
                        }
                    }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: Theme.spacingLg
                        rowSpacing: Theme.spacingMd

                        Label { text: "Source"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: cameraCombo
                            Layout.fillWidth: true
                            model: ["Default camera"]
                            onActivated: {
                                if (!root.settings) return
                                // Index 0 is "Default" — an empty id means
                                // "first available camera".
                                root.settings.video_input_device =
                                    currentIndex === 0 ? "" : (cameraCard.cameraIds[currentIndex - 1] || "")
                                cameraCard.applyCameraSettings()
                            }
                        }

                        Label { text: "Audio"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        // The share button asks about audio each time it starts
                        // a share, and writes the answer back here — so this is
                        // the pre-selected default rather than a separate
                        // setting that could disagree with what was chosen.
                        ComboBox {
                            id: contentAudioCombo
                            Layout.fillWidth: true
                            // "Follow" is first because it is right almost
                            // always: sharing an app sends that app's sound,
                            // sharing a screen sends the machine's, and a camera
                            // sends none — your microphone already carries you.
                            model: [
                                "Follow the shared source",
                                "This computer's audio",
                                "No audio"
                            ]
                            property var keys: ["auto", "system", "off"]
                            onActivated: {
                                if (!root.settings) return
                                root.settings.content_audio_mode = keys[currentIndex] || "auto"
                            }
                        }

                        // Empty cell keeps the two-column grid aligned.
                        Item {}
                        Label {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeMicro
                            text: {
                                var dev = root.settings ? root.settings.video_input_device : ""
                                var isScreen = dev.indexOf("window:") === 0
                                var isMonitor = dev.indexOf("monitor:") === 0
                                var mode = root.settings ? root.settings.content_audio_mode : "auto"
                                if (mode === "off")
                                    return qsTr("Shared video will be silent.")
                                if (mode === "system")
                                    return qsTr("Everything this computer plays is shared, whatever the source.")
                                if (isScreen)
                                    return qsTr("Only the shared application's audio is sent.")
                                if (isMonitor)
                                    return qsTr("Everything this computer plays is sent.")
                                return qsTr("A camera shares no extra audio — your microphone already carries you.")
                            }
                        }
                    }

                    // Preview of the selected source.
                    //
                    // Loaded rather than declared inline: a build without Qt
                    // Multimedia has no VideoTile in the qrc at all (see
                    // build.rs), and this page carries every other setting, so
                    // it must still parse there. VideoTile is reused as-is
                    // because the sink registration it does is the whole job —
                    // a second component would be a second lifecycle to leak.
                    Item {
                        Layout.preferredWidth: 320
                        Layout.preferredHeight: 180

                        Rectangle {
                            anchors.fill: parent
                            radius: Theme.radiusSm
                            color: Theme.bg1
                            border.color: Theme.bg3
                            border.width: 1
                            clip: true

                            Loader {
                                id: previewLoader
                                anchors.fill: parent
                                anchors.margins: 1
                                // Bound only while this page is on screen: the
                                // tile registers a video sink, and a registered
                                // sink is what tells a capture thread somebody
                                // is watching.
                                active: root.visible && root.currentTab === root.tabVideo
                                source: "qrc:/qt/qml/ConquerD/Client/qml/VideoTile.qml"

                                onLoaded: {
                                    // Our own id: captured frames are shown
                                    // locally under it, by the call path and
                                    // the preview path alike.
                                    item.peerId = Qt.binding(() => backend ? backend.public_id : "")
                                    item.streaming = Qt.binding(() => cameraCard.previewLive)
                                    item.showChrome = false
                                }
                                // Navigating away must not leave the camera on.
                                onActiveChanged: if (!active) cameraCard.stopPreview()
                            }

                            Label {
                                anchors.centerIn: parent
                                anchors.margins: Theme.spacingMd
                                width: parent.width - Theme.spacingLg
                                visible: previewLoader.status === Loader.Error
                                text: "Video preview is unavailable in this build."
                                horizontalAlignment: Text.AlignHCenter
                                wrapMode: Text.WordWrap
                                color: Theme.muted
                                font.pixelSize: Theme.fontSizeCaption
                            }
                        }
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingMd

                        Label {
                            text: !cameraCard.encoderAvailable
                                ? "Video unavailable on this platform — no encoder."
                                : (cameraCombo.count <= 1
                                    ? "No capture sources detected."
                                    : (backend && backend.video_active
                                        ? "Sharing is on."
                                        : (backend && backend.video_preview_active
                                            ? "Previewing — nobody else can see this."
                                            : "Sharing is off.")))
                            color: cameraCard.previewBlockedReason !== "" ? Theme.warn : Theme.muted
                            font.pixelSize: Theme.fontSizeCaption
                        }

                        StyledButton {
                            // Hidden while a call is sending: the preview is
                            // that stream, so stopping it here would mean
                            // stopping the call's camera from the settings page.
                            visible: !(backend && backend.video_active)
                            // Previously this stayed enabled with no camera and
                            // no encoder, so pressing it did nothing at all and
                            // said nothing about why. Stopping is always allowed.
                            enabled: previewLoader.status !== Loader.Error
                                     && (cameraCard.previewLive
                                         || cameraCard.previewBlockedReason === "")
                            text: cameraCard.previewLive ? "Stop preview" : "Preview"
                            onClicked: cameraCard.previewLive
                                ? cameraCard.stopPreview()
                                : cameraCard.startPreview()

                            ToolTip.text: cameraCard.previewBlockedReason
                            ToolTip.visible: hovered && cameraCard.previewBlockedReason !== ""
                        }

                        StyledButton {
                            // Windows come and go constantly, so the list is a
                            // snapshot rather than live — this is how the user
                            // picks up an app opened since the page loaded.
                            text: "Rescan"
                            onClicked: cameraCard.reloadCameras()
                        }

                        Item { Layout.fillWidth: true }
                    }

                    // Cameras, displays, and windows share one list because any
                    // of them can play either role: the main source that fills
                    // the frame, or an inset drawn over it.
                    function reloadCameras() {
                        if (!backend) return
                        var names = []
                        var ids = []
                        var firstCam = ""
                        try {
                            var res = JSON.parse(backend.listVideoDevices())
                            var groups = [
                                { items: res.cameras || [], prefix: "Camera — " },
                                { items: res.screens || [], prefix: "" },
                                { items: res.windows || [], prefix: "Window — " }
                            ]
                            firstCam = (res.cameras && res.cameras.length > 0)
                                ? (res.cameras[0].id || "") : ""
                            for (var g = 0; g < groups.length; g++) {
                                var items = groups[g].items
                                for (var i = 0; i < items.length; i++) {
                                    names.push(groups[g].prefix + (items[i].name || items[i].id))
                                    ids.push(items[i].id)
                                }
                            }
                        } catch (e) {}
                        cameraCard.cameraIds = ids
                        // Kept alongside the ids so the overlay rows can offer
                        // the same sources without the "Default camera" entry,
                        // which has no meaning for an inset.
                        cameraCard.cameraNames = names
                        cameraCard.firstCameraId = firstCam
                        cameraCombo.model = ["Default camera"].concat(names)
                        cameraCard.syncFromSettings()
                        // The overlay rows pick their source out of this same
                        // list, so rebuild them against the new one rather than
                        // leaving each row's combo pointing at a stale index.
                        pipCard.reload()
                    }

                    /// Point the combos at what is actually stored.
                    ///
                    /// Reselects the source by id: a camera can be unplugged and
                    /// a window closed between sessions, so falling back to
                    /// Default is expected, not an error.
                    function syncFromSettings() {
                        if (!root.settings) return
                        var idx = 0
                        if (root.settings.video_input_device !== "") {
                            var at = cameraCard.cameraIds.indexOf(root.settings.video_input_device)
                            idx = at >= 0 ? at + 1 : 0
                        }
                        cameraCombo.currentIndex = idx
                        var a = contentAudioCombo.keys.indexOf(root.settings.content_audio_mode)
                        contentAudioCombo.currentIndex = a >= 0 ? a : 0
                    }

                    // Settings are loaded from disk in MainWindow's own
                    // Component.onCompleted, which runs *after* its children —
                    // this page included. The stored selection therefore arrives
                    // as a property change, not as an initial value, and reading
                    // it once at completion would only ever see the defaults.
                    Connections {
                        target: root.settings
                        function onVideo_input_deviceChanged() { cameraCard.syncFromSettings() }
                        function onContent_audio_modeChanged() { cameraCard.syncFromSettings() }
                    }

                    Component.onCompleted: {
                        cameraCard.reloadCameras()
                        cameraCard.refreshEncoderAvailable()
                    }
                }

                // Encoding and streaming.
                //
                // An encoder is configured once at construction and cannot be
                // resized under a running capture, so every control here
                // restarts one through `cameraCard.applyCameraSettings`. The
                // adaptation switch is the exception: it only steers the rate
                // controller, so it applies live.
                //
                // No preset values are written here. The table lives in
                // `Quality::from_name`, and this card reads back what the
                // encoder will actually be given via
                // `effectiveVideoQualityJson()` — a copy in QML would drift and
                // quietly mislabel every preset.
                SettingsCard {
                    id: encodingCard
                    title: "Encoding"
                    subtitle: "What leaves this machine when you share video. Changes apply "
                        + "immediately to a share that is already running."

                    // "custom" is the one key with meaning in Rust: it is what
                    // makes the resolution and frame rate below take effect at
                    // all. See `Quality::resolve`.
                    readonly property var presetKeys: ["low", "balanced", "high", "custom"]
                    readonly property var presetLabels: ["Low", "Balanced", "High", "Custom"]

                    readonly property var resolutions: [
                        "320x180", "480x270", "640x360", "854x480",
                        "960x540", "1280x720", "1600x900", "1920x1080"
                    ]
                    readonly property var frameRates: [10, 15, 20, 24, 30, 48, 60]
                    // 0 is Auto — the bitrate the encoder derives from the size
                    // and frame rate actually chosen.
                    readonly property var bitrates: [
                        0, 250, 400, 600, 800, 1200, 1500, 2000, 2500, 3000, 4000, 6000, 8000
                    ]
                    readonly property var keyframeSecs: [1, 2, 4, 8, 10]

                    /// Codec ids offered by the picker, "auto" first. Filled
                    /// from the backend so a build without an H.264 encoder
                    /// never offers H.264.
                    property var codecIds: ["auto"]

                    /// The settings the encoder will actually be configured
                    /// with, once preset and overrides are resolved together.
                    /// Recomputed rather than cached because a preset change
                    /// moves all of them at once.
                    property var effective: ({ width: 640, height: 360, fps: 30,
                                               bitrate_bps: 600000, keyframe_secs: 4 })

                    /// True while `syncFromSettings` is writing combo indices,
                    /// so the `onActivated` handlers can tell a user's click
                    /// from the sync that follows it. Without this, syncing
                    /// would look like an edit and flip the preset to Custom.
                    property bool syncing: false

                    function refreshEffective() {
                        if (!root.settings) return
                        try {
                            encodingCard.effective =
                                JSON.parse(root.settings.effectiveVideoQualityJson())
                        } catch (e) {}
                    }

                    /// Record an explicit choice for one of the three settings a
                    /// preset also decides, and switch the preset to Custom.
                    ///
                    /// Silently leaving the preset saying "Balanced" while the
                    /// resolution said 1080p would be a label that lies, so the
                    /// preset follows the edit rather than the other way round.
                    function takeOver() {
                        if (!root.settings) return
                        if (root.settings.video_quality === "custom") return
                        // Freeze what the preset was giving, so switching to
                        // Custom changes exactly the one field just edited.
                        root.settings.video_resolution =
                            encodingCard.effective.width + "x" + encodingCard.effective.height
                        root.settings.video_fps = encodingCard.effective.fps
                        root.settings.video_quality = "custom"
                    }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        columnSpacing: Theme.spacingLg
                        rowSpacing: Theme.spacingMd

                        Label { text: "Quality"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: qualityCombo
                            Layout.fillWidth: true
                            // The stored value is the lowercase key, not the label.
                            model: encodingCard.presetLabels
                            onActivated: {
                                if (!root.settings || encodingCard.syncing) return
                                root.settings.video_quality =
                                    encodingCard.presetKeys[currentIndex] || "balanced"
                                cameraCard.applyCameraSettings()
                            }
                        }

                        Label { text: "Resolution"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: resolutionCombo
                            Layout.fillWidth: true
                            model: encodingCard.resolutions
                            onActivated: {
                                if (!root.settings || encodingCard.syncing) return
                                // Read before `takeOver`: it writes settings,
                                // which re-syncs this combo back to the value
                                // that was in effect a moment ago.
                                var picked = encodingCard.resolutions[currentIndex] || ""
                                encodingCard.takeOver()
                                root.settings.video_resolution = picked
                                cameraCard.applyCameraSettings()
                            }
                        }

                        Label { text: "Frame rate"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: fpsCombo
                            Layout.fillWidth: true
                            model: encodingCard.frameRates.map(function(f) { return f + " fps" })
                            onActivated: {
                                // Read before `takeOver`, same as above.
                                if (!root.settings || encodingCard.syncing) return
                                var picked = encodingCard.frameRates[currentIndex] || 30
                                encodingCard.takeOver()
                                root.settings.video_fps = picked
                                cameraCard.applyCameraSettings()
                            }
                        }

                        Label { text: "Bitrate"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: bitrateVideoCombo
                            Layout.fillWidth: true
                            model: encodingCard.bitrates.map(function(kbps) {
                                return kbps === 0
                                    ? qsTr("Auto — match the resolution")
                                    : (kbps >= 1000
                                        ? (kbps / 1000).toFixed(kbps % 1000 === 0 ? 0 : 1) + " Mbps"
                                        : kbps + " kbps")
                            })
                            onActivated: {
                                if (!root.settings || encodingCard.syncing) return
                                // Unlike resolution and frame rate, this does
                                // not force Custom: an explicit bitrate on top
                                // of a preset is a coherent thing to want, and
                                // the preset label stays honest about the size.
                                root.settings.video_bitrate_kbps =
                                    encodingCard.bitrates[currentIndex] || 0
                                cameraCard.applyCameraSettings()
                            }
                        }

                        Label { text: "Keyframe every"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: keyframeCombo
                            Layout.fillWidth: true
                            model: encodingCard.keyframeSecs.map(function(s) {
                                return s === 1 ? qsTr("1 second") : s + qsTr(" seconds")
                            })
                            onActivated: {
                                if (!root.settings || encodingCard.syncing) return
                                root.settings.video_keyframe_secs =
                                    encodingCard.keyframeSecs[currentIndex] || 4
                                cameraCard.applyCameraSettings()
                            }
                        }

                        Item {}
                        Label {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeMicro
                            text: qsTr("Shorter means someone joining sees a picture sooner and "
                                       + "costs more of the bitrate; longer is cheaper but leaves "
                                       + "a late joiner waiting.")
                        }

                        Label { text: "Codec"; color: Theme.muted; Layout.alignment: Qt.AlignRight }
                        ComboBox {
                            id: codecCombo
                            Layout.fillWidth: true
                            model: [qsTr("Automatic — best both ends support")]
                            onActivated: {
                                if (!root.settings || encodingCard.syncing) return
                                root.settings.video_codec =
                                    encodingCard.codecIds[currentIndex] || "auto"
                                cameraCard.applyCameraSettings()
                            }
                        }

                        Item {}
                        Label {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeMicro
                            text: {
                                if (!root.settings || root.settings.video_codec === "auto")
                                    return qsTr("Each peer is sent the best codec you both have.")
                                return qsTr("Used when the peer can decode it; otherwise the best "
                                            + "shared codec is used instead, so this can never "
                                            + "stop video from working.")
                            }
                        }
                    }

                    SettingSwitch {
                        title: "Adapt bitrate to the connection"
                        description: "Lower the bitrate when packets start dropping, and climb "
                            + "back when the link clears. Turn this off to hold the bitrate you "
                            + "chose on a link you know is fine."
                        checked: root.settings ? root.settings.video_adaptive_bitrate : true
                        // Applied live rather than through a capture restart:
                        // this only steers the rate controller, and restarting
                        // the camera to flip a switch would drop frames.
                        onChanged: function(on) {
                            if (!root.settings) return
                            root.settings.video_adaptive_bitrate = on
                            if (backend) backend.setVideoAdaptiveBitrate(on)
                        }
                    }

                    // What all of the above adds up to, in one line. Read back
                    // from the resolver rather than assembled from the controls,
                    // so a clamp applied in Rust is visible here rather than
                    // silently disagreeing with what the combos show.
                    Label {
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                        text: {
                            var e = encodingCard.effective
                            var mbps = (e.bitrate_bps / 1000000).toFixed(2)
                            return qsTr("Sending %1x%2 at %3 fps, up to %4 Mbps.")
                                .arg(e.width).arg(e.height).arg(e.fps).arg(mbps)
                        }
                    }

                    /// Point every combo at what is actually stored.
                    ///
                    /// The resolution and frame rate combos are synced from the
                    /// *effective* values, not the stored overrides: on a preset
                    /// the overrides are ignored, and showing them would label
                    /// Balanced with whatever Custom was last set to.
                    function syncFromSettings() {
                        if (!root.settings) return
                        encodingCard.refreshEffective()
                        encodingCard.syncing = true

                        var p = encodingCard.presetKeys.indexOf(root.settings.video_quality)
                        qualityCombo.currentIndex = p >= 0 ? p : 1

                        var res = encodingCard.effective.width + "x" + encodingCard.effective.height
                        var r = encodingCard.resolutions.indexOf(res)
                        // A size with no entry — hand-edited, or a preset that
                        // gained one — is shown as the nearest offered instead
                        // of silently snapping the combo to the first entry.
                        resolutionCombo.currentIndex =
                            r >= 0 ? r : encodingCard.nearestResolutionIndex(encodingCard.effective.width)

                        var f = encodingCard.frameRates.indexOf(encodingCard.effective.fps)
                        fpsCombo.currentIndex = f >= 0 ? f : encodingCard.frameRates.indexOf(30)

                        var stored = root.settings.video_bitrate_kbps
                        var b = encodingCard.bitrates.indexOf(stored)
                        // A hand-edited rate is shown as the nearest offered
                        // one rather than as "Auto", which would claim the
                        // opposite of what the file actually says.
                        bitrateVideoCombo.currentIndex =
                            b >= 0 ? b : (stored > 0 ? encodingCard.nearestBitrateIndex(stored) : 0)

                        var k = encodingCard.keyframeSecs.indexOf(root.settings.video_keyframe_secs)
                        keyframeCombo.currentIndex = k >= 0 ? k : encodingCard.keyframeSecs.indexOf(4)

                        var c = encodingCard.codecIds.indexOf(root.settings.video_codec)
                        codecCombo.currentIndex = c >= 0 ? c : 0

                        encodingCard.syncing = false
                    }

                    /// Nearest offered bitrate, never Auto — index 0 means
                    /// "derive one", which is a different answer entirely.
                    function nearestBitrateIndex(kbps) {
                        var best = 1
                        var bestGap = 1e9
                        for (var i = 1; i < encodingCard.bitrates.length; i++) {
                            var gap = Math.abs(encodingCard.bitrates[i] - kbps)
                            if (gap < bestGap) { bestGap = gap; best = i }
                        }
                        return best
                    }

                    function nearestResolutionIndex(width) {
                        var best = 0
                        var bestGap = 1e9
                        for (var i = 0; i < encodingCard.resolutions.length; i++) {
                            var w = parseInt(encodingCard.resolutions[i].split("x")[0], 10)
                            var gap = Math.abs(w - width)
                            if (gap < bestGap) { bestGap = gap; best = i }
                        }
                        return best
                    }

                    /// Offer only codecs this build can actually encode — the
                    /// list is a build fact, so a preference the binary has no
                    /// encoder for must not be selectable at all.
                    function reloadCodecs() {
                        var ids = ["auto"]
                        var labels = [qsTr("Automatic — best both ends support")]
                        if (backend) {
                            try {
                                var res = JSON.parse(backend.listVideoCodecs())
                                var list = res.codecs || []
                                for (var i = 0; i < list.length; i++) {
                                    if (!list[i].id) continue
                                    ids.push(list[i].id)
                                    labels.push(list[i].name || list[i].id)
                                }
                            } catch (e) {}
                        }
                        encodingCard.codecIds = ids
                        codecCombo.model = labels
                        encodingCard.syncFromSettings()
                    }

                    // Same reason as the camera card's: the stored values arrive
                    // as property changes after this page is built.
                    Connections {
                        target: root.settings
                        function onVideo_qualityChanged() { encodingCard.syncFromSettings() }
                        function onVideo_resolutionChanged() { encodingCard.syncFromSettings() }
                        function onVideo_fpsChanged() { encodingCard.syncFromSettings() }
                        function onVideo_bitrate_kbpsChanged() { encodingCard.syncFromSettings() }
                        function onVideo_keyframe_secsChanged() { encodingCard.syncFromSettings() }
                        function onVideo_codecChanged() { encodingCard.syncFromSettings() }
                    }

                    Component.onCompleted: encodingCard.reloadCodecs()
                }

                // Picture-in-picture.
                //
                // Only one video stream ever leaves this client — a frame names
                // its stream by sender alone — so "camera over game" cannot mean
                // two streams. The sources are merged into one frame before the
                // encoder instead, which is why this costs no extra bandwidth
                // and needs nothing from the receiving end.
                SettingsCard {
                    id: pipCard
                    title: "Picture-in-picture"
                    subtitle: "Overlay more sources on the main one — a webcam over a game, "
                        + "or a second app. They are combined into a single stream, so peers "
                        + "see one picture and the bandwidth is unchanged."

                    // Each overlay costs a capture device, a thread, and a scale
                    // per frame. Kept in step with `composite::MAX_OVERLAYS`.
                    readonly property int maxOverlays: 3
                    readonly property var corners: ["top-left", "top-right", "bottom-left", "bottom-right"]
                    readonly property var cornerLabels: ["Top left", "Top right", "Bottom left", "Bottom right"]
                    readonly property var sizes: [10, 15, 20, 25, 30, 40, 50]

                    /// Id the main source will actually open, resolving the
                    /// empty "Default camera" selection to the device it means.
                    readonly property string baseId: root.settings
                        ? (root.settings.video_input_device !== ""
                            ? root.settings.video_input_device
                            : cameraCard.firstCameraId)
                        : ""

                    ListModel { id: overlayModel }

                    /// Bumped whenever the layout changes, so bindings that call
                    /// the helper functions below re-evaluate. A binding cannot
                    /// see inside a function call, and `overlayModel.count`
                    /// alone misses an edit that only changed a row's source.
                    property int revision: 0

                    /// True while `persist` is writing, so the settings-change
                    /// handler does not rebuild the model out from under the
                    /// delegate whose click is still being handled.
                    property bool writing: false

                    /// Snap a stored width to the nearest offered one, so a
                    /// hand-edited value is shown honestly rather than silently
                    /// displaying something else.
                    function nearestSize(value) {
                        var best = 25
                        var bestGap = 1e9
                        for (var i = 0; i < pipCard.sizes.length; i++) {
                            var gap = Math.abs(pipCard.sizes[i] - value)
                            if (gap < bestGap) { bestGap = gap; best = pipCard.sizes[i] }
                        }
                        return best
                    }

                    function reload() {
                        overlayModel.clear()
                        if (!root.settings) return
                        var list = []
                        try {
                            list = JSON.parse(root.settings.video_overlays_json || "[]")
                        } catch (e) { list = [] }
                        if (!Array.isArray(list)) list = []
                        for (var i = 0; i < list.length && overlayModel.count < pipCard.maxOverlays; i++) {
                            var o = list[i] || {}
                            if (!o.id) continue
                            overlayModel.append({
                                sourceId: String(o.id),
                                corner: pipCard.corners.indexOf(o.corner) >= 0
                                    ? String(o.corner) : "bottom-right",
                                size: pipCard.nearestSize(Number(o.size) || 25)
                            })
                        }
                        pipCard.revision++
                    }

                    function persist() {
                        if (!root.settings) return
                        var out = []
                        for (var i = 0; i < overlayModel.count; i++) {
                            var r = overlayModel.get(i)
                            if (!r.sourceId) continue
                            out.push({ id: r.sourceId, corner: r.corner, size: r.size })
                        }
                        pipCard.writing = true
                        root.settings.video_overlays_json = JSON.stringify(out)
                        pipCard.writing = false
                        pipCard.revision++
                        // A running capture holds its devices open, so the new
                        // layout only takes effect on a restart.
                        cameraCard.applyCameraSettings()
                    }

                    // Same reason as the camera card's: the stored layout
                    // arrives as a property change after this page is built.
                    Connections {
                        target: root.settings
                        function onVideo_overlays_jsonChanged() {
                            if (!pipCard.writing) pipCard.reload()
                        }
                    }

                    Component.onCompleted: pipCard.reload()

                    /// Whether any stored overlay names `base` — passed in
                    /// rather than read off `pipCard` so the binding tracks it.
                    ///
                    /// Stored layouts can hold one even though `firstFreeSource`
                    /// never offers one: changing the main source afterwards
                    /// makes a previously fine overlay a duplicate of it, and
                    /// nothing rewrites the list behind the user's back.
                    function clashesWithBase(base) {
                        if (!base) return false
                        for (var i = 0; i < overlayModel.count; i++)
                            if (overlayModel.get(i).sourceId === base) return true
                        return false
                    }

                    /// First source not already spoken for.
                    ///
                    /// A device cannot be opened twice, so offering one that is
                    /// already the main source (or another overlay) would just
                    /// produce an inset that fails to open.
                    function firstFreeSource() {
                        var used = [pipCard.baseId]
                        for (var i = 0; i < overlayModel.count; i++)
                            used.push(overlayModel.get(i).sourceId)
                        for (var j = 0; j < cameraCard.cameraIds.length; j++) {
                            if (used.indexOf(cameraCard.cameraIds[j]) < 0)
                                return cameraCard.cameraIds[j]
                        }
                        return ""
                    }

                    Label {
                        visible: overlayModel.count === 0
                        Layout.fillWidth: true
                        text: "No overlays — the main source fills the frame."
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                    }

                    Repeater {
                        model: overlayModel

                        RowLayout {
                            id: overlayRow
                            required property int index
                            required property string sourceId
                            required property string corner
                            required property int size

                            Layout.fillWidth: true
                            spacing: Theme.spacingSm

                            Label {
                                text: (overlayRow.index + 1) + "."
                                color: Theme.muted
                                font.pixelSize: Theme.fontSizeCaption
                            }

                            ComboBox {
                                Layout.fillWidth: true
                                Layout.minimumWidth: 140
                                model: cameraCard.cameraNames
                                // -1 when the saved device is gone: unplugged
                                // cameras and closed windows are routine between
                                // sessions, so the row stays and says so rather
                                // than silently rebinding to some other source.
                                currentIndex: cameraCard.cameraIds.indexOf(overlayRow.sourceId)
                                // Says which row the capture will leave out, and
                                // why. Named here rather than only in the card's
                                // warning because with three rows "an overlay"
                                // is not enough to act on.
                                displayText: overlayRow.sourceId === pipCard.baseId
                                    ? "Same as main source"
                                    : (currentIndex >= 0 ? currentText : "Source unavailable")
                                onActivated: {
                                    overlayModel.setProperty(
                                        overlayRow.index, "sourceId",
                                        cameraCard.cameraIds[currentIndex] || "")
                                    pipCard.persist()
                                }
                            }

                            ComboBox {
                                Layout.preferredWidth: 130
                                model: pipCard.cornerLabels
                                currentIndex: Math.max(0, pipCard.corners.indexOf(overlayRow.corner))
                                onActivated: {
                                    overlayModel.setProperty(
                                        overlayRow.index, "corner",
                                        pipCard.corners[currentIndex] || "bottom-right")
                                    pipCard.persist()
                                }
                            }

                            ComboBox {
                                Layout.preferredWidth: 110
                                // Width only: the height follows the source's own
                                // aspect ratio, so a 4:3 webcam is not stretched
                                // into a 16:9 box.
                                model: pipCard.sizes.map(function (s) { return s + "% wide" })
                                currentIndex: Math.max(0, pipCard.sizes.indexOf(overlayRow.size))
                                onActivated: {
                                    overlayModel.setProperty(
                                        overlayRow.index, "size",
                                        pipCard.sizes[currentIndex] || 25)
                                    pipCard.persist()
                                }
                            }

                            StyledButton {
                                text: "Remove"
                                onClicked: {
                                    overlayModel.remove(overlayRow.index)
                                    pipCard.persist()
                                }
                            }
                        }
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacingMd

                        StyledButton {
                            text: "Add overlay"
                            // `revision` is what makes this re-evaluate — a
                            // binding cannot see the model reads inside the
                            // function call.
                            enabled: pipCard.revision >= 0
                                && overlayModel.count < pipCard.maxOverlays
                                && pipCard.firstFreeSource() !== ""
                            onClicked: {
                                var id = pipCard.firstFreeSource()
                                if (id === "") return
                                overlayModel.append({
                                    sourceId: id,
                                    corner: "bottom-right",
                                    size: 25
                                })
                                pipCard.persist()
                            }
                        }

                        Label {
                            Layout.fillWidth: true
                            text: pipCard.revision >= 0
                                && overlayModel.count >= pipCard.maxOverlays
                                ? "Maximum of " + pipCard.maxOverlays + " overlays."
                                : (pipCard.firstFreeSource() === ""
                                    ? "Every available source is already in the layout."
                                    : "Changing the layout restarts capture, so it may "
                                        + "flicker for a moment.")
                            color: Theme.muted
                            font.pixelSize: Theme.fontSizeCaption
                            wrapMode: Text.WordWrap
                        }
                    }

                    // The one way the list and the outgoing picture can disagree
                    // without the user being told: a device cannot be opened
                    // twice, so an overlay that is also the main source is left
                    // out of the composite. `baseId` is read in the binding so
                    // this re-evaluates when the main source changes, which is
                    // how the layout usually ends up in this state.
                    Label {
                        Layout.fillWidth: true
                        visible: pipCard.revision >= 0
                            && pipCard.clashesWithBase(pipCard.baseId)
                        text: "One overlay is the same device as the main source. A device "
                            + "cannot be captured twice, so it is left out of the picture — "
                            + "remove that row, or pick a different main source."
                        color: Theme.warn
                        font.pixelSize: Theme.fontSizeCaption
                        wrapMode: Text.WordWrap
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
                                    if (root.settings.ollama_model !== name)
                                        root.settings.ollama_model = name
                                }
                                onEditTextChanged: {
                                    if (!root.settings) return
                                    if (root.settings.ollama_model === editText) return
                                    root.settings.ollama_model = editText
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
                                if (!activeFocus && root.settings)
                                    root.settings.ollama_system_prompt = text
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
                                }
                            }
                            SettingSwitch {
                                title: "Auto-reply to room chat"
                                checked: root.settings ? root.settings.ollama_auto_respond_room : false
                                onChanged: {
                                    if (!root.settings) return
                                    root.settings.ollama_auto_respond_room = checked
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
                            // 4 = AI. Shifted from 3 when the Video section was
                            // inserted after Audio.
                            if (root.currentTab === root.tabAi)
                                ollamaCard.refreshOllamaModels()
                        }
                    }

                    Component.onCompleted: Qt.callLater(function() {
                        if (root.currentTab === root.tabAi)
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