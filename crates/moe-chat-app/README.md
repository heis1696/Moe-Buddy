# 文字聊天应用

日用桌面接线提供 `ModelSettings`、`configure_model`、一次性 `take_external_request` 与 `complete_external`。配置不含凭据；传输许可默认拒绝，明确包含人设、获准记忆与当前会话。Send 只准备并登记，返回不等于模型完成；宿主消费请求后在工作线程做 I/O，再回到应用线程交回结果。应用复验来源、版本、取消与权限；未知不重试。浏览器白名单未扩大。

`with_memory` 接收实现 `ChatMemory` 的宿主适配，`enable_history` 接收 `ChatHistory` 和显式 RawRetention。`HistoryAccess` 只在一次受信持久化调用内提供现有原文权限；不得序列化活句柄。适配错误链保留，公开错误文案不输出正文／路径。应用仍无文件、网络或系统时钟访问；宿主持久化时须注入 Unix epoch 毫秒。恢复只读历史，不续跑未结请求，取消／未知恢复结果保守隔离。原 `new` 保留无 I/O 的离线测试行为。

U2 的受信业务组合边界：复用记忆、API 预设、Agent 流程、诊断与 UI 管理，不依赖网络、文件、系统时钟或 GUI。协议快照只用于展示，不成为业务权限来源。

宿主调用 `ChatApplication::new(process_generation)`、`dispatch(action, now_ms)` 与 `snapshot()`；时间和非零进程代际由宿主注入。默认明确标识离线验收模式，不连接外部服务、不落盘、不自动重试结果未知的操作。

聊天、预设、记忆和诊断面板都由 `moe-ui-core` 的真实控制器与视图管理。隐藏保留控制器草稿；关闭有草稿时必须经内核确认；撤权清除缓存并禁止继续使用本次授权。

## 模块结构

| 模块 | 职责 |
| --- | --- |
| `contract` | 冻结的命令、动作与快照契约（字段与动作以该模块为准）。 |
| `app` | `ChatApplication`：唯一的 `UiManager` 持有者，`new/dispatch/snapshot` 组合边界。 |
| `panels` | 聊天/预设/记忆/诊断四类类型化控制器、事件与视图；展示缓存与真实 `mount/render/unmount` 计数只由视图回调维护。 |
| `services` | 受信服务组合：`TrustedIngress`、`SessionRuntime`、`AgentWorkflow`、`MemoryService`、`PresetRegistry`、`DiagnosticsService` 与明确标注"非真实模型"的离线验收模型。 |
| `error` | `AppError`：固定安全 `public_code`/`public_message`，原始错误链保留在 `Error::source`。 |

## 业务闭环

日用初版第一步新增记忆管理命令：`SearchMemories(query)` 经原授权读取边界返回最多 100 条匹配记录；`UpdateMemory { id, revision, text }` 与 `DeleteMemory { id, revision }` 只接受本应用明确创建的私密笔记，并由记忆核心重新校验归属、来源、保护和版本。`MemorySnapshot.revision` 用于乐观并发检查，旧版本修改/删除明确失败。查询条件在写入后保留；隐藏/关闭/撤权仍走 UI 生命周期与清理边界。这些命令目前只由原生宿主接入，回环 HTTP 白名单没有自动扩大。

