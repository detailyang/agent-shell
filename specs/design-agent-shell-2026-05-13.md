# 设计文档: agent-shell

由 /think 生成于 2026-05-13
状态: 草案

## 问题

AI agent 需要与交互式终端程序（SSH、gdb、Python REPL 等）交互。当前方案需要手动拼凑 tmux 命令——send-keys、Enter、sleep、capture-pane——其中 sleep 时长靠猜，输出是全量历史无增量，每个操作需要 3-4 步。这种方式脆弱、低效，且无法保证同步。

agent-shell 用 Rust daemon 替代此方案，持有 PTY session 并暴露 CLI。每个操作一条命令，同步基于 `tcgetpgrp()` + prompt 双重检测（而非 sleep），输出是增量的。

## 不在范围内

- **tmux 兼容性或迁移** — agent-shell 替代 tmux，不与其互操作。
- **Windows 支持** — 仅 macOS + Linux。
- **人类优先 UX** — 主要消费者是 AI agent；人类 attach 是次要场景。
- **跨 daemon 重启的 session 持久化** — daemon 崩溃丢失所有 session（与 `tmux kill-server` 语义相同）。
- **窗口/面板概念** — 一个 session 就是一个 PTY，没有窗口/面板/分割的概念。

## 约束

1. **不依赖 tmux** — 通过 `portable-pty` 直接管理 PTY。
2. **主要消费者是 AI agent** — 输出为 JSON，agent-shell 本身不产生交互式提示。
3. **目标平台: macOS + Linux** — 不支持 Windows。
4. **使用现有可靠的 Rust crate** — 最小化自定义底层代码。
5. **必须使用 daemon 架构** — PTY 文件描述符无法跨进程共享；必须有长驻进程持有它们。
6. **所有操作具有确定性超时** — 任何调用不得无限挂起。

## 假设

1. `portable-pty` 在 macOS 和 Linux 上提供可靠的 PTY spawn/read/write。（WezTerm 生产环境验证。）
2. `tcgetpgrp()` 在 macOS 和 Linux 上可靠反映 PTY 前台进程组变化，可用于检测 shell 命令执行结束。
3. 单个 daemon 进程足够 — 无需分布式 session 管理。
4. Agent 通过 shell 执行（子进程）调用 CLI；Unix socket IPC 的开销可接受（<10ms 每次调用）。
5. 每个 session 默认 512KB 环形缓冲区足以覆盖任何单次 `read` 或 `send` 响应。在 prompt 之间产出 >512KB 的程序超出 agent-shell 正常用途。

## 方案

### 架构

```
┌─────────────────────────────────────────────────────────┐
│                     agent-shell daemon                   │
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐              │
│  │ Session 1│  │ Session 2│  │ Session 3│  ...          │
│  │ PTY fd   │  │ PTY fd   │  │ PTY fd   │              │
│  │ RingBuf  │  │ RingBuf  │  │ RingBuf  │              │
│  │ VTE Grid │  │ VTE Grid │  │ VTE Grid │              │
│  │ FgPgid   │  │ FgPgid   │  │ FgPgid   │              │
│  │ PromptRe │  │ PromptRe │  │ PromptRe │              │
│  │ ExitFlag │  │ ExitFlag │  │ ExitFlag │              │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘              │
│       │              │              │                    │
│  ┌────┴──────────────┴──────────────┴─────┐             │
│  │         Session Manager (tokio)         │             │
│  └──────────────────┬─────────────────────┘             │
│                     │                                   │
│  ┌──────────────────┴─────────────────────┐             │
│  │      Unix Socket Server (tokio)        │             │
│  └──────────────────┬─────────────────────┘             │
└─────────────────────┼───────────────────────────────────┘
                      │ Unix socket
        ┌─────────────┼─────────────┐
        │             │             │
   ┌────┴────┐  ┌────┴────┐  ┌────┴────┐
   │ CLI (1) │  │ CLI (2) │  │ CLI (3) │
   │ agent   │  │ agent   │  │ attach  │
   └─────────┘  └─────────┘  └─────────┘
```

### 核心：PTY 输出的双路处理

PTY 输出同时写入两路，始终维护：

```
PTY output ──┬──▶ RingBuffer ──▶ send/read (agent, 增量原始字节)
             │
             └──▶ VTE Grid ───▶ attach (人类, 屏幕重绘)
                              └▶ read --screen (当前屏幕快照)
```

- **RingBuffer**：存储原始字节流，服务 `send`/`read` 的增量输出。Agent 直接拿到命令执行结果。
- **VTE Grid**：解析 VT100 转义序列维护屏幕状态（字符矩阵），服务 `attach` 的屏幕重绘和 `read --screen` 的当前屏幕查询。人类 attach 时看到完整屏幕内容而非空白。

### 核心：`send` 的就绪检测机制

`send` 默认等待"对端就绪"后返回。就绪检测采用双重机制，**先触发者胜出**：

| 检测机制 | 信号含义 | 覆盖场景 |
|----------|----------|----------|
| `tcgetpgrp()` 回到 shell pgid | shell 中命令执行完毕，前台进程组回归 | `hostname`、`nvidia-smi`、`gcc`、`ls` |
| `prompt_regex` 匹配 | 交互式子程序就绪，输出可识别的 prompt | `gdb`、`python3`、`psql` |

**序列号消歧**：每个 `send` 调用分配一个单调递增的 `seq: u64`。就绪信号携带触发它的 `seq`，确保连续快速发送时每个返回精确归属于对应的命令。

```
send #1 (seq=1, "hostname")
send #1 返回 → fg_pgid 回归，seq=1 ✅
send #2 (seq=2, "ls -la")
send #2 返回 → fg_pgid 回归，seq=2 ✅
```

