# agently-cli-mcp 设计文档

- 日期：2026-10-20
- 状态：待评审（v2：Go 单二进制 + stdio/HTTP 双传输）
- 仓库：`/home/pys1024/workspace/GoodixKey/agently-cli-mcp`

## 1. 背景与目标

`agently-cli` 是面向 Agent 的邮件命令行工具，负责 OAuth 授权、凭据保管、配额与邮件协议。本项目把它封装成 MCP Server，让 OpenCode、Claude Code/Desktop、Cursor 等支持 MCP 的客户端用原生工具调用完成收发邮件。

**成功标准**

1. 注册一条 MCP 配置即可完成「看最近邮件 → 读正文 → 回信」的完整链路。
2. MCP Server 不接触、不保存任何 OAuth 凭据；授权仍由 `agently-cli auth login` 管理。
3. 默认配置下，任何发信/删除操作都必须经过真人确认，模型无法单方面完成。
4. 产出**单个可执行文件**，覆盖 Windows / Darwin / Linux 的 x86_64 与 arm64 共 6 个平台，无运行时依赖。
5. 同一个二进制同时支持 **stdio** 与 **HTTP** 两种 MCP 传输。
6. 只调用系统中由 npm 安装的 `agently-cli`，**不内嵌、不重新打包** CLI。
7. `agently-cli` 独立升级不会破坏 MCP Server（除输出协议发生破坏性变更）。

**非目标（v1 不做）**

- 不重写 QQ 邮箱 HTTP API，不自己实现 OAuth 或 token 存储。
- 不内嵌 `agently-cli` 二进制，也不替用户安装/升级 CLI。
- 不做本地邮件缓存、数据库、索引或后台常驻服务。
- 不做多账号切换（沿用 CLI 当前账号；只展示 alias 列表）。
- 不做邮件 UI、富文本渲染、HTML 清洗。
- 不通过 MCP 传输附件二进制（只传本地路径）。
- 不内置 TLS 证书管理；HTTP 对外部署由反向代理终止 TLS。

**反目标（即使功能可用也算失败）**

- 模型可以不经真人确认就发出邮件或永久删除邮件。
- 邮件正文、token、凭据进入日志或错误信息。
- 把邮件正文当作可信指令执行，导致提示词注入。
- 为了「单二进制」把 CLI 或 Node 运行时打包进来。
- 在 Windows 上因为 npm `.cmd` shim 无法执行而直接失败。

## 2. 约束与既有事实

| 约束 | 影响 |
|---|---|
| `agently-cli` 当前 1.0.18（npm latest 也是 1.0.18） | 行为和帮助文本以实测为准 |
| 输出统一 JSON envelope `{ok, data}` | 直接解析 stdout，不做屏幕抓取 |
| 首次确认调用 **exit 0** 且 `data.confirmation_required:true` | 不能只看退出码判定成功 |
| 本地参数错误 exit 1 + 纯文本 stderr | 必须支持非 JSON 输出 |
| `+send/+reply/+forward` 支持 `--confirmed`；`+trash/+delete` 不支持 | `never` 模式无法绕过 trash/delete |
| `ctk_` 与完整请求绑定且一次性使用 | 需要保存已确认的规范化参数 |
| 文件参数必须是相对当前目录的路径 | 需要工作目录与路径沙箱 |
| `auth login` 是交互式浏览器 OAuth | 不能作为普通工具阻塞调用 |
| npm 在 Windows 上安装的是 `agently-cli.cmd` shim | Go 不能直接执行 `.cmd`，需要特殊处理 |
| MCP stdio 的 stdout 只能输出 JSON-RPC | 所有日志写 stderr |

## 3. 语言与运行时选型

### 3.1 候选对比

| 方案 | 单二进制 | 6 平台交叉编译 | MCP SDK | stdio + HTTP | 结论 |
|---|---|---|---|---|---|
| **Go** | 原生静态二进制 | **一个 Linux 主机即可交叉编译全部 6 个目标**（`CGO_ENABLED=0`） | 官方 `modelcontextprotocol/go-sdk` v1.8.0 | `StdioTransport` + `NewStreamableHTTPHandler` + `NewSSEHandler` | **选用** |
| Rust | 原生静态二进制 | 需要 cross/musl/各平台 linker，macOS 需 macOS runner | `rmcp` | 支持 | 次选；开发和 CI 成本更高 |
| TypeScript/Node | 需要 Bun compile 或 Node SEA，产物约 90MB 且平台绑定 | 复杂 | 官方 TS SDK 最成熟 | 支持 | 不满足「真正的单二进制」 |
| Java | GraalVM native-image | 复杂、体积大 | 有社区 SDK | 支持 | 不选 |
| C/C++ | 可以 | 需要自建 HTTP/JSON/MCP 栈 | 无成熟 SDK | 工作量大 | 不选 |

### 3.2 决定：Go

