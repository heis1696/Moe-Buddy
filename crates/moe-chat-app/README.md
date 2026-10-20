# 文字聊天应用

日用桌面接线提供 `ModelSettings`、`configure_model`、一次性 `take_external_request`、绑定票据的 `complete_external` 与可注入 `ReplyParser`。配置不含凭据；传输许可默认拒绝，明确包含人设、获准记忆与当前会话。Send 只准备并登记，返回不等于模型完成；宿主消费请求后在工作线程做 I/O，再回到应用线程交回结果。应用复验来源、版本、取消与权限；未知不重试。浏览器白名单未扩大。

每次线上 Send 生成一个不可从外部构造的 `ExternalRequestTicket`，绑定本轮与对应 `WorkflowRuntime` 实例。`take_external_request` 只返回一次不可变 `ExternalInput` 与票据；`complete_external(ticket, outcome, now_ms)` 在取走 pending 前核验票据，重复、跨轮、跨实例、撤权及迟到回调都不会消耗或破坏正确在途请求。没有任何省略票据的完成入口。

`with_memory` 接收实现 `ChatMemory` 的宿主适配，`enable_history` 接收 `ChatHistory` 和显式 RawRetention。`HistoryAccess` 只在一次受信持久化调用内提供现有原文权限；不得序列化活句柄。适配错误链保留，公开错误文案不输出正文／路径。应用仍无文件、网络或系统时钟访问；宿主持久化时须注入 Unix epoch 毫秒。恢复只读历史，不续跑未结请求，取消／未知恢复结果保守隔离。原 `new` 保留无 I/O 的离线测试行为。

## 伙伴身份与配置

`BuddyRegistry` 是零 I/O 的受信伙伴注册表；宿主适配器负责 ID 种子、持久化和应用组合。注册表最多 8 个档案。ID 首字符必须是 ASCII 字母或数字，后续只允许字母、数字、点、下划线或连字符，最多 64 字节；显示名最多 64 个 Unicode 标量且拒绝控制字符。人设必须含非空白字符、最多 4096 个 Unicode 标量，允许 LF/CR/TAB，拒绝其他控制字符，以保留旧合法多行设置。创建只接收 ID、显示名和人设，不复制私密经历，也不设置模型、凭据或外传许可。`create`、`switch`、`rename` 和 `commit_configuration` 都要求预期注册表修订；改名和配置提交另要求预期档案修订。修订在安全整数耗尽时拒绝，不回绕。

档案摘要仅含 ID、显示名和档案/人设/模型版本。受信 Rust 宿主可经 `BuddyRegistry::profile` 读取完整档案；`BuddyModelConfiguration` 仅含 endpoint、model 和所绑定的 preset ID，不含 API Key 或发送许可。配置提交将人设内容与模型绑定及其版本作为一个档案状态更新；更改人设推进人设版本，所有配置提交推进模型版本与档案版本。创建、切换和档案提交均推进注册表修订。

注册表故障处置由 `moe-desktop-adapters::BuddyStorage` 收口，不扩大 `ChatApplication` 的普通业务访问权限。创建可能先为候选身份准备隔离原文；若准备或注册表发布失败，适配器以持久候选标记保留现场并拒绝再次签发身份。注册表结果为 `Unknown` 时，宿主仍可通过适配器提供的已加载实例私有票据结算、只读检查及预期草稿水位绑定的全伙伴 Quit 处置入口完成安全退出。

`ChatApplication::with_memory_and_identity(process_generation, memory, identity, persona_revision, model_revision, persona)` 注入实际 `PetIdentity` 与受信版本。旧 `new` / `with_memory` 继续使用 `offline-companion`。`identity()` 和 `AppSnapshot::companion` 返回稳定实例 ID；面板控制器的 `companion()` 返回同一 ID 的 `&str`。显示名、人设和素材不是身份。恢复模型绑定用 `configure_model_binding(settings, model_revision, persona_revision, preset_id)`，调用前应由宿主按已提交的档案版本打开应用。`preflight_model_binding` 无副作用地检查候选绑定；持久化宿主仍须以真实发布回执为准。`preflight_buddy_switch` 在待关闭确认、本地执行/展示未完成或存储结果未知时拒绝切换，不消费草稿。恢复设置时必须把 `allow_transmission` 置为 false，并重新显式配置凭据与许可。

