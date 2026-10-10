#![doc = include_str!("../README.md")]

mod api;
mod dialogue;
mod error;
mod journal;

pub use api::{
    CleanupState, Committed, DeletionBoundary, Durability, LocalMemory, LocalMemoryClient,
    LocalMemoryReader, OperationId,
};
pub use dialogue::{
    ContextPath, ContextPreference, ContextReport, CorrectionDecision, CorrectionReview,
    DialogueCommit, DialogueError, DialogueFailure, DialogueReceipt, DialogueStage, HostDecision,
    OfflineDialogue, OfflineFixture, OfflineProposal, OfflineReview, OfflineTurn,
};
pub use error::PersistenceError;
