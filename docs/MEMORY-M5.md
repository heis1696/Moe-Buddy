# M5：最小离线对话链路

日期：2026-10-10。仅在 Moe-Buddy 的两个既有 crate 上实现，零第三方依赖。这里的“对话”是受信身份—获准上下文—离线注入候选—宿主审查—持久化回执的有限链路，不是生产模型接线或自主 Agent。既有 [M3 管理契约](MEMORY-M3.md) 与 [M4 发布契约](MEMORY-M4.md) 不变。

## 1. 身份与输入边界

`LocalMemory::offline_dialogue(identity, session, purpose)` 由已认证的宿主绑定稳定 PetIdentity 和 SessionId，只接受 OfflineReasoning。Unknown、UserInspection、ModelTransmission 仍拒绝；OfflineReasoning 仅允许此离线夹具，绝不是模型传输许可。本阶段没有端点、HTTP、SDK、网络回调或“启用模型传输”开关，也没有把 get/search 当传输绕过入口。

`OfflineDialogue`、原始 CRUD、来源登记、配置、审查回调、命令编解码及回执确认全部是受信宿主 API，不能注册为模型工具。模型输出的唯一槽位是 `OfflineFixture::Candidate(MemoryDraft)`，不含 owner、session、operationId、批准、正式资料写入或工具执行字段。MemoryDraft 新增完整只读审查访问器，不暴露可变记录或原始映射。

`begin_turn(query, preference, now_ms)` 使用 M3 begin_task 捕获实际授权读取集及可用身份锚点；先许可再检索/预算，分区、宠身份、会话、层和用途仍由服务控制。OfflineTurn 与 OfflineProposal 的身份/会话绑定和任务证明不透明，不允许宿主换另一宠/会话接收它们；重开也不能恢复任务 epoch。

本阶段只消费隔离的、合成的注入输出，没有用户聊天字符串/自由提示词的附加入口，也没有跨回合的模型隐式历史。宿主若日后接入真实模型，必须重新明确端点、传输用途及所有实际输入，不能换一个空任务隐藏既往私密上下文、额外检索、库外文本或模型会话历史。核心不能发现恶意宿主隐瞒输入或谎报来源；合成夹具不是语义脱敏器或生产来源认证。

## 2. 候选、审查与即时提交

| 入口 | 含义 | 不承诺的事情 |
| --- | --- | --- |
| begin_turn | 获准条目快照与身份锚点、上下文报告 | 不授予用户查看或网络传输 |
| generate(turn, fixture, now_ms) | 复验任务，接收离线候选或报告夹具失败 | 不调用真实模型、不写盘、不证明推理正确 |
| review_and_commit(proposal, operationId, reviewer) | 宿主逐候选审查，注入实际时间并立即复验/提交 | 不提供排队批准、不代替真实宿主审查 |
| correct_original | 宿主核验新的原始证据并同 ID CAS 纠正 | 不允许将模型派生伪装成无依赖原始事实 |
| confirm_dialogue(request) | 仅确认已经成功同步并写入日志的完整历史请求 | 不执行、重放、补提交或恢复授权 |

审查回调取得不可变候选正文/所有元数据、实际上下文、可用身份锚点及降级报告。HostDecision 仅有 Reject、CreatePrivate 和 PublishShared；时间必须由宿主在真实审查结束后采样，不能取提议时刻或预先取时后排队。同步方法随即验证生成时间、任务代际/配置/epoch、全部读取项与到期，再 prepare/commit；不向外返回可延迟执行的批准。

私密新建复用 M3 栅栏命令 tag 10：实际记录边自动补齐、所有来源必须保留、敏感度不能降低，原始候选依赖与完整读取请求仍保留在 wire。普通共享复用 M4 tag 11：使用原始生成时间和真实提交时间，独立发布许可且按真实输入硬拒绝 private（包括 Normal）、身份锚点、工作上下文、正式资料和漏源。读许可不是发布许可；自动发布仍恒拒绝。正式文档不从对话候选授予权限，也不能混入默认普通记忆。

候选仅是待审查数据，非法正文、分类、证据或越权请求在提交门明确拒绝，不能把“生成了候选”报告成成功写入。只有文件同步和内存发布成功才返回 DialogueReceipt 中的 Committed&lt;WriteReceipt&gt;，FileSynced 的含义不扩大。

## 3. 原始证据纠正

correct_original 是独立的受信宿主纠正入口，不直接接受 OfflineProposal。宿主取得本宠当前记录并核对精确 CAS；当前与新稿必须无记录依赖、性质为 UserStatement/Observation、分类相同。派生记录与模型推断不能经此去掉证据边；一般派生更新仍由既有 M3 宿主管理接口明确维护全部来源与依赖，不新增自动纠正或工作上下文提炼。