## 逻辑会话（首批）

`AppSnapshot` 增加 `active_session_id`、`sessions_revision`、无正文的 `sessions` 目录项（`id/label/archived/status`）和必填 `session_isolation_allowed`。ID 由应用签发，最多 64 字节 ASCII 安全标识；目录最多 64 项，修订号始终位于 JavaScript 安全整数范围。`session_isolation_allowed` 仅由应用在外部结果未知、本地处理已结束、原文账本可保留且无存储隔离/撤权时置为 true；生命周期命令再次复验。聊天草稿按逻辑会话保存在应用内，每份不超过 `MAX_TEXT_BYTES`；它们不进入 `ChatHistory`、RawSnapshot 或 Tauri 目录快照。撤权后活动 ID 为 `None`、目录为空。

应用动作 `NewSession`、`SwitchSession`、`ArchiveSession` 都要求当前活动 ID 与目录修订号。Tauri IPC 的 `new_session`、`switch_session`、`archive_session`、`set_draft`、`send` 使用冻结的 `expectedSessionId` / `expectedSessionsRevision` 字段；`set_draft` 与 `send` 的 Tauri 边界映射到 `SetDraftGuarded` / `SendGuarded`，在执行前复验。受信同步 Rust 宿主仍可用简短 `SetDraft` / `Send` 动作，不应把它们暴露为排队 IPC。

归档只标记当前会话并保留选中状态，不创建新会话，也不清理或延长原文；归档目标仍可切换查看，但不能编辑草稿或发送。新建/切换先复验聊天面板可见且可接收，再做持久化或运行代际提交；提交后展示失败按已提交操作报告。草稿权威属于逻辑会话，隐藏卸载只清展示缓存。`CloseChat` 只消费当前草稿；整应用 Quit 的确认绑定所有会话草稿集合版本、D 管理草稿版本和退出意图，取消保留全部，确认只消费匹配版本。外部 `Unknown` 的本地处理已结束后，持久化会话可显式隔离新建；volatile 模式在仍持有原文账本时会拒绝轮转，避免只留展示文本而丢失权威事件。旧会话仍保持 Unknown，重复、跨会话和迟到 `ExternalRequestTicket` 不消费当前请求。截止等号时，外部 Unknown 不会被本地 cancel 回执改写成 `send_cancelled`；有本地取消回执时仅结算本地回复预留并保持 `send_unknown/Unknown`，本地处置失败则登记待处置并阻断隔离。存储维护或 checkpoint 未知仍锁住原文写入。

`ChatHistory` 注入契约覆盖 body-free 目录恢复、按会话受限历史页、当前段检查点、分代轮转、归档 CAS 与冷段维护。启动恢复接收目录内每个会话的受信请求，先为冷热及归档段提交当前期限净化，再返回恢复快照和原文回执；旧格式未结事件的控制证据与正文展示过滤分离。持久化轮转先维护并检查点旧段，再为新执行上下文建立独立 RawSnapshot 基线，最后 CAS 发布目录；轮转发送附带本轮输入和回复预留，容量拒绝发生在任何轮转提交之前。文件和目录提交分别持有回执，不承诺跨文件原子。恢复不续跑未结请求；目录 v2 分别保存外部 Unknown 与经当前许可完整恢复得到的未结原文概要，v1 目录读取时将新增概要视为无证据。两种已核对证据都只允许显式隔离新会话，不证明历史请求实际外发；仅有 Unknown 状态不会授予隔离。概要先于原文净化提交，成功恢复后即使原文到期重启，旧会话仍保持 Unknown 且有据隔离。持久化活动段在 64 事件、65,536 字符或应用保留限前轮转；新代际只展示有界最近 128 条投影，不把冷段导入新的 Agent/SessionRuntime。归档目标重启后保持只读，不为其重复创建空写段。

`ChatApplication::history_commit_receipts()` 返回最近一次操作中已确认的 RawLocal revision/commit time 与目录 revision/file-sync/directory-sync 回执。目录同步不可用的平台保留 `directory_synced: false`；回执分别描述两个提交，不合并成跨文件成功证明。

