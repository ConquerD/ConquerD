---
name: ConquerD Design System
version: 1.0.0
theme: ConquerD Dark (Dracula/Discord-inspired)
primary_palette: Dark (with full light mode support)
framework: Qt 6 + QML (Rust / CXX-Qt)
status: Active
last_updated: 2026-06-26
---

# ConquerD Design System

## Overview
ConquerD is a high-information-density, privacy-focused P2P application. The UI emphasizes clarity, speed, and minimal distraction through:

- A layered dark-first aesthetic with subtle depth (no heavy shadows).
- Discord/Dracula-inspired color language.
- Strong typography hierarchy and consistent spacing.
- Fluid micro-animations on state changes.
- Full light mode support via `Theme.isDark`.

**Source of truth**: `rust/conquerd-client/qml/Theme.qml` + this document.
All new QML must consume tokens from `Theme.*` — never hardcode colors, sizes, or radii.

**Design Principles**:
- **Density over whitespace** — maximize useful content.
- **Clarity first** — semantic colors and clear visual hierarchy.
- **Motion with purpose** — short, meaningful animations (≤ 300ms).
- **Consistency** — every component follows the token system.
- **Performance** — lightweight delegates, lazy loading, minimal JS.

## Tokens

### Colors

#### Background Stack (creates depth)
Use layers strictly in order for visual hierarchy.

**Dark Mode (default)**
| Token       | Hex       | Role                              |
|-------------|-----------|-----------------------------------|
| `bg0`       | `#111214` | Window chrome, title bar          |
| `bg1`       | `#1E1F22` | Primary panels, sidebars          |
| `bg2`       | `#2B2D31` | Cards, inputs, section headers    |
| `bg3`       | `#383A40` | Hover, dividers, subtle surfaces  |

**Light Mode**
`bg0 #F2F3F5` → `bg1 #FFFFFF` → `bg2 #E9EAEC` → `bg3 #D8D9DD`

#### Text
| Token       | Hex       | Role                                      |
|-------------|-----------|-------------------------------------------|
| `text`      | `#DCDDDE` | Primary body / labels                     |
| `muted`     | `#72767D` | Secondary, captions, placeholders         |
| `textInv`   | `#FFFFFF` | Text on accent / semantic colored bg      |

#### Semantic
| Token       | Hex       | Role                                      |
|-------------|-----------|-------------------------------------------|
| `accent`    | `#5865F2` | Focus, selection, active states           |
| `online`    | `#3BA55D` | Live connections, success                 |
| `warn`      | `#FAA61A` | Relay / non-ideal states                  |
| `danger`    | `#FF2B40` | Errors, destructive actions, **brand**    |

### Typography
Uses system font (via `Material.Dark` / `SystemDefault`). No custom font loading unless decided later.

| Token              | Size   | Weight     | Role                          |
|--------------------|--------|------------|-------------------------------|
| `fontSizeTitle`    | 15px   | Medium     | Panel titles, peer names      |
| `fontSizeBody`     | 13px   | Regular    | Messages, main content        |
| `fontSizeCaption`  | 11px   | Regular    | Timestamps, badges, labels    |

**Rules**:
- Section headers: `uppercase`, `letterSpacing: 1.2`, `muted` color.
- Use `font.pixelSize` + `Theme.*` tokens.

### Spacing & Radius
| Token         | Value   | Usage                              |
|---------------|---------|------------------------------------|
| `spacingXs`   | 4px     | Icon gaps, tight elements          |
| `spacingSm`   | 8px     | Inner padding, icon+label          |
| `spacingMd`   | 12px    | Standard item padding              |
| `spacingLg`   | 16px    | Panel margins                      |
| `spacingXl`   | 24px    | Major section gaps                 |
| `radiusSm`    | 0px     | Angular inputs, chips, pills       |
| `radiusMd`    | 0px     | Angular buttons, cards, dialogs    |
| `radiusLg`    | 0px     | Angular panels / overlays          |
| `radiusPill`  | 999px   | Fully-rounded badges, status dots, circular icon buttons |

