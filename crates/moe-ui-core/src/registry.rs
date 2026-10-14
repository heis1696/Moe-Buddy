//! 类型化注册、内部安全类型擦除与存活实例。
//!
//! 公共 API 不暴露 `Any`、反射或可变面板引用;`as_any_mut` 仅在
//! crate 内部把已验证匹配的类型化组合安全降级回具体面板时使用。

use std::any::Any;
use std::collections::VecDeque;
use std::error::Error;

use crate::controller::{CloseDecision, UiController};
use crate::error::{AggregateError, PendingReleaseId, ReleaseCause, Stage, UiError};
use crate::identity::{
    InstanceSerial, PanelHandle, PanelKey, PanelKind, PanelSummary, ScopeLevel, Visibility,
};
use crate::panel::{NavQueue, NavSink, UiPanel};
use crate::view::{EventSink, PendingInput, UiView};

/// 注册受信面板的构造工厂;每次首次打开调用一次。
pub type PanelFactory<C, V> = Box<
    dyn FnMut(
            &PanelKey,
            &<C as UiController>::OpenArgs,
        ) -> Result<UiPanel<C, V>, Box<dyn Error + 'static>>
        + 'static,
>;

/// `register` 返回的类型化注册入口;与 `OpenArgs/Event` 绑定。
///
/// 只是一个已验证的编号,外部不能伪造;跨中心、错误入口使用均被拒绝。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PanelEntry<C, V> {
    pub(crate) center: u64,
    pub(crate) registration: usize,
    pub(crate) _marker: std::marker::PhantomData<fn(&C, &V)>,
}

impl<C, V> std::fmt::Debug for PanelEntry<C, V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PanelEntry")
            .field("center", &self.center)
            .field("registration", &self.registration)
            .finish()
    }
}

impl<C, V> PanelEntry<C, V> {
    pub(crate) fn new(center: u64, registration: usize) -> Self {
        Self {
            center,
            registration,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<C, V> PanelEntry<C, V>
where
    C: UiController,
    V: UiView<Model = C::Model, Event = C::Event>,
{
    /// 向可见实例发送一个普通类型化输入;队满或隐藏时原负载交还。
    pub fn send<Context: 'static>(
        &self,
        manager: &mut crate::manager::UiManager<Context>,
        handle: &PanelHandle,
        event: C::Event,
    ) -> Result<(), crate::manager::RejectedSubmit<C::Event>>
    where
        V: UiView<Context = Context>,
    {
        manager.send(self, handle, event)
    }

    /// 交回加载结果;入队与实际分发前均复验票据。
    pub fn submit<Context: 'static>(
        &self,
        manager: &mut crate::manager::UiManager<Context>,
        ticket: &crate::manager::LoadTicket<C, V>,
        event: C::Event,
    ) -> Result<(), crate::manager::RejectedSubmit<C::Event>>
    where
        V: UiView<Context = Context>,
    {
        manager.submit(self, ticket, event)
    }

    /// 为可见实例开始一次新的业务加载,取得绑定最新加载序号的票据。
    pub fn begin_load<Context: 'static>(
        &self,
        manager: &mut crate::manager::UiManager<Context>,
        handle: &PanelHandle,
    ) -> Result<crate::manager::LoadTicket<C, V>, UiError>
    where
        V: UiView<Context = Context>,
    {
        manager.begin_load(self, handle)
    }
}

/// 内部操作失败:阶段、原始错误链与同操作中先发生的初始错误。
pub(crate) struct OpFault {
    pub(crate) stage: Stage,
    pub(crate) error: Box<dyn Error + 'static>,
    pub(crate) initial: Option<Box<dyn Error + 'static>>,
}

pub(crate) enum InputProgress {
    Idle,
    Blocked,
    Consumed,
}

impl OpFault {
    pub(crate) fn new(stage: Stage, error: Box<dyn Error + 'static>) -> Self {
        Self {
            stage,
            error,
            initial: None,
        }
    }

    pub(crate) fn with_initial(mut self, initial: Box<dyn Error + 'static>) -> Self {
        self.initial = Some(initial);
        self
    }
}

