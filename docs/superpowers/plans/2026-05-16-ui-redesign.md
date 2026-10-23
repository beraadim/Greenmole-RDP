# MacRDP UI Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade macrdp-ui from a basic macOS appearance to the style of Xcode/Instruments professional tools, including a new color system, grouped sidebar, live dashboard charts and Console.app-style logs.

**Architecture:** Implement from the bottom up: establish the design-system foundation (CSS variables/fonts), build reusable components (StatusBar/MetricCard/ChartPanel and others), then rewrite each page. Each task produces independently verifiable work.

**Tech Stack:** React 19, TypeScript, Tailwind CSS v4, Radix UI, lucide-react, Tauri 2, hand-written SVG charts

**Spec:** `docs/superpowers/specs/2026-05-16-ui-redesign-design.md`

---

## File Map

### New Files
| File | Responsibility |
|------|------|
| `src/components/StatusBar.tsx` | Compact service status bar (state/port/uptime/Start and Stop) |
| `src/components/MetricCard.tsx` | Single-metric card (value, label and color) |
| `src/components/ChartPanel.tsx` | SVG chart container (area/bar charts) |
| `src/components/AreaChart.tsx` | SVG area chart (60-point ring buffer) |
| `src/components/BarChart.tsx` | SVG bar chart (traffic trend) |
| `src/components/ConnItem.tsx` | Connection item (quality indicator, user information and metrics) |
| `src/components/LogEntry.tsx` | Log entry (monospace text and colored levels) |
| `src/components/SettingRow.tsx` | Settings row (label and control layout) |
| `src/components/EmptyState.tsx` | Shared empty-state component (icon and text) |
| `src/hooks/useFpsHistory.ts` | FPS ring-buffer hook (60 samples) |

### Files to Rewrite
| File | Lines | Changes |
|------|------|----------|
| `src/styles/globals.css` | 222→~120 | Replace all CSS variables, remove oklch and introduce the new colors |
| `src/components/Sidebar.tsx` | 94→~130 | 180px width, section headings and bottom theme selector |
| `src/pages/Dashboard.tsx` | 177→~180 | Status bar, FPS chart, metric row and connection list |
| `src/pages/Settings.tsx` | 369→~280 | Two-column category navigation and options panel |
| `src/pages/Logs.tsx` | 170→~150 | Console.app appearance, monospace text and colored levels |
| `src/pages/Statistics.tsx` | 256→~200 | Time-range selector, bar chart and history table |
| `src/pages/Permissions.tsx` | 104→~110 | Restyled permission cards |
| `src/pages/About.tsx` | 154→~120 | Centered information card and simpler layout |

### Files to Modify
| File | Changes |
|------|----------|
| `src/App.tsx` | Width classes, replacement of macos-* references and removal of PermissionBanner |
| `src/components/ThemeToggle.tsx` | Adapt to the sidebar bottom with an icon-based selector |
| `src/pages/Popover.tsx` | Replace macos-* classes with the new variable names |
| `src-tauri/tauri.conf.json` | Window size 960×620, minimum 800×500 |
| `package.json` | Remove `@fontsource-variable/geist` |

### Files That Can Be Deleted
| File | Reason |
|------|------|
| `src/components/MetricsStrip.tsx` | Replaced by the MetricCard row |
| `src/components/StatusBadge.tsx` | Replaced by the StatusBar status dot |
| `src/components/ServerInfoTags.tsx` | Information merged into StatusBar |
| `src/components/PermissionCard.tsx` | Inlined into the Permissions page |
| `src/components/PermissionBanner.tsx` | Covered by the Sidebar permission dot and Permissions page |

---

## Task 1: Design-System Foundation — CSS Variables, Fonts and Window Configuration

