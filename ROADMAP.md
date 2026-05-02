# Roadmap — myDE on compositor-slint

This is the post-Slint-migration plan. Wave-based; each wave merges into
`develop` before the next starts. Anything ★ is part of wave 1 (in flight).

---

## Carried-over stubs (from the migration logs)

| # | Item | Where |
|---|---|---|
| 1 | DMA-BUF client buffers (wgpu-28 lacks stable HAL import) | renderer.rs |
| 2 | Multi-window WM beyond cascading-stack placement | renderer.rs / new wm module |
| 3 | SVG icons for Nautilus / generic fallback (Slint FemtoVG no resvg) | desktop.rs |
| 4 | `focused-app` panel field not tracked | renderer.rs / wayland |
| 5 | External layer-shell client compositing (third-party shells) | wayland/layer_shell.rs |
| 6 | Light-mode chrome colours (dark-mode used for both states) | chrome_shader.rs / theme |
| 7 | Shadow blur is `exp(-t²/2)` approx, not separable Gaussian | chrome.wgsl |
| 8 | `dock-pinned-apps: [string]` property (UI uses single `items` instead) | Compositor.slint |

---

## Your explicit asks

| # | Item | Notes |
|---|---|---|
| A | Light/dark mode + animated crossfade | global theme token + per-state animation |
| B | Hypr-quickshell-inspired panel + dock styling | look at /tmp/hypr-config/ui/ for style guide |
| C | Animated shadows on focus/blur | crossfade between active/inactive shadow stacks |
| D | Animated titlebars on focus/blur | bg + text alpha crossfade |
| E | SVG window control icons | replace placeholder squares; load via resvg |
| F | Resize cursor rotates around squircle corner | corner SDF + cursor angle = atan2(local), pivots on hover |
| G | Window open / close / minimize / maximize animations | spring-based scale + opacity |
| H | Window resizing | drag-from-edge logic + xdg-toplevel.configure resize |
| I | Custom wallpaper | already shipped in last wave; UX for changing it |
| J | Window borders inset / overlaid (not clipped) | chrome shader composes on top of client texture |
| K | "100 QoL improvements" | see below |

---

## 100 QoL items

Numbered for tracking. Dispatched across waves 3-6.

### Window management (1-15)
1. Workspaces with horizontal slide animation between them
2. Alt-Tab switcher with live thumbnails
3. Mission Control / Exposé (show all windows zoomed out)
4. Window snapping — edges, halves, quarters, thirds (Linux-superkey style)
5. Magnetic snap to other windows
6. Drag title bar to top edge → maximize
7. Drag title bar to side edge → snap to half
8. Snap-assist overlay when dragging
9. Double-click title bar to maximize / restore
10. Always-on-top toggle
11. Window groups / Stage Manager
12. Picture-in-picture for video clients
13. Per-window opacity hotkey
14. Hide window decorations toggle
15. Roll-up / windowshade

### Cursors + input (16-25)
16. Cursor theme support (XCursor + custom SVG)
17. Resize cursor rotates around squircle corner ★ wave 1
18. Move cursor (4-way arrow) on title bar drag
19. Smooth-follow cursor easing setting
20. Cursor hide on keyboard input
21. Touchpad gestures: 3-finger swipe = workspace switch
22. Touchpad gestures: 4-finger pinch = mission control
23. Touchpad edge swipes = launcher / control centre
24. Tablet stylus support (pressure, tilt)
25. Touch input + on-screen keyboard

### Visual polish (26-40)
26. Animated focus / blur shadow + titlebar ★ wave 1
27. Light / dark mode + animated crossfade ★ wave 1
28. Window open spring animation ★ wave 1
29. Window close spring animation ★ wave 1
30. Minimize-to-dock genie effect
31. Maximize zoom animation
32. Squircle corners pixel-perfect (true SDF, already shipped) ✅
33. Multi-layer macOS shadow (true separable Gaussian)
34. Backdrop blur for transparent surfaces (compositor-side blur)
35. Per-app accent colours
36. System-wide accent colour
37. Custom theme JSON
38. UI scale setting (independent of fractional scale)
39. Reduced motion accessibility
40. Animation intensity setting (off / reduced / normal / extra)

### Panel + dock (41-55)
41. Panel: focused-app text on left ★ wave 1
42. Panel: clock with seconds toggle
43. Panel: tray icons (StatusNotifierItem)
44. Panel: network indicator + popover
45. Panel: bluetooth indicator + popover
46. Panel: audio indicator + slider + device selector
47. Panel: brightness slider
48. Panel: battery indicator with detail
49. Panel: MPRIS now-playing widget
50. Control centre popout (top-right dropdown)
51. Date/time popout with calendar
52. World clocks
53. Weather widget
54. Dock auto-hide
55. Dock magnification on hover