执行代际同时跟踪会话服务实际签发的片段句柄。到达原有8片段额度时，在下一轮登记前封存并轮转；不只按64事件/65536字符判断。因此同一进程间隔活跃TTL聊天也不会在第九片段卡住，短间隔聊天仍共享片段。该计数随真实登记推进，只在旧代际成功封存并更换后释放，不提高原片段常量。

无持久化适配时仍可在原文账本为空时新建、切换和归档逻辑会话。单执行代际最多 64 原文事件/65,536 字符，单会话展示最多 128 条，目录最多 64 个逻辑会话；一旦已接纳原文，轮转会返回 `conversation_full/NotExecuted`，直到宿主可安全保留账本，不以计数清零或展示文本替代原文。此模式不依赖默认落盘，也不承诺无限内存运行。持久化宿主另有 64 会话、256 个活动或待退役段和 16 MiB 原文/目录根预算，详见适配器 README。

Tauri 等受信 Rust 宿主可读取 `chat_draft_set_revision()`、`has_unsubmitted_chat_drafts()`，先用 `preflight_quit_draft_discard(current_close_discards_active)` 检查确认期间需要的修订空间，再在 Quit 确认后以 `discard_all_chat_drafts(expected_revision)` 消费版本匹配的会话草稿；这些宿主 API 不改变前端快照形状。

`ChatApplication::chat_draft_snapshot()` 为宿主提供无正文的聊天草稿只读回执：`ChatDraftSnapshot { revision, discard, sessions }`。`discard` 只在一次明确丢弃（`CloseChat` 确认或整应用 Quit 确认）后出现，形状为 `ChatDraftDiscard { revision, session_ids }`；`revision` 是丢弃后的草稿集合修订（安全整数），`session_ids` 为被清空的会话 ID，已排序去重且不超过 64 项。`sessions` 为至多 64 项 `ChatDraftSession { id, revision, discarded_revision }`，记录每个活动逻辑会话最近一次草稿变更修订与最近一次明确丢弃修订（未丢弃为 `None`）；不含任何草稿正文、队列或执行权。普通空快照、隐藏/重开、只读重读和普通编辑都不会生成回执。回执保持可读直到下一次丢弃替换它；后续编辑只会推进对应会话与全局 `revision`，使旧回执成为历史。宿主必须按每会话水位消费：其他会话的编辑不能让 A 的旧回执失效，漏收中间丢弃事件时仍能读到该会话最新水位并清理本地缓存。`CloseChat` 回执只绑定当前会话；Quit 回执覆盖全部被丢弃会话，并会把紧邻的同一次确认中已由聊天关闭丢弃的会话一并计入。撤权后目录清空，回执与逐会话水位同时清空。

`checkpoint_history` 和宿主 30 秒有界周期分别维护活动运行时；适配器每次最多维护 4 个冷段。维护报告包含实际完成/剩余段数、冷段容量阻塞原因和已确认回执；容量不足不是普通分页结束，已完成段的回执仍返回。维护后若热段检查点无法确认，应用快照进入存储 Unknown 并封闭写入。冷热原文仍遵守正常 30 天、回收 7 天策略。

U2 的受信业务组合边界：复用记忆、API 预设、Agent 流程、诊断与 UI 管理，不依赖网络、文件、系统时钟或 GUI。协议快照只用于展示，不成为业务权限来源。

宿主调用 `ChatApplication::new(process_generation)`、`dispatch(action, now_ms)` 与 `snapshot()`；时间和非零进程代际由宿主注入。默认明确标识离线验收模式，不连接外部服务、不落盘、不自动重试结果未知的操作。

聊天、预设、记忆和诊断面板都由 `moe-ui-core` 的真实控制器与视图管理。隐藏保留控制器草稿；关闭有草稿时必须经内核确认；撤权清除缓存并禁止继续使用本次授权。

## 统一聊天阶段