- Go 1.25+，模块化，`CGO_ENABLED=0`，纯静态二进制（约 12–18MB）。
- 使用**官方 Go SDK** `github.com/modelcontextprotocol/go-sdk` v1.8.0：
  - `mcp.StdioTransport` 支持 stdio。
  - `mcp.NewStreamableHTTPHandler` 支持 Streamable HTTP；`mcp.NewSSEHandler` 可兼容旧版 SSE 客户端。
  - 新协议用 MRTR（`InputRequests` + `RequestState`）完成确认；旧协议的 `ServerSession.Elicit` 由 SDK 中间件自动桥接。
  - `mcp.AddTool[In, Out]` 从 Go struct 自动生成输入/输出 JSON Schema，并自动校验。
- HTTP 使用标准库 `net/http`，无需框架。
- 子进程管理用 `os/exec`，超时用 `context`。
- 测试用标准库 `testing` + `httptest` + SDK 的 `NewInMemoryTransports()`。

**理由**：六平台单二进制是硬指标，Go 的交叉编译成本最低；官方 SDK 恰好覆盖两种传输和 elicitation；团队维护成本低于 Rust，且不需要为目标平台准备 Node/Bun 运行时。

## 4. 总体架构

```
┌──────────────────────┐        ┌──────────────────────┐
│ stdio 客户端          │        │ HTTP 客户端           │
│ (Claude/OpenCode/…)  │        │ (远程/多会话)         │
└─────────┬────────────┘        └─────────┬────────────┘
          │ JSON-RPC over stdin/stdout     │ POST/GET /mcp
          ▼                                ▼
      ┌──────────────────────────────────────────┐
      │        agently-cli-mcp（单二进制）        │
      │  ┌────────────┐  ┌────────────────────┐  │
      │  │ stdio      │  │ Streamable HTTP    │  │
      │  │ transport  │  │ (+可选 legacy SSE) │  │
      │  └─────┬──────┘  └─────────┬──────────┘  │
      │        └────────┬──────────┘             │
      │        ┌────────▼─────────┐               │
      │        │ Tool Registry    │  Go struct →  │
      │        │ (12 tools)       │  JSON Schema  │
      │        └────────┬─────────┘               │
      │   ┌─────────────▼──────────────┐          │
      │   │ Guard: 路径沙箱/大小/确认/脱敏 │          │
      │   └─────────────┬──────────────┘          │
      │   ┌─────────────▼──────────────┐          │
      │   │ CLI Runner: exec + timeout  │          │
      │   └─────────────┬──────────────┘          │
      └─────────────────┼─────────────────────────┘
                        │ os/exec
                        ▼
           系统 npm 安装的 agently-cli
           （OAuth / keychain / 配额 / 邮件协议）
```

**进程模型**：MCP Server 是一个进程；每次工具调用启动一个短生命周期 `agently-cli` 子进程。无状态、崩溃隔离。HTTP 模式下多个会话共享同一个 Runner 与确认缓存。

## 5. 工具清单

统一使用 `mail_` 前缀。所有工具声明 `title`、`description`（含何时使用）、`annotations`、输入/输出 schema。

| 工具 | 对应 CLI | 读/写 | annotations |
|---|---|---|---|
| `mail_get_account` | `+me` + `auth status` | 读 | readOnly |
| `mail_refresh_auth` | `auth refresh` | 写本地凭据 | idempotent |
| `mail_list_messages` | `message +list` | 读 | readOnly |
| `mail_get_message` | `message +read` | 读（会标记为已读） | `readOnlyHint:false`、`idempotentHint:true` |
| `mail_search_messages` | `message +search` | 读 | readOnly |
| `mail_send_message` | `message +send` | 写（外发） | openWorld |
| `mail_reply_message` | `message +reply` | 写（外发） | openWorld |
| `mail_forward_message` | `message +forward` | 写（外发） | openWorld |
| `mail_move_to_trash` | `message +trash` | 写（可恢复） | destructive |
| `mail_delete_messages` | `message +delete` | 写（不可恢复） | destructive |
| `mail_watch_messages` | `message +watch` | 读（有界长轮询） | readOnly |
| `mail_download_attachment` | `attachment +download` | 写本地 | — |

> 不暴露 `attachment +upload`：`file_id` 没有下游消费者（`--attachment` 只接受本地路径，实测传 `file_id` 报 `unsafe attachment path`），而 `+send --dry-run` 会直接内联文件内容。

> `+read` 会把未读邮件置为已读（实测 `is_read` 由 false 变为 true），不能标注为 `readOnlyHint`；工具描述必须写明。

### 5.1 输入参数

**mail_list_messages**：`dir?`（inbox/sent/trash/spam）、`limit?`（1–50，默认 10）、`cursor?`、`after?`/`before?`（ISO 8601）、`has_attachments?`、`is_unread?`。

**mail_get_message**：`id`（必填，`msg_` 前缀校验）、`include_body?`（默认 true）、`max_body_chars?`（默认 50000，0 表示不限）。副作用：标记为已读。

**mail_search_messages**：`q?`、`search_in?`（SEARCH_IN_ALL/SEARCH_IN_SUBJECT/SEARCH_IN_CONTENT）、`from?`、`to?`、`dir?`、`after?`、`before?`、`has_attachments?`、`is_unread?`、`limit?`、`cursor?`。翻页必须原样保留搜索条件。

