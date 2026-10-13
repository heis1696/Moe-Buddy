# 审核与验收

> 更新：2026-10-13。对象：Moe-Buddy 已复核的记忆、Agent 与 RAG 离线底座及主分支集成。M0–M5、上下文精进与 G0–G5、RAG R0–R4 离线范围的证据分阶段记录；R4 三项 P2 修复后及主目录集成复验的独立清单均为 879 项（含 doctest 及 3 项显式 example 测试），R3 的 848 项仅为历史快照。R4 安全回归、冻结质量工程、显式真实校验入口与合成基准不推进 G6–G8，不接真实对话模型、对外 HTTP 网关、渠道或生产 Hooks；施工和复核当时未执行领队 commit/push/建分支，后续用户明确授权的合并、提交与推送见主分支集成记录。
> 结论：整体暂不能验收，R4 真实完整质量门尚未完成；用户后续复核复现的三项工程 P2 已修复并补回归，证据见 R4 同节。两项留存 P1 已修复；R4 独立审查和已完成定向回归范围内未发现新的已复现 P0/P1；审查提出的三项安全覆盖 P2 和两项质量工程 P2 均已补齐。Agent 具体记忆耦合已在离线范围经三方法核心窄契约关闭（P2-B）；夹具公开、职责集中和全局失效等整理项仍保留。离线/回环全绿不代表真实模型、生产闭环或完整 RAG 验收通过。

## 1. 修复闭环

### 1.1 收紧期限必须随净化结果持久化

- 根因：净化用收紧值判定保留，却把旧期限复制进保留片段；恢复只在内存收紧，二次重开能重新放宽。
- 修复：`crates/moe-agent-runtime/src/session/persistence.rs:515` 的 Keep/Transfer 两分支均保存有效正常截止、硬截止及已固定最终截止。正文、事件不改写，Keep 不推进生命周期版本，Transfer 沿用既有版本递增；移除分支继续保留稳定键、撤销及来源水位。
- 回归：`crates/moe-raw-local/src/tests.rs:593` 使用实际临时文件先存宽松政策、以严格政策恢复、直接关闭且不额外 checkpoint、再以宽松政策重开，分别覆盖正常区与提前软删回收区、最终截止等号与超过截止。修复前两测试失败，修复后通过。
- 纯快照回归：`crates/moe-agent-runtime/src/session/persistence.rs:2350` 覆盖 Keep、正常截止等号 Transfer、Recycle Keep；编码/解码后再用宽松政策净化仍不得放宽，正文与事件保持一致，到最终截止等号移除。
- 修正旧测试中“严格政策下整份快照编码必须不变”的错误预期：仅正文保真，控制期限必须收紧；新预期仍比较完整确定性编码，不删断言或扩大容忍范围。

### 1.2 导入账本不再拥有原文

- 根因：导入账本为了精确重复判定克隆完整快照；会话 purge 或到期后，该副本仍拥有完整正文。
- 修复：`crates/moe-agent-runtime/src/session/persistence.rs:169` 的记录只保留稳定域、32 字节规范内容指纹、片段键集合、首提交时间和首导入计费字节数，不含快照、事件或可逆正文编码。所有导入从一开始就不建立该长期副本，不需要给每条删除路径另补清理钩子。
- 指纹复用 v1 内容区编码：正文、域、授权修订、期限、生命周期、来源顺序、元数据及撤销栅栏均参与；提交时间排除。首导入的指纹计算/预算检查在状态发布前完成，失败不落半记录。
- 保留既有幂等及防复活契约：相同快照只解析当前存活键，清理后返回空片段；异指纹冲突，清理不删除导入记录、不释放累计接纳预算、不复用稳定键，不解除撤销栅栏。FNV32 碰撞拒绝回归继续通过。
- 内部回归：`crates/moe-agent-runtime/src/session/persistence.rs:2147` 分别走公开 purge 与最终截止维护，验证会话释放、账本只包含穷尽列出的无正文字段、同快照不复活、改正文仍冲突。修复前“内部快照仍持有事件”断言失败；更换账本结构后以无正文字段结构及行为验收，不只检查公开导出为空。
- 边界：清理不追回调用方已持有的快照/回执或外部备份；释放 String/临时编码缓冲不等于物理内存擦除。长期记忆 journal 的逻辑删除边界没有因这次原文修复而扩大。

### 1.3 指纹依赖取舍

本轮在运行层增加一个直接依赖 `sha2 0.11.0`，关闭默认特性；锁文件新增 9 个包，记忆核心依旧零第三方依赖。仅调用 SHA-256 纯计算，不引入文件/网络/系统时钟、异步运行时、模型 SDK 或云服务；公共 API 与 v1 线格式不变。

不选保留全文或压缩/编码副本（仍可恢复原文），不选 FNV/默认集合哈希（不能承担抗碰撞判重），不手写密码学，不引入宿主指纹插件体系。另一种零依赖方案是删除后拒绝所有重放，但会改变既有幂等回执语义，本次不采纳。此选择是有明确用途的窄依赖例外，已经同步运行层依赖说明，不再虚称该 crate 只有 std。

指纹判等依赖密码学抗碰撞假设，不是数学上的逐字节相等证明，也不作为权限或真实性凭据。计算临时编码缓冲受 16MiB 上限约束，不常驻账本；这不是安全擦除或匿名化功能。

## 2. 两系统总体判断

此处把“封闭单一”按封装、开闭原则与单一职责分别检查；权限/内容语义的封闭不等于所有枚举必须可无限扩展，也不等于所有宿主入口只能有四个 CRUD 方法。

| 维度 | 记忆系统：core + local | Agent/网关：runtime + raw-local + cli-hooks |
| --- | --- | --- |
| 封装 | 基本通过：模块、Store、Journal、集合及 LocalMemory 内部 service 不对外开放；只导出门面、命令和只读快照。 | 基本通过：身份、请求、能力、任务及注册表不能由外部直接改内部字段；宿主根入口与模型输入分开。 |
| 单一职责 | core 管记忆规则，local 管持久化与提交；M5 离线编排也进入 local，职责已有扩散，见 P2-A。 | 预设/鉴权/Hook/CLI 编解码有拆分；SessionRuntime 聚集会话、交互、批次、管理、恢复等多类变化原因，见 P2-C。 |
| 开闭与解耦 | 不依赖宿主、GUI、模型 SDK、网络或存储实现；Journal 私有可替换，当前只交付一个文件后端，不必提前公有化插件。 | 依赖单向，无对 raw-local/cli-hooks 的反向 import；模型有离线注入接口，R3 后 AgentServices 经核心窄契约接内存/持久化/检索门面，见 P2-B；不反依赖 search。 |
| 设计规模 | 未发现大量空 crate、全能 Agent 框架、向量/图数据库或通用插件引擎。 | 六个实际承担工作的 crate（含唯一检索适配）合理；目前没有理由再拆很多公共 crate 或上通用事件总线。 |
| 防御策略 | 权限/来源/版本/派生闭包/取消/提交验证必要；全量克隆事务有成本但不是无意义保护。 | 撤权、迟到结果、未知回执、防重放及恢复鉴权必要；全局失效、进程寿命额度和局部重复校验值得收敛。 |
| 测试与生产 | 存在公开 OfflineFixture 和离线编排，不满足“测试构造完全不进入库公共 API”。 | 合成 Hook 程序留在 examples，故障注入受 cfg(test) 限制；未发现默认生产路径使用合成凭据或把离线验证冒充生产联调。 |

