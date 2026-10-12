# moe-cli-hooks：Claude Code 2.1.286 `UserPromptSubmit` Hook 边界适配器

## 受信宿主接线

`accept_claude_submission` 将已解析事件交给 `HookRuntime`，固定为
`UnverifiedPrompt`；绑定必须显式指定完整入口授权、预期 CLI session_id、
上下文来源、三项独立许可和期限。JSON 不能选择绑定或回复目标。
`write_claude_context` 从当前受信上下文准备回复，写前再验权限/版本/取消，
并由宿主注入写前/写后的单调时间；不访问文件、网络或系统时钟。

全部字节及 flush 成功只记 `Written`，不是 CLI 已消费。零字节失败记 `Failed`；
部分输出或 flush 失败可能已有合法 JSON 被接收，记 `Unknown`，绝不声称
“失败即无注入”。写后到期或时间回退也不报告成功消费；取消不覆盖已有终态，
重复输出不重放。Debug 同时脱敏 prompt 和 session_id。

`cargo run -p moe-cli-hooks --example claude_hook_fixture` 是可运行的 stdin/stdout
**纯合成夹具**：只接受 synthetic-session，身份、凭据和上下文均为固定合成值，
不读取任何路径。不要将它安装为生产 Hook 或把合成身份当作已验证用户；
实际宿主仍须建立可信本机绑定和取消/当前来源更新。本批没有自动安装 CLI
配置、持续服务或真实模型会话验收。

供受信宿主嵌入的 CLI 边界库：对 Claude Code **2.1.286** 的同步
`UserPromptSubmit` command Hook，提供严格有界的 stdin JSON codec、
有界输入流读取，以及单对象上下文输出编码。

**当前能力仅为可嵌入的适配器，不是已安装的端到端 CLI Hook。** 本 crate
不启动/不派生任何 CLI 进程，不读取 transcript、环境变量或终端，不修改
用户配置，不联网、不调用模型、不写原文或快照；事件收敛、绑定登记、
许可裁决、取消与写出前复验由受信运行服务完成，已由本 crate 的 host 模块
接线；生产宿主安装/身份绑定仍须另外提供，契约见 `docs/GATEWAY.md`。

codec 的语法上限不是最终准入许可：纯运行服务进一步要求非空/非纯空白、
无 NUL 且最多 16384 字符的 prompt；session_id 还须非空、无首尾空白/控制字符、
最多 128 字符并精确匹配宿主绑定。codec 允许的空 prompt 或 256 字节会话 ID
不代表运行服务一定接纳，失败不会截断、改写或隐式换身份。

上下文修订墓碑、累计绑定/调用签发量及正文预算在当前服务实例内不回收；
达到预算后封闭拒绝。长跑宿主须显式重建服务实例、重新授权与绑定，旧句柄
全部失效；不能把重建称为已有外部调用已取消、旧快照已召回或清理已完成。

## 版本核验边界

- 唯一经核验的契约版本是 `2.1.286`（[`VerifiedCliVersion`] 只能为它构造，
  宿主报告的其它任何版本串被 [`UnsupportedCliVersion`] 拒绝）。
- 核验依据见 `docs/GATEWAY.md` §3：本机 `claude --version` 为
  2.1.286 (Claude Code)，对 @anthropic-ai/claude-code 的 package.json 与
  bin/claude.exe 做过只读字节区间核验。滚动官方文档不构成版本保证；
  升级 CLI 后必须重新核验并有意修改本 crate，未核验版本不被支持。
- 载荷本身不含版本字段，因此版本只能在流之外由宿主显式指名。

## 输入语义（严格有界，失败即拒绝）

`parse_user_prompt_submit` 接受一个 UTF-8 JSON 对象（值后只允许空白）：

- **只保留** `session_id`、`hook_event_name`、`prompt` 三个获准字段；
  `hook_event_name` 必须精确等于 `UserPromptSubmit`。其余字段——包括
  `transcript_path`、`cwd`、`permission_mode` 等条件字段——只做有界语法
  检查后丢弃。
