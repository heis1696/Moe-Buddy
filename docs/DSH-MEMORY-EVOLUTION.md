# DeepSeek Harness：记忆、上下文与可演化能力的本地源码拆解

> 研究日期：2026-10-10。状态：只读源码研究及讨论建议，不是新阶段施工授权。
> 本地参考：`D:/Program files/deepseek-harness`，远端 `deepseek-ai/deepseek-harness`，固定 HEAD `47f943859bef60e4160492346772ded9b24f765a`，提交日期 2026-08-13。本文不代表 GitHub 最新版本。
> 路径约定：`DSH/` 指上述参考根目录；`crates/`、`docs/` 指 Moe-Buddy。引用以该本地源码为准，不把社区记忆插件或旧 README 的描述算作已实现能力。
> 执行边界：未安装依赖、未启动 DSH、未调用模型或 MCP 服务、未运行参考项目测试、未修改参考项目。其原有未跟踪文件 `dsh-web.bat` 和 `examples/acp-agent/agentdeck.cordis.yml` 未读取或修改。

## 1. 核心结论

DSH 在这个快照里最值得借鉴的是会话事件记录、可重建的模型上下文、分级压缩和可撤销能力扩展，不能直接等同于已经实现了私密/共享长期记忆学习系统。

| 能力 | 本地证据 | 结论 |
| --- | --- | --- |
| 会话历史与模型可见历史分离 | `DSH/packages/core/session/src/index.ts:643`、`DSH/packages/core/session/src/index.ts:726` | 已有追加事件记录及独立 surface 投影。 |
| 上下文优化 | `DSH/packages/compaction/compaction-basic/src/index.ts:147`、`DSH/packages/compaction/compaction-tool-result-pruner/src/index.ts:136` | 已有压力触发、工具结果裁剪、摘要与溢出恢复，不是长期事实库。 |
| 技能按需加载 | `DSH/packages/skill/tool-skill/src/index.ts:127`、`DSH/packages/skill/tool-skill/src/index.ts:213` | 已有目录披露、完整正文加载及目录更新，不等于自动学会新技能。 |
| 模型创建动态能力 | `DSH/packages/extensions/cordis-host-runner/src/index.ts:270`、`DSH/packages/extensions/tool-cordis/src/prompt.ts:7` | 可暂时扩展当前进程，不会因此自动改仓库或跨重启保存。 |
| 外部长期记忆 | `DSH/examples/mcp-memory/README.md:5`、`DSH/examples/mcp-memory/README.md:11` | 三组默认关闭的第三方 MCP 互操作示例；不是官方内置知识库或效果背书。 |
| 跨轮执行连续性 | `DSH/packages/workflow/tool-ralph/src/index.ts:153` | 新 worker 读共同工作树及有界交接报告；这是编码任务的延续机制。 |

本次在 `packages/` 的 TypeScript 与包清单中检索 dream、self-evolution、long-term memory、memory consolidation，直接命中主要是 Ralph 把工作树作为长期记忆的描述。这个检索不足以证明全仓不存在其他学习逻辑，但没有证据把社区的 dream、信念强化、自动技能晋升归为这份官方快照的完整内置链路。

## 2. 可以借鉴的机制

### 2.1 事件是历史，模型上下文是投影

`Session.append` 先快照与冻结数据，校验 surface，再追加到内存 log 并通知观察者；`deriveMessages` 只遍历当前 surface，按新节点增量投影，surface 被替换时重建。压缩替换的是模型可见节点，不是覆写原始事件。证据：`DSH/packages/core/session/src/index.ts:627`、`DSH/packages/core/session/src/index.ts:643`、`DSH/packages/core/session/src/index.ts:709`。

对 Moe-Buddy 的启发是明确分开三种记录：交互事件、治理后的长期记忆、一次模型请求的上下文视图。事件不能因为“曾发生过”就被当作真实事实；模型输出、工具回执、用户纠正仍保留不同性质。

