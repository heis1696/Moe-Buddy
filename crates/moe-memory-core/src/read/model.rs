use std::collections::BTreeSet;
use std::fmt;

use crate::{
    Category, Epistemic, Lifecycle, MemoryDraft, MemoryError, MemoryRecord, SearchQuery,
    SearchScope, Sensitivity, SourceRef, validation,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionId(String);

impl SessionId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
    pub fn new(value: impl Into<String>) -> Result<Self, MemoryError> {
        let value = value.into();
        validation::identifier(&value)?;
        Ok(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadPurpose {
    OfflineReasoning,
    UserInspection,
    ModelTransmission,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadPolicy {
    pub(super) version: u64,
    pub(super) offline: bool,
    pub(super) model_transmission: bool,
    pub(super) sources: BTreeSet<SourceRef>,
}

impl ReadPolicy {
    pub fn deny_all(version: u64) -> Self {
        Self {
            version,
            offline: false,
            model_transmission: false,
            sources: BTreeSet::new(),
        }
    }

    pub fn offline(version: u64, sources: Vec<SourceRef>) -> Self {
        Self {
            version,
            offline: true,
            model_transmission: false,
            sources: sources.into_iter().collect(),
        }
    }

    /// Explicitly permit model input from precisely these sources as well as offline reads.
    /// The trusted caller must separately bind a destination and user/conversation consent.
    pub fn model_transmission(version: u64, sources: Vec<SourceRef>) -> Self {
        Self {
            version,
            offline: true,
            model_transmission: true,
            sources: sources.into_iter().collect(),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RetrievalLayer {
    WorkingContext,
    LongTerm,
    #[default]
    All,
    FormalMaterial,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetrievalQuery {
    pub(super) search: SearchQuery,
    pub(super) layer: RetrievalLayer,
    pub(super) char_budget: usize,
}

impl RetrievalQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            search: SearchQuery::new(text),
            layer: RetrievalLayer::All,
            char_budget: 16_384,
        }
    }

    pub fn with_scope(mut self, scope: SearchScope) -> Self {
        self.search = self.search.with_scope(scope);
        self
    }

    pub fn with_layer(mut self, layer: RetrievalLayer) -> Self {
        self.layer = layer;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.search = self.search.with_limit(limit);
        self
    }

    pub fn with_char_budget(mut self, char_budget: usize) -> Self {
        self.char_budget = char_budget;
        self
    }

    pub fn text(&self) -> &str {
        &self.search.text
    }

    pub fn scope(&self) -> SearchScope {
        self.search.scope
    }

    pub fn layer(&self) -> RetrievalLayer {
        self.layer
    }

    pub fn limit(&self) -> usize {
        self.search.limit
    }

    pub fn char_budget(&self) -> usize {
        self.char_budget
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct WorkingContext {
    pub(super) id: String,
    pub(super) revision: u64,
    pub(super) draft: MemoryDraft,
    pub(super) updated_at_ms: u64,
}

impl fmt::Debug for WorkingContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkingContext")
            .field("revision", &self.revision)
            .field("draft", &self.draft)
            .finish_non_exhaustive()
    }
}

impl WorkingContext {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetrievalItem {
    LongTerm(MemoryRecord),
    WorkingContext(WorkingContext),
}

impl RetrievalItem {
    fn draft(&self) -> &MemoryDraft {
        match self {
            Self::LongTerm(record) => &record.draft,
            Self::WorkingContext(context) => &context.draft,
        }
    }

    pub fn text(&self) -> &str {
        &self.draft().text
    }
    pub fn sources(&self) -> &[SourceRef] {
        &self.draft().sources
    }
    pub fn category(&self) -> Category {
        self.draft().category
    }
    pub fn lifecycle(&self) -> Lifecycle {
        self.draft().lifecycle
    }
    pub fn epistemic(&self) -> Epistemic {
        self.draft().epistemic
    }
    pub fn sensitivity(&self) -> Sensitivity {
        self.draft().sensitivity
    }

    pub fn updated_at_ms(&self) -> u64 {
        match self {
            Self::LongTerm(record) => record.updated_at_ms,
            Self::WorkingContext(context) => context.updated_at_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetrievalResult {
    pub(super) items: Vec<RetrievalItem>,
    pub(super) used_chars: usize,
}

impl RetrievalResult {
    pub fn items(&self) -> &[RetrievalItem] {
        &self.items
    }
    pub fn used_chars(&self) -> usize {
        self.used_chars
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct IdentityView {
    pub(super) version: u64,
    pub(super) text: String,
    pub(super) sources: Vec<SourceRef>,
}

impl fmt::Debug for IdentityView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityView")
            .field("version", &self.version)
            .field("source_count", &self.sources.len())
            .finish_non_exhaustive()
    }
}

impl IdentityView {
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn sources(&self) -> &[SourceRef] {
        &self.sources
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheStatus {
    Hit,
    Built,
    Fallback,
}

#[derive(Clone, PartialEq, Eq)]
pub struct EntityView {
    pub(super) label: String,
    pub(super) result: RetrievalResult,
}

impl fmt::Debug for EntityView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EntityView")
            .field("result", &self.result)
            .finish_non_exhaustive()
    }
}

impl EntityView {
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn items(&self) -> &[RetrievalItem] {
        self.result.items()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntityRead {
    View {
        view: EntityView,
        cache: CacheStatus,
    },
    Entries(RetrievalResult),
}

impl EntityRead {
    pub fn cache_status(&self) -> CacheStatus {
        match self {
            Self::View { cache, .. } => *cache,
            Self::Entries(_) => CacheStatus::Fallback,
        }
    }

    pub fn items(&self) -> &[RetrievalItem] {
        match self {
            Self::View { view, .. } => view.items(),
            Self::Entries(result) => result.items(),
        }
    }

    pub fn used_chars(&self) -> usize {
        match self {
            Self::View { view, .. } => view.result.used_chars(),
            Self::Entries(result) => result.used_chars(),
        }
    }
}
