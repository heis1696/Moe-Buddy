use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use super::MemoryReader;
use crate::{
    MemoryError, MemoryScope, MemoryService, MemoryTask, PetIdentity, ReadPurpose, RetrievalItem,
    RetrievalQuery, RetrievalResult, SearchScope, SessionId, SourceRef, policy, validation,
};

const MAX_EMBEDDING_DIMENSION: usize = 4096;
const MAX_ENDPOINT_CHARS: usize = 512;
const MAX_TRANSMISSION_RECORDS: usize = 8;
const MAX_TRANSMISSION_ENTRIES: usize = 32;
const MAX_TRANSMISSION_SOURCES: usize = 32;
const MAX_HYBRID_REQUEST_BYTES: usize = 64 * 1024;
const HYBRID_REQUEST_TAG: &[u8] = b"MOE-HYBRID-TASK\0\x01";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EmbeddingProfile {
    backend: String,
    model: String,
    dimension: usize,
}

impl EmbeddingProfile {
    pub fn new(
        backend: impl Into<String>,
        model: impl Into<String>,
        dimension: usize,
    ) -> Result<Self, MemoryError> {
        let backend = backend.into();
        let model = model.into();
        validation::identifier(&backend)?;
        validation::identifier(&model)?;
        if dimension == 0 || dimension > MAX_EMBEDDING_DIMENSION {
            return Err(MemoryError::InvalidInput("invalid embedding dimension"));
        }
        Ok(Self {
            backend,
            model,
            dimension,
        })
    }

