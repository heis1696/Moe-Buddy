//! 文字聊天应用的受信业务组合边界：复用记忆、API 预设、Agent 流程、
//! 诊断与 UI 管理，独立于传输与渲染。契约见 `contract`，用法见 README。

mod app;
pub mod contract;
mod error;
pub mod panels;
mod services;
mod storage;
pub use storage::{ChatHistory, ChatMemory, HistoryAccess};

pub use app::ChatApplication;
pub use contract::*;
pub use error::AppError;
pub use panels::{
    ChatController, ChatDisplay, ChatEvent, ChatModel, ChatView, DiagnosticsController,
    DiagnosticsDisplay, DiagnosticsEvent, DiagnosticsModel, DiagnosticsView, MemoryController,
    MemoryDisplay, MemoryEvent, MemoryModel, MemoryView, PanelArgs, PanelCounters, PanelError,
    PresetsController, PresetsDisplay, PresetsEvent, PresetsModel, PresetsView,
};
