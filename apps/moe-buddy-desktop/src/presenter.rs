//! 快照到界面模型的纯映射：状态文案、严重度、按钮可用性与边栏内容。
//!
//! 只读取 [`AppSnapshot`] 与契约类型，不触碰 Win32；文字与可用性结论
//! 在此集中测试，宿主只负责呈现。

use moe_chat_app::{
    AppSnapshot, ChatStatus, MessageRole, PanelKind, PanelVisibility, PresetSnapshot,
};

/// 状态条严重度；宿主同时给出形状与文字，不单独依赖颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Busy,
    Warn,
    Error,
}

/// 顶部伙伴信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderModel {
    pub name: &'static str,
    /// 由快照的离线模式与外传许可推导的准确标记。
    pub offline_text: String,
    /// 当前聊天预设的展示名；目录未装载时回退为预设 ID。
    pub preset_label: String,
    pub revoked: bool,
}

/// 底部状态条。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusModel {
    pub text: String,
    pub severity: Severity,
    pub send_enabled: bool,
}

/// 边栏列表行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub primary: String,
    pub secondary: Option<String>,
    pub active: bool,
}

/// 边栏一个面板的展示模型。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelModel {
    pub visible: bool,
    pub rows: Vec<Row>,
    /// 可见但无内容时的空态文案。
    pub empty_text: &'static str,
    /// 面板未打开时的提示文案。
    pub closed_text: &'static str,
}

/// 聊天历史中单条消息的角色标签。
pub fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "你",
        MessageRole::Assistant => "Moe",
    }
}

/// 聊天历史的空态文案。
pub fn chat_empty_text() -> &'static str {
    "今天的心情、突然的灵感，或一件小事。\r\n都可以从这里开始。"
}

/// 发送按钮提示行。
pub fn input_hint_text() -> &'static str {
    "Enter 发送 · Shift+Enter 换行"
}

/// 记忆输入按钮提示。
pub fn memory_hint_text() -> &'static str {
    "仅保存你明确写入的内容；聊天不会自动成为记忆。"
}

/// 边栏页签标题。
pub fn tab_title(tab: SideTab) -> &'static str {
    match tab {
        SideTab::Memory => "记忆",
        SideTab::Presets => "模型",
        SideTab::Logs => "运行",
    }
}

/// 边栏三个入口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideTab {
    Memory,
    Presets,
    Logs,
}

impl SideTab {
    pub const ALL: [SideTab; 3] = [SideTab::Memory, SideTab::Presets, SideTab::Logs];

    pub fn panel_kind(self) -> PanelKind {
        match self {
            SideTab::Memory => PanelKind::Memory,
            SideTab::Presets => PanelKind::Presets,
            SideTab::Logs => PanelKind::Diagnostics,
        }
    }

    fn snapshot_visible(snapshot: &AppSnapshot, tab: SideTab) -> bool {
        snapshot.panels.iter().any(|panel| {
            panel.kind == tab.panel_kind() && panel.visibility == PanelVisibility::Visible
        })
    }
}

/// 快照头部信息：准确的离线标记、活动预设展示名与撤销状态。
pub fn header(snapshot: &AppSnapshot) -> HeaderModel {
    let offline_text = if snapshot.revoked {
        String::from("授权已撤销 · 本进程不再接受操作")
    } else if snapshot.mode == "offline" && !snapshot.external_transmission_allowed {
        String::from("离线演示 · 仅本机")
    } else if snapshot.mode == "offline" {
        String::from("离线内存模式")
    } else if snapshot.mode == "online" {
        String::from(if snapshot.external_transmission_allowed {
            "模型已配置 · 已允许发送"
        } else {
            "模型已配置 · 等待发送许可"
        })
    } else {
        String::from("未知运行模式")
    };
    HeaderModel {
        name: "Moe Buddy",
        preset_label: preset_display_label(snapshot),
        revoked: snapshot.revoked,
        offline_text,
    }
}

/// 底部状态条；结果未知时禁用发送，失败保留草稿可重试。
pub fn status(snapshot: &AppSnapshot) -> StatusModel {
    if snapshot.revoked {
        return StatusModel {
            text: String::from("授权已撤销：面板与缓存已清理，不再接受操作"),
            severity: Severity::Error,
            send_enabled: false,
        };
    }
    match snapshot.chat.status {
        ChatStatus::Ready => StatusModel {
            text: if snapshot.mode == "online" {
                header(snapshot).offline_text
            } else {
                String::from("离线演示 · 未连接模型服务")
            },
            severity: Severity::Info,
            send_enabled: true,
        },
        ChatStatus::Sending => StatusModel {
            text: String::from("发送中…"),
            severity: Severity::Busy,
            send_enabled: false,
        },
        ChatStatus::Failed => StatusModel {
            text: format!(
                "上一条未完成（{}）；输入已保留，可修改后重试",
                failure_text(snapshot.chat.failure.as_deref())
            ),
            severity: Severity::Error,
            send_enabled: true,
        },
        ChatStatus::Unknown => StatusModel {
            text: String::from("上一轮结果未知：请勿重发；重启也不会自动重试"),
            severity: Severity::Warn,
            send_enabled: false,
        },
    }
}