线上与离线共用同一应用层组合器，固定定义 `chat-turn` v1 的七个节点：`admission` → `prepare_input` → `request_wait` → `receive` → `parse` → `commit` → `publish`。每一步都通过 `moe_agent_runtime::WorkflowRuntime` 的受控 `begin/complete` 实际推进，并调用原有可信入口、会话、Agent、记忆与展示门面；不复制业务校验，也不在聊天内再嵌套 `AgentWorkflow`。`commit` 只保留真实 `AgentRuntime::finish` 回执；`publish` 由应用层在所有业务阶段之后开始，但**延后到真实展示门面渲染完成后收尾**：组合器只保存私有待发布状态（轮次与 workflow 执行票据），应用层在 `ChatView::render` 真正写入展示缓存后报告成功，否则以 `Failed` 收尾。GUI/视图类型不进入流程内核，离线在本轮 `dispatch` 内以同一方式完成收尾；线上在 `request_wait` 保留执行票据并返回，宿主收到结果后经 `complete_external` 恢复接收、解析、提交，再由应用层取得展示结果后完成 `publish`。

`ChatApplication::chat_progress()` 返回只读 `ChatTurnProgress`（定义/版本、节点状态、`committed`/`published` 与 `business_outcome`），等待期间可观察到 `request_wait` 为 `Running`；没有改阶段或回写进度的入口。`published` 只在展示门面实际渲染本轮投影后为 `true`；展示失败时 `publish` 节点为 `Failed`、`published` 保持 `false`，即使 `commit` 已成功。`business_outcome` 是 `AgentRuntime::finish` 登记的业务终态（`Completed`/`Failed`/`Cancelled`/`Unknown`），流程 `Succeeded` 与已发布展示都不代表模型结果已知；仅在真实回执存在时出现，受理前失败为 `None`。

- **解析节点**：`install_reply_parser` 注入窄 `ReplyParser`；默认 `PlainTextReplyParser` 只做有界纯文本投影，不解释标签、脚本或工具语法。`RawModelReply` 保留模型原文与表现提议，`ParsedReply` 只含展示正文与提议；原始模型正文仍只登记在运行层会话账本，不新增落盘副本。解析失败会得到实际取消回执并以 `send_cancelled` 收口；`last_parsed_reply()` 只返回展示/提议，不返回原文。
- **业务回执**：`Completed`、`Failed`、`Cancelled`、`Unknown` 都经 `AgentRuntime::finish` 登记后才更新展示状态，并原样投影为 `ChatTurnProgress.business_outcome`；未知禁止自动重发。线上完成期间撤权、预设配置版本或记忆版本变化仍由 Agent 最终复验拒绝，错误链保留。
- **展示发布与提交分离**：`commit` 回执独立保留，`publish` 节点只在展示门面真实渲染后完成。线上等待期间隐藏/关闭面板后完成提交时，节点以 `Failed` 收尾且 `published=false`，`complete_external` 返回固定 `display_failed`（`ExecutionDisposition::Settled`）；回复已登记且可在重新打开面板后读到，不重发模型、不重复登记，也不把已终结流程倒改回成功。重开只经控制器 `refresh` 从受信投影重放展示，不再触达模型或流程节点。
- **执行证据**：`AppError::execution_disposition()` 由调用点记录：受理前拒绝为 `NotExecuted`，有权威取消/终态回执为 `Settled`，无法确认或隔离为 `Unknown`；没有按 `public_code` 推断，普通 `AppError::rejection` 保持 `Unknown`。`AppError::from_memory` 默认 `Unknown`，只有宿主的纯内存记忆适配在真实服务返回值处显式标记 `Settled`，构造期失败标记 `NotExecuted`。
- **回调隔离与独立失败**：`complete_external` 先核验票据，重复/跨轮/跨实例/撤权回调在消耗 pending 前拒绝，**不执行检查点、不刷新展示、不改变正确在途请求**；其触发的存储失败不能隔离后续正确请求。已受理回调的业务、本地保存与展示失败分别保留：主错误保持原公开码与 source 链，附加失败经 `AppError::retained_error()` 可达；任一未知副作用的组合结果不会冒充 `Settled`。

## 模块结构

| 模块 | 职责 |
| --- | --- |
| `contract` | 冻结的命令、动作与快照契约（字段与动作以该模块为准）。 |
| `app` | `ChatApplication`：唯一的 `UiManager` 持有者，`new/dispatch/snapshot` 组合边界。 |
| `panels` | 聊天/预设/记忆/诊断四类类型化控制器、事件与视图；展示缓存与真实 `mount/render/unmount` 计数只由视图回调维护。 |
| `services` | 受信服务组合：`TrustedIngress`、`SessionRuntime`、应用层 `chat-turn` 组合器、`MemoryService`、`PresetRegistry`、`DiagnosticsService` 与明确标注"非真实模型"的离线验收模型。 |
| `error` | `AppError`：固定安全 `public_code`/`public_message`，原始错误链保留在 `Error::source`。 |