源码依据：`crates/moe-memory-core/src/lib.rs:3`、`crates/moe-memory-core/src/store.rs:29`、`crates/moe-memory-local/src/lib.rs:3`、`crates/moe-memory-local/src/api.rs:118`、`crates/moe-memory-local/src/journal.rs:11`、`crates/moe-agent-runtime/src/lib.rs:3` 及各 crate 的 Cargo.toml。preset 中 std::net 的 IP 类型只用于地址解析，不是网络访问。

## 3. 分级整理意见

P2 是正式宿主接线前应收口的维护/架构项，不等同于已发生越权或原文丢失；P3 是非阻塞清晰度/性能建议。以下不在本轮擅自重构或修改公共接口。

### P2-A：测试构造确实进入了公共库接口

**证据**：`crates/moe-memory-local/src/lib.rs:12` 无条件导出 OfflineDialogue/OfflineFixture；`crates/moe-memory-local/src/dialogue.rs:140` 的 Candidate/Unavailable/Malformed 是固定离线构造，`crates/moe-memory-local/src/dialogue.rs:333` 的 generate 直接消费它，并非模型适配接口。

**判断**：这是测试/示范职责进入常规发布面的确定事实，不是“已经接了真实模型”。命名、README 与 M5 契约明确写离线，仅供夹具，未发现用它虚报生产交付。`crates/moe-agent-runtime/src/agent.rs:557` 的 OfflineModel 则是实际可注入的离线边界，不应仅因带 Offline 名字就认定为假实现；该名字也不是能限制宿主联网的安全沙箱。

**建议**：未来让通用对话编排归 Agent 适配层，Candidate/Unavailable/Malformed 等固定构造进入 examples/tests 或明确的可选离线支持模块；保持记忆存储门面只负责政策、读写与提交。不要为清理命名而把 OfflineReasoning 改成真实传输授权，更不能简单改名后宣布生产完成。

### P2-B：记忆门面替换点（R3 离线范围已关闭）

**原问题**：AgentServices 固定持有 `&mut MemoryService`，不能直接复用 LocalMemory；公开可变 service 会绕过 journal，不能用它补接线。

**R3 修复**：核心 `MemoryContextAccess` 仅包含建授权任务、装配和复验上下文，关联错误类型保留原后端错误。MemoryService / LocalMemory / SearchMemory 实现此窄契约，AgentServices 泛型化且支持 trait object，默认参数保留既有内存关键词调用。LocalMemory 新票据/传输入口均先 `ready`；Agent 仅依赖核心，不接触索引或可变内核。

**证据与边界**：LocalMemory trait-object、SearchMemory 混合任务直接 prepare→infer→finish、完整发送/查询来源 grant 及健康故障拒绝均已测试。这里只关闭离线替换点，不代表异步在线模型编排、生产持久化回复/提议流程或渠道联调已完成；当前 M5 夹具编排仍见 P2-A。

### P2-C：SessionRuntime 内部的变化原因较多

**证据**：`crates/moe-agent-runtime/src/session.rs:105` 起维护会话、交互、批次及多类账本；子模块 lifecycle 同时承担回收、管理权限与撤销栅栏，persistence 同时承担稳定域、编解码、预算、历史授权与恢复发布。后者当前 2538 行，包含内部测试，不能把总行数等同于业务复杂度。

**影响**：当前只是代码文件拆分，同一状态权威仍承接多个职责；留存 bug 恰好穿越净化、导入、文件恢复三层，阅读和改动验证成本高。但它们要协同原子发布，盲拆多个状态所有者同样危险。

**建议**：先在 crate 内拆纯 codec/快照净化、历史授权账本与恢复计划，最终仍由 SessionRuntime 统一预检和提交，外部接口不变。不为“单一职责”增加事件总线、共享可变状态或跨层回调。

### P2-D：全局失效与寿命配额是保守设计，不宜冒充长期运行方案

**证据**：`crates/moe-agent-runtime/src/ingress.rs:385` 的路由变更推进统一 generation，旧请求按 `crates/moe-agent-runtime/src/ingress.rs:889` 拒绝；`crates/moe-agent-runtime/src/agent.rs:295` 的 issued 账本只增长并按 tasks 限额拒绝新任务。管理/历史许可、撤销及导入账本也有累计限额；修复后导入字节预算仍是累计接纳量，不是实际正文占用。

**影响**：修改无关路由也会打断其他请求；片段已清理不代表任务/许可寿命额度释放。当前是有界原型的明确背压策略，防止无限增长和 ABA，不是逻辑 bug；但不能等到生产运行耗尽才发现要重建并重新授权。

**建议**：有真实运行数据后再决定是否按路由/凭据细分代际、给已终结任务压缩墓碑或提供受信轮换；继续禁止删除栅栏后直接复用旧句柄。未获明确需求前不擅自放宽配额，不把安全检查全部移除来解决可用性。

### P3-E：有局部重复复验与全状态复制成本

**证据**：`crates/moe-agent-runtime/src/agent.rs:382` 的 validate 已调用 context_view，infer 在 `crates/moe-agent-runtime/src/agent.rs:397` 未经外部调用又验证一次。`crates/moe-memory-local/src/api.rs:481` 每次提交克隆整个 service，写盘同步后才发布 candidate（`crates/moe-memory-local/src/api.rs:493`）。

**判断及建议**：同一次同步调用内的重复读取可在下一次接口收口时共用验证结果；推理前与最终交付前的跨阶段复验必须保留。全状态复制当前支持失败原子性，不是无意义防御；未做大库基准，不宣称已有性能故障。仅在真实瓶颈出现后考虑最小变更集/事务计划，不先引入数据库和复杂异步事务。

## 4. 不应误删的边界

- 受信身份、私密/共享归属、来源版本、读取用途与模型传输不是同一层授权；共享可读不等于共享可写，记忆里的工具说明不授予执行权。
- 模型只提议，正式协议/身份锚点受保护；纠正/遗忘联动派生闭包，不能靠摘要、卡片或重放绕回原文。
- 跨异步边界的取消/版本/权限复验、部分写出 unknown、禁止未知自动重发、错误后 poison 和恢复 CAS 都有实际故障模型，不按“检查多”删掉。
- 长期记忆 journal 与原文快照分离合理：前者追加受控命令并保留历史回执，后者必须按留存清正文。不要为了复用 codec 把聊天原文塞进长期日志。
- 不把本轮正式化的缺陷回归视为“测试案例转正”：修复前能失败、修复后约束真实公共行为的回归是正当测试；应警惕的是把固定输入/输出和离线支持当成生产业务实现。

## 5. 验证与未验证范围

- 新增 5 个正式回归：文件两次重开 2 个、导入账本 purge/最终到期 2 个、快照三分支期限固化 1 个；既有测试未删除，旧错误期望按正确期限契约收紧。
- 注册清单 679 项：运行层 279、记忆两 crate 314、原文文件 19、CLI 67。workspace test 包括 doc tests，子进程辅助执行不另计成新用例。
- 已通过 `cargo fmt --all -- --check`、全目标 `cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`、`cargo build --workspace --locked`、设置 `RUSTDOCFLAGS=-D warnings` 的 workspace doc，以及修复轮当时 23 份活动 Markdown 的文档门、`git diff --check`；后者的 CRLF 提示不是校验失败。
- 构建/测试输出在 `target/bug-architecture-review-20261012`；已用既有 `scripts/g5-source-manifest.py` 生成该目录下 manifest，修复轮 86 文件的 LF 清单 SHA-256 为 `043a4c0263047a2bc1397e618ecb00905de813ee8a891cfa154b8e9b0844ffea`。该指纹属于文档合并前的修复快照，不是维护后的当前源码标识；脚本包含 crate README，因此仅改文档也可能改变新清单。旧 G5 指纹未改写。
- 仅本机 Windows 的纯逻辑、临时文件和子进程测试；未跑真实 HTTP/模型/微信/QQ/生产 Hook，没有 Unix 真机或断电测试。G5 固定 CLI 版本证据沿用历史契约，本轮没有重新进行上游版本选型。
- 后续阶段检查结果：规划保留 G6–G8，但用户当前要求暂停后续、先修复和审核；本轮到此结束，不以计划表为由自动开工。架构整理意见也不是新增阶段授权。

