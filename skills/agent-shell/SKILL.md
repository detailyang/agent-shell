---
name: agent-shell
description: Persistent PTY session manager. Use when you need to run commands in an interactive shell and read their output programmatically — create a session, send commands, read output, attach to terminals. Not for one-shot commands (use Bash directly); use this when you need session persistence, incremental output reading, or interactive attach.
allowed-tools: Bash(agent-shell:*), Bash(agent-shell-cli:*), Bash(agent-shell-daemon:*)
hidden: true
---

# agent-shell

Persistent PTY sessions for AI agents. CLI produces JSON (except `attach` which streams raw bytes). Daemon auto-starts on first use.

```
agent → CLI (JSON) → Unix socket → daemon → PTY sessions
```

## Quick start

```bash
SID=$(agent-shell create --name task | jq -r .session_id)
agent-shell send  --session $SID --timeout 5000 "ls /tmp"   # blocks until prompt returns
agent-shell read  --session $SID --client-id reader         # incremental read
agent-shell destroy --session $SID
```

## Commands

### `create` — new PTY session

```bash
agent-shell create [--name NAME] [--shell SHELL] [--cwd DIR] [--env K=V]... [--prompt REGEX] [--rows N] [--cols N] [--buffer-size N] [--record]
```

→ `{"ok":true,"session_id":"a1b2c3d4","recording":"path"}`

### `send` — text or control char to session

```bash
agent-shell send --session ID [--timeout MS] [--nowait] [--client-id CID] [--ctrl CHAR] [TEXT]
```