**mail_send_message**：`to[]`（必填）、`subject`（必填）、`body?` / `body_file?`（二选一）、`body_format?`（plain/html/markdown）、`cc[]`、`bcc[]`、`attachments[]`、`confirmation_token?`。

**mail_reply_message**：`id`、`body?`/`body_file?`、`reply_all?`、`cc[]`、`bcc[]`、`attachments[]`、`confirmation_token?`。

**mail_forward_message**：`id`、`to[]`、`body`（**必填且非空**：实测缺省或空字符串都会触发 API 400 `ForwardMessageReq.body`）、`cc[]`、`bcc[]`、`include_attachments?`、`attachments[]`、`confirmation_token?`。

**mail_move_to_trash**：`id`、`confirmation_token?`。

**mail_delete_messages**：`id?` 或 `all?`（互斥，必须且只能给一个）、`confirmation_token?`。

**mail_watch_messages**：`timeout_seconds?`（5–120，默认 30）、`msg_format?`（full/event）。

**mail_download_attachment**：`msg`、`att`、`output?`（工作目录内目录，默认 `.`）。

### 5.2 输出格式

成功：

```json
{
  "ok": true,
  "tool": "mail_list_messages",
  "data": { "...": "CLI 原始 data，字段不改名" },
  "meta": { "duration_ms": 142, "cli_version": "1.0.18", "untrusted_content": false }
}
```

失败（同时设置 MCP `isError: true`）：

```json
{
  "ok": false,
  "tool": "mail_search_messages",
  "error": {
    "code": "RATE_LIMITED",
    "exit_code": 7,
    "message": "CLI 原文",
    "retryable": true,
    "retry_after_seconds": 30,
    "hint": "等待后重试，或降低请求频率"
  }
}
```

确认待办（`token` 模式；`isError: false`）。`elicit` 模式不返回 ctk，而是返回 MCP `InputRequests` + `RequestState`，见 6.2 节：

```json
{
  "ok": true,
  "status": "confirmation_required",
  "action": "send",
  "summary": {
    "action": "send",
    "from": "penn1024@agent.qq.com",
    "to": ["a@example.com"],
    "cc": ["c@example.com"],
    "bcc": ["b@example.com"],
    "subject": "周报",
    "attachments": [{ "filename": "report.pdf", "size": 12345 }],
    "body_preview": "前 200 字",
    "cli": { "action": "send", "attachment_count": 1, "from": "penn1024@agent.qq.com", "subject": "周报", "to": ["a@example.com", "c@example.com", "b@example.com"] }
  },
  "confirmation_token": "ctk_...",
  "expires_in": 300,
  "next": "请向用户展示 summary 并等待明确许可，然后用相同参数加 confirmation_token 再次调用"
}
```

同时返回人类可读的 `content` 摘要，便于不解析 structuredContent 的客户端展示。

### 5.3 搜索翻页上下文

`+search` 的 cursor **不携带查询条件**，CLI 帮助明确要求翻页时重复 `--q` 和全部过滤条件。工具是无状态的，模型可能漏传或改条件，导致分页结果错乱。

Server 增加一个 TTL/LRU 缓存：

```go
type SearchCtx struct {
    Q, SearchIn, From, To, Dir string
    After, Before              string
    HasAttachments, IsUnread   bool
    Limit                      int
    SessionID                  string
}
type Store struct { mu sync.Mutex; m map[string]SearchCtx } // key: cursor
```

- 首次搜索返回 `next_cursor` 时，把条件与 cursor 绑定写入，TTL 10 分钟，最多 200 条。
- 后续只带 `cursor` 调用时，自动补全原条件。
- 若同时带了条件且与缓存不一致 → `SEARCH_CONTEXT_MISMATCH`，提示不要修改翻页条件。
- 缓存未命中（重启/过期）且没有完整条件 → `SEARCH_CONTEXT_REQUIRED`，要求重新搜索。
- 会话绑定与确认缓存一致（HTTP 模式）。

## 6. 确认机制

写操作有两层防线：CLI 的 `ctk_` 两阶段机制 + MCP Server 的确认闸门。

### 6.1 实测到的 CLI 确认行为

- 首次调用（不带 token）**退出码 0**、`ok:true`、`data.confirmation_required:true`，返回 `ctk_`（5 分钟有效）。**不能只看退出码。**
- `ctk_` 与完整请求绑定且一次性；请求内容有任何变化（含收件人顺序、正文空白）都会被拒绝（exit 6）。
- 只有 `send/reply/forward` 支持 `--confirmed`；`trash/delete` 没有这个参数。
- CLI summary 很薄：`send` 的 `to` 把 cc/bcc 合并在一起；`reply` 没有收件人；`delete --all` 没有数量。

### 6.2 模式（`AGENTLY_MCP_CONFIRM` / `--confirm`）

**`elicit`（默认，强模式，模型只看到一次工具调用）**

Go SDK v1.8.0 默认协议版本为 `2026-07-28`。该版本起，MCP 规范（SEP-2322）**禁止**服务端主动发起 `elicitation/create`，必须使用 **multi round-trip requests（MRTR）**：handler 返回 `CallToolResult.InputRequests` + `RequestState`。因此确认流程按 MRTR 实现，由 SDK 中间件自动兼容旧客户端：

