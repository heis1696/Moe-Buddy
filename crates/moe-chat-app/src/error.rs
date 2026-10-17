//! 统一应用错误：固定安全公开码/公开文案，原始错误链保留在 `Error::source` 内。
//!
//! `public_code()`/`public_message()` 只返回本模块定义的静态串，供传输适配
//! 直接展示；不输出网关、记忆或 UI 内核的原始错误文本，也不包含任何正文。

use std::error::Error;
use std::fmt;

use moe_agent_runtime::{AgentError, AgentWorkflowError, DiagnosticsError, GatewayError};
use moe_ui_core::{DriveReport, RejectionReason, UiError};

#[derive(Debug)]
struct DriveFailure(DriveReport);

impl fmt::Display for DriveFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UI drive failed; the original bounded report is retained")
    }
}

impl Error for DriveFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        if let Some(fault) = self.0.faults().first() {
            Some(fault.source_error())
        } else {
            self.0
                .navigation_faults()
                .first()
                .map(|fault| &fault.error as &(dyn Error + 'static))
        }
    }
}

/// 输入/状态类拒绝的内部描述；只含固定静态文本，不含请求正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rejection(&'static str);

impl Rejection {
    pub(crate) const fn new(text: &'static str) -> Self {
        Self(text)
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for Rejection {}

/// 文字聊天应用的统一错误。
///
/// `public_code`/`public_message` 是固定安全类别；`Error::source` 保留底层
/// 网关、记忆、诊断或 UI 内核的原始错误及其完整链，仅供受信调用方检查。
/// 链上各层类型均不携带聊天/记忆正文或凭据，`Debug` 输出安全。
#[derive(Debug)]
pub struct AppError {
    code: &'static str,
    message: &'static str,
    source: Box<dyn Error + 'static>,
}

#[derive(Debug)]
struct DisposalFailure {
    primary: AppError,
    cleanup: GatewayError,
}

impl fmt::Display for DisposalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("post-admission disposition failed; interaction quarantined")
    }
}

impl Error for DisposalFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.primary)
    }
}

impl AppError {
    pub(crate) fn cancelled_after_admission(primary: Self) -> Self {
        Self {
            code: "send_cancelled",
            message: "该轮登记后失败，账本已确认取消；未登记回复，输入草稿已保留",
            source: Box::new(primary),
        }
    }

    pub(crate) fn quarantined_after_admission(primary: Self, cleanup: GatewayError) -> Self {
        Self {
            code: "send_unknown",
            message: "该轮登记后的结果未能确认，已隔离会话；请勿重发，请重启宿主",
            source: Box::new(DisposalFailure { primary, cleanup }),
        }
    }

    /// Additional disposition error retained alongside the primary source chain.
    pub fn disposition_error(&self) -> Option<&GatewayError> {
        self.source
            .downcast_ref::<DisposalFailure>()
            .map(|failure| &failure.cleanup)
    }

    pub(crate) fn rejection(
        code: &'static str,
        message: &'static str,
        detail: &'static str,
    ) -> Self {
        Self {
            code,
            message,
            source: Box::new(Rejection::new(detail)),
        }
    }