- **重复键在建对象前递归拒绝**：用 serde_json visitor 流式解析，每个
  对象的键以反转义形式收集，`"prompt"` 与 `"\u0070rompt"` 计为重复；
  未知字段嵌套的对象与数组同样检查。官方 JSON.parse「后值覆盖」的行为
  不可能被用来夹带字段。
- **拒绝**：坏 UTF-8、超长输入、嵌套超深、重复键、尾随对象/内容、
  非法 JSON、根非对象、获准字段缺失或非字符串、`session_id` 超限、
  事件名不匹配。所有错误经过脱敏：只含类别、上限与行列位置，不含
  输入内容。
- **不授信**：role、owner、权限、取消、回复目标等字段与任何未知数据
  一样被丢弃，绝不从中获得权力。按 §1，解析成功是 **UnverifiedPrompt**：
  不证明 prompt 是人类发言（外部通知也走同一入口），不得登记为用户
  事件或长期证据；`session_id` 只是关联键，不能认证或去重。

```text
let bytes = moe_cli_hooks::read_bounded(std::io::stdin().lock())?;
let version = VerifiedCliVersion::from_host_report("2.1.286")?;
let submission = parse_user_prompt_submit(version, &bytes)?;
```

## 输出语义（仅 InjectedContext）

`write_user_prompt_submit_context` 向写入器输出唯一允许的形态：
单个紧凑 JSON 对象
`{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":…}}`
加一个换行，字符串转义由 serde_json 按 JSON 规范完成；不输出顶层聊天
正文，不伪称助手消息。上下文必须是宿主登记的内容，本 crate 不生成正文。

- 成功返回 `ContextTransferred`：只证明写入器接受字节并完成 flush，
  **不**表示 CLI 已解析/采纳/消费，不表示 prompt 被阻断或放行。
- 失败返回脱敏错误，且不向输出流追加诊断正文；中途失败时已交给写入器
  的字节无法撤回，但绝不宣称 CLI 被阻断或已消费。

## 资源上限

| 常量 | 值 | 含义 |
| --- | --- | --- |
| `MAX_HOOK_INPUT_BYTES` | 1 MiB | 单次 stdin 载荷上限（读取与解析共用） |
| `MAX_SESSION_ID_BYTES` | 256 | `session_id` 字节上限（不施加格式） |
| `MAX_CONTAINER_DEPTH` | 64 | 容器嵌套深度上限，根对象计 1 |
| `MAX_ADDITIONAL_CONTEXT_BYTES` | 1 MiB | 输出上下文字节上限 |

## 依赖取舍

- `serde_json 1.x`：CLI 边界需要按官方契约严格解析并转义 UTF-8 JSON；
  visitor 流式 API 是「建对象前递归拒绝重复键」的最低成本可靠实现。
  未启用 derive、arbitrary_precision 等特性。
- `serde 1.x`（无 derive）：直接引用 `DeserializeSeed`/`Serialize` 等
  trait；它本就是 serde_json 的传递依赖。
- 除本地 moe-agent-runtime（测试/示例另用 moe-memory-core）之外零其它第三方依赖：无异步运行时、无数据库、无模型/GUI/网络栈。核心
  运行层不因本 crate 引入 Node 工具链或外部 CLI 的任何依赖。

## 验证

```text
cargo test -p moe-cli-hooks
```

外部合成测试覆盖：合法载荷与未知数据丢弃、转义等价重复键、嵌套/
数组内重复键、尾随对象、坏 UTF-8、超深/超长、字段缺失/类型错误、
版本拒绝、Debug 与错误脱敏、输出字节精确性与转义、写失败脱敏与
不追加诊断、读流上限与端到端链路。

## 明确不做

不启动 CLI、不装 Hook 配置、不读 transcript/环境/终端、不联网、
不调用模型、不写原文或快照、不承接 GUI 或工具执行；权限、归属、
版本、取消与回复目标由受信服务裁决。升级 CLI 版本前本 crate 拒绝
一切未知版本。