**Files:**
- Rewrite: `macrdp-ui/src/styles/globals.css`
- Modify: `macrdp-ui/src-tauri/tauri.conf.json`
- Modify: `macrdp-ui/package.json`
- Modify: `macrdp-ui/src/main.tsx` (remove any Geist font import)
- Modify: `macrdp-ui/src/App.tsx` (replace old classes such as `bg-macos-bg`)
- Modify: `macrdp-ui/src/pages/Popover.tsx` (replace `macos-*` references with the new names)

> **Migration strategy**: When rewriting globals.css, retain `--color-macos-*` aliases to the new variables at the bottom as a temporary compatibility layer, so files not yet rewritten (such as Popover) still build. Remove the aliases during Task 11. Alternatively, replace `macos-*` references in App.tsx and Popover.tsx in this task. The second approach is recommended because it completes the migration immediately.

- [ ] **Step 1: Rewrite globals.css**

Replace all CSS variables with the new color system. Remove oklch semantic variables and the `--color-macos-*` prefix. Retain the base variable mappings required by shadcn/ui (`--background`, `--foreground`, `--primary` and others), pointing them to the new colors so components under `src/components/ui/` continue rendering correctly.

Key structure:
```css
@import "tailwindcss";

@theme {
  /* Light mode (default) */
  --color-bg: #f5f5f7;
  --color-sidebar: #ebebed;
  --color-card: #ffffff;
  --color-border: #e8e8e8;
  --color-border-subtle: #f0f0f0;
  --color-text: #1d1d1f;
  --color-text-secondary: #333333;
  --color-text-muted: #86868b;
  --color-accent: #007aff;
  --color-green: #34c759;
  --color-orange: #ff9500;
  --color-red: #ff3b30;
  --color-purple: #af52de;
  --color-yellow: #ffcc00;

  /* Fonts */
  --font-sans: -apple-system, BlinkMacSystemFont, 'SF Pro Text', 'SF Pro Display', system-ui, sans-serif;
  --font-mono: 'SF Mono', SFMono-Regular, ui-monospace, Menlo, monospace;

  /* Corner radii */
  --radius-sm: 5px;
  --radius-md: 8px;
  --radius-lg: 10px;
}

.dark {
  --color-bg: #1a1a1a;
  --color-sidebar: #202020;
  --color-card: #252525;
  --color-border: #2d2d2d;
  --color-border-subtle: #3a3a3a;
  --color-text: #ffffff;
  --color-text-secondary: #cccccc;
  --color-text-muted: #8e8e93;
  --color-accent: #0a84ff;
  --color-green: #30d158;
  --color-orange: #ff9f0a;
  --color-red: #ff453a;
  --color-purple: #bf5af2;
  --color-yellow: #ffd60a;
}

/* Global base styles */
body {
  font-family: var(--font-sans);
  background: var(--color-bg);
  color: var(--color-text);
  -webkit-font-smoothing: antialiased;
}

/* Global transitions */
* { transition: color 150ms ease-out, background-color 150ms ease-out, border-color 150ms ease-out; }
```

> Note: Tailwind v4's `@theme` directive generates the corresponding utility classes automatically, such as `bg-card`, `text-muted` and `border-border`.

- [ ] **Step 2: Remove Geist and Replace Old Class References**

The Geist font import is in `globals.css` and was removed in Step 1. Check `main.tsx` as well and remove it there if present.

Remove the npm dependency if present:
```bash
cd macrdp-ui && npm uninstall @fontsource-variable/geist 2>/dev/null || true
```

Replace old `macos-*` classes in `App.tsx` and `Popover.tsx` with the new variables:
- `bg-macos-bg` → `bg-bg`
- `bg-macos-card` → `bg-card`
- `text-macos-text` → `text-text`
- `text-macos-secondary` → `text-text-secondary`
- `border-macos-border` → `border-border`
- `bg-macos-green` → `bg-green`
- `bg-macos-blue` → `bg-accent`
- `bg-macos-red` → `bg-red`
- `text-macos-green` → `text-green`
- Continue similarly: search for `macos-` throughout the project and replace each occurrence.

- [ ] **Step 3: Update Window Dimensions**

