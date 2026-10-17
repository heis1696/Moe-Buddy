//! 聊天、预设、记忆与诊断四类面板的类型化控制器/事件/视图。
//!
//! 控制器是唯一的面板业务入口：用户意图以类型化事件经 `UiManager`
//! 分发到控制器，控制器调用受信服务组合并更新展示模型；视图只在
//! 真实 `mount/render/unmount` 回调中写入展示缓存与计数。应用层不得
//! 绕过控制器直接改写面板状态。

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::fmt;
use std::rc::Rc;

use moe_ui_core::{CloseDecision, EventSink, NavSink, UiController, UiView};

use crate::contract::{
    ChatMessage, ChatStatus, DiagnosticSnapshot, MAX_TEXT_BYTES, MemorySnapshot, PresetSnapshot,
};
use crate::error::AppError;
use crate::services::{ChatCore, REMEMBER_MAX_CHARS};

/// 面板控制器/视图错误；原始错误链保留在 `Error::source` 中。
#[derive(Debug)]
pub struct PanelError {
    stage: &'static str,
    source: Box<dyn Error + 'static>,
}

impl PanelError {
    pub(crate) fn rejected(stage: &'static str, detail: &'static str) -> Self {
        Self {
            stage,
            source: Box::new(crate::error::Rejection::new(detail)),
        }
    }
}

impl fmt::Display for PanelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "panel callback failed at stage {}", self.stage)
    }
}

impl Error for PanelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// 面板实例的真实回调计数；由视图在 `mount/render/unmount` 内维护。
#[derive(Debug, Default)]
pub struct PanelCounters {
    mounts: Cell<u64>,
    renders: Cell<u64>,
    unmounts: Cell<u64>,
}

impl PanelCounters {
    /// 累计的真实挂载/渲染/卸载回调次数。
    pub fn get(&self) -> (u64, u64, u64) {
        (self.mounts.get(), self.renders.get(), self.unmounts.get())
    }

    fn record_mount(&self) {
        self.mounts.set(self.mounts.get().wrapping_add(1));
    }

    fn record_render(&self) {
        self.renders.set(self.renders.get().wrapping_add(1));
    }

    fn record_unmount(&self) {
        self.unmounts.set(self.unmounts.get().wrapping_add(1));
    }
}

/// 当前/最近一个实例的计数槽位；工厂创建实例时登记。
pub(crate) type CounterSlot = Rc<RefCell<Option<Rc<PanelCounters>>>>;

/// 控制器把业务失败交回受信应用的通道；保留原始 [`AppError`] 错误链。
pub(crate) type FailureCell = Rc<RefCell<Option<AppError>>>;

fn report_failure(cell: &FailureCell, error: AppError) {
    *cell.borrow_mut() = Some(error);
}

/// 面板打开入参；控制器据此固定归属，普通修订不重置草稿。
#[derive(Clone, Copy, Debug)]
pub struct PanelArgs {
    pub companion: &'static str,
}

/// 聊天面板事件。
#[derive(Debug)]
pub enum ChatEvent {
    /// Refresh the view after a trusted asynchronous result.
    TransportRefresh,
    /// 更新聊天草稿。
    DraftChanged(String),
    /// 以当前草稿发起一轮可信发送。
    SendRequested { now_ms: u64 },
}

/// 聊天展示模型；草稿隐藏时由控制器保留。
#[derive(Clone)]
pub struct ChatModel {
    pub draft: String,
    pub messages: Vec<ChatMessage>,
    pub status: ChatStatus,
    pub failure: Option<String>,
}

impl ChatModel {
    fn cleared() -> Self {
        Self {
            draft: String::new(),
            messages: Vec::new(),
            status: ChatStatus::Ready,
            failure: None,
        }
    }
}

/// 聊天展示缓存；只在真实渲染回调中更新，卸载即清理。
#[derive(Clone)]
pub struct ChatDisplay {
    pub draft: String,
    pub messages: Vec<ChatMessage>,
    pub status: ChatStatus,
    pub failure: Option<String>,
}

impl ChatDisplay {
    pub(crate) fn cleared() -> Self {
        Self {
            draft: String::new(),
            messages: Vec::new(),
            status: ChatStatus::Ready,
            failure: None,
        }
    }
}

