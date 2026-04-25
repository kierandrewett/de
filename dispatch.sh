#!/usr/bin/env bash
# dispatch.sh <crate-name> <prompt-file> [extra-context-file...]
# Renders the full prompt for a subagent and runs claude -p in the worktree.
# Stdout/stderr go to session-logs/<phase>/<crate>.log
set -uo pipefail

CRATE="$1"
PROMPT_FILE="$2"
shift 2
EXTRA_READS=("$@")

CLAUDE_BIN="/home/kieran/.local/share/claude/versions/2.1.119"
REPO_ROOT="/home/kieran/dev/de-prompts"
WORKTREE="/home/kieran/dev/wt-${CRATE}"

if [[ ! -d "$WORKTREE" ]]; then
  echo "ERROR: worktree $WORKTREE does not exist" >&2
  exit 1
fi

EXTRA_INSTR="$(cat <<'EOF'

--- WORK INSTRUCTIONS (orchestrator) ---
You are running NON-INTERACTIVELY in a git worktree. Your CWD is the root of the worktree.
- Read ARCHITECTURE.md before writing any code. It is at the root of this worktree.
- Read WINDOW_SPEC.md if your work involves rendering, theming, or window chrome.
- Read PROTOCOLS.md if your work touches Wayland protocol handlers.
- Create your crate under `crates/<crate-name>/` exactly per the prompt above.
- Do NOT modify the root Cargo.toml — the orchestrator adds workspace members at integration time. Your crate's Cargo.toml stands alone for now (you may use `package.edition = "2021"`).
- Work cycle, repeat until done:
  1. SCAFFOLD: Cargo.toml + module stubs. `cargo check`. Commit.
  2. IMPLEMENT one module at a time. Every ~100 lines: `cargo check`, `cargo clippy -- -D warnings`, `cargo test`, then commit.
  3. TEST as you go. At least one happy-path + one edge case per public function.
- Quality gates before exiting: `cargo build`, `cargo clippy -- -D warnings`, `cargo test`, `cargo doc --no-deps` all clean.
- Quality rules: `#![deny(missing_docs)]` on lib crates, no `unwrap()` in lib code, no `unsafe`, use `tracing` not `println!`.
- Commit messages: `feat(<crate>): <what>`, `fix(<crate>): <what>`, `test(<crate>): <what>`.
- If a dependency crate doesn't exist yet, define types locally with a `// TODO: import from <crate> once available` comment and proceed.
- Do not stop and ask questions. You are autonomous. Make the best decision you can and continue.
EOF
)"

# Render the full prompt
PROMPT="$(cat "$REPO_ROOT/$PROMPT_FILE")"
PROMPT="${PROMPT}${EXTRA_INSTR}"

# Launch claude in the worktree
cd "$WORKTREE"
echo "[$(date -Iseconds)] Dispatching ${CRATE} (prompt=${PROMPT_FILE})"
printf '%s' "$PROMPT" | "$CLAUDE_BIN" \
  -p \
  --model sonnet \
  --permission-mode bypassPermissions \
  --add-dir "$REPO_ROOT"
EXIT=$?
echo "[$(date -Iseconds)] ${CRATE} exited with code ${EXIT}"
exit $EXIT