1. 客户端调用写工具（无 `confirmation_token`、无 `RequestState`）。
2. Server 执行 CLI（不带 token），得到 `ctk_`、CLI summary、`expires_in`；补全 summary。
3. Server 生成随机 `pending_id`，把 `pending_id → {tool, canonicalArgs, ctk, sessionID, expiresAt}` 写入缓存。
4. Handler 返回：
   ```go
   &mcp.CallToolResult{
       InputRequests: map[string]mcp.InputRequest{
           "confirm": &mcp.ElicitParams{Message: ..., RequestedSchema: ...},
       },
       RequestState: pendingID,
   }
   ```
5. 客户端处理：
   - 协议 ≥ 2026-07-28：客户端 SDK 自动展示表单，然后带 `InputResponses` + `RequestState` **重试同一个 tools/call**。
   - 协议 < 2026-07-28：SDK 的服务端中间件自动调用 `ServerSession.Elicit`，然后重新调用 handler。
6. 第二次 handler 调用：用 `RequestState` 查缓存，校验会话与过期时间，读取 `confirm`。
   - accept 且 `confirm=true` → 用缓存中的 `ctk_` 和 `canonicalArgs` 执行第二次 CLI。
   - decline/cancel → 返回 `status:"declined"`，零副作用。
7. 缓存过期（首次调用后 300 秒）→ `CONFIRMATION_EXPIRED`，不执行第二次 CLI。

> 模型在 elicit 模式下**看不到 ctk**，也无法自行完成第二次调用；真人确认是唯一路径。

**`token`（兼容模式，两次调用）**：不返回 `InputRequests`，始终返回 `confirmation_required` + ctk；客户端展示 summary 后带 `confirmation_token` 再次调用。适用于不支持 MRTR 也不支持 elicitation 的客户端，或需要把确认交给外部系统的场景。

**`never`（自动化模式）**：`send/reply/forward` 直接传 `--confirmed`，一次完成；`trash/delete` 因 CLI 无 `--confirmed`，自动降级为 `token` 两阶段，**不能绕过**。启动时向 stderr 打印警告。

### 6.3 待确认状态缓存

`ctk_` 绑定请求，`token` 模式要求原样重发，否则失败。Server 维护确认缓存：

```go
type Pending struct {
    Tool          string
    ArgsHash      string
    CanonicalArgs []string   // 已确认的完整 CLI 参数
    Ctk           string     // CLI 返回的确认令牌，不写日志
    SessionID     string     // 绑定 MCP 会话，禁止跨会话复用
    ExpiresAt     time.Time
}
type Store struct { mu sync.Mutex; m map[string]Pending } // key: pendingID 或 ctk
```

- MRTR 模式用 `pendingID` 作为 key；`token` 模式用 `ctk` 作为 key。
- TTL 300 秒；最多 100 条，LRU 淘汰；仅内存，不落盘。
- 第二次调用命中时使用缓存的 `CanonicalArgs`；若本次 `ArgsHash` 不同，仍用缓存参数（防止确认后调包），并返回 `meta.args_changed:true`。
- 会话绑定：HTTP 模式下 `SessionID` 必须匹配；stdio 模式固定为 `stdio`。
- 未命中/过期 → `CONFIRMATION_EXPIRED`，要求重新发起。
- **不做服务端主动重试**；第二次 CLI 只在收到 `InputResponses` 或客户端带 token 的调用后执行。

### 6.4 Summary 生成规则

Summary 必须确定性、可验证，不能由模型自由撰写：

```text
summary = {
  action, from,
  to[], cc[], bcc[],                 // 规范化后的工具参数
  subject,
  attachments: [{filename, size}],   // 本地 stat，不读内容
  body_preview,                      // 正文前 200 字
  source_message: {id, subject?, from?}
}
```

- `to/cc/bcc/subject/body_preview` 取自已校验参数，**不采用 CLI 合并后的 `to`**。
- `reply/forward` 额外执行 `+read` 补全原邮件主题与收件人；**会标记已读**，在描述和结果中说明。
- `trash/delete` **不额外读取**；summary 只含 `message_id` 与 CLI summary。工具描述要求模型在用户按内容指代邮件时先调用 `mail_get_message`。
- `delete --all` 明确提示「永久删除回收站全部邮件，数量未知」。
- CLI 原文放在 `summary.cli`，便于排查。

### 6.5 不可违背的规则

- `elicit`/`token` 模式下**永不**自行加 `--confirmed`。
- 除 MRTR/elicitation 已获真人同意外，**永不**在拿到 `ctk_` 后自动续跑。
- **永不**自动重试写操作（网络错误、超时、exit 1/4 都不重试）。
- `confirmation_token` 不写日志；只显示前 8 位。
- 不接受模型自定义确认文案。
- 令牌过期或未命中缓存 → 重新发起。

> 已知边界：`token` 模式下模型理论上可自己连续调用两次完成发送；默认 `elicit` 正是为了消除它。`never` 完全信任调用方，仅用于用户明确配置的自动化。

