use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use moe_memory_core::{
    CandidateRef, CandidateTicket, EmbeddingProfile, QueryProof, RetrievalItem, RetrievalPlan,
    RetrievalQuery,
};

use crate::{
    CallControl, CohereReranker, EncodedQuery, IndexStatus, RetrievalHost, SearchError,
    SearchIndex, SearchMode, SearchReason, lexical_rank,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RerankMode {
    Disabled,
    Required,
    AllowFallback,
}

/// Threshold calibration is explicit and model-bound; no universal default exists.
#[derive(Clone, Debug, PartialEq)]
pub struct HybridConfig {
    profile: EmbeddingProfile,
    calibration_version: u64,
    minimum_cosine: f64,
    allow_keyword_fallback: bool,
    rerank: RerankMode,
    rerank_limit: usize,
}

impl HybridConfig {
    pub fn new(
        profile: EmbeddingProfile,
        calibration_version: u64,
        minimum_cosine: f64,
        allow_keyword_fallback: bool,
        rerank: RerankMode,
        rerank_limit: usize,
    ) -> Result<Self, SearchError> {
        if calibration_version == 0
            || !minimum_cosine.is_finite()
            || !(-1.0..=1.0).contains(&minimum_cosine)
            || !(1..=20).contains(&rerank_limit)
        {
            return Err(SearchError::InvalidConfig(
                "invalid hybrid calibration or limits",
            ));
        }
        Ok(Self {
            profile,
            calibration_version,
            minimum_cosine,
            allow_keyword_fallback,
            rerank,
            rerank_limit,
        })
    }

    pub fn calibration_version(&self) -> u64 {
        self.calibration_version
    }
}

pub struct HybridRanking {
    inputs: Vec<(CandidateRef, RetrievalItem)>,
    query: RetrievalQuery,
    plan: RetrievalPlan,
    proof: QueryProof,
    ranking: Vec<(CandidateRef, f64)>,
    config: HybridConfig,
    status: IndexStatus,
    mode: SearchMode,
    lexical_hits: usize,
    vector_hits: usize,
    omitted_for_budget: usize,
    used_chars: usize,
    elapsed_micros: u128,
}

impl std::fmt::Debug for HybridRanking {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HybridRanking")
            .field("mode", &self.mode)
            .field("status", &self.status)
            .field("lexical_hits", &self.lexical_hits)
            .field("vector_hits", &self.vector_hits)
            .field("used_chars", &self.used_chars)
            .field("omitted_for_budget", &self.omitted_for_budget)
            .field("elapsed_micros", &self.elapsed_micros)
            .finish_non_exhaustive()
    }
}

