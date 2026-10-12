use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Weak};

use moe_memory_core::{
    CandidateRef, CandidateTicket, EmbeddingProfile, MemoryId, MemoryScope, RecordVersion,
    RetrievalItem, SearchScope,
};
use sha2::{Digest, Sha256};

use crate::{
    ChunkConfig, EncodedQuery, RetrievalHost, SearchError, TextChunk, cache::FileCache,
    split_chunks, vector::UnitVector,
};

/// Trusted host binding, not a request-supplied tenant or a read capability.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StorageDomain(pub(crate) String);

impl StorageDomain {
    pub fn new(value: impl Into<String>) -> Result<Self, SearchError> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(SearchError::InvalidConfig("invalid storage domain"));
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for StorageDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StorageDomain([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Hybrid,
    KeywordOnly,
    PartialIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchReason {
    PartialIndex,
    QueueFull,
    ProfileMismatch,
    CacheUnavailable,
    RerankUnavailable,
    ModelUnavailable,
    TransmissionDenied,
}

/// All counts describe only this ticket's currently authorized candidate set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexStatus {
    pub candidates: usize,
    pub indexed_records: usize,
    pub missing_chunks: usize,
    pub queued_records: usize,
    pub reasons: Vec<SearchReason>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RecordKey(pub(crate) MemoryScope, pub(crate) MemoryId);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct IndexKey {
    pub(crate) domain: StorageDomain,
    pub(crate) scope: MemoryScope,
    pub(crate) parent: RecordVersion,
    pub(crate) config: ChunkConfig,
    pub(crate) ordinal: usize,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) profile: EmbeddingProfile,
}

impl IndexKey {
    pub(crate) fn record_key(&self) -> RecordKey {
        RecordKey(self.scope.clone(), self.parent.id())
    }
}

pub(crate) struct StoredVector {
    pub(crate) input: RetrievalItem,
    pub(crate) vector: UnitVector,
}

pub(crate) struct IndexSlice {
    pub(crate) key: IndexKey,
    pub(crate) reference: CandidateRef,
    pub(crate) input: RetrievalItem,
    pub(crate) chunk: TextChunk,
}

/// Sealed by an index from current core-authorized inputs; not constructible from IDs.
pub struct IndexBatch {
    pub(crate) lease: Arc<()>,
    pub(crate) domain: StorageDomain,
    pub(crate) generation: u64,
    pub(crate) profile: EmbeddingProfile,
    pub(crate) config: ChunkConfig,
    pub(crate) slices: Vec<IndexSlice>,
}

impl IndexBatch {
    pub fn len(&self) -> usize {
        self.slices.len()
    }
    pub fn is_empty(&self) -> bool {
        self.slices.is_empty()
    }
    pub(crate) fn validate(&self, ticket: &CandidateTicket) -> Result<(), SearchError> {
        if &self.profile != ticket.plan().profile() {
            return Err(SearchError::ProfileMismatch);
        }
        if self.slices.is_empty() || self.slices.len() > 32 {
            return Err(SearchError::InvalidInput("invalid index batch"));
        }
        for slice in &self.slices {
            if ticket.candidate(&slice.reference) != Some(&slice.input) {
                return Err(SearchError::StaleIndexWork);
            }
            let expected = split_chunks(slice.input.text(), &self.config)?;
            if expected.get(slice.chunk.ordinal()) != Some(&slice.chunk) {
                return Err(SearchError::StaleIndexWork);
            }
        }
        Ok(())
    }
}

/// Produced only by the authorized embedding adapter. No bare-vector insertion API exists.
pub struct CompletedIndexBatch {
    pub(crate) batch: IndexBatch,
    pub(crate) vectors: Vec<UnitVector>,
}

impl fmt::Debug for IndexBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IndexBatch([redacted])")
    }
}
impl fmt::Debug for CompletedIndexBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CompletedIndexBatch([redacted])")
    }
}

