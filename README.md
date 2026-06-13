# agent-shell

A high-performance PTY session manager for AI agents and automation pipelines, written in Rust.

`agent-shell` replaces the fragile `tmux send-keys / sleep / capture-pane` pattern with a
daemon-backed CLI that exposes every operation as a single, JSON-output command with
deterministic timeouts.

> 中文文档请参阅 [README_zh.md](README_zh.md)

---

## Table of Contents

- [Features](#features)
- [Architecture](#architecture)
- [Requirements](#requirements)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Command Reference](#command-reference)
- [Configuration](#configuration)
- [Recording & Replay](#recording--replay)
- [Contributing](#contributing)
- [License](#license)

---

## Features

| Capability | Detail |
|---|---|
| **PTY sessions** | Full pseudo-terminal via `portable-pty`; supports any interactive program |
| **Prompt detection** | Three-layer: regex match → process exit → `tcgetpgrp()` + output-stable 150 ms |
| **Incremental output** | Per-client ring-buffer cursor; no repeated full-history dumps |
| **Mouse input** | SGR mouse encoding: click, scroll, drag, press/release, move |
| **Attach** | Two-phase (JSON handshake → raw binary stream); read-only by default, `-W` for writable |
| **Recording & replay** | NDJSON event log; TUI replay with speed control and interrupt |
| **Auto-start** | Daemon spawns automatically on first CLI invocation |
| **JSON output** | Every command prints a structured JSON response; `ok: false` exits non-zero |
| **Interactive picker** | `attach` / `destroy` without `--session` open a `dialoguer` TUI picker |

---

## Architecture

```
┌───────────────────────────────────────────────────┐
│                  agent-shell daemon                │
│                                                   │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐        │
│  │ Session 1│  │ Session 2│  │ Session 3│  ...    │
│  │ PTY fd   │  │ PTY fd   │  │ PTY fd   │        │
│  │ RingBuf  │  │ RingBuf  │  │ RingBuf  │        │
│  │ TermEmu  │  │ TermEmu  │  │ TermEmu  │        │
│  └──────────┘  └──────────┘  └──────────┘        │
│                                                   │
│         Session Manager (tokio async)             │
│         Unix Socket Server (tokio)                │
└──────────────────┬────────────────────────────────┘
                   │ Unix socket (~/.agent-shell/daemon.sock)
        ┌──────────┼──────────┐
        │          │          │
   ┌────┴───┐ ┌────┴───┐ ┌───┴────┐
   │ CLI(1) │ │ CLI(2) │ │ attach │
   └────────┘ └────────┘ └────────┘
```

**PTY output dual-path:**

```
PTY output ──┬──▶ RingBuffer  ──▶ read / send / wait  (incremental raw bytes)
             └──▶ TermEmulator ──▶ attach snapshot / read --screen
```

### Workspace layout

```
crates/
  cli/    — agent-shell binary: subcommands, daemon auto-start, attach streaming
  core/   — session, ring buffer, terminal emulator, recording, protocol, config
  e2e/    — integration test harness (spawns real daemon)
```

---

## Requirements

- **Rust** 1.80+ (uses Rust 2021 edition)
- **macOS** 12+ or **Linux** (glibc 2.17+)
- Windows is not supported

---

## Installation

### From source

```bash
git clone https://github.com/your-org/agent-shell.git
cd agent-shell
cargo build --release
# Copy binary to PATH
cp target/release/agent-shell /usr/local/bin/
```

### Cargo install (once published to crates.io)

```bash
cargo install agent-shell
```

### Install as a Claude Code skill

The `skills/agent-shell/` directory contains a [Claude Code](https://claude.ai/code) skill
that lets AI agents invoke `agent-shell` through the `Bash` tool with least-privilege
`allowed-tools` restrictions.

**Via `npx skills` (recommended):**

```bash
npx skills add anthropic-cookbook/agent-shell
```

The CLI will prompt you to choose user-level (global) or project-level scope.

**Manual — user-level (available in all projects):**

```bash
cp -r skills/agent-shell ~/.claude/skills/
```

**Manual — project-level (current project only):**

```bash
mkdir -p .claude/skills
cp -r skills/agent-shell .claude/skills/
```

After installing, restart Claude Code or run `/skills` to confirm `agent-shell` appears in the
skill list. The agent can then call `agent-shell <subcommand>` via the `Bash` tool under the
permission set declared in `SKILL.md`.

---

## Quick Start

```bash
# Create a session (daemon auto-starts if not running)
agent-shell create --name myapp

# Send a command and wait for the prompt
agent-shell send --session <ID> "ls -la"

# Read incremental output
agent-shell read --session <ID>

# Attach interactively (read-only; Ctrl-C to detach)
agent-shell attach --session <ID>

# Attach in writable mode (keystrokes forwarded to PTY)
agent-shell attach --session <ID> --writable

# Destroy the session
agent-shell destroy --session <ID>

# List all sessions
agent-shell list

# Stop the daemon
agent-shell stop
```

---

## Command Reference

### `create`

Create a new PTY session.

```
agent-shell create [OPTIONS]

Options:
  --name <NAME>          Human-readable name
  --shell <SHELL>        Executable to launch (default: /bin/bash)
  --cwd <DIR>            Working directory
  --env <KEY=VAL>...     Extra environment variables
  --prompt <REGEX>       Prompt detection regex
  --rows <N>             Terminal rows (default: 24)
  --cols <N>             Terminal columns (default: 80)
  --buffer-size <BYTES>  Ring buffer size (default: 524288)
  --record               Enable session recording
```

**Output:** `{ "ok": true, "session_id": "a1b2c3d4" }`

---

### `send`

Write text to a session and wait for the prompt to reappear.

```
agent-shell send --session <ID> [OPTIONS] [TEXT]

Options:
  --ctrl <CHAR>          Send control character (c, d, z, \)
  --nowait               Do not wait for prompt; return immediately
  --timeout <MS>         Timeout in milliseconds (default: 30000)
  --client-id <ID>       Cursor identifier for incremental reads
```

**Output:** `{ "ok": true, "output": "...", "elapsed_ms": 42 }`

---

### `read`

Read buffered output for a session.

```
agent-shell read --session <ID> [--screen] [--client-id <ID>]

  --screen               Return the current terminal screen instead of raw bytes
```

---

### `wait`

Block until a pattern appears in output.

```
agent-shell wait --session <ID> [OPTIONS] <PATTERN>

Options:
  --fixed                Treat PATTERN as a fixed string (not a regex)
  --timeout <MS>         Timeout in milliseconds (default: 30000)
```

---

### `attach`

Stream live PTY output to your terminal.

```
agent-shell attach [--session <ID>] [-W / --writable]
```

- Omit `--session` to open an interactive picker.
- Read-only by default; Ctrl-C/Ctrl-D to detach.
- `--writable` (`-W`): forwards your keystrokes to the PTY; Ctrl-C detaches (not forwarded).

---

### `list`

Print all active and exited sessions as JSON.

```
agent-shell list
```

---

### `destroy`

Kill a session and clean up its resources.

```
agent-shell destroy [--session <ID>]
```

Omit `--session` to open an interactive picker (includes exited sessions).

---

### `resize`

Resize the terminal window for a session.

```
agent-shell resize --session <ID> --rows <N> --cols <N>
```

---

### `mouse`

Send a mouse event to a session (SGR encoding).

```
agent-shell mouse --session <ID> <ACTION> --x <COL> --y <ROW> [OPTIONS]

Actions: click | scroll | press | release | move | drag

Options:
  --button <left|middle|right>   (default: left)
  --direction <up|down>          Required for scroll
  --count <N>                    Repeat count (default: 1)
  --to-x <COL>                   Drag target column
  --to-y <ROW>                   Drag target row
  --steps <N>                    Drag interpolation steps (default: 5)
```

---

### `replay`

Play back a recording file in your terminal.

```
agent-shell replay <FILE> [--speed <FACTOR>] [--dump] [--force]

  --speed <FACTOR>   Playback speed multiplier (default: 1.0)
  --dump             Print events as NDJSON instead of rendering
  --force            Skip TTY check (non-interactive mode)
```

---

### `stop`

Gracefully shut down the daemon via socket.

```
agent-shell stop
```

### `kill-daemon`

Force-kill the daemon (SIGKILL) via PID file. Use when the daemon is unresponsive.

```
agent-shell kill-daemon
```

---

## Configuration

Config file: `~/.agent-shell/config.toml` (created on first run with defaults).

```toml
[daemon]
# Path to the Unix socket (default: ~/.agent-shell/daemon.sock)
socket_path = ""
# Auto-start daemon on first CLI call (default: true)
auto_start = true

[session]
# Default ring buffer size in bytes (default: 524288 = 512 KB)
default_buffer_size = 524288
# Default program to launch for new sessions (default: /bin/bash)
default_program = "/bin/bash"
# Default terminal rows and columns
default_rows = 24
default_cols = 80
# Default prompt regex (empty = no prompt detection)
default_prompt = ""
# Record all sessions by default
record_by_default = false

[recording]
# Directory to store recording files (default: ~/.agent-shell/recordings)
dir = "~/.agent-shell/recordings"
```

**Environment variables:**

| Variable | Effect |
|---|---|
| `AGENT_SHELL_HOME` | Override base directory (used for test isolation) |

---

## Recording & Replay

Enable recording for a session:

```bash
agent-shell create --name mysession --record
```

Recording files are NDJSON stored in `~/.agent-shell/recordings/<session-id>.ndjson`.

Each line is one of:
- **Header** (`dir: "meta"`): terminal geometry and program name
- **Event** (`dir: "in"` or `"out"`): timestamped base64-encoded bytes

Replay:

```bash
agent-shell replay ~/.agent-shell/recordings/<session-id>.ndjson
agent-shell replay <file> --speed 2.0     # 2× speed
agent-shell replay <file> --dump          # inspect raw events
```

Press `q` or `Ctrl-C` during replay to interrupt.

---

## Contributing

1. Fork the repository and create a feature branch.
2. Run tests before submitting a PR:
   ```bash
   cargo test --workspace
   cargo test -p agent-shell-e2e  # integration tests (spawns real daemon)
   ```
3. Commits follow [Conventional Commits](https://www.conventionalcommits.org/):
   `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`
4. CI must pass (see `.github/workflows/`).

---

## License

MIT — see [LICENSE](LICENSE).