如果 `send` 轮询检测到就绪信号但其 `seq` 与当前等待的 `seq` 不匹配（理论上不应发生，因为 `send` 串行等待），daemon 将此视为内部错误并返回 `error: "internal seq mismatch"`。

**原理**：

PTY slave 关联一个前台进程组。当 bash 执行普通命令时，bash 将子进程设为前台进程组，子进程退出后前台进程组回归 bash。`tcgetpgrp()` 可检测此变化。

```
创建 session:
  bash (pgid=100) 启动 → tcgetpgrp() = 100

send "hostname":
  bash fork → hostname (pgid=101)
  bash 将 pgid=101 设为前台 → tcgetpgrp() = 101   ← 命令开始
  hostname 退出
  bash 恢复前台 → tcgetpgrp() = 100                 ← 命令结束 ✅

send "gdb --quiet ...":
  bash fork → gdb (pgid=102)
  bash 将 pgid=102 设为前台 → tcgetpgrp() = 102   ← 命令开始
  gdb 不退出，持续运行 → tcgetpgrp() = 102         ← 前台不回归
  输出 "(gdb) " → prompt_regex 匹配                ← 交互式程序就绪 ✅
```

**关键收益**：
- **shell 命令不需要配置 prompt** — `tcgetpgrp()` 是纯内核态信号，100% 可靠
- **prompt 检测降级为交互式子程序的专用机制** — 只在 fg pgid 不回归时才依赖它
- **`create` 不再强制要求 prompt** — 纯 shell 命令场景下，`--prompt` 可完全不配置

### 数据流: `send` 命令

```
CLI                          Daemon
  │                             │
  │── send(session, "ls -la") ─▶│
  │                             │ 1. 分配 seq=N，记录当前 fg_pgid 和客户端游标
  │                             │ 2. 将 "ls -la\n" 写入 PTY
  │                             │ 3. 后台 task 持续读取 PTY 输出写入 RingBuf + VTE Grid
  │                             │ 4. 轮询检测就绪信号（携带 seq=N）：
  │                             │    a. tcgetpgrp() 回到 shell pgid → shell 命令完成
  │                             │    b. prompt_regex 匹配 → 交互式程序就绪
  │                             │    c. 子进程退出 → 进程结束
  │                             │ 5. 从 RingBuf 提取增量输出，返回 JSON (seq=N)
  │◀── JSON response ──────────│
  │                             │
```

### 数据流: `attach` 命令

```
CLI (attach rw)               Daemon
  │                             │
  │── attach(session, rw) ─────▶│
  │                             │ 1. 注册为订阅者（开始接收增量）
  │◀── 当前 VTE Grid 屏幕重绘 ─│  2. 发送完整屏幕快照
  │◀── stream: 增量重放 ────────│  3. 重放步骤1和步骤2之间产生的增量输出
  │◀── stream: VTE 实时更新 ───│  4. 后续跟随 PTY 实时输出
  │── 按键 ───────────────────▶│  (stdin raw mode → PTY 写入)
  │                             │
  │  (raw 模式下 Ctrl-C ───────▶│  断开 attach)
```

三步初始化顺序（订阅→快照→重放增量）确保客户端不会看到不一致的首屏。步骤 1 开始接收增量，步骤 2 发送快照，步骤 3 重放快照与当前之间的增量。客户端按序处理：快照建立基线，增量重放填补间隙，然后进入实时流。

### 数据流: `read --screen`

```
CLI                          Daemon
  │                             │
  │── read --screen ───────────▶│
  │                             │ 从 VTE Grid 提取当前屏幕内容
  │◀── JSON response ──────────│  {"screen": ["line1", "line2", ...], "cursor": [row, col]}
  │                             │
```

### Session 状态

```rust
struct Session {
    id: String,                          // UUID v4，如 "a3f1b2c4"
    name: Option<String>,                // 可读名称，如 "ssh-remote"
    pty: Box<dyn PtyMaster + Send>,      // portable-pty master 句柄
    pty_fd: RawFd,                       // master fd，用于 tcgetpgrp() 和 AsyncFd
    ringbuf: RingBuffer<u8>,             // 默认 512KB 输出缓冲区
    vte_grid: VteGrid,                   // VT100 屏幕状态
    write_cursor: u64,                   // 写入 ringbuf 的总字节数
    shell_pgid: pid_t,                   // 初始子进程（shell）的进程组 ID
    current_fg_pgid: pid_t,             // 当前前台进程组 ID
    prompt_regex: Option<Regex>,         // 交互式程序 prompt 正则（可选）
    child_pid: u32,                      // 子进程 PID
    exited: Option<i32>,                 // 进程退出时的退出码
    created_at: Instant,
    cwd: Option<PathBuf>,
    env: HashMap<String, String>,
    rows: u16,
    cols: u16,
    buffer_size: usize,                  // 环形缓冲区大小，默认 524288 (512KB)
    overflowed: bool,                    // 缓冲区溢出标志，send 检查后重置
    shell: String,                       // shell 路径，默认 "/bin/bash"
    send_seq: u64,                       // 下一个 send 序列号，单调递增
    subscribers: Vec<Subscriber>,        // 活跃的 attach 客户端
    recording: Option<Recording>,        // 录制状态（可选）
}

struct ClientState {
    session_id: String,
    read_cursor: u64,                    // 每个客户端在 ringbuf 中的读取位置
}
```

### 输出格式

所有 CLI 输出为 JSON 到 stdout。格式统一：