/// 聊天面板控制器：草稿编辑与发送都必须经此处进入可信链路。
pub struct ChatController {
    companion: &'static str,
    core: Rc<RefCell<ChatCore>>,
    failures: FailureCell,
    model: ChatModel,
    pending_draft: Option<String>,
}

impl ChatController {
    /// 控制器在工厂期固定的稳定归属；只读展示用途。
    pub fn companion(&self) -> &'static str {
        self.companion
    }

    pub(crate) fn new(
        companion: &'static str,
        core: Rc<RefCell<ChatCore>>,
        failures: FailureCell,
    ) -> Self {
        Self {
            companion,
            core,
            failures,
            model: ChatModel::cleared(),
            pending_draft: None,
        }
    }

    fn handle(&mut self, event: ChatEvent) -> Result<(), PanelError> {
        match event {
            ChatEvent::TransportRefresh => self.refresh(),
            ChatEvent::DraftChanged(text) => {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(PanelError::rejected("chat-draft", "input_too_large"));
                }
                self.model.draft = text;
                self.pending_draft = None;
                Ok(())
            }
            ChatEvent::SendRequested { now_ms } => {
                let draft = std::mem::take(&mut self.model.draft);
                if draft.trim().is_empty() {
                    self.model.draft = draft;
                    return Err(PanelError::rejected("chat-send", "empty_input"));
                }
                self.model.status = ChatStatus::Sending;
                let outcome = self.core.borrow_mut().send(&draft, now_ms);
                match outcome {
                    Ok(_outcome) => {
                        self.model.messages = self
                            .core
                            .borrow()
                            .chat_history()
                            .map_err(|_| PanelError::rejected("chat-refresh", "revoked"))?
                            .messages;
                        let history = self
                            .core
                            .borrow()
                            .chat_history()
                            .map_err(|_| PanelError::rejected("chat-refresh", "revoked"))?;
                        self.model.status = history.status;
                        self.model.failure = history.failure;
                        if self.model.status == ChatStatus::Sending {
                            self.pending_draft = Some(draft);
                        }
                        Ok(())
                    }
                    Err(error) => {
                        self.model.draft = draft;
                        let history = self
                            .core
                            .borrow()
                            .chat_history()
                            .map_err(|_| PanelError::rejected("chat-refresh", "revoked"))?;
                        self.model.messages = history.messages;
                        self.model.status = if error.public_code() == "send_unknown" {
                            ChatStatus::Unknown
                        } else {
                            ChatStatus::Failed
                        };
                        self.model.failure = Some(error.public_code().to_owned());
                        report_failure(&self.failures, error);
                        Ok(())
                    }
                }
            }
        }
    }
}

impl UiController for ChatController {
    type OpenArgs = PanelArgs;
    type Model = ChatModel;
    type Event = ChatEvent;
    type Error = PanelError;

    fn model(&self) -> &ChatModel {
        &self.model
    }

    fn apply(&mut self, event: ChatEvent, _navigation: &mut NavSink<'_>) -> Result<(), PanelError> {
        self.handle(event)
    }

    fn revise(
        &mut self,
        args: &PanelArgs,
        _navigation: &mut NavSink<'_>,
    ) -> Result<(), PanelError> {
        self.companion = args.companion;
        Ok(())
    }

    fn refresh(&mut self) -> Result<(), PanelError> {
        let history = self
            .core
            .borrow()
            .chat_history()
            .map_err(|_| PanelError::rejected("chat-refresh", "revoked"))?;
        self.model.messages = history.messages;
        self.model.status = history.status;
        self.model.failure = history.failure;
        if self.model.status != ChatStatus::Sending
            && let Some(draft) = self.pending_draft.take()
            && self.model.status == ChatStatus::Failed
            && self.model.draft.is_empty()
        {
            self.model.draft = draft;
        }
        Ok(())
    }

    fn suspend(&mut self) -> Result<(), PanelError> {
        Ok(())
    }

    fn close_request(&mut self) -> CloseDecision {
        if self.model.draft.is_empty() {
            CloseDecision::Allow
        } else {
            CloseDecision::RequireConfirmation
        }
    }

