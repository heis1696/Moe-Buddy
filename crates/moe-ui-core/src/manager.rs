//! 调用中心:单宿主持有的 `UiManager`,负责注册、生命周期、焦点、
//! 有界驱动与统一释放。
//!
//! 中心身份由进程内原子分配的独占序号保证,不使用全局单例、
//! 系统时钟或调用方提供的整数;所有按句柄操作都复验中心归属。

use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::UiConfig;
use crate::controller::{CloseConfirmation, CloseDecision, UiController};
use crate::error::{PendingReleaseId, ReleaseCause, Stage, UiError};
use crate::identity::{
    InstanceSerial, PanelHandle, PanelKey, PanelKind, PanelScope, PanelSummary, ScopeLevel,
    Visibility,
};
use crate::panel::{NavQueue, Navigate, UiPanel};
use crate::registry::{
    CreateFault, ErasedKind, InputProgress, LivePanel, OpFault, PanelEntry, PanelKindRegistry,
    PendingRelease, PendingReleaseSummary, erased_registration, typed_registration,
};
use crate::view::UiView;

/// 中心独占序号分配器;1 起单调分配,耗尽即永久明确报错,不回绕复用。
static NEXT_CENTER: AtomicU64 = AtomicU64::new(1);

/// 首次打开的结果;重复打开复用或恢复,不重建、不重置、不抢焦点。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenOutcome {
    Created(PanelHandle),
    Restored(PanelHandle),
    Reused(PanelHandle),
}

impl OpenOutcome {
    pub fn handle(&self) -> PanelHandle {
        match *self {
            Self::Created(handle) | Self::Restored(handle) | Self::Reused(handle) => handle,
        }
    }
}

/// 关闭请求结果;确认未完成前不释放实例。
#[derive(Clone, Debug)]
pub enum CloseRequest {
    Released,
    ConfirmationPending(CloseConfirmation),
}

impl CloseRequest {
    pub fn is_released(&self) -> bool {
        matches!(self, Self::Released)
    }

    pub fn confirmation(&self) -> Option<&CloseConfirmation> {
        match self {
            Self::Released => None,
            Self::ConfirmationPending(confirmation) => Some(confirmation),
        }
    }
}

/// 加载票据:绑定中心、实例与签发时的最新加载序号。
///
/// 新加载、可见性改变、新入参修订、关闭/释放使旧票据失效;入队与实际分发前均复验。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LoadTicket<C, V> {
    pub(crate) center: u64,
    pub(crate) registration: usize,
    pub(crate) serial: InstanceSerial,
    pub(crate) epoch: u64,
    pub(crate) _marker: PhantomData<fn(&C, &V)>,
}

/// 提交被拒的原因;负载原样交还调用者,不静默丢失。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RejectionReason {
    /// 实例输入队列已满。
    QueueFull,
    /// 已有更新的加载、可见性改变或释放,票据失效。
    TicketStale,
    /// 实例当前隐藏,不接收展示输入/结果。
    NotVisible,
    /// 实例已不存在。
    InstanceGone,
    /// 入口/票据不属于当前中心。
    ForeignCenter,
    /// 入口与票据/句柄不属于同一注册。
    ForeignRegistration,
}

/// 被拒提交;携带原因与原始类型化负载。
///
/// `Debug` 只输出原因与负载类型,不打印事件正文,避免业务负载进入日志。
pub struct RejectedSubmit<E> {
    pub reason: RejectionReason,
    pub event: E,
}

impl<E> fmt::Debug for RejectedSubmit<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RejectedSubmit")
            .field("reason", &self.reason)
            .field("event_type", &std::any::type_name::<E>())
            .finish()
    }
}

impl<E> RejectedSubmit<E> {
    /// 取回原始负载。
    pub fn into_event(self) -> E {
        self.event
    }
}

/// 单面板在驱动中的失败:保留句柄、键、阶段与原始错误链。
#[derive(Debug)]
pub struct PanelFault {
    pub handle: PanelHandle,
    pub key: PanelKey,
    pub stage: Stage,
    source: Box<dyn Error + 'static>,
}

impl PanelFault {
    /// 原始错误,含 `Error::source` 链。
    pub fn source_error(&self) -> &(dyn Error + 'static) {
        self.source.as_ref()
    }
}

/// 导航命令执行失败:保留原命令与错误。
#[derive(Debug)]
pub struct NavigateFault {
    pub command: Navigate,
    pub error: UiError,
}

/// 一轮 `drive` 的有界报告;不含任何业务正文。
#[derive(Debug)]
pub struct DriveReport {
    inputs_delivered: usize,
    inputs_stale: usize,
    inputs_backpressure: usize,
    inputs_pending: usize,
    renders_attempted: usize,
    navigations_executed: usize,
    navigations_refused: usize,
    navigations_pending: usize,
    focus_cleared: bool,
    faults: Vec<PanelFault>,
    navigation_faults: Vec<NavigateFault>,
    confirmations: Vec<CloseConfirmation>,
}

impl DriveReport {
    fn new() -> Self {
        Self {
            inputs_delivered: 0,
            inputs_stale: 0,
            inputs_backpressure: 0,
            inputs_pending: 0,
            renders_attempted: 0,
            navigations_executed: 0,
            navigations_refused: 0,
            navigations_pending: 0,
            focus_cleared: false,
            faults: Vec::new(),
            navigation_faults: Vec::new(),
            confirmations: Vec::new(),
        }
    }

    /// 本轮成功交付给控制器的事件数。
    pub fn inputs_delivered(&self) -> usize {
        self.inputs_delivered
    }