```json
// 成功
{"ok": true, "seq": 1, "output": "hello\n", "elapsed_ms": 150}

// 成功 + 进程退出
{"ok": true, "seq": 2, "output": "...", "exited": true, "exit_code": 0}

// 错误
{"ok": false, "seq": 3, "error": "timeout", "elapsed_ms": 30000}

// read --screen
{"ok": true, "screen": ["line1", "line2", ...], "cursor": [0, 5]}

// create
{"ok": true, "session_id": "a3f1b2c4", "prompt_detected": null}

// list
{"ok": true, "sessions": [{"id": "a3f1b2c4", "name": "ssh1", ...}, ...]}
```

### 每客户端读取游标

每次 CLI 调用连接 daemon 时，daemon 为每个 session 的每个客户端维护 `ClientState`。调用 `read` 时，只返回 `read_cursor` 到 `write_cursor` 之间的字节，并推进 `read_cursor`。这提供零开销的增量输出，agent 无需解析或 diff。

客户端身份是每次 CLI 调用时生成的随机 UUID，通过 `--client-id` 传递。Daemon 在 10 分钟不活跃后垃圾回收客户端状态。

### 输出环形缓冲区与溢出检测

每个 session 默认有一个 512KB 的 `RingBuffer<u8>`（可通过 `create --buffer-size` 配置，硬性下限 4KB）。PTY 输出写入 ringbuf；满时覆盖最旧字节。`write_cursor` 跟踪已写入的总字节数（单调递增）。客户端的 `read_cursor` 远远落后时可能遇到间隙 — `read` 返回 JSON 中 `gap: true, lost_bytes: N`。

**溢出检测与 `send` 报错**：每个 session 维护一个 `overflowed: bool` 标志。当 ringbuf 写入时发生覆盖（旧数据被淘汰），设置 `overflowed = true`。`send` 在返回前检查此标志：若 `overflowed` 为 true，返回 `{"ok": false, "seq": N, "error": "buffer_overflow", "lost_bytes": N}`，并重置标志。这确保 agent 能感知到缓冲区溢出并采取行动（增大 `--buffer-size` 重建 session，或 `destroy` 后重试）。

**Gap 恢复协议**：当 `read` 返回 `gap: true` 时，agent 应：
1. 接受 `lost_bytes: N` 表示不可恢复的数据丢失。
2. 执行 `read` 将 `read_cursor` 推进到 `write_cursor`（从当前位置继续）。
3. 若需要完整上下文，agent 应 `destroy` 该 session 并重新 `create`。

### VTE Grid

每个 session 维护一个 VTE 终端模拟器（`vte` crate），解析 PTY 输出的 VT100 转义序列，维护屏幕字符矩阵状态（rows × cols）。

- attach 首次连接时，发送完整屏幕内容重绘
- 后续 PTY 输出增量更新 VTE Grid，同时推送给 attach 客户端
- `read --screen` 返回当前 VTE Grid 的可见屏幕内容
- 窗口大小变更时（`resize` 命令），VTE Grid 跟随调整

### Prompt 配置（可选）

`--prompt` 仅在需要与交互式子程序（gdb、python3 等）配合时指定。不指定时，`send` 仅依赖 `tcgetpgrp()` 检测就绪 — 对纯 shell 命令场景足够。

指定 `--prompt` 后，`send` 同时运行双重检测：`tcgetpgrp()` 和 prompt 匹配，先触发者胜出。

Prompt 可通过 `set-prompt` 命令后续动态添加或修改。典型场景：在 bash session 中启动 gdb 后，`set-prompt '^\(gdb\) '`，后续 `send` 就能检测 gdb 就绪。

### 录制与回放

**录制**：每个 session 可选开启录制（`create --record` 或配置文件 `record_by_default = true`）。将 PTY 的输入和输出带时间戳写入 NDJSON 文件：

```jsonl
{"ts": 1715600000123, "dir": "out", "data": "aGVsbG8K"}
{"ts": 1715600001500, "dir": "in", "data": "bHMgLWxhCg=="}
```

- `ts`：Unix 时间戳毫秒
- `dir`：`"in"` (写入 PTY) 或 `"out"` (PTY 输出)
- `data`：Base64 编码的原始字节

录制文件路径：`$HOME/.agent-shell/recordings/<session-id>.jsonl`

**回放**：`agent-shell replay <file>` 读取 NDJSON 文件，重放到 stdout。两种模式：
- **默认模式（终端安全）**：按原始时间间隔输出，非打印字符（VT100 转义序列等）以 `\x1b` 风格转义显示，适合终端直接查看。`--speed 2x` 加速回放。
- **`--dump` 模式（原始字节）**：不按时间间隔，直接输出原始字节到 stdout，仅输出 `dir: "out"` 的事件，忽略 `dir: "in"` 事件。设计用于管道到文件：`replay recording.jsonl --dump > output.raw`。当 stdout 是终端时，`--dump` 打印警告并要求 `--force` 确认。

### 进程退出检测

每个 session 启动一个 tokio task，通过 `waitpid()` 等待子进程退出。当进程退出时：

1. 设置 `session.exited = Some(exit_code)`。
2. 将 PTY 剩余输出排入 ringbuf 和 VTE Grid。
3. 所有等待就绪的 `send` 调用返回 `{"ok": true, "exited": true, "exit_code": N, "output": "..."}`。
4. 所有 attach 订阅者收到退出事件并关闭流。
5. 后续对该 session 的 `send`/`read`/`wait` 返回 `{"ok": false, "error": "session exited", "exit_code": N}`。

### 前台进程组监控

每个 session 的后台 tokio task 周期性（50ms 间隔）调用 `tcgetpgrp(pty_fd)` 获取当前前台进程组 ID，与 `shell_pgid` 对比：

- `current_fg_pgid == shell_pgid` 且之前不同 → 前台回归 shell，命令执行完毕
- `current_fg_pgid != shell_pgid` → 命令/子程序正在运行中