### Motion
| Token         | Value   | Usage                                              |
|---------------|---------|----------------------------------------------------|
| `animMicro`   | 80ms    | Live audio rings, hover color flips, micro state   |
| `animFast`    | 160ms   | Mute toggle, panel open/close, opacity fades       |
| `animNormal`  | 250ms   | List item bg, selection highlight                  |
| `animSlow`    | 300ms   | Session banner tint, accent bar color              |

All `Behavior` blocks must reference a `Theme.anim*` token — never a raw integer.

## Components

### Core Components (Document each with)
- Visual description + screenshot (add images here when possible)
- Key properties & states
- Usage example / code snippet

### TitleBar
44px frameless custom bar (`bg0`). Hosts drag-to-move, double-click-to-maximize, and three window-control buttons (minimize, maximize/restore, close) via an internal `TitleBarButton` component. Button hover fills: `bg3` for minimize/maximize, `danger` for close. Default slot between logo and buttons accepts arbitrary `contentChildren` via `Layout.*`.

### SessionBanner
32px status strip below TitleBar. Left edge: 3px accent bar tinted by connection mode color. Row: 7px circular status dot + mode label (`connectionModeColor`) + optional `bannerText`. Background is `Qt.tint(bg1, connectionModeTint(mode))`. All color transitions use `animSlow`.

### PeerList / SidebarItem
**PeerList** — `bg1` panel. Section headers (Online / Offline) in `muted`, uppercase, `letterSpacing: 1.2`. Peer rows are 56px tall; selected state uses `selectedFill()` + 3px left accent bar. Avatar ring color: `danger` (blocked) → `online` (connected) → avatar tint (offline). Unread badge: `danger` fill, `radiusPill`, caption text. Right-click context menu: Start Call / Copy Peer ID / Remove Peer / Block or Unblock Peer / Clear Chat History.

**SidebarItem** — 44px `ItemDelegate`. Selected: `selectedFill()` bg + 3px `accent` left bar. Badge: `accent` fill, `radiusPill`.

### ChatPanel
Message list with bubbles, typing indicator, date separators, file-transfer chip UI (progress bar), Ollama streaming, and paginated history. Includes a stats overlay (`StatsPanel` + `ConnectionStatsChip`) anchored to the top-right of the message area.

### CallPanel
240×80px floating `bg2` rectangle with `accent` border. Contains: circular 10px state indicator (`online` / `warn` / `danger`), status text, mute toggle (mic icon colored by `online`/`danger`), end-call button (danger `x-circle` icon). Font uses `fontSizeBody`. Margins via `spacingMd`.

### Avatar
SVG identicon generated by `backend.avatarSvg(peerId, configJson)`. Default tint for unknown peers is `Theme.muted`. Optional status ring controlled by `showRing`, `ringColor`, `speaking`, and `audioLevel`:
- Static ring: `showRing: true`, fixed `ringColor`, ring width ≈ 17% of `size`.
- Speaking ring: `speaking: true`, ring width ≈ 22% of `size`, animated via `TalkingRing`.

Ring color convention in PeerList: `danger` = blocked, `online` = connected, avatar tint = offline.

---

### ConnectionStatsChip
Compact inline chip in the chat header showing live RTT, relay status, and a quality-colored indicator dot. Color thresholds: ≤150ms+<1.5% loss = `online`, ≤300ms+<4% = `warn`, else `danger`. Clicking expands `StatsPanel`. Hidden when no stats data is available (`hasData: false`).

### StatsPanel
220px overlay showing RTT, packet loss, jitter, bandwidth, and a quality tier label (Excellent / Good / Fair / Poor). Includes a 30-sample RTT sparkline canvas using `rttSparklineColor()` for the line color. Background: `overlayScrim` at 82% alpha. Dismisses on click-away.

### TalkingRing
GPU-accelerated 60-second voice-activity clock ring rendered via `ShaderEffect` (GLSL fragment shader), with a Canvas fallback. Samples are packed into 15 `Qt.vector4d` uniforms pushed to the shader at 1Hz. The ring functions as a clock heat-map: newest sample at 12 o'clock, older seconds clockwise. Heat colors map amplitude: cool teal → warm green → hot yellow. Muted state fades ring to 38% opacity (`animFast` transition). Used inside `ParticipantWidget` and as a standalone per-peer component in `VoiceRail`.

