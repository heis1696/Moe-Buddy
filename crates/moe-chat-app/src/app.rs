//! `ChatApplication`：受信文字聊天应用组合边界。
//!
//! 持有唯一的 [`UiManager`](moe_ui_core::UiManager) 与服务组合；宿主仅通过
//! `new/dispatch/snapshot` 交互。面板生命周期一律由 UI 内核管理，展示缓存
//! 只由真实视图回调写入，业务失败经类型化通道保留原始错误链上抛。

use moe_memory_core::{Namespace, PetIdentity};
use std::cell::RefCell;
use std::rc::Rc;

use moe_ui_core::{
    CloseConfirmation, CloseRequest, PanelEntry, PanelHandle, PanelKind as UiPanelKind, PanelScope,
    RejectionReason, ReleaseCause, ScopeLevel, UiConfig, UiError, UiManager, UiPanel, Visibility,
};

use crate::contract::{
    Action, AppSnapshot, ChatDraftDiscard, ChatDraftSession, ChatDraftSnapshot, ChatSnapshot,
    ChatTurnProgress, ExternalRequest, ExternalRequestTicket, MAX_LOGICAL_SESSIONS,
    MAX_SAFE_INTEGER, MAX_SESSION_ID_BYTES, MAX_TEXT_BYTES, PanelKind, PanelSnapshot,
    PanelVisibility, ParsedReply, ReplyParser, SessionSnapshot,
};
use crate::error::AppError;
use crate::panels::{
    ChatController, ChatDisplay, ChatEvent, ChatView, CounterSlot, DiagnosticsController,
    DiagnosticsDisplay, DiagnosticsEvent, DiagnosticsView, FailureCell, MemoryController,
    MemoryDisplay, MemoryEvent, MemoryView, PanelArgs, PanelCounters, PresetsController,
    PresetsDisplay, PresetsEvent, PresetsView,
};
use crate::services::{ChatCore, ExternalCallback, REMEMBER_MAX_CHARS};

const PANEL_COUNT: usize = 4;
const INDEX_CHAT: usize = 0;
const INDEX_PRESETS: usize = 1;
const INDEX_MEMORY: usize = 2;
const INDEX_DIAGNOSTICS: usize = 3;

struct LogicalSession {
    id: String,
    label: String,
    archived: bool,
    draft: String,
    /// Draft-set revision of this session's latest accepted draft change.
    draft_revision: u64,
    /// Draft-set revision of the latest explicit discard that consumed this
    /// session's unsent text, if any.
    discarded_revision: Option<u64>,
    status: crate::ChatStatus,
}

type ChatEntry = PanelEntry<ChatController, ChatView>;
type PresetsEntry = PanelEntry<PresetsController, PresetsView>;
type MemoryEntry = PanelEntry<MemoryController, MemoryView>;
type DiagnosticsEntry = PanelEntry<DiagnosticsController, DiagnosticsView>;

struct PanelEntries {
    chat: ChatEntry,
    presets: PresetsEntry,
    memory: MemoryEntry,
    diagnostics: DiagnosticsEntry,
}

fn index_of(kind: PanelKind) -> usize {
    match kind {
        PanelKind::Chat => INDEX_CHAT,
        PanelKind::Presets => INDEX_PRESETS,
        PanelKind::Memory => INDEX_MEMORY,
        PanelKind::Diagnostics => INDEX_DIAGNOSTICS,
    }
}

fn kind_at(index: usize) -> PanelKind {
    match index {
        INDEX_CHAT => PanelKind::Chat,
        INDEX_PRESETS => PanelKind::Presets,
        INDEX_MEMORY => PanelKind::Memory,
        _ => PanelKind::Diagnostics,
    }
}

/// 受信文字聊天应用；由单宿主持有，不可克隆。
pub struct ChatApplication {
    identity: PetIdentity,
    core: Rc<RefCell<ChatCore>>,
    ui: UiManager<()>,
    entries: PanelEntries,
    handles: [Option<PanelHandle>; PANEL_COUNT],
    counters: [CounterSlot; PANEL_COUNT],
    failures: [FailureCell; PANEL_COUNT],
    chat_display: Rc<RefCell<ChatDisplay>>,
    presets_display: Rc<RefCell<PresetsDisplay>>,
    memory_display: Rc<RefCell<MemoryDisplay>>,
    diagnostics_display: Rc<RefCell<DiagnosticsDisplay>>,
    pending_close: Option<(PanelKind, CloseConfirmation)>,
    context: (),
    revision: u64,
    active_session_id: Option<String>,
    sessions_revision: u64,
    draft_set_revision: u64,
    chat_draft_discard: Option<ChatDraftDiscard>,
    next_session_sequence: u64,
    sessions: Vec<LogicalSession>,
    notice: Option<String>,
}

impl ChatApplication {
    /// Enable explicit host history persistence before admitting any messages.
    pub fn enable_history(
        &mut self,
        store: Box<dyn crate::ChatHistory>,
        retention: moe_agent_runtime::RawRetention,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let recovery = self
            .core
            .borrow_mut()
            .enable_history(store, retention, now_ms)?;
        self.active_session_id = Some(recovery.active_session_id.clone());
        self.sessions_revision = recovery.sessions_revision;
        let draft_revision = self.draft_set_revision;
        self.sessions = recovery
            .sessions
            .into_iter()
            .map(|session| LogicalSession {
                id: session.id,
                label: session.label,
                archived: session.archived,
                draft: String::new(),
                draft_revision,
                discarded_revision: None,
                status: session.status,
            })
            .collect();
        self.next_session_sequence = self
            .sessions
            .iter()
            .filter_map(|session| session.id.strip_prefix("session-")?.parse::<u64>().ok())
            .max()
            .unwrap_or(1);
        self.sync_active_session();
        Ok(())
    }

    /// Last authoritative raw and directory commit receipts from the history adapter.
    pub fn history_commit_receipts(&self) -> crate::HistoryCommitReceipts {
        self.core.borrow().storage_commit_receipts()
    }

    /// Close business operations after a host has confirmed a newer persisted binding
    /// but could not apply it to this live instance.
    pub fn mark_storage_unknown(&mut self) {
        self.core.borrow_mut().mark_storage_unknown();
    }

    /// Close this instance if a committed buddy binding cannot be applied in memory.
    pub fn mark_buddy_configuration_unknown(&mut self) {
        self.core.borrow_mut().mark_buddy_configuration_unknown();
    }

    /// Run the existing bounded raw-history retention maintenance.
    pub fn maintain_retention(
        &mut self,
        now_ms: u64,
    ) -> Result<Option<crate::HistoryMaintenanceReport>, AppError> {
        let result = self
            .core
            .borrow_mut()
            .maintain_retention(now_ms, self.sessions_revision, 4);
        match result {
            Err(primary) => {
                if self.handles[INDEX_CHAT].is_some()
                    && let Err(refresh_error) = self.deliver_chat(ChatEvent::TransportRefresh)
                {
                    self.sync_active_session();
                    return Err(AppError::with_retained(primary, refresh_error));
                }
                self.sync_active_session();
                Err(primary)
            }
            Ok(report) => {
                if self.handles[INDEX_CHAT].is_some()
                    && let Err(error) = self.deliver_chat(ChatEvent::TransportRefresh)
                {
                    self.sync_active_session();
                    return Err(error);
                }
                self.sync_active_session();
                Ok(report)
            }
        }
    }