fn next_interaction_version(current: u64) -> Result<u64, UiError> {
    current.checked_add(1).ok_or(UiError::CapacityOverflow)
}

fn next_load_epoch(current: u64, exhausted: &mut bool) -> Result<u64, UiError> {
    if *exhausted {
        return Err(UiError::CapacityOverflow);
    }
    match current.checked_add(1) {
        Some(epoch) => Ok(epoch),
        None => {
            *exhausted = true;
            Err(UiError::CapacityOverflow)
        }
    }
}

fn expire_load_epoch(current: &mut u64, exhausted: &mut bool) {
    if *exhausted {
        return;
    }
    match current.checked_add(1) {
        Some(epoch) => *current = epoch,
        None => *exhausted = true,
    }
}

fn ticket_epoch_matches(current: u64, exhausted: bool, ticket: u64) -> bool {
    !exhausted && current == ticket
}

/// 首次打开失败的清理结局;区分已清理干净与清理亦失败须转入可重试记录。
pub(crate) enum FirstOpenCleanup {
    /// 面板资源已清理干净;保留核验/挂载/卸载的原始失败。
    Cleaned(OpFault),
    /// 清理亦失败;面板须由调用者转入可重试记录。
    CleanupFailed {
        trigger: Box<dyn Error + 'static>,
        cleanup: OpFault,
    },
}

/// 内部实例快照。
pub(crate) struct PanelSnapshot {
    pub(crate) visibility: Visibility,
    pub(crate) pending_inputs: usize,
}

/// 一个存活实例:类型化面板加生命周期、票据与队列状态。
pub(crate) struct LivePanel<C, V>
where
    C: UiController,
    V: UiView<Model = C::Model, Event = C::Event>,
{
    handle: PanelHandle,
    key: PanelKey,
    panel: UiPanel<C, V>,
    input_capacity: usize,
    visibility: Visibility,
    mounted: bool,
    /// 卸载曾失败且资源可能未释放完整;释放重试时仍须重试卸载。
    unmount_pending: bool,
    controller_released: bool,
    revoked: bool,
    loads_exhausted: bool,
    load_epoch: u64,
    interaction_version: u64,
    inputs: VecDeque<PendingInput<C::Event>>,
}

