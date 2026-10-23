# MacRDP UI Redesign Specification

## Overview

Completely redesign the visual appearance of macrdp-ui (the Tauri desktop application) in the style of **Xcode/Instruments professional tools**, improving information density, visual polish and usability.

**Current state**: The existing UI has a Tailwind w-52 (208px) sidebar with icons and text labels and a basic light-gray macOS appearance. This redesign reorganizes the sidebar with section headings and updates the color system and all page layouts.

**Scope**: All six main-window pages (Dashboard, Settings, Permissions, Logs, Statistics, About). The Popover window (the tray-icon menu, 300×280px) is outside this scope and retains its existing styling.

## Design Direction

- **Style**: Xcode/Instruments professional dark-tool appearance
- **Navigation**: 180px sidebar with section headings, similar to the Xcode Navigator
- **Theme**: Preserve the existing three-way selection (Light / Dark / System), with a quick toggle at the bottom of the sidebar and another selector in Settings → General
- **Dashboard**: Give charts and the connection list equal prominence

## Design System

### Colors

**Dark mode**:
| Purpose | Value |
|---------|-------|
| Background | `#1a1a1a` |
| Sidebar | `#202020` |
| Cards/panels | `#252525` |
| Borders | `#2d2d2d` / `#3a3a3a` |
| Heading text | `#ffffff` |
| Body text | `#cccccc` |
| Secondary text | `#8e8e93` (increase contrast to meet WCAG AA 4.5:1) |
| Blue accent | `#0a84ff` |
| Green (success/running) | `#30d158` |
| Orange (warning/moderate) | `#ff9f0a` |
| Red (error/stopped) | `#ff453a` |
| Purple (secondary accent) | `#bf5af2` |

**Light mode**:
| Purpose | Value |
|---------|-------|
| Background | `#f5f5f7` |
| Sidebar | `#ebebed` |
| Cards/panels | `#ffffff` with border `#e8e8e8` |
| Heading text | `#1d1d1f` |
| Body text | `#333333` |
| Secondary text | `#86868b` |
| Blue accent | `#007aff` |
| Green | `#34c759` |
| Orange | `#ff9500` |
| Red | `#ff3b30` |
| Purple | `#af52de` |

**Translucent variants**:
- Selected background: accent color at 15% opacity, such as `rgba(10,132,255,0.15)`
- Hover background: accent color at 8% opacity
- Disabled: foreground color at 40% opacity

**CSS variable migration**: Replace the existing `--color-macos-*` variables with consistent HEX values. Remove the shadcn oklch semantic variables, which are not heavily used, and switch fully to the new color system. Remove the Geist Variable font dependency and use the system font stack throughout.

### Typography

| Purpose | Font | Weight | Size |
|---------|------|--------|------|
| Page titles | SF Pro Display | 600 | 16px |
| Section headings | SF Pro Text | 600 | 10px (uppercase) |
| Body text/labels | SF Pro Text | 500 | 12px |
| Secondary information | SF Pro Text | 400 | 11px |
| Data/metrics | SF Pro Display | 700 | 16-20px |
| Logs/code | SF Mono | 400 | 11px |

Font stack: `-apple-system, BlinkMacSystemFont, 'SF Pro Text', 'SF Pro Display', system-ui, sans-serif`
Monospace font stack: `'SF Mono', SFMono-Regular, ui-monospace, Menlo, monospace`

### Spacing and Corner Radii

- **Corner radii**: 5px (controls/buttons) / 8px (cards/panels) / 10px (large containers)
- **Spacing**: 8px (compact, Tailwind p-2) / 12px (standard, p-3) / 16px (comfortable, p-4)
- **Sidebar width**: 180px
- **Minimum window size**: 800 × 500

> Spacing follows Tailwind's default 4px scale; no custom spacing configuration is needed.

### Transitions

| Scenario | Duration | Easing |
|----------|----------|--------|
| Color/background changes | 150ms | ease-out |
| Page navigation | 0ms (immediate) |  |
| Chart updates | 300ms | ease-in-out |
| Hover effects | 100ms | ease-out |

Global default: `transition-colors duration-150 ease-out`