CorrectionReview 提供旧记录和新原始稿，宿主完成事实/来源审查后返回当前提交时间；立即复用原有 update 命令和持久化流程。到期、归档、删除、保护、敏感度降级、源版本和时间倒退等仍按 M3 拒绝。成功纠正推进全库代际，旧 turn/proposal 取消；下次读取仅取得新版本，有旧边的派生项失效，不扩大删除或恢复范围。这个宿主存储检查不是 UserInspection 或 ModelTransmission 许可。

## 4. 可解释降级

ContextReport 明确报告条目数、身份锚点是否可用和 ContextPath。PreferEntity 的缓存命中/构建保留相应状态；关闭、满额、正式资料层不建卡片或空实体标签时，退回同一获准查询的原条目，报告 EntriesFallback。任务始终以实际条目读取集为权威，降级不能丢掉私密来源或改成空任务。无获准条目和无有效身份锚点也明确报告，不合成身份/证据。

DialogueError 区分 Binding/Context/Generation/Review/Commit/Correction/Confirmation 阶段及结构化原因：用途拒绝、绑定错误、ModelUnavailable、InvalidModelOutput、HostRejected、TaskCancelled、证据/权限/CAS 拒绝、OperationConflict、RecoveryRequired、CommitIndeterminate 等。模型夹具失败不产生候选、不写盘，也不声称回答或工具执行成功；宿主可以重新进行获准条目读取，但不能伪造模型回复或无授权回退。

Debug/Display 不输出候选正文、查询或身份锚点正文。错误状态是没有新的成功回执，不是“绝对未写盘”：CommitIndeterminate 可能已落入完整帧，必须关闭重开验证及成功同步；全部新旧 M5 入口在不确定失败后均封闭。半帧保留原档并拒绝，不截尾或自动修复。

## 5. 历史回执与恢复

DialogueCommit 是不可变完整请求句柄，成功回执和实际提交失败都可保留它；失败不能把它当已提交结果。to_bytes/from_bytes 只委托原有 MemoryCommand 编解码，operationId 单独保存，不增加日志封装或命令 tag。字节可能含正文，不能默认打印、传输或另建备份；解码不恢复实时任务/批准。

confirm_dialogue 只查已验证且同步恢复的成功账本，按 operationId 和完整命令字节比对。无历史成功即 NotFound，异请求即 OperationConflict，非写入回执类型明确拒绝；即使传入可由宿主执行的普通 CRUD 解码命令，也绝不执行它。旧成功的确认不返回正文、不改代际、不复活已删除内容、不恢复读/发布许可，不重新取一个旧提交时刻执行。重新审批是新真实请求，不能冒充历史请求。

日志外层 v1、命令 v1 和现有 v2 tag 4–13 完全不变。排他锁、私有候选副本、完整请求幂等、同步后发布及成功重放保持原语义；历史 v1 到期纠正、20,003 帧/10,001 次删除恢复和 100,000 墓碑上限仍有回归。运行策略/会话/身份/缓存/许可/水位重启清空，来源恢复仍要求显式升版。取消不等于停止外部 I/O，保留/备份/外部副本政策未定；日志/缓存/已持有快照可能留正文。Windows 仅文件同步，不保证目录断电耐久、防恶意有效旧日志回滚或物理擦除。

## 6. 运行与验收

受信宿主可在仓库根显式运行合成离线演示，父目录必须存在，日志路径必须尚不存在；重复运行需另给新文件名，不覆盖旧档：

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
cargo run -p moe-memory-local --example offline_dialogue -- target/m5-offline-demo.log
```

示例只对固定合成稿做确定性审查，不能用作生产自动发布规则；模型端点未选择、无工具执行，输出仅元数据/状态，不打印私人正文。独立公共夹具没有读入此前私人条目或任何模型历史，不能被解释为允许用空任务洗白。

针对验证入口为适配 tests/dialogue.rs 与内部 dialogue_* 故障用例，覆盖三条路由的写前/半帧/sync 失败和同步恢复，保留原 237 项。完整结果见 [施工计划 §11](AGENT-MEMORY-PLAN.md#11-m5-验收记录)；随后依次 fmt check、Clippy（-D warnings）、workspace test/build，另 rustdoc/文档测试（-D warnings）和活动文档 UTF-8/路径/围栏/空白检查。

收尾已检查 [施工计划 §3/§4](AGENT-MEMORY-PLAN.md#3-分批施工)：没有已定义的 M6 或明确下一阶段施工范围，不创建接力任务。端点/传输、GUI、持续行动循环、任意工具执行和旧档导入仍需另定范围；M5 离线链路完成不代表完整自主 Agent 交付。
