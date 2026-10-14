//! 面板身份:种类、作用域、键、实例句柄与只读摘要。
//!
//! 身份只用于定位,不授予任何权限;句柄由中心签发,外部不能构造。

use std::fmt;

use crate::error::UiError;

/// 校验稳定身份字符串:非空、无空白与控制字符、长度有界。
fn validate_identity(value: &str, what: &'static str) -> Result<(), UiError> {
    if value.is_empty() || value.len() > 64 {
        return Err(UiError::InvalidIdentity(what));
    }
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(UiError::InvalidIdentity(what));
    }
    Ok(())
}

/// 面板种类名,同一种类在中心内唯一注册。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PanelKind(String);

impl PanelKind {
    /// 创建面板种类名;拒绝空名、空白、控制字符与超过 64 字节的名称。
    pub fn new(name: &str) -> Result<Self, UiError> {
        validate_identity(name, "panel kind")?;
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PanelKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 稳定桌宠身份,绑定所属实例,不随全局选中宠物变化。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ScopeIdentity(String);

impl ScopeIdentity {
    /// 创建稳定身份;拒绝空名、空白、控制字符与超过 64 字节的名称。
    pub fn new(identity: &str) -> Result<Self, UiError> {
        validate_identity(identity, "scope identity")?;
        Ok(Self(identity.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScopeIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 作用域:应用级或绑定稳定桌宠身份。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PanelScope {
    Application,
    Pet(ScopeIdentity),
}

impl fmt::Display for PanelScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Application => formatter.write_str("application"),
            Self::Pet(identity) => write!(formatter, "pet:{identity}"),
        }
    }
}

/// 注册面板时声明的允许作用域层级。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ScopeLevel {
    Application,
    Pet,
}

impl ScopeLevel {
    pub(crate) fn accepts(self, scope: &PanelScope) -> bool {
        matches!(
            (self, scope),
            (Self::Application, PanelScope::Application) | (Self::Pet, PanelScope::Pet(_))
        )
    }
}

/// 面板键 = 种类 + 作用域;首版每个键至多一个存活实例。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PanelKey {
    kind: PanelKind,
    scope: PanelScope,
}

impl PanelKey {
    pub(crate) fn new(kind: PanelKind, scope: PanelScope) -> Self {
        Self { kind, scope }
    }

    pub fn kind(&self) -> &PanelKind {
        &self.kind
    }

    pub fn scope(&self) -> &PanelScope {
        &self.scope
    }
}

impl fmt::Display for PanelKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.kind, self.scope)
    }
}

/// 中心内单调分配的实例序号,0 不使用。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub(crate) struct InstanceSerial(std::num::NonZeroU64);

impl InstanceSerial {
    pub(crate) fn new(raw: u64) -> Result<Self, UiError> {
        Ok(Self(
            std::num::NonZeroU64::new(raw).ok_or(UiError::IdentifierExhausted)?,
        ))
    }

    pub(crate) fn as_non_zero(self) -> std::num::NonZeroU64 {
        self.0
    }
}

impl fmt::Display for InstanceSerial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// 由当前中心签发、绑定本次实例的句柄。
///
/// 关闭后重开产生新句柄;旧句柄不得命中新实例,也不能退回按键路由。
/// 字段私有,外部不能伪造;跨中心使用会被拒绝。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PanelHandle {
    pub(crate) center: u64,
    pub(crate) registration: usize,
    pub(crate) serial: InstanceSerial,
}

impl PanelHandle {
    pub(crate) fn new(center: u64, registration: usize, serial: InstanceSerial) -> Self {
        Self {
            center,
            registration,
            serial,
        }
    }
}

impl fmt::Display for PanelHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "panel#{}", self.serial)
    }
}

/// 存活可见性;未创建/已销毁由实例不存在表达。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Visibility {
    Visible,
    Hidden,
}

/// 只读生命周期摘要;不含模型、入参、事件或凭据。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PanelSummary {
    pub handle: PanelHandle,
    pub key: PanelKey,
    pub visibility: Visibility,
    pub focused: bool,
    pub pending_inputs: usize,
}