## R0 交付记录：RAG 纯契约与授权票据（2026-10-12）

- 范围：按 [MEMORY §R.8](MEMORY.md#r8-可派工批次) 仅交付核心纯契约。初版及第一次补修没有通过权限/证据审查；本节描述领队集成后的加固版本，不沿用队员的 697/704 项数字作为当前验收证据。
- 核心接口：`MemoryReader::begin_candidates` 全量扫描获准候选（不受 100 条结果上限约束），密封运行证明、实例、会话、查询、配置、策略、代际和候选版本；`AuthorizedCandidate` 的候选引用另绑定票据 nonce，相同 ID/版本的另一存储域、另一票据及未绑定快照也不能混入；`complete_retrieval` 拒绝外来/重复引用和非有限分数，重取权威正文并执行条数/Unicode 字符预算。混合 `MemoryTask` 按实际交付引用、顺序、身份和全部发送集复验，不重跑关键词排名；旧关键词入口与测试预期保持不变。
- 权限闭环：`TransmissionPolicy` 分别绑定 QueryEmbedding/EntryEmbedding/Rerank、端点、显式 `with_scope` 数据范围和精确来源。来源标签不能替代实际正文归属。查询默认本宠私密，受信宿主必须先以 `bind_query_provenance` 声明完整来源和派生记录/工作上下文引用，再取得查询许可及私密范围许可；Shared 搜索不改变查询归属。关闭 rerank 的计划不允许重排传输。
- 在途闭环：候选/发送票据均不可克隆，独立 nonce 防止同序号、同会话或服务克隆快照的票据混用。发出前预留发送名额，未回执阻止任务交付；回执先留审计，再按注入时间复验全部来源、记录/查询依赖、权限及到期等号。失败永久封闭该票据，交付后不能再追加发送。实际发送而未入选的全部来源与引用强制进入派生声明和依赖；工作上下文仍不能持久化。共享发布同时检查实际发送/查询归属与来源，不因 private 记录引用 shared 来源或裁剪 top-k 而洗白。
- 分叉快照补修：最终独立复核另确认两项 P1——同 epoch/代际/ID/版本但正文不同的服务分叉，可替换收口正文或错挂查询/未入选发送依赖。现已在候选收口、查询来源绑定、发送准备、回执、交付和写回逐项比较完整输入快照；`TransmissionTicket` 保存实际输入快照，`HybridSeal` 只保留实际查询/发送引用对应的完整快照，另密封并复验人设视图。正文证明仅留在有界运行期对象，不进 request/journal/磁盘，不以非抗碰撞哈希代替正文比较。新增 6 项正式回归含空/非空交付、直接/命令写回及未分叉克隆正例；旧关键词的克隆语义不改。
- 兼容与额度：仅新混合任务的运行期证明和有界 request 增加规范化发送证据（完整作用域、来源/引用、用途/端点、策略版本及授权/回执时间）。专用前缀与旧合法查询长度布局互斥；实际发送集不同即为不同幂等请求。单票最多预留/记录 8 次发送，每次最多 32 个引用/来源，混合 request 最多 64 KiB。既有命令/journal/TaskStamp 磁盘布局不变；解码仍不能恢复 LiveProof。
- 定向回归：`crates/moe-memory-core/tests/hybrid_retrieval.rs` 的 37 项全部通过，覆盖全量候选、非子串选择、A/B/跨服务隔离、非法引用/分数、预算、身份保护、版本/撤权/取消、当前时间与祖先到期、查询来源/私密范围、同序号 nonce、预留/失败/终态、共享来源的私密记录防洗白、纯共享正例、未入选发送和查询依赖的持久化失效、工作上下文禁持久化、发送集幂等及解码无现场证明。另以 compile-fail doctest 固定票据不可伪造/克隆边界。
- 实际 Rust 门：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`、`cargo build --workspace --locked`、设置 `RUSTDOCFLAGS=-D warnings` 的 `cargo doc --workspace --no-deps --locked` 全部通过。`cargo test --workspace --locked -- --list` 的独立清单为 723 项（含 doctest，不重复计算子进程输出）。日志位于本机忽略目录 `target/r0-validation-20261012`。`powershell -NoProfile -File scripts/check-docs.ps1`（10 份活动 Markdown 的 UTF-8/路径/锚点/格式）与 `git diff --check` 亦通过，CRLF 提示不是失败。
- 未验证边界：核心仍零第三方依赖，无模型、网络、文件或系统时钟访问；档案类型仅为配置身份，人工相关分数不冒充真实 Embedding。R1–R4 的真实编码/rerank、协议、索引、记忆门面和 Agent 接线、质量门尚未交付；G6–G8 仍暂停。未发生真实外传，也不承诺召回远端已接收副本；查询证明是受信宿主声明完整输入后的密封，不证明自然语言来源天然可信。运行期票据不跨进程恢复，重启须重新授权；`MemoryService::clone` 的既有内存快照孪生语义未改。

## R1 交付记录：真实编码及模型适配（2026-10-12）

- 实现范围：新增唯一 `moe-memory-search` crate，OpenAI-compatible float Embedding、官方核验的 Cohere v2 可选 rerank、封闭编码结果及精确余弦扫描；不实现 R2 的分块/BM25/RRF/索引/队列/缓存，不实现 R3 门面/Agent 接线，不重启 G6–G8。Agent 仍只依赖 core/sha2，LocalMemory 未开放可变 inner。
- 依赖评审：reqwest 0.13.5 禁默认特性、仅 rustls；tokio 1.53.2 的 macros/time（测试加 rt/net/io-util/sync）；serde_json 1.0.154；sha2 0.11.0 禁默认特性。直接版本精确固定、传递依赖锁定于 Cargo.lock；TLS 带入 AWS-LC 原生构建，本机 Rust 1.99.0/Windows 已可编译。没有 ONNX/ORT、模型下载或外部安装脚本；core 依旧零第三方依赖且无 I/O。
- 协议依据：2026-10-12 核验 [OpenAI Embeddings API](https://platform.openai.com/docs/api-reference/embeddings/create) 与 [Cohere v2 Rerank](https://docs.cohere.com/v2/reference/rerank)。分别校验 model/input/encoding_format 与 query/documents/top_n/max_tokens_per_doc、乱序结果 index 映射；不是 Chat 协议兼容假设。当前 Embedding 只验证原生维度，不请求降维；远端服务版本可能漂移。
- 安全闭环：所有实际文本来自本票查询/候选，查询默认私密；用途、完整端点、数据范围、全来源及查询证明分别检查。逐次 prepare/record 复验完整输入与人设快照、当前许可/到期和八次总额度；同代际/ID/版本的分叉不能错挂实际查询/发送正文。core 补纯 `fail_transmission`，失败/取消/未知结果保守记录可能发送输入并永久封票，不能无回执交付或失败后追加发送；外来失败回执不毒化其他票。
- 工程闭环：请求/响应/批次有硬上限，保守 UTF-8 字节拒绝超长且不静默截断；f64 稳定归一化，拒绝错维度/零范数/NaN/Inf。默认一次、最多三次，只重试 429/502/503/504；逐次复验、整体/单次超时、在途及退避取消、不跟随重定向/系统代理/客户端内部重试，敏感 Header 与错误/Debug 脱敏。Retry-After 仅支持有界 delta-seconds，其他形式不提前重试。
- 兼容：独立追加 PresetPurpose::Embedding=6 / Rerank=7，旧 Chat/ImageGeneration/PersonaInterview/Vision/Voice/MemoryExtraction=0..5 不动。Chat/凭据不隐式绑定新用途；用途切换、更新/删除的配置失效仍成立。
- 独立审查补修：审码判官指出两项 P1，补修后独立复核确认两项均关闭且无新增阻塞（复核限两项及其直接影响范围，未重新构建全链）。已补“七次成功＋第八次 429＋重试预留额度拒绝”的红/绿回归；现保留本次 pending，只有成功预留下次发送后才消费本次回执，无法继续即留证永久封票，不能回退交付。真实探针改为相关项必须严格优于至少一个干扰项，常量向量/同分 rerank 不再因稳定 ID 排序假通过。新增四项正式回归；另跑五个合成回环进程场景，常量向量（重排开/关）、同分重排均退出 1，区分性协议夹具退出 0；不将夹具进程通过计为真实效果。红回归与进程日志保存在同一验证目录。
- 已运行定向门：`cargo test -p moe-memory-search --all-targets --locked`（30 项协议/授权/探针回归、2 项数值单测）、`cargo test -p moe-memory-core --test hybrid_retrieval --locked`（39 项）、`cargo test -p moe-agent-runtime --test presets --locked`（11 项）、`cargo test -p moe-agent-runtime --lib preset --locked`（3 项），适配 crate 全目标 Clippy 亦通过。README doctest 通过；`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`、`cargo build --workspace --locked`、设置 `RUSTDOCFLAGS=-D warnings` 的 `cargo doc --workspace --no-deps --locked`、文档门与 `git diff --check` 全通过。独立 `cargo test --workspace --locked -- --list` 清单为 761 项（含 doctest，不重复计子进程），日志在本机忽略目录 `target/r1-validation-20261012`。
- 显式真实入口：`cargo run -p moe-memory-search --example online_probe --locked`，只使用硬编码合成语料，Embedding/Rerank 各需独立宿主用途/私密范围/来源许可、完整端点、凭据、模型和维度。已验证缺显式许可时退出码 1、无请求及上述回环进程边界；未获得受信真实调用环境，未使用/搜索真实凭据，未请求外部模型。真实 Embedding 非子串召回、真实 rerank 效果、未知 tokenizer 在远端的实际处理均未验证；回环人工向量仅证明协议与扫描，不证明语义质量。完整 40 查询质量门及性能对照仍留在 R4。
- 阶段接力检查：有明确后续 R2–R4。R1 代码/离线交付与真实门分开记录；缺真实环境不阻止按 MEMORY R.8 的例外继续 R2 离线索引施工，但不得把真实门当成通过，亦不得提前宣称最终 RAG 交付。R2 的可见 rerank 故障回退须尊重失败封票与实际发送证据，不能复用已失败票据。

## R2 交付记录：混合检索和索引生命周期（2026-10-12）

- 范围：仅既有 `moe-memory-search` 内实施词项/BM25、Unicode 字符区间分块、记录级精确余弦/RRF、可选 rerank、有界合并队列/单批租约、版本核对与显式可丢弃缓存。未施工 R3 持久化记忆门面/Agent 注入、R4 的完整质量门或 G6–G8；未迁入旧依赖、未提交/推送/建分支，保留既有未提交工作区。
- 窄契约评审：只读审查先确认失败旧票不能恢复；核心新增 `validate_candidate_ticket` 逐完整输入/查询来源/人设/发送证据复验，以及 `recover_failed_rerank` 一次性新 nonce/runtime 换票。原票永久失败、新票只收口禁再外传，所有原用途/端点/范围/来源/时间及未入选发送集完整迁移。取消/总期限/撤权/核心错误/额度拒绝禁止回退；硬失败 `RankingFailure` 仍保留票据和回执。只读 `MemoryId::as_u64` 用于可丢弃键，不提供授权或构造入口；核心仍零第三方依赖/无 I/O，旧持久化字节不变。
- 证明与生命周期：批次只来自当前密封候选，索引键含受信域、完整私密身份/共享命名空间、父版本、切片区间/配置、正文指纹与完整 EmbeddingProfile；运行期缓存还核对完整输入，文件装载与当前授权相交后重绑。授权先于统计、排序和统一字符预算；完整候选不被关键词 top-100 截断。同记录多片取最大余弦并去重，阈值不过可空结果，完整原记录由核心交付。重建与活动校准配置变化复用原取消栅栏；迟到/跨票/错模型不能写回或复活旧项。
- 故障与缓存：48 项模块单测、42 项回环集成测试及核心 45 项检索回归定向通过；覆盖缺项可见、跨域/跨宠同 ID、错模型/分块重建、到期等号/撤权/未通知更新、坏档/重复键/错维/坏范数/超限及快照替换失败保护。包含成功/失败/关闭 rerank、请求超时安全换票、取消/总期限/在途撤权不降级、七次成功＋第八次 429＋重试预留额度拒绝仍留八份回执封票，以及长记录仅外传有界切片但完整正文交付。缓存默认关闭，仅向宿主给出的既存受保护目录写固定单文件快照，不存原文/查询/会话/人设或凭据，不递归删除权威根；校验和不提供防伪，Windows/Unix 同步边界和物理擦除限制见 crate README。
- 独立审查与补修：未发现新增 P1；已补核心恢复资格红/绿回归，EntryEmbedding 先失败后迟到的 Rerank 回执不能洗成可恢复故障，只有明确留证的重排失败可一次性换票。审查另复现分块极值 P2：巨大重叠与单字节窗口相加溢出，debug 会 panic、release 可丢尾部；改为比较片内相对长度，极值完整覆盖回归在 debug/release 均通过。保留两项红测试日志，格式差异已通过 apply_patch 修正。
- 最终全链门：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo test --workspace --locked --offline`、`cargo build --workspace --locked --offline`、设置 `RUSTDOCFLAGS=-D warnings` 的 `cargo doc --workspace --no-deps --locked --offline` 全部通过；test 同样设置严格 Rustdoc，搜索 README 的 2 项 doctest 通过。`cargo test --workspace --locked --offline -- --list` 的独立清单为 826 项（含 doctest，不重复计算探针子进程输出）；文档门（11 份活动 Markdown）与差异检查通过。日志集中在本机忽略目录 `target/r2-validation-20261012`，不新增批次报告。
- 真实门仍未验证：缺受信真实端点/凭据/用途/范围/完整来源许可，未搜索或使用真实用户凭据、未请求外部模型。R1 五个回环探针场景与本批人工向量只证明协议和工程判据，不是真实 Embedding 非子串、真实 rerank 效果或 tokenizer 门通过；字符/UTF-8 限长不冒称 tokens。按 MEMORY R.8 缺环境例外继续离线范围，完整真实模型/中文标注集/性能对照留在 R4。
- 阶段接力检查：有明确后续 R3（门面与离线 Agent 接线）和 R4（回归/质量/最终复核）。R3 必须保留 LocalMemory 健康门与封装、Agent 不反依赖 search、完整上下文/发送依赖及默认关键词行为；不因缺真实模型环境重启 G6–G8。


## R3 交付记录：记忆门面与离线 Agent 最小接线（2026-10-13）

- 范围：核心三方法 `MemoryContextAccess`、LocalMemory 健康门覆盖的候选/收口/查询来源/传输/失败恢复转发、搜索授权窄桥与 `SearchMemory`、现有离线 Agent 最小泛型注入。没有新增适配 crate、权威正文副本、公共裸向量/任意 ID→正文写入、可变 LocalMemory service/inner 或新的协议格式；保留全部既有未提交工作，队员隔离成果经 apply_patch 集成，未移动领队 HEAD。
- 窄契约和兼容：MemoryService、LocalMemory、SearchMemory 分别保留 MemoryError / PersistenceError / SearchError；AgentError 泛型完整保留后端错误链。旧 AgentServices 构造及 SearchHost 继续可用，默认关键词语义不变；Agent 正常依赖不包含 search/local，memory-local 正常依赖仍只有 core。search 新增 local 正常依赖与 agent 测试依赖，锁文件仅追加仓内依赖边；reqwest 0.13.5（禁默认特性、rustls）、tokio 1.53.2、serde_json 1.0.154、sha2 0.11.0 均未变，核心零第三方依赖且无 I/O。
- 门面：宿主显式准备密封 RankedCandidates，再以完整身份/命名空间、真实绑定会话、用途和完整 RetrievalQuery 匹配、单次消费；首先检查本地健康，之后复验原票并由核心取权威正文收口。失配/旧票/失败/重复消费/预算不足不回退关键词或无记忆模型，context 装配与交付仍转发健康门。`into_memory` 仅消费封装归还原宿主所有权，不提供可变借用或复制内核。SearchDiagnostic 只镜像获准集合模式/缺项/命中/预算/排序耗时，非端到端或质量报告。
- 完整证据：候选、查询证明、人设和全部实际发送证据贯穿门面/任务/受保护上下文。Agent 在模型使用前检查人设、完整读取项及全部 `sent_sources`，查询来源或未入选私密发送来源缺会话许可均整体拒绝；最终 top-k 或字符裁剪不削减依赖。提议缺任一发送来源即 InvalidEvidence，持久化派生保留未入选发送条目的精确版本依赖。测试使用人工向量和合成发送回执，没有真实外传，也不把回执模拟称为真实传输证据。
- 集成补修：统一根门面/reader 的健康检查，避免重复实现出现检查次序差异；将原“关键词 Agent 与另一个门面上下文对照”改为同一持久化混合门面直接 prepare→infer→finish。另补红/绿回归：读健康故障发生在允许重排回退的复验点时，不得进入失败 rerank 的一次性换票恢复分支；修复前恢复调用计数为 1，修复后为 0，并原样返回 RecoveryRequired。原失败票永久封闭与完整输入复验均不放宽。
- 独立只读复核：审码判官在 R3 范围未发现阻塞 P0/P1，复用既有二进制复跑 73 项纯内存定向测试通过（不是重新构建全链）。两项非阻塞 P2 已补齐：SearchError 的 Eq / I/O ErrorKind 比较说明同步到实现；拒绝矩阵增加同实例 ID 不同 workspace/user 命名空间、查询文本及 private/shared 搜索范围变化，仍确认随后原完整请求可唯一交付、不因失配消耗或降级。最终全链另由领队按当前源码重跑。
- 已实际运行的定向门：core 的 `context` 24 项及 `hybrid_retrieval` 45 项；local 库测试 32 项及 `authorized_reads` 8 项；Agent `agents` 46 项；search 全目标 56 项模块、5 项门面/Agent/写回集成、42 项协议/授权/探针回归，搜索 README 3 项 doctest。人工非子串向量仅验证工程接线，非真实 Embedding 效果。
- 最终全链门：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo test --workspace --locked --offline`、`cargo build --workspace --locked --offline`、`RUSTDOCFLAGS=-D warnings` 的 `cargo doc --workspace --no-deps --locked --offline` 均通过，workspace test 同设严格 Rustdoc；独立 `-- --list` 清单为 848 项（含 doctest，不重复子进程输出）。文档门（11 份活动 Markdown）和 `git diff --check` 通过，日志在本机忽略目录 `target/r3-validation-20261013`，未新增批次报告。
- 未验证范围：没有受信真实端点、凭据及用途/范围/完整来源许可，未搜索或使用真实用户凭据、未请求外部模型。真实 Embedding 非子串效果、rerank 收益、真实 tokenizer/远端截断与 R.9 的完整中文标注集/性能对照仍未验证；R1 五个回环探针仅证明协议判据。缺环境按 R.8 显式例外继续离线施工，不默认外传或升级真实门为通过。
- 该R3快照当时的阶段检查：下一批为R4（现已实施的离线工程见下一节），按 MEMORY R.9 进行完整安全回归、冻结 40 查询/独立校准、KeywordOnly/Hybrid/可选 rerank 对照、显式真实验证入口及必要性能门和最终复核；真实环境仍缺时必须非零拒绝并分别记录未验证，不能以人工向量/协议桩替代。G6–G8 继续暂停，不扩对话模型、HTTP 网关、渠道或工具范围。

### R4 离线工程与最终复核（2026-10-13）

- 范围：只实施 MEMORY R.8/R.9 的 R4 离线工程、入口和复核；既有未提交成果不 reset、不覆盖，领队不 commit/push/建分支。自动集成工作副本的快照提交不当作领队发布提交；原主工作区 HEAD 仍为 `81ee06c`，未变。原 R4 新增仅示例/支持文件、合成 JSON 与测试；本次验收回退另最小修复三处生产实现并补回归，维护本节及既有入口，不新增批次报告。
- 工具链/依赖保持 Rust 1.99.0、Cargo workspace；reqwest 0.13.5（禁默认特性、rustls）、tokio 1.53.2、serde_json 1.0.154、sha2 0.11.0 精确锁定不变。core 零第三方依赖/无 I/O，local 正常依赖仅 core，Agent 正常依赖仅 core/sha2；没有新运行 crate、裸向量写入 API、通用数据库、双份权威正文或缓存事务日志。
- 安全定向：core hybrid 49（原45+2恢复+2收口）、search 模块64（原56+4迟到提交+3检索层+1直接队列快照）、在线协议44（原42+2档案身份）、Agent48（原46+2），另4项持久化 SearchMemory→Agent场景、5项冻结夹具/指标/关键词、3项真实入口配置和3项质量回环流水线。独立审查未见阻塞 P0/P1；同代际完整快照恢复、迟到提交矩阵及 infer/finish 健康故障三项 P2 已补，不以推进代际的普通更新替代分叉正文测试。两项质量 P2 已补：冷热各计摘要并分别过门、关键词 `(17,1,7)` 固定回归。

**用户验收回退的三项 P2**：本轮在集成快照 `80fe87e` 上最小修复，没有新增已复现 P0/P1，也不将修复等同于完整 R4 质量验收。

- 检索层误清：旧 `reconcile` 仅比较搜索范围，把 WorkingContext 或 FormalMaterial 未包含的长期记录误当失效。清理和提交后队列收束均改为同时匹配身份/搜索范围/检索层；普通与正式资料互不清理，统计和排名仍仅用本票完整获准集合。新增3项覆盖已编码/待编码混合、往返切层、正式资料独立排名，以及切层后更新/删除/撤权仍清理旧向量；修前60通过/3失败（可复现1→0），修后63全部通过。
- 收口成本：保留 `validate_candidate_ticket_state` 的候选/QueryProof/人设/完整发送快照复验；引用一次建表，完整排名检查有限分数、归属绑定和去重，再只克隆预算内实际交付正文。新增两项：10k完整逆序排名交付5条，以及限额/预算之后的重复、外票、无绑定引用、NaN/±Inf仍拒绝；49项core hybrid全部通过。未删减候选或把完整快照降为ID/版本比较。
- 档案身份：请求超时、尝试次数、退避从Embedding/rerank的共同空间指纹中移除；协议、端点、模型、维度和有界输入/归一化规则保留。凭据仍不入指纹，传输参数仍有界校验并受原授权/失败门约束。单加1秒超时的回归修前失败、修后通过；模型/端点/维度/输入规则变更仍隔离，44项在线协议全部通过。旧指纹缓存需一次重新生成，之后调传输参数不要求重建。

**独立复核与补强**：审码判官重新通过core hybrid49、search模块63、两项在线配置，共114项，支持关闭三项P2，未发现需要重开本轮修复的P2或阻塞P0/P1；未独立重跑全链或基准。其非阻塞P3指出后续fill可能自动补排、掩盖队列被误清。领队在index.rs的仅测试私有子模块新增直接回归：WorkingContext切层前后比较完整队列键和输入快照；普通/正式资料互切及双向commit只移除本次完成记录，其余快照逐项完全一致，全部断言在任何后续补排之前完成。该用例无I/O、不导出生产观察接口，不依赖fill掩盖损失；领队重新通过search模块64项。此最后新增用例及最终全链归属领队验证，不冒称独立审查已运行。

| R.9 安全项 | 本轮最终回归覆盖（含既有测试，未删除或降低断言） |
| --- | --- |
| 1 隔离与共享只读 | core hybrid 的跨服务/宠物/同版本引用、分叉与来源；search 的完整命名空间/真实会话/用途/查询矩阵及跨宠缓存。 |
| 2 授权先于统计/排序 | `complete_authorized_candidates_not_keyword_top100_feed_both_routes`、core 完整候选151条及未获准最高向量/互斥来源探针。 |
| 3 正式资料与身份 | publication 正式资料独立层及非工具授权；门面/Agent 必要人设预算不足明确拒绝。 |
| 4 全生命周期失效 | core/context/local 的更新/源撤权/依赖/取消/配置/到期等号、完整候选/查询/实际发送/人设快照复验。 |
| 5 迟到索引提交 | 新增删除、显式取消、换身份、重开4项 commit矩阵，旧批不增加/覆盖向量；保留原更新/撤权/模型/分块重建矩阵。 |
| 6 输入与确定性边界 | 向量维度/NaN/Inf/零范数、重复/外来引用、超大/重复键/坏缓存、空查询/阈值空集/并列/预算。 |
| 7 权威日志与降级 | 损坏/半缓存/写入失败/清缓存不改权威日志；Persistence/RecoveryRequired 不是 KeywordOnly 或 rerank恢复理由。 |
| 8 留存与外传 | 工作上下文不能持久化向量；在线42项回环协议用例覆盖端点/用途/范围/完整来源拒绝、请求体/脱敏/超时/有限重试/取消/重定向，默认不外连。 |
| 9 健康门与完整证据 | local入口健康门、SearchMemory一次性密封票及无可变service；Agent prepare/infer/finish复验、压缩/裁剪不洗掉私密证据。 |
| 10 重排与发送污点 | rerank只收获准引用，失败一次性换nonce/runtime且只收口、旧失败票永不复活；未入选发送项/私密查询来源保持写回依赖，Required故障不返回降级质量摘要。 |

**冻结与校准**：`crates/moe-memory-search/tests/fixtures/r4-chinese-v1.json`，SHA-256 `2d50243e19510c41514c1299e682ebbd8c490c7cefed18f06423b5c3f32dc6a8`，LF固定。40条评测（16私密、16共享含4正式、8无相关）、32记录、16释义，独立12校准/8记录；固定手工相关性等级3，未列出的记录不相关，尚未做外部人工盲审。真实校准在固定0..1/0.02网格最大化校准正确数，并列选更严格值，禁止评测调参。测试中校准用的0.92或正交向量门槛1.0仅为夹具，不是实际模型门槛。

| 同语料/权限/预算对照 | Hit@5 | 释义 | 无相关空集 | NDCG@5（有答案32条） | 越权/失效 |
| --- | --- | --- | --- | --- | --- |
| 实测离线新词项 KeywordOnly | 17/32 | 1/16 | 7/8 | 0.531250 | 0/0 |
| 标签映射人工正交向量 Hybrid，仅回环工程 | 32/32 | 16/16 | 7/8 | 1.000000 | 0/0 |
| 标签映射人工重排 Hybrid+Rerank，仅回环工程 | 32/32 | 16/16 | 7/8 | 1.000000 | 0/0 |
| 真实 Hybrid / 真实 rerank | 未验证 | 未验证 | 未验证 | 未验证 | 真实环境未运行 |

后两个人工列**不能证明语义质量、真实收益或真实门达标**；只验证公开授权编码/索引/重排/收口/装配流水线和指标，冷热均重复相同判据，噪声各1条。例：固定 q09 的 KeywordOnly 为空，人工映射后可交付 p09；这是夹具接线，不是供应商释义能力。额外用常量向量确认质量门失败、503 Required rerank确认立即错误且不产出降级摘要。离线 Agent 四场景均真实使用临时 journal/门面，但生成回复仍是离线夹具。

**显式真实入口**：`cargo run -p moe-memory-search --example embedding_check --locked --offline -- --backend online --rerank off`（开重排改为 `on`），配置清单见搜索 README。只有明确受信端点、凭据引用、模型/维度/修订及合成语料完整用途/范围/来源许可才发HTTP；`--offline`只约束Cargo，不代表HTTP禁用。无参数、online缺许可、local未实现、rerank缺独立许可、维度0、非回环明文HTTP端点6类均已实际非零拒绝（exit1），未 skip 后打印通过。预检另验证rerank不可继承Embedding许可及每个必需字段缺失拒绝。调用期间由该入口宿主动态采样时间，不把LocalSearchHost固定离线注入时间称为真实时钟。未搜索真实用户凭据、未请求外部模型。

**性能**：实际 `cargo run --release -p moe-memory-search --example scan_bench --locked --offline`，先以参数 `100` 跑debug定向；仅本进程回环人工向量，32维、一记录一切片，完整授权租约填充，队列4096、每票最多8次发送、每票/查询阶段120秒控制预算。以下为三项P2修复后领队重跑的一次release观测，不是固定性能承诺；完整初始填充仍保留逐批复验成本：

| 规模 | 建库 ms | 完整首次索引 ms（编码 / 簿记） | 冷查询端到端 ms | 热7次 p50 / p95 ms | 召回 / 收口 / 装配 p50 ms | 向量逻辑负载 |
| --- | --- | --- | --- | --- | --- | --- |
| 1,000 | 2.924 | 546.239（186.483 / 359.756） | 16.639 | 16.700 / 18.729 | 8.582 / 1.368 / 1.839 | 256,000 B |
| 10,000 | 8.944 | 75,197.587（21,151.639 / 54,045.947） | 237.505 | 235.404 / 261.127 | 122.419 / 17.078 / 24.062 | 2,560,000 B |

两规模分别4/40张票、32/313批，最终索引记录数恰为1000/10000、missing_chunks=0；回环请求数40/321（含冷+7热），无重试/真实用量或费用。内存负载按实际f64的8字节核算，纠正了队员最初按f32的低估；不是RSS/完整索引堆占用，缓存关闭、磁盘大小未测。召回时间包含词项/向量/RRF及授权快照检查，不冒称纯浮点扫描时间。修前历史release的10k热端到端p50为1,199.993ms、收口p50为1,027.297ms；本次相同示例分别为235.404ms与17.078ms，只作为同机两次观测，不承诺固定收益。本次10k首次填充反而为75,197.587ms（旧观测55,867.922ms），未隐藏或声称簿记开销已解决。逐批完整快照复验成本继续保留，不为过门缩小候选，不据此自动引入ANN或重构核心。真实tokenizer/Embedding冷加载/在线请求、真实重排附加耗时与用量均未验证。

另实际运行同一示例的默认debug 1k/10k完整索引（均missing_chunks=0），不只跑小规模：1k首次填充1,710.103ms（编码542.012/簿记1,168.092），冷端到端48.949ms，热p50/p95为44.858/52.081ms，热收口p50为3.597ms；10k首次填充183,897.697ms（编码51,969.905/簿记131,927.792），冷端到端542.648ms（收口43.622ms），热p50/p95为549.136/660.412ms，热收口p50为45.883ms。用户修前debug的约3.14s/收口2.85s是独立复现，不混入本轮测量或与release作精确倍数承诺。以上均为回环人工向量吞吐，不证明真实模型效果。

**最终门**：三项P2及P3队列断言补齐后领队在集成工作区重新实际执行并通过 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo test --workspace --locked --offline`、`cargo build --workspace --locked --offline`、`cargo doc --workspace --no-deps --locked --offline`；test/doc同设 `RUSTDOCFLAGS=-D warnings`。另执行独立 `cargo test --workspace --locked --offline -- --list` 计数879项（原871+本次8项，含doctest及example的3项配置测试，不重复子进程输出），Rust 1.99.0/Cargo 1.99.0版本已采集。11份活动Markdown的UTF-8/路径/锚点/格式门及 `git diff --check` 通过；正常依赖边界、精确锁版本、reqwest禁默认特性/rustls及冻结JSON的LF/SHA-256另行核验通过。显式入口重新构建后运行6类非零拒绝。本次修前复现、修后定向、debug/release基准及全链日志在本机忽略目录 `target/r4-p2-validation-20261013`（原R4日志 `target/r4-validation-20261013` 保留）；未沿用队员或旧快照全链结果，未新增批次报告。

**未完成与阶段检查**：整体 R4 仍暂不能验收，三项P2修复只关闭对应离线工程缺陷。缺受信真实端点/凭据与完整许可，真实Embedding非子串/40查询目标、真实rerank效果、本地模型资产/tokenizer/远端无截断仍未验证；R1五回环探针与本轮人工向量/合成回执均不能替代。按R.8缺环境例外交付离线工程，不宣布完整RAG验收。R4是当前R系列最后一批，没有已授权的新R批；再次核对PLAN，G6–G8仍暂停，真实对话模型/对外网关/渠道/工具/UI/生产Hooks均未启动，必须用户明确启动及逐项许可，不能自动接力重启暂停范围。

## 修复前 RAG 独立复核（历史，2026-10-13）

> 本节保留修复前的原始证据，不是当前缺陷或新施工指令；三项 P2 已修复，冻结语料、离线评测与完整合成索引基准亦已补齐。当前状态以 R4 交付记录及下节用户侧独立复审为准，真实模型质量门仍未完成。

结论：当前 R0–R3 离线接口、授权票据、在线协议适配、持久化健康门和 Agent 最小注入已可验证；本轮未发现可复现的新增 P0/P1 权限缺陷，但发现三项 P2。整体 RAG 暂不能标为完整验收：R4 的冻结 40 查询/独立校准、真实 Embedding/rerank 对照及实际索引性能门仍未齐备。以下是审核结果，不是修复交付；未改功能源码、未搜索/使用真实凭据、未请求外部模型，G6–G8 保持暂停。

| 等级 | 证据与影响 | 建议修复与回归 |
| --- | --- | --- |
| P2：检索层切换误清索引 | `crates/moe-memory-search/src/index.rs:324` 和同函数队列 retain 只按分区判断清理范围，却把当前查询层的 expected 当成该分区全部有效记录。独立公开 API 探针先装入一条有效长期向量，再对同宠执行 WorkingContext 查询；长期索引由 1 变 0，下一次长期查询缺片由 0 变 1。正式资料/普通共享资料也存在同类互删风险。不是权威记忆丢失，但会触发重复编码、费用和召回降级。 | 不以“本查询未包含”推断记录失效；清理必须区分查询层与当前权威授权/生命周期。补 LongTerm→WorkingContext→LongTerm、普通共享↔FormalMaterial 的向量保留回归，同时保留真正撤权/删除/到期的不可读门。 |
| P2：最终 top-k 收口二次遍历 | `crates/moe-memory-core/src/read/hybrid.rs:984` 在预算/条数截取之前，对整份 ranking 反复线性查 ticket/current 并克隆正文；ticket 的 candidate 查找也线性。现有 scan_bench 在本机 dev 构建、10,000 条/limit=5 时端到端约 3,138 ms，其中核心收口约 2,846–2,854 ms；1,000 条收口约 32 ms。不是生产延迟承诺，但源码与规模对照确认二次复杂度风险，且不含网络。 | 按引用构建一次查找表，仍验证整份排序的外来/重复/数值与权限，再只为预算内实际交付项取/克隆正文。补低 limit、大候选集及预算跳过场景；性能对照不得削弱完整发送依赖或授权门。 |
| P2：传输参数污染向量空间身份 | `crates/moe-memory-search/src/online.rs:192` 的配置哈希把 request_timeout、max_attempts、retry_delay 纳入 EmbeddingProfile；探针只把同端点/模型/维度的超时增加 1 秒，profile 就不同。调整网络容错会导致合法向量不兼容、要求重建和重复付费。 | 语义编码身份保留模型、端点/版本、维度和影响编码的处理规则；网络超时/重试作为独立执行参数。补只改网络参数不失配，以及真换模型/维度/编码规则必须失配的成对回归。 |

- 本轮实际验证：search 全目标（56 模块 + 5 门面 + 42 协议 + 4 新增 R4 Agent 回归）及 core hybrid 45 项定向通过；fmt、全目标 Clippy `-D warnings`、workspace test/build、严格 Rustdoc、文档检查和 diff 检查均通过。独立 `-- --list` 清单为 852 项（含 doctest），不累加子进程打印的重复测试结果；文档检查覆盖 11 份活动 Markdown。
- 本地证据在 `target/rag-audit-20261013/`：`workspace-tests.log`、`test-list.log`、`scan-bench.log`、`independent-probe.log`。探针源码位于该目录的 probe 子目录，全部为忽略区审核产物，不新增正式 crate 或测试后门；人工缓存向量只验证生命周期，不证明语义质量。
- R4 缺口需分别说明：`online_probe` 仍是一条查询/两条记录的最小 smoke；`scan_bench` 明确使用 KeywordOnly、实际 SearchIndex 向量为 0，另一段手工向量循环只测扫描成本。两者诚实标注了边界，没有拿合成结果冒充真实模型，但不能代替 MEMORY R.9 的完整质量/校准、重排对照和实际索引的热查询测量。缺凭据解释不了冻结语料与离线评测框架尚缺；这些可以先完成，真实门继续标记未验证。
- 封装判断：核心/运行层未新增网络或模型依赖；memory-local 健康门保留；Agent 经三方法核心契约注入搜索门面，无生产反向依赖。封闭/SRP/解耦方向基本达标；上述全量清理、重复扫描和运输参数指纹属于应收口的防御/职责边界问题。未要求另建通用数据库、调度或审批框架。
- 本轮仅审核，不自动施工或跨阶段接力。建议先修三项 P2，再补 R4 可离线完成的评测框架；没有受信真实环境时不把模型效果门升级为通过。
## 用户侧独立复审（2026-10-13）

- 审核对象是集成工作树 `task-t_mv5k8js6_c24076e8f3d0-integrated` 的 `6463762d693c6efb402568e91a1cbd3d5d7fe8ca`；开始时工作树干净。复审当时原主目录为 `81ee06c` 加未提交旧实现，线上配置指纹仍包含传输参数，不能把本次通过套用到当时未集成的主目录。本次复审未集成分支、未修改功能源码，仅补本节及忽略目录内的独立探针/日志；后续主分支集成另记。
- 三项 P2 支持关闭，本次范围内未发现新增阻塞 P0/P1 或需重开的 P2。逐项核对身份/范围/检索层清理与提交、完整候选/查询/人设/实际发送快照复验、全排名尾部合法性检查及空间身份隔离；直接队列快照回归在后续补排之前比较完整键和输入，未用自动补排掩盖清理损失。
- 重新实际通过 core hybrid 49 项、search 全目标（模块 64、门面 5、在线协议 44、R4 质量 5、Agent 场景 4、回环流水线 3、入口配置 3），以及 fmt、全目标 Clippy `-D warnings`、workspace test/build、严格 Rustdoc、11 份活动 Markdown 与 diff 检查。test/doc 均设 `RUSTDOCFLAGS=-D warnings`；Cargo 构建/测试使用 `--locked --offline`。另用独立 `-- --list` 清单计数 879 项，不累加子进程测试输出或下述临时探针。
- 独立公开接口探针用合成 SHA-256 封套缓存加载一条向量，交替 WorkingContext/FormalMaterial/All 后原长期层仍为 indexed=1、missing=0；真实删除再查长期层为 indexed=0、missing=0。合并调整超时/尝试次数/退避并轮换合成凭据时，Embedding 和 rerank 档案均保持相等；没有发出网络请求。
- 同一独立探针另建 10k 条合成记录、提交完整逆序排名、限额 5/预算 60，先验证限额后的重复引用仍拒绝，再验证仅交付正确的 5 条正文。每轮新票据，7 次收口 p50 为 debug 38.822 ms、release 13.790 ms；仅计公开核心收口，不含候选准备/索引填充/召回/装配，不能直接替换上文完整扫描基准。本轮未重跑完整 1k/10k 首次索引基准，亦未调用真实模型。
- 重新构建真实入口，实际运行无参数、online 缺许可、local 未实现、rerank 缺独立许可、维度 0、非回环明文 HTTP 六类拒绝，均 exit 1；仅使用显式合成配置，未搜索或读取真实凭据。独立日志与探针在本机忽略目录 `target/rag-rereview-20261013`，未新增正式文档或生产接口。
- 完整 R4 仍不验收：冻结语料和离线工程已存在，但真实 Embedding/非子串质量、真实 rerank 效果、tokenizer/远端无截断及真实调用性能/用量仍未验证。核对现行 PLAN 后没有获准的新 R 阶段，G6–G8 继续暂停，不自动接力。

## 主分支集成（2026-10-13）

- 用户明确授权合并到主分支并提交推送；主目录 `main` 已从 `81ee06c` 快进到已复审集成快照 `6463762`，保留完整原提交历史。集成后的 crates、Cargo 清单/锁文件、工具链和脚本与该审核快照完全一致，未改功能实现或放宽权限。
- 主目录原未提交状态已保存在本地 stash `e7f82957dbac728216d7a83cee064ed028d1f2f9`；与 R4 输入快照比较后确认独有差异仅为三份文档，已保留可见运行日志设计、计划入口及修复前审核原文，并合入用户侧独立复审。修复前审核显式标为历史，不再作为未完成施工指令；没有启动运行日志实现或 G6–G8。
- 在主目录重新实际通过 core hybrid 49 项、search 全目标、fmt、全目标 Clippy `-D warnings`、workspace test/build、严格 Rustdoc、11 份活动 Markdown、暂存及工作区 diff 检查；test/doc 均设 `RUSTDOCFLAGS=-D warnings`，Cargo 构建/测试使用 `--locked --offline`。独立 `-- --list` 清单仍为 879 项，没有沿用集成工作树的编译通过结论。
- 合并备份、补丁及本轮主目录验证日志在本机忽略目录 `target/main-integration-20261013`；构建产物、工作树、环境/密钥文件和向量缓存均未纳入版本控制。完整 R4 真实模型质量门仍未完成，提交推送不等于产品或真实模型验收。

## 历史验收

下表压缩原施工记录，只记录各快照当时的独立测试清单，不把前后快照相加、不重复计算子进程输出，也不代表今天重跑了历史版本。当前范围仍见现行计划。

| 快照 | 当时正式用例 | 关键交付/证据边界 |
| --- | --- | --- |
| M0 / M1 / M2 | 41 / 84 / 128 | 纯契约、日志适配、授权 reader。 |
| M3 / M4 / M5 | 181 / 237 / 280 | 生命周期、受控共享与离线对话。 |
| 敏感度修复 / 上下文精进 | 283 / 314 | 工作上下文收紧、保护装配与受控压缩。 |
| G0–G2 | G1：406；G2：454 | 预设/入口、暂存、统一离线 Agent；只列此处核对过的历史清单。 |
| G3–G4 | G4：588 | 有界回收生命周期、可恢复文件；G3 清理顺序水位补充修复另有独立探针。 |
| G5 | 674 | 固定 CLI 适配，运行 276、记忆 314、文件 17、CLI 67；无生产 Hook/模型联调。 |
| 两项留存修复后 | 679 | 新增 5 回归，运行 279、记忆 314、文件 19、CLI 67；修复及当前架构意见见本文前五节。 |

G5 旧源码清单仍为 `docs/G5-SOURCE.sha256`，86 文件、9032 字节，LF 清单 SHA-256 为 `9749e422f70ff0213d3a9d6244e0bfd35502fa378a94c706ae92478eaea055c9`。当时生成脚本、独立字节实现及 .NET Ordinal/SHA256 三路复核一致；归一化仅替换 CRLF→LF，保留 BOM/孤立 CR，非法 UTF-8 拒绝。这是历史证据，不是当前代码身份；清单格式及重新生成方法仍在 `scripts/g5-source-manifest.py`。

独立审查与探针不等于新正式用例；定向复核不冒称重复全链。Windows 文件同步/原子替换与 Unix 目录同步、真实断电、恶意完整旧档回滚、物理擦除和外部备份保证始终区分。原施工简报及重复阶段“下一步”已移除，不继续作为自动开工指令。

## 历史缺陷复核

修复前的整体复核曾通过正式 674 项，但独立探针揭示两个留存 P1，说明“测试全绿”不等于覆盖了所有语义。以下保留当时证据；前文已记录正式修复，不能用这些旧二进制证明新代码。

- 上轮“清理后丢失接入/来源顺序水位”的 P2 已修复；清理前对照、清理后接入顺序、清理后来源顺序共 4 项独立探针全部通过。
- 全新构建目录执行 `cargo test --workspace --locked --quiet`，正式独立清单 674 项全部通过；清单通过 `-- --list` 单独计数，不重复计算跨进程子测试输出。
- `cargo fmt --all -- --check`、全目标 Clippy `-D warnings`、workspace build、`RUSTDOCFLAGS=-D warnings` 的 workspace doc、活动 Markdown 校验及 `git diff --check` 均通过。正式测试本轮未额外设置严格 rustdoc 标志，不能把文档构建的严格标志冒称为该测试命令的标志。
- 源码清单生成和逐字节比对通过，标识与审核前记录相同；并未沿用历史 target 的编译结果作为本轮测试。
- 新增独立探针合计 8 项：顺序修复 4 项通过；留存收紧跨重启 2 项失败；内部正文清理 2 项失败。它们不计入仓库正式 674 项测试。
- 临时复现程序位于本机忽略的构建目录：`target/overall-review-20261012/review-ordering.exe`、`target/overall-review-20261012/review-retention.exe`、`target/overall-review-20261012/review-ledger-cleanup.exe`。这些二进制绑定审核时源码，修复后必须重新编译或将断言正式化，不能用旧二进制验证新代码。
