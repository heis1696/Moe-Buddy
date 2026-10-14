//! 视图契约与事件采集出口。
//!
//! 视图只借用展示模型,管理后端特有视图资源;不访问业务存储、
//! 不直接发送模型请求、不持有业务服务。

use std::collections::VecDeque;
use std::error::Error;

/// 面板视图;关联类型与 [`crate::UiController`] 在 `UiPanel` 组合时编译期对齐。
pub trait UiView: 'static {
    /// 展示模型;与控制器一致。
    type Model;
    /// 交互事件;与控制器一致。
    type Event: 'static;
    /// 当前 GUI 后端上下文,由宿主在每次驱动/生命周期调用时注入。
    type Context;
    /// 视图错误;保留原始 `Error::source` 链。
    type Error: Error + 'static;

    /// 挂载:创建/绑定后端视图资源;失败由内核负责清理。
    fn mount(&mut self, context: &mut Self::Context) -> Result<(), Self::Error>;

    /// 呈现模型并采集输入;新采集事件进入实例输入队列,受容量与预算约束。
    fn render(
        &mut self,
        model: &Self::Model,
        context: &mut Self::Context,
        events: &mut EventSink<'_, Self::Event>,
    ) -> Result<(), Self::Error>;

    /// 卸载:释放后端视图资源;隐藏与关闭都会调用。
    fn unmount(&mut self, context: &mut Self::Context) -> Result<(), Self::Error>;
}

/// 实例输入队列中的一项;带加载票据时在分发前复验加载序号。
pub(crate) struct PendingInput<E> {
    pub(crate) event: E,
    pub(crate) load_epoch: Option<u64>,
}

/// 视图采集事件的有界出口;队满把事件交还视图并计入背压。
pub struct EventSink<'a, E> {
    queue: &'a mut VecDeque<PendingInput<E>>,
    capacity: usize,
    refused: &'a mut usize,
}

impl<'a, E> EventSink<'a, E> {
    pub(crate) fn new(
        queue: &'a mut VecDeque<PendingInput<E>>,
        capacity: usize,
        refused: &'a mut usize,
    ) -> Self {
        Self {
            queue,
            capacity,
            refused,
        }
    }

    /// 提交一个交互事件;队满时原样交还(`Err`),不静默丢弃。
    pub fn submit(&mut self, event: E) -> Result<(), E> {
        if self.queue.len() >= self.capacity {
            *self.refused = self.refused.saturating_add(1);
            return Err(event);
        }
        self.queue.push_back(PendingInput {
            event,
            load_epoch: None,
        });
        Ok(())
    }
}