impl<C, V> LivePanel<C, V>
where
    C: UiController,
    V: UiView<Model = C::Model, Event = C::Event>,
{
    pub(crate) fn new(
        handle: PanelHandle,
        key: PanelKey,
        panel: UiPanel<C, V>,
        input_capacity: usize,
    ) -> Self {
        Self {
            handle,
            key,
            panel,
            input_capacity,
            visibility: Visibility::Hidden,
            mounted: false,
            unmount_pending: false,
            controller_released: false,
            revoked: false,
            loads_exhausted: false,
            load_epoch: 1,
            interaction_version: 1,
            inputs: VecDeque::new(),
        }
    }

    /// 首次打开:先控制器 refresh 核验,再挂载视图并标记可见;
    /// 失败时做完整清理,清理亦失败把面板交回调用者转入可重试记录。
    pub(crate) fn mount_new(&mut self, context: &mut V::Context) -> Result<(), FirstOpenCleanup> {
        if let Err(error) = self.panel.controller_mut().refresh() {
            return Err(self.cleanup_failed_first_open(
                context,
                false,
                OpFault::new(Stage::Refresh, Box::new(error)),
            ));
        }
        let mounted = {
            let (_controller, view) = self.panel.parts_mut();
            view.mount(context)
        };
        if let Err(error) = mounted {
            return Err(self.cleanup_failed_first_open(
                context,
                true,
                OpFault::new(Stage::Mount, Box::new(error)),
            ));
        }
        self.mounted = true;
        self.visibility = Visibility::Visible;
        Ok(())
    }

    /// 首次打开失败后的完整清理:挂载尝试过则先卸载(部分创建的资源
    /// 也要释放),再释放控制器;清理中的新失败与原始失败一并保留。
    fn cleanup_failed_first_open(
        &mut self,
        context: &mut V::Context,
        mount_attempted: bool,
        trigger: OpFault,
    ) -> FirstOpenCleanup {
        let unmount_error = if mount_attempted {
            match self.unmount_view(context) {
                Ok(()) => None,
                Err(error) => {
                    self.unmount_pending = true;
                    Some(error)
                }
            }
        } else {
            None
        };
        self.mounted = false;
        let released = self.panel.controller_mut().release();
        if released.is_ok() {
            self.controller_released = true;
        }
        match (unmount_error, released) {
            (None, Ok(())) => FirstOpenCleanup::Cleaned(trigger),
            (Some(error), Ok(())) => FirstOpenCleanup::CleanupFailed {
                trigger: trigger.error,
                cleanup: OpFault::new(Stage::Unmount, error),
            },
            (unmount, Err(error)) => {
                let chained: Box<dyn Error + 'static> = match unmount {
                    Some(unmount) => Box::new(AggregateError::new(unmount, Box::new(error))),
                    None => Box::new(error),
                };
                FirstOpenCleanup::CleanupFailed {
                    trigger: trigger.error,
                    cleanup: OpFault::new(Stage::ControllerRelease, chained),
                }
            }
        }
    }

    /// 显示/恢复:先失效旧关闭确认,再控制器 refresh 核验,成功后挂载视图;
    /// 挂载失败仍尝试卸载部分创建的资源,卸载失败不吞;实例保持隐藏可重试。
    fn show(&mut self, context: &mut V::Context) -> Result<(), OpFault> {
        if self.visibility == Visibility::Visible {
            return Ok(());
        }
        self.bump_interaction_version(Stage::Refresh)?;
        if let Err(error) = self.panel.controller_mut().refresh() {
            return Err(OpFault::new(Stage::Refresh, Box::new(error)));
        }
        self.bump_load_epoch(Stage::Invalidate)?;
        let mounted = {
            let (_controller, view) = self.panel.parts_mut();
            view.mount(context)
        };
        if let Err(error) = mounted {
            return Err(match self.unmount_view(context) {
                Ok(()) => OpFault::new(Stage::Mount, Box::new(error)),
                Err(unmount) => {
                    self.unmount_pending = true;
                    self.visibility = Visibility::Hidden;
                    OpFault::new(Stage::Unmount, unmount).with_initial(Box::new(error))
                }
            });
        }
        self.mounted = true;
        self.visibility = Visibility::Visible;
        Ok(())
    }

    /// 隐藏:暂停订阅、失效在途加载并卸载视图;已接纳的普通输入保留,
    /// 隐藏期间不分发,恢复后继续交付;草稿由控制器保留。
    fn hide(&mut self, context: &mut V::Context) -> Result<(), OpFault> {
        if self.visibility == Visibility::Hidden {
            return Ok(());
        }
        self.bump_interaction_version(Stage::Suspend)?;
        if let Err(error) = self.panel.controller_mut().suspend() {
            return Err(OpFault::new(Stage::Suspend, Box::new(error)));
        }
        if let Err(error) = self.invalidate_loads() {
            return Err(OpFault::new(Stage::Invalidate, Box::new(error)));
        }
        if let Err(error) = self.unmount_view(context) {
            self.unmount_pending = true;
            self.mounted = false;
            self.visibility = Visibility::Hidden;
            return Err(OpFault::new(Stage::Unmount, error));
        }
        self.mounted = false;
        self.visibility = Visibility::Hidden;
        Ok(())
    }

    fn unmount_view(&mut self, context: &mut V::Context) -> Result<(), Box<dyn Error + 'static>> {
        let (_controller, view) = self.panel.parts_mut();
        view.unmount(context).map_err(|error| Box::new(error) as _)
    }

    /// 失效在途展示加载;已接纳的普通输入不受影响,票据化结果在
    /// 分发前按加载序号复验失效。序号耗尽明确报错,不回绕复用票据。
    fn invalidate_loads(&mut self) -> Result<(), UiError> {
        self.load_epoch = next_load_epoch(self.load_epoch, &mut self.loads_exhausted)?;
        Ok(())
    }

    fn expire_loads(&mut self) {
        expire_load_epoch(&mut self.load_epoch, &mut self.loads_exhausted);
    }

    pub(crate) fn pending_inputs(&self) -> usize {
        self.inputs.len()
    }

    pub(crate) fn snapshot(&self) -> PanelSnapshot {
        PanelSnapshot {
            visibility: self.visibility,
            pending_inputs: self.inputs.len(),
        }
    }

    pub(crate) fn ticket_is_current(&self, epoch: u64) -> bool {
        ticket_epoch_matches(self.load_epoch, self.loads_exhausted, epoch)
    }

    pub(crate) fn enqueue_plain(&mut self, event: C::Event) {
        self.inputs.push_back(PendingInput {
            event,
            load_epoch: None,
        });
    }

    pub(crate) fn enqueue_ticketed(&mut self, event: C::Event, epoch: u64) {
        self.inputs.push_back(PendingInput {
            event,
            load_epoch: Some(epoch),
        });
    }

    /// 交互版本前进;任何可能改变控制器的回调(含返回 `Err` 的调用)
    /// 都在调用前执行,使旧关闭确认失效。
    fn bump_interaction_version(&mut self, stage: Stage) -> Result<(), OpFault> {
        self.interaction_version = next_interaction_version(self.interaction_version)
            .map_err(|error| OpFault::new(stage, Box::new(error)))?;
        Ok(())
    }

    fn bump_load_epoch(&mut self, stage: Stage) -> Result<(), OpFault> {
        self.load_epoch = next_load_epoch(self.load_epoch, &mut self.loads_exhausted)
            .map_err(|error| OpFault::new(stage, Box::new(error)))?;
        Ok(())
    }

    /// 已有实例收到新入参;调用前先失效旧关闭确认,新入参可能改变控制器。
    pub(crate) fn revise(
        &mut self,
        args: &C::OpenArgs,
        navigation: &mut NavQueue,
    ) -> Result<(), OpFault> {
        self.bump_interaction_version(Stage::ReviseArgs)?;
        self.invalidate_loads()
            .map_err(|error| OpFault::new(Stage::Invalidate, Box::new(error)))?;
        let handle = self.handle;
        let mut refused = 0usize;
        let mut sink = NavSink::bind(navigation, handle, &mut refused);
        let revised = self.panel.controller_mut().revise(args, &mut sink);
        if let Err(error) = revised {
            return Err(OpFault::new(Stage::ReviseArgs, Box::new(error)));
        }
        Ok(())
    }

    /// 分发既有输入:隐藏期间不分发且不丢弃,恢复后继续;票据化结果
    /// 分发前复验加载序号;调用控制器前先使旧关闭确认失效。
    fn drive_input_one(
        &mut self,
        report: &mut crate::manager::DriveReport,
        navigation: &mut NavQueue,
    ) -> InputProgress {
        if self.visibility != Visibility::Visible || self.unmount_pending {
            return InputProgress::Idle;
        }
        let Some(ticketed) = self.inputs.front().map(|pending| pending.load_epoch) else {
            return InputProgress::Idle;
        };
        if let Some(epoch) = ticketed
            && !self.ticket_is_current(epoch)
        {
            self.inputs.pop_front();
            report.record_stale_input();
            return InputProgress::Consumed;
        }
        if let Err(fault) = self.bump_interaction_version(Stage::ApplyEvent) {
            report.push_fault(self.handle, self.key.clone(), fault);
            return InputProgress::Blocked;
        }
        let Some(pending) = self.inputs.pop_front() else {
            return InputProgress::Idle;
        };
        let mut refused = 0usize;
        let outcome = {
            let handle = self.handle;
            let mut sink = NavSink::bind(navigation, handle, &mut refused);
            let applied = self.panel.controller_mut().apply(pending.event, &mut sink);
            applied.map_err(|error| OpFault::new(Stage::ApplyEvent, Box::new(error)))
        };
        report.add_navigations_refused(refused);
        match outcome {
            Ok(()) => report.record_input_delivered(),
            Err(fault) => report.push_fault(self.handle, self.key.clone(), fault),
        }
        InputProgress::Consumed
    }

    /// 呈现当前模型并采集新事件;采集只入队,本轮在全部面板渲染完成后
    /// 统一分发,队满计数背压。
    fn render_collect(
        &mut self,
        context: &mut V::Context,
        report: &mut crate::manager::DriveReport,
    ) {
        if self.visibility != Visibility::Visible || !self.mounted || self.unmount_pending {
            return;
        }
        let mut refused = 0usize;
        let outcome = {
            let (controller, view) = self.panel.parts_mut();
            let mut sink = EventSink::new(&mut self.inputs, self.input_capacity, &mut refused);
            view.render(controller.model(), context, &mut sink)
        };
        match outcome {
            Ok(()) => report.record_render_ok(),
            Err(error) => report.push_fault(
                self.handle,
                self.key.clone(),
                OpFault::new(Stage::Render, Box::new(error)),
            ),
        }
        report.add_input_backpressure(refused);
    }

    /// 关闭/释放收尾:强制释放先清除待处理负载与敏感缓存,再卸载视图并释放控制器。
    /// 普通释放卸载失败即保留重试;强制释放即使卸载失败也不跳过
    /// 敏感缓存清除与控制器释放,两个错误按序保留。
    fn finish_release_impl(
        &mut self,
        context: &mut V::Context,
        cause: Option<ReleaseCause>,
    ) -> Result<(), OpFault> {
        if cause.is_some() {
            self.inputs.clear();
            if !self.revoked {
                self.panel.controller_mut().revoke();
                self.revoked = true;
            }
        }
        let mut unmount_error = None;
        if self.mounted || self.unmount_pending {
            match self.unmount_view(context) {
                Ok(()) => {
                    self.unmount_pending = false;
                    self.mounted = false;
                }
                Err(error) => {
                    self.unmount_pending = true;
                    self.mounted = false;
                    self.visibility = Visibility::Hidden;
                    if cause.is_none() {
                        return Err(OpFault::new(Stage::Unmount, error));
                    }
                    unmount_error = Some(error);
                }
            }
        }
        let release_error = if self.controller_released {
            None
        } else {
            match self.panel.controller_mut().release() {
                Ok(()) => {
                    self.controller_released = true;
                    None
                }
                Err(error) => Some(Box::new(error) as Box<dyn Error + 'static>),
            }
        };
        match (unmount_error, release_error) {
            (None, None) => Ok(()),
            (Some(error), None) => Err(OpFault::new(Stage::Unmount, error)),
            (unmount, Some(error)) => {
                let chained: Box<dyn Error + 'static> = match unmount {
                    Some(unmount) => Box::new(AggregateError::new(unmount, error)),
                    None => error,
                };
                Err(OpFault::new(Stage::ControllerRelease, chained))
            }
        }
    }
}