/// 应用通知（诊断历史缺口、撤销确认等）；与状态条分开呈现。
pub fn notice(snapshot: &AppSnapshot) -> Option<String> {
    snapshot.notice.clone()
}

/// 记忆面板：授权检索返回的有界列表。
pub fn memory_panel(snapshot: &AppSnapshot) -> PanelModel {
    let visible = SideTab::snapshot_visible(snapshot, SideTab::Memory);
    PanelModel {
        visible,
        rows: snapshot
            .memories
            .iter()
            .map(|memory| Row {
                id: memory.id.clone(),
                primary: memory.text.clone(),
                secondary: None,
                active: false,
            })
            .collect(),
        empty_text: "还没有可显示的记忆。\r\n可以调整搜索词，\r\n或在下方保存一条新记忆。",
        closed_text: "记忆面板未打开；点击左侧“记忆”，查看你保存的内容。",
    }
}

/// 预设面板：目录 + 当前绑定标记。
pub fn presets_panel(snapshot: &AppSnapshot) -> PanelModel {
    let visible = SideTab::snapshot_visible(snapshot, SideTab::Presets);
    PanelModel {
        visible,
        rows: snapshot
            .presets
            .iter()
            .map(|preset| preset_row(preset, &snapshot.active_preset))
            .collect(),
        empty_text: "预设目录为空。",
        closed_text: "点击左侧“模型”，管理模型与人设。",
    }
}

/// 日志面板：白名单事件的有界一页。
pub fn logs_panel(snapshot: &AppSnapshot) -> PanelModel {
    let visible = SideTab::snapshot_visible(snapshot, SideTab::Logs);
    PanelModel {
        visible,
        rows: snapshot
            .diagnostics
            .iter()
            .map(|entry| Row {
                id: entry.sequence.to_string(),
                primary: format!("#{} {}", entry.sequence, entry.event),
                secondary: Some(format!("{} · {}", entry.run, entry.status)),
                active: false,
            })
            .collect(),
        empty_text: "暂无已获准查看的事件；点击“刷新”查询。",
        closed_text: "点击左侧“运行”，查看运行记录。",
    }
}

/// 面板标题；范围说明放在面板辅助信息中。
pub fn panel_title(tab: SideTab) -> &'static str {
    match tab {
        SideTab::Memory => "私密记忆",
        SideTab::Presets => "模型与人设",
        SideTab::Logs => "运行记录",
    }
}

/// 边栏详情区文本：记忆选中全文或预设详情。
pub fn panel_detail(tab: SideTab, selected: Option<&Row>) -> String {
    match tab {
        SideTab::Memory => match selected {
            Some(_) => String::from("选中后点击“编辑选中”，在输入框查看或修改全文。"),
            None => memory_hint_text().to_owned(),
        },
        SideTab::Presets => match selected {
            Some(row) => format!(
                "{}\r\n提供方：offline-fixture\r\n端点：离线夹具，不发送网络请求",
                row.primary
            ),
            None => String::from("选中一个预设查看详情；切换立即对下一轮生效。"),
        },
        SideTab::Logs => String::from("仅本次运行的记录，不包含聊天正文或凭据。"),
    }
}

/// Enter 键策略：输入法组合中与 Shift+Enter 都不发送。
pub fn enter_sends(ime_composing: bool, shift_down: bool) -> bool {
    !ime_composing && !shift_down
}

fn preset_row(preset: &PresetSnapshot, active_id: &str) -> Row {
    let active = preset.id == active_id;
    Row {
        id: preset.id.clone(),
        primary: if active {
            format!("{}（使用中）", preset.label)
        } else {
            preset.label.clone()
        },
        secondary: Some(preset.model.clone()),
        active,
    }
}

fn preset_display_label(snapshot: &AppSnapshot) -> String {
    if snapshot.active_preset.is_empty() {
        return String::from("未绑定");
    }
    snapshot
        .presets
        .iter()
        .find(|preset| preset.id == snapshot.active_preset)
        .map(|preset| preset.label.clone())
        .unwrap_or_else(|| snapshot.active_preset.clone())
}