    fn revoke(&mut self) {
        self.model = ChatModel::cleared();
        self.pending_draft = None;
    }

    fn release(&mut self) -> Result<(), PanelError> {
        Ok(())
    }
}

/// 聊天面板视图：渲染写入展示缓存并累计真实回调计数。
pub struct ChatView {
    cache: Rc<RefCell<ChatDisplay>>,
    counters: Rc<PanelCounters>,
}

impl ChatView {
    pub(crate) fn new(cache: Rc<RefCell<ChatDisplay>>, counters: Rc<PanelCounters>) -> Self {
        Self { cache, counters }
    }
}

impl UiView for ChatView {
    type Model = ChatModel;
    type Event = ChatEvent;
    type Context = ();
    type Error = PanelError;

    fn mount(&mut self, _context: &mut ()) -> Result<(), PanelError> {
        self.counters.record_mount();
        Ok(())
    }

    fn render(
        &mut self,
        model: &ChatModel,
        _context: &mut (),
        _events: &mut EventSink<'_, ChatEvent>,
    ) -> Result<(), PanelError> {
        self.counters.record_render();
        *self.cache.borrow_mut() = ChatDisplay {
            draft: model.draft.clone(),
            messages: model.messages.clone(),
            status: model.status,
            failure: model.failure.clone(),
        };
        Ok(())
    }

    fn unmount(&mut self, _context: &mut ()) -> Result<(), PanelError> {
        self.counters.record_unmount();
        *self.cache.borrow_mut() = ChatDisplay::cleared();
        Ok(())
    }
}

/// 预设面板事件。
#[derive(Debug)]
pub enum PresetsEvent {
    /// Refresh after trusted settings replacement.
    SettingsRefresh,
    /// 绑定已存在的受控离线聊天预设。
    SelectRequested { id: String, now_ms: u64 },
}

/// 预设展示模型。
#[derive(Clone)]
pub struct PresetsModel {
    pub catalog: Vec<PresetSnapshot>,
    pub active: String,
    pub failure: Option<String>,
}

impl PresetsModel {
    fn cleared() -> Self {
        Self {
            catalog: Vec::new(),
            active: String::new(),
            failure: None,
        }
    }
}

/// 预设展示缓存。
#[derive(Clone, Default)]
pub struct PresetsDisplay {
    pub catalog: Vec<PresetSnapshot>,
    pub active: String,
}

/// 预设面板控制器：切换走真实 `PresetRegistry` 逐用途重绑。
pub struct PresetsController {
    companion: &'static str,
    core: Rc<RefCell<ChatCore>>,
    failures: FailureCell,
    model: PresetsModel,
}

impl PresetsController {
    /// 控制器在工厂期固定的稳定归属；只读展示用途。
    pub fn companion(&self) -> &'static str {
        self.companion
    }

    pub(crate) fn new(
        companion: &'static str,
        core: Rc<RefCell<ChatCore>>,
        failures: FailureCell,
    ) -> Self {
        Self {
            companion,
            core,
            failures,
            model: PresetsModel::cleared(),
        }
    }
}

impl UiController for PresetsController {
    type OpenArgs = PanelArgs;
    type Model = PresetsModel;
    type Event = PresetsEvent;
    type Error = PanelError;

    fn model(&self) -> &PresetsModel {
        &self.model
    }

    fn apply(
        &mut self,
        event: PresetsEvent,
        _navigation: &mut NavSink<'_>,
    ) -> Result<(), PanelError> {
        let PresetsEvent::SelectRequested { id, now_ms } = event else {
            return self.refresh();
        };
        let outcome = self.core.borrow_mut().select_preset(&id, now_ms);
        match outcome {
            Ok(snapshot) => {
                self.model.catalog = self.core.borrow().preset_catalog();
                self.model.active = snapshot.id;
                self.model.failure = None;
            }
            Err(error) => {
                self.model.failure = Some(error.public_code().to_owned());
                report_failure(&self.failures, error);
            }
        }
        Ok(())
    }

    fn revise(
        &mut self,
        args: &PanelArgs,
        _navigation: &mut NavSink<'_>,
    ) -> Result<(), PanelError> {
        self.companion = args.companion;
        Ok(())
    }