### Component Specifications

**StatusBar**:
- Compact single row, approximately 36px high
- Left: status dot (8px, with glow), status text and supporting information (port/uptime)
- Right: action button (Start/Stop)
- Background one shade darker than the content area
- **Status mapping**: running=green / starting=yellow / stopped=gray / error=red (flashing)
- **Error state**: red background at 10% opacity, red border and error text

**MetricCard**:
- Equal widths (flex: 1)
- Centered layout: large colored value (16-20px) above a secondary-color label (10px)
- Distinguish metrics by color: FPS=blue, bandwidth=green, latency=orange, connections=purple
- **No-data state**: show `--` instead of a value, retain the label and use the secondary color

**ChartPanel**:
- Header: title on the left and current colored value on the right
- Body: SVG area or bar chart
- Area charts use a gradient from 30% accent opacity to 0%
- **No-data state**: gray dashed baseline with centered "Waiting for data..." text
- **Loading state**: low-opacity pulsing placeholder

**ConnItem**:
- Left colored indicator (3px wide, rounded): green=good / orange=fair / red=poor
- Primary information: username@IP (12px, white)
- Secondary information: connection duration (10px, secondary color)
- Right: aligned FPS and bandwidth
- **Empty-list state**: centered icon and "No active connections" text

**SettingRow**:
- Label on the left, control on the right
- Background one shade darker than the content area
- 6px corner radius, 4px spacing between rows
- Control types: Toggle / Select / Input

**LogEntry**:
- Monospace font (SF Mono)
- Three columns: timestamp (70px, gray) | level (40px, bold and colored) | message (flex, light gray)
- Level colors: INFO=blue / WARN=orange / ERROR=red / DEBUG=dark gray
- Compact line height, with a thin bottom border between entries

### Accessibility

- **Color contrast**: All text/background combinations meet WCAG AA (4.5:1). Secondary text uses `#8e8e93` in dark mode (contrast ratio 5.2:1).
- **Keyboard navigation**: Sidebar entries support Tab and Enter; Settings categories support arrow keys.
- **ARIA**: Use `role="navigation"` and `aria-current="page"` for the sidebar, and `role="status"` and `aria-live="polite"` for the status bar.
- **Focus indication**: 2px blue outline with 2px offset.

## Page Designs

### 1. Dashboard — Main Page

**Layout from top to bottom**:
1. **Status bar**: running state, port, uptime and Start/Stop button
2. **Live FPS chart**: Instruments-style area chart showing the last 60 seconds
3. **Metric-card row**: FPS / bandwidth / latency / connections, in four equal columns
4. **Active connections**: connection cards showing quality, user, duration and live metrics

**Data strategy**:
- **FPS history**: Maintain a 60-element ring buffer with `useRef`, adding a new sample from `useMetrics` each second. Reset and rebuild the buffer after navigating away; this is acceptable for noncritical historical data.
- Metric cards update live from Tauri events.
- Connection list updates are event-driven (connect/disconnect/metric changes).

**State handling**:
- **Service stopped**: gray "Stopped" status and "Start" button; "Waiting for data..." in the chart; empty connection list
- **Starting**: yellow "Starting..." status and disabled button
- **Running without connections**: show the chart normally and "No active connections" in the list

### 2. Settings

**Layout**: Two columns within the content area
- Left category navigation (140px): RDP Service / Network / Security / Display / Audio / General
- Right options panel: section heading and a list of SettingRow components

**Category contents** (aligned with UiConfig fields):
- **RDP Service**: encoder (`encoder`), bitrate (`bitrate_mbps`), frame rate (`frame_rate`), chroma mode (`chroma_mode`)
- **Network**: listening port (`port`), bind address (`bind_address`), maximum connections (`max_connections`), timeout (`idle_timeout_secs`)
- **Security**: username/password (`username`/`password`). UiConfig currently hardcodes TLS certificate/key paths to None; UI configuration requires extending UiConfig first.
- **Display**: HiDPI mode (`hidpi_mode`), cursor capture (`capture_cursor`), cursor visibility (`show_cursor`)
- **Audio**: UiConfig does not yet expose audio configuration. This category is a placeholder until the backend connects fields such as `AudioConfig.enabled` to UiConfig.
- **General**: log level (`log_level`), automatic startup (`auto_start`), theme selection (frontend ThemeContext)

