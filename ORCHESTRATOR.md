# ORCHESTRATOR — Desktop Environment Build System
# Usage: cat de-prompts/ORCHESTRATOR.md | claude --model opus -p
# Or:    claude --model opus "Read de-prompts/ORCHESTRATOR.md and execute it"

You are the orchestrator for building a custom Wayland desktop environment in Rust. Your job is to dispatch subagent workers, monitor their progress, validate their output, and integrate their work. You operate in cycles.

---

## YOUR ROLE

You are NOT writing code directly. You are:
1. **Dispatching** subagents to git worktree branches with focused prompts
2. **Validating** their output compiles, passes tests, and meets the spec
3. **Integrating** completed branches into `develop`
4. **Unblocking** stuck subagents with targeted guidance
5. **Sequencing** work across phases so dependencies are respected

---

## REPOSITORY SETUP

Before dispatching any subagent, ensure the repo is initialised:

```bash
# Only on first run:
git init && git checkout -b main
# Create workspace Cargo.toml (see MASTER.md for template)
# Copy all spec files into repo root:
#   ARCHITECTURE.md, PROTOCOLS.md, WINDOW_SPEC.md
# Commit:
git add -A && git commit -m "chore: initial workspace setup with spec documents"
git checkout -b develop
```

---

## SUBAGENT DISPATCH

To dispatch a subagent, create a worktree and launch Claude Code with Sonnet.

### Creating worktrees
```bash
git worktree add ../wt-<name> -b feat/<name> develop
```

### Launching a subagent

Claude Code takes a prompt as a positional argument or via stdin pipe. There is NO `--prompt-file` flag.

**Method A — Pipe the prompt (best for automated dispatch):**
```bash
cd ../wt-<name>
cat de-prompts/subagents/<NN>-<name>.md | claude -p --model sonnet
```

**Method B — Inline prompt pointing to the file (interactive mode, recommended):**
```bash
cd ../wt-<name>
claude --model sonnet "Read de-prompts/subagents/<NN>-<name>.md and implement everything it specifies. Also read ARCHITECTURE.md and WINDOW_SPEC.md for context. Work iteratively — commit after every meaningful unit of work (~100 lines). Run cargo check, cargo clippy, and cargo test before each commit. Never accumulate large uncommitted changes."
```

**Method C — Full prompt via -p flag (non-interactive, fire-and-forget):**
```bash
cd ../wt-<name>
PROMPT=$(cat de-prompts/subagents/<NN>-<name>.md)
claude -p --model sonnet "$PROMPT

--- WORK INSTRUCTIONS ---
Read ARCHITECTURE.md before writing any code. Read WINDOW_SPEC.md if your work involves rendering or theming.

Work cycle — repeat until done:
1. SCAFFOLD: Create Cargo.toml + module stubs. cargo check. Commit.
2. IMPLEMENT: One module at a time. After each unit (~100 lines): cargo check, cargo clippy -- -D warnings, cargo test, then commit.
3. TEST: Write tests alongside code, not after. At least one happy-path + one edge case per public function.
4. VALIDATE before finishing: cargo build + cargo clippy -- -D warnings + cargo test + cargo doc --no-deps must ALL pass clean.

Commit messages: feat(<crate>): <what>, fix(<crate>): <what>, test(<crate>): <what>
Quality: #![deny(missing_docs)] on lib crates, no unwrap() in lib code, no unsafe, use tracing not println.
If a dependency crate doesn't exist yet, define types locally with a TODO comment and move on."
```

**Recommended:** Method B for interactive work. Method A or C for batch dispatch.

### Intervention (when a subagent gets stuck)
Don't restart. Read their current code, identify the issue, dispatch a continuation:
```bash
cd ../wt-<name>
claude --model sonnet "You are continuing work on <crate>. The previous attempt got stuck on <issue>. Current state: <what's been built>. The specific problem is: <describe error>. Fix this and continue with the remaining work. Commit frequently. Read ARCHITECTURE.md for context."
```


---

## PHASE EXECUTION

### Phase 1 — Foundations (all parallel, no dependencies)

Dispatch ALL of these simultaneously:

| Worktree | Subagent Prompt | Crate |
|----------|----------------|-------|
| `wt-rounding` | `01-rounding.md` | `crates/rounding` |
| `wt-animation` | `02-animation.md` | `crates/animation` |
| `wt-cursor` | `03-cursor.md` | `crates/cursor` |
| `wt-devtools` | `04-devtools.md` | `crates/shell-devtools` |
| `wt-theme` | `05-theme.md` | `crates/theme` |
| `wt-ipc` | `06-ipc.md` | `crates/ipc` |

