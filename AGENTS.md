# Moe Buddy 开发指南

## 项目概要与阅读入口

Moe Buddy 是以独立陪伴 Agent 为目标的 Rust Cargo workspace，围绕稳定身份、人设、私密/共享记忆与可信交流入口建设。已有离线底座不等于真实模型、渠道及桌面表现的完整产品闭环。

- 动工前先读 [现行计划](docs/AGENT-MEMORY-PLAN.md) 的设计原则、架构与当前阶段；仅推进用户明确授权的范围，不自动启动暂停阶段或同时重构 UI、动画、工具执行、多宠窗口。
- 记忆、生命周期、上下文及 RAG 契约见 [记忆系统](docs/MEMORY.md)；预设、Agent、流程、暂存/回收与 Hooks 见 [网关契约](docs/GATEWAY.md)。
- 当前进度、暂停项与验收证据由现行计划和 [审核记录](docs/REVIEW.md) 维护；本文件只维护稳定概要与开发规则，不复制阶段流水账或测试总数。公开接口与示例查对应 crate README 和 Rustdoc。
- 旧项目归档仅供溯源，禁止据历史路径假定当前仓库已有对应实现，不沿用其 TypeScript/Node 工具链或桌面渲染约束。

## 模块职责

| 路径 | 职责 |
| --- | --- |
| `crates/moe-memory-core` | 纯记忆契约、授权、治理、检索收口与上下文规则。 |
| `crates/moe-memory-local` | 记忆持久化门面、日志与恢复适配。 |
| `crates/moe-memory-search` | 记忆系统内的索引、混合召回及 Embedding/rerank 适配。 |
| `crates/moe-agent-runtime` | API 预设、可信入口、会话暂存/回收、离线 Agent、程序流程执行、动画编排与纯内存诊断日志。 |
| `crates/moe-raw-local` | 会话原文文件、快照与恢复适配。 |
| `crates/moe-cli-hooks` | 固定 CLI Hooks 协议的输入输出适配。 |
| `crates/moe-ui-core` | 纯 UI 管理、类型化控制器/视图组合、逻辑面板生命周期与有界驱动；不选择 GUI 或接管业务权限。 |

## 诊断日志接入

类型从 `moe_agent_runtime` 根导入；完整签名与示例见 [运行层 README](crates/moe-agent-runtime/README.md#可见运行日志首批纯内存服务)，权限和范围见 [日志契约](docs/GATEWAY.md#可见运行日志)。

- 初始化：`DiagnosticsConfig::new(process_generation, budgets)` → `add_domain` → `DiagnosticsService::enable`；默认关闭，宿主显式提供非零且跨重启不复用的代际、稳定域、采集策略、查看许可与硬预算。
- 采集：宿主 `create_run/create_emitter` 后调用 `emit`，只传封闭 `EventInput` 与注入的 `ClockReading`；模型、Hook 输入及查看器不能签发句柄或写入自由正文。
- 查看：认证主体后 `authorize`；使用 `query_into/list_runs_into/inspect_run_alias_into/health_into` 和有界输出切片，每次读取复验许可。过滤类别不能超出授权；续页保留原过滤、方向和运行别名，提前结束调用 `close_cursor`。
- 真实性：业务与传输状态分开，Completed 不等于 Written；未知不自动重试，诊断失败不改变业务结果。Gap 要显式展示，读取拒绝须清理失效缓存，不打印原始错误或污染 Hooks stdout。
- 容量与范围：事件可循环淘汰，运行登记/别名是代际累计额度，到限明确拒绝；重配置清空旧诊断并使旧句柄失效。业务接线、查看器租约/GUI、文件及导出未默认启用，后续接入须单独明确范围。

## 架构与权限边界

- 依赖单向：记忆核心的纯契约/校验/策略 ← 运行与持久化/检索适配 ← 宿主应用。核心不得反向依赖宿主、GUI、模型 SDK 或存储实现；Agent 经核心窄契约使用记忆后端，不直接依赖具体持久化或搜索实现。
- 核心与运行层不直接访问文件、网络、系统时钟或全局活动角色；I/O 留在适配与宿主边界。时间、ID、运行身份与授权上下文按需注入，模型输入不能直接构造受信身份。
- 私密记忆按稳定桌宠实例身份隔离；共享知识库可共同查阅，不代表所有桌宠可共同写入。长短期是生命周期；实体卡片、向量和图谱是可重建派生视图，不另增一级记忆类别或成为权威记录。
- 模型只提议；归属、来源、权限、版本、取消和实际提交由受信服务裁决。受保护人设、工具权限和正式协议不能通过普通记忆写入修改；工具执行状态由运行时账本和回执证明。
- 流程层只标准化节点顺序、依赖与受控推进，不复制业务校验或接管 Agent 决策。流程终态不是业务回执，取消不等于回滚，未知结果不自动重试；累计实例容量不是并发上限，方法报错后的未结交互须由宿主显式处置。
- 禁止默认扩大云端传输、自动共享、用户查看、落盘、导出或删除范围；配置 API Key 不等于获准外传。未定政策不阻止离线夹具，但不得默认放行。

## 编程规范

- 保持单一职责和窄接口，只暴露必要门面、命令、受控句柄与只读快照；不开放内部可变集合、任意改状态或跳过校验的后门。
- 优先复用既有业务接口和规则；保持改动聚焦，修根因，不顺手修无关问题。避免为假设需求预埋空 crate、通用引擎或重复抽象。
- 保留权限、来源、版本、取消及提交边界的必要复验，不以类型安全替代它们；不堆无依据的兜底，不吞错、不伪造成功，保留后端错误类型与错误链。
- 核心保持零第三方依赖；新依赖按实际用途及层级评审，不能因适配层使用 SDK、异步运行时或序列化库而反向引入核心。
- 新运行模块与缺陷修复配套针对性测试。合成夹具留在测试边界，临时审查探针放忽略的 `target/`；不得为测试开放生产权限或把示例结果当成实际业务能力。
- 不复制真实聊天、凭据或用户存档作为默认夹具；不运行外部文章的安装脚本，不迁入旧项目依赖、构建产物或功能代码。

## 验证与交付

工具链以 `rust-toolchain.toml` 为准；需要时仅在当前进程加入用户 `.cargo/bin`。功能先跑针对性回归，再跑全链门；依赖已缓存且不需联网时可给 Cargo 构建、测试和文档命令加 `--offline`。

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
$env:RUSTDOCFLAGS = '-D warnings'
cargo test --workspace --locked
cargo build --workspace --locked
cargo doc --workspace --no-deps --locked
powershell -NoProfile -File scripts/check-docs.ps1
git diff --check
```

- 文档改动检查编码、路径、锚点与格式；仅改文档无需重跑无关 Rust 全链测试。不把未运行写成通过，也不把离线、回环或合成进程验证称为真实模型/渠道验收。
- 公开接口变更同步 crate README/Rustdoc；跨系统契约与验收更新既有四份专题文档，不为每次讨论、施工或审查新增一次性文档。若增加其他工具的规范入口，只指向本文件，不另维护一套重复规则。
- 不自行 commit、push 或创建分支；须用户明确授权。
