# moe-memory-core

M5 的 MemoryDraft 只读审查访问器完整提供正文、分类、性质、来源、生命周期、敏感度、期限和原始依赖；不授予审批、身份或可变状态。最小离线对话编排位于上层本地适配，不反向进入纯核心，见 [M5 契约](../../docs/MEMORY.md#离线对话)。

Rust 标准库实现的封装记忆核心。M0 提供纯内存 CRUD，M1 增加纯命令编解码，M2 增加授权会话读取与可降级实体视图，M3 增加生命周期管理、派生依赖和取消栅栏，M4 增加受控共享发布和独立正式资料，R0 增加密封候选票据、混合任务复验与在线传输许可契约。内部模块、存储集合、记录字段和策略实现均不导出；外部只能通过 crate 根公开的服务、绑定客户端与输入/快照类型交互。

## M4 受控发布

完整契约见 [M4 发布边界](../../docs/MEMORY.md#共享发布)。`set_publication_policy` 与 `set_formal_publication_policy` 分别安装逐宠普通共享和逐分区正式资料许可，均仅在运行时保存且严格升版。读许可不包含写许可；自动发布规则未定，`prepare_automatic_publication` 始终拒绝。

模型只能提普通候选；宿主用 `MemoryTask::propose_shared` 绑定真实读取集，审查只读候选后显式 `prepare_shared_publication(candidate, now_ms)`。漏实际来源、私密记录（包括 Normal）、会话工作项或私密身份锚点一律拒绝，不能借共享提升生命周期或敏感度。宿主必须如实绑定全部实际输入，并在真实提交时间批准/立即执行，不能将库外私密输入藏在另一个空任务后；核心不做语义脱敏或系统时钟检查。

正式文档用独立 `prepare_formal_publication` / `prepare_formal_revision`，仅接受共享 Reference 来源和 ToolDefinition/Protocol 分类，不接受普通记录派生。文档受保护但不授予工具执行、人设或运行协议权威。显式 `RetrievalLayer::FormalMaterial` 读取沿用用途/来源/预算约束，不混入默认普通记忆，也不聚合成实体卡片。

新命令均使用 v2 tag 11–13，包含完整原始候选/任务或正式 CAS/代际请求；原 v1/M3 字节不变。prepare 不写入，最终 execute 复验实时任务、运行实例、授权和配置；解码不恢复批准，`Eq` 仅比较 wire。所有配置变化/存储代际推进取消旧批准；重启后的重放仅恢复成功状态，不恢复许可。普通共享当前只追加，不扩大共享删除、查看或传输。

```rust
use moe_memory_core::{
    Category, CommandResult, Epistemic, MemoryDraft, MemoryError, MemoryScope,
    MemoryService, Namespace, PetIdentity, PublicationPolicy, ReadPolicy, ReadPurpose,
    RetrievalLayer, RetrievalQuery, SessionId,
};
fn main() -> Result<(), MemoryError> {
    let namespace = Namespace::new("home", "user")?;
    let pet = PetIdentity::new(namespace.clone(), "alice")?;
    let session = SessionId::new("offline")?;
    let mut service = MemoryService::new();
    let source = service.register_source(
        MemoryScope::Shared(namespace.clone()), "verified-reference", 1, Epistemic::Reference,
    )?;
    service.set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))?;
    service.set_publication_policy(&pet, PublicationPolicy::allow_sources(1, vec![source.clone()]))?;
    service.set_formal_publication_policy(&namespace, PublicationPolicy::allow_sources(1, vec![source.clone()]))?;
    let task = service.for_session(&pet, &session, ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("reference"), 100)?;
    let candidate = task.propose_shared(MemoryDraft::new(
        "reviewed shared experience", Category::Knowledge, Epistemic::Reference, vec![source.clone()],
    ), 100)?;
    assert_eq!(service.prepare_automatic_publication(&candidate), Err(MemoryError::AutomaticPublicationDisabled));
    assert_eq!(candidate.text(), "reviewed shared experience");
    let approved = service.prepare_shared_publication(&candidate, 100)?;
    assert!(matches!(approved.execute(&mut service)?, CommandResult::Write(_)));
    let document = service.prepare_formal_publication(&namespace, MemoryDraft::new(
        "protocol description, not execution authority", Category::Protocol,
        Epistemic::Reference, vec![source],
    ), 100)?;
    document.execute(&mut service)?;
    let query = RetrievalQuery::new("protocol").with_layer(RetrievalLayer::FormalMaterial);
    assert_eq!(service.for_session(&pet, &session, ReadPurpose::OfflineReasoning)
        .retrieve(&query, 100)?.items().len(), 1);
    Ok(())
}
```

候选、许可内部集合和实时证明不可从外部改写：

```compile_fail
use moe_memory_core::{SharedCandidate, MemoryDraft};
fn forge(mut candidate: SharedCandidate, draft: MemoryDraft) { candidate.draft = draft; }
```

```compile_fail
use moe_memory_core::PublicationPolicy;
fn widen(mut policy: PublicationPolicy) { policy.sources.clear(); }
```

```compile_fail
use moe_memory_core::MemoryCommand;
fn forge(mut command: MemoryCommand) { command.2 = None; }
```

## R0 检索票据与在线传输许可

完整契约见 [RAG 方案](../../docs/MEMORY.md#rag-检索扩展分批施工)。宿主用 `MemoryReader::begin_candidates` 取得密封候选票据：候选来自本次授权下的全量扫描，不受 100 条检索上限约束；票据绑定运行证明、稳定身份、会话、用途、原查询、`RetrievalPlan`（Embedding 档案与可选 rerank 档案）、策略/配置版本与代际，外部不能构造或改写。`AuthorizedCandidate` 提供只读条目和绑定本票 nonce 的引用，普通 `RetrievalItem` 快照的未绑定引用不能提交；相同 ID/版本的跨根、跨票据引用也被拒绝。`RetrievalQuery` 的只读访问器使后续适配可以直接使用票据原查询，不必另传可能不一致的正文。适配层只提交票据内候选引用与有限分数；`MemoryReader::complete_retrieval` 拒绝外来/重复引用、非有限分数，按当前授权、完整候选快照（不只 ID/版本）、有效期和预算重新逐引用取正文，生成密封交付的混合 `MemoryTask`。混合任务的 `validate_task` 逐条复验实际交付条目、身份、运行证明、权限、代际和有效期，不重跑关键词选择——合法语义命中不会被旧重查误取消。

在线外传需要独立的 `TransmissionPolicy`：按 `TransmissionUsage`（QueryEmbedding / EntryEmbedding / Rerank）分别安装并严格升版，绑定目的端点、显式 `with_scope` 数据范围与精确来源版本；默认无外传范围，不从来源标签推断范围。读许可不授予传输，三种用途互不替代。QueryEmbedding/Rerank 会发送查询本身，必须由受信宿主先用 `bind_query_provenance` 声明完整查询来源及派生记录/工作上下文引用，再在策略上显式 `transmit_query()`；查询保持本宠私密归属，不从搜索范围推断归属，并且策略须允许该私密范围——空条目的查询编码和 Shared 搜索都不会替私密归属的查询取得外传权，缺查询证明即拒绝。`prepare_transmission`/`record_transmission`/`fail_transmission` 由宿主注入当前时间：发出前与返回后都按全部发送候选（不只最终入选者）复验当前读许可、来源与到期等号边界，失败保留发送审计并禁止交付。每次 `CandidateTicket` 由服务分配独立运行期身份，回执绑定签发票据及其会话/查询/范围/模型配置，另一会话、另一查询或空候选票据不能代收；候选票据和发送票据均不可克隆，单票独立 nonce 防止快照孪生的序号碰撞；发送预留有界名额，未回执的发送阻止交付，交付后不能追加发送，失败回执永久封闭该票据。发送资格按实际候选 `MemoryScope` 与查询归属核验：私密记录（即使引用共享来源）不得借 Shared-only 许可外传，必须显式允许实际私密范围，来源白名单是另一项独立检查。实际查询/发送输入的完整快照在发送票据和混合任务密封中保留（只含实际输入、有界且不序列化），贯穿准备、回执、交付与写回比较；同 epoch/代际/版本但正文不同的克隆分叉也被拒绝，未分叉克隆仍可用。实际发送但未入选的引用、来源与查询证据全部进入派生校验——`create_from_task` 必须声明全部发送来源并继承其依赖（删除任一发送记录使派生失效），发送过工作上下文仍禁止持久化；`propose_shared` 拒绝含非共享发送集的任务。单票至多预留/记录 8 次发送，每次至多 32 个引用及 32 个来源；混合 request 上限 64 KiB，并以不兼容旧查询长度字段的专用标记区分。混合任务的有界 request 字节包含规范化完整发送证据（`MemoryTask::request_bytes`），最终结果相同而实际发送集不同即为不同请求；关键词任务字节、journal/TaskStamp 布局与解码不序列化 LiveProof 的既有语义不变。本核心仍无模型、网络、文件或时钟访问；R1 已在独立 [memory-search](../moe-memory-search/README.md) 实现在线编码/HTTP 协议及精确扫描（真实模型门未验证），R2 已交付索引与混合排序，R3 已交付健康门覆盖的门面与离线 Agent 窄契约注入；R4 已补完整快照恢复/迟到失效回归及离线质量入口；真实质量门仍未验证。失败/取消/未知网络结果由受信适配调用 `fail_transmission`：先按原回执规则记录可能发送的完整输入、复验授权，再永久封闭本票；外来失败回执不能毒化另一张票。核心仍无 I/O 或第三方依赖。

候选票据的受信宿主校验入口为 `MemoryService::validate_candidate_ticket(&mut self, ticket: &CandidateTicket, now_ms: u64) -> Result<(), MemoryError>`；它核对当前运行/配置/代际、读取授权、人设快照、完整候选条目、QueryProof 与全部发送证据，并拒绝失败或有待回执的票据。只有完整留证的 Rerank 故障明确获得恢复标记、没有其他失败/待回执且当前快照仍一致时，宿主才可调用 `MemoryService::recover_failed_rerank(&mut self, ticket: &mut CandidateTicket, now_ms: u64) -> Result<CandidateTicket, MemoryError>`。恢复不复活旧票：资格仅消费一次，新票使用新运行身份和 nonce、完整重映射所有候选/QueryProof/发送引用、保留原传输证据与时间，并永久禁止再次外传；随后仍须走 `complete_retrieval`、任务交付和写回复验。它是受信宿主 API，不是模型工具。外部 `CallControl` 取消、整体 deadline 与错误分类由 search 适配层检查；核心只接收注入时间并裁决票据/授权。

```rust
use moe_memory_core::{
    Category, EmbeddingProfile, Epistemic, MemoryDraft, MemoryError, MemoryScope, MemoryService,
    Namespace, PetIdentity, ReadPolicy, ReadPurpose, RetrievalPlan, RetrievalQuery, SessionId,
    TransmissionPolicy, TransmissionUsage,
};
fn main() -> Result<(), MemoryError> {
    let pet = PetIdentity::new(Namespace::new("home", "user")?, "pet")?;
    let session = SessionId::new("session")?;
    let mut host = MemoryService::new();
    let source = host.register_source(
        MemoryScope::Private(pet.clone()), "message", 1, Epistemic::UserStatement,
    )?;
    host.for_pet(&pet).create(MemoryDraft::new(
        "主人偏爱茉莉花茶", Category::Episode, Epistemic::UserStatement, vec![source.clone()],
    ), 100)?;
    host.set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))?;
    host.set_transmission_policy(&pet, TransmissionPolicy::deny_all(1, TransmissionUsage::Rerank))?;
    let plan = RetrievalPlan::new(EmbeddingProfile::new("fixture-backend", "fixture-model", 8)?);
    let mut reader = host.for_session(&pet, &session, ReadPurpose::OfflineReasoning);
    let ticket = reader.begin_candidates(&RetrievalQuery::new("茶饮偏好"), plan.clone(), 100)?;
    let ranking: Vec<_> = ticket
        .candidates()
        .iter()
        .map(|item| (item.candidate_ref(), 0.9))
        .collect();
    let task = reader.complete_retrieval(&ticket, &ranking, 100)?;
    assert_eq!(task.result().items().len(), 1);
    assert_eq!(task.retrieval_plan(), Some(&plan));
    host.validate_task(&task, 200)?;
    Ok(())
}
```

密封票据、发送集证据与许可来源集合同样不可从外部改写：

```compile_fail
use moe_memory_core::CandidateTicket;
fn forge(mut ticket: CandidateTicket) { ticket.generation = 0; }
```

```compile_fail
use moe_memory_core::TransmissionPolicy;
fn widen(mut policy: TransmissionPolicy) { policy.sources.clear(); }
```

```compile_fail
use moe_memory_core::TransmissionTicket;
fn forge(mut ticket: TransmissionTicket) { ticket.sent_sources.clear(); }
```

## 外部接口

| 入口 | 用途 |
| --- | --- |
| `MemoryService::new` | 由受信宿主创建服务，当前数据只存在于该对象内存中 |
| `register_source` | 由受信宿主登记实际证据的分区、版本与性质；不是模型工具 |
| `for_pet` | 由受信宿主绑定稳定实例身份，返回不能切换身份的客户端 |
| `MemoryClient::create` | 创建本宠私密记忆；输入没有 owner、scope、权限或受保护标志 |
| `get` / `search` | 获取只读快照；先过滤归属、来源版本、敏感度与过期状态 |
| `update` | 完整替换内容候选，要求预期版本，保留 ID、归属与创建时间 |
| `delete` | 要求预期版本，从当前内存删除正文，返回删除回执 |
| `set_read_policy` / `for_session` | 宿主安装版本化来源白名单并绑定身份、会话、用途，返回只读 `MemoryReader` |
| `MemoryReader::retrieve` / `entity` / `identity` | 授权检索、可降级实体视图、独立受保护身份视图 |
| `archive` / `restore_archive` / `replace` / `status` | 绑定客户端上的显式管理；状态仅含本宠元数据，不是用户查看入口 |
| `revoke_source` / `restore_source` / `cancel_tasks` | 宿主根上的精确来源撤权/升版恢复、全库代际取消 |
| `MemoryReader::begin_task` / `validate_task` / `create_from_task` | 获准实际读取集、交付前复验、受取消栅栏保护的派生新建 |
| `MemoryReader::begin_candidates` / `complete_retrieval` | 密封全量获准候选票据、按适配排序收口为混合任务并逐引用复验 |
| `set_transmission_policy` / `bind_query_provenance` / `prepare_transmission` / `record_transmission` / `fail_transmission` | 按用途/端点/实际数据范围/来源（及显式查询证明与许可）授予在线外传许可，发送集留证并在发出前后按当前时间复验 |
| `MemoryContextAccess` | 绑定身份、会话、用途与原查询以开始任务，并复用核心上下文装配和复验；由 `MemoryService` 与持久化门面实现，不暴露存储内部 |

每次操作都由宿主提供毫秒时间，不读取系统时钟。写回执仅证明当前内存操作成功，不表示已落盘或持久化。返回的快照不会随服务更新；调用方保留的旧副本不能由服务远程追回。

`MemoryService` 是受信宿主 API，不应暴露为模型可任意调用的根对象。宿主负责身份认证、事件真实性和当前时间；模型只能产生候选，不能自行注册证据、选择实例或直接批准写入。Rust 私有字段不是同进程恶意代码的沙箱，也不是网络鉴权系统。

来源登记保存的是元数据，不保存原始证据正文；性质匹配不证明模型陈述语义正确，实际事实核验与提议/提交工作流由后续受信运行层完成。

## 使用示例

```rust
use moe_memory_core::{
    Category, Epistemic, MemoryDraft, MemoryError, MemoryScope, MemoryService,
    Namespace, PetIdentity, SearchQuery,
};

fn main() -> Result<(), MemoryError> {
    let namespace = Namespace::new("home", "user-1")?;
    let identity = PetIdentity::new(namespace, "pet-1")?;
    let mut service = MemoryService::new();
    let source = service.register_source(
        MemoryScope::Private(identity.clone()), "message-1", 1,
        Epistemic::UserStatement,
    )?;
    let mut memory = service.for_pet(&identity);
    let created = memory.create(
        MemoryDraft::new("用户说自己喜欢红茶", Category::UserProfile,
            Epistemic::UserStatement, vec![source.clone()]),
        1_000,
    )?;
    let snapshot = memory.get(created.id(), 1_000)?;
    assert_eq!(snapshot.revision(), 1);
    assert_eq!(memory.search(&SearchQuery::new("红茶"), 1_000)?.len(), 1);
    let updated = memory.update(
        created.id(), created.revision(),
        MemoryDraft::new("用户表示自己喜欢喝红茶", Category::UserProfile,
            Epistemic::UserStatement, vec![source]),
        2_000,
    )?;
    memory.delete(updated.id(), updated.revision(), 3_000)?;
    Ok(())
}
```

## 封装的编译期边界

外部不能取出底层存储：

```compile_fail
use moe_memory_core::MemoryService;
let service = MemoryService::new();
let _ = service.store;
```

外部不能绕过门面导入存储实现：

```compile_fail
use moe_memory_core::store::Store;
let _ = Store::default();
```

候选没有可覆盖的归属字段：

```compile_fail
use moe_memory_core::{Category, Epistemic, MemoryDraft};
let mut draft = MemoryDraft::new("candidate", Category::Episode,
    Epistemic::Inference, Vec::new());
draft.owner_pet_id = "another-pet".to_owned();
```

## 当前边界

- 普通 CRUD 仅写本宠私密记录。M4 共享发布走独立根入口，不以可读默认获得可写权限；自动发布规则未定时明确拒绝。
- 身份、正式工具说明、协议与权威状态不能由普通 CRUD 创建；受保护记录不能经改类别降级。
- `Restricted` 不在通用读取/修改/删除或 M3 记录管理接口中放行，普通更新和替代也不能把 Private 敏感度降为 Normal；M3 不扩展细粒度同意、用户查看或物理擦除权限。
- 当前内存最多 10,000 条保留记录，另最多 100,000 个无正文删除墓碑（覆盖旧日志的 100,000 次成功操作容量）；达到上限拒绝，不驱逐历史。文本最多 4096 个 Unicode 字符，证据及直接记录依赖各最多 32 个且不能重复；检索最多 100 条。
- ID 在同一个核心服务对象的生命周期内不复用；跨重启稳定性和回执恢复由 M1 的 `moe-memory-local` 保证，直接使用 `MemoryService::new()` 不获得持久化保证。
- 空查询表示列举获准记录；默认检索仍为大小写归一的子串匹配，没有向量、图谱或自动实体消歧。语义选择只能通过 R0 密封候选票据进入核心，核心本身不计算向量，也不因票据存在而放宽用途授权。
- 此核心无文件、网络、后台任务或系统时钟访问；持久化、跨进程写入锁和操作幂等位于独立 `moe-memory-local` 适配。M3 只交付逻辑管理闭环，不交付备份恢复、压缩或物理擦除。

## M1 适配边界

`MemoryCommand` 是受信宿主专用的不可变命令，提供来源、CRUD 和 M3 管理构造器，以及版本化 `to_bytes` / `from_bytes`。`execute(&mut MemoryService)` 调用受控门面，执行来源、归属、时间、CAS 与任务栅栏校验；结果仅包含只读来源、写/替代回执或代际。它不是任意状态导入接口，没有记录集合、ID 计数器或可变状态的 getter/setter。

`MemoryService: Clone` 仅供受信适配在私有候选上验证，协调器在写盘同步后才发布副本。核心没有事务或幂等账本：直接调用核心命令/CRUD 仍只影响内存，不能冒充持久化成功。持久化宿主使用 `moe-memory-local::LocalMemory`，不能同时把该协调器内部的核心绕过持久化单独修改。

命令的身份、来源登记与解码入口和 `MemoryService` 根一样只交给受信宿主，不应暴露给模型任意选择身份或恢复字节。模型仍只提出候选；完整审批流程不在 M1 范围。

## M2 授权读取

旧 `for_pet` CRUD（含 `get` / `search`）和命令仍是受信宿主的存储门面，不是用途授权接口，不应作为模型、用户查看或传输接口。获准上下文必须通过宿主绑定的 `MemoryReader` 获取；reader 不提供写入、来源登记、换身份、改策略或命令解码能力。当前不接模型，也不把 `OfflineReasoning` 当作可发送模型端点的许可证。

- 默认没有读取策略，全部用途拒绝。只有宿主显式安装 `ReadPolicy::offline(version, sources)` 后才允许 `OfflineReasoning`；`Unknown`（默认）、`UserInspection`、`ModelTransmission` 始终拒绝，未提供扩权开关。`deny_all` 可立即停止后续读取。策略版本必须严格递增且大于零；来源白名单绑定完整分区和精确来源版本，不是来源名字列表。
- 先检查用途，再过滤实例/用户/工作区、敏感度、有效期、全部来源有效性和全部来源授权，之后才做大小写归一的字面相关性、排序、条数和字符预算。未经授权的条目不会占据 top-k 或预算，也不返回其命中数、得分或依赖。多来源记录不能只放行其中一个来源。
- `RetrievalLayer` 选择当前会话工作上下文、长期记录或二者。工作上下文由宿主 `put_working_context` 注入，按稳定身份和 `SessionId` 隔离，只接受 `WorkingContext + ShortTerm`，不占持久化 `MemoryId`。宿主为新会话分配新 ID，不能复用旧会话 ID 冒充新会话。无会话绑定的旧 CRUD 短期记录及 `WorkingContext` 不会被自动当作当前上下文或长期记录。
- 条数上限 100，默认 20；正文预算默认 16,384、上限 409,600 个 Unicode scalar，允许 0。超预算整条跳过，不截断证据；不是 token 预算，也不计算序列化元数据或另取的身份锚点。排序为匹配次数降序、更新时间降序，再按长期 ID / 上下文 ID 确定性排序；同时间长期项先于上下文项。
- 身份锚点仅由宿主 `set_identity_view` 提供，需本宠私密的 `AuthoritativeState` 来源和严格递增版本；普通记忆不能创建/覆盖它。`identity()` 单独检查用途、白名单和源版本，锚点不参与普通检索或实体聚合，普通内容中的身份陈述不会升级为权威身份。身份配置不授予工具权限或正式协议权限。
- 实体视图只是按字面标签提取的获准条目集合，保留来源、性质、敏感度和版本，不生成新事实，不做自动实体消歧。缓存键包含完整身份、会话、用途及查询全部选择/预算参数。每次返回前重新授权并扫描当前候选，比较记录/上下文版本、精确来源版本集合和策略版本；新增、删除、纠正、到期及撤权均不能命中陈旧视图。
- 缓存默认 64 个槽、最多 256 个；关闭（容量 0）或无空槽时返回 `EntityRead::Entries`，`CacheStatus::Fallback` 明示退回同样授权和预算的条目级结果，不用旧视图兜底。不是外部索引服务，也不承诺 Rust 进程内存耗尽后可恢复。默认 Debug 不打印正文、查询文本、实体标签或身份锚点。

策略、会话上下文、身份配置和缓存均仅存在于内存，不进入日志；宿主重开后必须重新认证、安装当前授权和身份配置，不能从旧日志恢复宽权限。M3 补充逻辑取消与撤权，但仍不声称擦除缓存旧字节、追回已交付快照或清除日志正文。

```rust
use moe_memory_core::{
    CacheStatus, Category, Epistemic, Lifecycle, MemoryDraft, MemoryError, MemoryScope,
    MemoryService, Namespace, PetIdentity, ReadPolicy, ReadPurpose, RetrievalQuery, SessionId,
};
fn main() -> Result<(), MemoryError> {
    let pet = PetIdentity::new(Namespace::new("home", "user")?, "pet")?;
    let session = SessionId::new("session-1")?;
    let mut host = MemoryService::new();
    let source = host.register_source(MemoryScope::Private(pet.clone()), "message", 1,
        Epistemic::UserStatement)?;
    host.for_pet(&pet).create(MemoryDraft::new("tea preference", Category::UserProfile,
        Epistemic::UserStatement, vec![source.clone()]), 100)?;
    host.put_working_context(&pet, &session, "current-task", 1,
        MemoryDraft::new("tea task", Category::WorkingContext, Epistemic::UserStatement,
            vec![source.clone()]).with_lifecycle(Lifecycle::ShortTerm), 100)?;
    host.set_read_policy(&pet, ReadPolicy::offline(1, vec![source]))?;
    let mut reader = host.for_session(&pet, &session, ReadPurpose::OfflineReasoning);
    let query = RetrievalQuery::new("tea").with_limit(2).with_char_budget(30);
    assert_eq!(reader.retrieve(&query, 100)?.items().len(), 2);
    assert_eq!(reader.entity(&query, 100)?.cache_status(), CacheStatus::Built);
    assert_eq!(reader.entity(&query, 101)?.cache_status(), CacheStatus::Hit);
    Ok(())
}
```

封装编译失败用例：绑定 reader 无法取根、改策略、写入或修改身份快照。

```compile_fail
use moe_memory_core::MemoryReader;
fn leak(reader: MemoryReader<'_>) { let _ = reader.service; }
```

```compile_fail
use moe_memory_core::{MemoryReader, ReadPolicy};
fn escalate(mut reader: MemoryReader<'_>) { reader.set_read_policy(ReadPolicy::deny_all(1)); }
```

```compile_fail
use moe_memory_core::{MemoryReader, MemoryDraft};
fn write(mut reader: MemoryReader<'_>, draft: MemoryDraft) { reader.create(draft, 100); }
```

```compile_fail
use moe_memory_core::IdentityView;
fn change(mut view: IdentityView) { view.text = "override".into(); }
```

## 获准上下文与压缩

`assemble_context`、`context_view`、`begin_compaction`、`apply_compaction` 只管理请求投影，复用实际任务的读取/取消栅栏；不覆盖长期记忆，不调用模型。可用身份与正式资料保留原文预算；摘要标记为 `UnverifiedSummary`，来源和性质沿用原输入，不构造新证据。完整预算、失效与不落盘契约见 [上下文契约](../../docs/MEMORY.md#上下文与压缩)。

`MemoryContextAccess` 是供 Agent 注入的窄 trait：`begin_memory_task` 绑定受信身份、会话、读取用途和 `RetrievalQuery`，`assemble_context` 与 `context_view` 继续使用本 crate 的完整授权、失效及受保护内容校验。`MemoryService` 的关联错误为 `MemoryError`；持久化实现可保留适配层自己的错误类型。

```rust
use moe_memory_core::{
    Category, ContextBlockKind, Epistemic, MemoryDraft, MemoryScope, MemoryService,
    Namespace, PetIdentity, ReadPolicy, ReadPurpose, RetrievalQuery, SessionId,
};

let mut memory = MemoryService::new();
let pet = PetIdentity::new(Namespace::new("home", "user")?, "alice")?;
let session = SessionId::new("current")?;
let source = memory.register_source(
    MemoryScope::Private(pet.clone()), "message", 1, Epistemic::UserStatement,
)?;
memory.for_pet(&pet).create(MemoryDraft::new(
    "tea before breakfast", Category::Episode, Epistemic::UserStatement,
    vec![source.clone()],
), 100)?;
memory.set_read_policy(&pet, ReadPolicy::offline(1, vec![source]))?;
let task = memory.for_session(&pet, &session, ReadPurpose::OfflineReasoning)
    .begin_task(&RetrievalQuery::new("tea"), 100)?;
let mut context = memory.assemble_context(&task, 100, 100)?;
let ticket = memory.begin_compaction(&context, 0..1, 100)?;
memory.apply_compaction(&mut context, &ticket, "morning tea", 110)?;
let view = memory.context_view(&context, 110)?;
assert_eq!(view.blocks()[0].kind(), ContextBlockKind::UnverifiedSummary);
assert_eq!(view.blocks()[0].evidence().len(), 1);
assert_eq!(view.usage().used_chars, 11);
# Ok::<(), moe_memory_core::MemoryError>(())
```

```compile_fail
use moe_memory_core::RequestContext;
fn overwrite(mut context: RequestContext) { context.view.blocks.clear(); }
```

```compile_fail
use moe_memory_core::CompactionTask;
fn approve(mut ticket: CompactionTask) { ticket.revision = 999; }
```

## M3 管理入口

完整语义、取消/恢复边界与命令 v2 见 [M3 契约](../../docs/MEMORY.md#生命周期与取消)。管理接口仍仅交给受信宿主，Unknown、UserInspection、ModelTransmission 始终拒绝。

- `update` 是同 ID 纠正；`replace` 原子创建新 ID 并将旧条目标记为 Superseded。过期记录不能普通更新续命，需显式替代。归档保留正文，仅通过升版解归档恢复；解归档也要求有效证据、依赖和期限。
- `MemoryDraft::with_dependencies` 声明精确 `RecordVersion`，要求完整来源及不降低敏感度。迭代检查传递闭包，覆盖派生记录、会话上下文和实体；身份视图只依赖私密权威来源。祖先失效只造成后代不可见，不自动扩大删除。
- `revoke_source` 封闭来源读写；恢复只能显式升版且不复活旧引用/白名单。`revoke_access` 清除本次运行的上下文/身份/缓存并保留版本水位；上下文/身份显式移除也升版留运行时墓碑。
- `begin_task` 捕获实际读取集、可用身份及不透明运行标记。`validate_task` 在交付前复验；`create_from_task` 在新提交前复验并强制来源/依赖。全库代际、任一运行配置变更、到期或重开使旧任务失效；含工作上下文的任务不准持久化派生。
- `MemoryTask::propose_create` 仅生成不可变宿主提议，不批准执行；实时执行保留并复验不透明任务证明，解码不恢复证明，不能跨运行实例实时执行。命令相等性只比较 wire 请求，不代表运行权限相等。`replay` 仅供受信适配恢复成功日志，保持历史 v1 语义，不安装策略或运行权限；不能用它绕过实时任务复验。
- `status` 只返回本宠元数据；存储有效性不等于用途授权。Deleted 无恢复入口，旧回执重试不重做状态。`physical_erasure_performed()` 始终 false；墓碑、日志、备份、缓存和已交付副本不能混同。

```rust
use moe_memory_core::{Category, Epistemic, MemoryDraft, MemoryScope, MemoryService,
    Namespace, PetIdentity, ReadPolicy, ReadPurpose, RecordState, RetrievalQuery, SessionId};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pet = PetIdentity::new(Namespace::new("home", "user")?, "pet")?;
    let session = SessionId::new("current")?;
    let mut host = MemoryService::new();
    let source = host.register_source(MemoryScope::Private(pet.clone()), "message", 1,
        Epistemic::UserStatement)?;
    let root = host.for_pet(&pet).create(MemoryDraft::new("tea original", Category::Episode,
        Epistemic::UserStatement, vec![source.clone()]), 100)?;
    host.set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))?;
    let task = host.for_session(&pet, &session, ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)?;
    let derived = host.create_from_task(&task, MemoryDraft::new("tea derived", Category::Episode,
        Epistemic::Inference, vec![source]), 200)?;
    host.for_pet(&pet).archive(root.id(), 1, 300)?;
    host.for_pet(&pet).restore_archive(root.id(), 2, 400)?;
    assert!(!host.for_pet(&pet).status(derived.id(), 400)?.is_available());
    host.for_pet(&pet).delete(derived.id(), 1, 500)?;
    assert_eq!(host.for_pet(&pet).status(derived.id(), 500)?.state(), RecordState::Deleted);
    Ok(())
}
```

```compile_fail
use moe_memory_core::MemoryTask;
fn bypass(mut task: MemoryTask) { task.stamp.generation = 0; }
```

```compile_fail
use moe_memory_core::MemoryStatus;
fn forge(mut status: MemoryStatus) { status.available = true; }
```

```compile_fail
use moe_memory_core::CandidateTicket;
fn fork(ticket: CandidateTicket) { let _copy = ticket.clone(); }
```

```compile_fail
use moe_memory_core::TransmissionTicket;
fn fork(ticket: TransmissionTicket) { let _copy = ticket.clone(); }
```

```compile_fail
use moe_memory_core::QueryProof;
fn forge(mut proof: QueryProof) { proof.bound = true; }
```
