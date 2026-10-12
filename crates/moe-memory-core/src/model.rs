use std::fmt;

use crate::{MemoryError, validation};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Namespace {
    workspace_id: String,
    user_id: String,
}

impl Namespace {
    pub fn new(
        workspace_id: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let workspace_id = workspace_id.into();
        let user_id = user_id.into();
        validation::identifier(&workspace_id)?;
        validation::identifier(&user_id)?;
        Ok(Self {
            workspace_id,
            user_id,
        })
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PetIdentity {
    namespace: Namespace,
    instance_id: String,
}

impl PetIdentity {
    pub fn new(namespace: Namespace, instance_id: impl Into<String>) -> Result<Self, MemoryError> {
        let instance_id = instance_id.into();
        validation::identifier(&instance_id)?;
        Ok(Self {
            namespace,
            instance_id,
        })
    }

    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryScope {
    Private(PetIdentity),
    Shared(Namespace),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Identity,
    UserProfile,
    Relationship,
    Episode,
    WorkingContext,
    Knowledge,
    Procedure,
    ToolDefinition,
    Protocol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Epistemic {
    UserStatement,
    Observation,
    Inference,
    Fiction,
    Reference,
    AuthoritativeState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lifecycle {
    ShortTerm,
    #[default]
    LongTerm,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sensitivity {
    #[default]
    Normal,
    Private,
    Restricted,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SourceKey {
    pub(crate) scope: MemoryScope,
    pub(crate) id: String,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceRef {
    pub(crate) key: SourceKey,
    pub(crate) revision: u64,
}

impl fmt::Debug for SourceRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRef")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl SourceRef {
    pub fn id(&self) -> &str {
        &self.key.id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemoryId(pub(crate) u64);

impl MemoryId {
    /// Stable numeric representation for derived cache keys, not authority.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MemoryDraft {
    pub(crate) text: String,
    pub(crate) category: Category,
    pub(crate) epistemic: Epistemic,
    pub(crate) sources: Vec<SourceRef>,
    pub(crate) lifecycle: Lifecycle,
    pub(crate) sensitivity: Sensitivity,
    pub(crate) expires_at_ms: Option<u64>,
    pub(crate) dependencies: Vec<crate::RecordVersion>,
}

impl fmt::Debug for MemoryDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryDraft")
            .field("category", &self.category)
            .field("epistemic", &self.epistemic)
            .field("source_count", &self.sources.len())
            .field("lifecycle", &self.lifecycle)
            .field("sensitivity", &self.sensitivity)
            .finish_non_exhaustive()
    }
}

impl MemoryDraft {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn category(&self) -> Category {
        self.category
    }

    pub fn epistemic(&self) -> Epistemic {
        self.epistemic
    }

    pub fn sources(&self) -> &[SourceRef] {
        &self.sources
    }

    pub fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }

    pub fn expires_at_ms(&self) -> Option<u64> {
        self.expires_at_ms
    }

    pub fn dependencies(&self) -> &[crate::RecordVersion] {
        &self.dependencies
    }

    pub fn new(
        text: impl Into<String>,
        category: Category,
        epistemic: Epistemic,
        sources: Vec<SourceRef>,
    ) -> Self {
        Self {
            text: text.into(),
            category,
            epistemic,
            sources,
            lifecycle: Lifecycle::LongTerm,
            sensitivity: Sensitivity::Normal,
            expires_at_ms: None,
            dependencies: Vec::new(),
        }
    }

    pub fn with_lifecycle(mut self, lifecycle: Lifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    pub fn with_sensitivity(mut self, sensitivity: Sensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    pub fn with_expiration(mut self, expires_at_ms: u64) -> Self {
        self.expires_at_ms = Some(expires_at_ms);
        self
    }

    pub fn with_dependencies(mut self, dependencies: Vec<crate::RecordVersion>) -> Self {
        self.dependencies = dependencies;
        self
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    pub(crate) id: MemoryId,
    pub(crate) revision: u64,
    pub(crate) scope: MemoryScope,
    pub(crate) draft: MemoryDraft,
    pub(crate) created_at_ms: u64,
    pub(crate) updated_at_ms: u64,
    pub(crate) protected: bool,
    pub(crate) state: crate::RecordState,
}

impl fmt::Debug for MemoryRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryRecord")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("draft", &self.draft)
            .finish_non_exhaustive()
    }
}

impl MemoryRecord {
    pub fn id(&self) -> MemoryId {
        self.id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn scope(&self) -> &MemoryScope {
        &self.scope
    }

    pub fn text(&self) -> &str {
        &self.draft.text
    }

    pub fn category(&self) -> Category {
        self.draft.category
    }

    pub fn epistemic(&self) -> Epistemic {
        self.draft.epistemic
    }

    pub fn sources(&self) -> &[SourceRef] {
        &self.draft.sources
    }

    pub fn lifecycle(&self) -> Lifecycle {
        self.draft.lifecycle
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.draft.sensitivity
    }

    pub fn expires_at_ms(&self) -> Option<u64> {
        self.draft.expires_at_ms
    }

    pub fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }

    pub fn updated_at_ms(&self) -> u64 {
        self.updated_at_ms
    }

    pub fn is_protected(&self) -> bool {
        self.protected
    }

    pub fn version_ref(&self) -> crate::RecordVersion {
        crate::RecordVersion::new(self.id, self.revision).expect("positive record revision")
    }

    pub fn dependencies(&self) -> &[crate::RecordVersion] {
        &self.draft.dependencies
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchScope {
    Private,
    Shared,
    #[default]
    All,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub(crate) text: String,
    pub(crate) scope: SearchScope,
    pub(crate) limit: usize,
}

impl fmt::Debug for SearchQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchQuery")
            .field("scope", &self.scope)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

impl SearchQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            scope: SearchScope::All,
            limit: 20,
        }
    }

    pub fn with_scope(mut self, scope: SearchScope) -> Self {
        self.scope = scope;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOperation {
    Created,
    Updated,
    Deleted,
    Archived,
    Restored,
    Superseded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteReceipt {
    pub(crate) id: MemoryId,
    pub(crate) revision: u64,
    pub(crate) operation: WriteOperation,
}

impl WriteReceipt {
    pub fn id(&self) -> MemoryId {
        self.id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn operation(&self) -> WriteOperation {
        self.operation
    }
}
