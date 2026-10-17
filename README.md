# Moe Buddy

以独立陪伴 Agent 为目标的 Rust 项目：先建立稳定身份、私密/共享记忆与可信交流入口，再接入真实模型、渠道和桌宠表现。

日用桌面第二步已接可配置模型后台请求及本地私密记忆／聊天恢复，详情见 [桌面说明](apps/moe-buddy-desktop/README.md)。API Key 和发送许可不落盘；当前验证为合成回环与恢复回归，完整实用性验收进入第三步，尚不声称真实模型服务验收。

## 当前状态

- M0–M5、上下文精进与 G0–G5 的离线范围已交付；两项原文留存 P1 已修复。R0–R4 离线工程及程序流程执行层第一版已完成；R1/R4 真实模型与完整质量门仍未验证，最新独立测试清单和全链证据见 REVIEW。
- 私密记忆按桌宠实例隔离，共享库可共阅不代表可共写；模型只提议，权限/来源/版本/取消及实际提交由可信服务裁决。
- 已有预设、会话暂存、回收区、原文文件恢复、固定 CLI Hooks 适配及受控流程执行；流程首接离线 Agent prepare/infer/finish，不接管业务裁决。G6–G8 暂停，未完成真实模型/对外 HTTP 网关/微信/QQ/生产 Hooks 联调；记忆模型出站仅回环协议已验证。
- UI 管理 U1 的两项生命周期 P2 已修并补正式回归；U2 已交付 Windows 原生桌面首版，复用记忆、预设、Agent 流程、诊断与 UI 管理，并接入受控真实模型请求与本地恢复。浏览器验证宿主已移除，当前仍不冒称真实模型服务验收。
- 动画编排 A1 已集成主目录：不可变 Clip 元信息、动作图/状态绑定、抢占/暂停和注入式 `ClipPlayer` 的纯 Rust 内核；不等于素材已加载或真实画面已播放。素材/显示 A2 与工作台 A3 未纳入本次集成。
- 架构基本分层，但夹具公开、会话职责集中等整理项仍存在；Agent 现经核心窄契约注入记忆门面，不把离线全绿当作生产闭环。

## 文档

项目概要、模块职责与编程规范统一见 [开发指南](AGENTS.md)；具体契约和进度使用以下专题入口。

| 内容 | 入口 |
| --- | --- |
| 决策、架构、阶段和统一验证门 | [现行计划](docs/AGENT-MEMORY-PLAN.md) |
| 记忆治理、上下文及 DSH 参考 | [记忆系统](docs/MEMORY.md) |
| 预设、Agent、流程执行、暂存/回收、文件格式和 Hooks | [网关契约](docs/GATEWAY.md) |
| 修复、架构待整理项和验收证据 | [审核与验收](docs/REVIEW.md) |

公开 API 与示例：[memory-search](crates/moe-memory-search/README.md)、[memory-core](crates/moe-memory-core/README.md)、[memory-local](crates/moe-memory-local/README.md)、[agent-runtime](crates/moe-agent-runtime/README.md)、[cli-hooks](crates/moe-cli-hooks/README.md)、[ui-core](crates/moe-ui-core/README.md)。raw-local 公开 API 见生成的 Rustdoc；旧项目原始材料见 [只读归档](docs/archive/pet-assistant/README.md)，不是现行实现说明。

## Native desktop companion

The product entry for the current slice is the Windows desktop host:

```powershell
cargo run -p moe-buddy-desktop --locked --offline
```

It opens a small always-on-top companion window and drives the same trusted `ChatApplication`
directly from the native event loop. The character, chat bubble, send action, memory entry and
activity drawer are desktop presentation concerns; business authorization remains in the Rust
application layer. This build is still explicitly offline and in-memory, so it does not claim a
real model, external channel, persistent memory or production animation assets.

## 业务边界与入口

业务边界见 [chat-app](crates/moe-chat-app/README.md)，桌面入口见 [moe-buddy-desktop](apps/moe-buddy-desktop/README.md)。桌面壳与动画只接表现适配，不成为聊天或记忆业务的权限来源。

## 验证

工具链固定为 Rust 1.99.0。完整统一门见现行计划，最小本地检查：

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
powershell -NoProfile -File scripts/check-docs.ps1
```

提交与推送须用户明确授权；不默认放行云传输、全量用户查看或工具执行。
