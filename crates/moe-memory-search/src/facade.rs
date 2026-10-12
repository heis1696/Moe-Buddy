//! Minimal synchronous offline retrieval facade over one `LocalMemory`.
//!
//! The default constructor keeps explicit keyword compatibility. The hybrid
//! constructor receives a sealed host-completed [`RankedCandidates`] and
//! delivers it at most once on an exact identity/session/purpose/query match,
//! always through the local health gate, current-ticket revalidation, and core
//! `complete_retrieval`. Mismatch, expiry, failure, and re-consumption never
//! fall back to keywords.

use moe_memory_core::{
    CandidateRef, CandidateTicket, ContextView, IdentityView, MemoryContextAccess, MemoryTask,
    PetIdentity, ReadPurpose, RequestContext, RetrievalQuery, RetrievalResult, SessionId,
};
use moe_memory_local::LocalMemory;

use crate::{HybridRanking, IndexStatus, RankedCandidates, SearchError, SearchMode};

/// Body-free local ranking diagnostics mirrored from [`HybridRanking`]:
/// mode, missing-index status, hit counts, budget and local ranking time.
/// This is not an end-to-end latency or quality report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchDiagnostic {
    mode: SearchMode,
    status: IndexStatus,
    lexical_hits: usize,
    vector_hits: usize,
    omitted_for_budget: usize,
    used_chars: usize,
    elapsed_micros: u128,
}

impl SearchDiagnostic {
    fn of(ranking: &HybridRanking) -> Self {
        Self {
            mode: ranking.mode(),
            status: ranking.status().clone(),
            lexical_hits: ranking.lexical_hits(),
            vector_hits: ranking.vector_hits(),
            omitted_for_budget: ranking.omitted_for_budget(),
            used_chars: ranking.used_chars(),
            elapsed_micros: ranking.elapsed_micros(),
        }
    }

    pub fn mode(&self) -> SearchMode {
        self.mode
    }

    pub fn status(&self) -> &IndexStatus {
        &self.status
    }

    pub fn lexical_hits(&self) -> usize {
        self.lexical_hits
    }

    pub fn vector_hits(&self) -> usize {
        self.vector_hits
    }

    pub fn omitted_for_budget(&self) -> usize {
        self.omitted_for_budget
    }

    pub fn used_chars(&self) -> usize {
        self.used_chars
    }

    pub fn elapsed_micros(&self) -> u128 {
        self.elapsed_micros
    }
}

/// Delivered task plus optional hybrid diagnostics. `Debug` never shows bodies.
#[derive(Debug)]
pub struct SearchTask {
    task: MemoryTask,
    diagnostic: Option<SearchDiagnostic>,
}

impl SearchTask {
    pub fn task(&self) -> &MemoryTask {
        &self.task
    }

    pub fn into_task(self) -> MemoryTask {
        self.task
    }

    pub fn diagnostic(&self) -> Option<&SearchDiagnostic> {
        self.diagnostic.as_ref()
    }
}

enum Prepared {
    Keyword,
    Armed(Box<ArmedRetrieval>),
    Consumed,
}

struct ArmedRetrieval {
    ticket: CandidateTicket,
    ranking: Vec<(CandidateRef, f64)>,
    diagnostic: SearchDiagnostic,
}

impl ArmedRetrieval {
    fn matches(
        &self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
    ) -> bool {
        self.ticket.identity() == identity
            && self.ticket.session() == session
            && self.ticket.purpose() == purpose
            && self.ticket.query() == query
    }
}

/// Minimal synchronous offline retrieval facade. It composes one
/// [`LocalMemory`] without exposing its mutable internals; the host keeps all
/// authority and time. Nothing here copies the authoritative store.
pub struct SearchMemory {
    memory: LocalMemory,
    prepared: Prepared,
}

impl SearchMemory {
    /// Explicit keyword-compatible facade; read behavior matches `LocalMemory`.
    pub fn new(memory: LocalMemory) -> Self {
        Self {
            memory,
            prepared: Prepared::Keyword,
        }
    }

    /// Receives an already-ranked sealed hybrid result. Storing it authorizes
    /// nothing: every consumption still passes the health gate, current-ticket
    /// revalidation, and core `complete_retrieval` with all candidates, query
    /// proof, persona view, and actual transmission evidence preserved.
    pub fn with_ranked(memory: LocalMemory, ranked: RankedCandidates) -> Self {
        let (ticket, ranking) = ranked.into_parts();
        let diagnostic = SearchDiagnostic::of(&ranking);
        Self {
            memory,
            prepared: Prepared::Armed(Box::new(ArmedRetrieval {
                ticket,
                ranking: ranking.ranking().to_vec(),
                diagnostic,
            })),
        }
    }

