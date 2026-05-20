# Architecture

## Overview

`agent-shell` gives AI agents and automation pipelines deterministic,
JSON-output access to interactive terminal programs (shells, debuggers,
REPLs, SSH sessions, etc.).

The core problem it solves: existing approaches built on
`tmux send-keys / sleep / capture-pane` rely on guessed sleep durations,
return full output history on every call, and require 3–4 steps per
operation. `agent-shell` replaces this with a daemon that owns every PTY
and exposes a single-command-per-operation CLI with synchronisation based
on OS-level signals rather than sleeps.

---

## System topology

```
┌───────────────────────────────────────────────────────────────┐
│                      agent-shell daemon                        │
│                                                               │
│  ┌──────────────────────────────────────────────────────┐    │
│  │                   AppState (Mutex)                    │    │
│  │                                                       │    │
│  │  sessions: HashMap<id, Session>                       │    │
│  │  clients:  HashMap<client_id, ClientState>            │    │
│  │  pty_output_notify: Arc<Notify>                       │    │
│  └──────────────────────────────────────────────────────┘    │
│                                                               │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐             │
│  │  Session   │  │  Session   │  │  Session   │  ...        │
│  │────────────│  │────────────│  │────────────│             │
│  │ PTY master │  │ PTY master │  │ PTY master │             │
│  │ RingBuffer │  │ RingBuffer │  │ RingBuffer │             │
│  │ TermEmu    │  │ TermEmu    │  │ TermEmu    │             │
│  │ shell_pgid │  │ shell_pgid │  │ shell_pgid │             │
│  │ fg_pgid    │  │ fg_pgid    │  │ fg_pgid    │             │
│  │ prompt_re  │  │ prompt_re  │  │ prompt_re  │             │
│  │ recording? │  │ recording? │  │ recording? │             │
│  └────────────┘  └────────────┘  └────────────┘             │
│                                                               │
│  Background tasks (tokio)                                     │
│  ├── PTY reaper (200 ms tick) — feed() + check_exited()      │
│  └── Client GC (60 s tick)   — evict cursors > 10 min idle   │
│                                                               │
│  Unix Socket Server (tokio UnixListener)                      │
└──────────────────────────────┬────────────────────────────────┘
                               │ ~/.agent-shell/daemon.sock
               ┌───────────────┼───────────────┐
               │               │               │
          ┌────┴───┐      ┌────┴───┐      ┌────┴───┐
          │ CLI(1) │      │ CLI(2) │      │ attach │
          │ agent  │      │ agent  │      │ human  │
          └────────┘      └────────┘      └────────┘
```

### Single-binary design

The daemon is embedded in the same `agent-shell` binary as the CLI.
`agent-shell daemon` starts the server; all other subcommands are
stateless CLI clients. This avoids installation of a separate daemon
binary and simplifies PATH resolution during auto-start.

---

## IPC protocol

Every CLI call connects to the Unix socket, sends one request, and reads
one response. The wire format is **length-prefixed JSON**:

```
┌──────────────────────────────────────────────────────┐
│  4 bytes (big-endian u32)  │  N bytes (JSON body)    │
└──────────────────────────────────────────────────────┘
```

