# agent-shell

> **English** | [中文](#中文)

A high-performance PTY session manager for AI agents and automation pipelines, written in Rust.

`agent-shell` replaces the fragile `tmux send-keys / sleep / capture-pane` pattern with a
daemon-backed CLI that exposes every operation as a single, JSON-output command with
deterministic timeouts.

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
- [中文](#中文)

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

---

---

# 中文

> [English](#agent-shell) | **中文**

`agent-shell` 是一个面向 AI agent 和自动化流水线的高性能 PTY 会话管理器，使用 Rust 编写。

它用守护进程（daemon）+ CLI 的架构替代了脆弱的 `tmux send-keys / sleep / capture-pane` 模式，每个操作只需一条命令，输出为 JSON，超时确定可控。

---

## 目录

- [特性](#特性)
- [架构](#架构)
- [要求](#要求)
- [安装](#安装)
- [快速开始](#快速开始)
- [命令参考](#命令参考)
- [配置](#配置)
- [录制与回放](#录制与回放)
- [贡献指南](#贡献指南)

---

## 特性

| 能力 | 说明 |
|---|---|
| **PTY 会话** | 通过 `portable-pty` 创建完整伪终端，支持任意交互式程序 |
| **Prompt 检测** | 三层机制：regex 匹配 → 进程退出 → `tcgetpgrp()` + 输出稳定 150ms |
| **增量输出** | 按客户端游标读取，不重复全量历史 |
| **鼠标输入** | SGR 鼠标编码：点击、滚动、拖拽、按下/释放、移动 |
| **Attach 模式** | 两阶段（JSON 握手 → 原始字节流）；默认只读，`-W` 开启可写 |
| **录制与回放** | NDJSON 事件日志；TUI 回放，支持倍速与中断 |
| **守护进程自动启动** | 首次 CLI 调用时自动启动 daemon |
| **JSON 输出** | 所有命令输出结构化 JSON；`ok: false` 以非零退出码退出 |
| **交互式选择器** | `attach` / `destroy` 省略 `--session` 时弹出 TUI 选择列表 |

---

## 架构

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
│         Session Manager (tokio 异步)              │
│         Unix Socket Server (tokio)                │
└──────────────────┬────────────────────────────────┘
                   │ Unix socket (~/.agent-shell/daemon.sock)
        ┌──────────┼──────────┐
        │          │          │
   ┌────┴───┐ ┌────┴───┐ ┌───┴────┐
   │ CLI(1) │ │ CLI(2) │ │ attach │
   └────────┘ └────────┘ └────────┘
```

**PTY 输出双路处理：**

```
PTY 输出 ──┬──▶ RingBuffer   ──▶ read / send / wait（增量原始字节）
           └──▶ TermEmulator ──▶ attach 快照 / read --screen
```

### 工作区结构

```
crates/
  cli/    — agent-shell 二进制：子命令、daemon 自动启动、attach 流式传输
  core/   — session、ring buffer、terminal emulator、recording、protocol、config
  e2e/    — 集成测试框架（启动真实 daemon 进程）
```

---

## 要求

- **Rust** 1.80+（Rust 2021 edition）
- **macOS** 12+ 或 **Linux**（glibc 2.17+）
- 不支持 Windows

---

## 安装

### 从源码构建

```bash
git clone https://github.com/your-org/agent-shell.git
cd agent-shell
cargo build --release
cp target/release/agent-shell /usr/local/bin/
```

### Cargo 安装（发布到 crates.io 后）

```bash
cargo install agent-shell
```

---

## 快速开始

```bash
# 创建会话（daemon 未运行时自动启动）
agent-shell create --name myapp

# 发送命令并等待 prompt 返回
agent-shell send --session <ID> "ls -la"

# 读取增量输出
agent-shell read --session <ID>

# 只读 attach（Ctrl-C 退出）
agent-shell attach --session <ID>

# 可写 attach（键盘输入转发到 PTY，Ctrl-C 断开连接）
agent-shell attach --session <ID> --writable

# 销毁会话
agent-shell destroy --session <ID>

# 列出所有会话
agent-shell list

# 停止 daemon
agent-shell stop
```

---

## 命令参考

### `create` — 创建 PTY 会话

```
agent-shell create [OPTIONS]

  --name <NAME>          可读名称
  --shell <SHELL>        启动的可执行文件（默认 /bin/bash）
  --cwd <DIR>            工作目录
  --env <KEY=VAL>...     额外环境变量
  --prompt <REGEX>       Prompt 检测正则
  --rows <N>             终端行数（默认 24）
  --cols <N>             终端列数（默认 80）
  --buffer-size <BYTES>  环形缓冲区大小（默认 524288）
  --record               开启录制
```

---

### `send` — 写入文本并等待 prompt

```
agent-shell send --session <ID> [OPTIONS] [TEXT]

  --ctrl <CHAR>          发送控制字符（c/d/z/\）
  --nowait               不等待 prompt，立即返回
  --timeout <MS>         超时毫秒数（默认 30000）
  --client-id <ID>       增量读取的游标标识
```

---

### `read` — 读取缓冲输出

```
agent-shell read --session <ID> [--screen] [--client-id <ID>]

  --screen               返回当前终端屏幕而非原始字节
```

---

### `wait` — 等待输出中出现 pattern

```
agent-shell wait --session <ID> [OPTIONS] <PATTERN>

  --fixed                PATTERN 视为固定字符串（非正则）
  --timeout <MS>         超时毫秒数（默认 30000）
```

---

### `attach` — 实时流式传输 PTY 输出

```
agent-shell attach [--session <ID>] [-W / --writable]
```

- 省略 `--session` 弹出交互式选择器
- 默认只读；Ctrl-C / Ctrl-D 退出
- `--writable` (`-W`)：键盘输入转发到 PTY；Ctrl-C 断开连接（不转发）

---

### `list` — 列出所有会话

```
agent-shell list
```

---

### `destroy` — 销毁会话

```
agent-shell destroy [--session <ID>]
```

省略 `--session` 弹出交互式选择器（包含已退出的会话）。

---

### `resize` — 调整终端尺寸

```
agent-shell resize --session <ID> --rows <N> --cols <N>
```

---

### `mouse` — 发送鼠标事件

```
agent-shell mouse --session <ID> <ACTION> --x <COL> --y <ROW> [OPTIONS]

ACTION: click | scroll | press | release | move | drag

  --button <left|middle|right>   默认 left
  --direction <up|down>          scroll 必填
  --count <N>                    重复次数（默认 1）
  --to-x <COL>                   拖拽目标列
  --to-y <ROW>                   拖拽目标行
  --steps <N>                    拖拽插值步数（默认 5）
```

---

### `replay` — 回放录制文件

```
agent-shell replay <FILE> [--speed <FACTOR>] [--dump] [--force]

  --speed <FACTOR>   播放倍速（默认 1.0）
  --dump             以 NDJSON 格式打印事件而非渲染
  --force            跳过 TTY 检测
```

按 `q` 或 `Ctrl-C` 中断回放。

---

### `stop` / `kill-daemon`

```bash
agent-shell stop        # 通过 socket 优雅关闭 daemon
agent-shell kill-daemon # SIGKILL 强制终止（daemon 无响应时使用）
```

---

## 配置

配置文件：`~/.agent-shell/config.toml`

```toml
[daemon]
socket_path = ""        # 空 = 使用默认路径 ~/.agent-shell/daemon.sock
auto_start = true       # 首次调用时自动启动 daemon

[session]
default_buffer_size = 524288   # 环形缓冲区大小（字节）
default_program = "/bin/bash"  # 默认启动程序
default_rows = 24
default_cols = 80
default_prompt = ""            # 默认 prompt 正则（空 = 禁用）
record_by_default = false      # 默认开启录制

[recording]
dir = "~/.agent-shell/recordings"  # 录制文件目录
```

**环境变量：**

| 变量 | 作用 |
|---|---|
| `AGENT_SHELL_HOME` | 覆盖基础目录（用于测试隔离） |

---

## 录制与回放

为会话启用录制：

```bash
agent-shell create --name mysession --record
```

录制文件存储在 `~/.agent-shell/recordings/<session-id>.ndjson`，NDJSON 格式：
- **Header**（`dir: "meta"`）：终端尺寸与程序名
- **Event**（`dir: "in"` 或 `"out"`）：带时间戳的 base64 编码字节

回放：

```bash
agent-shell replay ~/.agent-shell/recordings/<session-id>.ndjson
agent-shell replay <file> --speed 2.0   # 2 倍速
agent-shell replay <file> --dump        # 查看原始事件
```

---

## 贡献指南

1. Fork 仓库，创建功能分支。
2. 提交 PR 前运行测试：
   ```bash
   cargo test --workspace
   cargo test -p agent-shell-e2e   # 集成测试（启动真实 daemon）
   ```
3. Commit 遵循 [Conventional Commits](https://www.conventionalcommits.org/)：
   `feat:`、`fix:`、`refactor:`、`test:`、`docs:`、`chore:`
4. CI 必须通过（见 `.github/workflows/`）。
