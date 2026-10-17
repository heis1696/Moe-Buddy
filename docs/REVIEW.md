# 审核与验收

## 仓库结构整理（2026-10-17）

按用户明确要求整理仓库结构、清掉多余文件夹和文件，并经用户明确授权提交推送。本轮只做删除、引用同步与验证，不改业务逻辑、不改公开契约、不升级依赖版本。

- 移除：浏览器验证宿主 apps/moe-chat-host（回环 HTTP 宿主、浏览器验收页与其静态资源、协议回归）与 scripts/chat-browser-smoke.mjs。浏览器页由宿主以 `include_bytes!` 内联，无法只删页面保留宿主；桌面宿主已是产品入口，因此整套浏览器验证夹具移出仓库，历史仍可由 Git 与下方记录追溯。
- 同步：Cargo 工作区成员与 Cargo.lock；README 的入口/运行说明、AGENTS 模块职责表、现行计划的 U2 与阶段表、桌面 README 的宿主说明。工作区成员现为 8 个 crate 与桌面应用。
- 历史：浏览器闭环与 smoke 的既往验收证据原样保留在下方 UI U2 记录中，仅标注夹具已移除；本整理不重写历史结论。
- 提交：先提交既有交付工作，再单独提交本次结构整理，两者一并推送，不创建新分支。
- 验证：fmt、workspace Clippy（`-D warnings`）、workspace test/build、严格 Rustdoc 均 `--locked --offline` 通过，check-docs 与 diff check 通过，日志在 `target/structure-cleanup-gates/`。合成、回环与文档检查不等于真实模型或渠道验收。
- 边界：真实模型服务、外部渠道、动画素材、持久化与暂停阶段状态不变；未删除用户存档、未改动线上环境。

## 桌面视觉与布局重做（2026-10-17）

用户要求推翻旧界面，并明确改由领队直接实现。本轮重做限于 `apps/moe-buddy-desktop` 的表现层与相关交互；保留 Windows 原生宿主和既有业务门面，没有新增依赖、素材下载、外传或存档策略，也没有提交、推送或创建分支。

- 新界面：纸白/苔绿主题、带图形和中文标签的 88px 左导航、伙伴标题与真实模式标记、居中限宽聊天、整合编辑/提示/发送的输入卡、按需展开的右侧管理抽屉。空态、消息角色对齐、表单、列表、焦点/禁用状态及错误排版统一调整；运行记录将事件与状态分两行显示，保留原始事件语义。
- 交互修复：增加真实原生“聊天”导航，收起抽屉后回到输入框，保留聊天和记忆草稿；空列表隐藏覆盖提示的原生列表控件，有内容后恢复可选列表。高度变化按旧视口判断是否贴底，避免缩放或通知条导致阅读跳动；表单紧凑布局保留发送许可、应用按钮与保存说明。自绘按钮不再在持有状态借用时向子控件查询文本，新增控件/字体/画刷纳入撤权、Tab、DPI 重建与资源释放。
- 定向回归：桌面 51 项通过、1 项历史手动探针忽略。新增或扩展导航/草稿/焦点、空列表遮挡、真实窗口仅改高度后的滚动、96/120/144/192 DPI 的导航/输入卡/通知/模型表单边界与新标题字体检查；既有 IME 策略、记忆增改删、预设切换、未知禁发、撤权及同步重入/销毁回归继续通过。
- 实际窗口：仅给测试子进程设置隔离 `LOCALAPPDATA`，启动当前原生可执行文件，完成合成消息发送、保存记忆、三个管理入口切换、返回聊天、最小窗口与模型表单拒绝空输入。10 张 120 DPI 截图位于 `target/ui-redesign-screenshots/`，包括空态、空记忆、有消息、记忆、模型、运行、三个最小窗口场景和明确错误；已目视检查。探针在 `target/ui-redesign-final-probe.ps1`，最终数据在 `target/ui-redesign-profile-release/`，窗口正常关闭，未读取真实日用档案或调用外部模型。
- 缩放边界：正式回归覆盖 100%/125%/150%/200% 的几何与字体；另在 `target/` 下的源码副本运行注入 DPI 绘制探针。192 DPI 窗口超出本机工作区，其截图不计完整窗口像素验收；本轮未更改系统缩放、验证真实跨屏拖动或解决小于最小客户区的屏幕适配。完整实窗截图验收以本机 125% 为准。
- 最终全链门通过：fmt、workspace Clippy（`-D warnings`）、workspace test/build、严格 Rustdoc（`RUSTDOCFLAGS=-D warnings`），Cargo 使用 `--locked --offline`；文档编码/路径/锚点检查与 diff check 通过，另对未跟踪桌面文件检查 UTF-8 和尾随空白。日志集中于 `target/ui-redesign-gates/`。现行计划与桌面 README 已同步视觉、尺寸和入口。日用接线的三步已执行且没有明确第四阶段，本次重做不启动会话轮转、动画或其他暂停阶段；既有持续日用阻塞仍见下节。

## 日用初版第三步：实用性验收结论（2026-10-17）

结论：阶段 3 的回环原生窗口验收已执行并修复本轮发现的问题；**持续日用验收不通过**，当前仅适合受控短会话试用。未提供真实服务端点／凭据，因此没有真实模型服务验收。领队亲自执行与修改，未派单、未使用子代理、未 commit/push/创建分支，保留既有源码与未跟踪目录。

- 完整链：独立进程启动原生窗口，在模型表单首次配置并应用，发送经真实应用业务链、后台 HTTP 和实际回执显示回复；记忆列表选中后将 `tea` 编辑为 `coffee`，下一轮 HTTP 输入仅含新版本；关闭并重新启动，聊天只读恢复，模型／人设恢复，Key 为空、许可未选中；重新授权后的请求召回持久化记忆，不包含旧进程聊天正文。
- 异常链：真实 HTTP 401 明确认证失败；8 秒延迟期间可展开记忆抽屉并操作搜索／刷新；服务等待 65 秒触发客户端实际 60 秒超时，未知禁发且重启后仍禁发；坏 JSON 同样隔离，发送动作及重启均未引发重复请求。通过真实 `MessageBoxW` 撤权确认后，8 秒迟到回复未进入 UI，面板和输入禁用。没有把这项单次确认框验证泛化为全部模态交互覆盖。
- 文件失败：在独立存档制造 `settings.pending` 目录，应用设置显示“设置未保存”，不冒称落盘；重启出现保留现场的读取失败消息框，不创建空档替代。运行期配置已生效与磁盘保存失败是不同状态；测试没有删除失败现场。启动失败消息框的自动确认未使进程及时退出，最后终止了该测试进程，不计为正常关闭验收。
- 修复①：HTTP 成功但 JSON 无正文、空白正文或超长正文，原先被判为可重试失败；现在均为未知，避免把服务可能已执行的请求当作可安全重试。新增 `successful_http_without_usable_text_is_unknown` 回环回归。
- 修复②：异步认证失败曾清空原草稿；控制器在请求等待期间保留草稿，明确失败时恢复，期间编辑过的新草稿不被覆盖，撤权清除保留副本。`external_model` 覆盖恢复与新草稿保护，原生 `asynchronous_auth_failure_restores_input_and_error_severity` 验证控件实际回填。
- 修复③：后台失败和未知提示原先一律使用普通信息严重度；现在使用应用状态映射的 Error／Warn，保留文字和状态形状。修复后的原生截图再次确认认证错误红色提示及输入恢复、未知警告与禁发。

证据均在忽略目录：`target/daily-stage3-probe.ps1`、`daily-stage3-errors.ps1`、`daily-stage3-revoke.ps1`、`daily-stage3-save-failure.ps1`、`daily-stage3-final.ps1`；可控回环服务 `daily-stage3-server.py`；`target/daily-stage3/requests.jsonl` 和 `daily-stage3-audit.py` 校验实际 HTTP 内容、请求次数及应用存档不含合成 Key。仅使用 `target/daily-stage3-*` 隔离 LOCALAPPDATA，没有读取真实用户存档或凭据。第一轮探针漏发列表选中通知而新增两条笔记，已修正探针并以新 profile-b 完整重跑；新一轮只保留修改后的单条笔记。截图位于 `target/daily-stage3-screenshots/`，03/04 为编辑和召回、05/06 为重启恢复、08 为等待中操作、11/14 为未知恢复、13 为真实超时、15 为撤权、17 为读取失败、18/19 为修复后状态；后续截图采用 PrintWindow 避免其他窗口遮挡，均未把截图当成请求内容证明。