Update the main-window configuration in `macrdp-ui/src-tauri/tauri.conf.json`:
```json
{
  "width": 960,
  "height": 620,
  "minWidth": 800,
  "minHeight": 500
}
```

- [ ] **Step 4: Verify the Foundation Build**

```bash
cd macrdp-ui && npm run build
```
Expected: The build succeeds without errors. Pages load normally, with updated colors and unchanged layout.

- [ ] **Step 5: Commit**

```bash
git add macrdp-ui/src/styles/globals.css macrdp-ui/src-tauri/tauri.conf.json macrdp-ui/package.json macrdp-ui/package-lock.json macrdp-ui/src/main.tsx
git commit -m "refactor(ui): replace design system — new color tokens, system fonts, window sizing"
```

---

## Task 2: Sidebar Rewrite — Grouped Navigation and Bottom Theme Selector

**Files:**
- Rewrite: `macrdp-ui/src/components/Sidebar.tsx`
- Modify: `macrdp-ui/src/components/ThemeToggle.tsx`
- Modify: `macrdp-ui/src/App.tsx` (sidebar width class)

- [ ] **Step 1: Rewrite Sidebar.tsx**

New sidebar structure:
- Width 180px (`w-[180px]`)
- 28px drag region at the top
- Three sections: Service (Dashboard) / Configuration (Settings, Permissions) / Diagnostics (Logs, Statistics)
- Section headings: `text-[10px] font-semibold uppercase tracking-wider text-muted`
- Navigation entries: lucide-react icon (16px) and text label (12px)
- Selected state: `bg-accent/15 text-accent`
- Hover: `hover:bg-accent/8`
- Pinned bottom area: divider, About link and ThemeToggle

```tsx
// Navigation-entry data structure
const navGroups = [
  {
    label: "Service",
    items: [{ path: "/", icon: Monitor, label: "Dashboard" }],
  },
  {
    label: "Configuration",
    items: [
      { path: "/settings", icon: Settings, label: "Settings" },
      { path: "/permissions", icon: Shield, label: "Permissions" },
    ],
  },
  {
    label: "Diagnostics",
    items: [
      { path: "/logs", icon: FileText, label: "Logs" },
      { path: "/statistics", icon: Activity, label: "Statistics" },
    ],
  },
];
```

ARIA: `<nav role="navigation">`, with `aria-current="page"` on the current entry

- [ ] **Step 2: Refactor ThemeToggle.tsx**

Replace the standalone floating button with an inline element at the sidebar bottom:
- Remove fixed positioning and z-50
- Three-state icon button: Sun / Moon / Monitor (lucide-react)
- Click cycle: light → dark → system → light
- Small 28×28px rounded button
- Tooltip showing the current mode

- [ ] **Step 3: Update the App.tsx Layout**

Change the sidebar width class in the main layout:
```diff
- <div className="w-52 ...">
+ <div className="w-[180px] flex-shrink-0 ...">
```

Ensure the `flex-1 overflow-y-auto` content area fills the remaining space correctly.

- [ ] **Step 4: Verify Navigation**

```bash
cd macrdp-ui && npm run dev
```

Verify manually:
- The sidebar is 180px wide and section headings display correctly.
- All six routes can be opened by clicking.
- The selected entry is highlighted in blue.
- The bottom theme control cycles through all three modes correctly.
- The permission warning dot is retained.

- [ ] **Step 5: Commit**

```bash
git add macrdp-ui/src/components/Sidebar.tsx macrdp-ui/src/components/ThemeToggle.tsx macrdp-ui/src/App.tsx
git commit -m "refactor(ui): rewrite sidebar — grouped navigation, 180px width, bottom theme toggle"
```

---

## Task 3: Shared Components — StatusBar, MetricCard and EmptyState

**Files:**
- Create: `macrdp-ui/src/components/StatusBar.tsx`
- Create: `macrdp-ui/src/components/MetricCard.tsx`
- Create: `macrdp-ui/src/components/EmptyState.tsx`