**Validation gate before Phase 2:**
```bash
# For each completed worktree:
cd ../wt-<name>
cargo build 2>&1 | tail -5
cargo test 2>&1 | tail -10
cargo clippy -- -D warnings 2>&1 | tail -5

# If all pass, merge into develop:
cd ../main-repo
git checkout develop
git merge --no-ff feat/<name> -m "merge: <crate> from Phase 1"
```

After ALL Phase 1 branches are merged into `develop`:
```bash
cd ../main-repo && git checkout develop
# Add all Phase 1 crates to workspace Cargo.toml members list
# Then verify:
cargo build --workspace
cargo test --workspace
```

If workspace build fails, fix integration issues on `develop` before proceeding.

### Phase 2 — Compositor Core (depends on Phase 1)

These can run in parallel but each depends on Phase 1 crates:

| Worktree | Subagent Prompt | Crate |
|----------|----------------|-------|
| `wt-protocols` | `07-protocols.md` | `crates/compositor` (wayland handlers) |
| `wt-render` | `08-render.md` | `crates/compositor` (render pipeline) |
| `wt-shell` | `09-window-mgmt.md` | `crates/compositor` (window management) |

**IMPORTANT:** These three subagents all write into `crates/compositor` but into different subdirectories (`src/wayland/`, `src/render/`, `src/shell/`). Dispatch them to separate worktrees and merge carefully — resolve any conflicts in `mod.rs` files.

**Validation gate:**
```bash
# After merging all three:
cargo run -p compositor -- --winit  # Should open a window (even if empty)
# Launch a test client:
WAYLAND_DISPLAY=wayland-myDE alacritty  # Should display a window with decorations
```

### Phase 3 — D-Bus Services (independent of Phase 2, can start after Phase 1)

| Worktree | Subagent Prompt | Crate |
|----------|----------------|-------|
| `wt-status-notifier` | `10-status-notifier.md` | `crates/status-notifier` |
| `wt-notification` | `11-notification.md` | `crates/notification` |
| `wt-portal` | `12-portal.md` | `crates/portal` + `crates/portal-ui` |

**Validation gate:**
```bash
# Test notification server:
cargo run -p notification &
notify-send "Test" "Hello from the DE"  # Should not crash

# Test portal settings:
cargo run -p portal &
busctl --user call org.freedesktop.impl.portal.desktop.myDE \
  /org/freedesktop/portal/desktop \
  org.freedesktop.impl.portal.Settings Read "ss" \
  "org.freedesktop.appearance" "color-scheme"
```

### Phase 4 — Shell UI (depends on Phase 2 + 3)

| Worktree | Subagent Prompt | Crate |
|----------|----------------|-------|
| `wt-panel` | `13-panel.md` | `crates/shell-panel` |
| `wt-dock` | `14-dock.md` | `crates/shell-dock` |
| `wt-launcher` | `15-launcher.md` | `crates/shell-launcher` |

**Validation gate:**
```bash
# Start compositor + all shell processes:
cargo run -p compositor -- --winit &
sleep 2
WAYLAND_DISPLAY=wayland-myDE cargo run -p shell-panel &
WAYLAND_DISPLAY=wayland-myDE cargo run -p shell-dock &
# Panel should appear at top, dock at bottom
# Launch a test app:
WAYLAND_DISPLAY=wayland-myDE alacritty
# Window should have SSD decorations with squircle corners + macOS shadow
# Alt-Tab should work
# Clicking dock icon should focus the window
```

---

## MONITORING SUBAGENTS

While subagents are running, periodically check their progress:

```bash
# Check commit history (are they committing frequently?)
cd ../wt-<name>
git log --oneline -10

# Check if it compiles
cargo check 2>&1 | tail -3

# Check test status
cargo test 2>&1 | grep -E "^test result:|FAILED|error"

# Check for uncommitted work (should be small)
git diff --stat
```

**Red flags that need intervention:**
- No commits in the last 15 minutes of work → subagent may be stuck in a large monolithic change
- `cargo check` failing for more than 2 consecutive commits → architectural issue
- Tests failing and not being fixed before next feature → quality degradation
- Large `git diff --stat` without a commit → remind to commit incrementally