必须区分内存追加与耐久提交。DSH 的 append 是同步内存操作，外围持久化有 write-behind 与 flush；不能把“已记事件”直接翻译成“已同步落盘”。证据：`DSH/packages/session/session-persistence/src/write-behind.ts:12`、`DSH/packages/session/session-persistence/src/write-behind.ts:59`。我们的权威记忆继续保持同步落盘后才发布：`crates/moe-memory-local/src/api.rs:458`。

### 2.2 先做便宜的裁剪，再决定是否调用模型摘要

工具结果 pruner 保留有界头尾和省略标记，其他结果字段不变；追加新 `tool/result` 替换可见节点，并指向原事件。原完整结果留在事件记录中。二次裁剪不应不断制造更小副本。证据：`DSH/packages/compaction/compaction-tool-result-pruner/src/index.ts:83`、`DSH/packages/compaction/compaction-tool-result-pruner/src/index.ts:136`、`DSH/packages/compaction/compaction-tool-result-pruner/src/index.ts:167`。

自动 compaction 在步进前检查压力，在规范化的上下文溢出错误后有限重试；已取消的请求不因裁剪进展而复活。证据：`DSH/packages/compaction/compaction-basic/src/index.ts:147`、`DSH/packages/compaction/compaction-basic/src/index.ts:179`。

借鉴方向：先按权限和有效性过滤，再去重、限量、截断可截断的大块工具结果，最后才考虑模型摘要。用户原话、身份锚点和关键工具错误不能无差别套用头尾裁剪；省略必须可见，必要时按授权回取原证据。

### 2.3 摘要是一份带版本和来源的派生结果

压缩准备阶段捕获选区及计量节点；摘要返回后重新校验整份 surface 或选中片段，拒绝已变更的输入。连同包装文本计算出的摘要若不比被替代内容更小，就拒绝提交。证据：`DSH/packages/compaction/compaction-basic/src/region.ts:339`、`DSH/packages/compaction/compaction-basic/src/region.ts:368`、`DSH/packages/compaction/compaction-basic/src/region.ts:387`、`DSH/packages/compaction/compaction-basic/src/region.ts:403`。

提交会记录摘要、输入事件序号、provider/model、usage 等，并另追加带精确来源序号的替换消息。手动事务在闭合后执行 durability checkpoint；失败与取消不是成功。证据：`DSH/packages/compaction/compaction-basic/src/region.ts:189`、`DSH/packages/compaction/compaction-basic/src/region.ts:231`、`DSH/packages/compaction/compaction-basic/src/region.ts:447`、`DSH/packages/compaction/compaction-basic/src/region.ts:462`。

借鉴方向：整理任务也绑定真实输入集、实例代际、配置、权限及版本，提交时复查。摘要不能因为被反复读取或摘要而获得新的用户证据，更不能丢掉父来源后晋升共享。已有 `MemoryTask` 和派生依赖机制继续复用：`crates/moe-memory-core/src/management.rs:158`。

### 2.4 缓存友好的压缩不等于免费记忆

摘要调用保留原 system、tools 与会话前缀，把压缩指令追加在末尾，意图复用 provider 的前缀 KV cache；代码仍然执行一次 `ctx.llm.stream` 并记录 usage。证据：`DSH/packages/compaction/compaction-basic/src/summarizer.ts:24`、`DSH/packages/compaction/compaction-basic/src/summarizer.ts:145`。

这是请求布局的优化思路，不是实测成本保证。能否命中缓存取决于端点、路由和前缀一致性；仍有读取上下文与生成摘要的成本。当前 Moe-Buddy 尚未接真实端点，不预先承诺节省比例，也不为了缓存扩大传输范围。

### 2.5 先给能力目录，再按需取正文

skill 工具先确认当前作用域内存在且可由模型调用的技能，再加载正文。pre-step 获取带完整性标记的目录快照；不完整快照不覆盖旧目录。目录变化才发布替换，正文由工具结果交付，不再次复制进上下文。证据：`DSH/packages/skill/tool-skill/src/index.ts:127`、`DSH/packages/skill/tool-skill/src/index.ts:213`；说明见 `DSH/packages/skill/tool-skill/README.md:11`。