impl<Context: 'static, C, V> ErasedPanel<Context> for LivePanel<C, V>
where
    C: UiController,
    V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
{
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn handle(&self) -> PanelHandle {
        self.handle
    }

    fn key(&self) -> &PanelKey {
        &self.key
    }

    fn snapshot(&self) -> PanelSnapshot {
        LivePanel::snapshot(self)
    }

    fn pending_inputs(&self) -> usize {
        LivePanel::pending_inputs(self)
    }

    fn interaction_version(&self) -> u64 {
        self.interaction_version
    }

    fn invalidate(&mut self) -> Result<(), UiError> {
        self.expire_loads();
        Ok(())
    }

    fn begin_load(&mut self) -> Result<u64, UiError> {
        let epoch = next_load_epoch(self.load_epoch, &mut self.loads_exhausted)?;
        self.load_epoch = epoch;
        Ok(epoch)
    }

    fn begin_close(&mut self) -> Result<CloseDecision, OpFault> {
        self.bump_interaction_version(Stage::Invalidate)?;
        Ok(self.panel.controller_mut().close_request())
    }

    fn set_visible(&mut self, context: &mut Context, visible: bool) -> Result<(), OpFault> {
        if visible {
            self.show(context)
        } else {
            self.hide(context)
        }
    }

    fn drive_input_one(
        &mut self,
        report: &mut crate::manager::DriveReport,
        navigation: &mut NavQueue,
    ) -> InputProgress {
        LivePanel::drive_input_one(self, report, navigation)
    }

    fn requires_quarantine(&self) -> bool {
        self.unmount_pending
    }

    fn render_collect(&mut self, context: &mut Context, report: &mut crate::manager::DriveReport) {
        LivePanel::render_collect(self, context, report);
    }

    fn finish_release(
        &mut self,
        context: &mut Context,
        cause: Option<ReleaseCause>,
    ) -> Result<(), OpFault> {
        self.finish_release_impl(context, cause)
    }
}