针对性回归及主工作区 fmt、workspace Clippy（`-D warnings`）、test/build、严格 Rustdoc 已通过，日志 `target/daily-stage3-gates/`；文档检查及 diff check 随本节更新执行。不沿用历史测试总数。

阻塞与限制评估：

1. 未知／中断记录恢复后持续禁发，没有保留旧请求终态并显式另开会话的入口。一次超时即可阻断后续使用，属持续日用阻塞；不得指导用户删档规避。建议另行明确会话切换／归档与未知保留语义后补入口，而非把 Unknown 改回 Ready。
2. 外部请求每轮仍保守累计 4096 字回复预留，应用总额 65536 字，短输入约 15 轮即可能耗尽；运行／原文另有累计硬容量，无轮转。该设计是有界试用，不是全天陪伴；不能仅提升常量冒充解决。
3. 恢复原文可读但不自动进入新会话，持续记忆仍可召回。对隐私边界合理，但用户不能期待重启后无缝接续上下文。新网络请求缺完整诊断 run 事件，当前故障定位依赖聊天状态与宿主提示。
4. 真实模型兼容性／输出质量、真实 IME、多屏连续拖动及长期 GDI 尚未验；外部渠道、生产 Hooks、工具执行、动画素材未启动。计划只有三个步骤，没有已明确的后续阶段，本轮不再接力同阶段或自动开启暂停项。

## 日用初版第二步：模型后台请求与本地恢复（2026-10-17）

领队亲自实现原生模型表单与后台 Chat Completions 接线，未派单、未创建分支或提交。新增显式模型读取政策及 `prepare_external/dispatch_external/complete_external` 边界，未借用 `OfflineModel` 联网。应用继续单线程组合，宿主线程只处理不可变请求与单结果通道。设置不保存 Key 或发送许可；私密记忆与原文分别复用 LocalMemory 和 RawLocalStore，数据根／30 天正常期与 7 天回收期在 UI 和 README 可见。

定向证据：`crates/moe-chat-app/tests/external_model.rs` 覆盖默认拒绝、单次请求交出、实际结果提交、未知阻止重发、撤权及记忆修订拒绝迟到回复；桌面 `transport` 回环测试校验真实 HTTP 头和 JSON、成功文本、401 及不可解析响应；`storage` 冷开回归覆盖日志笔记修改、聊天恢复、重新授权召回和未结请求恢复后不重发。原文层会将未结交互恢复为取消，应用因此保守隔离恢复的取消／未知，不把恢复当作可继续执行。

主工作区 fmt、workspace Clippy（`-D warnings`）、全部 workspace test/build、严格 Rustdoc、check-docs 和 diff check 通过；日志在忽略目录 `target/daily-stage2-gates/`。最后的原生布局调整另补桌面回归与构建。实际 120 DPI 默认及最小窗口截图为 `target/daily-stage2-screenshots/01-chat.png`、`02-model.png`、`03-model-minimum.png`；已亲自查看模型表单截图，输入、许可、保存位置说明在最小窗口内完整显示。探针 `target/daily-stage2-probe.ps1` 使用隔离的 `target/daily-stage2-profile`，没有读取真实用户聊天或凭据，测试窗口已关闭。

边界：没有真实服务端点／凭据验收；完整首次配置→发送→编辑记忆→召回→重启，以及真实窗口请求中操作、超时、撤权和异常恢复，交下一阶段实用性验收。恢复的原文只读展示，不自动进入新会话输入；未知历史保守禁发，尚无显式放弃未知并另开会话入口。运行和原文均维持既有硬容量，不承诺无限会话或自动轮转。原生 IME、多显示器连续拖动、真实 MessageBox 自动化及长期 GDI 仍未验；外部渠道、生产 Hooks、工具执行与动画素材未启动。

## 日用初版第一步：领队界面与记忆管理（2026-10-17）

- 用户已明确授权真实模型与本地恢复，分步目标见现行计划“日用初版接线”。本节仅记录已完成的第一步，不把模型服务或持久化记为完成；后续第二步继续由领队亲自实现。
- 原生聊天默认居中限宽，顶部独立入口按需展开管理抽屉；布局增加记忆搜索与多行编辑区，编辑/删除只在选中后可用。修复自绘内缩辅助函数符号错误（此前边框落入子控件而被遮住），收敛标题/状态/提示文案。初始 1120×760、最小 880×640（96 DPI），实际截图检查于 120 DPI 完成。
- `SearchMemories`、`UpdateMemory`、`DeleteMemory` 通过应用与面板门面；只修改应用明确创建的私密笔记，核心校验来源、归属和版本。快照携带 revision，旧版本更新/删除失败；查询条件写入后保留，最多返回 100 条。原生编辑失败保留输入，确认删除不删除已有聊天，未保存记忆在关闭时提醒。HTTP 白名单未扩大，只调整测试枚举匹配和快照夹具以适配契约新增字段。
- 新增应用回归 `memory_management.rs`：检索、更新、旧版本拒绝、删除后下轮不召回、隐藏与撤权拒绝；新增原生回归 `memory_editor_search_save_delete_and_unsaved_close`：抽屉扩缩、新增/编辑、搜索、取消/确认删除、关闭取消保留记忆输入。旧原生/应用回归一并验证。取输入文本操作从 `build_window_plan` 的状态借用中移到借用前，避免可重入查询跨活跃借用。
- 实际窗口截图：`target/daily-ui-screenshots/01-chat.png` 与 `02-memory.png`；实际操作使用合成笔记，窗口已关闭。自动化探针在 `target/daily-ui-probe.ps1`，未触碰真实用户聊天、凭据或存档。测试使用原生确认桩，真实输入法/多屏连续拖动仍未验证。
- 当前仍只有离线回复和内存状态。第二步需接模型设置、后台请求、持久化/恢复和明确的传输许可；第三步做重启及失败路径实用性验收。禁止通过联网实现 `OfflineModel` 偷渡外传许可，禁止在 UI 中虚构真实模型完成状态。
- 本阶段主工作区全链门已运行并通过：fmt、workspace Clippy（-D warnings）、workspace test、build、严格 Rustdoc、文档检查和 diff check；Cargo 均使用 --locked --offline。日志在 `target/daily-stage1-gates/`，针对性日志在 `target/daily-stage1-tests.log`。未提交、推送或创建分支。

## 原生宿主生命周期阻塞项修复与计数修正（2026-10-17，第二轮）

- 基线：`021dc48` → `8c09ef3` 限定补丁（`apps/moe-buddy-desktop/src/host/mod.rs`、
  `src/host/tests.rs`、`docs/REVIEW.md`）经 `git apply --check` 通过后应用到隔离副本，
  三个文件与 `8c09ef3` 逐字节一致；本节为在该基线上的复审阻塞项修复。上一节（同日）
  关于本轮 8c09ef3 的"68 个 result 全绿"与"清单 1316 项"记录存在过度断言与重复计数，
  以本节为准。
- 阻塞项①根因（`host/mod.rs` 旧 `apply_dpi_at`）：`WM_SETFONT` 扇出与 `SetWindowPos`
  允许同步回调（自绘重绘回发 `WM_DRAWITEM`、同步 `WM_SIZE` 重入 `refresh`），子类化
  回调中 `DestroyWindow` 会经 `WM_NCDESTROY` 释放状态 Box，返回后的 `refresh(pointer)`
  解引用已释放内存。修复：引入窄的窗口实例生命周期——`WM_NCCREATE` 把随机实例代际
  登记入进程内注册表，`WM_NCDESTROY` 先清 `GWLP_USERDATA`、注销代际再释放 Box；
  `apply_fonts` 与 `SetWindowPos` 返回后按注册表代际复验（`instance_valid`），失效立即
  停止，不再解引用指针；`create_stage`、模态确认（撤销/关闭）同样改为代际复验。复验
  只比对登记表与栈上捕获值，绝不解引用已释放指针；窗口销毁（注销）、HWND 复用或堆
  地址复用（新实例新代际）都不会误判存活。原 `state_alive` 的"`GWLP_USERDATA` 等于
  指针"比对不足以排除复用，已废弃。
