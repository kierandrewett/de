# Wayland Protocol Reference: Building a Full Desktop Environment

**Target:** Feature parity with GNOME (Mutter), KDE (KWin), and Hyprland  
**Stack:** Smithay + Slint + custom compositor  
**Date:** April 2026

---

## Overview

There are ~80+ Wayland protocols across core, stable, staging, unstable, and compositor-specific extensions. Not all are relevant. This document categorises every protocol you need to implement, grouped by functional area, with priority tiers:

- **P0 — Launch blocker.** Without these, apps won't display or basic desktop interaction fails.
- **P1 — Expected by users.** Missing these means daily-driver breakage (clipboard, screenshots, gaming, screen sharing).
- **P2 — Full DE parity.** These bring you to GNOME/KDE-level polish (color management, accessibility, session restore).
- **P3 — Nice to have.** Compositor-specific extensions, edge cases.

Smithay coverage is noted where applicable (as of smithay `main`, April 2026).

---

## P0 — Core Foundation (Launch Blockers)

These are non-negotiable. Without them, nothing renders.

### Core Wayland Protocol
| Interface | Purpose | Smithay |
|---|---|---|
| `wl_compositor` (v6) | Surface creation and management | ✅ |
| `wl_subcompositor` (v1) | Subsurface hierarchy (video players, popups) | ✅ |
| `wl_shm` (v2) | Shared memory buffers (software rendering) | ✅ |
| `wl_seat` (v9) | Input devices — keyboard, pointer, touch | ✅ |
| `wl_output` (v4) | Monitor information — geometry, scale, transform | ✅ |
| `wl_data_device_manager` (v3) | Drag-and-drop + clipboard (copy/paste) | ✅ |
| `wl_fixes` (v1) | Protocol error corrections | ✅ |

### Window Management (XDG Shell)
| Protocol | Purpose | Smithay |
|---|---|---|
| `xdg-shell` (v7) | **The** window management protocol. Toplevels, popups, configure events, min/max/fullscreen states | ✅ |
| `xdg-output-unstable-v1` (v3) | Logical output geometry (needed for multi-monitor, fractional scaling) | ✅ |

### Buffer Handling
| Protocol | Purpose | Smithay |
|---|---|---|
| `linux-dmabuf-v1` (v5) | GPU buffer import — hardware-accelerated clients (GL, Vulkan) | ✅ |
| `single-pixel-buffer-v1` (v1) | Efficient solid-color surfaces (borders, backgrounds) | ✅ |

### Display Timing
| Protocol | Purpose | Smithay |
|---|---|---|
| `presentation-time` (v2) | Frame presentation feedback for video/media sync | ✅ |
| `viewporter` (v2) | Surface cropping and scaling | ✅ |

---

## P1 — Daily Driver Essentials

Without these, users will hit real pain points within the first hour.

### Shell Chrome & Panels
| Protocol | Purpose | Smithay |
|---|---|---|
| `wlr-layer-shell-v1` (v5) | Anchored surfaces for panels, docks, wallpaper, overlays, notifications. **Critical for your shell UI.** Every panel, dock, launcher, lock screen overlay uses this. | ✅ |
| `xdg-activation-v1` (v1) | App activation / focus stealing prevention. Lets apps request focus properly (e.g., clicking a notification opens the right window) | ✅ |

### Window Decorations
| Protocol | Purpose | Smithay |
|---|---|---|
| `xdg-decoration-unstable-v1` (v1) | Server-side vs client-side decoration negotiation. **Essential for your custom window decorations.** | ✅ |
| `kde-server-decoration` (v1) | Legacy KDE decoration negotiation. Qt apps may use this. | ✅ |

### Clipboard & Selections
| Protocol | Purpose | Smithay |
|---|---|---|
| `primary-selection-unstable-v1` (v1) | Middle-click paste (X11-style primary selection) | ✅ |
| `ext-data-control-v1` (v1) | Clipboard manager access (wl-clipboard, cliphist). Supersedes `wlr-data-control` | ✅ |
| `wlr-data-control-v1` (v2) | Clipboard manager access (legacy, still widely used by tools) | ✅ |