    /// 本轮因票据失效而丢弃的输入数。
    pub fn inputs_stale(&self) -> usize {
        self.inputs_stale
    }

    /// 视图采集时因队满被拒的事件数。
    pub fn inputs_backpressure(&self) -> usize {
        self.inputs_backpressure
    }

    /// 本轮结束后仍排队的输入数,保留至后续轮。
    pub fn inputs_pending(&self) -> usize {
        self.inputs_pending
    }

    /// 本轮完成的渲染次数(仅可见实例)。
    pub fn renders_attempted(&self) -> usize {
        self.renders_attempted
    }

    /// 本轮执行的导航命令数。
    pub fn navigations_executed(&self) -> usize {
        self.navigations_executed
    }

    /// 本轮控制器回调因导航队满被拒的命令次数;原命令已经 `Err`
    /// 交还回调保留重交,报告只计数,不重复保存命令正文。
    pub fn navigations_refused(&self) -> usize {
        self.navigations_refused
    }

    /// 本轮结束后仍在队列中的导航命令数,保留至后续轮。
    pub fn navigations_pending(&self) -> usize {
        self.navigations_pending
    }

    /// 本轮是否清空了逻辑焦点(焦点面板被隐藏或关闭)。
    pub fn focus_cleared(&self) -> bool {
        self.focus_cleared
    }

    /// 单面板失败列表;阶段与错误链完整保留。
    pub fn faults(&self) -> &[PanelFault] {
        &self.faults
    }

    /// 导航执行失败列表。
    pub fn navigation_faults(&self) -> &[NavigateFault] {
        &self.navigation_faults
    }

    /// 导航触发的关闭确认,由宿主交互后显式继续。
    pub fn confirmations(&self) -> &[CloseConfirmation] {
        &self.confirmations
    }

    pub(crate) fn record_input_delivered(&mut self) {
        self.inputs_delivered = self.inputs_delivered.saturating_add(1);
    }

    pub(crate) fn record_stale_input(&mut self) {
        self.inputs_stale = self.inputs_stale.saturating_add(1);
    }

    pub(crate) fn add_input_backpressure(&mut self, refused: usize) {
        self.inputs_backpressure = self.inputs_backpressure.saturating_add(refused);
    }

    pub(crate) fn add_navigations_refused(&mut self, refused: usize) {
        self.navigations_refused = self.navigations_refused.saturating_add(refused);
    }

    pub(crate) fn record_render_ok(&mut self) {
        self.renders_attempted = self.renders_attempted.saturating_add(1);
    }

    pub(crate) fn push_fault(&mut self, handle: PanelHandle, key: PanelKey, fault: OpFault) {
        self.faults.push(PanelFault {
            handle,
            key,
            stage: fault.stage,
            source: fault.error,
        });
    }

    fn record_navigation_fault(&mut self, command: Navigate, error: UiError) {
        self.navigation_faults
            .push(NavigateFault { command, error });
    }
}

/// 强制释放作用域的逐项结果;长度受存活实例容量约束。
#[derive(Debug)]
pub struct ScopeReleaseReport {
    released: usize,
    failed: Vec<ScopeReleaseFault>,
}

impl ScopeReleaseReport {
    /// 成功释放的实例数。
    pub fn released(&self) -> usize {
        self.released
    }

    /// 释放未完成的实例;错误保留阶段、原始链与可重试记录编号。
    pub fn failed(&self) -> &[ScopeReleaseFault] {
        &self.failed
    }
}

/// 作用域强制释放中的单个失败项;句柄与具体错误一并保留。
#[derive(Debug)]
pub struct ScopeReleaseFault {
    pub handle: PanelHandle,
    pub error: UiError,
}

/// UI 管理与呈现框架的调用中心;由单宿主持有,不可克隆、无全局访问。
pub struct UiManager<Context: 'static> {
    center: u64,
    config: UiConfig,
    registrations: Vec<Box<dyn ErasedKind<Context>>>,
    navigation: NavQueue,
    focus: Option<(usize, InstanceSerial)>,
    next_serial: u64,
    input_cursor: Option<(usize, InstanceSerial)>,
}

