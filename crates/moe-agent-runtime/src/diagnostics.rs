use std::fmt;
use std::mem::size_of;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

const IDENTIFIER_BYTES: usize = 64;
const CATEGORY_COUNT: usize = 3;
const DETAIL_COUNT: usize = 3;
const PARTITION_COUNT: usize = CATEGORY_COUNT * DETAIL_COUNT;
const ALLOCATION_OVERHEAD_BYTES: usize = 64;
const MAX_SEARCH_BYTES: usize = 96;
static NEXT_SERVICE_ID: AtomicU64 = AtomicU64::new(1);
pub const LOG_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct OpaqueIdentifier {
    bytes: [u8; IDENTIFIER_BYTES],
    length: u8,
}
impl OpaqueIdentifier {
    fn parse(value: &str) -> Result<Self, DiagnosticsError> {
        if value.is_empty() || value.len() > IDENTIFIER_BYTES {
            return Err(DiagnosticsError::InvalidConfiguration);
        }
        let mut bytes = [0; IDENTIFIER_BYTES];
        bytes[..value.len()].copy_from_slice(value.as_bytes());
        Ok(Self {
            bytes,
            length: value.len() as u8,
        })
    }
}
impl fmt::Debug for OpaqueIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StableDomain(OpaqueIdentifier);
impl StableDomain {
    pub fn new(value: &str) -> Result<Self, DiagnosticsError> {
        OpaqueIdentifier::parse(value).map(Self)
    }
}
impl fmt::Debug for StableDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StableDomain([redacted])")
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewerSubject(OpaqueIdentifier);
impl ViewerSubject {
    pub fn new(value: &str) -> Result<Self, DiagnosticsError> {
        OpaqueIdentifier::parse(value).map(Self)
    }
}
impl fmt::Debug for ViewerSubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ViewerSubject([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Category {
    Background = 0,
    Error = 1,
    Run = 2,
}
impl Category {
    const fn index(self) -> usize {
        self as usize
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum DetailLevel {
    Essential = 0,
    Standard = 1,
    Verbose = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Host,
    Ingress,
    Agent,
    Memory,
    Hook,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Admission,
    Prepare,
    Infer,
    Finish,
    MemoryQuery,
    HookWrite,
    Lifecycle,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BackgroundCode {
    ServiceStarted,
    Waiting,
    Initialized,
    BackendUnavailable,
    ProcessCrashed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    AuthorizationDenied,
    VersionConflict,
    ModelUnavailable,
    WriteOutcomeUnknown,
    Unclassified,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnownFailure {
    Authorization,
    VersionConflict,
    ModelUnavailable,
    WriteOutcomeUnknown,
    Other,
}
impl From<KnownFailure> for ErrorCode {
    fn from(value: KnownFailure) -> Self {
        match value {
            KnownFailure::Authorization => Self::AuthorizationDenied,
            KnownFailure::VersionConflict => Self::VersionConflict,
            KnownFailure::ModelUnavailable => Self::ModelUnavailable,
            KnownFailure::WriteOutcomeUnknown => Self::WriteOutcomeUnknown,
            KnownFailure::Other => Self::Unclassified,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RetryHint {
    ManualConfirmation,
    ExplicitRetry,
    Forbidden,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OperationState {
    InProgress,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransportState {
    NotAttempted,
    Written,
    Failed,
    Cancelled,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RunObservation {
    StageStarted(Step),
    StageReturned(Step),
    Operation(OperationState),
    Transport(TransportState),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticKind {
    Background(BackgroundCode),
    Error { code: ErrorCode, stage: Step },
    Run(RunObservation),
}

impl DiagnosticKind {
    const fn category(self) -> Category {
        match self {
            Self::Background(_) => Category::Background,
            Self::Error { .. } => Category::Error,
            Self::Run(_) => Category::Run,
        }
    }
    const fn code(self) -> EventCode {
        match self {
            Self::Background(code) => EventCode::Background(code),
            Self::Error { code, .. } => EventCode::Error(code),
            Self::Run(RunObservation::StageStarted(step)) => EventCode::StageStarted(step),
            Self::Run(RunObservation::StageReturned(step)) => EventCode::StageReturned(step),
            Self::Run(RunObservation::Operation(state)) => EventCode::Operation(state),
            Self::Run(RunObservation::Transport(state)) => EventCode::Transport(state),
        }
    }
    const fn message_key(self) -> MessageKey {
        match self {
            Self::Background(code) => MessageKey::Background(code),
            Self::Error { code, stage } => MessageKey::Error(code, stage),
            Self::Run(RunObservation::StageStarted(step)) => MessageKey::StageStarted(step),
            Self::Run(RunObservation::StageReturned(step)) => MessageKey::StageReturned(step),
            Self::Run(RunObservation::Operation(state)) => MessageKey::Operation(state),
            Self::Run(RunObservation::Transport(state)) => MessageKey::Transport(state),
        }
    }
    const fn severity(self) -> Severity {
        match self {
            Self::Background(BackgroundCode::ProcessCrashed) => Severity::Error,
            Self::Background(BackgroundCode::BackendUnavailable) => Severity::Warn,
            Self::Background(_) => Severity::Info,
            Self::Error {
                code: ErrorCode::Unclassified | ErrorCode::WriteOutcomeUnknown,
                ..
            } => Severity::Warn,
            Self::Error { .. } => Severity::Error,
            Self::Run(RunObservation::StageStarted(_)) => Severity::Debug,
            Self::Run(RunObservation::StageReturned(_)) => Severity::Info,
            Self::Run(
                RunObservation::Operation(OperationState::Failed)
                | RunObservation::Transport(TransportState::Failed),
            ) => Severity::Error,
            Self::Run(
                RunObservation::Operation(OperationState::Unknown)
                | RunObservation::Transport(TransportState::Unknown),
            ) => Severity::Warn,
            Self::Run(
                RunObservation::Operation(OperationState::Cancelled)
                | RunObservation::Transport(TransportState::Cancelled),
            ) => Severity::Info,
            Self::Run(_) => Severity::Info,
        }
    }
    const fn retry_hint(self) -> Option<RetryHint> {
        match self {
            Self::Error {
                code: ErrorCode::AuthorizationDenied | ErrorCode::VersionConflict,
                ..
            } => Some(RetryHint::Forbidden),
            Self::Error {
                code: ErrorCode::ModelUnavailable,
                ..
            } => Some(RetryHint::ExplicitRetry),
            Self::Error {
                code: ErrorCode::WriteOutcomeUnknown | ErrorCode::Unclassified,
                ..
            } => Some(RetryHint::ManualConfirmation),
            _ => None,
        }
    }
    const fn operation_state(self) -> Option<OperationState> {
        match self {
            Self::Run(RunObservation::Operation(state)) => Some(state),
            _ => None,
        }
    }
    const fn transport_state(self) -> Option<TransportState> {
        match self {
            Self::Run(RunObservation::Transport(state)) => Some(state),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventCode {
    Background(BackgroundCode),
    Error(ErrorCode),
    StageStarted(Step),
    StageReturned(Step),
    Operation(OperationState),
    Transport(TransportState),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessageKey {
    Background(BackgroundCode),
    Error(ErrorCode, Step),
    StageStarted(Step),
    StageReturned(Step),
    Operation(OperationState),
    Transport(TransportState),
}

impl MessageKey {
    const fn safe_search_text(self) -> (&'static str, &'static str) {
        match self {
            Self::Background(BackgroundCode::ServiceStarted) => {
                ("background.service_started", "Background service started")
            }
            Self::Background(BackgroundCode::Waiting) => {
                ("background.waiting", "Background service waiting")
            }
            Self::Background(BackgroundCode::Initialized) => {
                ("background.initialized", "Background service initialized")
            }
            Self::Background(BackgroundCode::BackendUnavailable) => {
                ("background.backend_unavailable", "Backend unavailable")
            }
            Self::Background(BackgroundCode::ProcessCrashed) => {
                ("process.crashed", "Process crashed")
            }
            Self::Error(ErrorCode::AuthorizationDenied, _) => {
                ("error.authorization_denied", "Authorization denied")
            }
            Self::Error(ErrorCode::VersionConflict, _) => {
                ("error.version_conflict", "Version conflict")
            }
            Self::Error(ErrorCode::ModelUnavailable, _) => {
                ("error.model_unavailable", "Model unavailable")
            }
            Self::Error(ErrorCode::WriteOutcomeUnknown, _) => {
                ("error.write_outcome_unknown", "Write outcome unknown")
            }
            Self::Error(ErrorCode::Unclassified, _) => ("error.unclassified", "Unclassified error"),
            Self::StageStarted(Step::Admission) => {
                ("run.stage_started.admission", "Admission started")
            }
            Self::StageStarted(Step::Prepare) => {
                ("run.stage_started.prepare", "Preparation started")
            }
            Self::StageStarted(Step::Infer) => ("run.stage_started.infer", "Inference started"),
            Self::StageStarted(Step::Finish) => ("run.stage_started.finish", "Finish started"),
            Self::StageStarted(Step::MemoryQuery) => {
                ("run.stage_started.memory_query", "Memory query started")
            }
            Self::StageStarted(Step::HookWrite) => {
                ("run.stage_started.hook_write", "Hook write started")
            }
            Self::StageStarted(Step::Lifecycle) => {
                ("run.stage_started.lifecycle", "Lifecycle stage started")
            }
            Self::StageReturned(Step::Admission) => {
                ("run.stage_returned.admission", "Admission returned")
            }
            Self::StageReturned(Step::Prepare) => {
                ("run.stage_returned.prepare", "Preparation returned")
            }
            Self::StageReturned(Step::Infer) => ("run.stage_returned.infer", "Inference returned"),
            Self::StageReturned(Step::Finish) => ("run.stage_returned.finish", "Finish returned"),
            Self::StageReturned(Step::MemoryQuery) => {
                ("run.stage_returned.memory_query", "Memory query returned")
            }
            Self::StageReturned(Step::HookWrite) => {
                ("run.stage_returned.hook_write", "Hook write returned")
            }
            Self::StageReturned(Step::Lifecycle) => {
                ("run.stage_returned.lifecycle", "Lifecycle stage returned")
            }
            Self::Operation(OperationState::InProgress) => {
                ("run.operation.in_progress", "Operation in progress")
            }
            Self::Operation(OperationState::Completed) => {
                ("run.operation.completed", "Operation completed")
            }
            Self::Operation(OperationState::Failed) => ("run.operation.failed", "Operation failed"),
            Self::Operation(OperationState::Cancelled) => {
                ("run.operation.cancelled", "Operation cancelled")
            }
            Self::Operation(OperationState::Unknown) => {
                ("run.operation.unknown", "Operation outcome unknown")
            }
            Self::Transport(TransportState::NotAttempted) => {
                ("run.transport.not_attempted", "Transport not attempted")
            }
            Self::Transport(TransportState::Written) => {
                ("run.transport.written", "Transport written")
            }
            Self::Transport(TransportState::Failed) => ("run.transport.failed", "Transport failed"),
            Self::Transport(TransportState::Cancelled) => {
                ("run.transport.cancelled", "Transport cancelled")
            }
            Self::Transport(TransportState::Unknown) => {
                ("run.transport.unknown", "Transport outcome unknown")
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockReading {
    pub monotonic_ms: u64,
    pub wall_clock_ms: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventInput {
    pub source: Source,
    pub detail: DetailLevel,
    pub elapsed_ms: Option<u64>,
    pub kind: DiagnosticKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureSetting {
    Disabled,
    Through(DetailLevel),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureStrategy {
    settings: [CaptureSetting; CATEGORY_COUNT],
}
impl CaptureStrategy {
    pub const fn disabled() -> Self {
        Self {
            settings: [CaptureSetting::Disabled; CATEGORY_COUNT],
        }
    }
    pub const fn all_through(max: DetailLevel) -> Self {
        Self {
            settings: [CaptureSetting::Through(max); CATEGORY_COUNT],
        }
    }
    pub const fn with(mut self, category: Category, setting: CaptureSetting) -> Self {
        self.settings[category.index()] = setting;
        self
    }
    const fn allows(self, category: Category, detail: DetailLevel) -> bool {
        match self.settings[category.index()] {
            CaptureSetting::Disabled => false,
            CaptureSetting::Through(max) => detail as u8 <= max as u8,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CategorySet(u8);
impl CategorySet {
    pub const fn empty() -> Self {
        Self(0)
    }
    pub const fn all() -> Self {
        Self((1 << CATEGORY_COUNT) - 1)
    }
    pub const fn only(category: Category) -> Self {
        Self(1 << category.index())
    }
    pub const fn with(self, category: Category) -> Self {
        Self(self.0 | (1 << category.index()))
    }
    const fn contains(self, category: Category) -> bool {
        self.0 & (1 << category.index()) != 0
    }
    const fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewerGrant {
    subject: ViewerSubject,
    version: u64,
    expires_at_monotonic_ms: u64,
    categories: CategorySet,
    maximum_detail: DetailLevel,
}
impl ViewerGrant {
    pub const fn new(
        subject: ViewerSubject,
        version: u64,
        expires_at_monotonic_ms: u64,
        categories: CategorySet,
        maximum_detail: DetailLevel,
    ) -> Self {
        Self {
            subject,
            version,
            expires_at_monotonic_ms,
            categories,
            maximum_detail,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardBudgets {
    pub max_domains: usize,
    pub max_grants_per_domain: usize,
    /// Retained ring-window size per partition; eviction reuses event slots.
    pub events_per_partition: usize,
    /// Cumulative unique run IDs accepted per partition until reconfiguration.
    pub runs_per_partition: usize,
    /// Cumulative opaque alias mappings per partition and grant until reconfiguration.
    pub aliases_per_partition: usize,
    pub cursors_per_grant: usize,
    pub max_page_size: usize,
    pub max_query_work: usize,
    pub max_summaries_per_query: usize,
    pub max_health_slots: usize,
    pub max_concurrent_queries: usize,
    pub max_event_bytes: usize,
    pub max_allocated_bytes: usize,
}
impl HardBudgets {
    fn validate(self) -> Result<Self, DiagnosticsError> {
        let limits = [
            self.max_domains,
            self.max_grants_per_domain,
            self.events_per_partition,
            self.runs_per_partition,
            self.aliases_per_partition,
            self.cursors_per_grant,
            self.max_page_size,
            self.max_query_work,
            self.max_summaries_per_query,
            self.max_health_slots,
            self.max_concurrent_queries,
            self.max_event_bytes,
            self.max_allocated_bytes,
        ];
        let health_slots = self
            .max_domains
            .checked_mul(PARTITION_COUNT)
            .ok_or(DiagnosticsError::InvalidConfiguration)?;
        if limits.contains(&0)
            || self.max_concurrent_queries != 1
            || self.max_health_slots < health_slots
            || self.max_event_bytes < size_of::<StoredEvent>()
        {
            return Err(DiagnosticsError::InvalidConfiguration);
        }
        Ok(self)
    }
}

struct PartitionState {
    events: Vec<Option<StoredEvent>>,
    seen_runs: Vec<Option<u64>>,
    last_sequence: u64,
    retained: usize,
    dropped: u64,
    accepted: u64,
    omitted: u64,
    rejected: u64,
    clock_regressed: bool,
    last_monotonic_ms: Option<u64>,
}

impl PartitionState {
    fn empty() -> Self {
        Self {
            events: Vec::new(),
            seen_runs: Vec::new(),
            last_sequence: 0,
            retained: 0,
            dropped: 0,
            accepted: 0,
            omitted: 0,
            rejected: 0,
            clock_regressed: false,
            last_monotonic_ms: None,
        }
    }
    fn new(budgets: HardBudgets) -> Result<Self, DiagnosticsError> {
        let mut events = Vec::new();
        events
            .try_reserve_exact(budgets.events_per_partition)
            .map_err(|_| DiagnosticsError::CapacityExceeded)?;
        events.resize(budgets.events_per_partition, None);
        let mut seen_runs = Vec::new();
        seen_runs
            .try_reserve_exact(budgets.runs_per_partition)
            .map_err(|_| DiagnosticsError::CapacityExceeded)?;
        seen_runs.resize(budgets.runs_per_partition, None);
        Ok(Self {
            events,
            seen_runs,
            last_sequence: 0,
            retained: 0,
            dropped: 0,
            accepted: 0,
            omitted: 0,
            rejected: 0,
            clock_regressed: false,
            last_monotonic_ms: None,
        })
    }
    fn oldest_sequence(&self) -> u64 {
        self.last_sequence
            .saturating_sub(self.retained as u64)
            .saturating_add(1)
    }
    fn get(&self, sequence: u64) -> Option<StoredEvent> {
        if sequence == 0 || sequence < self.oldest_sequence() || sequence > self.last_sequence {
            return None;
        }
        let index = (sequence - 1) as usize % self.events.len();
        self.events[index].filter(|event| event.sequence == sequence)
    }
    fn find_run(&self, run_id: u64) -> bool {
        self.seen_runs.contains(&Some(run_id))
    }
    fn register_run(&mut self, run_id: u64) -> Result<(), DiagnosticsError> {
        if self.find_run(run_id) {
            return Ok(());
        }
        let slot = self
            .seen_runs
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(DiagnosticsError::RunBudgetExceeded)?;
        *slot = Some(run_id);
        Ok(())
    }
}

struct GrantState {
    grant: ViewerGrant,
    aliases: [AliasState; PARTITION_COUNT],
    next_run_alias: u64,
    cursors: Vec<Option<CursorState>>,
    next_cursor_token: u64,
    last_verified_monotonic_ms: Option<u64>,
    expired: bool,
    clock_regressed: bool,
}

struct AliasState {
    entries: Vec<Option<AliasEntry>>,
    next_event_alias: u64,
}

impl AliasState {
    fn empty() -> Self {
        Self {
            entries: Vec::new(),
            next_event_alias: 1,
        }
    }
    fn new(budget: usize) -> Result<Self, DiagnosticsError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(budget)
            .map_err(|_| DiagnosticsError::CapacityExceeded)?;
        entries.resize(budget, None);
        Ok(Self {
            entries,
            next_event_alias: 1,
        })
    }
}

impl GrantState {
    fn new(grant: ViewerGrant, budgets: HardBudgets) -> Result<Self, DiagnosticsError> {
        if grant.categories == CategorySet::empty() || grant.version == 0 {
            return Err(DiagnosticsError::InvalidConfiguration);
        }
        let mut aliases = std::array::from_fn(|_| AliasState::empty());
        for alias_store in &mut aliases {
            *alias_store = AliasState::new(budgets.aliases_per_partition)?;
        }
        let mut cursors = Vec::new();
        cursors
            .try_reserve_exact(budgets.cursors_per_grant)
            .map_err(|_| DiagnosticsError::CapacityExceeded)?;
        cursors.resize(budgets.cursors_per_grant, None);
        Ok(Self {
            grant,
            aliases,
            next_run_alias: 1,
            cursors,
            next_cursor_token: 1,
            last_verified_monotonic_ms: None,
            expired: false,
            clock_regressed: false,
        })
    }
}

struct DomainState {
    key: StableDomain,
    capture: CaptureStrategy,
    partitions: [PartitionState; PARTITION_COUNT],
    grants: Vec<GrantState>,
    next_run_id: u64,
}

impl DomainState {
    fn new(
        key: StableDomain,
        capture: CaptureStrategy,
        grants: &[ViewerGrant],
        budgets: HardBudgets,
    ) -> Result<Self, DiagnosticsError> {
        if grants.len() > budgets.max_grants_per_domain {
            return Err(DiagnosticsError::CapacityExceeded);
        }
        for (index, grant) in grants.iter().enumerate() {
            if grants[..index]
                .iter()
                .any(|prior| prior.subject == grant.subject)
            {
                return Err(DiagnosticsError::InvalidConfiguration);
            }
        }
        let mut partitions = std::array::from_fn(|_| PartitionState::empty());
        for partition in &mut partitions {
            *partition = PartitionState::new(budgets)?;
        }
        let mut grant_states = Vec::new();
        grant_states
            .try_reserve_exact(grants.len())
            .map_err(|_| DiagnosticsError::CapacityExceeded)?;
        for grant in grants {
            grant_states.push(GrantState::new(*grant, budgets)?);
        }
        Ok(Self {
            key,
            capture,
            partitions,
            grants: grant_states,
            next_run_id: 1,
        })
    }
}

pub struct DiagnosticsConfig {
    process_generation: u128,
    budgets: HardBudgets,
    domains: Vec<DomainState>,
}

impl DiagnosticsConfig {
    /// The trusted host must provide a nonzero generation never reused across process restarts.
    pub fn new(process_generation: u128, budgets: HardBudgets) -> Result<Self, DiagnosticsError> {
        if process_generation == 0 {
            return Err(DiagnosticsError::InvalidConfiguration);
        }
        let budgets = budgets.validate()?;
        let reserved_domains = budgets
            .max_domains
            .checked_mul(size_of::<DomainState>())
            .ok_or(DiagnosticsError::CapacityExceeded)?;
        let minimum_bytes = size_of::<DiagnosticsService>()
            .checked_add(ALLOCATION_OVERHEAD_BYTES * 2)
            .and_then(|bytes| bytes.checked_add(reserved_domains))
            .ok_or(DiagnosticsError::CapacityExceeded)?;
        if minimum_bytes > budgets.max_allocated_bytes {
            return Err(DiagnosticsError::CapacityExceeded);
        }
        let mut domains = Vec::new();
        domains
            .try_reserve_exact(budgets.max_domains)
            .map_err(|_| DiagnosticsError::CapacityExceeded)?;
        let config = Self {
            process_generation,
            budgets,
            domains,
        };
        if config.allocated_bytes()? > budgets.max_allocated_bytes {
            return Err(DiagnosticsError::CapacityExceeded);
        }
        Ok(config)
    }
    pub fn add_domain(
        &mut self,
        key: StableDomain,
        capture: CaptureStrategy,
        grants: &[ViewerGrant],
    ) -> Result<(), DiagnosticsError> {
        if self.domains.len() >= self.budgets.max_domains
            || self.domains.iter().any(|domain| domain.key == key)
        {
            return Err(DiagnosticsError::CapacityExceeded);
        }
        if (self.domains.len() + 1)
            .checked_mul(PARTITION_COUNT)
            .is_none_or(|count| count > self.budgets.max_health_slots)
        {
            return Err(DiagnosticsError::CapacityExceeded);
        }
        validate_grants(grants, self.budgets)?;
        let required = domain_dynamic_bytes(self.budgets, grants.len())?;
        if self
            .allocated_bytes()?
            .checked_add(required)
            .is_none_or(|bytes| bytes > self.budgets.max_allocated_bytes)
        {
            return Err(DiagnosticsError::CapacityExceeded);
        }
        let state = DomainState::new(key, capture, grants, self.budgets)?;
        self.domains.push(state);
        if self.allocated_bytes()? > self.budgets.max_allocated_bytes {
            self.domains.pop();
            return Err(DiagnosticsError::CapacityExceeded);
        }
        Ok(())
    }
    pub fn allocated_bytes(&self) -> Result<usize, DiagnosticsError> {
        let mut bytes = size_of::<DiagnosticsService>()
            .checked_add(ALLOCATION_OVERHEAD_BYTES * 2)
            .ok_or(DiagnosticsError::CapacityExceeded)?;
        bytes = bytes
            .checked_add(
                self.domains
                    .capacity()
                    .checked_mul(size_of::<DomainState>())
                    .ok_or(DiagnosticsError::CapacityExceeded)?,
            )
            .ok_or(DiagnosticsError::CapacityExceeded)?;
        for domain in &self.domains {
            bytes = bytes
                .checked_add(ALLOCATION_OVERHEAD_BYTES)
                .ok_or(DiagnosticsError::CapacityExceeded)?;
            for partition in &domain.partitions {
                let event_bytes = partition
                    .events
                    .capacity()
                    .checked_mul(size_of::<Option<StoredEvent>>())
                    .ok_or(DiagnosticsError::CapacityExceeded)?;
                let run_bytes = partition
                    .seen_runs
                    .capacity()
                    .checked_mul(size_of::<Option<u64>>())
                    .ok_or(DiagnosticsError::CapacityExceeded)?;
                bytes = bytes
                    .checked_add(event_bytes)
                    .and_then(|n| n.checked_add(run_bytes))
                    .and_then(|n| n.checked_add(ALLOCATION_OVERHEAD_BYTES * 2))
                    .ok_or(DiagnosticsError::CapacityExceeded)?;
            }
            bytes = bytes
                .checked_add(
                    domain
                        .grants
                        .capacity()
                        .checked_mul(size_of::<GrantState>())
                        .ok_or(DiagnosticsError::CapacityExceeded)?,
                )
                .and_then(|n| n.checked_add(ALLOCATION_OVERHEAD_BYTES))
                .ok_or(DiagnosticsError::CapacityExceeded)?;
            for grant in &domain.grants {
                for alias_store in &grant.aliases {
                    let aliases = alias_store
                        .entries
                        .capacity()
                        .checked_mul(size_of::<Option<AliasEntry>>())
                        .ok_or(DiagnosticsError::CapacityExceeded)?;
                    bytes = bytes
                        .checked_add(aliases)
                        .and_then(|n| n.checked_add(ALLOCATION_OVERHEAD_BYTES))
                        .ok_or(DiagnosticsError::CapacityExceeded)?;
                }
                let cursors = grant
                    .cursors
                    .capacity()
                    .checked_mul(size_of::<Option<CursorState>>())
                    .ok_or(DiagnosticsError::CapacityExceeded)?;
                bytes = bytes
                    .checked_add(cursors)
                    .and_then(|n| n.checked_add(ALLOCATION_OVERHEAD_BYTES))
                    .ok_or(DiagnosticsError::CapacityExceeded)?;
            }
        }
        Ok(bytes)
    }
}

pub struct DiagnosticsService {
    identity: Rc<()>,
    service_id: Option<u64>,
    generation: u64,
    config: Option<DiagnosticsConfig>,
}

impl DiagnosticsService {
    pub fn disabled() -> Self {
        Self {
            identity: Rc::new(()),
            service_id: NEXT_SERVICE_ID
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .ok(),
            generation: 0,
            config: None,
        }
    }
    pub fn enable(&mut self, config: DiagnosticsConfig) -> Result<(), DiagnosticsError> {
        self.service_id
            .ok_or(DiagnosticsError::IdentifierExhausted)?;
        if config.domains.is_empty()
            || config.allocated_bytes()? > config.budgets.max_allocated_bytes
        {
            return Err(DiagnosticsError::InvalidConfiguration);
        }
        let Some(generation) = self.generation.checked_add(1) else {
            self.config = None;
            return Err(DiagnosticsError::IdentifierExhausted);
        };
        self.generation = generation;
        self.config = Some(config);
        Ok(())
    }
    pub fn disable(&mut self) -> Result<(), DiagnosticsError> {
        let Some(generation) = self.generation.checked_add(1) else {
            self.config = None;
            return Err(DiagnosticsError::IdentifierExhausted);
        };
        self.generation = generation;
        self.config = None;
        Ok(())
    }
    pub fn authorize(
        &mut self,
        domain: StableDomain,
        subject: ViewerSubject,
        version: u64,
        clock: ClockReading,
    ) -> Result<LogAccess, DiagnosticsError> {
        let config = self.config.as_mut().ok_or(DiagnosticsError::Unavailable)?;
        let (domain_index, domain_state) = config
            .domains
            .iter_mut()
            .enumerate()
            .find(|(_, item)| item.key == domain)
            .ok_or(DiagnosticsError::Unauthorized)?;
        let (grant_index, grant) = domain_state
            .grants
            .iter_mut()
            .enumerate()
            .find(|(_, item)| item.grant.subject == subject)
            .ok_or(DiagnosticsError::Unauthorized)?;
        validate_grant_clock(grant, clock.monotonic_ms)?;
        if grant.grant.version != version {
            return Err(DiagnosticsError::Unauthorized);
        }
        Ok(LogAccess {
            service: Rc::downgrade(&self.identity),
            generation: self.generation,
            domain_index,
            grant_index,
            version,
        })
    }
    pub fn create_run(&mut self, domain: StableDomain) -> Result<RunHandle, DiagnosticsError> {
        let domain_index = self.domain_index(domain)?;
        let state = &mut self
            .config
            .as_mut()
            .ok_or(DiagnosticsError::Unavailable)?
            .domains[domain_index];
        let run_id = state.next_run_id;
        state.next_run_id = run_id
            .checked_add(1)
            .ok_or(DiagnosticsError::IdentifierExhausted)?;
        Ok(RunHandle {
            service: Rc::downgrade(&self.identity),
            generation: self.generation,
            domain_index,
            run_id,
        })
    }
    pub fn create_emitter(
        &self,
        domain: StableDomain,
        run: Option<&RunHandle>,
    ) -> Result<LogEmitter, DiagnosticsError> {
        let domain_index = self.domain_index(domain)?;
        if let Some(run) = run {
            self.validate_run(run, domain_index)?;
        }
        Ok(LogEmitter {
            service: Rc::downgrade(&self.identity),
            generation: self.generation,
            domain_index,
            run_id: run.map(|item| item.run_id),
        })
    }
    pub fn emit(
        &mut self,
        emitter: &LogEmitter,
        input: EventInput,
        clock: ClockReading,
    ) -> Result<EmitOutcome, DiagnosticsError> {
        self.validate_emitter(emitter)?;
        let domain = &mut self
            .config
            .as_mut()
            .ok_or(DiagnosticsError::Unavailable)?
            .domains[emitter.domain_index];
        let category = input.kind.category();
        let acceptance_order = domain
            .partitions
            .iter()
            .map(|partition| u128::from(partition.last_sequence))
            .sum::<u128>()
            + 1;
        let partition = &mut domain.partitions[partition_index(category, input.detail)];
        if !domain.capture.allows(category, input.detail) {
            increment(&mut partition.omitted);
            return Ok(EmitOutcome::Omitted);
        }
        if partition.clock_regressed {
            increment(&mut partition.rejected);
            return Err(DiagnosticsError::ClockWentBackwards);
        }
        if partition
            .last_monotonic_ms
            .is_some_and(|prior| clock.monotonic_ms < prior)
        {
            partition.clock_regressed = true;
            increment(&mut partition.rejected);
            return Err(DiagnosticsError::ClockWentBackwards);
        }
        let Some(sequence) = partition.last_sequence.checked_add(1) else {
            increment(&mut partition.rejected);
            return Err(DiagnosticsError::IdentifierExhausted);
        };
        if let Some(run_id) = emitter.run_id {
            if !partition.find_run(run_id) && !partition.seen_runs.iter().any(Option::is_none) {
                increment(&mut partition.rejected);
                return Err(DiagnosticsError::RunBudgetExceeded);
            }
            if let Err(error) = partition.register_run(run_id) {
                increment(&mut partition.rejected);
                return Err(error);
            }
        }
        let event = StoredEvent {
            sequence,
            acceptance_order,
            monotonic_ms: clock.monotonic_ms,
            wall_clock_ms: clock.wall_clock_ms,
            elapsed_ms: input.elapsed_ms,
            source: input.source,
            detail: input.detail,
            severity: input.kind.severity(),
            kind: input.kind,
            run_id: emitter.run_id,
        };
        let index = (sequence - 1) as usize % partition.events.len();
        let evicted = partition.events[index].is_some();
        partition.events[index] = Some(event);
        partition.last_sequence = sequence;
        partition.last_monotonic_ms = Some(clock.monotonic_ms);
        if evicted {
            increment(&mut partition.dropped);
        } else {
            partition.retained += 1;
        }
        increment(&mut partition.accepted);
        Ok(if evicted {
            EmitOutcome::AcceptedWithEviction
        } else {
            EmitOutcome::Accepted
        })
    }

    pub fn query_into(
        &mut self,
        access: &LogAccess,
        filter: LogFilter,
        cursor: Option<&LogCursor>,
        clock: ClockReading,
        output: &mut [Option<LogEntry>],
    ) -> Result<QueryPage, DiagnosticsError> {
        output.fill(None);
        let result = self.query_into_inner(access, filter, cursor, clock, output);
        if result.is_err() {
            output.fill(None);
        }
        result
    }

    fn query_into_inner(
        &mut self,
        access: &LogAccess,
        filter: LogFilter,
        cursor: Option<&LogCursor>,
        clock: ClockReading,
        output: &mut [Option<LogEntry>],
    ) -> Result<QueryPage, DiagnosticsError> {
        output.fill(None);
        let (domain_index, grant_index) = self.validate_access(access, clock)?;
        if let Some(handle) = cursor {
            self.validate_cursor(handle, domain_index, grant_index)?;
        }
        let generation = self.generation;
        let service = Rc::downgrade(&self.identity);
        let service_id = self
            .service_id
            .ok_or(DiagnosticsError::IdentifierExhausted)?;
        let config = self.config.as_mut().ok_or(DiagnosticsError::Unavailable)?;
        let process_generation = config.process_generation;
        let budgets = config.budgets;
        if output.is_empty() || output.len() > budgets.max_page_size {
            return Err(DiagnosticsError::InvalidLimit);
        }
        let domain = &mut config.domains[domain_index];
        let grant = &mut domain.grants[grant_index];
        if !filter.categories.is_subset_of(grant.grant.categories) {
            return Err(DiagnosticsError::Unauthorized);
        }
        let mut state = if let Some(handle) = cursor {
            grant
                .cursors
                .get(handle.slot)
                .and_then(|entry| *entry)
                .filter(|state| state.token == handle.token)
                .ok_or(DiagnosticsError::StaleHandle)?
        } else {
            CursorState::new(filter, &domain.partitions, grant.grant)
        };
        if state.filter != filter {
            return Err(DiagnosticsError::InvalidCursor);
        }
        let (used, total, gap) =
            collect_page(&domain.partitions, grant.grant, &mut state, budgets, output)?;
        let has_more = total > used;
        let alias_needs = missing_aliases(grant, output, used);
        for (index, needed) in alias_needs.slots.iter().enumerate() {
            let aliases = &grant.aliases[index];
            if aliases
                .entries
                .iter()
                .filter(|entry| entry.is_none())
                .count()
                < *needed
            {
                clear_entries(output);
                return Err(DiagnosticsError::AliasBudgetExceeded);
            }
            let count = u64::try_from(alias_needs.event_ids[index])
                .map_err(|_| DiagnosticsError::IdentifierExhausted)?;
            if aliases.next_event_alias.checked_add(count).is_none() {
                clear_entries(output);
                return Err(DiagnosticsError::IdentifierExhausted);
            }
        }
        let new_run_aliases = u64::try_from(alias_needs.new_run_ids)
            .map_err(|_| DiagnosticsError::IdentifierExhausted)?;
        if grant.next_run_alias.checked_add(new_run_aliases).is_none() {
            clear_entries(output);
            return Err(DiagnosticsError::IdentifierExhausted);
        }
        let free_cursor = if cursor.is_none() && has_more {
            Some(
                grant
                    .cursors
                    .iter()
                    .position(Option::is_none)
                    .ok_or(DiagnosticsError::CursorBudgetExceeded)?,
            )
        } else {
            None
        };
        let cursor_token = if has_more {
            Some(grant.next_cursor_token)
        } else {
            None
        };
        let following_cursor_token = cursor_token
            .map(|token| {
                token
                    .checked_add(1)
                    .ok_or(DiagnosticsError::IdentifierExhausted)
            })
            .transpose()?;
        for entry in output.iter_mut().take(used).flatten() {
            entry.event_alias = match add_alias(
                grant,
                (service_id, process_generation),
                domain_index,
                grant_index,
                entry.partition_index,
                AliasTarget::Event {
                    partition: entry.partition_index,
                    sequence: entry.sequence,
                },
                generation,
            ) {
                Ok(alias) => alias,
                Err(error) => {
                    clear_entries(output);
                    return Err(error);
                }
            };
            if let Some(run_id) = entry.run_id {
                entry.run_alias = match add_alias(
                    grant,
                    (service_id, process_generation),
                    domain_index,
                    grant_index,
                    entry.partition_index,
                    AliasTarget::Run(run_id),
                    generation,
                ) {
                    Ok(alias) => Some(alias),
                    Err(error) => {
                        clear_entries(output);
                        return Err(error);
                    }
                };
            }
        }
        let next_cursor = if has_more {
            let slot = if let Some(handle) = cursor {
                handle.slot
            } else {
                free_cursor.ok_or(DiagnosticsError::CursorBudgetExceeded)?
            };
            let token = cursor_token.ok_or(DiagnosticsError::IdentifierExhausted)?;
            grant.next_cursor_token =
                following_cursor_token.ok_or(DiagnosticsError::IdentifierExhausted)?;
            state.token = token;
            grant.cursors[slot] = Some(state);
            Some(LogCursor {
                service,
                generation,
                domain_index,
                grant_index,
                slot,
                token: state.token,
            })
        } else {
            if let Some(handle) = cursor {
                grant.cursors[handle.slot] = None;
            }
            None
        };
        Ok(QueryPage {
            returned: used,
            has_more,
            gap: if gap { Gap::Possible } else { Gap::None },
            next_cursor,
        })
    }

    pub fn health_into(
        &mut self,
        access: &LogAccess,
        clock: ClockReading,
        output: &mut [Option<HealthSnapshot>],
    ) -> Result<usize, DiagnosticsError> {
        output.fill(None);
        let (domain_index, grant_index) = self.validate_access(access, clock)?;
        let domain = &self
            .config
            .as_ref()
            .ok_or(DiagnosticsError::Unavailable)?
            .domains[domain_index];
        let grant = domain.grants[grant_index].grant;
        let count = visible_partition_count(grant);
        if output.len() < count {
            return Err(DiagnosticsError::InvalidLimit);
        }
        let mut written = 0;
        for (index, partition) in domain.partitions.iter().enumerate() {
            let (category, detail) = coordinates(index);
            if grant.categories.contains(category) && detail <= grant.maximum_detail {
                output[written] = Some(HealthSnapshot {
                    category,
                    detail,
                    capture_enabled: domain.capture.allows(category, detail),
                    retained: partition.retained,
                    dropped: partition.dropped,
                    accepted: partition.accepted,
                    omitted: partition.omitted,
                    rejected: partition.rejected,
                    clock_regressed: partition.clock_regressed,
                });
                written += 1;
            }
        }
        Ok(written)
    }

    pub fn list_runs_into(
        &mut self,
        access: &LogAccess,
        clock: ClockReading,
        output: &mut [Option<RunSummary>],
    ) -> Result<RunList, DiagnosticsError> {
        output.fill(None);
        let result = self.list_runs_into_inner(access, clock, output);
        if result.is_err() {
            output.fill(None);
        }
        result
    }

    fn list_runs_into_inner(
        &mut self,
        access: &LogAccess,
        clock: ClockReading,
        output: &mut [Option<RunSummary>],
    ) -> Result<RunList, DiagnosticsError> {
        let (domain_index, grant_index) = self.validate_access(access, clock)?;
        let generation = self.generation;
        let service_id = self
            .service_id
            .ok_or(DiagnosticsError::IdentifierExhausted)?;
        let config = self.config.as_mut().ok_or(DiagnosticsError::Unavailable)?;
        let process_generation = config.process_generation;
        let budgets = config.budgets;
        if output.len() > budgets.max_summaries_per_query {
            return Err(DiagnosticsError::InvalidLimit);
        }
        let domain = &mut config.domains[domain_index];
        let (partitions, grants) = (&domain.partitions, &mut domain.grants);
        let grant = &mut grants[grant_index];
        let mut count = 0usize;
        let mut work = 0usize;
        let mut truncated = false;
        for (partition_index, partition) in partitions.iter().enumerate() {
            let (category, detail) = coordinates(partition_index);
            if !grant.grant.categories.contains(category) || detail > grant.grant.maximum_detail {
                continue;
            }
            for sequence in partition.oldest_sequence()..=partition.last_sequence {
                work = work
                    .checked_add(1)
                    .ok_or(DiagnosticsError::QueryBudgetExceeded)?;
                if work > budgets.max_query_work {
                    output.fill(None);
                    return Err(DiagnosticsError::QueryBudgetExceeded);
                }
                let Some(event) = partition.get(sequence) else {
                    continue;
                };
                let Some(run_id) = event.run_id else {
                    continue;
                };
                if let Some(existing) = output[..count]
                    .iter_mut()
                    .flatten()
                    .find(|summary| summary.run_id == run_id)
                {
                    existing.observe(event, partition_index);
                } else if count < output.len() {
                    output[count] = Some(RunSummary::from_event(run_id, event, partition_index));
                    count += 1;
                } else {
                    truncated = true;
                }
            }
        }
        let alias_needs = missing_run_aliases(grant, output, count);
        for (partition, needed) in alias_needs.slots.iter().enumerate() {
            let aliases = &grant.aliases[partition];
            if aliases
                .entries
                .iter()
                .filter(|entry| entry.is_none())
                .count()
                < *needed
            {
                output.fill(None);
                return Err(DiagnosticsError::AliasBudgetExceeded);
            }
            let event_aliases = u64::try_from(alias_needs.event_ids[partition])
                .map_err(|_| DiagnosticsError::IdentifierExhausted)?;
            if aliases
                .next_event_alias
                .checked_add(event_aliases)
                .is_none()
            {
                output.fill(None);
                return Err(DiagnosticsError::IdentifierExhausted);
            }
        }
        let new_run_aliases = u64::try_from(alias_needs.new_run_ids)
            .map_err(|_| DiagnosticsError::IdentifierExhausted)?;
        if grant.next_run_alias.checked_add(new_run_aliases).is_none() {
            output.fill(None);
            return Err(DiagnosticsError::IdentifierExhausted);
        }
        let incomplete = partitions.iter().enumerate().any(|(index, partition)| {
            let (category, detail) = coordinates(index);
            grant.grant.categories.contains(category)
                && detail <= grant.grant.maximum_detail
                && (partition.dropped > 0 || partition.rejected > 0 || partition.omitted > 0)
        });
        for summary in output.iter_mut().take(count).flatten() {
            let partition = summary.first_order.partition;
            summary.alias = Some(add_alias(
                grant,
                (service_id, process_generation),
                domain_index,
                grant_index,
                partition,
                AliasTarget::Run(summary.run_id),
                generation,
            )?);
            summary.evidence_complete = !incomplete;
            if incomplete && summary.operation_state == OperationState::InProgress {
                summary.operation_state = OperationState::Unknown;
            }
        }
        for left in 0..count {
            for right in (left + 1)..count {
                if output[right].as_ref().unwrap().first_order
                    < output[left].as_ref().unwrap().first_order
                {
                    output.swap(left, right);
                }
            }
        }
        Ok(RunList { count, truncated })
    }

    pub fn inspect_run_into(
        &mut self,
        access: &LogAccess,
        run: &RunHandle,
        clock: ClockReading,
        output: &mut [Option<LogEntry>],
    ) -> Result<QueryPage, DiagnosticsError> {
        output.fill(None);
        let (domain_index, grant_index) = self.validate_access(access, clock)?;
        self.validate_run(run, domain_index)?;
        let categories = self
            .config
            .as_ref()
            .ok_or(DiagnosticsError::Unavailable)?
            .domains[domain_index]
            .grants[grant_index]
            .grant
            .categories;
        self.query_into(
            access,
            LogFilter::all().categories(categories).for_run(run),
            None,
            clock,
            output,
        )
    }

    /// Resolves a run alias under current access and supports bounded cursor pagination.
    pub fn inspect_run_alias_into(
        &mut self,
        access: &LogAccess,
        alias: OpaqueAlias,
        filter: LogFilter,
        cursor: Option<&LogCursor>,
        clock: ClockReading,
        output: &mut [Option<LogEntry>],
    ) -> Result<QueryPage, DiagnosticsError> {
        output.fill(None);
        let (domain_index, grant_index) = self.validate_access(access, clock)?;
        let service_id = self
            .service_id
            .ok_or(DiagnosticsError::IdentifierExhausted)?;
        if alias.kind != AliasKind::Run
            || alias.service_id != service_id
            || self
                .config
                .as_ref()
                .is_none_or(|config| alias.process_generation != config.process_generation)
            || alias.generation != self.generation
            || alias.domain_index != domain_index
            || alias.grant_index != grant_index
        {
            return Err(DiagnosticsError::Unauthorized);
        }
        let grant = &self
            .config
            .as_ref()
            .ok_or(DiagnosticsError::Unavailable)?
            .domains[domain_index]
            .grants[grant_index];
        let run_id = grant
            .aliases
            .iter()
            .flat_map(|store| store.entries.iter().flatten())
            .find(|entry| entry.alias == alias)
            .and_then(|entry| match entry.target {
                AliasTarget::Run(run_id) => Some(run_id),
                AliasTarget::Event { .. } => None,
            })
            .ok_or(DiagnosticsError::Unauthorized)?;
        if !filter.categories.is_subset_of(grant.grant.categories) {
            return Err(DiagnosticsError::Unauthorized);
        }
        self.query_into(access, filter.for_run_id(run_id), cursor, clock, output)
    }

    pub fn close_cursor(&mut self, cursor: LogCursor) -> Result<(), DiagnosticsError> {
        self.validate_service(&cursor.service, cursor.generation)?;
        let grant = self
            .config
            .as_mut()
            .and_then(|config| config.domains.get_mut(cursor.domain_index))
            .and_then(|domain| domain.grants.get_mut(cursor.grant_index))
            .ok_or(DiagnosticsError::StaleHandle)?;
        if grant
            .cursors
            .get(cursor.slot)
            .and_then(|slot| *slot)
            .is_some_and(|state| state.token == cursor.token)
        {
            grant.cursors[cursor.slot] = None;
            Ok(())
        } else {
            Err(DiagnosticsError::StaleHandle)
        }
    }

    fn validate_access(
        &mut self,
        access: &LogAccess,
        clock: ClockReading,
    ) -> Result<(usize, usize), DiagnosticsError> {
        self.validate_service(&access.service, access.generation)?;
        let grant = self
            .config
            .as_mut()
            .ok_or(DiagnosticsError::Unavailable)?
            .domains
            .get_mut(access.domain_index)
            .ok_or(DiagnosticsError::StaleHandle)?
            .grants
            .get_mut(access.grant_index)
            .ok_or(DiagnosticsError::StaleHandle)?;
        if grant.grant.version != access.version {
            return Err(DiagnosticsError::Unauthorized);
        }
        validate_grant_clock(grant, clock.monotonic_ms)?;
        Ok((access.domain_index, access.grant_index))
    }
    fn validate_run(&self, run: &RunHandle, domain_index: usize) -> Result<(), DiagnosticsError> {
        self.validate_service(&run.service, run.generation)?;
        if run.domain_index != domain_index {
            return Err(DiagnosticsError::Unauthorized);
        }
        Ok(())
    }
    fn validate_emitter(&self, emitter: &LogEmitter) -> Result<(), DiagnosticsError> {
        self.validate_service(&emitter.service, emitter.generation)?;
        if self
            .config
            .as_ref()
            .and_then(|config| config.domains.get(emitter.domain_index))
            .is_none()
        {
            return Err(DiagnosticsError::StaleHandle);
        }
        Ok(())
    }
    fn validate_cursor(
        &self,
        cursor: &LogCursor,
        domain: usize,
        grant: usize,
    ) -> Result<(), DiagnosticsError> {
        self.validate_service(&cursor.service, cursor.generation)?;
        if cursor.domain_index != domain || cursor.grant_index != grant {
            return Err(DiagnosticsError::Unauthorized);
        }
        Ok(())
    }
    fn validate_service(
        &self,
        service: &Weak<()>,
        generation: u64,
    ) -> Result<(), DiagnosticsError> {
        if generation != self.generation || !Weak::ptr_eq(service, &Rc::downgrade(&self.identity)) {
            return Err(DiagnosticsError::StaleHandle);
        }
        Ok(())
    }
    fn domain_index(&self, key: StableDomain) -> Result<usize, DiagnosticsError> {
        let config = self.config.as_ref().ok_or(DiagnosticsError::Unavailable)?;
        config
            .domains
            .iter()
            .position(|domain| domain.key == key)
            .ok_or(DiagnosticsError::Unauthorized)
    }
}

impl Default for DiagnosticsService {
    fn default() -> Self {
        Self::disabled()
    }
}
impl fmt::Debug for DiagnosticsService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiagnosticsService")
            .field("enabled", &self.config.is_some())
            .field("generation", &"[redacted]")
            .finish()
    }
}

#[derive(Clone)]
pub struct LogAccess {
    service: Weak<()>,
    generation: u64,
    domain_index: usize,
    grant_index: usize,
    version: u64,
}
impl fmt::Debug for LogAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LogAccess([redacted])")
    }
}
pub struct LogEmitter {
    service: Weak<()>,
    generation: u64,
    domain_index: usize,
    run_id: Option<u64>,
}
impl fmt::Debug for LogEmitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LogEmitter([redacted])")
    }
}
pub struct RunHandle {
    service: Weak<()>,
    generation: u64,
    domain_index: usize,
    run_id: u64,
}
impl fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RunHandle([redacted])")
    }
}
#[derive(Clone)]
pub struct LogCursor {
    service: Weak<()>,
    generation: u64,
    domain_index: usize,
    grant_index: usize,
    slot: usize,
    token: u64,
}
impl fmt::Debug for LogCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LogCursor([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmitOutcome {
    Accepted,
    AcceptedWithEviction,
    Omitted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    None,
    Possible,
}
/// Orders pages by trusted acceptance order, independent of clock resolution or rollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageDirection {
    /// Return the oldest matching event first.
    OldestFirst,
    /// Return the newest matching event first.
    NewestFirst,
}
pub struct QueryPage {
    pub returned: usize,
    pub has_more: bool,
    pub gap: Gap,
    pub next_cursor: Option<LogCursor>,
}
impl fmt::Debug for QueryPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryPage")
            .field("returned", &self.returned)
            .field("has_more", &self.has_more)
            .field("gap", &self.gap)
            .field(
                "next_cursor",
                &self.next_cursor.as_ref().map(|_| "[opaque]"),
            )
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LogFilter {
    categories: CategorySet,
    severity: Option<Severity>,
    source: Option<Source>,
    event_code: Option<EventCode>,
    run_id: Option<u64>,
    time_range: Option<TimeRange>,
    search: Option<SearchTerm>,
    direction: PageDirection,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimeRange {
    start_inclusive_ms: u64,
    end_exclusive_ms: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct SearchTerm {
    bytes: [u8; MAX_SEARCH_BYTES],
    length: u8,
}
impl SearchTerm {
    fn new(query: &str) -> Result<Self, DiagnosticsError> {
        if query.is_empty() || query.len() > MAX_SEARCH_BYTES || query.chars().any(char::is_control)
        {
            return Err(DiagnosticsError::InvalidSearch);
        }
        let mut bytes = [0; MAX_SEARCH_BYTES];
        bytes[..query.len()].copy_from_slice(query.as_bytes());
        Ok(Self {
            bytes,
            length: query.len() as u8,
        })
    }
    fn matches(self, value: &str) -> bool {
        let query = &self.bytes[..self.length as usize];
        let haystack = value.as_bytes();
        haystack.windows(query.len()).any(|window| {
            window
                .iter()
                .zip(query)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
        })
    }
}
impl LogFilter {
    pub const fn all() -> Self {
        Self {
            categories: CategorySet::all(),
            severity: None,
            source: None,
            event_code: None,
            run_id: None,
            time_range: None,
            search: None,
            direction: PageDirection::OldestFirst,
        }
    }
    pub const fn categories(mut self, value: CategorySet) -> Self {
        self.categories = value;
        self
    }
    pub const fn severity(mut self, value: Severity) -> Self {
        self.severity = Some(value);
        self
    }
    pub const fn source(mut self, value: Source) -> Self {
        self.source = Some(value);
        self
    }
    pub const fn event_code(mut self, value: EventCode) -> Self {
        self.event_code = Some(value);
        self
    }
    /// Filters on injected monotonic time with an inclusive start and exclusive end.
    pub fn time_range(
        mut self,
        start_inclusive_ms: u64,
        end_exclusive_ms: u64,
    ) -> Result<Self, DiagnosticsError> {
        if start_inclusive_ms >= end_exclusive_ms {
            return Err(DiagnosticsError::InvalidTimeRange);
        }
        self.time_range = Some(TimeRange {
            start_inclusive_ms,
            end_exclusive_ms,
        });
        Ok(self)
    }
    /// Searches fixed safe event codes and templates; accepts 1–96 UTF-8 bytes.
    /// Control characters are rejected and matching is ASCII case-insensitive.
    pub fn search(mut self, query: &str) -> Result<Self, DiagnosticsError> {
        self.search = Some(SearchTerm::new(query)?);
        Ok(self)
    }
    pub const fn direction(mut self, value: PageDirection) -> Self {
        self.direction = value;
        self
    }
    fn for_run(mut self, run: &RunHandle) -> Self {
        self.run_id = Some(run.run_id);
        self
    }
    fn for_run_id(mut self, run_id: u64) -> Self {
        self.run_id = Some(run_id);
        self
    }
}
impl fmt::Debug for LogFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogFilter")
            .field(stringify!(categories), &self.categories)
            .field(stringify!(severity), &self.severity)
            .field(stringify!(source), &self.source)
            .field(stringify!(event_code), &self.event_code)
            .field(stringify!(has_run), &self.run_id.is_some())
            .field(stringify!(time_range), &self.time_range)
            .field(stringify!(has_search), &self.search.is_some())
            .field(stringify!(direction), &self.direction)
            .finish()
    }
}

/// A process-local, view-bound alias, not an authorization capability.
///
/// Hashing is intentionally unavailable: a caller-controlled hasher must not extract
/// private service, domain, subject or sequence bindings. Use bounded equality-based indexes.
///
/// ```compile_fail
/// use moe_agent_runtime::OpaqueAlias;
/// fn requires_hash<Value: std::hash::Hash>() {}
/// requires_hash::<OpaqueAlias>();
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OpaqueAlias {
    service_id: u64,
    process_generation: u128,
    generation: u64,
    domain_index: usize,
    grant_index: usize,
    partition_index: usize,
    ordinal: u64,
    kind: AliasKind,
}
impl fmt::Debug for OpaqueAlias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpaqueAlias([redacted])")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum AliasKind {
    Event,
    Run,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redaction {
    MetadataOnly,
}

#[derive(Clone, Copy)]
pub struct LogEntry {
    event_alias: OpaqueAlias,
    run_alias: Option<OpaqueAlias>,
    occurred_at_ms: Option<u64>,
    elapsed_ms: Option<u64>,
    source: Source,
    category: Category,
    detail: DetailLevel,
    severity: Severity,
    event_code: EventCode,
    message_key: MessageKey,
    retry_hint: Option<RetryHint>,
    operation_state: Option<OperationState>,
    transport_state: Option<TransportState>,
    redaction: Redaction,
    run_id: Option<u64>,
    partition_index: usize,
    sequence: u64,
    order: OrderKey,
}
impl LogEntry {
    pub const fn schema_version(&self) -> u16 {
        LOG_SCHEMA_VERSION
    }
    pub const fn event_alias(&self) -> OpaqueAlias {
        self.event_alias
    }
    pub const fn run_alias(&self) -> Option<OpaqueAlias> {
        self.run_alias
    }
    pub const fn occurred_at_ms(&self) -> Option<u64> {
        self.occurred_at_ms
    }
    pub const fn elapsed_ms(&self) -> Option<u64> {
        self.elapsed_ms
    }
    pub const fn source(&self) -> Source {
        self.source
    }
    pub const fn category(&self) -> Category {
        self.category
    }
    pub const fn detail(&self) -> DetailLevel {
        self.detail
    }
    pub const fn severity(&self) -> Severity {
        self.severity
    }
    pub const fn event_code(&self) -> EventCode {
        self.event_code
    }
    pub const fn message_key(&self) -> MessageKey {
        self.message_key
    }
    pub const fn retry_hint(&self) -> Option<RetryHint> {
        self.retry_hint
    }
    pub const fn operation_state(&self) -> Option<OperationState> {
        self.operation_state
    }
    pub const fn transport_state(&self) -> Option<TransportState> {
        self.transport_state
    }
    pub const fn redaction(&self) -> Redaction {
        self.redaction
    }
}
impl PartialEq for LogEntry {
    fn eq(&self, other: &Self) -> bool {
        self.event_alias == other.event_alias
            && self.run_alias == other.run_alias
            && self.occurred_at_ms == other.occurred_at_ms
            && self.elapsed_ms == other.elapsed_ms
            && self.source == other.source
            && self.category == other.category
            && self.detail == other.detail
            && self.severity == other.severity
            && self.event_code == other.event_code
            && self.message_key == other.message_key
            && self.retry_hint == other.retry_hint
            && self.operation_state == other.operation_state
            && self.transport_state == other.transport_state
            && self.redaction == other.redaction
    }
}
impl Eq for LogEntry {}
impl fmt::Debug for LogEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogEntry")
            .field("event_alias", &self.event_alias)
            .field("run_alias", &self.run_alias)
            .field("occurred_at_ms", &self.occurred_at_ms)
            .field("source", &self.source)
            .field("category", &self.category)
            .field("detail", &self.detail)
            .field("severity", &self.severity)
            .field("event_code", &self.event_code)
            .field("redaction", &self.redaction)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub category: Category,
    pub detail: DetailLevel,
    pub capture_enabled: bool,
    pub retained: usize,
    pub dropped: u64,
    pub accepted: u64,
    pub omitted: u64,
    pub rejected: u64,
    pub clock_regressed: bool,
}
#[derive(Clone, Copy)]
pub struct RunSummary {
    alias: Option<OpaqueAlias>,
    operation_state: OperationState,
    transport_state: TransportState,
    operation_order: Option<OrderKey>,
    transport_order: Option<OrderKey>,
    first_seen_at_ms: Option<u64>,
    last_seen_at_ms: Option<u64>,
    evidence_complete: bool,
    run_id: u64,
    first_order: OrderKey,
    latest_order: OrderKey,
}
impl RunSummary {
    /// Returns the grant-scoped alias after a successful run listing.
    pub const fn alias(&self) -> Option<OpaqueAlias> {
        self.alias
    }
    pub const fn operation_state(&self) -> OperationState {
        self.operation_state
    }
    pub const fn transport_state(&self) -> TransportState {
        self.transport_state
    }
    pub const fn first_seen_at_ms(&self) -> Option<u64> {
        self.first_seen_at_ms
    }
    pub const fn last_seen_at_ms(&self) -> Option<u64> {
        self.last_seen_at_ms
    }
    pub const fn evidence_complete(&self) -> bool {
        self.evidence_complete
    }
    fn from_event(run_id: u64, event: StoredEvent, partition: usize) -> Self {
        let order = OrderKey::from_event(event, partition);
        let mut summary = Self {
            alias: None,
            operation_state: OperationState::Unknown,
            transport_state: TransportState::Unknown,
            operation_order: None,
            transport_order: None,
            first_seen_at_ms: event.wall_clock_ms,
            last_seen_at_ms: event.wall_clock_ms,
            evidence_complete: true,
            run_id,
            first_order: order,
            latest_order: order,
        };
        summary.observe(event, partition);
        summary
    }
    fn observe(&mut self, event: StoredEvent, partition: usize) {
        let order = OrderKey::from_event(event, partition);
        if order < self.first_order {
            self.first_order = order;
            self.first_seen_at_ms = event.wall_clock_ms;
        }
        if order >= self.latest_order {
            self.latest_order = order;
            self.last_seen_at_ms = event.wall_clock_ms;
        }
        if let Some(state) = event.kind.operation_state()
            && self.operation_order.is_none_or(|latest| order >= latest)
        {
            self.operation_order = Some(order);
            self.operation_state = state;
        }
        if let Some(state) = event.kind.transport_state()
            && self.transport_order.is_none_or(|latest| order >= latest)
        {
            self.transport_order = Some(order);
            self.transport_state = state;
        }
    }
}
impl fmt::Debug for RunSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunSummary")
            .field("alias", &self.alias)
            .field("operation_state", &self.operation_state)
            .field("transport_state", &self.transport_state)
            .field("evidence_complete", &self.evidence_complete)
            .finish()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunList {
    pub count: usize,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticsError {
    InvalidConfiguration,
    CapacityExceeded,
    Unavailable,
    Unauthorized,
    StaleHandle,
    InvalidLimit,
    InvalidTimeRange,
    InvalidSearch,
    InvalidCursor,
    ClockWentBackwards,
    IdentifierExhausted,
    RunBudgetExceeded,
    AliasBudgetExceeded,
    CursorBudgetExceeded,
    QueryBudgetExceeded,
}
impl fmt::Display for DiagnosticsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "diagnostics configuration is invalid",
            Self::CapacityExceeded => "diagnostics capacity is unavailable",
            Self::Unavailable => "diagnostics are unavailable",
            Self::Unauthorized => "diagnostics access is not authorized",
            Self::StaleHandle => "diagnostics handle is no longer current",
            Self::InvalidLimit => "diagnostics query limit is invalid",
            Self::InvalidTimeRange => "diagnostics time range is invalid",
            Self::InvalidSearch => "diagnostics search is invalid",
            Self::InvalidCursor => "diagnostics cursor does not match the query",
            Self::ClockWentBackwards => "injected monotonic time moved backwards",
            Self::IdentifierExhausted => "diagnostics identifier space is exhausted",
            Self::RunBudgetExceeded => "diagnostics run budget is exhausted",
            Self::AliasBudgetExceeded => "diagnostics alias budget is exhausted",
            Self::CursorBudgetExceeded => "diagnostics cursor budget is exhausted",
            Self::QueryBudgetExceeded => "diagnostics query budget is exhausted",
        })
    }
}
impl std::error::Error for DiagnosticsError {}

#[derive(Clone, Copy)]
struct StoredEvent {
    sequence: u64,
    acceptance_order: u128,
    monotonic_ms: u64,
    wall_clock_ms: Option<u64>,
    elapsed_ms: Option<u64>,
    source: Source,
    detail: DetailLevel,
    severity: Severity,
    kind: DiagnosticKind,
    run_id: Option<u64>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum AliasTarget {
    Event { partition: usize, sequence: u64 },
    Run(u64),
}
#[derive(Clone, Copy)]
struct AliasEntry {
    target: AliasTarget,
    alias: OpaqueAlias,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct OrderKey {
    acceptance_order: u128,
    partition: usize,
    sequence: u64,
}
impl OrderKey {
    fn from_event(event: StoredEvent, partition: usize) -> Self {
        Self {
            acceptance_order: event.acceptance_order,
            partition,
            sequence: event.sequence,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorPartition {
    high_water: u64,
    last_sequence: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorState {
    token: u64,
    filter: LogFilter,
    partitions: [CursorPartition; PARTITION_COUNT],
}
impl CursorState {
    fn new(
        filter: LogFilter,
        partitions: &[PartitionState; PARTITION_COUNT],
        grant: ViewerGrant,
    ) -> Self {
        let mut state = Self {
            token: 0,
            filter,
            partitions: [CursorPartition {
                high_water: 0,
                last_sequence: 0,
            }; PARTITION_COUNT],
        };
        for (index, partition) in partitions.iter().enumerate() {
            let (category, detail) = coordinates(index);
            if grant.categories.contains(category)
                && filter.categories.contains(category)
                && detail <= grant.maximum_detail
            {
                state.partitions[index].high_water = partition.last_sequence;
            }
        }
        state
    }
}
#[derive(Clone, Copy)]
struct PageCandidate {
    event: StoredEvent,
    partition: usize,
    key: OrderKey,
}

fn coordinates(index: usize) -> (Category, DetailLevel) {
    let category = match index / DETAIL_COUNT {
        0 => Category::Background,
        1 => Category::Error,
        _ => Category::Run,
    };
    let detail = match index % DETAIL_COUNT {
        0 => DetailLevel::Essential,
        1 => DetailLevel::Standard,
        _ => DetailLevel::Verbose,
    };
    (category, detail)
}
fn partition_index(category: Category, detail: DetailLevel) -> usize {
    category.index() * DETAIL_COUNT + detail as usize
}
fn visible_partition_count(grant: ViewerGrant) -> usize {
    (0..PARTITION_COUNT)
        .filter(|index| {
            let (category, detail) = coordinates(*index);
            grant.categories.contains(category) && detail <= grant.maximum_detail
        })
        .count()
}
fn increment(value: &mut u64) {
    *value = value.saturating_add(1);
}

fn validate_grants(grants: &[ViewerGrant], budgets: HardBudgets) -> Result<(), DiagnosticsError> {
    if grants.len() > budgets.max_grants_per_domain {
        return Err(DiagnosticsError::CapacityExceeded);
    }
    for (index, grant) in grants.iter().enumerate() {
        if grant.version == 0 || grant.categories == CategorySet::empty() {
            return Err(DiagnosticsError::InvalidConfiguration);
        }
        if grants[..index]
            .iter()
            .any(|previous| previous.subject == grant.subject)
        {
            return Err(DiagnosticsError::InvalidConfiguration);
        }
    }
    Ok(())
}

fn validate_grant_clock(grant: &mut GrantState, now_ms: u64) -> Result<(), DiagnosticsError> {
    if grant.clock_regressed {
        return Err(DiagnosticsError::ClockWentBackwards);
    }
    if grant.expired {
        return Err(DiagnosticsError::Unauthorized);
    }
    if grant
        .last_verified_monotonic_ms
        .is_some_and(|last| now_ms < last)
    {
        grant.clock_regressed = true;
        return Err(DiagnosticsError::ClockWentBackwards);
    }
    grant.last_verified_monotonic_ms = Some(now_ms);
    if now_ms >= grant.grant.expires_at_monotonic_ms {
        grant.expired = true;
        return Err(DiagnosticsError::Unauthorized);
    }
    Ok(())
}

fn domain_dynamic_bytes(
    budgets: HardBudgets,
    grant_count: usize,
) -> Result<usize, DiagnosticsError> {
    let event_bytes = budgets
        .events_per_partition
        .checked_mul(size_of::<Option<StoredEvent>>())
        .ok_or(DiagnosticsError::CapacityExceeded)?;
    let run_bytes = budgets
        .runs_per_partition
        .checked_mul(size_of::<Option<u64>>())
        .ok_or(DiagnosticsError::CapacityExceeded)?;
    let partition_bytes = event_bytes
        .checked_add(run_bytes)
        .and_then(|bytes| bytes.checked_add(ALLOCATION_OVERHEAD_BYTES * 2))
        .and_then(|bytes| bytes.checked_mul(PARTITION_COUNT))
        .ok_or(DiagnosticsError::CapacityExceeded)?;
    let grant_headers = grant_count
        .checked_mul(size_of::<GrantState>())
        .and_then(|bytes| bytes.checked_add(ALLOCATION_OVERHEAD_BYTES))
        .ok_or(DiagnosticsError::CapacityExceeded)?;
    let alias_bytes = budgets
        .aliases_per_partition
        .checked_mul(size_of::<Option<AliasEntry>>())
        .and_then(|bytes| bytes.checked_add(ALLOCATION_OVERHEAD_BYTES))
        .and_then(|bytes| bytes.checked_mul(PARTITION_COUNT))
        .ok_or(DiagnosticsError::CapacityExceeded)?;
    let cursor_bytes = budgets
        .cursors_per_grant
        .checked_mul(size_of::<Option<CursorState>>())
        .ok_or(DiagnosticsError::CapacityExceeded)?;
    let per_grant = alias_bytes
        .checked_add(cursor_bytes)
        .and_then(|bytes| bytes.checked_add(ALLOCATION_OVERHEAD_BYTES))
        .and_then(|bytes| bytes.checked_mul(grant_count))
        .ok_or(DiagnosticsError::CapacityExceeded)?;
    partition_bytes
        .checked_add(grant_headers)
        .and_then(|bytes| bytes.checked_add(per_grant))
        .ok_or(DiagnosticsError::CapacityExceeded)
}
fn placeholder_alias(
    service_id: u64,
    generation: u64,
    domain_index: usize,
    grant_index: usize,
    partition_index: usize,
    kind: AliasKind,
) -> OpaqueAlias {
    OpaqueAlias {
        service_id,
        process_generation: 0,
        generation,
        domain_index,
        grant_index,
        partition_index,
        ordinal: 0,
        kind,
    }
}
fn clear_entries(output: &mut [Option<LogEntry>]) {
    output.fill(None);
}

fn matches_filter(event: StoredEvent, filter: LogFilter) -> bool {
    filter.severity.is_none_or(|value| event.severity == value)
        && filter.source.is_none_or(|value| event.source == value)
        && filter
            .event_code
            .is_none_or(|value| event.kind.code() == value)
        && filter
            .run_id
            .is_none_or(|value| event.run_id == Some(value))
        && filter.time_range.is_none_or(|range| {
            event.monotonic_ms >= range.start_inclusive_ms
                && event.monotonic_ms < range.end_exclusive_ms
        })
        && filter.search.is_none_or(|search| {
            let (code, template) = event.kind.message_key().safe_search_text();
            search.matches(code) || search.matches(template)
        })
}

fn entry_from_event(candidate: PageCandidate) -> LogEntry {
    let event = candidate.event;
    LogEntry {
        event_alias: placeholder_alias(0, 0, 0, 0, candidate.partition, AliasKind::Event),
        run_alias: None,
        occurred_at_ms: event.wall_clock_ms,
        elapsed_ms: event.elapsed_ms,
        source: event.source,
        category: event.kind.category(),
        detail: event.detail,
        severity: event.severity,
        event_code: event.kind.code(),
        message_key: event.kind.message_key(),
        retry_hint: event.kind.retry_hint(),
        operation_state: event.kind.operation_state(),
        transport_state: event.kind.transport_state(),
        redaction: Redaction::MetadataOnly,
        run_id: event.run_id,
        partition_index: candidate.partition,
        sequence: event.sequence,
        order: candidate.key,
    }
}

fn collect_page(
    partitions: &[PartitionState; PARTITION_COUNT],
    grant: ViewerGrant,
    state: &mut CursorState,
    budgets: HardBudgets,
    output: &mut [Option<LogEntry>],
) -> Result<(usize, usize, bool), DiagnosticsError> {
    let mut work = 0usize;
    let mut used = 0usize;
    let mut total = 0usize;
    let mut gap = false;
    for (index, partition) in partitions.iter().enumerate() {
        let (category, detail) = coordinates(index);
        if !grant.categories.contains(category)
            || detail > grant.maximum_detail
            || !state.filter.categories.contains(category)
        {
            continue;
        }
        let part_cursor = &mut state.partitions[index];
        let oldest = partition.oldest_sequence();
        let next_expected = match state.filter.direction {
            PageDirection::OldestFirst => part_cursor.last_sequence.saturating_add(1),
            PageDirection::NewestFirst if part_cursor.last_sequence == 0 => part_cursor.high_water,
            PageDirection::NewestFirst => part_cursor.last_sequence.saturating_sub(1),
        };
        let lost_before_cursor = match state.filter.direction {
            PageDirection::OldestFirst => {
                next_expected <= part_cursor.high_water && oldest > next_expected
            }
            PageDirection::NewestFirst => next_expected > 0 && oldest > 1,
        };
        if lost_before_cursor {
            gap = true;
            if state.filter.direction == PageDirection::OldestFirst {
                part_cursor.last_sequence = oldest.saturating_sub(1).min(part_cursor.high_water);
            }
        }
        let snapshot_end = partition.last_sequence.min(part_cursor.high_water);
        for sequence in oldest..=snapshot_end {
            work = work
                .checked_add(1)
                .ok_or(DiagnosticsError::QueryBudgetExceeded)?;
            if work > budgets.max_query_work {
                clear_entries(output);
                return Err(DiagnosticsError::QueryBudgetExceeded);
            }
            let already_passed = match state.filter.direction {
                PageDirection::OldestFirst => sequence <= part_cursor.last_sequence,
                PageDirection::NewestFirst => {
                    part_cursor.last_sequence != 0 && sequence >= part_cursor.last_sequence
                }
            };
            if already_passed || sequence > part_cursor.high_water {
                continue;
            }
            let Some(event) = partition.get(sequence) else {
                continue;
            };
            if !matches_filter(event, state.filter) {
                continue;
            }
            total = total
                .checked_add(1)
                .ok_or(DiagnosticsError::QueryBudgetExceeded)?;
            let candidate = PageCandidate {
                event,
                partition: index,
                key: OrderKey::from_event(event, index),
            };
            let mut insert_at = used;
            for (position, existing) in output.iter().take(used).enumerate() {
                work = work
                    .checked_add(1)
                    .ok_or(DiagnosticsError::QueryBudgetExceeded)?;
                if work > budgets.max_query_work {
                    clear_entries(output);
                    return Err(DiagnosticsError::QueryBudgetExceeded);
                }
                let precedes = existing.is_some_and(|entry| match state.filter.direction {
                    PageDirection::OldestFirst => candidate.key < entry.order,
                    PageDirection::NewestFirst => candidate.key > entry.order,
                });
                if precedes {
                    insert_at = position;
                    break;
                }
            }
            let new_used = (used + 1).min(output.len());
            if insert_at < new_used {
                for position in (insert_at + 1..new_used).rev() {
                    output[position] = output[position - 1];
                }
                output[insert_at] = Some(entry_from_event(candidate));
                used = new_used;
            }
        }
    }
    for entry in output.iter().take(used).flatten() {
        let cursor = &mut state.partitions[entry.partition_index];
        cursor.last_sequence = match state.filter.direction {
            PageDirection::OldestFirst => cursor.last_sequence.max(entry.sequence),
            PageDirection::NewestFirst if cursor.last_sequence == 0 => entry.sequence,
            PageDirection::NewestFirst => cursor.last_sequence.min(entry.sequence),
        };
    }
    Ok((used, total, gap))
}

struct AliasNeeds {
    slots: [usize; PARTITION_COUNT],
    event_ids: [usize; PARTITION_COUNT],
    new_run_ids: usize,
}

fn missing_aliases(grant: &GrantState, output: &[Option<LogEntry>], count: usize) -> AliasNeeds {
    let mut missing = AliasNeeds {
        slots: [0; PARTITION_COUNT],
        event_ids: [0; PARTITION_COUNT],
        new_run_ids: 0,
    };
    for (index, entry) in output.iter().take(count).flatten().enumerate() {
        let partition = entry.partition_index;
        let aliases = &grant.aliases[partition];
        let event_target = AliasTarget::Event {
            partition,
            sequence: entry.sequence,
        };
        if !aliases
            .entries
            .iter()
            .flatten()
            .any(|alias| alias.target == event_target)
        {
            missing.slots[partition] += 1;
            missing.event_ids[partition] += 1;
        }
        if let Some(run_id) = entry.run_id {
            let target = AliasTarget::Run(run_id);
            let local_missing = !aliases
                .entries
                .iter()
                .flatten()
                .any(|alias| alias.target == target);
            let appeared_locally =
                output.iter().take(index).flatten().any(|prior| {
                    prior.partition_index == partition && prior.run_id == Some(run_id)
                });
            if local_missing && !appeared_locally {
                missing.slots[partition] += 1;
            }
            let known_globally = grant.aliases.iter().any(|store| {
                store
                    .entries
                    .iter()
                    .flatten()
                    .any(|alias| alias.target == target)
            });
            let appeared_globally = output
                .iter()
                .take(index)
                .flatten()
                .any(|prior| prior.run_id == Some(run_id));
            if !known_globally && !appeared_globally {
                missing.new_run_ids += 1;
            }
        }
    }
    missing
}

fn missing_run_aliases(
    grant: &GrantState,
    output: &[Option<RunSummary>],
    count: usize,
) -> AliasNeeds {
    let mut missing = AliasNeeds {
        slots: [0; PARTITION_COUNT],
        event_ids: [0; PARTITION_COUNT],
        new_run_ids: 0,
    };
    for (index, summary) in output.iter().take(count).flatten().enumerate() {
        let partition = summary.first_order.partition;
        let target = AliasTarget::Run(summary.run_id);
        if !grant.aliases[partition]
            .entries
            .iter()
            .flatten()
            .any(|alias| alias.target == target)
        {
            missing.slots[partition] += 1;
        }
        let known_globally = grant.aliases.iter().any(|store| {
            store
                .entries
                .iter()
                .flatten()
                .any(|alias| alias.target == target)
        });
        let appeared = output
            .iter()
            .take(index)
            .flatten()
            .any(|prior| prior.run_id == summary.run_id);
        if !known_globally && !appeared {
            missing.new_run_ids += 1;
        }
    }
    missing
}

fn add_alias(
    grant: &mut GrantState,
    namespace: (u64, u128),
    domain_index: usize,
    grant_index: usize,
    partition_index: usize,
    target: AliasTarget,
    generation: u64,
) -> Result<OpaqueAlias, DiagnosticsError> {
    let (service_id, process_generation) = namespace;
    if let Some(entry) = grant.aliases[partition_index]
        .entries
        .iter()
        .flatten()
        .find(|entry| entry.target == target)
    {
        return Ok(entry.alias);
    }
    let slot_index = grant.aliases[partition_index]
        .entries
        .iter()
        .position(Option::is_none)
        .ok_or(DiagnosticsError::AliasBudgetExceeded)?;
    let alias = match target {
        AliasTarget::Event { .. } => {
            let store = &mut grant.aliases[partition_index];
            let ordinal = store.next_event_alias;
            let next = ordinal
                .checked_add(1)
                .ok_or(DiagnosticsError::IdentifierExhausted)?;
            store.next_event_alias = next;
            OpaqueAlias {
                service_id,
                process_generation,
                generation,
                domain_index,
                grant_index,
                partition_index,
                ordinal,
                kind: AliasKind::Event,
            }
        }
        AliasTarget::Run(run_id) => {
            let existing = grant.aliases.iter().find_map(|store| {
                store
                    .entries
                    .iter()
                    .flatten()
                    .find(|entry| entry.target == AliasTarget::Run(run_id))
                    .map(|entry| entry.alias)
            });
            if let Some(alias) = existing {
                alias
            } else {
                let ordinal = grant.next_run_alias;
                let next = ordinal
                    .checked_add(1)
                    .ok_or(DiagnosticsError::IdentifierExhausted)?;
                grant.next_run_alias = next;
                OpaqueAlias {
                    service_id,
                    process_generation,
                    generation,
                    domain_index,
                    grant_index,
                    partition_index: PARTITION_COUNT,
                    ordinal,
                    kind: AliasKind::Run,
                }
            }
        }
    };
    grant.aliases[partition_index].entries[slot_index] = Some(AliasEntry { target, alias });
    Ok(alias)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_budgets() -> HardBudgets {
        HardBudgets {
            max_domains: 1,
            max_grants_per_domain: 1,
            events_per_partition: 4,
            runs_per_partition: 4,
            aliases_per_partition: 4,
            cursors_per_grant: 2,
            max_page_size: 2,
            max_query_work: 1_000,
            max_summaries_per_query: 8,
            max_health_slots: PARTITION_COUNT,
            max_concurrent_queries: 1,
            max_event_bytes: 2_048,
            max_allocated_bytes: 1_000_000,
        }
    }

    fn configured() -> (DiagnosticsService, StableDomain, ViewerSubject) {
        let domain = StableDomain::new("test-domain").unwrap();
        let subject = ViewerSubject::new("test-viewer").unwrap();
        let grant = ViewerGrant::new(
            subject,
            1,
            10_000,
            CategorySet::all(),
            DetailLevel::Essential,
        );
        let mut config = DiagnosticsConfig::new(1, test_budgets()).unwrap();
        config
            .add_domain(
                domain,
                CaptureStrategy::all_through(DetailLevel::Verbose),
                &[grant],
            )
            .unwrap();
        let mut service = DiagnosticsService::default();
        service.enable(config).unwrap();
        (service, domain, subject)
    }

    fn clock(monotonic_ms: u64) -> ClockReading {
        ClockReading {
            monotonic_ms,
            wall_clock_ms: Some(monotonic_ms),
        }
    }

    fn background_event(detail: DetailLevel) -> EventInput {
        EventInput {
            source: Source::Host,
            detail,
            elapsed_ms: None,
            kind: DiagnosticKind::Background(BackgroundCode::Waiting),
        }
    }

    #[test]
    fn hidden_partitions_do_not_advance_visible_cursor_or_run_aliases() {
        let (mut service, domain, subject) = configured();
        let access = service.authorize(domain, subject, 1, clock(0)).unwrap();
        let emitter = service.create_emitter(domain, None).unwrap();
        service
            .emit(&emitter, background_event(DetailLevel::Essential), clock(1))
            .unwrap();
        service
            .emit(&emitter, background_event(DetailLevel::Essential), clock(2))
            .unwrap();
        service
            .emit(&emitter, background_event(DetailLevel::Verbose), clock(3))
            .unwrap();

        let hidden_run = service.create_run(domain).unwrap();
        let hidden_emitter = service.create_emitter(domain, Some(&hidden_run)).unwrap();
        service
            .emit(
                &hidden_emitter,
                EventInput {
                    source: Source::Agent,
                    detail: DetailLevel::Verbose,
                    elapsed_ms: None,
                    kind: DiagnosticKind::Run(RunObservation::Operation(OperationState::Completed)),
                },
                clock(4),
            )
            .unwrap();
        let visible_run = service.create_run(domain).unwrap();
        let visible_emitter = service.create_emitter(domain, Some(&visible_run)).unwrap();
        service
            .emit(
                &visible_emitter,
                EventInput {
                    source: Source::Agent,
                    detail: DetailLevel::Essential,
                    elapsed_ms: None,
                    kind: DiagnosticKind::Run(RunObservation::Operation(OperationState::Completed)),
                },
                clock(5),
            )
            .unwrap();

        let mut entries = [None; 1];
        let page = service
            .query_into(
                &access,
                LogFilter::all().categories(CategorySet::only(Category::Background)),
                None,
                clock(6),
                &mut entries,
            )
            .unwrap();
        let cursor = page.next_cursor.unwrap();
        let hidden_background = partition_index(Category::Background, DetailLevel::Verbose);
        let high_water = service.config.as_ref().unwrap().domains[0].grants[0].cursors[cursor.slot]
            .unwrap()
            .partitions[hidden_background]
            .high_water;
        assert_eq!(high_water, 0);

        let mut summaries = [None; 8];
        let listing = service
            .list_runs_into(&access, clock(7), &mut summaries)
            .unwrap();
        assert_eq!(listing.count, 1);
        let run_alias = summaries[0].unwrap().alias.unwrap();
        assert_eq!(run_alias.ordinal, 1);
        assert_eq!(
            service.config.as_ref().unwrap().domains[0].grants[0].next_run_alias,
            2
        );
    }

    #[test]
    fn checked_identifier_exhaustion_never_wraps() {
        let (mut service, domain, _subject) = configured();
        service.config.as_mut().unwrap().domains[0].next_run_id = u64::MAX;
        assert!(matches!(
            service.create_run(domain),
            Err(DiagnosticsError::IdentifierExhausted)
        ));

        let (mut service, domain, _) = configured();
        let emitter = service.create_emitter(domain, None).unwrap();
        service.config.as_mut().unwrap().domains[0].partitions[0].last_sequence = u64::MAX;
        assert!(matches!(
            service.emit(&emitter, background_event(DetailLevel::Essential), clock(1)),
            Err(DiagnosticsError::IdentifierExhausted)
        ));

        let (mut service, domain, subject) = configured();
        let access = service.authorize(domain, subject, 1, clock(0)).unwrap();
        let emitter = service.create_emitter(domain, None).unwrap();
        for tick in 1..=2 {
            service
                .emit(
                    &emitter,
                    background_event(DetailLevel::Essential),
                    clock(tick),
                )
                .unwrap();
        }
        service.config.as_mut().unwrap().domains[0].grants[0].next_cursor_token = u64::MAX;
        let mut output = [None; 1];
        assert!(matches!(
            service.query_into(
                &access,
                LogFilter::all().categories(CategorySet::only(Category::Background)),
                None,
                clock(3),
                &mut output,
            ),
            Err(DiagnosticsError::IdentifierExhausted)
        ));
        assert!(output[0].is_none());

        let subject = ViewerSubject::new("alias-viewer").unwrap();
        let viewer_grant = ViewerGrant::new(
            subject,
            1,
            10_000,
            CategorySet::all(),
            DetailLevel::Essential,
        );
        let mut grant_state = GrantState::new(viewer_grant, test_budgets()).unwrap();
        grant_state.aliases[0].next_event_alias = u64::MAX;
        assert!(matches!(
            add_alias(
                &mut grant_state,
                (1, 1),
                0,
                0,
                0,
                AliasTarget::Event {
                    partition: 0,
                    sequence: 1,
                },
                1,
            ),
            Err(DiagnosticsError::IdentifierExhausted)
        ));
        grant_state.next_run_alias = u64::MAX;
        assert!(matches!(
            add_alias(
                &mut grant_state,
                (1, 1),
                0,
                0,
                partition_index(Category::Run, DetailLevel::Essential),
                AliasTarget::Run(1),
                1,
            ),
            Err(DiagnosticsError::IdentifierExhausted)
        ));

        let mut service = DiagnosticsService::disabled();
        service.generation = u64::MAX;
        assert!(matches!(
            service.disable(),
            Err(DiagnosticsError::IdentifierExhausted)
        ));
    }

    #[test]
    fn hidden_sequence_exhaustion_does_not_block_visible_ordering() {
        let (mut service, domain, subject) = configured();
        let access = service.authorize(domain, subject, 1, clock(0)).unwrap();
        let hidden_index = partition_index(Category::Run, DetailLevel::Verbose);
        service.config.as_mut().unwrap().domains[0].partitions[hidden_index].last_sequence =
            u64::MAX;
        let emitter = service.create_emitter(domain, None).unwrap();
        service
            .emit(&emitter, background_event(DetailLevel::Essential), clock(1))
            .unwrap();
        let mut entries = [None];
        service
            .query_into(
                &access,
                LogFilter::all().categories(CategorySet::only(Category::Background)),
                None,
                clock(2),
                &mut entries,
            )
            .unwrap();
        let stored = service.config.as_ref().unwrap().domains[0].partitions[0]
            .get(1)
            .unwrap();
        assert_eq!(stored.acceptance_order, u128::from(u64::MAX) + 1);
        let projected = entries[0].unwrap();
        let mut different_private_order = projected;
        different_private_order.order.acceptance_order += 1;
        different_private_order.sequence += 1;
        different_private_order.run_id = Some(100);
        assert_eq!(projected, different_private_order);
    }

    #[test]
    fn service_identifier_exhaustion_cannot_enable_collection() {
        let (mut service, _, _) = configured();
        let config = service.config.take().unwrap();
        service.service_id = None;
        assert_eq!(
            service.enable(config),
            Err(DiagnosticsError::IdentifierExhausted)
        );
        assert!(service.config.is_none());
    }
}