> Audio is currently a placeholder, and TLS settings under Security are also unavailable. Enable these progressively as the backend UiConfig fields expand.

### 3. Logs

**Layout**:
1. **Toolbar**: level filters (All/Error/Warn/Info/Debug), search and live indicator
2. **Log list**: Console.app style, monospace, colored levels and compact rows

**Interaction**:
- Highlight the selected filter
- Filter immediately as the search changes
- Automatic scrolling, with a pause option
- Show Live/Paused status in the upper-right corner

**State handling**:
- **No logs**: centered "No log entries" text
- **No search results**: "No matching logs"

### 4. Statistics

**Layout**:
1. **Header**: title and time-range selector (7 days / 30 days)
2. **Summary cards**: total connections / total traffic / average session duration
3. **Traffic chart**: bar chart aggregated by day
4. **Connection history**: four columns for user / time / duration / traffic

> Offer only 7-day and 30-day ranges, matching the existing daily aggregation in `getTrafficStats(days)`. A 24-hour view with hourly aggregation is a future feature, pending backend support.

**State handling**:
- **No statistics**: "No statistics available" in the chart and `--` in summary cards
- **Loading**: skeleton placeholders

### 5. Permissions

**Layout**: Permission cards
- One card per permission: icon, name, status (Granted/Not granted) and request button
- Permission types: Screen Recording, Accessibility and Microphone
- Not granted: orange warning and "Open System Settings" button
- All granted: green "All permissions are ready" message at the top

### 6. About

**Layout**: Centered information card
- App icon, name and version
- Technology details (IronRDP, OpenH264, ScreenCaptureKit)
- Links: GitHub / License / Check for updates

## Navigation Structure

**Sidebar sections**:
```
Service
  └── Dashboard
Configuration
  ├── Settings
  └── Permissions
Diagnostics
  ├── Logs
  └── Statistics
──────
  About ← pinned to the bottom
```

**Sidebar interaction**:
- Selected entry: translucent blue background (15%) and blue text
- Hover: subtle background-color change (8%)
- Section headings: uppercase, 10px, gray and noninteractive
- Separate the bottom About entry from main navigation with a divider
- Place an icon-based theme selector (☀/🌙/Auto) below About

## Window Configuration Changes

| Property | Current | New |
|----------|---------|-----|
| Default width | 900px | 960px |
| Default height | 600px | 620px |
| Minimum width | 720px | 800px |
| Minimum height | 480px | 500px |

## Implementation Notes

### Preserve
- React 19 + TypeScript + Vite 6 + Tauri 2
- Tailwind CSS v4 (utility-first)
- Radix UI primitives (Select, Switch, Tooltip)
- lucide-react icons
- Three-way ThemeContext theme selection
- Tauri IPC communication

### Change
- **globals.css**: replace CSS variables with the new HEX color system; remove oklch and the `--color-macos-*` prefix
- **Fonts**: remove `@fontsource-variable/geist` and use the system font stack
- **Layout**: 180px sidebar with section headings
- **Components**: extract StatusBar, MetricCard, ChartPanel, ConnItem, SettingRow and LogEntry
- **Charts**: lightweight hand-written SVG area/bar charts, avoiding the heavy recharts dependency; manage history with a `useRef` ring buffer
- **Settings**: replace six tiled cards with a two-column layout
- **Logs**: monospace text and colored severity levels
- **tauri.conf.json**: update window dimensions

### No New External Dependencies
Use hand-written SVG chart components to keep the bundle small.

## Design Principles

1. **Appropriate information density**: professional and readable, without crowding
2. **Consistency**: shared components and spacing across pages
3. **Performance first**: smooth chart updates and no unnecessary re-renders
4. **Native macOS appearance**: SF fonts, system colors and rounded corners
5. **Progressive enhancement**: core functionality first, animation refinements later
6. **Accessibility**: WCAG AA contrast and keyboard navigation
