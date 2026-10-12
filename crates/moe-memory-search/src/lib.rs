//! Explicitly authorized online encoding and pure exact vector scanning.
//!
//! Construction never sends requests. Online operations require a trusted
//! [`SearchHost`] (or the narrower [`RetrievalHost`]), sealed core tickets, and
//! separate transmission permissions. Derived indexes and optional disposable
//! caches never grant read authority. The offline [`SearchMemory`] facade keeps
//! keyword compatibility and delivers sealed hybrid rankings at most once;
//! explicit offline Agent injection uses the core context contract. Implicit
//! model fallback is never provided.

#![doc = include_str!("../README.md")]

mod authority;
mod cache;
mod chunk;
mod facade;
mod hybrid;
mod index;
mod lexical;
mod online;
mod vector;

use std::fmt;

pub use authority::{LocalSearchHost, RetrievalHost, SearchAuthority};
pub use chunk::{ChunkConfig, TextChunk, split_chunks};
pub use facade::{SearchDiagnostic, SearchMemory, SearchReader, SearchTask};
pub use hybrid::{
    HybridConfig, HybridRanking, RankedCandidates, RankingFailure, RerankMode, hybrid_rank,
    rerank_or_fallback,
};
pub use index::{
    CompletedIndexBatch, IndexBatch, IndexStatus, SearchIndex, SearchMode, SearchReason,
    StorageDomain,
};
pub use lexical::{LexicalHit, lexical_rank};
pub use online::{CallControl, Cancellation, CohereReranker, OnlineEmbedding, OnlineOptions};
pub use vector::{EmbeddedCandidate, EncodedQuery, exact_scan};

/// Only a trusted host may supply current authority and wall-clock timestamps.
/// Implementations may refresh/revoke permissions at each checkpoint.
pub trait SearchHost {
    fn memory(&mut self) -> &mut moe_memory_core::MemoryService;
    fn now_ms(&mut self) -> u64;
}

/// Contains no credentials, request/response bodies, or transport error strings.
/// Core errors keep their own variant; persistence failures carry the original
/// [`moe_memory_local::PersistenceError`] and are never ranking degradations.
#[derive(Debug)]
pub enum SearchError {
    Core(moe_memory_core::MemoryError),
    Persistence(moe_memory_local::PersistenceError),
    InvalidConfig(&'static str),
    InvalidInput(&'static str),
    InvalidResponse(&'static str),
    ProfileMismatch,
    InputTooLong,
    ResponseTooLarge,
    Cancelled,
    DeadlineExceeded,
    RequestTimedOut,
    TransportUnavailable,
    HttpStatus(u16),
    StaleIndexWork,
    CacheUnavailable,
}

impl From<moe_memory_core::MemoryError> for SearchError {
    fn from(error: moe_memory_core::MemoryError) -> Self {
        Self::Core(error)
    }
}

impl From<moe_memory_local::PersistenceError> for SearchError {
    fn from(error: moe_memory_local::PersistenceError) -> Self {
        Self::Persistence(error)
    }
}

impl PartialEq for SearchError {
    fn eq(&self, other: &Self) -> bool {
        use moe_memory_local::PersistenceError as Persistence;
        match (self, other) {
            (Self::Core(left), Self::Core(right)) => left == right,
            (Self::Persistence(left), Self::Persistence(right)) => match (left, right) {
                (Persistence::Io(left), Persistence::Io(right)) => left.kind() == right.kind(),
                (
                    Persistence::Corrupt {
                        offset: left,
                        reason: left_reason,
                    },
                    Persistence::Corrupt {
                        offset: right,
                        reason: right_reason,
                    },
                ) => left == right && left_reason == right_reason,
                (Persistence::UnsupportedVersion(left), Persistence::UnsupportedVersion(right)) => {
                    left == right
                }
                (Persistence::Memory(left), Persistence::Memory(right)) => left == right,
                (Persistence::Locked, Persistence::Locked)
                | (Persistence::OperationConflict, Persistence::OperationConflict)
                | (Persistence::RecoveryRequired, Persistence::RecoveryRequired)
                | (Persistence::CapacityExceeded, Persistence::CapacityExceeded) => true,
                _ => false,
            },
            (Self::InvalidConfig(left), Self::InvalidConfig(right))
            | (Self::InvalidInput(left), Self::InvalidInput(right))
            | (Self::InvalidResponse(left), Self::InvalidResponse(right)) => left == right,
            (Self::ProfileMismatch, Self::ProfileMismatch)
            | (Self::InputTooLong, Self::InputTooLong)
            | (Self::ResponseTooLarge, Self::ResponseTooLarge)
            | (Self::Cancelled, Self::Cancelled)
            | (Self::DeadlineExceeded, Self::DeadlineExceeded)
            | (Self::RequestTimedOut, Self::RequestTimedOut)
            | (Self::TransportUnavailable, Self::TransportUnavailable)
            | (Self::StaleIndexWork, Self::StaleIndexWork)
            | (Self::CacheUnavailable, Self::CacheUnavailable) => true,
            (Self::HttpStatus(left), Self::HttpStatus(right)) => left == right,
            _ => false,
        }
    }
}

impl fmt::Display for SearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Eq for SearchError {}

impl std::error::Error for SearchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(error) => Some(error),
            Self::Persistence(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
