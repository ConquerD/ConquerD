---
name: ConquerD Design System
version: 1.0.0
theme: ConquerD Dark (Dracula/Discord-inspired)
primary_palette: Dark (with full light mode support)
framework: Qt 6 + QML + PySide6
status: Active
last_updated: 2026-06-06
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

## Components

### Core Components (Document each with)
- Visual description + screenshot (add images here when possible)
- Key properties & states
- Usage example / code snippet

**TitleBar** — 44px frameless custom bar (`bg0`)
**SessionBanner** — Status strip with animated tint + accent bar
**PeerList / SidebarItem** — Navigation rail with sections and context menus
**ChatPanel** — Message list, bubbles, typing indicator, file transfers, Ollama streaming
**CallPanel** — Compact floating overlay
**Avatar** — Deterministic SVG identicons via Rust backend

*(Expand each with real code patterns as the app grows)*

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
- 2026-06-06: Initial design system document v1.0

---