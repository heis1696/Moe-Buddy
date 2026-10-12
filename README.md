# Moe Buddy

以独立陪伴 Agent 为目标的 Rust 项目：先建立稳定身份、私密/共享记忆与可信交流入口，再接入真实模型、渠道和桌宠表现。

## 当前状态

- M0–M5、上下文精进与 G0–G5 的离线范围已交付；两项原文留存 P1 已修复，最近 workspace 独立清单为 848 项（含 doctest）；R1–R3 在线协议适配、混合检索/索引及持久化门面/离线 Agent 注入已交付离线范围，下一批 R4 质量与最终复核，真实模型门仍未验证，最新全链证据见 REVIEW。
- 私密记忆按桌宠实例隔离，共享库可共阅不代表可共写；模型只提议，权限/来源/版本/取消及实际提交由可信服务裁决。
- 已有预设、会话暂存、回收区、原文文件恢复和固定 CLI Hooks 适配。G6–G8 暂停，未完成真实模型/对外 HTTP 网关/微信/QQ/生产 Hooks 联调；记忆模型出站仅回环协议已验证。
- 架构基本分层，但夹具公开、会话职责集中等整理项仍存在；Agent 现经核心窄契约注入记忆门面，不把离线全绿当作生产闭环。

## 文档

| 内容 | 入口 |
| --- | --- |
| 决策、架构、阶段和统一验证门 | [现行计划](docs/AGENT-MEMORY-PLAN.md) |
| 记忆治理、上下文及 DSH 参考 | [记忆系统](docs/MEMORY.md) |
| 预设、Agent、暂存/回收、文件格式和 Hooks | [网关契约](docs/GATEWAY.md) |
| 修复、架构待整理项和验收证据 | [审核与验收](docs/REVIEW.md) |

公开 API 与示例：[memory-search](crates/moe-memory-search/README.md)、[memory-core](crates/moe-memory-core/README.md)、[memory-local](crates/moe-memory-local/README.md)、[agent-runtime](crates/moe-agent-runtime/README.md)、[cli-hooks](crates/moe-cli-hooks/README.md)。raw-local 公开 API 见生成的 Rustdoc；旧项目原始材料见 [只读归档](docs/archive/pet-assistant/README.md)，不是现行实现说明。

## 验证

工具链固定为 Rust 1.99.0。完整统一门见现行计划，最小本地检查：

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
powershell -NoProfile -File scripts/check-docs.ps1
```

不自行提交或推送，不默认放行云传输、全量用户查看或工具执行。