/// 统一存放不同面板的内部门面;满足 dyn compatibility,不对外暴露。
pub(crate) trait ErasedPanel<Context> {
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn handle(&self) -> PanelHandle;
    fn key(&self) -> &PanelKey;
    fn snapshot(&self) -> PanelSnapshot;
    fn pending_inputs(&self) -> usize;
    fn interaction_version(&self) -> u64;
    fn invalidate(&mut self) -> Result<(), UiError>;
    fn begin_load(&mut self) -> Result<u64, UiError>;
    fn begin_close(&mut self) -> Result<CloseDecision, OpFault>;
    fn set_visible(&mut self, context: &mut Context, visible: bool) -> Result<(), OpFault>;
    fn drive_input_one(
        &mut self,
        report: &mut crate::manager::DriveReport,
        navigation: &mut NavQueue,
    ) -> InputProgress;
    fn requires_quarantine(&self) -> bool;
    fn render_collect(&mut self, context: &mut Context, report: &mut crate::manager::DriveReport);
    fn finish_release(
        &mut self,
        context: &mut Context,
        cause: Option<ReleaseCause>,
    ) -> Result<(), OpFault>;
}

/// 清理亦失败时保留的可枚举、可重试记录;记录不可渲染、聚焦或接收事件,
/// 在释放成功前持续计入实例容量。
pub(crate) struct PendingRelease<Context> {
    pub(crate) id: PendingReleaseId,
    pub(crate) key: PanelKey,
    pub(crate) cause: Option<ReleaseCause>,
    pub(crate) stage: Stage,
    pub(crate) panel: Box<dyn ErasedPanel<Context>>,
}

