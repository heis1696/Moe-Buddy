//! 宿主显式提供的非零容量与每轮预算。

use std::num::NonZeroUsize;

use crate::error::UiError;

/// 中心硬预算:全部由宿主显式配置且必须非零。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UiConfig {
    registration_capacity: NonZeroUsize,
    live_instance_capacity: NonZeroUsize,
    instance_input_capacity: NonZeroUsize,
    navigation_capacity: NonZeroUsize,
    events_per_round: NonZeroUsize,
    navigations_per_round: NonZeroUsize,
}

impl UiConfig {
    /// 创建配置;任一容量或预算为零即拒绝。
    ///
    /// - `registration_capacity`:可注册的面板种类数上限。
    /// - `live_instance_capacity`:同时存活实例上限;尚未成功释放的资源计入,
    ///   释放完成即归还额度,历史累计打开数不计入。
    /// - `instance_input_capacity`:每个实例的类型化输入队列长度。
    /// - `navigation_capacity`:中心导航命令队列长度。
    /// - `events_per_round`:每轮 `drive` 处理的事件(输入)预算。
    /// - `navigations_per_round`:每轮 `drive` 执行的导航命令预算。
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        registration_capacity: usize,
        live_instance_capacity: usize,
        instance_input_capacity: usize,
        navigation_capacity: usize,
        events_per_round: usize,
        navigations_per_round: usize,
    ) -> Result<Self, UiError> {
        let build = |raw: usize| NonZeroUsize::new(raw).ok_or(UiError::CapacityOverflow);
        Ok(Self {
            registration_capacity: build(registration_capacity)?,
            live_instance_capacity: build(live_instance_capacity)?,
            instance_input_capacity: build(instance_input_capacity)?,
            navigation_capacity: build(navigation_capacity)?,
            events_per_round: build(events_per_round)?,
            navigations_per_round: build(navigations_per_round)?,
        })
    }

    pub fn registration_capacity(&self) -> usize {
        self.registration_capacity.get()
    }

    pub fn live_instance_capacity(&self) -> usize {
        self.live_instance_capacity.get()
    }

    pub fn instance_input_capacity(&self) -> usize {
        self.instance_input_capacity.get()
    }

    pub fn navigation_capacity(&self) -> usize {
        self.navigation_capacity.get()
    }

    pub fn events_per_round(&self) -> usize {
        self.events_per_round.get()
    }

    pub fn navigations_per_round(&self) -> usize {
        self.navigations_per_round.get()
    }
}