- 阻塞项②根因（旧 `apply_window_plan`）：`WindowPlan` 无实例/状态修订校验。阶段 B 的
  `EnableWindow` 等副作用同步重入（审查员注入 `WM_ENABLE`）完成撤权并嵌套刷新后，
  外层计划继续执行：重新启用控件并把阶段 A 捕获的过期草稿回写输入框，审查员实测
  `revoked=true, text="sensitive draft", send_enabled=1`。修复：`DesktopState.revision`
  修订号在每个结构性 `Action` 成功后递增（`SetDraft` 除外：真实键入由队列驱动，不会
  在阶段 B 内交错；阶段 B 的 `EN_CHANGE` 只能是本计划 `SetWindowTextW` 的幂等回显，
  排除它避免良性回显中止剩余布局副作用）；计划捕获构建时的代际+修订号，阶段 B 每个
  允许同步回调的副作用（撤权清空、逐控件 `EnableWindow`/`ShowWindow`、列表 `LB_*`
  扇出、草稿回写、`MoveWindow`、`LB_SETITEMHEIGHT`）返回后经 `WindowPlan::valid()`
  复验，实例失效或修订号前进立即中止，剩余副作用交由重入方自己的刷新收敛。另两处
  硬化：撤权计划永不回写草稿（`interactive=false` 时 `draft=None`）；`GetWindowTextW`/
  `LB_GETCURSEL` 按可重入回调对待——`control_text_if_alive`/`selection_if_alive` 返回
  后复验实例才允许建立下一段借用，并修掉 `LBN_SELCHANGE` 两臂在 `&mut` 借用内调用
  `LB_GETCURSEL` 违反本模块纪律的问题（查询移出借用）。未引入通用引擎、未吞通知、
  未用裸指针掩盖活跃借用；`WindowPlan.state` 裸指针仅用于 `valid()` 在实例存活确认后
  读取修订号。
- 确定性回归（`src/host/tests.rs`，桌面 crate 38→44 项）：`wm_enable_nested_revocation_
  stops_stale_plan`（`WM_ENABLE` 子类注入"撤权+嵌套刷新"，断言文本空、发送禁用、记忆
  面板保持隐藏、实例仍在册——修复前实测会回写"敏感草稿"）；`nested_structural_change_
  aborts_old_plan_before_writeback`（只撤权不刷新，断言旧计划中止后不回写过期草稿，
  后续显式刷新收敛）；`wm_setfont_destroy_during_fanout_stops_refresh` 与
  `set_window_pos_destroy_during_resize_stops_refresh`（`WM_SETFONT`/同步 `WM_SIZE`
  内销毁主窗口，断言扇出/刷新停止、窗口销毁、注册表注销）；`modal_reentry_delivers_
  command_and_keeps_window` 与 `destroy_during_modal_confirm_aborts_revoke`（确认桩
  以 `PostMessageW` 显式投递真实消息进模态循环：重入命令真实生效且窗口保留；模态期
  销毁后撤权流程立即放弃）；`real_resize_recomputes_layout_from_actual_client`（用
  `SetWindowPos` 真实改变客户区并缩到最小尺寸，布局与纯几何 `layout::compute` 期望
  一致——替换原直接发 `WM_SIZE`、客户区实际未变的伪缩放测试）。自证修正：DPI 字体
  断言改为独立构造期望 `LOGFONTW`（字号/字重/字符集/字体名）经 `GetObjectW` 与生产
  HFONT 逐字段核对，并用 `CreateFontIndirectW` 现建独立字体在真实 DC 量测行高，不再
  以生产 `font()` 生成期望；滚动测试覆盖 `SB_THUMBPOSITION` 与 `SB_THUMBTRACK` 两条
  路径的完整 32 位位置并回验 `SIF_TRACKPOS`（文档注明与真实鼠标拖动的区别：自动化
  无法注入 `nTrackPos`，验证的是消息处理契约，real mouse dragging was not revalidated in this round）；着色测试用
  `GetTextColor`/`GetBkColor` 回读所提供 HDC 的实际着色结果，防止参数互换回归。
- 变异验证（红/绿证据）：把 `WindowPlan::valid()` 临时短路为 `true` 后，两条计划中止
  测试精确在"过期草稿被回写"断言失败（41 通过 2 失败，其余不回归），恢复后全绿——
  证明回归针对被修行为。销毁路径不做变异运行（等于故意制造 UB）；其存活复验由注册
  表注销断言与桩注入销毁测试覆盖。
- Lead integration verification: added per-call invalidation checks inside font, list,
  layout, visibility and input-clearing fan-out. Desktop regression: 43 passed, 1 ignored.
  Integrated workspace fmt, Clippy with warnings denied, tests, build, strict Rustdoc,
  documentation checks and diff whitespace checks all passed. Logs: `target/desktop-lead-gates/`.
- The reported 1319 workspace total is withdrawn pending a consistent recount; do not add
  doctests twice or count subprocess output as separate tests.
- Limits: callback injection and a modal stub are not real user interaction acceptance.
  Real mouse dragging, multi-monitor DPI, vendor IME, native MessageBox automation and
  long-running GDI resource behavior remain unverified. The lead reviewed the final changes;
  no additional independent reviewer pass was available after the final fixes.

## Native host callback and DPI follow-up (2026-10-17)