## 业务闭环

日用初版第一步新增记忆管理命令：`SearchMemories(query)` 经原授权读取边界返回最多 100 条匹配记录；`UpdateMemory { id, revision, text }` 与 `DeleteMemory { id, revision }` 只接受本应用明确创建的私密笔记，并由记忆核心重新校验归属、来源、保护和版本。`MemorySnapshot.revision` 用于乐观并发检查，旧版本修改/删除明确失败。查询条件在写入后保留；隐藏/关闭/撤权仍走 UI 生命周期与清理边界。这些命令目前只由原生宿主接入，回环 HTTP 白名单没有自动扩大。

- **发送**：`dispatch(Action::Send)` 要求聊天面板已打开且可见（拒绝顺序固定：未打开 → 不可见 → 空白草稿）。确定性准入（当前会话状态、执行/展示待处理、线上传输许可及存储预算）在自动代际轮转之前完成；轮转提交后仍在请求受理点复验。轮转已提交后发生的拒绝保留轮转回执，并且不标为 `NotExecuted`。每轮依次走可信入口 `admit`（固定路由/凭据/会话提示）→ `SessionRuntime::register_user` → `AgentRuntime::bind_session` → 上述七阶段 `chat-turn` 工作流；线上 `prepare_external`/`dispatch_external`，离线 `prepare`/`infer`，提交统一走 `finish`。结论只以实际 `AgentDelivery` 判定：`Completed` 显示解析后的回复；`Failed`/`Cancelled` 记为失败；`Unknown` 记为未知；流程成功从不冒充业务成功。离线回复由离线验收模型确定性生成并标注"离线验收·非真实模型"，同时体现当前预设与人设；不传凭据、端点或工具。 登记前按与会话/Agent同源配置的字符及事件预算预留本轮输入和最多4096字符回复；预留只在登记成功后记账，成功交付按实际回执退还未用回复额度。登记后、提交回执前任一阶段报错统一调用实际会话取消：取得取消回执才报告已取消；处置被拒绝或已闭合但状态无法确认时保留原始与处置错误，持有待处置交互并隔离后续发送，展示 Unknown，绝不由方法 Err 推定可安全重试。提交回执后的展示发布失败不取消已终结交互：保留提交，`publish` 节点按真实展示结果收尾，`display_failed` 不表示可重发。
- **明确记住**：`Action::Remember` 经 `MemoryService` 的受控私密写入（`UserProfile`/`UserStatement`/`Private`，绑定专用登记来源），聊天原文永不自动入库；下一轮经授权检索（`ReadPurpose::OfflineReasoning`）装入 `OfflineInput` 完成召回。记忆面板刷新使用同一授权读取边界的空查询列表语义，返回有界条目。
- **预设**：两个真实 `PresetRegistry` 离线预设（`offline-echo`/`offline-reflect`），`Action::SelectPreset` 走逐用途真实重绑，下一轮 `prepare` 解析即生效；未知 ID 明确拒绝。可见目录由已注册 `PresetsView` 的真实渲染缓存提供；隐藏/关闭清空展示缓存，重开重新装配创建预设时的同一份配置（注册表无按 ID 读取入口），当前绑定始终以注册表解析为准；不含凭据。
- **诊断**：初始化即按宿主注入的非零代际启用 `DiagnosticsService`（单一稳定域、全部类别、`Essential` 级、独立查看主体/许可/硬预算）。发送、记忆与预设切换映射为白名单运行阶段/操作/错误事件；采集句柄或写入失败不改变业务结果，健康计数经授权查询可见。外部回调只带来宿主的业务 `ModelOutcome`，没有传输回执证明请求已写出、写出前失败或远端取消，因此传输状态固定记录为 `Unknown`，不从模型正文、`Text`/`Failed`/`Cancelled` 等业务状态推断 `Written`/`Failed`/`Cancelled`；离线路径才记录有据的 `NotAttempted`。`Action::RefreshDiagnostics` 每次认证主体后取一页有界安全投影（≤8 条，事件码/严重度/业务与传输状态），游标即时关闭；条目不含提示词、回复正文、记忆正文、凭据或原始错误，运行以展示别名呈现，历史缺口经 `notice` 显式提示。
- **面板生命周期**：四面板全部经 `UiManager` 注册与驱动；`mounts/renders/unmounts` 是当前/最近实例的真实回调计数（关闭释放后重开从新实例重新计数）。隐藏保留控制器草稿并清理展示缓存；恢复重走核验边界。会话展示历史由应用持有有界交付投影，不随面板实例关闭销毁；重开重新核验并装配，草稿只在确认丢弃后清空。关闭有草稿的聊天面板返回内核确认凭据：`discard: false` 取消且不丢草稿；面板状态在确认签发后变化（如继续输入）时内核拒绝旧确认（`confirm_stale`），草稿不丢；`discard: true` 确认后释放。不可见/未打开面板的输入一律拒绝。
- **撤权**：`Action::Revoke` 先关闭业务闸门，再以 `ContextRevoked` 强制释放全部面板（控制器清敏感缓存、视图卸载清展示缓存），随后撤销入口凭据并停用诊断采集（清空诊断缓冲）。此后任何动作都以 `revoked` 拒绝；快照保持可读。
- **容量与失败**：输入按 `MAX_TEXT_BYTES`（8000 字节）拒绝，记忆正文另按 4096 字符拒绝；会话/事件/任务/诊断全部有界。失败与未知不自动重试：业务失败保留 `ChatStatus::Failed/Unknown` 与固定失败码，仅宿主显式重新派发才会重试。UI 内核驱动报告中的面板故障不吞掉：`AppError` 持有整份有界 `DriveReport`，通过 `Error::source` 可达原始回调或导航错误，不扩展 UI 内核 API、不把原始错误写入浏览器快照。会话展示投影最多128条消息，容量在业务接纳前检查；发送失败保留输入草稿，不自动重发。