- [ ] **Step 1: Implement StatusBar**

Props:
```tsx
interface StatusBarProps {
  state: "running" | "starting" | "stopped" | "error";
  port?: number;
  uptimeSeconds?: number;
  errorMessage?: string;
  onStart: () => void;
  onStop: () => void;
}
```

Layout: single flex row, 36px high, `bg-card rounded-[8px] px-3`
- Left: status dot (8px, glow via box-shadow), text and supporting information
- Right: Start/Stop button
- Status colors: running=green, starting=yellow, stopped=gray (`text-muted`), error=red
- Error state: `bg-red/10 border border-red/20` and error message
- `role="status" aria-live="polite"`

Uptime format: `formatUptime(seconds)` → "2h 34m" / "5m" / "< 1m"

- [ ] **Step 2: Implement MetricCard**

Props:
```tsx
interface MetricCardProps {
  value: number | null;
  label: string;
  color: "blue" | "green" | "orange" | "purple";
  unit?: string;
  formatter?: (v: number) => string;
}
```

Layout: `flex-1 bg-card rounded-[8px] p-2 text-center`
- Value: `text-xl font-bold` and mapped color
- No data: `--` and `text-muted`
- Label: `text-[10px] text-muted mt-0.5`

Color mapping (Tailwind classes):
```tsx
const colorMap = {
  blue: "text-accent",
  green: "text-green",
  orange: "text-orange",
  purple: "text-purple",
};
```

- [ ] **Step 3: Implement EmptyState**

Props:
```tsx
interface EmptyStateProps {
  icon?: LucideIcon;
  message: string;
}
```

Layout: `flex flex-col items-center justify-center py-8 text-muted`
- 32px icon and 12px text

- [ ] **Step 4: Verify Component Rendering**

Temporarily import and render StatusBar and MetricCard in Dashboard to verify the styling.

- [ ] **Step 5: Commit**

```bash
git add macrdp-ui/src/components/StatusBar.tsx macrdp-ui/src/components/MetricCard.tsx macrdp-ui/src/components/EmptyState.tsx
git commit -m "feat(ui): add StatusBar, MetricCard, EmptyState components"
```

---

## Task 4: Chart Components — AreaChart, BarChart, ChartPanel and useFpsHistory

**Files:**
- Create: `macrdp-ui/src/components/AreaChart.tsx`
- Create: `macrdp-ui/src/components/BarChart.tsx`
- Create: `macrdp-ui/src/components/ChartPanel.tsx`
- Create: `macrdp-ui/src/hooks/useFpsHistory.ts`

- [ ] **Step 1: Implement the useFpsHistory Hook**

Use a 60-element ring buffer and push a metrics sample each second. Use `useState` to trigger rendering:
```tsx
export function useFpsHistory(currentFps: number | null, maxPoints = 60): number[] {
  const bufferRef = useRef<number[]>([]);
  const [, setTick] = useState(0);

  useEffect(() => {
    if (currentFps === null) return;
    const buf = bufferRef.current;
    buf.push(currentFps);
    if (buf.length > maxPoints) buf.shift();
    setTick(t => t + 1); // Trigger a render so AreaChart updates
  }, [currentFps, maxPoints]);

  return bufferRef.current;
}
```

> Note: `useRef` alone does not trigger React rendering. `setTick` provides a lightweight forceUpdate mechanism.

- [ ] **Step 2: Implement AreaChart**

Pure SVG area chart:
```tsx
interface AreaChartProps {
  data: number[];
  color: string;       // CSS color value
  height?: number;     // default 60
  maxValue?: number;   // auto-scale if omitted
}
```

- SVG `viewBox="0 0 {width} {height}"`, `preserveAspectRatio="none"`
- Data points → SVG path (polyline smoothed with simple interpolation)
- Gradient fill: `<linearGradient>` from `{color}/30%` to `transparent`
- Line: `stroke={color} stroke-width="1.5" fill="none"`
- Empty data: render nothing; ChartPanel handles the empty state.