## 7. 路径沙箱与文件规则

- 工作根目录：`--workdir` / `AGENTLY_MCP_WORKDIR`，未设置时用进程 CWD。
- 所有文件参数解析后必须落在根目录内（对已存在路径做 `EvalSymlinks`，防止符号链接逃逸）。
- 绝对路径若在根目录内，转换为相对路径传给 CLI；根目录外直接拒绝。
- 附件只允许普通文件（`os.Stat().Mode().IsRegular()`），拒绝设备、socket、目录。
- 写文件前 `os.MkdirAll` 目标目录，并把相对目录传给 CLI。
- 依据 `+me` 限制预校验：单文件 ≤ 20MB、总数 ≤ 50、总大小 ≤ 20MB；超限在启动子进程前失败。
- v1 不提供越界开关；需要更上层目录时调整 `--workdir`。

## 8. CLI Runner

```go
type Runner struct {
    Bin     string
    Workdir string
    Env     []string
}
func (r *Runner) Run(ctx context.Context, args []string, timeout time.Duration) (*Result, error)
```

- `os/exec.CommandContext` + 参数数组，**禁止** shell 拼接。
- **进程树管理**：npm 的 `agently-cli` 是 Node shim，内部再 `execFileSync` 真正的平台二进制（实测 Linux 上 `agently-cli` → `scripts/run.js` → `agently-cli-linux-x64/bin/agently-cli`）。只杀 shim 会留下孤儿进程，导致 `watch` 停不下来。
  - 优先直接解析并执行平台真二进制（见 8.1），减少一层进程。
  - 无论是否直接解析，子进程都放入独立进程组：Unix `SysProcAttr{Setpgid:true}`，超时时对 `-pid` 发 SIGTERM，2 秒后 SIGKILL；Windows 使用 Job Object（`KILL_ON_JOB_CLOSE`）确保整棵树退出。
- **并发**：默认 `--max-concurrency 1`，所有非 watch 的 CLI 调用串行，避免共享凭据文件与自动刷新竞争。`watch` 不占全局锁，只持独立的 `watchMu`；第二个 watch 返回 `WATCH_BUSY`。`mail_refresh_auth` 在 watch 运行期间返回 `WATCH_BUSY`，避免 refresh 与长轮询并发改凭据。
- 超时：默认 30s；read/search 60s；send 60s。超时杀进程组。
- `watch` 没有 CLI 超时参数（实测无限阻塞）；Server 到 `timeout_seconds` 主动杀进程组，收集已输出的 NDJSON，返回 `{events,count,timed_out:true}`。零事件也是成功。
- **大正文**：`send/reply/forward` 的正文一律写入工作目录内临时文件，用 `--body-file` 传相对路径，并显式带 `--body-format`。原因：Windows `cmd.exe` 命令行上限约 32K，CLI 允许 1MB 正文；临时文件同时规避转义问题。扩展名按格式选择（`.md` / `.html` / `.txt`），文件权限 0600，调用结束（含失败/超时）后删除。
- stdout 整体解析 JSON；`watch` 按行解析 NDJSON。
- `_notice.update` 透传到 `meta.update_available`。
- 启动时执行一次 `agently-cli --version` 探测；要求 `>=1.0.18`，不满足只告警。

### 8.1 二进制定位（跨平台）

查找顺序：

1. `--cli` 参数
2. `AGENTLY_CLI_PATH` 环境变量
3. `exec.LookPath("agently-cli")`
4. 报 `CLI_NOT_FOUND`，提示 `npm install -g @tencent-qqmail/agently-cli`

**解析真实二进制（best-effort）**：

1. 若 `LookPath` 结果是符号链接（Unix 典型布局），取 `EvalSymlinks`，若指向 `.../agently-cli/scripts/run.js`，则包根为 `dirname(dirname(path))`。
2. 由 `runtime.GOOS`/`GOARCH` 映射平台包名：`@tencent-qqmail/agently-cli-{linux|darwin|win32}-{x64|arm64}`。
3. 依次探测：
   - `<pkgRoot>/node_modules/<platformPkg>/bin/agently-cli[.exe]`（局部安装）
   - `<pkgRoot>/../../<platformPkg>/bin/agently-cli[.exe]`（npm 全局提升）
   - `<npmPrefix>/lib/node_modules/<platformPkg>/bin/agently-cli[.exe]`（Linux/macOS 全局）
   - `<npmPrefix>/node_modules/<platformPkg>/bin/agently-cli[.exe]`（Windows 全局）
4. 找到则直接执行；找不到则回退到 shim，并依靠进程组/Job Object 管理进程树。

支持 npm 全局、nvm、nvm-windows、pnpm、Volta 的核心是：先用 `LookPath` 找到用户实际会执行的那个入口，再尽力解析；**回退路径必须可用**，不能把直接解析当成唯一方案。

**Windows 特例**：npm 安装的是 `agently-cli.cmd`（内部调用 `scripts/run.js`，最终执行平台包里的 `agently-cli.exe`）。`CreateProcess` 不能直接执行 `.cmd`，因此：

