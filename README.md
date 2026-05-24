# Zap

> **Snip noisy command output before it hits your AI.**
> 60–90% fewer tokens. Zero quality loss. Runs entirely on-device.

Zap is a high-performance CLI proxy written in Rust. It sits between your AI
coding assistant (Claude, Copilot, Cursor, Gemini, …) and the shell, then
filters, groups, deduplicates, and truncates command output so the AI gets a
compact summary instead of thousands of noisy lines.

```
Without Zap                                    With Zap

AI  --git status-->  shell  -->  git           AI  --git status-->  zap  -->  git
  ^                              |               ^                   |        |
  |  ~2,000 tokens (raw)         |               |   ~200 tokens     | filter |
  +------------------------------+               +-------(filtered)--+--------+
```

---

## Token Savings (real-world session)

| Operation              | Frequency | Raw    | Zap    | Savings |
| ---------------------- | --------- | ------ | ------ | ------- |
| `ls` / `tree`          | 10×       | 2,000  | 400    | -80%    |
| `cat` / `read`         | 20×       | 40,000 | 12,000 | -70%    |
| `grep` / `rg`          | 8×        | 16,000 | 3,200  | -80%    |
| `git status`           | 10×       | 3,000  | 600    | -80%    |
| `git diff`             | 5×        | 10,000 | 2,500  | -75%    |
| `git log`              | 5×        | 2,500  | 500    | -80%    |
| `git add/commit/push`  | 8×        | 1,600  | 120    | -92%    |
| `cargo test`/`npm test`| 5×        | 25,000 | 2,500  | -90%    |
| `pytest`               | 4×        | 8,000  | 800    | -90%    |
| `go test`              | 3×        | 6,000  | 600    | -90%    |
| `docker ps`            | 3×        | 900    | 180    | -80%    |
| **Total**              |           | **~115,000** | **~23,400** | **-80%** |

---

## Installation

### Prerequisites

> **⚠️ Rust toolchain is required.** Zap is a Rust binary you build from source.
> If you don't already have Rust installed, install it first (takes ~2 minutes):

```bash
# Install Rust via rustup (official installer)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Activate it in the current shell (only needed for this terminal session)
source "$HOME/.cargo/env"

# Verify
cargo --version    # should print: cargo 1.xx.x
```

When the rustup installer asks, just press **Enter** to accept the default options. It installs to `~/.cargo` and `~/.rustup` — no `sudo` needed, nothing system-wide.

Already have Rust? Make sure it's reasonably current:
```bash
rustup update stable
```

### Build & install Zap

```bash
git clone https://github.com/bitan-del/zap.git
cd zap
cargo install --path .
```

This compiles Zap in release mode (~1–2 minutes first time) and puts the `zap` binary in `~/.cargo/bin/`. Make sure that's on your `PATH`:

```bash
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.zshrc   # or ~/.bashrc
source ~/.zshrc
```

### Verify

```bash
zap --version       # → zap 0.1.0
zap --help
zap git status      # inside any git repo
```

### Troubleshooting

| Problem | Fix |
|---------|-----|
| `command not found: cargo` | Run `source "$HOME/.cargo/env"` or restart your terminal |
| `command not found: zap` after install | `export PATH="$HOME/.cargo/bin:$PATH"` |
| `cargo install` fails with compiler errors | `rustup update stable` to update Rust |
| Build is slow first time | Normal (~2 min). Subsequent builds are seconds. |

---

## Quick Start

```bash
# Files
zap ls .
zap read src/main.rs
zap grep "pattern" .
zap find "*.rs" .

# Git
zap git status
zap git log -n 10
zap git diff
zap git push           # → "ok main"

# Tests
zap cargo test
zap pytest
zap go test
zap jest / zap vitest

# Build & lint
zap cargo build
zap cargo clippy
zap lint               # ESLint, grouped by rule
zap tsc                # TypeScript errors, grouped by file
zap ruff check
zap golangci-lint run

# Cloud
zap docker ps
zap kubectl pods
zap aws ec2 describe-instances

# Analytics
zap gain               # See token savings stats
zap gain --graph       # ASCII graph (last 30 days)
zap gain --history     # Recent command history
```

---

## How It Works

Zap applies **12 filtering strategies** depending on the command:

| # | Strategy            | Example                                              | Reduction |
| - | ------------------- | ---------------------------------------------------- | --------- |
| 1 | Stats Extraction    | 5000 diff lines → `"3 files, +142/-89"`              | 90–99%    |
| 2 | Error Only          | mixed stdout+stderr → stderr only                    | 60–80%    |
| 3 | Grouping            | 100 lint errors → `"no-unused-vars: 23, semi: 45"`   | 80–90%    |
| 4 | Deduplication       | repeated logs → `"[ERROR] ... (×5)"`                 | 70–85%    |
| 5 | Structure Only      | huge JSON → keys + types, values stripped            | 80–95%    |
| 6 | Code Filtering      | source → strip comments/bodies (level-based)         | 20–90%    |
| 7 | Failure Focus       | 100 tests → only failures                            | 94–99%    |
| 8 | Tree Compression    | flat list → tree with counts                         | 50–70%    |
| 9 | Progress Filtering  | ANSI progress bars → final result                    | 85–95%    |
| 10| JSON/Text Dual Mode | uses tool's `--format json` when available           | 80%+      |
| 11| State Machine       | pytest output → counts + failures                    | 90%+      |
| 12| NDJSON Streaming    | `go test` events → aggregated summary                | 90%+      |

---

## Hook Integration (Claude Code)

For zero-friction usage, install the hook so every `git status` is auto-rewritten
to `zap git status` before execution:

```bash
zap init -g           # Install global hook for Claude Code
```

Then restart Claude Code. After that, the AI's normal shell commands get
filtered transparently — no need to type `zap` yourself.

Hooks are also available for Cursor, Gemini CLI, Copilot, Windsurf, Cline, and
more. Run `zap init --help` for the full list.

---

## Global Flags

```
-v, --verbose          # Increase verbosity (-v, -vv, -vvv)
-u, --ultra-compact    # ASCII icons + inline format (maximum compression)
```

---

## Configuration

Config lives at `~/.config/zap/config.toml` (or `~/Library/Application Support/zap/config.toml` on macOS).

```toml
[hooks]
exclude_commands = ["curl", "playwright"]   # skip rewrite for these

[tee]
enabled = true        # save raw output on failure (default: true)
mode = "failures"     # "failures", "always", or "never"
```

When a command fails, Zap saves the full unfiltered output to a `tee` file
so the AI can read it without re-executing the command.

---

## Performance

- Binary: ~4 MB stripped
- Startup: <10ms cold
- Memory: <5 MB typical
- Filter overhead: 2–15ms depending on strategy

---

## Architecture

- `src/main.rs` — Clap router, command dispatch
- `src/cmds/` — 42 command filter modules (git, cargo, npm, pytest, docker, …)
- `src/core/` — Shared infra (utils, tracking, tee, config, TOML filter engine)
- `src/filters/` — 60+ declarative TOML filter recipes
- `src/hooks/` — Hook installer (`zap init`) for 10+ AI tools
- `src/analytics/` — Token savings reporting (`zap gain`)

---

## Contributing

PRs welcome. New filters are easy to add — see `src/cmds/README.md` for the
step-by-step checklist.

---

## License

Apache License 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE).
