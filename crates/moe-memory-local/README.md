# moe-memory-local

M1 本地版本化持久化、M2 授权读取、M3 生命周期/撤权/取消、M4 受控共享发布及 M5 最小离线对话适配。仅依赖 workspace 内的 `moe-memory-core`，没有第三方依赖、网络、真实模型连接、GUI 或旧档迁移。

## M5 离线对话

完整现行契约见 [M5 契约](../../docs/MEMORY-M5.md)。受信宿主以 `offline_dialogue` 绑定稳定身份/会话和 OfflineReasoning，依次 begin_turn → generate（仅 OfflineFixture 注入）→ review_and_commit。Unknown/UserInspection/ModelTransmission 仍拒绝；OfflineReasoning 不代表任何模型传输许可。模型 SDK、端点/网络回调、任意工具执行和隐式模型历史均不存在。

候选不会自行生效。宿主审查不可变候选全文/全部元数据、真实上下文和身份锚点后，返回 Reject 或附真实当前时间的 CreatePrivate/PublishShared；方法立即复验任务并同步提交，不交出可排队的批准。实际来源/版本和原始依赖分别复用 M3 tag 10、M4 tag 11，命令/日志格式不变。private（包括 Normal）、工作项、身份锚点和正式资料仍不能洗入普通共享，自动发布恒拒绝。生成夹具不能暗中添加库外输入或复用其他模型历史。

ContextReport 明报条目数/身份可用性；实体索引不可用退回同一获准条目且报告 EntriesFallback，不能吞掉拒绝授权或隐藏私密输入。模型不可用/坏输出返回结构化 DialogueError，不生成候选、不写盘、不谎报回答或执行成功；错误的阶段与原因不含正文。非法候选在提交门拒绝，不因生成成功变成受信事实。

correct_original 是宿主核验原始 UserStatement/Observation 的独立同 ID CAS 纠正，仅限无依赖且分类相同的原始记录/新稿；不接受 OfflineProposal，不替模型派生去掉证据。一般派生更新仍走明确维护来源/依赖的 M3 宿主管理接口。纠正仍拒绝到期续命、敏感度降级及越权，并取消旧任务。

成功 DialogueReceipt 或提交错误可以持有不可变 DialogueCommit；to_bytes/from_bytes 只编解码原 MemoryCommand，operationId 另存且不恢复实时证明。confirm_dialogue 只按完整请求确认已经同步恢复的成功账本；缺失/冲突拒绝，不执行、不改代际或恢复许可，普通 CRUD 解码请求也不能借此补提交。写盘不确定错误不是“保证未写盘”：全部 M5 入口封闭，完整帧须重开验证/同步，半帧保档拒绝。字节可能含正文，不默认打印/传输或扩大保留副本。

编译验证的宿主封装，reviewer 必须在真实审查结束后采样当前提交时间；实际可运行的合成夹具在 [offline_dialogue 示例](examples/offline_dialogue.rs)：

```rust
use moe_memory_core::{MemoryDraft, PetIdentity, ReadPurpose, RetrievalQuery, SessionId};
use moe_memory_local::{ContextPreference, DialogueError, DialogueReceipt, HostDecision,
    LocalMemory, OfflineFixture, OfflineReview, OperationId};
fn reviewed_turn(
    memory: &mut LocalMemory, identity: &PetIdentity, session: &SessionId,
    draft: MemoryDraft, issued_at_ms: u64, operation_id: OperationId,
    reviewer: impl FnOnce(OfflineReview<'_>) -> HostDecision,
) -> Result<DialogueReceipt, DialogueError> {
    let mut dialogue = memory.offline_dialogue(identity, session, ReadPurpose::OfflineReasoning)?;
    let turn = dialogue.begin_turn(&RetrievalQuery::new("fixture"), ContextPreference::PreferEntity, issued_at_ms)?;
    let proposal = dialogue.generate(turn, OfflineFixture::Candidate(draft), issued_at_ms)?;
    dialogue.review_and_commit(&proposal, operation_id, reviewer)
}
```

身份绑定、不可变候选和确认句柄不能从模型字段伪造，也没有可变草稿访问器：

```compile_fail
use moe_memory_local::OfflineTurn;
fn forge(turn: &mut OfflineTurn, task: moe_memory_core::MemoryTask) { turn.task = task; }
```

```compile_fail
use moe_memory_local::OfflineProposal;
fn forge(proposal: &mut OfflineProposal, draft: moe_memory_core::MemoryDraft) { proposal.draft = draft; }
```