- 若解析到 `.exe`，直接 `exec.Command`。
- 若解析到 `.cmd`/`.bat`，通过 `cmd.exe /d /s /c` 执行，并对参数做 Windows 命令行转义（单独实现并单测）。
- 大正文已改为 `--body-file`，因此转义函数只需处理普通参数（路径、邮箱、ID），复杂度可控。
- 所有平台都支持 `--cli` 指向真实 `.exe` 的逃生口。

### 8.2 状态判定顺序（优先 JSON，退出码兜底）

1. JSON 可解析且 `data.confirmation_required === true` → **确认流程**（不是成功）。
2. JSON 可解析且 `ok === false` → 按 `error.type` / `error.code` / `error.message` 分类。
3. 非 JSON 或缺少 `ok` → 按退出码兜底，message 取 stderr 首行。
4. 只有 `ok === true` 且无 `confirmation_required` 才算成功。

### 8.3 退出码映射（观察值 + 兜底）

| exit | 观察到的场景 | code | retryable | 处理 |
|---|---|---|---|---|
| 0 | 成功；确认待办；`auth status` 未登录也返回 0 | — | — | 由 JSON 字段决定 |
| 1 | 本地参数校验、文件不存在、请求构建失败、部分网络错误 | `INVALID_ARGUMENT` / `UPSTREAM_ERROR` | **否** | 返回 stderr 首行 |
| 2 | 文档约定，实测未触发 | `INVALID_ARGUMENT` | 否 | 原样返回 |
| 3 | 授权失效 | `AUTH_REQUIRED` | 否 | hint：`agently-cli auth login` |
| 4 | 文档约定，实测未触发 | `NETWORK_ERROR` | 否 | 提示可重试，由调用方决定 |
| 6 | API 400/404、确认令牌过期或被修改 | `REJECTED` | 否 | 原样返回 `error.message` |
| 7 | 触发限频（文档约定） | `RATE_LIMITED` | 是 | 读取 `Retry-After` |
| 8 | 文档约定，实测未触发 | `CONFIRMATION_REQUIRED` | — | 进入确认流程 |
| 其他 | — | `CLI_ERROR` | 否 | 附带 exit code 与脱敏 stderr |

**任何操作都不自动重试。**

## 9. 传输层

### 9.1 stdio（默认）

- `agently-cli-mcp` 无参数启动即 stdio。
- stdout 只输出 JSON-RPC；日志全部 stderr。
- 适合 Claude Desktop/Code、OpenCode、Cursor 等本地客户端。

### 9.2 Streamable HTTP

- `agently-cli-mcp --transport http --listen 127.0.0.1:8787`
- MCP 端点：`POST/GET/DELETE /mcp`，由 `mcp.NewStreamableHTTPHandler` 处理。
- 兼容端点：`--enable-sse` 时额外挂载 legacy `GET /sse` + `POST /message`。
- 健康检查：`GET /healthz` 返回 `{"ok":true,"version":...}`，不含敏感信息。
- 默认只监听 loopback。
- 绑定非 loopback 地址时**必须**设置 `AGENTLY_MCP_HTTP_TOKEN`，否则拒绝启动。
- 认证：`Authorization: Bearer <token>`，常量时间比较；`/healthz` 可豁免。
- 每个 HTTP 会话创建独立 `mcp.Server` 实例（`getServer` 回调），共享 Runner 与确认缓存。
- 确认 token 与会话 ID 绑定，禁止跨会话使用。
- 不内置 TLS；对外部署由反向代理终止 TLS。可选 `--tls-cert/--tls-key` 便于内网自签。
- CORS 默认关闭；需要浏览器客户端时用 `--cors-origin` 显式允许。
- 请求体默认上限 4MB（SDK 默认）。

### 9.3 传输选择

| 场景 | 传输 |
|---|---|
| 本地 IDE / 桌面客户端 | stdio |
| 远程共享、多会话、容器部署 | HTTP |
| 旧版只支持 SSE 的客户端 | HTTP + `--enable-sse` |

## 10. 单二进制与跨平台构建

- 构建：`CGO_ENABLED=0 go build -trimpath -ldflags="-s -w -X main.version=$VERSION" ./cmd/agently-cli-mcp`
- 目标矩阵：

| GOOS | GOARCH | 产物 |
|---|---|---|
| linux | amd64 | `agently-cli-mcp_linux_amd64` |
| linux | arm64 | `agently-cli-mcp_linux_arm64` |
| darwin | amd64 | `agently-cli-mcp_darwin_amd64` |
| darwin | arm64 | `agently-cli-mcp_darwin_arm64` |
| windows | amd64 | `agently-cli-mcp_windows_amd64.exe` |
| windows | arm64 | `agently-cli-mcp_windows_arm64.exe` |

- 纯 Go（无 cgo）时，一个 Linux runner 可交叉编译全部 6 个目标；CI 仍应在 macOS/Windows runner 上各跑一次冒烟测试。
- macOS 分发需要 codesign + notarize，否则 Gatekeeper 会拦截；Release 流水线预留签名步骤。
- 二进制不内嵌 CLI；运行时通过 `--cli` / `AGENTLY_CLI_PATH` / `PATH` 找到系统安装的 `agently-cli`。