    /// Install trusted settings; a configured model never falls back to a fake reply.
    pub fn configure_model(&mut self, settings: &crate::ModelSettings) -> Result<(), AppError> {
        let next_revision = self.checked_revision()?;
        self.core.borrow_mut().configure_model(settings)?;
        if self.handles[INDEX_PRESETS]
            .as_ref()
            .and_then(|handle| self.ui.get(handle))
            .is_some_and(|summary| summary.visibility == Visibility::Visible)
        {
            self.deliver_presets(PresetsEvent::SettingsRefresh)?;
        }
        self.revision = next_revision;
        Ok(())
    }

    /// Restore a persisted, non-secret model binding at its trusted profile versions.
    /// Transmission consent remains a host-only, per-process setting.
    pub fn configure_model_binding(
        &mut self,
        settings: &crate::ModelSettings,
        model_revision: u64,
        persona_revision: u64,
        preset_id: &str,
    ) -> Result<(), AppError> {
        let next_revision = self.checked_revision()?;
        self.core.borrow_mut().configure_model_binding(
            settings,
            model_revision,
            persona_revision,
            preset_id,
        )?;
        if self.handles[INDEX_PRESETS]
            .as_ref()
            .and_then(|handle| self.ui.get(handle))
            .is_some_and(|summary| summary.visibility == Visibility::Visible)
        {
            self.deliver_presets(PresetsEvent::SettingsRefresh)?;
        }
        self.revision = next_revision;
        Ok(())
    }

    /// Validate a persisted binding without changing memory, permissions, or UI state.
    pub fn preflight_model_binding(
        &self,
        settings: &crate::ModelSettings,
        model_revision: u64,
        persona_revision: u64,
        preset_id: &str,
    ) -> Result<(), AppError> {
        self.checked_revision()?;
        self.core.borrow().preflight_model_binding(
            settings,
            model_revision,
            persona_revision,
            preset_id,
        )
    }

    /// Check that the host may move trusted selection away from this instance.
    /// Drafts remain owned by this application and are not consumed.
    pub fn preflight_buddy_switch(&self) -> Result<(), AppError> {
        if self.pending_close.is_some() {
            return Err(AppError::rejection(
                "close_confirmation_pending",
                "关闭确认尚未处理，当前伙伴不能切换",
                "a close confirmation is pending",
            )
            .not_executed());
        }
        self.core.borrow().ensure_session_lifecycle_available()
    }

    /// Consume the prepared request immediately after Send, on the UI thread.
    /// Move only this immutable input and its ticket to the host worker; never
    /// the application. There is no completion path without the returned ticket.
    pub fn take_external_request(&mut self) -> Option<ExternalRequest> {
        self.core.borrow_mut().take_external_request()
    }

