# agent-shell

用 Rust 编写的高性能 PTY 会话管理器，专为 AI Agent 和自动化流水线设计。

`agent-shell` 替代了脆弱的 `tmux send-keys / sleep / capture-pane` 模式，提供一个守护进程驱动的 CLI，每个操作都是单条命令、JSON 输出、确定性超时。

> For English documentation, see [README.md](README.md)

---

## 目录

- [功能特性](#功能特性)
- [架构](#架构)
- [环境要求](#环境要求)
- [安装](#安装)
- [快速开始](#快速开始)
- [命令参考](#命令参考)
- [配置](#配置)
- [录制与回放](#录制与回放)
- [贡献指南](#贡献指南)
- [许可证](#许可证)

---

## 功能特性

| 能力 | 说明 |
|---|---|
| **PTY 会话** | 通过 `portable-pty` 提供完整伪终端，支持任意交互式程序 |
| **提示符检测** | 三层机制：正则匹配 → 进程退出 → `tcgetpgrp()` + 输出稳定 150 ms |
| **增量输出** | 每客户端独立环形缓冲区游标，不重复输出完整历史 |
| **鼠标输入** | SGR 鼠标编码：点击、滚动、拖拽、按下/释放、移动 |
| **Attach** | 两阶段协议（JSON 握手 → 原始二进制流），默认只读，`-W` 可写 |
| **录制与回放** | NDJSON 事件日志，TUI 回放支持倍速控制和中断 |
| **自动启动** | 首次调用 CLI 时守护进程自动拉起 |
| **JSON 输出** | 每条命令返回结构化 JSON，`ok: false` 时以非零退出码退出 |
| **交互式选择器** | `attach` / `destroy` 不带 `--session` 时弹出 `dialoguer` TUI 选择器 |

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

**PTY 输出双路径：**

```
PTY output ──┬──▶ RingBuffer  ──▶ read / send / wait  （增量原始字节）
             └──▶ TermEmulator ──▶ attach 快照 / read --screen
```

### 工作区结构

```
crates/
  cli/    — agent-shell 二进制：子命令、守护进程自动启动、attach 流式传输
  core/   — 会话、环形缓冲区、终端模拟器、录制、协议、配置
  e2e/    — 集成测试框架（启动真实守护进程）
```

---

## 环境要求

- **Rust** 1.80+（使用 Rust 2021 edition）
- **macOS** 12+ 或 **Linux**（glibc 2.17+）
- 不支持 Windows

---

## 安装

### 从源码编译

```bash
git clone https://github.com/your-org/agent-shell.git
cd agent-shell
cargo build --release
# 将二进制文件复制到 PATH
cp target/release/agent-shell /usr/local/bin/
```

### Cargo 安装（发布到 crates.io 后可用）

```bash
cargo install agent-shell
```

### 作为 Claude Code Skill 安装

`skills/agent-shell/` 目录包含一个 [Claude Code](https://claude.ai/code) skill，
允许 AI Agent 通过 `Bash` 工具以最小权限 `allowed-tools` 调用 `agent-shell`。

**通过 `npx skills`（推荐）：**

```bash
npx skills add anthropic-cookbook/agent-shell
```

CLI 会提示选择用户级（全局）或项目级作用域。

**手动安装 — 用户级（所有项目可用）：**

```bash
cp -r skills/agent-shell ~/.claude/skills/
```

**手动安装 — 项目级（仅当前项目）：**

```bash
mkdir -p .claude/skills
cp -r skills/agent-shell .claude/skills/
```

安装后重启 Claude Code 或运行 `/skills` 确认 `agent-shell` 出现在 skill 列表中。
Agent 即可通过 `Bash` 工具在 `SKILL.md` 声明的权限集下调用 `agent-shell <subcommand>`。

---

## 快速开始

```bash
# 创建会话（守护进程未运行时自动启动）
agent-shell create --name myapp

# 发送命令并等待提示符
agent-shell send --session <ID> "ls -la"

# 读取增量输出
agent-shell read --session <ID>

# 交互式 attach（只读；Ctrl-C 退出）
agent-shell attach --session <ID>

# 可写模式 attach（按键转发到 PTY）
agent-shell attach --session <ID> --writable

# 销毁会话
agent-shell destroy --session <ID>

# 列出所有会话
agent-shell list

# 停止守护进程
agent-shell stop
```

---

## 命令参考

### `create`

创建新的 PTY 会话。

```
agent-shell create [OPTIONS]

Options:
  --name <NAME>          可读名称
  --shell <SHELL>        启动的可执行文件（默认：/bin/bash）
  --cwd <DIR>            工作目录
  --env <KEY=VAL>...     额外环境变量
  --prompt <REGEX>       提示符检测正则
  --rows <N>             终端行数（默认：24）
  --cols <N>             终端列数（默认：80）
  --buffer-size <BYTES>  环形缓冲区大小（默认：524288）
  --record               启用会话录制
```

**输出：** `{ "ok": true, "session_id": "a1b2c3d4" }`

---

### `send`

向会话写入文本并等待提示符重新出现。

```
agent-shell send --session <ID> [OPTIONS] [TEXT]

Options:
  --ctrl <CHAR>          发送控制字符（c, d, z, \）
  --nowait               不等待提示符，立即返回
  --timeout <MS>         超时时间（毫秒，默认：30000）
  --client-id <ID>       增量读取的游标标识符
```

**输出：** `{ "ok": true, "output": "...", "elapsed_ms": 42 }`

---

### `read`

读取会话的缓冲输出。

```
agent-shell read --session <ID> [--screen] [--client-id <ID>]

  --screen               返回当前终端屏幕内容而非原始字节
```

---

### `wait`

阻塞直到输出中出现指定模式。

```
agent-shell wait --session <ID> [OPTIONS] <PATTERN>

Options:
  --fixed                将 PATTERN 视为固定字符串（非正则）
  --timeout <MS>         超时时间（毫秒，默认：30000）
```

---

### `attach`

将 PTY 实时输出流式传输到当前终端。

```
agent-shell attach [--session <ID>] [-W / --writable]
```

- 省略 `--session` 时打开交互式选择器。
- 默认只读；Ctrl-C / Ctrl-D 退出。
- `--writable`（`-W`）：将按键转发到 PTY；Ctrl-C 退出（不转发）。

---

### `list`

以 JSON 格式打印所有活跃和已退出的会话。

```
agent-shell list
```

---

### `destroy`

终止会话并清理其资源。

```
agent-shell destroy [--session <ID>]
```

省略 `--session` 时打开交互式选择器（包含已退出的会话）。

---

### `resize`

调整会话的终端窗口大小。

```
agent-shell resize --session <ID> --rows <N> --cols <N>
```

---

### `mouse`

向会话发送鼠标事件（SGR 编码）。

```
agent-shell mouse --session <ID> <ACTION> --x <COL> --y <ROW> [OPTIONS]

Actions: click | scroll | press | release | move | drag

Options:
  --button <left|middle|right>   （默认：left）
  --direction <up|down>          scroll 时必填
  --count <N>                    重复次数（默认：1）
  --to-x <COL>                   拖拽目标列
  --to-y <ROW>                   拖拽目标行
  --steps <N>                    拖拽插值步数（默认：5）
```

---

### `replay`

在终端中回放录制文件。

```
agent-shell replay <FILE> [--speed <FACTOR>] [--dump] [--force]

  --speed <FACTOR>   回放速度倍数（默认：1.0）
  --dump             以 NDJSON 格式打印事件而非渲染
  --force            跳过 TTY 检查（非交互模式）
```

---

### `stop`

通过 socket 优雅关闭守护进程。

```
agent-shell stop
```

### `kill-daemon`

通过 PID 文件强制杀死守护进程（SIGKILL）。守护进程无响应时使用。

```
agent-shell kill-daemon
```

---

## 配置

配置文件：`~/.agent-shell/config.toml`（首次运行时以默认值创建）。

```toml
[daemon]
# Unix socket 路径（默认：~/.agent-shell/daemon.sock）
socket_path = ""
# 首次 CLI 调用时自动启动守护进程（默认：true）
auto_start = true

[session]
# 默认环形缓冲区大小，字节（默认：524288 = 512 KB）
default_buffer_size = 524288
# 新会话默认启动的程序（默认：/bin/bash）
default_program = "/bin/bash"
# 默认终端行列数
default_rows = 24
default_cols = 80
# 默认提示符正则（空 = 不启用提示符检测）
default_prompt = ""
# 默认为所有会话启用录制
record_by_default = false

[recording]
# 录制文件存储目录（默认：~/.agent-shell/recordings）
dir = "~/.agent-shell/recordings"
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

录制文件以 NDJSON 格式存储在 `~/.agent-shell/recordings/<session-id>.ndjson`。

每行是以下之一：
- **Header**（`dir: "meta"`）：终端几何信息和程序名称
- **Event**（`dir: "in"` 或 `"out"`）：带时间戳的 base64 编码字节

回放：

```bash
agent-shell replay ~/.agent-shell/recordings/<session-id>.ndjson
agent-shell replay <file> --speed 2.0     # 2 倍速
agent-shell replay <file> --dump          # 查看原始事件
```

回放过程中按 `q` 或 `Ctrl-C` 中断。

---

## 贡献指南

1. Fork 仓库并创建功能分支。
2. 提交 PR 前运行测试：
   ```bash
   cargo test --workspace
   cargo test -p agent-shell-e2e  # 集成测试（启动真实守护进程）
   ```
3. 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/)：
   `feat:`、`fix:`、`refactor:`、`test:`、`docs:`、`chore:`
4. CI 必须通过（见 `.github/workflows/`）。

---

## 许可证

MIT — 详见 [LICENSE](LICENSE)。