impl<Context: 'static> UiManager<Context> {
    /// 创建中心;宿主显式提供全部非零容量与每轮预算。
    pub fn new(config: UiConfig) -> Result<Self, UiError> {
        let center = alloc_center()?;
        Ok(Self {
            center,
            config,
            registrations: Vec::new(),
            navigation: NavQueue::new(config.navigation_capacity()),
            focus: None,
            next_serial: 1,
            input_cursor: None,
        })
    }

    /// 中心的容量与预算配置。
    pub fn config(&self) -> &UiConfig {
        &self.config
    }

    /// 注册受信面板描述与构造工厂,返回类型化注册入口。
    ///
    /// 工厂每次首次打开都会收到中心构造的不可变 `PanelKey` 和类型化入参,
    /// 可据实际作用域绑定控制器归属。不扫描脚本、目录或自动装载插件;
    /// 同一种类重复注册被拒绝。
    pub fn register<C, V>(
        &mut self,
        kind: PanelKind,
        scope_level: ScopeLevel,
        factory: impl FnMut(&PanelKey, &C::OpenArgs) -> Result<UiPanel<C, V>, Box<dyn Error + 'static>>
        + 'static,
    ) -> Result<PanelEntry<C, V>, UiError>
    where
        C: UiController,
        V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
    {
        if self
            .registrations
            .iter()
            .any(|registration| registration.kind() == &kind)
        {
            return Err(UiError::KindAlreadyRegistered(kind));
        }
        let next = self
            .registrations
            .len()
            .checked_add(1)
            .ok_or(UiError::CapacityOverflow)?;
        if next > self.config.registration_capacity() {
            return Err(UiError::RegistrationCapacity);
        }
        let index = self.registrations.len();
        self.registrations
            .push(Box::new(PanelKindRegistry::<Context, C, V>::new(
                kind,
                scope_level,
                Box::new(factory),
            )));
        Ok(PanelEntry::new(self.center, index))
    }

    /// 按注册入口与作用域按需创建并显示。
    ///
    /// 已有可见实例时复用;已有隐藏实例时恢复并重走核验/刷新边界,
    /// 不清空草稿、不重新初始化、不抢焦点。新入参交给原控制器明确处理;
    /// 普通重复打开传 `None` 保持原状态。首次打开提供类型化入参,
    /// 并先过控制器 `refresh` 核验边界再挂载。
    pub fn open<C, V>(
        &mut self,
        context: &mut Context,
        entry: &PanelEntry<C, V>,
        scope: PanelScope,
        args: Option<&C::OpenArgs>,
    ) -> Result<OpenOutcome, UiError>
    where
        C: UiController,
        V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
    {
        if entry.center != self.center {
            return Err(UiError::ForeignEntry);
        }
        {
            let registration = erased_registration(&mut self.registrations, entry.registration)?;
            if !registration.scope_level().accepts(&scope) {
                return Err(UiError::ScopeLevelRejected {
                    kind: registration.kind().clone(),
                    scope,
                });
            }
        }
        let kind = self.registrations[entry.registration].kind().clone();
        let key = PanelKey::new(kind, scope);
        let existing = {
            let registration = erased_registration(&mut self.registrations, entry.registration)?;
            registration.find_by_key(&key)
        };
        if let Some(serial) = existing {
            let handle = PanelHandle::new(self.center, entry.registration, serial);
            let visible = {
                let registration =
                    erased_registration(&mut self.registrations, entry.registration)?;
                registration
                    .instance_mut(serial)
                    .map(|instance| instance.snapshot().visibility == Visibility::Visible)
                    .unwrap_or(false)
            };
            if visible {
                if let Some(args) = args {
                    self.revise_instance_typed::<C, V>(handle, &key, args)?;
                }
                return Ok(OpenOutcome::Reused(handle));
            }
            self.restore_instance::<C, V>(context, handle, &key, args)?;
            return Ok(OpenOutcome::Restored(handle));
        }
        let args = args.ok_or(UiError::MissingOpenArgs)?;
        self.check_instance_capacity()?;
        let serial = self.alloc_serial()?;
        let handle = PanelHandle::new(self.center, entry.registration, serial);
        let input_capacity = self.config.instance_input_capacity();
        let created = {
            let registration =
                typed_registration::<Context, C, V>(&mut self.registrations, entry.registration)?;
            registration.create_instance(handle, key.clone(), context, args, input_capacity)
        };
        match created {
            Ok(()) => Ok(OpenOutcome::Created(handle)),
            Err(CreateFault::Factory(source)) => Err(UiError::PanelFault {
                key,
                stage: Stage::Factory,
                source,
                initial: None,
            }),
            Err(CreateFault::Failed {
                stage,
                error,
                initial,
            }) => Err(UiError::PanelFault {
                key,
                stage,
                source: error,
                initial,
            }),
            Err(CreateFault::CleanupFailed {
                trigger,
                cleanup,
                panel,
            }) => {
                let record = self.store_failed_release(
                    entry.registration,
                    handle.serial,
                    panel,
                    None,
                    &cleanup,
                )?;
                Err(UiError::ReleaseIncomplete {
                    record,
                    key,
                    stage: cleanup.stage,
                    source: cleanup.error,
                    initial: Some(trigger),
                })
            }
        }
    }

    /// 显示已有实例;不存在时明确返回未找到,不暗中创建。
    pub fn show(&mut self, context: &mut Context, handle: &PanelHandle) -> Result<(), UiError> {
        self.own_handle(handle)?;
        let result = {
            let registration = self.registration_for_handle(handle)?;
            let instance = registration
                .instance_mut(handle.serial)
                .ok_or(UiError::InstanceNotFound)?;
            instance.set_visible(context, true)
        };
        result.map_err(|fault| self.lifecycle_fault_error(*handle, fault))
    }

    /// 隐藏已有实例;隐藏不等于销毁,允许保留的草稿由控制器持有,
    /// 已接纳的普通输入保留,隐藏期间不分发。隐藏当前焦点面板时
    /// 中心清除该焦点;返回是否清除了焦点。
    pub fn hide(&mut self, context: &mut Context, handle: &PanelHandle) -> Result<bool, UiError> {
        self.own_handle(handle)?;
        let result = {
            let registration = self.registration_for_handle(handle)?;
            let instance = registration
                .instance_mut(handle.serial)
                .ok_or(UiError::InstanceNotFound)?;
            instance.set_visible(context, false)
        };
        result.map_err(|fault| self.lifecycle_fault_error(*handle, fault))?;
        Ok(self.clear_focus_if(handle.registration, handle.serial))
    }

    /// 请求已有可见面板获得逻辑输入焦点;不暗中打开或显示,
    /// 不冒充操作系统已前置。
    pub fn focus(&mut self, handle: &PanelHandle) -> Result<(), UiError> {
        self.own_handle(handle)?;
        let registration = self.registration_for_handle(handle)?;
        let instance = registration
            .instance_mut(handle.serial)
            .ok_or(UiError::InstanceNotFound)?;
        if instance.snapshot().visibility != Visibility::Visible {
            return Err(UiError::NotVisible);
        }
        self.focus = Some((handle.registration, handle.serial));
        Ok(())
    }

    /// 当前逻辑焦点面板。
    pub fn focused(&self) -> Option<PanelHandle> {
        self.focus
            .map(|(registration, serial)| PanelHandle::new(self.center, registration, serial))
    }

    /// 请求允许关闭后释放实例。
    ///
    /// 控制器需要处理未保存草稿时返回确认凭据,由宿主交互并在
    /// [`Self::confirm_close`] 后显式继续;不能默认丢弃或自动保存。
    /// 实例仍有已接纳未处理的输入时明确拒绝,不静默丢弃;
    /// 关闭不是仅隐藏;释放完成后旧句柄失效。
    pub fn close(
        &mut self,
        context: &mut Context,
        handle: &PanelHandle,
    ) -> Result<CloseRequest, UiError> {
        self.own_handle(handle)?;
        let (decision, version) = {
            let registration = self.registration_for_handle(handle)?;
            let instance = registration
                .instance_mut(handle.serial)
                .ok_or(UiError::InstanceNotFound)?;
            if instance.pending_inputs() > 0 {
                return Err(UiError::CloseBlockedByPendingInputs);
            }
            let key = instance.key().clone();
            let decision = instance
                .begin_close()
                .map_err(|fault| UiError::PanelFault {
                    key,
                    stage: fault.stage,
                    source: fault.error,
                    initial: fault.initial,
                })?;
            (decision, instance.interaction_version())
        };
        match decision {
            CloseDecision::Allow => {
                self.release_instance(context, *handle, None)?;
                Ok(CloseRequest::Released)
            }
            CloseDecision::RequireConfirmation => {
                Ok(CloseRequest::ConfirmationPending(CloseConfirmation {
                    center: self.center,
                    registration: handle.registration,
                    serial: handle.serial,
                    interaction_version: version,
                }))
            }
        }
    }

    /// 在确认后继续关闭;执行前复验实例与签发时的交互版本。
    ///
    /// 任何可能改变控制器的回调(含返回 `Err` 的调用)都会先使旧确认
    /// 失效;实例仍有已接纳未处理的输入时明确拒绝。未确认/取消不释放。
    pub fn confirm_close(
        &mut self,
        context: &mut Context,
        confirmation: &CloseConfirmation,
    ) -> Result<(), UiError> {
        if confirmation.center != self.center {
            return Err(UiError::ForeignHandle);
        }
        let version = {
            let registration = self
                .registrations
                .get_mut(confirmation.registration)
                .ok_or(UiError::ConfirmationInstanceGone)?;
            let instance = registration
                .instance_mut(confirmation.serial)
                .ok_or(UiError::ConfirmationInstanceGone)?;
            if instance.pending_inputs() > 0 {
                return Err(UiError::CloseBlockedByPendingInputs);
            }
            instance.interaction_version()
        };
        if version != confirmation.interaction_version {
            return Err(UiError::ConfirmationStale);
        }
        let handle = PanelHandle::new(self.center, confirmation.registration, confirmation.serial);
        self.release_instance(context, handle, None)?;
        Ok(())
    }

    /// 宿主清理入口:绕过草稿确认强制失效并释放;仅用于宿主退出、
    /// 所属桌宠删除或受信上下文撤销,不伪装成业务撤销或事务回滚。
    ///
    /// 撤权流程会无条件调用控制器 `revoke` 清除敏感缓存,即使视图
    /// 卸载失败也不跳过;真实像素的紧急隐藏仍由宿主负责。
    pub fn force_release(
        &mut self,
        context: &mut Context,
        handle: &PanelHandle,
        cause: ReleaseCause,
    ) -> Result<(), UiError> {
        self.own_handle(handle)?;
        self.release_instance(context, *handle, Some(cause))?;
        Ok(())
    }

    /// 强制释放某作用域下的全部实例(如所属桌宠删除)。
    ///
    /// 返回有界逐项结果:单个实例失败保留具体错误链与可重试记录编号,
    /// 同时继续处理其余实例;也会处理该作用域先前留下的待释放记录并清除敏感缓存。
    pub fn force_release_scope(
        &mut self,
        context: &mut Context,
        scope: &PanelScope,
        cause: ReleaseCause,
    ) -> ScopeReleaseReport {
        let mut targets = Vec::new();
        for (index, registration) in self.registrations.iter().enumerate() {
            let mut summaries = Vec::new();
            registration.describe_all(index, self.focus, &mut summaries);
            for summary in summaries {
                if summary.key.scope() == scope {
                    targets.push(summary.handle);
                }
            }
        }
        let mut report = ScopeReleaseReport {
            released: 0,
            failed: Vec::new(),
        };
        let mut pending_targets = Vec::new();
        for registration in &self.registrations {
            registration.pending_targets(scope, &mut pending_targets);
        }
        for handle in targets {
            match self.release_instance(context, handle, Some(cause)) {
                Ok(_) => report.released = report.released.saturating_add(1),
                Err(error) => report.failed.push(ScopeReleaseFault { handle, error }),
            }
        }
        for (handle, record) in pending_targets {
            match self.force_pending_release(context, handle, record, cause) {
                Ok(()) => report.released = report.released.saturating_add(1),
                Err(error) => report.failed.push(ScopeReleaseFault { handle, error }),
            }
        }
        report
    }

    /// 当前保留的待重试释放记录;尚未成功释放的资源持续计入容量。
    pub fn pending_releases(&self) -> Vec<PendingReleaseSummary> {
        let mut out = Vec::new();
        for registration in &self.registrations {
            registration.pending_summaries_into(&mut out);
        }
        out
    }

    /// 重试一次未完成的释放;成功即归还容量额度,失败保留记录。
    pub fn retry_release(
        &mut self,
        context: &mut Context,
        record: PendingReleaseId,
    ) -> Result<(), UiError> {
        if record.center != self.center {
            return Err(UiError::PendingReleaseNotFound);
        }
        for registration in self.registrations.iter_mut() {
            if !registration.has_pending(record) {
                continue;
            }
            let Some(mut pending) = registration.take_pending(record) else {
                return Err(UiError::PendingReleaseNotFound);
            };
            let id = pending.id;
            let key = pending.key.clone();
            let cause = pending.cause;
            return match pending.panel.finish_release(context, cause) {
                Ok(()) => Ok(()),
                Err(fault) => {
                    pending.stage = fault.stage;
                    registration.store_pending(pending);
                    Err(UiError::ReleaseIncomplete {
                        record: id,
                        key,
                        stage: fault.stage,
                        source: fault.error,
                        initial: fault.initial,
                    })
                }
            };
        }
        Err(UiError::PendingReleaseNotFound)
    }

    fn force_pending_release(
        &mut self,
        context: &mut Context,
        handle: PanelHandle,
        record: PendingReleaseId,
        cause: ReleaseCause,
    ) -> Result<(), UiError> {
        let registration = self
            .registrations
            .get_mut(handle.registration)
            .ok_or(UiError::PendingReleaseNotFound)?;
        let mut pending = registration
            .take_pending(record)
            .ok_or(UiError::PendingReleaseNotFound)?;
        let key = pending.key.clone();
        pending.cause = Some(cause);
        match pending.panel.finish_release(context, Some(cause)) {
            Ok(()) => Ok(()),
            Err(fault) => {
                pending.stage = fault.stage;
                registration.store_pending(pending);
                Err(UiError::ReleaseIncomplete {
                    record,
                    key,
                    stage: fault.stage,
                    source: fault.error,
                    initial: fault.initial,
                })
            }
        }
    }

    /// 只读生命周期摘要;不含模型、入参、事件或凭据。
    pub fn get(&self, handle: &PanelHandle) -> Option<PanelSummary> {
        if handle.center != self.center {
            return None;
        }
        self.registrations.get(handle.registration)?.describe(
            handle.serial,
            handle.registration,
            self.focus,
        )
    }

    /// 是否存在句柄对应的存活实例;跨中心句柄返回 `false`。
    pub fn contains(&self, handle: &PanelHandle) -> bool {
        self.get(handle).is_some()
    }

    /// 全部存活实例的有界摘要列表,长度不超过存活实例容量。
    pub fn list(&self) -> Vec<PanelSummary> {
        let mut out = Vec::new();
        for (index, registration) in self.registrations.iter().enumerate() {
            registration.describe_all(index, self.focus, &mut out);
        }
        out
    }

    /// 为可见实例开始一次新的业务加载;返回绑定最新加载序号的票据。
    pub fn begin_load<C, V>(
        &mut self,
        entry: &PanelEntry<C, V>,
        handle: &PanelHandle,
    ) -> Result<LoadTicket<C, V>, UiError>
    where
        C: UiController,
        V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
    {
        if entry.center != self.center {
            return Err(UiError::ForeignEntry);
        }
        self.own_handle(handle)?;
        if handle.registration != entry.registration {
            return Err(UiError::ForeignEntry);
        }
        let registration = self.registration_for_handle(handle)?;
        let instance = registration
            .instance_mut(handle.serial)
            .ok_or(UiError::InstanceNotFound)?;
        if instance.snapshot().visibility != Visibility::Visible {
            return Err(UiError::NotVisible);
        }
        let epoch = instance.begin_load()?;
        Ok(LoadTicket {
            center: self.center,
            registration: entry.registration,
            serial: handle.serial,
            epoch,
            _marker: PhantomData,
        })
    }

    /// 串行驱动一轮:先交付既有输入/结果,更新模型;随后全部可见面板
    /// 完成渲染与采集;再在剩余事件预算内统一处理新采集事件;
    /// 最后在导航预算内执行导航。
    ///
    /// 回调不能递归调用中心;剩余输入与导航保留至后续轮,不循环驱动到队空。
    pub fn drive(&mut self, context: &mut Context) -> DriveReport {
        let mut report = DriveReport::new();
        let mut event_budget = self.config.events_per_round();
        self.drive_inputs_fair(&mut event_budget, &mut report);
        for registration in self.registrations.iter_mut() {
            registration.render_all(context, &mut report);
        }
        self.drive_inputs_fair(&mut event_budget, &mut report);
        let mut navigation_budget = self.config.navigations_per_round();
        while navigation_budget > 0 {
            let Some(command) = self.navigation.pop_front() else {
                break;
            };
            navigation_budget -= 1;
            self.execute_navigation(command, context, &mut report);
        }
        report.navigations_pending = self.navigation.len();
        let mut pending = 0usize;
        for registration in &self.registrations {
            pending = pending.saturating_add(registration.pending_input_total());
        }
        report.inputs_pending = pending;
        report
    }

    fn drive_inputs_fair(&mut self, event_budget: &mut usize, report: &mut DriveReport) {
        if *event_budget == 0 {
            return;
        }
        let mut summaries = Vec::new();
        for (registration_index, registration) in self.registrations.iter().enumerate() {
            registration.describe_all(registration_index, self.focus, &mut summaries);
        }
        let targets: Vec<_> = summaries
            .into_iter()
            .map(|summary| (summary.handle.registration, summary.handle.serial))
            .collect();
        if targets.is_empty() {
            return;
        }
        let mut start = self
            .input_cursor
            .and_then(|cursor| targets.iter().position(|target| *target > cursor))
            .unwrap_or(0);
        let mut blocked = vec![false; targets.len()];
        while *event_budget > 0 {
            let mut progressed = false;
            let pass_start = start;
            for offset in 0..targets.len() {
                let index = (pass_start + offset) % targets.len();
                if blocked[index] {
                    continue;
                }
                let target = targets[index];
                let progress = self.registrations[target.0]
                    .instance_mut(target.1)
                    .map(|instance| instance.drive_input_one(report, &mut self.navigation))
                    .unwrap_or(InputProgress::Idle);
                match progress {
                    InputProgress::Idle => {}
                    InputProgress::Blocked => blocked[index] = true,
                    InputProgress::Consumed => {
                        *event_budget -= 1;
                        self.input_cursor = Some(target);
                        start = (index + 1) % targets.len();
                        progressed = true;
                        if *event_budget == 0 {
                            break;
                        }
                    }
                }
            }
            if !progressed {
                break;
            }
            if let Some(cursor) = self.input_cursor {
                start = targets
                    .iter()
                    .position(|target| *target > cursor)
                    .unwrap_or(0);
            }
        }
    }

    /// 把一条受控导航命令重新入队;队满时原命令交还,不静默丢失。
    pub fn requeue_navigation(&mut self, command: Navigate) -> Result<(), UiError> {
        if self.navigation.push(command) {
            return Ok(());
        }
        Err(UiError::NavigationQueueFull(command))
    }

    /// 向可见实例发送一个普通类型化输入;队满或隐藏时原负载交还。
    pub fn send<C, V>(
        &mut self,
        entry: &PanelEntry<C, V>,
        handle: &PanelHandle,
        event: C::Event,
    ) -> Result<(), RejectedSubmit<C::Event>>
    where
        C: UiController,
        V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
    {
        if entry.center != self.center || handle.center != self.center {
            return Err(RejectedSubmit {
                reason: RejectionReason::ForeignCenter,
                event,
            });
        }
        if let Err(UiError::ForeignHandle) = self.own_handle(handle) {
            return Err(RejectedSubmit {
                reason: RejectionReason::ForeignCenter,
                event,
            });
        }
        if handle.registration != entry.registration {
            return Err(RejectedSubmit {
                reason: RejectionReason::ForeignRegistration,
                event,
            });
        }
        let input_capacity = self.config.instance_input_capacity();
        let instance = match self.typed_instance::<C, V>(entry.registration, handle.serial) {
            Ok(instance) => instance,
            Err(_) => {
                return Err(RejectedSubmit {
                    reason: RejectionReason::ForeignRegistration,
                    event,
                });
            }
        };
        if let Some(instance) = instance {
            if instance.snapshot().visibility != Visibility::Visible {
                return Err(RejectedSubmit {
                    reason: RejectionReason::NotVisible,
                    event,
                });
            }
            if instance.pending_inputs() >= input_capacity {
                return Err(RejectedSubmit {
                    reason: RejectionReason::QueueFull,
                    event,
                });
            }
            instance.enqueue_plain(event);
            return Ok(());
        }
        Err(RejectedSubmit {
            reason: RejectionReason::InstanceGone,
            event,
        })
    }

    /// 交回加载结果:入队前复验票据的中心、注册、实例与加载序号;
    /// 实际分发前由驱动再次复验。队满、票据失效或隐藏时原负载交还。
    pub fn submit<C, V>(
        &mut self,
        entry: &PanelEntry<C, V>,
        ticket: &LoadTicket<C, V>,
        event: C::Event,
    ) -> Result<(), RejectedSubmit<C::Event>>
    where
        C: UiController,
        V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
    {
        if entry.center != self.center || ticket.center != self.center {
            return Err(RejectedSubmit {
                reason: RejectionReason::ForeignCenter,
                event,
            });
        }
        if entry.registration != ticket.registration {
            return Err(RejectedSubmit {
                reason: RejectionReason::ForeignRegistration,
                event,
            });
        }
        let input_capacity = self.config.instance_input_capacity();
        let instance = match self.typed_instance::<C, V>(entry.registration, ticket.serial) {
            Ok(instance) => instance,
            Err(_) => {
                return Err(RejectedSubmit {
                    reason: RejectionReason::ForeignRegistration,
                    event,
                });
            }
        };
        let Some(instance) = instance else {
            return Err(RejectedSubmit {
                reason: RejectionReason::InstanceGone,
                event,
            });
        };
        if instance.snapshot().visibility != Visibility::Visible {
            return Err(RejectedSubmit {
                reason: RejectionReason::NotVisible,
                event,
            });
        }
        if !instance.ticket_is_current(ticket.epoch) {
            return Err(RejectedSubmit {
                reason: RejectionReason::TicketStale,
                event,
            });
        }
        if instance.pending_inputs() >= input_capacity {
            return Err(RejectedSubmit {
                reason: RejectionReason::QueueFull,
                event,
            });
        }
        instance.enqueue_ticketed(event, ticket.epoch);
        Ok(())
    }

    // ---- 内部实现 ----

    fn own_handle(&self, handle: &PanelHandle) -> Result<(), UiError> {
        if handle.center != self.center {
            Err(UiError::ForeignHandle)
        } else {
            Ok(())
        }
    }

    fn registration_for_handle(
        &mut self,
        handle: &PanelHandle,
    ) -> Result<&mut dyn ErasedKind<Context>, UiError> {
        erased_registration(&mut self.registrations, handle.registration)
            .map_err(|_| UiError::InstanceNotFound)
    }

    fn alloc_serial(&mut self) -> Result<InstanceSerial, UiError> {
        let raw = self.next_serial;
        let serial = InstanceSerial::new(raw)?;
        self.next_serial = raw.checked_add(1).ok_or(UiError::IdentifierExhausted)?;
        Ok(serial)
    }

    fn check_instance_capacity(&self) -> Result<(), UiError> {
        let live = self
            .registrations
            .iter()
            .map(|registration| registration.live_count())
            .try_fold(0usize, usize::checked_add)
            .ok_or(UiError::CapacityOverflow)?;
        let pending = self
            .registrations
            .iter()
            .map(|registration| registration.pending_count())
            .try_fold(0usize, usize::checked_add)
            .ok_or(UiError::CapacityOverflow)?;
        let total = checked_instance_total(live, pending, 1)?;
        if total > self.config.live_instance_capacity() {
            return Err(UiError::InstanceCapacity);
        }
        Ok(())
    }

    fn clear_focus_if(&mut self, registration: usize, serial: InstanceSerial) -> bool {
        if self.focus == Some((registration, serial)) {
            self.focus = None;
            true
        } else {
            false
        }
    }

    fn restore_instance<C, V>(
        &mut self,
        context: &mut Context,
        handle: PanelHandle,
        key: &PanelKey,
        args: Option<&C::OpenArgs>,
    ) -> Result<(), UiError>
    where
        C: UiController,
        V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
    {
        self.show(context, &handle)?;
        if let Some(args) = args {
            self.revise_instance_typed::<C, V>(handle, key, args)?;
        }
        Ok(())
    }

    fn revise_instance_typed<C, V>(
        &mut self,
        handle: PanelHandle,
        key: &PanelKey,
        args: &C::OpenArgs,
    ) -> Result<(), UiError>
    where
        C: UiController,
        V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
    {
        let outcome = {
            let registration =
                typed_registration::<Context, C, V>(&mut self.registrations, handle.registration)?;
            let instance = registration
                .instance_mut(handle.serial)
                .ok_or(UiError::InstanceNotFound)?;
            let live = instance
                .as_any_mut()
                .downcast_mut::<LivePanel<C, V>>()
                .ok_or(UiError::ForeignEntry)?;
            live.revise(args, &mut self.navigation)
        };
        outcome.map_err(|fault| UiError::PanelFault {
            key: key.clone(),
            stage: fault.stage,
            source: fault.error,
            initial: fault.initial,
        })
    }

    /// 定位类型化实例;`Ok(None)` 表示实例不存在,`Err` 表示注册入口错误。
    fn typed_instance<C, V>(
        &mut self,
        registration_index: usize,
        serial: InstanceSerial,
    ) -> Result<Option<&mut LivePanel<C, V>>, UiError>
    where
        C: UiController,
        V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
    {
        let registration =
            typed_registration::<Context, C, V>(&mut self.registrations, registration_index)?;
        let Some(instance) = registration.instance_mut(serial) else {
            return Ok(None);
        };
        let live = instance
            .as_any_mut()
            .downcast_mut::<LivePanel<C, V>>()
            .ok_or(UiError::ForeignEntry)?;
        Ok(Some(live))
    }

    /// 统一释放:失效票据,卸载并释放;实例离开存活集合时立即清焦点,
    /// 失败保留可重试记录,记录编号复用实例序号,不因编号耗尽丢资源。
    fn release_instance(
        &mut self,
        context: &mut Context,
        handle: PanelHandle,
        cause: Option<ReleaseCause>,
    ) -> Result<bool, UiError> {
        let registration = self.registration_for_handle(&handle)?;
        let instance = registration
            .instance_mut(handle.serial)
            .ok_or(UiError::InstanceNotFound)?;
        let key = instance.key().clone();
        instance.invalidate().map_err(|error| UiError::PanelFault {
            key: key.clone(),
            stage: Stage::Invalidate,
            source: Box::new(error),
            initial: None,
        })?;
        let Some(mut panel) = registration.take_instance(handle.serial) else {
            return Err(UiError::InstanceNotFound);
        };
        let focus_cleared = self.clear_focus_if(handle.registration, handle.serial);
        match panel.finish_release(context, cause) {
            Ok(()) => Ok(focus_cleared),
            Err(fault) => {
                let record = self.store_failed_release(
                    handle.registration,
                    handle.serial,
                    panel,
                    cause,
                    &fault,
                )?;
                Err(UiError::ReleaseIncomplete {
                    record,
                    key,
                    stage: fault.stage,
                    source: fault.error,
                    initial: fault.initial,
                })
            }
        }
    }

    fn lifecycle_fault_error(
        &mut self,
        handle: PanelHandle,
        fault: crate::registry::OpFault,
    ) -> UiError {
        let Some((key, requires_quarantine)) = self
            .registrations
            .get_mut(handle.registration)
            .and_then(|registration| registration.instance_mut(handle.serial))
            .map(|instance| (instance.key().clone(), instance.requires_quarantine()))
        else {
            return UiError::InstanceNotFound;
        };
        if !requires_quarantine {
            return UiError::PanelFault {
                key,
                stage: fault.stage,
                source: fault.error,
                initial: fault.initial,
            };
        }
        let Some(panel) = self.registrations[handle.registration].take_instance(handle.serial)
        else {
            return UiError::InstanceNotFound;
        };
        self.clear_focus_if(handle.registration, handle.serial);
        match self.store_failed_release(handle.registration, handle.serial, panel, None, &fault) {
            Ok(record) => UiError::ReleaseIncomplete {
                record,
                key,
                stage: fault.stage,
                source: fault.error,
                initial: fault.initial,
            },
            Err(error) => error,
        }
    }

    fn store_failed_release(
        &mut self,
        registration_index: usize,
        serial: InstanceSerial,
        panel: Box<dyn crate::registry::ErasedPanel<Context>>,
        cause: Option<ReleaseCause>,
        fault: &OpFault,
    ) -> Result<PendingReleaseId, UiError> {
        let id = PendingReleaseId::from_serial(self.center, serial);
        let Some(registration) = self.registrations.get_mut(registration_index) else {
            return Err(UiError::InstanceNotFound);
        };
        registration.store_pending(PendingRelease {
            id,
            key: panel.key().clone(),
            cause,
            stage: fault.stage,
            panel,
        });
        Ok(id)
    }

    fn execute_navigation(
        &mut self,
        command: Navigate,
        context: &mut Context,
        report: &mut DriveReport,
    ) {
        let focused_before = self.focus;
        match command {
            Navigate::Close(handle) => match self.close_via_navigation(context, handle) {
                Ok((request, focus_cleared)) => {
                    report.navigations_executed = report.navigations_executed.saturating_add(1);
                    match request {
                        CloseRequest::Released => {
                            if focus_cleared {
                                report.focus_cleared = true;
                            }
                        }
                        CloseRequest::ConfirmationPending(confirmation) => {
                            report.confirmations.push(confirmation);
                        }
                    }
                }
                Err(error) => report.record_navigation_fault(command, error),
            },
            Navigate::Hide(handle) => match self.hide(context, &handle) {
                Ok(cleared) => {
                    report.navigations_executed = report.navigations_executed.saturating_add(1);
                    if cleared {
                        report.focus_cleared = true;
                    }
                }
                Err(error) => report.record_navigation_fault(command, error),
            },
            Navigate::Show(handle) => match self.show(context, &handle) {
                Ok(()) => {
                    report.navigations_executed = report.navigations_executed.saturating_add(1)
                }
                Err(error) => report.record_navigation_fault(command, error),
            },
            Navigate::Focus(handle) => match self.focus(&handle) {
                Ok(()) => {
                    report.navigations_executed = report.navigations_executed.saturating_add(1)
                }
                Err(error) => report.record_navigation_fault(command, error),
            },
        }
        if focused_before.is_some() && self.focus.is_none() {
            report.focus_cleared = true;
        }
    }

    fn close_via_navigation(
        &mut self,
        context: &mut Context,
        handle: PanelHandle,
    ) -> Result<(CloseRequest, bool), UiError> {
        self.own_handle(&handle)?;
        let (decision, version) = {
            let registration = self.registration_for_handle(&handle)?;
            let instance = registration
                .instance_mut(handle.serial)
                .ok_or(UiError::InstanceNotFound)?;
            if instance.pending_inputs() > 0 {
                return Err(UiError::CloseBlockedByPendingInputs);
            }
            let key = instance.key().clone();
            let decision = instance
                .begin_close()
                .map_err(|fault| UiError::PanelFault {
                    key,
                    stage: fault.stage,
                    source: fault.error,
                    initial: fault.initial,
                })?;
            (decision, instance.interaction_version())
        };
        match decision {
            CloseDecision::Allow => {
                let focus_cleared = self.release_instance(context, handle, None)?;
                Ok((CloseRequest::Released, focus_cleared))
            }
            CloseDecision::RequireConfirmation => Ok((
                CloseRequest::ConfirmationPending(CloseConfirmation {
                    center: self.center,
                    registration: handle.registration,
                    serial: handle.serial,
                    interaction_version: version,
                }),
                false,
            )),
        }
    }
}