    /// Submit the single worker result bound to its request/flow ticket.
    ///
    /// Duplicate, cross-turn, cross-instance and revoked callbacks are rejected
    /// before the current pending request can be consumed or any checkpoint,
    /// display refresh or publish completion can run. An admitted callback later
    /// has its business, storage and display failures retained independently:
    /// none of them may mask another unknown side effect.
    pub fn complete_external(
        &mut self,
        ticket: ExternalRequestTicket,
        outcome: moe_agent_runtime::ModelOutcome,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let next_revision = self.checked_revision()?;
        let callback = self
            .core
            .borrow_mut()
            .complete_external(ticket, outcome, now_ms);
        let result = match callback {
            ExternalCallback::Rejected(error) => {
                self.revision = next_revision;
                return Err(error);
            }
            ExternalCallback::Admitted { result } => result,
        };
        let mut failure = result.err();
        if let Err(error) = self
            .core
            .borrow_mut()
            .checkpoint_history(now_ms, self.sessions_revision)
        {
            failure = combine_failure(failure, Some(error));
        }
        let token = self.core.borrow().pending_publication();
        let display = self.deliver_chat(ChatEvent::TransportRefresh);
        if let Some(token) = token {
            let publication;
            let mut display_drive_failure = None;
            match display {
                Ok(()) => {
                    publication = if self.chat_display.borrow().publication == Some(token) {
                        Ok(())
                    } else {
                        Err(display_not_published())
                    };
                }
                Err(error) => {
                    if self.chat_display.borrow().publication == Some(token) {
                        // The display facade rendered the turn; the drive failure
                        // belongs to another panel and is retained separately.
                        publication = Ok(());
                        display_drive_failure = Some(error);
                    } else {
                        publication = Err(error);
                    }
                }
            }
            failure = combine_failure(failure, display_drive_failure);
            if let Err(error) = self
                .core
                .borrow_mut()
                .finish_display_publish(publication, now_ms)
            {
                failure = combine_failure(failure, Some(error));
            }
        } else if let Err(error) = display {
            failure = combine_failure(failure, Some(error));
        }
        self.sync_active_session();
        self.revision = next_revision;
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Install the narrow bounded reply parser used by the parse node.
    ///
    /// Replacement is refused while an external request is pending so a turn's
    /// parsed display cannot change underneath an already dispatched request.
    pub fn install_reply_parser(&mut self, parser: Box<dyn ReplyParser>) -> Result<(), AppError> {
        self.core.borrow_mut().install_reply_parser(parser)
    }

    /// Read-only phase progress for the current or most recent turn.
    pub fn chat_progress(&self) -> Option<ChatTurnProgress> {
        self.core.borrow().chat_progress()
    }

    /// Display and presentation proposals produced by the last parse node.
    ///
    /// The raw model text remains in the runtime session ledger; this accessor
    /// never exposes a second persistent raw copy.
    pub fn last_parsed_reply(&self) -> Option<ParsedReply> {
        self.core.borrow().last_parsed_reply()
    }
    /// 创建应用；`process_generation` 必须非零，且由宿主保证跨重启不复用。
    pub fn new(process_generation: u64) -> Result<Self, AppError> {
        Self::with_memory(
            process_generation,
            Box::new(crate::storage::VolatileMemory(
                moe_memory_core::MemoryService::new(),
            )),
        )
    }

    /// Construct with a host-owned memory adapter. Restored records are reauthorized here.
    pub fn with_memory(
        process_generation: u64,
        memory: Box<dyn crate::ChatMemory>,
    ) -> Result<Self, AppError> {
        let identity = PetIdentity::new(
            Namespace::new("moe-chat", "local")
                .map_err(|error| AppError::from_memory(error).not_executed())?,
            crate::services::COMPANION,
        )
        .map_err(|error| AppError::from_memory(error).not_executed())?;
        Self::with_memory_and_identity(
            process_generation,
            memory,
            identity,
            1,
            1,
            crate::buddy::default_persona(),
        )
    }

    /// Construct with a host-owned memory adapter and stable companion identity.
    /// The identity and persona revision are restored from the trusted profile;
    /// the host remains responsible for persistence and process generation.
    pub fn with_memory_and_identity(
        process_generation: u64,
        memory: Box<dyn crate::ChatMemory>,
        identity: PetIdentity,
        persona_revision: u64,
        model_revision: u64,
        persona: &str,
    ) -> Result<Self, AppError> {
        if process_generation == 0 {
            return Err(AppError::rejection(
                "invalid_process_generation",
                "进程代际必须为非零值",
                "process generation must be nonzero",
            )
            .not_executed());
        }
        if persona_revision == 0
            || model_revision == 0
            || persona_revision > crate::MAX_SAFE_INTEGER
            || model_revision > crate::MAX_SAFE_INTEGER
            || !crate::buddy::valid_persona(persona)
        {
            return Err(AppError::rejection(
                "invalid_buddy_profile",
                "伙伴身份或配置版本无效",
                "invalid trusted buddy profile",
            )
            .not_executed());
        }
        let initial_session_id = String::from("session-1");
        let core = Rc::new(RefCell::new(ChatCore::new_for_identity(
            process_generation,
            memory,
            initial_session_id.clone(),
            identity.clone(),
            persona,
            persona_revision,
            model_revision,
        )?));
        let config = UiConfig::new(8, 8, 16, 8, 32, 8).map_err(AppError::from_ui)?;
        let mut ui = UiManager::new(config).map_err(AppError::from_ui)?;

        let counters: [CounterSlot; PANEL_COUNT] =
            std::array::from_fn(|_| Rc::new(RefCell::new(None::<Rc<PanelCounters>>)));
        let failures: [FailureCell; PANEL_COUNT] =
            std::array::from_fn(|_| Rc::new(RefCell::new(None::<AppError>)));
        let chat_display = Rc::new(RefCell::new(ChatDisplay::cleared()));
        let presets_display = Rc::new(RefCell::new(PresetsDisplay::default()));
        let memory_display = Rc::new(RefCell::new(MemoryDisplay::default()));
        let diagnostics_display = Rc::new(RefCell::new(DiagnosticsDisplay::default()));

        let chat_entry = {
            let core = core.clone();
            let counters = counters[INDEX_CHAT].clone();
            let failures = failures[INDEX_CHAT].clone();
            let display = chat_display.clone();
            ui.register(
                UiPanelKind::new("chat").map_err(AppError::from_ui)?,
                ScopeLevel::Application,
                move |_key, args: &PanelArgs| {
                    let instance_counters = Rc::new(PanelCounters::default());
                    *counters.borrow_mut() = Some(instance_counters.clone());
                    Ok(UiPanel::new(
                        ChatController::new(args.companion.clone(), core.clone(), failures.clone()),
                        ChatView::new(display.clone(), instance_counters),
                    ))
                },
            )?
        };
        let presets_entry = {
            let core = core.clone();
            let counters = counters[INDEX_PRESETS].clone();
            let failures = failures[INDEX_PRESETS].clone();
            let display = presets_display.clone();
            ui.register(
                UiPanelKind::new("presets").map_err(AppError::from_ui)?,
                ScopeLevel::Application,
                move |_key, args: &PanelArgs| {
                    let instance_counters = Rc::new(PanelCounters::default());
                    *counters.borrow_mut() = Some(instance_counters.clone());
                    Ok(UiPanel::new(
                        PresetsController::new(
                            args.companion.clone(),
                            core.clone(),
                            failures.clone(),
                        ),
                        PresetsView::new(display.clone(), instance_counters),
                    ))
                },
            )?
        };
        let memory_entry = {
            let core = core.clone();
            let counters = counters[INDEX_MEMORY].clone();
            let failures = failures[INDEX_MEMORY].clone();
            let display = memory_display.clone();
            ui.register(
                UiPanelKind::new("memory").map_err(AppError::from_ui)?,
                ScopeLevel::Application,
                move |_key, args: &PanelArgs| {
                    let instance_counters = Rc::new(PanelCounters::default());
                    *counters.borrow_mut() = Some(instance_counters.clone());
                    Ok(UiPanel::new(
                        MemoryController::new(
                            args.companion.clone(),
                            core.clone(),
                            failures.clone(),
                        ),
                        MemoryView::new(display.clone(), instance_counters),
                    ))
                },
            )?
        };
        let diagnostics_entry = {
            let core = core.clone();
            let counters = counters[INDEX_DIAGNOSTICS].clone();
            let failures = failures[INDEX_DIAGNOSTICS].clone();
            let display = diagnostics_display.clone();
            ui.register(
                UiPanelKind::new("diagnostics").map_err(AppError::from_ui)?,
                ScopeLevel::Application,
                move |_key, args: &PanelArgs| {
                    let instance_counters = Rc::new(PanelCounters::default());
                    *counters.borrow_mut() = Some(instance_counters.clone());
                    Ok(UiPanel::new(
                        DiagnosticsController::new(
                            args.companion.clone(),
                            core.clone(),
                            failures.clone(),
                        ),
                        DiagnosticsView::new(display.clone(), instance_counters),
                    ))
                },
            )?
        };

        Ok(Self {
            identity,
            core,
            ui,
            entries: PanelEntries {
                chat: chat_entry,
                presets: presets_entry,
                memory: memory_entry,
                diagnostics: diagnostics_entry,
            },
            handles: [None; PANEL_COUNT],
            counters,
            failures,
            chat_display,
            presets_display,
            memory_display,
            diagnostics_display,
            pending_close: None,
            context: (),
            revision: 1,
            active_session_id: Some(initial_session_id.clone()),
            sessions_revision: 1,
            draft_set_revision: 1,
            chat_draft_discard: None,
            next_session_sequence: 1,
            sessions: vec![LogicalSession {
                id: initial_session_id,
                label: String::from("会话 1"),
                archived: false,
                draft: String::new(),
                draft_revision: 1,
                discarded_revision: None,
                status: crate::ChatStatus::Ready,
            }],
            notice: None,
        })
    }

    /// Stable identity for the lifetime of this application instance.
    pub fn identity(&self) -> &PetIdentity {
        &self.identity
    }

    /// 处理一条受控命令；`now_ms` 为宿主注入的单调毫秒时间，回退即拒绝。
    ///
    /// The snapshot revision is checked before the command runs: an exhausted
    /// revision space must not execute a business operation and then claim it
    /// was `NotExecuted`.
    pub fn dispatch(&mut self, action: Action, now_ms: u64) -> Result<(), AppError> {
        self.core.borrow().ensure_usable()?;
        let save_history = matches!(&action, Action::Send | Action::SendGuarded { .. });
        let next_revision = self.checked_revision()?;
        let previous_turn = if save_history {
            self.core
                .borrow()
                .chat_progress()
                .map(|progress| progress.turn)
        } else {
            None
        };
        let inner = self.dispatch_inner(action, now_ms);
        let inner_succeeded = inner.is_ok();
        let mut failure = inner.err();
        if self.core.borrow().has_pending_publish() {
            let token = self.core.borrow().pending_publication();
            let published =
                token.is_some_and(|token| self.chat_display.borrow().publication == Some(token));
            let publication = if published {
                Ok(())
            } else {
                Err(failure.take().unwrap_or_else(display_not_published))
            };
            if let Err(error) = self
                .core
                .borrow_mut()
                .finish_display_publish(publication, now_ms)
            {
                failure = combine_failure(failure, Some(error));
            }
        }
        // A rejected Send must not checkpoint an earlier successful turn.
        // Only this command's commit can justify saving after a display error.
        let committed =
            save_history
                && self.core.borrow().chat_progress().is_some_and(|progress| {
                    progress.committed && Some(progress.turn) != previous_turn
                });
        let should_checkpoint =
            save_history && (inner_succeeded || committed) && !self.core.borrow().is_revoked();
        let checkpoint_result = if should_checkpoint {
            self.core
                .borrow_mut()
                .checkpoint_history(now_ms, self.sessions_revision)
        } else {
            Ok(())
        };
        if let Err(error) = checkpoint_result {
            failure = combine_failure(failure, Some(error));
            if self.handles[INDEX_CHAT].is_some()
                && let Err(refresh_error) = self.deliver_chat(ChatEvent::TransportRefresh)
            {
                failure = combine_failure(failure, Some(refresh_error));
            }
        } else if should_checkpoint
            && self.handles[INDEX_CHAT].is_some()
            && let Err(refresh_error) = self.deliver_chat(ChatEvent::TransportRefresh)
        {
            failure = combine_failure(failure, Some(refresh_error));
        }
        self.sync_active_session();
        self.revision = next_revision;
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn validate_session_guard(
        &self,
        expected_session_id: &str,
        expected_sessions_revision: u64,
    ) -> Result<(), AppError> {
        if self.active_session_id.as_deref() != Some(expected_session_id)
            || self.sessions_revision != expected_sessions_revision
        {
            return Err(session_error("stale_session_command"));
        }
        Ok(())
    }

    fn lifecycle_preflight(
        &mut self,
        expected_session_id: &str,
        expected_sessions_revision: u64,
    ) -> Result<u64, AppError> {
        self.validate_session_guard(expected_session_id, expected_sessions_revision)?;
        if self.pending_close.is_some() {
            return Err(session_error("close_confirmation_pending"));
        }
        self.handle_of(PanelKind::Chat)?;
        self.require_visible_chat()?;
        self.core.borrow().ensure_session_lifecycle_available()?;
        let next_revision = self.checked_sessions_revision()?;
        self.deliver_chat(ChatEvent::TransportRefresh)?;
        Ok(next_revision)
    }

    fn new_session(
        &mut self,
        expected_session_id: &str,
        expected_sessions_revision: u64,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let next_revision =
            self.lifecycle_preflight(expected_session_id, expected_sessions_revision)?;
        let core = self.core.borrow();
        let active_status = core.chat_history()?.status;
        if active_status == crate::ChatStatus::Unknown && !core.session_isolation_allowed() {
            return Err(session_error("session_isolation_denied"));
        }
        drop(core);
        if self.sessions.len() >= MAX_LOGICAL_SESSIONS {
            return Err(session_error("sessions_full"));
        }
        let next_sequence = self
            .next_session_sequence
            .checked_add(1)
            .ok_or_else(|| session_error("session_id_exhausted"))?;
        let id = format!("session-{next_sequence}");
        if id.len() > MAX_SESSION_ID_BYTES {
            return Err(session_error("session_id_exhausted"));
        }
        self.sync_active_session();
        let persistent = self.core.borrow().history_enabled();
        if persistent {
            let recovery = self.core.borrow_mut().rotate_persistent_session(
                &id,
                self.sessions_revision,
                crate::HistoryChange::Create(crate::HistorySession {
                    id: id.clone(),
                    label: format!("会话 {next_sequence}"),
                    archived: false,
                    status: crate::ChatStatus::Ready,
                }),
                now_ms,
                None,
            )?;
            self.replace_sessions_from_recovery(recovery, Some(String::new()));
        } else {
            self.core.borrow_mut().create_logical_session(&id, now_ms)?;
            self.next_session_sequence = next_sequence;
            self.sessions.push(LogicalSession {
                id: id.clone(),
                label: format!("会话 {next_sequence}"),
                archived: false,
                draft: String::new(),
                draft_revision: self.draft_set_revision,
                discarded_revision: None,
                status: crate::ChatStatus::Ready,
            });
            self.active_session_id = Some(id.clone());
            self.sessions_revision = next_revision;
        }
        self.deliver_chat(ChatEvent::SessionChanged {
            draft: String::new(),
        })
        .map_err(AppError::settled)
    }

    fn switch_session(
        &mut self,
        id: &str,
        expected_session_id: &str,
        expected_sessions_revision: u64,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let next_revision =
            self.lifecycle_preflight(expected_session_id, expected_sessions_revision)?;
        if self.core.borrow().chat_history()?.status == crate::ChatStatus::Unknown {
            return Err(session_error("session_isolation_denied"));
        }
        if self.active_session_id.as_deref() == Some(id) {
            return Err(session_error("session_already_active"));
        }
        let Some(target_index) = self.sessions.iter().position(|session| session.id == id) else {
            return Err(session_error("unknown_session"));
        };
        let target_draft = self.sessions[target_index].draft.clone();
        self.sync_active_session();
        if self.core.borrow().history_enabled() {
            let recovery = self.core.borrow_mut().rotate_persistent_session(
                id,
                self.sessions_revision,
                crate::HistoryChange::Switch(id.to_owned()),
                now_ms,
                None,
            )?;
            self.replace_sessions_from_recovery(recovery, Some(target_draft.clone()));
        } else {
            self.core.borrow_mut().switch_logical_session(id, now_ms)?;
            self.active_session_id = Some(id.to_owned());
            self.sessions_revision = next_revision;
        }
        self.deliver_chat(ChatEvent::SessionChanged {
            draft: target_draft,
        })
        .map_err(AppError::settled)
    }

    fn archive_session(
        &mut self,
        id: &str,
        expected_session_id: &str,
        expected_sessions_revision: u64,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let next_revision =
            self.lifecycle_preflight(expected_session_id, expected_sessions_revision)?;
        if id != expected_session_id {
            return Err(session_error("archive_target_not_active"));
        }
        self.sync_active_session();
        let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
            return Err(session_error("unknown_session"));
        };
        if self.sessions[index].archived {
            return Err(session_error("session_archived"));
        }
        let active_draft = self.active_draft().unwrap_or_default();
        if self.core.borrow().history_enabled() {
            let recovery = self.core.borrow_mut().archive_persistent_session(
                id,
                self.sessions_revision,
                now_ms,
            )?;
            self.replace_sessions_from_recovery(recovery, Some(active_draft));
        } else {
            self.sessions[index].archived = true;
            self.sessions_revision = next_revision;
        }
        Ok(())
    }

    fn replace_sessions_from_recovery(
        &mut self,
        recovery: crate::HistoryRecovery,
        active_draft: Option<String>,
    ) {
        let drafts = self
            .sessions
            .iter()
            .map(|session| (session.id.clone(), session.draft.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let draft_revisions = self
            .sessions
            .iter()
            .map(|session| (session.id.clone(), session.draft_revision))
            .collect::<std::collections::BTreeMap<_, _>>();
        let discarded_revisions = self
            .sessions
            .iter()
            .map(|session| (session.id.clone(), session.discarded_revision))
            .collect::<std::collections::BTreeMap<_, _>>();
        self.active_session_id = Some(recovery.active_session_id.clone());
        self.sessions_revision = recovery.sessions_revision;
        let baseline_revision = self.draft_set_revision;
        self.sessions = recovery
            .sessions
            .into_iter()
            .map(|session| LogicalSession {
                draft: if self.active_session_id.as_deref() == Some(session.id.as_str()) {
                    active_draft.clone().unwrap_or_default()
                } else {
                    drafts.get(&session.id).cloned().unwrap_or_default()
                },
                draft_revision: draft_revisions
                    .get(&session.id)
                    .copied()
                    .unwrap_or(baseline_revision),
                discarded_revision: discarded_revisions.get(&session.id).copied().flatten(),
                id: session.id,
                label: session.label,
                archived: session.archived,
                status: session.status,
            })
            .collect();
        self.next_session_sequence = self
            .sessions
            .iter()
            .filter_map(|session| session.id.strip_prefix("session-")?.parse::<u64>().ok())
            .max()
            .unwrap_or(1);
    }

    fn checked_sessions_revision(&self) -> Result<u64, AppError> {
        self.sessions_revision
            .checked_add(1)
            .filter(|revision| *revision <= MAX_SAFE_INTEGER)
            .ok_or_else(|| session_error("sessions_revision_exhausted"))
    }

    fn active_session_archived(&self) -> bool {
        self.active_session_id.as_deref().is_some_and(|id| {
            self.sessions
                .iter()
                .find(|session| session.id == id)
                .is_some_and(|session| session.archived)
        })
    }

    fn sync_active_session(&mut self) {
        let Some(active_id) = self.active_session_id.as_deref() else {
            return;
        };
        if self.core.borrow().is_revoked() {
            return;
        }
        let status = self
            .core
            .borrow()
            .chat_history()
            .ok()
            .map(|history| history.status);
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
            && let Some(status) = status
        {
            session.status = status;
        }
    }

    /// Version of the application-owned set of unsent chat drafts.
    ///
    /// Hosts bind whole-application quit confirmation to this value so an
    /// edit in an inactive logical session invalidates an older confirmation.
    pub fn chat_draft_set_revision(&self) -> u64 {
        self.draft_set_revision
    }

    /// Read-only, body-free receipt for the application-owned chat draft set.
    ///
    /// The current revision advances on every accepted draft mutation. The
    /// latest [`ChatDraftDiscard`] names only the sessions whose unsent text an
    /// explicit `CloseChat`/Quit discard actually consumed; normal empty
    /// snapshots, hiding, re-reads and ordinary edits never create one. The
    /// per-session list exposes each live session's latest draft revision and
    /// latest discard watermark, so an edit in another session cannot make an
    /// old receipt look stale for this session. Hosts consume a discard per
    /// session revision, never from the global revision alone.
    pub fn chat_draft_snapshot(&self) -> ChatDraftSnapshot {
        let mut sessions = self
            .sessions
            .iter()
            .map(|session| ChatDraftSession {
                id: session.id.clone(),
                revision: session.draft_revision,
                discarded_revision: session.discarded_revision,
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.id.cmp(&right.id));
        sessions.truncate(MAX_LOGICAL_SESSIONS);
        ChatDraftSnapshot {
            revision: self.draft_set_revision,
            discard: self.chat_draft_discard.clone(),
            sessions,
        }
    }

    /// Whether any logical session still owns unsent text.
    pub fn has_unsubmitted_chat_drafts(&self) -> bool {
        self.sessions
            .iter()
            .any(|session| !session.draft.is_empty())
    }

    /// Ensure all version advances needed by a confirmed Quit can complete.
    pub fn preflight_quit_draft_discard(
        &self,
        current_close_discards_active: bool,
    ) -> Result<(), AppError> {
        let active_id = self.active_session_id.as_deref();
        let active_draft = self
            .sessions
            .iter()
            .find(|session| Some(session.id.as_str()) == active_id)
            .is_some_and(|session| !session.draft.is_empty());
        let other_drafts = self
            .sessions
            .iter()
            .any(|session| Some(session.id.as_str()) != active_id && !session.draft.is_empty());
        let increments =
            usize::from(current_close_discards_active && active_draft) + usize::from(other_drafts);
        self.draft_set_revision
            .checked_add(increments as u64)
            .ok_or_else(|| {
                AppError::rejection(
                    "counter_overflow",
                    "草稿集合修订号已耗尽",
                    "chat draft set revision space exhausted before Quit",
                )
                .not_executed()
            })?;
        Ok(())
    }

    /// Discard all chat drafts only after the host confirms the exact set version.
    ///
    /// The resulting receipt lists every session whose unsent text this action
    /// consumed. When an immediately preceding confirmed chat close already
    /// discarded the active draft without any draft edit in between (the Quit
    /// sequence does exactly that), its session is folded into the same receipt
    /// so a whole-application Quit never loses the active session.
    pub fn discard_all_chat_drafts(&mut self, expected_revision: u64) -> Result<(), AppError> {
        if self.draft_set_revision != expected_revision {
            return Err(session_error("confirm_stale"));
        }
        let mut discarded = self
            .sessions
            .iter()
            .filter(|session| !session.draft.is_empty())
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        if let Some(previous) = &self.chat_draft_discard
            && previous.revision == self.draft_set_revision
        {
            discarded.extend(previous.session_ids.iter().cloned());
        }
        if discarded.is_empty() {
            return Ok(());
        }
        let next_revision = self.checked_draft_set_revision()?;
        for session in &mut self.sessions {
            session.draft.clear();
            if discarded.iter().any(|id| id == &session.id) {
                session.draft_revision = next_revision;
                session.discarded_revision = Some(next_revision);
            }
        }
        self.draft_set_revision = next_revision;
        discarded.sort();
        discarded.dedup();
        discarded.truncate(MAX_LOGICAL_SESSIONS);
        self.chat_draft_discard = Some(ChatDraftDiscard {
            revision: next_revision,
            session_ids: discarded,
        });
        Ok(())
    }

    fn checked_draft_set_revision(&self) -> Result<u64, AppError> {
        self.draft_set_revision.checked_add(1).ok_or_else(|| {
            AppError::rejection(
                "counter_overflow",
                "草稿集合修订号已耗尽",
                "chat draft set revision space exhausted",
            )
            .not_executed()
        })
    }

    fn set_active_draft(&mut self, text: String) -> Result<(), AppError> {
        let next_revision = self.checked_draft_set_revision()?;
        let active_id = self
            .active_session_id
            .as_deref()
            .ok_or_else(|| session_error("unknown_session"))?;
        let session = self
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
            .ok_or_else(|| session_error("unknown_session"))?;
        session.draft = text;
        session.draft_revision = next_revision;
        self.draft_set_revision = next_revision;
        Ok(())
    }

    fn active_draft(&self) -> Option<String> {
        let active_id = self.active_session_id.as_deref()?;
        self.sessions
            .iter()
            .find(|session| session.id == active_id)
            .map(|session| session.draft.clone())
    }

    fn checked_revision(&self) -> Result<u64, AppError> {
        self.revision.checked_add(1).ok_or_else(|| {
            AppError::rejection(
                "counter_overflow",
                "快照修订号耗尽",
                "snapshot revision space exhausted",
            )
            .not_executed()
        })
    }

    /// 读取当前展示快照；面板数据来自真实视图回调维护的缓存。
    pub fn snapshot(&self) -> AppSnapshot {
        let core = self.core.borrow();
        let mut notice = self.notice.clone();
        if self.diagnostics_display.borrow().gap {
            notice = Some(String::from("诊断日志存在历史缺口，缺失数量未知。"));
        }
        let panels = (0..PANEL_COUNT)
            .map(|index| {
                let kind = kind_at(index);
                let visibility = self.handles[index]
                    .and_then(|handle| self.ui.get(&handle))
                    .map(|summary| match summary.visibility {
                        Visibility::Visible => PanelVisibility::Visible,
                        Visibility::Hidden => PanelVisibility::Hidden,
                    })
                    .unwrap_or(PanelVisibility::Closed);
                let (mounts, renders, unmounts) = self.counters[index]
                    .borrow()
                    .as_ref()
                    .map(|counters| counters.get())
                    .unwrap_or((0, 0, 0));
                PanelSnapshot {
                    kind,
                    visibility,
                    confirmation: self
                        .pending_close
                        .as_ref()
                        .is_some_and(|(pending, _)| *pending == kind),
                    mounts,
                    renders,
                    unmounts,
                }
            })
            .collect();
        let chat_display = self.chat_display.borrow();
        let chat = ChatSnapshot {
            draft: chat_display.draft.clone(),
            messages: chat_display.messages.clone(),
            status: chat_display.status,
            failure: chat_display.failure.clone(),
        };
        let sessions = self
            .sessions
            .iter()
            .map(|session| SessionSnapshot {
                id: session.id.clone(),
                label: session.label.clone(),
                archived: session.archived,
                status: if self.active_session_id.as_deref() == Some(session.id.as_str()) {
                    core.chat_history()
                        .map(|history| history.status)
                        .unwrap_or(chat_display.status)
                } else {
                    session.status
                },
            })
            .collect();
        AppSnapshot {
            revision: self.revision,
            active_session_id: self.active_session_id.clone(),
            sessions_revision: self.sessions_revision,
            sessions,
            session_isolation_allowed: core.session_isolation_allowed(),
            mode: if core.online { "online" } else { "offline" },
            companion: self.identity.instance_id().to_owned(),
            active_preset: core.active_preset_id(),
            external_transmission_allowed: core.transmission_allowed && !core.is_revoked(),
            revoked: core.is_revoked(),
            panels,
            chat,
            presets: self.presets_display.borrow().catalog.clone(),
            memories: self.memory_display.borrow().entries.clone(),
            diagnostics: self.diagnostics_display.borrow().entries.clone(),
            notice,
        }
    }

    fn dispatch_inner(&mut self, action: Action, now_ms: u64) -> Result<(), AppError> {
        if self.core.borrow().is_revoked() {
            return Err(AppError::rejection(
                "revoked",
                "本次授权已撤销，应用不再接受任何操作",
                "authorization revoked for this process",
            )
            .not_executed());
        }
        match action {
            Action::Open(kind) => self.open_panel(kind),
            Action::Hide(kind) => {
                let handle = self.handle_of(kind)?;
                self.ui
                    .hide(&mut self.context, &handle)
                    .map_err(AppError::from_ui)?;
                self.drive()
            }
            Action::Close(kind) => self.close_panel(kind),
            Action::ConfirmClose { panel, discard } => self.confirm_close(panel, discard),
            Action::NewSession {
                expected_session_id,
                expected_sessions_revision,
            } => self.new_session(&expected_session_id, expected_sessions_revision, now_ms),
            Action::SwitchSession {
                id,
                expected_session_id,
                expected_sessions_revision,
            } => self.switch_session(
                &id,
                &expected_session_id,
                expected_sessions_revision,
                now_ms,
            ),
            Action::ArchiveSession {
                id,
                expected_session_id,
                expected_sessions_revision,
            } => self.archive_session(
                &id,
                &expected_session_id,
                expected_sessions_revision,
                now_ms,
            ),
            Action::SetDraft(text) => self.dispatch_set_draft(text),
            Action::SetDraftGuarded {
                text,
                expected_session_id,
                expected_sessions_revision,
            } => {
                self.validate_session_guard(&expected_session_id, expected_sessions_revision)?;
                self.dispatch_set_draft(text)
            }
            Action::Send => self.dispatch_send(now_ms),
            Action::SendGuarded {
                expected_session_id,
                expected_sessions_revision,
            } => {
                self.validate_session_guard(&expected_session_id, expected_sessions_revision)?;
                self.dispatch_send(now_ms)
            }
            Action::SelectPreset(id) => {
                if id.trim().is_empty() {
                    return Err(blank_input());
                }
                self.deliver_presets(PresetsEvent::SelectRequested { id, now_ms })
            }
            Action::Remember(text) => {
                if text.trim().is_empty() {
                    return Err(blank_input());
                }
                if text.len() > MAX_TEXT_BYTES || text.chars().count() > REMEMBER_MAX_CHARS {
                    return Err(too_large());
                }
                self.deliver_memory(MemoryEvent::RememberRequested { text, now_ms })
            }
            Action::RefreshMemories => {
                self.deliver_memory(MemoryEvent::RefreshRequested { now_ms })
            }
            Action::SearchMemories(text) => {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(too_large());
                }
                self.deliver_memory(MemoryEvent::SearchRequested { text, now_ms })
            }
            Action::UpdateMemory { id, revision, text } => {
                if text.trim().is_empty() {
                    return Err(blank_input());
                }
                if text.len() > MAX_TEXT_BYTES || text.chars().count() > REMEMBER_MAX_CHARS {
                    return Err(too_large());
                }
                self.deliver_memory(MemoryEvent::ChangeRequested {
                    id,
                    revision,
                    text: Some(text),
                    now_ms,
                })
            }
            Action::DeleteMemory { id, revision } => {
                self.deliver_memory(MemoryEvent::ChangeRequested {
                    id,
                    revision,
                    text: None,
                    now_ms,
                })
            }
            Action::RefreshDiagnostics => {
                self.deliver_diagnostics(DiagnosticsEvent::RefreshRequested { now_ms })
            }
            Action::Revoke => self.revoke(),
        }
    }

    fn dispatch_set_draft(&mut self, text: String) -> Result<(), AppError> {
        if self.active_session_archived() {
            return Err(session_error("session_archived"));
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(too_large());
        }
        let handle = self.require_visible_chat()?;
        self.checked_draft_set_revision()?;
        if let Err(rejected) =
            self.entries
                .chat
                .send(&mut self.ui, &handle, ChatEvent::DraftChanged(text.clone()))
        {
            return Err(AppError::from_submit(rejected.reason));
        }
        self.set_active_draft(text)?;
        self.drive()
    }

    fn dispatch_send(&mut self, now_ms: u64) -> Result<(), AppError> {
        if self.active_session_archived() {
            return Err(session_error("session_archived"));
        }
        let handle = self.require_visible_chat()?;
        let draft = self.active_draft().unwrap_or_default();
        if draft.trim().is_empty() {
            return Err(blank_input());
        }
        self.checked_draft_set_revision()?;
        let rotate_generation = {
            let core = self.core.borrow();
            core.history_enabled()
                && core.chat_history()?.status != crate::ChatStatus::Unknown
                && core.needs_generation_rotation(&draft)
        };
        if rotate_generation && self.pending_close.is_some() {
            return Err(session_error("close_confirmation_pending").not_executed());
        }
        self.core
            .borrow_mut()
            .preflight_send(&draft, self.sessions_revision, now_ms)?;
        let mut generation_rotated = false;
        if rotate_generation {
            let session_id = self
                .active_session_id
                .clone()
                .ok_or_else(|| session_error("unknown_session"))?;
            self.sync_active_session();
            let recovery = self.core.borrow_mut().rotate_persistent_session(
                &session_id,
                self.sessions_revision,
                crate::HistoryChange::Renew(session_id.clone()),
                now_ms,
                Some(crate::HistorySendReservation {
                    user_characters: draft.chars().count(),
                    reserved_reply_characters: crate::services::REPLY_CHARS_BUDGET,
                }),
            )?;
            generation_rotated = true;
            self.replace_sessions_from_recovery(recovery, Some(draft.clone()));
            if let Err(error) =
                self.core
                    .borrow_mut()
                    .preflight_send(&draft, self.sessions_revision, now_ms)
            {
                return Err(committed_send_error(error, generation_rotated));
            }
        }
        let previous_turn = self
            .core
            .borrow()
            .chat_progress()
            .map(|progress| progress.turn);
        if let Err(rejected) =
            self.entries
                .chat
                .send(&mut self.ui, &handle, ChatEvent::SendRequested { now_ms })
        {
            return Err(committed_send_error(
                AppError::from_submit(rejected.reason),
                generation_rotated,
            ));
        }
        let result = self
            .drive()
            .map_err(|error| committed_send_error(error, generation_rotated));
        let was_admitted_or_committed = {
            let core = self.core.borrow();
            core.chat_history()
                .is_ok_and(|history| history.status == crate::ChatStatus::Sending)
                || core.chat_progress().is_some_and(|progress| {
                    progress.committed && Some(progress.turn) != previous_turn
                })
        };
        if result.is_ok() || was_admitted_or_committed {
            self.set_active_draft(String::new())?;
        }
        result
    }

    fn open_panel(&mut self, kind: PanelKind) -> Result<(), AppError> {
        let args = PanelArgs {
            companion: self.identity.instance_id().to_owned(),
        };
        let outcome = match kind {
            PanelKind::Chat => self
                .ui
                .open(
                    &mut self.context,
                    &self.entries.chat,
                    PanelScope::Application,
                    Some(&args),
                )
                .map_err(AppError::from_ui)?
                .handle(),
            PanelKind::Presets => self
                .ui
                .open(
                    &mut self.context,
                    &self.entries.presets,
                    PanelScope::Application,
                    Some(&args),
                )
                .map_err(AppError::from_ui)?
                .handle(),
            PanelKind::Memory => self
                .ui
                .open(
                    &mut self.context,
                    &self.entries.memory,
                    PanelScope::Application,
                    Some(&args),
                )
                .map_err(AppError::from_ui)?
                .handle(),
            PanelKind::Diagnostics => self
                .ui
                .open(
                    &mut self.context,
                    &self.entries.diagnostics,
                    PanelScope::Application,
                    Some(&args),
                )
                .map_err(AppError::from_ui)?
                .handle(),
        };
        self.handles[index_of(kind)] = Some(outcome);
        self.drive()
    }

    fn close_panel(&mut self, kind: PanelKind) -> Result<(), AppError> {
        let handle = self.handle_of(kind)?;
        let request = self
            .ui
            .close(&mut self.context, &handle)
            .map_err(AppError::from_ui)?;
        match request {
            CloseRequest::Released => {
                self.handles[index_of(kind)] = None;
                self.pending_close = None;
            }
            CloseRequest::ConfirmationPending(confirmation) => {
                self.pending_close = Some((kind, confirmation));
            }
        }
        self.drive()
    }

    fn confirm_close(&mut self, panel: PanelKind, discard: bool) -> Result<(), AppError> {
        // Bind the exact session and post-discard revision before validating the
        // UI confirmation. A draft edit accepted after the close request makes
        // the UI confirmation stale, so a stale confirm never discards text.
        let discard_target = if discard
            && panel == PanelKind::Chat
            && self.active_draft().is_some_and(|draft| !draft.is_empty())
        {
            Some((
                self.active_session_id.clone(),
                self.checked_draft_set_revision()?,
            ))
        } else {
            None
        };
        let pending = match &self.pending_close {
            Some((pending_kind, _)) if *pending_kind == panel => self.pending_close.take(),
            _ => None,
        };
        let Some(confirmation) = pending else {
            return Err(AppError::rejection(
                "no_pending_confirmation",
                "该面板没有待确认的关闭请求",
                "no pending close confirmation for this panel",
            )
            .not_executed());
        };
        if !discard {
            return Ok(());
        }
        match self.ui.confirm_close(&mut self.context, &confirmation.1) {
            Ok(()) => {
                self.handles[index_of(panel)] = None;
                if let Some((Some(active_id), next_revision)) = discard_target
                    && let Some(session) = self
                        .sessions
                        .iter_mut()
                        .find(|session| session.id == active_id)
                {
                    session.draft.clear();
                    session.draft_revision = next_revision;
                    session.discarded_revision = Some(next_revision);
                    self.draft_set_revision = next_revision;
                    // This is the only chat-draft discard receipt for a plain
                    // chat close; other sessions keep their own drafts.
                    self.chat_draft_discard = Some(ChatDraftDiscard {
                        revision: next_revision,
                        session_ids: vec![active_id],
                    });
                }
            }
            Err(UiError::ConfirmationStale) => {
                return Err(AppError::rejection(
                    "confirm_stale",
                    "关闭确认已失效：面板状态在其后发生变化，草稿未丢弃",
                    "close confirmation is stale; draft preserved",
                )
                .not_executed());
            }
            Err(UiError::ConfirmationInstanceGone) => {
                return Err(AppError::rejection(
                    "panel_not_open",
                    "目标面板未打开",
                    "panel instance no longer exists",
                )
                .not_executed());
            }
            Err(error) => return Err(AppError::from_ui(error)),
        }
        self.drive()
    }

    fn revoke(&mut self) -> Result<(), AppError> {
        self.core.borrow_mut().mark_revoked();
        let mut first_error = None;
        for index in 0..PANEL_COUNT {
            if let Some(handle) = self.handles[index] {
                if let Err(error) =
                    self.ui
                        .force_release(&mut self.context, &handle, ReleaseCause::ContextRevoked)
                {
                    first_error.get_or_insert(AppError::from_ui(error));
                }
                self.handles[index] = None;
            }
        }
        self.pending_close = None;
        let service_result = self.core.borrow_mut().revoke_services();
        self.active_session_id = None;
        self.sessions.clear();
        // A receipt may only name live application-owned sessions; revocation
        // clears the directory, so the old receipt must not survive it.
        self.chat_draft_discard = None;
        if self.sessions_revision < MAX_SAFE_INTEGER {
            self.sessions_revision += 1;
        }
        self.notice = Some(String::from(
            "已撤销本次授权：面板与展示缓存已清理，不再接受新操作。",
        ));
        match (first_error, service_result) {
            (Some(error), _) => Err(error),
            (None, Err(error)) => Err(error),
            (None, Ok(())) => Ok(()),
        }
    }

    fn deliver_chat(&mut self, event: ChatEvent) -> Result<(), AppError> {
        let handle = self.handle_of(PanelKind::Chat)?;
        if let Err(rejected) = self.entries.chat.send(&mut self.ui, &handle, event) {
            return Err(AppError::from_submit(rejected.reason));
        }
        self.drive()
    }

    fn deliver_presets(&mut self, event: PresetsEvent) -> Result<(), AppError> {
        let handle = self.handle_of(PanelKind::Presets)?;
        if let Err(rejected) = self.entries.presets.send(&mut self.ui, &handle, event) {
            return Err(AppError::from_submit(rejected.reason));
        }
        self.drive()
    }

    fn deliver_memory(&mut self, event: MemoryEvent) -> Result<(), AppError> {
        let handle = self.handle_of(PanelKind::Memory)?;
        if let Err(rejected) = self.entries.memory.send(&mut self.ui, &handle, event) {
            return Err(AppError::from_submit(rejected.reason));
        }
        self.drive()
    }

    fn deliver_diagnostics(&mut self, event: DiagnosticsEvent) -> Result<(), AppError> {
        let handle = self.handle_of(PanelKind::Diagnostics)?;
        if let Err(rejected) = self.entries.diagnostics.send(&mut self.ui, &handle, event) {
            return Err(AppError::from_submit(rejected.reason));
        }
        self.drive()
    }

    fn handle_of(&self, kind: PanelKind) -> Result<PanelHandle, AppError> {
        self.handles[index_of(kind)].ok_or_else(|| {
            AppError::rejection(
                "panel_not_open",
                "目标面板未打开",
                "panel instance does not exist",
            )
            .not_executed()
        })
    }

    fn require_visible_chat(&self) -> Result<PanelHandle, AppError> {
        let handle = self.handle_of(PanelKind::Chat)?;
        match self.ui.get(&handle) {
            Some(summary) if summary.visibility == Visibility::Visible => Ok(handle),
            Some(_) => Err(AppError::from_submit(RejectionReason::NotVisible)),
            None => Err(AppError::rejection(
                "panel_not_open",
                "目标面板未打开",
                "panel instance does not exist",
            )
            .not_executed()),
        }
    }

    /// 驱动一轮 UI 内核；内核报告的失败与控制器交回的业务失败都不吞掉。
    fn drive(&mut self) -> Result<(), AppError> {
        let report = self.ui.drive(&mut self.context);
        if let Some(fault) = report.faults().first() {
            self.notice = Some(format!(
                "面板 {} 在 {} 阶段失败；阶段信息由 UI 内核报告保留。",
                fault.key, fault.stage
            ));
            return Err(AppError::from_drive_report(report));
        }
        if !report.navigation_faults().is_empty() {
            self.notice = Some(String::from("导航命令未执行，请检查面板状态。"));
            return Err(AppError::from_drive_report(report));
        }
        for cell in &self.failures {
            if let Some(error) = cell.borrow_mut().take() {
                return Err(error);
            }
        }
        Ok(())
    }
}

fn combine_failure(current: Option<AppError>, next: Option<AppError>) -> Option<AppError> {
    match (current, next) {
        (None, next) => next,
        (current, None) => current,
        (Some(primary), Some(secondary)) => Some(AppError::with_retained(primary, secondary)),
    }
}

fn display_not_published() -> AppError {
    AppError::rejection(
        "display_missing",
        "展示门面未反映本轮发布",
        "display cache did not record the staged publication",
    )
}

fn blank_input() -> AppError {
    AppError::rejection("empty_input", "输入为空白，已拒绝", "input is blank")
        .with_execution_disposition(crate::ExecutionDisposition::NotExecuted)
}

fn committed_send_error(error: AppError, generation_rotated: bool) -> AppError {
    if generation_rotated
        && error.execution_disposition() == crate::ExecutionDisposition::NotExecuted
    {
        error.with_execution_disposition(crate::ExecutionDisposition::Settled)
    } else {
        error
    }
}

fn too_large() -> AppError {
    AppError::rejection(
        "input_too_large",
        "输入超过容量上限，已拒绝",
        "input exceeds the bounded capacity",
    )
    .with_execution_disposition(crate::ExecutionDisposition::NotExecuted)
}

fn session_error(code: &'static str) -> AppError {
    let (message, detail) = match code {
        "stale_session_command" => (
            "会话命令已过期；请同步当前会话后重试",
            "expected logical session or directory revision does not match",
        ),
        "close_confirmation_pending" => (
            "关闭确认尚未处理，暂不能执行会话操作",
            "a close confirmation is active",
        ),
        "session_execution_pending" => (
            "当前本地执行或展示尚未结束，不能切换会话",
            "logical session change requires locally quiescent execution",
        ),
        "session_isolation_denied" => (
            "未知请求仅允许显式新建隔离会话",
            "an unknown session can only authorize an explicit isolated new session",
        ),
        "session_archived" => (
            "归档会话为只读状态，不能修改草稿或发送",
            "archived logical sessions are read-only",
        ),
        "archive_target_not_active" => (
            "只能归档当前选中的会话",
            "archive target must be the active logical session",
        ),
        "sessions_full" => (
            "会话目录已达到容量上限",
            "logical session directory capacity exhausted",
        ),
        "sessions_revision_exhausted" => (
            "会话目录修订号已耗尽",
            "directory revision exceeds JavaScript safe integer range",
        ),
        "session_id_exhausted" => (
            "会话标识容量已耗尽",
            "application session identifier sequence exhausted",
        ),
        "session_already_active" => (
            "目标会话已经处于选中状态",
            "target logical session is already active",
        ),
        "unknown_session" => (
            "目标会话不存在",
            "target logical session is not in the trusted directory",
        ),
        _ => ("会话操作未执行", "logical session operation rejected"),
    };
    AppError::rejection(code, message, detail)
        .with_execution_disposition(crate::ExecutionDisposition::NotExecuted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_overflow_is_checked_before_the_command_runs() {
        let mut app = ChatApplication::new(1).unwrap();
        app.dispatch(Action::Open(PanelKind::Chat), 0).unwrap();
        app.revision = u64::MAX;

        let error = app
            .dispatch(Action::SetDraft(String::from("must not be applied")), 1)
            .expect_err("revision space is exhausted");
        assert_eq!(error.public_code(), "counter_overflow");
        assert_eq!(
            error.execution_disposition(),
            crate::ExecutionDisposition::NotExecuted
        );
        assert_eq!(
            app.snapshot().chat.draft,
            "",
            "the command must not execute before the overflow is detected"
        );
        assert_eq!(app.revision, u64::MAX);

        let error = app
            .configure_model(&crate::ModelSettings {
                endpoint: "https://offline.invalid/v1".into(),
                model: "synthetic".into(),
                persona: "synthetic".into(),
                allow_transmission: false,
            })
            .expect_err("revision space is exhausted");
        assert_eq!(error.public_code(), "counter_overflow");
        assert_eq!(app.snapshot().mode, "offline");
    }
}