The `attach` command is the only exception: after the JSON handshake
response it transitions the connection into a raw binary byte stream
(see [Attach protocol](#attach-protocol)).

### Request / Response types

`Request` is a tagged enum serialised with `serde`:

| Tag | Purpose |
|---|---|
| `create` | Spawn a new PTY session |
| `destroy` | Kill session and clean up |
| `send` | Write text + wait for readiness |
| `read` | Read incremental ring-buffer output |
| `wait` | Block until pattern appears |
| `set_prompt` | Update prompt regex at runtime |
| `list` | Enumerate all sessions |
| `attach` | Stream PTY output (+ optional input) |
| `resize` | Change terminal dimensions |
| `mouse` | Inject SGR mouse event |
| `stop` | Graceful daemon shutdown |

`Response` is a flat struct; unused fields are omitted via
`skip_serializing_if = "Option::is_none"`:

```json
{ "ok": true,  "session_id": "a1b2c3d4", "output": "...", "elapsed_ms": 42 }
{ "ok": false, "error": "timeout", "elapsed_ms": 30000 }
```

Non-zero exit codes (`ok: false`) cause the CLI to exit with code 1,
making the JSON output directly usable in shell pipelines and agent tool
calls.

---

## Session lifecycle

```
create ──▶ Session::new()
              │
              ├─ openpty()          portable-pty allocates PTY pair
              ├─ spawn_command()    child process started on PTY slave
              ├─ tcgetpgrp()        shell_pgid captured (with 500 ms retry)
              ├─ RingBuffer::new()  512 KB circular output buffer
              ├─ TermEmulator::new() alacritty_terminal state machine
              └─ Recording::new()  optional NDJSON file (if --record)
                   │
                   ▼
           [active — PTY reaper feeds output every 200 ms]
                   │
         ┌─────────┴──────────┐
         │                    │
      child exits          destroy
         │                    │
         ▼                    ▼
   exited=Some(code)    kill() → SIGHUP
   recording closed     wait 2 s → force_kill() → SIGKILL
                        reap zombie via portable-pty
                        remove from sessions map
```

### Session fields (abbreviated)

| Field | Type | Purpose |
|---|---|---|
| `id` | `String` (8-char UUID v4 prefix) | Primary key |
| `pty_master` | `Box<dyn MasterPty>` | PTY master handle |
| `pty_reader` | `Box<dyn Read>` | Cloned reader for feed loop |
| `pty_writer` | `Mutex<Box<dyn Write>>` | Guarded writer (send/ctrl) |
| `ringbuf` | `RingBuffer` | Circular output buffer |
| `term` | `TermEmulator` | Full VTE screen state |
| `shell_pgid` | `i32` | Initial shell process group ID |
| `current_fg_pgid` | `i32` | Current foreground process group |
| `prev_fg_pgid` | `i32` | Previous fg pgid (for edge detection) |
| `prompt_regex` | `Option<Regex>` | Optional prompt pattern |
| `send_seq` | `u64` | Monotonically-increasing sequence number |
| `recording` | `Option<Recording>` | NDJSON writer, if active |
| `exited` | `Option<i32>` | Exit code once child terminates |
| `destroying` | `bool` | Guard flag: rejects new ops during teardown |

---

## PTY output dual-path

Every byte read from the PTY master goes through two independent paths:

```
PTY master fd
      │
      ▼
  Session::feed()          (called by PTY reaper task, every 200 ms)
      │
      ├──▶ RingBuffer::write()    raw bytes, monotonic write_cursor
      │         │
      │         └──▶ send / read / wait — incremental byte access
      │
      └──▶ TermEmulator::process() VTE state machine (alacritty_terminal)
                │
                └──▶ attach handshake snapshot
                     read --screen
```

**Why two paths?**

- `RingBuffer` stores raw bytes — what the agent needs for text parsing,
  pattern matching, and incremental diffing. It is the primary output
  source for all agent-facing commands (`send`, `read`, `wait`).
- `TermEmulator` tracks the rendered screen state (cells, colors, cursor
  position, alternate screen). It generates the full-redraw snapshot
  sent when a human `attach`es to an existing session, so the terminal
  shows the correct current screen rather than a blank or partial view.

These two consumers have incompatible requirements and cannot share a
single representation.

---

## Readiness detection (`send`)

`send` writes text to the PTY and then blocks until the session is
"ready" — i.e. the child has finished processing the command. Two
mechanisms run in parallel; the first to fire wins:

### Layer 1 — `tcgetpgrp()`

When a shell executes a command, it forks a child and sets that child as
the foreground process group (`tcgetpgrp()` returns the child's pgid).
When the child exits, the shell reclaims the foreground (`tcgetpgrp()`
returns `shell_pgid`). This transition is a purely kernel-level event —
no shell cooperation is required.

```
bash starts:   tcgetpgrp() == shell_pgid  (100)

send "ls -la":
  bash forks   → tcgetpgrp() == child_pgid (101)   ← command running
  ls exits     → tcgetpgrp() == shell_pgid  (100)   ← ✅ READY
```

`fg_returned_to_shell()` checks:
```rust
self.current_fg_pgid == self.shell_pgid && self.prev_fg_pgid != self.shell_pgid
```

This covers all ordinary shell commands without any configuration.

### Layer 2 — prompt regex

For interactive sub-programs (gdb, python3, psql, …) that never yield
the foreground back to the shell, a configurable regex is matched against
newly-buffered ring-buffer output. The first match fires the ready signal.

```
send "gdb --quiet ./binary":
  tcgetpgrp() stays == gdb_pgid  (102)  ← never returns to shell
  ringbuf output contains "(gdb) "      ← ✅ READY (regex matched)
```

### Layer 3 — child exit

If the child process exits (detected via `portable-pty try_wait()`),
`send` returns with `exited: true`.

### Sequence numbers

Each `send` allocates a monotonically-increasing `seq: u64`. The ready
signal carries the triggering `seq` so that rapid back-to-back `send`
calls cannot misattribute responses:

```
send #1 (seq=1, "hostname") → ready detected → response seq=1
send #2 (seq=2, "ls -la")   → ready detected → response seq=2
```

### Poll interval

The ready poll loop runs at **50 ms** intervals inside the send handler.
The PTY reaper feeds new bytes every 200 ms and fires
`pty_output_notify`, which the send loop also awaits, so effective
latency is sub-200 ms for prompt-detection cases.

---

## Ring buffer

```
capacity = 524288 bytes (512 KB default, min 4096 bytes)

write_cursor  ──────────────────────────────────────────────────▶ u64 (monotonic)
                                                                    never resets

 ┌──────┬──────┬──────┬──────┬──────┬──────┬──────┬──────┐
 │      │ old  │ old  │ data │ data │ data │ data │      │  (circular)
 └──────┴──────┴──────┴──────┴──────┴──────┴──────┴──────┘
         ▲ start (oldest valid byte)
```

Key properties:

- **Monotonic `write_cursor`**: clients store a cursor value, not a byte
  index. When `write_cursor - capacity > client_cursor`, data is gone;
  `read()` returns `gap: true, lost_bytes: N`.
- **Per-client cursors** (`ClientState.read_cursor`): each logical
  client (identified by `--client-id`) has an independent read position,
  so multiple agents can consume the same session output concurrently
  without interfering.
- **Client GC**: `ClientState` entries inactive for more than 10 minutes
  are evicted by a background task.
- **Overflow reporting**: the `overflowed` flag is checked by `send`
  before returning; if set, the response is `ok: false, error:
  "buffer_overflow"`. Callers can increase `--buffer-size` on the next
  `create`.

---

## Terminal emulator (`TermEmulator`)

`TermEmulator` wraps `alacritty_terminal`:

```
raw PTY bytes
     │
     ▼
alacritty_terminal::vte::Processor::advance()
     │
     ▼
alacritty_terminal::Term   (cell grid, SGR attributes, cursor, alt screen)
     │
     ├── screen() → Vec<String>          text snapshot per line (read --screen)
     └── full_redraw_bytes() → Vec<u8>   ANSI escape sequence to repaint terminal
```

`alacritty_terminal` was chosen over a hand-rolled VTE grid because it
correctly handles wide (CJK) characters, SGR color/bold/underline
attributes, cursor-save/restore, and the alternate screen buffer — all
necessary for accurate TUI app snapshots.

---

## Attach protocol

Attach is the only streaming command. It uses the existing socket
connection in two phases:

```
CLI                                  Daemon
 │                                      │
 │── JSON: Attach { session_id, ... } ──▶│
 │                                      │ 1. Resize session to client's terminal
 │                                      │    (via separate socket connection,
 │                                      │     before attach handshake)
 │                                      │ 2. Generate full-redraw snapshot
 │                                      │    (TermEmulator::full_redraw_bytes,
 │                                      │     base64-encoded in Response.output)
 │◀── JSON: Response { ok, output } ────│
 │                                      │
 │  ════════ raw binary stream ══════════│
 │                                      │
 │◀═══ PTY bytes (live output) ═════════│ 3. Ring-buffer tail + live PTY output
 │                                      │    forwarded as raw bytes
 │═══ stdin bytes (writable mode) ══════▶│ 4. Client keystrokes written to PTY
 │                                      │    (only when --writable / -W)
```

**DSR stripping**: `ESC[6n` (Device Status Report) sequences are removed
from the handshake snapshot. Without stripping, the client terminal would
emit a CPR (`ESC[row;colR`) response that, in writable mode, gets
forwarded to the PTY and interpreted as keystrokes.

**Read-only vs. writable**:

| Mode | Stdin | Ctrl-C behaviour |
|---|---|---|
| read-only (default) | ignored (only checked for `0x03`/`0x04` to detach) | detach |
| writable (`-W`) | forwarded raw to PTY | detach (not forwarded) |

The resize-before-attach ordering matters: `attach` immediately enters
raw binary mode, so the resize must happen on a separate connection
first. This ensures TUI programs (vim, htop, …) receive the correct
terminal dimensions from their very first byte of output.

---

## Recording format

Recording files are **NDJSON** (newline-delimited JSON), one object per
line, stored in `~/.agent-shell/recordings/<session-id>.jsonl`.

### Line 1: header

```json
{ "dir": "meta", "ts": 1715600000000, "rows": 24, "cols": 80, "program": "/bin/bash" }
```

### Subsequent lines: events

```json
{ "ts": 1715600000123, "dir": "out", "data": "aGVsbG8K" }
{ "ts": 1715600001500, "dir": "in",  "data": "bHMgLWxhCg==" }
```

| Field | Type | Description |
|---|---|---|
| `ts` | `u64` | Unix timestamp in milliseconds |
| `dir` | `"in"` \| `"out"` | `in` = written to PTY; `out` = PTY output |
| `data` | `String` | Base64-encoded raw bytes |

Events are written and flushed after every PTY read / write so that
data is never silently lost if the daemon crashes.

### Replay

`replay` reads the NDJSON file, reconstructs inter-event delays, and
feeds `out` events through a fresh `TermEmulator` to render the
session to the local terminal. `--speed` scales all delays; `--dump`
prints raw bytes instead of rendering. A `SIGINT` / `q` interrupt exits
cleanly by restoring the original terminal mode via RAII guard.

---

## Mouse input

Mouse events use **SGR encoding** (`\x1b[<{code};{col};{row}M/m`),
which supports coordinates beyond 223 columns (unlike the original X10
encoding).

```
action=click, button=left, x=10, y=5:
  press:   \x1b[<0;10;5M
  release: \x1b[<0;10;5m

action=scroll, direction=down, x=5, y=3:
  \x1b[<65;5;3M

action=drag, from=(1,1) to=(1,10), steps=5:
  motion events interpolated at (1,1), (1,3), (1,5), (1,7), (1,10)
  each encoded as \x1b[<32;col;rowM  (button 0 + 32 = motion code)
```

All events are written directly to the PTY master via `send_raw_bytes`.

---

## Daemon lifecycle

```
First CLI invocation
        │
        ▼
socket exists?
  yes → connect OK? → proceed
  yes → connect fail → remove stale socket → spawn daemon
  no  → spawn daemon
        │
        ▼
   daemon spawns self with "daemon" subcommand
   exponential backoff: 100 ms → 200 ms → 400 ms → 800 ms → 1600 ms
        │
        ▼
daemon run():
  create ~/.agent-shell/     (mode 0700)
  write  ~/.agent-shell/daemon.pid
  bind   ~/.agent-shell/daemon.sock  (mode 0700)
  install SIGTERM / SIGINT handlers (atomic flag)
  spawn  PTY reaper task (200 ms tick)
  spawn  client GC task  (60 s tick)
  spawn  shutdown watcher task (polls atomic flag, fires watch::Sender)
  accept loop (tokio select: accept | shutdown)
        │
        ▼  on shutdown signal:
  SIGHUP all sessions
  sleep 100 ms
  SIGKILL remaining sessions
  remove daemon.sock + daemon.pid
  exit
```

**PID file** (`daemon.pid`) enables `kill-daemon` to force-kill an
unresponsive daemon via SIGKILL without going through the socket.

---

## Concurrency model

The entire `AppState` is protected by a single `tokio::sync::Mutex`.
This is intentional:

- Sessions are not CPU-bound inside the lock — the only work done
  while holding it is ring-buffer reads/writes and hashmap lookups.
- PTY I/O (`feed()`) is called by the background reaper task, not by
  request handlers, so handlers never block waiting for PTY data.
- The `pty_output_notify` `Arc<Notify>` allows `send` / `wait` /
  `attach` handlers to sleep outside the lock and be woken when new
  PTY output arrives, eliminating busy-polling.

A finer-grained lock (per-session RwLock) would increase complexity
without measurable throughput benefit at the expected concurrency level
(tens of sessions, not thousands).

---

## Crate selection rationale

| Crate | Version | Role | Rationale |
|---|---|---|---|
| `portable-pty` | 0.8 | PTY alloc + child spawn | WezTerm-grade, cross-platform, `Send`-safe handles |
| `alacritty_terminal` | 0.26 | VTE screen emulation | Production-hardened; handles wide chars, SGR, alt screen |
| `tokio` | 1 | Async runtime, Unix socket server | Ecosystem standard; `UnixListener`, `AsyncFd`, timers |
| `serde` + `serde_json` | 1 | IPC serialisation, recording format | Zero-friction JSON in every agent framework |
| `nix` | 0.29 | `tcgetpgrp`, `waitpid`, signals | Only reliable POSIX binding for process-group APIs |
| `clap` | 4 | CLI argument parsing | Derive-based, zero boilerplate |
| `uuid` | 1 | Session ID generation (v4) | Collision-free 128-bit IDs; 8-char prefix for display |
| `regex` | 1 | Prompt detection, `wait` pattern | Standard; compiled once per session |
| `base64` | 0.22 | Recording encoding, attach snapshot | No ambiguity in NDJSON / JSON strings |
| `dialoguer` | 0.12 | Interactive session picker | Minimal TUI dependency, no crossterm dependency pull |
| `shellexpand` | 2 | `~` expansion in config paths | Lightweight, no shell subprocess |
| `toml` | 0.8 | Config file parsing | `serde`-integrated, human-editable |

**Why not `expectrl`?** `expectrl` is a synchronous library built for
scripted expect/send sequences. Bridging it into a `tokio` daemon that
serves multiple concurrent sessions would require `spawn_blocking` on
every send, introducing unnecessary latency and API friction. The
readiness-detection logic (`tcgetpgrp` + prompt regex) is ~150 lines of
Rust and fits naturally into the async model.

---

## Workspace layout

```
agent-shell/
├── Cargo.toml              workspace manifest; shared dependency versions
├── Cargo.lock
├── .cargo/config.toml      LIBRARY_PATH override for libiconv (macOS)
│
├── crates/
│   ├── cli/                agent-shell binary
│   │   └── src/
│   │       ├── main.rs     subcommand dispatch, attach streaming, daemon auto-start
│   │       └── server.rs   tokio Unix socket server, all request handlers
│   │
│   ├── core/               agent-shell-core library (no binary)
│   │   └── src/
│   │       ├── lib.rs      module declarations
│   │       ├── config.rs   Config, DaemonConfig, SessionConfig, RecordingConfig
│   │       ├── protocol.rs Request / Response / SessionInfo (serde types)
│   │       ├── session.rs  Session struct, PTY lifecycle, kill/resize
│   │       ├── ringbuf.rs  RingBuffer (write, read, overflow detection)
│   │       ├── term_emulator.rs  TermEmulator (alacritty_terminal wrapper)
│   │       ├── recording.rs      Recording writer + replay engine
│   │       ├── mouse.rs    SGR mouse event encoding
│   │       └── terminal.rs raw-mode RAII guard, terminal size query
│   │
│   └── e2e/                integration test crate (not a library)
│       └── src/lib.rs      DaemonHandle, start_daemon(), cli_json(), rpc()
│       └── tests/
│           └── integration.rs  all integration tests
│
├── docs/
│   ├── architecture.md     this document
│   └── glossary.md         precise terminology reference
│
└── specs/                  historical design documents (read-only reference)
```

---

## Out of scope

The following are explicit non-goals:

- **Windows support** — PTY and process-group semantics require POSIX.
- **tmux interoperability** — `agent-shell` replaces, not wraps, tmux.
- **Session persistence across daemon restarts** — PTY file descriptors
  and child processes cannot be serialised. Daemon restart means all
  sessions are lost (equivalent to `tmux kill-server`).
- **Window / pane / split concepts** — one session = one PTY.
- **Human-first UX** — the primary consumer is an AI agent; interactive
  human use (`attach`) is a secondary, debugging-oriented feature.
- **Distributed session management** — a single daemon per user is the
  design target.
