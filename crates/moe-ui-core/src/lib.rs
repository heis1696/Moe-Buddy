//! UI 管理与呈现框架内核(见 crate README)。

#![doc = include_str!("../README.md")]

mod config;
mod controller;
mod error;
mod identity;
mod manager;
mod panel;
mod registry;
mod view;

pub use config::UiConfig;
pub use controller::{CloseConfirmation, CloseDecision, UiController};
pub use error::{AggregateError, PendingReleaseId, ReleaseCause, Stage, UiError};
pub use identity::{
    PanelHandle, PanelKey, PanelKind, PanelScope, PanelSummary, ScopeIdentity, ScopeLevel,
    Visibility,
};
pub use manager::{
    CloseRequest, DriveReport, LoadTicket, NavigateFault, OpenOutcome, PanelFault, RejectedSubmit,
    RejectionReason, ScopeReleaseFault, ScopeReleaseReport, UiManager,
};
pub use panel::{NavSink, Navigate, UiPanel};
pub use registry::{PanelEntry, PendingReleaseSummary};
pub use view::{EventSink, UiView};
