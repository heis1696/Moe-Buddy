#![doc = include_str!("../README.md")]

mod api;
mod command;
mod context;
mod error;
mod management;
mod model;
mod policy;
mod publication;
mod read;
mod store;
mod validation;

pub use api::{MemoryClient, MemoryService};
pub use command::{CommandResult, MemoryCommand};
pub use context::access::MemoryContextAccess;
pub use context::{
    CompactionTask, ContextBlock, ContextBlockKind, ContextEvidence, ContextOrigin, ContextUsage,
    ContextView, RequestContext,
};
pub use error::MemoryError;
pub use management::{MemoryStatus, MemoryTask, RecordState, RecordVersion, ReplacementReceipt};
pub use model::{
    Category, Epistemic, Lifecycle, MemoryDraft, MemoryId, MemoryRecord, MemoryScope, Namespace,
    PetIdentity, SearchQuery, SearchScope, Sensitivity, SourceRef, WriteOperation, WriteReceipt,
};
pub use publication::{PublicationPolicy, SharedCandidate};
pub use read::{
    AuthorizedCandidate, CacheStatus, CandidateRef, CandidateTicket, EmbeddingProfile, EntityRead,
    EntityView, IdentityView, MemoryReader, QueryProof, ReadPolicy, ReadPurpose, RerankProfile,
    RetrievalItem, RetrievalLayer, RetrievalPlan, RetrievalQuery, RetrievalResult, SessionId,
    TransmissionEvidence, TransmissionPolicy, TransmissionTicket, TransmissionUsage,
    WorkingContext,
};
