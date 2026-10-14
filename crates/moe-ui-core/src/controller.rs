//! 控制器契约:面板用例、交互状态与类型化事件处理。
//!
//! 控制器不绘制控件、不访问全局面板表、不替业务服务裁决权限;
//! 只能通过 [`NavSink`] 提交受控导航命令,不能拿到中心引用。

use std::error::Error;

use crate::panel::NavSink;

/// 面板控制器;关联类型与 [`crate::UiView`] 在 `UiPanel` 组合时编译期对齐。
pub trait UiController: 'static {
    /// 面板特有入参;不能是万能字符串字典。
    type OpenArgs;
    /// 展示模型;视图只借用它,不能获得控制器内部状态的可变引用。
    type Model;
    /// 面板专属类型化事件;业务加载结果也以该事件交回。
    type Event: 'static;
    /// 控制器错误;保留原始 `Error::source` 链。
    type Error: Error + 'static;

    /// 当前展示模型。
    fn model(&self) -> &Self::Model;

    /// 处理一个类型化事件并更新模型;成功后实例交互版本前进。
    fn apply(
        &mut self,
        event: Self::Event,
        navigation: &mut NavSink<'_>,
    ) -> Result<(), Self::Error>;

    /// 已有实例收到新入参时由控制器明确处理;不默认覆盖或重置草稿。
    /// 调用前旧加载票据和关闭确认均失效,即使本回调返回错误也不恢复。
    fn revise(
        &mut self,
        args: &Self::OpenArgs,
        navigation: &mut NavSink<'_>,
    ) -> Result<(), Self::Error>;

    /// 显示/恢复边界:重新核验授权与数据时效,按需刷新;不把旧缓存当长期许可。
    fn refresh(&mut self) -> Result<(), Self::Error>;

    /// 隐藏边界:暂停仅服务于可见界面的订阅/刷新;允许保留的草稿由控制器持有。
    fn suspend(&mut self) -> Result<(), Self::Error>;

    /// 关闭前决定是否需要处理未保存草稿;需要确认时返回可区分结果。
    /// 因该回调可变,内核调用前先使已有关闭确认失效。
    fn close_request(&mut self) -> CloseDecision;

    /// 撤权/宿主强制释放边界:清除面板持有的敏感缓存。
    ///
    /// 仅在 `force_release`/`force_release_scope` 等撤权流程中由内核
    /// 无条件调用,即使视图卸载失败也不跳过;普通关闭与隐藏不调用。
    fn revoke(&mut self);

    /// 释放控制器资源;失败会进入可重试的失败记录,不冒称已关闭。
    fn release(&mut self) -> Result<(), Self::Error>;
}

/// 关闭请求的裁决。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CloseDecision {
    /// 允许关闭,立即进入释放流程。
    Allow,
    /// 存在未保存草稿,需要宿主交互确认;未确认/取消不释放。
    RequireConfirmation,
}

/// 关闭确认凭据;绑定实例与签发时的交互版本。
///
/// 确认交给宿主交互;后续事件、新入参、关闭请求或加载结果改变控制器后,
/// 旧确认在 [`crate::UiManager::confirm_close`] 复验时被拒绝,不会丢弃新草稿。
/// 未确认(直接丢弃本凭据)不释放实例。
#[derive(Clone, Debug)]
pub struct CloseConfirmation {
    pub(crate) center: u64,
    pub(crate) registration: usize,
    pub(crate) serial: crate::identity::InstanceSerial,
    pub(crate) interaction_version: u64,
}
