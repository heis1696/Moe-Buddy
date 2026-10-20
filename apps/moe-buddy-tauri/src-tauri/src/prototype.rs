use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc, mpsc::SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use crate::contract::{
    BuddyView, ChatDraftDiscardView, ChatDraftSessionView, ChatDraftView, CloseIntent,
    DiagnosticView, DispatchResult, Failure, FrameSnapshot, ManagementDraftKind,
    ManagementDraftView, ManagementExitView, MemoryView, ModelSettingsView, PresetView,
    SessionView, Snapshot,
};
use moe_agent_runtime::animation::{
    AnimationAction, AnimationClip, AnimationDefinition, AnimationNode, AnimationSpec, Animator,
    ClipCapabilities, ClipPlayer, PlaybackAck, PlaybackAdvance, PlaybackMode, PlaybackPosition,
};
use moe_chat_app::{
    Action, AppSnapshot, ChatApplication, MAX_SESSION_ID_BYTES, ModelSettings, PanelKind,
    PanelVisibility,
};
use moe_desktop_adapters::storage::{BuddyDraftRevision, BuddyStorage};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, tray::TrayIconBuilder};

#[cfg(windows)]
use crate::hit_mask::{apply_hit_mask, poll_cursor_passthrough};

const QUEUE_CAPACITY: usize = 32;
const CLOSE_QUEUE_CAPACITY: usize = 8;
const MAX_COMMAND_BYTES: usize = 32_768;
const MAX_HIT_CELLS: usize = 256 * 256;
const MAX_MANAGEMENT_DRAFT_VERSION: u64 = 9_007_199_254_740_991;
const MAX_PRESENTATION_SEQUENCE: u64 = 9_007_199_254_740_991;
const RETENTION_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);
const TRAY_ID: &str = "moe-buddy-tray";
/// 退出待处理时，先把已经受理的普通命令与关闭命令处理掉，避免越过刚排队的草稿。
const MAX_DRAIN_PER_TURN: usize = QUEUE_CAPACITY + CLOSE_QUEUE_CAPACITY;

use crate::contract::material_descriptor;
use crate::contract::{ChatView, MessageView};
use crate::material::host::{MaterialCommand, MaterialHost, MaterialState};

#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ChatCommand {
    CreateBuddy {
        label: String,
        persona: String,
        expected_management_draft_version: u64,
    },
    SwitchBuddy {
        id: String,
    },
    RenameBuddy {
        label: String,
        expected_management_draft_version: u64,
    },
    NewSession {
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    SwitchSession {
        id: String,
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    ArchiveSession {
        id: String,
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    SetDraft {
        text: String,
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    Send {
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    HideChat,
    CloseChat,
    ConfirmClose {
        discard: bool,
        #[serde(deserialize_with = "deserialize_required_close_intent")]
        expected_intent: Option<CloseIntent>,
    },
    ConfirmManagementDiscard {
        version: u64,
        intent: CloseIntent,
    },
    CancelManagementExit {
        version: u64,
        intent: CloseIntent,
    },
    Revoke,
    OpenPanel {
        panel: String,
    },
    HidePanel {
        panel: String,
    },
    ConfigureModel {
        endpoint: String,
        model: String,
        persona: String,
        allow_transmission: bool,
        #[serde(rename = "apiKey", default)]
        api_key: RequiredNullableString,
    },
    SelectPreset {
        id: String,
    },
    Remember {
        text: String,
    },
    SearchMemories {
        query: String,
    },
    UpdateMemory {
        id: String,
        revision: u64,
        text: String,
    },
    DeleteMemory {
        id: String,
        revision: u64,
    },
    RefreshMemories,
    RefreshDiagnostics,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuddyContext {
    expected_buddy_id: String,
    expected_buddies_revision: u64,
}

#[derive(Clone, Default)]
enum RequiredNullableString {
    #[default]
    Missing,
    Present(Option<String>),
}

impl<'de> Deserialize<'de> for RequiredNullableString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer).map(Self::Present)
    }
}

impl Serialize for RequiredNullableString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Missing => serializer.serialize_none(),
            Self::Present(value) => value.serialize(serializer),
        }
    }
}

impl RequiredNullableString {
    fn into_option(self) -> Result<Option<String>, ()> {
        match self {
            Self::Missing => Err(()),
            Self::Present(value) => Ok(value),
        }
    }
}

enum Request {
    Dispatch(ChatCommand, BuddyContext, mpsc::SyncSender<DispatchResult>),
    HitMask {
        width: u16,
        height: u16,
        alpha: Vec<u8>,
        frame: MaterialFrameTriple,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    MaterialCommand {
        command: MaterialCommand,
        context: BuddyContext,
        reply: mpsc::SyncSender<MaterialReply>,
    },
    MaterialPixels {
        frame: MaterialFrameTriple,
        reply: mpsc::SyncSender<Result<Vec<u8>, String>>,
    },
    MaterialState(mpsc::SyncSender<MaterialState>),
    TogglePause(mpsc::SyncSender<Result<(), String>>),
    Snapshot(mpsc::SyncSender<DispatchResult>),
    ManagementDraftState {
        version: u64,
        dirty: bool,
        kind: ManagementDraftKind,
        context: BuddyContext,
        reply: mpsc::SyncSender<Result<ManagementDraftAck, String>>,
    },
    SetVisibility {
        label: &'static str,
        visible: bool,
        context: Option<BuddyContext>,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MaterialFrameTriple {
    resource_generation: u64,
    playback_generation: u64,
    presentation_sequence: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MaterialReply {
    ok: bool,
    state: MaterialState,
    error: Option<Failure>,
}

/// 关闭/确认走独立的小队列：普通命令队列满时仍能受理关闭确认与取消。
enum CloseRequest {
    CloseChat {
        context: BuddyContext,
        reply: mpsc::SyncSender<DispatchResult>,
    },
    ConfirmClose {
        discard: bool,
        expected_intent: Option<CloseIntent>,
        context: BuddyContext,
        reply: mpsc::SyncSender<DispatchResult>,
    },
    ConfirmManagementDiscard {
        version: u64,
        intent: CloseIntent,
        context: BuddyContext,
        reply: mpsc::SyncSender<DispatchResult>,
    },
    CancelManagementExit {
        version: u64,
        intent: CloseIntent,
        context: BuddyContext,
        reply: mpsc::SyncSender<DispatchResult>,
    },
}

fn deserialize_required_close_intent<'de, D>(
    deserializer: D,
) -> Result<Option<CloseIntent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<CloseIntent>::deserialize(deserializer)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ManagementDraftStatus {
    version: u64,
    dirty: bool,
    revoked: bool,
    discarded_version: Option<u64>,
    kind: ManagementDraftKind,
    context: Option<BuddyContext>,
}

#[derive(Default)]
struct ManagementDraftMailbox {
    status: Mutex<ManagementDraftStatus>,
}

impl ManagementDraftMailbox {
    fn update(
        &self,
        version: u64,
        dirty: bool,
        kind: ManagementDraftKind,
        context: BuddyContext,
    ) -> Result<ManagementDraftStatus, &'static str> {
        if version > MAX_MANAGEMENT_DRAFT_VERSION {
            return Err("management draft version exceeds the safe integer limit");
        }
        if dirty == (kind == ManagementDraftKind::None) {
            return Err("management draft kind does not match its dirty state");
        }
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if status
            .context
            .as_ref()
            .is_some_and(|current| current != &context)
            && (status.dirty || version <= status.version)
        {
            return Err("management draft context is stale");
        }
        if status.revoked {
            if version > status.version {
                status.version = version;
                status.discarded_version = None;
                status.kind = kind;
                status.context = Some(context);
            }
            return Ok(status.clone());
        }
        if version < status.version {
            return Ok(status.clone());
        }
        if version == status.version {
            if status.dirty != dirty || status.kind != kind {
                return Err("management draft version was reused with different state");
            }
            return Ok(status.clone());
        }
        status.version = version;
        status.dirty = dirty;
        status.kind = kind;
        status.context = Some(context);
        status.discarded_version = None;
        Ok(status.clone())
    }

    fn current(&self) -> ManagementDraftStatus {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn with_version<T>(
        &self,
        version: u64,
        action: impl FnOnce(&mut ManagementDraftStatus) -> T,
    ) -> Result<T, ManagementDraftStatus> {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if status.version != version {
            return Err(status.clone());
        }
        Ok(action(&mut status))
    }

    fn revoke(&self) -> ManagementDraftStatus {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        status.revoked = true;
        status.dirty = false;
        status.kind = ManagementDraftKind::None;
        status.discarded_version = None;
        status.clone()
    }

    fn confirm_discard(
        &self,
        version: u64,
    ) -> Result<ManagementDraftStatus, ManagementDraftStatus> {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if status.version != version {
            return Err(status.clone());
        }
        if status.dirty && !status.revoked {
            status.dirty = false;
            status.kind = ManagementDraftKind::None;
            status.discarded_version = Some(version);
        }
        Ok(status.clone())
    }

    fn rebind(
        &self,
        context: BuddyContext,
    ) -> Result<ManagementDraftStatus, ManagementDraftStatus> {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if status.dirty {
            return Err(status.clone());
        }
        status.context = Some(context);
        status.kind = ManagementDraftKind::None;
        status.discarded_version = None;
        Ok(status.clone())
    }

    fn coordinate_commit(
        &self,
        from: &BuddyContext,
        to: BuddyContext,
    ) -> Result<ManagementDraftStatus, ManagementDraftStatus> {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if status.context.as_ref() != Some(from) || status.revoked {
            return Err(status.clone());
        }
        status.context = Some(to);
        Ok(status.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitIntent {
    CloseChat,
    QuitApplication,
}

impl ExitIntent {
    /// 退出强于只关闭聊天；后来的关闭请求不能把已受理的退出降级。
    fn precedence(self, other: Self) -> Self {
        if self == Self::QuitApplication || other == Self::QuitApplication {
            Self::QuitApplication
        } else {
            Self::CloseChat
        }
    }
}

fn close_intent(intent: ExitIntent) -> CloseIntent {
    match intent {
        ExitIntent::CloseChat => CloseIntent::CloseChat,
        ExitIntent::QuitApplication => CloseIntent::QuitApplication,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExitRequest {
    id: u64,
    intent: ExitIntent,
    buddy_context: Option<BuddyContext>,
}

#[derive(Default)]
struct ExitMailboxState {
    next_id: u64,
    pending: Option<ExitRequest>,
}

/// 唯一的退出/原生关闭入口。槽位独立于普通命令队列，只保留最新意图，
/// 因此队列满时退出请求不会丢失，也不会被之后的普通聊天命令覆盖。
#[derive(Default)]
struct ExitMailbox {
    state: Mutex<ExitMailboxState>,
}

impl ExitMailbox {
    fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn submit(&self, intent: ExitIntent) -> ExitRequest {
        self.submit_with_context(intent, None)
    }

    fn submit_with_context(
        &self,
        intent: ExitIntent,
        buddy_context: Option<BuddyContext>,
    ) -> ExitRequest {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.next_id = state.next_id.saturating_add(1).max(1);
        let request = ExitRequest {
            id: state.next_id,
            intent,
            buddy_context,
        };
        let (intent, buddy_context) =
            state
                .pending
                .as_ref()
                .map_or((intent, request.buddy_context.clone()), |pending| {
                    (
                        pending.intent.precedence(intent),
                        request
                            .buddy_context
                            .clone()
                            .or_else(|| pending.buddy_context.clone()),
                    )
                });
        let request = ExitRequest {
            intent,
            buddy_context,
            ..request
        };
        state.pending = Some(request.clone());
        request
    }

    fn take(&self) -> Option<ExitRequest> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .take()
    }

    fn has_pending(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .is_some()
    }
}

struct HostState {
    sender: SyncSender<Request>,
    close_sender: SyncSender<CloseRequest>,
    latest: Arc<Mutex<Snapshot>>,
    frame: Arc<Mutex<FrameSnapshot>>,
    exit: Arc<ExitMailbox>,
}

fn project(app: &ChatApplication) -> Snapshot {
    let snapshot = app.snapshot();
    let chat_draft = app.chat_draft_snapshot();
    Snapshot {
        revision: snapshot.revision,
        active_buddy_id: Some(snapshot.companion.clone()),
        buddies_revision: 1,
        buddies: vec![BuddyView {
            id: snapshot.companion,
            label: "离线验收伙伴".to_owned(),
            profile_revision: 1,
            persona_revision: 1,
            model_revision: 1,
        }],
        active_buddy_persona: None,
        buddy_commit: None,
        active_session_id: snapshot.active_session_id,
        sessions_revision: snapshot.sessions_revision,
        session_isolation_allowed: snapshot.session_isolation_allowed,
        sessions: snapshot
            .sessions
            .into_iter()
            .map(|session| SessionView {
                id: session.id,
                label: session.label,
                archived: session.archived,
                status: session.status.as_str().to_owned(),
            })
            .collect(),
        presentation_revision: snapshot.revision,
        mode: snapshot.mode.to_owned(),
        revoked: snapshot.revoked,
        chat: ChatView {
            draft: snapshot.chat.draft,
            messages: snapshot
                .chat
                .messages
                .into_iter()
                .map(|message| MessageView {
                    role: message.role.as_str().to_owned(),
                    text: message.text,
                })
                .collect(),
            status: snapshot.chat.status.as_str().to_owned(),
            failure: snapshot.chat.failure,
        },
        close_confirmation: snapshot
            .panels
            .iter()
            .any(|panel| panel.kind == PanelKind::Chat && panel.confirmation),
        close_intent: None,
        management_draft: ManagementDraftView {
            version: 0,
            dirty: false,
            kind: ManagementDraftKind::None,
        },
        management_exit: None,
        management_discarded_version: None,
        chat_draft: ChatDraftView {
            revision: chat_draft.revision,
            discard: chat_draft.discard.map(|discard| ChatDraftDiscardView {
                revision: discard.revision,
                session_ids: discard.session_ids,
            }),
            sessions: chat_draft
                .sessions
                .into_iter()
                .map(|session| ChatDraftSessionView {
                    id: session.id,
                    revision: session.revision,
                    discarded_revision: session.discarded_revision,
                })
                .collect(),
        },
        active_preset: snapshot.active_preset,
        external_transmission_allowed: snapshot.external_transmission_allowed,
        presets: snapshot
            .presets
            .into_iter()
            .map(|preset| PresetView {
                id: preset.id,
                label: preset.label,
                provider: preset.provider,
                model: preset.model,
                endpoint: preset.endpoint,
            })
            .collect(),
        memories: snapshot
            .memories
            .into_iter()
            .map(|memory| MemoryView {
                id: memory.id,
                revision: memory.revision,
                text: memory.text,
            })
            .collect(),
        diagnostics: snapshot
            .diagnostics
            .into_iter()
            .map(|event| DiagnosticView {
                sequence: event.sequence,
                run: event.run,
                event: event.event,
                status: event.status,
            })
            .collect(),
        notice: snapshot.notice,
        model_settings: None,
        persistence_enabled: false,
    }
}

fn project_storage(storage: &BuddyStorage) -> Result<Snapshot, String> {
    let registry = storage
        .registry_snapshot()
        .map_err(|error| error.to_string())?;
    let active_id = storage
        .active_buddy_id()
        .map_err(|error| error.to_string())?
        .to_owned();
    let profile = storage
        .buddy_profile(&active_id)
        .map_err(|error| error.to_string())?;
    let app = storage
        .active_application()
        .map_err(|error| error.to_string())?;
    let mut projected = project(app);
    projected.active_buddy_id = Some(active_id.clone());
    projected.buddies_revision = registry.revision;
    projected.buddies = registry
        .buddies
        .into_iter()
        .map(|buddy| BuddyView {
            id: buddy.id,
            label: buddy.label,
            profile_revision: buddy.profile_revision,
            persona_revision: buddy.persona_revision,
            model_revision: buddy.model_revision,
        })
        .collect();
    projected.active_buddy_persona = Some(profile.persona().to_owned());
    if projected.revoked {
        projected.active_buddy_id = None;
        projected.buddies.clear();
        projected.active_buddy_persona = None;
    }
    Ok(projected)
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        revision: 0,
        active_buddy_id: Some("offline-companion".into()),
        buddies_revision: 1,
        buddies: vec![BuddyView {
            id: "offline-companion".into(),
            label: "离线验收伙伴".into(),
            profile_revision: 1,
            persona_revision: 1,
            model_revision: 1,
        }],
        active_buddy_persona: None,
        buddy_commit: None,
        active_session_id: Some("session-1".into()),
        sessions_revision: 1,
        session_isolation_allowed: false,
        sessions: vec![SessionView {
            id: "session-1".into(),
            label: "会话 1".into(),
            archived: false,
            status: "ready".into(),
        }],
        presentation_revision: 0,
        mode: "offline".into(),
        revoked: false,
        chat: ChatView {
            draft: String::new(),
            messages: Vec::new(),
            status: "ready".into(),
            failure: None,
        },
        close_confirmation: false,
        close_intent: None,
        management_draft: ManagementDraftView {
            version: 0,
            dirty: false,
            kind: ManagementDraftKind::None,
        },
        management_exit: None,
        management_discarded_version: None,
        chat_draft: ChatDraftView {
            revision: 1,
            discard: None,
            sessions: Vec::new(),
        },
        active_preset: "offline-echo".into(),
        external_transmission_allowed: false,
        presets: Vec::new(),
        memories: Vec::new(),
        diagnostics: Vec::new(),
        notice: None,
        model_settings: None,
        persistence_enabled: false,
    }
}

fn decorate_snapshot(
    snapshot: &mut Snapshot,
    settings: Option<&ModelSettings>,
    has_api_key: bool,
    persistence_enabled: bool,
    host_notice: Option<String>,
) {
    snapshot.model_settings = settings.map(|settings| ModelSettingsView {
        endpoint: settings.endpoint.clone(),
        model: settings.model.clone(),
        persona: settings.persona.clone(),
        allow_transmission: settings.allow_transmission,
        has_api_key,
    });
    snapshot.persistence_enabled = persistence_enabled;
    if host_notice.is_some() {
        snapshot.notice = host_notice;
    }
}

fn chat_visibility(snapshot: &AppSnapshot) -> PanelVisibility {
    snapshot
        .panels
        .iter()
        .find(|panel| panel.kind == PanelKind::Chat)
        .map(|panel| panel.visibility)
        .unwrap_or(PanelVisibility::Closed)
}

/// 宿主的窗口/事件效果抽象。生产实现直接调用 Tauri；测试实现记录受控效果，
/// 让协调逻辑可以沿生产入口回归，而不必在测试里重新实现一遍业务规则。
trait HostPort {
    fn show(&mut self, label: &str) -> Result<(), String>;
    fn hide(&mut self, label: &str) -> Result<(), String>;
    fn focus(&mut self, label: &str) -> Result<(), String>;
    fn unminimize(&mut self, label: &str) -> Result<(), String>;
    fn is_visible(&mut self, label: &str) -> Result<bool, String>;
    fn emit_snapshot(&mut self, snapshot: &Snapshot) -> Result<(), String>;
    fn emit_frame(&mut self, frame: FrameSnapshot) -> Result<(), String>;
    fn emit_material(&mut self, _state: MaterialState) -> Result<(), String> {
        Ok(())
    }
    fn apply_hit_mask(&mut self, width: u16, height: u16, alpha: &[u8]) -> Result<(), String>;
    fn poll_hit_mask(&mut self, width: u16, height: u16, alpha: &[u8]);
    fn notify_error(&mut self, code: &str);
    fn request_exit(&mut self, code: i32);
}

fn feedback_message(code: &str) -> &'static str {
    match code {
        "animation_progress_failed" => return "动画进度读取失败，已暂停播放。",
        "animation_progress_out_of_bounds" => return "动画进度超出素材范围，已暂停播放。",
        "animation_material_invalid" => return "动画素材描述无效，已暂停播放。",
        _ => {}
    }
    match code {
        "window_show_failed" => "无法显示聊天窗口，请从托盘重试。",
        "window_hide_failed" => "无法隐藏聊天窗口，请从托盘重试。",
        "animation_advance_failed" => "动画推进失败，已暂停播放。",
        "animation_pause_failed" => "动画暂停切换失败。",
        "animation_release_failed" => "动画资源未能正常释放，退出可能不完整。",
        "snapshot_emit_failed" => "桌面状态未能推送到界面。",
        "exit_failed" => "关闭或退出请求未能完成，窗口已保留。",
        "exit_request_failed" => "退出请求未能送达桌面宿主。",
        "tray_command_failed" => "托盘命令未送达桌面宿主，请稍后重试。",
        "host_state_unavailable" => "桌面宿主状态不可用。",
        _ => "桌面宿主操作失败。",
    }
}

fn material_descriptor_is_valid(descriptor: &crate::contract::MaterialDescriptor) -> bool {
    descriptor.frame_count > 0
        && descriptor.frame_duration_ms > 0
        && descriptor
            .frame_duration_ms
            .checked_mul(u64::from(descriptor.frame_count))
            == Some(descriptor.clip_duration_ms)
        && descriptor.clip_duration_ms > 0
}

fn frame_index_for_progress(
    elapsed_ms: Option<u64>,
    descriptor: &crate::contract::MaterialDescriptor,
) -> Result<Option<u8>, ()> {
    if !material_descriptor_is_valid(descriptor) {
        return Err(());
    }
    match elapsed_ms {
        None => Ok(None),
        Some(elapsed_ms) if elapsed_ms < descriptor.clip_duration_ms => {
            Ok(Some((elapsed_ms / descriptor.frame_duration_ms) as u8))
        }
        Some(_) => Err(()),
    }
}

struct TauriPort {
    handle: AppHandle,
}

impl TauriPort {
    fn new(handle: AppHandle) -> Self {
        Self { handle }
    }

    fn window(&self, label: &str) -> Result<tauri::WebviewWindow, String> {
        self.handle
            .get_webview_window(label)
            .ok_or_else(|| "window unavailable".to_owned())
    }
}

impl HostPort for TauriPort {
    fn show(&mut self, label: &str) -> Result<(), String> {
        self.window(label)?
            .show()
            .map_err(|_| "window show failed".to_owned())
    }

    fn hide(&mut self, label: &str) -> Result<(), String> {
        self.window(label)?
            .hide()
            .map_err(|_| "window hide failed".to_owned())
    }

    fn focus(&mut self, label: &str) -> Result<(), String> {
        self.window(label)?
            .set_focus()
            .map_err(|_| "window focus failed".to_owned())
    }

    fn unminimize(&mut self, label: &str) -> Result<(), String> {
        self.window(label)?
            .unminimize()
            .map_err(|_| "window restore failed".to_owned())
    }

    fn is_visible(&mut self, label: &str) -> Result<bool, String> {
        self.window(label)?
            .is_visible()
            .map_err(|_| "window visibility query failed".to_owned())
    }

    fn emit_snapshot(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        self.handle
            .emit_to(
                "main",
                "prototype://snapshot",
                DispatchResult {
                    snapshot: Some(snapshot.clone()),
                    error: None,
                },
            )
            .map_err(|_| "snapshot event delivery failed".to_owned())
    }

    fn emit_frame(&mut self, frame: FrameSnapshot) -> Result<(), String> {
        for label in ["main", "pet"] {
            if let Some(window) = self.handle.get_webview_window(label) {
                window
                    .emit("prototype://frame", frame.clone())
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    fn emit_material(&mut self, state: MaterialState) -> Result<(), String> {
        for label in ["main", "pet"] {
            if let Some(window) = self.handle.get_webview_window(label) {
                window
                    .emit("prototype://material", state.clone())
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    fn apply_hit_mask(&mut self, width: u16, height: u16, alpha: &[u8]) -> Result<(), String> {
        #[cfg(windows)]
        {
            apply_hit_mask(&self.handle, width, height, alpha)
        }
        #[cfg(not(windows))]
        {
            let _ = (width, height, alpha);
            Err("alpha hit masks are supported only by the Windows host".to_owned())
        }
    }

    fn poll_hit_mask(&mut self, width: u16, height: u16, alpha: &[u8]) {
        #[cfg(windows)]
        poll_cursor_passthrough(&self.handle, width, height, alpha);
        #[cfg(not(windows))]
        {
            let _ = (width, height, alpha);
        }
    }

    fn notify_error(&mut self, code: &str) {
        eprintln!("moe-buddy-tauri: {code}");
        if let Some(tray) = self.handle.tray_by_id(TRAY_ID) {
            // 托盘提示只使用固定安全文案，不回显宿主原始错误。
            let tooltip = format!("Moe Buddy：{}", feedback_message(code));
            if let Err(error) = tray.set_tooltip(Some(tooltip)) {
                eprintln!("moe-buddy-tauri: unable to update tray feedback: {error}");
            }
        }
    }

    fn request_exit(&mut self, code: i32) {
        self.handle.exit(code);
    }
}

#[derive(Clone)]
struct PendingExit {
    intent: ExitIntent,
    request_id: u64,
    buddy_context: BuddyContext,
    chat_draft_revisions: Vec<BuddyDraftRevision>,
    management_draft_version: u64,
    management_discard_confirmed_version: Option<u64>,
    host_confirmation: bool,
}

struct HostWorkerContext {
    #[cfg(test)]
    model_settings: Option<ModelSettings>,
    persistence_enabled: bool,
    management_drafts: Arc<ManagementDraftMailbox>,
    epoch_start_ms: u64,
    latest: Arc<Mutex<Snapshot>>,
    frame: Arc<Mutex<FrameSnapshot>>,
}

/// 宿主协调器：业务状态、关闭确认、退出意图和受控窗口效果只看这一处。
/// 普通聊天命令、UI 关闭、原生关闭与托盘退出都经由它的受控入口。
struct HostWorker<P: HostPort> {
    app: Option<ChatApplication>,
    buddy_storage: Option<BuddyStorage>,
    animator: Option<Animator<FourFramePlayer>>,
    #[cfg(test)]
    player: FourFramePlayer,
    port: P,
    latest: Arc<Mutex<Snapshot>>,
    frame: Arc<Mutex<FrameSnapshot>>,
    material_host: MaterialHost,
    material_load: Option<MaterialLoadTask>,
    material_backend_error: Option<Box<dyn std::error::Error>>,
    pending_exit: Option<PendingExit>,
    management_drafts: Arc<ManagementDraftMailbox>,
    management_status: ManagementDraftStatus,
    management_exit_version: Option<u64>,
    management_discarded_version: Option<u64>,
    buddy_commit: Option<crate::contract::BuddyCommitView>,
    shutdown_code: Option<i32>,
    last_frame: Option<FrameSnapshot>,
    presentation_sequence: u64,
    hit_mask: Option<(u16, u16, Vec<u8>, MaterialFrameTriple)>,
    hit_mask_clear_failed: bool,
    animation_faulted: bool,
    animation_error_reported: bool,
    model_settings: Option<ModelSettings>,
    model_key: Option<String>,
    buddy_runtime: BTreeMap<String, BuddyRuntimeAuthorization>,
    model_requests: BTreeMap<String, PendingExternalRequest>,
    persistence_enabled: bool,
    host_notice: Option<String>,
    retention_notice: Option<String>,
    last_retention_maintenance: Instant,
    epoch_start_ms: u64,
    started: Instant,
}

#[derive(Clone, Default)]
struct BuddyRuntimeAuthorization {
    api_key: Option<String>,
    allow_transmission: bool,
}

struct PendingExternalRequest {
    receiver: mpsc::Receiver<moe_desktop_adapters::transport::Reply>,
    ticket: moe_chat_app::ExternalRequestTicket,
}

struct MaterialLoadTask {
    context: BuddyContext,
    resource_generation: u64,
    playback_generation: u64,
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<crate::material::MaterialPack, crate::material::MaterialError>>,
    worker: Option<thread::JoinHandle<()>>,
    pending: Option<Result<crate::material::MaterialPack, crate::material::MaterialError>>,
}

fn buddy_registry_failure(error: &moe_chat_app::BuddyError) -> (String, String, &'static str) {
    let (code, message) = match error {
        moe_chat_app::BuddyError::InvalidId => ("buddy_id_invalid", "伙伴 ID 无效"),
        moe_chat_app::BuddyError::InvalidLabel => ("buddy_label_invalid", "伙伴名称无效"),
        moe_chat_app::BuddyError::InvalidPersona => {
            ("invalid_persona", "人设不能为空且最多 4096 字")
        }
        moe_chat_app::BuddyError::InvalidConfiguration => ("model_configuration", "模型配置无效"),
        moe_chat_app::BuddyError::InvalidRegistry => ("buddy_registry_invalid", "伙伴档案无效"),
        moe_chat_app::BuddyError::DuplicateId => ("buddy_id_duplicate", "伙伴 ID 已存在"),
        moe_chat_app::BuddyError::NotFound => ("buddy_not_found", "伙伴档案不存在"),
        moe_chat_app::BuddyError::StaleRegistry => {
            ("buddy_context_stale", "伙伴选择已变化，本次操作未执行")
        }
        moe_chat_app::BuddyError::StaleProfile => {
            ("buddy_profile_stale", "伙伴档案版本已变化，本次操作未执行")
        }
        moe_chat_app::BuddyError::CapacityExceeded => ("buddies_full", "伙伴档案容量已满"),
        moe_chat_app::BuddyError::RevisionExhausted => {
            ("buddy_revision_exhausted", "伙伴修订号已耗尽")
        }
    };
    (code.to_owned(), message.to_owned(), "not_executed")
}

impl<P: HostPort> HostWorker<P> {
    #[cfg(test)]
    fn with_application(port: P, app: ChatApplication, context: HostWorkerContext) -> Self {
        let HostWorkerContext {
            model_settings,
            persistence_enabled,
            management_drafts,
            epoch_start_ms,
            latest,
            frame,
        } = context;
        let (mut animator, _player) = create_animator();
        animator
            .request_action("idle")
            .expect("built-in animation initialization");
        let mut snapshot = project(&app);
        decorate_snapshot(
            &mut snapshot,
            model_settings.as_ref(),
            false,
            persistence_enabled,
            None,
        );
        let management_context = BuddyContext {
            expected_buddy_id: snapshot.active_buddy_id.clone().unwrap_or_default(),
            expected_buddies_revision: snapshot.buddies_revision,
        };
        let management_status = management_drafts
            .rebind(management_context)
            .unwrap_or_else(|status| status);
        snapshot.management_draft = ManagementDraftView {
            version: management_status.version,
            dirty: management_status.dirty,
            kind: management_status.kind,
        };
        *latest.lock().expect("snapshot mutex poisoned") = snapshot;
        Self {
            app: Some(app),
            buddy_storage: None,
            animator: Some(animator),
            #[cfg(test)]
            player: _player,
            port,
            latest,
            frame,
            material_host: MaterialHost::default(),
            material_load: None,
            material_backend_error: None,
            pending_exit: None,
            management_drafts,
            management_status: management_status.clone(),
            management_exit_version: None,
            management_discarded_version: management_status.discarded_version,
            buddy_commit: None,
            shutdown_code: None,
            last_frame: None,
            presentation_sequence: 1,
            hit_mask: None,
            hit_mask_clear_failed: false,
            animation_faulted: false,
            animation_error_reported: false,
            model_settings,
            model_key: None,
            buddy_runtime: BTreeMap::new(),
            model_requests: BTreeMap::new(),
            persistence_enabled,
            host_notice: None,
            retention_notice: None,
            last_retention_maintenance: Instant::now(),
            epoch_start_ms,
            started: Instant::now(),
        }
    }

    fn with_buddy_storage(
        port: P,
        buddy_storage: BuddyStorage,
        context: HostWorkerContext,
    ) -> Result<Self, String> {
        let HostWorkerContext {
            persistence_enabled,
            management_drafts,
            epoch_start_ms,
            latest,
            frame,
            ..
        } = context;
        let active_id = buddy_storage
            .active_buddy_id()
            .map_err(|error| error.to_string())?
            .to_owned();
        let profile = buddy_storage
            .buddy_profile(&active_id)
            .map_err(|error| error.to_string())?;
        let settings = profile
            .model()
            .map(|model| model.to_model_settings(profile.persona()));
        let buddy_context = BuddyContext {
            expected_buddy_id: active_id.clone(),
            expected_buddies_revision: buddy_storage
                .registry_snapshot()
                .map_err(|error| error.to_string())?
                .revision,
        };
        let mut snapshot = project_storage(&buddy_storage)?;
        decorate_snapshot(
            &mut snapshot,
            settings.as_ref(),
            false,
            persistence_enabled,
            None,
        );
        let existing_management_status = management_drafts.current();
        let management_status = if existing_management_status.dirty {
            if existing_management_status.context.as_ref() != Some(&buddy_context) {
                return Err("management draft is dirty during host initialization".to_owned());
            }
            existing_management_status
        } else {
            management_drafts
                .rebind(buddy_context)
                .map_err(|_| "management draft is dirty during host initialization".to_owned())?
        };
        snapshot.management_draft = ManagementDraftView {
            version: management_status.version,
            dirty: management_status.dirty,
            kind: management_status.kind,
        };
        *latest
            .lock()
            .map_err(|_| "snapshot mutex poisoned".to_owned())? = snapshot;
        let (mut animator, _player) = create_animator();
        animator
            .request_action("idle")
            .expect("built-in animation initialization");
        let mut buddy_runtime = BTreeMap::new();
        buddy_runtime.insert(active_id, BuddyRuntimeAuthorization::default());
        let worker = Self {
            app: None,
            buddy_storage: Some(buddy_storage),
            animator: Some(animator),
            #[cfg(test)]
            player: _player,
            port,
            latest,
            frame,
            material_host: MaterialHost::default(),
            material_load: None,
            material_backend_error: None,
            pending_exit: None,
            management_drafts,
            management_status,
            management_exit_version: None,
            management_discarded_version: None,
            buddy_commit: None,
            shutdown_code: None,
            last_frame: None,
            presentation_sequence: 1,
            hit_mask: None,
            hit_mask_clear_failed: false,
            animation_faulted: false,
            animation_error_reported: false,
            model_settings: settings,
            model_key: None,
            buddy_runtime,
            model_requests: BTreeMap::new(),
            persistence_enabled,
            host_notice: None,
            retention_notice: None,
            last_retention_maintenance: Instant::now(),
            epoch_start_ms,
            started: Instant::now(),
        };
        Ok(worker)
    }

    fn now_ms(&self) -> u64 {
        self.epoch_start_ms
            .saturating_add(self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
    }

    fn maintain_retention_if_due(&mut self) {
        if !self.persistence_enabled
            || self.last_retention_maintenance.elapsed() < RETENTION_MAINTENANCE_INTERVAL
        {
            return;
        }
        self.last_retention_maintenance = Instant::now();
        let now_ms = self.now_ms();
        self.retention_notice = match self.app_mut() {
            Err(_) => Some("原文留存状态无法读取，写入已封闭".into()),
            Ok(app) => match app.maintain_retention(now_ms) {
                Ok(Some(report)) if report.cold_blocked.is_some() => {
                    Some(if report.live.pending_transfer > 0 {
                        format!(
                            "原文维护因存储容量不足停止；已处理 {} 个冷段、确认 {} 笔提交，尚有 {} 个待处理；另有 {} 项等待回收区容量",
                            report.cold_segments_processed,
                            report.receipts.raw.len(),
                            report.cold_segments_remaining,
                            report.live.pending_transfer
                        )
                    } else {
                        format!(
                            "原文维护因存储容量不足停止；已处理 {} 个冷段、确认 {} 笔提交，尚有 {} 个待处理",
                            report.cold_segments_processed,
                            report.receipts.raw.len(),
                            report.cold_segments_remaining
                        )
                    })
                }
                Ok(Some(report)) if report.live.pending_transfer > 0 => {
                    Some("原文留存维护遇到回收区容量背压，未转移内容仍被保留".into())
                }
                Ok(Some(report)) if report.cold_segments_remaining > 0 => Some(format!(
                    "原文维护本轮处理 {} 个冷段，尚有 {} 个冷段待处理",
                    report.cold_segments_processed, report.cold_segments_remaining
                )),
                Ok(_) => None,
                Err(_) => Some("原文留存维护失败，写入已封闭".into()),
            },
        };
        self.publish_current_snapshot();
    }

    fn combined_host_notice(&self) -> Option<String> {
        match (&self.host_notice, &self.retention_notice) {
            (Some(host), Some(retention)) => Some(format!("{host}；{retention}")),
            (Some(notice), None) | (None, Some(notice)) => Some(notice.clone()),
            (None, None) => None,
        }
    }

    fn current_projection(&self) -> Result<Snapshot, String> {
        if let Some(storage) = self.buddy_storage.as_ref() {
            project_storage(storage)
        } else {
            self.app
                .as_ref()
                .map(project)
                .ok_or_else(|| "chat host released".to_owned())
        }
    }

    fn app_mut(&mut self) -> Result<&mut ChatApplication, String> {
        if let Some(storage) = self.buddy_storage.as_mut() {
            storage
                .active_application_mut()
                .map_err(|error| error.to_string())
        } else {
            self.app
                .as_mut()
                .ok_or_else(|| "chat host released".to_owned())
        }
    }

    fn app_snapshot(&self) -> Result<AppSnapshot, String> {
        if let Some(storage) = self.buddy_storage.as_ref() {
            storage
                .active_application()
                .map(|app| app.snapshot())
                .map_err(|error| error.to_string())
        } else {
            self.app
                .as_ref()
                .map(ChatApplication::snapshot)
                .ok_or_else(|| "chat host released".to_owned())
        }
    }

    fn active_context(&self) -> Result<BuddyContext, String> {
        if let Some(storage) = self.buddy_storage.as_ref() {
            let registry = storage.registry_snapshot();
            if registry.is_err() && storage.registry_is_unknown() {
                let snapshot = self.latest_snapshot();
                return Ok(BuddyContext {
                    expected_buddy_id: snapshot.active_buddy_id.unwrap_or_default(),
                    expected_buddies_revision: snapshot.buddies_revision,
                });
            }
            let registry = registry.map_err(|error| error.to_string())?;
            let expected_buddy_id = storage
                .active_buddy_id()
                .map_err(|error| error.to_string())?
                .to_owned();
            Ok(BuddyContext {
                expected_buddy_id,
                expected_buddies_revision: registry.revision,
            })
        } else {
            let snapshot = self.latest_snapshot();
            Ok(BuddyContext {
                expected_buddy_id: snapshot.active_buddy_id.unwrap_or_default(),
                expected_buddies_revision: snapshot.buddies_revision,
            })
        }
    }

    fn sync_selected_buddy_runtime(&mut self) -> Result<BuddyContext, String> {
        let (context, profile) = {
            let storage = self
                .buddy_storage
                .as_ref()
                .ok_or_else(|| "伙伴存储尚未就绪".to_owned())?;
            let registry = storage
                .registry_snapshot()
                .map_err(|error| error.to_string())?;
            let buddy_id = storage
                .active_buddy_id()
                .map_err(|error| error.to_string())?
                .to_owned();
            let profile = storage
                .buddy_profile(&buddy_id)
                .map_err(|error| error.to_string())?;
            (
                BuddyContext {
                    expected_buddy_id: buddy_id,
                    expected_buddies_revision: registry.revision,
                },
                profile,
            )
        };
        let runtime = self
            .buddy_runtime
            .entry(context.expected_buddy_id.clone())
            .or_default()
            .clone();
        self.model_settings = profile.model().map(|model| {
            let mut settings = model.to_model_settings(profile.persona());
            settings.allow_transmission = runtime.allow_transmission;
            settings
        });
        self.model_key = runtime.api_key;
        Ok(context)
    }

    fn context_failure(&self, context: &BuddyContext) -> Option<Failure> {
        if self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown)
        {
            return Some(failure_with_disposition(
                "buddy_storage_unknown",
                "伙伴档案状态未知；普通业务已冻结",
                "unknown",
            ));
        }
        match self.active_context() {
            Ok(current) if current == *context => None,
            Ok(_) => Some(failure(
                "buddy_context_stale",
                "伙伴选择已变化，本次操作未执行",
            )),
            Err(message) => Some(failure_with_disposition(
                "buddy_storage_unknown",
                &message,
                "unknown",
            )),
        }
    }

    fn window_context_failure(&self, context: &BuddyContext) -> Option<Failure> {
        if context.expected_buddy_id.is_empty()
            && self.latest_snapshot().revoked
            && self.active_context().is_ok_and(|current| {
                current.expected_buddies_revision == context.expected_buddies_revision
            })
        {
            return None;
        }
        self.context_failure(context)
    }

    fn last_known_context_matches(&self, context: &BuddyContext) -> bool {
        let snapshot = self.latest_snapshot();
        snapshot.active_buddy_id.as_deref() == Some(context.expected_buddy_id.as_str())
            && snapshot.buddies_revision == context.expected_buddies_revision
    }

    fn exit_context_failure(&self, context: &BuddyContext) -> Option<Failure> {
        if self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown)
        {
            return (!self.last_known_context_matches(context))
                .then(|| failure("buddy_context_stale", "退出处置上下文已变化"));
        }
        self.window_context_failure(context)
    }

    fn exit_context_matches(&self, expected: &BuddyContext, received: &BuddyContext) -> bool {
        expected == received
            || (self
                .buddy_storage
                .as_ref()
                .is_some_and(BuddyStorage::registry_is_unknown)
                && self.last_known_context_matches(received)
                && self.last_known_context_matches(expected))
            || (received.expected_buddy_id.is_empty()
                && self.latest_snapshot().revoked
                && expected.expected_buddies_revision == received.expected_buddies_revision)
    }

    fn draft_revision_set(&self) -> Result<Vec<BuddyDraftRevision>, String> {
        if let Some(storage) = self.buddy_storage.as_ref() {
            return storage
                .draft_revision_set()
                .map_err(|error| error.to_string());
        }
        let app = self
            .app
            .as_ref()
            .ok_or_else(|| "chat host released".to_owned())?;
        let id = self.active_context()?.expected_buddy_id;
        let draft = app.chat_draft_snapshot();
        Ok(vec![BuddyDraftRevision {
            buddy_id: id,
            revision: draft.revision,
            has_unsubmitted_drafts: app.has_unsubmitted_chat_drafts(),
            sessions: draft
                .sessions
                .into_iter()
                .map(
                    |session| moe_desktop_adapters::storage::BuddyDraftWatermark {
                        session_id: session.id,
                        revision: session.revision,
                        discarded_revision: session.discarded_revision,
                    },
                )
                .collect(),
        }])
    }

    fn preflight_quit_drafts(
        &self,
        expected: &[BuddyDraftRevision],
        active_close_discards_active: bool,
    ) -> Result<(), String> {
        if let Some(storage) = self.buddy_storage.as_ref() {
            return storage
                .preflight_quit_draft_discard(expected, active_close_discards_active)
                .map_err(|error| error.to_string());
        }
        if self.draft_revision_set()? != expected {
            return Err("草稿已变化，请重新确认退出".into());
        }
        self.app
            .as_ref()
            .ok_or_else(|| "chat host released".to_owned())?
            .preflight_quit_draft_discard(active_close_discards_active)
            .map_err(|error| error.public_message().to_owned())
    }

    fn discard_quit_drafts(&mut self, expected: &[BuddyDraftRevision]) -> Result<(), String> {
        if let Some(storage) = self.buddy_storage.as_mut() {
            return storage
                .discard_all_chat_drafts(expected)
                .map_err(|error| error.to_string());
        }
        if self.draft_revision_set()? != expected {
            return Err("草稿已变化，请重新确认退出".into());
        }
        let revision = expected
            .first()
            .ok_or_else(|| "chat host released".to_owned())?
            .revision;
        self.app
            .as_mut()
            .ok_or_else(|| "chat host released".to_owned())?
            .discard_all_chat_drafts(revision)
            .map_err(|error| error.public_message().to_owned())
    }

    fn apply_exit_confirmation_overlay(&self, snapshot: &mut Snapshot) {
        if self
            .pending_exit
            .as_ref()
            .is_some_and(|pending| pending.host_confirmation)
        {
            snapshot.close_confirmation = true;
            snapshot.close_intent = Some(CloseIntent::QuitApplication);
        }
    }

    fn latest_snapshot(&self) -> Snapshot {
        let mut snapshot = self.latest.lock().expect("snapshot mutex poisoned").clone();
        if self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown)
        {
            snapshot.external_transmission_allowed = false;
            if let Some(settings) = &mut snapshot.model_settings {
                settings.allow_transmission = false;
            }
        }
        snapshot
    }

    fn publish_snapshot(&mut self, snapshot: &Snapshot) {
        let mut snapshot = snapshot.clone();
        let prior = self
            .latest
            .lock()
            .expect("snapshot mutex poisoned")
            .presentation_revision;
        snapshot.presentation_revision = prior
            .checked_add(1)
            .map(|next| next.max(snapshot.revision))
            .unwrap_or(prior);
        *self.latest.lock().expect("snapshot mutex poisoned") = snapshot.clone();
        if snapshot.close_confirmation || snapshot.management_exit.is_some() {
            self.cancel_material_candidate();
        }
        if let Err(error) = self.port.emit_snapshot(&snapshot) {
            eprintln!("moe-buddy-tauri: {error}");
            self.port.notify_error("snapshot_emit_failed");
        }
    }

    fn apply_business(&mut self, action: Action, now_ms: u64) -> DispatchResult {
        let revoke = matches!(&action, Action::Revoke);
        if revoke
            && self
                .buddy_storage
                .as_ref()
                .is_some_and(BuddyStorage::registry_is_unknown)
        {
            let owner = self.latest_snapshot().active_buddy_id.unwrap_or_default();
            let error = self
                .buddy_storage
                .as_mut()
                .expect("checked storage exists")
                .revoke_known_buddy(&owner, now_ms)
                .err()
                .map(|error| {
                    failure_with_disposition("revoke_failed", &error.to_string(), "unknown")
                });
            if error.is_none() {
                self.model_key = None;
                self.model_settings = None;
                self.buddy_runtime.remove(&owner);
                self.management_status = self.management_drafts.revoke();
                self.management_discarded_version = None;
                self.buddy_commit = None;
                self.host_notice = Some("授权已撤回；模型设置与本次凭据已清除".into());
                let mut snapshot = self.latest_snapshot();
                snapshot.revoked = true;
                snapshot.buddy_commit = None;
                snapshot.model_settings = None;
                snapshot.external_transmission_allowed = false;
                self.decorate_management_snapshot(&mut snapshot);
                self.publish_snapshot(&snapshot);
            }
            return DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error,
            };
        }
        let error = match self.app_mut() {
            Ok(app) => app.dispatch(action, now_ms).err().map(|error| Failure {
                code: error.public_code().to_owned(),
                message: error.public_message().to_owned(),
                disposition: error.execution_disposition().as_str().to_owned(),
            }),
            Err(message) => Some(failure_with_disposition(
                "buddy_storage_unknown",
                &message,
                "unknown",
            )),
        };
        if revoke && error.is_none() {
            self.model_key = None;
            self.model_settings = None;
            if let Ok(context) = self.active_context() {
                self.buddy_runtime.remove(&context.expected_buddy_id);
            }
            self.management_status = self.management_drafts.revoke();
            self.buddy_commit = None;
            self.management_discarded_version = None;
            if self.pending_exit.is_some() {
                self.management_exit_version = Some(self.management_status.version);
            }
            self.host_notice = Some("授权已撤回；模型设置与本次凭据已清除".into());
        }
        let mut snapshot = match self.current_projection() {
            Ok(snapshot) => snapshot,
            Err(message) => {
                return DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: Some(error.unwrap_or_else(|| {
                        failure_with_disposition("buddy_storage_unknown", &message, "unknown")
                    })),
                };
            }
        };
        self.apply_exit_confirmation_overlay(&mut snapshot);
        snapshot.close_intent = snapshot.close_confirmation.then(|| {
            match self.pending_exit.as_ref().map(|pending| pending.intent) {
                Some(ExitIntent::QuitApplication) => CloseIntent::QuitApplication,
                _ => CloseIntent::CloseChat,
            }
        });
        snapshot.buddy_commit = self.buddy_commit.clone();
        decorate_snapshot(
            &mut snapshot,
            self.model_settings.as_ref(),
            self.model_key.is_some(),
            self.persistence_enabled,
            self.combined_host_notice(),
        );
        self.decorate_management_snapshot(&mut snapshot);
        self.publish_snapshot(&snapshot);
        let snapshot = self.latest_snapshot();
        DispatchResult {
            snapshot: Some(snapshot),
            error,
        }
    }

    /// 显示/隐藏 main：先完成业务 Open/Hide（或确认目标已满足），成功后才改变物理窗口。
    fn set_visibility(&mut self, label: &str, visible: bool, now_ms: u64) -> Result<(), String> {
        match (label, visible) {
            ("main", true) => self.show_main(now_ms),
            ("main", false) => self.hide_main(now_ms),
            ("pet", true) => self.change_window("pet", true),
            ("pet", false) => self.change_window("pet", false),
            _ => Err("window label is not supported".to_owned()),
        }
    }

    fn change_window(&mut self, label: &str, visible: bool) -> Result<(), String> {
        let changed = if visible {
            self.port.show(label)
        } else {
            self.port.hide(label)
        };
        if changed.is_err() {
            self.port.notify_error(if visible {
                "window_show_failed"
            } else {
                "window_hide_failed"
            });
        }
        changed
    }

    fn show_main(&mut self, now_ms: u64) -> Result<(), String> {
        if self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown)
        {
            self.port.show("main")?;
            return self.restore_and_focus_main();
        }
        let snapshot = self.app_snapshot()?;
        // 撤权后没有可重新打开的业务面板；仍允许显示只读的撤权状态与退出入口。
        if !snapshot.revoked && chat_visibility(&snapshot) != PanelVisibility::Visible {
            let opened = self.apply_business(Action::Open(PanelKind::Chat), now_ms);
            if let Some(error) = opened.error {
                self.port.notify_error("window_show_failed");
                return Err(error.message);
            }
        }
        if let Err(detail) = self.port.show("main") {
            self.port.notify_error("window_show_failed");
            return Err(detail);
        }
        self.restore_and_focus_main()
    }

    fn restore_and_focus_main(&mut self) -> Result<(), String> {
        if let Err(error) = self.port.unminimize("main") {
            eprintln!("moe-buddy-tauri: {error}");
        }
        if let Err(detail) = self.port.focus("main") {
            self.port.notify_error("window_show_failed");
            return Err(detail);
        }
        Ok(())
    }

    fn hide_main(&mut self, now_ms: u64) -> Result<(), String> {
        if self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown)
        {
            return self.port.hide("main");
        }
        let snapshot = self.app_snapshot()?;
        if !snapshot.revoked && chat_visibility(&snapshot) == PanelVisibility::Visible {
            let hidden = self.apply_business(Action::Hide(PanelKind::Chat), now_ms);
            if let Some(error) = hidden.error {
                self.port.notify_error("window_hide_failed");
                return Err(error.message);
            }
        }
        if let Err(detail) = self.port.hide("main") {
            self.port.notify_error("window_hide_failed");
            return Err(detail);
        }
        Ok(())
    }

    fn toggle_pause(&mut self) -> Result<(), String> {
        if self.hit_mask_clear_failed {
            return Err("material_hit_mask_clear_failed".into());
        }
        let state = self.material_host.state();
        if !matches!(state.status.as_str(), "playing" | "paused") || state.source == "empty" {
            return Err("no active playback to pause or resume".into());
        }
        let command = if state.status == "paused" {
            MaterialCommand::Resume {
                expected_resource_generation: state.resource_generation,
                expected_playback_generation: state.playback_generation,
            }
        } else {
            MaterialCommand::Pause {
                expected_resource_generation: state.resource_generation,
                expected_playback_generation: state.playback_generation,
            }
        };
        self.material_host.preflight_command(&command)?;
        if state.source == "package" {
            let result = if state.status == "paused" {
                self.material_host.resume()
            } else {
                self.material_host.pause()
            };
            result?;
            self.publish_material_state();
            self.emit_material_frame();
            return Ok(());
        }
        let Some(animator) = self.animator.as_mut() else {
            return Err("animation host is released".to_owned());
        };
        if animator.snapshot().ticket.is_none() {
            return Err("no active builtin playback to pause or resume".into());
        }
        let outcome = if animator.snapshot().paused {
            animator.resume().map(|_| ())
        } else {
            animator.pause().map(|_| ())
        };
        if let Err(error) = &outcome {
            eprintln!("moe-buddy-tauri: animator pause toggle failed: {error}");
            self.port.notify_error("animation_pause_failed");
            return Err("animator pause toggle failed".to_owned());
        }
        self.animation_faulted = false;
        self.animation_error_reported = false;
        self.material_host
            .set_builtin_status(if animator.snapshot().paused {
                "paused"
            } else {
                "playing"
            })?;
        self.publish_material_state();
        Ok(())
    }

    fn dispatch_command(
        &mut self,
        command: ChatCommand,
        context: BuddyContext,
        now_ms: u64,
    ) -> DispatchResult {
        let restricted_unknown_revoke = matches!(&command, ChatCommand::Revoke)
            && self
                .buddy_storage
                .as_ref()
                .is_some_and(BuddyStorage::registry_is_unknown);
        let restricted_unknown_panel_hide = matches!(
            &command,
            ChatCommand::HidePanel { panel }
                if panel_kind(panel).is_some_and(|panel| panel != PanelKind::Chat)
        ) && self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown);
        let guard_error = if restricted_unknown_revoke || restricted_unknown_panel_hide {
            self.exit_context_failure(&context)
        } else {
            self.context_failure(&context)
        };
        if let Some(error) = guard_error {
            return DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: Some(error),
            };
        }
        if matches!(
            &command,
            ChatCommand::NewSession { .. }
                | ChatCommand::SwitchSession { .. }
                | ChatCommand::ArchiveSession { .. }
                | ChatCommand::CreateBuddy { .. }
                | ChatCommand::SwitchBuddy { .. }
                | ChatCommand::RenameBuddy { .. }
        ) && (self.pending_exit.is_some() || self.management_exit_version.is_some())
        {
            return self.command_failure(
                "close_confirmation_pending",
                "关闭确认尚未处理，暂不能执行会话操作",
            );
        }
        match command {
            ChatCommand::CreateBuddy {
                label,
                persona,
                expected_management_draft_version,
            } => self.create_buddy(
                &context,
                label,
                persona,
                expected_management_draft_version,
                now_ms,
            ),
            ChatCommand::SwitchBuddy { id } => self.switch_buddy(&context, id, now_ms),
            ChatCommand::RenameBuddy {
                label,
                expected_management_draft_version,
            } => self.rename_buddy(&context, label, expected_management_draft_version),
            ChatCommand::CloseChat => self.close_from_ui(context, now_ms),
            ChatCommand::ConfirmClose { .. } => {
                self.command_failure("invalid_close_command", "关闭确认入口无效")
            }
            ChatCommand::ConfirmManagementDiscard { .. }
            | ChatCommand::CancelManagementExit { .. } => {
                self.command_failure("invalid_close_command", "退出确认入口无效")
            }
            ChatCommand::HideChat => match self.set_visibility("main", false, now_ms) {
                Ok(()) => DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: None,
                },
                Err(message) => DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: Some(Failure {
                        code: "window_hide_failed".to_owned(),
                        message,
                        disposition: "unknown".to_owned(),
                    }),
                },
            },
            ChatCommand::NewSession {
                expected_session_id,
                expected_sessions_revision,
            } => self.apply_business(
                Action::NewSession {
                    expected_session_id,
                    expected_sessions_revision,
                },
                now_ms,
            ),
            ChatCommand::SwitchSession {
                id,
                expected_session_id,
                expected_sessions_revision,
            } => self.apply_business(
                Action::SwitchSession {
                    id,
                    expected_session_id,
                    expected_sessions_revision,
                },
                now_ms,
            ),
            ChatCommand::ArchiveSession {
                id,
                expected_session_id,
                expected_sessions_revision,
            } => self.apply_business(
                Action::ArchiveSession {
                    id,
                    expected_session_id,
                    expected_sessions_revision,
                },
                now_ms,
            ),
            ChatCommand::SetDraft {
                text,
                expected_session_id,
                expected_sessions_revision,
            } => self.apply_business(
                Action::SetDraftGuarded {
                    text,
                    expected_session_id,
                    expected_sessions_revision,
                },
                now_ms,
            ),
            ChatCommand::Send {
                expected_session_id,
                expected_sessions_revision,
            } => {
                let mut result = self.apply_business(
                    Action::SendGuarded {
                        expected_session_id,
                        expected_sessions_revision,
                    },
                    now_ms,
                );
                if result.error.is_none() {
                    self.start_external_request();
                    result.snapshot = Some(self.latest_snapshot());
                }
                result
            }
            ChatCommand::Revoke => self.apply_business(Action::Revoke, now_ms),
            ChatCommand::OpenPanel { panel } => match panel_kind(&panel) {
                Some(panel) => self.apply_business(Action::Open(panel), now_ms),
                None => self.command_failure("invalid_panel", "unsupported panel"),
            },
            ChatCommand::HidePanel { panel } => match panel_kind(&panel) {
                Some(panel) if restricted_unknown_panel_hide => {
                    let result = self
                        .buddy_storage
                        .as_mut()
                        .expect("unknown presentation guard requires persistent storage")
                        .hide_management_panel_during_unknown(
                            &context.expected_buddy_id,
                            context.expected_buddies_revision,
                            panel,
                            now_ms,
                        );
                    match result {
                        Ok(()) => DispatchResult {
                            snapshot: Some(self.latest_snapshot()),
                            error: None,
                        },
                        Err(message) => self.command_failure("panel_close_failed", &message),
                    }
                }
                Some(panel) => self.apply_business(Action::Hide(panel), now_ms),
                None => self.command_failure("invalid_panel", "unsupported panel"),
            },
            ChatCommand::ConfigureModel {
                endpoint,
                model,
                persona,
                allow_transmission,
                api_key,
            } => match api_key.into_option() {
                Ok(api_key) => self.configure_model(
                    ModelSettings {
                        endpoint,
                        model,
                        persona,
                        allow_transmission,
                    },
                    api_key,
                    &context,
                    None,
                    now_ms,
                ),
                Err(()) => {
                    self.command_failure("api_key_required", "apiKey must be a string or null")
                }
            },
            ChatCommand::SelectPreset { id } => self.select_preset(&context, id, now_ms),
            ChatCommand::Remember { text } => self.apply_business(Action::Remember(text), now_ms),
            ChatCommand::SearchMemories { query } => {
                self.apply_business(Action::SearchMemories(query), now_ms)
            }
            ChatCommand::UpdateMemory { id, revision, text } => {
                self.apply_business(Action::UpdateMemory { id, revision, text }, now_ms)
            }
            ChatCommand::DeleteMemory { id, revision } => {
                self.apply_business(Action::DeleteMemory { id, revision }, now_ms)
            }
            ChatCommand::RefreshMemories => self.apply_business(Action::RefreshMemories, now_ms),
            ChatCommand::RefreshDiagnostics => {
                self.apply_business(Action::RefreshDiagnostics, now_ms)
            }
        }
    }

    fn management_submission_matches(
        &self,
        context: &BuddyContext,
        version: u64,
        kind: ManagementDraftKind,
    ) -> bool {
        let status = self.management_drafts.current();
        status.version == version
            && status.dirty
            && status.kind == kind
            && status.context.as_ref() == Some(context)
    }

    fn consume_management_submission(
        &mut self,
        context: BuddyContext,
        version: u64,
        kind: ManagementDraftKind,
    ) {
        let mailbox = self.management_drafts.clone();
        let _ = mailbox.with_version(version, |status| {
            if status.context.as_ref() == Some(&context) && status.dirty && status.kind == kind {
                status.dirty = false;
                status.kind = ManagementDraftKind::None;
                status.discarded_version = None;
                if let Ok(next) = self.active_context() {
                    status.context = Some(next);
                }
            }
        });
        self.management_status = mailbox.current();
        self.management_discarded_version = self.management_status.discarded_version;
        self.management_exit_version = None;
    }

    fn create_buddy(
        &mut self,
        context: &BuddyContext,
        label: String,
        persona: String,
        expected_management_draft_version: u64,
        now_ms: u64,
    ) -> DispatchResult {
        if !self.management_submission_matches(
            context,
            expected_management_draft_version,
            ManagementDraftKind::BuddyCreate,
        ) {
            return self
                .command_failure("management_draft_stale", "伙伴创建草稿已变化或上下文已过期");
        }
        let Some(storage) = self.buddy_storage.as_mut() else {
            return self.command_failure("buddy_storage_unavailable", "伙伴存储尚未就绪");
        };
        let result =
            storage.create_buddy(context.expected_buddies_revision, &label, &persona, now_ms);
        match result {
            Ok(receipt) => {
                let active_id = receipt.registry.active_buddy_id.unwrap_or_default();
                self.buddy_commit = Some(crate::contract::BuddyCommitView {
                    kind: "create_buddy".into(),
                    from_buddy_id: context.expected_buddy_id.clone(),
                    from_revision: context.expected_buddies_revision,
                    to_buddy_id: active_id.clone(),
                    to_revision: receipt.registry.revision,
                    management_version: expected_management_draft_version,
                });
                self.buddy_runtime
                    .insert(active_id, BuddyRuntimeAuthorization::default());
                self.model_settings = None;
                self.model_key = None;
                self.consume_management_submission(
                    context.clone(),
                    expected_management_draft_version,
                    ManagementDraftKind::BuddyCreate,
                );
                self.host_notice = Some("伙伴已创建；凭据与发送许可尚未配置".into());
                self.publish_current_snapshot();
                DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: None,
                }
            }
            Err(error) => {
                if error.publication_state()
                    == Some(moe_desktop_adapters::storage::BuddyPublicationState::Published)
                    && self.sync_selected_buddy_runtime().is_err()
                {
                    self.model_settings = None;
                    self.model_key = None;
                }
                let result = self.buddy_storage_failure(error);
                if result
                    .error
                    .as_ref()
                    .is_some_and(|failure| failure.disposition != "not_executed")
                {
                    self.publish_current_snapshot();
                }
                result
            }
        }
    }

    fn switch_buddy(&mut self, context: &BuddyContext, id: String, now_ms: u64) -> DispatchResult {
        if self.management_status.dirty
            || self.management_status.kind != ManagementDraftKind::None
            || self.management_exit_version.is_some()
            || self.pending_exit.is_some()
        {
            return self.command_failure(
                "management_draft_pending",
                "请先处理未保存的管理编辑或关闭确认",
            );
        }
        let current_id = context.expected_buddy_id.clone();
        let result = match self.buddy_storage.as_mut() {
            Some(storage) => storage.switch_buddy(&id, context.expected_buddies_revision, now_ms),
            None => return self.command_failure("buddy_storage_unavailable", "伙伴存储尚未就绪"),
        };
        match result {
            Ok(receipt) => {
                self.buddy_runtime.insert(
                    current_id.clone(),
                    BuddyRuntimeAuthorization {
                        api_key: self.model_key.clone(),
                        allow_transmission: self
                            .model_settings
                            .as_ref()
                            .is_some_and(|settings| settings.allow_transmission),
                    },
                );
                let active_id = receipt.registry.active_buddy_id.unwrap_or_default();
                self.buddy_commit = Some(crate::contract::BuddyCommitView {
                    kind: "switch_buddy".into(),
                    from_buddy_id: current_id.clone(),
                    from_revision: context.expected_buddies_revision,
                    to_buddy_id: active_id.clone(),
                    to_revision: receipt.registry.revision,
                    management_version: self.management_status.version,
                });
                let runtime = self
                    .buddy_runtime
                    .entry(active_id.clone())
                    .or_default()
                    .clone();
                self.model_settings = self
                    .buddy_storage
                    .as_ref()
                    .and_then(|storage| storage.buddy_profile(&active_id).ok())
                    .and_then(|profile| {
                        profile.model().map(|model| {
                            let mut settings = model.to_model_settings(profile.persona());
                            settings.allow_transmission = runtime.allow_transmission;
                            settings
                        })
                    });
                self.model_key = runtime.api_key;
                self.management_status = self
                    .management_drafts
                    .rebind(BuddyContext {
                        expected_buddy_id: active_id,
                        expected_buddies_revision: receipt.registry.revision,
                    })
                    .unwrap_or_else(|status| status);
                self.management_discarded_version = self.management_status.discarded_version;
                self.host_notice = None;
                self.publish_current_snapshot();
                DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: None,
                }
            }
            Err(error) => {
                if error.publication_state()
                    == Some(moe_desktop_adapters::storage::BuddyPublicationState::Published)
                    && let Ok(context) = self.sync_selected_buddy_runtime()
                {
                    self.management_status = self
                        .management_drafts
                        .rebind(context)
                        .unwrap_or_else(|status| status);
                    self.management_discarded_version = self.management_status.discarded_version;
                    self.publish_current_snapshot();
                }
                self.buddy_storage_failure(error)
            }
        }
    }

    fn rename_buddy(
        &mut self,
        context: &BuddyContext,
        label: String,
        expected_management_draft_version: u64,
    ) -> DispatchResult {
        if !self.management_submission_matches(
            context,
            expected_management_draft_version,
            ManagementDraftKind::BuddyRename,
        ) {
            return self
                .command_failure("management_draft_stale", "伙伴改名草稿已变化或上下文已过期");
        }
        let profile = match self.buddy_storage.as_ref() {
            Some(storage) => match storage.buddy_profile(&context.expected_buddy_id) {
                Ok(profile) => profile,
                Err(error) => return self.buddy_storage_failure(error),
            },
            None => return self.command_failure("buddy_storage_unavailable", "伙伴存储尚未就绪"),
        };
        let result = match self.buddy_storage.as_mut() {
            Some(storage) => storage.rename_buddy(
                &context.expected_buddy_id,
                context.expected_buddies_revision,
                profile.profile_revision(),
                &label,
            ),
            None => return self.command_failure("buddy_storage_unavailable", "伙伴存储尚未就绪"),
        };
        match result {
            Ok(receipt) => {
                self.buddy_commit = Some(crate::contract::BuddyCommitView {
                    kind: "rename_buddy".into(),
                    from_buddy_id: context.expected_buddy_id.clone(),
                    from_revision: context.expected_buddies_revision,
                    to_buddy_id: context.expected_buddy_id.clone(),
                    to_revision: receipt.registry.revision,
                    management_version: expected_management_draft_version,
                });
                self.consume_management_submission(
                    context.clone(),
                    expected_management_draft_version,
                    ManagementDraftKind::BuddyRename,
                );
                self.publish_current_snapshot();
                DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: None,
                }
            }
            Err(error) => {
                if error.publication_state()
                    == Some(moe_desktop_adapters::storage::BuddyPublicationState::Published)
                {
                    self.publish_current_snapshot();
                }
                self.buddy_storage_failure(error)
            }
        }
    }

    fn buddy_storage_failure(
        &self,
        error: moe_desktop_adapters::storage::BuddyStorageError,
    ) -> DispatchResult {
        use moe_desktop_adapters::storage::BuddyStorageError;
        let (code, message, disposition) = match &error {
            BuddyStorageError::Application(app_error) => (
                app_error.public_code().to_owned(),
                app_error.public_message().to_owned(),
                app_error.execution_disposition().as_str(),
            ),
            BuddyStorageError::Registry(registry_error) => buddy_registry_failure(registry_error),
            BuddyStorageError::RegistryCommit {
                disposition: moe_desktop_adapters::storage::BuddyPublicationState::NotPublished,
                source,
            } => match source.as_ref() {
                BuddyStorageError::Application(app_error) => (
                    app_error.public_code().to_owned(),
                    app_error.public_message().to_owned(),
                    app_error.execution_disposition().as_str(),
                ),
                BuddyStorageError::Registry(registry_error) => {
                    buddy_registry_failure(registry_error)
                }
                BuddyStorageError::ModelConfiguration(error) => (
                    "model_configuration".to_owned(),
                    error.to_string(),
                    "not_executed",
                ),
                BuddyStorageError::Preparation(_) => (
                    "buddy_preparation_failed".to_owned(),
                    source.to_string(),
                    "unknown",
                ),
                BuddyStorageError::PendingRegistry
                | BuddyStorageError::PendingLegacySettings
                | BuddyStorageError::RevisionConflict
                | BuddyStorageError::BuddyNotFound
                | BuddyStorageError::InactiveBuddy => (
                    "buddy_storage_failed".to_owned(),
                    source.to_string(),
                    "not_executed",
                ),
                _ => (
                    "buddy_storage_failed".to_owned(),
                    error.to_string(),
                    "unknown",
                ),
            },
            BuddyStorageError::RegistryCommit { .. }
            | BuddyStorageError::CandidateCommit { .. } => (
                "buddy_storage_failed".to_owned(),
                error.to_string(),
                "unknown",
            ),
            BuddyStorageError::Preparation(_) => (
                "buddy_preparation_failed".to_owned(),
                error.to_string(),
                "unknown",
            ),
            BuddyStorageError::Io {
                stage:
                    moe_desktop_adapters::storage::BuddyRegistryIoStage::WriteCandidate
                    | moe_desktop_adapters::storage::BuddyRegistryIoStage::SyncCandidate
                    | moe_desktop_adapters::storage::BuddyRegistryIoStage::SyncCandidateDirectory,
                ..
            } => (
                "buddy_preparation_failed".to_owned(),
                error.to_string(),
                "unknown",
            ),
            _ => (
                "buddy_storage_failed".to_owned(),
                error.to_string(),
                "not_executed",
            ),
        };
        DispatchResult {
            snapshot: Some(self.latest_snapshot()),
            error: Some(failure_with_disposition(&code, &message, disposition)),
        }
    }

    fn select_preset(&mut self, context: &BuddyContext, id: String, now_ms: u64) -> DispatchResult {
        let Some(storage) = self.buddy_storage.as_ref() else {
            return self.apply_business(Action::SelectPreset(id), now_ms);
        };
        let profile = match storage.buddy_profile(&context.expected_buddy_id) {
            Ok(profile) => profile,
            Err(error) => return self.buddy_storage_failure(error),
        };
        let Some(model) = profile.model() else {
            return self.apply_business(Action::SelectPreset(id), now_ms);
        };
        if model.preset_id() == id {
            return DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: None,
            };
        }
        let preset = match self
            .latest_snapshot()
            .presets
            .into_iter()
            .find(|preset| preset.id == id)
        {
            Some(preset) => preset,
            None => return self.command_failure("invalid_preset", "预设不存在，已拒绝绑定"),
        };
        let current = self
            .model_settings
            .clone()
            .unwrap_or_else(|| model.to_model_settings(profile.persona()));
        let changed_target = current.endpoint != preset.endpoint || current.model != preset.model;
        self.configure_model(
            ModelSettings {
                endpoint: preset.endpoint,
                model: preset.model,
                persona: profile.persona().to_owned(),
                allow_transmission: current.allow_transmission && !changed_target,
            },
            changed_target.then(String::new),
            context,
            Some(id),
            now_ms,
        )
    }

    fn command_failure(&self, code: &str, message: &str) -> DispatchResult {
        DispatchResult {
            snapshot: Some(self.latest_snapshot()),
            error: Some(failure(code, message)),
        }
    }

    fn configure_model(
        &mut self,
        settings: ModelSettings,
        api_key: Option<String>,
        context: &BuddyContext,
        preset_id_override: Option<String>,
        now_ms: u64,
    ) -> DispatchResult {
        if let Some(current) = self.model_settings.as_ref()
            && (current.endpoint != settings.endpoint || current.model != settings.model)
            && api_key.is_none()
        {
            return self.command_failure(
                "api_key_required",
                "端点或模型已更改，请明确替换 API Key；空字符串可清除",
            );
        }
        let next_key = match api_key {
            Some(value) if !value.is_empty() => Some(value),
            Some(_) => None,
            None => self.model_key.clone(),
        };
        if settings.allow_transmission && next_key.is_none() {
            return self.command_failure("api_key_required", "启用发送许可前必须提供 API Key");
        }
        let active_preset = self.latest_snapshot().active_preset;
        let management_version = self.management_status.version;
        let commit_kind = if preset_id_override.is_some() {
            "select_preset"
        } else {
            "configure_model"
        };
        if let Some(storage) = self.buddy_storage.as_mut() {
            let profile = match storage.buddy_profile(&context.expected_buddy_id) {
                Ok(profile) => profile,
                Err(error) => return self.buddy_storage_failure(error),
            };
            let preset_id = preset_id_override.unwrap_or_else(|| {
                profile
                    .model()
                    .map_or_else(|| active_preset, |model| model.preset_id().to_owned())
            });
            let model_binding = match moe_chat_app::BuddyModelConfiguration::new(
                settings.endpoint.clone(),
                settings.model.clone(),
                preset_id,
            ) {
                Ok(binding) => binding,
                Err(error) => {
                    return self.command_failure("model_configuration", &error.to_string());
                }
            };
            let result = storage.commit_configuration_with_runtime(
                &context.expected_buddy_id,
                context.expected_buddies_revision,
                profile.profile_revision(),
                model_binding,
                settings.clone(),
                now_ms,
            );
            let receipt = match result {
                Ok(receipt) => receipt,
                Err(error) => {
                    let uncertain = matches!(
                        error.publication_state(),
                        Some(
                            moe_desktop_adapters::storage::BuddyPublicationState::Published
                                | moe_desktop_adapters::storage::BuddyPublicationState::Unknown
                        )
                    );
                    if uncertain {
                        if let Some(settings) = &mut self.model_settings {
                            settings.allow_transmission = false;
                        }
                        if let Some(runtime) =
                            self.buddy_runtime.get_mut(&context.expected_buddy_id)
                        {
                            runtime.allow_transmission = false;
                        }
                        self.host_notice =
                            Some("伙伴配置提交已发生或结果未知；当前实例已封闭配置与发送".into());
                        self.publish_current_snapshot();
                    }
                    return self.buddy_storage_failure(error);
                }
            };
            let next_context = BuddyContext {
                expected_buddy_id: context.expected_buddy_id.clone(),
                expected_buddies_revision: receipt.registry.revision,
            };
            if let Ok(status) = self
                .management_drafts
                .coordinate_commit(context, next_context)
            {
                self.management_status = status;
                self.management_discarded_version = self.management_status.discarded_version;
            }
            self.buddy_commit = Some(crate::contract::BuddyCommitView {
                kind: commit_kind.into(),
                from_buddy_id: context.expected_buddy_id.clone(),
                from_revision: context.expected_buddies_revision,
                to_buddy_id: context.expected_buddy_id.clone(),
                to_revision: receipt.registry.revision,
                management_version,
            });
        } else {
            if self.persistence_enabled {
                return self.command_failure("buddy_storage_unavailable", "伙伴存储尚未就绪");
            }
            let Ok(app) = self.app_mut() else {
                return self.command_failure("application_unavailable", "聊天应用当前不可用");
            };
            if let Err(error) = app.configure_model(&settings) {
                return self.app_failure(error);
            }
        }
        let committed_allow_transmission = settings.allow_transmission;
        self.model_settings = Some(settings);
        self.model_key = next_key;
        if self.buddy_storage.is_none() {
            self.buddy_commit = None;
        }
        self.buddy_runtime.insert(
            context.expected_buddy_id.clone(),
            BuddyRuntimeAuthorization {
                api_key: self.model_key.clone(),
                allow_transmission: committed_allow_transmission,
            },
        );
        self.host_notice = Some("模型设置已保存；密钥与发送许可仅在本次进程有效".into());
        self.publish_current_snapshot();
        DispatchResult {
            snapshot: Some(self.latest_snapshot()),
            error: None,
        }
    }

    fn app_failure(&self, error: moe_chat_app::AppError) -> DispatchResult {
        DispatchResult {
            snapshot: Some(self.latest_snapshot()),
            error: Some(Failure {
                code: error.public_code().to_owned(),
                message: error.public_message().to_owned(),
                disposition: error.execution_disposition().as_str().to_owned(),
            }),
        }
    }

    fn publish_current_snapshot(&mut self) {
        let storage_unknown = self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown);
        let mut snapshot = self.current_projection().unwrap_or_else(|message| {
            let mut fallback = self.latest_snapshot();
            fallback.close_confirmation = false;
            fallback.close_intent = None;
            if storage_unknown {
                fallback.external_transmission_allowed = false;
                if let Some(settings) = &mut fallback.model_settings {
                    settings.allow_transmission = false;
                }
            }
            fallback.notice = Some(format!("伙伴存储状态未知；当前操作已封闭：{message}"));
            fallback
        });
        if storage_unknown {
            snapshot.external_transmission_allowed = false;
            if let Some(settings) = &mut snapshot.model_settings {
                settings.allow_transmission = false;
            }
        }
        self.apply_exit_confirmation_overlay(&mut snapshot);
        snapshot.close_intent = snapshot.close_confirmation.then(|| {
            match self.pending_exit.as_ref().map(|pending| pending.intent) {
                Some(ExitIntent::QuitApplication) => CloseIntent::QuitApplication,
                _ => CloseIntent::CloseChat,
            }
        });
        snapshot.buddy_commit = self.buddy_commit.clone();
        decorate_snapshot(
            &mut snapshot,
            self.model_settings.as_ref(),
            self.model_key.is_some(),
            self.persistence_enabled,
            self.combined_host_notice(),
        );
        if storage_unknown {
            snapshot.external_transmission_allowed = false;
            if let Some(settings) = &mut snapshot.model_settings {
                settings.allow_transmission = false;
            }
        }
        self.decorate_management_snapshot(&mut snapshot);
        self.publish_snapshot(&snapshot);
    }

    fn decorate_management_snapshot(&self, snapshot: &mut Snapshot) {
        snapshot.management_draft = ManagementDraftView {
            version: self.management_status.version,
            dirty: self.management_status.dirty,
            kind: self.management_status.kind,
        };
        snapshot.management_exit = self.management_exit_version.and_then(|version| {
            self.pending_exit
                .as_ref()
                .map(|pending| ManagementExitView {
                    version,
                    intent: close_intent(pending.intent),
                })
        });
        snapshot.management_discarded_version = self.management_discarded_version;
    }

    fn sync_management_drafts(&mut self) {
        let status = self.management_drafts.current();
        if status == self.management_status {
            return;
        }
        self.management_status = status.clone();
        self.management_discarded_version = status.discarded_version;
        if self.pending_exit.is_some() {
            self.management_exit_version = Some(status.version);
        }
        self.publish_current_snapshot();
    }

    fn accept_management_draft_state(
        &mut self,
        version: u64,
        dirty: bool,
        kind: ManagementDraftKind,
        context: BuddyContext,
    ) -> Result<ManagementDraftAck, String> {
        let restricted_unknown_update = self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown)
            && self.last_known_context_matches(&context)
            && self.management_status.context.as_ref() == Some(&context);
        if !restricted_unknown_update && let Some(error) = self.context_failure(&context) {
            return Err(error.message);
        }
        let accepted = self
            .management_drafts
            .update(version, dirty, kind, context)
            .map_err(str::to_owned)?;
        self.management_status = accepted.clone();
        self.management_discarded_version = accepted.discarded_version;
        if self.pending_exit.is_some() {
            self.management_exit_version = Some(accepted.version);
        }
        self.publish_current_snapshot();
        Ok(ManagementDraftAck {
            version: accepted.version,
            dirty: accepted.dirty,
        })
    }

    fn start_external_request(&mut self) {
        let owner = match self.active_context() {
            Ok(context) => context.expected_buddy_id,
            Err(message) => {
                self.host_notice = Some(message);
                self.publish_current_snapshot();
                return;
            }
        };
        if self.model_requests.contains_key(&owner) {
            return;
        }
        let request = match self.app_mut() {
            Ok(app) => app.take_external_request(),
            Err(message) => {
                self.host_notice = Some(message);
                self.publish_current_snapshot();
                return;
            }
        };
        let Some(request) = request else {
            return;
        };
        let (input, ticket) = request.into_parts();
        let api_key = if self.buddy_storage.is_some() {
            self.buddy_runtime
                .get(&owner)
                .and_then(|runtime| runtime.api_key.clone())
                .unwrap_or_default()
        } else {
            self.model_key.clone().unwrap_or_default()
        };
        match moe_desktop_adapters::transport::start(input, api_key) {
            Ok(receiver) => {
                self.model_requests
                    .insert(owner, PendingExternalRequest { receiver, ticket });
            }
            Err(_) => self.complete_external(
                &owner,
                ticket,
                moe_agent_runtime::ModelOutcome::Failed(
                    moe_agent_runtime::ModelFailure::Unavailable,
                ),
                "无法启动请求线程；尚未发送".into(),
            ),
        }
    }

    fn poll_external_request(&mut self) {
        let owners = self.model_requests.keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let result = match self
                .model_requests
                .get(&owner)
                .map(|request| request.receiver.try_recv())
            {
                Some(Ok(reply)) => Some((reply.outcome, reply.message.to_owned())),
                Some(Err(mpsc::TryRecvError::Empty)) | None => None,
                Some(Err(mpsc::TryRecvError::Disconnected)) => Some((
                    moe_agent_runtime::ModelOutcome::Unknown,
                    "请求线程已中断；结果未知，不会重发".into(),
                )),
            };
            if let Some((outcome, message)) = result
                && let Some(request) = self.model_requests.remove(&owner)
            {
                self.complete_external(&owner, request.ticket, outcome, message);
            }
        }
    }

    fn complete_external(
        &mut self,
        owner: &str,
        ticket: moe_chat_app::ExternalRequestTicket,
        outcome: moe_agent_runtime::ModelOutcome,
        message: String,
    ) {
        let now_ms = self.now_ms();
        let result = if let Some(storage) = self.buddy_storage.as_mut() {
            storage
                .complete_external_for_buddy(owner, ticket, outcome, now_ms)
                .map_err(|error| error.to_string())
        } else {
            self.app_mut().and_then(|app| {
                app.complete_external(ticket, outcome, now_ms)
                    .map_err(|error| error.public_message().to_owned())
            })
        };
        if self
            .active_context()
            .is_ok_and(|context| context.expected_buddy_id == owner)
        {
            self.host_notice = Some(result.err().unwrap_or(message));
            self.publish_current_snapshot();
        }
    }

    /// CloseChat consumes only the selected app's current session draft.
    fn close_from_ui(&mut self, context: BuddyContext, now_ms: u64) -> DispatchResult {
        self.sync_management_drafts();
        if self.app_snapshot().is_ok_and(|snapshot| snapshot.revoked) {
            return match self.finish_close(None) {
                Ok(()) => DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: None,
                },
                Err(message) => DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: Some(failure_with_disposition(
                        "window_hide_failed",
                        &message,
                        "unknown",
                    )),
                },
            };
        }
        if self.management_status.dirty {
            if let Err(message) = self.associate_exit(
                ExitRequest {
                    id: 0,
                    intent: ExitIntent::CloseChat,
                    buddy_context: Some(context.clone()),
                },
                context,
            ) {
                return self.command_failure("close_state_unavailable", &message);
            }
            self.management_exit_version = Some(self.management_status.version);
            self.publish_current_snapshot();
            return DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: None,
            };
        }
        let result = self.apply_business(Action::Close(PanelKind::Chat), now_ms);
        if result.error.is_some() {
            return result;
        }
        let Some(snapshot) = result.snapshot.as_ref() else {
            return self.command_failure("close_state_unavailable", "关闭状态无法读取");
        };
        if snapshot.close_confirmation {
            if self.pending_exit.is_some()
                && let Err(error) = self.present_confirmation()
            {
                eprintln!("moe-buddy-tauri: unable to present close confirmation: {error}");
            }
            return result;
        }
        let intent = self.pending_exit.take().map(|pending| pending.intent);
        match self.finish_close(intent) {
            Ok(()) => result,
            Err(message) => DispatchResult {
                snapshot: result.snapshot,
                error: Some(Failure {
                    code: "window_hide_failed".to_owned(),
                    message,
                    disposition: "unknown".to_owned(),
                }),
            },
        }
    }

    fn confirm_close(
        &mut self,
        discard: bool,
        expected_intent: Option<CloseIntent>,
        context: BuddyContext,
        now_ms: u64,
    ) -> DispatchResult {
        if let Some(error) = self.exit_context_failure(&context) {
            return DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: Some(error),
            };
        }
        self.sync_management_drafts();
        if self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown)
        {
            let Some(pending) = self.pending_exit.clone() else {
                return self.command_failure("no_pending_confirmation", "没有待处理的退出确认");
            };
            if pending.intent != ExitIntent::QuitApplication
                || expected_intent != Some(CloseIntent::QuitApplication)
                || pending.buddy_context != context
                || !pending.host_confirmation
            {
                return self
                    .command_failure("close_confirmation_stale", "退出确认或伙伴上下文已变化");
            }
            if !discard {
                self.pending_exit = None;
                self.management_exit_version = None;
                self.publish_current_snapshot();
                return DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: None,
                };
            }
            let management_confirmed = self.pending_exit.as_ref().is_some_and(|pending| {
                pending.management_discard_confirmed_version == Some(self.management_status.version)
            });
            if self.management_status.dirty && !management_confirmed {
                return self.command_failure("management_draft_pending", "请先明确处理管理草稿");
            }
            let revisions = match self.draft_revision_set() {
                Ok(revisions) => revisions,
                Err(message) => return self.quit_state_failure(message),
            };
            if revisions != pending.chat_draft_revisions {
                return self.command_failure("confirm_stale", "聊天草稿已变化，请重新确认退出");
            }
            if let Err(message) = self.preflight_quit_drafts(&revisions, false) {
                return self.quit_state_failure(message);
            }
            if let Err(message) = self.discard_quit_drafts(&revisions) {
                return self.quit_state_failure(message);
            }
            if self.management_status.dirty && management_confirmed {
                match self
                    .management_drafts
                    .confirm_discard(self.management_status.version)
                {
                    Ok(status) => {
                        self.management_status = status.clone();
                        self.management_discarded_version = status.discarded_version;
                    }
                    Err(_) => {
                        return self
                            .command_failure("confirm_stale", "管理草稿已变化，请重新确认退出");
                    }
                }
            }
            return match self.finish_close(Some(ExitIntent::QuitApplication)) {
                Ok(()) => DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: None,
                },
                Err(message) => self.command_failure("window_hide_failed", &message),
            };
        }
        let current_snapshot = match self.app_snapshot() {
            Ok(snapshot) => snapshot,
            Err(message) => {
                return DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: Some(failure_with_disposition(
                        "buddy_storage_unknown",
                        &message,
                        "unknown",
                    )),
                };
            }
        };
        let app_close_is_pending = current_snapshot
            .panels
            .iter()
            .any(|panel| panel.kind == PanelKind::Chat && panel.confirmation);
        let pending = self.pending_exit.clone();
        let host_close_is_pending = pending
            .as_ref()
            .is_some_and(|pending| pending.host_confirmation);
        if !app_close_is_pending && !host_close_is_pending {
            return self.command_failure("no_pending_confirmation", "没有待处理的聊天关闭确认");
        }
        let current_intent = Some(
            pending
                .as_ref()
                .map(|pending| close_intent(pending.intent))
                .unwrap_or(CloseIntent::CloseChat),
        );
        if pending
            .as_ref()
            .is_some_and(|pending| !self.exit_context_matches(&pending.buddy_context, &context))
            || expected_intent != current_intent
        {
            self.publish_current_snapshot();
            return self.command_failure("close_confirmation_stale", "关闭意图或伙伴上下文已变化");
        }

        if !discard {
            if app_close_is_pending {
                let result = self.apply_business(
                    Action::ConfirmClose {
                        panel: PanelKind::Chat,
                        discard: false,
                    },
                    now_ms,
                );
                if result.error.is_some() {
                    self.pending_exit = None;
                    self.publish_current_snapshot();
                    return result;
                }
            }
            self.pending_exit = None;
            self.management_exit_version = None;
            self.publish_current_snapshot();
            return DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: None,
            };
        }

        if let Some(pending) = pending.as_ref()
            && pending.intent == ExitIntent::QuitApplication
        {
            let current_set = match self.draft_revision_set() {
                Ok(set) => set,
                Err(message) => {
                    return self.quit_state_failure(message);
                }
            };
            if current_set != pending.chat_draft_revisions
                || self.management_status.version != pending.management_draft_version
                || self.management_status.context.as_ref() != Some(&pending.buddy_context)
                || (self.management_status.dirty
                    && pending.management_discard_confirmed_version
                        != Some(pending.management_draft_version))
            {
                if app_close_is_pending {
                    let _ = self.apply_business(
                        Action::ConfirmClose {
                            panel: PanelKind::Chat,
                            discard: false,
                        },
                        now_ms,
                    );
                }
                self.pending_exit = None;
                self.management_exit_version = None;
                self.publish_current_snapshot();
                return self.command_failure("confirm_stale", "草稿已变化，请重新确认退出");
            }
            if let Err(message) = self.preflight_quit_drafts(&current_set, app_close_is_pending) {
                return self.quit_state_failure(message);
            }
        }

        let close_result = if host_close_is_pending && current_snapshot.revoked {
            DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: None,
            }
        } else if host_close_is_pending {
            self.apply_business(Action::Close(PanelKind::Chat), now_ms)
        } else {
            self.apply_business(
                Action::ConfirmClose {
                    panel: PanelKind::Chat,
                    discard: true,
                },
                now_ms,
            )
        };
        if close_result.error.is_some() {
            self.pending_exit = None;
            self.publish_current_snapshot();
            return close_result;
        }
        let after_close = match self.app_snapshot() {
            Ok(snapshot) => snapshot,
            Err(message) => return self.quit_state_failure(message),
        };
        let app_confirmation_remains = after_close
            .panels
            .iter()
            .any(|panel| panel.kind == PanelKind::Chat && panel.confirmation);
        if app_confirmation_remains {
            if let Some(pending) = &mut self.pending_exit {
                pending.host_confirmation = false;
            }
            self.publish_current_snapshot();
            return close_result;
        }

        let Some(pending) = pending.filter(|pending| pending.intent == ExitIntent::QuitApplication)
        else {
            let intent = self.pending_exit.take().map(|pending| pending.intent);
            return match self.finish_close(intent) {
                Ok(()) => close_result,
                Err(message) => DispatchResult {
                    snapshot: close_result.snapshot,
                    error: Some(failure_with_disposition(
                        "window_hide_failed",
                        &message,
                        "unknown",
                    )),
                },
            };
        };
        let current_set = match self.draft_revision_set() {
            Ok(set) => set,
            Err(message) => return self.quit_state_failure(message),
        };
        let old_buddies = pending
            .chat_draft_revisions
            .iter()
            .map(|entry| entry.buddy_id.as_str())
            .collect::<Vec<_>>();
        let new_buddies = current_set
            .iter()
            .map(|entry| entry.buddy_id.as_str())
            .collect::<Vec<_>>();
        if old_buddies != new_buddies {
            return self.command_failure("confirm_stale", "已加载伙伴集合已变化，请重新确认退出");
        }
        if let Err(message) = self.preflight_quit_drafts(&current_set, false) {
            return self.quit_state_failure(message);
        }
        let mailbox = self.management_drafts.clone();
        let result = mailbox.with_version(pending.management_draft_version, |status| {
            if status.context.as_ref() != Some(&pending.buddy_context)
                || (status.dirty
                    && pending.management_discard_confirmed_version != Some(status.version))
            {
                return Err("管理草稿已变化，请重新确认退出".to_owned());
            }
            self.discard_quit_drafts(&current_set)?;
            if status.dirty && !status.revoked {
                status.dirty = false;
                status.kind = ManagementDraftKind::None;
                status.discarded_version = Some(status.version);
            }
            self.management_status = status.clone();
            self.management_discarded_version = status.discarded_version;
            self.management_exit_version = None;
            self.publish_current_snapshot();
            self.finish_close(Some(ExitIntent::QuitApplication))
        });
        match result {
            Ok(Ok(())) => DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: None,
            },
            Ok(Err(message)) => self.quit_state_failure(message),
            Err(status) => {
                self.management_status = status.clone();
                self.management_discarded_version = status.discarded_version;
                self.command_failure("confirm_stale", "管理草稿已变化，请重新确认退出")
            }
        }
    }

    fn quit_state_failure(&mut self, message: String) -> DispatchResult {
        if let Some(pending) = &mut self.pending_exit
            && pending.intent == ExitIntent::QuitApplication
        {
            pending.host_confirmation = true;
        }
        self.publish_current_snapshot();
        DispatchResult {
            snapshot: Some(self.latest_snapshot()),
            error: Some(failure_with_disposition(
                "quit_draft_state_unknown",
                &message,
                "unknown",
            )),
        }
    }

    fn confirm_management_discard(
        &mut self,
        version: u64,
        intent: CloseIntent,
        context: BuddyContext,
    ) -> DispatchResult {
        self.sync_management_drafts();
        if let Some(error) = self.exit_context_failure(&context) {
            return DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: Some(error),
            };
        }
        if self.management_exit_version != Some(version)
            || self.management_status.context.as_ref() != Some(&context)
            || self
                .pending_exit
                .as_ref()
                .map(|pending| close_intent(pending.intent))
                != Some(intent)
        {
            return self.command_failure(
                "management_confirmation_stale",
                "管理编辑已变化，请重新确认退出",
            );
        }
        if intent == CloseIntent::QuitApplication {
            let draft_revisions = match self.draft_revision_set() {
                Ok(revisions) => revisions,
                Err(message) => return self.quit_state_failure(message),
            };
            let Some(pending) = &mut self.pending_exit else {
                return self.command_failure(
                    "management_confirmation_stale",
                    "管理编辑已变化，请重新确认退出",
                );
            };
            pending.buddy_context = context;
            pending.chat_draft_revisions = draft_revisions;
            pending.management_draft_version = version;
            pending.management_discard_confirmed_version = Some(version);
            self.management_exit_version = None;
            self.publish_current_snapshot();
            let request = self.pending_exit.as_ref().map(|pending| ExitRequest {
                id: pending.request_id,
                intent: pending.intent,
                buddy_context: Some(pending.buddy_context.clone()),
            });
            if let Some(request) = request
                && let Err(message) = self.handle_exit(request)
            {
                self.fail_exit(&message);
                return DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: Some(failure_with_disposition(
                        "exit_after_discard_failed",
                        &message,
                        "unknown",
                    )),
                };
            }
            return DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: None,
            };
        }
        let status = match self.management_drafts.confirm_discard(version) {
            Ok(status) => status,
            Err(status) => {
                self.management_status = status.clone();
                self.management_exit_version = self.pending_exit.as_ref().map(|_| status.version);
                self.management_discarded_version = status.discarded_version;
                self.publish_current_snapshot();
                return self.command_failure(
                    "management_confirmation_stale",
                    "管理编辑已变化，请重新确认退出",
                );
            }
        };
        self.management_status = status.clone();
        self.management_discarded_version = status.discarded_version;
        self.management_exit_version = None;
        self.publish_current_snapshot();

        if let Some(pending) = self.pending_exit.as_ref() {
            let request = ExitRequest {
                id: pending.request_id,
                intent: pending.intent,
                buddy_context: Some(pending.buddy_context.clone()),
            };
            if let Err(message) = self.handle_exit(request) {
                self.fail_exit(&message);
                return DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: Some(failure_with_disposition(
                        "exit_after_discard_failed",
                        &message,
                        "unknown",
                    )),
                };
            }
        }
        DispatchResult {
            snapshot: Some(self.latest_snapshot()),
            error: None,
        }
    }

    fn cancel_management_exit(
        &mut self,
        version: u64,
        intent: CloseIntent,
        context: BuddyContext,
    ) -> DispatchResult {
        self.sync_management_drafts();
        if let Some(error) = self.exit_context_failure(&context) {
            return DispatchResult {
                snapshot: Some(self.latest_snapshot()),
                error: Some(error),
            };
        }
        if self.management_exit_version != Some(version)
            || self.management_status.version != version
            || self.management_status.context.as_ref() != Some(&context)
            || self
                .pending_exit
                .as_ref()
                .map(|pending| close_intent(pending.intent))
                != Some(intent)
        {
            return self.command_failure(
                "management_confirmation_stale",
                "管理编辑已变化，请重新确认退出",
            );
        }
        self.management_exit_version = None;
        self.pending_exit = None;
        self.publish_current_snapshot();
        DispatchResult {
            snapshot: Some(self.latest_snapshot()),
            error: None,
        }
    }

    /// 唯一的关闭收尾：意图 `None` 表示只关闭聊天；`CloseChat` 表示原生关窗；
    /// `QuitApplication` 表示关闭聊天后结束进程。物理隐藏先完成，再请求事件循环停止。
    fn finish_close(&mut self, intent: Option<ExitIntent>) -> Result<(), String> {
        self.pending_exit = None;
        self.management_exit_version = None;
        let hidden = self.port.hide("main");
        if hidden.is_err() {
            self.port.notify_error("window_hide_failed");
        }
        if intent == Some(ExitIntent::QuitApplication) {
            // 进程退出会关闭窗口；隐藏失败不能让退出请求丢失。
            self.shutdown_code = Some(0);
            return Ok(());
        }
        hidden
    }

    /// 把受理到的退出请求和当前关联意图合并。新请求只会提升意图，
    /// 后来的普通关闭请求不会清掉或降级它；更旧的通知也不能覆盖新意图。
    fn associate_exit(
        &mut self,
        request: ExitRequest,
        context: BuddyContext,
    ) -> Result<ExitIntent, String> {
        let chat_draft_revisions = self.draft_revision_set()?;
        self.sync_management_drafts();
        if self.management_status.context.as_ref() != Some(&context) {
            if self.management_status.dirty {
                return Err("管理草稿属于另一个伙伴上下文".into());
            }
            self.management_status = self
                .management_drafts
                .rebind(context.clone())
                .map_err(|_| "管理草稿属于另一个伙伴上下文".to_owned())?;
        }
        let management_draft_version = self.management_status.version;
        match &mut self.pending_exit {
            Some(active) if request.id >= active.request_id => {
                let confirmed = (active.management_draft_version == management_draft_version)
                    .then_some(active.management_discard_confirmed_version)
                    .flatten();
                active.intent = active.intent.precedence(request.intent);
                active.request_id = request.id;
                active.buddy_context = context;
                active.chat_draft_revisions = chat_draft_revisions;
                active.management_draft_version = management_draft_version;
                active.management_discard_confirmed_version = confirmed;
                active.host_confirmation = false;
                Ok(active.intent)
            }
            Some(active) => Ok(active.intent),
            None => {
                self.pending_exit = Some(PendingExit {
                    intent: request.intent,
                    request_id: request.id,
                    buddy_context: context,
                    chat_draft_revisions,
                    management_draft_version,
                    management_discard_confirmed_version: None,
                    host_confirmation: false,
                });
                Ok(request.intent)
            }
        }
    }

    /// 原生关窗、托盘退出和 main 的 quit 命令共用入口。退出意图独立于普通队列。
    fn handle_exit(&mut self, request: ExitRequest) -> Result<(), String> {
        let context = request
            .buddy_context
            .clone()
            .map(Ok)
            .unwrap_or_else(|| self.active_context())
            .inspect_err(|message| self.fail_exit(message))?;
        if let Some(error) = self.exit_context_failure(&context) {
            return Err(error.message);
        }
        if request.intent == ExitIntent::QuitApplication {
            self.cancel_material_candidate();
        }
        self.sync_management_drafts();
        if self
            .buddy_storage
            .as_ref()
            .is_some_and(BuddyStorage::registry_is_unknown)
        {
            let intent = self.associate_exit(request, context.clone())?;
            if intent != ExitIntent::QuitApplication {
                return Err("伙伴档案状态未知；仅允许显式确认整应用退出".into());
            }
            let management_confirmed = self.pending_exit.as_ref().is_some_and(|pending| {
                pending.management_discard_confirmed_version == Some(self.management_status.version)
            });
            if self.management_status.dirty && !management_confirmed {
                self.management_exit_version = Some(self.management_status.version);
                self.publish_current_snapshot();
                return self.present_confirmation();
            }
            let revisions = self.draft_revision_set()?;
            self.preflight_quit_drafts(&revisions, false)?;
            let pending = self.pending_exit.as_mut().expect("exit was associated");
            pending.chat_draft_revisions = revisions.clone();
            pending.management_draft_version = self.management_status.version;
            if revisions.iter().any(|draft| draft.has_unsubmitted_drafts) {
                pending.host_confirmation = true;
                self.publish_current_snapshot();
                return self.present_confirmation();
            }
            if self.management_status.dirty && management_confirmed {
                let status = self
                    .management_drafts
                    .confirm_discard(self.management_status.version)
                    .map_err(|_| "管理草稿已变化，请重新确认退出".to_owned())?;
                self.management_status = status.clone();
                self.management_discarded_version = status.discarded_version;
            }
            self.finish_close(Some(ExitIntent::QuitApplication))?;
            return Ok(());
        }
        let current = self
            .app_snapshot()
            .inspect_err(|message| self.fail_exit(message))?;
        let intent = self
            .associate_exit(request, context.clone())
            .inspect_err(|message| self.fail_exit(message))?;
        let pending_before = self.pending_exit.clone();
        let management_discard_confirmed = pending_before.as_ref().is_some_and(|pending| {
            pending.intent == ExitIntent::QuitApplication
                && pending.buddy_context == context
                && pending.management_discard_confirmed_version
                    == Some(self.management_status.version)
        });
        if self.management_status.dirty && !management_discard_confirmed {
            self.management_exit_version = Some(self.management_status.version);
            if chat_visibility(&current) != PanelVisibility::Visible
                && let Err(error) = self.show_main(self.now_ms())
            {
                self.pending_exit = None;
                self.management_exit_version = None;
                self.fail_exit(&error);
                return Err(error);
            }
            self.publish_current_snapshot();
            return self.present_confirmation();
        }
        self.management_exit_version = None;
        let all_draft_revisions = if intent == ExitIntent::QuitApplication {
            self.draft_revision_set()
                .inspect_err(|message| self.fail_exit(message))?
        } else {
            Vec::new()
        };
        let drafts_pending = all_draft_revisions
            .iter()
            .any(|entry| entry.has_unsubmitted_drafts);
        if current.revoked && intent == ExitIntent::CloseChat {
            return self.finish_exit_request();
        }
        if chat_visibility(&current) == PanelVisibility::Closed && !drafts_pending {
            return self.finish_exit_request();
        }
        if current
            .panels
            .iter()
            .any(|panel| panel.kind == PanelKind::Chat && panel.confirmation)
        {
            // 已有未确认的关闭请求：提升意图并恢复确认入口，不重复关闭使核心确认陈旧。
            let mut snapshot = self.latest_snapshot();
            snapshot.close_intent = Some(
                match self.pending_exit.as_ref().map(|pending| pending.intent) {
                    Some(ExitIntent::QuitApplication) => CloseIntent::QuitApplication,
                    _ => CloseIntent::CloseChat,
                },
            );
            self.publish_snapshot(&snapshot);
            return self.present_confirmation();
        }

        // 隐藏时先按业务规则重新打开：隐藏会卸载视图并清空展示缓存，
        // 重新打开才能恢复草稿与确认入口，取消后也能继续编辑/发送。
        if !current.revoked && chat_visibility(&current) != PanelVisibility::Visible {
            let opened = self.apply_business(Action::Open(PanelKind::Chat), self.now_ms());
            if let Some(error) = opened.error {
                self.pending_exit = None;
                self.fail_exit(&error.message);
                return Err(error.message);
            }
        }

        let active_draft_is_dirty = self
            .app_snapshot()
            .map(|snapshot| !snapshot.chat.draft.is_empty())
            .inspect_err(|message| self.fail_exit(message))?;
        if intent == ExitIntent::QuitApplication && drafts_pending && !active_draft_is_dirty {
            if let Some(pending) = &mut self.pending_exit {
                pending.chat_draft_revisions = all_draft_revisions;
                pending.management_draft_version = self.management_status.version;
                pending.host_confirmation = true;
            }
            self.publish_current_snapshot();
            return self.present_confirmation();
        }

        let result = self.apply_business(Action::Close(PanelKind::Chat), self.now_ms());
        if let Some(error) = &result.error {
            if matches!(error.code.as_str(), "panel_not_open" | "revoked") {
                if intent == ExitIntent::CloseChat {
                    return self.finish_exit_request();
                }
                if !self
                    .draft_revision_set()?
                    .iter()
                    .any(|entry| entry.has_unsubmitted_drafts)
                {
                    return self.finish_exit_request();
                }
            }
            self.pending_exit = None;
            self.fail_exit(&error.message);
            return Err(error.message.clone());
        }
        let Some(snapshot) = result.snapshot else {
            return Err("business close snapshot unavailable".into());
        };
        if snapshot.close_confirmation {
            self.present_confirmation()
        } else {
            self.finish_exit_request()
        }
    }

    fn finish_exit_request(&mut self) -> Result<(), String> {
        let Some(pending) = self.pending_exit.clone() else {
            return Ok(());
        };
        if pending.intent == ExitIntent::QuitApplication {
            let current = self.draft_revision_set()?;
            if current.iter().any(|entry| entry.has_unsubmitted_drafts) {
                return Err("全伙伴会话仍有未保存草稿，必须完成退出确认".into());
            }
            let current_ids = current
                .iter()
                .map(|entry| entry.buddy_id.as_str())
                .collect::<Vec<_>>();
            let expected_ids = pending
                .chat_draft_revisions
                .iter()
                .map(|entry| entry.buddy_id.as_str())
                .collect::<Vec<_>>();
            if current_ids != expected_ids {
                return Err("已加载伙伴集合已变化，请重新确认退出".into());
            }
            let mailbox = self.management_drafts.clone();
            let result = mailbox.with_version(pending.management_draft_version, |status| {
                let management_confirmed =
                    pending.management_discard_confirmed_version == Some(status.version);
                if status.context.as_ref() != Some(&pending.buddy_context)
                    || (status.dirty && !management_confirmed)
                {
                    return Err("管理草稿已变化，请重新确认退出".to_owned());
                }
                if status.dirty {
                    status.dirty = false;
                    status.kind = ManagementDraftKind::None;
                    status.discarded_version = Some(status.version);
                }
                self.management_status = status.clone();
                self.management_discarded_version = status.discarded_version;
                self.management_exit_version = None;
                self.publish_current_snapshot();
                self.finish_close(Some(ExitIntent::QuitApplication))
            });
            return match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(detail)) => {
                    self.fail_exit(&detail);
                    Err(detail)
                }
                Err(status) => {
                    self.management_status = status.clone();
                    self.management_discarded_version = status.discarded_version;
                    self.fail_exit("management confirmation became stale");
                    Err("management confirmation became stale".to_owned())
                }
            };
        }
        self.pending_exit = None;
        match self.finish_close(Some(pending.intent)) {
            Ok(()) => Ok(()),
            Err(detail) => {
                self.fail_exit(&detail);
                Err(detail)
            }
        }
    }

    fn fail_exit(&mut self, detail: &str) {
        eprintln!("moe-buddy-tauri: exit request failed: {detail}");
        self.port.notify_error("exit_failed");
    }

    /// 隐藏中的确认必须恢复可操作入口：显示、取消最小化并聚焦。
    fn present_confirmation(&mut self) -> Result<(), String> {
        let visible = self.port.is_visible("main").unwrap_or(false);
        if !visible && let Err(detail) = self.port.show("main") {
            self.port.notify_error("window_show_failed");
            return Err(detail);
        }
        self.restore_and_focus_main()
    }

    fn handle_close(&mut self, request: CloseRequest) {
        let now_ms = self.now_ms();
        match request {
            CloseRequest::CloseChat { context, reply } => {
                let result = self
                    .exit_context_failure(&context)
                    .map(|error| DispatchResult {
                        snapshot: Some(self.latest_snapshot()),
                        error: Some(error),
                    })
                    .unwrap_or_else(|| self.close_from_ui(context, now_ms));
                let _ = reply.send(result);
            }
            CloseRequest::ConfirmClose {
                discard,
                expected_intent,
                context,
                reply,
            } => {
                let result = self
                    .exit_context_failure(&context)
                    .map(|error| DispatchResult {
                        snapshot: Some(self.latest_snapshot()),
                        error: Some(error),
                    })
                    .unwrap_or_else(|| {
                        self.confirm_close(discard, expected_intent, context, now_ms)
                    });
                let _ = reply.send(result);
            }
            CloseRequest::ConfirmManagementDiscard {
                version,
                intent,
                context,
                reply,
            } => {
                let result = self
                    .exit_context_failure(&context)
                    .map(|error| DispatchResult {
                        snapshot: Some(self.latest_snapshot()),
                        error: Some(error),
                    })
                    .unwrap_or_else(|| self.confirm_management_discard(version, intent, context));
                let _ = reply.send(result);
            }
            CloseRequest::CancelManagementExit {
                version,
                intent,
                context,
                reply,
            } => {
                let result = self
                    .exit_context_failure(&context)
                    .map(|error| DispatchResult {
                        snapshot: Some(self.latest_snapshot()),
                        error: Some(error),
                    })
                    .unwrap_or_else(|| self.cancel_management_exit(version, intent, context));
                let _ = reply.send(result);
            }
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Dispatch(command, context, reply) => {
                let previous_context = self.active_context().ok();
                let result = self.dispatch_command(command, context, self.now_ms());
                if previous_context != self.active_context().ok()
                    || self.material_lifecycle_failure().is_some()
                {
                    self.cancel_material_candidate();
                }
                let _ = reply.send(result);
            }
            Request::HitMask {
                width,
                height,
                alpha,
                frame,
                reply,
            } => {
                let empty_frame =
                    self.frame.lock().expect("frame mutex poisoned").source == "empty";
                let empty_mask_invalid =
                    empty_frame && (width != 1 || height != 1 || alpha.as_slice() != [0]);
                let applied = if self.current_material_triple() != frame {
                    Err("hit mask frame is stale".into())
                } else if self.hit_mask_clear_failed {
                    Err("material hit mask recovery requires explicit stop or unload".into())
                } else if empty_mask_invalid {
                    Err("empty frames only accept a 1x1 zero-alpha mask".into())
                } else {
                    self.port.apply_hit_mask(width, height, &alpha)
                };
                if applied.is_ok() {
                    self.hit_mask = (width != 1 || height != 1 || alpha != [0])
                        .then_some((width, height, alpha, frame));
                }
                let _ = reply.send(applied);
            }
            Request::MaterialCommand {
                command,
                context,
                reply,
            } => {
                let result = self.handle_material_command(command, context);
                let _ = reply.send(result);
            }
            Request::MaterialPixels { frame, reply } => {
                let result = self.material_host.frame_pixels((
                    frame.resource_generation,
                    frame.playback_generation,
                    frame.presentation_sequence,
                ));
                let _ = reply.send(result);
            }
            Request::MaterialState(reply) => {
                let _ = reply.send(self.material_host.state());
            }
            Request::TogglePause(reply) => {
                let outcome = self.toggle_pause();
                let _ = reply.send(outcome);
            }
            Request::Snapshot(reply) => {
                self.sync_management_drafts();
                let result = DispatchResult {
                    snapshot: Some(self.latest_snapshot()),
                    error: None,
                };
                let _ = reply.send(result);
            }
            Request::ManagementDraftState {
                version,
                dirty,
                kind,
                context,
                reply,
            } => {
                let result = self.accept_management_draft_state(version, dirty, kind, context);
                let _ = reply.send(result);
            }
            Request::SetVisibility {
                label,
                visible,
                context,
                reply,
            } => {
                let outcome = match context {
                    Some(context) => self.window_context_failure(&context).map_or_else(
                        || self.set_visibility(label, visible, self.now_ms()),
                        |error| Err(error.message),
                    ),
                    None if label == "main" && visible => self
                        .port
                        .show("main")
                        .and_then(|()| self.restore_and_focus_main()),
                    None if label == "pet" => self.change_window("pet", visible),
                    None => Err("presentation-only window request is not allowed".into()),
                };
                let _ = reply.send(outcome);
            }
        }
    }

    fn pet_visible(&mut self) -> bool {
        self.port.is_visible("pet").unwrap_or(true)
    }

    fn current_material_triple(&self) -> MaterialFrameTriple {
        let frame = self.frame.lock().expect("frame mutex poisoned");
        MaterialFrameTriple {
            resource_generation: frame.resource_generation,
            playback_generation: frame.playback_generation,
            presentation_sequence: frame.presentation_sequence,
        }
    }

    fn cancel_material_candidate(&mut self) {
        if let Some(task) = self.material_load.as_ref() {
            task.cancel.store(true, Ordering::Release);
            if self.material_host.state().loading {
                let _ = self.material_host.set_loading(false);
                self.publish_material_state();
            }
        }
    }

    fn publish_material_state(&mut self) {
        let state = self.material_host.state();
        if let Err(error) = self.port.emit_material(state) {
            eprintln!("moe-buddy-tauri: material state event failed: {error}");
        }
    }

    fn material_lifecycle_failure(&self) -> Option<Failure> {
        let snapshot = self.latest_snapshot();
        if snapshot.revoked {
            return Some(failure(
                "material_lifecycle_blocked",
                "授权已撤回，不能加载或变更素材",
            ));
        }
        if self.shutdown_code.is_some()
            || self.pending_exit.is_some()
            || snapshot.close_confirmation
            || snapshot.management_exit.is_some()
        {
            return Some(failure(
                "material_lifecycle_blocked",
                "关闭或退出处理中，不能加载或变更素材",
            ));
        }
        None
    }

    fn handle_material_command(
        &mut self,
        command: MaterialCommand,
        context: BuddyContext,
    ) -> MaterialReply {
        let context_error = self
            .context_failure(&context)
            .or_else(|| self.material_lifecycle_failure());
        if let Some(error) = context_error {
            return MaterialReply {
                ok: false,
                state: self.material_host.state(),
                error: Some(error),
            };
        }
        let (resource, playback) = command.generations();
        let state = self.material_host.state();
        let initial_material_revision = state.revision;
        if resource != state.resource_generation || playback != state.playback_generation {
            return MaterialReply {
                ok: false,
                state,
                error: Some(failure(
                    "material_generation_stale",
                    "素材状态已变化，请重新读取后重试",
                )),
            };
        }
        if let Err(code) = self.material_host.preflight_command(&command) {
            return MaterialReply {
                ok: false,
                state,
                error: Some(failure(&code, material_error_message(&code))),
            };
        }
        if self.hit_mask_clear_failed {
            let is_recovery = matches!(
                command,
                MaterialCommand::Stop { .. } | MaterialCommand::Unload { .. }
            );
            if !is_recovery {
                return MaterialReply {
                    ok: false,
                    state,
                    error: Some(failure_with_disposition(
                        "material_hit_mask_clear_failed",
                        "请显式停止或卸载以恢复命中区域",
                        "not_executed",
                    )),
                };
            }
            if self.port.apply_hit_mask(1, 1, &[0]).is_err() {
                return MaterialReply {
                    ok: false,
                    state,
                    error: Some(failure_unknown(
                        "material_hit_mask_clear_failed",
                        "无法确认旧命中区域已清除，请再次显式停止或卸载",
                    )),
                };
            }
            self.hit_mask = None;
            self.hit_mask_clear_failed = false;
            if let Err(code) = self.material_host.recover_presentation("stopped") {
                return MaterialReply {
                    ok: false,
                    state: self.material_host.state(),
                    error: Some(failure_unknown(&code, material_error_message(&code))),
                };
            }
        }
        if matches!(
            command,
            MaterialCommand::Load { .. }
                | MaterialCommand::Play { .. }
                | MaterialCommand::Pause { .. }
                | MaterialCommand::Resume { .. }
                | MaterialCommand::Builtin { .. }
        ) && self.material_load.is_some()
        {
            return MaterialReply {
                ok: false,
                state,
                error: Some(failure(
                    "material_busy",
                    material_error_message("material_busy"),
                )),
            };
        }
        let loading = self.material_load.is_some();
        let source = state.source.clone();
        let is_load = matches!(&command, MaterialCommand::Load { .. });
        let result = match command {
            MaterialCommand::Load { path, .. } => {
                if loading {
                    Err("material_busy".into())
                } else {
                    self.start_material_load(path, context)
                }
            }
            MaterialCommand::Play { action_id, .. } if !loading && source == "builtin" => {
                if action_id != "idle" {
                    Err("unknown_material_action".into())
                } else {
                    match self.stop_builtin_material() {
                        Err(error) => Err(error),
                        Ok(()) => self.start_builtin_material(),
                    }
                }
            }
            MaterialCommand::Play { action_id, .. } if !loading => {
                self.material_host.play(&action_id)
            }
            MaterialCommand::Pause { .. } if !loading && source == "builtin" => {
                self.pause_builtin_material()
            }
            MaterialCommand::Pause { .. } if !loading => self.material_host.pause(),
            MaterialCommand::Resume { .. } if !loading && source == "builtin" => {
                self.resume_builtin_material()
            }
            MaterialCommand::Resume { .. } if !loading => self.material_host.resume(),
            MaterialCommand::Stop { .. } => {
                if let Some(task) = self.material_load.as_ref() {
                    task.cancel.store(true, Ordering::Release);
                }
                let _ = self.material_host.set_loading(false);
                if source == "builtin" {
                    match self.stop_builtin_material() {
                        Ok(()) => self.material_host.set_builtin_status("stopped"),
                        Err(error) => Err(error),
                    }
                } else {
                    self.material_host.stop()
                }
            }
            MaterialCommand::Unload { .. } => {
                if let Some(task) = self.material_load.as_ref() {
                    task.cancel.store(true, Ordering::Release);
                }
                let _ = self.material_host.set_loading(false);
                if source == "builtin"
                    && let Err(_error) = self.stop_builtin_material()
                {
                    return MaterialReply {
                        ok: false,
                        state: self.material_host.state(),
                        error: Some(failure_unknown(
                            "material_stop_failed",
                            "无法确认素材播放已停止",
                        )),
                    };
                }
                self.material_host.unload()
            }
            MaterialCommand::Builtin { .. } if !loading => match self.stop_builtin_material() {
                Err(error) => Err(error),
                Ok(()) => match self.material_host.builtin() {
                    Err(error) => Err(error),
                    Ok(()) => self.start_builtin_material(),
                },
            },
            _ => Err("material_busy".into()),
        };
        let state_changed = self.material_host.state().revision != initial_material_revision;
        if result.is_ok() || state_changed {
            if !is_load {
                self.tick(false, 0);
            }
            self.publish_material_state();
        }
        let state = self.material_host.state();
        match result {
            Ok(())
                if state
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == "material_hit_mask_clear_failed") =>
            {
                MaterialReply {
                    ok: false,
                    state,
                    error: Some(failure_unknown(
                        "material_hit_mask_clear_failed",
                        "无法确认旧命中区域已清除，请显式重试停止",
                    )),
                }
            }
            Ok(()) => MaterialReply {
                ok: true,
                state,
                error: None,
            },
            Err(code) => MaterialReply {
                ok: false,
                state,
                error: Some(
                    if code == "material_stop_failed" || code == "material_playback_failed" {
                        failure_unknown(&code, material_error_message(&code))
                    } else if state_changed {
                        failure_with_disposition(&code, material_error_message(&code), "settled")
                    } else {
                        failure(&code, material_error_message(&code))
                    },
                ),
            },
        }
    }

    fn start_material_load(&mut self, path: String, context: BuddyContext) -> Result<(), String> {
        self.start_material_load_with_barrier(path, context, None, None)
    }

    fn start_material_load_with_barrier(
        &mut self,
        path: String,
        context: BuddyContext,
        barrier: Option<Arc<std::sync::Barrier>>,
        candidate_dropped: Option<Arc<AtomicBool>>,
    ) -> Result<(), String> {
        if !std::path::Path::new(&path).is_absolute() {
            return Err("material_path_invalid".into());
        }
        self.material_host.ensure_revision_available()?;
        let path = std::path::PathBuf::from(path);
        let root = if path.file_name().is_some_and(|name| name == "manifest.json") {
            path.parent().ok_or("material_path_invalid")?.to_path_buf()
        } else {
            path
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let thread_barrier = barrier;
        let thread_candidate_dropped = candidate_dropped;
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("moe-material-loader".into())
            .spawn(move || {
                let result = if thread_cancel.load(Ordering::Acquire) {
                    Err(crate::material::MaterialError::Cancelled)
                } else {
                    crate::material::MaterialPack::load_cancellable(&root, &thread_cancel)
                };
                if let Some(barrier) = thread_barrier {
                    barrier.wait();
                    barrier.wait();
                }
                if thread_cancel.load(Ordering::Acquire) {
                    drop(result);
                    if let Some(marker) = thread_candidate_dropped {
                        marker.store(true, Ordering::Release);
                    }
                    return;
                }
                if !thread_cancel.load(Ordering::Acquire) {
                    let _ = sender.send(result);
                }
            })
            .map_err(|_| "material_loader_unavailable")?;
        let state = self.material_host.state();
        self.material_host.set_loading(true)?;
        self.material_load = Some(MaterialLoadTask {
            context,
            resource_generation: state.resource_generation,
            playback_generation: state.playback_generation,
            cancel,
            receiver,
            worker: Some(worker),
            pending: None,
        });
        self.publish_material_state();
        Ok(())
    }

    fn poll_material_load(&mut self) {
        if self.material_load.is_none() {
            return;
        }
        if self
            .material_load
            .as_ref()
            .is_some_and(|task| task.cancel.load(Ordering::Acquire))
        {
            if self.material_load.as_ref().is_some_and(|task| {
                task.worker
                    .as_ref()
                    .is_some_and(thread::JoinHandle::is_finished)
            }) && let Some(mut task) = self.material_load.take()
            {
                if let Some(worker) = task.worker.take() {
                    let _ = worker.join();
                }
                while task.receiver.try_recv().is_ok() {}
                drop(task);
                self.publish_material_state();
            }
            return;
        }
        let still_current = {
            let task = self.material_load.as_ref().expect("checked material task");
            self.context_failure(&task.context).is_none()
                && self.material_lifecycle_failure().is_none()
                && self.material_host.state().resource_generation == task.resource_generation
                && self.material_host.state().playback_generation == task.playback_generation
        };
        if !still_current {
            if let Some(task) = self.material_load.as_ref() {
                task.cancel.store(true, Ordering::Release);
            }
            let _ = self.material_host.set_loading(false);
            self.publish_material_state();
            return;
        }
        let received = self
            .material_load
            .as_ref()
            .filter(|task| task.pending.is_none())
            .map(|task| task.receiver.try_recv());
        let disconnected = matches!(
            received.as_ref(),
            Some(Err(mpsc::TryRecvError::Disconnected))
        );
        if let Some(Ok(result)) = received
            && let Some(task) = self.material_load.as_mut()
        {
            task.pending = Some(result);
        }
        if self.material_load.as_ref().is_some_and(|task| {
            task.pending.is_some()
                && task
                    .worker
                    .as_ref()
                    .is_some_and(thread::JoinHandle::is_finished)
        }) {
            let mut task = self.material_load.take().expect("checked material task");
            if let Some(worker) = task.worker.take() {
                let _ = worker.join();
            }
            let result = task.pending.take().expect("checked material result");
            while task.receiver.try_recv().is_ok() {}
            drop(task);
            match result {
                Ok(pack) => {
                    if self.material_host.state().source == "builtin"
                        && let Some(animator) = self.animator.as_mut()
                        && let Err(error) = animator.stop()
                    {
                        self.material_host.retain_backend_error(error);
                        let _ = self
                            .material_host
                            .set_playback_fault("material_stop_failed", "");
                        self.publish_material_state();
                        return;
                    }
                    match self.material_host.install(pack) {
                        Ok(()) => {
                            self.emit_material_frame();
                            self.publish_material_state();
                        }
                        Err(code) => {
                            let _ = self
                                .material_host
                                .set_error(&code, material_error_message(&code));
                            self.publish_material_state();
                        }
                    }
                }
                Err(error) => {
                    let cancelled = matches!(error, crate::material::MaterialError::Cancelled);
                    self.material_backend_error = Some(Box::new(error));
                    let code = if cancelled {
                        "material_load_cancelled"
                    } else {
                        "material_package_invalid"
                    };
                    let _ = self
                        .material_host
                        .set_error(code, material_error_message(code));
                    self.publish_material_state();
                }
            }
        } else if disconnected
            && self.material_load.as_ref().is_some_and(|task| {
                task.worker
                    .as_ref()
                    .is_some_and(thread::JoinHandle::is_finished)
            })
            && let Some(mut task) = self.material_load.take()
        {
            if let Some(worker) = task.worker.take() {
                let _ = worker.join();
            }
            while task.receiver.try_recv().is_ok() {}
            drop(task);
            let _ = self
                .material_host
                .set_error("material_loader_failed", "素材加载失败，旧素材保持不变");
            self.publish_material_state();
        }
    }

    fn emit_material_frame(&mut self) {
        if self.hit_mask_clear_failed {
            return;
        }
        if let Err(error) = self.material_host.set_presentation_sequence(
            self.presentation_sequence
                .max(self.material_host.frame_triple().2),
        ) {
            eprintln!("moe-buddy-tauri: material presentation sequence exhausted: {error}");
            return;
        }
        let frame = self.material_host.frame_snapshot();
        if self.last_frame.as_ref().is_some_and(|previous| {
            previous.resource_generation != frame.resource_generation
                || previous.playback_generation != frame.playback_generation
        }) {
            if !self.clear_hit_mask_before_transition() {
                return;
            }
            self.hit_mask = None;
        }
        *self.frame.lock().expect("frame mutex poisoned") = frame.clone();
        if self.last_frame.as_ref() != Some(&frame) {
            if let Err(error) = self.port.emit_frame(frame.clone()) {
                eprintln!("moe-buddy-tauri: {error}");
            }
            self.last_frame = Some(frame);
        }
    }

    fn tick(&mut self, active: bool, delta_ms: u64) {
        if self.hit_mask_clear_failed {
            return;
        }
        self.presentation_sequence = self
            .presentation_sequence
            .max(self.material_host.frame_triple().2);
        self.poll_material_load();
        let source = self.material_host.state().source;
        if source == "package" {
            let material_revision = self.material_host.state().revision;
            if active
                && self.material_host.advance(delta_ms).is_err()
                && let Err(error) = self
                    .material_host
                    .set_playback_fault("material_playback_failed", "素材播放失败，已停止当前动作")
            {
                eprintln!("moe-buddy-tauri: unable to close failed material playback: {error}");
            }
            if self.material_host.state().revision != material_revision {
                self.publish_material_state();
            }
            self.emit_material_frame();
        } else if source == "empty" {
            self.emit_material_frame();
        }
        if source == "builtin"
            && matches!(
                self.material_host.state().status.as_str(),
                "playing" | "paused"
            )
            && active
            && !self.animation_faulted
        {
            if self.presentation_sequence >= MAX_PRESENTATION_SEQUENCE
                || self.material_host.ensure_revision_available().is_err()
            {
                self.port.notify_error("animation_generation_exhausted");
                return;
            }
            let mut failed = false;
            if let Some(animator) = self.animator.as_mut()
                && !animator.snapshot().paused
                && let Err(error) = animator.advance(delta_ms)
            {
                failed = true;
                eprintln!("moe-buddy-tauri: animation advance failed: {error}");
                if let Err(pause_error) = animator.pause() {
                    eprintln!(
                        "moe-buddy-tauri: animation pause after failure failed: {pause_error}"
                    );
                }
            }
            if failed {
                self.animation_faulted = true;
                if !self.animation_error_reported {
                    self.animation_error_reported = true;
                    self.port.notify_error("animation_advance_failed");
                }
            }
        }
        if let Some((width, height, alpha, frame)) = self.hit_mask.take()
            && self.current_material_triple() == frame
        {
            self.port.poll_hit_mask(width, height, &alpha);
            self.hit_mask = Some((width, height, alpha, frame));
        }
        if source != "builtin" {
            return;
        }
        let descriptor = material_descriptor();
        let descriptor_valid = material_descriptor_is_valid(descriptor);
        let playback = if descriptor_valid {
            match self.animator.as_mut() {
                Some(animator) => match animator.playback_progress() {
                    Ok(progress) => progress,
                    Err(error) => {
                        eprintln!("moe-buddy-tauri: animation progress failed: {error}");
                        self.animation_faulted = true;
                        if !self.animation_error_reported {
                            self.animation_error_reported = true;
                            self.port.notify_error("animation_progress_failed");
                        }
                        None
                    }
                },
                None => None,
            }
        } else {
            eprintln!("moe-buddy-tauri: invalid animation material descriptor");
            self.animation_faulted = true;
            if !self.animation_error_reported {
                self.animation_error_reported = true;
                self.port.notify_error("animation_material_invalid");
            }
            None
        };
        let frame_index = match frame_index_for_progress(
            playback.map(|position| position.elapsed_ms),
            descriptor,
        ) {
            Ok(Some(index)) => index,
            Ok(None) => 0,
            Err(()) => {
                eprintln!("moe-buddy-tauri: animation progress exceeded clip duration");
                self.animation_faulted = true;
                if !self.animation_error_reported {
                    self.animation_error_reported = true;
                    self.port.notify_error("animation_progress_out_of_bounds");
                }
                0
            }
        };
        let animation = self.animator.as_ref().map(|animator| animator.snapshot());
        let faulted =
            self.animation_faulted || animation.as_ref().is_some_and(|state| state.faulted);
        let paused = faulted || animation.as_ref().map(|state| state.paused).unwrap_or(true);
        let changed = self.last_frame.as_ref().is_none_or(|previous| {
            previous.frame_index != Some(frame_index)
                || previous.paused != paused
                || previous.faulted != faulted
                || previous.resource_generation != self.material_host.state().resource_generation
                || previous.playback_generation != self.material_host.state().playback_generation
        });
        if changed && self.last_frame.is_some() {
            let Some(sequence) = self
                .presentation_sequence
                .checked_add(1)
                .filter(|sequence| *sequence <= MAX_PRESENTATION_SEQUENCE)
            else {
                return;
            };
            self.presentation_sequence = sequence;
        }
        let mut current = FrameSnapshot::builtin_with_sequence(
            frame_index,
            paused,
            faulted,
            self.presentation_sequence,
        );
        current.resource_generation = self.material_host.state().resource_generation;
        current.playback_generation = self.material_host.state().playback_generation;
        if self.material_host.state().status == "stopped" {
            current.source = "empty".into();
            current.clip_id = None;
            current.frame_index = None;
            current.width = 0;
            current.height = 0;
        }
        if let Err(error) = self
            .material_host
            .set_presentation_sequence(self.presentation_sequence)
        {
            eprintln!("moe-buddy-tauri: material presentation sequence exhausted: {error}");
            return;
        }
        if self.last_frame.as_ref().is_some_and(|previous| {
            previous.resource_generation != current.resource_generation
                || previous.playback_generation != current.playback_generation
        }) {
            if !self.clear_hit_mask_before_transition() {
                return;
            }
            self.hit_mask = None;
        }
        *self.frame.lock().expect("frame mutex poisoned") = current.clone();
        if self.last_frame.as_ref() != Some(&current) {
            if let Err(error) = self.port.emit_frame(current.clone()) {
                eprintln!("moe-buddy-tauri: {error}");
            }
            self.last_frame = Some(current);
        }
    }

    fn should_shutdown(&self) -> bool {
        self.shutdown_code.is_some()
    }

    fn clear_hit_mask_before_transition(&mut self) -> bool {
        if self.hit_mask_clear_failed {
            return false;
        }
        match self.port.apply_hit_mask(1, 1, &[0]) {
            Ok(()) => true,
            Err(_) => {
                self.hit_mask_clear_failed = true;
                let _ = self
                    .material_host
                    .set_presentation_fault("material_hit_mask_clear_failed");
                self.port.notify_error("material_hit_mask_clear_failed");
                self.publish_material_state();
                false
            }
        }
    }

    fn stop_builtin_material(&mut self) -> Result<(), String> {
        let result = self
            .animator
            .as_mut()
            .ok_or("animation_unavailable")?
            .stop();
        if let Err(error) = result {
            self.material_host.retain_backend_error(error);
            let _ = self
                .material_host
                .set_playback_fault("material_stop_failed", "");
            return Err("material_stop_failed".into());
        }
        Ok(())
    }

    fn start_builtin_material(&mut self) -> Result<(), String> {
        let start = self
            .animator
            .as_mut()
            .ok_or("animation_unavailable")?
            .request_action("idle");
        if let Err(error) = start {
            self.material_host.retain_backend_error(error);
            let _ = self
                .material_host
                .set_playback_fault("material_start_failed", "");
            return Err("material_start_failed".into());
        }
        let active = self.animator.as_ref().is_some_and(|animator| {
            let snapshot = animator.snapshot();
            snapshot.action.as_deref() == Some("idle") && snapshot.ticket.is_some()
        });
        if !active {
            let _ = self
                .material_host
                .set_playback_fault("material_start_failed", "");
            return Err("material_start_failed".into());
        }
        self.material_host.restart_builtin()
    }

    fn pause_builtin_material(&mut self) -> Result<(), String> {
        self.validate_builtin_control("playing", "material_pause_unavailable")?;
        let result = self
            .animator
            .as_mut()
            .ok_or("animation_unavailable")?
            .pause();
        if let Err(error) = result {
            self.material_host.retain_backend_error(error);
            let _ = self
                .material_host
                .set_playback_fault("material_pause_failed", "");
            return Err("material_pause_failed".into());
        }
        self.material_host.set_builtin_status("paused")
    }

    fn resume_builtin_material(&mut self) -> Result<(), String> {
        self.validate_builtin_control("paused", "material_resume_unavailable")?;
        let result = self
            .animator
            .as_mut()
            .ok_or("animation_unavailable")?
            .resume();
        if let Err(error) = result {
            self.material_host.retain_backend_error(error);
            let _ = self
                .material_host
                .set_playback_fault("material_resume_failed", "");
            return Err("material_resume_failed".into());
        }
        self.material_host.set_builtin_status("playing")
    }

    fn validate_builtin_control(
        &self,
        expected_status: &str,
        error_code: &str,
    ) -> Result<(), String> {
        let state = self.material_host.state();
        let Some(animator) = self.animator.as_ref() else {
            return Err(error_code.into());
        };
        let snapshot = animator.snapshot();
        if state.source != "builtin"
            || state.status != expected_status
            || state.action_id.is_none()
            || snapshot.ticket.is_none()
            || snapshot.action.is_none()
            || snapshot.action != state.action_id
            || snapshot.paused != (expected_status == "paused")
        {
            return Err(error_code.into());
        }
        Ok(())
    }

    /// 先停止动画播放器并释放应用对象，再由调用方向事件循环发出退出请求。
    fn shutdown(&mut self) -> Option<i32> {
        if let Some(task) = self.material_load.as_ref() {
            task.cancel.store(true, Ordering::Release);
        }
        if let Some(task) = self.material_load.as_mut()
            && let Some(worker) = task.worker.take()
            && worker.join().is_err()
        {
            self.port.notify_error("material_loader_panicked");
            self.material_backend_error = Some(Box::new(std::io::Error::other(
                "material loader thread panicked during confirmed shutdown",
            )));
        }
        if let Some(task) = self.material_load.take() {
            while task.receiver.try_recv().is_ok() {}
            drop(task);
        }
        if let Err(error) = self.material_host.release_for_shutdown() {
            self.port.notify_error("material_release_unconfirmed");
            let _ = error;
        }
        if let Some(mut animator) = self.animator.take()
            && let Err(error) = animator.stop()
        {
            self.material_host.retain_backend_error(error);
            self.port.notify_error("animation_release_failed");
        }
        if self.hit_mask.is_some() {
            if self.port.apply_hit_mask(1, 1, &[0]).is_ok() {
                self.hit_mask = None;
                self.hit_mask_clear_failed = false;
            } else {
                self.hit_mask_clear_failed = true;
                self.port.notify_error("material_hit_mask_clear_failed");
            }
        }
        self.app.take();
        self.buddy_storage.take();
        self.model_requests.clear();
        self.buddy_runtime.clear();
        self.model_key = None;
        self.model_settings = None;
        self.shutdown_code.take()
    }

    fn request_event_loop_exit(&mut self, code: i32) {
        self.port.request_exit(code);
    }
}

fn run_worker_loop<P, F>(
    build: F,
    receiver: mpsc::Receiver<Request>,
    close_receiver: mpsc::Receiver<CloseRequest>,
    exit_mailbox: Arc<ExitMailbox>,
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    event_loop_alive: Arc<AtomicBool>,
) where
    P: HostPort,
    F: FnOnce() -> HostWorker<P>,
{
    let mut worker = build();
    let mut last_tick = Instant::now();
    'run: loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        worker.sync_management_drafts();
        // 关闭确认走优先小队列，普通队列满时仍可受理。
        loop {
            match close_receiver.try_recv() {
                Ok(request) => {
                    handle_close_with_exit_mailbox(&mut worker, request, &exit_mailbox, &receiver);
                    if worker.should_shutdown() {
                        break 'run;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'run,
            }
        }
        if exit_mailbox.has_pending() {
            // 先处理已经受理、排在退出请求之前的命令，确保最新草稿参与关闭确认。
            for _ in 0..MAX_DRAIN_PER_TURN {
                match receiver.try_recv() {
                    Ok(request) => {
                        worker.handle(request);
                        if worker.should_shutdown() {
                            break 'run;
                        }
                    }
                    Err(_) => break,
                }
            }
            while let Ok(request) = close_receiver.try_recv() {
                handle_close_with_exit_mailbox(&mut worker, request, &exit_mailbox, &receiver);
                if worker.should_shutdown() {
                    break 'run;
                }
            }
            if worker.should_shutdown() {
                break;
            }
            if let Some(request) = exit_mailbox.take()
                && let Err(error) = worker.handle_exit(request)
            {
                // handle_exit 已给出固定安全反馈；这里只保留内部诊断。
                eprintln!("moe-buddy-tauri: exit handling did not complete: {error}");
            }
            if worker.should_shutdown() {
                break;
            }
        }
        let active = worker.pet_visible();
        match receiver.recv_timeout(if active {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(250)
        }) {
            Ok(request) => {
                worker.handle(request);
                if worker.should_shutdown() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        worker.poll_external_request();
        worker.maintain_retention_if_due();
        let now = Instant::now();
        let delta = now
            .duration_since(last_tick)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        last_tick = now;
        worker.tick(active, delta);
        if worker.should_shutdown() {
            break;
        }
    }
    let exit_code = worker.shutdown().unwrap_or(0);
    // 先标记完成，再请求事件循环退出；事件循环据此确认 worker 已释放窗口访问。
    finished.store(true, Ordering::Release);
    if event_loop_alive.load(Ordering::Acquire) {
        worker.request_event_loop_exit(exit_code);
    }
}

fn handle_close_with_exit_mailbox<P: HostPort>(
    worker: &mut HostWorker<P>,
    request: CloseRequest,
    exit_mailbox: &ExitMailbox,
    receiver: &mpsc::Receiver<Request>,
) {
    let is_confirmation = matches!(
        &request,
        CloseRequest::ConfirmClose { .. }
            | CloseRequest::ConfirmManagementDiscard { .. }
            | CloseRequest::CancelManagementExit { .. }
    );
    if is_confirmation {
        let mut accepted = exit_mailbox.take();
        // Both confirmation queues must observe accepted edits, even when no
        // separate native/tray exit is pending. Keep each drain bounded.
        for _ in 0..MAX_DRAIN_PER_TURN {
            match receiver.try_recv() {
                Ok(request) => worker.handle(request),
                Err(_) => break,
            }
        }
        if let Some(next) = exit_mailbox.take() {
            accepted = Some(if let Some(mut exit) = accepted {
                exit.intent = exit.intent.precedence(next.intent);
                exit.id = exit.id.max(next.id);
                exit
            } else {
                next
            });
        }
        worker.sync_management_drafts();
        if let Some(accepted) = accepted
            && let Err(error) = worker.handle_exit(accepted)
        {
            worker.fail_exit(&error);
        }
    }
    worker.handle_close(request);
}

#[tauri::command]
async fn prototype_snapshot(
    window: tauri::WebviewWindow,
    state: State<'_, HostState>,
) -> Result<DispatchResult, String> {
    if !dispatch_source_allowed(window.label()) {
        return Ok(snapshot_response(
            window.label(),
            state
                .latest
                .lock()
                .expect("snapshot mutex poisoned")
                .clone(),
        ));
    }
    let (reply, receive) = mpsc::sync_channel(1);
    if state.sender.try_send(Request::Snapshot(reply)).is_err() {
        return Ok(DispatchResult {
            snapshot: None,
            error: Some(failure("queue_full", "host command queue is full")),
        });
    }
    Ok(await_dispatch(receive).await)
}

fn snapshot_response(label: &str, snapshot: Snapshot) -> DispatchResult {
    if !dispatch_source_allowed(label) {
        return DispatchResult {
            snapshot: None,
            error: Some(failure("command_source", "snapshot is main-window only")),
        };
    }
    DispatchResult {
        snapshot: Some(snapshot),
        error: None,
    }
}

async fn await_dispatch(receive: mpsc::Receiver<DispatchResult>) -> DispatchResult {
    tauri::async_runtime::spawn_blocking(move || receive.recv())
        .await
        .unwrap_or(Err(mpsc::RecvError))
        .unwrap_or_else(|_| DispatchResult {
            snapshot: None,
            error: Some(failure_unknown(
                "host_stopped",
                "host stopped before completing the command",
            )),
        })
}

#[tauri::command]
async fn prototype_dispatch(
    command: ChatCommand,
    context: BuddyContext,
    window: tauri::WebviewWindow,
    state: State<'_, HostState>,
) -> Result<DispatchResult, String> {
    if !dispatch_source_allowed(window.label()) {
        return Ok(DispatchResult {
            snapshot: None,
            error: Some(failure(
                "command_source",
                "chat commands are main-window only",
            )),
        });
    }
    if command_over_budget(&command) {
        return Ok(DispatchResult {
            snapshot: None,
            error: Some(failure("input_budget", "command exceeds the byte budget")),
        });
    }
    let (reply, receive) = mpsc::sync_channel(1);
    let queued = match command {
        ChatCommand::CloseChat => state
            .close_sender
            .try_send(CloseRequest::CloseChat { context, reply })
            .is_ok(),
        ChatCommand::ConfirmClose {
            discard,
            expected_intent,
        } => state
            .close_sender
            .try_send(CloseRequest::ConfirmClose {
                discard,
                expected_intent,
                context,
                reply,
            })
            .is_ok(),
        ChatCommand::ConfirmManagementDiscard { version, intent } => state
            .close_sender
            .try_send(CloseRequest::ConfirmManagementDiscard {
                version,
                intent,
                context,
                reply,
            })
            .is_ok(),
        ChatCommand::CancelManagementExit { version, intent } => state
            .close_sender
            .try_send(CloseRequest::CancelManagementExit {
                version,
                intent,
                context,
                reply,
            })
            .is_ok(),
        command => state
            .sender
            .try_send(Request::Dispatch(command, context, reply))
            .is_ok(),
    };
    if !queued {
        return Ok(DispatchResult {
            snapshot: None,
            error: Some(failure("queue_full", "host command queue is full")),
        });
    }
    Ok(await_dispatch(receive).await)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagementDraftAck {
    version: u64,
    dirty: bool,
}

#[tauri::command]
async fn prototype_management_draft_state(
    version: u64,
    dirty: bool,
    kind: ManagementDraftKind,
    context: BuddyContext,
    window: tauri::WebviewWindow,
    state: State<'_, HostState>,
) -> Result<ManagementDraftAck, String> {
    if !dispatch_source_allowed(window.label()) {
        return Err("management draft state is main-window only".into());
    }
    let (reply, receive) = mpsc::sync_channel(1);
    state
        .sender
        .try_send(Request::ManagementDraftState {
            version,
            dirty,
            kind,
            context,
            reply,
        })
        .map_err(|_| "host queue full".to_owned())?;
    tauri::async_runtime::spawn_blocking(move || receive.recv())
        .await
        .map_err(|_| "host stopped".to_owned())?
        .map_err(|_| "host stopped".to_owned())?
}

#[tauri::command]
async fn prototype_window(
    command: String,
    context: Option<BuddyContext>,
    window: tauri::WebviewWindow,
    state: State<'_, HostState>,
) -> Result<(), String> {
    if command.len() > 32 {
        return Err("unknown window command".into());
    }
    match command.as_str() {
        command @ ("show_chat" | "hide_chat" | "show_pet" | "hide_pet")
            if window.label() == "main" || (window.label() == "pet" && command == "show_chat") =>
        {
            let target = if command.ends_with("pet") {
                "pet"
            } else {
                "main"
            };
            let visible = command.starts_with("show");
            let context = if target == "main" && window.label() == "main" {
                Some(required_window_context(context)?)
            } else {
                None
            };
            let (reply, receive) = mpsc::sync_channel(1);
            state
                .sender
                .try_send(Request::SetVisibility {
                    label: target,
                    visible,
                    context,
                    reply,
                })
                .map_err(|_| "host queue full".to_owned())?;
            tauri::async_runtime::spawn_blocking(move || receive.recv())
                .await
                .map_err(|_| "host stopped".to_owned())?
                .map_err(|_| "host stopped".to_owned())?
        }
        "toggle_pause" if window.label() == "pet" || window.label() == "main" => {
            let (reply, receive) = mpsc::sync_channel(1);
            state
                .sender
                .try_send(Request::TogglePause(reply))
                .map_err(|_| "host queue full".to_owned())?;
            tauri::async_runtime::spawn_blocking(move || receive.recv())
                .await
                .map_err(|_| "host stopped".to_owned())?
                .map_err(|_| "host stopped".to_owned())?
        }
        "start_dragging" if window.label() == "pet" => {
            window.start_dragging().map_err(|e| e.to_string())
        }
        "quit" if window.label() == "main" => {
            let context = required_window_context(context)?;
            state
                .exit
                .submit_with_context(ExitIntent::QuitApplication, Some(context));
            Ok(())
        }
        _ => Err("command is not allowed from this window".to_owned()),
    }
}

fn required_window_context(context: Option<BuddyContext>) -> Result<BuddyContext, String> {
    context.ok_or_else(|| "window command requires buddy context".to_owned())
}

#[tauri::command]
async fn prototype_set_hit_mask(
    width: u16,
    height: u16,
    alpha: Vec<u8>,
    frame: MaterialFrameTriple,
    window: tauri::WebviewWindow,
    state: State<'_, HostState>,
) -> Result<(), String> {
    if window.label() != "pet" {
        return Err("hit mask is pet-only".into());
    }
    #[cfg(not(windows))]
    return Err("alpha hit masks are supported only by the Windows host".into());
    #[cfg(windows)]
    {
        validate_mask(width, height, alpha.len())?;
        let (reply, receive) = mpsc::sync_channel(1);
        state
            .sender
            .try_send(Request::HitMask {
                width,
                height,
                alpha,
                frame,
                reply,
            })
            .map_err(|_| "host queue full".to_owned())?;
        tauri::async_runtime::spawn_blocking(move || receive.recv())
            .await
            .map_err(|_| "host stopped".to_owned())?
            .map_err(|_| "host stopped".to_owned())?
    }
}

#[tauri::command]
async fn prototype_material_state(
    window: tauri::WebviewWindow,
    state: State<'_, HostState>,
) -> Result<MaterialState, String> {
    if window.label() != "main" && window.label() != "pet" {
        return Err("material state is unavailable to this window".into());
    }
    let (reply, receive) = mpsc::sync_channel(1);
    state
        .sender
        .try_send(Request::MaterialState(reply))
        .map_err(|_| "host queue full".to_owned())?;
    tauri::async_runtime::spawn_blocking(move || receive.recv())
        .await
        .map_err(|_| "host stopped".to_owned())?
        .map_err(|_| "host stopped".to_owned())
}

#[tauri::command]
async fn prototype_material_command(
    command: MaterialCommand,
    context: BuddyContext,
    window: tauri::WebviewWindow,
    state: State<'_, HostState>,
) -> Result<MaterialReply, String> {
    if window.label() != "main" {
        return Err("material commands are main-window only".into());
    }
    if serde_json::to_vec(&command).map_or(true, |bytes| bytes.len() > MAX_COMMAND_BYTES) {
        return Err("material command exceeds the byte budget".into());
    }
    let (reply, receive) = mpsc::sync_channel(1);
    state
        .sender
        .try_send(Request::MaterialCommand {
            command,
            context,
            reply,
        })
        .map_err(|_| "host queue full".to_owned())?;
    tauri::async_runtime::spawn_blocking(move || receive.recv())
        .await
        .map_err(|_| "host stopped".to_owned())?
        .map_err(|_| "host stopped".to_owned())
}

#[tauri::command]
async fn prototype_material_pixels(
    frame: MaterialFrameTriple,
    window: tauri::WebviewWindow,
    state: State<'_, HostState>,
) -> Result<tauri::ipc::Response, String> {
    if window.label() != "pet" {
        return Err("material pixels are pet-window only".into());
    }
    if [
        frame.resource_generation,
        frame.playback_generation,
        frame.presentation_sequence,
    ]
    .into_iter()
    .any(|value| value == 0 || value > MAX_PRESENTATION_SEQUENCE)
    {
        return Err("invalid material frame token".into());
    }
    let (reply, receive) = mpsc::sync_channel(1);
    state
        .sender
        .try_send(Request::MaterialPixels { frame, reply })
        .map_err(|_| "host queue full".to_owned())?;
    let pixels = tauri::async_runtime::spawn_blocking(move || receive.recv())
        .await
        .map_err(|_| "host stopped".to_owned())?
        .map_err(|_| "host stopped".to_owned())??;
    Ok(tauri::ipc::Response::new(pixels))
}

#[tauri::command]
fn prototype_frame(state: State<'_, HostState>) -> FrameSnapshot {
    state.frame.lock().expect("frame mutex poisoned").clone()
}

fn failure(code: &str, message: &str) -> Failure {
    failure_with_disposition(code, message, "not_executed")
}

fn failure_unknown(code: &str, message: &str) -> Failure {
    failure_with_disposition(code, message, "unknown")
}

fn failure_with_disposition(code: &str, message: &str, disposition: &str) -> Failure {
    Failure {
        code: code.into(),
        message: message.into(),
        disposition: disposition.into(),
    }
}

fn material_error_message(code: &str) -> &'static str {
    match code {
        "material_busy" => "素材任务正在处理，请等待或停止当前加载",
        "material_path_invalid" => "素材路径无效或不受支持",
        "material_package_invalid" => "素材包无效，旧素材保持不变",
        "material_loader_unavailable" => "无法启动素材加载，旧素材保持不变",
        "material_generation_stale" => "素材状态已变化，请重新读取后重试",
        "material_action_unknown" | "unknown_material_action" => "所选动作不存在",
        "no_material_package" => "当前没有已加载的素材包",
        "material_stop_failed" => "素材停止失败，当前状态需要重新读取",
        "material_start_failed" => "素材播放启动失败，需确认停止后重试",
        "material_generation_exhausted" => "素材代际已耗尽，操作未应用",
        "material_unload_failed" => "素材卸载失败，当前状态需要重新读取",
        _ => "素材操作失败，旧素材保持不变",
    }
}

fn command_over_budget(command: &ChatCommand) -> bool {
    let id_over_budget = match command {
        ChatCommand::NewSession {
            expected_session_id,
            ..
        }
        | ChatCommand::SetDraft {
            expected_session_id,
            ..
        }
        | ChatCommand::Send {
            expected_session_id,
            ..
        } => expected_session_id.len() > MAX_SESSION_ID_BYTES,
        ChatCommand::SwitchSession {
            id,
            expected_session_id,
            ..
        }
        | ChatCommand::ArchiveSession {
            id,
            expected_session_id,
            ..
        } => id.len() > MAX_SESSION_ID_BYTES || expected_session_id.len() > MAX_SESSION_ID_BYTES,
        _ => false,
    };
    id_over_budget
        || serde_json::to_vec(command).map_or(true, |bytes| bytes.len() > MAX_COMMAND_BYTES)
}

fn validate_mask(width: u16, height: u16, alpha_len: usize) -> Result<(), String> {
    let cells = usize::from(width)
        .checked_mul(usize::from(height))
        .ok_or("mask dimensions overflow")?;
    if width == 0
        || height == 0
        || width > 256
        || height > 256
        || cells > MAX_HIT_CELLS
        || cells != alpha_len
    {
        return Err("invalid hit mask dimensions or length".into());
    }
    Ok(())
}

fn dispatch_source_allowed(label: &str) -> bool {
    label == "main"
}

#[derive(Default)]
struct PlaybackState {
    ticket: Option<moe_agent_runtime::animation::PlaybackTicket>,
    elapsed_ms: u64,
    paused: bool,
}

#[derive(Clone, Default)]
struct FourFramePlayer(Arc<Mutex<PlaybackState>>);

#[derive(Debug)]
struct PlayerError;
impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid clip playback ticket")
    }
}
impl Error for PlayerError {}

impl ClipPlayer for FourFramePlayer {
    type Error = PlayerError;
    fn prepare(
        &mut self,
        _clip: &AnimationClip,
        _mode: PlaybackMode,
    ) -> Result<ClipCapabilities, Self::Error> {
        Ok(ClipCapabilities { pause_resume: true })
    }
    fn start(
        &mut self,
        _clip: &AnimationClip,
        _mode: PlaybackMode,
        ticket: &moe_agent_runtime::animation::PlaybackTicket,
    ) -> Result<PlaybackAck, Self::Error> {
        let mut state = self.0.lock().map_err(|_| PlayerError)?;
        state.ticket = Some(ticket.clone());
        state.elapsed_ms = 0;
        state.paused = false;
        Ok(PlaybackAck {
            ticket: ticket.clone(),
        })
    }
    fn advance(
        &mut self,
        ticket: &moe_agent_runtime::animation::PlaybackTicket,
        delta_ms: u64,
    ) -> Result<PlaybackAdvance, Self::Error> {
        let mut state = self.0.lock().map_err(|_| PlayerError)?;
        if state.ticket.as_ref() != Some(ticket) || state.paused {
            return Err(PlayerError);
        }
        let consumed = delta_ms.min(material_descriptor().clip_duration_ms - state.elapsed_ms);
        state.elapsed_ms += consumed;
        let boundary = (state.elapsed_ms == material_descriptor().clip_duration_ms)
            .then_some(moe_agent_runtime::animation::PlaybackBoundary::LoopBoundary);
        if boundary.is_some() {
            state.elapsed_ms = 0;
        }
        Ok(PlaybackAdvance {
            ticket: ticket.clone(),
            consumed_ms: consumed,
            boundary,
        })
    }
    fn pause(
        &mut self,
        ticket: &moe_agent_runtime::animation::PlaybackTicket,
    ) -> Result<PlaybackAck, Self::Error> {
        let mut state = self.0.lock().map_err(|_| PlayerError)?;
        if state.ticket.as_ref() != Some(ticket) {
            return Err(PlayerError);
        }
        state.paused = true;
        Ok(PlaybackAck {
            ticket: ticket.clone(),
        })
    }
    fn resume(
        &mut self,
        ticket: &moe_agent_runtime::animation::PlaybackTicket,
    ) -> Result<PlaybackAck, Self::Error> {
        let mut state = self.0.lock().map_err(|_| PlayerError)?;
        if state.ticket.as_ref() != Some(ticket) {
            return Err(PlayerError);
        }
        state.paused = false;
        Ok(PlaybackAck {
            ticket: ticket.clone(),
        })
    }
    fn stop(
        &mut self,
        ticket: &moe_agent_runtime::animation::PlaybackTicket,
    ) -> Result<PlaybackAck, Self::Error> {
        let mut state = self.0.lock().map_err(|_| PlayerError)?;
        if state.ticket.as_ref() != Some(ticket) {
            return Err(PlayerError);
        }
        state.ticket = None;
        state.elapsed_ms = 0;
        state.paused = false;
        Ok(PlaybackAck {
            ticket: ticket.clone(),
        })
    }
    fn progress(
        &self,
        ticket: &moe_agent_runtime::animation::PlaybackTicket,
    ) -> Result<PlaybackPosition, Self::Error> {
        let state = self.0.lock().map_err(|_| PlayerError)?;
        if state.ticket.as_ref() != Some(ticket) {
            return Err(PlayerError);
        }
        Ok(PlaybackPosition {
            ticket: ticket.clone(),
            elapsed_ms: state.elapsed_ms,
            paused: state.paused,
        })
    }
}

fn create_animator() -> (Animator<FourFramePlayer>, FourFramePlayer) {
    let clip = AnimationClip::new("idle-four-frame", 1, material_descriptor().clip_duration_ms)
        .expect("valid built-in clip");
    let definition = AnimationDefinition::new(AnimationSpec {
        version: 1,
        clips: vec![clip],
        actions: vec![AnimationAction {
            id: "idle".into(),
            entry: "idle-loop".into(),
            exit: None,
            priority: 0,
            preemption: moe_agent_runtime::animation::PreemptionPolicy::Immediate,
            nodes: vec![AnimationNode {
                id: "idle-loop".into(),
                clip: "idle-four-frame".into(),
                mode: PlaybackMode::Loop,
                edges: vec![],
            }],
        }],
        parameters: vec![],
        triggers: vec![],
        bindings: vec![],
    })
    .expect("valid built-in animation");
    let player = FourFramePlayer::default();
    (Animator::new(definition, player.clone()), player)
}

fn submit_visibility(
    sender: &SyncSender<Request>,
    label: &'static str,
    visible: bool,
    context: Option<BuddyContext>,
) -> Result<(), ()> {
    let (reply, _receive) = mpsc::sync_channel(1);
    sender
        .try_send(Request::SetVisibility {
            label,
            visible,
            context,
            reply,
        })
        .map_err(|_| ())
}

fn submit_toggle_pause(sender: &SyncSender<Request>) -> Result<(), ()> {
    let (reply, _receive) = mpsc::sync_channel(1);
    sender.try_send(Request::TogglePause(reply)).map_err(|_| ())
}

fn tray_feedback(app: &AppHandle, code: &str) {
    eprintln!("moe-buddy-tauri: {code}");
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let tooltip = format!("Moe Buddy：{}", feedback_message(code));
        if let Err(error) = tray.set_tooltip(Some(tooltip)) {
            eprintln!("moe-buddy-tauri: unable to update tray feedback: {error}");
        }
    }
}

fn handle_tray_menu(app: &AppHandle, id: &str) {
    let Some(state) = app.try_state::<HostState>() else {
        tray_feedback(app, "host_state_unavailable");
        return;
    };
    let main_context = || {
        state
            .latest
            .lock()
            .map(|snapshot| {
                let snapshot = snapshot.clone();
                BuddyContext {
                    expected_buddy_id: snapshot.active_buddy_id.unwrap_or_default(),
                    expected_buddies_revision: snapshot.buddies_revision,
                }
            })
            .map_err(|_| ())
    };
    let outcome = match id {
        "show" => main_context()
            .and_then(|context| submit_visibility(&state.sender, "main", true, Some(context))),
        "hide" => main_context()
            .and_then(|context| submit_visibility(&state.sender, "main", false, Some(context))),
        "pause" => submit_toggle_pause(&state.sender),
        "quit" => {
            state
                .exit
                .submit_with_context(ExitIntent::QuitApplication, main_context().ok());
            Ok(())
        }
        _ => return,
    };
    if outcome.is_err() {
        tray_feedback(app, "tray_command_failed");
    }
}

fn join_worker(worker: &Mutex<Option<thread::JoinHandle<()>>>) {
    let join = worker.lock().expect("worker mutex poisoned").take();
    if let Some(join) = join
        && join.join().is_err()
    {
        eprintln!("moe-buddy-tauri: chat host thread panicked");
    }
}

pub fn run() -> i32 {
    let generation = match process_generation() {
        Ok(generation) => generation,
        Err(error) => {
            eprintln!("moe-buddy-tauri: {error}");
            return 1;
        }
    };
    let startup = match startup_paths_from_args() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("moe-buddy-tauri: {error}");
            return 1;
        }
    };
    let epoch_start_ms = match current_epoch_ms() {
        Ok(now) => now,
        Err(error) => {
            eprintln!("moe-buddy-tauri: {error}");
            return 1;
        }
    };
    let (sender, receiver) = mpsc::sync_channel::<Request>(QUEUE_CAPACITY);
    let (close_sender, close_receiver) = mpsc::sync_channel::<CloseRequest>(CLOSE_QUEUE_CAPACITY);
    let initial = empty_snapshot();
    let latest = Arc::new(Mutex::new(initial));
    let frame = Arc::new(Mutex::new(FrameSnapshot::builtin(0, false, false)));
    let exit = Arc::new(ExitMailbox::new());
    let management_drafts = Arc::new(ManagementDraftMailbox::default());
    let stop = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let event_loop_alive = Arc::new(AtomicBool::new(true));
    let worker_started = Arc::new(AtomicBool::new(false));
    let worker: Arc<Mutex<Option<thread::JoinHandle<()>>>> = Arc::new(Mutex::new(None));

    let host_latest = latest.clone();
    let host_frame = frame.clone();
    let setup_exit = exit.clone();
    let host_data_root = startup.data_root;
    let host_generation = generation;
    let host_epoch_start_ms = epoch_start_ms;
    let host_stop = stop.clone();
    let host_finished = finished.clone();
    let host_alive = event_loop_alive.clone();
    let setup_worker = worker.clone();
    let setup_started = worker_started.clone();
    let mut context = tauri::generate_context!();
    let mut portable_windows = Vec::new();
    if let Some(profile) = startup.webview_data {
        for config in &mut context.config_mut().app.windows {
            portable_windows.push((config.clone(), profile.clone()));
            // Config dataDirectory only accepts relative paths. Build these
            // windows explicitly so the host can supply the absolute profile.
            config.create = false;
        }
    }
    let builder = tauri::Builder::default()
        .manage(HostState {
            sender: sender.clone(),
            close_sender: close_sender.clone(),
            latest,
            frame,
            exit: exit.clone(),
        })
        .invoke_handler(tauri::generate_handler![
            prototype_snapshot,
            prototype_dispatch,
            prototype_management_draft_state,
            prototype_window,
            prototype_set_hit_mask,
            prototype_frame,
            prototype_material_state,
            prototype_material_command,
            prototype_material_pixels
        ])
        .setup(move |app| {
            for (config, profile) in &portable_windows {
                tauri::WebviewWindowBuilder::from_config(app, config)?
                    .data_directory(profile.clone())
                    .build()?;
            }
            let handle = app.handle().clone();
            let worker_handle = handle.clone();
            let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
            let join = thread::Builder::new()
                .name("moe-chat-owner".into())
                .spawn(move || {
                    let mut storage = match BuddyStorage::open(
                        &host_data_root,
                        host_generation,
                        host_epoch_start_ms,
                    ) {
                        Ok(storage) => storage,
                        Err(error) => {
                            let message = format!("local restore refused: {error}");
                            eprintln!("moe-buddy-tauri: {message}");
                            let _ = startup_sender.send(Err(message));
                            host_finished.store(true, Ordering::Release);
                            return;
                        }
                    };
                    let app = match storage.active_application_mut() {
                        Ok(app) => app,
                        Err(error) => {
                            let message = format!("local restore refused: {error}");
                            eprintln!("moe-buddy-tauri: {message}");
                            let _ = startup_sender.send(Err(message));
                            host_finished.store(true, Ordering::Release);
                            return;
                        }
                    };
                    if let Err(error) =
                        app.dispatch(Action::Open(PanelKind::Chat), host_epoch_start_ms)
                    {
                        let message = format!("chat restore refused: {}", error.public_message());
                        eprintln!("moe-buddy-tauri: {message}");
                        let _ = startup_sender.send(Err(message));
                        host_finished.store(true, Ordering::Release);
                        return;
                    }
                    let worker = match HostWorker::with_buddy_storage(
                        TauriPort::new(worker_handle),
                        storage,
                        HostWorkerContext {
                            #[cfg(test)]
                            model_settings: None,
                            persistence_enabled: true,
                            management_drafts: management_drafts.clone(),
                            epoch_start_ms: host_epoch_start_ms,
                            latest: host_latest,
                            frame: host_frame,
                        },
                    ) {
                        Ok(worker) => worker,
                        Err(message) => {
                            eprintln!("moe-buddy-tauri: {message}");
                            let _ = startup_sender.send(Err(message));
                            host_finished.store(true, Ordering::Release);
                            return;
                        }
                    };
                    if startup_sender.send(Ok(())).is_err() {
                        return;
                    }
                    run_worker_loop(
                        move || worker,
                        receiver,
                        close_receiver,
                        setup_exit,
                        host_stop,
                        host_finished,
                        host_alive,
                    )
                })
                .map_err(tauri::Error::from)?;
            *setup_worker.lock().expect("worker mutex poisoned") = Some(join);
            match startup_receiver.recv() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    join_worker(&setup_worker);
                    return Err(std::io::Error::other(error).into());
                }
                Err(_) => {
                    join_worker(&setup_worker);
                    return Err(std::io::Error::other("host worker stopped during startup").into());
                }
            }
            setup_started.store(true, Ordering::Release);
            let show = tauri::menu::MenuItem::with_id(app, "show", "显示聊天", true, None::<&str>)?;
            let hide = tauri::menu::MenuItem::with_id(app, "hide", "隐藏聊天", true, None::<&str>)?;
            let pause =
                tauri::menu::MenuItem::with_id(app, "pause", "暂停动画", true, None::<&str>)?;
            let quit = tauri::menu::MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = tauri::menu::Menu::with_items(app, &[&show, &hide, &pause, &quit])?;
            let icon = app.default_window_icon().cloned().unwrap_or_else(|| {
                let mut rgba = vec![0_u8; 16 * 16 * 4];
                for pixel in rgba.as_chunks_mut::<4>().0 {
                    pixel.copy_from_slice(&[76, 132, 166, 255]);
                }
                tauri::image::Image::new_owned(rgba, 16, 16)
            });
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(icon)
                .tooltip("Moe Buddy Prototype")
                .menu(&menu)
                .on_menu_event(|app, event| handle_tray_menu(app, event.id().as_ref()))
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.label() == "pet" {
                    if let Err(error) = window.hide() {
                        eprintln!("moe-buddy-tauri: unable to hide pet window: {error}");
                    }
                } else if window.label() == "main"
                    && let Some(state) = window.app_handle().try_state::<HostState>()
                {
                    let context = state.latest.lock().ok().map(|snapshot| BuddyContext {
                        expected_buddy_id: snapshot.active_buddy_id.clone().unwrap_or_default(),
                        expected_buddies_revision: snapshot.buddies_revision,
                    });
                    state
                        .exit
                        .submit_with_context(ExitIntent::CloseChat, context);
                }
            }
        });
    let app = match builder.build(context) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("moe-buddy-tauri: startup failed: {error}");
            event_loop_alive.store(false, Ordering::Release);
            stop.store(true, Ordering::Release);
            join_worker(&worker);
            return 1;
        }
    };
    let run_stop = stop.clone();
    let run_finished = finished.clone();
    let run_started = worker_started.clone();
    let exit_code = app.run_return(move |_app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if run_finished.load(Ordering::Acquire) || !run_started.load(Ordering::Acquire) {
                return;
            }
            // worker 仍可能访问窗口；先阻止退出，让它完成收起与释放后自己请求退出。
            api.prevent_exit();
            run_stop.store(true, Ordering::Release);
        }
    });
    event_loop_alive.store(false, Ordering::Release);
    stop.store(true, Ordering::Release);
    if finished.load(Ordering::Acquire) {
        join_worker(&worker);
    } else {
        // 事件循环已停止时，不能等待仍依赖窗口查询的线程；异常路径只记录并让其随进程结束。
        eprintln!(
            "moe-buddy-tauri: event loop stopped before the chat host released window access"
        );
    }
    exit_code
}

fn process_generation() -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| "unable to create a process generation".to_owned())?;
    let generation = u64::from_le_bytes(bytes);
    Ok(if generation == 0 { 1 } else { generation })
}

fn current_epoch_ms() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock exceeds the supported range".to_owned())
}

fn startup_paths_from_args() -> Result<crate::startup::StartupPaths, String> {
    let explicit = parse_data_dir_args(std::env::args_os().skip(1))?;
    let executable = std::env::current_exe().map_err(|_| "executable path unavailable")?;
    crate::startup::resolve(explicit, &executable)
}

fn parse_data_dir_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<Option<std::path::PathBuf>, String> {
    let mut selected = None;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        if argument == "--data-dir" {
            if selected.is_some() {
                return Err("--data-dir may only be provided once".into());
            }
            let Some(path) = args.next() else {
                return Err("--data-dir requires a path".into());
            };
            if path.is_empty() {
                return Err("--data-dir requires a path".into());
            }
            selected = Some(std::path::PathBuf::from(path));
        } else if let Some(argument) = argument.to_str()
            && let Some(path) = argument.strip_prefix("--data-dir=")
        {
            if selected.is_some() || path.is_empty() {
                return Err("--data-dir requires one non-empty path".into());
            }
            selected = Some(std::path::PathBuf::from(path));
        }
    }
    Ok(selected)
}

fn panel_kind(value: &str) -> Option<PanelKind> {
    match value {
        "presets" => Some(PanelKind::Presets),
        "memory" => Some(PanelKind::Memory),
        "diagnostics" => Some(PanelKind::Diagnostics),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototype_window_requires_context_for_main_window_commands() {
        assert_eq!(
            required_window_context(None).unwrap_err(),
            "window command requires buddy context"
        );
        let context = BuddyContext {
            expected_buddy_id: "offline-companion".into(),
            expected_buddies_revision: 1,
        };
        assert_eq!(required_window_context(Some(context.clone())), Ok(context));
    }

    fn guarded_draft(snapshot: &Snapshot, text: impl Into<String>) -> ChatCommand {
        ChatCommand::SetDraft {
            text: text.into(),
            expected_session_id: snapshot
                .active_session_id
                .clone()
                .expect("active session in test snapshot"),
            expected_sessions_revision: snapshot.sessions_revision,
        }
    }

    fn guarded_send(snapshot: &Snapshot) -> ChatCommand {
        ChatCommand::Send {
            expected_session_id: snapshot
                .active_session_id
                .clone()
                .expect("active session in test snapshot"),
            expected_sessions_revision: snapshot.sessions_revision,
        }
    }
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn isolated_root() -> std::path::PathBuf {
        static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "moe-buddy-tauri-test-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn update_management_draft_state(
        mailbox: &ManagementDraftMailbox,
        _sender: &SyncSender<Request>,
        version: u64,
        dirty: bool,
        kind: ManagementDraftKind,
        context: BuddyContext,
    ) -> Result<ManagementDraftAck, String> {
        let accepted = mailbox
            .update(version, dirty, kind, context)
            .map_err(str::to_owned)?;
        Ok(ManagementDraftAck {
            version: accepted.version,
            dirty: accepted.dirty,
        })
    }

    fn configure_command(
        endpoint: String,
        allow_transmission: bool,
        api_key: Option<&str>,
    ) -> ChatCommand {
        ChatCommand::ConfigureModel {
            endpoint,
            model: "synthetic-model".into(),
            persona: "synthetic persona".into(),
            allow_transmission,
            api_key: RequiredNullableString::Present(api_key.map(str::to_owned)),
        }
    }

    #[test]
    fn data_root_argument_accepts_explicit_paths_and_rejects_ambiguous_values() {
        let selected = parse_data_dir_args([
            std::ffi::OsString::from("--data-dir"),
            std::ffi::OsString::from("synthetic-root"),
        ])
        .unwrap();
        assert_eq!(selected, Some(std::path::PathBuf::from("synthetic-root")));
        let selected =
            parse_data_dir_args([std::ffi::OsString::from("--data-dir=synthetic-root-equals")])
                .unwrap();
        assert_eq!(
            selected,
            Some(std::path::PathBuf::from("synthetic-root-equals"))
        );
        assert!(parse_data_dir_args(std::iter::empty()).unwrap().is_none());
        assert!(parse_data_dir_args([std::ffi::OsString::from("--data-dir")]).is_err());
        assert!(parse_data_dir_args([std::ffi::OsString::from("--data-dir=")]).is_err());
        assert!(
            parse_data_dir_args([
                std::ffi::OsString::from("--data-dir"),
                std::ffi::OsString::from("one"),
                std::ffi::OsString::from("--data-dir"),
                std::ffi::OsString::from("two"),
            ])
            .is_err()
        );
    }

    fn synthetic_reply_server(text: &'static str) -> (SocketAddr, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_synthetic_request(&mut stream);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer synthetic-key")
            );
            assert!(request.contains("synthetic request body"));
            let body = format!(
                "{{\"choices\":[{{\"message\":{{\"content\":{}}}}}]}}",
                serde_json::to_string(text).unwrap()
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            request
        });
        (address, worker)
    }

    fn gated_synthetic_server(
        text: &'static str,
    ) -> (
        SocketAddr,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_synthetic_request(&mut stream);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer synthetic-key")
            );
            accepted_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let body = format!(
                "{{\"choices\":[{{\"message\":{{\"content\":{}}}}}]}}",
                serde_json::to_string(text).unwrap()
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });
        (address, accepted_rx, release_tx, worker)
    }

    fn read_synthetic_request(stream: &mut std::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
            let text = String::from_utf8_lossy(&bytes);
            if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .expect("synthetic request has a bounded content length");
                if body.len() >= length {
                    return text.into_owned();
                }
            }
            assert!(bytes.len() < 32_768);
        }
    }

    #[test]
    fn exit_mailbox_preserves_quit_precedence_and_latest_request_id() {
        let mailbox = ExitMailbox::new();
        let first = mailbox.submit(ExitIntent::QuitApplication);
        let second = mailbox.submit(ExitIntent::CloseChat);
        assert!(second.id > first.id);
        assert_eq!(
            mailbox.take(),
            Some(ExitRequest {
                id: second.id,
                intent: ExitIntent::QuitApplication,
                buddy_context: None,
            })
        );
        assert_eq!(mailbox.take(), None);

        mailbox.submit(ExitIntent::CloseChat);
        mailbox.submit(ExitIntent::CloseChat);
        assert_eq!(mailbox.take().unwrap().intent, ExitIntent::CloseChat);
        assert_eq!(mailbox.take(), None);
    }

    #[test]
    fn shared_fixture_matches_rust_contract_enums_and_fields() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../src/core/contract-fixture.json")).unwrap();
        let close = serde_json::to_value(CloseIntent::CloseChat).unwrap();
        let quit = serde_json::to_value(CloseIntent::QuitApplication).unwrap();
        assert!(fixture["closeIntents"].as_array().unwrap().contains(&close));
        assert!(fixture["closeIntents"].as_array().unwrap().contains(&quit));
        let dispatch_commands = fixture["dispatchCommands"].as_array().unwrap();
        for command in [
            "new_session",
            "switch_session",
            "archive_session",
            "confirm_management_discard",
            "cancel_management_exit",
        ] {
            assert!(dispatch_commands.contains(&serde_json::json!(command)));
        }
        assert_eq!(
            fixture["hostCommands"],
            serde_json::json!(["prototype_management_draft_state"])
        );
        let dispositions = [
            moe_chat_app::ExecutionDisposition::NotExecuted,
            moe_chat_app::ExecutionDisposition::Settled,
            moe_chat_app::ExecutionDisposition::Unknown,
        ]
        .map(|value| value.as_str());
        let expected_dispositions = fixture["dispositions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(dispositions.as_slice(), expected_dispositions.as_slice());
        assert_eq!(
            material_descriptor().frame_count,
            fixture["frameCount"].as_u64().unwrap() as u8
        );
        assert_eq!(
            material_descriptor().frame_duration_ms * u64::from(material_descriptor().frame_count),
            material_descriptor().clip_duration_ms
        );
        let snapshot = Snapshot {
            revision: 1,
            active_buddy_id: Some("offline-companion".into()),
            buddies_revision: 1,
            buddies: vec![BuddyView {
                id: "offline-companion".into(),
                label: "离线验收伙伴".into(),
                profile_revision: 1,
                persona_revision: 1,
                model_revision: 1,
            }],
            active_buddy_persona: None,
            buddy_commit: None,
            active_session_id: Some("session-1".into()),
            sessions_revision: 1,
            session_isolation_allowed: false,
            sessions: vec![SessionView {
                id: "session-1".into(),
                label: "会话 1".into(),
                archived: false,
                status: "ready".into(),
            }],
            presentation_revision: 1,
            mode: "offline".into(),
            revoked: false,
            chat: ChatView {
                draft: String::new(),
                messages: Vec::new(),
                status: "ready".into(),
                failure: None,
            },
            close_confirmation: false,
            close_intent: None,
            management_draft: ManagementDraftView {
                version: 0,
                dirty: false,
                kind: ManagementDraftKind::None,
            },
            management_exit: None,
            management_discarded_version: None,
            chat_draft: ChatDraftView {
                revision: 1,
                discard: None,
                sessions: Vec::new(),
            },
            active_preset: "offline-echo".into(),
            external_transmission_allowed: false,
            presets: Vec::new(),
            memories: Vec::new(),
            diagnostics: Vec::new(),
            notice: None,
            model_settings: None,
            persistence_enabled: false,
        };
        let serialized = serde_json::to_value(snapshot).unwrap();
        assert!(serialized["buddyCommit"].is_null());
        let commit = serde_json::to_value(crate::contract::BuddyCommitView {
            kind: "configure_model".into(),
            from_buddy_id: "offline-companion".into(),
            from_revision: 4,
            to_buddy_id: "offline-companion".into(),
            to_revision: 5,
            management_version: 7,
        })
        .unwrap();
        let mut commit_fields = commit
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        commit_fields.sort();
        let mut expected_commit_fields = fixture["buddyCommitFields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        expected_commit_fields.sort();
        assert_eq!(commit_fields, expected_commit_fields);
        assert!(
            fixture["buddyCommitKinds"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(commit["kind"]))
        );
        assert_eq!(
            serialized["managementDraft"],
            serde_json::json!({ "version": 0, "dirty": false, "kind": "none" })
        );
        assert_eq!(
            serialized["chatDraft"],
            serde_json::json!({ "revision": 1, "discard": null, "sessions": [] })
        );
        let mut chat_draft_expected = fixture["chatDraftFields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        chat_draft_expected.sort();
        let mut chat_draft_fields = serialized["chatDraft"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        chat_draft_fields.sort();
        assert_eq!(chat_draft_fields, chat_draft_expected);
        let mut draft_session_expected = fixture["chatDraftSessionFields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        draft_session_expected.sort();
        let mut draft_session_fields = serde_json::to_value(ChatDraftSessionView {
            id: String::from("session-1"),
            revision: 1,
            discarded_revision: Some(1),
        })
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
        draft_session_fields.sort();
        assert_eq!(draft_session_fields, draft_session_expected);
        let fields = serialized
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut expected = fixture["snapshotFields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(fields, expected);
        let mut session_expected = fixture["sessionFields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        session_expected.sort();
        let mut session_fields = serde_json::to_value(SessionView {
            id: String::from("session-1"),
            label: String::from("会话 1"),
            archived: false,
            status: String::from("ready"),
        })
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
        session_fields.sort();
        assert_eq!(session_fields, session_expected);
        let mut frame_expected = fixture["frameFields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        frame_expected.sort();
        assert_eq!(
            serde_json::to_value(FrameSnapshot::builtin(0, true, false))
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            frame_expected
        );
        let mut error_expected = fixture["errorFields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        error_expected.sort();
        assert_eq!(
            serde_json::to_value(Failure {
                code: "safe".into(),
                message: "safe message".into(),
                disposition: "unknown".into(),
            })
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
            error_expected
        );
    }

    #[test]
    fn dispatch_wait_disconnect_reports_unknown_after_acceptance() {
        let (reply, receive) = mpsc::sync_channel(1);
        drop(reply);
        let result = tauri::async_runtime::block_on(await_dispatch(receive));
        let failure = result.error.expect("disconnected accepted request fails");
        assert_eq!(failure.code, "host_stopped");
        assert_eq!(failure.disposition, "unknown");
    }

    #[test]
    fn playback_progress_handles_idle_valid_and_out_of_range_states() {
        let descriptor = crate::contract::MaterialDescriptor {
            frame_count: 4,
            frame_duration_ms: 100,
            clip_duration_ms: 400,
        };
        assert_eq!(frame_index_for_progress(None, &descriptor), Ok(None));
        assert_eq!(frame_index_for_progress(Some(0), &descriptor), Ok(Some(0)));
        assert_eq!(
            frame_index_for_progress(Some(399), &descriptor),
            Ok(Some(3))
        );
        assert_eq!(frame_index_for_progress(Some(400), &descriptor), Err(()));
        assert_eq!(
            frame_index_for_progress(
                Some(1),
                &crate::contract::MaterialDescriptor {
                    clip_duration_ms: 401,
                    ..descriptor
                }
            ),
            Err(())
        );
        assert_eq!(
            frame_index_for_progress(
                Some(1),
                &crate::contract::MaterialDescriptor {
                    frame_count: 0,
                    ..descriptor
                }
            ),
            Err(())
        );
        assert!(feedback_message("animation_progress_failed").contains("暂停"));
        assert!(feedback_message("animation_progress_out_of_bounds").contains("超出"));
        assert!(feedback_message("animation_material_invalid").contains("无效"));
    }

    #[test]
    fn business_rejections_keep_host_settled_disposition_and_messages() {
        let mut host = TestHost::new();
        let empty = host.dispatch(guarded_send(&host.snapshot()));
        let empty = empty.error.expect("empty send is rejected");
        assert_eq!(empty.code, "empty_input");
        assert_eq!(empty.disposition, "not_executed");
        assert!(!empty.message.is_empty());

        let oversized = host.dispatch(guarded_draft(&host.snapshot(), "x".repeat(8_001)));
        let oversized = oversized.error.expect("oversized draft is rejected");
        assert_eq!(oversized.code, "input_too_large");
        assert_eq!(oversized.disposition, "not_executed");
        assert!(!oversized.message.is_empty());
    }

    #[test]
    fn command_dto_requires_exact_camel_case_and_explicit_nullable_key() {
        let valid: ChatCommand = serde_json::from_value(serde_json::json!({
            "kind": "configure_model",
            "endpoint": "http://127.0.0.1:9/v1/chat/completions",
            "model": "synthetic-model",
            "persona": "synthetic persona",
            "allowTransmission": true,
            "apiKey": null
        }))
        .unwrap();
        assert!(matches!(valid, ChatCommand::ConfigureModel { .. }));
        assert!(
            serde_json::from_value::<ChatCommand>(serde_json::json!({
                "kind": "send"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ChatCommand>(serde_json::json!({
                "kind": "set_draft",
                "text": "synthetic"
            }))
            .is_err()
        );
        assert!(matches!(
            serde_json::from_value::<ChatCommand>(serde_json::json!({
                "kind": "switch_session",
                "id": "session-2",
                "expectedSessionId": "session-1",
                "expectedSessionsRevision": 1
            }))
            .unwrap(),
            ChatCommand::SwitchSession {
                id,
                expected_session_id,
                expected_sessions_revision: 1
            } if id == "session-2" && expected_session_id == "session-1"
        ));
        assert!(matches!(
            serde_json::from_value::<ChatCommand>(serde_json::json!({
                "kind": "confirm_close",
                "discard": true,
                "expectedIntent": null
            }))
            .unwrap(),
            ChatCommand::ConfirmClose {
                discard: true,
                expected_intent: None
            }
        ));
        assert!(
            serde_json::from_value::<ChatCommand>(serde_json::json!({
                "kind": "confirm_close",
                "discard": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ChatCommand>(serde_json::json!({
                "kind": "confirm_close",
                "discard": true,
                "expectedIntent": "exit"
            }))
            .is_err()
        );
        assert!(matches!(
            serde_json::from_value::<ChatCommand>(serde_json::json!({
                "kind": "confirm_management_discard",
                "version": 7,
                "intent": "quit_application"
            }))
            .unwrap(),
            ChatCommand::ConfirmManagementDiscard {
                version: 7,
                intent: CloseIntent::QuitApplication
            }
        ));
        assert!(
            serde_json::from_value::<ChatCommand>(serde_json::json!({
                "kind": "cancel_management_exit",
                "version": 7,
                "intent": "close_chat",
                "extra": true
            }))
            .is_err()
        );
        let missing_key: ChatCommand = serde_json::from_value(serde_json::json!({
            "kind": "configure_model",
            "endpoint": "http://127.0.0.1:9/v1/chat/completions",
            "model": "synthetic-model",
            "persona": "synthetic persona",
            "allowTransmission": true
        }))
        .unwrap();
        let mut host = TestHost::new();
        let missing_key = host.dispatch(missing_key);
        assert_eq!(
            missing_key.error.as_ref().map(|error| error.code.as_str()),
            Some("api_key_required")
        );
        assert!(
            serde_json::from_value::<ChatCommand>(serde_json::json!({
                "kind": "configure_model",
                "endpoint": "http://127.0.0.1:9/v1/chat/completions",
                "model": "synthetic-model",
                "persona": "synthetic persona",
                "allow_transmission": true,
                "apiKey": "synthetic-key"
            }))
            .is_err()
        );
    }

    #[test]
    fn no_transmission_permission_never_starts_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let mut host = TestHost::new();
        let configured = host.dispatch(configure_command(
            format!("http://{address}/v1/chat/completions"),
            false,
            None,
        ));
        assert!(configured.error.is_none());
        assert!(!host.snapshot().external_transmission_allowed);
        host.dispatch(guarded_draft(&host.snapshot(), "synthetic request body"));
        let rejected = host
            .dispatch(guarded_send(&host.snapshot()))
            .error
            .expect("send without explicit permission is rejected");
        assert_eq!(rejected.code, "transmission_denied");
        assert_eq!(rejected.disposition, "not_executed");
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn model_key_rules_and_registry_failure_preserve_the_committed_binding() {
        let mut host = TestHost::new();
        let invalid_configuration = host.dispatch(ChatCommand::ConfigureModel {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic-model".into(),
            persona: String::new(),
            allow_transmission: true,
            api_key: RequiredNullableString::Present(Some("synthetic-key".into())),
        });
        assert_eq!(
            invalid_configuration
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("invalid_persona")
        );
        assert!(!host.snapshot().external_transmission_allowed);
        assert!(host.worker.model_key.is_none());
        assert!(
            !host.root.as_ref().unwrap().join("buddies.v1").exists()
                || !std::fs::read_to_string(host.root.as_ref().unwrap().join("buddies.v1"))
                    .unwrap()
                    .contains("synthetic-key")
        );

        let configured = host.dispatch(configure_command(
            "http://127.0.0.1:9/v1/chat/completions".into(),
            true,
            Some("synthetic-key"),
        ));
        assert!(configured.error.is_none());
        let snapshot = host.snapshot();
        assert!(snapshot.model_settings.as_ref().unwrap().has_api_key);
        assert!(snapshot.external_transmission_allowed);
        let registry_bytes =
            std::fs::read_to_string(host.root.as_ref().unwrap().join("buddies.v1")).unwrap();
        assert!(!registry_bytes.contains("synthetic-key"));
        assert!(!registry_bytes.contains("allowTransmission"));

        let changed_endpoint = host.dispatch(configure_command(
            "http://127.0.0.1:10/v1/chat/completions".into(),
            true,
            None,
        ));
        assert_eq!(
            changed_endpoint
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("api_key_required")
        );
        let cleared = host.dispatch(configure_command(
            "http://127.0.0.1:9/v1/chat/completions".into(),
            false,
            Some(""),
        ));
        assert!(cleared.error.is_none());
        assert!(!host.snapshot().model_settings.as_ref().unwrap().has_api_key);

        std::fs::write(
            host.root.as_ref().unwrap().join("buddies.pending"),
            b"occupied",
        )
        .unwrap();
        let failed_save = host.dispatch(configure_command(
            "http://127.0.0.1:11/v1/chat/completions".into(),
            true,
            Some("synthetic-key"),
        ));
        assert_eq!(
            failed_save.error.as_ref().map(|error| error.code.as_str()),
            Some("buddy_storage_failed")
        );
        assert_eq!(
            failed_save
                .error
                .as_ref()
                .map(|error| error.disposition.as_str()),
            Some("not_executed"),
            "a pending registry prevents both the persisted and runtime binding update"
        );
        assert_eq!(
            host.snapshot().model_settings.as_ref().unwrap().endpoint,
            "http://127.0.0.1:9/v1/chat/completions"
        );
        assert!(!host.snapshot().external_transmission_allowed);
        assert!(!host.snapshot().model_settings.as_ref().unwrap().has_api_key);
    }

    #[test]
    fn two_buddy_apps_keep_memory_drafts_models_and_credentials_isolated_a_b_a() {
        let mut host = TestHost::new();
        let first = host.snapshot();
        let first_id = first.active_buddy_id.clone().unwrap();
        assert!(
            host.dispatch(guarded_draft(&first, "A private unsent draft"))
                .error
                .is_none()
        );
        host.dispatch(ChatCommand::OpenPanel {
            panel: "memory".into(),
        });
        host.dispatch(ChatCommand::Remember {
            text: "A private memory".into(),
        });
        host.dispatch(ChatCommand::RefreshMemories);
        assert!(
            host.dispatch(configure_command(
                "http://127.0.0.1:9/v1/chat/completions".into(),
                true,
                Some("synthetic-A-key"),
            ))
            .error
            .is_none()
        );

        let second_id = host.create_buddy("Buddy B", "B persona");
        let second = host.snapshot();
        let create_commit = second.buddy_commit.as_ref().unwrap();
        assert_eq!(create_commit.kind, "create_buddy");
        assert_eq!(create_commit.from_buddy_id, first_id);
        assert_eq!(create_commit.to_buddy_id, second_id);
        assert_eq!(create_commit.to_revision, second.buddies_revision);
        assert_ne!(second_id, first_id);
        assert_eq!(second.buddies.len(), 2);
        assert!(second.buddies_revision > first.buddies_revision);
        assert_eq!(second.active_buddy_persona.as_deref(), Some("B persona"));
        assert!(second.model_settings.is_none());
        assert!(!second.external_transmission_allowed);
        assert_eq!(second.chat.draft, "");
        assert_eq!(second.active_session_id, first.active_session_id);

        assert!(
            host.dispatch(guarded_draft(&second, "B private unsent draft"))
                .error
                .is_none()
        );
        host.dispatch(ChatCommand::OpenPanel {
            panel: "memory".into(),
        });
        host.dispatch(ChatCommand::RefreshMemories);
        assert!(host.snapshot().memories.is_empty());
        host.dispatch(ChatCommand::Remember {
            text: "B private memory".into(),
        });
        host.dispatch(ChatCommand::RefreshMemories);
        let context_before_model_save = host.worker.active_context().unwrap();
        assert!(
            host.dispatch(configure_command(
                "http://127.0.0.1:10/v1/chat/completions".into(),
                true,
                Some("synthetic-B-key"),
            ))
            .error
            .is_none()
        );
        let configured = host.snapshot();
        let model_commit = configured.buddy_commit.as_ref().unwrap();
        assert_eq!(model_commit.kind, "configure_model");
        assert_eq!(model_commit.from_buddy_id, second_id);
        assert_eq!(
            model_commit.from_revision,
            context_before_model_save.expected_buddies_revision
        );
        assert_eq!(model_commit.to_revision, configured.buddies_revision);
        assert_eq!(
            model_commit.management_version,
            host.management_drafts.current().version
        );
        let stale_edit = host.worker.accept_management_draft_state(
            host.management_drafts.current().version + 1,
            true,
            ManagementDraftKind::General,
            context_before_model_save,
        );
        assert!(
            stale_edit.is_err(),
            "old queued edit must not inherit the new revision"
        );

        let revision_before = host.snapshot().buddies_revision;
        let presentation_before = host.snapshot().presentation_revision;
        let first_again = host.switch_buddy(&first_id);
        assert_eq!(
            first_again.buddy_commit.as_ref().unwrap().kind,
            "switch_buddy"
        );
        assert!(first_again.buddies_revision > revision_before);
        assert!(first_again.presentation_revision > presentation_before);
        assert_eq!(
            first_again.active_buddy_id.as_deref(),
            Some(first_id.as_str())
        );
        assert_eq!(
            first_again.active_buddy_persona.as_deref(),
            Some("synthetic persona")
        );
        assert_eq!(first_again.chat.draft, "A private unsent draft");
        assert_eq!(
            first_again
                .model_settings
                .as_ref()
                .map(|settings| settings.endpoint.as_str()),
            Some("http://127.0.0.1:9/v1/chat/completions")
        );
        assert!(first_again.model_settings.as_ref().unwrap().has_api_key);
        assert!(first_again.external_transmission_allowed);
        assert!(
            first_again
                .memories
                .iter()
                .any(|memory| memory.text == "A private memory")
        );
        assert!(
            first_again
                .memories
                .iter()
                .all(|memory| memory.text != "B private memory")
        );

        let second_again = host.switch_buddy(&second_id);
        assert!(second_again.buddies_revision > first_again.buddies_revision);
        assert!(second_again.presentation_revision > first_again.presentation_revision);
        assert_eq!(second_again.chat.draft, "B private unsent draft");
        assert_eq!(
            second_again
                .model_settings
                .as_ref()
                .map(|settings| settings.endpoint.as_str()),
            Some("http://127.0.0.1:10/v1/chat/completions")
        );
        assert!(second_again.model_settings.as_ref().unwrap().has_api_key);
        assert!(second_again.external_transmission_allowed);
        assert!(
            second_again
                .memories
                .iter()
                .any(|memory| memory.text == "B private memory")
        );
        assert!(
            second_again
                .memories
                .iter()
                .all(|memory| memory.text != "A private memory")
        );

        let before_animation = host.snapshot();
        host.worker.toggle_pause().unwrap();
        let after_animation = host.snapshot();
        assert_eq!(
            after_animation.active_buddy_id,
            before_animation.active_buddy_id
        );
        assert_eq!(
            after_animation.buddies_revision,
            before_animation.buddies_revision
        );

        let registry =
            std::fs::read_to_string(host.root.as_ref().unwrap().join("buddies.v1")).unwrap();
        assert!(!registry.contains("synthetic-A-key"));
        assert!(!registry.contains("synthetic-B-key"));
        assert!(!registry.contains("allowTransmission"));
    }

    #[test]
    fn late_ticket_uses_its_captured_buddy_and_cannot_consume_another_pending_request() {
        fn take_ticket(
            host: &mut TestHost,
            buddy_id: &str,
            text: &str,
            now_ms: u64,
        ) -> moe_chat_app::ExternalRequestTicket {
            let storage = host.worker.buddy_storage.as_mut().unwrap();
            let app = storage.application_mut(buddy_id).unwrap();
            let before = app.snapshot();
            app.dispatch(
                Action::SetDraftGuarded {
                    text: text.into(),
                    expected_session_id: before.active_session_id.unwrap(),
                    expected_sessions_revision: before.sessions_revision,
                },
                now_ms,
            )
            .unwrap();
            let before_send = app.snapshot();
            app.dispatch(
                Action::SendGuarded {
                    expected_session_id: before_send.active_session_id.unwrap(),
                    expected_sessions_revision: before_send.sessions_revision,
                },
                now_ms,
            )
            .unwrap();
            app.take_external_request().unwrap().into_parts().1
        }

        let mut host = TestHost::new();
        let first_id = host.snapshot().active_buddy_id.clone().unwrap();
        let second_id = host.create_buddy("Buddy B", "B persona");
        assert!(
            host.dispatch(configure_command(
                "http://127.0.0.1:10/v1/chat/completions".into(),
                true,
                Some("synthetic-B-key"),
            ))
            .error
            .is_none()
        );
        host.switch_buddy(&first_id);
        assert!(
            host.dispatch(configure_command(
                "http://127.0.0.1:9/v1/chat/completions".into(),
                true,
                Some("synthetic-A-key"),
            ))
            .error
            .is_none()
        );

        let now_a = host.worker.now_ms();
        let ticket_a = take_ticket(&mut host, &first_id, "A request", now_a);
        let late_ticket_a = ticket_a.clone();
        host.worker
            .buddy_storage
            .as_mut()
            .unwrap()
            .application_mut(&first_id)
            .unwrap()
            .complete_external(
                ticket_a,
                moe_agent_runtime::ModelOutcome::Text {
                    text: "A original result".into(),
                    presentation: Vec::new(),
                },
                now_a + 1,
            )
            .unwrap();
        host.switch_buddy(&second_id);

        let now_b = host.worker.now_ms();
        let ticket_b = take_ticket(&mut host, &second_id, "B request", now_b);
        host.worker.complete_external(
            &first_id,
            late_ticket_a,
            moe_agent_runtime::ModelOutcome::Text {
                text: "late A result".into(),
                presentation: Vec::new(),
            },
            "late A result".into(),
        );
        let b_pending = host
            .worker
            .buddy_storage
            .as_mut()
            .unwrap()
            .application_mut(&second_id)
            .unwrap()
            .snapshot();
        assert_eq!(b_pending.chat.status.as_str(), "sending");
        assert!(
            !host
                .snapshot()
                .chat
                .messages
                .iter()
                .any(|message| message.text.contains("late A result"))
        );

        host.worker.complete_external(
            &second_id,
            ticket_b,
            moe_agent_runtime::ModelOutcome::Text {
                text: "B result".into(),
                presentation: Vec::new(),
            },
            "B result".into(),
        );
        let final_snapshot = host.snapshot();
        assert!(
            final_snapshot.chat.messages.iter().any(|message| {
                message.role == "assistant" && message.text.contains("B result")
            })
        );
        assert!(
            !final_snapshot
                .chat
                .messages
                .iter()
                .any(|message| message.text.contains("late A result"))
        );
    }

    #[test]
    fn stale_buddy_context_is_rejected_by_business_close_and_management_mailbox() {
        let mut host = TestHost::new();
        let stale_context = host.worker.active_context().unwrap();
        host.edit_management(1, ManagementDraftKind::BuddyRename);
        let renamed = host.dispatch(ChatCommand::RenameBuddy {
            label: "Renamed A".into(),
            expected_management_draft_version: 1,
        });
        assert!(renamed.error.is_none());
        let current = host.snapshot();
        assert_eq!(current.buddy_commit.as_ref().unwrap().kind, "rename_buddy");
        assert_eq!(current.buddies[0].label, "Renamed A");
        assert!(current.buddies_revision > stale_context.expected_buddies_revision);

        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::Dispatch(
            ChatCommand::RefreshMemories,
            stale_context.clone(),
            reply,
        ));
        assert_eq!(
            receive.recv().unwrap().error.unwrap().code,
            "buddy_context_stale"
        );

        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle_close(CloseRequest::CloseChat {
            context: stale_context.clone(),
            reply,
        });
        assert_eq!(
            receive.recv().unwrap().error.unwrap().code,
            "buddy_context_stale"
        );
        assert!(!host.snapshot().close_confirmation);

        assert!(
            host.worker
                .accept_management_draft_state(
                    2,
                    true,
                    ManagementDraftKind::General,
                    stale_context.clone(),
                )
                .is_err()
        );
        assert_eq!(host.management_drafts.current().version, 1);
        assert!(!host.management_drafts.current().dirty);
        assert!(
            serde_json::from_value::<BuddyContext>(serde_json::json!({
                "expectedBuddyId": "offline-companion"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<BuddyContext>(serde_json::json!({
                "expectedBuddyId": "offline-companion",
                "expectedBuddiesRevision": current.buddies_revision,
                "path": "unexpected"
            }))
            .is_err()
        );
    }

    #[test]
    fn unknown_registry_allows_cancelled_or_confirmed_quit_with_exact_draft_disposal() {
        let mut host = TestHost::new();
        assert!(
            host.dispatch(guarded_draft(
                &host.snapshot(),
                "keep until storage recovers"
            ))
            .error
            .is_none()
        );
        let first_id = host.snapshot().active_buddy_id.clone().unwrap();
        let second_id = host.create_buddy("Buddy B", "B persona");
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "B draft"))
                .error
                .is_none()
        );
        host.switch_buddy(&first_id);
        let (address, accepted, release, server) = gated_synthetic_server("settled in flight");
        assert!(
            host.dispatch(configure_command(
                format!("http://{address}/v1/chat/completions"),
                true,
                Some("synthetic-key"),
            ))
            .error
            .is_none()
        );
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "synthetic request body"))
                .error
                .is_none()
        );
        assert!(
            host.dispatch(guarded_send(&host.snapshot()))
                .error
                .is_none()
        );
        accepted.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(host.worker.model_requests.contains_key(&first_id));
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "A draft retained"))
                .error
                .is_none()
        );

        let root = host.root.as_ref().unwrap();
        let registry = root.join("buddies.v1");
        let preserved = root.join("buddies.saved");
        std::fs::rename(&registry, &preserved).unwrap();
        std::fs::create_dir(&registry).unwrap();
        host.edit_management(2, ManagementDraftKind::BuddyRename);
        let failed = host.dispatch(ChatCommand::RenameBuddy {
            label: "A renamed".into(),
            expected_management_draft_version: 2,
        });
        assert_eq!(failed.error.as_ref().unwrap().disposition, "unknown");
        assert!(host.dispatch(ChatCommand::RefreshMemories).error.is_some());

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while host.worker.model_requests.contains_key(&first_id) && Instant::now() < deadline {
            host.worker.poll_external_request();
            thread::sleep(Duration::from_millis(5));
        }
        server.join().unwrap();
        assert!(!host.worker.model_requests.contains_key(&first_id));
        let settled = host
            .worker
            .buddy_storage
            .as_ref()
            .unwrap()
            .inspect_known_buddy(&first_id)
            .unwrap();
        assert_eq!(settled.chat.status.as_str(), "ready");
        assert!(settled.chat.messages.iter().any(|message| {
            message.role == moe_chat_app::MessageRole::Assistant
                && message.text.contains("settled in flight")
        }));

        let request = host.exit.submit(ExitIntent::QuitApplication);
        assert!(host.worker.handle_exit(request).is_ok());
        assert!(!host.worker.should_shutdown());
        assert_eq!(host.snapshot().management_exit.unwrap().version, 2);
        let cancelled_management = host.cancel_management_exit(2, CloseIntent::QuitApplication);
        assert!(cancelled_management.error.is_none());
        assert!(!host.worker.should_shutdown());
        assert!(host.management_drafts.current().dirty);
        assert!(
            host.worker
                .draft_revision_set()
                .unwrap()
                .iter()
                .all(|draft| { draft.has_unsubmitted_drafts })
        );

        let request = host.exit.submit(ExitIntent::QuitApplication);
        assert!(host.worker.handle_exit(request).is_ok());
        assert!(
            host.confirm_management_exit(2, CloseIntent::QuitApplication)
                .error
                .is_none()
        );
        assert!(host.snapshot().close_confirmation);
        let cancelled = host.confirm(false);
        assert!(cancelled.error.is_none());
        assert!(!host.worker.should_shutdown());
        assert!(
            host.worker
                .draft_revision_set()
                .unwrap()
                .iter()
                .all(|draft| { draft.has_unsubmitted_drafts })
        );

        let request = host.exit.submit(ExitIntent::QuitApplication);
        assert!(host.worker.handle_exit(request).is_ok());
        assert_eq!(host.snapshot().management_exit.unwrap().version, 2);
        assert!(
            host.confirm_management_exit(2, CloseIntent::QuitApplication)
                .error
                .is_none()
        );
        assert!(host.snapshot().close_confirmation);
        let confirmed = host.confirm(true);
        assert!(
            confirmed.error.is_none(),
            "{:?}",
            confirmed
                .error
                .as_ref()
                .map(|error| (&error.code, &error.message))
        );
        assert!(host.worker.should_shutdown());
        assert!(
            host.worker
                .draft_revision_set()
                .unwrap()
                .iter()
                .all(|draft| !draft.has_unsubmitted_drafts)
        );
        assert!(
            host.worker
                .buddy_storage
                .as_ref()
                .unwrap()
                .registry_is_unknown()
        );
        assert!(!second_id.is_empty());
    }

    #[test]
    fn unknown_registry_allows_only_context_bound_revocation_of_loaded_active_buddy() {
        let mut host = TestHost::new();
        assert!(
            host.dispatch(configure_command(
                "http://127.0.0.1:9/v1/chat/completions".into(),
                true,
                Some("synthetic-key"),
            ))
            .error
            .is_none()
        );
        host.edit_management(1, ManagementDraftKind::BuddyRename);
        let root = host.root.as_ref().unwrap();
        let registry = root.join("buddies.v1");
        let preserved = root.join("synthetic-registry-for-revoke");
        std::fs::rename(&registry, &preserved).unwrap();
        std::fs::create_dir(&registry).unwrap();
        let failed = host.dispatch(ChatCommand::RenameBuddy {
            label: "unknown archive".into(),
            expected_management_draft_version: 1,
        });
        assert_eq!(failed.error.as_ref().unwrap().disposition, "unknown");

        let revoked = host.dispatch(ChatCommand::Revoke);
        assert!(revoked.error.is_none());
        let snapshot = host.snapshot();
        assert!(snapshot.revoked);
        assert!(snapshot.model_settings.is_none());
        assert!(!snapshot.external_transmission_allowed);
        assert!(snapshot.buddy_commit.is_none());
        assert!(host.worker.model_key.is_none());
    }

    #[test]
    fn unknown_registry_closes_transmission_projection_and_allows_presentation_hide() {
        let mut host = TestHost::new();
        assert!(
            host.dispatch(configure_command(
                "http://127.0.0.1:9/v1/chat/completions".into(),
                true,
                Some("synthetic-key"),
            ))
            .error
            .is_none()
        );
        assert!(host.snapshot().external_transmission_allowed);
        assert!(
            host.dispatch(ChatCommand::OpenPanel {
                panel: "memory".into(),
            })
            .error
            .is_none()
        );
        host.edit_management(1, ManagementDraftKind::BuddyRename);
        let root = host.root.as_ref().unwrap();
        let registry = root.join("buddies.v1");
        std::fs::rename(&registry, root.join("synthetic-registry-before-unknown")).unwrap();
        std::fs::create_dir(&registry).unwrap();
        let failed = host.dispatch(ChatCommand::RenameBuddy {
            label: "synthetic unknown publish".into(),
            expected_management_draft_version: 1,
        });
        assert_eq!(failed.error.as_ref().unwrap().disposition, "unknown");
        let hidden = host.dispatch(ChatCommand::HidePanel {
            panel: "memory".into(),
        });
        assert!(
            hidden.error.is_none(),
            "management presentation should remain closable"
        );

        host.worker.publish_current_snapshot();
        let projected = host.snapshot();
        assert!(!projected.external_transmission_allowed);
        assert!(!projected.model_settings.unwrap().allow_transmission);
        assert!(host.worker.hide_main(10).is_ok());
        assert!(!host.port.main_visible());
        assert!(host.worker.show_main(11).is_ok());
        assert!(host.port.main_visible());
        assert!(
            host.worker
                .buddy_storage
                .as_ref()
                .unwrap()
                .registry_is_unknown()
        );
    }

    #[test]
    fn quit_binds_all_loaded_buddy_drafts_and_invalidates_after_inactive_edit() {
        let mut host = TestHost::new();
        let first_id = host.snapshot().active_buddy_id.clone().unwrap();
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "A draft"))
                .error
                .is_none()
        );
        let second_id = host.create_buddy("Buddy B", "B persona");
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "B draft"))
                .error
                .is_none()
        );
        host.edit_management(2, ManagementDraftKind::General);

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        assert_eq!(host.snapshot().management_exit.unwrap().version, 2);
        assert!(
            host.confirm_management_exit(2, CloseIntent::QuitApplication)
                .error
                .is_none()
        );
        assert!(host.snapshot().close_confirmation);
        let captured = host.worker.pending_exit.as_ref().unwrap();
        assert_eq!(captured.chat_draft_revisions.len(), 2);
        assert!(
            captured
                .chat_draft_revisions
                .iter()
                .any(|entry| entry.buddy_id == first_id)
        );
        assert!(
            captured
                .chat_draft_revisions
                .iter()
                .any(|entry| entry.buddy_id == second_id)
        );

        let now_ms = host.worker.now_ms();
        {
            let storage = host.worker.buddy_storage.as_mut().unwrap();
            let app = storage.application_mut(&first_id).unwrap();
            let app_snapshot = app.snapshot();
            app.dispatch(
                Action::SetDraftGuarded {
                    text: "A edited after Quit prompt".into(),
                    expected_session_id: app_snapshot.active_session_id.unwrap(),
                    expected_sessions_revision: app_snapshot.sessions_revision,
                },
                now_ms,
            )
            .unwrap();
        }

        let stale = host.confirm(true);
        assert_eq!(stale.error.as_ref().unwrap().code, "confirm_stale");
        assert!(!host.worker.should_shutdown());
        assert_eq!(
            host.worker
                .buddy_storage
                .as_mut()
                .unwrap()
                .application_mut(&first_id)
                .unwrap()
                .snapshot()
                .chat
                .draft,
            "A edited after Quit prompt"
        );

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        assert!(host.snapshot().management_exit.is_some());
        assert!(
            host.confirm_management_exit(2, CloseIntent::QuitApplication)
                .error
                .is_none()
        );
        assert!(host.snapshot().close_confirmation);
        assert!(host.confirm(true).error.is_none());
        assert!(host.worker.should_shutdown());
        assert_eq!(host.snapshot().management_discarded_version, Some(2));
        let storage = host.worker.buddy_storage.as_mut().unwrap();
        assert!(
            !storage
                .application_mut(&first_id)
                .unwrap()
                .has_unsubmitted_chat_drafts()
        );
        assert!(
            !storage
                .application_mut(&second_id)
                .unwrap()
                .has_unsubmitted_chat_drafts()
        );
    }

    #[test]
    fn quit_after_revocation_still_protects_drafts_in_other_loaded_buddies() {
        let mut host = TestHost::new();
        let first_id = host.snapshot().active_buddy_id.clone().unwrap();
        let second_id = host.create_buddy("Buddy B", "B persona");
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "B survives A revoke"))
                .error
                .is_none()
        );
        host.switch_buddy(&first_id);
        assert!(host.dispatch(ChatCommand::Revoke).error.is_none());
        let revoked = host.snapshot();
        assert!(revoked.revoked);
        assert!(revoked.active_buddy_id.is_none());
        assert!(revoked.buddies.is_empty());

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        let prompt = host.snapshot();
        assert!(prompt.close_confirmation);
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle_close(CloseRequest::ConfirmClose {
            discard: true,
            expected_intent: Some(CloseIntent::QuitApplication),
            context: BuddyContext {
                expected_buddy_id: String::new(),
                expected_buddies_revision: prompt.buddies_revision,
            },
            reply,
        });
        assert!(receive.recv().unwrap().error.is_none());
        assert!(host.worker.should_shutdown());
        assert!(
            !host
                .worker
                .buddy_storage
                .as_mut()
                .unwrap()
                .application_mut(&second_id)
                .unwrap()
                .has_unsubmitted_chat_drafts()
        );
    }

    #[test]
    fn production_host_completes_loopback_request_and_reports_disconnect_unknown() {
        let (address, server) = synthetic_reply_server("synthetic loopback reply");
        let mut host = TestHost::new();
        let configured = host.dispatch(configure_command(
            format!("http://{address}/v1/chat/completions"),
            true,
            Some("synthetic-key"),
        ));
        assert!(configured.error.is_none());
        host.dispatch(guarded_draft(&host.snapshot(), "synthetic request body"));
        assert!(
            host.dispatch(guarded_send(&host.snapshot()))
                .error
                .is_none()
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        while host.snapshot().chat.status == "sending" && Instant::now() < deadline {
            host.worker.poll_external_request();
            thread::sleep(Duration::from_millis(5));
        }
        server.join().unwrap();
        let snapshot = host.snapshot();
        assert_eq!(snapshot.chat.status, "ready");
        assert!(snapshot.chat.messages.iter().any(|message| {
            message.role == "assistant" && message.text.contains("synthetic loopback reply")
        }));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut disconnected = TestHost::new();
        assert!(
            disconnected
                .dispatch(configure_command(
                    format!("http://{address}/v1/chat/completions"),
                    true,
                    Some("synthetic-key"),
                ))
                .error
                .is_none()
        );
        disconnected.dispatch(guarded_draft(
            &disconnected.snapshot(),
            "synthetic request body",
        ));
        assert!(
            disconnected
                .dispatch(guarded_send(&disconnected.snapshot()))
                .error
                .is_none()
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        while disconnected.snapshot().chat.status == "sending" && Instant::now() < deadline {
            disconnected.worker.poll_external_request();
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(disconnected.snapshot().chat.status, "unknown");
        assert!(
            disconnected
                .snapshot()
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("未知"))
        );
    }

    #[test]
    fn memory_commands_preserve_revision_conflicts_and_restore_from_isolated_root() {
        let mut host = TestHost::new();
        host.dispatch(ChatCommand::OpenPanel {
            panel: "memory".into(),
        });
        assert!(
            host.dispatch(ChatCommand::Remember {
                text: "synthetic blue tea".into(),
            })
            .error
            .is_none()
        );
        let note = host.snapshot().memories.first().unwrap().clone();
        assert!(
            host.dispatch(ChatCommand::SearchMemories {
                query: "tea".into(),
            })
            .error
            .is_none()
        );
        assert!(
            host.snapshot()
                .memories
                .iter()
                .any(|entry| entry.id == note.id)
        );
        assert!(
            host.dispatch(ChatCommand::UpdateMemory {
                id: note.id.clone(),
                revision: note.revision,
                text: "synthetic green tea".into(),
            })
            .error
            .is_none()
        );
        let changed = host.snapshot().memories[0].clone();
        assert!(changed.revision > note.revision);
        let stale = host.dispatch(ChatCommand::UpdateMemory {
            id: note.id.clone(),
            revision: note.revision,
            text: "stale overwrite".into(),
        });
        assert!(stale.error.is_some(), "stale revision must be rejected");
        assert_eq!(host.snapshot().memories[0].text, "synthetic green tea");
        let stale_delete = host.dispatch(ChatCommand::DeleteMemory {
            id: note.id.clone(),
            revision: note.revision,
        });
        assert!(
            stale_delete.error.is_some(),
            "stale memory deletion was accepted"
        );
        host.dispatch(ChatCommand::OpenPanel {
            panel: "diagnostics".into(),
        });
        assert!(
            host.dispatch(ChatCommand::RefreshDiagnostics)
                .error
                .is_none()
        );
        assert!(host.snapshot().diagnostics.len() <= 8);

        assert!(
            host.dispatch(configure_command(
                "http://127.0.0.1:9/v1/chat/completions".into(),
                true,
                Some("synthetic-key"),
            ))
            .error
            .is_none()
        );
        let root = host.preserve_root();
        let registry_json = std::fs::read_to_string(root.join("buddies.v1")).unwrap();
        assert!(!registry_json.contains("synthetic-key"));
        assert!(!registry_json.contains("allowTransmission"));
        assert!(!registry_json.contains("transmissionAllowed"));
        let restore_now = 1_800_000_010_000;
        let mut restored =
            BuddyStorage::open(&root, 2, restore_now).expect("isolated saved archive restores");
        let restored_snapshot = {
            let app = restored.active_application_mut().unwrap();
            app.dispatch(Action::Open(PanelKind::Memory), restore_now)
                .unwrap();
            app.dispatch(Action::RefreshMemories, restore_now + 1)
                .unwrap();
            app.snapshot()
        };
        assert!(!restored_snapshot.external_transmission_allowed);
        assert!(
            restored_snapshot
                .memories
                .iter()
                .any(|entry| { entry.id == note.id && entry.text == "synthetic green tea" })
        );
        drop(restored);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revocation_clears_credentials_and_rejects_late_ticket_result() {
        let (address, accepted, release, server) = gated_synthetic_server("late synthetic secret");
        let mut host = TestHost::new();
        let first_id = host.snapshot().active_buddy_id.clone().unwrap();
        let second_id = host.create_buddy("Buddy B", "B persona");
        host.switch_buddy(&first_id);
        assert!(
            host.dispatch(configure_command(
                format!("http://{address}/v1/chat/completions"),
                true,
                Some("synthetic-key"),
            ))
            .error
            .is_none()
        );
        host.dispatch(guarded_draft(&host.snapshot(), "synthetic request body"));
        assert!(
            host.dispatch(guarded_send(&host.snapshot()))
                .error
                .is_none()
        );
        accepted.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(host.worker.model_requests.contains_key(&first_id));

        assert!(host.dispatch(ChatCommand::Revoke).error.is_none());
        let revoked = host.snapshot();
        assert!(revoked.revoked);
        assert!(!revoked.external_transmission_allowed);
        assert!(revoked.model_settings.is_none());
        assert!(host.worker.model_key.is_none());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while host.worker.model_requests.contains_key(&first_id) && Instant::now() < deadline {
            host.worker.poll_external_request();
            thread::sleep(Duration::from_millis(5));
        }
        server.join().unwrap();
        assert!(!host.worker.model_requests.contains_key(&first_id));
        let final_snapshot = host.snapshot();
        assert!(final_snapshot.revoked);
        assert!(final_snapshot.model_settings.is_none());
        assert!(
            !final_snapshot
                .chat
                .messages
                .iter()
                .any(|message| { message.text.contains("late synthetic secret") })
        );
        let second_runtime = host.worker.buddy_runtime.get(&second_id).unwrap();
        assert!(second_runtime.api_key.is_none());
        assert!(!second_runtime.allow_transmission);
        let second_app = host
            .worker
            .buddy_storage
            .as_mut()
            .unwrap()
            .application_mut(&second_id)
            .unwrap()
            .snapshot();
        assert!(
            !second_app
                .chat
                .messages
                .iter()
                .any(|message| { message.text.contains("late synthetic secret") })
        );
    }

    #[derive(Default)]
    struct RecordingState {
        effects: Vec<String>,
        errors: Vec<String>,
        snapshots: Vec<Snapshot>,
        frames: Vec<FrameSnapshot>,
        main_visible: bool,
        pet_visible: bool,
        exit_code: Option<i32>,
        stopped: bool,
        after_stop: Vec<String>,
        fail_show: bool,
        fail_hide: bool,
        fail_hit_mask: bool,
    }

    #[derive(Clone, Default)]
    struct RecordingPort {
        state: Arc<Mutex<RecordingState>>,
    }

    impl RecordingPort {
        fn visible(main: bool, pet: bool) -> Self {
            let port = Self::default();
            {
                let mut state = port.state.lock().unwrap();
                state.main_visible = main;
                state.pet_visible = pet;
            }
            port
        }

        fn record(&self, effect: String) {
            let mut state = self.state.lock().unwrap();
            if state.stopped {
                state.after_stop.push(effect.clone());
            }
            state.effects.push(effect);
        }

        fn main_visible(&self) -> bool {
            self.state.lock().unwrap().main_visible
        }

        fn last_effect(&self) -> Option<String> {
            self.state.lock().unwrap().effects.last().cloned()
        }

        fn errors(&self) -> Vec<String> {
            self.state.lock().unwrap().errors.clone()
        }

        fn after_stop(&self) -> Vec<String> {
            self.state.lock().unwrap().after_stop.clone()
        }

        fn exit_code(&self) -> Option<i32> {
            self.state.lock().unwrap().exit_code
        }
    }

    impl HostPort for RecordingPort {
        fn show(&mut self, label: &str) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            if state.fail_show {
                return Err("test show failure".to_owned());
            }
            if state.stopped {
                state.after_stop.push(format!("show:{label}"));
            }
            state.effects.push(format!("show:{label}"));
            if label == "main" {
                state.main_visible = true;
            } else if label == "pet" {
                state.pet_visible = true;
            }
            Ok(())
        }

        fn hide(&mut self, label: &str) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            if state.fail_hide {
                return Err("test hide failure".to_owned());
            }
            if state.stopped {
                state.after_stop.push(format!("hide:{label}"));
            }
            state.effects.push(format!("hide:{label}"));
            if label == "main" {
                state.main_visible = false;
            } else if label == "pet" {
                state.pet_visible = false;
            }
            Ok(())
        }

        fn focus(&mut self, label: &str) -> Result<(), String> {
            self.record(format!("focus:{label}"));
            Ok(())
        }

        fn unminimize(&mut self, label: &str) -> Result<(), String> {
            self.record(format!("unminimize:{label}"));
            Ok(())
        }

        fn is_visible(&mut self, label: &str) -> Result<bool, String> {
            let state = self.state.lock().unwrap();
            Ok(match label {
                "main" => state.main_visible,
                "pet" => state.pet_visible,
                _ => false,
            })
        }

        fn emit_snapshot(&mut self, snapshot: &Snapshot) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            if state.stopped {
                state
                    .after_stop
                    .push(format!("snapshot:{}", snapshot.revision));
            }
            state
                .effects
                .push(format!("snapshot:{}", snapshot.revision));
            state.snapshots.push(snapshot.clone());
            Ok(())
        }

        fn emit_frame(&mut self, frame: FrameSnapshot) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            if state.stopped {
                state
                    .after_stop
                    .push(format!("frame:{:?}", frame.frame_index));
            }
            state.effects.push(format!("frame:{:?}", frame.frame_index));
            state.frames.push(frame);
            Ok(())
        }

        fn apply_hit_mask(
            &mut self,
            _width: u16,
            _height: u16,
            _alpha: &[u8],
        ) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            state.effects.push("hit_mask".to_owned());
            if state.fail_hit_mask {
                Err("test hit mask failure".to_owned())
            } else {
                Ok(())
            }
        }

        fn poll_hit_mask(&mut self, _width: u16, _height: u16, _alpha: &[u8]) {}

        fn notify_error(&mut self, code: &str) {
            self.state.lock().unwrap().errors.push(code.to_owned());
        }

        fn request_exit(&mut self, code: i32) {
            let mut state = self.state.lock().unwrap();
            if state.stopped {
                state.after_stop.push(format!("exit:{code}"));
            }
            state.effects.push(format!("exit:{code}"));
            state.exit_code = Some(code);
            state.stopped = true;
        }
    }

    struct TestHost {
        worker: HostWorker<RecordingPort>,
        port: RecordingPort,
        latest: Arc<Mutex<Snapshot>>,
        exit: Arc<ExitMailbox>,
        management_drafts: Arc<ManagementDraftMailbox>,
        root: Option<std::path::PathBuf>,
    }

    impl TestHost {
        fn new_volatile() -> Self {
            let port = RecordingPort::visible(true, true);
            let epoch = 1_800_000_000_000;
            let app = chat_app();
            let latest = Arc::new(Mutex::new(project(&app)));
            let frame = Arc::new(Mutex::new(FrameSnapshot::builtin(0, false, false)));
            let exit = Arc::new(ExitMailbox::new());
            let management_drafts = Arc::new(ManagementDraftMailbox::default());
            let worker = HostWorker::with_application(
                port.clone(),
                app,
                HostWorkerContext {
                    model_settings: None,
                    persistence_enabled: false,
                    management_drafts: management_drafts.clone(),
                    epoch_start_ms: epoch,
                    latest: latest.clone(),
                    frame,
                },
            );
            Self {
                worker,
                port,
                latest,
                exit,
                management_drafts,
                root: None,
            }
        }

        fn new() -> Self {
            let port = RecordingPort::visible(true, true);
            let root = isolated_root();
            let epoch = 1_800_000_000_000;
            let mut storage =
                BuddyStorage::open(&root, 1, epoch).expect("isolated buddy storage opens");
            storage
                .active_application_mut()
                .unwrap()
                .dispatch(Action::Open(PanelKind::Chat), epoch)
                .expect("isolated chat opens");
            let initial = project_storage(&storage).expect("isolated buddy snapshot projects");
            let latest = Arc::new(Mutex::new(initial));
            let frame = Arc::new(Mutex::new(FrameSnapshot::builtin(0, false, false)));
            let exit = Arc::new(ExitMailbox::new());
            let management_drafts = Arc::new(ManagementDraftMailbox::default());
            let worker = HostWorker::with_buddy_storage(
                port.clone(),
                storage,
                HostWorkerContext {
                    model_settings: None,
                    persistence_enabled: true,
                    management_drafts: management_drafts.clone(),
                    epoch_start_ms: epoch,
                    latest: latest.clone(),
                    frame,
                },
            )
            .expect("worker attaches to shared buddy storage");
            Self {
                worker,
                port,
                latest,
                exit,
                management_drafts,
                root: Some(root),
            }
        }

        fn preserve_root(mut self) -> std::path::PathBuf {
            self.worker.app.take();
            self.worker.buddy_storage.take();
            self.root.take().expect("test root is available")
        }

        fn dispatch(&mut self, command: ChatCommand) -> DispatchResult {
            let (reply, receive) = mpsc::sync_channel(1);
            let context = self.worker.active_context().unwrap();
            // 经生产请求入口进入协调器，而不是直接调用 ChatApplication。
            self.worker
                .handle(Request::Dispatch(command, context, reply));
            receive.recv().expect("dispatch reply")
        }

        fn material_command(
            &mut self,
            command: MaterialCommand,
            context: BuddyContext,
        ) -> MaterialReply {
            let (reply, receive) = mpsc::sync_channel(1);
            self.worker.handle(Request::MaterialCommand {
                command,
                context,
                reply,
            });
            receive.recv().expect("material command reply")
        }

        fn material_pixels(&mut self, frame: MaterialFrameTriple) -> Result<Vec<u8>, String> {
            let (reply, receive) = mpsc::sync_channel(1);
            self.worker.handle(Request::MaterialPixels { frame, reply });
            receive.recv().expect("material pixel reply")
        }

        fn close(&mut self) -> DispatchResult {
            let (reply, receive) = mpsc::sync_channel(1);
            let context = self.worker.active_context().unwrap();
            self.worker
                .handle_close(CloseRequest::CloseChat { context, reply });
            receive.recv().expect("close reply")
        }

        fn confirm_with_intent(
            &mut self,
            discard: bool,
            expected_intent: Option<CloseIntent>,
        ) -> DispatchResult {
            let (reply, receive) = mpsc::sync_channel(1);
            let context = self.worker.active_context().unwrap();
            self.worker.handle_close(CloseRequest::ConfirmClose {
                discard,
                expected_intent,
                context,
                reply,
            });
            receive.recv().expect("confirm reply")
        }

        fn confirm(&mut self, discard: bool) -> DispatchResult {
            self.confirm_with_intent(discard, self.snapshot().close_intent)
        }

        fn confirm_management_exit(&mut self, version: u64, intent: CloseIntent) -> DispatchResult {
            let (reply, receive) = mpsc::sync_channel(1);
            let context = self.worker.active_context().unwrap();
            self.worker
                .handle_close(CloseRequest::ConfirmManagementDiscard {
                    version,
                    intent,
                    context,
                    reply,
                });
            receive.recv().expect("management confirmation reply")
        }

        fn cancel_management_exit(&mut self, version: u64, intent: CloseIntent) -> DispatchResult {
            let (reply, receive) = mpsc::sync_channel(1);
            let context = self.worker.active_context().unwrap();
            self.worker
                .handle_close(CloseRequest::CancelManagementExit {
                    version,
                    intent,
                    context,
                    reply,
                });
            receive.recv().expect("management cancel reply")
        }

        fn visibility(&mut self, label: &'static str, visible: bool) -> Result<(), String> {
            let (reply, receive) = mpsc::sync_channel(1);
            let context = (label == "main").then(|| self.worker.active_context().unwrap());
            self.worker.handle(Request::SetVisibility {
                label,
                visible,
                context,
                reply,
            });
            receive.recv().expect("visibility reply")
        }

        fn request_exit(&mut self, intent: ExitIntent) -> Result<(), String> {
            self.exit.submit(intent);
            let request = self
                .exit
                .take()
                .expect("exit mailbox has the submitted intent");
            self.worker.handle_exit(request)
        }

        fn snapshot(&self) -> Snapshot {
            self.latest.lock().unwrap().clone()
        }

        fn edit_management(&mut self, version: u64, kind: ManagementDraftKind) {
            let context = self.worker.active_context().unwrap();
            self.management_drafts
                .update(version, true, kind, context)
                .unwrap();
            self.worker.sync_management_drafts();
        }

        fn create_buddy(&mut self, label: &str, persona: &str) -> String {
            let context = self.worker.active_context().unwrap();
            let version = self.management_drafts.current().version + 1;
            self.management_drafts
                .update(version, true, ManagementDraftKind::BuddyCreate, context)
                .unwrap();
            self.worker.sync_management_drafts();
            let result = self.dispatch(ChatCommand::CreateBuddy {
                label: label.into(),
                persona: persona.into(),
                expected_management_draft_version: version,
            });
            assert!(
                result.error.is_none(),
                "buddy creation failed: {:?}",
                result.error.map(|error| error.message)
            );
            self.snapshot()
                .active_buddy_id
                .expect("created buddy is selected")
        }

        fn switch_buddy(&mut self, buddy_id: &str) -> Snapshot {
            let result = self.dispatch(ChatCommand::SwitchBuddy {
                id: buddy_id.to_owned(),
            });
            assert!(
                result.error.is_none(),
                "buddy switch failed: {:?}",
                result.error.map(|error| error.message)
            );
            result.snapshot.expect("switch publishes a snapshot")
        }

        /// 走生产 `Request::Snapshot` 入口重新读取状态，而不是直接读缓存。
        fn read_snapshot(&mut self) -> DispatchResult {
            let (reply, receive) = mpsc::sync_channel(1);
            self.worker.handle(Request::Snapshot(reply));
            receive.recv().expect("snapshot reply")
        }

        fn app_snapshot(&self) -> AppSnapshot {
            self.worker.app_snapshot().unwrap()
        }

        fn chat_visibility(&self) -> PanelVisibility {
            chat_visibility(&self.app_snapshot())
        }
    }

    impl Drop for TestHost {
        fn drop(&mut self) {
            self.worker.app.take();
            self.worker.buddy_storage.take();
            if let Some(mut animator) = self.worker.animator.take() {
                let _ = animator.stop();
            }
            if let Some(root) = self.root.take() {
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }

    #[test]
    fn management_status_mailbox_survives_normal_queue_backpressure_and_rejects_reordering() {
        let mut host = TestHost::new();
        let mailbox = host.management_drafts.clone();
        let (sender, _receiver) = mpsc::sync_channel(1);
        let (reply, _receive) = mpsc::sync_channel(1);
        sender.try_send(Request::Snapshot(reply)).unwrap();
        let context = host.worker.active_context().unwrap();

        let accepted = update_management_draft_state(
            &mailbox,
            &sender,
            20,
            true,
            ManagementDraftKind::General,
            context.clone(),
        )
        .unwrap();
        assert_eq!(accepted.version, 20);
        assert!(accepted.dirty);
        assert_eq!(mailbox.current().version, 20);
        assert!(mailbox.current().dirty);

        let stale = update_management_draft_state(
            &mailbox,
            &sender,
            19,
            false,
            ManagementDraftKind::None,
            context.clone(),
        )
        .unwrap();
        assert_eq!(stale.version, 20);
        assert!(
            stale.dirty,
            "an older clean update must not clear newer edits"
        );
        assert!(
            update_management_draft_state(
                &mailbox,
                &sender,
                20,
                false,
                ManagementDraftKind::None,
                context.clone(),
            )
            .is_err()
        );
        assert!(
            update_management_draft_state(
                &mailbox,
                &sender,
                MAX_MANAGEMENT_DRAFT_VERSION + 1,
                true,
                ManagementDraftKind::General,
                context,
            )
            .is_err()
        );

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        assert!(host.snapshot().management_draft.dirty);
        assert_eq!(host.snapshot().management_exit.unwrap().version, 20);
    }

    #[test]
    fn management_edits_guard_hidden_window_exit_and_confirmation_is_version_bound() {
        let mut host = TestHost::new();
        host.edit_management(12, ManagementDraftKind::General);
        host.visibility("main", false).unwrap();
        assert!(!host.port.main_visible());

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        let prompt = host.snapshot();
        assert!(
            host.port.main_visible(),
            "a hidden window must reopen for confirmation"
        );
        assert!(prompt.management_draft.dirty);
        assert_eq!(prompt.management_draft.version, 12);
        assert_eq!(
            prompt.management_exit.unwrap().intent,
            CloseIntent::QuitApplication
        );
        assert_eq!(host.worker.shutdown_code, None);

        let cancelled = host.cancel_management_exit(12, CloseIntent::QuitApplication);
        assert!(cancelled.error.is_none());
        assert!(host.snapshot().management_draft.dirty);
        assert!(host.snapshot().management_exit.is_none());
        assert_eq!(host.worker.shutdown_code, None);

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        host.edit_management(13, ManagementDraftKind::General);
        let stale = host.confirm_management_exit(12, CloseIntent::QuitApplication);
        assert_eq!(
            stale.error.as_ref().map(|error| error.code.as_str()),
            Some("management_confirmation_stale")
        );
        assert!(host.snapshot().management_draft.dirty);
        assert_eq!(host.snapshot().management_exit.unwrap().version, 13);
        assert_eq!(host.worker.shutdown_code, None);

        let confirmed = host.confirm_management_exit(13, CloseIntent::QuitApplication);
        assert!(confirmed.error.is_none());
        assert!(!host.snapshot().management_draft.dirty);
        assert_eq!(host.snapshot().management_discarded_version, Some(13));
        assert_eq!(host.worker.shutdown_code, Some(0));
    }

    #[test]
    fn ui_close_chat_uses_management_confirmation_without_clearing_drafts() {
        let mut host = TestHost::new();
        host.edit_management(3, ManagementDraftKind::General);
        let result = host.close();
        assert!(result.error.is_none());
        assert_eq!(
            result.snapshot.unwrap().management_exit.unwrap().intent,
            CloseIntent::CloseChat
        );

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        let stale_intent = host.confirm_management_exit(3, CloseIntent::CloseChat);
        assert_eq!(
            stale_intent.error.as_ref().map(|error| error.code.as_str()),
            Some("management_confirmation_stale")
        );
        assert_eq!(
            host.snapshot().management_exit.unwrap().intent,
            CloseIntent::QuitApplication
        );

        let cancelled = host.cancel_management_exit(3, CloseIntent::QuitApplication);
        assert!(cancelled.error.is_none());
        assert!(host.snapshot().management_draft.dirty);
        assert!(host.port.main_visible());
        assert_eq!(host.worker.shutdown_code, None);
    }

    fn chat_app() -> ChatApplication {
        let mut app = ChatApplication::new(1).unwrap();
        app.dispatch(Action::Open(PanelKind::Chat), 0).unwrap();
        app
    }

    fn assistant_text(snapshot: &Snapshot) -> Option<&str> {
        snapshot
            .chat
            .messages
            .iter()
            .find(|message| message.role == "assistant")
            .map(|message| message.text.as_str())
    }

    #[test]
    fn command_budget_and_queue_are_bounded() {
        assert!(dispatch_source_allowed("main"));
        assert!(!dispatch_source_allowed("pet"));
        assert!(!command_over_budget(&guarded_draft(&empty_snapshot(), "x")));
        assert!(command_over_budget(&guarded_draft(
            &empty_snapshot(),
            "x".repeat(MAX_COMMAND_BYTES)
        )));
        assert!(validate_mask(256, 256, MAX_HIT_CELLS).is_ok());
        assert!(validate_mask(257, 1, 257).is_err());
        assert!(validate_mask(4, 4, 15).is_err());
        let (sender, _receiver) = mpsc::sync_channel(1);
        sender.try_send(Request::TogglePause).unwrap();
        assert!(sender.try_send(Request::TogglePause).is_err());
    }

    #[test]
    fn rejected_snapshot_has_no_data_and_uses_the_standard_envelope() {
        let actual = snapshot_response("pet", project(&chat_app()));
        let json = serde_json::to_value(actual).unwrap();
        assert!(json["snapshot"].is_null());
        assert_eq!(json["error"]["code"], "command_source");
        assert!(json.get("revision").is_none());

        let accepted = snapshot_response("main", project(&chat_app()));
        let json = serde_json::to_value(accepted).unwrap();
        assert!(json["snapshot"].is_object());
        assert_eq!(json["snapshot"]["activeSessionId"], "session-1");
        assert_eq!(json["snapshot"]["sessionsRevision"], 1);
        assert_eq!(json["snapshot"]["sessions"][0]["id"], "session-1");
        assert!(json["snapshot"]["sessions"][0].get("draft").is_none());
        assert!(json["error"].is_null());
    }

    #[test]
    fn offline_chat_dispatch_close_cancel_and_revoke_use_application_contract() {
        let mut app = chat_app();
        app.dispatch(Action::SetDraft("hello offline".into()), 1)
            .unwrap();
        app.dispatch(Action::Send, 2).unwrap();
        let sent = app.snapshot();
        assert_eq!(sent.mode, "offline");
        assert!(
            sent.chat
                .messages
                .iter()
                .any(|message| message.role.as_str() == "user" && message.text == "hello offline")
        );

        app.dispatch(Action::SetDraft("keep me".into()), 3).unwrap();
        app.dispatch(Action::Close(PanelKind::Chat), 4).unwrap();
        assert!(project(&app).close_confirmation);
        app.dispatch(
            Action::ConfirmClose {
                panel: PanelKind::Chat,
                discard: false,
            },
            5,
        )
        .unwrap();
        assert_eq!(app.snapshot().chat.draft, "keep me");
        app.dispatch(Action::Close(PanelKind::Chat), 6).unwrap();
        app.dispatch(
            Action::ConfirmClose {
                panel: PanelKind::Chat,
                discard: true,
            },
            7,
        )
        .unwrap();
        app.dispatch(Action::Revoke, 8).unwrap();
        assert!(app.snapshot().revoked);
    }

    #[test]
    fn animator_pause_resume_is_deterministic() {
        let (mut animator, player) = create_animator();
        animator.request_action("idle").unwrap();
        animator.advance(200).unwrap();
        assert_eq!(player.0.lock().unwrap().elapsed_ms, 200);
        animator.pause().unwrap();
        animator.advance(0).unwrap();
        assert_eq!(player.0.lock().unwrap().elapsed_ms, 200);
        animator.resume().unwrap();
        animator.advance(200).unwrap();
        assert_eq!(player.0.lock().unwrap().elapsed_ms, 400);
    }

    #[test]
    fn frame_presentation_sequence_advances_with_authoritative_builtin_frames() {
        let mut host = TestHost::new();
        host.worker
            .tick(true, material_descriptor().frame_duration_ms);
        let first = host.worker.last_frame.clone().unwrap();
        assert_eq!(first.source, "builtin");
        assert_eq!(first.clip_id.as_deref(), Some("idle-four-frame"));
        assert_eq!(
            (first.width, first.height, first.frame_index),
            (320, 320, Some(1))
        );
        host.worker
            .tick(true, material_descriptor().frame_duration_ms);
        let second = host.worker.last_frame.clone().unwrap();
        assert_eq!(second.frame_index, Some(2));
        assert!(second.presentation_sequence > first.presentation_sequence);
    }

    #[test]
    fn host_close_without_draft_reopens_and_sends() {
        let mut host = TestHost::new();
        let close = host.close();
        assert!(close.error.is_none());
        assert!(!host.snapshot().close_confirmation);
        assert_eq!(host.chat_visibility(), PanelVisibility::Closed);
        assert!(!host.port.main_visible());

        host.visibility("main", true).unwrap();
        assert!(host.port.main_visible());
        assert_eq!(host.chat_visibility(), PanelVisibility::Visible);

        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "after reopen"))
                .error
                .is_none()
        );
        let send = host.dispatch(guarded_send(&host.snapshot()));
        assert!(send.error.is_none());
        let snapshot = host.snapshot();
        assert_eq!(snapshot.chat.draft, "");
        assert!(
            snapshot
                .chat
                .messages
                .iter()
                .any(|message| message.role == "user" && message.text == "after reopen")
        );
        let reply = assistant_text(&snapshot).expect("offline reply exists");
        assert!(!reply.trim().is_empty());
    }

    #[test]
    fn session_ipc_uses_directory_guards_and_keeps_archived_sessions_read_only() {
        let mut host = TestHost::new_volatile();
        let initial = host.snapshot();
        let created = host.dispatch(ChatCommand::NewSession {
            expected_session_id: initial.active_session_id.clone().unwrap(),
            expected_sessions_revision: initial.sessions_revision,
        });
        assert!(created.error.is_none());
        let current = host.snapshot();
        assert_eq!(current.active_session_id.as_deref(), Some("session-2"));
        assert_eq!(current.sessions_revision, initial.sessions_revision + 1);

        let stale = host.dispatch(ChatCommand::SetDraft {
            text: "stale draft".into(),
            expected_session_id: initial.active_session_id.unwrap(),
            expected_sessions_revision: initial.sessions_revision,
        });
        assert_eq!(stale.error.unwrap().code, "stale_session_command");
        assert_eq!(host.snapshot().chat.draft, "");

        let archived = host.dispatch(ChatCommand::ArchiveSession {
            id: String::from("session-2"),
            expected_session_id: String::from("session-2"),
            expected_sessions_revision: current.sessions_revision,
        });
        assert!(archived.error.is_none());
        let archived = host.snapshot();
        assert!(archived.sessions[1].archived);
        let send = host.dispatch(ChatCommand::Send {
            expected_session_id: String::from("session-2"),
            expected_sessions_revision: archived.sessions_revision,
        });
        assert_eq!(send.error.unwrap().code, "session_archived");
    }

    #[test]
    fn hidden_tray_quit_binds_all_chat_and_management_drafts_until_final_confirmation() {
        let mut host = TestHost::new_volatile();
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "session A draft"))
                .error
                .is_none()
        );
        let first = host.snapshot();
        assert!(
            host.dispatch(ChatCommand::NewSession {
                expected_session_id: first.active_session_id.unwrap(),
                expected_sessions_revision: first.sessions_revision,
            })
            .error
            .is_none()
        );
        assert_eq!(
            host.snapshot().active_session_id.as_deref(),
            Some("session-2")
        );
        host.edit_management(7, ManagementDraftKind::General);
        host.visibility("main", false).unwrap();
        assert!(!host.port.main_visible());

        host.exit.submit(ExitIntent::QuitApplication);
        let tray_request = host
            .exit
            .take()
            .expect("tray quit is in the production mailbox");
        host.worker.handle_exit(tray_request).unwrap();
        assert!(host.port.main_visible());
        assert_eq!(host.snapshot().management_exit.unwrap().version, 7);
        assert!(!host.snapshot().close_confirmation);

        let management = host.confirm_management_exit(7, CloseIntent::QuitApplication);
        assert!(management.error.is_none());
        let chat_prompt = host.snapshot();
        assert!(chat_prompt.close_confirmation);
        assert_eq!(chat_prompt.close_intent, Some(CloseIntent::QuitApplication));
        assert!(chat_prompt.management_draft.dirty);
        assert_eq!(host.worker.shutdown_code, None);

        let cancel = host.confirm(false);
        assert!(cancel.error.is_none());
        assert!(!host.worker.should_shutdown());
        assert!(host.snapshot().management_draft.dirty);
        let current = host.snapshot();
        assert!(
            host.dispatch(ChatCommand::SwitchSession {
                id: String::from("session-1"),
                expected_session_id: current.active_session_id.unwrap(),
                expected_sessions_revision: current.sessions_revision,
            })
            .error
            .is_none()
        );
        assert_eq!(host.snapshot().chat.draft, "session A draft");
        let current = host.snapshot();
        assert!(
            host.dispatch(ChatCommand::SwitchSession {
                id: String::from("session-2"),
                expected_session_id: current.active_session_id.unwrap(),
                expected_sessions_revision: current.sessions_revision,
            })
            .error
            .is_none()
        );
        assert_eq!(host.snapshot().chat.draft, "");

        host.exit.submit(ExitIntent::QuitApplication);
        host.worker
            .handle_exit(host.exit.take().expect("second tray quit"))
            .unwrap();
        assert_eq!(host.snapshot().management_exit.unwrap().version, 7);
        assert!(
            host.confirm_management_exit(7, CloseIntent::QuitApplication)
                .error
                .is_none()
        );
        assert!(host.snapshot().close_confirmation);
        assert!(host.snapshot().management_draft.dirty);
        let confirmed = host.confirm(true);
        assert!(confirmed.error.is_none());
        assert!(host.worker.should_shutdown());
        assert!(!host.management_drafts.current().dirty);
        assert_eq!(host.management_drafts.current().discarded_version, Some(7));
        assert!(
            !host
                .worker
                .app
                .as_ref()
                .unwrap()
                .has_unsubmitted_chat_drafts()
        );
    }

    #[test]
    fn chat_edit_invalidates_a_pending_quit_confirmation_and_preserves_other_sessions() {
        let mut host = TestHost::new_volatile();
        host.dispatch(guarded_draft(&host.snapshot(), "inactive draft"));
        let first = host.snapshot();
        host.dispatch(ChatCommand::NewSession {
            expected_session_id: first.active_session_id.unwrap(),
            expected_sessions_revision: first.sessions_revision,
        });
        host.request_exit(ExitIntent::QuitApplication).unwrap();
        assert!(host.snapshot().close_confirmation);

        let (sender, receiver) = mpsc::sync_channel(1);
        let (edit_reply, edit_result) = mpsc::sync_channel(1);
        let current = host.snapshot();
        let context = host.worker.active_context().unwrap();
        sender
            .try_send(Request::Dispatch(
                ChatCommand::SetDraft {
                    text: String::from("accepted edit after prompt"),
                    expected_session_id: current.active_session_id.unwrap(),
                    expected_sessions_revision: current.sessions_revision,
                },
                context.clone(),
                edit_reply,
            ))
            .unwrap();
        let (confirmation, confirm_result) = mpsc::sync_channel(1);
        handle_close_with_exit_mailbox(
            &mut host.worker,
            CloseRequest::ConfirmClose {
                discard: true,
                expected_intent: Some(CloseIntent::QuitApplication),
                context,
                reply: confirmation,
            },
            &host.exit,
            &receiver,
        );
        assert!(edit_result.recv().unwrap().error.is_none());
        assert_eq!(
            confirm_result.recv().unwrap().error.unwrap().code,
            "confirm_stale"
        );
        assert!(!host.worker.should_shutdown());
        assert_eq!(host.snapshot().chat.draft, "accepted edit after prompt");
        let current = host.snapshot();
        assert!(
            host.dispatch(ChatCommand::SwitchSession {
                id: String::from("session-1"),
                expected_session_id: current.active_session_id.unwrap(),
                expected_sessions_revision: current.sessions_revision,
            })
            .error
            .is_none()
        );
        assert_eq!(host.snapshot().chat.draft, "inactive draft");
    }

    #[test]
    fn close_chat_keeps_drafts_owned_by_other_sessions() {
        let mut host = TestHost::new_volatile();
        host.dispatch(guarded_draft(&host.snapshot(), "preserved inactive draft"));
        let first = host.snapshot();
        assert!(
            host.dispatch(ChatCommand::NewSession {
                expected_session_id: first.active_session_id.unwrap(),
                expected_sessions_revision: first.sessions_revision,
            })
            .error
            .is_none()
        );
        let close = host.close();
        assert!(close.error.is_none());
        assert_eq!(host.chat_visibility(), PanelVisibility::Closed);

        host.visibility("main", true).unwrap();
        let current = host.snapshot();
        assert!(
            host.dispatch(ChatCommand::SwitchSession {
                id: String::from("session-1"),
                expected_session_id: current.active_session_id.unwrap(),
                expected_sessions_revision: current.sessions_revision,
            })
            .error
            .is_none()
        );
        assert_eq!(host.snapshot().chat.draft, "preserved inactive draft");
    }

    #[test]
    fn confirmed_chat_close_projects_a_versioned_discard_receipt_on_reopen_and_reread() {
        let mut host = TestHost::new_volatile();
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "discard me"))
                .error
                .is_none()
        );
        let close = host.close();
        assert!(close.error.is_none());
        assert!(host.snapshot().close_confirmation);
        let active = host.snapshot().active_session_id.expect("active session");

        let confirmed = host.confirm(true);
        assert!(confirmed.error.is_none());
        let discarded = host.snapshot();
        assert_eq!(discarded.chat.draft, "");
        let receipt = discarded
            .chat_draft
            .discard
            .as_ref()
            .expect("confirmed chat close emits an authoritative receipt");
        assert_eq!(receipt.revision, discarded.chat_draft.revision);
        assert_eq!(receipt.session_ids, vec![active.clone()]);
        let entry = discarded
            .chat_draft
            .sessions
            .iter()
            .find(|session| session.id == active)
            .expect("discarded session has a body-free watermark");
        assert_eq!(entry.revision, receipt.revision);
        assert_eq!(entry.discarded_revision, Some(receipt.revision));
        assert!(!host.port.main_visible());

        // 隐藏重开并走生产快照读取：普通重读不得伪造新回执或再清其他会话。
        host.visibility("main", true).unwrap();
        let reread = host.read_snapshot();
        let reread = reread.snapshot.expect("snapshot envelope");
        assert_eq!(reread.chat.draft, "");
        let reread_receipt = reread
            .chat_draft
            .discard
            .as_ref()
            .expect("receipt survives a status re-read");
        assert_eq!(reread_receipt.revision, receipt.revision);
        assert_eq!(reread_receipt.session_ids, vec![active.clone()]);
        assert_eq!(reread.chat_draft.revision, discarded.chat_draft.revision);
        let reread_entry = reread
            .chat_draft
            .sessions
            .iter()
            .find(|session| session.id == active)
            .expect("reread keeps the same per-session watermark");
        assert_eq!(reread_entry.revision, receipt.revision);
        assert_eq!(reread_entry.discarded_revision, Some(receipt.revision));

        // 取消后的普通关闭确认不会产生新回执。
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "kept"))
                .error
                .is_none()
        );
        let keep = host.close();
        assert!(keep.error.is_none());
        let cancelled = host.confirm(false);
        assert!(cancelled.error.is_none());
        assert_eq!(host.snapshot().chat.draft, "kept");
        assert_eq!(
            host.snapshot()
                .chat_draft
                .discard
                .as_ref()
                .map(|receipt| receipt.revision),
            Some(reread_receipt.revision),
            "an ordinary edit/cancel keeps the old receipt historical, never a new discard"
        );

        // 新会话编辑只推进自己的会话修订和全局修订，A 的丢弃水位仍可读。
        let before_new = host.snapshot();
        assert!(
            host.dispatch(ChatCommand::NewSession {
                expected_session_id: before_new.active_session_id.unwrap(),
                expected_sessions_revision: before_new.sessions_revision,
            })
            .error
            .is_none()
        );
        let new_session = host.snapshot().active_session_id.expect("new session");
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "B newer draft"))
                .error
                .is_none()
        );
        let after_new = host.snapshot();
        let a_entry = after_new
            .chat_draft
            .sessions
            .iter()
            .find(|session| session.id == active)
            .expect("A keeps its per-session entry");
        assert_eq!(
            a_entry.discarded_revision,
            Some(reread_receipt.revision),
            "an edit of B must not invalidate A's discard watermark"
        );
        let b_entry = after_new
            .chat_draft
            .sessions
            .iter()
            .find(|session| session.id == new_session)
            .expect("B has its own entry");
        assert_eq!(b_entry.discarded_revision, None);
        assert!(b_entry.revision >= after_new.chat_draft.revision);
        assert!(after_new.chat_draft.revision > reread_receipt.revision);
    }

    #[test]
    fn hidden_reopen_keeps_the_draft_without_faking_a_discard_receipt() {
        let mut host = TestHost::new_volatile();
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "hidden keeps me"))
                .error
                .is_none()
        );
        host.visibility("main", false).unwrap();
        assert!(host.snapshot().chat.draft.is_empty());
        assert!(host.snapshot().chat_draft.discard.is_none());
        host.visibility("main", true).unwrap();
        assert_eq!(host.snapshot().chat.draft, "hidden keeps me");
        assert!(host.snapshot().chat_draft.discard.is_none());
    }

    #[test]
    fn candidate_publish_failure_keeps_unknown_disposition_and_evidence() {
        let mut host = TestHost::new();
        let before = host.snapshot();
        let root = host.root.clone().unwrap();
        let registry = root.join("buddies.v1");
        std::fs::rename(&registry, root.join("buddies.saved")).unwrap();
        std::fs::create_dir(&registry).unwrap();
        host.edit_management(1, ManagementDraftKind::BuddyCreate);
        let result = host.dispatch(ChatCommand::CreateBuddy {
            label: "synthetic failed creation".into(),
            persona: "synthetic candidate persona".into(),
            expected_management_draft_version: 1,
        });
        assert!(root.join("buddies.create-candidate").is_file());
        assert!(root.join("buddies.pending").is_file());
        assert_eq!(result.error.unwrap().disposition, "unknown");
        assert_eq!(host.snapshot().active_buddy_id, before.active_buddy_id);
        assert!(
            host.worker
                .buddy_storage
                .as_ref()
                .unwrap()
                .registry_is_unknown()
        );
    }

    #[test]
    fn stale_quit_context_is_rejected_before_associating_exit() {
        for same_buddy in [false, true] {
            let mut host = TestHost::new();
            let previous = host.worker.active_context().unwrap();
            host.exit
                .submit_with_context(ExitIntent::QuitApplication, Some(previous));
            let request = host.exit.take().unwrap();
            if same_buddy {
                let configured = host.dispatch(configure_command(
                    "http://127.0.0.1:9/v1/chat/completions".into(),
                    false,
                    Some(""),
                ));
                assert!(configured.error.is_none());
            } else {
                host.create_buddy("synthetic second", "synthetic second persona");
            }
            let selected = host.snapshot().active_buddy_id;
            assert!(host.worker.handle_exit(request).is_err());
            assert!(!host.worker.should_shutdown());
            assert!(host.worker.pending_exit.is_none());
            assert_eq!(host.snapshot().active_buddy_id, selected);
            assert!(host.port.is_visible("main").unwrap());
            host.request_exit(ExitIntent::QuitApplication).unwrap();
            assert!(host.worker.should_shutdown());
        }
    }

    #[test]
    fn pet_visibility_is_presentation_only_and_keeps_chat_state() {
        let mut host = TestHost::new();
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "retained draft"))
                .error
                .is_none()
        );
        let before = serde_json::to_value(host.snapshot()).unwrap();
        host.visibility("pet", false).unwrap();
        assert!(!host.port.is_visible("pet").unwrap());
        host.visibility("pet", true).unwrap();
        assert!(host.port.is_visible("pet").unwrap());
        assert_eq!(serde_json::to_value(host.snapshot()).unwrap(), before);
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::SetVisibility {
            label: "main",
            visible: false,
            context: None,
            reply,
        });
        assert!(receive.recv().unwrap().is_err());
        assert!(host.port.is_visible("main").unwrap());
    }

    #[test]
    fn whole_application_quit_projects_one_receipt_for_every_discarded_session() {
        let mut host = TestHost::new_volatile();
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "first draft"))
                .error
                .is_none()
        );
        let first = host.snapshot();
        assert!(
            host.dispatch(ChatCommand::NewSession {
                expected_session_id: first.active_session_id.unwrap(),
                expected_sessions_revision: first.sessions_revision,
            })
            .error
            .is_none()
        );
        assert!(
            host.dispatch(guarded_draft(&host.snapshot(), "second draft"))
                .error
                .is_none()
        );

        host.edit_management(3, ManagementDraftKind::General);
        host.request_exit(ExitIntent::QuitApplication).unwrap();
        assert!(
            host.confirm_management_exit(3, CloseIntent::QuitApplication)
                .error
                .is_none()
        );
        assert!(host.snapshot().close_confirmation);
        let confirmed = host.confirm(true);
        assert!(confirmed.error.is_none());
        assert!(host.worker.should_shutdown());

        let after = host.snapshot();
        let returned = confirmed
            .snapshot
            .expect("Quit returns its final projection");
        assert_eq!(returned.presentation_revision, after.presentation_revision);
        assert_eq!(returned.management_discarded_version, Some(3));
        assert_eq!(
            returned.chat_draft.discard.as_ref().unwrap().session_ids,
            after.chat_draft.discard.as_ref().unwrap().session_ids,
        );
        assert_eq!(after.chat.draft, "");
        let receipt = after
            .chat_draft
            .discard
            .expect("quit discard receipt reaches the host DTO");
        assert_eq!(receipt.revision, after.chat_draft.revision);
        assert_eq!(
            receipt.session_ids,
            vec![String::from("session-1"), String::from("session-2")]
        );
        assert!(
            !host
                .worker
                .app
                .as_ref()
                .unwrap()
                .has_unsubmitted_chat_drafts()
        );
    }

    #[test]
    fn host_retention_maintenance_runs_on_the_bounded_cadence() {
        let mut host = TestHost::new();
        let initial = host.snapshot().presentation_revision;
        host.worker.last_retention_maintenance = Instant::now() - RETENTION_MAINTENANCE_INTERVAL;
        host.worker.maintain_retention_if_due();
        let maintained = host.snapshot().presentation_revision;
        assert!(maintained > initial);

        host.worker.maintain_retention_if_due();
        assert_eq!(host.snapshot().presentation_revision, maintained);
    }

    #[test]
    fn management_exit_confirmation_blocks_session_ipc() {
        let mut host = TestHost::new_volatile();
        host.edit_management(9, ManagementDraftKind::General);
        host.request_exit(ExitIntent::QuitApplication).unwrap();
        let prompt = host.snapshot();
        assert_eq!(prompt.management_exit.unwrap().version, 9);

        let result = host.dispatch(ChatCommand::NewSession {
            expected_session_id: prompt.active_session_id.unwrap(),
            expected_sessions_revision: prompt.sessions_revision,
        });
        assert_eq!(result.error.unwrap().code, "close_confirmation_pending");
        assert_eq!(host.snapshot().sessions.len(), 1);
    }

    #[test]
    fn host_draft_close_cancel_keeps_draft_and_send() {
        let mut host = TestHost::new();
        host.dispatch(guarded_draft(&host.snapshot(), "keep me"));
        let close = host.close();
        assert!(close.error.is_none());
        assert!(host.snapshot().close_confirmation);
        assert_eq!(host.snapshot().chat.draft, "keep me");
        assert!(host.port.main_visible());

        let cancel = host.confirm(false);
        assert!(cancel.error.is_none());
        assert!(!host.snapshot().close_confirmation);
        assert_eq!(host.snapshot().chat.draft, "keep me");
        assert_eq!(host.chat_visibility(), PanelVisibility::Visible);

        let send = host.dispatch(guarded_send(&host.snapshot()));
        assert!(send.error.is_none());
        assert!(assistant_text(&host.snapshot()).is_some());
    }

    #[test]
    fn hidden_exit_cancel_restores_editor_and_sending() {
        let mut host = TestHost::new();
        host.dispatch(guarded_draft(&host.snapshot(), "hidden draft"));
        host.visibility("main", false).unwrap();
        assert!(!host.port.main_visible());
        assert_eq!(host.chat_visibility(), PanelVisibility::Hidden);

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        // 隐藏时退出必须先恢复可操作确认入口与展示草稿，再产生关闭确认。
        assert!(host.port.main_visible());
        assert_eq!(host.chat_visibility(), PanelVisibility::Visible);
        assert_eq!(host.snapshot().chat.draft, "hidden draft");
        assert!(host.snapshot().close_confirmation);
        assert!(!host.worker.should_shutdown());

        let cancel = host.confirm(false);
        assert!(cancel.error.is_none());
        assert!(!host.worker.should_shutdown());
        assert!(!host.snapshot().close_confirmation);
        assert_eq!(host.snapshot().chat.draft, "hidden draft");
        assert!(host.port.main_visible());
        assert_eq!(host.chat_visibility(), PanelVisibility::Visible);

        let send = host.dispatch(guarded_send(&host.snapshot()));
        assert!(send.error.is_none());
        assert!(assistant_text(&host.snapshot()).is_some());
    }

    #[test]
    fn hidden_exit_confirm_quits_after_hiding() {
        let mut host = TestHost::new();
        host.dispatch(guarded_draft(&host.snapshot(), "discard on exit"));
        host.visibility("main", false).unwrap();
        host.request_exit(ExitIntent::QuitApplication).unwrap();
        assert!(host.snapshot().close_confirmation);
        assert!(host.port.main_visible());

        let confirm = host.confirm(true);
        assert!(confirm.error.is_none());
        assert!(host.worker.should_shutdown());
        assert!(!host.port.main_visible());
        assert_eq!(host.chat_visibility(), PanelVisibility::Closed);

        let code = host.worker.shutdown().expect("quit code");
        assert!(host.worker.animator.is_none(), "动画播放器必须先释放");
        assert!(host.worker.app.is_none(), "业务应用必须先释放");
        host.worker.request_event_loop_exit(code);
        assert_eq!(host.port.exit_code(), Some(0));
        assert_eq!(host.port.last_effect().as_deref(), Some("exit:0"));
        assert!(host.port.after_stop().is_empty());
    }

    #[test]
    fn closed_chat_exit_still_quits() {
        let mut host = TestHost::new();
        let close = host.close();
        assert!(close.error.is_none());
        assert_eq!(host.chat_visibility(), PanelVisibility::Closed);

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        assert!(host.worker.should_shutdown());
        let code = host.worker.shutdown().expect("quit code");
        host.worker.request_event_loop_exit(code);
        assert_eq!(host.port.exit_code(), Some(0));
    }

    #[test]
    fn revoked_chat_exit_still_quits() {
        let mut host = TestHost::new();
        let revoked = host.dispatch(ChatCommand::Revoke);
        assert!(revoked.error.is_none());
        assert!(host.snapshot().revoked);
        assert_eq!(host.chat_visibility(), PanelVisibility::Closed);

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        assert!(host.worker.should_shutdown());
        assert!(host.port.errors().is_empty());
        let code = host.worker.shutdown().expect("quit code");
        host.worker.request_event_loop_exit(code);
        assert_eq!(host.port.exit_code(), Some(0));
    }

    #[test]
    fn exit_survives_full_command_queue_with_confirmation() {
        let mut host = TestHost::new();
        let (sender, receiver) = mpsc::sync_channel::<Request>(QUEUE_CAPACITY);
        let mut replies = Vec::new();
        let context = host.worker.active_context().unwrap();
        for index in 0..QUEUE_CAPACITY {
            let (reply, receive) = mpsc::sync_channel(1);
            replies.push(receive);
            sender
                .try_send(Request::Dispatch(
                    guarded_draft(&host.snapshot(), format!("queued-draft-{index}")),
                    context.clone(),
                    reply,
                ))
                .expect("fill queue");
        }
        let (probe, _probe_receive) = mpsc::sync_channel(1);
        assert!(
            sender.try_send(Request::Snapshot(probe)).is_err(),
            "普通队列应已满"
        );

        // 退出走独立邮箱：普通队列满也不能丢失。
        host.exit.submit(ExitIntent::QuitApplication);
        assert!(host.exit.has_pending());

        // 复现宿主循环：先处理已受理命令，再从独立邮箱取退出请求。
        while let Ok(queued) = receiver.try_recv() {
            host.worker.handle(queued);
        }
        let request = host
            .exit
            .take()
            .expect("exit mailbox must keep the request");
        host.worker.handle_exit(request).unwrap();

        let snapshot = host.snapshot();
        assert_eq!(snapshot.chat.draft, "queued-draft-31");
        assert!(
            snapshot.close_confirmation,
            "队列中的最新草稿必须参与退出确认"
        );
        assert!(host.port.main_visible());
        assert!(!host.worker.should_shutdown());

        let confirm = host.confirm(true);
        assert!(confirm.error.is_none());
        assert!(host.worker.should_shutdown());
    }

    #[test]
    fn chat_confirmation_observes_queued_edit_without_exit_mailbox_request() {
        let mut host = TestHost::new();
        host.dispatch(guarded_draft(&host.snapshot(), "original synthetic draft"));
        assert!(host.close().error.is_none());
        assert!(host.snapshot().close_confirmation);
        assert!(!host.exit.has_pending());

        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (edited, edit_result) = mpsc::sync_channel(1);
        let context = host.worker.active_context().unwrap();
        sender
            .send(Request::Dispatch(
                guarded_draft(&host.snapshot(), "new accepted synthetic draft"),
                context.clone(),
                edited,
            ))
            .unwrap();
        let (reply, result) = mpsc::sync_channel(1);
        handle_close_with_exit_mailbox(
            &mut host.worker,
            CloseRequest::ConfirmClose {
                discard: true,
                expected_intent: Some(CloseIntent::CloseChat),
                context,
                reply,
            },
            &host.exit,
            &receiver,
        );
        assert!(edit_result.recv().unwrap().error.is_none());
        assert!(result.recv().unwrap().error.is_some());
        assert_eq!(host.snapshot().chat.draft, "new accepted synthetic draft");
        assert_eq!(host.chat_visibility(), PanelVisibility::Visible);
        assert!(host.port.main_visible());
        assert!(!host.worker.should_shutdown());
    }

    #[test]
    fn repeated_and_stale_confirmation_never_exits() {
        let mut host = TestHost::new();
        host.dispatch(guarded_draft(&host.snapshot(), "first"));
        host.request_exit(ExitIntent::CloseChat).unwrap();
        assert!(host.snapshot().close_confirmation);
        assert!(host.confirm(true).error.is_none(), "第一次确认应关闭聊天");
        assert_eq!(host.chat_visibility(), PanelVisibility::Closed);
        assert!(!host.worker.should_shutdown());

        let repeated = host.confirm(true);
        assert_eq!(
            repeated.error.as_ref().map(|failure| failure.code.as_str()),
            Some("no_pending_confirmation")
        );
        assert!(!host.worker.should_shutdown());

        let mut stale_host = TestHost::new();
        stale_host.dispatch(guarded_draft(&stale_host.snapshot(), "stale draft"));
        stale_host
            .request_exit(ExitIntent::QuitApplication)
            .unwrap();
        assert!(stale_host.snapshot().close_confirmation);
        // 确认签发后草稿又变化，核心关闭确认失效；陈旧确认不能当作退出同意。
        stale_host.dispatch(guarded_draft(&stale_host.snapshot(), "stale draft changed"));
        let stale = stale_host.confirm(true);
        assert_eq!(
            stale.error.as_ref().map(|failure| failure.code.as_str()),
            Some("confirm_stale")
        );
        assert!(!stale_host.worker.should_shutdown());
        assert_eq!(stale_host.snapshot().chat.draft, "stale draft changed");
        // 陈旧确认已使退出意图失效，稍后的重复确认不再退出。
        let repeated_stale = stale_host.confirm(true);
        assert!(repeated_stale.error.is_some());
        assert!(!stale_host.worker.should_shutdown());

        // 取消/陈旧后重新发起退出仍可工作。
        stale_host
            .request_exit(ExitIntent::QuitApplication)
            .unwrap();
        assert!(stale_host.snapshot().close_confirmation);
        assert!(stale_host.confirm(true).error.is_none());
        assert!(stale_host.worker.should_shutdown());
    }

    #[test]
    fn raising_close_intent_updates_presentation_revision_without_business_revision() {
        let mut host = TestHost::new();
        host.dispatch(guarded_draft(&host.snapshot(), "keep me"));
        host.request_exit(ExitIntent::CloseChat).unwrap();
        let close = host.snapshot();
        assert_eq!(close.close_intent, Some(CloseIntent::CloseChat));
        host.request_exit(ExitIntent::QuitApplication).unwrap();
        let quit = host.snapshot();
        assert_eq!(quit.revision, close.revision);
        assert!(quit.presentation_revision > close.presentation_revision);
        assert_eq!(quit.close_intent, Some(CloseIntent::QuitApplication));
        let cancel = host.confirm(false);
        assert!(cancel.error.is_none());
        assert_eq!(host.snapshot().close_intent, None);
        assert!(!host.worker.should_shutdown());
    }

    #[test]
    fn chat_close_confirmation_rejects_intent_drift_and_preserves_the_draft() {
        let mut host = TestHost::new();
        host.dispatch(guarded_draft(&host.snapshot(), "preserve chat draft"));
        assert!(host.close().error.is_none());
        assert!(host.snapshot().close_confirmation);
        assert_eq!(host.snapshot().close_intent, Some(CloseIntent::CloseChat));

        host.request_exit(ExitIntent::QuitApplication).unwrap();
        let stale = host.confirm_with_intent(true, None);
        assert_eq!(
            stale.error.as_ref().map(|error| error.code.as_str()),
            Some("close_confirmation_stale")
        );
        let prompt = host.snapshot();
        assert!(prompt.close_confirmation);
        assert_eq!(prompt.close_intent, Some(CloseIntent::QuitApplication));
        assert_eq!(prompt.chat.draft, "preserve chat draft");
        assert!(!host.worker.should_shutdown());

        let confirmed = host.confirm_with_intent(true, Some(CloseIntent::QuitApplication));
        assert!(confirmed.error.is_none());
        assert!(host.worker.should_shutdown());
        assert_eq!(host.snapshot().chat.draft, "");
    }

    #[test]
    fn snapshot_barrier_reads_prior_host_dispatch() {
        let mut host = TestHost::new();
        let (sender, receiver) = mpsc::sync_channel::<Request>(QUEUE_CAPACITY);
        let (draft_reply, draft_receive) = mpsc::sync_channel(1);
        let context = host.worker.active_context().unwrap();
        sender
            .try_send(Request::Dispatch(
                guarded_draft(&host.snapshot(), "barrier"),
                context,
                draft_reply,
            ))
            .unwrap();
        let (snapshot_reply, snapshot_receive) = mpsc::sync_channel(1);
        sender.try_send(Request::Snapshot(snapshot_reply)).unwrap();
        while let Ok(queued) = receiver.try_recv() {
            host.worker.handle(queued);
        }
        let draft_result = draft_receive.recv().unwrap();
        let snapshot_result = snapshot_receive.recv().unwrap();
        assert_eq!(
            draft_result
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.chat.draft.as_str()),
            Some("barrier")
        );
        let barrier = snapshot_result.snapshot.expect("snapshot reply");
        assert_eq!(barrier.chat.draft, "barrier");
        assert_eq!(barrier.revision, host.snapshot().revision);
        assert!(
            host.port
                .state
                .lock()
                .unwrap()
                .snapshots
                .iter()
                .any(|emitted| emitted.revision == barrier.revision)
        );
    }

    #[test]
    fn later_close_request_does_not_downgrade_pending_quit() {
        let mut host = TestHost::new();
        host.dispatch(guarded_draft(&host.snapshot(), "quit guard"));
        host.request_exit(ExitIntent::QuitApplication).unwrap();
        assert!(host.snapshot().close_confirmation);
        assert!(!host.worker.should_shutdown());

        // 后续原生关窗请求只能复用/提升意图，不能把已受理的退出降级为只关聊天。
        host.request_exit(ExitIntent::CloseChat).unwrap();
        assert!(host.snapshot().close_confirmation);
        assert!(!host.worker.should_shutdown());
        let confirm = host.confirm(true);
        assert!(confirm.error.is_none(), "确认不应因重复关闭而陈旧");
        assert!(host.worker.should_shutdown());
    }

    #[test]
    fn animation_advance_and_pause_failures_are_reported() {
        let mut host = TestHost::new();
        // 让播放器票据失效，迫使 Animator::advance 返回错误。
        host.worker.player.0.lock().unwrap().ticket = None;
        host.worker.tick(true, 16);
        let last_frame = host
            .port
            .state
            .lock()
            .unwrap()
            .frames
            .last()
            .cloned()
            .unwrap();
        assert!(last_frame.faulted);
        assert!(last_frame.paused);
        let advance_errors = host
            .port
            .errors()
            .iter()
            .filter(|error| error.as_str() == "animation_advance_failed")
            .count();
        assert_eq!(advance_errors, 1, "动画推进错误必须显式反馈一次");
        // 故障后停止继续推进，而不是每个 tick 重复吞错/刷错误。
        host.worker.tick(true, 16);
        assert_eq!(
            host.port
                .errors()
                .iter()
                .filter(|error| error.as_str() == "animation_advance_failed")
                .count(),
            1
        );
        assert!(host.worker.toggle_pause().is_err());
        assert!(
            host.port
                .errors()
                .iter()
                .any(|error| error == "animation_pause_failed")
        );
    }

    #[test]
    fn main_visibility_failures_are_reported_and_retryable() {
        let mut host = TestHost::new();
        host.dispatch(guarded_draft(&host.snapshot(), "retry draft"));
        host.port.state.lock().unwrap().fail_hide = true;
        assert!(host.visibility("main", false).is_err());
        assert!(host.port.main_visible(), "失败时不得假装已隐藏");
        assert!(
            host.port
                .errors()
                .iter()
                .any(|error| error == "window_hide_failed")
        );

        host.port.state.lock().unwrap().fail_hide = false;
        // 业务 Hide 已成功，重试只补物理隐藏即可。
        host.visibility("main", false).unwrap();
        assert!(!host.port.main_visible());

        host.port.state.lock().unwrap().fail_show = true;
        assert!(host.visibility("main", true).is_err());
        assert!(!host.port.main_visible());
        assert!(
            host.port
                .errors()
                .iter()
                .any(|error| error == "window_show_failed")
        );
        host.port.state.lock().unwrap().fail_show = false;
        host.visibility("main", true).unwrap();
        assert!(host.port.main_visible());
        assert_eq!(host.chat_visibility(), PanelVisibility::Visible);
        // 重新打开恢复展示缓存，草稿不因隐藏/失败重试而丢失。
        assert_eq!(host.snapshot().chat.draft, "retry draft");
    }

    #[test]
    fn worker_stops_after_release_and_requests_exit_last() {
        let state = Arc::new(Mutex::new(RecordingState {
            main_visible: true,
            pet_visible: true,
            ..RecordingState::default()
        }));
        let port = RecordingPort {
            state: state.clone(),
        };
        let latest = Arc::new(Mutex::new(Snapshot {
            revision: 0,
            active_buddy_id: Some("offline-companion".into()),
            buddies_revision: 1,
            buddies: vec![BuddyView {
                id: "offline-companion".into(),
                label: "离线验收伙伴".into(),
                profile_revision: 1,
                persona_revision: 1,
                model_revision: 1,
            }],
            active_buddy_persona: None,
            buddy_commit: None,
            active_session_id: Some("session-1".into()),
            sessions_revision: 1,
            session_isolation_allowed: false,
            sessions: vec![SessionView {
                id: "session-1".into(),
                label: "会话 1".into(),
                archived: false,
                status: "ready".into(),
            }],
            presentation_revision: 0,
            mode: "offline".to_owned(),
            revoked: false,
            chat: ChatView {
                draft: String::new(),
                messages: Vec::new(),
                status: "ready".to_owned(),
                failure: None,
            },
            close_confirmation: false,
            close_intent: None,
            management_draft: ManagementDraftView {
                version: 0,
                dirty: false,
                kind: ManagementDraftKind::None,
            },
            management_exit: None,
            management_discarded_version: None,
            chat_draft: ChatDraftView {
                revision: 1,
                discard: None,
                sessions: Vec::new(),
            },
            active_preset: "offline-echo".into(),
            external_transmission_allowed: false,
            presets: Vec::new(),
            memories: Vec::new(),
            diagnostics: Vec::new(),
            notice: None,
            model_settings: None,
            persistence_enabled: false,
        }));
        let frame = Arc::new(Mutex::new(FrameSnapshot::builtin(0, false, false)));
        let (sender, receiver) = mpsc::sync_channel::<Request>(QUEUE_CAPACITY);
        let (_close_sender, close_receiver) =
            mpsc::sync_channel::<CloseRequest>(CLOSE_QUEUE_CAPACITY);
        let exit = Arc::new(ExitMailbox::new());
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let (done_sender, done_receiver) = mpsc::channel();
        let run_exit = exit.clone();
        let worker_stop = stop.clone();
        let worker_finished = finished.clone();
        let worker_alive = alive.clone();
        let root = isolated_root();
        let worker_root = root.clone();
        let worker_latest = latest.clone();
        let worker_frame = frame.clone();
        let handle = thread::spawn(move || {
            run_worker_loop(
                move || {
                    let epoch = 1_800_000_000_000;
                    let mut storage = BuddyStorage::open(&worker_root, 1, epoch).unwrap();
                    storage
                        .active_application_mut()
                        .unwrap()
                        .dispatch(Action::Open(PanelKind::Chat), epoch)
                        .unwrap();
                    let management_drafts = Arc::new(ManagementDraftMailbox::default());
                    HostWorker::with_buddy_storage(
                        port,
                        storage,
                        HostWorkerContext {
                            model_settings: None,
                            persistence_enabled: true,
                            management_drafts,
                            epoch_start_ms: epoch,
                            latest: worker_latest,
                            frame: worker_frame,
                        },
                    )
                    .unwrap()
                },
                receiver,
                close_receiver,
                run_exit,
                worker_stop,
                worker_finished,
                worker_alive,
            );
            let _ = done_sender.send(());
        });
        // 保持发送端存活，确保循环不是因为通道断开而退出。
        let _sender = sender;
        exit.submit(ExitIntent::QuitApplication);
        assert!(
            done_receiver.recv_timeout(Duration::from_secs(5)).is_ok(),
            "worker 必须在收到退出请求后结束"
        );
        handle.join().unwrap();
        assert!(finished.load(Ordering::Acquire));
        assert_eq!(state.lock().unwrap().exit_code, Some(0));
        assert_eq!(
            state.lock().unwrap().effects.last().map(String::as_str),
            Some("exit:0"),
            "退出请求必须在窗口收起与释放之后最后发出"
        );
        assert!(state.lock().unwrap().after_stop.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_queue_merges_quit_before_stale_management_confirmation_and_cancel() {
        let state = Arc::new(Mutex::new(RecordingState {
            main_visible: true,
            pet_visible: true,
            ..RecordingState::default()
        }));
        let port = RecordingPort {
            state: state.clone(),
        };
        let latest = Arc::new(Mutex::new(project(&chat_app())));
        let frame = Arc::new(Mutex::new(FrameSnapshot::builtin(0, false, false)));
        let (sender, receiver) = mpsc::sync_channel::<Request>(QUEUE_CAPACITY);
        let (close_sender, close_receiver) =
            mpsc::sync_channel::<CloseRequest>(CLOSE_QUEUE_CAPACITY);
        let exit = Arc::new(ExitMailbox::new());
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let management_drafts = Arc::new(ManagementDraftMailbox::default());
        let context = BuddyContext {
            expected_buddy_id: "offline-companion".into(),
            expected_buddies_revision: 1,
        };
        management_drafts
            .update(1, true, ManagementDraftKind::General, context.clone())
            .unwrap();

        let (draft_reply, draft_result) = mpsc::sync_channel(1);
        let current = latest.lock().unwrap().clone();
        sender
            .try_send(Request::Dispatch(
                guarded_draft(&current, "accepted before confirmation"),
                context.clone(),
                draft_reply,
            ))
            .unwrap();
        for _ in 1..QUEUE_CAPACITY {
            let (reply, _receive) = mpsc::sync_channel(1);
            sender.try_send(Request::TogglePause(reply)).unwrap();
        }
        let (reply, _receive) = mpsc::sync_channel(1);
        assert!(sender.try_send(Request::TogglePause(reply)).is_err());

        let (close_reply, close_result) = mpsc::sync_channel(1);
        close_sender
            .try_send(CloseRequest::CloseChat {
                context: context.clone(),
                reply: close_reply,
            })
            .unwrap();
        let (confirm_reply, confirm_result) = mpsc::sync_channel(1);
        close_sender
            .try_send(CloseRequest::ConfirmManagementDiscard {
                version: 1,
                intent: CloseIntent::CloseChat,
                context: context.clone(),
                reply: confirm_reply,
            })
            .unwrap();
        let (stale_cancel_reply, stale_cancel_result) = mpsc::sync_channel(1);
        close_sender
            .try_send(CloseRequest::CancelManagementExit {
                version: 1,
                intent: CloseIntent::CloseChat,
                context: context.clone(),
                reply: stale_cancel_reply,
            })
            .unwrap();
        let (cancel_reply, cancel_result) = mpsc::sync_channel(1);
        close_sender
            .try_send(CloseRequest::CancelManagementExit {
                version: 1,
                intent: CloseIntent::QuitApplication,
                context: context.clone(),
                reply: cancel_reply,
            })
            .unwrap();
        exit.submit(ExitIntent::QuitApplication);

        let root = isolated_root();
        let worker_root = root.clone();
        let worker_latest = latest.clone();
        let worker_frame = frame.clone();
        let worker_drafts = management_drafts.clone();
        let worker_exit = exit.clone();
        let worker_stop = stop.clone();
        let worker_finished = finished.clone();
        let worker_alive = alive.clone();
        let handle = thread::spawn(move || {
            run_worker_loop(
                move || {
                    let epoch = 1_800_000_000_000;
                    let mut storage = BuddyStorage::open(&worker_root, 1, epoch).unwrap();
                    storage
                        .active_application_mut()
                        .unwrap()
                        .dispatch(Action::Open(PanelKind::Chat), epoch)
                        .unwrap();
                    HostWorker::with_buddy_storage(
                        port,
                        storage,
                        HostWorkerContext {
                            model_settings: None,
                            persistence_enabled: true,
                            management_drafts: worker_drafts,
                            epoch_start_ms: epoch,
                            latest: worker_latest,
                            frame: worker_frame,
                        },
                    )
                    .unwrap()
                },
                receiver,
                close_receiver,
                worker_exit,
                worker_stop,
                worker_finished,
                worker_alive,
            );
        });

        let close = close_result
            .recv_timeout(Duration::from_secs(5))
            .expect("queued close opens management confirmation");
        assert_eq!(
            close.snapshot.unwrap().management_exit.unwrap().intent,
            CloseIntent::CloseChat
        );
        let stale = confirm_result
            .recv_timeout(Duration::from_secs(5))
            .expect("worker rejects old queued confirmation");
        assert_eq!(
            stale.error.as_ref().map(|error| error.code.as_str()),
            Some("management_confirmation_stale")
        );
        let prompt = stale.snapshot.unwrap();
        assert!(prompt.management_draft.dirty);
        assert_eq!(prompt.management_draft.version, 1);
        assert_eq!(prompt.chat.draft, "accepted before confirmation");
        assert_eq!(
            draft_result
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .snapshot
                .unwrap()
                .chat
                .draft,
            "accepted before confirmation"
        );
        assert_eq!(
            prompt.management_exit.unwrap().intent,
            CloseIntent::QuitApplication
        );

        let stale_cancel = stale_cancel_result
            .recv_timeout(Duration::from_secs(5))
            .expect("worker rejects old queued cancellation");
        assert_eq!(
            stale_cancel.error.as_ref().map(|error| error.code.as_str()),
            Some("management_confirmation_stale")
        );
        assert_eq!(
            stale_cancel
                .snapshot
                .unwrap()
                .management_exit
                .unwrap()
                .intent,
            CloseIntent::QuitApplication
        );

        let cancelled = cancel_result
            .recv_timeout(Duration::from_secs(5))
            .expect("matching cancellation is honored");
        assert!(cancelled.error.is_none());
        assert!(cancelled.snapshot.unwrap().management_exit.is_none());
        assert!(management_drafts.current().dirty);

        stop.store(true, Ordering::Release);
        handle.join().unwrap();
        assert!(finished.load(Ordering::Acquire));
        assert!(!exit.has_pending());
        drop(sender);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_queue_resolves_unknown_management_draft_and_quits_normally() {
        let state = Arc::new(Mutex::new(RecordingState {
            main_visible: true,
            pet_visible: true,
            ..RecordingState::default()
        }));
        let port = RecordingPort {
            state: state.clone(),
        };
        let latest = Arc::new(Mutex::new(project(&chat_app())));
        let frame = Arc::new(Mutex::new(FrameSnapshot::builtin(0, false, false)));
        let (sender, receiver) = mpsc::sync_channel::<Request>(QUEUE_CAPACITY);
        let (close_sender, close_receiver) =
            mpsc::sync_channel::<CloseRequest>(CLOSE_QUEUE_CAPACITY);
        let exit = Arc::new(ExitMailbox::new());
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let root = isolated_root();
        let epoch = 1_800_000_000_000;
        let management_drafts = Arc::new(ManagementDraftMailbox::default());
        let worker_latest = latest.clone();
        let worker_frame = frame.clone();
        let worker_exit = exit.clone();
        let worker_stop = stop.clone();
        let worker_finished = finished.clone();
        let worker_alive = alive.clone();
        let worker_drafts = management_drafts.clone();
        let worker_root = root.clone();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let handle = thread::spawn(move || {
            run_worker_loop(
                move || {
                    let mut storage = BuddyStorage::open(&worker_root, 1, epoch).unwrap();
                    let first_id = storage.active_buddy_id().unwrap().to_owned();
                    let first_revision = storage.registry_snapshot().unwrap().revision;
                    let created = storage
                        .create_buddy(first_revision, "second", "synthetic second", epoch + 1)
                        .unwrap();
                    let second_id = created.registry.active_buddy_id.unwrap();
                    for (id, text) in [
                        (&first_id, "synthetic first buddy draft"),
                        (&second_id, "synthetic second buddy draft"),
                    ] {
                        let app = storage.application_mut(id).unwrap();
                        app.dispatch(Action::Open(PanelKind::Chat), epoch + 2)
                            .unwrap();
                        app.dispatch(Action::SetDraft(text.into()), epoch + 3)
                            .unwrap();
                    }
                    let context = BuddyContext {
                        expected_buddy_id: storage.active_buddy_id().unwrap().to_owned(),
                        expected_buddies_revision: storage.registry_snapshot().unwrap().revision,
                    };
                    let draft_set = storage.draft_revision_set().unwrap();
                    assert_eq!(draft_set.len(), 2);
                    assert!(draft_set.iter().all(|draft| draft.has_unsubmitted_drafts));
                    worker_drafts
                        .update(1, true, ManagementDraftKind::BuddyCreate, context.clone())
                        .unwrap();
                    let mut worker = HostWorker::with_buddy_storage(
                        port,
                        storage,
                        HostWorkerContext {
                            model_settings: None,
                            persistence_enabled: true,
                            management_drafts: worker_drafts,
                            epoch_start_ms: epoch,
                            latest: worker_latest,
                            frame: worker_frame,
                        },
                    )
                    .unwrap();
                    let registry = worker_root.join("buddies.v1");
                    std::fs::rename(&registry, worker_root.join("synthetic-registry-backup"))
                        .unwrap();
                    std::fs::create_dir(&registry).unwrap();
                    let error = worker
                        .buddy_storage
                        .as_mut()
                        .unwrap()
                        .create_buddy(
                            context.expected_buddies_revision,
                            "unknown",
                            "synthetic",
                            epoch + 2,
                        )
                        .unwrap_err();
                    assert_eq!(
                        error.publication_state(),
                        Some(moe_desktop_adapters::storage::BuddyPublicationState::Unknown)
                    );
                    ready_sender.send((context, draft_set)).unwrap();
                    worker
                },
                receiver,
                close_receiver,
                worker_exit,
                worker_stop,
                worker_finished,
                worker_alive,
            );
        });
        let (context, all_buddy_drafts) =
            ready_receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(all_buddy_drafts.len(), 2);
        assert!(
            all_buddy_drafts
                .iter()
                .all(|draft| draft.has_unsubmitted_drafts)
        );

        let update = |version, dirty, kind| {
            let (reply, receive) = mpsc::sync_channel(1);
            sender
                .try_send(Request::ManagementDraftState {
                    version,
                    dirty,
                    kind,
                    context: context.clone(),
                    reply,
                })
                .unwrap();
            receive.recv_timeout(Duration::from_secs(5)).unwrap()
        };
        let snapshot = || {
            let (reply, receive) = mpsc::sync_channel(1);
            sender.try_send(Request::Snapshot(reply)).unwrap();
            receive
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .snapshot
                .unwrap()
        };
        let queue_exit =
            || exit.submit_with_context(ExitIntent::QuitApplication, Some(context.clone()));
        let confirm_management = |version| {
            let (reply, receive) = mpsc::sync_channel(1);
            close_sender
                .try_send(CloseRequest::ConfirmManagementDiscard {
                    version,
                    intent: CloseIntent::QuitApplication,
                    context: context.clone(),
                    reply,
                })
                .unwrap();
            receive.recv_timeout(Duration::from_secs(5)).unwrap()
        };

        queue_exit();
        let mut prompt = snapshot();
        for _ in 0..50 {
            if prompt.management_exit.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
            prompt = snapshot();
        }
        assert_eq!(
            prompt.management_exit.as_ref().map(|exit| exit.version),
            Some(1),
            "initial management exit should bind the dirty version"
        );
        assert!(update(2, false, ManagementDraftKind::None).is_ok());
        let abandoned = confirm_management(2);
        assert!(abandoned.error.is_none());

        let edited = update(3, true, ManagementDraftKind::General);
        assert!(edited.is_ok());
        prompt = snapshot();
        assert_eq!(prompt.management_draft.version, 3);
        assert_eq!(
            prompt.management_exit.as_ref().map(|exit| exit.version),
            Some(3),
            "edited management exit should advance to version 3"
        );
        let (reply, receive) = mpsc::sync_channel(1);
        close_sender
            .try_send(CloseRequest::CancelManagementExit {
                version: 3,
                intent: CloseIntent::QuitApplication,
                context: context.clone(),
                reply,
            })
            .unwrap();
        assert!(
            receive
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .error
                .is_none()
        );
        assert!(snapshot().management_exit.is_none());

        queue_exit();
        prompt = snapshot();
        for _ in 0..50 {
            if prompt.management_exit.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
            prompt = snapshot();
        }
        assert_eq!(
            prompt.management_exit.as_ref().map(|exit| exit.version),
            Some(3),
            "reopened management exit should bind version 3"
        );
        assert!(confirm_management(3).error.is_none());
        let (reply, receive) = mpsc::sync_channel(1);
        close_sender
            .try_send(CloseRequest::ConfirmClose {
                discard: true,
                expected_intent: Some(CloseIntent::QuitApplication),
                context,
                reply,
            })
            .unwrap();
        let final_result = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(final_result.error.is_none());
        for _ in 0..500 {
            if finished.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            finished.load(Ordering::Acquire) || state.lock().unwrap().exit_code == Some(0),
            "confirmed quit must finish through the worker exit path"
        );
        let _sender = sender;
        assert!(finished.load(Ordering::Acquire));
        handle.join().unwrap();
        assert_eq!(state.lock().unwrap().exit_code, Some(0));
        assert!(state.lock().unwrap().after_stop.is_empty());
        std::fs::remove_dir(root.join("buddies.v1")).unwrap();
        std::fs::rename(
            root.join("synthetic-registry-backup"),
            root.join("buddies.v1"),
        )
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn material_worker_rechecks_buddy_context_and_keeps_business_live_during_load() {
        let mut host = TestHost::new_volatile();
        let context = host.worker.active_context().unwrap();
        let initial = host.worker.material_host.state();
        let mut stale = context.clone();
        stale.expected_buddies_revision += 1;
        let rejected = host.material_command(
            MaterialCommand::Load {
                path: std::env::temp_dir().display().to_string(),
                expected_resource_generation: initial.resource_generation,
                expected_playback_generation: initial.playback_generation,
            },
            stale,
        );
        assert!(!rejected.ok);
        assert_eq!(rejected.error.unwrap().code, "buddy_context_stale");
        assert_eq!(host.worker.material_host.state().revision, initial.revision);

        let root = isolated_root();
        std::fs::create_dir(&root).unwrap();
        let accepted = host.material_command(
            MaterialCommand::Load {
                path: root.display().to_string(),
                expected_resource_generation: initial.resource_generation,
                expected_playback_generation: initial.playback_generation,
            },
            context.clone(),
        );
        assert!(accepted.ok);
        assert!(accepted.state.loading);
        let busy = host.material_command(
            MaterialCommand::Play {
                action_id: "idle".into(),
                expected_resource_generation: accepted.state.resource_generation,
                expected_playback_generation: accepted.state.playback_generation,
            },
            context.clone(),
        );
        assert!(!busy.ok);
        assert_eq!(busy.error.unwrap().code, "material_busy");

        let latest = host.latest.lock().unwrap().clone();
        let business = host.dispatch(ChatCommand::SetDraft {
            text: "still responsive".into(),
            expected_session_id: latest.active_session_id.unwrap(),
            expected_sessions_revision: latest.sessions_revision,
        });
        assert!(business.error.is_none());
        for _ in 0..100 {
            host.worker.poll_material_load();
            if !host.worker.material_host.state().loading {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        let failed = host.worker.material_host.state();
        assert!(!failed.loading);
        assert_eq!(failed.source, "builtin");
        assert_eq!(failed.status, "playing");
        assert!(failed.error.is_some());

        let cancelled = host.material_command(
            MaterialCommand::Load {
                path: root.display().to_string(),
                expected_resource_generation: failed.resource_generation,
                expected_playback_generation: failed.playback_generation,
            },
            context.clone(),
        );
        assert!(cancelled.ok && cancelled.state.loading);
        let stopped = host.material_command(
            MaterialCommand::Stop {
                expected_resource_generation: cancelled.state.resource_generation,
                expected_playback_generation: cancelled.state.playback_generation,
            },
            context.clone(),
        );
        assert!(stopped.ok);
        if host.worker.material_load.is_some() {
            let state = stopped.state.clone();
            let busy_load = host.material_command(
                MaterialCommand::Load {
                    path: root.display().to_string(),
                    expected_resource_generation: state.resource_generation,
                    expected_playback_generation: state.playback_generation,
                },
                context.clone(),
            );
            assert_eq!(busy_load.error.unwrap().code, "material_busy");
        }
        for _ in 0..100 {
            host.worker.poll_material_load();
            if host.worker.material_load.is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(host.worker.material_load.is_none());

        let state = host.worker.material_host.state();
        let _ = host.material_command(
            MaterialCommand::Load {
                path: root.display().to_string(),
                expected_resource_generation: state.resource_generation,
                expected_playback_generation: state.playback_generation,
            },
            context,
        );
        assert!(host.worker.material_load.is_some());
        let cancel = Arc::clone(&host.worker.material_load.as_ref().unwrap().cancel);
        let quit_context = host.worker.active_context().unwrap();
        host.worker
            .handle_exit(ExitRequest {
                id: 1,
                intent: ExitIntent::QuitApplication,
                buddy_context: Some(quit_context),
            })
            .unwrap();
        assert!(cancel.load(Ordering::Acquire));
        assert!(!host.worker.material_host.state().loading);
        host.worker.shutdown();
        assert!(host.worker.material_load.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn synthetic_png_rgba(pixel: [u8; 4]) -> Vec<u8> {
        let raw = [0, pixel[0], pixel[1], pixel[2], pixel[3]];
        let mut zlib = vec![0x78, 0x01, 0x01, 5, 0, 250, 255];
        zlib.extend_from_slice(&raw);
        let mut a = 1u32;
        let mut b = 0u32;
        for byte in raw {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        append_test_png_chunk(&mut png, b"IHDR", &ihdr);
        append_test_png_chunk(&mut png, b"IDAT", &zlib);
        append_test_png_chunk(&mut png, b"IEND", &[]);
        png
    }

    fn append_test_png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        output.extend_from_slice(&(data.len() as u32).to_be_bytes());
        output.extend_from_slice(kind);
        output.extend_from_slice(data);
        let start = output.len() - data.len() - 4;
        let mut crc = !0u32;
        for byte in &output[start..] {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        output.extend_from_slice(&(!crc).to_be_bytes());
    }

    #[test]
    fn material_worker_loads_package_and_validates_pixels_and_hit_masks_by_frame() {
        let mut host = TestHost::new_volatile();
        let context = host.worker.active_context().unwrap();
        let builtin = host.worker.material_host.state();
        assert_eq!(
            builtin.actions.first().map(|action| action.id.as_str()),
            Some("idle")
        );
        let replayed_builtin = host.material_command(
            MaterialCommand::Play {
                action_id: "idle".into(),
                expected_resource_generation: builtin.resource_generation,
                expected_playback_generation: builtin.playback_generation,
            },
            context.clone(),
        );
        assert!(replayed_builtin.ok);
        assert_eq!(replayed_builtin.state.action_id.as_deref(), Some("idle"));
        assert!(replayed_builtin.state.playback_generation > builtin.playback_generation);
        let root = isolated_root();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("frame.png"),
            synthetic_png_rgba([10, 20, 30, 255]),
        )
        .unwrap();
        std::fs::write(root.join("manifest.json"), r#"{"formatVersion":1,"id":"test-pack","version":1,"label":"Test pack","clips":[{"id":"walk","type":"sequence","frames":[{"path":"frame.png","durationMs":10}]}],"actions":[{"id":"walk","label":"Walk","clip":"walk","mode":"loop"}],"defaultAction":"walk"}"#).unwrap();
        let initial = replayed_builtin.state;
        let accepted = host.material_command(
            MaterialCommand::Load {
                path: root.display().to_string(),
                expected_resource_generation: initial.resource_generation,
                expected_playback_generation: initial.playback_generation,
            },
            context.clone(),
        );
        assert!(accepted.ok && accepted.state.loading);
        for _ in 0..100 {
            host.worker.poll_material_load();
            if !host.worker.material_host.state().loading {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        let loaded = host.worker.material_host.state();
        assert_eq!(loaded.source, "package");
        assert_eq!(loaded.pack.as_ref().unwrap().id, "test-pack");
        let frame = host.worker.frame.lock().unwrap().clone();
        assert_eq!(frame.source, "package");
        let triple = MaterialFrameTriple {
            resource_generation: frame.resource_generation,
            playback_generation: frame.playback_generation,
            presentation_sequence: frame.presentation_sequence,
        };
        assert_eq!(host.material_pixels(triple).unwrap(), [10, 20, 30, 255]);
        let stale = MaterialFrameTriple {
            presentation_sequence: triple.presentation_sequence - 1,
            ..triple
        };
        assert!(host.material_pixels(stale).is_err());

        let invalid_root = isolated_root();
        std::fs::create_dir(&invalid_root).unwrap();
        let failed_switch = host.material_command(
            MaterialCommand::Load {
                path: invalid_root.display().to_string(),
                expected_resource_generation: loaded.resource_generation,
                expected_playback_generation: loaded.playback_generation,
            },
            context.clone(),
        );
        assert!(failed_switch.ok && failed_switch.state.loading);
        for _ in 0..100 {
            host.worker.poll_material_load();
            if !host.worker.material_host.state().loading {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        let preserved = host.worker.material_host.state();
        assert_eq!(preserved.source, "package");
        assert_eq!(preserved.pack.as_ref().unwrap().id, "test-pack");
        assert_eq!(preserved.playback_generation, loaded.playback_generation);
        assert_eq!(preserved.status, "playing");
        assert!(preserved.error.is_some());
        assert_eq!(host.material_pixels(triple).unwrap(), [10, 20, 30, 255]);
        std::fs::remove_dir_all(invalid_root).unwrap();

        let effects_before = host.port.state.lock().unwrap().effects.len();
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::HitMask {
            width: 1,
            height: 1,
            alpha: vec![255],
            frame: stale,
            reply,
        });
        assert!(receive.recv().unwrap().is_err());
        assert_eq!(
            host.port.state.lock().unwrap().effects.len(),
            effects_before
        );
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::HitMask {
            width: 1,
            height: 1,
            alpha: vec![255],
            frame: triple,
            reply,
        });
        assert!(receive.recv().unwrap().is_ok());

        let played = host.material_command(
            MaterialCommand::Play {
                action_id: "walk".into(),
                expected_resource_generation: loaded.resource_generation,
                expected_playback_generation: loaded.playback_generation,
            },
            context.clone(),
        );
        assert!(played.ok);
        assert_ne!(played.state.playback_generation, loaded.playback_generation);
        let playing_frame = host.worker.frame.lock().unwrap().clone();
        let playing_triple = MaterialFrameTriple {
            resource_generation: playing_frame.resource_generation,
            playback_generation: playing_frame.playback_generation,
            presentation_sequence: playing_frame.presentation_sequence,
        };
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::HitMask {
            width: 1,
            height: 1,
            alpha: vec![255],
            frame: playing_triple,
            reply,
        });
        assert!(receive.recv().unwrap().is_ok());
        let frame_before_failed_clear = host.worker.frame.lock().unwrap().clone();
        host.port.state.lock().unwrap().fail_hit_mask = true;
        let stopped = host.material_command(
            MaterialCommand::Stop {
                expected_resource_generation: played.state.resource_generation,
                expected_playback_generation: played.state.playback_generation,
            },
            context.clone(),
        );
        assert!(!stopped.ok);
        assert_eq!(stopped.error.as_ref().unwrap().disposition, "unknown");
        assert_eq!(stopped.state.status, "faulted");
        assert!(*host.worker.frame.lock().unwrap() == frame_before_failed_clear);
        assert!(
            host.worker.hit_mask.is_some(),
            "unconfirmed native mask remains owned"
        );

        let failed_projection = serde_json::to_value(host.worker.material_host.state()).unwrap();
        let failed_frame = host.worker.frame.lock().unwrap().clone();
        let failed_mask = host.worker.hit_mask.clone();
        assert!(host.worker.toggle_pause().is_err());
        host.worker.tick(true, 100);
        assert_eq!(
            serde_json::to_value(host.worker.material_host.state()).unwrap(),
            failed_projection
        );
        assert!(*host.worker.frame.lock().unwrap() == failed_frame);
        assert_eq!(host.worker.hit_mask, failed_mask);
        let mut rejected_commands = vec![
            MaterialCommand::Load {
                path: root.display().to_string(),
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
            MaterialCommand::Play {
                action_id: "walk".into(),
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
            MaterialCommand::Pause {
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
            MaterialCommand::Resume {
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
            MaterialCommand::Builtin {
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
        ];
        for command in rejected_commands.drain(..) {
            let reply = host.material_command(command, context.clone());
            assert!(!reply.ok);
            assert_eq!(reply.error.as_ref().unwrap().disposition, "not_executed");
            assert_eq!(
                serde_json::to_value(reply.state).unwrap(),
                failed_projection
            );
            assert!(*host.worker.frame.lock().unwrap() == failed_frame);
            assert_eq!(host.worker.hit_mask, failed_mask);
        }
        let effects_before_pet_clear = host.port.state.lock().unwrap().effects.len();
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::HitMask {
            width: 1,
            height: 1,
            alpha: vec![0],
            frame: playing_triple,
            reply,
        });
        assert!(
            receive.recv().unwrap().is_err(),
            "pet mask response cannot clear the host lock"
        );
        assert_eq!(
            host.port.state.lock().unwrap().effects.len(),
            effects_before_pet_clear
        );
        assert!(host.worker.hit_mask_clear_failed);
        host.port.state.lock().unwrap().fail_hit_mask = true;
        let failed_recovery = host.material_command(
            MaterialCommand::Stop {
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
            context.clone(),
        );
        assert!(!failed_recovery.ok);
        assert_eq!(
            failed_recovery.error.as_ref().unwrap().disposition,
            "unknown"
        );
        assert!(host.worker.hit_mask_clear_failed);
        assert!(*host.worker.frame.lock().unwrap() == failed_frame);

        host.port.state.lock().unwrap().fail_hit_mask = false;
        let stopped = host.material_command(
            MaterialCommand::Stop {
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
            context.clone(),
        );
        assert!(stopped.ok, "explicit stop retries the native mask clear");
        let empty = host.worker.frame.lock().unwrap().clone();
        assert_eq!(
            (
                empty.source.as_str(),
                empty.width,
                empty.height,
                empty.frame_index
            ),
            ("empty", 0, 0, None)
        );
        let empty_triple = MaterialFrameTriple {
            resource_generation: empty.resource_generation,
            playback_generation: empty.playback_generation,
            presentation_sequence: empty.presentation_sequence,
        };
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::HitMask {
            width: 1,
            height: 1,
            alpha: vec![255],
            frame: empty_triple,
            reply,
        });
        assert!(receive.recv().unwrap().is_err());
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::HitMask {
            width: 1,
            height: 1,
            alpha: vec![0],
            frame: empty_triple,
            reply,
        });
        assert!(receive.recv().unwrap().is_ok());
        assert!(!host.worker.hit_mask_clear_failed);
        let replayed = host.material_command(
            MaterialCommand::Play {
                action_id: "walk".into(),
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
            context.clone(),
        );
        assert!(replayed.ok, "play succeeds after explicit mask recovery");
        let replay_frame = host.worker.frame.lock().unwrap().clone();
        let replay_triple = MaterialFrameTriple {
            resource_generation: replay_frame.resource_generation,
            playback_generation: replay_frame.playback_generation,
            presentation_sequence: replay_frame.presentation_sequence,
        };
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::HitMask {
            width: 2,
            height: 2,
            alpha: vec![255; 4],
            frame: replay_triple,
            reply,
        });
        assert!(receive.recv().unwrap().is_ok());
        host.port.state.lock().unwrap().fail_hit_mask = true;
        let failed_stop = host.material_command(
            MaterialCommand::Stop {
                expected_resource_generation: replayed.state.resource_generation,
                expected_playback_generation: replayed.state.playback_generation,
            },
            context.clone(),
        );
        assert!(!failed_stop.ok);
        assert_eq!(failed_stop.error.as_ref().unwrap().disposition, "unknown");
        host.port.state.lock().unwrap().fail_hit_mask = false;
        let unloaded = host.material_command(
            MaterialCommand::Unload {
                expected_resource_generation: failed_stop.state.resource_generation,
                expected_playback_generation: failed_stop.state.playback_generation,
            },
            context.clone(),
        );
        assert!(unloaded.ok && unloaded.state.source == "empty");
        assert!(!host.worker.hit_mask_clear_failed);
        let builtin = host.material_command(
            MaterialCommand::Builtin {
                expected_resource_generation: unloaded.state.resource_generation,
                expected_playback_generation: unloaded.state.playback_generation,
            },
            context,
        );
        assert!(builtin.ok && builtin.state.source == "builtin");
        assert_eq!(builtin.state.action_id.as_deref(), Some("idle"));
        assert_eq!(builtin.state.actions[0].id, "idle");
        assert_eq!(builtin.state.status, "playing");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builtin_material_mask_clear_failure_locks_commands_until_explicit_recovery() {
        let mut host = TestHost::new_volatile();
        let context = host.worker.active_context().unwrap();
        let initial = host.worker.material_host.state();
        let played = host.material_command(
            MaterialCommand::Play {
                action_id: "idle".into(),
                expected_resource_generation: initial.resource_generation,
                expected_playback_generation: initial.playback_generation,
            },
            context.clone(),
        );
        assert!(played.ok);
        let frame = host.worker.frame.lock().unwrap().clone();
        let triple = MaterialFrameTriple {
            resource_generation: frame.resource_generation,
            playback_generation: frame.playback_generation,
            presentation_sequence: frame.presentation_sequence,
        };
        let (reply, receive) = mpsc::sync_channel(1);
        host.worker.handle(Request::HitMask {
            width: 2,
            height: 2,
            alpha: vec![255; 4],
            frame: triple,
            reply,
        });
        assert!(receive.recv().unwrap().is_ok());
        host.port.state.lock().unwrap().fail_hit_mask = true;
        let stopped = host.material_command(
            MaterialCommand::Stop {
                expected_resource_generation: played.state.resource_generation,
                expected_playback_generation: played.state.playback_generation,
            },
            context.clone(),
        );
        assert!(!stopped.ok);
        assert_eq!(stopped.error.as_ref().unwrap().disposition, "unknown");
        assert!(host.worker.hit_mask_clear_failed);
        let failed = serde_json::to_value(stopped.state).unwrap();
        let failed_frame = host.worker.frame.lock().unwrap().clone();
        let mut generation = host.worker.material_host.state().resource_generation;
        let playback_generation = host.worker.material_host.state().playback_generation;
        for command in [
            MaterialCommand::Load {
                path: std::env::temp_dir().display().to_string(),
                expected_resource_generation: generation,
                expected_playback_generation: playback_generation,
            },
            MaterialCommand::Play {
                action_id: "idle".into(),
                expected_resource_generation: generation,
                expected_playback_generation: playback_generation,
            },
            MaterialCommand::Pause {
                expected_resource_generation: generation,
                expected_playback_generation: playback_generation,
            },
            MaterialCommand::Resume {
                expected_resource_generation: generation,
                expected_playback_generation: playback_generation,
            },
            MaterialCommand::Builtin {
                expected_resource_generation: generation,
                expected_playback_generation: playback_generation,
            },
        ] {
            let reply = host.material_command(command, context.clone());
            assert!(!reply.ok);
            assert_eq!(reply.error.as_ref().unwrap().disposition, "not_executed");
            assert_eq!(serde_json::to_value(reply.state).unwrap(), failed);
            assert!(*host.worker.frame.lock().unwrap() == failed_frame);
            generation = host.worker.material_host.state().resource_generation;
        }
        let failed_recovery = host.material_command(
            MaterialCommand::Stop {
                expected_resource_generation: generation,
                expected_playback_generation: playback_generation,
            },
            context.clone(),
        );
        assert!(!failed_recovery.ok);
        assert_eq!(
            failed_recovery.error.as_ref().unwrap().disposition,
            "unknown"
        );
        assert!(host.worker.hit_mask_clear_failed);
        assert_eq!(serde_json::to_value(failed_recovery.state).unwrap(), failed);
        assert!(*host.worker.frame.lock().unwrap() == failed_frame);
        host.port.state.lock().unwrap().fail_hit_mask = false;
        let recovered = host.material_command(
            MaterialCommand::Stop {
                expected_resource_generation: generation,
                expected_playback_generation: playback_generation,
            },
            context.clone(),
        );
        assert!(
            recovered.ok,
            "recovery failed: {}",
            recovered
                .error
                .as_ref()
                .map(|error| error.code.as_str())
                .unwrap_or("no error")
        );
        assert!(!host.worker.hit_mask_clear_failed);
        assert_eq!(recovered.state.status, "stopped");
        let replayed = host.material_command(
            MaterialCommand::Play {
                action_id: "idle".into(),
                expected_resource_generation: recovered.state.resource_generation,
                expected_playback_generation: recovered.state.playback_generation,
            },
            context,
        );
        assert!(replayed.ok, "play succeeds after explicit native recovery");
        assert_eq!(replayed.state.status, "playing");
    }

    #[test]
    fn stopped_builtin_pause_and_resume_are_rejected_before_animator_calls() {
        let mut host = TestHost::new_volatile();
        let context = host.worker.active_context().unwrap();
        let initial = host.worker.material_host.state();
        let played = host.material_command(
            MaterialCommand::Play {
                action_id: "idle".into(),
                expected_resource_generation: initial.resource_generation,
                expected_playback_generation: initial.playback_generation,
            },
            context.clone(),
        );
        assert!(played.ok);
        let paused = host.material_command(
            MaterialCommand::Pause {
                expected_resource_generation: played.state.resource_generation,
                expected_playback_generation: played.state.playback_generation,
            },
            context.clone(),
        );
        assert!(paused.ok && paused.state.status == "paused");
        let resumed = host.material_command(
            MaterialCommand::Resume {
                expected_resource_generation: paused.state.resource_generation,
                expected_playback_generation: paused.state.playback_generation,
            },
            context.clone(),
        );
        assert!(resumed.ok && resumed.state.status == "playing");
        let stopped = host.material_command(
            MaterialCommand::Stop {
                expected_resource_generation: resumed.state.resource_generation,
                expected_playback_generation: resumed.state.playback_generation,
            },
            context.clone(),
        );
        assert!(stopped.ok);
        let stopped_projection = serde_json::to_value(&stopped.state).unwrap();
        let empty_frame = host.worker.frame.lock().unwrap().clone();
        assert_eq!(empty_frame.source, "empty");
        let animator_snapshot = host.worker.animator.as_ref().unwrap().snapshot();
        for command in [
            MaterialCommand::Pause {
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
            MaterialCommand::Resume {
                expected_resource_generation: stopped.state.resource_generation,
                expected_playback_generation: stopped.state.playback_generation,
            },
        ] {
            let reply = host.material_command(command, context.clone());
            assert!(!reply.ok);
            assert_eq!(reply.error.as_ref().unwrap().disposition, "not_executed");
            assert_eq!(
                serde_json::to_value(reply.state).unwrap(),
                stopped_projection
            );
            assert!(*host.worker.frame.lock().unwrap() == empty_frame);
            assert_eq!(
                host.worker.animator.as_ref().unwrap().snapshot(),
                animator_snapshot
            );
        }
    }

    #[test]
    fn material_lifecycle_blocks_load_after_revoke_and_during_close_confirmation() {
        for revoke in [true, false] {
            let mut host = TestHost::new();
            let context = host.worker.active_context().unwrap();
            if revoke {
                assert!(host.dispatch(ChatCommand::Revoke).error.is_none());
                assert!(host.snapshot().revoked);
                assert_eq!(host.worker.active_context().unwrap(), context);
            } else {
                let draft = guarded_draft(&host.snapshot(), "protected synthetic draft");
                assert!(host.dispatch(draft).error.is_none());
                assert!(host.close().error.is_none());
                assert!(host.snapshot().close_confirmation);
            }
            let before = host.worker.material_host.state();
            let reply = host.material_command(
                MaterialCommand::Load {
                    path: host.root.as_ref().unwrap().display().to_string(),
                    expected_resource_generation: before.resource_generation,
                    expected_playback_generation: before.playback_generation,
                },
                context,
            );
            let spawned = host.worker.material_load.is_some();
            host.worker.shutdown();
            assert!(!reply.ok, "load accepted with revoke={revoke}");
            assert!(!spawned, "background task created with revoke={revoke}");
            assert_eq!(reply.error.unwrap().disposition, "not_executed");
            assert_eq!(reply.state.revision, before.revision);
        }
    }

    #[test]
    fn material_lifecycle_revoke_invalidates_prepared_candidate_with_same_buddy_context() {
        let mut host = TestHost::new();
        let context = host.worker.active_context().unwrap();
        let root = host.root.as_ref().unwrap().join("synthetic-material");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("frame.png"), synthetic_png_rgba([1, 2, 3, 255])).unwrap();
        std::fs::write(root.join("manifest.json"), r#"{"formatVersion":1,"id":"test-pack","version":1,"label":"Test pack","clips":[{"id":"walk","type":"sequence","frames":[{"path":"frame.png","durationMs":10}]}],"actions":[{"id":"walk","label":"Walk","clip":"walk","mode":"loop"}],"defaultAction":"walk"}"#).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let dropped = Arc::new(AtomicBool::new(false));
        host.worker
            .start_material_load_with_barrier(
                root.display().to_string(),
                context.clone(),
                Some(Arc::clone(&barrier)),
                Some(Arc::clone(&dropped)),
            )
            .unwrap();
        barrier.wait();
        let cancel = Arc::clone(&host.worker.material_load.as_ref().unwrap().cancel);
        let result = host.dispatch(ChatCommand::Revoke);
        let cancelled_at_revoke = cancel.load(Ordering::Acquire);
        let context_after = host.worker.active_context().unwrap();
        barrier.wait();
        for _ in 0..100 {
            host.worker.poll_material_load();
            if host.worker.material_load.is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        let state = host.worker.material_host.state();
        host.worker.shutdown();
        assert!(result.error.is_none());
        assert_eq!(context_after, context);
        assert!(cancelled_at_revoke, "revocation retained a live candidate");
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(state.source, "builtin");
        assert!(state.pack.is_none());
    }

    #[test]
    fn legacy_pause_does_not_revive_stopped_or_unloaded_material() {
        let mut host = TestHost::new_volatile();
        let context = host.worker.active_context().unwrap();
        for unload in [false, true] {
            let state = host.worker.material_host.state();
            let command = if unload {
                MaterialCommand::Unload {
                    expected_resource_generation: state.resource_generation,
                    expected_playback_generation: state.playback_generation,
                }
            } else {
                MaterialCommand::Stop {
                    expected_resource_generation: state.resource_generation,
                    expected_playback_generation: state.playback_generation,
                }
            };
            assert!(host.material_command(command, context.clone()).ok);
            let before = serde_json::to_value(host.worker.material_host.state()).unwrap();
            let frame = host.worker.frame.lock().unwrap().clone();
            assert_eq!(frame.source, "empty");
            let (reply, receive) = mpsc::sync_channel(1);
            host.worker.handle(Request::TogglePause(reply));
            assert!(receive.recv().unwrap().is_err(), "unload={unload}");
            host.worker.tick(false, 0);
            assert_eq!(
                serde_json::to_value(host.worker.material_host.state()).unwrap(),
                before
            );
            assert_eq!(host.worker.frame.lock().unwrap().source, "empty");
        }
    }

    #[test]
    fn material_close_then_cancel_before_tick_keeps_candidate_invalidated() {
        let mut host = TestHost::new();
        let context = host.worker.active_context().unwrap();
        let draft = guarded_draft(&host.snapshot(), "synthetic protected draft");
        assert!(host.dispatch(draft).error.is_none());
        let root = host.root.as_ref().unwrap().join("synthetic-close-material");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("frame.png"), synthetic_png_rgba([1, 2, 3, 255])).unwrap();
        std::fs::write(root.join("manifest.json"), r#"{"formatVersion":1,"id":"test-pack","version":1,"label":"Test pack","clips":[{"id":"walk","type":"sequence","frames":[{"path":"frame.png","durationMs":10}]}],"actions":[{"id":"walk","label":"Walk","clip":"walk","mode":"loop"}],"defaultAction":"walk"}"#).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        host.worker
            .start_material_load_with_barrier(
                root.display().to_string(),
                context,
                Some(Arc::clone(&barrier)),
                None,
            )
            .unwrap();
        barrier.wait();
        let cancel = Arc::clone(&host.worker.material_load.as_ref().unwrap().cancel);
        let closed = host.close();
        let prompted = host.snapshot().close_confirmation;
        let cancelled = host.confirm(false);
        let candidate_invalid = cancel.load(Ordering::Acquire);
        barrier.wait();
        for _ in 0..100 {
            host.worker.poll_material_load();
            if host.worker.material_load.is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        let state = host.worker.material_host.state();
        host.worker.shutdown();
        assert!(closed.error.is_none() && prompted && cancelled.error.is_none());
        assert!(
            candidate_invalid,
            "cancelled close revived the earlier candidate"
        );
        assert_eq!(state.source, "builtin");
    }

    #[test]
    fn cancelled_material_candidate_is_dropped_before_task_slot_clears() {
        let mut host = TestHost::new_volatile();
        let context = host.worker.active_context().unwrap();
        let root = isolated_root();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("frame.png"), synthetic_png_rgba([1, 2, 3, 255])).unwrap();
        std::fs::write(root.join("manifest.json"), r#"{"formatVersion":1,"id":"test-pack","version":1,"label":"Test pack","clips":[{"id":"walk","type":"sequence","frames":[{"path":"frame.png","durationMs":10}]}],"actions":[{"id":"walk","label":"Walk","clip":"walk","mode":"loop"}],"defaultAction":"walk"}"#).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let dropped = Arc::new(AtomicBool::new(false));
        host.worker
            .start_material_load_with_barrier(
                root.display().to_string(),
                context,
                Some(Arc::clone(&barrier)),
                Some(Arc::clone(&dropped)),
            )
            .unwrap();
        barrier.wait();
        let cancel = Arc::clone(&host.worker.material_load.as_ref().unwrap().cancel);
        cancel.store(true, Ordering::Release);
        host.worker.poll_material_load();
        assert!(host.worker.material_load.is_some());
        assert!(!dropped.load(Ordering::Acquire));
        barrier.wait();
        for _ in 0..100 {
            host.worker.poll_material_load();
            if host.worker.material_load.is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(host.worker.material_load.is_none());
        assert!(dropped.load(Ordering::Acquire));
        host.worker.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
}
