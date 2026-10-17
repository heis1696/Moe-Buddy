//! 统一错误类型、失败阶段与强制释放原因。

use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;

use crate::identity::{InstanceSerial, PanelKey, PanelKind, PanelScope};
use crate::panel::Navigate;

/// 待重试释放记录的编号;由中心签发并绑定中心,外部不能伪造或从原始数还原。
///
/// 编号直接复用签发时实例的中心内序号:序号单调分配且永不复用,
/// 因此记录编号分配不会失败,不会因编号耗尽丢弃待释放资源。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PendingReleaseId {
    pub(crate) center: u64,
    serial: NonZeroU64,
}

impl PendingReleaseId {
    pub(crate) fn from_serial(center: u64, serial: InstanceSerial) -> Self {
        Self {
            center,
            serial: serial.as_non_zero(),
        }
    }
}

impl fmt::Display for PendingReleaseId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "pending-release#{}", self.serial)
    }
}

/// 同一操作中按发生顺序保留的多个原始错误及其具体类型。
///
/// 两个错误及各自的 `Error::source` 链均可分别检查,不将具体类型压成文本。
#[derive(Debug)]
pub struct AggregateError {
    first: Box<dyn Error + 'static>,
    second: Box<dyn Error + 'static>,
}

impl AggregateError {
    pub(crate) fn new(first: Box<dyn Error + 'static>, second: Box<dyn Error + 'static>) -> Self {
        Self { first, second }
    }

    /// 首个原始错误,可继续沿其 `Error::source` 链检查。
    pub fn first_error(&self) -> &(dyn Error + 'static) {
        self.first.as_ref()
    }

    /// 后续原始错误,可继续沿其 `Error::source` 链检查。
    pub fn second_error(&self) -> &(dyn Error + 'static) {
        self.second.as_ref()
    }
}

impl fmt::Display for AggregateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}; then {}", self.first, self.second)
    }
}

impl Error for AggregateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.first.as_ref())
    }
}

/// 单面板操作失败的阶段,保留在错误与报告中,不与生命周期混用。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Stage {
    Factory,
    Mount,
    Refresh,
    Suspend,
    Unmount,
    Render,
    ApplyEvent,
    ReviseArgs,
    ControllerRelease,
    Navigation,
    Invalidate,
}

impl fmt::Display for Stage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Factory => "factory",
            Self::Mount => "mount",
            Self::Refresh => "refresh",
            Self::Suspend => "suspend",
            Self::Unmount => "unmount",
            Self::Render => "render",
            Self::ApplyEvent => "event-apply",
            Self::ReviseArgs => "revise-args",
            Self::ControllerRelease => "controller-release",
            Self::Navigation => "navigation",
            Self::Invalidate => "invalidate",
        };
        formatter.write_str(text)
    }
}

/// 强制清理的原因,只能来自明确的宿主生命周期,不伪造业务回滚。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ReleaseCause {
    HostExit,
    IdentityRemoved,
    ContextRevoked,
}

impl fmt::Display for ReleaseCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::HostExit => "host exit",
            Self::IdentityRemoved => "pet identity removed",
            Self::ContextRevoked => "trusted context revoked",
        };
        formatter.write_str(text)
    }
}

/// UI 内核统一错误;单面板错误保留阶段与原始 `Error::source` 链。
#[derive(Debug)]
pub enum UiError {
    /// 面板种类或作用域身份不合法。
    InvalidIdentity(&'static str),
    /// 注册的作用域层级与打开时提供的作用域不匹配。
    ScopeLevelRejected { kind: PanelKind, scope: PanelScope },
    /// 注册容量已满。
    RegistrationCapacity,
    /// 同一种类重复注册。
    KindAlreadyRegistered(PanelKind),
    /// 同时存活实例容量已满;未成功释放的资源计入容量。
    InstanceCapacity,
    /// 计数溢出,明确报错。
    CapacityOverflow,
    /// 中心/实例编号空间耗尽。
    IdentifierExhausted,
    /// 注册入口不属于当前中心。
    ForeignEntry,
    /// 句柄不属于当前中心。
    ForeignHandle,
    /// 句柄在本中心没有对应实例(未创建、已释放或已转入失败记录)。
    InstanceNotFound,
    /// 待重试释放记录不存在。
    PendingReleaseNotFound,
    PendingReleaseForKey {
        key: PanelKey,
        record: PendingReleaseId,
    },
    /// 实例不可见;不暗中显示。
    NotVisible,
    /// 首次打开缺少类型化入参。
    MissingOpenArgs,
    /// 实例仍有已接纳未处理的输入;普通关闭被拒绝,
    /// 须先驱动消费或由宿主强制清理,不静默丢弃。
    CloseBlockedByPendingInputs,
    /// 关闭确认绑定旧交互版本;控制器已被新事件/入参改变时拒绝。
    ConfirmationStale,
    /// 确认对应的实例已不存在。
    ConfirmationInstanceGone,
    /// 导航队列已满,原命令交还调用者。
    NavigationQueueFull(Navigate),
    /// 单面板操作失败:保留阶段与原始错误链,不阻断其他面板。
    ///
    /// `source` 是报告阶段的失败;`initial_source` 是同一操作中先于它
    /// 发生并保留的初始错误(如挂载失败后卸载亦失败时的挂载错误)。
    PanelFault {
        key: PanelKey,
        stage: Stage,
        source: Box<dyn Error + 'static>,
        initial: Option<Box<dyn Error + 'static>>,
    },
    /// 释放未完成:已保留可由宿主重试的失败记录。
    ///
    /// `source` 是清理/释放阶段的失败;`initial_source` 是触发清理的
    /// 初始错误(如首次打开的挂载失败),直接释放失败时为 `None`。
    ReleaseIncomplete {
        record: PendingReleaseId,
        key: PanelKey,
        stage: Stage,
        source: Box<dyn Error + 'static>,
        initial: Option<Box<dyn Error + 'static>>,
    },
}

impl UiError {
    /// 同一操作中先于 `source` 发生并保留的初始错误;普通单点失败为 `None`。
    pub fn initial_source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::PanelFault { initial, .. } | Self::ReleaseIncomplete { initial, .. } => {
                initial.as_deref()
            }
            _ => None,
        }
    }
}