impl HybridRanking {
    pub fn ranking(&self) -> &[(CandidateRef, f64)] {
        &self.ranking
    }
    pub fn status(&self) -> &IndexStatus {
        &self.status
    }
    pub fn mode(&self) -> SearchMode {
        self.mode
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

    fn budget(&mut self, ticket: &CandidateTicket) {
        self.used_chars = 0;
        self.omitted_for_budget = 0;
        let mut count = 0;
        for (reference, _) in &self.ranking {
            if count == ticket.query().limit() {
                break;
            }
            let cost = ticket
                .candidate(reference)
                .expect("sealed ranking")
                .text()
                .chars()
                .count();
            if cost > ticket.query().char_budget() - self.used_chars {
                self.omitted_for_budget += 1;
                continue;
            }
            self.used_chars += cost;
            count += 1;
        }
    }
}

/// Holds the actual one-shot core ticket, including all actual transmission evidence.
#[derive(Debug)]
pub struct RankedCandidates {
    ticket: CandidateTicket,
    result: HybridRanking,
}

impl RankedCandidates {
    pub fn ticket(&self) -> &CandidateTicket {
        &self.ticket
    }
    pub fn result(&self) -> &HybridRanking {
        &self.result
    }
    /// Crate-internal decomposition for the R3 facade; not a public constructor.
    pub(crate) fn into_parts(self) -> (CandidateTicket, HybridRanking) {
        (self.ticket, self.result)
    }
}

/// Hard failures retain the permanently closed ticket and all actual send receipts.
#[derive(Debug)]
pub struct RankingFailure {
    error: SearchError,
    ticket: Box<CandidateTicket>,
}

impl RankingFailure {
    pub fn error(&self) -> &SearchError {
        &self.error
    }
    pub fn ticket(&self) -> &CandidateTicket {
        &self.ticket
    }
}

impl std::fmt::Display for RankingFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for RankingFailure {}

/// Authorizes before any statistics, similarity, ranking or budget calculation.
/// Neither this function nor cache hits can complete/deliver a core task.
pub fn hybrid_rank<Host: RetrievalHost>(
    index: &mut SearchIndex,
    host: &mut Host,
    ticket: &CandidateTicket,
    query: Option<&EncodedQuery>,
    config: &HybridConfig,
) -> Result<HybridRanking, SearchError> {
    let started = Instant::now();
    index.authorize(host, ticket)?;
    if ticket.plan().profile() != &config.profile {
        return Err(SearchError::ProfileMismatch);
    }
    if query
        .is_some_and(|query| query.profile != config.profile || query.text != ticket.query().text())
    {
        return Err(SearchError::ProfileMismatch);
    }
    index.bind_calibration(host, config)?;
    let mut status = index.reconcile(host, ticket)?;
    let mut result = HybridRanking {
        inputs: ticket
            .candidates()
            .iter()
            .map(|candidate| (candidate.candidate_ref(), candidate.item().clone()))
            .collect(),
        query: ticket.query().clone(),
        plan: ticket.plan().clone(),
        proof: ticket.query_proof().clone(),
        ranking: Vec::new(),
        config: config.clone(),
        mode: SearchMode::Hybrid,
        status: IndexStatus::default(),
        lexical_hits: 0,
        vector_hits: 0,
        omitted_for_budget: 0,
        used_chars: 0,
        elapsed_micros: 0,
    };
    if ticket.query().text().is_empty() {
        let mut candidates: Vec<_> = ticket.candidates().iter().collect();
        candidates.sort_by(|left, right| {
            right
                .item()
                .updated_at_ms()
                .cmp(&left.item().updated_at_ms())
                .then_with(|| left.candidate_ref().cmp(&right.candidate_ref()))
        });
        result.ranking = candidates
            .into_iter()
            .map(|candidate| (candidate.candidate_ref(), 0.0))
            .collect();
        result.mode = SearchMode::KeywordOnly;
    } else {
        let lexical = lexical_rank(ticket);
        let vectors = match query {
            Some(query) => index.vector_rank(ticket, query, config.minimum_cosine)?,
            None if config.allow_keyword_fallback => {
                status.reasons.push(SearchReason::ModelUnavailable);
                Vec::new()
            }
            None => {
                return Err(SearchError::InvalidInput(
                    "strict vector mode requires encoded query",
                ));
            }
        };
        if !config.allow_keyword_fallback && status.missing_chunks > 0 {
            return Err(SearchError::InvalidInput(
                "strict vector mode requires complete index",
            ));
        }
        result.lexical_hits = lexical.len();
        result.vector_hits = vectors.len();
        let exact: BTreeSet<_> = lexical
            .iter()
            .filter(|hit| hit.exact_match())
            .map(|hit| hit.reference().clone())
            .collect();
        let mut fused: BTreeMap<CandidateRef, f64> = BTreeMap::new();
        for (position, hit) in lexical.iter().enumerate() {
            *fused.entry(hit.reference().clone()).or_default() += 1.0 / (61 + position) as f64;
        }
        for (position, (reference, _)) in vectors.iter().enumerate() {
            *fused.entry(reference.clone()).or_default() += 1.0 / (61 + position) as f64;
        }
        result.ranking = fused.into_iter().collect();
        result.ranking.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| exact.contains(&right.0).cmp(&exact.contains(&left.0)))
                .then_with(|| left.0.cmp(&right.0))
        });
        result.mode = if query.is_none() {
            SearchMode::KeywordOnly
        } else if status.missing_chunks > 0 {
            SearchMode::PartialIndex
        } else {
            SearchMode::Hybrid
        };
    }
    result.status = status;
    result.budget(ticket);
    result.elapsed_micros = started.elapsed().as_micros();
    Ok(result)
}

/// Optional rerank never adds a candidate or revives a failed ticket.
/// Safe transport/protocol fallback obtains a new delivery-only core ticket.
pub async fn rerank_or_fallback<Host: RetrievalHost>(
    index: &SearchIndex,
    host: &mut Host,
    mut ticket: CandidateTicket,
    mut result: HybridRanking,
    query: Option<&EncodedQuery>,
    reranker: Option<&CohereReranker>,
    control: &CallControl,
) -> Result<RankedCandidates, RankingFailure> {
    match rerank_checked(
        index,
        host,
        &mut ticket,
        &mut result,
        query,
        reranker,
        control,
    )
    .await
    {
        Ok(()) => Ok(RankedCandidates { ticket, result }),
        Err(error) => Err(RankingFailure {
            error,
            ticket: Box::new(ticket),
        }),
    }
}