/// Disposable derived state only. Construction and all ranking are network-free.
pub struct SearchIndex {
    pub(crate) domain: StorageDomain,
    pub(crate) profile: EmbeddingProfile,
    pub(crate) config: ChunkConfig,
    pub(crate) entries: BTreeMap<IndexKey, StoredVector>,
    queue: BTreeMap<RecordKey, RetrievalItem>,
    queue_capacity: usize,
    batch_limit: usize,
    pub(crate) vector_capacity: usize,
    generation: u64,
    in_flight: Weak<()>,
    pub(crate) cache: Option<FileCache>,
    pub(crate) notices: Vec<SearchReason>,
    pub(crate) calibration: Option<crate::HybridConfig>,
}

impl fmt::Debug for SearchIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SearchIndex([redacted])")
    }
}

impl SearchIndex {
    pub fn new(
        domain: StorageDomain,
        profile: EmbeddingProfile,
        config: ChunkConfig,
        queue_capacity: usize,
        batch_limit: usize,
        vector_capacity: usize,
    ) -> Result<Self, SearchError> {
        if !(1..=4096).contains(&queue_capacity)
            || !(1..=32).contains(&batch_limit)
            || !(1..=65536).contains(&vector_capacity)
        {
            return Err(SearchError::InvalidConfig("invalid index capacities"));
        }
        Ok(Self {
            domain,
            profile,
            config,
            entries: BTreeMap::new(),
            queue: BTreeMap::new(),
            queue_capacity,
            batch_limit,
            vector_capacity,
            generation: 1,
            in_flight: Weak::new(),
            cache: None,
            notices: Vec::new(),
            calibration: None,
        })
    }

