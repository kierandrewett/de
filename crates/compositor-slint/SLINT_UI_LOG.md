# SLINT UI LOG

## Per-deliverable status

| File | Status | Notes |
|---|---|---|
| `slint/Tokens.slint` | ✅ DONE | Design tokens from WINDOW_SPEC.md: colours, radii, typography, sizes |
| `slint/Wallpaper.slint` | ✅ DONE | Full-screen `Image` with `#1e1e2e` fallback |
| `slint/Panel.slint` | ✅ DONE | 34 px top bar, 3-zone layout, clock callback, CC callback, hover highlights |
| `slint/Dock.slint` | ✅ DONE | Floating dock with hover-scale 1.0→1.15, active dots, pinned/running separator |
| `slint/WindowChrome.slint` | ✅ DONE | 33 px title bar, dark active colours, inner highlight strip, controls row |
| `slint/LayerSurface.slint` | ✅ DONE | Positioned Image for external layer-shell clients |
| `slint/Compositor.slint` | ✅ DONE | Top-level component with full Rust-bindable contract |
| `slint/preview/PreviewMain.slint` | ✅ DONE | Mock data preview, inherits Compositor directly |
| `slint/widgets/StatusIcon.slint` | ✅ DONE | Coloured-square fallback + real icon support |
| `slint/widgets/IconButton.slint` | ✅ DONE | Circular button, hover/press colours, scale feedback |
| `build.rs` | ✅ DONE | Compiles `slint/Compositor.slint` via slint-build |
| `Cargo.toml` | ✅ DONE | Added `slint-build = "1.16"` build-dependency |
| `src/main.rs` | ✅ DONE | Replaced inline `slint!{}` block; shim keeps `CompositorUI` for renderer.rs |

## Compile verification

All `.slint` files pass `slint-build` with **zero errors and zero warnings** (verified via a standalone validation crate in `/tmp/slint-validate` — the main workspace target disk was full during this session).

`slint-viewer` was not installed; no screenshot available.

## Approximations and Slint model limitations

- **Squircle (corner-smoothing: 0.6):** Slint's `border-radius` is a circular arc. True squircle SDF is not expressible in `.slint`; the GPU agent must apply the squircle shader on top. All corner radii use the spec'd 14 px / 20 px values as circular-arc approximations.

- **Multi-layer shadow:** Slint supports exactly one `drop-shadow-blur / drop-shadow-offset-y / drop-shadow-color` per `Rectangle`. The WINDOW_SPEC requires 3 active layers and 2 inactive layers. Only the medium layer (blur: 24 px, offset-y: 8 px, color: rgba(0,0,0,0.45)) is expressed in Slint; the GPU agent must composite the additional layers.

- **0.5 px border:** CSS sub-pixel borders are not supported in Slint; 1 px borders are used throughout with reduced opacity to approximate the 0.5 px visual weight.

- **`z` must be integer literal:** Slint's `z` property requires a compile-time literal. The z-order constants in `Tokens.slint` are documented as comments; `Compositor.slint` uses inline literals (0, 10, 20, 30, 40, 50).

- **Inner highlight as gradient strip:** The WINDOW_SPEC non-uniform inset stroke is approximated as a 2 px tall gradient Rectangle at the top edge of the chrome, fading from rgba(255,255,255,0.08) to transparent. A true per-edge alpha-modulated squircle stroke requires GPU-layer post-processing.