- 修复（上一轮局部）：`theme::Metrics` 的字体字号保留 96 DPI 基准值，Win32 `CreateFontW` 入口统一按窗口 DPI 缩放一次；撤权快照应用时清空记忆输入框、Edit 撤销缓冲、所选记忆 ID 与详情文本；垂直滚动拖动读取 `GetScrollInfo(SIF_TRACKPOS)` 的 32 位位置；编辑框/列表框着色消息从 `wParam` 取得 HDC。
- P1 根因消除（本轮）：`apps/moe-buddy-desktop/src/host/mod.rs` 重写为"状态计算与窗口副作用分离"的两阶段窗口过程，回调不再创建跨可重入 Win32 调用存活的整个 `DesktopState` 可变引用。阶段 A 在短借用内完成视图模型、布局几何与画布量测，允许的 Win32 限于不分发消息的封闭集合（`GetClientRect`、`GetDpiForWindow`、`GetWindowText*`、`LB_/EM_` 只读查询、建字/量测 GDI、`InvalidateRect`/`SetScrollInfo`），产出只含拷贝数据的具名 `WindowPlan`；阶段 B 无借用执行 `SetWindowTextW`、`EnableWindow`、`ShowWindow`、`MoveWindow`、`LB_*`、`WM_SETFONT` 扇出等可同步重入副作用，重入消息（`EN_CHANGE`、`WM_DRAWITEM`/`WM_CTLCOLOR*`、`WM_SIZE`）各自建立自己的短借用，任意时刻至多一个借用存活。未把引用改成裸指针句柄、未加标志吞通知、未用 RefCell 重入 panic 代替修复，也未引入泛化命令引擎。配套修正：模态确认（撤销/关闭）改为阶段机——`MessageBoxW` 在无借用时进入模态循环，返回后先复验 `GWLP_USERDATA` 归属再继续，窗口若在模态期间销毁则不再触碰指针；`DestroyWindow` 只在阶段 B 调用且返回后不再使用指针；`WM_NCDESTROY` 先清空 `GWLP_USERDATA` 再经 `Drop` 释放 GDI 资源与状态 Box 并 `PostQuitMessage`，此后迟到消息取不到指针；消息循环改用局部 HWND + `IsWindow` 判断，销毁后不再解引用状态；`intercept_enter`/`redirect_wheel` 先拷贝所需 HWND、结束借用再 `SendMessageW`；预设选中即时启用按钮的 SELCHANGE 刷新（`update_apply_enabled`）保留。
- 真实宿主回归：`apps/moe-buddy-desktop/src/host/tests.rs` 在真实窗口类与真实 Win32 控件上驱动（确认框以脚本化桩替换并在桩内泵队列消息模拟模态循环重入；未开放业务权限，未启动真实模态框）：`en_change_reentry_and_set_text_draft_converge`（WM_CHAR 真实键入的 EN_CHANGE 在 `WM_CHAR` 内同步重入、草稿逐字符收敛；实测本机 Windows 的 EDIT 对 `SetWindowTextW` 不回发 EN_CHANGE，草稿保持权威、刷新收敛且无回写循环）、`close_confirm_idno_keeps_draft_then_idyes_destroys` 与 `close_with_empty_draft_destroys_directly`（关闭确认嵌套分发：取消保留窗口与草稿、确认后销毁、销毁后迟到消息安全）、`revoke_clears_text_undo_and_disables_everything`（撤权后聊天与记忆 EDIT 的 `WM_GETTEXT` 均为空、`EM_CANUNDO`/`EM_UNDO` 均为 0、控件全禁用、`WM_SETFOCUS` 不再聚焦聊天输入、撤权后关闭直接销毁）、`dpi_144_and_192_rebuild_real_fonts_once`（144/192 DPI 状态行高与按目标 DPI 现建真实 HFONT 的 `GetTextMetricsW` 度量完全一致，即字体只缩放一次；控件随 `SetWindowPos` 同步移动；实时 `WM_DPICHANGED` 入口收敛）、`thumb_drag_beyond_65535_keeps_full_position`（滚动条置于 70000px 后发 `SB_THUMBPOSITION`，`wParam` 高 16 位截断为 4464 而实现读 `SIF_TRACKPOS` 得完整 70000 并同步滚动条）、`coloring_and_ownerdraw_paths_return_brush_and_flag`（着色返回面板画刷、`WM_DRAWITEM` 返回 1、主窗口与画布 `WM_PAINT` 真实路径）、`send_button_click_delivers_offline_reply`、`memory_panel_flow_remember_select_detail`、`preset_flow_select_enables_apply_and_applies`、`wm_size_recomputes_layout_without_aliasing`（聊天/记忆/预设/布局基本操作）。探针 `probe_dump_host_metrics_to_target`（ignored）转储 DPI/行高/布局/列表状态到忽略目录 `target/desktop-host-probe/host-probe.txt`；本轮为消息级验证，未重新生成截图。
- 门禁：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`（68 个 result 全绿）、`cargo build --workspace --locked`、`RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps` 均通过（依赖已缓存，`--offline`）；`scripts/check-docs.ps1` PASS（15 个活动 Markdown）；`git diff --check` 干净。独立清单 1316 项（1243 单测 + 73 doctest；moe-buddy-desktop 38 项 = 既有 26 项 + 本轮 11 项原生宿主回归 + 1 项 ignored 探针）。注意：既有 26 项全部是 theme/presenter/layout/chatlog 纯表现模块测试，不代表原生窗口行为；原生行为验收以本轮 `host::tests` 为准。未 commit/push/建分支。
- 边界与剩余风险：真实 `MessageBoxW` 模态循环未在自动化中驱动（会阻塞），以脚本桩加"无借用进入模态"的结构保证；`SetWindowTextW` 不回发 EN_CHANGE 是本机实测的 OS 行为，若其他版本回发，重入由同一两阶段纪律覆盖（测试不假设其存在）。多显示器实时拖动 DPI、真实中文 IME 各厂商、长时间运行的 GDI 句柄周期与旧平台未验证；动画、多宠窗口、真实模型/渠道仍不在范围。

## Native desktop host (2026-10-15)

> 2026-10-17 更新：桌面宿主已从最小测试面板改造为可用的初期原生桌面 UI；本节记录当前
> 交付与实际验证。首版仅编译验证的旧状态不再是当前结论。

- 交付：`apps/moe-buddy-desktop` 重写为完整桌面聊天布局——顶部伙伴信息栏（克制身份占位
  "M" 圆形占位与"离线验收 · 非真实模型 · 仅本机"准确标记）、自绘滚动聊天气泡（贴底自动
  跟随、上翻保持阅读位置）、舒适输入区（Enter 发送、Shift+Enter 换行、IMM 组合串非空时
  Enter 不误发、失败保留草稿、空草稿明确反馈）、右侧 记忆/预设/日志 三个独立入口及真实
  内容（授权记忆列出与"记住"、离线预设目录与"使用此预设"切换、白名单日志投影与刷新）、
  状态栏（空态/请求中/失败/结果未知各成文案与形状，未知禁用发送）、"撤销授权"入口
  （确认后全部控件禁用并显示撤销通知）。带未发送草稿关闭先走 `ChatApplication` 关闭确认：
  取消保留草稿与窗口，确认丢弃后销毁窗口。窗口初始客户区 1024×704、最小 752×480，支持
  缩放/最小化/关闭，Per-Monitor v2 DPI 感知，Tab 循环与可见焦点框。视觉按 U2 规范：冷白
  `#F3F5FA`、白面板、深墨 `#252B3D`、墨蓝 `#5263A7`、次要字 `#778096`、边框 `#DCE3EF`，
  中文 Microsoft YaHei UI、日志 Consolas，8px 间距与 8px 控件圆角。没有角色素材，主区
  不再绘制大面积简陋角色，仅用身份占位。
- 结构：单文件拆为纯表现模块 `theme.rs`（U2 颜色/字号/8px 度量）、`presenter.rs`（快照→
  界面模型纯映射：状态文案、严重度、可用性、离线标记、边栏内容、Enter 策略）、
  `chatlog.rs`（注入测量器的纯历史布局与滚动钳制/贴底）、`layout.rs`（客户区与面板纯几何、
  最小尺寸钳制）与 Windows 宿主 `host/mod.rs`（事件循环、控件编排、IME Enter 拦截、动作
  分发、关闭确认）及 `host/paint.rs`（信息栏/通知条/气泡/空态/状态条/自绘按钮与列表行）。
  宿主只经既有 `Action` 与快照交互；`windows-sys` 仅追加同锁定版本内的
  `Win32_UI_Controls/Win32_UI_HiDpi/Win32_UI_Input_Ime` 特性，无新依赖、未换 GUI 框架、
  未改浏览器夹具；二进制改为 GUI 子系统，启动失败经消息框呈现。
- 实际窗口验证：本机 Windows 桌面会话启动原生窗口，脚本驱动（消息与真实键鼠）完成
  空态、空草稿发送反馈、草稿粘贴 + Enter 发送与离线回复气泡、记忆面板记住一条并列出、
  预设面板选中第二项后按钮启用、应用后标题/列表/详情同步"（使用中）"、日志面板受控
  事件与刷新、缩到最小客户区 752×480（窗口 768×519，布局无溢出）、带草稿关闭弹出内核
  确认框、取消保留窗口与草稿、再次关闭确认丢弃后进程以退出码 0 结束。期间发现并修复：
  预设选中后"使用此预设"未即时启用（SELCHANGE 缺按钮状态刷新）、边栏详情区文字被裁、
  预设切换后列表行文本未刷新（展示缓存签名改为 FNV-1a 覆盖行文本与激活标记）。
  12 张截图存于忽略目录 `target/desktop-u2-screenshots/`（01 空态、02 空发送反馈、
  03 草稿、04 回复、05 记忆空态、06 记住、07/07b/08 预设选中与应用、09 日志、10 最小
  尺寸、11 关闭确认全屏），操作日志在 `target/desktop-u2-gate-logs/run-desktop.log`。
- 门禁：纯表现模块 26 项针对性回归（离线标记准确性、结果未知/撤销禁用发送、Enter 策略、
  面板可见性与行映射、布局钳制、贴底滚动、度量随 DPI 缩放等）全部通过；workspace fmt、
  全目标严格 Clippy `-D warnings`、test、build、严格 Rustdoc 均 `--locked --offline` 通过，
  独立 `-- --list` 清单 1231 项（= 上一清单 1205 + 本轮 26，含 doctest，不累加子进程输出），
  日志在 `target/desktop-u2-gate-logs/`。未执行 commit/push/建分支。
- 边界：仍是离线内存验收——回复来自明确标注的离线验收模型，不声称真实模型、外部渠道、
  持久化、素材动画或托盘生命周期；160ms 状态过渡动画、多伙伴窗口、角色素材与文件持久化
  不在本轮；诊断日志仅经既有授权查询。原回环 HTTP/浏览器回归夹具 apps/moe-chat-host 已在
  2026-10-17 结构整理中移除，其历史 smoke 覆盖保留在下方记录，不再作为现行门。输入法组合判断依赖 IMM 组合串查询，真实中文 IME
  各厂商行为仅做了机制级验证（组合中 Enter 交还控件），未逐一实测全部输入法。

## UI U2 文字聊天浏览器闭环（2026-10-15，离线闭环已验收）