/// 待重试释放记录的只读摘要;不含原始错误正文,避免敏感内容进入查询。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PendingReleaseSummary {
    pub id: PendingReleaseId,
    pub key: PanelKey,
    pub cause: Option<ReleaseCause>,
    pub stage: Stage,
}

/// 首次打开的构造失败;挂载/核验失败时把面板交回中心走清理。
pub(crate) enum CreateFault<Context> {
    Factory(Box<dyn Error + 'static>),
    /// 首次核验/挂载/卸载失败,面板资源已清理干净。
    Failed {
        stage: Stage,
        error: Box<dyn Error + 'static>,
        initial: Option<Box<dyn Error + 'static>>,
    },
    /// 清理亦失败;面板保留在可重试记录中,初始与清理错误一并上报。
    CleanupFailed {
        trigger: Box<dyn Error + 'static>,
        cleanup: OpFault,
        panel: Box<dyn ErasedPanel<Context>>,
    },
}

/// 同一种类面板的类型化注册表;实例以擦除门面统一存放。
pub(crate) struct PanelKindRegistry<Context, C, V>
where
    Context: 'static,
    C: UiController,
    V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
{
    kind: PanelKind,
    scope_level: ScopeLevel,
    factory: PanelFactory<C, V>,
    instances: Vec<Box<dyn ErasedPanel<Context>>>,
    pending: Vec<PendingRelease<Context>>,
}

impl<Context, C, V> PanelKindRegistry<Context, C, V>
where
    Context: 'static,
    C: UiController,
    V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
{
    pub(crate) fn new(
        kind: PanelKind,
        scope_level: ScopeLevel,
        factory: PanelFactory<C, V>,
    ) -> Self {
        Self {
            kind,
            scope_level,
            factory,
            instances: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// 首次打开:核验、构造、挂载,成功才登记实例;失败不留下半注册实例。
    pub(crate) fn create_instance(
        &mut self,
        handle: PanelHandle,
        key: PanelKey,
        context: &mut Context,
        args: &C::OpenArgs,
        input_capacity: usize,
    ) -> Result<(), CreateFault<Context>> {
        let panel = match (self.factory)(&key, args) {
            Ok(panel) => panel,
            Err(error) => return Err(CreateFault::Factory(error)),
        };
        let mut live = LivePanel::new(handle, key, panel, input_capacity);
        match live.mount_new(context) {
            Ok(()) => {
                self.instances.push(Box::new(live));
                Ok(())
            }
            Err(FirstOpenCleanup::Cleaned(fault)) => Err(CreateFault::Failed {
                stage: fault.stage,
                error: fault.error,
                initial: fault.initial,
            }),
            Err(FirstOpenCleanup::CleanupFailed { trigger, cleanup }) => {
                Err(CreateFault::CleanupFailed {
                    trigger,
                    cleanup,
                    panel: Box::new(live),
                })
            }
        }
    }

    fn find_index(&self, serial: InstanceSerial) -> Option<usize> {
        self.instances
            .iter()
            .position(|instance| instance.handle().serial == serial)
    }
}

impl<Context: 'static, C, V> ErasedKind<Context> for PanelKindRegistry<Context, C, V>
where
    C: UiController,
    V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
{
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn kind(&self) -> &PanelKind {
        &self.kind
    }

    fn scope_level(&self) -> ScopeLevel {
        self.scope_level
    }

    fn live_count(&self) -> usize {
        self.instances.len()
    }

    fn pending_count(&self) -> usize {
        self.pending.len()
    }

    fn find_by_key(&self, key: &PanelKey) -> Option<InstanceSerial> {
        self.instances
            .iter()
            .find(|instance| instance.key() == key)
            .map(|instance| instance.handle().serial)
    }

    fn instance_mut(&mut self, serial: InstanceSerial) -> Option<&mut dyn ErasedPanel<Context>> {
        let index = self.find_index(serial)?;
        Some(self.instances[index].as_mut())
    }

    fn take_instance(&mut self, serial: InstanceSerial) -> Option<Box<dyn ErasedPanel<Context>>> {
        let index = self.find_index(serial)?;
        Some(self.instances.remove(index))
    }

    fn describe(
        &self,
        serial: InstanceSerial,
        registration: usize,
        focus: Option<(usize, InstanceSerial)>,
    ) -> Option<PanelSummary> {
        let instance = self
            .instances
            .iter()
            .find(|instance| instance.handle().serial == serial)?;
        let snapshot = instance.snapshot();
        Some(PanelSummary {
            handle: instance.handle(),
            key: instance.key().clone(),
            visibility: snapshot.visibility,
            focused: focus == Some((registration, serial)),
            pending_inputs: snapshot.pending_inputs,
        })
    }

    fn describe_all(
        &self,
        registration: usize,
        focus: Option<(usize, InstanceSerial)>,
        out: &mut Vec<PanelSummary>,
    ) {
        for instance in &self.instances {
            let snapshot = instance.snapshot();
            out.push(PanelSummary {
                handle: instance.handle(),
                key: instance.key().clone(),
                visibility: snapshot.visibility,
                focused: focus == Some((registration, instance.handle().serial)),
                pending_inputs: snapshot.pending_inputs,
            });
        }
    }

    fn render_all(&mut self, context: &mut Context, report: &mut crate::manager::DriveReport) {
        for instance in &mut self.instances {
            instance.render_collect(context, report);
        }
    }

    fn pending_input_total(&self) -> usize {
        self.instances
            .iter()
            .map(|instance| instance.snapshot().pending_inputs)
            .fold(0usize, usize::saturating_add)
    }

    fn store_pending(&mut self, record: PendingRelease<Context>) {
        self.pending.push(record);
    }

    fn has_pending(&self, id: PendingReleaseId) -> bool {
        self.pending.iter().any(|record| record.id == id)
    }

    fn take_pending(&mut self, id: PendingReleaseId) -> Option<PendingRelease<Context>> {
        let index = self.pending.iter().position(|record| record.id == id)?;
        Some(self.pending.remove(index))
    }

    fn pending_summaries_into(&self, out: &mut Vec<PendingReleaseSummary>) {
        for record in &self.pending {
            out.push(PendingReleaseSummary {
                id: record.id,
                key: record.key.clone(),
                cause: record.cause,
                stage: record.stage,
            });
        }
    }

    fn pending_targets(
        &self,
        scope: &crate::identity::PanelScope,
        out: &mut Vec<(PanelHandle, PendingReleaseId)>,
    ) {
        for record in &self.pending {
            if record.key.scope() == scope {
                out.push((record.panel.handle(), record.id));
            }
        }
    }
}

/// 统一存放不同注册的内部门面;满足 dyn compatibility,不对外暴露。
pub(crate) trait ErasedKind<Context> {
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn kind(&self) -> &PanelKind;
    fn scope_level(&self) -> ScopeLevel;
    fn live_count(&self) -> usize;
    fn pending_count(&self) -> usize;
    fn find_by_key(&self, key: &PanelKey) -> Option<InstanceSerial>;
    fn instance_mut(&mut self, serial: InstanceSerial) -> Option<&mut dyn ErasedPanel<Context>>;
    fn take_instance(&mut self, serial: InstanceSerial) -> Option<Box<dyn ErasedPanel<Context>>>;
    fn describe(
        &self,
        serial: InstanceSerial,
        registration: usize,
        focus: Option<(usize, InstanceSerial)>,
    ) -> Option<PanelSummary>;
    fn describe_all(
        &self,
        registration: usize,
        focus: Option<(usize, InstanceSerial)>,
        out: &mut Vec<PanelSummary>,
    );
    fn render_all(&mut self, context: &mut Context, report: &mut crate::manager::DriveReport);
    fn pending_input_total(&self) -> usize;
    fn store_pending(&mut self, record: PendingRelease<Context>);
    fn has_pending(&self, id: PendingReleaseId) -> bool;
    fn take_pending(&mut self, id: PendingReleaseId) -> Option<PendingRelease<Context>>;
    fn pending_summaries_into(&self, out: &mut Vec<PendingReleaseSummary>);
    fn pending_targets(
        &self,
        scope: &crate::identity::PanelScope,
        out: &mut Vec<(PanelHandle, PendingReleaseId)>,
    );
}

/// 在擦除注册表中定位并降级回类型化注册表;类型不匹配即视为错误入口。
pub(crate) fn typed_registration<Context: 'static, C, V>(
    registrations: &mut [Box<dyn ErasedKind<Context>>],
    index: usize,
) -> Result<&mut PanelKindRegistry<Context, C, V>, UiError>
where
    C: UiController,
    V: UiView<Model = C::Model, Event = C::Event, Context = Context>,
{
    let erased = registrations.get_mut(index).ok_or(UiError::ForeignEntry)?;
    erased
        .as_any_mut()
        .downcast_mut::<PanelKindRegistry<Context, C, V>>()
        .ok_or(UiError::ForeignEntry)
}

/// 按编号定位擦除注册表;越界视为错误入口。
pub(crate) fn erased_registration<Context: 'static>(
    registrations: &mut [Box<dyn ErasedKind<Context>>],
    index: usize,
) -> Result<&mut dyn ErasedKind<Context>, UiError> {
    match registrations.get_mut(index) {
        Some(boxed) => Ok(boxed.as_mut()),
        None => Err(UiError::ForeignEntry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_version_exhaustion_does_not_wrap() {
        assert!(matches!(
            next_interaction_version(u64::MAX),
            Err(UiError::CapacityOverflow)
        ));
        assert_eq!(next_interaction_version(u64::MAX - 1).unwrap(), u64::MAX);
    }

    #[test]
    fn load_epoch_exhaustion_invalidates_tickets_and_refuses_new_loads() {
        let mut exhausted = false;
        assert!(matches!(
            next_load_epoch(u64::MAX, &mut exhausted),
            Err(UiError::CapacityOverflow)
        ));
        assert!(exhausted);
        assert!(!ticket_epoch_matches(u64::MAX, exhausted, u64::MAX));
        assert!(matches!(
            next_load_epoch(u64::MAX, &mut exhausted),
            Err(UiError::CapacityOverflow)
        ));

        let mut epoch = u64::MAX;
        let mut release_exhausted = false;
        expire_load_epoch(&mut epoch, &mut release_exhausted);
        assert!(release_exhausted);
        assert!(!ticket_epoch_matches(epoch, release_exhausted, u64::MAX));
    }
}