    fn refresh(&mut self) -> Result<(), PanelError> {
        self.model.catalog = self.core.borrow().preset_catalog();
        self.model.active = self.core.borrow().active_preset_id();
        Ok(())
    }

    fn suspend(&mut self) -> Result<(), PanelError> {
        Ok(())
    }

    fn close_request(&mut self) -> CloseDecision {
        CloseDecision::Allow
    }

    fn revoke(&mut self) {
        self.model = PresetsModel::cleared();
    }

    fn release(&mut self) -> Result<(), PanelError> {
        Ok(())
    }
}

/// 预设面板视图。
pub struct PresetsView {
    cache: Rc<RefCell<PresetsDisplay>>,
    counters: Rc<PanelCounters>,
}

impl PresetsView {
    pub(crate) fn new(cache: Rc<RefCell<PresetsDisplay>>, counters: Rc<PanelCounters>) -> Self {
        Self { cache, counters }
    }
}

impl UiView for PresetsView {
    type Model = PresetsModel;
    type Event = PresetsEvent;
    type Context = ();
    type Error = PanelError;

    fn mount(&mut self, _context: &mut ()) -> Result<(), PanelError> {
        self.counters.record_mount();
        Ok(())
    }

    fn render(
        &mut self,
        model: &PresetsModel,
        _context: &mut (),
        _events: &mut EventSink<'_, PresetsEvent>,
    ) -> Result<(), PanelError> {
        self.counters.record_render();
        *self.cache.borrow_mut() = PresetsDisplay {
            catalog: model.catalog.clone(),
            active: model.active.clone(),
        };
        Ok(())
    }

    fn unmount(&mut self, _context: &mut ()) -> Result<(), PanelError> {
        self.counters.record_unmount();
        *self.cache.borrow_mut() = PresetsDisplay::default();
        Ok(())
    }
}

/// 记忆面板事件。
#[derive(Debug)]
pub enum MemoryEvent {
    SearchRequested {
        text: String,
        now_ms: u64,
    },
    ChangeRequested {
        id: String,
        revision: u64,
        text: Option<String>,
        now_ms: u64,
    },
    /// 明确写入本伙伴的私密记忆。
    RememberRequested {
        text: String,
        now_ms: u64,
    },
    /// 经权限复验刷新获准记忆列表。
    RefreshRequested {
        now_ms: u64,
    },
}

/// 记忆展示模型；只保存授权检索返回的有界列表。
#[derive(Clone, Default)]
pub struct MemoryModel {
    pub entries: Vec<MemorySnapshot>,
    pub failure: Option<String>,
    pub query: String,
}

/// 记忆展示缓存；隐藏/关闭即清空，快照仅在可见面板返回条目。
#[derive(Clone, Default)]
pub struct MemoryDisplay {
    pub entries: Vec<MemorySnapshot>,
}

/// 记忆面板控制器。
pub struct MemoryController {
    companion: &'static str,
    core: Rc<RefCell<ChatCore>>,
    failures: FailureCell,
    model: MemoryModel,
}

impl MemoryController {
    fn accept_result(&mut self, result: Result<Vec<MemorySnapshot>, AppError>) {
        match result {
            Ok(entries) => {
                self.model.entries = entries;
                self.model.failure = None;
            }
            Err(error) => {
                self.model.failure = Some(error.public_code().to_owned());
                report_failure(&self.failures, error);
            }
        }
    }
    /// 控制器在工厂期固定的稳定归属；只读展示用途。
    pub fn companion(&self) -> &'static str {
        self.companion
    }

    pub(crate) fn new(
        companion: &'static str,
        core: Rc<RefCell<ChatCore>>,
        failures: FailureCell,
    ) -> Self {
        Self {
            companion,
            core,
            failures,
            model: MemoryModel::default(),
        }
    }
}

impl UiController for MemoryController {
    type OpenArgs = PanelArgs;
    type Model = MemoryModel;
    type Event = MemoryEvent;
    type Error = PanelError;

    fn model(&self) -> &MemoryModel {
        &self.model
    }