`send` 处理器利用此信息判断就绪，无需主动轮询 — 后台 task 维护 `current_fg_pgid`，`send` 等待其变化即可。

### 配置机制

配置文件路径：`$HOME/.agent-shell/config.toml`。CLI 参数覆盖配置文件，配置文件覆盖默认值。

```toml
[daemon]
socket_path = ""                  # 空字符串表示使用 $HOME/.agent-shell/daemon.sock
auto_start = true

[session]
default_buffer_size = 524288      # 512KB
default_shell = "/bin/bash"       # 默认 shell
default_rows = 24
default_cols = 80
default_prompt = ""               # 默认无 prompt
record_by_default = false

[recording]
dir = "~/.agent-shell/recordings"
```

### Daemon 生命周期

**自动启动**: CLI 调用时，若 socket 文件存在但无进程监听，删除残留 socket 并在后台启动新 daemon（`std::process::Command::new(agent-shell-daemon)`）。CLI 以指数退避重试连接（100ms, 200ms, 400ms，最多 5 次）。

**Socket 路径**: `$HOME/.agent-shell/daemon.sock`。创建时设置权限 0700。可通过配置文件 `daemon.socket_path` 覆盖。

**PID 文件**: `$HOME/.agent-shell/daemon.pid`。用于存活性检测。

**关闭**: daemon 仅在以下情况退出：
- 收到 SIGTERM/SIGINT：向所有子进程发送 SIGHUP，等待 5 秒后 SIGKILL，然后退出。
- `agent-shell stop` 命令：同上。
- 无空闲超时自动退出。daemon 作为基础设施常驻，避免 agent 误判 daemon 状态。

### Crate 选型

| Crate | 版本 | 用途 | 选择理由 |
|-------|------|------|----------|
| `portable-pty` | 0.8 | PTY spawn，master/slave 管理 | WezTerm 生产级质量，跨平台，活跃维护 |
| `tokio` | 1 | 异步运行时，Unix socket，task 调度 | 事实标准，生态丰富 |
| `serde` + `serde_json` | 1 | IPC 序列化 + 录制文件格式 | 标准选择 |
| `regex` | 1 | prompt 模式匹配，`wait` 模式匹配 | 标准选择 |
| `clap` | 4 | CLI 参数解析 | 事实标准，支持 derive 宏 |
| `uuid` | 1 | Session 和客户端 ID 生成 | 标准选择 |
| `nix` | 0.29 | `tcgetpgrp()`、`waitpid()`、信号处理 | 标准 Unix 绑定，`tcgetpgrp()` 的唯一可靠来源 |
| `vte` | 0.13 | VT100 终端解析器，维护屏幕 Grid | GNOME VTE 的 Rust 实现，生产级质量 |
| `toml` | 0.8 | 配置文件解析 | 标准选择 |
| `base64` | 0.22 | 录制文件的二进制编码 | 标准选择 |

**为什么不用 `expectrl`**: expectrl 是同步库，设计用于脚本风格的 expect/send 序列。在服务于多个并发 session 的 tokio daemon 中，用 `spawn_blocking` 包裹会引入延迟和复杂度。我们需要的就绪检测逻辑（`tcgetpgrp()` 轮询 + prompt 正则扫描）约 150 行 Rust，不值得为此引入抽象层不匹配的依赖。

## 关键决策

1. **选择 `tcgetpgrp()` + prompt 双重检测作为 `send` 的就绪机制**，因为 `tcgetpgrp()` 对 shell 命令 100% 可靠且无需任何配置，prompt 检测仅作为交互式子程序的补充。此决策成立的条件：macOS 和 Linux 的 PTY 实现正确维护前台进程组信息（已通过 POSIX 标准和 WezTerm 实践验证）。

2. **选择 PTY 输出双路处理（RingBuffer + VTE Grid）**，因为 RingBuffer 提供增量原始字节（agent 需要），VTE Grid 提供屏幕状态（人类 attach 需要），两者不可互替。此决策成立的条件：VTE 解析器的 CPU 开销在典型 PTY 输出速率下可忽略（<1% 单核）。

3. **选择 `portable-pty` 而非 `expectrl`**，因为 daemon 架构需要异步 I/O 而 expectrl 是同步的。就绪检测逻辑（约 150 行）相比桥接同步/异步的开销微不足道。此决策成立的条件：`portable-pty` 的 master PTY 句柄是 `Send` 且可从 tokio task 中使用（已验证 — 它只是文件描述符的包装）。

4. **选择 JSON 作为统一输出格式**，因为 agent 需要结构化解析输出，JSON 是最通用的格式。此决策成立的条件：JSON 解析在所有 agent 框架中零摩擦。

5. **选择每客户端读取游标而非每 session 快照**，因为游标每客户端 O(1)、每次读取 O(1)、无需复制。此决策成立的条件：512KB ring buffer 足够大，实际使用中间隙极少。

6. **选择 daemon 自动启动而非手动管理**，因为 agent 工作流是脚本驱动的，agent 不应有独立的"启动 daemon"步骤。代价是首次 CLI 调用约 500ms 延迟。此决策成立的条件：自动启动检测（socket 存在 + pid 存活）在 macOS 和 Linux 上可靠。

7. **选择内存 session（无持久化）而非 session 持久化**，因为 daemon 崩溃是异常事件，且 session 状态（PTY fd、子进程）无法序列化。此决策成立的条件：agent 具有幂等的 session 创建逻辑（应该有 — `create` + `send` 易于重放）。

## 失败模式与缓解

