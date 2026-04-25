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
- **iced** — UI framework for shell chrome, window decorations, and apps
- **resvg/usvg** — SVG rasterisation (cursors, icons)
- **tiny-skia** — CPU rendering (prototyping, fallback)
- **wgpu** or smithay's GlesRenderer — GPU rendering
- **calloop** — event loop (smithay's default)
- **zbus** — D-Bus (portals, notifications, tray, MPRIS)
- **PipeWire** — screen sharing/recording streams

## Backends
- **Dev mode:** `cargo run -p compositor -- --winit` — runs as nested compositor in a winit window
- **Prod mode:** `cargo run -p compositor -- --tty-udev` — runs on bare TTY with DRM/KMS

## Workspace Structure
```
Cargo.toml                    # workspace root
crates/
├── compositor/               # Wayland compositor binary (smithay)
│   └── src/
│       ├── main.rs           # backend selection (winit vs udev)
│       ├── state.rs          # global compositor state
│       ├── render/           # rendering pipeline, borders, shadows, clipping
│       ├── shell/            # window management, snapping, animations
│       ├── wayland/          # protocol handler wiring
│       └── ipc.rs            # unix socket IPC server
├── rounding/                 # Squircle corner rounding engine
├── animation/                # Spring + easing animation engine
├── cursor/                   # SVG cursor theme loader + renderer
├── status-notifier/          # StatusNotifierItem D-Bus host
├── notification/             # org.freedesktop.Notifications server
├── portal/                   # xdg-desktop-portal backend (D-Bus service)
├── portal-ui/                # File picker UI (standalone iced app)
├── shell-panel/              # Top panel + control centre (iced, layer-shell)
├── shell-dock/               # Dock (iced, layer-shell)
├── shell-launcher/           # Spotlight search (iced, layer-shell)
├── shell-devtools/           # Iced UI inspector/devtools
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
| `feat/devtools` | Iced DevTools inspector | `subagents/04-devtools.md` |
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
| Branch | Subagent | Prompt File |
|--------|----------|-------------|
| `feat/shell-panel` | Panel + control centre | `subagents/13-panel.md` |
| `feat/shell-dock` | Dock | `subagents/14-dock.md` |
| `feat/shell-launcher` | Spotlight launcher | `subagents/15-launcher.md` |

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

## Root Cargo.toml Template
```toml
[workspace]
resolver = "2"
members = [
    "crates/rounding",
    "crates/animation",
    "crates/cursor",
    "crates/theme",
    "crates/ipc",
    "crates/shell-devtools",
    "crates/status-notifier",
    "crates/notification",
    "crates/portal",
    "crates/portal-ui",
    "crates/compositor",
    "crates/shell-panel",
    "crates/shell-dock",
    "crates/shell-launcher",
]

[workspace.dependencies]
tiny-skia = "0.11"
resvg = "0.44"
usvg = "0.44"
zbus = "5"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
```
