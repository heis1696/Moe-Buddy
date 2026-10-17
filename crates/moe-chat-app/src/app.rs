//! `ChatApplication`：受信文字聊天应用组合边界。
//!
//! 持有唯一的 [`UiManager`](moe_ui_core::UiManager) 与服务组合；宿主仅通过
//! `new/dispatch/snapshot` 交互。面板生命周期一律由 UI 内核管理，展示缓存
//! 只由真实视图回调写入，业务失败经类型化通道保留原始错误链上抛。

use std::cell::RefCell;
use std::rc::Rc;

use moe_ui_core::{
    CloseConfirmation, CloseRequest, PanelEntry, PanelHandle, PanelKind as UiPanelKind, PanelScope,
    RejectionReason, ReleaseCause, ScopeLevel, UiConfig, UiError, UiManager, UiPanel, Visibility,
};

use crate::contract::{
    Action, AppSnapshot, ChatSnapshot, MAX_TEXT_BYTES, PanelKind, PanelSnapshot, PanelVisibility,
};
use crate::error::AppError;
use crate::panels::{
    ChatController, ChatDisplay, ChatEvent, ChatView, CounterSlot, DiagnosticsController,
    DiagnosticsDisplay, DiagnosticsEvent, DiagnosticsView, FailureCell, MemoryController,
    MemoryDisplay, MemoryEvent, MemoryView, PanelArgs, PanelCounters, PresetsController,
    PresetsDisplay, PresetsEvent, PresetsView,
};
use crate::services::{COMPANION, ChatCore, REMEMBER_MAX_CHARS};

const PANEL_COUNT: usize = 4;
const INDEX_CHAT: usize = 0;
const INDEX_PRESETS: usize = 1;
const INDEX_MEMORY: usize = 2;
const INDEX_DIAGNOSTICS: usize = 3;

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
        self.core
            .borrow_mut()
            .enable_history(store, retention, now_ms)
    }

    /// Install trusted settings; a configured model never falls back to a fake reply.
    pub fn configure_model(&mut self, settings: &crate::ModelSettings) -> Result<(), AppError> {
        self.core.borrow_mut().configure_model(settings)?;
        if self.handles[INDEX_PRESETS]
            .as_ref()
            .and_then(|handle| self.ui.get(handle))
            .is_some_and(|summary| summary.visibility == Visibility::Visible)
        {
            self.deliver_presets(PresetsEvent::SettingsRefresh)?;
        }
        self.revision += 1;
        Ok(())
    }

    /// Consume the prepared request immediately after Send, on the UI thread.
    /// Move only this immutable input to the host worker, never the application.
    pub fn take_external_request(&mut self) -> Option<moe_agent_runtime::ExternalInput> {
        self.core.borrow_mut().external_input.take()
    }

    /// Submit the single worker result. Revoked or stale contexts cannot commit output.
    pub fn complete_external(
        &mut self,
        outcome: moe_agent_runtime::ModelOutcome,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let result = self.core.borrow_mut().complete_external(outcome, now_ms);
        let saved = self.core.borrow_mut().checkpoint_history(now_ms);
        let result = result.and(saved);
        self.revision += 1;
        if !self.core.borrow().is_revoked() {
            self.deliver_chat(ChatEvent::TransportRefresh)?;
        }
        result
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
        if process_generation == 0 {
            return Err(AppError::rejection(
                "invalid_process_generation",
                "进程代际必须为非零值",
                "process generation must be nonzero",
            ));
        }
        let core = Rc::new(RefCell::new(ChatCore::new(process_generation, memory)?));
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
                        ChatController::new(args.companion, core.clone(), failures.clone()),
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
                        PresetsController::new(args.companion, core.clone(), failures.clone()),
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
                        MemoryController::new(args.companion, core.clone(), failures.clone()),
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
                        DiagnosticsController::new(args.companion, core.clone(), failures.clone()),
                        DiagnosticsView::new(display.clone(), instance_counters),
                    ))
                },
            )?
        };

        Ok(Self {
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
            notice: None,
        })
    }

    /// 处理一条受控命令；`now_ms` 为宿主注入的单调毫秒时间，回退即拒绝。
    pub fn dispatch(&mut self, action: Action, now_ms: u64) -> Result<(), AppError> {
        let save_history = matches!(action, Action::Send);
        let result = self.dispatch_inner(action, now_ms);
        let result = if save_history && result.is_ok() {
            self.core.borrow_mut().checkpoint_history(now_ms)
        } else {
            result
        };
        if save_history && result.is_err() && !self.core.borrow().is_revoked() {
            self.deliver_chat(ChatEvent::TransportRefresh)?;
        }
        self.revision = match self.revision.checked_add(1) {
            Some(revision) => revision,
            None => {
                return Err(AppError::rejection(
                    "counter_overflow",
                    "快照修订号耗尽",
                    "snapshot revision space exhausted",
                ));
            }
        };
        result
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
        AppSnapshot {
            revision: self.revision,
            mode: if core.online { "online" } else { "offline" },
            companion: COMPANION,
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
            ));
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
            Action::SetDraft(text) => {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(too_large());
                }
                self.deliver_chat(ChatEvent::DraftChanged(text))
            }
            Action::Send => {
                let handle = self.handle_of(PanelKind::Chat)?;
                match self.ui.get(&handle) {
                    Some(summary) if summary.visibility == Visibility::Visible => {}
                    Some(_) => {
                        return Err(AppError::from_submit(RejectionReason::NotVisible));
                    }
                    None => {
                        return Err(AppError::rejection(
                            "panel_not_open",
                            "目标面板未打开",
                            "panel instance does not exist",
                        ));
                    }
                }
                if self.chat_display.borrow().draft.trim().is_empty() {
                    return Err(blank_input());
                }
                if let Err(rejected) = self.entries.chat.send(
                    &mut self.ui,
                    &handle,
                    ChatEvent::SendRequested { now_ms },
                ) {
                    return Err(AppError::from_submit(rejected.reason));
                }
                self.drive()
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

    fn open_panel(&mut self, kind: PanelKind) -> Result<(), AppError> {
        let args = PanelArgs {
            companion: COMPANION,
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
        let pending = match &self.pending_close {
            Some((pending_kind, _)) if *pending_kind == panel => self.pending_close.take(),
            _ => None,
        };
        let Some(confirmation) = pending else {
            return Err(AppError::rejection(
                "no_pending_confirmation",
                "该面板没有待确认的关闭请求",
                "no pending close confirmation for this panel",
            ));
        };
        if !discard {
            return Ok(());
        }
        match self.ui.confirm_close(&mut self.context, &confirmation.1) {
            Ok(()) => {
                self.handles[index_of(panel)] = None;
            }
            Err(UiError::ConfirmationStale) => {
                return Err(AppError::rejection(
                    "confirm_stale",
                    "关闭确认已失效：面板状态在其后发生变化，草稿未丢弃",
                    "close confirmation is stale; draft preserved",
                ));
            }
            Err(UiError::ConfirmationInstanceGone) => {
                return Err(AppError::rejection(
                    "panel_not_open",
                    "目标面板未打开",
                    "panel instance no longer exists",
                ));
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
        })
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

fn blank_input() -> AppError {
    AppError::rejection("empty_input", "输入为空白，已拒绝", "input is blank")
}

fn too_large() -> AppError {
    AppError::rejection(
        "input_too_large",
        "输入超过容量上限，已拒绝",
        "input exceeds the bounded capacity",
    )
}