- [ ] **Step 3: Implement BarChart**

Pure SVG bar chart:
```tsx
interface BarChartProps {
  data: { label: string; value: number }[];
  color: string;
  height?: number;    // default 70
}
```

- Equal-width bars with 2px spacing and rounded tops (`rx="2"`)
- Normalize bar heights by maxValue
- Increase opacity to 1.0 on hover (default 0.7)

- [ ] **Step 4: Implement ChartPanel**

Container component:
```tsx
interface ChartPanelProps {
  title: string;
  value?: string;         // Current value shown on the right
  valueColor?: string;    // Current-value color
  empty?: boolean;        // Whether data is unavailable
  emptyText?: string;     // Empty-state message (default "Waiting for data...")
  children: React.ReactNode;
}
```

Layout: `bg-card rounded-[8px] p-3`
- Header: title on the left (`text-[11px] text-muted`) and value on the right (`text-[11px] font-medium`)
- Body: children (the chart)
- Empty state: gray dashed line and centered text

- [ ] **Step 5: Verify Chart Rendering**

Temporarily render AreaChart and BarChart in Dashboard using static data, and verify the SVG output.

- [ ] **Step 6: Commit**

```bash
git add macrdp-ui/src/components/AreaChart.tsx macrdp-ui/src/components/BarChart.tsx macrdp-ui/src/components/ChartPanel.tsx macrdp-ui/src/hooks/useFpsHistory.ts
git commit -m "feat(ui): add SVG chart components (AreaChart, BarChart, ChartPanel) and useFpsHistory hook"
```

---

## Task 5: Connection Component — ConnItem

**Files:**
- Create: `macrdp-ui/src/components/ConnItem.tsx`

- [ ] **Step 1: Implement ConnItem**

Props (reuse the existing `Connection` type):
```tsx
interface ConnItemProps {
  connection: Connection;
}
```

Layout: `flex items-center justify-between bg-card rounded-[8px] p-2`
- Left 3px colored indicator, based on a simple connection-duration/traffic heuristic:
  - Connected > 10min → green (stable connection)
  - Connected > 1min → orange (new connection)
  - Otherwise → accent (just connected)
- Main area: `client_name`@`client_ip` (`text-xs font-medium text-text`) and connection duration (`text-[10px] text-muted`)
- Right: traffic (`text-[11px] font-medium`, formatted as MB/GB) (`text-[10px] text-muted`)

> Note: `Connection` contains only `client_ip`, `client_name`, `connected_at` and `bytes_total`. It has no per-connection FPS data. Base the quality indicator on connection duration instead of FPS.

Connection duration: calculate the difference between `connected_at` and `Date.now()`; reuse or add a `formatDuration()` helper.

- [ ] **Step 2: Commit**

```bash
git add macrdp-ui/src/components/ConnItem.tsx
git commit -m "feat(ui): add ConnItem component with quality indicator"
```

---

## Task 6: Dashboard Page Rewrite

**Files:**
- Rewrite: `macrdp-ui/src/pages/Dashboard.tsx`
- Delete: `macrdp-ui/src/components/MetricsStrip.tsx`
- Delete: `macrdp-ui/src/components/StatusBadge.tsx`
- Delete: `macrdp-ui/src/components/ServerInfoTags.tsx`

- [ ] **Step 1: Rewrite Dashboard.tsx**

Import the new components: StatusBar, MetricCard, ChartPanel, AreaChart, ConnItem, EmptyState
Import hooks: useServerStatus, useMetrics, useConnections, useFpsHistory

Layout (top to bottom, `flex flex-col gap-3 p-4 overflow-y-auto`):
```
1. StatusBar — state/port/uptime/onStart/onStop
2. ChartPanel("Frame rate (FPS)") → AreaChart(fpsHistory, accent color)
3. div.flex.gap-2 →
   MetricCard(fps, "FPS", "blue")
   MetricCard(bitrate, "Mbps", "green", v => (v/1000).toFixed(1))
   MetricCard(latency, "ms latency", "orange")
   MetricCard(connectionCount, "Connections", "purple")
4. ConnPanel (bg-card rounded-[8px] p-3):
   Header: "Active connections" and count badge
   connections.map(c => <ConnItem key={c.id} connection={c} />)
   Or EmptyState("No active connections")
```

