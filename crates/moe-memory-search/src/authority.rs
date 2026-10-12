//! Narrow authority bridge for R2 algorithms and persistent-memory hosts.
//!
//! Retrieval code only ever sees [`SearchAuthority`] — the health-checked
//! ticket/transmission operations — plus injected time through
//! [`RetrievalHost`]. Neither trait exposes a kernel service, journal, CRUD,
//! or any other read entry. Legacy [`SearchHost`] implementations keep working
//! through the blanket [`RetrievalHost`] impl.

use moe_memory_core::{
    CandidateRef, CandidateTicket, MemoryService, TransmissionTicket, TransmissionUsage,
};
use moe_memory_local::{LocalMemory, OperationId};

use crate::{SearchError, SearchHost};

/// Health-checked authority operations only. Implementations must keep every
/// existing fence: a failed or revoked ticket stays closed, and persistence
/// failures such as [`moe_memory_local::PersistenceError::RecoveryRequired`]
/// are surfaced verbatim, never as degradable ranking errors.
pub trait SearchAuthority {
    fn validate_candidate_ticket(
        &mut self,
        ticket: &CandidateTicket,
        now_ms: u64,
    ) -> Result<(), SearchError>;

    fn recover_failed_rerank(
        &mut self,
        ticket: &mut CandidateTicket,
        now_ms: u64,
    ) -> Result<CandidateTicket, SearchError>;

    fn prepare_transmission(
        &mut self,
        ticket: &CandidateTicket,
        usage: TransmissionUsage,
        endpoint: &str,
        entries: &[CandidateRef],
        now_ms: u64,
    ) -> Result<TransmissionTicket, SearchError>;

    fn record_transmission(
        &mut self,
        ticket: &mut CandidateTicket,
        sent: &TransmissionTicket,
        now_ms: u64,
    ) -> Result<(), SearchError>;

    fn fail_transmission(
        &mut self,
        ticket: &mut CandidateTicket,
        sent: &TransmissionTicket,
        now_ms: u64,
    ) -> Result<(), SearchError>;

    /// Cancels all in-flight tasks at the authority's current generation in one
    /// checked step (the `cancel_tasks` fence plus generation read).
    fn cancel_current_tasks(&mut self) -> Result<(), SearchError>;
}

/// Host binding for retrieval algorithms: an authority bridge plus injected
/// time. Only a trusted host may implement it; model input cannot construct
/// authority.
pub trait RetrievalHost {
    fn authority(&mut self) -> &mut dyn SearchAuthority;
    fn now_ms(&mut self) -> u64;
}

impl<Host: SearchHost> RetrievalHost for Host {
    fn authority(&mut self) -> &mut dyn SearchAuthority {
        self.memory()
    }

    fn now_ms(&mut self) -> u64 {
        SearchHost::now_ms(self)
    }
}

impl SearchAuthority for MemoryService {
    fn validate_candidate_ticket(
        &mut self,
        ticket: &CandidateTicket,
        now_ms: u64,
    ) -> Result<(), SearchError> {
        Ok(MemoryService::validate_candidate_ticket(
            self, ticket, now_ms,
        )?)
    }

    fn recover_failed_rerank(
        &mut self,
        ticket: &mut CandidateTicket,
        now_ms: u64,
    ) -> Result<CandidateTicket, SearchError> {
        Ok(MemoryService::recover_failed_rerank(self, ticket, now_ms)?)
    }

    fn prepare_transmission(
        &mut self,
        ticket: &CandidateTicket,
        usage: TransmissionUsage,
        endpoint: &str,
        entries: &[CandidateRef],
        now_ms: u64,
    ) -> Result<TransmissionTicket, SearchError> {
        Ok(MemoryService::prepare_transmission(
            self, ticket, usage, endpoint, entries, now_ms,
        )?)
    }

    fn record_transmission(
        &mut self,
        ticket: &mut CandidateTicket,
        sent: &TransmissionTicket,
        now_ms: u64,
    ) -> Result<(), SearchError> {
        Ok(MemoryService::record_transmission(
            self, ticket, sent, now_ms,
        )?)
    }

    fn fail_transmission(
        &mut self,
        ticket: &mut CandidateTicket,
        sent: &TransmissionTicket,
        now_ms: u64,
    ) -> Result<(), SearchError> {
        Ok(MemoryService::fail_transmission(
            self, ticket, sent, now_ms,
        )?)
    }

    fn cancel_current_tasks(&mut self) -> Result<(), SearchError> {
        let generation = MemoryService::generation(self);
        MemoryService::cancel_tasks(self, generation)?;
        Ok(())
    }
}

impl SearchAuthority for LocalMemory {
    fn validate_candidate_ticket(
        &mut self,
        ticket: &CandidateTicket,
        now_ms: u64,
    ) -> Result<(), SearchError> {
        Ok(LocalMemory::validate_candidate_ticket(
            self, ticket, now_ms,
        )?)
    }

    fn recover_failed_rerank(
        &mut self,
        ticket: &mut CandidateTicket,
        now_ms: u64,
    ) -> Result<CandidateTicket, SearchError> {
        Ok(LocalMemory::recover_failed_rerank(self, ticket, now_ms)?)
    }

    fn prepare_transmission(
        &mut self,
        ticket: &CandidateTicket,
        usage: TransmissionUsage,
        endpoint: &str,
        entries: &[CandidateRef],
        now_ms: u64,
    ) -> Result<TransmissionTicket, SearchError> {
        Ok(LocalMemory::prepare_transmission(
            self, ticket, usage, endpoint, entries, now_ms,
        )?)
    }

    fn record_transmission(
        &mut self,
        ticket: &mut CandidateTicket,
        sent: &TransmissionTicket,
        now_ms: u64,
    ) -> Result<(), SearchError> {
        Ok(LocalMemory::record_transmission(
            self, ticket, sent, now_ms,
        )?)
    }

    fn fail_transmission(
        &mut self,
        ticket: &mut CandidateTicket,
        sent: &TransmissionTicket,
        now_ms: u64,
    ) -> Result<(), SearchError> {
        Ok(LocalMemory::fail_transmission(self, ticket, sent, now_ms)?)
    }

    fn cancel_current_tasks(&mut self) -> Result<(), SearchError> {
        let generation = LocalMemory::generation(self)?;
        let operation = OperationId::new(format!("moe-memory-search-cancel-{generation}"))?;
        LocalMemory::cancel_tasks(self, operation, generation)?;
        Ok(())
    }
}

/// Trusted-host adapter that explicitly borrows the persistent facade and an
/// injected clock to drive R2 ranking. It never exposes `LocalMemory`
/// internals and cannot be constructed from model input.
pub struct LocalSearchHost<'memory> {
    memory: &'memory mut LocalMemory,
    now_ms: u64,
}

impl<'memory> LocalSearchHost<'memory> {
    pub fn new(memory: &'memory mut LocalMemory, now_ms: u64) -> Self {
        Self { memory, now_ms }
    }

    /// The host advances its own injected clock between checkpoints.
    pub fn set_now_ms(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
    }
}

impl RetrievalHost for LocalSearchHost<'_> {
    fn authority(&mut self) -> &mut dyn SearchAuthority {
        self.memory
    }

    fn now_ms(&mut self) -> u64 {
        self.now_ms
    }
}