| 失败场景 | 缓解措施 | 接受的残余风险 |
|----------|----------|----------------|
| 进程挂起（gdb 停在断点、Python input()） | 所有 `send`/`wait` 有强制性超时（默认 30s，可通过 `--timeout` 配置）。`send --nowait` + `read` 循环是逃逸口。 | Agent 必须处理超时错误；无自动杀死。Agent 可 `destroy` 强制杀死。 |
| 输出丢失（环形缓冲区溢出） | 每个 session 默认 512KB ring buffer，可配置；溢出时 `overflowed` 标志置位，`send` 返回 `error: "buffer_overflow"` 并报告 `lost_bytes`；`read` 返回 `gap: true`；agent 遵循 gap 恢复协议。 | 单条命令产出超过缓冲区容量仍会丢失数据，agent 应调大 `--buffer-size` 或更频繁 `read`。 |
| 并发 CLI 客户端访问同一 session | Session 隔离；daemon 使用 tokio 处理并发 I/O；每客户端游标避免读取干扰。多个读写 attach 客户端都写入 PTY（无锁 — 与 tmux 行为相同）。 | 两个读写 attacher 同时发送产生交错输入。可接受 — 与 tmux 行为一致。 |
| SSH 断连 | 进程退出检测触发；`send`/`read` 返回 `exited: true`；agent 可 `destroy` 后重新 `create`。 | 无自动重连。Agent 必须手动重建 SSH。 |
| Daemon 崩溃 | 所有 session 丢失。Agent 必须重建。下次 CLI 调用自动启动。 | 无持久化。接受 — 与 `tmux kill-server` 等价。 |
| 残留 socket 文件 | 自动启动检测到无监听的 socket 文件，删除后启动新 daemon。 | daemon 正在启动但尚未监听的竞争窗口（通过退避重试处理）。 |
| `tcgetpgrp()` 不可用或不准确 | 极端情况下（如某些容器环境），回退到 prompt 检测。Agent 可显式指定 `--prompt`。 | 容器/特殊环境下 shell 命令的就绪检测降级为 prompt 模式。 |
| 管道/后台命令导致 fg pgid 不变 | `send "ls &"` 后台运行，`tcgetpgrp()` 立即回归 shell，`send` 过早返回。 | 这是正确行为 — 后台命令的确立即"完成"。Agent 应用 `wait` 等待特定输出。 |
| VTE 解析性能瓶颈 | `vte` crate 是零拷贝状态机解析器，典型 PTY 输出速率远低于解析能力。 | 极高输出速率（如 `cat /dev/urandom`）可能导致 VTE 解析延迟，不影响 RingBuffer 写入。 |

## 测试覆盖

### 正常路径

| 测试 | 输入 | 期望结果 |
|------|------|----------|
| 创建 session | `create` 启动 bash | 返回 `ok: true`，包含 session_id |
| 创建 session 并指定 prompt | `create --prompt '^\(gdb\) '` | 返回 `ok: true`，prompt 已设置 |
| 发送 shell 命令 | `create` 后 `send "echo hello"` | 返回 `ok: true`，output 包含命令输出 |
| 发送交互式程序命令 | `create --prompt '^\(gdb\) '` 后 `send "print 1"` | 返回 `ok: true`，output 包含结果 |
| 发送命令不等待 | `send --nowait "y"` | 立即返回，不等待就绪信号 |
| 增量读取 | `send` 后 `read` | 仅返回上次读取后的新字节 |
| 读取屏幕 | `read --screen` | 返回当前 VTE Grid 屏幕内容 |
| 等待模式 | `wait "BUILD OK"` | 阻塞直到模式出现，返回匹配上下文 |
| 发送控制字符 | `send --ctrl c` | 向子进程发送 SIGINT |
| 只读 attach | `attach --readonly` | 流式输出，Ctrl-C 退出 |
| 读写 attach | `attach` | 流式输出，转发按键，Ctrl-C 退出 |
| 销毁 session | `destroy` | 进程被杀，session 被移除 |
| 列出 session | `list` | 返回活跃 session 列表及元数据 |
| 进程退出检测 | bash 中 `send "exit"` | 返回 `exited: true, exit_code: 0` |
| 动态设置 prompt | `set-prompt '^\(gdb\) '` | 后续 `send` 使用 prompt 检测 |
| 自定义缓冲区大小 | `create --buffer-size 1048576` | Session 使用 1MB 缓冲区 |
| 录制 | `create --record` 后 `send "echo hi"` → `destroy` | 录制文件存在且包含输入输出 |
| 回放 | `replay recording.jsonl` | 按时间间隔输出到 stdout |

### 错误分支

| 测试 | 输入 | 期望结果 |
|------|------|----------|
| 对不存在的 session 发送 | `send --session nope "ls"` | 返回 `ok: false, error: "session not found"` |
| 发送超时 | 进程卡住时 `send` | 返回 `ok: false, error: "timeout"` |
| 读取已退出 session | 进程退出后 `read` | 返回 `ok: false, error: "session exited"` |
| 创建时指定无效 prompt 正则 | `create --prompt '[invalid'` | 返回 `ok: false, error: "invalid regex"` |
| 缓冲区大小低于下限 | `create --buffer-size 1024` | 返回 `ok: false, error: "buffer_size below minimum 4096"` |
| 环形缓冲区间隙 | 产出超过缓冲区容量后 `read` | 返回 `ok: true, gap: true, lost_bytes: N` |

### 边界情况

| 测试 | 输入 | 期望结果 |
|------|------|----------|
| 空发送 | `send ""` | 无操作，返回当前读取状态 |
| 无 prompt 且交互式子程序启动 | `create`（无 prompt）后 `send "gdb ..."` | `tcgetpgrp()` 不回归，prompt 无匹配，超时返回 |
| 同一 session 多个并发 send | 两个 CLI 客户端同时 `send` | 均返回；输出交错；无死锁 |
| attach 已退出的 session | 进程退出后 `attach` | 返回 `ok: false, error: "session exited"` |
| Daemon 未运行 + 残留 socket | 旧 socket 文件存在时 CLI 调用 | 自动启动：删除残留 socket，启动 daemon，重试 |
| SSH 密码提示 | `create` 后 `send "ssh host"` 后 `wait "password:"` | `wait` 返回匹配；agent 通过 `send --nowait` 发送密码 |
| 后台命令 | `send "sleep 10 &"` | `tcgetpgrp()` 立即回归，`send` 返回 |