- 交付：`crates/moe-chat-app` 提供纯受信业务组合，apps/moe-chat-host 提供可启动回环宿主及真实浏览器界面（该夹具已在 2026-10-17 结构整理中移除，历史记录保留）。聊天、预设、记忆、日志四面板经 `UiManager` 注册和驱动，展示及生命周期计数来自真实视图回调；动画、桌宠显示、外部渠道、工具和自动落盘未接入。
- 业务：发送走可信入口、会话登记和 `AgentWorkflow::prepare/infer/finish`，以实际交付回执判断；明确私密记忆写入经授权召回，预设走真实注册表，日志采集与查看许可独立且不含正文或凭据。宿主仅回环，严格 Host/Origin/Cookie/CSRF，请求 ID 有界去重且不淘汰历史后放行重放；非法请求不得执行业务。默认明确标识离线验收，不声称真实模型联调。
- UI 实测发现并修复：关闭聊天视图曾销毁展示历史；改为应用持有最多128条消息的有界交付投影，重开核验后重新装配，草稿仍由控制器持有。关闭面板不再等于销毁业务会话。发送失败保留输入草稿；驱动错误由应用持有完整有界报告，原错误链不再随临时报告丢失，也不暴露给浏览器。
- 真实浏览器验证：浏览器 smoke 脚本（已随夹具移除，历史证据保留）连接实际 Rust 宿主与应用，通过十组检查：真实挂载、IME/换行与发送、记住后召回、预设绑定、受控日志、隐藏/恢复/确认关闭/重开、同步草稿删除后的隐藏/关闭/重开、桌面与窄屏布局、回执丢失不重发及只读恢复、撤权清缓存。撤权前未同步聊天/记忆文本会被浏览器和服务端快照同时清除；截图已目视检查，结果留在 `target/chat-u2-browser/`。前端十二项 Node 定向测试通过。
- 全链：在独立 `target/chat-u2-verification/clean-build` 通过 workspace fmt、全目标 Clippy `-D warnings`、test、build、严格 rustdoc，构建命令均 `--locked --offline`；最终测试清单独立计数1205项，不累加子进程重复输出。活动 Markdown 的编码/路径/锚点/格式门及 `git diff --check` 通过。本轮仍未 commit/push；真实模型、外部渠道、持久化和动画不在范围内。
- 终审结果：登记前按真实 Session/Agent 上限预留输入与回复空间；登记后 bind/prepare/infer/finish 失败必须拿到真实取消回执，否则交互隔离、保留原始与处置错误、阻止重试并显示 Unknown。预设目录来自真实视图缓存。`set_draft` 支持空字符串清除，记忆仍拒绝空白；HTTP 分包头严格按完整头预算拒绝超限。诊断错误按原始 `GatewayError` 变体分类，容量/时钟/过期不再误报授权拒绝。两项 UI P3 正式回归、四项应用异常处置回归、宿主新增协议回归均通过。

> 更新：2026-10-14。对象：Moe-Buddy 已复核的记忆、Agent 与 RAG 离线底座及主分支集成。M0–M5、上下文精进与 G0–G5、RAG R0–R4 离线范围的证据分阶段记录；此前 R4 三项 P2 修复后及主目录集成复验的独立清单均为 879 项（含 doctest 及 3 项显式 example 测试），R3 的 848 项仅为历史快照。R4 安全回归、冻结质量工程、显式真实校验入口与合成基准不推进 G6–G8，不接真实对话模型、对外 HTTP 网关、渠道或生产 Hooks；施工和复核当时未执行领队 commit/push/建分支，后续用户明确授权的合并、提交与推送见主分支集成记录。
> 结论：整体暂不能验收，R4 真实完整质量门尚未完成；用户后续复核复现的三项工程 P2 已修复并补回归，证据见 R4 同节。两项留存 P1 已修复；R4 独立审查和已完成定向回归范围内未发现新的已复现 P0/P1；审查提出的三项安全覆盖 P2 和两项质量工程 P2 均已补齐。Agent 具体记忆耦合已在离线范围经三方法核心窄契约关闭（P2-B）；夹具公开、职责集中和全局失效等整理项仍保留。离线/回环全绿不代表真实模型、生产闭环或完整 RAG 验收通过。
> 当前工作区：以主干 `96c1274` 为基线，U1 补充复审的两项生命周期 P2 已修并新增 7 项正式回归；本轮 UI 65 项、workspace 独立清单 1131 项（含 doctest）与全链门通过。Windows 原生桌面宿主已加入工作区，仍为离线/内存范围；修复已按用户明确授权提交并推送，动画素材、真实模型/渠道和持久化不在本轮。历史 944/1002/1024/1124 等数字只对应各自审核快照；R1/R4 真实门与其他暂停边界不变。

## 主干集成与验证（2026-10-14）

- 授权与范围：按用户明确要求在 `main` 集成现有流程层、UI U1、记忆缺陷修复，以及已独立审核的 A1 动画内核。动画仅取 `animation.rs`、`animation/definition.rs`、两份专项回归、公开模块入口和对应 README；两份内核文件的原始 SHA-256 与上文 A1 审核记录一致，集成后的内容逐文件核对一致。没有合入工作树中另有的 `moe-animation-assets` 或其 workspace/素材层变更，没有选择 GUI 或开启 A2/A3。
- 本轮实际验证：Rust/Cargo 1.99.0，独立构建目录 `target/main-integration-20261014`；先通过动画定义 51 项、运行器 67 项及 UI 原有 58 项，再通过 fmt、workspace 全目标严格 Clippy、workspace test/build、严格 Rustdoc test/doc。Cargo 构建/测试/文档均使用 `--locked --offline`；独立 `-- --list` 清单为 1124 项（含 doctest），不是将专项回归与 workspace 总数重复累加。完整命令日志保留于该目录的 `gate-logs/`。
- 真实性与遗留项：当前正式测试全绿不关闭 UI 补充复审发现的两项 P2（待释放同 key 重开、隐藏后的失效加载阻止关闭），其反例与修复要求仍见本页 U1 补充复审。记忆真实模型/完整质量门、真实 GUI、动画素材导入/播放、业务诊断接线与工具执行未验收；没有以离线测试代替生产闭环。

## UI U1 两项生命周期 P2 修复（2026-10-14）

- 基线与复现：主干 `96c1274`，修复前新增六项 `headless` 回归全部失败，复现同 key 待释放时再次调用工厂，以及隐藏/新加载/失败修订的失效负载占用队列、阻止普通/确认关闭；不是将编译错误或未运行的探针当作反例。
- 同 key 资源占用：注册表新增内部待释放按 key 查询；`open` 在工厂、入参与容量分配之前返回 `UiError::PendingReleaseForKey { key, record }`，重试失败仍阻止同 key 重开，成功释放后才允许新建，其他 key 不受影响。回归覆盖部分挂载、普通关闭、隐藏清理失败及失败/成功重试，并断言工厂次数与不同作用域隔离。
- 失效加载：加载序号前进或耗尽即移除已失效的票据化负载，隐藏、恢复、新加载、修订与释放共享该边界；普通输入和当前有效结果仍保留并阻止普通关闭。原队列/析构断言同步为立即清理语义，驱动 stale 计数不重复统计生命周期已丢弃的负载，没有靠恢复显示或伪造强制撤权绕过保护。
- 所有权回归：第七项 `revocation` 回归在 `unmount` 入口断言失效负载已析构，覆盖卸载成功与失败后重试。旧强制释放测试原先在待释放同 key 下再次构造实例，改用两个不同桌宠作用域继续验证独立失败记录和撤权边界，不保留已确认的非法重开前提。
- 验证：UI 65 项；Rust/Cargo 1.99.0，独立目录 `target/ui-p2-chat-20261014` 的 fmt、workspace 全目标严格 Clippy、workspace test/build、严格 Rustdoc test/doc 通过，Cargo 使用 `--locked --offline`。独立 `-- --list` 清单 1131 项，不累加 writer-lock 子进程输出；日志在 `gate-logs/`。本批只关闭两项已知 P2，未选择 GUI、接聊天/诊断业务或扩大模型/落盘权限。

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

## 程序流程执行层第一版（2026-10-13）

