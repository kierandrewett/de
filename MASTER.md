# MASTER ORCHESTRATOR — Custom Wayland Desktop Environment

You are orchestrating the build of a custom Wayland desktop environment in Rust. You will deploy subagent workers using Claude Code with Sonnet, each operating in its own git worktree branch.

**CRITICAL:** Before deploying any subagent, ensure `ARCHITECTURE.md` is present in the repo root. Every subagent prompt references it. It contains:
- Why we chose Smithay over wlroots (and which wlr-* protocols we still need)
- How Wayland compositing works (surface→texture→composite pipeline)
- Complete protocol reference (P0/P1/P2 with smithay coverage status)
- Smithay backend architecture (winit dev mode + udev production mode)
- Smithay API patterns (delegate pattern, GlesRenderer, calloop)
- COSMIC as reference architecture
- Development workflow (nested compositor via winit)

## Stack
- **Smithay** — Wayland compositor library (Rust)
- **Slint** (1.16, unstable-wgpu-28) — single-process UI for shell chrome,
  window decorations, panel, dock, popouts. Replaces the iced-based
  multi-process shell.
- **wgpu-28** + custom WGSL passes — GPU compositing (shadow / border /
  highlight / blur)
- **resvg/usvg** — SVG rasterisation (cursors, icons)
- **tiny-skia** — CPU rendering (cursor + backdrop synth)
- **calloop** — event loop (smithay's default)
- **zbus** — D-Bus (portals, notifications, tray, MPRIS)
- **PipeWire** — screen sharing/recording streams (planned)

## Workspace Structure
```
Cargo.toml                    # workspace root
crates/
├── compositor-slint/         # Wayland compositor + shell (single binary)
│   └── src/
│       ├── main.rs           # entry point
│       ├── renderer.rs       # frame loop, wgpu pipeline, IPC dispatch
│       ├── wayland/          # per-protocol handler split
│       ├── render/           # chrome shader passes (shadow / border / hl)
│       ├── wm/               # window manager (z-order, animations)
│       ├── desktop.rs        # dock entry resolution + icon loading
│       ├── tray.rs           # StatusNotifierWatcher / KStatusNotifierItem
│       └── ipc_server.rs     # unix socket IPC server
├── rounding/                 # Squircle corner rounding engine
├── animation/                # Spring + easing animation engine
├── cursor/                   # SVG cursor theme loader + renderer
├── text-render/              # Text rasterisation helper
├── status-notifier/          # StatusNotifierItem D-Bus host (lib)
├── notification/             # org.freedesktop.Notifications server (bin)
├── portal/                   # xdg-desktop-portal backend (bin)
├── theme/                    # Shared theme tokens, colours, spacing
└── ipc/                      # Shared IPC message types
```

## Worktree Branch Strategy
Each subagent works on its own branch via `git worktree`:
```bash
git worktree add ../de-rounding feat/rounding
git worktree add ../de-animation feat/animation
git worktree add ../de-cursor feat/cursor
# ... etc
```

Subagents MUST:
1. Create their crate with `Cargo.toml` and full source
2. Write comprehensive tests (`cargo test`)
3. Run `cargo clippy -- -D warnings`
4. Use `#![deny(missing_docs)]` on library crates
5. Commit iteratively with meaningful messages
6. NOT modify crates outside their assignment
7. NOT add workspace members to root `Cargo.toml` (orchestrator does that)

## Deployment Phases

### Phase 1 — Foundations (no inter-dependencies, deploy all in parallel)
| Branch | Subagent | Prompt File |
|--------|----------|-------------|
| `feat/rounding` | Squircle rounding engine | `subagents/01-rounding.md` |
| `feat/animation` | Spring + easing animation | `subagents/02-animation.md` |
| `feat/cursor` | SVG cursor subsystem | `subagents/03-cursor.md` |
| `feat/theme` | Shared theme tokens | `subagents/05-theme.md` |
| `feat/ipc` | Shared IPC types | `subagents/06-ipc.md` |

### Phase 2 — Compositor core (depends on Phase 1)
| Branch | Subagent | Prompt File |
|--------|----------|-------------|
| `feat/compositor-protocols` | Wayland protocol wiring | `subagents/07-protocols.md` |
| `feat/compositor-render` | Render pipeline + borders | `subagents/08-render.md` |
| `feat/compositor-shell` | Window management + snapping | `subagents/09-window-mgmt.md` |

### Phase 3 — D-Bus services (independent of Phase 2)
| Branch | Subagent | Prompt File |
|--------|----------|-------------|
| `feat/status-notifier` | System tray (SNI) | `subagents/10-status-notifier.md` |
| `feat/notification` | Notification server | `subagents/11-notification.md` |
| `feat/portal` | XDG Desktop Portal | `subagents/12-portal.md` |

### Phase 4 — Shell UI (depends on Phase 2 + 3)
The original plan was three separate iced layer-shell binaries (panel,
dock, launcher). The current implementation folds all three into the
compositor's own Slint scene under `crates/compositor-slint/slint/` —
single process, single render pipeline, no IPC roundtrip for shell UI.

## Subagent Dispatch Command
For each subagent, run:
```bash
cd ../de-<name>
claude --model sonnet "Read de-prompts/subagents/<NN>-<n>.md and implement everything. Read ARCHITECTURE.md for context. Work iteratively, commit frequently."
```

## Integration
After all subagents in a phase complete:
1. Merge their branches into `develop`
2. Add all new crate members to workspace `Cargo.toml`
3. Run `cargo build --workspace` to verify integration
4. Run `cargo test --workspace`
5. Fix any cross-crate type mismatches
6. Then deploy next phase

## Root Cargo.toml
The active workspace is the source of truth — see `Cargo.toml` at the
repo root. The shell-* crates listed in earlier revisions of this doc
have been retired.