    fn apply(
        &mut self,
        event: MemoryEvent,
        _navigation: &mut NavSink<'_>,
    ) -> Result<(), PanelError> {
        match event {
            MemoryEvent::RememberRequested { text, now_ms } => {
                if text.trim().is_empty() {
                    return Err(PanelError::rejected("memory-remember", "empty_input"));
                }
                if text.len() > MAX_TEXT_BYTES || text.chars().count() > REMEMBER_MAX_CHARS {
                    return Err(PanelError::rejected("memory-remember", "input_too_large"));
                }
                let mut core = self.core.borrow_mut();
                let outcome = match core.remember(&text, now_ms) {
                    Ok(_) => core.search_memories(&self.model.query, now_ms),
                    Err(error) => Err(error),
                };
                drop(core);
                match outcome {
                    Ok(entries) => {
                        self.model.entries = entries;
                        self.model.failure = None;
                    }
                    Err(error) => {
                        self.model.failure = Some(error.public_code().to_owned());
                        report_failure(&self.failures, error);
                    }
                }
                Ok(())
            }
            MemoryEvent::RefreshRequested { now_ms } => {
                let outcome = self
                    .core
                    .borrow_mut()
                    .search_memories(&self.model.query, now_ms);
                match outcome {
                    Ok(entries) => {
                        self.model.entries = entries;
                        self.model.failure = None;
                    }
                    Err(error) => {
                        self.model.failure = Some(error.public_code().to_owned());
                        report_failure(&self.failures, error);
                    }
                }
                Ok(())
            }
            MemoryEvent::SearchRequested { text, now_ms } => {
                let result = self.core.borrow_mut().search_memories(&text, now_ms);
                if result.is_ok() {
                    self.model.query = text;
                }
                self.accept_result(result);
                Ok(())
            }
            MemoryEvent::ChangeRequested {
                id,
                revision,
                text,
                now_ms,
            } => {
                let result = {
                    let mut core = self.core.borrow_mut();
                    core.change_memory(&id, revision, text.as_deref(), now_ms)
                        .and_then(|()| core.search_memories(&self.model.query, now_ms))
                };
                self.accept_result(result);
                Ok(())
            }
        }
    }

    fn revise(
        &mut self,
        args: &PanelArgs,
        _navigation: &mut NavSink<'_>,
    ) -> Result<(), PanelError> {
        self.companion = args.companion;
        Ok(())
    }

    fn refresh(&mut self) -> Result<(), PanelError> {
        Ok(())
    }

    fn suspend(&mut self) -> Result<(), PanelError> {
        Ok(())
    }

    fn close_request(&mut self) -> CloseDecision {
        CloseDecision::Allow
    }

    fn revoke(&mut self) {
        self.model = MemoryModel::default();
    }

    fn release(&mut self) -> Result<(), PanelError> {
        Ok(())
    }
}

/// 记忆面板视图。
pub struct MemoryView {
    cache: Rc<RefCell<MemoryDisplay>>,
    counters: Rc<PanelCounters>,
}

impl MemoryView {
    pub(crate) fn new(cache: Rc<RefCell<MemoryDisplay>>, counters: Rc<PanelCounters>) -> Self {
        Self { cache, counters }
    }
}

impl UiView for MemoryView {
    type Model = MemoryModel;
    type Event = MemoryEvent;
    type Context = ();
    type Error = PanelError;

    fn mount(&mut self, _context: &mut ()) -> Result<(), PanelError> {
        self.counters.record_mount();
        Ok(())
    }

    fn render(
        &mut self,
        model: &MemoryModel,
        _context: &mut (),
        _events: &mut EventSink<'_, MemoryEvent>,
    ) -> Result<(), PanelError> {
        self.counters.record_render();
        *self.cache.borrow_mut() = MemoryDisplay {
            entries: model.entries.clone(),
        };
        Ok(())
    }

    fn unmount(&mut self, _context: &mut ()) -> Result<(), PanelError> {
        self.counters.record_unmount();
        *self.cache.borrow_mut() = MemoryDisplay::default();
        Ok(())
    }
}

/// 诊断面板事件。
#[derive(Debug)]
pub enum DiagnosticsEvent {
    /// 经独立查看许可刷新受控日志。
    RefreshRequested { now_ms: u64 },
}

