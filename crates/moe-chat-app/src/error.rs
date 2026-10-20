//! 统一应用错误：固定安全公开码/公开文案，原始错误链保留在 `Error::source` 内。
//!
//! `public_code()`/`public_message()` 只返回本模块定义的静态串，供传输适配
//! 直接展示；不输出网关、记忆或 UI 内核的原始错误文本，也不包含任何正文。

use std::error::Error;
use std::fmt;

use moe_agent_runtime::{AgentError, DiagnosticsError, GatewayError, WorkflowError};
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

/// Evidence about the current command, independent of its public error code.
/// This does not describe whether an earlier model request reached a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionDisposition {
    /// Rejected before this command performed its business operation.
    NotExecuted,
    /// The operation has an authoritative terminal result, possibly a failure.
    Settled,
    /// The available evidence cannot establish the operation's result.
    Unknown,
}

impl ExecutionDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotExecuted => "not_executed",
            Self::Settled => "settled",
            Self::Unknown => "unknown",
        }
    }
}

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
    disposition: ExecutionDisposition,
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

#[derive(Debug)]
struct DisplayFailure {
    primary: AppError,
}

impl fmt::Display for DisplayFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("reply committed but display publication failed")
    }
}

impl Error for DisplayFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.primary)
    }
}

/// Secondary failure retained next to an authoritative primary result.
///
/// The primary error stays on the ordinary `Error::source` chain; the secondary
/// one is reachable through [`AppError::retained_error`]. This is used when one
/// accepted command has independent failure domains (business result, local
/// persistence and display publication) that must not overwrite each other.
#[derive(Debug)]
struct RetainedFailure {
    primary: AppError,
    secondary: AppError,
}

impl fmt::Display for RetainedFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("additional independent failure retained next to the primary result")
    }
}

impl Error for RetainedFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.primary)
    }
}

impl AppError {
    /// The application records this at the operation boundary, never by matching
    /// public error strings in the UI or transport adapter.
    pub fn execution_disposition(&self) -> ExecutionDisposition {
        self.disposition
    }

    pub(crate) fn with_execution_disposition(mut self, value: ExecutionDisposition) -> Self {
        self.disposition = value;
        self
    }

    /// Rejected before this command performed its business operation.
    pub(crate) fn not_executed(self) -> Self {
        self.with_execution_disposition(ExecutionDisposition::NotExecuted)
    }

    /// An authoritative terminal result, possibly a failure.
    pub(crate) fn settled(self) -> Self {
        self.with_execution_disposition(ExecutionDisposition::Settled)
    }

    pub(crate) fn display_after_commit(primary: Self) -> Self {
        Self {
            disposition: ExecutionDisposition::Settled,
            code: "display_failed",
            message: "回复已提交与登记，但展示刷新失败；未重发模型，请重新打开面板查看",
            source: Box::new(DisplayFailure { primary }),
        }
    }

    /// Retain an independent secondary failure without dropping the primary
    /// result. An `Unknown` secondary makes the combined command result unknown
    /// as well so a storage or display side effect cannot be masked by a settled
    /// business receipt.
    pub(crate) fn with_retained(primary: Self, secondary: Self) -> Self {
        let disposition = if primary.disposition == ExecutionDisposition::Unknown
            || secondary.disposition == ExecutionDisposition::Unknown
        {
            ExecutionDisposition::Unknown
        } else {
            primary.disposition
        };
        Self {
            code: primary.code,
            message: primary.message,
            disposition,
            source: Box::new(RetainedFailure { primary, secondary }),
        }
    }

    /// Inspect the secondary failure retained alongside the primary result.
    pub fn retained_error(&self) -> Option<&AppError> {
        self.source
            .downcast_ref::<RetainedFailure>()
            .map(|failure| &failure.secondary)
    }