- 范围：用户本轮明确启动计划新增的最小执行层。仅在 `moe-agent-runtime` 内新增 `workflow.rs` 和 `agent_workflow.rs`；没有新增 crate、第三方依赖、I/O、系统时钟、脚本/表达式入口或可见日志实现。核心与存储适配未改，Cargo 清单/锁文件未改。
- 内核：受信静态定义及非零版本、1–128 节点；启动前拒绝重复节点、缺失/重复前置与循环。实例捕获不可变定义，按首个满足全部成功前置的声明顺序推进；私有票据绑定协调器/实例/本次节点执行，重复、迟到、取消后及跨协调器结果均拒绝。失败/未知/取消停止，实例互不阻塞，累计容量和独立只读进度快照均有回归。
- 首个业务接线：`AgentWorkflow` 包装既有 prepare/infer/finish，任务及未验收结果不外露，复用原权限/来源/时间/预设/版本和最终提交复验。推理成功只表示已有待裁决结果；模型失败/取消/未知经原账本 finish 登记后再映射流程终态。准备后取消先走 G1，取消失败不改进度；Agent 报错或背压即停止且不重跑，流程失败不冒充会话失败回执，未结交互须宿主显式处置。
- 测试与审查：队员交付内核 10 项及包装 9 项，领队审核并集成；移除新增代码注释以符合工程约束，补充当前节点在途时重交前节点票据不改进度，以及 prepare/infer/finish 三阶段后端 `RecoveryRequired` 原错误类型/完整错误链保留、零额外推理与不伪造终态回归。当前 `workflows` 10 项、`agents` 58 项（包装新增共 10 项）在主目录实际通过；运行 crate doctest 18 项，其中新增 3 项、原离线示例改走包装入口。独立审码判官只读复核未发现可复现阻塞缺陷；容量非法复用 `InvalidDefinition` 仅为非阻塞命名观察，本批未扩接口。
- 最终门：Rust/Cargo 1.99.0。领队在当前主目录实际通过 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo test --workspace --locked --offline`、`cargo build --workspace --locked --offline`、`cargo doc --workspace --no-deps --locked --offline`；test/doc 均设 `RUSTDOCFLAGS=-D warnings`。独立 `cargo test --workspace --locked --offline -- --list` 清单为 902 项（879 + 20 个运行回归 + 3 个 doctest），不累加测试内子进程输出。11 份活动 Markdown 的 UTF-8/路径/锚点/格式门及 `git diff --check` 同时通过；原始验证日志留在本机忽略目录 `target/workflow-validation-20261013`，未提交构建产物。
- 用户请求后的独立复核（2026-10-13）：审查当前主目录未提交的第一版实现，而非仅查看交付摘要；内核与业务包装职责分离，推进票据、只读快照及原 Agent 业务校验未发现新增可复现阻塞缺陷，本批离线范围通过，无需因本次审核返工。重新实际通过上述全部门，针对性 `workflows` 10 项、`agents` 58 项、运行层 doctest 18 项；workspace 独立清单仍为 902 项，11 份活动 Markdown 与 diff 检查通过。此次未修改功能代码。
- 独立探针：在忽略区编写并实际通过 3 项公开 API 探针。用全排列判定器核对四节点全部 4096 个无自环有向图，正确接受 543 个 DAG、拒绝其余有环图；逐一验证确定性顺序、越步拒绝、重复票据不改进度，以及各 DAG 每个节点位置三种停止结果合计 6516 种终态场景。另验证推理后跨协调器/时钟回拨取消被拒且进度不变、合法取消阻止后续交付，以及错误 Agent 运行实例不会调用模型或改写任一会话。探针及原始复核日志仅留本机 `target/flow-review-20261013`，不进入生产接口、不计入正式 902 项清单。
- 后续边界：本批最小流程已完成，未安排新增流程阶段；复杂分支/循环、自动重试、并行调度、跨重启恢复仍不实现。通用协调器的容量是累计实例数而非并发上限，宿主不能无界复用；包装器方法错误后流程停止不等于会话已终结，宿主须按既有授权接口处置未结交互。这两项是已明确并验证的契约，不作为新增缺陷或要求本批扩功能。R1/R4 真实模型质量门仍未验证，G6–G8 与可见运行日志实现没有自动启动，不将离线验证冒充产品完整验收；本轮领队未执行 commit、push 或创建分支。

## 动画编排 A0 契约定稿（2026-10-13）

- 范围：用户要求按现行计划开始施工，先完成动画 A0 首版契约；唯一设计入口为 [动画施工契约](AGENT-MEMORY-PLAN.md#首版冻结契约a02026-10-13)。本批只修改计划与审核记录，没有新增动画代码、公开 API、crate 或依赖，不把文档定稿称为动画播放已经实现。
- 独立复核：审码判官只读审查提出三项阻塞运行语义的 P2 与一项非阻塞 P3，未发现 P1 越权。领队采纳并裁定：重复状态二元组不覆盖显式候选，真正改选替换候选；暂停期显式请求只排队；收尾中正常退出/同优先级/AfterExit 请求只更新后继而不重入；成功停止、无后继终止与故障确认停止清理旧余量及候选。上述场景已加入 A1 必测清单，当前还不是已运行的 Rust 回归。
- 票据保证收口：仅保证跨播放重复/迟到与错误实例/定义/资产的隔离；首版同步后端须返回本次调用结果，没有逐操作序号，不宣称可识别同一播放内格式合法的结果重放。保留单通道停止确认、未知结果故障锁定、唯一后端时序与宿主注入时间，不改业务流程 DAG 或记忆/工具权限。
- 验证：领队实际通过 11 份活动 Markdown 的 UTF-8、路径/锚点、围栏与空白检查，以及 `git diff --check`。本批只有文档改动，未运行 Rust 测试、构建、clippy 或 Rustdoc；902 项仍是上一批独立清单，不新增或累加本批测试数。独立复核针对修订前契约，修订由领队核对，不冒称已获第二次独立复核。
- 下一阶段：A0 已完成，A1 的纯 Rust 定义/校验、状态绑定、Animator/ClipPlayer 及无头回归可以继续，限现有运行 crate。A2 的素材来源/许可、预算、封装、真实显示宿主/依赖仍须确认，A3 工作台尚未启动；不启用 G6–G8、可见日志、工具执行或多宠窗口，不执行 commit、push 或创建分支。工作区其他已有/并行改动不属于本批交付。

## 动画编排 A1：独立审核（2026-10-13）

- 对象：本仓库集成工作树 `.agentdeck-worktrees/task-t_mv6ik69f_df3bfbad4bbd-integrated` 中的未提交 A1 实现，基线 HEAD 为 `2d8248155f201be288a1584f9f7113c398590a48`。主目录仍在 `6172d0a`，没有动画源文件；不能把该基线提交、主目录或并行诊断模块当作本次动画代码身份。本轮不集成、不改功能代码、不提交推送。
- 源码身份（路径相对该工作树的 `crates/moe-agent-runtime`）：`src/animation.rs` SHA-256 为 `ca7c96d4b4c3cd60142ac1299084754f4c4bef1ed770200990f050608f8f7845`；`src/animation/definition.rs` 为 `9072d1ef6909bef3524f62e808c96c048aa50b197c86361a44868587ec6870cf`。两份源码及两份专项测试在复核前后哈希一致，未用测试期间变化的快照充当同一版本。2026-10-14 复核原文件后修正 `animation.rs` 哈希中多写的一个 `e`，不代表源码变更或重新验收动画后续阶段。
- 结论：在 A0/A1 已约定范围内通过，未发现新增可复现的 P0/P1/P2 契约缺陷，不需要因本次审核返工。动画节点只引用不可变 Clip 元信息，后端由 `ClipPlayer` 注入；状态绑定、组合动作、转换和抢占没有侵入记忆或程序流程。没有新增 crate、依赖、I/O、时钟、解码、GUI 或通用脚本入口；合成后端只在测试边界，未作为产品播放器导出。
- 运行复核：检查即时环/时序环区分、有限收尾、重复同步与显式候选、三阶段抢占、暂停排队、旧播放停止确认、跨播放票据、错误链和故障锁定，以及时间余量、预算和清债。保守的序号耗尽 preflight 和不提供操作级防重放是已有明确边界，不作为新缺陷；不能据元信息或合成回报宣称实际资源已呈现。
- 实际验证：Rust/Cargo 1.99.0；先通过定义 51 项、运行器 67 项与运行层 doctest 21 项，再通过 fmt、全目标 Clippy `-D warnings`、workspace test/build、严格 Rustdoc、11 份活动 Markdown 和 diff 门。test/doc 均设 `RUSTDOCFLAGS=-D warnings`，Cargo 构建/测试使用 `--locked --offline`；独立 `-- --list` 清单为 1024 项，含序号耗尽单测，不累加子进程输出。该总数属于审核工作树，不替换主目录的旧清单或证明并行功能已经验收。
- 额外 3 项公开 API 探针全部通过：复用测试目录的合成后端，用独立时长计算核对三片段的 7250 种时间分配及 3 种总时长为 `u64::MAX` 的组合；用模运算核对 19 个时序环总时长（最大 65537ms），在每次预算让出之间暂停/恢复并排空余量；同一不可变定义的两个实例执行 10000 条确定性交错推进/暂停/恢复/停止/重播命令，游标正确且另一实例快照不变。探针不计入正式 1024 项，也不作为生产后端。日志、哈希记录及探针仅留该工作树忽略目录 `target/animation-review-20261013`。
- 未交付范围：A2 的序列帧/GIF 导入、真实播放器与实际显示，以及 A3 可视化无限画布均尚未实现。只有定义、共用校验与只读快照的编辑预留，不等于编辑器已有拖拽/连线。下一步仍需确认素材/许可、资源预算、封装、显示宿主和依赖；本轮不擅自进入 A2/A3，不改变 R1/R4 真实门或其他暂停阶段。

## 可见运行日志首批内存交付（2026-10-13）

- 范围：按用户明确启动，在 `moe-agent-runtime` 新增 `diagnostics.rs`、受控导出及合成 `tests/diagnostics.rs`。标准库纯内存实现，默认关闭，采集和查看分别配置；没有新增 crate、依赖、I/O、真实业务接线、GUI、文件或导出，不改记忆日志、业务账本、R4 及 G6–G8 的既有边界。
- 交付：宿主注入进程代际/稳定域/主体/许可/单调时间，服务签发不可外部构造的 emitter/access/run/cursor；每次读取复验。分区硬预算包含实际容器容量、健康、索引、别名及 cursor。白名单事件、安全错误码、正反向快照分页、安全搜索、健康查询、运行别名查询与业务/传输双状态投影已有针对性回归。
- 复核闭环：修复跨分区双状态证据被共享水位吞掉、崩溃严重度错误、错误输出残留与查询缺项。独立审查另发现 1 项 P1（自定义 Hasher 可抽取 opaque 别名私有域/主体偏移）及 3 项 P2（服务地址复用导致别名碰撞、同毫秒跨分区倒序、已知拒收仍宣称证据完整）；领队移除公开别名 Hash、引入 checked 实例标识与宿主跨重启代际、按受理顺序投影并隔离私有序号的等值比较、将可见拒收/省略纳入完整性判断，补入正式回归及 compile-fail 契约。
- 容量语义：事件是可淘汰保留窗口；运行登记与别名是代际累计额度，到限明确拒绝，不承诺无限尾随。可见分区损失会保守影响同域其他可见运行，不是逐运行精确丢失追踪；隐藏分区不影响可见结果。字节计费使用实际 Vec 容量与保守固定开销，不声称测量分配器元数据或系统峰值。
- 最终实际验证：日志集成回归 24 项、模块内单元回归 4 项通过，另有 README 合成示例及 opaque 别名禁用 Hash 的 compile-fail 契约。领队已在主工作区通过 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo test --workspace --locked --offline`、`cargo build --workspace --locked --offline`、`cargo doc --workspace --no-deps --locked --offline`；test/doc 设置 `RUSTDOCFLAGS=-D warnings`。独立 `cargo test --workspace --locked --offline -- --list` 清单为 932 项（含 doctest，不累加子进程输出），11 份活动 Markdown 的 UTF-8/路径/锚点/格式门及 `git diff --check` 通过。原始最终门日志在本机忽略目录 `target/diagnostics-validation-20261013`。补充回归修复了反向查询整页容纳保留窗口时漏报已淘汰前缀的 Gap，崩溃码固定为 `process.crashed`。
- 最终独立复核通过：原 1 项 P1、3 项 P2 均确认闭环，限定日志范围内未发现新的可复现 P0/P1/P2。复核队员从当前源码重新编译并实际通过集成 24 项、单元 4 项及 Hash compile-fail 1 项；隔离工作副本的忽略区另有 5 项探针通过，覆盖隐藏分区损失/其他主体查询非干扰、跨主体/域/服务/代际别名解析拒绝、运行 cursor 隔离、六个可见分区正反分页及淘汰缺口。探针不进入正式测试、不计入 932 项清单；队员未复跑 workspace 全链门，其结论不替代领队的最终门记录。本轮仅补文档，重新通过活动 Markdown 与 diff 检查。
- 提交前隔离验证（用户明确授权精简指南并 commit/push）：本次仅提交日志模块、相关契约/验收与精简后的 AGENTS 接入索引，不包含并行流程层或动画计划的未提交改动。上述主工作区 932 项包含并行流程层变更；从暂存区导出的独立提交快照实际通过 fmt、workspace clippy/test/build/doc（locked/offline，test/doc 设置 `RUSTDOCFLAGS=-D warnings`），独立测试清单为 909 项，活动 Markdown 与 diff 检查亦通过。原始日志位于本机忽略目录 `target/diagnostics-commit-validation-20261013`，不把并行任务的结果冒充本提交独立验收。
- 本批没有授权自动施工的后续日志阶段。业务采集、查看器缓存租约/缓存管理、重复告警合并、优先级预留、文件与导出均未实现；桌面和真实模型/渠道仍未验收，既有其他任务的未提交改动保持不动。提交推送已获用户明确授权，不创建新分支。