### Pointer & Input
| Protocol | Purpose | Smithay |
|---|---|---|
| `relative-pointer-unstable-v1` (v1) | Relative mouse motion — **required for FPS games, 3D apps, Blender** | ✅ |
| `pointer-constraints-unstable-v1` (v1) | Pointer lock and confinement — **required for games** | ✅ |
| `pointer-gestures-unstable-v1` (v3) | Touchpad pinch/swipe/hold gestures | ✅ |
| `cursor-shape-v1` (v2) | Named cursor shapes without client-side cursor rendering | ✅ |
| `keyboard-shortcuts-inhibit-unstable-v1` (v1) | Let apps grab all keyboard input (remote desktop, VMs, games) | ✅ |
| `pointer-warp-v1` (v1) | Programmatic cursor repositioning (accessibility, remote desktop) | ✅ |

### Idle & Power
| Protocol | Purpose | Smithay |
|---|---|---|
| `ext-idle-notify-v1` (v2) | Idle timeout detection for screen lockers, DPMS | ✅ |
| `idle-inhibit-unstable-v1` (v1) | Prevent idle while video playing, presenting | ✅ |

### Screen Lock
| Protocol | Purpose | Smithay |
|---|---|---|
| `ext-session-lock-v1` (v1) | Secure session locking with custom graphics. Replaces the old `wlr-input-inhibitor` approach | ✅ |

### Fractional Scaling & HiDPI
| Protocol | Purpose | Smithay |
|---|---|---|
| `fractional-scale-v1` (v1) | Non-integer scale factors (125%, 150%, 175%) | ✅ |

### Screenshot & Screen Capture
| Protocol | Purpose | Smithay |
|---|---|---|
| `ext-image-capture-source-v1` (v1) | Define capture sources (output, window, workspace) | ✅ |
| `ext-image-copy-capture-v1` (v1) | Capture screen content into client buffers. Used for screenshots and screen recording. Supersedes `wlr-screencopy` | ✅ |
| `wlr-screencopy-v1` (v3) | Legacy screencopy — still used by `grim`, `wf-recorder`, OBS wlroots plugin. Implement for compat. | ❌ (use ext- variant) |

### Gaming & VRR
| Protocol | Purpose | Smithay |
|---|---|---|
| `tearing-control-v1` (v1) | Allow tearing for low-latency gaming (like `allowtearing` in Hyprland) | 🚧 |
| `content-type-v1` (v1) | Hint content type (none/photo/video/game) for VRR and display optimisation | ✅ |
| `fifo-v1` (v1) | FIFO presentation for proper vsync without mailbox buffering. Prevents frame drops and VRR judder | ✅ |
| `commit-timing-v1` (v1) | Client-controlled frame pacing | ✅ |

### Text Input & IME
| Protocol | Purpose | Smithay |
|---|---|---|
| `text-input-unstable-v3` (v1) | IME / virtual keyboard integration (CJK input, emoji pickers) | ✅ |
| `input-method-unstable-v2` (v1) | Input method framework (IBus, Fcitx5) | ✅ |
| `virtual-keyboard-v1` (v1) | Virtual/on-screen keyboard injection | ✅ |

### Tablet / Stylus
| Protocol | Purpose | Smithay |
|---|---|---|
| `tablet-v2` (v2) | Graphics tablet support — pressure, tilt, rotation, tool types | 🚧 (lacks pad support) |

---

## P1.5 — Multi-Monitor & Output Management

Critical for any multi-monitor setup, which is most desktop users.

| Protocol | Purpose | Smithay |
|---|---|---|
| `wlr-output-management-v1` (v4) | Runtime output configuration (resolution, position, scale, transform). Used by `wdisplays`, `kanshi`, `wlr-randr` | ❌ (implement yourself) |
| `wlr-output-power-management-v1` (v1) | DPMS control — turn displays on/off | ❌ (implement yourself) |
| `wlr-gamma-control-v1` (v1) | Night light / blue light filter (`wlsunset`, `gammastep`) | ❌ (implement yourself) |

---

## P2 — Full DE Parity

These protocols bring you from "usable compositor" to "competitive desktop environment."

