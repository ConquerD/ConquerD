// Theme.qml — Conquerd Dracula/Discord-dark palette singleton with dark/light toggle.
//
// Usage: Theme.bg0, Theme.accent, etc.
// Toggle dark/light: Theme.isDark = false
// All QML files in ConquerD.Client 1.0 can reference this without extra imports.
pragma Singleton
import QtQuick

QtObject {
    // ── Dark/light toggle ────────────────────────────────────────────────────
    property bool isDark: true
    onIsDarkChanged: _applyPalette()

    // Apply the correct palette. Called once on init (via Component.onCompleted
    // equivalent — the first property evaluation sets the dark values directly
    // as initial literals, and this function handles subsequent toggles).
    function _applyPalette() {
        if (isDark) {
            bg0 = "#111214"; bg1 = "#1E1F22"; bg2 = "#2B2D31"; bg3 = "#383A40"
            text = "#DCDDDE"; muted = "#72767D"; textInv = "#FFFFFF"
            border = "#1E1F22"; divider = "#383A40"
            linkMine = "#DCE0FF"; linkPeer = "#8EA7FF"
        } else {
            bg0 = "#F2F3F5"; bg1 = "#FFFFFF"; bg2 = "#E9EAEC"; bg3 = "#D8D9DD"
            text = "#2E3338"; muted = "#6D6F78"; textInv = "#FFFFFF"
            border = "#E3E5E8"; divider = "#C8C9CC"
            linkMine = "#3C45A5"; linkPeer = "#5865F2"
        }
    }

    // ── Background layers (dark values as initial literals) ──────────────────
    property color bg0:  "#111214"
    property color bg1:  "#1E1F22"
    property color bg2:  "#2B2D31"
    property color bg3:  "#383A40"

    // ── Text ─────────────────────────────────────────────────────────────────
    property color text:    "#DCDDDE"
    property color muted:   "#72767D"
    property color textInv: "#FFFFFF"

    // ── Semantic colours (same in both modes) ─────────────────────────────────
    readonly property color accent:  "#5865F2"
    readonly property color online:  "#3BA55D"
    readonly property color danger:  "#FF2B40"
    readonly property color warn:    "#FAA61A"

    // ── Borders / dividers ────────────────────────────────────────────────────
    property color border:  "#1E1F22"
    property color divider: "#383A40"

    // ── Typography ────────────────────────────────────────────────────────────
    readonly property int fontSizeBody:    13
    readonly property int fontSizeCaption: 11
    readonly property int fontSizeTitle:   15

    // ── Geometry ──────────────────────────────────────────────────────────────
    readonly property int radiusSm: 0
    readonly property int radiusMd: 0
    readonly property int radiusLg: 0

    readonly property int spacingXs: 4
    readonly property int spacingSm: 8
    readonly property int spacingMd: 12
    readonly property int spacingLg: 16
    readonly property int spacingXl: 24

    readonly property int controlHeight: 32
    readonly property int touchTarget: 44
    readonly property int sidebarWidth: 240
    readonly property int titleBarHeight: 44
    readonly property int bannerHeight: 32

    // ── Typography extras ─────────────────────────────────────────────────────
    readonly property int fontSizeDialog: 18
    readonly property int fontSizeMicro: 9

    // Chat link colours (updated per palette in _applyPalette)
    property color linkMine: "#DCE0FF"
    property color linkPeer: "#8EA7FF"

    // ── Motion (DESIGN.md: ≤ 300 ms) ──────────────────────────────────────────
    readonly property int animFast: 160
    readonly property int animNormal: 250
    readonly property int animSlow: 300

    // ── Overlays ──────────────────────────────────────────────────────────────
    readonly property color overlayScrim: "#000000"

    // ── Colour helpers ────────────────────────────────────────────────────────
    function withAlpha(c, a) {
        return Qt.rgba(c.r, c.g, c.b, a)
    }

    function selectedFill() {
        return withAlpha(accent, 0.15)
    }

    function semanticTint(c, strength) {
        return withAlpha(c, strength !== undefined ? strength : 0.12)
    }

    function connectionModeColor(mode) {
        switch (mode) {
            case "direct": return online
            case "relay":  return warn
            case "error":  return danger
            default:       return muted
        }
    }

    function connectionModeLabel(mode) {
        switch (mode) {
            case "direct": return "Direct"
            case "relay":  return "Relay"
            case "error":  return "Error"
            default:       return "Offline"
        }
    }

    function connectionModeTint(mode) {
        return semanticTint(connectionModeColor(mode), 0.06)
    }

    function toHex(c) {
        function channel(v) {
            var n = Math.round(Math.max(0, Math.min(1, v)) * 255).toString(16)
            return n.length === 1 ? "0" + n : n
        }
        return "#" + channel(c.r) + channel(c.g) + channel(c.b)
    }

    function qualityTierColor(tier) {
        switch (tier) {
            case "Excellent": return online
            case "Good":      return online
            case "Fair":      return warn
            default:          return danger
        }
    }

    function rttSparklineColor(maxMs) {
        if (maxMs > 300) return danger
        if (maxMs > 150) return warn
        return online
    }
}