- **发送**：`dispatch(Action::Send)` 要求聊天面板已打开且可见（拒绝顺序固定：未打开 → 不可见 → 空白草稿）。每轮依次走可信入口 `admit`（固定路由/凭据/会话提示）→ `SessionRuntime::register_user` → `AgentRuntime::bind_session` → `AgentWorkflow::prepare/infer/finish`。结论只以实际 `AgentDelivery` 判定：`Completed` 显示回复；`Failed`/`Cancelled` 记为失败；`Unknown` 记为未知；流程 `Succeeded` 从不冒充业务成功。回复由离线验收模型确定性生成并标注"离线验收·非真实模型"，同时体现当前预设与人设；不传凭据、端点或工具。 登记前按与会话/Agent同源配置的字符及事件预算预留本轮输入和最多4096字符回复；预留只在登记成功后记账，成功交付按实际回执退还未用回复额度。登记后任一阶段报错统一调用实际会话取消：取得取消回执才报告已取消；处置被拒绝或已闭合但状态无法确认时保留原始与处置错误，持有待处置交互并隔离后续发送，展示 Unknown，绝不由方法 Err 推定可安全重试。
- **明确记住**：`Action::Remember` 经 `MemoryService` 的受控私密写入（`UserProfile`/`UserStatement`/`Private`，绑定专用登记来源），聊天原文永不自动入库；下一轮经授权检索（`ReadPurpose::OfflineReasoning`）装入 `OfflineInput` 完成召回。记忆面板刷新使用同一授权读取边界的空查询列表语义，返回有界条目。
- **预设**：两个真实 `PresetRegistry` 离线预设（`offline-echo`/`offline-reflect`），`Action::SelectPreset` 走逐用途真实重绑，下一轮 `prepare` 解析即生效；未知 ID 明确拒绝。可见目录由已注册 `PresetsView` 的真实渲染缓存提供；隐藏/关闭清空展示缓存，重开重新装配创建预设时的同一份配置（注册表无按 ID 读取入口），当前绑定始终以注册表解析为准；不含凭据。
- **诊断**：初始化即按宿主注入的非零代际启用 `DiagnosticsService`（单一稳定域、全部类别、`Essential` 级、独立查看主体/许可/硬预算）。发送、记忆与预设切换映射为白名单运行阶段/操作/错误事件；采集句柄或写入失败不改变业务结果，健康计数经授权查询可见。`Action::RefreshDiagnostics` 每次认证主体后取一页有界安全投影（≤8 条，事件码/严重度/业务与传输状态），游标即时关闭；条目不含提示词、回复正文、记忆正文、凭据或原始错误，运行以展示别名呈现，历史缺口经 `notice` 显式提示。
- **面板生命周期**：四面板全部经 `UiManager` 注册与驱动；`mounts/renders/unmounts` 是当前/最近实例的真实回调计数（关闭释放后重开从新实例重新计数）。隐藏保留控制器草稿并清理展示缓存；恢复重走核验边界。会话展示历史由应用持有有界交付投影，不随面板实例关闭销毁；重开重新核验并装配，草稿只在确认丢弃后清空。关闭有草稿的聊天面板返回内核确认凭据：`discard: false` 取消且不丢草稿；面板状态在确认签发后变化（如继续输入）时内核拒绝旧确认（`confirm_stale`），草稿不丢；`discard: true` 确认后释放。不可见/未打开面板的输入一律拒绝。
- **撤权**：`Action::Revoke` 先关闭业务闸门，再以 `ContextRevoked` 强制释放全部面板（控制器清敏感缓存、视图卸载清展示缓存），随后撤销入口凭据并停用诊断采集（清空诊断缓冲）。此后任何动作都以 `revoked` 拒绝；快照保持可读。
- **容量与失败**：输入按 `MAX_TEXT_BYTES`（8000 字节）拒绝，记忆正文另按 4096 字符拒绝；会话/事件/任务/诊断全部有界。失败与未知不自动重试：业务失败保留 `ChatStatus::Failed/Unknown` 与固定失败码，仅宿主显式重新派发才会重试。UI 内核驱动报告中的面板故障不吞掉：`AppError` 持有整份有界 `DriveReport`，通过 `Error::source` 可达原始回调或导航错误，不扩展 UI 内核 API、不把原始错误写入浏览器快照。会话展示投影最多128条消息，容量在业务接纳前检查；发送失败保留输入草稿，不自动重发。

## 错误与公开面

`AppError::public_code()/public_message()` 只返回固定安全类别（如 `empty_input`、`input_too_large`、`unknown_preset`、`panel_not_open`、`panel_not_visible`、`revoked`、`confirm_stale`、`gateway_rejected`、`memory_rejected`、`agent_failed`、`diagnostics_rejected`、`panel_callback_failed` 等）；`Error::source` 保留网关/记忆/诊断/UI 内核的原始错误链，链上类型均不携带正文或凭据。快照 `revision` 随每次 `dispatch` 前进；`mode` 恒为 `offline`，`external_transmission_allowed` 恒为 `false`。 取消已确认时原始阶段错误仍在主 source 链；处置失败时 `disposition_error()` 可独立检查附加处置错误，不把两类错误或未知终态吞掉。

定向回归见 `tests/chat_app_flow.rs`（发送与交付判定、记住后召回、预设切换、日志脱敏与有界查询、真实生命周期计数与隐藏恢复草稿、隐藏/未打开拒绝、关闭确认/取消/失效、撤权清场、输入与错误边界、时钟回退不自动重试）。运行层、记忆核心与 UI 内核的语义由各自 crate 的回归保证，本 crate 不复制。
