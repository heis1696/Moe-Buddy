//! 面板组合、受控导航命令与导航出口。
//!
//! `UiPanel` 用结构体组合替代可继承基类树:控制器与视图的
//! `Model/Event` 必须一致,组合在编译期验证;视图只借用展示模型。

use std::collections::VecDeque;

use crate::controller::UiController;
use crate::identity::PanelHandle;
use crate::view::UiView;

/// 一个控制器与一个视图的组合;按统一边界驱动、转交事件及清理。
pub struct UiPanel<C, V> {
    controller: C,
    view: V,
}

impl<C, V> UiPanel<C, V> {
    /// 只读访问控制器;不提供可变引用。
    pub fn controller(&self) -> &C {
        &self.controller
    }

    pub(crate) fn controller_mut(&mut self) -> &mut C {
        &mut self.controller
    }

    /// 拆分借用,供内核在渲染时同时只读控制器、可变视图。
    pub(crate) fn parts_mut(&mut self) -> (&C, &mut V) {
        (&self.controller, &mut self.view)
    }
}

impl<C, V> UiPanel<C, V>
where
    C: UiController,
    V: UiView<Model = C::Model, Event = C::Event>,
{
    /// 组合控制器与视图;模型与事件类型不一致时无法编译。
    pub fn new(controller: C, view: V) -> Self {
        Self { controller, view }
    }
}

/// 面板导航命令;由控制器经 [`NavSink`] 提交,中心在安全边界执行。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Navigate {
    Close(PanelHandle),
    Hide(PanelHandle),
    Show(PanelHandle),
    Focus(PanelHandle),
}

impl Navigate {
    /// 命令目标实例;中心执行前复验其存在性与可见性。
    pub fn target(&self) -> PanelHandle {
        match *self {
            Self::Close(handle) | Self::Hide(handle) | Self::Show(handle) | Self::Focus(handle) => {
                handle
            }
        }
    }
}

/// 中心的有界导航队列;容量来自 [`crate::UiConfig::navigation_capacity`]。
pub(crate) struct NavQueue {
    queue: VecDeque<Navigate>,
    capacity: usize,
}

impl NavQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            capacity,
        }
    }

    pub(crate) fn push(&mut self, command: Navigate) -> bool {
        if self.queue.len() >= self.capacity {
            return false;
        }
        self.queue.push_back(command);
        true
    }

    pub(crate) fn pop_front(&mut self) -> Option<Navigate> {
        self.queue.pop_front()
    }

    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }
}

/// 控制器提交导航命令的受控出口;不暴露中心引用,不能递归修改面板集合。
///
/// 队满时 [`NavSink::submit`] 把原命令经 `Err` 交还,由调用者保留并择机
/// 重交;报告只累计拒绝次数,不保存命令正文,不与调用者重复重交同一命令。
pub struct NavSink<'a> {
    queue: &'a mut NavQueue,
    self_handle: PanelHandle,
    refused: &'a mut usize,
}

impl<'a> NavSink<'a> {
    pub(crate) fn bind(
        queue: &'a mut NavQueue,
        self_handle: PanelHandle,
        refused: &'a mut usize,
    ) -> Self {
        Self {
            queue,
            self_handle,
            refused,
        }
    }

    /// 请求关闭本面板;可能进入关闭确认流程。
    pub fn close_self(&mut self) -> Result<(), Navigate> {
        self.submit(Navigate::Close(self.self_handle))
    }

    /// 请求隐藏本面板。
    pub fn hide_self(&mut self) -> Result<(), Navigate> {
        self.submit(Navigate::Hide(self.self_handle))
    }

    /// 请求显示目标面板(不抢焦点)。
    pub fn show(&mut self, target: PanelHandle) -> Result<(), Navigate> {
        self.submit(Navigate::Show(target))
    }

    /// 请求把逻辑焦点交给目标可见面板。
    pub fn focus(&mut self, target: PanelHandle) -> Result<(), Navigate> {
        self.submit(Navigate::Focus(target))
    }

    /// 提交任意受控导航命令;队满时原命令经 `Err` 交还调用者保留,不静默丢失。
    pub fn submit(&mut self, command: Navigate) -> Result<(), Navigate> {
        if self.queue.push(command) {
            return Ok(());
        }
        *self.refused = self.refused.saturating_add(1);
        Err(command)
    }
}
