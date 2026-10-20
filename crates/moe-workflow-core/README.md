# moe-workflow-core

纯 Rust 程序流程定义与有界运行时。该 crate 不依赖记忆、Agent、UI、宿主或第三方库；适用于任何由受信 Rust 代码定义步骤、由调用方执行步骤并回报结果的流程。

定义使用不可变静态标识，校验重复节点、缺失/重复前置和循环。运行时限制累计实例容量，每个实例至多一个在途执行票据，且按定义顺序推进满足前置条件的步骤。失败、未知和取消停止实例；票据绑定具体运行与步骤，重复、跨运行协调器、过期及取消后的票据会被拒绝。

内核不执行步骤，不提供全局调度、脚本、任意跳节点、自动重试或跨重启恢复。进度仅是脱离运行时的只读元数据快照，不代表业务提交或外部回执。旧有 Agent 使用路径由 `moe-agent-runtime` 重导出这些类型。

```rust
use moe_workflow_core::{WorkflowDefinition, WorkflowOutcome, WorkflowRuntime,
    WorkflowStatus, WorkflowStep};

let definition = WorkflowDefinition::new("document-pipeline", 1, vec![
    WorkflowStep::new("extract", &[]),
    WorkflowStep::new("index", &["extract"]),
    WorkflowStep::new("publish", &["index"]),
])?;
let mut runtime = WorkflowRuntime::new(4)?;
let run = runtime.start(&definition)?;
for step in ["extract", "index", "publish"] {
    let ticket = runtime.begin(&run, step)?;
    runtime.complete(&ticket, WorkflowOutcome::Succeeded)?;
}
assert_eq!(runtime.progress(&run)?.status, WorkflowStatus::Succeeded);
# Ok::<(), Box<dyn std::error::Error>>(())
```

```compile_fail
use moe_workflow_core::WorkflowExecution;
let forged = WorkflowExecution {};
```