## 待决问题

无。

## 成功标准

在单一自动化测试中复现参考对话的完整流程：

1. `create --name ssh1` → 返回 `ok: true`，session 创建
2. `send "ssh detailyang@172.23.142.121"` → 返回 `ok: true`，就绪检测通过
3. `send "hostname"` → 返回 `ok: true`，output 包含主机名
4. `send "nvidia-smi"` → 返回 `ok: true`，output 包含 GPU 信息
5. `create --name gdb1 --prompt '^\(gdb\) ' --cwd /tmp` → 返回 `ok: true`
6. `send "gcc -g -o test_gdb test_gdb.c"` → 返回 `ok: true`，编译成功
7. `send "gdb --quiet /tmp/test_gdb"` → 返回 `ok: true`，gdb 就绪
8. `send "set pagination off"` → 返回 `ok: true`
9. `send "break main"` → 返回 `ok: true`，output 包含断点信息
10. `send "run"` → 返回 `ok: true`，output 包含断点命中信息
11. `send "print n"` → 返回 `ok: true`，output 包含变量值
12. `send "backtrace"` → 返回 `ok: true`，output 包含调用栈
13. `send "quit"` + `send --nowait "y"` → gdb 退出
14. `destroy --session gdb1` → 返回 `ok: true`
15. ssh1 上 `send "exit"` → 返回 `exited: true`
16. `destroy --session ssh1` → 返回 `ok: true`

每步恰好一条 CLI 调用。无手动 sleep。无需解析全量输出找增量内容。

## 实现步骤

### 步骤 1: 项目骨架

- 初始化 Cargo workspace，包含三个 crate：`agent-shell`（CLI 二进制）、`agent-shell-daemon`（daemon 二进制）、`agent-shell-core`（共享库）。
- `Cargo.toml` 依赖：`portable-pty`、`tokio`（full features）、`serde`、`serde_json`、`regex`、`clap`（derive）、`uuid`（v4）、`nix`、`vte`、`toml`、`base64`。
- 目录结构：
  ```
  agent-shell/
  ├── Cargo.toml          (workspace)
  ├── crates/
  │   ├── cli/            (agent-shell 二进制)
  │   │   ├── Cargo.toml
  │   │   └── src/main.rs
  │   ├── daemon/         (agent-shell-daemon 二进制)
  │   │   ├── Cargo.toml
  │   │   └── src/main.rs
  │   ├── core/           (共享库: Session, RingBuffer, VteGrid, 协议类型)
  │   │   ├── Cargo.toml
  │   │   └── src/lib.rs
  │   └── e2e/            (端到端测试框架)
  │       ├── Cargo.toml
  │       └── src/lib.rs
  ```

### 步骤 2: IPC 协议类型 (`crates/core/src/protocol.rs`)

- 定义 `Request` 枚举，变体：`Create`、`Destroy`、`Send`、`Read`、`Wait`、`SetPrompt`、`List`、`Attach`、`Resize`。
- 定义 `Response` 结构体：`{ ok: bool, data: Value, error: Option<String> }`。
- 定义 `SessionInfo` 结构体：`{ id, name, prompt, exited, exit_code, pid, created_at, buffer_size, recording }`。
- 所有类型派生 `Serialize`、`Deserialize`。
- 单元测试：每种 Request 和 Response 变体的序列化/反序列化往返。

### 步骤 3: 环形缓冲区 (`crates/core/src/ringbuf.rs`)

- 实现 `RingBuffer`，可配置容量（默认 524288 字节 / 512KB），硬性下限 4096 字节（4KB）。
- `write_cursor: u64`，`read(from_cursor: u64) -> (Vec<u8>, bool, u64)`，其中 bool 表示是否存在间隙。
- 写入追加字节，推进 `write_cursor`，满时覆盖最旧数据。
- 读取接受游标参数，返回 `[from_cursor, write_cursor)` 范围的字节，若 `from_cursor` 落后于当前起始位置则标记间隙。
- 单元测试：向 4KB 缓冲区写入 8KB，从游标 0 读取 → 检测到间隙；从较近游标读取 → 无间隙。

### 步骤 4: VTE Grid 封装 (`crates/core/src/vte_grid.rs`)

- 封装 `vte` crate 的 `Vt` 解析器，维护屏幕字符矩阵。
- `VteGrid::new(rows, cols)` 初始化。
- `VteGrid::process(bytes)` 喂入 PTY 输出字节，更新内部屏幕状态。
- `VteGrid::screen() -> Vec<String>` 返回当前可见屏幕的行内容。
- `VteGrid::cursor() -> (usize, usize)` 返回当前光标位置 (row, col)。
- `VteGrid::resize(new_rows, new_cols)` 调整屏幕大小。
- `VteGrid::full_redraw_bytes() -> Vec<u8>` 生成完整屏幕重绘的 VT100 转义序列（用于 attach 首次连接）。
- 单元测试：喂入 `"hello\n"` → `screen()` 返回包含 "hello" 的行；喂入光标移动
转义序列 → 验证光标位置更新。

### 步骤 5: 录制模块 (`crates/core/src/recording.rs`)