Blocks until prompt returns (regex match / fg-pgid back + output stable 150ms / process exits). `--nowait` returns immediately. `--ctrl`: `c`=SIGINT, `d`=EOF, `z`=SIGTSTP, `\`=SIGQUIT.

→ success: `{"ok":true,"seq":3,"output":"...\n","elapsed_ms":142}`
→ timeout: `{"ok":false,"error":"timeout","output":"partial..."}`
→ exited: `{"ok":true,"exited":true,"exit_code":0,"seq":5,"output":"..."}`

### `read` — session output

```bash
agent-shell read --session ID [--client-id CID] [--screen]
```

- No `--client-id`: all buffered output (from ringbuf start).
- With `--client-id`: only new bytes since this client's last read. **Preferred** for streaming.
- `--screen`: VTE-parsed screen rows + cursor position instead of raw text.

### `wait` — block until pattern matches output

```bash
agent-shell wait --session ID PATTERN [--timeout MS] [--fixed] [--client-id CID]
```

Pattern is regex by default. `--fixed` treats it as literal string.

### `attach` — interactive terminal

```bash
agent-shell attach [--session ID] [-W]
```

No `--session` → TUI session picker (↑↓/j/k, Enter, Esc). After handshake, raw binary streaming: keystrokes→PTY, PTY→terminal. Default is **read-only** (Ctrl-C/Ctrl-D exits). `-W` enables writable mode (Ctrl-C detaches, keystrokes forwarded to PTY).

### `mouse` — send mouse event to session

```bash
agent-shell mouse --session ID ACTION --x COL --y ROW [--button left|middle|right] [--direction up|down] [--count N] [--to-x COL] [--to-y ROW] [--steps N]
```

Actions: `click`, `scroll`, `press`, `release`, `move`, `drag`. Coordinates are 1-based (col, row). Events encoded as SGR escape sequences (`CSI < ...`). Fire-and-forget — no readiness detection.

- `click` — press + release (default left button). `--count 2` = double-click.
- `scroll` — requires `--direction up|down`. `--count 3` = 3 scroll events (default 1).
- `press` / `release` / `move` — atomic primitives for manual drag control.
- `drag` — press(x,y) → interpolated moves → release(to-x,to-y). `--steps` controls intermediate points (default 5).

→ `{"ok":true,"session_id":"a1b2c3d4"}`
→ error: `{"ok":false,"error":"x coordinate 81 exceeds terminal width 80"}`

### `list` / `destroy` / `resize` / `set-prompt` / `stop` / `kill-daemon`

```bash
agent-shell list                                        # → {sessions:[{id,name,exited,pid,created_at,recording}]}
agent-shell destroy [--session ID]                     # no --session → TUI picker (includes exited); SIGHUP→SIGKILL→reap→remove
agent-shell resize --session ID --rows N --cols N      # PTY + VTE grid resize
agent-shell set-prompt --session ID [REGEX]            # set/clear prompt regex
agent-shell stop                                       # graceful daemon shutdown (via socket)
agent-shell kill-daemon                                # SIGKILL daemon, clean up files (bypasses socket)
```

### `replay` — replay a recording

```bash
agent-shell replay FILE [--speed F] [--dump] [--force]
```

Plays back a `.jsonl` recording to stdout at `F`× real-time speed (default 1.0). Interruptible at any point:

- **Ctrl-C** (SIGINT) — works in both interactive and scripted contexts.
- **Ctrl-D** or **stdin EOF** — useful when stdin is a pipe; detects the `0x04` byte or a closed pipe.
- **`--dump`** — emit raw bytes without timing; requires `--force` if stdout is a terminal.

## Patterns

**REPL interaction:**
```bash
SID=$(agent-shell create --prompt ">>> " | jq -r .session_id)
agent-shell send --session $SID --timeout 10000 "python3"
agent-shell send --session $SID --timeout 5000 "2+2"
agent-shell send --session $SID --ctrl d
```

**Interrupt long command:**
```bash
agent-shell send --session $SID --nowait "sleep 9999"
sleep 1
agent-shell send --session $SID --ctrl c
agent-shell send --session $SID --timeout 5000 "echo ok"
```

**Stream output via polling:**
```bash
agent-shell send --session $SID --nowait "long-job"
while true; do agent-shell read --session $SID --client-id poll; sleep 3; done
```

**Wait for async event:**
```bash
agent-shell send --session $SID --nowait "tail -f /var/log/app.log"
agent-shell wait --session $SID "ERROR" --timeout 60000
```

**Mouse interaction with TUI:**
```bash
agent-shell send --session $SID --timeout 5000 "vim file.txt"
agent-shell mouse --session $SID click --x 10 --y 5                          # click at col 10, row 5
agent-shell mouse --session $SID scroll --x 1 --y 1 --direction down --count 5  # scroll down 5 ticks
agent-shell mouse --session $SID drag --x 1 --y 3 --to-x 20 --to-y 3        # drag to select text
```

**Record and replay a session:**
```bash
SID=$(agent-shell create --name demo --record | jq -r .session_id)
agent-shell send --session $SID --timeout 5000 "echo hello"
REC=$(agent-shell list | jq -r '.sessions[] | select(.id=="'$SID'") | .recording')
agent-shell destroy --session $SID
agent-shell replay "$REC" --speed 2        # 2× speed; Ctrl-C or Ctrl-D to stop early
```

**Custom env + cwd:**
```bash
agent-shell create --name ci --env NODE_ENV=test --cwd /proj | jq -r .session_id
```

## Key rules

- **All output is JSON.** Gate on `ok` field. Parse with `jq`.
- **`send` blocks until prompt.** No manual polling needed. Use `--nowait` for fire-and-forget.
- **Always use `--client-id` with `read`.** Without it you re-read the entire ring buffer every call.
- **`attach` default is read-only.** Use `-W` to forward keystrokes; Ctrl-C then detaches (not sent to PTY).
- **`destroy` with no `--session` shows a TUI picker** (includes exited sessions). In non-interactive contexts (stdin not a tty), lists sessions to stderr and exits 0 without destroying anything.
- **`replay` is interruptible.** Ctrl-C, Ctrl-D, and pipe EOF all stop playback cleanly.
- **`destroy` kills the process group.** No orphan processes.
- **Daemon auto-starts.** First CLI command launches it if needed. `kill-daemon` to force-stop.
- **Ring buffer is 512 KB.** Old data overwritten when full. Lagging `--client-id` gets `gap:true,lost_bytes:N`.
- **`mouse` is fire-and-forget.** If the PTY program hasn't enabled mouse mode, sequences are silently ignored.
- **Mouse coordinates are 1-based** and validated against session cols/rows. Out-of-bounds → error.
- **Mouse `--count` max is 100, `--steps` max is 100.** Prevents accidental flooding.
- **`AGENT_SHELL_HOME`** overrides base directory (for testing).