    /// 对外展示的固定安全类别码。
    pub fn public_code(&self) -> &'static str {
        self.code
    }

    /// 对外展示的固定安全提示；不含原始错误文本或业务正文。
    pub fn public_message(&self) -> &'static str {
        self.message
    }

    /// Preserve an adapter error without exposing paths, bodies or credentials in UI text.
    pub fn from_storage<E: Error + 'static>(error: E) -> Self {
        Self {
            code: "storage_failed",
            message: "本地保存或恢复失败，原存档已保留，请检查文件与占用",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_ui(error: UiError) -> Self {
        Self {
            code: "ui_rejected",
            message: "UI 内核拒绝了该面板操作",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_drive_report(report: DriveReport) -> Self {
        Self {
            code: "panel_callback_failed",
            message: "面板驱动失败，原始错误链已保留；请检查面板状态",
            source: Box::new(DriveFailure(report)),
        }
    }

    pub(crate) fn from_gateway(error: GatewayError) -> Self {
        Self {
            code: "gateway_rejected",
            message: "可信入口或会话运行时拒绝了该操作",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_memory<E: Error + 'static>(error: E) -> Self {
        Self {
            code: "memory_rejected",
            message: "记忆服务拒绝了该操作",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_diagnostics(error: DiagnosticsError) -> Self {
        Self {
            code: "diagnostics_rejected",
            message: "诊断服务拒绝了该操作",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_workflow<E: Error + 'static>(error: AgentWorkflowError<E>) -> Self {
        Self {
            code: "agent_failed",
            message: "离线 Agent 链路失败，未发布成功结果",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_submit(reason: RejectionReason) -> Self {
        let (code, message, detail) = match reason {
            RejectionReason::NotVisible => (
                "panel_not_visible",
                "目标面板当前不可见，已拒绝输入",
                "panel instance is not visible",
            ),
            RejectionReason::InstanceGone => (
                "panel_not_open",
                "目标面板未打开",
                "panel instance does not exist",
            ),
            RejectionReason::QueueFull => (
                "input_backpressure",
                "面板输入队列已满，原输入已退回",
                "panel input queue is full",
            ),
            RejectionReason::ForeignCenter
            | RejectionReason::ForeignRegistration
            | RejectionReason::TicketStale => (
                "panel_binding_rejected",
                "面板句柄与注册入口不匹配，已拒绝",
                "panel handle does not match the registration",
            ),
        };
        Self::rejection(code, message, detail)
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl From<UiError> for AppError {
    fn from(error: UiError) -> Self {
        Self::from_ui(error)
    }
}

impl<E: Error + 'static> From<AgentError<E>> for AppError {
    fn from(error: AgentError<E>) -> Self {
        Self {
            code: "agent_failed",
            message: "离线 Agent 链路失败，未发布成功结果",
            source: Box::new(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moe_ui_core::{
        CloseDecision, EventSink, NavSink, PanelKind, PanelScope, ScopeLevel, UiConfig,
        UiController, UiManager, UiPanel, UiView,
    };

    const SENTINEL_BODY: &str = "private sentinel failure body";

    #[derive(Debug)]
    struct SentinelError;

    impl fmt::Display for SentinelError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(SENTINEL_BODY)
        }
    }

    impl Error for SentinelError {}

    struct DriveTestController {
        model: (),
    }

    impl UiController for DriveTestController {
        type OpenArgs = ();
        type Model = ();
        type Event = ();
        type Error = SentinelError;

        fn model(&self) -> &Self::Model {
            &self.model
        }

        fn apply(
            &mut self,
            _event: Self::Event,
            _navigation: &mut NavSink<'_>,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn revise(
            &mut self,
            _args: &Self::OpenArgs,
            _navigation: &mut NavSink<'_>,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn refresh(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn suspend(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn close_request(&mut self) -> CloseDecision {
            CloseDecision::Allow
        }

        fn revoke(&mut self) {}

        fn release(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct FailingDriveTestView;

    impl UiView for FailingDriveTestView {
        type Model = ();
        type Event = ();
        type Context = ();
        type Error = SentinelError;

        fn mount(&mut self, _context: &mut Self::Context) -> Result<(), Self::Error> {
            Ok(())
        }

        fn render(
            &mut self,
            _model: &Self::Model,
            _context: &mut Self::Context,
            _events: &mut EventSink<'_, Self::Event>,
        ) -> Result<(), Self::Error> {
            Err(SentinelError)
        }

        fn unmount(&mut self, _context: &mut Self::Context) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn public_surface_is_fixed_and_chain_is_preserved() {
        let error = AppError::from_gateway(GatewayError::Revoked);
        assert_eq!(error.public_code(), "gateway_rejected");
        assert_eq!(error.public_message(), "可信入口或会话运行时拒绝了该操作");
        assert!(
            std::error::Error::source(&error)
                .and_then(|source| source.downcast_ref::<GatewayError>())
                .is_some(),
            "original gateway error stays reachable via source()"
        );
        assert!(!error.to_string().contains("no longer current"));
    }

    #[test]
    fn submit_rejections_map_to_fixed_codes() {
        assert_eq!(
            AppError::from_submit(RejectionReason::NotVisible).public_code(),
            "panel_not_visible"
        );
        assert_eq!(
            AppError::from_submit(RejectionReason::InstanceGone).public_code(),
            "panel_not_open"
        );
        assert_eq!(
            AppError::from_submit(RejectionReason::QueueFull).public_code(),
            "input_backpressure"
        );
    }

    #[test]
    fn drive_report_preserves_original_error_without_exposing_its_body() {
        let config = UiConfig::new(1, 1, 1, 1, 1, 1).unwrap();
        let mut ui = UiManager::new(config).unwrap();
        let entry = ui
            .register::<DriveTestController, FailingDriveTestView>(
                PanelKind::new("drive-error-test").unwrap(),
                ScopeLevel::Application,
                |_, _| {
                    Ok(UiPanel::new(
                        DriveTestController { model: () },
                        FailingDriveTestView,
                    ))
                },
            )
            .unwrap();
        ui.open(&mut (), &entry, PanelScope::Application, Some(&()))
            .unwrap();
        let report = ui.drive(&mut ());
        assert_eq!(report.faults().len(), 1);

        let error = AppError::from_drive_report(report);

        assert_eq!(error.public_code(), "panel_callback_failed");
        assert!(!error.public_code().contains(SENTINEL_BODY));
        assert!(!error.public_message().contains(SENTINEL_BODY));
        assert!(!error.to_string().contains(SENTINEL_BODY));
        let mut source = Error::source(&error).map(|source| source as &dyn Error);
        let mut found_sentinel = false;
        while let Some(current) = source {
            if current.downcast_ref::<SentinelError>().is_some() {
                found_sentinel = true;
            }
            source = current.source();
        }
        assert!(
            found_sentinel,
            "original drive error stays reachable via source()"
        );
    }
}