## 错误与公开面

受信持久化适配器仅在确认版本冲突发生于日志追加前时可调用 `AppError::memory_revision_conflict(error)`。该窄构造固定为 `memory_revision_conflict` / `NotExecuted` 并保留原错误链；I/O 与恢复状态错误继续使用未知处置。

`AppError::public_code()/public_message()` 只返回固定安全类别（如 `empty_input`、`input_too_large`、`unknown_preset`、`panel_not_open`、`panel_not_visible`、`revoked`、`confirm_stale`、`gateway_rejected`、`memory_rejected`、`agent_failed`、`diagnostics_rejected`、`panel_callback_failed`、`stale_external_ticket`、`reply_parse_failed`、`display_failed` 等）；`Error::source` 保留网关/记忆/诊断/UI 内核的原始错误链，链上类型均不携带正文或凭据。快照 `revision` 随每次 `dispatch` 前进，且溢出检查前置于命令执行（不会执行后再报 `NotExecuted`）；`mode`/`external_transmission_allowed` 反映当前模型配置与允许状态。 取消已确认时原始阶段错误仍在主 source 链；处置失败时 `disposition_error()` 可独立检查附加处置错误，不把两类错误或未知终态吞掉。同一已受理回调的业务/存储/展示独立失败经 `retained_error()` 保留附加错误，任一未知副作用使组合结果保持 `Unknown`。`display_failed` 的主 source 链保留展示失败原始错误，且仅表示回复已提交、展示未发布（`publish` 节点与 `published` 标志同步反映）。

定向回归见 `tests/chat_app_flow.rs`（新建/切换/归档、分会话草稿、目录修订拒旧命令、归档只读、关闭确认保护，以及原有发送、记忆、预设、诊断、UI 生命周期和撤权）、`tests/external_model.rs`（截止等号外部 Unknown 隔离许可、跨会话旧票据不消费正确 pending、维护先于 checkpoint、未知取消不误报 settled，以及票据/七阶段/撤权/存储失败回归）与 `tests/memory_management.rs`。跨重启、超过 4096 轮、段容量、期限、目录和逐点故障在 `moe-desktop-adapters` 合成临时根回归；运行层、记忆核心与 UI 内核的语义由各自 crate 的测试保证。