```compile_fail
use moe_memory_local::DialogueCommit;
fn forge(request: &mut DialogueCommit, command: moe_memory_core::MemoryCommand) { request.command = command; }
```

```compile_fail
use moe_memory_core::MemoryDraft;
fn mutate(draft: &mut MemoryDraft) { draft.text().push_str("changed"); }
```

## M4 发布适配

完整语义见 [M4 契约](../../docs/MEMORY-M4.md)。根提供 `set_publication_policy`、`set_formal_publication_policy`、`prepare_shared_publication(candidate, now_ms)`、`prepare_automatic_publication`、`prepare_formal_publication`、`prepare_formal_revision` 和 `commit_publication`。许可与批准均仅供受信宿主；来源白名单不是自动批准，低风险规则未定始终拒绝自动发布。

prepare 使用注入的真实提交时间，迟到候选重验到期/实际读取集；其结果是带实时证明的不可变命令，不是已经落盘的回执。宿主审查后应立即用相同实际提交时间提交；不能给排队命令谎报旧时间。`commit_publication` 只接受 M4 命令，并复用私有副本、追加日志、文件同步、再发布与 `Committed<WriteReceipt>`。原日志外层 v1 和命令 v1 字节不变，M4 新增命令 v2 tag 11–13。

相同 operationId/完整 wire 可在撤权/重开后确认旧成功回执，包含解码的原请求，但不重做、不恢复批准或许可。不同原始候选依赖、提议时间、查询/预算/层或正式 CAS 请求冲突；新 decoded 请求不能执行。运行发布许可和水位不写日志，重开需重新认证；replay 只恢复已成功状态。正式文档通过 reader 的 `RetrievalLayer::FormalMaterial` 显式读取，默认普通检索不混入它，读取仍拒绝未知/用户查看/模型传输。

任何写入/同步不确定失败封闭所有发布配置、prepare、提交和正式读取。半帧保档拒绝；完整帧重新验证并成功同步后可确认，不自动修复。逻辑保留、外部副本、Windows 文件同步与防旧日志回滚限制均不变，不新增共享删除或传输权。

已认证、完成来源登记并装好精确读/发布许可的宿主，可把逐候选审查后的提交封装为：

```rust
use moe_memory_core::{SharedCandidate, WriteReceipt};
use moe_memory_local::{Committed, LocalMemory, OperationId, PersistenceError};
fn commit_reviewed(
    memory: &mut LocalMemory, operation_id: OperationId,
    candidate: &SharedCandidate, now_ms: u64,
) -> Result<Committed<WriteReceipt>, PersistenceError> {
    let approved = memory.prepare_shared_publication(candidate, now_ms)?;
    memory.commit_publication(operation_id, approved)
}
```

批准不能靠篡改内部服务状态构造：

```compile_fail
use moe_memory_local::LocalMemory;
fn forge(memory: &mut LocalMemory) { memory.service = moe_memory_core::MemoryService::new(); }
```

## 宿主接口

`LocalMemory::create(path)` 只创建新日志，文件已存在即失败；`open(path)` 只恢复现有日志，文件缺失、损坏或版本不支持即失败。宿主必须明确区分首次初始化与恢复，不能捕获恢复错误后自动创建空库。父目录必须已经存在，并由宿主保护访问权限。

根对象、`register_source`、`for_pet`、`for_session` 和全部管理/配置入口仅交给受信宿主。宿主认证稳定实例身份后把绑定客户端用于候选的受控提交，不能把根对象或核心 `MemoryCommand` 的解码/执行/重放接口注册为模型工具。M3 提供逻辑管理及取消栅栏，不提供完整模型提议/审批流程，也不证明候选的语义真实性。

## M2 运行时读取

`set_read_policy`、`put_working_context`、`set_identity_view` 和 `set_entity_cache_capacity` 只配置本次进程的核心状态，返回 `Result<()>`，不追加日志、无 operationId、无持久化回执；重启全部清空，默认拒绝读取。来源元数据仍须经 `register_source` 同步提交。身份锚点由宿主可信配置重建，不从普通记忆自动升级。会话注入不分配持久化记录 ID，不改变日志重放或旧操作幂等结果。