Data flow:
- `const { status, start, stop } = useServerStatus()`
- `const metrics = useMetrics()`
- `const connections = useConnections()`
- `const fpsHistory = useFpsHistory(metrics?.fps ?? null)`

- [ ] **Step 2: Delete Obsolete Components**

Delete `MetricsStrip.tsx`, `StatusBadge.tsx` and `ServerInfoTags.tsx`. Search the project to confirm that no references remain.

- [ ] **Step 3: Verify Dashboard**

```bash
cd macrdp-ui && npm run dev
```

Verify manually:
- Service stopped: gray status bar, empty chart and empty connection list
- Service running: green status bar, FPS curve fills progressively and metric cards update live
- Connected: connection list displays with the correct colored indicators

- [ ] **Step 4: Commit**

```bash
git add macrdp-ui/src/pages/Dashboard.tsx
git rm macrdp-ui/src/components/MetricsStrip.tsx macrdp-ui/src/components/StatusBadge.tsx macrdp-ui/src/components/ServerInfoTags.tsx
git commit -m "feat(ui): rewrite Dashboard — status bar, FPS chart, metrics cards, connection list"
```

---

## Task 7: Settings Page Rewrite — Two-Column Layout

**Files:**
- Create: `macrdp-ui/src/components/SettingRow.tsx`
- Rewrite: `macrdp-ui/src/pages/Settings.tsx`

- [ ] **Step 1: Implement SettingRow**

Props:
```tsx
interface SettingRowProps {
  label: string;
  description?: string;
  children: React.ReactNode;  // control (Switch, Select, Input)
}
```

Layout: `flex items-center justify-between bg-card rounded-[6px] px-3 py-2`
- Left: label (`text-xs text-text-secondary`) and optional description (`text-[10px] text-muted`)
- Right: children (control slot)

- [ ] **Step 2: Rewrite Settings.tsx**

Two-column structure:
```tsx
const categories = [
  { id: "rdp", label: "RDP Service" },
  { id: "network", label: "Network" },
  { id: "security", label: "Security" },
  { id: "display", label: "Display" },
  { id: "audio", label: "Audio" },
  { id: "general", label: "General" },
];
```

Layout: `flex h-full`
- Left navigation (140px, `border-r border-border`): category list with `bg-accent/12 text-accent` for the selected item
- Right panel (`flex-1 overflow-y-auto p-4`): section heading and SettingRow list

Map each category directly to UiConfig fields:
- RDP Service: encoder (Select), bitrate_mbps (Select: 2/4/8/16/32), frame_rate (Select: 30/60/120), chroma_mode (Select)
- Network: port (Input), bind_address (Input), max_connections (Input), idle_timeout_secs (Input)
- Security: username (Input), password (Input type=password)
- Display: hidpi_scale (Select: 1/2/3/4), show_cursor (Switch)
- Audio: show an "Audio configuration is not yet available" notice
- General: log_level (Select), auto_start (Switch), theme (three-way Select)

Preserve the existing `setConfig(key, value)` calling pattern and `restart_required` notice.

- [ ] **Step 3: Verify Settings**

Verify manually:
- Clicking left-side categories changes the selection.
- The right-side options display correctly.
- Editing values calls `setConfig` correctly.
- The restart_required notice appears correctly.

- [ ] **Step 4: Commit**

```bash
git add macrdp-ui/src/components/SettingRow.tsx macrdp-ui/src/pages/Settings.tsx
git commit -m "feat(ui): rewrite Settings — split panel layout with category navigation"
```

---

## Task 8: Logs Page Rewrite — Console.app Style