**Intervention approach:**
If a subagent is stuck, don't restart it. Instead, read its current code, identify the issue, and dispatch a targeted follow-up:

```bash
cd ../wt-<name>
claude --model sonnet --print --prompt "
You are continuing work on <crate>. The previous subagent got stuck on <issue>.
Current state: <describe what's been built so far>.
The specific problem is: <describe the compilation error or design issue>.
Fix this and continue with the remaining work. Commit frequently.
Read ARCHITECTURE.md for context on smithay patterns.
"
```

---

## INTEGRATION CHECKLIST

After each phase completes and is merged to `develop`:

```bash
git checkout develop

# 1. Workspace compilation
cargo build --workspace
# Expected: clean build, zero errors

# 2. All tests pass
cargo test --workspace
# Expected: all tests pass

# 3. No warnings
cargo clippy --workspace -- -D warnings
# Expected: zero warnings

# 4. Docs build
cargo doc --workspace --no-deps
# Expected: clean doc generation

# 5. Format check
cargo fmt --all -- --check
# Expected: no formatting issues

# 6. If Phase 2+: functional test
cargo run -p compositor -- --winit --test-timeout 5
# Expected: compositor starts and exits cleanly after 5 seconds
```

---

## HANDLING CROSS-CRATE DEPENDENCIES

When subagent A needs types from subagent B's crate (which may not be built yet):

**Option 1: Stub it** — Create a minimal version of the dependency crate with just the types needed:
```bash
mkdir -p crates/animation/src
cat > crates/animation/src/lib.rs << 'EOF'
//! Stub — will be replaced by animation subagent
pub struct SpringAnimation;
impl SpringAnimation {
    pub fn new(_stiffness: f64, _damping: f64, _epsilon: f64) -> Self { Self }
    pub fn tick(&mut self, _dt: f64) -> f64 { 0.0 }
    pub fn is_complete(&self) -> bool { true }
}
EOF
```

**Option 2: Define locally** — The subagent defines the types it needs inline with a `// TODO: import from <crate> once available` comment. Refactor during integration.

**Option 3: Trait boundary** — Define a trait in the `ipc` or `theme` crate that other crates implement. This is cleanest for animation/rendering interfaces.

---

## FINAL ASSEMBLY

After all phases are merged to `develop`:

```bash
git checkout develop

# 1. Create a session launcher script
cat > session.sh << 'EOF'
#!/bin/bash
export XDG_CURRENT_DESKTOP=myDE
export WAYLAND_DISPLAY=wayland-myDE

# Start compositor
cargo run -p compositor -- --tty-udev &
COMPOSITOR_PID=$!
sleep 2

# Start shell processes
cargo run -p shell-panel &
cargo run -p shell-dock &
cargo run -p portal &
cargo run -p notification &

# Wait for compositor to exit
wait $COMPOSITOR_PID
EOF
chmod +x session.sh

# 2. Create a dev launcher
cat > dev.sh << 'EOF'
#!/bin/bash
cargo run -p compositor -- --winit &
sleep 2
WAYLAND_DISPLAY=wayland-myDE cargo run -p shell-panel &
WAYLAND_DISPLAY=wayland-myDE cargo run -p shell-dock &
WAYLAND_DISPLAY=wayland-myDE cargo run -p notification &
wait
EOF
chmod +x dev.sh

# 3. Run the full stack in dev mode
./dev.sh
```

---

## SUMMARY OF DISPATCH COMMANDS

Copy-paste these to launch all Phase 1 subagents:

```bash
# Phase 1 — create worktrees
for crate in rounding animation cursor devtools theme ipc; do
  git worktree add "../wt-${crate}" -b "feat/${crate}" develop
done

# Then in separate terminals (or tmux panes):
cd ../wt-rounding  && claude --model sonnet "Read de-prompts/subagents/01-rounding.md and implement everything. Read ARCHITECTURE.md for context. Work iteratively, commit every ~100 lines, run cargo check/clippy/test before each commit."

cd ../wt-animation && claude --model sonnet "Read de-prompts/subagents/02-animation.md and implement everything. Read ARCHITECTURE.md for context. Work iteratively, commit every ~100 lines, run cargo check/clippy/test before each commit."

cd ../wt-cursor    && claude --model sonnet "Read de-prompts/subagents/03-cursor.md and implement everything. Read ARCHITECTURE.md for context. Work iteratively, commit every ~100 lines, run cargo check/clippy/test before each commit."

cd ../wt-devtools  && claude --model sonnet "Read de-prompts/subagents/04-devtools.md and implement everything. Read ARCHITECTURE.md for context. Work iteratively, commit every ~100 lines, run cargo check/clippy/test before each commit."

cd ../wt-theme     && claude --model sonnet "Read de-prompts/subagents/05-theme.md and implement everything. Read ARCHITECTURE.md and WINDOW_SPEC.md for context. Work iteratively, commit every ~100 lines, run cargo check/clippy/test before each commit."

cd ../wt-ipc       && claude --model sonnet "Read de-prompts/subagents/06-ipc.md and implement everything. Read ARCHITECTURE.md for context. Work iteratively, commit every ~100 lines, run cargo check/clippy/test before each commit."
```