宿主显式提供精确来源白名单，再调用 `for_session(identity, session, purpose)` 得到 `LocalMemoryReader`。它只提供 `retrieve`、`entity`、`identity` 和 M3 的 `begin_task`，不能切换身份、用途或会话，也不能取根对象。核心的授权先过滤、预算、身份保护和版本化缓存规则完全复用，详见 [核心 M2 契约](../moe-memory-core/README.md#m2-授权读取)。`Unknown`、用户查看和模型传输用途仍拒绝，旧 `get` / `search` 仅限受信宿主存储检查，不得绕过 reader 充当上下文读取工具。

写盘候选携带配置和缓存的私有副本，仍须文件同步后才发布；失败对象连 M2 读取、缓存命中及运行时配置都返回 `RecoveryRequired`。重开验证并同步成功不等于恢复旧授权：即使完整不确定帧已恢复，也需宿主重新安装当前策略，才能读取其正文。半帧依旧明确拒绝，不自动修复。

```rust
use moe_memory_core::{Category, Epistemic, MemoryDraft, MemoryScope, Namespace, PetIdentity,
    ReadPolicy, ReadPurpose, RetrievalQuery, SessionId};
use moe_memory_local::{LocalMemory, OperationId};
# fn main() -> Result<(), Box<dyn std::error::Error>> {
# let path = std::env::temp_dir().join(format!("moe-memory-m2-doc-{}.journal", std::process::id()));
let pet = PetIdentity::new(Namespace::new("home", "user")?, "pet")?;
let session = SessionId::new("session-1")?;
let mut host = LocalMemory::create(&path)?;
let source = host.register_source(OperationId::new("source")?, MemoryScope::Private(pet.clone()),
    "message", 1, Epistemic::UserStatement)?.value().clone();
host.for_pet(&pet).create(OperationId::new("create")?, MemoryDraft::new("tea preference",
    Category::UserProfile, Epistemic::UserStatement, vec![source.clone()]), 100)?;
host.set_read_policy(&pet, ReadPolicy::offline(1, vec![source]))?;
assert_eq!(host.for_session(&pet, &session, ReadPurpose::OfflineReasoning)
    .retrieve(&RetrievalQuery::new("tea"), 100)?.items().len(), 1);
drop(host);
# std::fs::remove_file(path)?;
# Ok(())
# }
```

```compile_fail
use moe_memory_local::LocalMemoryReader;
fn leak(reader: LocalMemoryReader<'_>) { let _ = reader.coordinator; }
```

```compile_fail
use moe_memory_local::LocalMemoryReader;
use moe_memory_core::{PetIdentity, SessionId, ReadPurpose};
fn rebind(mut reader: LocalMemoryReader<'_>, pet: PetIdentity, session: SessionId) {
    reader.for_session(&pet, &session, ReadPurpose::OfflineReasoning);
}
```

| 入口 | 契约 |
| --- | --- |
| `register_source(operation_id, scope, source_id, revision, epistemic)` | 同步持久化来源元数据；旧版本和同版本异性质拒绝 |
| `for_pet(identity)` | 绑定身份，客户端不能选择 owner 或切换分区 |
| `create(operation_id, draft, now_ms)` | 验证来源与策略，分配不复用的 ID |
| `update(operation_id, id, expected_revision, draft, now_ms)` | CAS 全量更新；旧版本、旧时间与过期/失效证据拒绝 |
| `delete(operation_id, id, expected_revision, now_ms)` | CAS 删除当前可检索记录，保留操作与 ID 高水位历史 |
| `get(id, now_ms)` / `search(query, now_ms)` | 仅返回已发布的只读快照，沿用核心的隔离与来源过滤 |

所有时间由宿主注入。写入返回 `Committed<WriteReceipt>`，来源登记返回 `Committed<SourceRef>`；`operation_id()`、`sequence()` 与 `durability() == FileSynced` 证明该请求已经通过文件同步屏障，不再沿用 M0 的纯内存回执语义。`value()` 是只读结果，外部不能构造成功回执或获取内部核心、映射及可变记录。

```rust
use moe_memory_core::{
    Category, Epistemic, MemoryDraft, MemoryScope, Namespace, PetIdentity, SearchQuery,
};
use moe_memory_local::{Durability, LocalMemory, OperationId};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
# let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
# let path = std::env::temp_dir().join(format!("moe-memory-doc-{}-{suffix}", std::process::id()));
let pet = PetIdentity::new(Namespace::new("home", "user")?, "pet-1")?;
let mut memory = LocalMemory::create(&path)?;
let source = memory.register_source(
    OperationId::new("source-message-1")?, MemoryScope::Private(pet.clone()),
    "message-1", 1, Epistemic::UserStatement,
)?;
let candidate = MemoryDraft::new("用户喜欢红茶", Category::UserProfile,
    Epistemic::UserStatement, vec![source.value().clone()]);
let created = memory.for_pet(&pet).create(OperationId::new("create-1")?, candidate.clone(), 1_000)?;
assert_eq!(created.durability(), Durability::FileSynced);
drop(memory);

let mut memory = LocalMemory::open(&path)?;
let retried = memory.for_pet(&pet).create(OperationId::new("create-1")?, candidate, 1_000)?;
assert_eq!(retried, created);
assert_eq!(memory.for_pet(&pet).search(&SearchQuery::new("红茶"), 1_000)?.len(), 1);
memory.for_pet(&pet).delete(OperationId::new("delete-1")?, created.value().id(), 1, 2_000)?;
drop(memory);
# std::fs::remove_file(path)?;
# Ok(())
# }
```

外部不能通过持久化门面绕过协调器：

```compile_fail
use moe_memory_local::LocalMemory;
fn bypass(memory: LocalMemory) {
    let _ = memory.service;
}
```

```compile_fail
use moe_memory_local::journal::FileJournal;
```

## 单写入与发布顺序

一个对象拥有整个日志文件的排他 OS 锁，持锁直到对象释放，第二个进程/对象不能同时恢复或写入。同对象调用通过 Rust 独占借用串行化；多线程宿主可以使用 `Arc<Mutex<LocalMemory>>`，没有可克隆的写入句柄。锁失败不降级为无锁运行，锁不是对不合作进程的安全沙箱。

每次提交顺序为：检查 operationId → 在核心私有副本验证来源、策略和 CAS → 编码单个帧 → 追加全部字节 → `File::sync_all` → 发布新核心状态和回执。副本仅在受信适配内部持有，不暴露底层集合。完整日志不被覆盖或 rename，因此不依赖 Windows/Unix 替换已存在文件的差异；单帧也不假设一次 write 是原子的。

operationId 在整个日志内唯一。相同 ID 与完全相同的请求返回原始回执且不追加；请求包含操作种类、身份/分区、全部候选字段、来源顺序、预期版本和宿主时间，重试必须保留这些值。不同请求使用同一 ID 返回 `OperationConflict`，不会泄露另一身份的历史结果。新请求必须使用新的 ID；验证失败不占用 ID。重复旧创建/更新只返回历史回执，不重做操作，因此不能复活删除内容或覆盖后来的版本。

## 恢复与失败语义

- 任何追加/同步错误都不发布候选、不返回成功；该对象立即进入 `RecoveryRequired`，连读取和旧操作重试也停止，防止继续使用不确定状态。首次返回的 I/O 错误可能意味着提交结果不确定，不能据此断言磁盘上一定没有该请求。
- 关闭后重新 `open` 会完整验证和重放；重新同步通过后才提供服务。完整但先前未获回执的帧可以恢复，使用原 operationId 重试得到唯一回执；没有写入字节的失败可重新提交。
- 半帧、校验错误、未知格式、乱序、重复操作、非法命令或不满足核心策略的日志都报错并保持文件原样；不会自动截尾、跳帧、退回旧文件或返回空库。损坏文件需要宿主显式处置，M1 不提供自动修复/备份回滚工具。
- 来源版本、所有成功操作的确定性 ID 分配和删除顺序都通过重放恢复；已删除 ID 不再分配，旧来源候选不能因重启变有效。

## 格式、限制与安全边界

日志封装 v1 是小端二进制：16 字节头（8 字节 `MOEMEMJ\0`、u64 版本 1），后接连续帧。每帧为 u64 正文长度、正文、u64 FNV-1a 校验；正文为 u64 序号、u64 前帧校验、u64 operationId UTF-8 长度及正文、u64 命令长度及正文。命令独立标记版本，原命令 v1 不变，M3 使用命令 v2 与显式 tag，不依赖 Rust 内存布局。恢复在分配前检查长度、UTF-8、枚举、尾部与数量界限，再走核心受控重放；没有可导入任意状态的快照格式。

最多 100,000 个成功操作、256 MiB 日志、256 KiB 帧；operationId 最多 128 个 Unicode 字符，核心命令最多 128 KiB。达到限制明确拒绝，不删除幂等历史。当前每次写入复制活跃核心状态，启动完整重放日志；压缩、索引和数据库优化不在 M1 范围。

日志含历史明文，包括已逻辑删除的正文；删除保证恢复后不可检索且不会复活，不等于物理擦除、备份清理或管理遗忘。校验用于检测意外损坏，不是认证、加密或防恶意回滚。替换为一份完整有效旧日志/整帧前缀、删除整个文件等外部破坏没有独立可信高水位可供检测；宿主不得自动把缺档当首次启动。目录与 ACL、备份保留及恶意本机进程防护由宿主负责，不因这个库而扩大任何权限。

`FileSynced` 精确表示操作系统文件同步调用成功，依赖本地文件系统和设备兑现 flush；不宣称硬件故障下绝对不会丢数据。新建日志在 Unix 另同步父目录；Windows 当前只有文件同步，不承诺新文件名在突然断电后的目录元数据耐久性。没有跨平台实测就不宣称跨平台耐久保证；当前验收平台为 Windows MSVC，覆盖真实文件、跨进程锁与无析构退出恢复，写失败/半写/sync 失败另通过私有故障接缝注入。

## 请求上下文适配

`LocalMemory` 公开 `assemble_context`、`context_view`、`begin_compaction`、`apply_compaction`，与核心同名接口一致，先检查本地健康状态，再复验真实任务和投影版本。四个接口均不追加日志、不分配 operationId、不产生耐久回执；压缩结果只是运行时未核验摘要，不能作为新事实或共享授权。重开后旧上下文及票据均失效，不确定写盘后四个入口全部拒绝。详情及核心调用示例见 [上下文契约](../../docs/MEMORY-CONTEXT.md) 和 [核心 API](../moe-memory-core/README.md)。

## M3 管理与恢复

完整核心语义见 [M3 契约](../../docs/MEMORY-M3.md)。`LocalMemoryClient` 提供 `archive`、`restore_archive`、`replace` 与仅元数据的 `status`；根提供 `revoke_source`、严格升版的 `restore_source`、`generation`、CAS `cancel_tasks`、`validate_task`、`create_from_task`。只有成功持久化操作返回 `Committed`，仍遵守排他锁、完整请求 operationId 幂等、私有副本、追加、文件同步、再发布的顺序。替代的旧状态与新条目写在同一帧，不会只发布一半。

全库取消代际、来源撤权、归档/替代/删除墓碑及精确记录依赖经日志重放恢复；运行标记、读取策略、上下文、身份和缓存不恢复。`revoke_access`、`remove_working_context`、`remove_identity_view` 与配置同属运行时管理，无 operationId/耐久回执，重开后默认拒绝；恢复旧日志不能恢复宽权限。含工作条目的任务拒绝持久化派生，不能借此把会话临时输入自动升级为长期证据。

新任务提交在最终提交前复验运行标记、代际、配置、授权、期限与实际读取集。成功 task 请求的相同 operationId/完整 payload 重试可在撤权、删除或重开后返回原历史回执，但不执行、不返回正文、不改变取消代际；不同查询/预算/会话/发行时间/读取版本或原始候选任一字段均冲突，自动补依赖不抹去原始输入差异。旧任务不能换新 operationId 复活内容，解码也不恢复实时任务证明。重放 task 命令仅恢复已写成功状态，不认证原运行权限。历史 v1 曾接受到期后纠正的合法帧及超过 10,000 次删除的合法日志仍能恢复，新的实时普通更新不可用该路径续命。

任一写入/同步不确定失败仍封闭全部 M3 方法（包括状态、清理报告、任务、撤权和运行时移除）。完整不确定管理帧可重开验证、同步后幂等确认；半帧原样保留并拒绝，不自动截尾或修复。

`deletion_boundary()` 明确返回只读边界：物理擦除 `NotPerformed`、日志 `HistoryRetained`、备份 `PolicyUndetermined`、外部副本 `Unmanaged`、已交付快照 `NotRecallable`。检索不可见不等于字节擦除，失效缓存/已持有快照可能留有历史正文。M3 不覆盖日志、不扫描/删除备份、不召回外部副本；政策未定时不扩大删除、查看或传输范围。完整有效旧日志替换仍无法检测，Windows 新文件名突然断电的目录耐久性仍不作保证。

```compile_fail
use moe_memory_local::DeletionBoundary;
fn forge(mut report: DeletionBoundary) { report.journal = report.physical_erasure(); }
```

验证：针对两个 crate 的 `tests/management.rs` 与适配故障测试，随后执行仓库 fmt、Clippy、workspace test/build 门。真实用户档案、模型连接和网络均不参与夹具。