    /// A trusted configuration change invalidates in-flight work and old vectors.
    pub fn rebuild<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        profile: EmbeddingProfile,
        config: ChunkConfig,
    ) -> Result<(), SearchError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(SearchError::InvalidConfig("index generation exhausted"))?;
        host.authority().cancel_current_tasks()?;
        self.generation = generation;
        self.profile = profile;
        self.config = config;
        self.entries.clear();
        self.queue.clear();
        self.in_flight = Weak::new();
        self.calibration = None;
        self.notices.push(SearchReason::ProfileMismatch);
        Ok(())
    }

    pub fn chunk_config(&self) -> &ChunkConfig {
        &self.config
    }

    pub(crate) fn bind_calibration<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        config: &crate::HybridConfig,
    ) -> Result<(), SearchError> {
        if self
            .calibration
            .as_ref()
            .is_some_and(|current| current != config)
        {
            host.authority().cancel_current_tasks()?;
            self.calibration = Some(config.clone());
            return Err(moe_memory_core::MemoryError::TaskCancelled.into());
        }
        self.calibration = Some(config.clone());
        Ok(())
    }

    pub(crate) fn authorize<Host: RetrievalHost>(
        &self,
        host: &mut Host,
        ticket: &CandidateTicket,
    ) -> Result<(), SearchError> {
        let now = host.now_ms();
        host.authority().validate_candidate_ticket(ticket, now)?;
        if ticket.plan().profile() != &self.profile {
            return Err(SearchError::ProfileMismatch);
        }
        Ok(())
    }

    pub(crate) fn expected(
        &self,
        ticket: &CandidateTicket,
    ) -> Result<Vec<IndexSlice>, SearchError> {
        let mut slices = Vec::new();
        for candidate in ticket.candidates() {
            let RetrievalItem::LongTerm(record) = candidate.item() else {
                continue;
            };
            let fingerprint: [u8; 32] = Sha256::digest(record.text().as_bytes()).into();
            for chunk in split_chunks(record.text(), &self.config)? {
                let key = IndexKey {
                    domain: self.domain.clone(),
                    scope: record.scope().clone(),
                    parent: record.version_ref(),
                    config: self.config.clone(),
                    ordinal: chunk.ordinal(),
                    start: chunk.start_char(),
                    end: chunk.end_char(),
                    fingerprint,
                    profile: self.profile.clone(),
                };
                slices.push(IndexSlice {
                    key,
                    reference: candidate.candidate_ref(),
                    input: candidate.item().clone(),
                    chunk,
                });
            }
        }
        Ok(slices)
    }

    fn in_scope(ticket: &CandidateTicket, scope: &MemoryScope) -> bool {
        match scope {
            MemoryScope::Private(owner) => {
                owner == ticket.identity() && ticket.query().scope() != SearchScope::Shared
            }
            MemoryScope::Shared(namespace) => {
                namespace == ticket.identity().namespace()
                    && ticket.query().scope() != SearchScope::Private
            }
        }
    }

    /// Missing notifications are repaired by comparison with the complete current ticket.
    /// Queue merging is record-level; no authoritative commit is performed here.
    pub fn reconcile<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        ticket: &CandidateTicket,
    ) -> Result<IndexStatus, SearchError> {
        self.authorize(host, ticket)?;
        let expected = self.expected(ticket)?;
        let allowed: BTreeMap<_, _> = expected
            .iter()
            .map(|slice| (slice.key.clone(), &slice.input))
            .collect();
        self.entries.retain(|key, stored| {
            !Self::in_scope(ticket, &key.scope)
                || allowed
                    .get(key)
                    .is_some_and(|input| *input == &stored.input)
        });
        let records: BTreeMap<_, _> = expected
            .iter()
            .map(|slice| (slice.key.record_key(), &slice.input))
            .collect();
        self.queue.retain(|key, input| {
            !Self::in_scope(ticket, &key.0)
                || records.get(key).is_some_and(|current| *current == input)
        });
        let mut missing = BTreeSet::new();
        let mut present = BTreeSet::new();
        let mut status = IndexStatus {
            candidates: ticket.candidates().len(),
            reasons: std::mem::take(&mut self.notices),
            ..IndexStatus::default()
        };
        for slice in expected {
            if self.entries.contains_key(&slice.key) {
                present.insert(slice.key.record_key());
                continue;
            }
            status.missing_chunks += 1;
            let record = slice.key.record_key();
            missing.insert(record.clone());
            if self.queue.contains_key(&record) || self.queue.len() < self.queue_capacity {
                self.queue.insert(record, slice.input);
            } else if !status.reasons.contains(&SearchReason::QueueFull) {
                status.reasons.push(SearchReason::QueueFull);
            }
        }
        status.indexed_records = present.len();
        status.queued_records = missing
            .iter()
            .filter(|key| self.queue.contains_key(*key))
            .count();
        if status.missing_chunks > 0 {
            status.reasons.push(SearchReason::PartialIndex);
        }
        Ok(status)
    }

    /// The caller runs at most one batch at a time; a batch has at most 32 slices.
    pub fn take_batch<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        ticket: &CandidateTicket,
    ) -> Result<Option<IndexBatch>, SearchError> {
        self.reconcile(host, ticket)?;
        if self.in_flight.upgrade().is_some() {
            return Err(SearchError::InvalidInput("index batch already in flight"));
        }
        let slices: Vec<_> = self
            .expected(ticket)?
            .into_iter()
            .filter(|slice| {
                self.queue.get(&slice.key.record_key()) == Some(&slice.input)
                    && !self.entries.contains_key(&slice.key)
            })
            .take(self.batch_limit)
            .collect();
        if slices.is_empty() {
            return Ok(None);
        }
        let lease = Arc::new(());
        self.in_flight = Arc::downgrade(&lease);
        Ok(Some(IndexBatch {
            lease,
            domain: self.domain.clone(),
            generation: self.generation,
            profile: self.profile.clone(),
            config: self.config.clone(),
            slices,
        }))
    }

    /// Late, revoked, wrong-profile and foreign-ticket results cannot change active state.
    pub fn commit<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        ticket: &CandidateTicket,
        completed: CompletedIndexBatch,
    ) -> Result<(), SearchError> {
        self.authorize(host, ticket)?;
        let batch = completed.batch;
        batch.validate(ticket)?;
        if batch.domain != self.domain
            || batch.generation != self.generation
            || batch.config != self.config
            || batch.slices.len() != completed.vectors.len()
            || !self.in_flight.ptr_eq(&Arc::downgrade(&batch.lease))
        {
            return Err(SearchError::StaleIndexWork);
        }
        let added = batch
            .slices
            .iter()
            .filter(|slice| !self.entries.contains_key(&slice.key))
            .count();
        if added > self.vector_capacity.saturating_sub(self.entries.len()) {
            return Err(SearchError::InvalidInput("index vector capacity exceeded"));
        }
        for (slice, vector) in batch.slices.into_iter().zip(completed.vectors) {
            self.entries.insert(
                slice.key,
                StoredVector {
                    input: slice.input,
                    vector,
                },
            );
        }
        let remaining: BTreeSet<_> = self
            .expected(ticket)?
            .into_iter()
            .filter(|slice| !self.entries.contains_key(&slice.key))
            .map(|slice| slice.key.record_key())
            .collect();
        self.queue
            .retain(|key, _| !Self::in_scope(ticket, &key.0) || remaining.contains(key));
        Ok(())
    }

    pub(crate) fn vector_rank(
        &self,
        ticket: &CandidateTicket,
        query: &EncodedQuery,
        minimum: f64,
    ) -> Result<Vec<(CandidateRef, f64)>, SearchError> {
        if query.profile != self.profile
            || query.text != ticket.query().text()
            || query.text.is_empty()
        {
            return Err(SearchError::ProfileMismatch);
        }
        let mut records: BTreeMap<CandidateRef, f64> = BTreeMap::new();
        for slice in self.expected(ticket)? {
            if let Some(stored) = self
                .entries
                .get(&slice.key)
                .filter(|stored| stored.input == slice.input)
            {
                let score = query.vector.cosine(&stored.vector);
                if score >= minimum {
                    records
                        .entry(slice.reference)
                        .and_modify(|current| *current = current.max(score))
                        .or_insert(score);
                }
            }
        }
        let mut ranked: Vec<_> = records.into_iter().collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok(ranked)
    }

    pub(crate) fn rerank_chunks(
        &self,
        ticket: &CandidateTicket,
        entries: &[CandidateRef],
        query: Option<&EncodedQuery>,
    ) -> Result<Vec<String>, SearchError> {
        let slices = self.expected(ticket)?;
        entries
            .iter()
            .map(|reference| {
                let item = ticket
                    .candidate(reference)
                    .ok_or(SearchError::InvalidInput("foreign rerank reference"))?;
                let mut best = None;
                for slice in slices.iter().filter(|slice| &slice.reference == reference) {
                    let semantic = query
                        .and_then(|query| {
                            self.entries
                                .get(&slice.key)
                                .map(|stored| query.vector.cosine(&stored.vector))
                        })
                        .unwrap_or(-1.0);
                    let exact = !ticket.query().text().is_empty()
                        && slice.chunk.text().contains(ticket.query().text());
                    let score = (
                        exact,
                        crate::lexical::matching_terms(ticket.query().text(), slice.chunk.text()),
                        semantic,
                    );
                    if best.as_ref().is_none_or(|(current, _)| score > *current) {
                        best = Some((score, slice.chunk.text().to_owned()));
                    }
                }
                match best {
                    Some((_, text)) => Ok(text),
                    None => split_chunks(item.text(), &self.config)?
                        .into_iter()
                        .max_by_key(|chunk| {
                            crate::lexical::matching_terms(ticket.query().text(), chunk.text())
                        })
                        .map(|chunk| chunk.text().to_owned())
                        .ok_or(SearchError::InputTooLong),
                }
            })
            .collect()
    }

    /// Explicit opt-in. The existing protected directory is host-selected.
    pub fn enable_file_cache(
        &mut self,
        protected_directory: &Path,
        max_bytes: usize,
    ) -> Result<(), SearchError> {
        self.cache = Some(FileCache::new(protected_directory, max_bytes)?);
        Ok(())
    }
}
