# moe-memory-search

R1–R4 离线工程：受信宿主显式调用的在线 Embedding、可关闭 Cohere v2 rerank、完整获准集合的 BM25/精确余弦/RRF、分块、有界索引队列、显式可丢弃文件缓存，以及 R3 的最小同步离线检索门面（`SearchMemory`）与窄授权桥。仓内依赖记忆核心与 memory-local，不依赖 Agent；agent-runtime 仅是测试 dev-dependency，Agent 经核心窄契约直接消费门面的离线接线已验证（见 R3 节），R4 已冻结中文标签集、补齐安全/Agent 回归并交付真实校验和合成性能入口；真实质量门仍需受信环境执行。普通构造、构建和测试不下载模型、不请求外部模型。

## 公开接口

- `OnlineEmbedding::new(endpoint, api_key, model, dimension, options)` 接受完整 `/embeddings` URL；`profile()` 用于核心 `RetrievalPlan`。`embed_query` 从密封票据取查询，`embed_entries` 只从本票绑定引用取正文，分别取得 QueryEmbedding/EntryEmbedding 许可。
- `CohereReranker::new` 只实现完整 `/v2/rerank` 协议，独立 `profile()`。`rerank` 接收最多 20 个本票候选引用，返回 `Some(引用/分数)`；核心计划未启用 rerank 时返回 `None`，不检查供应商可用性、不发送请求、不创建回执。
- `SearchHost` 只提供受信 `MemoryService` 与当前毫秒时间。宿主负责身份、查询完整来源声明及版本化策略；模型输入、API Key、Chat 预设、共享可读权限均不构成外传授权。
- `RetrievalHost` 是 R2 算法实际使用的新窄桥：`authority()` 返回 `&mut dyn SearchAuthority`，`now_ms()` 返回注入时钟；旧 `SearchHost` 通过 blanket impl 继续可用。`SearchAuthority` 仅涵盖 `validate_candidate_ticket`、`recover_failed_rerank`、`prepare_transmission`、`record_transmission`、`fail_transmission` 与 `cancel_current_tasks`（封装 generation 读取加 `cancel_tasks` 栅栏），分别实现于 `MemoryService` 与 `LocalMemory`；不暴露内核可变服务、日志或任何读取旁路。`LocalMemory` 的取消使用 `moe-memory-search-cancel-{generation}` 操作 ID（受信宿主保留此前缀，按代际幂等重试）；冲突或日志故障原样失败，不推进索引配置。
- `CallControl::new(Cancellation, budget)` 提供显式整体预算（最多 120 秒），可跨多次调用共享；等待请求、响应正文和重试退避时都检查取消/截止。传输取消不取代核心 `cancel_tasks`，已发内容也不能保证远端撤回。
- `EncodedQuery` / `EmbeddedCandidate` 的向量、profile 和完整输入快照不可由调用者构造或改写；Debug 脱敏。`exact_scan` 只在本票获准引用中比较，核对完整输入和向量空间，拒绝外来/重复引用，按余弦和稳定引用排序。宿主提供校准门槛，低于门槛允许空集，不为凑 k 补无关候选；最终仍须核心 `complete_retrieval`/交付复验。

配置示例（不发送请求）：

```rust
use moe_memory_search::{OnlineEmbedding, OnlineOptions};
let backend = OnlineEmbedding::new(
    "https://embedding.example.test/v1/embeddings",
    "synthetic-not-a-real-key", "configured-native-model", 512,
    OnlineOptions::default(),
)?;
let plan = moe_memory_core::RetrievalPlan::new(backend.profile().clone());
assert!(plan.rerank().is_none());
# Ok::<(), moe_memory_search::SearchError>(())
```

## 协议与边界

