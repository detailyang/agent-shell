# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

---

## [0.1.0] — 2026-05-20

Initial public release.

### Added

#### Core

- **PTY session management** via `portable-pty`: create, destroy, resize PTY sessions
  with configurable terminal geometry (rows/cols) and ring-buffer size.
- **Ring buffer** (`RingBuffer`): fixed-capacity circular output buffer (default 512 KB)
  with per-client monotonic cursors; reports `gap` and `lost_bytes` on overflow.
- **Terminal emulator** (`TermEmulator`): Alacritty-backed VTE parser for accurate
  screen-state snapshots (`read --screen`, attach handshake full-redraw).
- **Prompt detection** — three-layer strategy:
  1. Regex match against buffered output
  2. Child process exit
  3. `tcgetpgrp()` foreground process group + output stable for 150 ms
- **Session recording**: NDJSON event log (header + timestamped base64 events) written
  to `~/.agent-shell/recordings/`; controlled per-session or via `record_by_default`.
- **Recording replay** (`replay`): TUI playback with configurable speed multiplier,
  NDJSON dump mode, and `q`/`Ctrl-C` interrupt.
- **SGR mouse input** (`mouse`): click, scroll, press, release, move, drag with
  interpolated drag steps; full SGR `\x1b[<...M/m` encoding.
- **IPC protocol**: length-prefixed JSON over Unix socket; typed `Request`/`Response`
  with `serde` round-trip; all optional fields use `skip_serializing_if`.
- **Configuration** (`config.toml`): daemon socket path, session defaults, recording
  directory; `AGENT_SHELL_HOME` env override for test isolation.

#### CLI / Daemon

- **Single binary**: daemon mode merged into the CLI binary (`agent-shell daemon`);
  no separate daemon binary required.
- **Auto-start**: daemon spawns automatically on first CLI invocation if not running;
  exponential-backoff socket readiness poll.
- **Subcommands**: `create`, `destroy`, `send`, `read`, `wait`, `set-prompt`, `list`,
  `attach`, `resize`, `mouse`, `replay`, `stop`, `kill-daemon`, `daemon`.
- **`send`**: writes text or control characters (`--ctrl c/d/z/\`) to the PTY, waits
  for prompt detection; `--nowait` skips waiting; `--timeout` configures deadline.
- **`attach`**: two-phase protocol (JSON handshake with full-redraw snapshot →
  raw binary byte stream); read-only by default; `--writable` (`-W`) forwards
  keystrokes; DSR (`ESC[6n`) sequences stripped from snapshot to prevent CPR echo.
- **`destroy` / `attach` interactive picker**: `dialoguer::Select` TUI when
  `--session` is omitted and stdin is a TTY; falls back to plain listing otherwise.
- **`kill-daemon`**: reads PID file, sends `SIGKILL`, reaps zombie, cleans up
  socket and PID artifacts; bypasses socket for unresponsive daemon scenarios.
- **Graceful shutdown**: `SIGTERM`/`SIGINT` handler sets atomic flag; background
  tokio task watches flag and fires `watch::Sender`; server drains active connections.
- **`create --shell`**: `--shell <executable>` passes argv[0] to the daemon;
  no shell argument means the configured `default_program` is used.

#### Tests

- **Unit tests**: `RingBuffer` (read/write/overflow/cursor/gap), `protocol`
  (serde round-trips for all variants), `config` (defaults, file loading,
  `AGENT_SHELL_HOME` isolation), `mouse` (SGR encoding for all actions).
- **Integration tests** (`agent-shell-e2e`): real daemon process spawned per test
  suite; covers create/destroy lifecycle, send/echo/whoami/pwd, prompt detection,
  multiple clients, session recording, recording isolation, replay correctness,
  vim interaction, mouse events, attach render edge cases, and resize behavior.

### Architecture decisions

- Daemon merged into CLI binary (single artifact, simpler distribution).
- `VteGrid` replaced by Alacritty's `alacritty_terminal` for accurate multi-cell
  character and SGR attribute handling.
- Attach uses raw PTY bytes (base64 in handshake snapshot; raw stream thereafter)
  instead of VTE-grid text redraw, preserving colors and TUI state faithfully.
- Ring buffer uses monotonic `write_cursor` (not byte index) so clients can detect
  data loss without storing the previous read position modulo capacity.

---

[Unreleased]: https://github.com/your-org/agent-shell/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/your-org/agent-shell/releases/tag/v0.1.0