### Toplevel Management (Taskbar, Alt-Tab, Expose)
| Protocol | Purpose | Smithay |
|---|---|---|
| `ext-foreign-toplevel-list-v1` (v1) | List all open windows — powers taskbars, dock window previews, app switchers | ✅ |
| `wlr-foreign-toplevel-management-v1` | Full toplevel control (activate, close, set fullscreen) from external clients. Powers advanced taskbars and scripting | ❌ (implement yourself) |
| `xdg-foreign-unstable-v2` (v1) | Cross-client parent-child relationships (e.g., file picker dialog parented to calling app) | ✅ |
| `xdg-toplevel-drag-v1` | Drag-and-drop of entire windows (tab detach, window reordering) | ❌ |
| `xdg-toplevel-icon-v1` (v1) | App-provided window icons | ✅ |
| `xdg-toplevel-tag-v1` (v1) | Persistent window identification across sessions | ✅ |

### Workspace Management
| Protocol | Purpose | Smithay |
|---|---|---|
| `ext-workspace-v1` | Workspace protocol — lets external tools switch/create/manage workspaces. **If you want workspace indicators in your panel.** | ❌ |

### Dialogs
| Protocol | Purpose | Smithay |
|---|---|---|
| `xdg-dialog-v1` (v1) | Mark windows as dialogs (affects stacking, focus behaviour) | ✅ |

### System Notifications
| Protocol | Purpose | Smithay |
|---|---|---|
| `xdg-system-bell-v1` (v1) | System bell / urgent notification hint | ✅ |

### Background Effects
| Protocol | Purpose | Smithay |
|---|---|---|
| `ext-background-effect-v1` (v1) | Client-requested background blur (terminal transparency, overlays). **Brand new protocol (May 2025).** | ✅ |

### Alpha & Compositing
| Protocol | Purpose | Smithay |
|---|---|---|
| `alpha-modifier-v1` (v1) | Per-surface alpha without client re-rendering. Useful for fade animations, dimming inactive windows | ✅ |

### DRM / GPU Advanced
| Protocol | Purpose | Smithay |
|---|---|---|
| `linux-drm-syncobj-v1` (v1) | Explicit GPU synchronisation (replaces implicit sync). **Required for proper Vulkan compositing and multi-GPU** | ✅ |
| `drm-lease-v1` (v1) | DRM lease for VR headsets (SteamVR, Monado) | ✅ |

### Security
| Protocol | Purpose | Smithay |
|---|---|---|
| `security-context-v1` (v1) | Sandboxed client identification (Flatpak, Snap). Lets you apply per-sandbox policies | ✅ |

### Color Management & HDR
| Protocol | Purpose | Smithay |
|---|---|---|
| `color-management-v1` | Full color management — ICC profiles, HDR, wide gamut. **KDE and GNOME both implementing this.** | 🚧 |
| `color-representation-v1` | Color representation metadata for HDR content | ❌ |

### Session Management
| Protocol | Purpose | Smithay |
|---|---|---|
| `xx-session-management-v1` | Session save/restore — remember window positions across logout/login. **Experimental but important for DE-level UX** | ❌ |

---

## P2.5 — XDG Desktop Portal Integration

These aren't Wayland protocols — they're D-Bus interfaces. But they're **essential** for a complete DE because they're how sandboxed apps (Flatpak) and regular apps access privileged compositor features.

You need to build (or adapt) an `xdg-desktop-portal` backend for your compositor.

| Portal Interface | Purpose | Notes |
|---|---|---|
| `org.freedesktop.portal.ScreenCast` | Screen sharing (Discord, Teams, OBS, browsers) | Uses PipeWire. You feed frames from your compositor into a PipeWire stream. |
| `org.freedesktop.portal.Screenshot` | Screenshots for sandboxed apps | |
| `org.freedesktop.portal.RemoteDesktop` | Remote desktop input injection (via libei) | |
| `org.freedesktop.portal.FileChooser` | Native file picker dialogs | Can delegate to GTK/Qt implementations |
| `org.freedesktop.portal.Settings` | Desktop settings (dark mode, accent color, font) | Apps read these to respect your DE's theme |
| `org.freedesktop.portal.Notification` | Desktop notifications | |
| `org.freedesktop.portal.Background` | Background app permission | |
| `org.freedesktop.portal.DynamicLauncher` | App pinning to dock/launcher | |
| `org.freedesktop.portal.Wallpaper` | Wallpaper setting | |
| `org.freedesktop.portal.GlobalShortcuts` | App-registered global keybindings | |
| `org.freedesktop.portal.Inhibit` | Prevent suspend/idle (presentations, video calls) | |

You can start with `xdg-desktop-portal-wlr` or `xdg-desktop-portal-generic` as a base and extend it, or write your own from scratch using the Wayland protocols you already implement (particularly `ext-image-copy-capture` for ScreenCast).