**Files:**
- Create: `macrdp-ui/src/components/LogEntry.tsx`
- Rewrite: `macrdp-ui/src/pages/Logs.tsx`

- [ ] **Step 1: Implement LogEntry**

Props (reuse the existing `LogEntry` type):
```tsx
interface LogEntryRowProps {
  entry: LogEntry;
}
```

Layout: `flex gap-2 px-2.5 py-1 border-b border-border font-mono text-[11px]`
- Timestamp: `min-w-[70px] text-muted` (format: "HH:mm:ss")
- Level: `min-w-[40px] font-semibold` and mapped color
  - "info" → `text-accent`, "warn" → `text-orange`, "error" → `text-red`, "debug" → `text-muted`, "trace" → `text-muted`
- Message: `flex-1 text-text-secondary truncate`

- [ ] **Step 2: Rewrite Logs.tsx**

Layout (`flex flex-col gap-3 p-4 h-full`):
```
1. Header: "Logs" and live indicator (green ● "Live" / gray ⏸ "Paused")
2. Toolbar: level filters and search input (flex gap-1.5)
   - Filters: "All" | "Error" | "Warn" | "Info" | "Debug"
   - Selected state: bg-accent/15 border-accent text-accent
   - Default state: bg-card border-border-subtle text-muted
   - Search: flex-1, bg-card, border, rounded-[5px], placeholder "Search logs..."
3. LogList: flex-1 overflow-y-auto, bg-[#1a1a1a] dark:bg-[#111] rounded-[6px] border
   - logs.filter(levelFilter).filter(searchFilter).map(log => <LogEntry />)
   - Empty state: EmptyState("No log entries") or "No matching logs"
```

Preserve the existing useLogs polling mechanism and automatic-scrolling logic.

- [ ] **Step 3: Verify Logs**

Verify manually:
- Monospace text renders correctly.
- Severity colors are correct (INFO=blue, WARN=orange, ERROR=red).
- Filtering and search work.
- Automatic scrolling and pause/resume work.

- [ ] **Step 4: Commit**

```bash
git add macrdp-ui/src/components/LogEntry.tsx macrdp-ui/src/pages/Logs.tsx
git commit -m "feat(ui): rewrite Logs — Console.app style with monospace font and color-coded levels"
```

---

## Task 9: Statistics Page Rewrite

**Files:**
- Rewrite: `macrdp-ui/src/pages/Statistics.tsx`

- [ ] **Step 1: Rewrite Statistics.tsx**

Layout (`flex flex-col gap-3 p-4 overflow-y-auto`):
```
1. Header: "Statistics" and time-range selector (7 days | 30 days, pill-button group)
2. Summary-card row: 3× MetricCard
   - Total connections (blue)
   - Total traffic (green, formatted as GB/MB)
   - Average session duration (orange, formatted as h m)
3. ChartPanel("Traffic trend") → BarChart(trafficStats)
4. Connection-history table:
   bg-card rounded-[8px] p-3
   Header: User | Time | Duration | Traffic (text-[10px] text-muted font-medium)
   Rows: text-[11px], border-b border-border
   Preserve the existing pagination logic (PAGE_SIZE=20).
```

Data: reuse `getTrafficStats(days)` and `getConnectionHistory(limit, offset)`.
Set `days` from the selected time range (7 or 30).

Empty state: MetricCard shows `--`, ChartPanel shows "No statistics available", and the table shows EmptyState.

- [ ] **Step 2: Verify Statistics**

Verify manually:
- The 7-day/30-day selector requests the correct data.
- The bar chart renders correctly.
- Pagination works.
- The empty state appears when no data is available.

- [ ] **Step 3: Commit**

```bash
git add macrdp-ui/src/pages/Statistics.tsx
git commit -m "feat(ui): rewrite Statistics — time range selector, bar chart, summary cards"
```

---

## Task 10: Permissions and About Page Rewrites