/// 诊断展示模型；条目为白名单字段投影，`gap` 表示历史缺口。
#[derive(Clone, Default)]
pub struct DiagnosticsModel {
    pub entries: Vec<DiagnosticSnapshot>,
    pub gap: bool,
    pub failure: Option<String>,
}

/// 诊断展示缓存；隐藏/关闭即清空。
#[derive(Clone, Default)]
pub struct DiagnosticsDisplay {
    pub entries: Vec<DiagnosticSnapshot>,
    pub gap: bool,
}

/// 诊断面板控制器。
pub struct DiagnosticsController {
    companion: &'static str,
    core: Rc<RefCell<ChatCore>>,
    failures: FailureCell,
    model: DiagnosticsModel,
}

impl DiagnosticsController {
    /// 控制器在工厂期固定的稳定归属；只读展示用途。
    pub fn companion(&self) -> &'static str {
        self.companion
    }

    pub(crate) fn new(
        companion: &'static str,
        core: Rc<RefCell<ChatCore>>,
        failures: FailureCell,
    ) -> Self {
        Self {
            companion,
            core,
            failures,
            model: DiagnosticsModel::default(),
        }
    }
}

impl UiController for DiagnosticsController {
    type OpenArgs = PanelArgs;
    type Model = DiagnosticsModel;
    type Event = DiagnosticsEvent;
    type Error = PanelError;

    fn model(&self) -> &DiagnosticsModel {
        &self.model
    }

    fn apply(
        &mut self,
        event: DiagnosticsEvent,
        _navigation: &mut NavSink<'_>,
    ) -> Result<(), PanelError> {
        let DiagnosticsEvent::RefreshRequested { now_ms } = event;
        let outcome = self.core.borrow_mut().query_diagnostics(now_ms);
        match outcome {
            Ok(page) => {
                self.model.entries = page.entries;
                self.model.gap = page.gap_possible;
                self.model.failure = None;
            }
            Err(error) => {
                self.model.failure = Some(error.public_code().to_owned());
                report_failure(&self.failures, error);
            }
        }
        Ok(())
    }

    fn revise(
        &mut self,
        args: &PanelArgs,
        _navigation: &mut NavSink<'_>,
    ) -> Result<(), PanelError> {
        self.companion = args.companion;
        Ok(())
    }

    fn refresh(&mut self) -> Result<(), PanelError> {
        Ok(())
    }

    fn suspend(&mut self) -> Result<(), PanelError> {
        Ok(())
    }

    fn close_request(&mut self) -> CloseDecision {
        CloseDecision::Allow
    }

    fn revoke(&mut self) {
        self.model = DiagnosticsModel::default();
    }

    fn release(&mut self) -> Result<(), PanelError> {
        Ok(())
    }
}

/// 诊断面板视图。
pub struct DiagnosticsView {
    cache: Rc<RefCell<DiagnosticsDisplay>>,
    counters: Rc<PanelCounters>,
}

impl DiagnosticsView {
    pub(crate) fn new(cache: Rc<RefCell<DiagnosticsDisplay>>, counters: Rc<PanelCounters>) -> Self {
        Self { cache, counters }
    }
}

impl UiView for DiagnosticsView {
    type Model = DiagnosticsModel;
    type Event = DiagnosticsEvent;
    type Context = ();
    type Error = PanelError;

    fn mount(&mut self, _context: &mut ()) -> Result<(), PanelError> {
        self.counters.record_mount();
        Ok(())
    }

    fn render(
        &mut self,
        model: &DiagnosticsModel,
        _context: &mut (),
        _events: &mut EventSink<'_, DiagnosticsEvent>,
    ) -> Result<(), PanelError> {
        self.counters.record_render();
        *self.cache.borrow_mut() = DiagnosticsDisplay {
            entries: model.entries.clone(),
            gap: model.gap,
        };
        Ok(())
    }

    fn unmount(&mut self, _context: &mut ()) -> Result<(), PanelError> {
        self.counters.record_unmount();
        *self.cache.borrow_mut() = DiagnosticsDisplay::default();
        Ok(())
    }
}
