// TalkingRing.qml - GPU-accelerated 60-second voice-activity clock.
//
// Architecture:
//   - Once per second, 60 amplitude samples are packed into 15 Qt.vector4d
//     uniforms and pushed straight to the shader.
//   - The shader renders a clock heat map: newest sample at 12 o'clock, older
//     seconds clockwise, and radial cells filled from the avatar outward.
//   - A short NumberAnimation drives barRotation so the clock advances smoothly
//     by one second without per-frame JavaScript or Canvas repainting.

import QtQuick
import QtQuick.Window
import ConquerD.Client 1.0

Item {
    id: root

    property var ringStore: null
    property bool active: false
    property bool muted: false
    property real level: 0.0
    property int sampleCount: 60
    property real baseRadiusRatio: 0.300   // kept for API compatibility
    property real amplitudeRatio: 0.205    // kept for API compatibility
    property color accentColor: Theme.online

    // When true: GPU ShaderEffect renderer. When false: Canvas fallback.
    property bool gpuRenderer: true

    // Activity-ring mode: newest sample is always fixed at 12 o'clock;
    // no rotation animation is played. Requires `level` to be bound.
    property bool noRotation: false

    property real _rotation: 0.0
    property var _localSamples: []
    property int _localWp: 0
    property int _lastRingWp: -1
    property var _sampleData: []

    readonly property real _tickMs: 1000
    readonly property real _stepDeg: 360.0 / sampleCount
    readonly property real _animMs: _tickMs * 0.92

    width: 80
    height: 80

    function _clamp(v, lo, hi) {
        return v < lo ? lo : (v > hi ? hi : v)
    }

    function _lerp(a, b, t) {
        return a + (b - a) * _clamp(t, 0.0, 1.0)
    }

    function _heatColor(level, alpha) {
        var t = _clamp(level, 0.0, 1.0)
        var cool = [48, 204, 255]
        var warm = [87, 242, 135]
        var hot = [254, 231, 92]
        var a = t < 0.55 ? cool : warm
        var b = t < 0.55 ? warm : hot
        var local = t < 0.55 ? t / 0.55 : (t - 0.55) / 0.45
        var r = Math.round(_lerp(a[0], b[0], local))
        var g = Math.round(_lerp(a[1], b[1], local))
        var bl = Math.round(_lerp(a[2], b[2], local))
        return "rgba(" + r + "," + g + "," + bl + "," + _clamp(alpha, 0.0, 1.0).toFixed(3) + ")"
    }

    function _ensureLocalSamples() {
        if (_localSamples.length === sampleCount) return
        var next = new Array(sampleCount)
        for (var i = 0; i < sampleCount; i++) next[i] = 0.0
        _localSamples = next
        _localWp = 0
    }

    function _pushLocalSample() {
        _ensureLocalSamples()
        _localSamples[_localWp] = muted ? 0.0 : _clamp(level, 0.0, 1.0)
        _localWp = (_localWp + 1) % sampleCount
    }

    function _sampleAt(age) {
        if (!ringStore || !ringStore.samples || ringStore.samples.length === 0)
            return 0.0
        var samples = ringStore.samples
        var n = samples.length
        var wp = ringStore.wp || 0
        var idx = (wp - 1 - age + n * 4) % n
        var v = samples[idx] || 0.0
        return root.muted ? 0.0 : _clamp(v, 0.0, 1.0)
    }

    function _localSampleAt(age) {
        _ensureLocalSamples()
        var n = _localSamples.length
        var idx = (_localWp - 1 - age + n * 4) % n
        var v = _localSamples[idx] || 0.0
        return root.muted ? 0.0 : _clamp(v, 0.0, 1.0)
    }

    function _updateTexture() {
        var n = sampleCount
        // In noRotation mode we always pull from the local push-front buffer
        // (level is bound by the parent) so the ring stays in sync with the
        // live audio level rather than the 1 Hz ring-store history.
        var useExternal = !root.noRotation && !!(ringStore && ringStore.samples)
        var data = new Array(n)
        for (var i = 0; i < n; i++)
            data[i] = useExternal ? _sampleAt(i) : _localSampleAt(i)
        _sampleData = data

        if (root.gpuRenderer) {
            ringShader.s0  = Qt.vector4d(data[0]  || 0, data[1]  || 0, data[2]  || 0, data[3]  || 0)
            ringShader.s1  = Qt.vector4d(data[4]  || 0, data[5]  || 0, data[6]  || 0, data[7]  || 0)
            ringShader.s2  = Qt.vector4d(data[8]  || 0, data[9]  || 0, data[10] || 0, data[11] || 0)
            ringShader.s3  = Qt.vector4d(data[12] || 0, data[13] || 0, data[14] || 0, data[15] || 0)
            ringShader.s4  = Qt.vector4d(data[16] || 0, data[17] || 0, data[18] || 0, data[19] || 0)
            ringShader.s5  = Qt.vector4d(data[20] || 0, data[21] || 0, data[22] || 0, data[23] || 0)
            ringShader.s6  = Qt.vector4d(data[24] || 0, data[25] || 0, data[26] || 0, data[27] || 0)
            ringShader.s7  = Qt.vector4d(data[28] || 0, data[29] || 0, data[30] || 0, data[31] || 0)
            ringShader.s8  = Qt.vector4d(data[32] || 0, data[33] || 0, data[34] || 0, data[35] || 0)
            ringShader.s9  = Qt.vector4d(data[36] || 0, data[37] || 0, data[38] || 0, data[39] || 0)
            ringShader.s10 = Qt.vector4d(data[40] || 0, data[41] || 0, data[42] || 0, data[43] || 0)
            ringShader.s11 = Qt.vector4d(data[44] || 0, data[45] || 0, data[46] || 0, data[47] || 0)
            ringShader.s12 = Qt.vector4d(data[48] || 0, data[49] || 0, data[50] || 0, data[51] || 0)
            ringShader.s13 = Qt.vector4d(data[52] || 0, data[53] || 0, data[54] || 0, data[55] || 0)
            ringShader.s14 = Qt.vector4d(data[56] || 0, data[57] || 0, data[58] || 0, data[59] || 0)
        } else {
            canvasFallback.requestPaint()
        }
    }

    function _externalWpDelta() {
        if (!ringStore || !ringStore.samples) return 0
        var n = ringStore.samples.length || sampleCount
        var wp = ringStore.wp || 0
        if (_lastRingWp < 0) { _lastRingWp = wp; return 0 }
        var delta = (wp - _lastRingWp + n) % n
        _lastRingWp = wp
        return delta
    }

    function _advanceRotation(steps) {
        if (steps <= 0) return
        _rotAnim.stop()
        _rotation = _rotation - _stepDeg * steps
        _rotAnim.from = _rotation
        _rotAnim.to = 0.0
        _rotAnim.start()
    }

    function refresh() {
        if (root.noRotation) {
            _pushLocalSample()   // always sample live level; ignore ringStore
            _updateTexture()
        } else {
            var steps = 1
            if (ringStore && ringStore.samples) {
                steps = _externalWpDelta()
            } else {
                _pushLocalSample()
            }
            _updateTexture()
            _advanceRotation(steps)
        }
    }

    function repaintOnly() { _updateTexture() }

    Component.onCompleted: repaintOnly()
    onRingStoreChanged: { _lastRingWp = -1; Qt.callLater(function() { root.repaintOnly() }) }
    onActiveChanged: Qt.callLater(function() { root.repaintOnly() })
    onMutedChanged: Qt.callLater(function() { root.repaintOnly() })
    onGpuRendererChanged: Qt.callLater(function() { root.repaintOnly() })

    NumberAnimation {
        id: _rotAnim
        target: root
        property: "_rotation"
        duration: root._animMs
        easing.type: Easing.Linear
    }

    Timer {
        id: _phaseDelay
        interval: Math.floor(Math.random() * root._tickMs)
        running: true
        repeat: false
        onTriggered: _renderTimer.start()
    }

    Timer {
        id: _renderTimer
        interval: root._tickMs
        running: false
        repeat: true
        onTriggered: root.refresh()
    }

    ShaderEffect {
        id: ringShader
        anchors.fill: parent
        fragmentShader: "qrc:/shaders/talkingring.frag.qsb"
        visible: root.gpuRenderer

        // Names and declaration order must match the GLSL UBO exactly.
        property real barRotation: root._rotation
        property real activeBoost: root.active ? 1.0 : 0.58
        property real sampleCountF: root.sampleCount
        property vector4d s0:  Qt.vector4d(0,0,0,0)
        property vector4d s1:  Qt.vector4d(0,0,0,0)
        property vector4d s2:  Qt.vector4d(0,0,0,0)
        property vector4d s3:  Qt.vector4d(0,0,0,0)
        property vector4d s4:  Qt.vector4d(0,0,0,0)
        property vector4d s5:  Qt.vector4d(0,0,0,0)
        property vector4d s6:  Qt.vector4d(0,0,0,0)
        property vector4d s7:  Qt.vector4d(0,0,0,0)
        property vector4d s8:  Qt.vector4d(0,0,0,0)
        property vector4d s9:  Qt.vector4d(0,0,0,0)
        property vector4d s10: Qt.vector4d(0,0,0,0)
        property vector4d s11: Qt.vector4d(0,0,0,0)
        property vector4d s12: Qt.vector4d(0,0,0,0)
        property vector4d s13: Qt.vector4d(0,0,0,0)
        property vector4d s14: Qt.vector4d(0,0,0,0)

        opacity: root.muted ? 0.38 : 1.0

        Behavior on opacity {
            NumberAnimation { duration: 140; easing.type: Easing.OutQuad }
        }
    }

    // CPU Canvas fallback — used when gpuRenderer is false (no compiled shader
    // or GPU unavailable). Mirrors the shader's 60-sector clock heat-map.
    Canvas {
        id: canvasFallback
        anchors.fill: parent
        visible: !root.gpuRenderer
        opacity: root.muted ? 0.38 : 1.0

        Behavior on opacity {
            NumberAnimation { duration: 140; easing.type: Easing.OutQuad }
        }

        onPaint: {
            var ctx = getContext("2d")
            ctx.clearRect(0, 0, width, height)
            var data = root._sampleData
            var amp  = data && data.length > 0 ? root._clamp(data[0] || 0.0, 0.0, 1.0) : 0.0

            var cx          = width  * 0.5
            var cy          = height * 0.5
            var dim         = Math.min(width, height)
            var INNER_R     = 0.235 * dim
            var OUTER_R     = 0.476 * dim
            var activeBoost = root.active ? 1.0 : 0.58
            var shaped      = Math.pow(amp, 0.72)

            // Faint background ring — always visible so the element is legible
            // even during silence.
            ctx.beginPath()
            ctx.arc(cx, cy, OUTER_R, 0, 2 * Math.PI)
            ctx.arc(cx, cy, INNER_R, 0, 2 * Math.PI, true)
            ctx.closePath()
            ctx.fillStyle = "rgba(50,56,65,0.06)"
            ctx.fill()

            // Heat-coloured fill ring, width proportional to current amplitude.
            var fillR = INNER_R + (OUTER_R - INNER_R) * shaped
            if (fillR > INNER_R + 0.5) {
                var heatA = (0.20 + shaped * 0.72) * activeBoost
                ctx.beginPath()
                ctx.arc(cx, cy, fillR, 0, 2 * Math.PI)
                ctx.arc(cx, cy, INNER_R, 0, 2 * Math.PI, true)
                ctx.closePath()
                ctx.fillStyle = root._heatColor(amp, heatA)
                ctx.fill()
            }
        }
    }
}