- OpenAI-compatible 请求为 `model`、字符串数组 `input`、`encoding_format: float`。当前不请求降维，只验证宿主声明的原生维度；响应 `model` 必须精确匹配，`data[].index` 必须完整覆盖输入，数量/重复/缺失/越界均拒绝。按原输入次序映射，而非相信远端返回顺序。模型别名不匹配时明确失败，不猜测等价版本。
- Cohere v2 请求为 `model/query/documents/top_n/max_tokens_per_doc`，要求完整 shortlist 返回；`results[].index/relevance_score` 严格映射，拒绝缺项、外来/重复索引和非有限/区间外分数。不会调用 Chat 协议、自动换供应商或补入其他候选。
- 每项输入按 UTF-8 字节拒绝超长，默认 1024、最多 4096；批次最多 32，rerank 最多 20，rerank 每个 query+document 另限 3072 字节并显式设 `max_tokens_per_doc=4096`。R1 的整条编码入口不切片，R2 的索引批入口只编码显式字符区间切片；都不静默截断、不把字符数冒称 tokenizer token 数。这是未知 tokenizer 下的保守拒绝规则，实际端点的 token 行为/是否截断仍须真实环境核验。
- 请求序列化最多 256 KiB，成功响应最多 8 MiB，长度声明和流式读取均受限；错误响应正文不读取/显示。向量以 f64 缩放后归一化，拒绝错维度、NaN/Inf、零范数；极大/极小有限值不因直接平方溢出而误判。
- 档案 SHA-256 绑定协议/处理规则、完整端点、模型、维度及有界输入配置；同维度不代表同空间。请求超时、尝试次数和退避时间只控制传输，不参与向量空间身份；三者及凭据轮换均不要求重建索引，但仍执行原有参数校验、发送授权与失败留证。本次修正改变旧档案指纹，旧缓存不可混用，需一次按新档案重建。该指纹仅为配置身份，不替代核心完整正文/人设快照证明；远端未固定服务版本仍可能漂移，不能冒称固定权重 revision。
- 端点只允许规范化 HTTPS 或字面 `127.0.0.1`/`[::1]` 回环 HTTP（协议测试）；拒绝 userinfo、query、fragment、含混 URL。无默认端点/凭据，禁重定向、Referer、系统代理与客户端内部重试。凭据 Header 标敏感，所有错误不包含原始传输错误、响应正文或密钥。
- 默认一次尝试；最多三次，仅 429/502/503/504 可重试。每次实际发送都重新取得核心票据并占用原有单票最多八次总额度，返回后再复验当前许可、全部实际输入和到期。支持 `Retry-After` 的 0–30 秒 delta-seconds；不支持的 HTTP-date/过大/非法值直接失败，不早于服务器要求重发。未知网络结果、超时、无效凭据/输入、非法响应不重试。
- 失败/取消/未知结果调用核心 `fail_transmission`：保守记录可能发送的输入，永久封闭候选票据，不允许用失败旧票回退交付或追加发送。已证实的暂时 HTTP 故障在完成退避、复验并成功预留下次发送后，才消耗本次回执并重试；若下次预留撞上额度/权限拒绝，仍用本次回执留证并永久封票；未完成/失败的请求不能假装成功。R2 的显式回退只允许经核心一次性换取保留全部发送证据且禁再发送的新票，不复活失败旧票，详见下节。