## 11. 分发

优先级从高到低：

1. **GitHub Releases**：6 个压缩包 + `checksums.txt` + SBOM。
2. **npm 元包**（可选，与 CLI 的分发方式一致）：`agently-cli-mcp` 主包 + 6 个平台包，通过 `optionalDependencies` 分发；主包只含一个启动 shim，真正的 MCP Server 仍是单二进制。此方式只是投递便利，不改变「无运行时依赖」。
3. **Docker**（可选，仅 HTTP 模式）：`linux/amd64`、`linux/arm64`。
4. `go install github.com/<org>/agently-cli-mcp/cmd/agently-cli-mcp@latest`（面向 Go 用户）。

## 12. 工程结构

```text
agently-cli-mcp/
├── go.mod
├── go.sum
├── Makefile
├── .github/workflows/release.yml
├── cmd/
│   └── agently-cli-mcp/
│       └── main.go             # flag 解析、传输选择、信号处理
├── internal/
│   ├── config/
│   │   └── config.go           # env/flag 合并、默认值、校验
│   ├── cli/
│   │   ├── locate.go           # 跨平台定位
│   │   ├── locate_windows.go   # .cmd/.bat 处理
│   │   ├── runner.go           # exec、超时、JSON/NDJSON 解析
│   │   ├── proc_unix.go        # Setpgid + 杀进程组
│   │   ├── proc_windows.go     # Job Object + 杀进程树
│   │   ├── bodyfile.go         # 正文临时文件
│   │   └── errors.go           # 状态判定与退出码映射
│   ├── guard/
│   │   ├── paths.go            # 沙箱、realpath、大小校验
│   │   └── redact.go           # token/正文脱敏
│   ├── confirm/
│   │   ├── store.go            # pendingID/ctk → canonical args 缓存
│   │   ├── summary.go          # 确定性 summary 生成
│   │   └── mrtr.go             # InputRequests/RequestState + token 降级
│   ├── search/
│   │   └── store.go            # cursor → 搜索条件（TTL/LRU）
│   ├── tools/
│   │   ├── account.go
│   │   ├── read.go             # list/get/search/watch
│   │   ├── write.go            # send/reply/forward/trash/delete
│   │   └── attachments.go
│   └── mcpserver/
│       ├── server.go           # 注册工具
│       ├── stdio.go
│       └── http.go             # Streamable HTTP + SSE + auth
├── testdata/
│   └── fake-agently-cli/       # Go 编写的假 CLI，可跨平台编译
├── test/
│   └── e2e/
└── docs/
    └── superpowers/specs/
```

工具注册使用 SDK 的泛型 `mcp.AddTool[In, Out]`，一个工具一个函数，schema 由 struct 生成，避免手写 JSON Schema。

## 13. 测试策略

1. **假 CLI**：`testdata/fake-agently-cli` 是 Go 程序，按 `FAKE_SCENARIO` 返回固定 JSON、指定退出码，并把 argv 写入临时文件。测试断言：
   - 参数映射正确；
   - 未确认时**没有** `--confirmed`；
   - 二次调用带正确 `--confirmation-token`；
   - `trash/delete` 在任何模式下都不出现 `--confirmed`；
   - 正文走 `--body-file`，临时文件在调用后被删除。
2. **状态矩阵测试**：
   - exit 0 + `confirmation_required:true` → 确认流程；
   - exit 0 + `ok:false` → 错误；
   - exit 1 + plain-text stderr → `INVALID_ARGUMENT`，不重试；
   - exit 6 + `error.code:404` → `NOT_FOUND`；
   - 非 JSON stdout → `PROTOCOL_ERROR`，结果不含原始 stdout；
   - 超时 → 杀进程组，只返回脱敏信息。
3. **确认流程测试**：
   - MRTR：首次返回 `InputRequests` + `RequestState`；accept 后 handler 二次调用并带 ctk；decline 零副作用；
   - 旧协议客户端：SDK 中间件走 `Elicit` 再重入；
   - token 模式：ctk 缓存命中/过期/参数变更/跨会话；
   - 超过 300 秒 → `CONFIRMATION_EXPIRED`，不执行第二次 CLI。
4. **进程树测试**：假 CLI 启动一个孙子进程并 sleep；触发超时后断言孙子进程也被杀掉（Unix 用 `pgrep`/`kill -0`，Windows 用 Job Object 测试）。
5. **单元测试**：路径沙箱（`..`、绝对路径、符号链接逃逸）、summary 生成、Windows 命令行转义、正文截断、脱敏、搜索上下文（命中/冲突/过期）。
6. **传输测试**：
   - stdio：用 `NewInMemoryTransports()` 加 SDK Client 调每个工具；
   - HTTP：`httptest.NewServer` + `StreamableClientTransport`，覆盖鉴权、会话隔离、`/healthz`、MRTR 输入重试；
   - SSE：旧客户端握手与消息收发。