    pub(crate) fn cancelled_after_admission(primary: Self) -> Self {
        Self {
            disposition: ExecutionDisposition::Settled,
            code: "send_cancelled",
            message: "该轮登记后失败，账本已确认取消；未登记回复，输入草稿已保留",
            source: Box::new(primary),
        }
    }

    pub(crate) fn unknown_after_admission(primary: Self) -> Self {
        Self {
            disposition: ExecutionDisposition::Unknown,
            code: "send_unknown",
            message: "外部请求结果未知；本地取消不能证明远端已取消，请勿重发",
            source: Box::new(primary),
        }
    }

    pub(crate) fn quarantined_after_admission(primary: Self, cleanup: GatewayError) -> Self {
        Self {
            disposition: ExecutionDisposition::Unknown,
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
            disposition: ExecutionDisposition::Unknown,
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
            disposition: ExecutionDisposition::Unknown,
            code: "storage_failed",
            message: "本地保存或恢复失败，原存档已保留，请检查文件与占用",
            source: Box::new(error),
        }
    }

    /// Record a revision conflict that a trusted persistence adapter observed
    /// before the journal append. The original persistence error remains the
    /// source, while the public result is fixed as `NotExecuted`.
    pub fn memory_revision_conflict<E: Error + 'static>(error: E) -> Self {
        Self {
            disposition: ExecutionDisposition::NotExecuted,
            code: "memory_revision_conflict",
            message: "记忆版本已变化，本次操作未执行；请刷新列表后再操作",
            source: Box::new(error),
        }
    }

    /// A host adapter's preflight proved that no journal append or segment
    /// publication was attempted because a hard capacity budget was exhausted.
    pub fn memory_capacity_exceeded<E: Error + 'static>(error: E) -> Self {
        Self {
            disposition: ExecutionDisposition::NotExecuted,
            code: "memory_capacity_exceeded",
            message: "本地记忆达到硬容量上限，本次操作未执行",
            source: Box::new(error),
        }
    }

    /// A persistence adapter's complete peak-budget preflight rejected a write
    /// before any raw or directory commit began.
    pub fn storage_capacity_exceeded<E: Error + 'static>(error: E) -> Self {
        Self {
            disposition: ExecutionDisposition::NotExecuted,
            code: "storage_capacity_exceeded",
            message: "本地会话存储达到硬容量上限，本次操作未执行",
            source: Box::new(error),
        }
    }

    /// A stale directory CAS or an invalid target rejected before storage work.
    pub fn session_directory_conflict<E: Error + 'static>(error: E) -> Self {
        Self {
            disposition: ExecutionDisposition::NotExecuted,
            code: "session_directory_conflict",
            message: "会话目录已变化或目标无效，本次操作未执行",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_ui(error: UiError) -> Self {
        Self {
            disposition: ExecutionDisposition::Unknown,
            code: "ui_rejected",
            message: "UI 内核拒绝了该面板操作",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_drive_report(report: DriveReport) -> Self {
        Self {
            disposition: ExecutionDisposition::Unknown,
            code: "panel_callback_failed",
            message: "面板驱动失败，原始错误链已保留；请检查面板状态",
            source: Box::new(DriveFailure(report)),
        }
    }

    pub(crate) fn from_gateway(error: GatewayError) -> Self {
        Self {
            disposition: ExecutionDisposition::Unknown,
            code: "gateway_rejected",
            message: "可信入口或会话运行时拒绝了该操作",
            source: Box::new(error),
        }
    }

    /// Preserve a memory adapter error without guessing whether its side effects
    /// settled. Call sites that have authoritative evidence mark the disposition
    /// explicitly instead.
    pub(crate) fn from_memory<E: Error + 'static>(error: E) -> Self {
        Self {
            disposition: ExecutionDisposition::Unknown,
            code: "memory_rejected",
            message: "记忆服务拒绝了该操作",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_diagnostics(error: DiagnosticsError) -> Self {
        Self {
            disposition: ExecutionDisposition::Unknown,
            code: "diagnostics_rejected",
            message: "诊断服务拒绝了该操作",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_workflow_core(error: WorkflowError) -> Self {
        Self {
            disposition: ExecutionDisposition::Unknown,
            code: "chat_phase_failed",
            message: "聊天阶段未能按流程契约推进；未发布成功结果",
            source: Box::new(error),
        }
    }

    pub(crate) fn from_parse(error: crate::ReplyParseError) -> Self {
        Self {
            disposition: ExecutionDisposition::Unknown,
            code: "reply_parse_failed",
            message: "模型回复解析失败，未提交回复",
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
        Self::rejection(code, message, detail).not_executed()
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
            disposition: ExecutionDisposition::Unknown,
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
    fn disposition_is_recorded_by_the_calling_boundary() {
        let rejected = AppError::rejection("synthetic", "safe", "direct rejection");
        assert_eq!(
            rejected.execution_disposition(),
            ExecutionDisposition::Unknown,
            "a plain rejection does not guess from its public code"
        );
        assert_eq!(
            AppError::from_submit(RejectionReason::NotVisible).execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        let cancelled = AppError::cancelled_after_admission(rejected);
        assert_eq!(
            cancelled.execution_disposition(),
            ExecutionDisposition::Settled
        );
        let quarantined = AppError::quarantined_after_admission(
            AppError::from_gateway(GatewayError::Revoked),
            GatewayError::Revoked,
        );
        assert_eq!(
            quarantined.execution_disposition(),
            ExecutionDisposition::Unknown
        );
        assert_eq!(ExecutionDisposition::NotExecuted.as_str(), "not_executed");
        assert_eq!(ExecutionDisposition::Settled.as_str(), "settled");
        assert_eq!(ExecutionDisposition::Unknown.as_str(), "unknown");
    }

    #[test]
    fn memory_error_disposition_requires_explicit_evidence() {
        let conservative = AppError::from_memory(moe_memory_core::MemoryError::NotFound);
        assert_eq!(
            conservative.execution_disposition(),
            ExecutionDisposition::Unknown,
            "the adapter wrapper must not guess an authoritative terminal state"
        );
        let marked = AppError::from_memory(moe_memory_core::MemoryError::NotFound).settled();
        assert_eq!(
            marked.execution_disposition(),
            ExecutionDisposition::Settled
        );
    }

    #[test]
    fn retained_failure_keeps_the_secondary_chain_and_escalates_unknown() {
        let primary = AppError::rejection("business_failed", "safe", "business failed").settled();
        let secondary = AppError::from_storage(SentinelError);
        let combined = AppError::with_retained(primary, secondary);
        assert_eq!(
            combined.execution_disposition(),
            ExecutionDisposition::Unknown,
            "an unknown secondary side effect is not masked by a settled primary"
        );
        assert_eq!(combined.public_code(), "business_failed");
        let retained = combined.retained_error().expect("secondary retained");
        assert_eq!(retained.public_code(), "storage_failed");
        assert!(
            retained
                .source()
                .and_then(|source| source.downcast_ref::<SentinelError>())
                .is_some(),
            "the adapter error stays reachable through the retained failure"
        );
    }

    #[test]
    fn display_failure_after_commit_is_settled_and_keeps_the_source_chain() {
        let primary = AppError::from_gateway(GatewayError::Revoked);
        let error = AppError::display_after_commit(primary);
        assert_eq!(error.public_code(), "display_failed");
        assert_eq!(error.execution_disposition(), ExecutionDisposition::Settled);
        assert!(error.source().is_some());
        let mut source = error.source();
        let mut found_primary = false;
        while let Some(current) = source {
            if current
                .downcast_ref::<AppError>()
                .is_some_and(|current| current.public_code() == "gateway_rejected")
            {
                found_primary = true;
            }
            source = current.source();
        }
        assert!(found_primary, "primary display error stays in the chain");
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
