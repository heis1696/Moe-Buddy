//! 文字聊天应用的受信业务组合边界：经统一聊天工作流复用记忆、API 预设、
//! Agent、诊断与 UI 管理，独立于传输与渲染。契约见 `contract`，用法见 README。

mod app;
mod buddy;
pub mod contract;
mod error;
pub mod panels;
mod services;
mod storage;
pub use storage::{
    ChatHistory, ChatMemory, ColdHistoryMaintenance, ColdMaintenanceBlock, DirectoryPublishReceipt,
    HistoryAccess, HistoryChange, HistoryCommitReceipts, HistoryMaintenanceReport, HistoryRecovery,
    HistoryRequest, HistorySendReservation, HistorySession, RawHistoryCommitReceipt,
};

pub use app::ChatApplication;
pub use buddy::{
    BuddyError, BuddyId, BuddyModelConfiguration, BuddyProfile, BuddyRegistry,
    BuddyRegistrySnapshot, BuddySummary, MAX_BUDDIES, MAX_BUDDY_ID_BYTES, MAX_BUDDY_LABEL_CHARS,
    MAX_PERSONA_CHARS,
};
pub use contract::*;
pub use error::{AppError, ExecutionDisposition};
pub use panels::{
    ChatController, ChatDisplay, ChatEvent, ChatModel, ChatView, DiagnosticsController,
    DiagnosticsDisplay, DiagnosticsEvent, DiagnosticsModel, DiagnosticsView, MemoryController,
    MemoryDisplay, MemoryEvent, MemoryModel, MemoryView, PanelArgs, PanelCounters, PanelError,
    PresetsController, PresetsDisplay, PresetsEvent, PresetsModel, PresetsView,
};
