#![doc = include_str!("../README.md")]

mod agent;
mod error;
mod hooks;
mod ingress;
mod preset;
mod session;
mod validation;

pub use agent::{
    AgentAccess, AgentBinding, AgentDelivery, AgentError, AgentLimits, AgentResult, AgentRuntime,
    AgentServices, AgentTask, ModelFailure, ModelOutcome, OfflineInput, OfflineModel,
    PresentationEvent,
};
pub use error::GatewayError;
pub use hooks::{
    HookBinding, HookBindingConfig, HookContext, HookEvent, HookInvocation, HookLimits,
    HookPermissions, HookReceipt, HookReply, HookReplyRole, HookRuntime, HookTransportOutcome,
    UnverifiedPrompt,
};
pub use ingress::{
    CallerSubject, ChannelId, ChannelScope, CredentialId, HintIsolation, INGRESS_MAX_TEXT_CHARS,
    IngressAdmission, IngressLimits, IngressToken, RawIngressCall, ReplyTarget, RequestIsolation,
    TrustedIngress, TrustedRequest, UnsupportedFeature,
};
pub use preset::{
    CredentialRef, ExtensionValue, PresetConfig, PresetId, PresetPurpose, PresetRegistry,
    PresetVersion, ResolvedPreset,
};
pub use session::{
    BatchAcknowledgement, BatchProgress, BatchTicket, EventId, EventRole, HostEventId,
    InteractionId, LifecycleReceipt, MaintenanceReport, ManagedFragmentSnapshot,
    ManagedFragmentSummary, ManagementCapability, ManagementPermissions, PurgeReceipt,
    RAW_SNAPSHOT_MAX_TOTAL_BYTES, RawDomainKey, RawHistoryGrant, RawImportReceipt, RawRetention,
    RawSnapshotSweep, RawSnapshotV1, RecycleEmptyReport, Registration, RelatedEvent,
    RestoredRawFragment, RetentionZone, SessionAccess, SessionId, SessionLimits, SessionRuntime,
    SessionSnapshot, SessionTiming, StagedEvent, TerminalReceipt, TerminalState, UserEvent,
    UserEventKind,
};