7. **Schema 快照**：对 `tools/list` 输出做 golden 测试，防止 schema 漂移。
8. **真实环境冒烟**：`AGENTLY_MCP_LIVE=1` 只跑只读工具；发信仅在 `AGENTLY_MCP_LIVE_SEND=1` 且收件人为自己时启用。
9. 覆盖率目标：`internal/` ≥ 80%，确认状态机与错误映射 100%；CI 跑 `go test -race`。

## 14. 实施阶段

| 阶段 | 内容 | 完成标准 |
|---|---|---|
| P0 脚手架 | go.mod、Makefile、config、logger、CI 骨架 | `go build ./...`、`go test ./...` 通过；6 目标构建脚本可用 |
| P1 CLI 适配 | locate（真实二进制 + Windows shim）、进程组/Job Object、body-file、状态判定、fake CLI | 覆盖实测状态矩阵；Windows 转义单测通过；超时能杀掉整棵进程树 |
| P2 只读工具 | account/refresh/list/get/search/download + 搜索上下文 | stdio 端到端可列出并读取真实邮件；翻页只传 cursor 也能正确续页；截断与已读副作用生效 |
| P3 写工具与确认 | confirm store、summary、MRTR/token/never + 5 个写工具 | MRTR 与 token 两模式测试通过；拒绝时零副作用；trash/delete 不被 never 绕过 |
| P4 双传输 | stdio 收尾 + Streamable HTTP + 可选 SSE + 鉴权/会话 | 两种传输跑同一套工具测试；非 loopback 无 token 拒绝启动 |
| P5 watch 与发布 | watch 有界轮询、大小预检、6 平台构建、Release | 6 个二进制可下载并通过冒烟；checksums 与 README 完整 |

每个阶段独立可交付、可测试，不合并成大提交。

## 15. 待确认问题

1. **npm 元包是否需要**：只发 GitHub Releases，还是同时按 CLI 的方式发 npm 平台包？
2. **`mail_watch_messages` 是否进 v1**：它会阻塞一次工具调用最长 120 秒；可以放到 P5 或直接后置。
3. **HTTP 默认监听**：确认默认 `127.0.0.1:8787`，非 loopback 强制 Bearer token。
4. **Windows 是否要求 CI 实机冒烟**：交叉编译可以保证构建，但 `.cmd` shim 的真实执行最好在 Windows runner 上验证。

## 附录 A：2026-10-20 对 CLI 1.0.18 的实测记录

> 用户更新后复查：`agently-cli version 1.0.18`，npm latest 仍为 1.0.18，帮助文本与下列行为均无变化。

| 场景 | 实际结果 |
|---|---|
| `message +send`（无 token） | exit 0，`confirmation_required:true`，返回 `ctk_`；summary 含 `action/attachment_count/from/subject/to` |
| `message +send --cc c --bcc b` | summary 的 `to` 同时包含 to/cc/bcc，无法区分 |
| `message +reply`（无 token） | exit 0，确认待办；summary 只有 `action/attachment_count/from/message_id` |
| `message +forward`（无 token，带 body） | exit 0，确认待办；summary 含 `to`，无 subject/cc/bcc |
| `message +forward`（无 body 或空 body） | exit 6，API 400 `ForwardMessageReq.body` |
| `message +trash`（无 token） | exit 0，确认待办；summary 只有 `action:"delete", message_id` |
| `message +delete --all`（无 token） | exit 0，确认待办；summary 只有 `action:"clear_trash"` |
| `message +trash --confirmed` | exit 1，未知参数（不支持 `--confirmed`） |
| `message +read --id msg_nonexistent` | exit 6，`{ok:false,error:{type:"api_error",code:404}}` |
| `message +read` 真实邮件 | exit 0，邮件 `is_read` 由 false 变为 true |
| `message +list --limit 999` | exit 1，stderr plain text：`--limit must be between 1 and 50` |
| `message +send --to not-an-email` | exit 1，stderr plain text：`--to must be a valid email address` |
| `attachment +upload --file ./README.md` | exit 0，返回 `file_id`；但该 `file_id` 不能用于 `--attachment` |
| `message +watch` | 无 `--timeout` 参数；无邮件时无限阻塞且无输出（4 秒后 `timeout` 杀掉，exit 124） |

## 附录 B：Go SDK v1.8.0 能力验证

| 能力 | 结论 |
|---|---|
| stdio | `mcp.StdioTransport` 可用 |
| Streamable HTTP | `mcp.NewStreamableHTTPHandler` 可用，默认请求体上限 4MB |
| legacy SSE | `mcp.NewSSEHandler` 可用 |
| elicitation（旧协议） | `ServerSession.Elicit` 可用，但协议 ≥ `2026-07-28` 时被禁止 |
| elicitation（新协议） | 必须用 MRTR：handler 返回 `CallToolResult.InputRequests` + `RequestState`；客户端 SDK 自动重试 |
| 服务端兼容中间件 | 旧客户端由 SDK 自动把 `InputRequests` 转成 `Elicit` 并重入 handler |
| 类型化工具 | `mcp.AddTool[In, Out]` 自动生成输入/输出 schema 并校验 |
| 内存传输测试 | `mcp.NewInMemoryTransports()` 可用 |
| 交叉编译 | 纯 Go + `CGO_ENABLED=0`，支持 6 个目标 |