官方核验（2026-10-12）：[OpenAI Embeddings API](https://platform.openai.com/docs/api-reference/embeddings/create)、[Cohere v2 Rerank](https://docs.cohere.com/v2/reference/rerank)。回环测试校验 wire 请求、映射及工程边界，不证明真实模型语义质量或远端没有截断。

## I/O 依赖评审

直接版本精确固定，传递版本由 workspace Cargo.lock 锁定：`reqwest=0.13.5`（禁默认特性，仅 rustls）、`tokio=1.53.2`（直接仅 macros/time；测试加 rt/net/io-util/sync）、`serde_json=1.0.154`（有界 JSON）、`sha2=0.11.0`（复用运行层版本、禁默认特性，仅配置指纹）。HTTP/TLS/运行时均只进入适配层；无存储实现、模型 SDK、ONNX/ORT 或隐式模型下载。TLS 采用平台证书验证，当前 rustls 特性带入 AWS-LC 原生构建依赖；本机 Rust 1.99.0 / Windows 构建已验证，不承诺无 C/CMake 工具的机器可直接编译。

## 验证与显式真实探针

定向离线门：`cargo test -p moe-memory-search --all-targets --locked`。当前 56 项模块单测（R2 的 48 项加 R3 门面/窄桥/健康恢复 8 项，人工向量只在内部测试）、42 项协议/授权/探针回归（请求仅回环）、5 项 R3 公开 API 集成（`tests/facade.rs`：密封混合任务/拒绝矩阵、门面直接注入离线 Agent、缺未入选私密发送或查询来源许可拒绝、提议/写回完整依赖）。README 示例由 doctest 验证。默认测试没有外部服务调用或真实语义效果断言。

`cargo run -p moe-memory-search --example online_probe --locked` 是 R1 最小真实非子串 smoke，不是 R4 的 40 查询质量/延迟对照门。必须由受信宿主显式提供以下环境，不设置默认值，也不从 `.env` 自动加载：

- `MOE_SEARCH_ALLOW_SYNTHETIC_EMBEDDING=query-and-entry-private-source`：只批准该示例硬编码合成私密查询、两条正文及其合成来源的 QueryEmbedding/EntryEmbedding；另提供 `MOE_SEARCH_EMBEDDING_ENDPOINT`、`MOE_SEARCH_EMBEDDING_KEY`、`MOE_SEARCH_EMBEDDING_MODEL`、`MOE_SEARCH_EMBEDDING_DIMENSION`。
- rerank 默认关闭；启用需 `MOE_SEARCH_ENABLE_RERANK=yes`、单独 `MOE_SEARCH_ALLOW_SYNTHETIC_RERANK=query-and-shortlist-private-source`，以及 `MOE_SEARCH_RERANK_ENDPOINT`、`MOE_SEARCH_RERANK_KEY`、`MOE_SEARCH_RERANK_MODEL`。Embedding 的许可不替代 Rerank 许可。
- 缺设置、配置不匹配、请求失败或真实释义排序未严格优于干扰项均非零退出；常量向量、同分重排和按 ID 破平局不能通过；不 skip 后打印通过。探针不写记忆文件、不接入用户真实聊天或存档。已验证缺许可退出码 1、无请求，以及五个回环进程场景：常量向量（重排开/关）与同分重排退出 1，有区分的协议夹具退出 0；这些均为合成协议检查，不是真实模型门。未提供受信真实环境，真实 Embedding 非子串门与真实 rerank 效果均未验证。

## R2 混合排序与索引生命周期

- `SearchIndex::new(StorageDomain, EmbeddingProfile, ChunkConfig, queue_capacity, batch_limit, vector_capacity)` 只创建可丢弃派生状态。存储域必须由宿主绑定，不能取请求自报租户；完整私密身份或共享命名空间、父版本、切片配置/区间/顺序、正文 SHA-256 和完整 profile 共同组成键。指纹不是授权证明。
- `reconcile(host, ticket)` 在统计、排名和预算之前调用核心 `validate_candidate_ticket`，复验完整获准候选/查询来源/人设/发送快照。漏通知、正文纠正、撤权与到期不依赖通知可靠性；只清理本票身份/搜索范围及检索层共同覆盖的不可用活动向量和任务。WorkingContext 不清长期索引；FormalMaterial 与普通 LongTerm/All 是独立集合，互不因未包含而清空。未覆盖的旧向量不能参与本票排名，之后在覆盖它的获准候选复验中再清理真实更新/删除/撤权项。缺项返回 `PartialIndex`，词项仍覆盖完整获准集合，不以已有关键词 top-100 代替召回。
- `take_batch` 按分区/记录合并排队，容量 1–4096 个记录，批量 1–32 个切片，并行固定为 1；不可克隆批任务的存活租约阻止重复领取。丢弃批任务可重新领取，通知不授予正文或提交权。`OnlineEmbedding::embed_index_batch` 只编码该批密封切片，核心发送回执保守绑定完整父记录输入和全部来源。
- 索引扫描逐片借用父正文，不在队列/批量筛选前物化全部切片；队列、实际批次与向量条目共享不可变父记录快照，只为实际批次拥有切片正文。队列已满仍完整校验候选，扫描失败不提前修改队列或索引；公开 `split_chunks` 的返回类型与分块行为不变。完整扫描和字符边界计算仍有成本，不把内存改善称为常量时间检索。
- `commit(host, ticket, completed)` 同时检查当前核心完整快照、本票引用、域、模型、切片配置及本索引代际/租约；没有任意 ID→正文或裸向量写入入口。迟到、跨票据、错模型、超容量与失效结果显式报错，不写权威日志，也不回滚记忆提交。
- `rebuild(host, profile, chunks)` 清空派生状态，并复用核心现有 `cancel_tasks` 栅栏取消旧任务；持久化桥委托 `LocalMemory::cancel_current_tasks` 从完整幂等账本选取未占用的操作 ID，不与合法业务 ID 冲突，不修改 journal 格式。更换 profile/分块或活动 `HybridConfig`（含校准版本、阈值及重排策略）使旧票据/任务失效，宿主必须重新准备票据。首次排序绑定配置，后续配置变化的首次调用返回 `TaskCancelled`；不偷偷在旧票上重试。取消失败保留原错误，不自动重试或假称取消成功。
- `hybrid_rank` 使用明确模型绑定的 `HybridConfig`，不提供通用相似度阈值。纯词项降级须显式允许，严格模式缺查询向量或索引缺项则报错。向量取同记录有效切片的最高余弦，再与词项按 `sum(1/(60+rank))` 记录级融合；平局按原文精确子串信号及稳定引用解决。无词项命中且向量不过门槛允许空集，空查询沿用原列表顺序、不编码空查询。
- 词项版本为本实现 v1：连续拉丁字母/ASCII 数字转小写，中文连续字符重叠二元组，孤立单字用一元词项，标点/其他文字切边界；不做 Unicode 规范化。BM25 的 `k1=1.2/b=0.75` 仅统计本票完整集合。原文大小写敏感完整子串信号独立保留；原核心关键词入口的大小写归一子串语义不变。
- `ChunkConfig` 显式约束 Unicode 字符、UTF-8 字节、重叠和版本；切片优先段落/标题/列表/句末边界，字符区间完整覆盖正文，最多 1024 片，不丢尾部。它是保守字符/字节预算，**不是已验证 tokenizer**，不把示例参数称为 384/48 tokens 或远端不截断保证。核心 `complete_retrieval` 先复验完整候选/证据并对引用一次建表、检查全部排名（含交付限制之后的重复/外来/非有限项），再仅克隆预算内实际交付的完整正文，超预算整条省略；工作上下文只参加本次词项与统一字符预算，不持久化向量。
- `rerank_or_fallback` 消费实际核心票据和密封 `HybridRanking`；`Disabled` 不发重排请求，`Required` 故障报错，`AllowFallback` 才可见地保留 RRF。只发送过门的至多 20 条记录的命中切片，不补回未获准/无关候选。预发送传输拒绝与服务缺失可在读权限仍有效时本地回退；在途撤权、取消、整体截止、核心错误和额度拒绝不能回退。
- 已发送重排的超时/HTTP/协议故障永久封闭旧票；核心 `recover_failed_rerank` 一次性生成新 nonce/runtime 的**只收口、禁再外传**票据，完整重绑所有候选、QueryProof、人设和每次实际发送证据，之后仍需核心完成/交付/写回复验。RRF 回退不缩减私密发送依赖；硬失败通过 `RankingFailure::ticket()` 保留失败票及完整回执，不能因返回错误丢失留证。`RankingFailure::error()` 和标准 `Error::source()` 均保留内部 `SearchError`，错误链可继续取得核心或持久化原因。
- `HybridRanking` 提供 Hybrid/KeywordOnly/PartialIndex、获准候选/缺项/命中数、整条字符预算省略及本地排序/可选重排耗时。它不是 Embedding/装配的端到端延迟报告，也不是 R4 性能结论。不得将这些接口作为模型工具暴露。

纯离线词项降级示例（无向量、无外传许可，不声称真实语义命中）：

```rust
use moe_memory_core::{Category, EmbeddingProfile, Epistemic, MemoryDraft, MemoryScope,
    MemoryService, Namespace, PetIdentity, ReadPolicy, ReadPurpose, RetrievalPlan,
    RetrievalQuery, SessionId};
use moe_memory_search::{ChunkConfig, HybridConfig, RerankMode, SearchHost,
    SearchIndex, SearchMode, StorageDomain, hybrid_rank};
struct Host(MemoryService);
impl SearchHost for Host {
    fn memory(&mut self) -> &mut MemoryService { &mut self.0 }
    fn now_ms(&mut self) -> u64 { 100 }
}
let mut host = Host(MemoryService::new());
let owner = PetIdentity::new(Namespace::new("fixture", "user")?, "alice")?;
let source = host.0.register_source(MemoryScope::Private(owner.clone()),
    "synthetic", 1, Epistemic::UserStatement)?;
host.0.for_pet(&owner).create(MemoryDraft::new("tea before breakfast",
    Category::Episode, Epistemic::UserStatement, vec![source.clone()]), 100)?;
host.0.set_read_policy(&owner, ReadPolicy::offline(1, vec![source]))?;
let profile = EmbeddingProfile::new("unconfigured", "fixture-model", 2)?;
let session = SessionId::new("session")?;
let ticket = host.0.for_session(&owner, &session, ReadPurpose::OfflineReasoning)
    .begin_candidates(&RetrievalQuery::new("tea"), RetrievalPlan::new(profile.clone()), 100)?;
let mut index = SearchIndex::new(StorageDomain::new("host-bound-root")?, profile.clone(),
    ChunkConfig::new(1, 512, 16, 1024)?, 32, 16, 1024)?;
let config = HybridConfig::new(profile, 1, 0.8, true, RerankMode::Disabled, 20)?;
let ranked = hybrid_rank(&mut index, &mut host, &ticket, None, &config)?;
assert_eq!(ranked.mode(), SearchMode::KeywordOnly);
let task = host.0.for_session(&owner, &session, ReadPurpose::OfflineReasoning)
    .complete_retrieval(&ticket, ranked.ranking(), 100)?;
assert_eq!(task.result().items().len(), 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## R3 离线检索门面与窄授权桥

`SearchMemory` 是受信宿主组合 `LocalMemory` 的最小同步离线门面，不复制权威库、不暴露可变 inner：

- `SearchMemory::new(local)` 是明确的关键词兼容构造：`for_session` reader 的 `begin_task`/`retrieve`/`identity` 与 `MemoryContextAccess`（`begin_memory_task`/`assemble_context`/`context_view`）全部转发 `LocalMemory` 健康门语义。
- `SearchMemory::with_ranked(local, ranked)` 从既有密封 `RankedCandidates` 接收宿主已经完成的混合排名（内部经 crate 专属 `into_parts`）。准备态按 identity/session/purpose/完整 `RetrievalQuery` 精确匹配，仅匹配消费一次（复验失败也不恢复待交付态）；每次先 `LocalMemory` 健康门、当前票据复验，再核心 `complete_retrieval`，候选/查询证明/人设/全部实际发送证据原样保留。失配、过期、失败与已消费一律报错，绝不退回关键词；混合模式下 reader 的 `retrieve` 也被拒绝。
- `SearchDiagnostic` 镜像 `HybridRanking` 的模式/缺项/命中/预算/耗时，不含任何正文，也不是端到端延迟或质量结论。
- `LocalSearchHost::new(&mut local, now_ms)` 显式借用 `LocalMemory` 与注入时钟驱动现有 R2 排名；本任务不执行 HTTP，离线测试可用 rerank `Disabled` 或词项降级。`SearchError` 保留原 `PersistenceError`（手工实现 `PartialEq` 并保留 `Eq`，既有变体比较兼容；持久化 I/O 错误仅按 `ErrorKind` 比较，不比较底层消息）；`RecoveryRequired` 等持久化故障永不作为可降级错误。

纯离线门面示例（关键词排序密封后一次性交付，不声称向量效果）：

```rust
use moe_memory_core::{Category, EmbeddingProfile, Epistemic, MemoryDraft, MemoryScope,
    Namespace, PetIdentity, ReadPolicy, ReadPurpose, RetrievalPlan, RetrievalQuery, SessionId};
use moe_memory_local::{LocalMemory, OperationId};
use moe_memory_search::{CallControl, Cancellation, ChunkConfig, HybridConfig, LocalSearchHost,
    RerankMode, SearchIndex, SearchMemory, StorageDomain, hybrid_rank, rerank_or_fallback};

# async fn scenario() -> Result<(), Box<dyn std::error::Error>> {
let directory = std::env::temp_dir().join(format!("moe-search-doc-{}", std::process::id()));
std::fs::create_dir_all(&directory).unwrap();
let mut local = LocalMemory::create(directory.join("memory.journal"))?;
let owner = PetIdentity::new(Namespace::new("docs", "user")?, "alice")?;
let source = local.register_source(OperationId::new("doc-source")?,
    MemoryScope::Private(owner.clone()), "synthetic", 1, Epistemic::UserStatement)?
    .value().clone();
local.for_pet(&owner).create(OperationId::new("doc-create")?,
    MemoryDraft::new("tea before breakfast", Category::Episode, Epistemic::UserStatement,
    vec![source.clone()]), 100)?;
local.set_read_policy(&owner, ReadPolicy::offline(1, vec![source]))?;
let profile = EmbeddingProfile::new("unconfigured", "doc-model", 2)?;
let session = SessionId::new("session")?;
let query = RetrievalQuery::new("tea");
let ticket = local.for_session(&owner, &session, ReadPurpose::OfflineReasoning)
    .begin_candidates(&query, RetrievalPlan::new(profile.clone()), 100)?;
let mut index = SearchIndex::new(StorageDomain::new("doc-root")?, profile.clone(),
    ChunkConfig::new(1, 512, 16, 1024)?, 32, 16, 1024)?;
let config = HybridConfig::new(profile, 1, 0.8, true, RerankMode::Disabled, 20)?;
let ranking = {
    let mut host = LocalSearchHost::new(&mut local, 100);
    let ranking = hybrid_rank(&mut index, &mut host, &ticket, None, &config)?;
    let control = CallControl::new(Cancellation::default(), std::time::Duration::from_secs(1))?;
    rerank_or_fallback(&index, &mut host, ticket, ranking, None, None, &control).await?
};
let mut facade = SearchMemory::with_ranked(local, ranking);
let task = facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100)?;
assert_eq!(task.task().result().items()[0].text(), "tea before breakfast");
assert!(task.diagnostic().is_some());
assert!(matches!(
    facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100),
    Err(moe_memory_search::SearchError::InvalidInput(_))
));
drop(facade);
std::fs::remove_dir_all(&directory)?;
Ok(())
# }
#
# tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(scenario())?;
#
# Ok::<(), Box<dyn std::error::Error>>(())
```

`agent-runtime` 仅为 dev-dependency，Agent 正常依赖仍只有核心窄契约，不反依赖 search。受信宿主通过 `AgentServices::new(&mut sessions, &mut facade)` 注入 SearchMemory；混合票必须按真实 `AgentBinding::memory_session()` 和同一身份/用途/完整查询准备。prepare 检查完整读取集与所有发送来源，infer/finish 经健康门重验原受保护上下文；错误完整保留为 `AgentError<SearchError>`，不调用无记忆模型或切回关键词。直接 prepare→infer→finish、查询来源和未入选私密发送项越 grant 拒绝均有离线回归。合成回执只验证留证/依赖判据，不证明真实网络发送或模型质量。

此门面是同步、宿主预先完成排名的最小离线注入，不在 Agent 内启动异步编码、重排、调度或 HTTP。`LocalSearchHost::new(local, now_ms)` 使用宿主本次注入时间，宿主应在阶段边界更新；不能将固定离线时间误称为真实远端调用期间持续采样的时钟。真实模型端到端/质量门仍未验证；R4 的离线入口与合成基准不提升该状态。

## R4 冻结质量集与显式校验

[冻结夹具](tests/fixtures/r4-chinese-v1.json) 的 SHA-256 为 `2d50243e19510c41514c1299e682ebbd8c490c7cefed18f06423b5c3f32dc6a8`，由 `.gitattributes` 固定 LF。包含 32 条有答案查询（16 私密、16 共享，其中 4 条显式正式资料）、8 条无相关查询，恰好 16 条释义查询；手工冻结相关性等级为 3，未列出为不相关。另有不重叠的 8 条校准记录、12 条校准查询（8 有答案、4 无相关），评测标签不参与阈值选择。未做外部人工盲审，不把标签映射的人工向量当真实模型。

`embedding_check` 是受信宿主独立运行的真实质量入口，不是自动测试、生产后端或模型工具。没有隐式后端；`local` 明确未实现、非零拒绝且不下载资产、不回退 online。支持的在线路径与开关：

```powershell
cargo run -p moe-memory-search --example embedding_check --locked --offline -- --backend online --rerank off
cargo run -p moe-memory-search --example embedding_check --locked --offline -- --backend online --rerank on
cargo run -p moe-memory-search --example embedding_check --locked --offline -- --backend local --rerank off
```

这里 `--offline` 仅限制 Cargo 依赖解析，**不**禁止明确获准的在线 HTTP。执行前必须由受信宿主在当前进程逐项提供环境设置；示例不枚举凭据库、不继承 Chat 许可：

| 设置 | 要求 |
| --- | --- |
| `MOE_R4_ALLOW_EMBEDDING` | 精确值 `synthetic-only:query+entry:private+shared:all-fixture-sources`，仅许可本冻结合成集的 QueryEmbedding/EntryEmbedding、私密与共享范围及全部来源。 |
| `MOE_R4_EMBEDDING_ENDPOINT` / `MOE_R4_EMBEDDING_MODEL` / `MOE_R4_EMBEDDING_DIMENSION` / `MOE_R4_EMBEDDING_REVISION` | 固定完整 `/embeddings` 端点、原生模型/维度及宿主记录的服务修订；不能固定远端权重时修订只是声明，不是密码学证明。 |
| `MOE_R4_EMBEDDING_KEY_REF` | 显式指定一个 `MOE_R4_` 前缀的大写环境变量名；该变量的值才是凭据，不直接把真实 Key 写入命令或文档。 |
| `MOE_R4_ALLOW_RERANK` | 仅 `--rerank on` 需要，精确值 `synthetic-only:query+shortlist:private+shared:all-fixture-sources`；不继承 Embedding 许可。 |
| `MOE_R4_RERANK_ENDPOINT` / `MOE_R4_RERANK_MODEL` / `MOE_R4_RERANK_REVISION` / `MOE_R4_RERANK_KEY_REF` | 独立完整 `/v2/rerank` 端点、模型、修订及同规则凭据引用。 |

缺资产/配置/许可、维度或协议不符、调用失败、以及质量不达标均非零退出，不 skip 后报通过。先以独立校准的真实余弦分数在固定 `0..=1`、步长 `0.02` 网格选择门槛（最大正确数，并列取更严格值）；再以同语料/权限/条数与字符预算运行新词项 KeywordOnly、严格完整索引 Hybrid、可选 Required rerank。不能改标签、删困难样本或使用评测集重新调参。

冷、热路径分别统计并要求 Hit@5 ≥28/32、释义 ≥12/16 且严格优于关键词、无相关空集 ≥7/8、越权/失效为 0。输出每条查询的固定编号和交付记录编号、噪声/空集、按有答案 32 条平均的 NDCG@5、冷建索引/编码/召回/rerank（含该阶段复验）/装配及热端到端时间；不输出查询正文、凭据或原始响应。每次在线调用最多一次尝试，调用数按阶段报告，远端用量/费用当前适配未暴露则明确 `unavailable`。rerank 另报命中与 NDCG 差，未必有收益，默认关闭。

该入口的宿主在每个 `now_ms()` 检查点动态采样系统时钟，不使用固定时间的 `LocalSearchHost` 冒充真实在途时钟。128 字符/16 重叠/1024 UTF-8 字节只是保守分块/长度拒绝规则，**不是 tokenizer 或远端无截断证明**；即使真实质量达标，也明确输出 tokenizer 和整体未验证边界，不能宣布完整 R4/RAG 通过。

标准测试仅计算指标、运行关键词和使用明确的回环人工向量/重排桩：已知关键词结果固定为 Hit@5 17/32、释义 1/16、无相关空集 7/8、NDCG@5 0.531250；真实 Hybrid/rerank 的对应值尚未取得。常量人工向量必须不能过质量门，Required rerank 故障不能返回降级摘要。完整安全回归与实际验证记录见 [REVIEW](../../docs/REVIEW.md#r4-离线工程与最终复核2026-10-13)。

## R4 合成规模基准

```powershell
cargo run --release -p moe-memory-search --example scan_bench --locked --offline
cargo run -p moe-memory-search --example scan_bench --locked --offline -- 100
```

默认 1,000/10,000 条合成记录；显式小规模参数用于针对性检查。仅连接本进程 `127.0.0.1` 的人工向量端点，经过完整 `take_batch → embed_index_batch → commit` 授权路径填满 `SearchIndex`，最终条目数必须等于规模且缺片数为零。每票至多 8 次发送、换新票继续排空 4096 队列；每票/查询阶段另设不超过 120 秒的控制预算，不更改生产硬限制。

报告建库、完整首次索引、回环编码、核心/索引簿记、冷查询与 7 次热查询 p50/p95、召回/收口/装配；逻辑向量负载按实际 `f64` 的 8 字节计，不是 RSS/完整索引堆占用。文件缓存关闭，磁盘大小 `unmeasured`。固定合成权威时间仅用于离线夹具，墙钟耗时由 `Instant` 采样。10k 完整快照复验成本如实保留，不通过分片或缩小候选伪装完整规模性能；此基准不能证明真实语义、tokenizer、在线费用或 rerank 收益。

## 显式可丢弃文件缓存

默认无文件 I/O；未启用缓存仍可通过在线批编码和内存扫描工作。宿主使用 `enable_file_cache(protected_directory, max_bytes)` 显式提供**已存在、同受保护存储域**目录和 1 KiB–64 MiB 配额；不自动创建目录或 journal。固定缓存文件名 `moe-memory-vectors.cache`，不接受模型提供的路径。

`save_file_cache(host, ticket)` 只保存当前完整获准长期候选的现有向量、格式/域/完整分区/版本/模型/分块清单和正文指纹；不保存原文、查询、会话、人设、来源正文或凭据。向量仍是敏感派生数据，不宣称匿名/公开/不可反推。格式 v1 使用有界长度、SHA-256 完整性封套和 JSON；校验和不是防伪授权能力。

`load_file_cache` 先过核心健康/授权快照门，再校验格式、大小、校验和、重复键、维度、有限值和单位范数，与**当前密封候选及实际切片键**相交后才重绑当前完整输入。坏档/缺档/旧版本用 `CacheUnavailable` 或 `ProfileMismatch` 可见地丢弃重建，不从文件恢复票据或授权，原日志不受影响。

完整快照经同目录 `create_new` 临时文件、写入、文件同步和 rename 替换；Unix 另尝试目录同步。Windows 目标占用或替换受限时可失败并保留旧可验证快照，未宣称跨平台绝对原子/断电耐久。既有 `.pending` 不覆盖、不删除；本次创建的临时文件失败时按单文件清理。`clear_file_cache` 只删除固定缓存文件，不递归处理目录、权威根、journal 或备份；删除不等于物理擦除。

总体批次与未验证边界见 [MEMORY](../../docs/MEMORY.md#rag-检索扩展分批施工)、[REVIEW](../../docs/REVIEW.md#r2-交付记录混合检索和索引生命周期2026-10-12)。