## 五系统审核确认问题修复（2026-10-14）

- 范围：用户批准先修问题，本批只关闭已确认的两项 P2、两项 P3；不改 API 预设、程序流程或诊断日志行为，不集成动画工作树，不启动动画 A2/A3、G6–G8 或真实模型验收。主目录 HEAD 为 `65b8968a0a7d6b4959c2d55f33e3e271abd05557`，原有流程层等未提交内容保留；没有执行 commit、push 或创建分支。
- P2 切片内存放大：`chunk.rs` 增加私有借用式惰性迭代器，公开 `split_chunks` 行为不变；`index.rs`、`cache.rs` 逐片扫描，队列/批次/向量条目共享不可变父记录快照，只为实际批次拥有切片正文。完整扫描、健康/授权复验和失败不发布仍保留。回归覆盖 16 条各 4096 汉字、3072 重叠、共 16384 片且队列/批次/向量容量各为 1，以及队列已满仍发现后续非法记录、错误不修改队列、切片借用父正文和父快照共享。
- P2 内部取消 ID 冲突：`LocalMemory::cancel_current_tasks` 从完整持久化幂等账本选取空闲 ID，搜索桥不再自行构造取消 ID；合法业务可以占用任意同形 ID，历史幂等回执不变。队员原案的固定 64 次探测上限会产生伪容量拒绝，领队先以占用 128 个候选的回归证实失败，再改为至多账本条目数加一的探测，保证找到空位，仍受真实操作数/日志/代际上限约束。复用原 CAS、追加/同步后发布与不确定写盘失败关闭，不吞错、不自动重试。回归另覆盖旧业务 ID 导致重建失败、连续取消/重开、真正日志容量拒绝、BeforeWrite/Sync 故障及成功帧恢复确认。队员交付落盘失败部分仅人工恢复必要源码/测试/README，没有集成构建产物。
- P3 排名错误链：`RankingFailure::source` 返回内部 `SearchError`；正式回归在实际取消任务后逐层取得 `Core(TaskCancelled)`，修前失败、修后通过。P3 动画审核哈希誊写：重新核对未集成工作树中的 `animation.rs`，删去记录中多写的一个 `e`，正确 SHA-256 见动画 A1 记录；没有修改动画源码或宣称重新验收。
- 兼容复验：集成时另发现惰性扫描将 WorkingContext 的并列命中切片由末片改成首片；领队以正式回归先确认变化后恢复原规则，LongTerm 并列取首片、WorkingContext 并列取末片。没有借优化改变重排选片语义。
- 内存实测：对当前主目录源码重新编译原公开 API 压力探针，在 Windows 同一高重叠场景两次 `PeakWorkingSet64` 为 14.49/6.75 MiB，`reconcile` 耗时 1787/1795ms；修前约 375.10 MiB、2306ms。零重叠对照为 6.75 MiB、5ms。数据仅代表本机调试构建与该合成场景，内存改善不等于已消除高重叠字符扫描成本，也不是真实模型或完整索引堆测量。
- 实际验证：Rust/Cargo 1.99.0，独立 `target/system-fixes-20261014/clean-build`；定向取消、索引、选片和错误链回归及 workspace fmt、全目标 Clippy `-D warnings`、test、build、doc 均通过。Cargo 使用 `--locked --offline`（fmt 除外），test/doc 设置 `RUSTDOCFLAGS=-D warnings`；另以 `cargo test --workspace --locked --offline -- --list` 单独计数为 944 项，包含 doctest，不累加子进程输出。11 份活动 Markdown 的 UTF-8/路径/锚点/格式门与 `git diff --check` 也通过。验证日志和临时探针只留忽略的 `target/system-fixes-20261014/`；此前队员测试数与历史缓存不作为本轮主目录总数。
- 最终独立复核：审码判官只读审核本批有效源码快照 `c3ce91d`；领队核对关键源码及两份新增集成回归与主目录一致，该快照是系统回放未提交工作区的基线，不是领队新提交。结论为两项 P2 与错误链 P3 修复可接受，授权、版本、取消、健康门、批次租约与追加同步后发布未弱化，不阻塞本批离线范围交付。队员在独立且已忽略的 `target/review-fixes-20261014-c4` 以 Rust/Cargo 1.99.0、`--locked --offline` 实际通过搜索库 68、index_memory 1、cancellation_ids 2、facade 5、本地库 37、回环 online 44，共 157 项，未修改源码或文档。157 项是针对性复验，不能与领队 944 项清单相加；队员未重跑全链门、未复测峰值内存或真实模型质量。保留一项非阻塞 P3 测试覆盖建议：为 `index.rs` 的 `invalid_late_candidate_keeps_a_full_queue_unchanged` 补非空索引前后相等断言，当前实现顺序正确，本批未增加该断言。动画哈希誊写由领队重新哈希核实，不冒称队员另行验收动画。
- 未验证边界：真实模型质量、远端 tokenizer、费用、Unix/断电、物理擦除、生产渠道/GUI 均未验证；诊断日志的累计额度和保守拒绝策略不在本批变更范围，离线全绿不代表完整 RAG 或桌面产品验收。