/// 中心独占序号:进程内单调分配,耗尽即永久报错,不回绕复用。
fn alloc_center() -> Result<u64, UiError> {
    allocate_counter(&NEXT_CENTER)
}

fn allocate_counter(counter: &AtomicU64) -> Result<u64, UiError> {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map_err(|_| UiError::IdentifierExhausted)
}

fn checked_instance_total(
    live: usize,
    pending: usize,
    additional: usize,
) -> Result<usize, UiError> {
    live.checked_add(pending)
        .and_then(|total| total.checked_add(additional))
        .ok_or(UiError::CapacityOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UiConfig;

    fn config() -> UiConfig {
        UiConfig::new(8, 8, 8, 8, 8, 8).unwrap()
    }

    #[test]
    fn centers_receive_distinct_positive_ids() {
        let first: UiManager<()> = UiManager::new(config()).unwrap();
        let second: UiManager<()> = UiManager::new(config()).unwrap();
        assert!(first.center >= 1);
        assert!(second.center >= 1);
        assert_ne!(first.center, second.center);
        let third: UiManager<()> = UiManager::new(config()).unwrap();
        assert_ne!(third.center, first.center);
        assert_ne!(third.center, second.center);
    }

    #[test]
    fn center_counter_exhaustion_is_reported_without_wrapping() {
        let counter = AtomicU64::new(u64::MAX);
        assert!(matches!(
            allocate_counter(&counter),
            Err(UiError::IdentifierExhausted)
        ));
        assert_eq!(counter.load(Ordering::Acquire), u64::MAX);
    }

    #[test]
    fn instance_counter_exhaustion_is_reported_without_reuse() {
        let mut manager: UiManager<()> = UiManager::new(config()).unwrap();
        manager.next_serial = u64::MAX;
        assert!(matches!(
            manager.alloc_serial(),
            Err(UiError::IdentifierExhausted)
        ));
        assert_eq!(manager.next_serial, u64::MAX);
    }

    #[test]
    fn instance_capacity_counter_overflow_is_reported() {
        assert!(matches!(
            checked_instance_total(usize::MAX, 0, 1),
            Err(UiError::CapacityOverflow)
        ));
    }
}