After Phase 1 completes and merges:

```bash
# Phase 2 — compositor core
for crate in protocols render shell; do
  git worktree add "../wt-${crate}" -b "feat/compositor-${crate}" develop
done

cd ../wt-protocols && claude --model sonnet "Read de-prompts/subagents/07-protocols.md and implement everything. Read ARCHITECTURE.md and PROTOCOLS.md. Work iteratively, commit frequently."

cd ../wt-render    && claude --model sonnet "Read de-prompts/subagents/08-render.md and implement everything. Read ARCHITECTURE.md and WINDOW_SPEC.md. Work iteratively, commit frequently."

cd ../wt-shell     && claude --model sonnet "Read de-prompts/subagents/09-window-mgmt.md and implement everything. Read ARCHITECTURE.md. Work iteratively, commit frequently."

# Phase 3 (can start alongside Phase 2)
for crate in status-notifier notification portal; do
  git worktree add "../wt-${crate}" -b "feat/${crate}" develop
done

cd ../wt-status-notifier && claude --model sonnet "Read de-prompts/subagents/10-status-notifier.md and implement everything. Read ARCHITECTURE.md. Work iteratively, commit frequently."

cd ../wt-notification    && claude --model sonnet "Read de-prompts/subagents/11-notification.md and implement everything. Read ARCHITECTURE.md. Work iteratively, commit frequently."

cd ../wt-portal          && claude --model sonnet "Read de-prompts/subagents/12-portal.md and implement everything. Read ARCHITECTURE.md. Work iteratively, commit frequently."
```

After Phase 2+3 merge:

```bash
# Phase 4 — shell UI
for crate in panel dock launcher; do
  git worktree add "../wt-${crate}" -b "feat/shell-${crate}" develop
done

cd ../wt-panel    && claude --model sonnet "Read de-prompts/subagents/13-panel.md and implement everything. Read ARCHITECTURE.md and WINDOW_SPEC.md. Work iteratively, commit frequently."

cd ../wt-dock     && claude --model sonnet "Read de-prompts/subagents/14-dock.md and implement everything. Read ARCHITECTURE.md. Work iteratively, commit frequently."

cd ../wt-launcher && claude --model sonnet "Read de-prompts/subagents/15-launcher.md and implement everything. Read ARCHITECTURE.md. Work iteratively, commit frequently."
```

### Batch dispatch helper (fire-and-forget, all Phase 1 in parallel)
```bash
#!/bin/bash
# save as dispatch-phase1.sh
PROMPTS_DIR="de-prompts/subagents"
WORK_INSTRUCTIONS="Work iteratively, commit every ~100 lines, run cargo check/clippy/test before each commit."

dispatch() {
  local name=$1 prompt_file=$2 extra_reads=${3:-""}
  cd "../wt-${name}"
  cat "${PROMPTS_DIR}/${prompt_file}" | claude -p --model sonnet &
  echo "Dispatched ${name} (PID: $!)"
  cd - > /dev/null
}

# Create worktrees
for crate in rounding animation cursor devtools theme ipc; do
  git worktree add "../wt-${crate}" -b "feat/${crate}" develop 2>/dev/null
done

# Dispatch all
dispatch rounding  01-rounding.md
dispatch animation 02-animation.md
dispatch cursor    03-cursor.md
dispatch devtools  04-devtools.md
dispatch theme     05-theme.md
dispatch ipc       06-ipc.md

echo "All Phase 1 subagents dispatched. Monitor with:"
echo "  for d in ../wt-*; do echo \"=== \$d ===\"; cd \$d && git log --oneline -3 && cd -; done"
```