async fn rerank_checked<Host: RetrievalHost>(
    index: &SearchIndex,
    host: &mut Host,
    ticket: &mut CandidateTicket,
    result: &mut HybridRanking,
    query: Option<&EncodedQuery>,
    reranker: Option<&CohereReranker>,
    control: &CallControl,
) -> Result<(), SearchError> {
    let started = Instant::now();
    control.check()?;
    let now = host.now_ms();
    host.authority().validate_candidate_ticket(ticket, now)?;
    if index.calibration.as_ref() != Some(&result.config) {
        return Err(SearchError::StaleIndexWork);
    }
    if result.inputs.len() != ticket.candidates().len()
        || result.query != *ticket.query()
        || result.plan != *ticket.plan()
        || result.proof != *ticket.query_proof()
        || result
            .inputs
            .iter()
            .any(|(reference, input)| ticket.candidate(reference) != Some(input))
    {
        return Err(SearchError::InvalidInput(
            "ranking belongs to another ticket",
        ));
    }
    for (reference, _) in &result.ranking {
        if ticket.candidate(reference).is_none() {
            return Err(SearchError::InvalidInput(
                "ranking belongs to another ticket",
            ));
        }
    }
    if result.config.rerank == RerankMode::Disabled
        || ticket.query().text().is_empty()
        || result.ranking.is_empty()
    {
        return Ok(());
    }
    if query.is_some_and(|query| {
        query.profile != *ticket.plan().profile() || query.text != ticket.query().text()
    }) {
        return Err(SearchError::ProfileMismatch);
    }
    let entries: Vec<_> = result
        .ranking
        .iter()
        .take(result.config.rerank_limit)
        .map(|(reference, _)| reference.clone())
        .collect();
    let response = match reranker {
        Some(reranker) => {
            let texts = index.rerank_chunks(ticket, &entries, query)?;
            reranker
                .rerank_slices(host, ticket, &entries, &texts, control)
                .await
        }
        None => Err(SearchError::InvalidInput("reranker unavailable")),
    };
    match response {
        Ok(ranked) => {
            let tail = result.ranking.split_off(entries.len());
            result.ranking = ranked;
            result.ranking.extend(tail);
            for (position, (_, score)) in result.ranking.iter_mut().enumerate() {
                *score = 1.0 / (61 + position) as f64;
            }
        }
        Err(error) => {
            control.check()?;
            let transmission_denied = transmission_denied(&error);
            if result.config.rerank != RerankMode::AllowFallback
                || (!fallback_eligible(&error) && !transmission_denied)
            {
                return Err(error);
            }
            let now = host.now_ms();
            if let Err(validation_error) = host.authority().validate_candidate_ticket(ticket, now) {
                if transmission_denied
                    || !matches!(
                        &validation_error,
                        SearchError::Core(moe_memory_core::MemoryError::TaskCancelled)
                            | SearchError::Persistence(moe_memory_local::PersistenceError::Memory(
                                moe_memory_core::MemoryError::TaskCancelled
                            ))
                    )
                {
                    return Err(validation_error);
                }
                let recovered = host.authority().recover_failed_rerank(ticket, now)?;
                for (reference, _) in &mut result.ranking {
                    let input = ticket
                        .candidate(reference)
                        .ok_or(SearchError::InvalidInput("missing original input"))?;
                    *reference = recovered
                        .candidates()
                        .iter()
                        .find(|candidate| candidate.item() == input)
                        .ok_or(SearchError::StaleIndexWork)?
                        .candidate_ref();
                }
                *ticket = recovered;
                result.inputs = ticket
                    .candidates()
                    .iter()
                    .map(|candidate| (candidate.candidate_ref(), candidate.item().clone()))
                    .collect();
                result.proof = ticket.query_proof().clone();
            }
            result.status.reasons.push(SearchReason::RerankUnavailable);
            if transmission_denied {
                result.status.reasons.push(SearchReason::TransmissionDenied);
            }
        }
    }
    control.check()?;
    let now = host.now_ms();
    host.authority().validate_candidate_ticket(ticket, now)?;
    result.budget(ticket);
    result.elapsed_micros += started.elapsed().as_micros();
    Ok(())
}

/// Pre-send authority denial is the only core-shaped error with a local,
/// still-authorized fallback. RecoveryRequired and every other core or
/// persistence failure is never a degradable error.
pub(crate) fn transmission_denied(error: &SearchError) -> bool {
    matches!(
        error,
        SearchError::Core(moe_memory_core::MemoryError::TransmissionDenied)
            | SearchError::Persistence(moe_memory_local::PersistenceError::Memory(
                moe_memory_core::MemoryError::TransmissionDenied
            ))
    )
}

pub(crate) fn fallback_eligible(error: &SearchError) -> bool {
    matches!(
        error,
        SearchError::RequestTimedOut
            | SearchError::TransportUnavailable
            | SearchError::HttpStatus(_)
            | SearchError::InvalidResponse(_)
            | SearchError::ResponseTooLarge
            | SearchError::InputTooLong
            | SearchError::ProfileMismatch
            | SearchError::InvalidInput("reranker unavailable")
    )
}