---

## P3 — Extended Compatibility & Edge Cases

### XWayland
| Protocol | Purpose | Smithay |
|---|---|---|
| `xwayland-shell-v1` (v1) | Associate X11 windows with Wayland surfaces | ✅ |
| `xwayland-keyboard-grab-v1` (v1) | Let XWayland apps grab keyboard (games, VMs) | ✅ |

### Advanced Input
| Protocol | Purpose | Smithay |
|---|---|---|
| `ext-transient-seat-v1` | Temporary input seats (remote desktop sessions) | ❌ |
| `wlr-virtual-pointer-v1` | Virtual pointer injection | ❌ |

### DMA-BUF Export
| Protocol | Purpose | Smithay |
|---|---|---|
| `wlr-export-dmabuf-v1` | Export output as DMA-BUF (screen recording without copy) | ❌ |

### Accessibility
| Protocol | Purpose | Smithay |
|---|---|---|
| `cosmic-atspi-unstable-v1` | AT-SPI2 accessibility bridge | ❌ (COSMIC-specific) |

This is a major gap in the Wayland ecosystem. GNOME handles accessibility through internal Mutter/AT-SPI integration, not a Wayland protocol. COSMIC is pioneering Wayland-native a11y protocols. You'll likely need to build your own AT-SPI bridge or adopt COSMIC's approach.

---

## What Smithay Gives You for Free vs What You Build

### Smithay handles (just wire it up):
All core protocols, `xdg-shell`, `linux-dmabuf`, `wlr-layer-shell`, `xdg-decoration`, clipboard protocols, pointer constraints, gestures, fractional scaling, idle notification, session lock, DRM syncobj, cursor shape, text input, input method, virtual keyboard, image capture, security context, activation, foreign toplevel list, alpha modifier, fifo, commit timing, background effect, pointer warp, dialog, system bell, toplevel icon/tag, DRM lease, xwayland shell

### You build yourself:
- Output management (resolution, position, scale config UI)
- Gamma control (night light)
- Output power management (DPMS)
- Foreign toplevel management (full taskbar control)
- Workspace protocol
- Color management (WIP in smithay)
- Session management
- XDG Desktop Portal backend
- Tearing control (WIP in smithay)
- Your own custom protocols for DE-specific features (like COSMIC does)

---

## Protocol Implementation Priority Roadmap

### Phase 1 — Get windows on screen
Core wayland, `xdg-shell`, `linux-dmabuf`, `wl_shm`, `wl_seat`, `wl_output`, `viewporter`, `presentation-time`, `single-pixel-buffer`, `xdg-output`

### Phase 2 — Make it a desktop
`wlr-layer-shell`, `xdg-decoration`, `kde-server-decoration`, `ext-session-lock`, `ext-idle-notify`, `idle-inhibit`, `fractional-scale`, `cursor-shape`, `xdg-activation`, `primary-selection`, `ext-data-control`, `wlr-data-control`

### Phase 3 — Make it usable for real work
`relative-pointer`, `pointer-constraints`, `pointer-gestures`, `keyboard-shortcuts-inhibit`, `text-input-v3`, `input-method-v2`, `virtual-keyboard`, `ext-image-copy-capture`, `ext-image-capture-source`, `xdg-foreign`, `ext-foreign-toplevel-list`, `alpha-modifier`, `xdg-dialog`

### Phase 4 — Gaming & media
`tearing-control`, `content-type`, `fifo`, `commit-timing`, `linux-drm-syncobj`, `drm-lease`, `tablet-v2`

### Phase 5 — Full DE
Output management, gamma control, portal backend, workspace protocol, toplevel management, color management, session management, background effect, pointer warp, a11y

### Phase 6 — Polish
Toplevel drag, toplevel icon/tag, system bell, security context, transient seat, xwayland keyboard grab, your own custom protocols

---

## Key References

- **Wayland Explorer:** https://wayland.app/protocols/
- **Smithay protocol tracking:** https://github.com/Smithay/smithay/issues/781
- **COSMIC compositor source:** https://github.com/pop-os/cosmic-comp
- **xdg-desktop-portal docs:** https://flatpak.github.io/xdg-desktop-portal/
- **xdg-desktop-portal-generic:** https://github.com/lamco-admin/xdg-desktop-portal-generic