## UI U1 内核验证（2026-10-14）

- 范围：独立标准库 `moe-ui-core` 接入 workspace，提供类型化注册/面板组合、应用/稳定桌宠作用域、逻辑可见性/焦点、关闭确认、加载票据与有界驱动。中心不依赖记忆、Agent、GUI 或第三方库，业务服务的授权、版本、来源与提交边界不变。
- 缺陷收口：首次核验与部分挂载清理、恢复/隐藏清理失败隔离、已成功资源释放步骤不重复执行、待释放编号中心绑定、陈旧确认及加载票据、真实 key 注入工厂、队列原负载背压、跨注册/实例公平轮转和独立错误链均有针对性回归。普通关闭拒绝尚未处理的输入；隐藏不静默丢弃普通输入；作用域撤权还清除既有失败记录中的敏感缓存。查询不存错误正文，组合错误可按原类型分别检查；错误报告不替代业务回执。
- 主目录实际验证：Rust/Cargo 1.99.0；UI 定向清单 58 项（8 个私有边界单元、45 个无头集成、1 个示例 doctest、4 个 compile-fail），全部通过。随后在独立 `target/ui-u1-20261014/clean-build` 通过 workspace fmt、全目标 Clippy `-D warnings`、test、build 和严格 rustdoc；Cargo 使用 `--locked --offline`（fmt 除外），test/doc 设置 `RUSTDOCFLAGS=-D warnings`。`cargo test --workspace --locked --offline -- --list` 独立计数为 1002 项，不与定向清单或队员结果相加。12 份活动 Markdown 的 UTF-8、路径/锚点、格式门及 `git diff --check` 通过；日志只留忽略的 `target/ui-u1-20261014/`。
- 独立审查复算前版 13 文件摘要 `ebb4f44bd0ad4efa82625890a1a65102c206cf888c075c1ed9ccb369a79a9072` 一致，并定位撤权未清内核加载队列的 P2、失败导航遗漏焦点清除报告的 P3。两项已由领队修复：强制释放在卸载前独立清空本实例待处理负载并调用敏感缓存清除边界，导航报告按实际焦点变化记录，卸载失败不妨碍清理。新增 5 项正式回归；同一测试文件针对保留的前版实际复现 4 项失败，正常隐藏保留普通输入的保护用例通过，修复后 5 项全过。前版反例日志与临时 manifest 只留忽略的 `target/ui-u1-20261014/`。
- 最终只读复核确认撤权 P2、焦点报告 P3 均已修复，U1 可验收、无阻塞意见；独立复跑 58 项 UI 用例通过，旧版反例再次得到 4 失败/1 正常隐藏保护通过，未修改生产源码或文档。该复核没有重跑完整 workspace，全链与 1002 项清单使用领队主目录验证，不与队员定向清单相加。最终源码范围为根 Cargo 配置及 UI crate 的 Cargo、源码、正式测试共 14 文件；按仓库相对路径排序的逐文件 SHA-256 清单（仅 CRLF→LF 归一化）整体 SHA-256 为 `8f0d592b1eee8dcd64a1c199f81951bb5be5b5865f51d663ef7451928709b496`；清单行使用“相对路径:哈希加 LF”，整体按 UTF-8 编码。
- 保留一项非阻塞回归建议：在测试视图的卸载入口断言负载析构及撤权已发生，以防仅检查调用返回后的计数漏检顺序回退；当前源码先清除队列/缓存再卸载，顺序正确，不阻碍 U1 验收。
- 未验证：真实 GUI 像素、鼠标/键盘/系统焦点、实际业务预设面板、日志查看租约、业务诊断采集或真实模型均不在 U1。真实像素的紧急隐藏由宿主负责，`revoke` 是控制器必须兑现的敏感缓存清除边界，不承诺物理擦除或业务回滚。U2 需另行明确宿主/依赖/面板并启动；动画 A2/A3、G6–G8 与 R1/R4 真实门不变。本批保留既有未提交改动，没有自行提交、推送或建分支。

### U1 本轮领队补充复审（2026-10-14）

本节为修前审核快照；两项 P2 的后续修复与验证见本页“UI U1 两项生命周期 P2 修复”，不再将修前结论当作当前验收状态。

- 最新结论：未发现本范围内 P0/P1，但本轮独立反例复现两项生命周期 P2，修复前暂不验收 U1。前述定向用例与早前独立审查通过属于已有覆盖，不代表本轮反例已关闭；无需重做整体架构，也不启动 U2。
- 审核对象：主目录 HEAD `65b8968a0a7d6b4959c2d55f33e3e271abd05557` 上未提交 U1。审核前后 14 文件源码清单保持不变，归一化 SHA-256 仍为 `8f0d592b1eee8dcd64a1c199f81951bb5be5b5865f51d663ef7451928709b496`，与前述最终源码身份一致。本轮只更新本记录与计划，未修改功能代码、合并、提交或推送。
- P2：`open` 只在存活表按 key 查找（`crates/moe-ui-core/src/manager.rs:402`、`crates/moe-ui-core/src/registry.rs:795`），不检查待释放记录。同 key 原实例卸载失败、旧资源尚在时，容量有余便再次调用工厂创建新实例；独立用例实际观察到工厂调用两次、同 key 同时出现在存活与待释放记录中。另在按 PanelKey 组织资源的合成宿主中，重试旧清理删除了新视图的资源，中心却仍报告新实例可见；这是无头资源表反例，不是真实 GUI 故障验收。修复应在创建前保留同 key 的释放占用并明确拒绝重开，释放成功后才允许新建；不应因此阻断其他 key，也不自动丢弃旧资源或重复构造。正式回归须覆盖部分挂载、正常关闭、隐藏清理失败及重试后恢复。
- P2：隐藏仅失效加载序号，已接纳加载负载仍留队列（`crates/moe-ui-core/src/registry.rs:336`）；驱动对隐藏实例直接返回，甚至不丢弃失效结果（`crates/moe-ui-core/src/registry.rs:435`）；普通关闭又按全部队列长度拒绝（`crates/moe-ui-core/src/manager.rs:543`，确认/导航关闭亦有同口径）。独立用例只有一条随后失效的加载结果，无草稿、无普通输入，隐藏后驱动 16 轮仍返回 `CloseBlockedByPendingInputs`。修复只清除确已失效的加载结果，使其不再阻止关闭/占用队列；真实普通输入仍须保留并受关闭保护，不能靠重新显示或伪造撤权/退出原因强制释放兜底。正式回归应覆盖纯失效加载、与普通输入混合、隐藏期间关闭及新加载替代。
- 独立验证：UI 原有 58 项、fmt 与独立构建目录的全目标严格 Clippy、workspace test/build、严格 Rustdoc 的 test/doc、12 份活动 Markdown 及 diff 门均通过；workspace 清单逐条与套件小计均为 1002 项，58 项是其中子集。默认构建目录首轮 Clippy 曾报流程导入错误，未改源码，改用本轮独立构建目录后全链通过；不把该首轮错误归为 UI 缺陷。额外 5 项忽略区探针连续两次为 3 失败/2 通过：三条反例支撑上述两项 P2，普通隐藏输入的保护及卸载前队列析构/撤权顺序均通过。日志、源码清单与探针仅在 `target/ui-u1-audit-20261014`，不计正式用例总数。
- 结构结论：中心、控制器、视图和组合的职责分开；内部安全类型擦除未泄漏为公共反射/可变实例接口，零第三方依赖，未发现测试视图转正或新增通用业务引擎。本轮问题是两处生命周期边界，其中失效加载阻止关闭属于保护范围过宽，不需要增加新的调度器、缓存池或业务状态机。

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