借鉴方向：共享资料可以提供轻量目录及按需授权查询，不默认全文注入。目录不能泄露本来无权知道的私密标题或实体存在性；目录可见不代表正文可读，正文可读也不代表工具可执行。

### 2.6 能力可变，要有可撤销的运行生命周期

动态 Plugin 可以定义不可变 Package 版本，分别运行或切换版本；撤销时释放 handler 和 fiber。归属检查按定义它的 session 做，而运行效果可能影响共同进程。证据：`DSH/packages/extensions/cordis-host-runner/src/index.ts:768`、`DSH/packages/extensions/cordis-host-runner/src/index.ts:1219`、`DSH/packages/extensions/cordis-host-runner/src/index.ts:1232`；生命周期说明见 `DSH/packages/extensions/tool-cordis/README.md:19`。

重要限制：host-only 分支可直接进入 activate；Client 分支另有审批与版本授权规则，不能笼统说每次执行都有人确认。`node:vm` 限制环境不等于恶意代码安全隔离。证据：`DSH/packages/extensions/cordis-host-runner/src/index.ts:270`、`DSH/packages/extensions/cordis-host-runner/src/index.ts:277`、`DSH/packages/extensions/cordis-host-runner/src/sandbox.ts:229`、`DSH/packages/extensions/tool-cordis/src/prompt.ts:8`。

对我们只借鉴“候选版本、受控启用、失败撤销”的机制，不引入让桌宠运行模型生成任意代码的能力。受保护人设、工具授权和正式协议仍属于受信控制面，不能因“自我进化”而放开。

### 2.7 不依赖无限长会话维持连续性

Ralph 每轮交给不继承旧会话的新 worker，传递固定目标与有界结构化报告，并明确要求以工作树确认报告。达到轮数上限返回 budget-limited，不把无限循环当完成。证据：`DSH/packages/workflow/tool-ralph/src/index.ts:153`、`DSH/packages/workflow/tool-ralph/src/index.ts:171`。

借鉴的是“任务恢复依赖可验证产物，不依赖无限历史”。桌宠不能照搬共同文件夹为共同记忆：不同桌宠的私人经历仍隔离，共享产物仍需发布许可。完成声明也需实际回执或评测，不以模型自报作独立认证。

## 3. 与 Moe-Buddy 现有实现的对应

| 已有基础 | DSH 带来的增量思路 | 不变的限制 |
| --- | --- | --- |
| 私密/共享分区及授权检索 | 在获准条目之上组装可回查的一次请求视图 | 不放行 Unknown、UserInspection 或 ModelTransmission。 |
| 版本、来源和精确依赖 | 会话摘要及经验候选也保存实际输入证据 | 模型只能提议，不自填有效归属或解除敏感度。 |
| `MemoryTask` 及取消栅栏 | 整理、反思和评测各自绑定输入快照 | 纠正、删除、到期或撤权后，迟到结果不得回写。 |
| 原子持久化及历史回执 | 会话事件日志与记忆命令日志明确分工 | 不把 DSH 的异步事件持久化替换掉现有提交语义。 |
| 共享发布、正式资料独立入口 | 经验可逐步形成可复用资料或技能候选 | 私密复述不是匿名化，读许可不是发布许可。 |
| M5 离线对话 | 用离线夹具验证上下文构造与学习候选链路 | 尚无真实模型、持续行动或工具执行交付。 |

当前含工作上下文的派生输出被明确禁止持久化，见 `crates/moe-memory-core/src/management.rs:177`。因此不能只接一个“turn 结束时写摘要”的监听器，就声称实现了合规长期学习；必须先定义可持久化的交互证据与生命周期转换。

## 4. 建议讨论的最小框架

