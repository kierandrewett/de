# SUBAGENT: Spotlight Search / App Launcher
# Crate: `crates/shell-launcher`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/shell-launcher`

You are building a Spotlight-style search launcher for a Wayland DE. Standalone iced app, layer-shell overlay, centred on screen. Super key toggles it.

## UI
Floating search bar (wide, centred, ~600px) with results dropdown below. Spring animation on open (scale 0.95→1.0 + opacity 0→1, 200ms). Background blur.

## Search Sources (prioritised)
1. **Applications:** Parse `.desktop` files from `/usr/share/applications`, `~/.local/share/applications`, flatpak dirs. Search by Name, GenericName, Keywords, Categories. Show icon + name + description.
2. **Calculator:** Regex detect math expressions (digits + operators). Evaluate with `meval` crate. Show result as top result with "=" prefix.
3. **Recent files:** Parse `~/.local/share/recently-used.xbel` (freedesktop spec, XML).
4. **System commands:** "shutdown"→poweroff, "restart"→reboot, "lock"→lock_session, "sleep"→suspend, "logout"→terminate_session. Show with system icon.
5. **Web search fallback:** No matches → "Search the web for '{query}'" → open `xdg-open "https://google.com/search?q=..."`.

## Search Algorithm
Use `nucleo` crate (from helix editor — fast, Unicode-aware fuzzy matching).
Scoring: exact prefix > word boundary > substring > fuzzy.
Boost recently launched apps: track launch counts in `~/.local/share/myDE/launch_history.json`, increment on each launch.
Debounce: 50ms after last keystroke.

## Keyboard
- Type to search (autofocus)
- Up/Down: navigate results
- Enter: launch selected result
- Escape: close launcher
- Tab: cycle result categories

## App Launching
- Parse `.desktop` Exec field, handle %f/%F/%u/%U/%i/%c/%k substitutions
- `std::process::Command::new("sh").arg("-c").arg(exec_line)` with `setsid` for detachment
- After launch: close launcher, send `xdg-activation` token for focus

## Visual
- Search bar: squircle-rounded, large text input, magnifying glass icon
- Results: flat list, selected item highlighted with accent colour
- App icons rendered at 32px
- Category headers: "Applications", "Calculator", "Recent Files", "System"
- Max 8 results visible, scrollable

## Dependencies
`iced`, `nucleo`, `meval`, `freedesktop-desktop-entry`, `quick-xml` (for xbel parsing), `ipc` crate, `theme` crate, `rounding` crate, `animation` crate

Work iteratively: .desktop parsing + display first, then search with nucleo, then calculator, then recent files, then system commands, then launch history boosting, then animations/polish.