fn failure_text(code: Option<&str>) -> &str {
    match code {
        Some("send_failed") => "发送失败",
        Some("send_cancelled") => "已取消",
        Some("send_unknown") => "结果未知",
        Some("conversation_full") => "会话容量已满",
        Some(code) => code,
        None => "原因未提供",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moe_chat_app::{
        ChatMessage, ChatSnapshot, DiagnosticSnapshot, MemorySnapshot, PanelSnapshot,
        SessionSnapshot,
    };

    fn panel(kind: PanelKind, visibility: PanelVisibility) -> PanelSnapshot {
        PanelSnapshot {
            kind,
            visibility,
            confirmation: false,
            mounts: 0,
            renders: 0,
            unmounts: 0,
        }
    }

    fn snapshot() -> AppSnapshot {
        AppSnapshot {
            revision: 1,
            active_session_id: Some("session-1".to_owned()),
            sessions_revision: 1,
            session_isolation_allowed: false,
            sessions: vec![SessionSnapshot {
                id: "session-1".to_owned(),
                label: "会话 1".to_owned(),
                archived: false,
                status: ChatStatus::Ready,
            }],
            mode: "offline",
            companion: "offline-companion".to_owned(),
            active_preset: "offline-echo".to_owned(),
            external_transmission_allowed: false,
            revoked: false,
            panels: vec![
                panel(PanelKind::Chat, PanelVisibility::Visible),
                panel(PanelKind::Presets, PanelVisibility::Closed),
                panel(PanelKind::Memory, PanelVisibility::Closed),
                panel(PanelKind::Diagnostics, PanelVisibility::Closed),
            ],
            chat: ChatSnapshot {
                draft: String::new(),
                messages: Vec::new(),
                status: ChatStatus::Ready,
                failure: None,
            },
            presets: vec![PresetSnapshot {
                id: "offline-echo".to_owned(),
                label: "离线验收·回声预设".to_owned(),
                provider: "offline-fixture".to_owned(),
                model: "offline-verifier-v1".to_owned(),
                endpoint: "https://offline.invalid/v1".to_owned(),
            }],
            memories: Vec::new(),
            diagnostics: Vec::new(),
            notice: None,
        }
    }

    #[test]
    fn offline_marker_reflects_mode_and_transmission_permission() {
        let base = snapshot();
        assert_eq!(header(&base).offline_text, "离线演示 · 仅本机");

        let mut transmitted = snapshot();
        transmitted.external_transmission_allowed = true;
        assert_eq!(header(&transmitted).offline_text, "离线内存模式");

        let mut unknown_mode = snapshot();
        unknown_mode.mode = "elsewhere";
        assert_eq!(header(&unknown_mode).offline_text, "未知运行模式");
    }

    #[test]
    fn header_prefers_preset_label_but_falls_back_to_id() {
        let base = snapshot();
        assert_eq!(header(&base).preset_label, "离线验收·回声预设");

        let mut uncataloged = snapshot();
        uncataloged.presets.clear();
        assert_eq!(header(&uncataloged).preset_label, "offline-echo");

        let mut unbound = snapshot();
        unbound.active_preset.clear();
        assert_eq!(header(&unbound).preset_label, "未绑定");
    }

    #[test]
    fn send_availability_follows_status_and_revocation() {
        let base = snapshot();
        assert!(status(&base).send_enabled);

        let mut sending = snapshot();
        sending.chat.status = ChatStatus::Sending;
        let sending_status = status(&sending);
        assert!(!sending_status.send_enabled);
        assert_eq!(sending_status.severity, Severity::Busy);

        let mut unknown = snapshot();
        unknown.chat.status = ChatStatus::Unknown;
        unknown.chat.failure = Some("send_unknown".to_owned());
        let unknown_status = status(&unknown);
        assert!(!unknown_status.send_enabled);
        assert_eq!(unknown_status.severity, Severity::Warn);
        assert!(unknown_status.text.contains("请勿重发"));

        let mut failed = snapshot();
        failed.chat.status = ChatStatus::Failed;
        failed.chat.failure = Some("send_cancelled".to_owned());
        let failed_status = status(&failed);
        assert!(failed_status.send_enabled);
        assert_eq!(failed_status.severity, Severity::Error);
        assert!(failed_status.text.contains("已取消"));
        assert!(failed_status.text.contains("可修改后重试"));

        let mut revoked = snapshot();
        revoked.revoked = true;
        let revoked_status = status(&revoked);
        assert!(!revoked_status.send_enabled);
        assert_eq!(revoked_status.severity, Severity::Error);
        assert!(header(&revoked).revoked);
    }

    #[test]
    fn failure_text_maps_known_codes_and_keeps_unknown_verbatim() {
        assert_eq!(failure_text(Some("send_failed")), "发送失败");
        assert_eq!(failure_text(Some("send_cancelled")), "已取消");
        assert_eq!(failure_text(Some("conversation_full")), "会话容量已满");
        assert_eq!(failure_text(Some("mystery")), "mystery");
        assert_eq!(failure_text(None), "原因未提供");
    }

    #[test]
    fn panels_report_visibility_and_bounds() {
        let mut open_memory = snapshot();
        open_memory
            .panels
            .push(panel(PanelKind::Memory, PanelVisibility::Visible));
        open_memory.memories.push(MemorySnapshot {
            revision: 1,
            id: "7".to_owned(),
            text: "喜欢安静的背景音".to_owned(),
        });
        let memory = memory_panel(&open_memory);
        assert!(memory.visible);
        assert_eq!(memory.rows.len(), 1);
        assert_eq!(memory.rows[0].primary, "喜欢安静的背景音");

        let closed = memory_panel(&snapshot());
        assert!(!closed.visible);
        assert!(closed.closed_text.contains("未打开"));
        assert_eq!(closed.empty_text, memory.empty_text);

        let mut open_logs = snapshot();
        open_logs
            .panels
            .push(panel(PanelKind::Diagnostics, PanelVisibility::Visible));
        open_logs.diagnostics.push(DiagnosticSnapshot {
            sequence: 3,
            run: "run-1".to_owned(),
            event: "run.stage_started.infer".to_owned(),
            status: "info/in_progress".to_owned(),
        });
        let logs = logs_panel(&open_logs);
        assert!(logs.visible);
        assert_eq!(logs.rows[0].primary, "#3 run.stage_started.infer");
        assert_eq!(
            logs.rows[0].secondary.as_deref(),
            Some("run-1 · info/in_progress")
        );
    }

    #[test]
    fn preset_rows_mark_the_active_binding() {
        let mut catalog = snapshot();
        catalog.presets.push(PresetSnapshot {
            id: "offline-reflect".to_owned(),
            label: "离线验收·回照预设".to_owned(),
            provider: "offline-fixture".to_owned(),
            model: "offline-verifier-v2".to_owned(),
            endpoint: "https://offline.invalid/v1".to_owned(),
        });
        let presets = presets_panel(&catalog);
        assert_eq!(presets.rows.len(), 2);
        assert!(presets.rows[0].active);
        assert!(presets.rows[0].primary.contains("使用中"));
        assert!(!presets.rows[1].active);
        assert_eq!(
            presets.rows[1].secondary.as_deref(),
            Some("offline-verifier-v2")
        );

        let mut rebound = catalog;
        rebound.active_preset = "offline-reflect".to_owned();
        let presets = presets_panel(&rebound);
        assert!(!presets.rows[0].active);
        assert!(!presets.rows[0].primary.contains("使用中"));
        assert!(presets.rows[1].active);
    }

    #[test]
    fn notice_passes_through_for_gap_and_revocation() {
        let mut gap = snapshot();
        gap.notice = Some("诊断日志存在历史缺口，缺失数量未知。".to_owned());
        assert_eq!(
            notice(&gap).as_deref(),
            Some("诊断日志存在历史缺口，缺失数量未知。")
        );
        assert_eq!(notice(&snapshot()), None);
    }

    #[test]
    fn enter_policy_never_sends_during_composition_or_shift() {
        assert!(enter_sends(false, false));
        assert!(!enter_sends(true, false));
        assert!(!enter_sends(false, true));
        assert!(!enter_sends(true, true));
    }

    #[test]
    fn role_labels_are_localized() {
        assert_eq!(role_label(MessageRole::User), "你");
        assert_eq!(role_label(MessageRole::Assistant), "Moe");
    }

    #[test]
    fn panel_detail_prefers_selection_for_memory_and_preset() {
        let row = Row {
            id: "7".to_owned(),
            primary: "喜欢安静的背景音".to_owned(),
            secondary: None,
            active: false,
        };
        let memory_detail = panel_detail(SideTab::Memory, Some(&row));
        assert!(memory_detail.contains("编辑选中"));
        let memory_hint = panel_detail(SideTab::Memory, None);
        assert!(memory_hint.contains("明确写入"));
        assert!(memory_hint.contains("不会自动"));

        let preset_row = Row {
            id: "offline-echo".to_owned(),
            primary: "离线验收·回声预设（使用中）".to_owned(),
            secondary: None,
            active: true,
        };
        let preset_detail = panel_detail(SideTab::Presets, Some(&preset_row));
        assert!(preset_detail.contains("离线验收·回声预设（使用中）"));
        assert!(preset_detail.contains("不发送网络请求"));
        assert!(panel_detail(SideTab::Logs, None).contains("本次运行"));
    }

    #[test]
    fn messages_keep_role_information_available_for_bubbles() {
        let mut chat = snapshot();
        chat.chat.messages.push(ChatMessage {
            role: MessageRole::User,
            text: "你好".to_owned(),
        });
        assert_eq!(chat.chat.messages[0].role, MessageRole::User);
    }
}
