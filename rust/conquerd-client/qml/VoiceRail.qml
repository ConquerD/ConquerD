// VoiceRail.qml — Right-side voice/room panel.
//
// Replaces the small floating CallPanel with a proper collapsible strip that
// Shows during both direct P2P calls and SFU room sessions.
// 
//
// Width animates between 0 (hidden) and 200 (visible) so the centre chat
// panel expands and contracts smoothly.

import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts
import ConquerD.Client 1.0

Rectangle {
    id: root
    color: Theme.bg2
    clip: true

    // ── Public API ────────────────────────────────────────────────────────

    /// The RoomModel (or a synthetic 2-entry ListModel for direct calls).
    property var participantModel: null

    /// Display name shown in the header (room name or remote peer handle).
    property string contextName: ""

    /// Hosting supernode identity when `inRoom` (Ed25519 pub / peer id).
    property string supernodeId: ""

    /// Trusted-peer handle for the hosting supernode, when known.
    property string supernodeHandle: ""

    // Header avatar is room-only (supernode host). Direct P2P peers already
    // appear in the participant flow below — same voice rail, no duplicate tile.
    readonly property string headerAvatarPeerId: root.inRoom ? root.supernodeId : ""
    readonly property string headerAvatarHandle: root.inRoom ? root.supernodeHandle : ""
    readonly property bool showNameBubbles: root.inRoom
        || root.callState === "connecting"
        || root.callState === "in_call"

    /// Call/room state passed from bridge: "idle" | "connecting" | "in_call"
    property string callState: "idle"

    /// True when in an SFU room session (controls Leave vs End label).
    property bool inRoom: false

    /// Whether the local mic is muted.
    property bool muted: false

    /// Whether we are currently sharing video.
    property bool videoOn: false
    /// True while audio is actually going out alongside the video.
    ///
    /// Reflects what the backend achieved, not what was asked for: choosing an
    /// audio option is not a guarantee that a loopback endpoint opened. The
    /// badge on the share button is the only place a user can see the
    /// difference between "sharing silently on purpose" and "audio failed".
    property bool shareAudioOn: false

    /// Capture sources the share menu offers, as `[{ id, name }]`, with the
    /// empty-id "Default camera" entry first.
    ///
    /// Supplied by the host rather than read from the backend here: `AppBridge`
    /// declares its id in MainWindow, and a QML id is scoped to the file that
    /// declares it, so it is simply not in scope in this one.
    property var videoSources: []

    /// Main capture source id — `video_input_device`. Empty means the default
    /// camera.
    property string videoSourceId: ""

    /// Picture-in-picture layout, verbatim `video_overlays_json`.
    property string videoOverlaysJson: "[]"

    /// Audio mode the share menu pre-selects: "auto" | "system" | "off".
    property string contentAudioMode: "auto"

    /// Elapsed call seconds (driven by bridge.call_duration_secs).
    property int durationSecs: 0

    /// Connection mode for the header pill.
    property string connectionMode: "offline"

    // ── Ring history store ─────────────────────────────────────────────────────────────
    // Keyed by peerId → { samples, wp, ema, peak, ceil }.
    //   peak — short-window envelope (rises instantly, decays ~3 dB/100 ms).
    //   ceil — rolling ceiling per peer (rises instantly to new peaks,
    //          decays toward _ringMinCeil over ~1.5 s).  Samples are
    //          divided by `ceil` so mics with very different gain
    //          produce comparable ring heights, while still recovering
    //          quickly from one-off spikes so subsequent quieter speech
    //          still registers.
    property var _ringStore: ({})
    // Minimum ceiling: prevents division blow-up during long silences.
    // Kept low (0.10) so even very quiet speech normalises to near full
    // scale instead of being suppressed by an artificially high floor.
    readonly property real _ringMinCeil: 0.10
    // Perceptual compression exponent.  The level from Rust is already
    // on a dB scale (linear-in-dB ≈ log in amplitude), which partially
    // compensates for loudness perception.  A mild ^0.75 nudges quiet
    // voices up further without over-compressing the loud end.
    readonly property real _ringPerceptualExp: 0.75

    function ringStateForPeer(pid) {
        if (!_ringStore[pid]) {
            var arr = new Array(60)
            for (var i = 0; i < 60; i++) arr[i] = 0.0
            _ringStore[pid] = { samples: arr, wp: 0, ema: 0.0, peak: 0.0, ceil: _ringMinCeil }
        }
        return _ringStore[pid]
    }

    // Fast envelope tracker: polls audioLevel ~30× per second.
    //   * `peak` is the short envelope used by ringTimer (1 Hz aliasing fix).
    //   * `ceil` is the rolling ceiling used for adaptive normalization.
    Timer {
        id: peakTimer
        interval: 33
        running:  true
        repeat:   true
        onTriggered: {
            for (var i = 0; i < participantsRepeater.count; i++) {
                var item = participantsRepeater.itemAt(i)
                if (!item) continue
                var pid = item.peerId
                var st  = _ringStore[pid]
                if (!st) continue
                var raw = item.isMuted ? 0.0 : item.audioLevel
                // Mild perceptual compression to nudge quiet voices up.
                var lvl = raw > 0.0 ? Math.pow(raw, root._ringPerceptualExp) : 0.0

                // Short envelope — fast attack, ~3 dB / 100 ms release.
                var decayed = st.peak * 0.92
                st.peak = lvl > decayed ? lvl : decayed

                // Rolling ceiling — instant rise, ~1.5 s half-life decay
                // (0.985^30 ≈ 0.64 per second @ 30 Hz) so a one-off spike
                // doesn't suppress the next few seconds of normal speech.
                var ceilDecayed = st.ceil * 0.985
                if (ceilDecayed < root._ringMinCeil) ceilDecayed = root._ringMinCeil
                st.ceil = lvl > ceilDecayed ? lvl : ceilDecayed
            }
        }
    }

    // Single persistent timer — iterates every live Repeater delegate, reads
    // the per-peer peak envelope (updated by peakTimer above), normalizes
    // it against the rolling ceiling, then pushes the sample.
    // ParticipantWidget reads live audio levels directly for its lightweight
    // visual ring; this history remains available for richer renderers.
    Timer {
        id: ringTimer
        interval: 1000
        running:  true      // VoiceRail is only mounted when voice_active
        repeat:   true
        onTriggered: {
            for (var i = 0; i < participantsRepeater.count; i++) {
                var item = participantsRepeater.itemAt(i)
                if (!item) continue
                var pid = item.peerId
                var st  = _ringStore[pid]
                if (!st) continue
                var raw  = item.isMuted ? 0.0 : st.peak
                var norm = raw / st.ceil
                if (norm > 1.0) norm = 1.0
                if (norm < 0.0) norm = 0.0
                st.samples[st.wp] = norm
                st.wp  = (st.wp + 1) % 60
                st.ema = st.ema * 0.7 + norm * 0.3
                // Reset short envelope so the next one-second window starts fresh.
                st.peak = raw * 0.25
            }
        }
    }

    signal endCallRequested()
    signal muteToggled(bool muted)
    /// Camera button pressed; `on` is the requested new state.
    /// Start sharing, with `audioMode` one of "auto", "system", "off".
    ///
    /// Video and its audio start together: the audio is stamped against the
    /// video session's clock, so it cannot meaningfully exist without it.
    signal shareRequested(string audioMode)
    /// Stop sharing video and any audio that went with it.
    signal stopShareRequested()

    /// The share menu is opening — re-enumerate capture sources. Windows and
    /// cameras come and go between one share and the next, so the list is a
    /// snapshot rather than something held across sessions.
    signal shareOptionsOpened()
    /// Main capture source picked in the share menu.
    signal videoSourceSelected(string sourceId)
    /// Overlay layout edited in the share menu; carries the replacement
    /// `video_overlays_json`.
    signal videoOverlaysEdited(string overlaysJson)

    /// A peer's video should be shown in the centre expand region.
    signal expandVideoRequested(string peerId)
    /// A peer's video should be shown in its own detached window.
    signal popoutVideoRequested(string peerId)

    /// Peers currently expanded in the centre region, so the menu can offer
    /// "Collapse" instead of "Expand" for those already showing.
    property var expandedPeers: []

    function isExpanded(pid) {
        return root.expandedPeers.indexOf(pid) !== -1
    }

    // ── Helpers ───────────────────────────────────────────────────────────
    function pad(n) { return n < 10 ? "0" + n : n.toString() }

    // ── Share menu model ──────────────────────────────────────────────────
    //
    // Settings › Video owns the full editor, warnings and preview included.
    // What is repeated here is only what can still be decided in the moment
    // before sharing starts — which source, which insets, which audio — because
    // that is the one moment the answer is actually in question, and sending a
    // whole screen when a webcam was meant is not something a user can take
    // back afterwards.

    readonly property var shareCorners: ["top-left", "top-right", "bottom-left", "bottom-right"]
    readonly property var shareCornerLabels: [
        qsTr("Top left"), qsTr("Top right"), qsTr("Bottom left"), qsTr("Bottom right")
    ]
    readonly property var shareOverlaySizes: [10, 15, 20, 25, 30, 40, 50]
    /// Kept in step with `composite::MAX_OVERLAYS`.
    readonly property int shareMaxOverlays: 3

    /// Sources offerable as an inset: every real device, minus the "Default
    /// camera" entry, which names no device of its own.
    readonly property var shareOverlaySources:
        root.videoSources.filter(function (s) { return s && s.id !== "" })

    readonly property var shareOverlays: root.parseShareOverlays(root.videoOverlaysJson)

    /// Tolerant by design: the blob is user-editable, and a layout that failed
    /// to parse must mean "no overlays" rather than a share menu that cannot
    /// open.
    function parseShareOverlays(json) {
        var list
        try { list = JSON.parse(json || "[]") } catch (e) { return [] }
        return Array.isArray(list) ? list.slice(0, root.shareMaxOverlays) : []
    }

    function shareSourceIndex(id) {
        for (var i = 0; i < root.videoSources.length; i++) {
            if (root.videoSources[i].id === id)
                return i
        }
        return -1
    }

    function shareOverlaySourceIndex(id) {
        for (var i = 0; i < root.shareOverlaySources.length; i++) {
            if (root.shareOverlaySources[i].id === id)
                return i
        }
        return -1
    }

    /// First source not already spoken for. A device cannot be opened twice, so
    /// offering one that is already the main source (or another inset) would
    /// only add a row that fails to capture.
    function firstFreeOverlaySource() {
        var used = [root.videoSourceId]
        var list = root.shareOverlays
        for (var i = 0; i < list.length; i++)
            used.push(list[i].id)
        for (var j = 0; j < root.shareOverlaySources.length; j++) {
            var id = root.shareOverlaySources[j].id
            if (used.indexOf(id) < 0)
                return id
        }
        return ""
    }

    function editShareOverlay(index, key, value) {
        var list = root.parseShareOverlays(root.videoOverlaysJson)
        if (index < 0 || index >= list.length)
            return
        list[index][key] = value
        root.videoOverlaysEdited(JSON.stringify(list))
    }

    function removeShareOverlay(index) {
        var list = root.parseShareOverlays(root.videoOverlaysJson)
        if (index < 0 || index >= list.length)
            return
        list.splice(index, 1)
        root.videoOverlaysEdited(JSON.stringify(list))
    }

    function addShareOverlay() {
        var id = root.firstFreeOverlaySource()
        var list = root.parseShareOverlays(root.videoOverlaysJson)
        if (id === "" || list.length >= root.shareMaxOverlays)
            return
        list.push({ id: id, corner: "bottom-right", size: 25 })
        root.videoOverlaysEdited(JSON.stringify(list))
    }

    /// What "audio from the shared source" actually resolves to for the source
    /// currently selected — the one part of that option a user cannot infer.
    function shareAudioHint() {
        if (root.videoSourceId.indexOf("window:") === 0)
            return qsTr("Only that application's audio is sent.")
        if (root.videoSourceId.indexOf("monitor:") === 0)
            return qsTr("Everything this computer plays is sent.")
        return qsTr("A camera carries no audio of its own — your microphone already carries you.")
    }

    // ── Left border separator ─────────────────────────────────────────────
    Rectangle {
        anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
        width: 1
        color: Theme.divider
    }

    // ── Shared peer context menu ──────────────────────────────────────────
    //
    // One instance for the whole rail, retargeted before each popup(). Putting
    // a Menu inside the Repeater delegate would create one per participant.
    Menu {
        id: peerMenu

        property string targetPeerId: ""
        property string targetName: ""
        property bool targetLocalMuted: false
        property int targetVolume: 100
        property bool targetVideoActive: false
        property bool targetIsSelf: false

        function openFor(pid, name, muted, volume, videoActive, isSelf) {
            peerMenu.targetPeerId = pid
            peerMenu.targetName = name
            peerMenu.targetLocalMuted = muted
            peerMenu.targetVolume = volume
            peerMenu.targetVideoActive = videoActive
            peerMenu.targetIsSelf = isSelf
            peerMenu.popup()
        }

        MenuItem {
            // Muting yourself locally would be meaningless — you don't hear
            // your own playback — so the entry is disabled rather than absent,
            // keeping the menu's shape stable between peers.
            enabled: !peerMenu.targetIsSelf
            checkable: true
            checked: peerMenu.targetLocalMuted
            text: qsTr("Mute for me")
            onTriggered: {
                var next = !peerMenu.targetLocalMuted
                peerMenu.targetLocalMuted = next
                backend.setPeerAudioPref(peerMenu.targetPeerId, next, peerMenu.targetVolume)
                if (root.participantModel && root.participantModel.setLocalAudio)
                    root.participantModel.setLocalAudio(peerMenu.targetPeerId, next, peerMenu.targetVolume)
            }
        }

        MenuItem {
            enabled: !peerMenu.targetIsSelf
            text: qsTr("Volume…")
            onTriggered: volumePopup.openFor(
                peerMenu.targetPeerId, peerMenu.targetName, peerMenu.targetVolume)
        }

        MenuSeparator {}

        MenuItem {
            // Collapsing stays available after the camera goes off. Gating both
            // directions on `targetVideoActive` stranded the tile: the peer
            // stops sharing, the entry greys out, and the only control that
            // removes the tile is gone. Expanding still requires a live camera.
            enabled: peerMenu.targetVideoActive || root.isExpanded(peerMenu.targetPeerId)
            text: root.isExpanded(peerMenu.targetPeerId)
                ? qsTr("Collapse video")
                : qsTr("Expand video")
            onTriggered: root.expandVideoRequested(peerMenu.targetPeerId)
        }

        MenuItem {
            enabled: peerMenu.targetVideoActive
            text: qsTr("Pop out video")
            onTriggered: root.popoutVideoRequested(peerMenu.targetPeerId)
        }

        MenuSeparator {}

        MenuItem {
            text: qsTr("Copy Peer ID")
            onTriggered: backend.copyToClipboard(peerMenu.targetPeerId)
        }
    }

    PeerVolumePopup {
        id: volumePopup
        x: Math.round((root.width - width) / 2)
        y: Math.round((root.height - height) / 2)
        onVolumeChanged: function(pid, pct) {
            // Unmute implicitly when the listener raises the volume — leaving
            // someone muted while their slider reads 80% would be baffling.
            var muted = pct === 0
            backend.setPeerAudioPref(pid, muted, pct)
            if (root.participantModel && root.participantModel.setLocalAudio)
                root.participantModel.setLocalAudio(pid, muted, pct)
            if (peerMenu.targetPeerId === pid) {
                peerMenu.targetVolume = pct
                peerMenu.targetLocalMuted = muted
            }
        }
    }

    ColumnLayout {
        anchors { fill: parent; leftMargin: 1 }
        spacing: 0

        // ── Header ────────────────────────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            height: 52
            color: Theme.bg3

            RowLayout {
                anchors {
                    fill: parent
                    leftMargin: Theme.spacingMd
                    rightMargin: Theme.spacingSm
                    topMargin: Theme.spacingXs
                    bottomMargin: Theme.spacingXs
                }
                spacing: Theme.spacingSm

                Item {
                    visible: root.headerAvatarPeerId !== ""
                    Layout.preferredWidth: 36
                    Layout.preferredHeight: 36
                    Layout.alignment: Qt.AlignVCenter

                    Avatar {
                        anchors.centerIn: parent
                        peerId: root.headerAvatarPeerId
                        size: 28
                        showRing: true
                    }

                    Rectangle {
                        visible: root.headerAvatarHandle !== ""
                        anchors {
                            horizontalCenter: parent.horizontalCenter
                            bottom: parent.bottom
                        }
                        implicitWidth: Math.min(handleLabel.implicitWidth + 6, 52)
                        width: implicitWidth
                        height: 12
                        radius: height / 2
                        color: Theme.accent

                        Text {
                            id: handleLabel
                            anchors.centerIn: parent
                            width: parent.width - 4
                            text: root.headerAvatarHandle
                            color: Theme.textInv
                            font.pixelSize: Theme.fontSizeMicro
                            font.bold: true
                            elide: Text.ElideRight
                            horizontalAlignment: Text.AlignHCenter
                        }
                    }
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2

                    // Room/peer name
                    Text {
                        text: root.contextName || (root.inRoom ? "Voice Room" : "Call")
                        color: Theme.text
                        font.pixelSize: Theme.fontSizeBody
                        font.bold: true
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }

                    // Connection mode pill
                    Rectangle {
                        width: modePillText.implicitWidth + Theme.spacingSm
                        height: Theme.fontSizeCaption + Theme.spacingXs
                        radius: Theme.radiusSm
                        color: Theme.semanticTint(
                            Theme.connectionModeColor(root.connectionMode),
                            0.18
                        )

                        Text {
                            id: modePillText
                            anchors.centerIn: parent
                            text: root.callState === "connecting"
                                ? "Connecting..."
                                : Theme.connectionModeLabel(root.connectionMode)
                            color: Theme.connectionModeColor(root.connectionMode)
                            font.pixelSize: Theme.fontSizeCaption
                            font.bold: true
                        }
                    }
                }
            }
        }

        // ── Participant tiles ─────────────────────────────────────────────
        Flow {
            id: participantFlow
            Layout.fillWidth: true
            Layout.fillHeight: true
            padding: Theme.spacingSm
            spacing: Theme.spacingSm

            // Connecting spinner when no participants yet
            Item {
                visible: (root.participantModel === null ||
                          (root.participantModel && root.participantModel.rowCount &&
                           root.participantModel.rowCount() === 0)) &&
                         root.callState === "connecting"
                width: participantFlow.width - Theme.spacingSm * 2
                height: 80

                Column {
                    anchors.centerIn: parent
                    spacing: 8

                    BusyIndicator {
                        anchors.horizontalCenter: parent.horizontalCenter
                        running: true
                        width: 32; height: 32
                    }

                    Text {
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: "Connecting..."
                        color: Theme.muted
                        font.pixelSize: Theme.fontSizeCaption
                    }
                }
            }

            Repeater {
                id: participantsRepeater
                model: root.participantModel

                ParticipantWidget {
                    peerId:      model.peerId
                    displayName: model.handle || model.peerId || ""
                    isMuted:     model.muted
                    audioLevel:  model.isSelf ? backend.mic_level : model.audioLevel
                    isSelf:      model.isSelf
                    ringStore:   root.ringStateForPeer(model.peerId)
                    showNameBubbles: root.showNameBubbles
                    videoActive: model.videoActive === true
                    locallyMuted: model.localMuted === true

                    onContextMenuRequested: peerMenu.openFor(
                        model.peerId,
                        model.handle || model.peerId || "",
                        model.localMuted === true,
                        model.localVolume === undefined ? 100 : model.localVolume,
                        model.videoActive === true,
                        model.isSelf === true)
                    onExpandVideoRequested: root.expandVideoRequested(model.peerId)
                }
            }
        }

        // ── Duration counter ──────────────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            height: 24
            color: Theme.bg3
            visible: root.callState === "in_call" || root.inRoom

            Text {
                anchors.centerIn: parent
                text: {
                    var s = root.durationSecs
                    var h = Math.floor(s / 3600)
                    var m = Math.floor((s % 3600) / 60)
                    var sec = s % 60
                    if (h > 0) {
                        return root.pad(h) + ":" + root.pad(m) + ":" + root.pad(sec)
                    }
                    return root.pad(m) + ":" + root.pad(sec)
                }
                color: Theme.muted
                font.pixelSize: Theme.fontSizeCaption
                font.family: "monospace"
            }

            function pad(n) { return n < 10 ? "0" + n : n.toString() }
        }

        // ── Controls bar ─────────────────────────────────────────────────
        Rectangle {
            Layout.fillWidth: true
            height: 52
            color: Theme.bg3

            // Top divider
            Rectangle {
                anchors { top: parent.top; left: parent.left; right: parent.right }
                height: 1
                color: Theme.divider
            }

            RowLayout {
                anchors {
                    fill: parent
                    leftMargin: Theme.spacingMd
                    rightMargin: Theme.spacingMd
                }
                spacing: Theme.spacingSm

                // Mute toggle
                Rectangle {
                    width: 36; height: 36; radius: Theme.radiusPill
                    color: root.muted ? Theme.danger : Theme.bg2

                    Behavior on color { ColorAnimation { duration: Theme.animFast } }

                    Image {
                        anchors.centerIn: parent
                        source: root.muted ? "qrc:/qt/qml/ConquerD/Client/icons/mic-off.svg" : "qrc:/qt/qml/ConquerD/Client/icons/mic.svg"
                        sourceSize.width: 18
                        sourceSize.height: 18
                        width: 18
                        height: 18
                        fillMode: Image.PreserveAspectFit
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            root.muted = !root.muted
                            root.muteToggled(root.muted)
                        }
                    }

                    ToolTip.text: root.muted ? "Unmute microphone" : "Mute microphone"
                    ToolTip.visible: muteHover.hovered
                    HoverHandler { id: muteHover }
                }

                // One control for sharing, not two.
                //
                // Video and the audio that belongs with it are a single act
                // from the user's point of view — "share this" — so the audio
                // is an *option of sharing*, chosen when sharing starts, rather
                // than a second toggle to remember afterwards. It also removes
                // the state that made no sense: an audio button that did
                // nothing until a camera was already running.
                Rectangle {
                    id: shareButton
                    width: 36; height: 36; radius: Theme.radiusPill
                    color: root.videoOn ? Theme.accent : Theme.bg2

                    Behavior on color { ColorAnimation { duration: Theme.animFast } }

                    Image {
                        anchors.centerIn: parent
                        source: root.videoOn
                            ? "qrc:/qt/qml/ConquerD/Client/icons/video.svg"
                            : "qrc:/qt/qml/ConquerD/Client/icons/video-off.svg"
                        sourceSize.width: 18
                        sourceSize.height: 18
                        width: 18
                        height: 18
                        fillMode: Image.PreserveAspectFit
                    }

                    // Audio-included badge.
                    //
                    // Sharing is otherwise indistinguishable whether or not the
                    // audio half started, and it can fail on its own (no
                    // loopback endpoint, device in use) while the video keeps
                    // running. Without this the user's first clue is a peer
                    // saying they hear nothing.
                    Rectangle {
                        visible: root.videoOn && root.shareAudioOn
                        width: 12; height: 12; radius: 6
                        anchors { right: parent.right; bottom: parent.bottom }
                        color: Theme.bg2
                        border.color: Theme.accent
                        border.width: 1

                        Image {
                            anchors.centerIn: parent
                            source: "qrc:/qt/qml/ConquerD/Client/icons/headphone.svg"
                            sourceSize.width: 8; sourceSize.height: 8
                            width: 8; height: 8
                            fillMode: Image.PreserveAspectFit
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        // Stopping is unambiguous, so it needs no menu. Starting
                        // asks what to do about audio, because that choice is
                        // only meaningful at the moment sharing begins.
                        onClicked: {
                            if (root.videoOn)
                                root.stopShareRequested()
                            else
                                sharePopup.open()
                        }
                    }

                    ToolTip.text: !root.videoOn
                        ? qsTr("Share video")
                        : (root.shareAudioOn
                            ? qsTr("Stop sharing (video and audio)")
                            : qsTr("Stop sharing (video only)"))
                    ToolTip.visible: shareHover.hovered && !sharePopup.opened
                    HoverHandler { id: shareHover }

                    Popup {
                        id: sharePopup
                        // Right-aligned to the button. The menu is far wider
                        // than the rail it hangs off, so growing rightward
                        // would put most of it past the window edge.
                        x: shareButton.width - width
                        y: -implicitHeight - Theme.spacingXs
                        width: 320
                        padding: Theme.spacingSm
                        modal: false
                        // A snapshot, taken each time: windows open and close
                        // constantly, and a list built once at startup would
                        // offer things that are no longer there.
                        onAboutToShow: root.shareOptionsOpened()
                        background: Rectangle {
                            color: Theme.bg2
                            border.color: Theme.border
                            radius: Theme.radiusSm
                        }

                        ColumnLayout {
                            anchors.fill: parent
                            spacing: Theme.spacingXs

                            Label {
                                text: qsTr("Share video with…")
                                color: Theme.text
                                font.pixelSize: Theme.fontSizeCaption
                                font.bold: true
                            }

                            // ── What is being shared ──────────────────────
                            Label {
                                text: qsTr("Source")
                                color: Theme.muted
                                font.pixelSize: Theme.fontSizeMicro
                            }

                            ComboBox {
                                id: shareSourceCombo
                                Layout.fillWidth: true
                                Layout.preferredHeight: 32
                                font.pixelSize: Theme.fontSizeCaption
                                model: root.videoSources.map(function (s) { return s.name || s.id })
                                currentIndex: root.shareSourceIndex(root.videoSourceId)
                                // -1 when the saved device is gone — an
                                // unplugged camera or a closed window is
                                // routine, and saying so beats silently
                                // sharing whatever happens to be first.
                                displayText: currentIndex >= 0
                                    ? currentText
                                    : qsTr("Source unavailable")
                                onActivated: {
                                    var s = root.videoSources[currentIndex]
                                    root.videoSourceSelected(s ? (s.id || "") : "")
                                    // Selecting broke the binding above by
                                    // writing currentIndex directly, and this
                                    // combo outlives the menu — without
                                    // rebinding, a source changed from Settings
                                    // afterwards would never show here.
                                    currentIndex = Qt.binding(function () {
                                        return root.shareSourceIndex(root.videoSourceId)
                                    })
                                }
                            }

                            // ── Overlays ──────────────────────────────────
                            RowLayout {
                                Layout.fillWidth: true
                                spacing: Theme.spacingXs

                                Label {
                                    Layout.fillWidth: true
                                    text: qsTr("Overlays")
                                    color: Theme.muted
                                    font.pixelSize: Theme.fontSizeMicro
                                }
                                StyledButton {
                                    text: qsTr("Add")
                                    enabled: root.shareOverlays.length < root.shareMaxOverlays
                                        && root.firstFreeOverlaySource() !== ""
                                    onClicked: root.addShareOverlay()
                                }
                            }

                            Label {
                                visible: root.shareOverlays.length === 0
                                Layout.fillWidth: true
                                wrapMode: Text.WordWrap
                                text: qsTr("None — the source fills the frame. Overlays are merged "
                                    + "into it before encoding, so peers still see one picture.")
                                color: Theme.muted
                                font.pixelSize: Theme.fontSizeMicro
                            }

                            Repeater {
                                model: root.shareOverlays

                                delegate: ColumnLayout {
                                    id: overlayRow
                                    required property int index
                                    required property var modelData

                                    Layout.fillWidth: true
                                    spacing: 2

                                    ComboBox {
                                        Layout.fillWidth: true
                                        Layout.preferredHeight: 30
                                        font.pixelSize: Theme.fontSizeCaption
                                        model: root.shareOverlaySources.map(
                                            function (s) { return s.name || s.id })
                                        currentIndex: root.shareOverlaySourceIndex(
                                            overlayRow.modelData.id)
                                        // Names the row that will be left out
                                        // of the picture, and why: a device
                                        // cannot be captured twice, so an
                                        // overlay that is also the main source
                                        // is dropped by the compositor.
                                        displayText: overlayRow.modelData.id === root.videoSourceId
                                            ? qsTr("Same as source — not shown")
                                            : (currentIndex >= 0
                                                ? currentText
                                                : qsTr("Source unavailable"))
                                        onActivated: {
                                            var s = root.shareOverlaySources[currentIndex]
                                            root.editShareOverlay(
                                                overlayRow.index, "id", s ? s.id : "")
                                        }
                                    }

                                    RowLayout {
                                        Layout.fillWidth: true
                                        spacing: Theme.spacingXs

                                        ComboBox {
                                            Layout.fillWidth: true
                                            Layout.preferredHeight: 30
                                            font.pixelSize: Theme.fontSizeCaption
                                            model: root.shareCornerLabels
                                            currentIndex: Math.max(0, root.shareCorners.indexOf(
                                                overlayRow.modelData.corner))
                                            onActivated: root.editShareOverlay(
                                                overlayRow.index, "corner",
                                                root.shareCorners[currentIndex] || "bottom-right")
                                        }

                                        ComboBox {
                                            Layout.preferredWidth: 96
                                            Layout.preferredHeight: 30
                                            font.pixelSize: Theme.fontSizeCaption
                                            // Width only: the height follows
                                            // the source's own aspect ratio.
                                            model: root.shareOverlaySizes.map(
                                                function (s) { return s + "%" })
                                            currentIndex: Math.max(0, root.shareOverlaySizes.indexOf(
                                                Number(overlayRow.modelData.size) || 25))
                                            onActivated: root.editShareOverlay(
                                                overlayRow.index, "size",
                                                root.shareOverlaySizes[currentIndex] || 25)
                                        }

                                        StyledButton {
                                            text: qsTr("Remove")
                                            onClicked: root.removeShareOverlay(overlayRow.index)
                                        }
                                    }
                                }
                            }

                            // ── Audio, which also starts the share ────────
                            //
                            // Picking the audio is the commit, so everything
                            // above it is still editable when the choice is
                            // made.
                            Label {
                                Layout.topMargin: Theme.spacingXs
                                text: qsTr("Audio — pick one to start sharing")
                                color: Theme.muted
                                font.pixelSize: Theme.fontSizeMicro
                            }

                            Repeater {
                                model: [
                                    { key: "auto",   label: qsTr("Audio from the shared source") },
                                    { key: "system", label: qsTr("This computer's audio") },
                                    { key: "off",    label: qsTr("No audio") }
                                ]
                                delegate: Rectangle {
                                    id: audioOption
                                    required property var modelData
                                    Layout.fillWidth: true
                                    height: 30
                                    radius: Theme.radiusSm
                                    color: optHover.hovered ? Theme.bg3 : "transparent"

                                    Text {
                                        anchors.verticalCenter: parent.verticalCenter
                                        anchors.left: parent.left
                                        anchors.leftMargin: Theme.spacingSm
                                        // The last choice made is marked, not
                                        // pre-applied: this list is a set of
                                        // actions, so a "selected" row with no
                                        // way to confirm it would read as a
                                        // share that had already started.
                                        text: (audioOption.modelData.key === root.contentAudioMode
                                                ? "• " : "   ")
                                            + audioOption.modelData.label
                                        color: Theme.text
                                        font.pixelSize: Theme.fontSizeCaption
                                    }
                                    HoverHandler { id: optHover }
                                    MouseArea {
                                        anchors.fill: parent
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: {
                                            sharePopup.close()
                                            root.shareRequested(audioOption.modelData.key)
                                        }
                                    }
                                }
                            }

                            Label {
                                Layout.fillWidth: true
                                wrapMode: Text.WordWrap
                                text: root.shareAudioHint()
                                color: Theme.muted
                                font.pixelSize: Theme.fontSizeMicro
                            }
                        }
                    }
                }

                Item { Layout.fillWidth: true }

                // End / Leave button
                Rectangle {
                    width: 36; height: 36; radius: Theme.radiusPill
                    color: Theme.danger

                    Image {
                        anchors.centerIn: parent
                        source: root.inRoom
                            ? "qrc:/qt/qml/ConquerD/Client/icons/stop.svg"
                            : "qrc:/qt/qml/ConquerD/Client/icons/x-circle.svg"
                        width: 18; height: 18
                        smooth: true
                        antialiasing: true
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.endCallRequested()
                    }

                    ToolTip.text: root.inRoom ? "Leave room" : "End call"
                    ToolTip.visible: endHover.hovered
                    HoverHandler { id: endHover }
                }
            }
        }
    }
}