### VoiceRail
Collapsible right-side panel (width animates 0↔200px via `animFast + InOutQuad`). Shown only when `backend.voice_active`. Structure:
- **Header** (52px, `bg3`): Avatar + handle label + room/peer name + connection mode pill.
- **Participant flow**: `Flow` of `ParticipantWidget` tiles. Shows connecting spinner when no participants yet.
- **Duration counter** (24px, `bg3`): `mm:ss` or `h:mm:ss` monospaced timer.
- **Controls bar** (52px, `bg3`): Mute toggle (36px circular, `bg2`/`danger` fill, `animFast` color) + End/Leave button (36px circular, `danger`). Both buttons use `radiusPill`.

Adaptive audio normalization uses a 30Hz peak envelope tracker (`peakTimer`) and a per-peer rolling ceiling to normalize very different mic gains.

### ParticipantWidget
80×80px tile. Contains a 48px `Avatar` with `showRing: true` at center, overlaid by a property-bound activity ring (two concentric `Rectangle` borders). Activity ring color uses a heat map from `Qt.rgba` (same cool→warm→hot scale as TalkingRing). Ring width and opacity animate at `animMicro` for live audio responsiveness. Mute badge: 18px circular `danger` with mic-off icon at bottom-right. Optional name bubble: `accent` pill at bottom-center, visible when `showNameBubbles: true`.

### FilePreviewPanel
Inline file preview using `ConquerdWebView`. Supports: images, PDF, HTML, text/code, video (HTML5 `<video>`), audio (HTML5 `<audio>`). Navigation restricted to `file://` and `data:` URIs — no outbound network. Shows "cannot preview" message for unsupported types.

### ConquerdWebView
Shared secure `QtWebEngine` wrapper. Always off-the-record (no persistent cookies, cache, localStorage, or history). Navigation whitelist: only hosts matching `allowedDomains` suffixes are allowed; `file://` and `data:` are always permitted. `allowAll: true` bypasses the whitelist (browser panel). `allowConquerd: true` permits `conquerd://` URLs for supernode portal pages. No `QWebChannel` bridge — zero access to Rust/AppBridge peer data.

## States & Interactions

- **Hover**: `bg3` fill or slight brightness shift.
- **Selected / Active**: `accent` 15% fill + 3px left accent bar with angular ends.
- **Focus**: `accent` border.
- **Disabled**: 50% opacity + `muted` text.
- **Transitions**: Use `ColorAnimation { duration: 250 }` or `NumberAnimation` for all state changes.
- **Right-click**: Consistent context menus with Qt `Menu`.

## Icons & Assets
- Prefer **Material Icons** (via `MaterialIcon` or SVG) for consistency.
- Custom SVGs only for brand (logo, avatars).
- All icons: 20–24px, use `Theme.text` or semantic colors.
- Avatars: Generated server-side via `backend.avatarSvg(...)`.

## Theming & Modes
- Toggle: `Theme.isDark` (reactive property).
- All colors defined as properties in `Theme.qml`.
- Light mode is fully supported but secondary — test both regularly.

## Do's and Don'ts (Rules)

**Do**:
- Always use `Theme.*` tokens.
- Follow the background layer order (`bg0` → `bg1` → `bg2` → `bg3`).
- Keep animations short and purposeful.
- Use Layouts + anchors for responsiveness.
- Test on multiple DPIs / window sizes.

**Don't**:
- Hardcode hex values, pixel sizes, or colors.
- Use `danger` for non-destructive elements (it's the brand color).
- Overuse `accent` — reserve for focus/selection.
- Put heavy logic inside UI components (keep in Python/Rust models).

## Accessibility
- Minimum contrast ratios (WCAG AA).
- Keyboard navigation support.
- Screen reader friendly labels (`Accessible` role).
- Scalable fonts and touch targets (min 44px).

## Screenshots / Visual Inventory
*(Add gallery here as components mature)*

## Changelog
- 2026-06-26: Add `radiusPill` + `animMicro` tokens; sweep all hardcoded animation durations and badge radii to token references; document TitleBar buttons, Avatar ring behavior, and 7 new components (ConnectionStatsChip, StatsPanel, TalkingRing, VoiceRail, ParticipantWidget, FilePreviewPanel, ConquerdWebView)
- 2026-06-06: Initial design system document v1.0

---