    /// Returns the persistent facade to the trusted host; no inner exposure.
    pub fn into_memory(self) -> LocalMemory {
        self.memory
    }

    /// Task entry used by [`MemoryContextAccess`]. In hybrid mode this consumes
    /// the armed retrieval on an exact match only; see [`Self::for_session`].
    /// The armed state is taken by the first exact-match request even if that
    /// delivery then fails — a failed ticket stays closed either way.
    pub fn begin_task(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<SearchTask, SearchError> {
        self.for_session(identity, session, purpose)
            .begin_task(query, now_ms)
    }

    /// Session-bound reader; see [`SearchReader`].
    pub fn for_session(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
    ) -> SearchReader<'_> {
        SearchReader {
            memory: &mut self.memory,
            prepared: &mut self.prepared,
            identity: identity.clone(),
            session: session.clone(),
            purpose,
        }
    }
}

impl MemoryContextAccess for SearchMemory {
    type Error = SearchError;

    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<MemoryTask, Self::Error> {
        Ok(Self::begin_task(self, identity, session, purpose, query, now_ms)?.into_task())
    }

    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget_chars: usize,
        now_ms: u64,
    ) -> Result<RequestContext, Self::Error> {
        Ok(self.memory.assemble_context(task, budget_chars, now_ms)?)
    }

    fn context_view<'context>(
        &mut self,
        context: &'context RequestContext,
        now_ms: u64,
    ) -> Result<&'context ContextView, Self::Error> {
        Ok(self.memory.context_view(context, now_ms)?)
    }
}

/// Session-bound reader over the facade. Keyword mode forwards to
/// `LocalMemory` semantics. Armed hybrid mode delivers the sealed ranking on
/// an exact identity/session/purpose/query match only, at most once; every
/// other request — including plain keyword reads — is refused, because a
/// keyword answer would silently bypass the sealed ranking.
pub struct SearchReader<'session> {
    memory: &'session mut LocalMemory,
    prepared: &'session mut Prepared,
    identity: PetIdentity,
    session: SessionId,
    purpose: ReadPurpose,
}

impl SearchReader<'_> {
    /// Keyword `begin_task` in keyword mode; one-shot sealed delivery in armed
    /// hybrid mode. Health gate and current-ticket revalidation always run
    /// before core `complete_retrieval`.
    pub fn begin_task(
        &mut self,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<SearchTask, SearchError> {
        self.memory.generation()?;
        let request_matches = match &*self.prepared {
            Prepared::Keyword => None,
            Prepared::Consumed => {
                return Err(SearchError::InvalidInput(
                    "prepared hybrid retrieval was already consumed",
                ));
            }
            Prepared::Armed(armed) => {
                Some(armed.matches(&self.identity, &self.session, self.purpose, query))
            }
        };
        match request_matches {
            None => Ok(SearchTask {
                task: self
                    .memory
                    .for_session(&self.identity, &self.session, self.purpose)
                    .begin_task(query, now_ms)?,
                diagnostic: None,
            }),
            Some(false) => Err(SearchError::InvalidInput(
                "request does not match the prepared hybrid retrieval",
            )),
            Some(true) => {
                let armed = match std::mem::replace(&mut *self.prepared, Prepared::Consumed) {
                    Prepared::Armed(armed) => *armed,
                    _ => unreachable!("arm was just matched"),
                };
                self.memory
                    .validate_candidate_ticket(&armed.ticket, now_ms)?;
                let task = self
                    .memory
                    .for_session(&self.identity, &self.session, self.purpose)
                    .complete_retrieval(&armed.ticket, &armed.ranking, now_ms)?;
                Ok(SearchTask {
                    task,
                    diagnostic: Some(armed.diagnostic),
                })
            }
        }
    }

    /// Keyword list read. Refused in armed hybrid mode: the sealed ranking is
    /// the only authorized delivery for this session state.
    pub fn retrieve(
        &mut self,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<RetrievalResult, SearchError> {
        self.memory.generation()?;
        if !matches!(&*self.prepared, Prepared::Keyword) {
            return Err(SearchError::InvalidInput(
                "hybrid mode delivers only the sealed ranking",
            ));
        }
        Ok(self
            .memory
            .for_session(&self.identity, &self.session, self.purpose)
            .retrieve(query, now_ms)?)
    }

    /// Persona anchor through the local health gate.
    pub fn identity(&mut self) -> Result<IdentityView, SearchError> {
        Ok(self
            .memory
            .for_session(&self.identity, &self.session, self.purpose)
            .identity()?)
    }
}