### Shell apps (56-70)
56. App launcher (Spotlight-style, fuzzy + recent + math)
57. Calculator in launcher
58. Unit conversion in launcher
59. File search in launcher
60. Emoji picker
61. Color picker
62. Clipboard manager (history + search)
63. Notifications (org.freedesktop.Notifications)
64. Notification stacking + grouping
65. Notification do-not-disturb scheduling
66. Notification action buttons
67. Screenshot tool (region select, window, full)
68. Screen recording (with audio toggle)
69. Quick notes scratchpad
70. Pomodoro / break reminders

### Sessions, lock, power (71-80)
71. Power menu (shutdown / reboot / suspend / lock / log out)
72. Auto-lock on idle
73. Lock screen with clock + notifications
74. greetd integration for login
75. Per-user wallpaper
76. Per-user theme
77. Wallpaper slideshow
78. Wallpaper from Picture / URL / generative
79. Night light (warm tint) with schedule
80. Auto light/dark on sunset/sunrise

### Compositor protocols + DMA-BUF (81-90)
81. DMA-BUF client buffers (wgpu-29 or EGL bridge) ★ wave 1
82. ext-foreign-toplevel-list full coverage
83. Screen sharing portal (PipeWire)
84. Screen recording portal (PipeWire)
85. xdg-activation for app focus requests
86. Idle-inhibit during video playback
87. Session-lock protocol full impl
88. wlr-output-management for per-monitor setup
89. wlr-gamma-control for night light
90. ext-workspace-v1 for external workspace indicators

### Quality + accessibility (91-100)
91. Smooth scrolling everywhere
92. Configurable keybindings (JSON config)
93. Hotkeys overlay (press to see all)
94. Right-click context menus on dock + window decorations
95. Window menu (right-click title bar)
96. Quick look (preview file with space)
97. Magnifier / zoom accessibility
98. High contrast theme
99. Screen reader hooks (AT-SPI)
100. Keyboard navigation everywhere (no mouse needed)

---

## Wave structure

### Wave 1 (in flight) — foundations everything else depends on

| Agent | Scope |
|---|---|
| A | Multi-window WM + window open/close/min/max animations |
| B | Light/dark theme + animated focus/blur (shadows, titlebars, chrome) |
| C | Cursor system + corner-rotation resize cursor + window resizing logic + SVG control icons |
| D | DMA-BUF (wgpu-29 upgrade or EGL bridge), border-overlaid contract, true Gaussian shadow blur, SVG icon decode (resvg for dock) |

### Wave 2 — visual polish + shell

| Agent | Scope |
|---|---|
| E | Quickshell-inspired panel + dock visual redesign (use /tmp/hypr-config as reference) |
| F | External layer-shell client compositing + focused-app tracking + control centre popout + date/time popout |
| G | Notifications + screenshot + screen recording portals |
| H | Audio/brightness/network/bluetooth/battery indicator widgets |

### Wave 3 — workspaces + WM polish

| Agent | Scope |
|---|---|
| I | Workspaces + alt-tab switcher + Mission Control |
| J | Window snapping + magnetic edges + drag-from-edge resize |
| K | App launcher (Spotlight-style) + calculator + emoji picker + clipboard manager |
| L | Touchpad gestures + IME + on-screen keyboard |

### Wave 4 — sessions + lock + power

| Agent | Scope |
|---|---|
| M | Lock screen + auto-lock + greetd integration |
| N | Power menu + idle handling + session lock protocol |
| O | Night light + auto light/dark + wallpaper management UX |
| P | wlr-output-management + per-monitor setup + per-monitor scale |

### Wave 5 — accessibility + custom

| Agent | Scope |
|---|---|
| Q | Cursor themes + icon themes + accent colours |
| R | UI scaling + reduced motion + high contrast |
| S | Settings UI for everything above |
| T | Magnifier + screen reader hooks + keyboard nav |

### Wave 6 — depth

Everything left in the QoL list, plus polish passes on what's been shipped.

---

## Non-goals (for now)

- X11 compatibility (we're Wayland-native; XWayland later if needed)
- Mobile / tablet form factor (desktop only; touch supported but not optimised)
- Bug-for-bug compatibility with macOS or GNOME — we draw inspiration but choose our own UX
- Stable plugin API — internal velocity matters more than ABI stability right now

---

## Velocity expectations

Each wave is 3-7 days of agent + merge work depending on complexity. Total
roadmap is realistically 2-4 months of focused effort. The numbered QoL items
are commit-trackable; we update this file as items land (✅) or get deferred.
