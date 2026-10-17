# 本机文字聊天界面

Rust 受信宿主与浏览器展示适配。仅本机回环、单伙伴和内存会话；离线回复是验证五系统接线的确定性适配，不是已连接的真实模型。关闭进程会丢弃聊天和记忆；不默认落盘、调用外部服务或开放工具。

This crate is intentionally a QA and protocol fixture. The product-facing desktop entry is
`apps/moe-buddy-desktop`; this host remains useful for loopback HTTP policy and browser smoke
coverage and is not a desktop-pet implementation.

## 启动

```powershell
cargo run -p moe-chat-host --locked --offline
```

默认绑定 `127.0.0.1:0`，控制台输出本次实际访问地址。可使用 `--port 8787` 固定端口；不接受外网绑定。宿主负责时钟、随机进程代际和会话凭据，应用层不直接使用这些 I/O。

## 验证

```powershell
cargo test -p moe-chat-app -p moe-chat-host --locked --offline
node --test apps/moe-chat-host/assets/app.test.js
cargo build -p moe-chat-host --locked --offline
node scripts/chat-browser-smoke.mjs
```

浏览器 smoke 使用 Node 22+ 自带 WebSocket 和独立的无头 Edge/Chromium，无需前端包管理器。Windows 自动查找 Edge；其他位置可设置 `MOE_BROWSER`，非默认构建目录可设置 `MOE_CHAT_BINARY`。脚本只启动和结束自己的测试进程，创建隔离浏览器配置，结束时校验路径后清理；截图和结论保留在忽略的 `target/chat-u2-browser/`。

smoke 连接真实宿主及真实 `ChatApplication`，验证输入法/键盘、发送、明确记住与下轮召回、预设绑定、受控日志、隐藏与恢复草稿、内核关闭确认与重开、桌面/窄屏布局、回执丢失不重发、只读状态恢复和撤权清缓存；不是远程模型验收。当前运行日志只取八条有界事件，存在历史缺口会提示，不提供全量导出。

## 应用端口

纯业务组合位于 `moe-chat-app`：

- `ChatApplication::new(process_generation: u64) -> Result<Self, AppError>`，代际必须非零。
- `dispatch(&mut self, action: Action, now_ms: u64) -> Result<(), AppError>`。
- `snapshot(&self) -> AppSnapshot`。
- `AppError::public_code(&self) -> &'static str` 与 `public_message(&self) -> &'static str`，只向浏览器展示固定安全信息，原错误链留在业务错误内，不输出原文。

## HTTP 展示协议

静态资源只有 `/`、`/app.js`、`/style.css`，来自本 crate 的 `assets/`。不加载 CDN、外部字体或远程图片；界面不能直接访问模型、记忆后端或诊断服务。

- 首次打开 `/` 设置随机会话 Cookie；`HttpOnly; SameSite=Strict; Path=/`。只校验当前回环监听地址的精确 Host；Origin 存在时必须同源，POST 必须提供同源 Origin。
- `GET /api/bootstrap`：要求会话 Cookie，返回 `{ "csrf": "本次随机令牌", "snapshot": AppSnapshot }`。无 CORS 放行。
- `GET /api/state`：要求会话 Cookie，返回 `{ "snapshot": AppSnapshot }`；不重试任何业务操作。
- `POST /api/action`：要求会话 Cookie、精确同源 Origin、`Content-Type: application/json`、`X-CSRF-Token`。请求 `{ "id": "唯一请求ID", "action": "白名单动作", ... }`；成功返回 `{ "snapshot": AppSnapshot }`。
- 失败返回 `{ "error": { "code": "固定类别", "message": "安全提示" }, "snapshot": AppSnapshot }`。未通过认证的请求不得返回快照或其他受保护信息。
- 请求 ID 为 1–64 位 ASCII 字母、数字、连字符或下划线。同一进程不允许重复提交同一 ID；有界去重账本满后拒绝新动作，不淘汰旧 ID 后放行重放。请求失败或结果未知不能自动重试。
- 请求支持白名单 GET/POST、HTTP/1.1、单请求连接和显式 Content-Length；拒绝 Transfer-Encoding、多重/歧义长度或关键头、超限头/正文与其他方法。完整请求头（含终止符）不超过 8192 字节：分包到达仍按整头计算，头后同包的正文不计入该上限。连接读写有超时和有界内存；只监听回环。
- 静态页与 API 均禁缓存；`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`、CSP 限制同源脚本/样式/请求并禁止框架嵌入与对象。

动作字段：

| action | 额外字段 | 对应应用命令 |
| --- | --- | --- |
| `open` / `hide` / `close` | `panel` | 打开、隐藏或请求关闭面板 |
| `confirm_close` | `panel`, `discard`（bool） | 使用宿主持有的内核确认凭据，确认丢弃或取消关闭 |
| `set_draft` | `text`（可为空串） | 更新聊天控制器草稿；空串或纯空白即清空 |
| `send` | 无 | 发送已同步到控制器的草稿；空白由应用控制器判定 |
| `select_preset` | `preset` | 绑定已存在的受控离线聊天预设 |
| `remember` | `text` | 明确写入本伙伴的私密记忆 |
| `refresh_memories` | 无 | 经权限复验查询记忆 |
| `refresh_diagnostics` | 无 | 经独立查看许可查询受控日志 |
| `revoke` | 无 | 结束本次授权并清理所有面板敏感缓存 |

`panel` 只允许 `chat`、`presets`、`memory`、`diagnostics`；正文按 UTF-8 字节限制 8000 字节。`set_draft` 只校验类型与上限，空串和纯空白用于清空草稿，不算非法正文；`remember` 仍拒绝空白，`send` 的空白判定由应用控制器完成。未知动作、未知字段、错误类型、缺失字段和空白记忆必须明确拒绝。

## 快照字段

JSON 字段与 `moe-chat-app/src/contract.rs` 同名，枚举通过 `as_str()` 输出：

- 顶层：`revision`、`mode`（`offline`）、`companion`、`active_preset`、`external_transmission_allowed`（false）、`revoked`、`notice`（字符串或 null）。
- `panels[]`：`kind`、`visibility`（`visible` / `hidden` / `closed`）、`confirmation`、真实视图回调的 `mounts` / `renders` / `unmounts`。
- `chat`：`draft`、`status`（`ready` / `sending` / `failed` / `unknown`）、`failure`（字符串或 null）、`messages[]`（`role`: `user` / `assistant`，`text`）。
- `presets[]`：`id`、`label`、`provider`、`model`、`endpoint`；不包含凭据。
- `memories[]`：`id`、`text`；只在获准的可见记忆面板返回。
- `diagnostics[]`：`sequence`、`run`、`event`、`status`；只在获准的可见日志面板返回，事件不包含提示词、回复、记忆正文、凭据或原始错误。

浏览器依照服务端 `panels[].visibility` 展示面板，不能自行决定生命周期。隐藏保留草稿但清理展示缓存；关闭有草稿时显示内核发出的确认状态，不能用客户端确认框冒充成功关闭。前端输入同步请求须串行，在发送/隐藏/关闭前完成草稿同步；输入法组合过程中 Enter 不发送。显示明确的离线、仅内存、请求中、失败与结果未知状态。
