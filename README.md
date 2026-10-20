# Moe Buddy

以独立陪伴 Agent 为目标的 Rust 项目：先建立稳定身份、私密/共享记忆与可信交流入口，再接入真实模型、渠道和桌宠表现。

Tauri 桌面已完成 D→A→B→C：模型配置与聊天、持续会话、稳定伙伴档案、PNG/GIF 素材播放，以及私密记忆管理、诊断与本地恢复。API Key 和发送许可不落盘；当前验收使用合成回环和隔离存档，真实模型服务尚未验收。详情见 [桌面说明](apps/moe-buddy-tauri/README.md) 与 [现行计划](docs/AGENT-MEMORY-PLAN.md)。

## Windows 测试版

完整解压便携 ZIP，双击 **Moe Buddy.exe**。无需 Rust 或 Node；需要 Windows x64 和 Microsoft Edge WebView2 Runtime。主窗右上角 **退出应用** 会关闭整个程序，有未保存内容时先确认。窗口 X 仍是关闭聊天/隐藏到后台。

便携包内的 `moe-buddy.portable` 标记选择同目录 `data` 存档及 WebView 缓存；首次启动自动创建。升级前退出程序，保留完整数据目录。包内不包含已有聊天、密钥或私人素材。将素材目录或其 `manifest.json` 拖入主窗素材区即可播放；素材选择目前不跨重启保存。

开发者在依赖已缓存的工作区生成测试包：

```powershell
powershell -NoProfile -File scripts/package-windows.ps1
```

产物写入忽略的 `releases/`：便携目录、ZIP、SHA-256 文件；包内 BUILD-INFO.json 记录源码版本及二进制哈希。打包不会发布 GitHub Release。首次准备前端依赖仍需执行下面的 `npm ci`。

## 仓库目录

| 目录 | 内容 |
| --- | --- |
| `apps/moe-buddy-tauri/` | 当前产品入口；src 为 Vue 界面，src-tauri 为 Rust 宿主，tests 为界面与真窗验证 |
| `apps/moe-buddy-desktop/` | 保留的 Windows 原生验证宿主，界面停止扩展 |
| `crates/` | 记忆、流程、Agent、UI、聊天应用与共享桌面适配，各模块职责见 AGENTS |
| `docs/` | 四份现行专题文档；archive 仅作历史溯源 |
| `scripts/` | 仓库文档检查、构建打包及审查辅助脚本 |
| `target/` | 忽略的编译缓存、临时合成数据及验收证据；不能整体清理来代替存档治理 |
| `releases/` | 忽略的本机测试包输出，不入源码提交 |
| `local-materials/` | 忽略的个人导入素材，不入源码提交 |

平台队员工作副本及报告也忽略；不要手动删除已注册的工作树。源码与文档保留在既有层级，构建产物、个人素材、测试存档不混入源码。

## 当前状态

- M0–M5、上下文精进与 G0–G5 的离线范围已交付；两项原文留存 P1 已修复。R0–R4 离线工程及程序流程执行层第一版已完成；R1/R4 真实模型与完整质量门仍未验证，最新独立测试清单和全链证据见 REVIEW。
- 私密记忆按桌宠实例隔离，共享库可共阅不代表可共写；模型只提议，权限/来源/版本/取消及实际提交由可信服务裁决。
- 已有预设、会话暂存、回收区、原文文件恢复及固定 CLI Hooks 适配。通用流程已下沉到零依赖 `moe-workflow-core`；应用通过七阶段统一线上与离线聊天，异步结果绑定请求票据，提交与展示发布分别记录。真实服务验收、对外 HTTP 网关、微信/QQ 和生产 Hooks 仍未完成。
- UI 管理 U1 的两项生命周期 P2 已修并补正式回归；U2 已交付 Windows 原生桌面首版，复用记忆、预设、Agent 流程、诊断与 UI 管理，并接入受控真实模型请求与本地恢复。浏览器验证宿主已移除，当前仍不冒称真实模型服务验收。
- 动画编排 A1 与产品阶段 C 已集成：PNG 序列/GIF 实际解码、动作选择、播放/暂停/继续/停止重播、切包和释放沿用同一 Animator。坏包拒绝保留有效旧播放，换皮保持伙伴身份；工作台 A3 仍暂停。
- 架构基本分层，但夹具公开、会话职责集中等整理项仍存在；Agent 现经核心窄契约注入记忆门面，不把离线全绿当作生产闭环。

## 文档

项目概要、模块职责与编程规范统一见 [开发指南](AGENTS.md)；具体契约和进度使用以下专题入口。

| 内容 | 入口 |
| --- | --- |
| 决策、架构、阶段和统一验证门 | [现行计划](docs/AGENT-MEMORY-PLAN.md) |
| 记忆治理、上下文及 DSH 参考 | [记忆系统](docs/MEMORY.md) |
| 预设、Agent、流程执行、暂存/回收、文件格式和 Hooks | [网关契约](docs/GATEWAY.md) |
| 修复、架构待整理项和验收证据 | [审核与验收](docs/REVIEW.md) |

公开 API 与示例：[workflow-core](crates/moe-workflow-core/README.md)、[memory-search](crates/moe-memory-search/README.md)、[memory-core](crates/moe-memory-core/README.md)、[memory-local](crates/moe-memory-local/README.md)、[agent-runtime](crates/moe-agent-runtime/README.md)、[cli-hooks](crates/moe-cli-hooks/README.md)、[ui-core](crates/moe-ui-core/README.md)。raw-local 公开 API 见生成的 Rustdoc；旧项目原始材料见 [只读归档](docs/archive/pet-assistant/README.md)，不是现行实现说明。

## 桌面验证入口与跨端原型

已有 Windows 原生桌面验证入口：

```powershell
cargo run -p moe-buddy-desktop --locked --offline
```

该入口的界面停止继续扩展；既有模型接线、存档和持续日用限制见 [原生宿主说明](apps/moe-buddy-desktop/README.md)。

Tauri 2 + Vue 3 + TypeScript 宿主包含透明宠物窗、托盘、测试猫动画，以及复用 Rust 业务的模型聊天、记忆、诊断和恢复入口。从仓库根目录运行：

```powershell
npm --prefix apps/moe-buddy-tauri ci
npm --prefix apps/moe-buddy-tauri run build
cargo run -p moe-buddy-tauri --locked
```

无便携标记时默认读取 `%LOCALAPPDATA%\MoeBuddy\daily`；开发验证应通过 `--data-dir <path>` 显式选择临时根，该参数优先于便携标记。配置完整模型端点并明确允许发送人设、获准私密记忆和当前会话后才联网；恢复不续跑旧请求、不恢复密钥或发送许可。阶段 A 已交付会话切换、归档、轮转和 Unknown 后隔离新会话，阶段 B 已交付最多 8 个稳定伙伴的创建、切换和恢复。运行与真实界面 smoke 见 [宿主说明](apps/moe-buddy-tauri/README.md)，逐平台验证边界见 [审核记录](docs/REVIEW.md)。

## 业务边界与入口

业务边界见 [chat-app](crates/moe-chat-app/README.md)，网络和保存适配见 [desktop-adapters](crates/moe-desktop-adapters/README.md)。桌面壳与动画只接表现适配，不成为聊天或记忆业务的权限来源。

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
