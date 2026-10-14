# moe-agent-runtime

API 预设、可信桌宠入口、有界会话暂存、统一离线 Agent 及原文生命周期回收区的 Rust 运行层。G0/G1/G2/G3 实现与验证见 [网关施工计划](../../docs/GATEWAY.md#预设与统一离线入口)。

使用标准库、`moe-memory-core` 的稳定身份/授权读取/上下文投影，以及关闭默认特性的 `sha2`（仅用于无正文导入账本的 SHA-256 指纹）；不反向修改记忆核心，不访问文件、网络或系统时钟。配置、凭据引用和鉴权状态只在当前进程内存在；入口本身不授予记忆读取、模型传输或工具权限，没有上游请求或磁盘原文持久化。依赖评审与当前审核见 [修复与架构审核](../../docs/REVIEW.md)。

## 动画编排 A1

`animation` 是独立于 `WorkflowRuntime` 的纯 Rust 单主体通道内核，不使用业务流程的 DAG 规则，不负责素材导入、解码、像素绘制、GUI 或模型调用。完整冻结契约与阶段门见 [现行计划](../../docs/AGENT-MEMORY-PLAN.md#首版冻结契约a02026-10-13)，无头证据见 [审核记录](../../docs/REVIEW.md#动画编排-a1独立审核2026-10-13)。合成播放器仅在测试目录中存在，不作为产品后端导出。

- `AnimationClip::new` 登记非空 ASCII ID、非零资产版本和正整数毫秒时长；不接收文件路径、格式或字节，不证明资产已加载。`AnimationSpec` 是可编辑的执行草稿；`validate_animation` 与 `AnimationDefinition::new` 共用静态校验，后者捕获不可变 `Arc` 快照，只有只读访问器，不支持在途热改。
- 动作入口、可选收尾、片段节点、`Once/Loop`、`Finish` 和四种边条件构成动作图。边及状态绑定按优先级降序、同优先级声明顺序裁决；参数/触发即时环包括不可达环均拒绝，正时长播放边界环合法。收尾子图只允许单次及至多一个完成后继，必须终结。上限为 128 Clip/动作/总节点/绑定、512 边、64 布尔参数/触发；ID 为 1–128 字节的 ASCII 字母数字或 `_`/`-`。
- 每个桌宠由宿主持有自己的 `Animator<P>`，它独占一个 `ClipPlayer`。`sync_state`、`set_parameter` 和 `trigger` 只接受受信宿主命令；未知参数/触发拒绝，未绑定的合法状态请求正常退出。相同选择不重播已经自然结束的动作；显式 `request_action` 可重播。重复状态/选择二元组不撤销显式候选，真正改选可替换它；只保留一个最新候选，每种触发至多一份且不跨动作继承。
- 显式请求返回 `Accepted` 或无副作用的 `RejectedLowerPriority`。同优先级等待正常退出，高优先级按新动作的 `Immediate/AfterExit` 策略；状态改选不会被旧优先级无限阻挡。`request_exit` 等当前单次完成或循环边界进入可选收尾；已在收尾中不重入，普通退出、同优先级及 `AfterExit` 不截断它，只有有效高优先级 `Immediate` 可抢占。`stop` 独立强停，不播放收尾或代表业务回滚。
- `prepare` 必须核对具体资产/模式/能力且不停止旧播放；准备成功后仍必须取得旧票据的停止确认才能 `start`。每次开始的 `PlaybackTicket` 私有构造，绑定实例、定义版本、Clip/资产版本和单调序号，不复用耗尽序号。只允许同步回传收到的票据，没有任意外部事件提交入口；保证跨播放隔离，不保证同一播放内操作级防重放。
- `ClipPlayer::advance` 是游标/时间边界唯一权威，每次回传实际消耗及至多一个模式匹配的边界；无边界消耗全部步长，有边界消耗正时间且不超步长。`Animator::advance` 续传后继余量，每次最多处理 128 次转换/边界，返回 `Yielded` 并保留余量，可用零增量继续；累加溢出先拒绝且不改实例。成功强停、无后继终止及故障确认停止清空旧余量、候选、触发，新请求不会借零增量消费旧债。
- `pause/resume` 成功重复调用幂等。暂停冻结游标和余量，正增量拒绝、零增量只查询，不积累时间；状态、参数、触发和显式请求只更新待处理意图，不准备、启动或转换，恢复后处理。不支持暂停明确返回 `PauseUnsupported`；强停不受暂停限制并清除暂停标记。普通命令只处理零时间意图，不排空已有时间债。
- `AnimationError<P::Error>` 保留后端原类型和 `Error::source` 错误链；后端错误/非法回报进入故障锁定，不自动重试开始或推进，只允许显式 `stop` 确认清空后重新接收动作。`snapshot` 返回版本、动作/节点/票据、状态/选择/候选、暂停/故障/退出标记及余量的独立只读报告；`playback_progress` 校验后端只读位置及票据，不开放后端可变访问。

下面只构造不可变动作定义，不伪造实际播放或呈现成功：

```rust
use moe_agent_runtime::animation::{
    AnimationAction, AnimationBinding, AnimationClip, AnimationDefinition,
    AnimationNode, AnimationSpec, Animator, ClipPlayer, PlaybackMode, PreemptionPolicy,
};

let definition = AnimationDefinition::new(AnimationSpec {
    version: 1,
    clips: vec![AnimationClip::new("rest", 1, 40)?],
    actions: vec![AnimationAction {
        id: "resting".into(), entry: "body".into(), exit: None, priority: 0,
        preemption: PreemptionPolicy::AfterExit,
        nodes: vec![AnimationNode {
            id: "body".into(), clip: "rest".into(), mode: PlaybackMode::Loop,
            edges: vec![],
        }],
    }],
    parameters: vec![], triggers: vec![],
    bindings: vec![AnimationBinding {
        state: "rest".into(), parameter: None, action: "resting".into(), priority: 0,
    }],
})?;
assert_eq!(definition.version(), 1);

fn attach_host_player<P: ClipPlayer>(definition: AnimationDefinition, player: P) -> Animator<P> {
    Animator::new(definition, player)
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

A2 素材层和实际显示未纳入本次主干集成；下一阶段须另行明确范围与验收。

播放票据与发布定义不开放修改入口：

```compile_fail
use moe_agent_runtime::animation::PlaybackTicket;
fn rewrite(ticket: &mut PlaybackTicket) {
    ticket.sequence = 1;
}
```

```compile_fail
use moe_agent_runtime::animation::AnimationDefinition;
fn rewrite(definition: &mut AnimationDefinition) {
    definition.actions()[0].entry = "another-node".into();
}
```

## 预设管理

- `PresetConfig::new` 校验显示名、协议标识、端点、凭据引用、模型及扩展参数；只描述配置，不表示有协议适配器或供应商可用性证明。
- `PresetRegistry::new` 的容量为 1–256；`create/update/delete` 返回或检查单调版本。失败不发布状态，删除再建不复用旧版本；版本仅在当前注册表实例内有意义。
- `bind/unbind/resolve` 对八种 `PresetPurpose` 独立配置：保留原有 Chat、ImageGeneration、PersonaInterview、Vision、Voice、MemoryExtraction 映射，并新增 Embedding、Rerank；返回实际预设 ID、版本和只读配置。缺失为 `MissingBinding`，悬空为 `PresetNotFound`，没有隐式跨供应商回退。
- Embedding/Rerank 必须各自显式绑定；Chat 绑定或其凭据引用不会自动绑定、授权这两种用途。用途选择和 API 凭据都不构成模型外传许可，传输仍须由宿主按用途、端点、数据范围与来源另行授权。
- 端点允许 HTTPS 或字面回环 HTTP，拒绝认证信息、查询、fragment、含混 authority 和非法端口；不解析 DNS、不探测服务，也不保证未来目标可达或允许传输。
- 扩展字段仅接受已校验的 `temperature`、`top_p`、`frequency_penalty`、`presence_penalty`、`max_tokens`、`seed` 和 `reasoning_effort`；不接受任意嵌套对象或宿主字段。此集合是离线配置范围，不宣称每个供应商支持每个字段；真正 payload 构造前仍需按协议/模型校验。
- `CredentialRef` 仅为受信宿主的 opaque 引用，没有供应商密钥保存、解析或导出；配置、扩展值及引用默认 Debug 脱敏，显式只读访问器供受信宿主使用。

## 可信入口

`TrustedIngress` 由受信宿主配置路由及凭据，`RawIngressCall` 仅包含 token、别名、用户正文、可选会话提示/客户端请求 ID 和不支持能力声明。没有外部自填的 owner、说话者、渠道、权限或回复目标。纯文本中包含指令仍是用户正文，不会变成宿主角色；实际 HTTP 请求解析不在 G0 范围。

`admit` 依次鉴权、校验路由及白名单、拒绝不支持能力、校验正文和提示，再分配唯一序号。正文最多 16,384 字符，不改写原文；不按相同正文或客户端请求 ID 自动去重。无会话提示每次独立；有提示则按运行实例、凭据、主体、完整桌宠身份、渠道及渠道范围隔离。

`TrustedRequest::validate` 复验运行实例证明及配置代际。任一路由/凭据变更保守失效全部旧请求；异实例返回 `ForeignRequest`，配置变化/撤销返回 `Revoked`。访问器只给不可变快照，旧快照不能追回；每次实际使用前仍必须调用验证。`IngressAdmission` 只是零权限标记，不是可替代请求复验的授权票据。

`CredentialId` 的私有构造同时绑定签发运行实例和局部序号，比较/查表使用完整句柄；`value()` 仅用于本实例内关联，不能当跨实例凭据标识或恢复授权。`revoke_credential` 先检查签发实例，跨实例或重建前的旧句柄返回 `ForeignRequest`，不会撤销本实例同序号凭据、烧毁 token 或推进配置代际。

`IngressLimits` 默认最多 256 条路由、256 个活跃凭据、累计签发 512 个凭据；`new` 默认把累计限额设为活跃限额两倍，`with_issuance_limit` 可显式配置。撤销 token 以精确值保留在有界进程内墓碑中，不采用可能碰撞的自制摘要；墓碑计入累计签发限额，不能借撤销释放预算无限增长。预算满仍可撤销，但新签发拒绝，需受信宿主另建运行实例并重新配置/鉴权。运行序号、凭据序号、配置代际和运行实例编号溢出均不复用旧值。

## 离线示例

```rust
use std::collections::BTreeMap;
use moe_agent_runtime::{
    CallerSubject, ChannelId, ChannelScope, IngressToken, PresetConfig, PresetId,
    PresetPurpose, PresetRegistry, RawIngressCall, ReplyTarget, TrustedIngress,
};
use moe_memory_core::{Namespace, PetIdentity};

let mut presets = PresetRegistry::new(4)?;
let preset_id = PresetId::new("synthetic-provider")?;
let config = PresetConfig::new(
    "Synthetic", "chat-completions", "https://api.example.com/v1", None,
    "synthetic-model", BTreeMap::new(),
)?;
presets.create(preset_id.clone(), config)?;
presets.bind(PresetPurpose::Chat, preset_id.clone())?;

let pet = PetIdentity::new(Namespace::new("workspace", "owner")?, "stable-pet")?;
let mut ingress = TrustedIngress::new();
ingress.register_route("buddy/synthetic", pet.clone())?;
let credential = ingress.install_credential(
    IngressToken::new("synthetic-token-not-for-production")?,
    CallerSubject::new("caller-not-owner")?,
    ["buddy/synthetic".to_owned()],
    ChannelId::new("external-api")?,
    ChannelScope::new("private-thread")?,
    ReplyTarget::new("original-target")?,
)?;
let request = ingress.admit(RawIngressCall::new(
    IngressToken::new("synthetic-token-not-for-production")?,
    "buddy/synthetic", "合成用户输入",
).with_session_hint("client-conversation"))?;
request.validate(&ingress)?;
assert_eq!(request.identity(), &pet);
assert_eq!(presets.resolve(PresetPurpose::Chat)?.id(), &preset_id);
ingress.revoke_credential(credential)?;
assert!(request.validate(&ingress).is_err());
# Ok::<(), Box<dyn std::error::Error>>(())
```

外部调用方不能自行构造可信请求：

```compile_fail
use moe_agent_runtime::TrustedRequest;
let forged = TrustedRequest {};
```

入口 token 不提供公开的原始值访问器：

```compile_fail
use moe_agent_runtime::IngressToken;
let token = IngressToken::new("synthetic-token").unwrap();
println!("{}", token.0);
```

## G1 会话与暂存

`SessionRuntime::new` 由宿主明确提供 `SessionLimits`、`SessionTiming` 和当前进程的单调毫秒数。容量参数依次为保留片段数、每片段事件数、全局事件数、单条正文字数、全局正文字数、全局在途批次数，全部必须为正；没有隐式无限容量。正文按 Unicode 字符计数，不是 token 或实际堆字节预算；每条元数据由既有标识长度及事件数共同约束。已到期、已确认或已撤销的片段仍占容量；不自动驱逐证据，满额返回 `CapacityExceeded`。当前没有删除、回收区、恢复或磁盘格式。

每次操作传入 `SessionAccess::new(&ingress, &request, now_ms)`；这只是注入参数，不是新授权。所有运行操作重新验证 G0 私有请求、入口实例、配置代际、会话隔离键以及当前协调器的不透明句柄。时间回退拒绝；失败不推进时间、计数器、正文、终态或水位。每个宿主运行使用一个协调器管理其全部会话并发；不同协调器互不接受句柄，不提供跨进程分布式配额。

- 默认活跃 1,800,000 毫秒。`register_user` 只从 G0 请求取得正文；`UserEventKind::New` 且接入序号/可选受信来源顺序均为新、成功登记时才滑动续期。来源时间单独保存，不能决定活跃期限。历史与乱序事件保留用户历史角色并标记 `Unknown`，不启动新任务或伪造历史执行成功。
- 同一可信请求重试，或同隔离域内相同 `HostEventId` 的一致重传，返回原登记且不续期；冲突返回 `EventConflict`。客户端 request ID 只存为提示，正文相同不判重。宿主事件 ID 和来源顺序只能来自经核验的适配器，不能把客户端提示直接包装成受信事实。无会话提示仍遵循 G0 每次接纳独立。
- `read_active` 在恰好到期时拒绝；之后的新用户事件创建新片段，不带入旧原文。历史重传不复活旧片段；撤销或配置升版后的旧来源 ID 也不能重放成新事件。进程重启不恢复任何会话、原文或单调时间。
- `append_related` 保留 Assistant、ExternalAgent、ToolReceipt、InjectedContext、Background 来源，不允许改成 User。它只登记受信宿主提供的事件，绝不证明工具执行或授予受保护角色。新登记的相关事件必须属于尚未终结且未过任务期限的原交互。
- `finish_interaction` 原子登记实际回复及 Completed/Failed/Unknown 终态，返回固定原请求 `ReplyTarget`；相同终态/回复重试不重复正文。回执仅确认宿主登记，不是网络发送或工具执行成功证明。默认任务期限为登记后 60 分钟，可显式配置；会话到期不强制取消期限内在途任务，但其结果只归原片段/目标，不能复活活跃状态或写入新片段。
- `cancel_interaction` 幂等阻断后续任务输出并失效该片段旧批次；`revoke_session` 保守撤销整个片段的读取与写回。原文纠正/删除/来源撤权应先撤销受影响片段，再以新来源建立新片段；G1 不提供原地改写、物理擦除或可恢复删除。取消不是成功，已交付快照无法追回。

### 离线闲时票据

`issue_idle_batch` 要求同一逻辑会话至少安静 180,000 毫秒、该片段全部相关交互终结且宿主显式允许预算；安静下限不可配置为更短。同逻辑会话的旧/新片段也串行，全部隔离会话受配置的全局并发限制。未终结或预算拒绝返回 `NotReady`，已有任务返回 `BatchInFlight`，并发满返回背压。

票据绑定协调器、片段、发起请求、版本、水位和固定事件闭区间；默认批次期限 5 分钟，可显式配置，到截止即拒绝读/确认。`read_batch` 只返回该范围，新消息不进入在途批次。批次可以处理获准保留但不再活跃的旧片段，不能把它重新注入当前聊天；持有旧票据也不能确认新片段事件。

`acknowledge_batch` 对每个预期事件显式 Confirmed 或 Skipped；同值重复幂等、冲突拒绝，全部确认才推进至固定终点。G1 没有候选提炼或长期提交，这些是离线事件级处理确认，不是持久化提交凭据。部分确认留在有界原事件中；`cancel_batch` 或到期后重发新票据时继续显示已确认项，失败不推进水位。旧版本、旧票据、异请求、撤权和迟到结果均拒绝；确认成功不自动删除原文。

### 合成会话示例

```rust
use moe_agent_runtime::{
    BatchAcknowledgement, CallerSubject, ChannelId, ChannelScope, IngressToken,
    RawIngressCall, ReplyTarget, SessionAccess, SessionLimits, SessionRuntime,
    SessionTiming, TerminalState, TrustedIngress, UserEvent, UserEventKind,
};
use moe_memory_core::{Namespace, PetIdentity};

let mut ingress = TrustedIngress::new();
ingress.register_route("buddy/demo", PetIdentity::new(
    Namespace::new("synthetic-workspace", "synthetic-owner")?, "synthetic-pet",
)?)?;
ingress.install_credential(
    IngressToken::new("synthetic-session-token")?, CallerSubject::new("caller")?,
    ["buddy/demo".to_owned()], ChannelId::new("offline")?,
    ChannelScope::new("private-thread")?, ReplyTarget::new("original-target")?,
)?;
let request = ingress.admit(RawIngressCall::new(
    IngressToken::new("synthetic-session-token")?, "buddy/demo", "合成用户原文",
).with_session_hint("synthetic-conversation"))?;
let mut sessions = SessionRuntime::new(
    &ingress, SessionLimits::new(8, 64, 256, 16_384, 65_536, 2)?,
    SessionTiming::default(), 0,
);
let registration = sessions.register_user(
    SessionAccess::new(&ingress, &request, 0), UserEvent::new(UserEventKind::New),
)?;
let receipt = sessions.finish_interaction(
    SessionAccess::new(&ingress, &request, 1), &registration.interaction,
    TerminalState::Completed, Some("宿主登记的合成实际回复"),
)?;
assert_eq!(receipt.reply_target.as_str(), "original-target");
let ticket = sessions.issue_idle_batch(
    SessionAccess::new(&ingress, &request, 180_000), &registration.session, true,
)?;
for event in sessions.read_batch(SessionAccess::new(&ingress, &request, 180_000), &ticket)? {
    sessions.acknowledge_batch(
        SessionAccess::new(&ingress, &request, 180_000), &ticket,
        event.id(), BatchAcknowledgement::Skipped,
    )?;
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

会话、交互及批次证明都不能由外部输入构造：

```compile_fail
use moe_agent_runtime::SessionId;
let forged = SessionId {};
```

```compile_fail
use moe_agent_runtime::{InteractionId, EventId};
fn forge(event: EventId) -> InteractionId { InteractionId(event) }
```

```compile_fail
use moe_agent_runtime::BatchTicket;
let forged = BatchTicket {};
```

## G2 统一离线 Agent

`AgentRuntime` 只接已接纳的 `TrustedRequest`、G1 用户交互和宿主注入的权威记忆访问。`AgentAccess` 每次带入当前入口、预设注册表和进程内单调毫秒时间；`AgentServices<'a, M = MemoryService>` 借用当前会话协调器及实现 `MemoryContextAccess` 的记忆门面。默认 `M` 仍为 `MemoryService`；也可直接注入 `LocalMemory` 或 `dyn MemoryContextAccess<Error = E>`，不要求暴露存储 reader。宿主必须始终传入当前权威服务，不能以历史服务克隆代替撤权后的状态。

`AgentError<E = MemoryError>` 的 `Memory(E)` 保留注入门面的原始错误类型与错误链，例如 `LocalMemory` 的 `PersistenceError::RecoveryRequired`；健康故障不会退化为关键词绕行或无记忆模型调用。

### 授权与上下文

- `bind_session` 由受信宿主显式给每个活跃片段安装精确 `SourceRef` 白名单，包含人设权威来源；至多 256 项。它不是读取授权的替代品：既有 `ReadPolicy::offline` 和片段来源许可必须同时满足。同片段同许可重复绑定幂等，不允许原地改写或扩大许可；`revoke_binding` 幂等立即阻止该绑定的使用及迟到结果。
- `AgentBinding::memory_session` 是本运行实例为该 G1 片段唯一分配的记忆会话 ID，供宿主安装已有 `WorkingContext`；不从客户端 hint 或渠道名称拼接。不同渠道、凭据、说话者、桌宠及新片段不共用原文或工作区，重建 Agent 也不复用旧工作区。长期记忆只在宿主对该渠道显式授权后参与；共用稳定桌宠不继承其它渠道的来源许可。
- `prepare` 固定原交互、片段及角色保真的近期事件，通过 `MemoryContextAccess::begin_memory_task` 使用 `OfflineReasoning` 开始真实记忆任务，再用同一窄契约的 `assemble_context` 装配请求投影。受保护人设必须获准且存在，缺失返回 `PersonaUnavailable`，不退化成裸模型调用。
- 核心先做现有用途/来源/归属授权及检索；运行层随后在任何模型使用前检查人设、**完整实际读取集**及每份 `transmissions().sent_sources()` 是否均在片段许可内。传输检查包含查询来源和未入选候选；预算遗漏或未进入最终 top-k 的项仍不能绕过来源授权。超出时整体返回 `MemoryScopeDenied`，不默默裁掉、扩大授权或重检索较弱证据。
- `AgentLimits::new` 显式配置绑定总量、累计任务签发量、记忆投影字符预算及回复字符预算；均须为正。任务签发记录用于阻止同交互重复推理，累计预算耗尽后拒绝新任务，但仍允许撤销。记忆投影预算不含 G1 已有独立上限约束的会话原文，也不是 token 或总提示词预算。

### 推理与返回

`infer` 只向可注入 `OfflineModel` 提供只读 `OfflineInput`：稳定身份、当前可信渠道/范围、原始输入、人设、获准记忆和固定事件快照，以及实际 Chat 预设 ID/版本。人设与记忆在**同一次推理前**参与，不事后二次润色。不传供应商密钥、端点、鉴权头或可执行工具，`tools_enabled()` 始终为 false。离线 trait 是受信宿主夹具边界，不是沙箱；不能把网络模型包装成夹具来绕过 `ModelTransmission` 的默认拒绝。

`infer` 和 `finish` 都重验可信请求/配置、片段版本/取消/任务期限和核心请求投影；核心再校验精确读取集、来源、记录/身份/工作区/策略版本及全库取消代际。预设注册表使用不可复用的进程内配置证明，改绑后恢复旧绑定、删除重建或换同版本注册表也使旧任务失效；失败的配置变更不失效任务。原始事件追加不进入已经捕获的任务，来源纠正/撤权须沿用 G1 的片段撤销规则。

`AgentResult` 仅由 `infer` 创建且绑定唯一任务，不公开未验收正文。`finish` 最终复验后才原子登记 G1 终态并返回 `AgentDelivery`；固定原渠道/目标，文本逐字返回，`PresentationEvent` 是独立的有限提示列表，不执行动画/语音/工具。空白、NUL、超长回复或超过 16 项表现提示转换为 `Failed(InvalidOutput)`；模型失败、取消、未知均无回复正文或表现事件，绝不登记 Completed。G1 背压失败不发布回复或成功终态，可以用同一结果重试最终登记，不重复推理。

会话活跃到期不取消此前已准备且仍在任务期限内的工作；结果只能回原片段，不能复活其活跃状态或写入新片段。取消/撤权不是可重试成功，已交付的快照不能追回。Completed 只证明本次离线纯文本返回和内存终态登记，不证明外部网络送达、工具执行、长期记忆提交或表现执行。模块不产生日志，所有新增任务/结果/输入/返回类型的 Debug 隐去正文、凭据及回复目标；显式正文访问器仅供受信调用方使用。

### 合成离线示例

```rust
use std::collections::BTreeMap;
use moe_agent_runtime::*;
use moe_memory_core::{Category, Epistemic, MemoryDraft, MemoryScope, MemoryService,
    Namespace, PetIdentity, ReadPolicy, RetrievalQuery};

struct FixtureModel;
impl OfflineModel for FixtureModel {
    fn infer(&mut self, input: OfflineInput<'_>) -> ModelOutcome {
        assert_eq!(input.persona(), "合成人设");
        assert!(!input.tools_enabled());
        ModelOutcome::Text {
            text: format!("{}：{}", input.persona(), input.memory()[0].text()),
            presentation: vec![PresentationEvent::Neutral],
        }
    }
}

let identity = PetIdentity::new(Namespace::new("fixture", "owner")?, "pet")?;
let mut ingress = TrustedIngress::new();
ingress.register_route("buddy/demo", identity.clone())?;
ingress.install_credential(IngressToken::new("synthetic-offline-token")?,
    CallerSubject::new("caller")?, ["buddy/demo".to_owned()], ChannelId::new("offline")?,
    ChannelScope::new("private")?, ReplyTarget::new("original-target")?)?;
let request = ingress.admit(RawIngressCall::new(
    IngressToken::new("synthetic-offline-token")?, "buddy/demo", "合成用户输入"))?;
let mut presets = PresetRegistry::new(4)?;
let preset = PresetId::new("fixture-chat")?;
presets.create(preset.clone(), PresetConfig::new("fixture", "offline-fixture",
    "https://example.invalid/v1", None, "fixture-model", BTreeMap::new())?)?;
presets.bind(PresetPurpose::Chat, preset)?;
let mut sessions = SessionRuntime::new(&ingress,
    SessionLimits::new(8, 32, 64, 4096, 16384, 2)?, SessionTiming::default(), 0);
let registration = sessions.register_user(SessionAccess::new(&ingress, &request, 0),
    UserEvent::new(UserEventKind::New))?;
let mut memory = MemoryService::new();
let authority = memory.register_source(MemoryScope::Private(identity.clone()),
    "persona", 1, Epistemic::AuthoritativeState)?;
let source = memory.register_source(MemoryScope::Private(identity.clone()),
    "fact", 1, Epistemic::UserStatement)?;
memory.set_identity_view(&identity, 1, "合成人设", vec![authority.clone()])?;
memory.for_pet(&identity).create(MemoryDraft::new("合成获准事实", Category::Episode,
    Epistemic::UserStatement, vec![source.clone()]), 0)?;
memory.set_read_policy(&identity, ReadPolicy::offline(1,
    vec![authority.clone(), source.clone()]))?;
let mut agent = AgentRuntime::new(&ingress, AgentLimits::new(8, 64, 4096, 4096)?)?;
let access = AgentAccess::new(&ingress, &request, &presets, 0);
let binding = agent.bind_session(&access, &mut sessions, &registration.session,
    vec![authority, source])?;
let mut workflow = AgentWorkflow::start()?;
workflow.prepare(&mut agent, &access, AgentServices::new(&mut sessions, &mut memory),
    &binding, &registration.interaction, &RetrievalQuery::new(""))?;
workflow.infer(&agent, &access, AgentServices::new(&mut sessions, &mut memory),
    &mut FixtureModel)?;
let delivery = workflow.finish(&agent, &access,
    AgentServices::new(&mut sessions, &mut memory))?;
assert_eq!(workflow.progress().status, WorkflowStatus::Succeeded);
assert_eq!(delivery.state(), TerminalState::Completed);
assert_eq!(delivery.text(), Some("合成人设：合成获准事实"));
assert_eq!(delivery.reply_target().as_str(), "original-target");
# Ok::<(), Box<dyn std::error::Error>>(())
```

绑定、任务和模型结果不能由外部构造，也不能序列化为可恢复授权：

```compile_fail
use moe_agent_runtime::AgentBinding;
let forged = AgentBinding {};
```

```compile_fail
use moe_agent_runtime::AgentTask;
let forged = AgentTask {};
```

```compile_fail
use moe_agent_runtime::AgentResult;
let forged = AgentResult {};
```

## 程序流程执行层

`WorkflowDefinition` 由受信 Rust 程序使用静态标识与 `WorkflowStep` 定义；没有模型输入、脚本、表达式或反序列化入口。定义 ID/节点 ID 限 1–128 个 ASCII 字母、数字、`-_.`，版本必须非零，节点数为 1–`MAX_WORKFLOW_STEPS`（128）。构造时拒绝重复节点、缺失/重复前置与循环（含自依赖），成功定义不可变；`start` 捕获定义快照，新版本不改变在途实例。

`WorkflowRuntime::new(capacity)` 显式提供正的累计实例容量；终态仍计入容量，不自动驱逐证据或复用句柄。每个实例仅允许一个在途节点；`begin(run, step_id)` 必须等于当前第一个已满足全部成功前置的待执行节点，同为就绪时按定义声明顺序推进。允许向前引用前置节点，但不是条件分支，所有节点都需执行；一个实例等待结果不阻塞另一个实例。

`begin` 返回私有构造的 `WorkflowExecution`，`complete` 只接受该具体实例/节点执行的票据。跨运行协调器、重复、过期或取消后票据均拒绝；失败、未知或取消立即停止，不自动重试或允许跳节点。`cancel` 对已取消实例幂等，不改写其他终态，也不撤销宿主已发生的副作用。`progress` 返回独立元数据快照，修改快照不修改运行状态；定义/票据/进度无正文或凭据，流程状态不是业务提交、外部送达或工具执行回执。

```rust
use moe_agent_runtime::{WorkflowDefinition, WorkflowOutcome, WorkflowRuntime,
    WorkflowStatus, WorkflowStep};

let definition = WorkflowDefinition::new("synthetic-flow", 1, vec![
    WorkflowStep::new("prepare", &[]),
    WorkflowStep::new("finish", &["prepare"]),
])?;
let mut runtime = WorkflowRuntime::new(8)?;
let run = runtime.start(&definition)?;
let prepare = runtime.begin(&run, "prepare")?;
runtime.complete(&prepare, WorkflowOutcome::Succeeded)?;
let finish = runtime.begin(&run, "finish")?;
runtime.complete(&finish, WorkflowOutcome::Succeeded)?;
assert_eq!(runtime.progress(&run)?.status, WorkflowStatus::Succeeded);
# Ok::<(), Box<dyn std::error::Error>>(())
```

```compile_fail
use moe_agent_runtime::WorkflowRun;
let forged = WorkflowRun {};
```

```compile_fail
use moe_agent_runtime::WorkflowExecution;
let forged = WorkflowExecution {};
```

`AgentWorkflow::start()` 固定定义 `offline-agent` v1 的 `prepare → infer → finish`，将 G2 私有任务与结果保留在包装器内；上述合成离线示例经过该入口。每步仍调用原 `AgentRuntime` 方法，沿用当前来源、权限、预设、版本、时间与最终提交复验；`AgentWorkflowError<E>::Agent` 保留原 `AgentError<E>` 和后端错误链，不新增传输、查看或工具权限。顺序错误在业务调用前拒绝，原 G2 直连 API 不变。

`infer` 节点成功仅表示获得待裁决的模型结果；模型失败/取消/未知仍须执行 `finish`，由会话账本登记实际终态后映射为流程 Failed/Cancelled/Unknown，不伪造成功。任何 Agent 方法返回错误即停止该包装器，不重试推理或最终登记；与原 G2 可显式重试背压的直连入口不同，宿主须另外处置未终结交互，流程 Failed 不冒充会话 Failed 回执。准备后调用 `cancel(access, sessions)` 先走 G1 授权取消，成功才停止流程并丢弃私有任务/结果；授权取消失败保留进度。准备前仅取消本地流程，重复已取消调用不重交账本。

内核无 I/O、系统时钟、全局活动角色或新增依赖；业务执行由宿主完成。没有并行调度、复杂分支/循环、自动重试、跨重启恢复或运行日志接线，不改变 G6–G8 暂停及 R4 真实门未通过的事实。

## G3 原文生命周期与本机回收区

G3 是宿主显式启用的运行层有界内存契约；`SessionRuntime::new` 的 G1/G2 行为保持不变，未配置留存时所有生命周期与管理 API 返回 `RetentionNotConfigured`，且不会隐式安装任何期限、回收区或本机查看许可。

- `SessionRuntime::with_retention` 由宿主提供 `RawRetention`：正常留存毫秒、回收留存毫秒与回收区片段/事件/字符容量全部必填正值，没有内置 24 小时或 7 天默认值。任何期限、版本、计数或代际运算溢出都明确失败，不发布半更新。
- 首次成功登记原文的受信时间固定为 `created_at`；初始正常截止为 `created_at + normal_ms`，硬截止为再加 `recycle_ms`。新消息、回复、读取、整理确认与重复删除都不延长既有片段期限；自然到期的逻辑回收时间固定为原正常截止，迟到维护不重算新的完整回收窗口。首次进入回收区把最终截止固定为 `min(硬截止, 逻辑回收时间 + 回收时长)`，早删取删除时刻窗口；恢复后正常截止为 `min(恢复时刻 + normal_ms, 最终截止)`；重复删除/恢复不延长最终截止。边界统一 `now >= deadline` 即失效；活跃 TTL、任务期限、正常留存与回收留存是四个独立概念。
- 转换为：正常区 --正常到期/获准软删除--> 回收区；回收区 --显式恢复且未到最终截止--> 正常区（新生命周期代际）；任一区 --最终到期/获准清空/撤权清理--> 清理。每次实际转换推进不可复用的生命周期版本与协调器代际；片段与事件 ID 不变，清理后旧句柄永远返回 `StaleHandle`，ID 不复用。回收使旧在途批次票据与 G2 任务立即失效并释放批次并发占额，恢复后也不重新有效；回收时未终结交互强制置为 `Cancelled`，活跃截止清空，恢复不激活历史会话、不恢复旧交互执行权。
- 正常到期即对全部读取、事件登记、交互完成、批次签发/读取/确认及 G2 绑定/准备/推理/最终登记生效，不依赖维护或定时器。管理读取同样不依赖维护：`inspect_fragment` 对逻辑到期待转移与回收区内的片段仍返回快照，到达最终截止（含未维护的正常区硬截止）即返回 `RawExpired`，`soft_delete_fragment` 对最终到期片段拒绝并保持原子；`list_managed_fragments` 以 `logically_expired` 与 `final_expired` 区分"正常到期待转移"与"最终到期"。回收区满额时保持"逻辑到期、待转移"的原区计费状态并显式背压；`maintain_retention` 是可重复调用的受信维护入口，必须以当前权威 `TrustedIngress` 调用：先对原片段授权代际落后于当前配置的受管理原文执行撤权清理（计入 `authorization_removed`），再转移到期片段、移除最终到期内容并释放容量。多片段处理没有全局事务，单片段转换原子；维护报告分别计入转移、清理、撤权清理与待转移，`released_events`/`released_chars` 累计两种实际清理路径释放的容量；统计溢出先于该单项清理拒绝。
- 本机管理能力由受信宿主在本机主人校验后用 `install_management_capability` 显式安装：私有构造，绑定协调器、入口运行实例、签发配置代际、完整桌宠身份、原隔离域与精确片段范围；范围内任一片段的原始授权代际落后于当前权威配置时整体拒绝（`Revoked`），失败不产生能力记录。`ManagementPermissions` 默认无权限，查看/软删除/恢复/清空分别裁决，账本上限 64。仅持入口 token 或 `TrustedRequest` 不能取得管理权限；管理操作同时要求当前权威入口配置有效（换配置代际返回 `VersionConflict`）并核验目标片段的原始授权代际（落后返回 `Revoked`），不能用重新签发的能力或历史配置恢复旧授权；异协调器、异入口运行、跨宠、跨凭据/渠道/范围一律 `ForeignRequest` 且不改变状态；撤销幂等立即生效。
- 管理读取返回只读快照；快照、能力、错误与状态的 Debug 不含正文、token 或凭据。恢复要求有效能力、预期生命周期版本、未到最终截止与正常区全部容量；容量不足原子失败且回收区内容、截止与水位不变。恢复保留原始事件身份、正文与确认水位，不重建记忆来源（`HostEventId` 不是 `SourceRef`）；再次处理未确认内容需显式签发新批次并通过全部现行检查。启用 G3 后 `revoke_session` 即撤权清理，立即移除受管理原文；清理同时保留与正文分离的请求序号及隔离域内宿主事件 ID 栅栏，不依赖可选 `HostEventId`。两类账本分别以正常区全局事件数为累计上限；任何一次清理无法容纳全部新增栅栏时，清理仍成功但永久关闭本协调器的新用户/关联事件接纳，返回 `ReplayProtectionExhausted`，不能静默丢弃后继续放行。已知撤销请求/来源返回 `Revoked`；正常未到期片段的严格重复回执仍可返回，但不新增事件或执行权。查看、恢复、维护、清空和原有交互的终态登记继续按各自授权/容量检查工作；重新提供入口需要受信宿主使用新入口运行实例重新鉴权，不复用旧请求。无跨重启或永久外部来源去重保证。
- 清理另按完整 `RequestIsolation` 保留接入序号和来源顺序的最大水位，每隔离域最多一条、不含正文，隔离域条目累计上限为正常区全局事件数。登记将清理水位与尚存片段水位取最大值；尚未登记的迟到请求或乱序来源仍为 `UserHistory`，不续活跃期或获得回复执行权。跨渠道/范围/主体/凭据/桌宠/会话提示不共用水位，缺失来源顺序不会抹去已有最大值，乱序清理不会回退水位。水位预算耗尽也执行上述永久拒绝策略；清空或恢复不能重置。
- 清理只覆盖受管理在线正文、重试正文副本与批次缓存，释放所在区域片段/事件/字符容量，不留正文墓碑；已交付的快照、模型输入与平台消息无法追回，也不承诺 RAM/磁盘安全擦除、备份清理或跨重启去重。清理会话原文不调用长期事实删除或共享撤回。

### 生命周期示例

```rust
use moe_agent_runtime::{
    CallerSubject, ChannelId, ChannelScope, IngressToken, ManagementPermissions,
    RawIngressCall, RawRetention, ReplyTarget, RetentionZone, SessionAccess, SessionLimits,
    SessionRuntime, SessionTiming, TrustedIngress, UserEvent, UserEventKind,
};
use moe_memory_core::{Namespace, PetIdentity};

let mut ingress = TrustedIngress::new();
ingress.register_route("buddy/demo", PetIdentity::new(
    Namespace::new("synthetic-workspace", "synthetic-owner")?, "synthetic-pet",
)?)?;
ingress.install_credential(
    IngressToken::new("synthetic-lifecycle-token")?, CallerSubject::new("caller")?,
    ["buddy/demo".to_owned()], ChannelId::new("offline")?,
    ChannelScope::new("private-thread")?, ReplyTarget::new("original-target")?,
)?;
let request = ingress.admit(RawIngressCall::new(
    IngressToken::new("synthetic-lifecycle-token")?, "buddy/demo", "需要留存的合成原文",
).with_session_hint("synthetic-conversation"))?;
// 两段留存与回收区容量全部由宿主显式配置，无默认天数。
let mut sessions = SessionRuntime::with_retention(
    &ingress, SessionLimits::new(8, 16, 32, 4096, 16_384, 1)?,
    SessionTiming::default(), RawRetention::new(1_000, 2_000, 4, 64, 8_192)?, 0,
);
let registration = sessions.register_user(
    SessionAccess::new(&ingress, &request, 0), UserEvent::new(UserEventKind::New),
)?;
// 宿主完成本机主人校验后安装最小能力；此处为示例一并授予四项权限。
let capability = sessions.install_management_capability(
    &ingress, request.identity().clone(), request.isolation().clone(),
    vec![registration.session.clone()],
    ManagementPermissions::default().with_view().with_soft_delete()
        .with_restore().with_purge(),
)?;
let deleted = sessions.soft_delete_fragment(
    SessionAccess::new(&ingress, &request, 100), &capability,
    &registration.session, 0,
)?;
assert_eq!(deleted.zone, RetentionZone::Recycle);
// 回收区内仍可查看，但使用路径全部拒绝。
let snapshot = sessions.inspect_fragment(
    SessionAccess::new(&ingress, &request, 100), &capability, &registration.session,
)?;
assert_eq!(snapshot.events[0].body(), "需要留存的合成原文");
// 回收清空活跃截止：读取返回会话非活跃；完成/批次等使用路径返回 Recycled。
assert_eq!(
    sessions.read_active(SessionAccess::new(&ingress, &request, 100), &registration.session)
        .unwrap_err(),
    moe_agent_runtime::GatewayError::SessionExpired,
);
let restored = sessions.restore_fragment(
    SessionAccess::new(&ingress, &request, 200), &capability,
    &registration.session, deleted.lifecycle_version,
)?;
assert_eq!(restored.zone, RetentionZone::Normal);
// 恢复不激活历史会话；最终截止固定为 2_100，重复删除/恢复不延长。
assert_eq!(restored.final_deadline_ms, Some(2_100));
assert_eq!(
    sessions.read_active(SessionAccess::new(&ingress, &request, 200), &registration.session)
        .unwrap_err(),
    moe_agent_runtime::GatewayError::SessionExpired,
);
let report = sessions.maintain_retention(&ingress, 2_100)?;
assert_eq!(report.removed, 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```

管理能力不能由外部构造或伪造：

```compile_fail
use moe_agent_runtime::ManagementCapability;
let forged = ManagementCapability {};
```

## G4 步骤 2：原文快照导出/导入纯契约

G4 步骤 2 在留存已启用的协调器上增加无 I/O 的原文快照导出/导入；未配置留存时四个持久化入口全部返回 `RetentionNotConfigured`，G1–G3 行为不变。完整线格式、时间模型与恢复算法见 [GATEWAY-RAW-RUNTIME](../../docs/GATEWAY.md#运行快照)；本模块不访问文件、网络或系统时钟，`moe-raw-local` 文件适配已集成，整批验收以施工记录为准。

- `RawDomainKey` 是跨重启可比对的稳定归属域：完整桌宠身份（工作区/用户/实例）、调用主体、渠道、渠道范围、**受信宿主声明并绑定到当前已认证凭据的稳定凭据归属**与会话 hint 六元组。宿主用 `TrustedIngress::bind_stable_credential` 为在册凭据显式声明归属（运行层从不用运行实例号或凭据局部序号冒充稳定键；在册凭据归属不可中途改写，一条归属同时只属一张在册凭据，撤销后保留为历史归属供域比对），`from_request` 从当前已接纳请求加入口投影；无会话提示或凭据未绑定归属的请求没有稳定归属，明确拒绝持久化。归属绑定同时携带**授权修订**：`bind_stable_credential` 以当前修订绑定（新归属为修订 1），`bind_stable_credential_revision` 供宿主宣告授权撤销并换发（修订只前进，已退役修订不能借新凭据或重签许可重绑），`stable_credential_revision` 供跨重启比对。同一入口内归属不同的两张有效凭据即使主体/渠道/范围/hint 全同也是不同稳定域，正文与水位双向隔离。Debug 隐去 hint。
- `install_raw_history_grant` 由受信宿主在本机主人校验并核对旧授权仍有效后安装独立、精确历史许可（绑定协调器、入口运行实例与精确域，域内归属必须当前有在册凭据持有，账本上限 64）；`revoke_raw_history_grant` 幂等撤销。**撤销许可只撤销句柄，不删除正文**——正文仍按 G3 生命周期留在域内，由管理查看/软删除/清空或受信维护处置。快照数据、校验和或档内数字均不能替代许可。
- `export_raw_snapshot` 导出当前请求稳定域的全部片段（正常区、回收区、逻辑到期待转移者）与该域清理栅栏（宿主事件 ID、**稳定片段键**、来源顺序水位）。三阶段执行，任一失败不产生半更新——域内任一片段 `revoked` 置位或原授权代际落后于当前权威配置返回 `Revoked`，任一片段已到有效最终截止返回 `RawExpired`，快照构建后先**预演编码**再一次性提交新片段键：编码超限或键候选耗尽时不发布任何键或序号推进。新键分配跳过本域已用与已撤销键（撤销键不复用），档内极端键值不折入全局序号。能力撤销与历史授权撤销的内容都只以栅栏进入快照，重签能力或重启不能洗成可恢复内容。`committed_at_ms` 由受信宿主注入且不得早于当前时刻。
- `RawSnapshotV1` 是无权限的结构化 v1 快照：固定 magic/版本、小端长度前缀编码，FNV-1a 仅检测意外损坏，不证明同一性或授权。编解码有 16MiB 预算，解码与导入共享语义校验，坏档在发布前拒绝。`sweep_expired` 全量预检后移除最终到期正文并保留撤销栅栏，转移逻辑到期片段；所有保留/转移片段均固化收紧后的正常、硬及已固定最终期限，正文不改写。恢复成功后即使直接关闭、不再 checkpoint，下次宽松重开也不能放宽已保存期限。
- `import_raw_snapshot` 完整预检后原子发布：当前身份/域/历史许可/授权修订、时间、撤销栅栏、容量和有效最终截止（含等号）全部校验。导入账本上限 64 域，保留首导入内容区累计 16MiB 接纳预算；只存规范内容 SHA-256 指纹、稳定域、片段键、首提交时间和计费字节数，绝不保存全文副本。重复导入按排除提交时间的指纹判定，异指纹返回 `VersionConflict`；FNV32 碰撞不能判重。指纹依赖抗碰撞假设，不宣称数学上无碰撞。清理后同一快照仅返回仍存活句柄，不复活正文；稳定键、撤销栅栏、来源水位和累计预算不被清理重置。
- 导入不产生活跃会话、可完结交互、批次或 Agent 执行权：片段 `active_until` 为空，用户事件一律补 History 型登记元数据并挂接请求序号为 0 的惰性交互记录——一致重传命中幂等重复，而终结/追加/取消/推理被既有闸门拒绝。导入的未确认历史并非永久禁止新批次：历史只允许在当前完整闸门下显式开新批次——新用户事件按现行规则登记续期后 `issue_idle_batch` 才可能就绪；导入本身不签发批次或任务（导入后立即签发返回 `NotReady`），旧批次/交互/Agent 任务或结果不可复用，新处理不代表长期提交。撤销宿主事件 ID、稳定片段键、来源顺序水位与耗尽标志并入当前隔离域账本，跨恢复不清零；运行局部请求序号栅栏不导出不导入（旧请求结构上无法跨重启重放）。

### 持久化示例

```rust
use moe_agent_runtime::{
    CallerSubject, ChannelId, ChannelScope, GatewayError, IngressToken, ManagementPermissions,
    RawDomainKey, RawIngressCall, RawRetention, RawSnapshotV1, ReplyTarget, SessionAccess,
    SessionLimits, SessionRuntime, SessionTiming, TrustedIngress, UserEvent, UserEventKind,
};
use moe_memory_core::{Namespace, PetIdentity};

let mut egress = TrustedIngress::new();
egress.register_route("buddy/demo", PetIdentity::new(
    Namespace::new("synthetic-workspace", "synthetic-owner")?, "synthetic-pet",
)?)?;
let export_credential = egress.install_credential(
    IngressToken::new("synthetic-export-token")?, CallerSubject::new("caller")?,
    ["buddy/demo".to_owned()], ChannelId::new("offline")?,
    ChannelScope::new("private-thread")?, ReplyTarget::new("original-target")?,
)?;
// 受信宿主为凭据声明稳定归属；运行层不凭局部序号推测稳定域。
egress.bind_stable_credential(export_credential, "stable-channel-identity")?;
let mut source = SessionRuntime::with_retention(
    &egress, SessionLimits::new(8, 16, 32, 4096, 16_384, 1)?,
    SessionTiming::default(), RawRetention::new(1_000_000, 1_000_000, 4, 64, 8_192)?, 0,
);
let export_request = egress.admit(RawIngressCall::new(
    IngressToken::new("synthetic-export-token")?, "buddy/demo", "需要留存的合成原文",
).with_session_hint("synthetic-conversation"))?;
source.register_user(
    SessionAccess::new(&egress, &export_request, 0), UserEvent::new(UserEventKind::New),
)?;
let snapshot = source.export_raw_snapshot(
    SessionAccess::new(&egress, &export_request, 1), 1,
)?;
let bytes = snapshot.encode()?;

let mut ingress = TrustedIngress::new();
ingress.register_route("buddy/demo", PetIdentity::new(
    Namespace::new("synthetic-workspace", "synthetic-owner")?, "synthetic-pet",
)?)?;
let import_credential = ingress.install_credential(
    IngressToken::new("synthetic-import-token")?, CallerSubject::new("caller")?,
    ["buddy/demo".to_owned()], ChannelId::new("offline")?,
    ChannelScope::new("private-thread")?, ReplyTarget::new("original-target")?,
)?;
ingress.bind_stable_credential(import_credential, "stable-channel-identity")?;
let mut restored = SessionRuntime::with_retention(
    &ingress, SessionLimits::new(8, 16, 32, 4096, 16_384, 1)?,
    SessionTiming::default(), RawRetention::new(1_000_000, 1_000_000, 4, 64, 8_192)?, 0,
);
let import_request = ingress.admit(RawIngressCall::new(
    IngressToken::new("synthetic-import-token")?, "buddy/demo", "无关正文",
).with_session_hint("synthetic-conversation"))?;
let grant = restored.install_raw_history_grant(
    &ingress, RawDomainKey::from_request(&ingress, &import_request)?,
)?;
let receipt = restored.import_raw_snapshot(
    SessionAccess::new(&ingress, &import_request, 2),
    &grant, &RawSnapshotV1::decode(&bytes)?,
)?;
assert_eq!(receipt.fragments.len(), 1);
assert_eq!(
    restored.read_active(
        SessionAccess::new(&ingress, &import_request, 2),
        &receipt.fragments[0].session,
    ).unwrap_err(),
    GatewayError::SessionExpired,
);
# Ok::<(), Box<dyn std::error::Error>>(())
```

历史许可不能由外部构造或伪造：

```compile_fail
use moe_agent_runtime::RawHistoryGrant;
let forged = RawHistoryGrant {};
```

## 可见运行日志：首批纯内存服务

`DiagnosticsService::default()` 关闭采集。受信宿主先用 `DiagnosticsConfig::new(process_generation, budgets)` 显式配置进程代际，再通过 `add_domain` 声明精确稳定域、采集策略及查看许可，最后 `enable`。代际必须非零且跨重启不复用；域必须映射完整稳定实例身份或独立授权的宿主诊断域，不能用昵称、当前角色或“所有桌宠”替代。下例仅为合成离线配置，不批准生产采集级别、预算或保留策略。

- `create_run/create_emitter` 只供受信宿主调用；模型、Hook 输入与查询参数不能签发它们。采集与查看许可独立，公开封闭枚举只是诊断数据，不证明实际业务已经执行；宿主必须映射真实返回值/精确回执，无证据时用 Unknown。
- `authorize` 签发绑定服务、域、主体、版本与期限的 `LogAccess`；所有读取再次复验。`disable/enable` 是当前重配置/撤权入口，丢弃旧诊断并使全部旧 emitter/access/run/cursor 失效；授权改回原版本也不复活。到期或已观察到单调时钟回拨的许可不再使用。
- `query_into`、`health_into`、`list_runs_into` 与 `inspect_run_alias_into` 写入调用方有界切片；服务不无限分配输出或缓存。查询支持固定枚举过滤、单调时间半开区间、最多 96 UTF-8 字节安全模板搜索及 `PageDirection` 正反分页。后续页必须传原过滤和 cursor；cursor 消费后旧票据失效，提前结束用 `close_cursor` 释放服务槽位。
- 事件数、实际容器容量/字节、域、运行登记、别名、健康槽、cursor、页大小与查询工作量均受硬预算约束。事件槽按域×类别×详细度分区循环复用；运行登记和每查看授权的别名映射是重配置前累计上限，不回收或复用标识，到限明确拒绝。别名不提供 `Hash` 或序列化，去重使用有界等值索引；不承诺持续无限尾随。同步服务仅接受并发查询预算 1。
- `Gap::Possible` 表示可见历史存在缺口，数量未知；运行摘要不是业务账本。业务与传输状态独立，Completed 不等于 Written，更不等于 CLI 消费。可见采集损失保守标记证据不完整，隐藏分区不影响可见分页、健康及摘要；读取失败清空输出，不回退打印原始错误。

```rust
use moe_agent_runtime::{
    BackgroundCode, CaptureStrategy, Category, CategorySet, ClockReading, DetailLevel,
    DiagnosticKind, DiagnosticsConfig, DiagnosticsService, EventInput, HardBudgets,
    LogFilter, Source, StableDomain, ViewerGrant, ViewerSubject,
};

let domain = StableDomain::new("synthetic-private-pet")?;
let viewer = ViewerSubject::new("synthetic-viewer")?;
let budgets = HardBudgets {
    max_domains: 1, max_grants_per_domain: 1,
    events_per_partition: 8, runs_per_partition: 4, aliases_per_partition: 16,
    cursors_per_grant: 2, max_page_size: 2, max_query_work: 1024,
    max_summaries_per_query: 4, max_health_slots: 9, max_concurrent_queries: 1,
    max_event_bytes: 2048, max_allocated_bytes: 1_048_576,
};
let categories = CategorySet::only(Category::Background);
let grant = ViewerGrant::new(viewer, 1, 1000, categories, DetailLevel::Essential);
let mut config = DiagnosticsConfig::new(1, budgets)?;
config.add_domain(domain, CaptureStrategy::all_through(DetailLevel::Essential), &[grant])?;
let mut diagnostics = DiagnosticsService::default();
diagnostics.enable(config)?;
let access = diagnostics.authorize(domain, viewer, 1, ClockReading {
    monotonic_ms: 0, wall_clock_ms: None,
})?;
let emitter = diagnostics.create_emitter(domain, None)?;
diagnostics.emit(&emitter, EventInput {
    source: Source::Host, detail: DetailLevel::Essential, elapsed_ms: None,
    kind: DiagnosticKind::Background(BackgroundCode::ServiceStarted),
}, ClockReading { monotonic_ms: 1, wall_clock_ms: None })?;
let mut entries = [None; 2];
let page = diagnostics.query_into(
    &access, LogFilter::all().categories(categories), None,
    ClockReading { monotonic_ms: 2, wall_clock_ms: None }, &mut entries,
)?;
assert_eq!(page.returned, 1);
assert_eq!(entries[0].unwrap().occurred_at_ms(), None);
# Ok::<(), Box<dyn std::error::Error>>(())
```

没有 Agent/记忆/Hooks 的自动采集接线、文件、终端输出、导出、桌面查看器或查看端缓存租约；没有重复告警合并和终态优先级预留。交付 DTO 不是长期展示许可，后续查看器仍须实现独立预算、租约、断连/撤权清缓存；现阶段不挂载此类查看器。完整目标与首批限制见 [网关契约](../../docs/GATEWAY.md#可见运行日志)，实际验证见 [审核记录](../../docs/REVIEW.md#可见运行日志首批内存交付2026-10-13)。

## 当前限制

G0–G3 没有监听服务、兼容 HTTP 解析、流式协议、QQ/微信/Hooks、真实模型调用、长期整理提交或工具执行。G4 步骤 2 只交付运行层内的无 I/O 快照导出/导入契约：没有文件存储、锁、崩溃原子性或断电耐久（`moe-raw-local` 另批交付），快照可被整份伪造，内容真实性仅由非密码学校验和覆盖，防重放栅栏不含运行局部请求序号。从未被当前权威栅栏记录过的旧正文快照无法被运行层单独识别为过期——适配必须持久化并呈现最新快照与栅栏。稳定凭据归属及其授权修订由受信宿主声明，运行层只保证"未声明即拒绝、声明即精确比对、在册凭据归属不可中途改写、一条归属同时只属一张在册凭据、修订只前进且已退役修订不可重绑"，无法验证宿主所报归属是否真是跨重启同一逻辑凭据或其修订语义是否属实。G3 回收区与恢复在当前运行实例的有界内存内成立；持久化导出/导入要求宿主在单一受信 Unix epoch 毫秒时间轴上注入时间，运行层无法自行验证时间轴，谎报时间不可检测。本机管理能力与历史许可是离线宿主能力契约，不是 OS 登录认证、密码学 capability 或本机 GUI，宿主根接口不得作为模型工具、Hook 或远程 API 开放。安装、会话、Agent、管理、导出与导入 API 是受信宿主边界，不是模型可调用工具；不收集鉴权头、供应商密钥、隐藏推理或真实用户记录作为默认事件。含会话工作项的记忆任务仍禁止持久化派生，运行层不提供任何长期写入或自动共享路径。

token 校验仅排除空值、空白/控制字符和超长值；没有随机生成、熵检查、恒定时间比较、TLS、限流、密码学评审或可靠清零保证。撤销墓碑包含内存 token，进程结束后不恢复；此实现不得直接当生产网络认证上线。入口凭据和供应商凭据始终分离，建立预设与鉴权成功均不允许把记忆发送至外部模型。

## G5 Hooks 受信入口

`HookRuntime` 是独立有界内存账本，沿用同一 `TrustedIngress` 的完整身份、
凭据与配置代际证明，不制造占位用户发言。`HookBindingConfig` 必须通过
`with_cli_session_id` 固定宿主核准的 CLI 会话；默认没有观察、上下文输出
或模型输入许可。`HookContext` 由宿主审核来源及修订，绑定/来源更新使用 CAS。
模型或 CLI 载荷不能调用登记、授权、取消或提交根接口。

接纳后输入仅可按 `UnverifiedPrompt` 观察；回复固定为 `InjectedContext`。
本模块不调用 G1 用户登记或 G2 推理，不写原文，不读取记忆或自动共享。
`prepare_reply` 之后必须 `authorize_before_write`，然后由宿主登记传输回执；
`Written` 仅表示宿主写出，不表示 CLI 已消费。未写出前失败可通过
`fail_invocation` 封闭，不能借此伪造 Written/Cancelled。

实际 Claude Code 2.1.286 codec 与宿主接线在 `moe-cli-hooks` 边界 crate，
只有该边界引入经评审的 serde/serde_json；纯运行层仍只依赖记忆核心与标准库。
契约、包内核验依据与验收记录见 [G5 Hooks](../../docs/GATEWAY.md#hooks-适配)。

R4 回归补充：窄契约故障桩覆盖成功 prepare 后的 infer 健康故障（同一实际模型探针零调用）及成功 infer 后的 finish 故障（账本无成功终态）；SearchMemory 四场景回归位于检索 crate，仅为 dev-dependency 集成，不改变运行层正常依赖。真实 Embedding/rerank/tokenizer 和真实对话模型仍未验证，详见 [REVIEW](../../docs/REVIEW.md#r4-离线工程与最终复核2026-10-13)。