    pub fn backend(&self) -> &str {
        &self.backend
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RerankProfile {
    backend: String,
    model: String,
}

impl RerankProfile {
    pub fn new(backend: impl Into<String>, model: impl Into<String>) -> Result<Self, MemoryError> {
        let backend = backend.into();
        let model = model.into();
        validation::identifier(&backend)?;
        validation::identifier(&model)?;
        Ok(Self { backend, model })
    }

    pub fn backend(&self) -> &str {
        &self.backend
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RetrievalPlan {
    profile: EmbeddingProfile,
    rerank: Option<RerankProfile>,
}

impl RetrievalPlan {
    pub fn new(profile: EmbeddingProfile) -> Self {
        Self {
            profile,
            rerank: None,
        }
    }

    pub fn with_rerank(mut self, rerank: RerankProfile) -> Self {
        self.rerank = Some(rerank);
        self
    }

    pub fn profile(&self) -> &EmbeddingProfile {
        &self.profile
    }

    pub fn rerank(&self) -> Option<&RerankProfile> {
        self.rerank.as_ref()
    }

    fn encode(&self, request: &mut Vec<u8>) {
        push_string(request, self.profile.backend());
        push_string(request, self.profile.model());
        request.extend_from_slice(&(self.profile.dimension as u64).to_le_bytes());
        match &self.rerank {
            Some(rerank) => {
                request.push(1);
                push_string(request, rerank.backend());
                push_string(request, rerank.model());
            }
            None => request.push(0),
        }
    }
}

fn push_string(request: &mut Vec<u8>, value: &str) {
    request.extend_from_slice(&(value.len() as u64).to_le_bytes());
    request.extend_from_slice(value.as_bytes());
}

fn push_scope(request: &mut Vec<u8>, scope: &MemoryScope) {
    let namespace = match scope {
        MemoryScope::Private(identity) => {
            request.push(0);
            push_string(request, identity.instance_id());
            identity.namespace()
        }
        MemoryScope::Shared(namespace) => {
            request.push(1);
            namespace
        }
    };
    push_string(request, namespace.workspace_id());
    push_string(request, namespace.user_id());
}

fn push_reference(request: &mut Vec<u8>, reference: &CandidateRef) {
    match reference.working_context() {
        None => {
            let version = reference.record().expect("validated record reference");
            request.push(0);
            request.extend_from_slice(&version.id().0.to_le_bytes());
            request.extend_from_slice(&version.revision().to_le_bytes());
        }
        Some((id, revision)) => {
            request.push(1);
            push_string(request, id);
            request.extend_from_slice(&revision.to_le_bytes());
        }
    }
}

fn entry_scope(item: &RetrievalItem, identity: &PetIdentity) -> MemoryScope {
    match item {
        RetrievalItem::LongTerm(record) => record.scope().clone(),
        RetrievalItem::WorkingContext(_) => MemoryScope::Private(identity.clone()),
    }
}

fn ticket_inputs(
    ticket: &CandidateTicket,
    entries: &[CandidateRef],
) -> Result<Vec<RetrievalItem>, MemoryError> {
    entries
        .iter()
        .map(|reference| {
            ticket
                .candidate(reference)
                .cloned()
                .ok_or(MemoryError::TaskCancelled)
        })
        .collect()
}

fn require_current(
    current: &[RetrievalItem],
    entries: &[CandidateRef],
    inputs: &[RetrievalItem],
) -> Result<(), MemoryError> {
    for reference in entries {
        let actual = current
            .iter()
            .find(|candidate| candidate.candidate_ref().kind == reference.kind)
            .ok_or(MemoryError::TaskCancelled)?;
        let expected = inputs
            .iter()
            .find(|candidate| candidate.candidate_ref().kind == reference.kind)
            .ok_or(MemoryError::TaskCancelled)?;
        if actual != expected {
            return Err(MemoryError::TaskCancelled);
        }
    }
    Ok(())
}

fn validate_evidence(
    reader: &MemoryReader<'_>,
    candidates: &[RetrievalItem],
    evidence: &TransmissionEvidence,
    inputs: &[RetrievalItem],
    query_proof: Option<&QueryProof>,
    now_ms: u64,
) -> Result<(), MemoryError> {
    let read_policy = reader.authorize().map_err(|_| MemoryError::TaskCancelled)?;
    let policy = reader
        .service
        .transmissions
        .policies
        .get(&(reader.identity.clone(), evidence.usage))
        .ok_or(MemoryError::TaskCancelled)?;
    if now_ms < evidence.recorded_at_ms
        || evidence.authorized_at_ms > evidence.recorded_at_ms
        || evidence.include_query != evidence.usage.includes_query()
        || policy.version != evidence.policy_version
        || policy.endpoint != evidence.endpoint
        || (evidence.include_query && !policy.query)
    {
        return Err(MemoryError::TaskCancelled);
    }
    let mut expected_sources = BTreeSet::new();
    let mut expected_entries = BTreeSet::new();
    let mut expected_scopes = BTreeSet::new();
    if evidence.include_query
        && let Some(query_proof) = query_proof
    {
        if !query_proof.bound {
            return Err(MemoryError::TaskCancelled);
        }
        expected_sources.extend(query_proof.sources.iter().cloned());
        expected_entries.extend(query_proof.entries.iter().cloned());
        expected_scopes.insert(query_proof.data_scope.clone());
    }
    for reference in &evidence.sent_entries {
        let item = candidates
            .iter()
            .find(|candidate| candidate.candidate_ref().kind == reference.kind)
            .ok_or(MemoryError::TaskCancelled)?;
        expected_sources.extend(item.sources().iter().cloned());
        expected_entries.insert(reference.clone());
        expected_scopes.insert(entry_scope(item, &reader.identity));
    }
    let recorded_sources: BTreeSet<_> = evidence.sent_sources.iter().cloned().collect();
    let recorded_entries: BTreeSet<_> = evidence.sent_entries.iter().cloned().collect();
    let recorded_scopes: BTreeSet<_> = evidence.sent_scopes.iter().cloned().collect();
    if recorded_sources.len() != evidence.sent_sources.len()
        || recorded_entries.len() != evidence.sent_entries.len()
        || recorded_scopes.len() != evidence.sent_scopes.len()
        || if query_proof.is_some() || !evidence.include_query {
            recorded_sources != expected_sources
                || recorded_entries != expected_entries
                || recorded_scopes != expected_scopes
        } else {
            !expected_sources.is_subset(&recorded_sources)
                || !expected_entries.is_subset(&recorded_entries)
                || !expected_scopes.is_subset(&recorded_scopes)
        }
        || !recorded_scopes
            .iter()
            .all(|scope| policy.scopes.contains(scope))
        || !recorded_sources.iter().all(|source| {
            policy.sources.contains(source)
                && read_policy.sources.contains(source)
                && reader.service.source_current(source)
        })
    {
        return Err(MemoryError::TaskCancelled);
    }
    require_current(candidates, &evidence.sent_entries, inputs)
}

fn validate_candidate_snapshot(
    ticket: &CandidateTicket,
    current: &[RetrievalItem],
) -> Result<(), MemoryError> {
    if ticket.candidates.len() != current.len() {
        return Err(MemoryError::TaskCancelled);
    }
    let mut current_by_kind = BTreeMap::new();
    for item in current {
        if current_by_kind
            .insert(item.candidate_ref().kind, item)
            .is_some()
        {
            return Err(MemoryError::TaskCancelled);
        }
    }
    let mut seen = BTreeSet::new();
    for candidate in &ticket.candidates {
        if !candidate
            .reference
            .binding
            .as_ref()
            .is_some_and(|binding| Arc::ptr_eq(&binding.0, &ticket.nonce))
            || candidate.reference.kind != candidate.item.candidate_ref().kind
            || !seen.insert(candidate.reference.kind.clone())
            || current_by_kind.get(&candidate.reference.kind) != Some(&&candidate.item)
        {
            return Err(MemoryError::TaskCancelled);
        }
    }
    Ok(())
}

fn validate_query_proof(
    reader: &MemoryReader<'_>,
    ticket: &CandidateTicket,
    current: &[RetrievalItem],
) -> Result<(), MemoryError> {
    let proof = &ticket.query_proof;
    if proof.identity != ticket.identity
        || proof.session != ticket.session
        || proof.scope != ticket.query.search.scope
        || proof.policy_version != ticket.policy_version
        || proof.data_scope != MemoryScope::Private(ticket.identity.clone())
    {
        return Err(MemoryError::TaskCancelled);
    }
    if !proof.bound {
        if !proof.sources.is_empty() || !proof.entries.is_empty() {
            return Err(MemoryError::TaskCancelled);
        }
        return Ok(());
    }
    if proof.sources.is_empty()
        || proof.sources.len() > MAX_TRANSMISSION_SOURCES
        || proof.entries.len() > MAX_TRANSMISSION_ENTRIES
    {
        return Err(MemoryError::TaskCancelled);
    }
    let source_set: BTreeSet<_> = proof.sources.iter().cloned().collect();
    let entry_set: BTreeSet<_> = proof.entries.iter().cloned().collect();
    let read_policy = reader.authorize().map_err(|_| MemoryError::TaskCancelled)?;
    if source_set.len() != proof.sources.len()
        || entry_set.len() != proof.entries.len()
        || !proof.sources.iter().all(|source| {
            read_policy.sources.contains(source)
                && reader.service.source_current(source)
                && policy::scope_readable(&source.key.scope, &ticket.identity)
        })
    {
        return Err(MemoryError::TaskCancelled);
    }
    for reference in &proof.entries {
        let expected = ticket
            .candidate(reference)
            .ok_or(MemoryError::TaskCancelled)?;
        let actual = current
            .iter()
            .find(|item| item.candidate_ref().kind == reference.kind)
            .ok_or(MemoryError::TaskCancelled)?;
        if actual != expected
            || !actual
                .sources()
                .iter()
                .all(|source| source_set.contains(source))
        {
            return Err(MemoryError::TaskCancelled);
        }
    }
    Ok(())
}

fn validate_candidate_ticket_state(
    reader: &MemoryReader<'_>,
    ticket: &CandidateTicket,
    now_ms: u64,
    failed: bool,
    allow_pending: bool,
) -> Result<Vec<RetrievalItem>, MemoryError> {
    if !Arc::ptr_eq(&reader.service.reads.epoch, &ticket.epoch)
        || reader.service.store.generation != ticket.generation
        || reader.service.reads.configuration != ticket.configuration
        || ticket.failed.get() != failed
        || (!allow_pending && !ticket.pending.borrow().is_empty())
        || ticket.sealed.get()
        || reader.identity != ticket.identity
        || reader.session != ticket.session
        || reader.purpose != ticket.purpose
        || ticket.runtime == 0
        || ticket.runtime > reader.service.reads.ticket_sequence
        || now_ms < ticket.issued_at_ms
    {
        return Err(MemoryError::TaskCancelled);
    }
    let read_policy = reader.authorize().map_err(|_| MemoryError::TaskCancelled)?;
    if read_policy.version != ticket.policy_version {
        return Err(MemoryError::TaskCancelled);
    }
    let (current, _) = reader
        .authorized_candidates(&ticket.query, now_ms)
        .map_err(|_| MemoryError::TaskCancelled)?;
    if reader.identity().ok() != ticket.identity_view {
        return Err(MemoryError::TaskCancelled);
    }
    validate_candidate_snapshot(ticket, &current)?;
    validate_query_proof(reader, ticket, &current)?;
    let evidence = ticket.transmissions();
    let input_refs: BTreeSet<_> = evidence
        .iter()
        .flat_map(|item| item.sent_entries.iter().cloned())
        .collect();
    let inputs = ticket_inputs(ticket, &input_refs.into_iter().collect::<Vec<_>>())?;
    for item in &evidence {
        validate_evidence(
            reader,
            &current,
            item,
            &inputs,
            Some(&ticket.query_proof),
            now_ms,
        )?;
    }
    Ok(current)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CandidateRef {
    kind: CandidateKind,
    binding: Option<CandidateBinding>,
}

#[derive(Clone, Debug)]
struct CandidateBinding(Arc<()>);

impl PartialEq for CandidateBinding {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CandidateBinding {}

impl PartialOrd for CandidateBinding {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CandidateBinding {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (Arc::as_ptr(&self.0) as usize).cmp(&(Arc::as_ptr(&other.0) as usize))
    }
}

#[derive(Clone, Debug)]
pub struct AuthorizedCandidate {
    item: RetrievalItem,
    reference: CandidateRef,
}

impl AuthorizedCandidate {
    fn new(item: RetrievalItem, nonce: Arc<()>) -> Self {
        let mut reference = item.candidate_ref();
        reference.binding = Some(CandidateBinding(nonce));
        Self { item, reference }
    }

    pub fn candidate_ref(&self) -> CandidateRef {
        self.reference.clone()
    }

    pub fn item(&self) -> &RetrievalItem {
        &self.item
    }
}

impl std::ops::Deref for AuthorizedCandidate {
    type Target = RetrievalItem;

    fn deref(&self) -> &Self::Target {
        &self.item
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CandidateKind {
    Record(crate::RecordVersion),
    WorkingContext { id: String, revision: u64 },
}

impl CandidateRef {
    pub fn record(&self) -> Option<crate::RecordVersion> {
        match &self.kind {
            CandidateKind::Record(version) => Some(*version),
            CandidateKind::WorkingContext { .. } => None,
        }
    }

    pub fn working_context(&self) -> Option<(&str, u64)> {
        match &self.kind {
            CandidateKind::Record(_) => None,
            CandidateKind::WorkingContext { id, revision } => Some((id, *revision)),
        }
    }
}

impl RetrievalItem {
    pub fn candidate_ref(&self) -> CandidateRef {
        match self {
            RetrievalItem::LongTerm(record) => CandidateRef {
                kind: CandidateKind::Record(record.version_ref()),
                binding: None,
            },
            RetrievalItem::WorkingContext(context) => CandidateRef {
                kind: CandidateKind::WorkingContext {
                    id: context.id().to_owned(),
                    revision: context.revision(),
                },
                binding: None,
            },
        }
    }
}

/// 查询保持本宠私密归属；受信宿主须以 `bind_query_provenance` 绑定完整
/// 来源与派生引用。搜索范围不代表查询归属，未绑定时不能外传。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryProof {
    identity: PetIdentity,
    session: SessionId,
    scope: SearchScope,
    policy_version: u64,
    data_scope: MemoryScope,
    sources: Vec<SourceRef>,
    entries: Vec<CandidateRef>,
    bound: bool,
}

impl QueryProof {
    pub fn identity(&self) -> &PetIdentity {
        &self.identity
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn scope(&self) -> SearchScope {
        self.scope
    }

    pub fn policy_version(&self) -> u64 {
        self.policy_version
    }

    pub fn data_scope(&self) -> &MemoryScope {
        &self.data_scope
    }

    pub fn sources(&self) -> &[SourceRef] {
        &self.sources
    }

    pub fn entries(&self) -> &[CandidateRef] {
        &self.entries
    }

    pub fn is_bound(&self) -> bool {
        self.bound
    }
}

pub struct CandidateTicket {
    pub(crate) epoch: Arc<()>,
    nonce: Arc<()>,
    pub(crate) runtime: u64,
    pub(crate) identity: PetIdentity,
    pub(crate) session: SessionId,
    pub(crate) purpose: ReadPurpose,
    pub(crate) query: RetrievalQuery,
    pub(crate) plan: RetrievalPlan,
    pub(crate) candidates: Vec<AuthorizedCandidate>,
    pub(crate) policy_version: u64,
    pub(crate) configuration: u64,
    pub(crate) generation: u64,
    pub(crate) issued_at_ms: u64,
    pub(crate) query_proof: QueryProof,
    identity_view: Option<crate::IdentityView>,
    transmissions: RefCell<Vec<TransmissionEvidence>>,
    pending: RefCell<Vec<Arc<()>>>,
    sealed: Cell<bool>,
    failed: Cell<bool>,
    rerank_failure_recorded: Cell<bool>,
    rerank_recovery_consumed: Cell<bool>,
    transmissions_disabled: bool,
}

impl fmt::Debug for CandidateTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CandidateTicket")
            .field("runtime", &self.runtime)
            .field("generation", &self.generation)
            .field("plan", &self.plan)
            .field("candidate_count", &self.candidates.len())
            .field("transmission_count", &self.transmissions.borrow().len())
            .finish_non_exhaustive()
    }
}

impl CandidateTicket {
    pub fn runtime(&self) -> u64 {
        self.runtime
    }

    pub fn identity(&self) -> &PetIdentity {
        &self.identity
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn purpose(&self) -> ReadPurpose {
        self.purpose
    }

    pub fn query(&self) -> &RetrievalQuery {
        &self.query
    }

    pub fn plan(&self) -> &RetrievalPlan {
        &self.plan
    }

    pub fn candidates(&self) -> &[AuthorizedCandidate] {
        &self.candidates
    }

    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    pub fn query_proof(&self) -> &QueryProof {
        &self.query_proof
    }

    pub fn transmissions(&self) -> Vec<TransmissionEvidence> {
        self.transmissions.borrow().clone()
    }

    pub fn candidate(&self, reference: &CandidateRef) -> Option<&RetrievalItem> {
        if !reference
            .binding
            .as_ref()
            .is_some_and(|binding| Arc::ptr_eq(&binding.0, &self.nonce))
        {
            return None;
        }
        self.candidates
            .iter()
            .find(|candidate| &candidate.candidate_ref() == reference)
            .map(AuthorizedCandidate::item)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransmissionUsage {
    QueryEmbedding,
    EntryEmbedding,
    Rerank,
}

impl TransmissionUsage {
    fn includes_query(self) -> bool {
        matches!(self, Self::QueryEmbedding | Self::Rerank)
    }
}

#[derive(Clone)]
pub struct TransmissionPolicy {
    version: u64,
    usage: TransmissionUsage,
    endpoint: String,
    sources: BTreeSet<SourceRef>,
    query: bool,
    scopes: BTreeSet<MemoryScope>,
}

impl TransmissionPolicy {
    pub fn deny_all(version: u64, usage: TransmissionUsage) -> Self {
        Self {
            version,
            usage,
            endpoint: String::new(),
            sources: BTreeSet::new(),
            query: false,
            scopes: BTreeSet::new(),
        }
    }

    pub fn new(
        version: u64,
        usage: TransmissionUsage,
        endpoint: impl Into<String>,
        sources: Vec<SourceRef>,
    ) -> Result<Self, MemoryError> {
        let endpoint = endpoint.into();
        if version == 0
            || endpoint.is_empty()
            || endpoint.trim() != endpoint
            || endpoint.chars().count() > MAX_ENDPOINT_CHARS
            || endpoint.chars().any(char::is_control)
            || sources.len() > validation::MAX_RECORDS
        {
            return Err(MemoryError::InvalidInput("invalid transmission policy"));
        }
        Ok(Self {
            version,
            usage,
            endpoint,
            sources: sources.into_iter().collect(),
            query: false,
            scopes: BTreeSet::new(),
        })
    }

    /// 显式授权该用途把密封查询本身发往端点。默认拒绝：允许共享条目外传
    /// 不代表允许私密归属的查询外传；空条目的 QueryEmbedding 同样需要本许可。
    pub fn transmit_query(mut self) -> Self {
        self.query = true;
        self
    }

    pub fn with_scope(mut self, scope: MemoryScope) -> Self {
        self.scopes.insert(scope);
        self
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn usage(&self) -> TransmissionUsage {
        self.usage
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn allows_query(&self) -> bool {
        self.query
    }
}

impl fmt::Debug for TransmissionPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransmissionPolicy")
            .field("version", &self.version)
            .field("usage", &self.usage)
            .field("endpoint", &self.endpoint)
            .field("source_count", &self.sources.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default)]
pub(crate) struct TransmissionState {
    pub(crate) policies: BTreeMap<(PetIdentity, TransmissionUsage), TransmissionPolicy>,
}

pub struct TransmissionTicket {
    epoch: Arc<()>,
    candidate_nonce: Arc<()>,
    dispatch_nonce: Arc<()>,
    runtime: u64,
    identity: PetIdentity,
    session: SessionId,
    scope: SearchScope,
    plan: RetrievalPlan,
    usage: TransmissionUsage,
    endpoint: String,
    include_query: bool,
    sent_sources: Vec<SourceRef>,
    sent_entries: Vec<CandidateRef>,
    sent_scopes: Vec<MemoryScope>,
    inputs: Vec<RetrievalItem>,
    policy_version: u64,
    configuration: u64,
    generation: u64,
    issued_at_ms: u64,
}

impl fmt::Debug for TransmissionTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransmissionTicket")
            .field("runtime", &self.runtime)
            .field("usage", &self.usage)
            .field("endpoint", &self.endpoint)
            .field("include_query", &self.include_query)
            .field("sent_source_count", &self.sent_sources.len())
            .field("sent_entry_count", &self.sent_entries.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl TransmissionTicket {
    /// 签发本回执的 CandidateTicket 运行期身份；只有同一票据可接收回执。
    pub fn candidate_runtime(&self) -> u64 {
        self.runtime
    }

    pub fn identity(&self) -> &PetIdentity {
        &self.identity
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn scope(&self) -> SearchScope {
        self.scope
    }

    pub fn plan(&self) -> &RetrievalPlan {
        &self.plan
    }

    pub fn usage(&self) -> TransmissionUsage {
        self.usage
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn include_query(&self) -> bool {
        self.include_query
    }

    pub fn sent_sources(&self) -> &[SourceRef] {
        &self.sent_sources
    }

    pub fn sent_entries(&self) -> &[CandidateRef] {
        &self.sent_entries
    }

    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TransmissionEvidence {
    usage: TransmissionUsage,
    endpoint: String,
    include_query: bool,
    policy_version: u64,
    authorized_at_ms: u64,
    sent_sources: Vec<SourceRef>,
    sent_entries: Vec<CandidateRef>,
    sent_scopes: Vec<MemoryScope>,
    recorded_at_ms: u64,
}

impl TransmissionEvidence {
    pub fn usage(&self) -> TransmissionUsage {
        self.usage
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn include_query(&self) -> bool {
        self.include_query
    }

    pub fn policy_version(&self) -> u64 {
        self.policy_version
    }

    pub fn authorized_at_ms(&self) -> u64 {
        self.authorized_at_ms
    }

    pub fn sent_sources(&self) -> &[SourceRef] {
        &self.sent_sources
    }

    /// 本次实际发送的全部候选引用（含最终未入选者），是派生与发布校验的
    /// 依赖来源；不是只有最终交付的 top-k。
    pub fn sent_entries(&self) -> &[CandidateRef] {
        &self.sent_entries
    }

    pub fn sent_scopes(&self) -> &[MemoryScope] {
        &self.sent_scopes
    }

    pub fn recorded_at_ms(&self) -> u64 {
        self.recorded_at_ms
    }
}

impl MemoryReader<'_> {
    pub fn begin_candidates(
        &mut self,
        query: &RetrievalQuery,
        plan: RetrievalPlan,
        now_ms: u64,
    ) -> Result<CandidateTicket, MemoryError> {
        let (candidates, _) = self.authorized_candidates(query, now_ms)?;
        let policy_version = self.authorize()?.version;
        let runtime = self.service.next_ticket_runtime()?;
        let nonce = Arc::new(());
        let candidates = candidates
            .into_iter()
            .map(|item| AuthorizedCandidate::new(item, nonce.clone()))
            .collect();
        Ok(CandidateTicket {
            epoch: self.service.reads.epoch.clone(),
            nonce,
            runtime,
            identity: self.identity.clone(),
            session: self.session.clone(),
            purpose: self.purpose,
            query: query.clone(),
            plan,
            candidates,
            policy_version,
            configuration: self.service.reads.configuration,
            generation: self.service.store.generation,
            issued_at_ms: now_ms,
            query_proof: QueryProof {
                identity: self.identity.clone(),
                session: self.session.clone(),
                scope: query.search.scope,
                policy_version,
                data_scope: MemoryScope::Private(self.identity.clone()),
                sources: Vec::new(),
                entries: Vec::new(),
                bound: false,
            },
            identity_view: self.identity().ok(),
            transmissions: RefCell::new(Vec::new()),
            pending: RefCell::new(Vec::new()),
            sealed: Cell::new(false),
            failed: Cell::new(false),
            rerank_failure_recorded: Cell::new(false),
            rerank_recovery_consumed: Cell::new(false),
            transmissions_disabled: false,
        })
    }

    pub fn complete_retrieval(
        &self,
        ticket: &CandidateTicket,
        ranking: &[(CandidateRef, f64)],
        now_ms: u64,
    ) -> Result<MemoryTask, MemoryError> {
        validate_candidate_ticket_state(self, ticket, now_ms, false, false)?;
        let candidates: BTreeMap<_, _> = ticket
            .candidates
            .iter()
            .map(|candidate| (&candidate.reference, &candidate.item))
            .collect();
        let mut seen = BTreeSet::new();
        for (reference, score) in ranking {
            if !score.is_finite() {
                return Err(MemoryError::InvalidInput("relevance score must be finite"));
            }
            if !candidates.contains_key(reference) || !seen.insert(reference) {
                return Err(MemoryError::InvalidEvidence);
            }
        }
        let mut evidence = ticket.transmissions();
        evidence.sort();
        let input_refs: BTreeSet<_> = evidence
            .iter()
            .flat_map(|item| item.sent_entries.iter().cloned())
            .collect();
        let inputs = ticket_inputs(ticket, &input_refs.into_iter().collect::<Vec<_>>())?;
        let identity_view = self.identity().ok();
        if identity_view != ticket.identity_view {
            return Err(MemoryError::TaskCancelled);
        }
        let mut result = RetrievalResult {
            items: Vec::new(),
            used_chars: 0,
        };
        let mut entries = Vec::with_capacity(ranking.len().min(ticket.query.search.limit));
        for (reference, _) in ranking {
            let item = candidates
                .get(reference)
                .ok_or(MemoryError::InvalidEvidence)?;
            let cost = item.text().chars().count();
            if cost > ticket.query.char_budget - result.used_chars {
                continue;
            }
            result.used_chars += cost;
            entries.push(reference.clone());
            result.items.push((*item).clone());
            if result.items.len() == ticket.query.search.limit {
                break;
            }
        }
        let mut request = HYBRID_REQUEST_TAG.to_vec();
        for number in [
            now_ms,
            ticket.query.search.text.len() as u64,
            ticket.query.search.limit as u64,
            ticket.query.char_budget as u64,
        ] {
            request.extend_from_slice(&number.to_le_bytes());
        }
        request.extend_from_slice(ticket.query.search.text.as_bytes());
        request.push(match ticket.query.search.scope {
            crate::SearchScope::Private => 0,
            crate::SearchScope::Shared => 1,
            crate::SearchScope::All => 2,
        });
        request.push(match ticket.query.layer {
            crate::RetrievalLayer::WorkingContext => 0,
            crate::RetrievalLayer::LongTerm => 1,
            crate::RetrievalLayer::All => 2,
            crate::RetrievalLayer::FormalMaterial => 3,
        });
        request.extend_from_slice(
            &identity_view
                .as_ref()
                .map_or(0, crate::IdentityView::version)
                .to_le_bytes(),
        );
        request.extend_from_slice(&(result.items.len() as u64).to_le_bytes());
        for reference in &entries {
            push_reference(&mut request, reference);
        }
        ticket.plan.encode(&mut request);
        request.extend_from_slice(&(evidence.len() as u64).to_le_bytes());
        for item in &evidence {
            request.push(match item.usage() {
                TransmissionUsage::QueryEmbedding => 0,
                TransmissionUsage::EntryEmbedding => 1,
                TransmissionUsage::Rerank => 2,
            });
            request.push(u8::from(item.include_query()));
            request.extend_from_slice(&item.policy_version().to_le_bytes());
            request.extend_from_slice(&item.authorized_at_ms().to_le_bytes());
            request.extend_from_slice(&item.recorded_at_ms().to_le_bytes());
            push_string(&mut request, item.endpoint());
            request.extend_from_slice(&(item.sent_sources().len() as u64).to_le_bytes());
            for source in item.sent_sources() {
                push_scope(&mut request, &source.key.scope);
                push_string(&mut request, source.id());
                request.extend_from_slice(&source.revision().to_le_bytes());
            }
            request.extend_from_slice(&(item.sent_entries().len() as u64).to_le_bytes());
            for reference in item.sent_entries() {
                push_reference(&mut request, reference);
            }
            request.extend_from_slice(&(item.sent_scopes.len() as u64).to_le_bytes());
            for scope in &item.sent_scopes {
                push_scope(&mut request, scope);
            }
        }
        if request.len() > MAX_HYBRID_REQUEST_BYTES {
            return Err(MemoryError::CapacityExceeded);
        }
        ticket.sealed.set(true);
        Ok(MemoryTask {
            epoch: ticket.epoch.clone(),
            identity: ticket.identity.clone(),
            purpose: ticket.purpose,
            stamp: crate::management::TaskStamp {
                generation: self.service.store.generation,
                configuration: self.service.reads.configuration,
                policy: ticket.policy_version,
                session: ticket.session.clone(),
                request,
            },
            query: ticket.query.clone(),
            result,
            identity_view,
            issued_at_ms: now_ms,
            delivery: crate::management::Delivery::Hybrid(crate::management::HybridSeal {
                plan: ticket.plan.clone(),
                entries,
                inputs,
                transmissions: evidence,
            }),
        })
    }
}

pub(crate) fn validate_hybrid_task(
    service: &mut MemoryService,
    task: &MemoryTask,
    seal: &crate::management::HybridSeal,
    now_ms: u64,
) -> Result<(), MemoryError> {
    let reader = MemoryReader {
        service,
        identity: task.identity.clone(),
        session: task.stamp.session.clone(),
        purpose: task.purpose,
    };
    if reader
        .authorize()
        .map_err(|_| MemoryError::TaskCancelled)?
        .version
        != task.stamp.policy
    {
        return Err(MemoryError::TaskCancelled);
    }
    let (candidates, _) = reader
        .authorized_candidates(&task.query, now_ms)
        .map_err(|_| MemoryError::TaskCancelled)?;
    if seal.entries.len() != task.result().items().len() {
        return Err(MemoryError::TaskCancelled);
    }
    for (entry, item) in seal.entries.iter().zip(task.result().items()) {
        let current = candidates
            .iter()
            .find(|candidate| candidate.candidate_ref().kind == entry.kind)
            .ok_or(MemoryError::TaskCancelled)?;
        if current != item {
            return Err(MemoryError::TaskCancelled);
        }
    }
    for evidence in &seal.transmissions {
        validate_evidence(&reader, &candidates, evidence, &seal.inputs, None, now_ms)?;
    }
    if reader.identity().ok() != task.identity_view {
        return Err(MemoryError::TaskCancelled);
    }
    Ok(())
}

impl MemoryService {
    pub fn set_transmission_policy(
        &mut self,
        identity: &PetIdentity,
        policy: TransmissionPolicy,
    ) -> Result<(), MemoryError> {
        if policy.version == 0 || policy.sources.len() > validation::MAX_RECORDS {
            return Err(MemoryError::InvalidInput("invalid transmission policy"));
        }
        let key = (identity.clone(), policy.usage);
        if self
            .transmissions
            .policies
            .get(&key)
            .is_some_and(|current| policy.version <= current.version)
        {
            return Err(MemoryError::PolicyVersionConflict);
        }
        if policy.scopes.len() > 2
            || !policy
                .scopes
                .iter()
                .all(|scope| policy::scope_readable(scope, identity))
            || !policy.sources.iter().all(|source| {
                policy::scope_readable(&source.key.scope, identity) && self.source_current(source)
            })
        {
            return Err(MemoryError::InvalidEvidence);
        }
        if !self.transmissions.policies.contains_key(&key)
            && self.transmissions.policies.len() >= validation::MAX_RECORDS
        {
            return Err(MemoryError::CapacityExceeded);
        }
        let configuration = self.next_configuration()?;
        self.transmissions.policies.insert(key, policy);
        self.reads.configuration = configuration;
        Ok(())
    }

    pub fn validate_candidate_ticket(
        &mut self,
        ticket: &CandidateTicket,
        now_ms: u64,
    ) -> Result<(), MemoryError> {
        let reader = self.for_session(&ticket.identity, &ticket.session, ticket.purpose);
        validate_candidate_ticket_state(&reader, ticket, now_ms, false, false).map(|_| ())
    }

    pub fn recover_failed_rerank(
        &mut self,
        ticket: &mut CandidateTicket,
        now_ms: u64,
    ) -> Result<CandidateTicket, MemoryError> {
        if !ticket.failed.get()
            || !ticket.rerank_failure_recorded.get()
            || !ticket.pending.borrow().is_empty()
            || ticket.sealed.get()
            || ticket.rerank_recovery_consumed.get()
            || ticket.plan.rerank().is_none()
            || !ticket
                .transmissions
                .borrow()
                .last()
                .is_some_and(|evidence| evidence.usage == TransmissionUsage::Rerank)
        {
            return Err(MemoryError::TaskCancelled);
        }
        {
            let reader = self.for_session(&ticket.identity, &ticket.session, ticket.purpose);
            validate_candidate_ticket_state(&reader, ticket, now_ms, true, false)?;
        }
        let nonce = Arc::new(());
        let candidates: Vec<_> = ticket
            .candidates
            .iter()
            .map(|candidate| AuthorizedCandidate::new(candidate.item.clone(), nonce.clone()))
            .collect();
        let reference_map: BTreeMap<_, _> = ticket
            .candidates
            .iter()
            .zip(&candidates)
            .map(|(previous, current)| (previous.candidate_ref(), current.candidate_ref()))
            .collect();
        let remap_reference = |reference: &CandidateRef| {
            reference_map
                .get(reference)
                .cloned()
                .ok_or(MemoryError::TaskCancelled)
        };
        let mut query_proof = ticket.query_proof.clone();
        query_proof.entries = query_proof
            .entries
            .iter()
            .map(&remap_reference)
            .collect::<Result<_, _>>()?;
        let mut transmissions = ticket.transmissions();
        for evidence in &mut transmissions {
            evidence.sent_entries = evidence
                .sent_entries
                .iter()
                .map(&remap_reference)
                .collect::<Result<_, _>>()?;
        }
        let runtime = self.next_ticket_runtime()?;
        let recovered = CandidateTicket {
            epoch: ticket.epoch.clone(),
            nonce,
            runtime,
            identity: ticket.identity.clone(),
            session: ticket.session.clone(),
            purpose: ticket.purpose,
            query: ticket.query.clone(),
            plan: ticket.plan.clone(),
            candidates,
            policy_version: ticket.policy_version,
            configuration: ticket.configuration,
            generation: ticket.generation,
            issued_at_ms: now_ms,
            query_proof,
            identity_view: ticket.identity_view.clone(),
            transmissions: RefCell::new(transmissions),
            pending: RefCell::new(Vec::new()),
            sealed: Cell::new(false),
            failed: Cell::new(false),
            rerank_failure_recorded: Cell::new(false),
            rerank_recovery_consumed: Cell::new(true),
            transmissions_disabled: true,
        };
        {
            let reader =
                self.for_session(&recovered.identity, &recovered.session, recovered.purpose);
            validate_candidate_ticket_state(&reader, &recovered, now_ms, false, false)?;
        }
        ticket.rerank_recovery_consumed.set(true);
        Ok(recovered)
    }

    fn current_ticket_candidates(
        &mut self,
        ticket: &CandidateTicket,
        now_ms: u64,
    ) -> Result<Vec<RetrievalItem>, MemoryError> {
        let reader = self.for_session(&ticket.identity, &ticket.session, ticket.purpose);
        validate_candidate_ticket_state(&reader, ticket, now_ms, false, true)
    }

    pub fn bind_query_provenance(
        &mut self,
        ticket: &mut CandidateTicket,
        sources: Vec<SourceRef>,
        entries: &[CandidateRef],
        now_ms: u64,
    ) -> Result<(), MemoryError> {
        if ticket.sealed.get()
            || ticket.query_proof.bound
            || !ticket.pending.borrow().is_empty()
            || !ticket.transmissions.borrow().is_empty()
        {
            return Err(MemoryError::InvalidState);
        }
        if sources.is_empty()
            || sources.len() > MAX_TRANSMISSION_SOURCES
            || entries.len() > MAX_TRANSMISSION_ENTRIES
        {
            return Err(MemoryError::InvalidEvidence);
        }
        let current = self.current_ticket_candidates(ticket, now_ms)?;
        let source_set: BTreeSet<_> = sources.iter().cloned().collect();
        let entry_set: BTreeSet<_> = entries.iter().cloned().collect();
        let read_policy = &self.reads.policies[&ticket.identity];
        if source_set.len() != sources.len()
            || entry_set.len() != entries.len()
            || !sources.iter().all(|source| {
                read_policy.sources.contains(source)
                    && self.source_current(source)
                    && policy::scope_readable(&source.key.scope, &ticket.identity)
            })
        {
            return Err(MemoryError::InvalidEvidence);
        }
        for reference in &entry_set {
            let item = current
                .iter()
                .find(|item| item.candidate_ref().kind == reference.kind)
                .ok_or(MemoryError::InvalidEvidence)?;
            if ticket.candidate(reference).is_none()
                || !item
                    .sources()
                    .iter()
                    .all(|source| source_set.contains(source))
            {
                return Err(MemoryError::InvalidEvidence);
            }
        }
        let entries: Vec<_> = entry_set.into_iter().collect();
        require_current(&current, &entries, &ticket_inputs(ticket, &entries)?)?;
        ticket.query_proof.sources = source_set.into_iter().collect();
        ticket.query_proof.entries = entries;
        ticket.query_proof.bound = true;
        Ok(())
    }

    pub fn prepare_transmission(
        &mut self,
        ticket: &CandidateTicket,
        usage: TransmissionUsage,
        endpoint: &str,
        entries: &[CandidateRef],
        now_ms: u64,
    ) -> Result<TransmissionTicket, MemoryError> {
        if ticket.sealed.get() {
            return Err(MemoryError::TaskCancelled);
        }
        if ticket.transmissions_disabled {
            return Err(MemoryError::TransmissionDenied);
        }
        if usage == TransmissionUsage::Rerank && ticket.plan.rerank().is_none() {
            return Err(MemoryError::TransmissionDenied);
        }
        let current = self.current_ticket_candidates(ticket, now_ms)?;
        if ticket.pending.borrow().len() + ticket.transmissions.borrow().len()
            >= MAX_TRANSMISSION_RECORDS
        {
            return Err(MemoryError::CapacityExceeded);
        }
        match usage {
            TransmissionUsage::QueryEmbedding if entries.is_empty() => {}
            TransmissionUsage::EntryEmbedding | TransmissionUsage::Rerank
                if !entries.is_empty() => {}
            _ => {
                return Err(MemoryError::InvalidInput(
                    "transmission entries do not match usage",
                ));
            }
        }
        if entries.len() > MAX_TRANSMISSION_ENTRIES {
            return Err(MemoryError::CapacityExceeded);
        }
        let policy = self
            .transmissions
            .policies
            .get(&(ticket.identity.clone(), usage))
            .ok_or(MemoryError::TransmissionDenied)?;
        if policy.endpoint != endpoint {
            return Err(MemoryError::TransmissionDenied);
        }
        let mut seen = BTreeSet::new();
        let mut sent_entries = BTreeSet::new();
        let mut sent_sources = BTreeSet::new();
        let mut sent_scopes = BTreeSet::new();
        if usage.includes_query() {
            let proof = &ticket.query_proof;
            if !proof.bound || !policy.query || !policy.scopes.contains(&proof.data_scope) {
                return Err(MemoryError::TransmissionDenied);
            }
            require_current(
                &current,
                &proof.entries,
                &ticket_inputs(ticket, &proof.entries)?,
            )?;
            sent_sources.extend(proof.sources.iter().cloned());
            sent_entries.extend(proof.entries.iter().cloned());
            sent_scopes.insert(proof.data_scope.clone());
        }
        for reference in entries {
            let item = ticket
                .candidate(reference)
                .ok_or(MemoryError::InvalidEvidence)?;
            if !seen.insert(reference) {
                return Err(MemoryError::InvalidEvidence);
            }
            let current_item = current
                .iter()
                .find(|candidate| candidate.candidate_ref().kind == reference.kind)
                .ok_or(MemoryError::TaskCancelled)?;
            if current_item != item {
                return Err(MemoryError::TaskCancelled);
            }
            let scope = entry_scope(item, &ticket.identity);
            if !policy.scopes.contains(&scope) {
                return Err(MemoryError::TransmissionDenied);
            }
            sent_sources.extend(item.sources().iter().cloned());
            sent_entries.insert(reference.clone());
            sent_scopes.insert(scope);
        }
        if sent_sources.len() > MAX_TRANSMISSION_SOURCES
            || sent_entries.len() > MAX_TRANSMISSION_ENTRIES
        {
            return Err(MemoryError::CapacityExceeded);
        }
        if !sent_sources.iter().all(|source| {
            policy.sources.contains(source)
                && self.source_current(source)
                && self.reads.policies[&ticket.identity]
                    .sources
                    .contains(source)
        }) {
            return Err(MemoryError::TransmissionDenied);
        }
        let dispatch_nonce = Arc::new(());
        let sent_entries: Vec<_> = sent_entries.into_iter().collect();
        let inputs = ticket_inputs(ticket, &sent_entries)?;
        ticket.pending.borrow_mut().push(dispatch_nonce.clone());
        Ok(TransmissionTicket {
            epoch: ticket.epoch.clone(),
            candidate_nonce: ticket.nonce.clone(),
            dispatch_nonce,
            runtime: ticket.runtime,
            identity: ticket.identity.clone(),
            session: ticket.session.clone(),
            scope: ticket.query.search.scope,
            plan: ticket.plan.clone(),
            usage,
            endpoint: policy.endpoint.clone(),
            include_query: usage.includes_query(),
            sent_sources: sent_sources.into_iter().collect(),
            sent_entries,
            sent_scopes: sent_scopes.into_iter().collect(),
            inputs,
            policy_version: policy.version,
            configuration: self.reads.configuration,
            generation: self.store.generation,
            issued_at_ms: now_ms,
        })
    }

    /// Records a possibly sent, unsuccessful request and permanently closes its
    /// candidate ticket. A failed/unknown network result must not permit delivery.
    pub fn fail_transmission(
        &mut self,
        ticket: &mut CandidateTicket,
        sent: &TransmissionTicket,
        now_ms: u64,
    ) -> Result<(), MemoryError> {
        self.record_transmission(ticket, sent, now_ms)?;
        ticket.failed.set(true);
        ticket
            .rerank_failure_recorded
            .set(sent.usage == TransmissionUsage::Rerank);
        Ok(())
    }

    pub fn record_transmission(
        &mut self,
        ticket: &mut CandidateTicket,
        sent: &TransmissionTicket,
        now_ms: u64,
    ) -> Result<(), MemoryError> {
        if !Arc::ptr_eq(&self.reads.epoch, &sent.epoch)
            || !Arc::ptr_eq(&ticket.epoch, &sent.epoch)
            || !Arc::ptr_eq(&ticket.nonce, &sent.candidate_nonce)
            || ticket.runtime != sent.runtime
            || ticket.identity != sent.identity
            || ticket.session != sent.session
            || ticket.query.search.scope != sent.scope
            || ticket.plan != sent.plan
            || ticket.generation != sent.generation
            || ticket.configuration != sent.configuration
        {
            return Err(MemoryError::TaskCancelled);
        }
        let position = ticket
            .pending
            .borrow()
            .iter()
            .position(|nonce| Arc::ptr_eq(nonce, &sent.dispatch_nonce))
            .ok_or(MemoryError::InvalidEvidence)?;
        ticket.pending.borrow_mut().remove(position);
        let evidence = TransmissionEvidence {
            usage: sent.usage,
            endpoint: sent.endpoint.clone(),
            include_query: sent.include_query,
            policy_version: sent.policy_version,
            authorized_at_ms: sent.issued_at_ms,
            sent_sources: sent.sent_sources.clone(),
            sent_entries: sent.sent_entries.clone(),
            sent_scopes: sent.sent_scopes.clone(),
            recorded_at_ms: now_ms,
        };
        ticket.transmissions.borrow_mut().push(evidence.clone());
        let result = (|| {
            if now_ms < sent.issued_at_ms {
                return Err(MemoryError::TaskCancelled);
            }
            let current = self.current_ticket_candidates(ticket, now_ms)?;
            let reader = self.for_session(&ticket.identity, &ticket.session, ticket.purpose);
            validate_evidence(
                &reader,
                &current,
                &evidence,
                &sent.inputs,
                Some(&ticket.query_proof),
                now_ms,
            )
        })();
        if result.is_err() {
            ticket.failed.set(true);
            ticket.rerank_failure_recorded.set(false);
        }
        result
    }
}