impl fmt::Display for UiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity(what) => write!(
                formatter,
                "invalid {what}: empty, whitespace or over 64 bytes"
            ),
            Self::ScopeLevelRejected { kind, scope } => {
                write!(formatter, "panel kind {kind} does not accept scope {scope}")
            }
            Self::RegistrationCapacity => formatter.write_str("registration capacity exceeded"),
            Self::KindAlreadyRegistered(kind) => {
                write!(formatter, "panel kind {kind} already registered")
            }
            Self::InstanceCapacity => formatter
                .write_str("live instance capacity exceeded; unreleased resources still count"),
            Self::CapacityOverflow => formatter.write_str("internal counter overflow"),
            Self::IdentifierExhausted => formatter.write_str("identifier space exhausted"),
            Self::ForeignEntry => formatter.write_str("panel entry does not belong to this center"),
            Self::ForeignHandle => {
                formatter.write_str("panel handle does not belong to this center")
            }
            Self::InstanceNotFound => {
                formatter.write_str("panel instance not found for this handle")
            }
            Self::PendingReleaseNotFound => formatter.write_str("pending release record not found"),
            Self::PendingReleaseForKey { key, record } => {
                write!(
                    formatter,
                    "panel {key} still has pending release {record}; retry cleanup before reopening"
                )
            }
            Self::NotVisible => formatter.write_str("panel instance is not visible"),
            Self::MissingOpenArgs => formatter.write_str("first open requires typed open args"),
            Self::CloseBlockedByPendingInputs => formatter.write_str(
                "close rejected: instance still holds accepted inputs; drive them or force release",
            ),
            Self::ConfirmationStale => formatter
                .write_str("close confirmation stale: controller state changed since issuance"),
            Self::ConfirmationInstanceGone => {
                formatter.write_str("close confirmation target instance no longer exists")
            }
            Self::NavigationQueueFull(command) => {
                write!(
                    formatter,
                    "navigation queue full; command returned: {command:?}"
                )
            }
            Self::PanelFault { key, stage, .. } => {
                write!(formatter, "panel {key} failed at stage {stage}")
            }
            Self::ReleaseIncomplete {
                record, key, stage, ..
            } => {
                write!(
                    formatter,
                    "release of panel {key} incomplete at stage {stage}; retry via {record}"
                )
            }
        }
    }
}

impl Error for UiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::PanelFault { source, .. } | Self::ReleaseIncomplete { source, .. } => {
                Some(&**source)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_release_ids_are_center_bound_and_unique() {
        let first = InstanceSerial::new(7).unwrap();
        let second = InstanceSerial::new(9).unwrap();
        assert_eq!(
            PendingReleaseId::from_serial(1, first),
            PendingReleaseId::from_serial(1, first)
        );
        assert_ne!(
            PendingReleaseId::from_serial(1, first),
            PendingReleaseId::from_serial(2, first)
        );
        assert_ne!(
            PendingReleaseId::from_serial(1, first),
            PendingReleaseId::from_serial(1, second)
        );
        assert_eq!(
            PendingReleaseId::from_serial(3, second).to_string(),
            "pending-release#9"
        );
    }

    #[test]
    fn stages_display_without_sensitive_content() {
        assert_eq!(Stage::Invalidate.to_string(), "invalidate");
        assert_eq!(Stage::ControllerRelease.to_string(), "controller-release");
    }
}