- 定义 `RecordingEvent` 结构体：`{ ts: u64, dir: "in"|"out", data: String (base64) }`，派生 `Serialize`。
- `Recording::new(path: PathBuf)` 打开 NDJSON 文件。
- `Recording::record_in(bytes)` / `Recording::record_out(bytes)` 写入带时间戳的事件行。
- `replay(file, speed, dump)` 函数：读取 NDJSON 文件，按时间间隔输出到 stdout。
- 单元测试：录制几条事件 → 回放 → 验证输出内容。

### 步骤 6: 配置模块 (`crates/core/src/config.rs`)

- 定义 `Config` 结构体，字段对应 `$HOME/.agent-shell/config.toml` 的所有配置项。
- `Config::load()` 读取配置文件，不存在则返回默认值。
- `Config::default()` 返回硬编码默认值。
- 单元测试：写入临时配置文件 → 加载 → 验证字段值。

### 步骤 7: Session 管理 (`crates/core/src/session.rs`)

- 实现 `Session` 结构体，字段如方案部分所定义。
- 实现 `Session::new()`：通过 `portable-pty::native_pty_system().openpty()` 生成 PTY，设置窗口大小，以指定 cwd/env 启动子进程。记录 `shell_pgid` 为初始子进程的进程组 ID。初始化 RingBuffer 和 VteGrid。
- 实现 `Session::feed()`：非阻塞读取 PTY master fd 的可用输出，同时写入 ringbuf 和 vte_grid。若录制开启，调用 `recording.record_out()`。
- 实现 `Session::send_text()`：向 PTY master 写入字符串 + `\n`。若录制开启，调用 `recording.record_in()`。
- 实现 `Session::send_ctrl()`：向 PTY master 写入控制字节。若录制开启，调用 `recording.record_in()`。
- 实现 `Session::check_fg_pgid()`：调用 `tcgetpgrp(pty_fd)` 获取当前前台进程组，与 `shell_pgid` 对比。
- 实现 `Session::check_exited()`：轮询子进程 PID 的 `try_wait()`。
- 单元测试：启动 `bash --norc --noprofile`，验证 `shell_pgid` 获取正确，发送 `echo hello`，`check_fg_pgid()` 检测到回归，读取输出。

### 步骤 8: Daemon socket 服务器 (`crates/daemon/src/server.rs`)

- 启动时加载配置文件，在规范路径创建 Unix socket。
- 接受连接，读取长度前缀 JSON `Request`，分发给处理函数，写入长度前缀 JSON `Response`。
- 使用 `tokio::net::UnixListener` 和 `tokio::io::AsyncReadExt` / `AsyncWriteExt`。
- `SessionManager` 持有 `HashMap<String, Session>` 和 `HashMap<String, ClientState>`。
- 为每个 session 启动两个 tokio task：
  - **PTY 读取 task**：持续从 PTY master fd 读取输出，写入 ringbuf + vte_grid + recording（通过 `AsyncFd` 包装 master fd）。
  - **退出监控 task**：调用 `waitpid()` 等待子进程退出。
- 后台 task 同时维护 `current_fg_pgid`（周期性 50ms 调用 `tcgetpgrp()`）。
- 集成测试：启动 daemon，通过 Unix socket 连接，发送 `Create` 请求，接收包含 session 信息的 `Response`。

### 步骤 9: CLI 二进制 (`crates/cli/src/main.rs`)

- 使用 `clap` derive 定义子命令：`create`、`destroy`、`send`、`read`、`wait`、`set-prompt`、`list`、`attach`、`resize`、`replay`、`stop`。
- 每个子命令连接 daemon Unix socket，发送 `Request`，将 `Response` 以 JSON 格式输出到 stdout。
- 自动启动逻辑：连接被拒绝时，检查 socket 路径是否存在，尝试启动 `agent-shell-daemon` 二进制（搜索 `PATH` 或 CLI 同目录），以退避方式重试最多 5 次。
- `--timeout` 标志用于 `send` 和 `wait`（默认 30s）。
- `--client-id` 标志用于游标追踪；未提供时自动生成。
- `replay` 子命令直接读取录制文件，不连接 daemon。
- 单元测试：CLI `--help` 输出包含所有子命令。
- 集成测试：完整往返 `create` → `send "echo hi"` → `destroy`，配合运行中的 daemon。

### 步骤 10: `send` 处理器含双重就绪检测

- 在 daemon 中，`send` 处理器：
  1. 分配 seq=N（递增 session.send_seq），记录当前 `current_fg_pgid` 和客户端游标。
  2. 向 PTY 写入文本 + `\n`。
  3. 进入轮询循环（间隔 50ms），检测三个信号（均携带 seq=N）：
     - `current_fg_pgid == shell_pgid` 且之前不同 → shell 命令完成 ✅
     - `prompt_regex` 匹配 ringbuf 新增输出 → 交互式程序就绪 ✅
     - 子进程退出 → 进程结束 ✅
  4. 从 RingBuf 提取增量输出，推进客户端游标。若 `overflowed` 标志为 true，返回 `{"ok": false, "seq": N, "error": "buffer_overflow", "lost_bytes": N}` 并重置标志；否则返回 `{"ok": true, "seq": N, "output": "...", "elapsed_ms": N}`。
  5. 超时：返回部分输出，`{"ok": false, "seq": N, "error": "timeout", "elapsed_ms": N}`。
- `--nowait` 标志：跳过就绪检测，写入后立即返回。
- 集成测试：`create` → `send "sleep 0.5 && echo done"` → output 包含 "done"。
- 集成测试：`create --prompt '^\(gdb\) '` → `send "echo test"` → 验证 `tcgetpgrp()` 先于 prompt 触发。

### 步骤 11: `read` 处理器含增量输出