下面是讨论方案，不是已批准的新阶段或待自动执行的计划。先不改记忆大类，不引入图数据库、模型 SDK、通用插件运行时，也不承诺新的 crate 数量。

```text
可信交互与工具回执
        ↓
会话事件记录 ──→ 获准上下文视图 ──→ 模型/离线夹具
        ↓                               ↓
可核验原始证据                    学习候选，不直接写库
        └──────────→ 宿主校验 ←─────────┘
                         ↓
                  已有私密记忆接口
                         ↓
              独立共享审查 / 技能候选
                         ↓
                离线评测、受控启用、回退
```

- **会话记录适配**：记录稳定宠身份、会话/事件 ID、输入性质和必要回执；与现有记忆命令日志分开，不默认长期保留原始聊天、屏幕内容或凭据。留存、备份及删除政策待确认。
- **上下文组装器**：保留受保护身份与必要约束的预算，按权限、有效性和任务选片，返回真实输入引用、预算与降级原因。若要复现模型请求，需要定义受控快照或引用保留策略，不能借审计复制更多私人正文。
- **学习协调器**：以用户明确纠正、独立工具回执或经过审查的交互作为候选证据；区分事实、推断、临时任务状态与技能建议，复用任务栅栏和提交接口，不把模型注入内容算成新观察。
- **可演化策略/技能管理**：后续再定义版本、试用、回退与有效范围。先允许建议，不默认允许修改身份核心、系统协议、工具权限或执行代码。

优先讨论顺序是：先明确交互证据和会话留存，再定义请求上下文与可回查性，然后做离线学习候选和固定场景评测；自动共享、真实传输及能力执行另行确认。这里不创建 M6 或新的接力任务。

## 5. “进化有效”的验收建议

以下是建议的后续测试，不代表本轮已经实现或运行：

1. 用户明确纠正后，下一次回答采用新事实，旧摘要与依赖项失效。
2. 空会话反复开启、检索、压缩，不新增独立证据、不续期、不提高事实可信度。
3. A/B 桌宠同文私人记录不混入目录、提示词、日志恢复或共享资料。
4. 整理过程中发生删除、到期、撤权或切换实例，迟到候选不能提交。
5. 裁剪省略可见，工具 call/result 关系完整，摘要比原输入小且绑定正确版本。
6. 一项技能改善预先定义的目标场景，同时不增加越权、来源错误或其他场景的退化；否则拒绝启用或回退。
7. 执行失败、取消、不确定提交与预算耗尽分别报告，不用生成的解释冒充成功回执。
8. 检索/压缩前后记录用量、延迟与质量；真实端点未测之前不宣称固定成本节省或效果提升。

## 6. 可信度与范围

源码已阅读到事件追加/投影、摘要准备/提交、选区复核、裁剪、技能加载/目录刷新、动态运行/版本/撤销和 Ralph 的关键实现。参考项目有投影缓存和手动压缩竞态测试，例如 `DSH/packages/core/session/tests/derived-cache.spec.ts:23`、`DSH/packages/compaction/compaction-basic/tests/manual-compaction.spec.ts:487`；本轮只检查存在及相关内容，没有复跑，不宣称这些测试通过。

DSH 根说明仍列出 `self-modification/`，实际所读源码在 `extensions/`；`tool-cordis` 的 README 与源码模型提示在工具清单和生命周期描述上也有差异。以上不是对项目全面审计，只说明该快照研究必须以实际实现为准：`DSH/AGENTS.md:35`、`DSH/packages/extensions/tool-cordis/README.md:5`、`DSH/packages/extensions/tool-cordis/src/prompt.ts:26`。

本地根 LICENSE 声明 MIT；没有复制其实现或引入其依赖，也未审计每个 vendored/第三方组件许可。社区 `dsh-agent-memory`、`dsh-evolve`、`dsh-evolution` 本轮没有本地固定快照，不能用本文认证其源码、安全性或效果。需要继续研究它们时应分别固定提交，再核验候选、授权、删除闭包与实际启用链路。
