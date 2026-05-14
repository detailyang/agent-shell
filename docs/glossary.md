# Glossary
> 模糊说法 → 精确术语。与 LLM 对齐用。只记理解错了会导致用错的词。

## 架构

| 说法 | 精确术语 | 不是 |
|---------|---------|------|
| 终端、shell 进程、会话 | **Session** | 不是 session_id（8 字符 UUID，只是标识符） |
| 后台进程、守护进程 | **Daemon** | 不是 CLI（CLI 无状态，每次调用独立） |
| 客户端、读取者 | **Client** | 不是进程、不是连接。是 `--client-id` 标识的独立读取游标 |
| 会话已退出 | **exited** | 不是已销毁。exited 的 Session 仍在内存，可 read 残余输出，但无法 send。需 destroy 才清理 |

## 数据

| 说法 | 精确术语 | 不是 |
|---------|---------|------|
| 缓冲区、输出缓冲 | **RingBuffer** | 不是日志（内存、不持久化）；不是 VteGrid（RingBuffer 存原始字节，VteGrid 存字符位置） |
| 屏幕状态、终端画面 | **VteGrid** | 不保留颜色/粗体/下划线等 SGR 属性；不用于 attach 渲染（attach 直接传 raw bytes） |
| 日志、录制文件 | **Recording** | 不是 RingBuffer（Recording 在磁盘、持久、不会丢失数据） |
| 输出 | **output** | 编码取决于来源：`send`/`read`/`wait` = UTF-8 文本；`attach` 握手 = base64 编码的 raw PTY bytes |

## 操作

| 说法 | 精确术语 | 不是 |
|---------|---------|------|
| 关掉终端、杀掉会话 | **destroy** | 杀单个 Session（SIGHUP→SIGKILL→reap→移除） |
| 关掉 daemon、停服务 | **stop** | 优雅关闭整个 daemon，走 socket |
| 强杀 daemon | **kill-daemon** | SIGKILL daemon 进程，绕过 socket，daemon 无响应时用 |
| 从终端读输出 | **read** | 消费者从 RingBuffer 读（CLI 命令）。不是 feed（PTY→RingBuffer 吸入，daemon 内部） |
| 往终端写命令 | **send** | 写入 PTY 并阻塞等 prompt。不等用 `--nowait` |
| 发 Ctrl-C 等 | **send_ctrl** | `--ctrl c/d/z/\`，发单个控制字节 |
| 连上终端 | **attach** | 默认只读：两阶段（JSON 握手→raw binary 单向流），Ctrl-C/Ctrl-D 退出。`-W` 可写模式：双向流，Ctrl-C 断开连接，不转发到 PTY |
| 命令完成判断 | **Prompt Detection** | 三层：① regex 匹配 ② 进程退出 ③ fg_pgid 回到 shell + 输出稳定 150ms |