- 返回从客户端 `read_cursor` 到 session `write_cursor` 的字节，JSON 中 `output` 字段。
- 若游标落后于 ringbuf 起始位置，JSON 中 `gap: true, lost_bytes: N`。
- 推进客户端 `read_cursor` 到 `write_cursor`。
- 若进程已退出且无新输出，JSON 中 `exited: true, exit_code: N`。
- `--screen` 标志：从 VTE Grid 读取当前屏幕快照，返回 `screen` 和 `cursor` 字段。
- 集成测试：`create` → `send "echo hello"` → `read` → output 仅包含命令输出。

### 步骤 12: `wait` 处理器含模式匹配

- 阻塞直到 ringbuf 中从客户端游标开始的输出匹配正则或固定字符串。
- 轮询循环，可配置超时（默认 30s）。
- 匹配成功：返回从客户端游标到匹配结束的完整增量输出，推进游标。
- `--fixed` 标志用于字面量字符串匹配（不解释为正则）。
- 集成测试：`create` → `send --nowait "for i in 1 2 3; do echo line$i; sleep 0.2; done"` → `wait "line3"` → output 包含 "line3"。

### 步骤 13: `attach` 处理器含流式输出

- 收到 `attach` 请求时，daemon 按三步初始化：
  1. 注册客户端为订阅者（开始缓存增量）。
  2. 从 VTE Grid 生成完整屏幕重绘序列发送给客户端。
  3. 重放步骤 1 和步骤 2 之间缓存的增量输出。
  之后进入实时流模式。
- 读写 attach：同时从客户端连接读取原始字节写入 PTY master。
- 客户端 CLI 在读写 attach 时进入 raw terminal 模式（使用 `crossterm` 或手动 `termios` 设置）；按 `Ctrl-C` 时拦截（不转发到 PTY）并断开 attach。
- 只读 attach：客户端仅接收流；按 `Ctrl-C` 退出。
- 进程退出时：发送退出事件并关闭连接。
- 集成测试：启动两个 CLI 客户端，一个只读 attach，一个 send；验证只读客户端看到输出。

### 步骤 14: 进程退出检测与清理

- 每个 session 启动 `tokio::task` 调用子进程 PID 的 `waitpid()`。
- 退出时：设置 `session.exited`，排空 PTY 剩余输出到 ringbuf + vte_grid，通知所有等待中的 send/wait 调用者，通知所有 attach 订阅者。关闭录制文件。
- `destroy` 处理器：向子进程发送 SIGHUP，等待 2s 后 SIGKILL，从管理器中移除 session，关闭 PTY master fd，关闭录制文件。
- 集成测试：`create` → `send "exit"` → 验证返回 `exited: true` → `destroy` 成功。

### 步骤 15: 控制字符

- `send --ctrl c` 映射到字节 `0x03`（ETX，SIGINT）。
- `send --ctrl d` 映射到字节 `0x04`（EOT）。
- `send --ctrl z` 映射到字节 `0x1a`（SUB，SIGTSTP）。
- 直接向 PTY master 写入原始字节，不追加换行。
- 始终 `nowait` — 控制字符不等待就绪信号。
- 集成测试：`create` → `send "cat"`（阻塞等待 stdin）→ `send --ctrl c` → 验证 prompt 返回。

### 步骤 16: Daemon 生命周期管理

- 启动时：加载配置文件，以权限 0700 创建 `$HOME/.agent-shell/` 目录，写入 PID 文件。
- 收到 SIGTERM/SIGINT 或 `stop` 命令：优雅关闭（向所有子进程 SIGHUP，等待 5s，SIGKILL，退出）。
- 无空闲超时。daemon 常驻，仅通过信号或 `stop` 命令退出。
- CLI 自动启动：检测连接被拒绝 → 检查残留 socket → 启动 daemon → 以退避重试（100ms × 2^i，最多 5 次）。
- 集成测试：杀死 daemon → 下次 CLI 调用自动启动 → `create` 成功。

### 步骤 17: `resize` 命令

- `resize --session <id> --rows <N> --cols <N>`：调整 PTY 窗口大小，同时更新 VTE Grid。
- 调用 `portable-pty` 的 `MasterPty::resize()` 和 `VteGrid::resize()`。
- 集成测试：`create` → `resize --rows 40 --cols 120` → `read --screen` → 验证屏幕大小。

### 步骤 18: 录制与回放

- `create --record` 开启录制，录制文件路径 `$HOME/.agent-shell/recordings/<session-id>.jsonl`。
- PTY 输入输出事件写入 NDJSON 文件。
- `destroy` 时关闭录制文件。
- `agent-shell replay <file>` 读取 NDJSON 文件，按时间间隔输出到 stdout。
- `--speed` 和 `--dump` 标志。
- 集成测试：`create --record` → `send "echo hi"` → `destroy` → `replay recording.jsonl --dump` → stdout 包含 "hi"。

### 步骤 19: 端到端测试框架 (`crates/e2e/`)

- 独立 crate `agent-shell-e2e`，仅包含集成测试，不作为库发布。
- 提供测试辅助模块：
  - `start_daemon() -> DaemonHandle`：启动 daemon 进程，等待 socket 就绪。
  - `DaemonHandle::cli(args) -> Output`：执行 CLI 命令并返回 stdout/stderr/exit_code。
  - `DaemonHandle::stop()`：发送 SIGTERM 并等待退出。
  - `assert_ok(output, expected_fields)`：断言 CLI 输出 JSON `ok: true` 且包含指定字段。
  - `assert_error(output, expected_error)`：断言 CLI 输出 JSON `ok: false` 且 error 匹配。
- 测试用例覆盖成功标准中的全部 16 步（本地 bash + 本地 gdb），以及错误分支和边界情况。
- `cargo test -p agent-shell-e2e` 运行全部端到端测试。