**Files:**
- Rewrite: `macrdp-ui/src/pages/Permissions.tsx`
- Rewrite: `macrdp-ui/src/pages/About.tsx`
- Modify: `macrdp-ui/src/App.tsx` (remove the PermissionBanner import and rendering)
- Delete: `macrdp-ui/src/components/PermissionCard.tsx` (inline into the page)
- Delete: `macrdp-ui/src/components/PermissionBanner.tsx` (replace with an inline notice)

- [ ] **Step 1: Rewrite Permissions.tsx**

Layout (`flex flex-col gap-3 p-4`):
```
1. Header: "Permissions"
2. When all permissions are granted: green notice (bg-green/10 border-green/20 text-green)
3. Permission-card list (gap-2):
   Each card: bg-card rounded-[8px] p-3
   - Left: icon (Monitor/Hand/Mic), name and status text
   - Right: granted → green ✓ / not granted → "Open System Settings" button (orange)
```

Permissions: screen_capture (Screen Recording), accessibility (Accessibility), microphone (Microphone)

- [ ] **Step 2: Rewrite About.tsx**

Layout (`flex flex-col items-center justify-center h-full p-4`):
```
Centered card (bg-card rounded-[10px] p-6 max-w-sm text-center):
1. App icon (64px)
2. "MacRDP" (text-lg font-semibold)
3. Version (text-xs text-muted)
4. Divider
5. Technology: IronRDP · OpenH264 · ScreenCaptureKit (text-[11px] text-muted)
6. Links: GitHub | License | Check for updates (text-xs text-accent)
```

- [ ] **Step 3: Delete Obsolete Components and Update App.tsx**

Delete `PermissionCard.tsx` and `PermissionBanner.tsx`.
Update `App.tsx` at the same time: remove `import PermissionBanner` and the `<PermissionBanner />` rendering currently between Sidebar and the main content. The new Sidebar permission dot and Permissions page cover the warning functionality.

Search to confirm that no other references remain:
```bash
cd macrdp-ui && grep -r "PermissionBanner\|PermissionCard" src/ --include="*.tsx"
```

- [ ] **Step 4: Verify Both Pages**

Verify manually that Permissions and About display correctly.

- [ ] **Step 5: Commit**

```bash
git add macrdp-ui/src/pages/Permissions.tsx macrdp-ui/src/pages/About.tsx macrdp-ui/src/App.tsx
git rm macrdp-ui/src/components/PermissionCard.tsx macrdp-ui/src/components/PermissionBanner.tsx
git commit -m "feat(ui): rewrite Permissions and About pages, remove PermissionBanner"
```

---

## Task 11: Cleanup and Complete Verification

**Files:**
- Modify: Various (cleanup unused imports, dead code)

- [ ] **Step 1: Remove Unused shadcn Variables**

Check globals.css for remaining references to old variables. Search the entire `src/` directory:
```bash
cd macrdp-ui && grep -r "macos-" src/ --include="*.tsx" --include="*.ts" --include="*.css"
```

Replace all old `macos-*` variable references with the new names.

- [ ] **Step 2: Check for Unused Imports**

```bash
cd macrdp-ui && npx tsc --noEmit
```

Fix all TypeScript compilation errors and unused imports.

- [ ] **Step 3: Verify the Build**

```bash
cd macrdp-ui && npm run build
```

Expected: The build succeeds without errors or warnings.

- [ ] **Step 4: Complete Functional Verification**

Test each page manually:
- [ ] Dashboard: service Start/Stop, chart rendering and connection list
- [ ] Settings: all categories, controls and configuration persistence
- [ ] Permissions: permission-state checks and links to System Settings
- [ ] Logs: filters, search and automatic scrolling
- [ ] Statistics: time-range selection and pagination
- [ ] About: version information and links
- [ ] Theme: Light/Dark/System modes work correctly
- [ ] Popover: unaffected and still functional

- [ ] **Step 5: Final Commit**

```bash
git add -A macrdp-ui/
git commit -m "chore(ui): cleanup dead code, fix remaining references to old design system"
```
