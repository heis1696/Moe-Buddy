use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use crate::{
    Category, Epistemic, IdentityView, Lifecycle, MemoryError, MemoryService, MemoryTask,
    RecordVersion, RetrievalItem, Sensitivity, SessionId, SourceRef, validation,
};

#[derive(Clone, PartialEq, Eq)]
pub enum ContextOrigin {
    LongTerm(RecordVersion),
    WorkingContext {
        session: SessionId,
        id: String,
        revision: u64,
    },
}

impl fmt::Debug for ContextOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LongTerm(version) => formatter.debug_tuple("LongTerm").field(version).finish(),
            Self::WorkingContext { revision, .. } => formatter
                .debug_struct("WorkingContext")
                .field("revision", revision)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextEvidence {
    origin: ContextOrigin,
    sources: Vec<SourceRef>,
    category: Category,
    epistemic: Epistemic,
    lifecycle: Lifecycle,
    sensitivity: Sensitivity,
}

impl ContextEvidence {
    pub fn origin(&self) -> &ContextOrigin {
        &self.origin
    }
    pub fn sources(&self) -> &[SourceRef] {
        &self.sources
    }
    pub fn category(&self) -> Category {
        self.category
    }
    pub fn epistemic(&self) -> Epistemic {
        self.epistemic
    }
    pub fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }
    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextBlockKind {
    Entry,
    ProtectedMaterial,
    UnverifiedSummary,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ContextBlock {
    text: String,
    kind: ContextBlockKind,
    evidence: Vec<ContextEvidence>,
}

impl fmt::Debug for ContextBlock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextBlock")
            .field("kind", &self.kind)
            .field("evidence", &self.evidence)
            .finish_non_exhaustive()
    }
}

impl ContextBlock {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn kind(&self) -> ContextBlockKind {
        self.kind
    }
    pub fn evidence(&self) -> &[ContextEvidence] {
        &self.evidence
    }

    fn from_item(item: &RetrievalItem, session: &SessionId) -> Self {
        let origin = match item {
            RetrievalItem::LongTerm(record) => ContextOrigin::LongTerm(record.version_ref()),
            RetrievalItem::WorkingContext(context) => ContextOrigin::WorkingContext {
                session: session.clone(),
                id: context.id().to_owned(),
                revision: context.revision(),
            },
        };
        let kind = match item.category() {
            Category::Identity | Category::ToolDefinition | Category::Protocol => {
                ContextBlockKind::ProtectedMaterial
            }
            _ => ContextBlockKind::Entry,
        };
        Self {
            text: item.text().to_owned(),
            kind,
            evidence: vec![ContextEvidence {
                origin,
                sources: item.sources().to_vec(),
                category: item.category(),
                epistemic: item.epistemic(),
                lifecycle: item.lifecycle(),
                sensitivity: item.sensitivity(),
            }],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextUsage {
    pub budget_chars: usize,
    pub used_chars: usize,
    pub retrieved_items: usize,
    pub omitted_items: usize,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextView {
    identity: Option<IdentityView>,
    blocks: Vec<ContextBlock>,
    usage: ContextUsage,
}

impl ContextView {
    pub fn identity(&self) -> Option<&IdentityView> {
        self.identity.as_ref()
    }
    pub fn blocks(&self) -> &[ContextBlock] {
        &self.blocks
    }
    pub fn usage(&self) -> ContextUsage {
        self.usage
    }
}

pub struct RequestContext {
    owner: Arc<()>,
    task: MemoryTask,
    view: ContextView,
    updated_at_ms: u64,
}

impl fmt::Debug for RequestContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestContext")
            .field("usage", &self.view.usage)
            .finish_non_exhaustive()
    }
}

pub struct CompactionTask {
    owner: Arc<()>,
    revision: u64,
    range: Range<usize>,
    inputs: Vec<ContextBlock>,
    issued_at_ms: u64,
}

impl fmt::Debug for CompactionTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompactionTask")
            .field("revision", &self.revision)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

impl CompactionTask {
    pub fn inputs(&self) -> &[ContextBlock] {
        &self.inputs
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl MemoryService {
    pub fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget_chars: usize,
        now_ms: u64,
    ) -> Result<RequestContext, MemoryError> {
        self.validate_task(task, now_ms)?;
        if budget_chars > (validation::MAX_RESULTS + 1) * validation::MAX_TEXT_CHARS {
            return Err(MemoryError::InvalidInput("context budget is too large"));
        }
        let identity = task.identity_view().cloned();
        let identity_chars = identity
            .as_ref()
            .map_or(0, |view| view.text().chars().count());
        let candidates: Vec<_> = task
            .result()
            .items()
            .iter()
            .map(|item| ContextBlock::from_item(item, &task.stamp.session))
            .collect();
        let required = identity_chars
            + candidates
                .iter()
                .filter(|block| block.kind == ContextBlockKind::ProtectedMaterial)
                .map(|block| block.text.chars().count())
                .sum::<usize>();
        if required > budget_chars {
            return Err(MemoryError::ContextBudgetExceeded {
                required,
                available: budget_chars,
            });
        }
        let mut used_chars = required;
        let mut blocks = Vec::new();
        for block in candidates {
            if block.kind != ContextBlockKind::ProtectedMaterial {
                let cost = block.text.chars().count();
                if cost > budget_chars - used_chars {
                    continue;
                }
                used_chars += cost;
            }
            blocks.push(block);
        }
        let retrieved_items = task.result().items().len();
        let omitted_items = retrieved_items - blocks.len();
        Ok(RequestContext {
            owner: Arc::new(()),
            task: task.clone(),
            updated_at_ms: now_ms,
            view: ContextView {
                identity,
                blocks,
                usage: ContextUsage {
                    budget_chars,
                    used_chars,
                    retrieved_items,
                    omitted_items,
                    revision: 1,
                },
            },
        })
    }

    pub fn context_view<'context>(
        &mut self,
        context: &'context RequestContext,
        now_ms: u64,
    ) -> Result<&'context ContextView, MemoryError> {
        self.validate_task(&context.task, now_ms)?;
        if now_ms < context.updated_at_ms {
            return Err(MemoryError::TaskCancelled);
        }
        Ok(&context.view)
    }

    pub fn begin_compaction(
        &mut self,
        context: &RequestContext,
        range: Range<usize>,
        now_ms: u64,
    ) -> Result<CompactionTask, MemoryError> {
        self.context_view(context, now_ms)?;
        let inputs = context
            .view
            .blocks
            .get(range.clone())
            .filter(|blocks| !blocks.is_empty())
            .ok_or(MemoryError::InvalidInput("invalid compaction range"))?;
        if inputs
            .iter()
            .any(|block| block.kind == ContextBlockKind::ProtectedMaterial)
        {
            return Err(MemoryError::ProtectedContent);
        }
        if inputs
            .iter()
            .any(|block| block.kind == ContextBlockKind::UnverifiedSummary)
        {
            return Err(MemoryError::InvalidState);
        }
        Ok(CompactionTask {
            owner: context.owner.clone(),
            revision: context.view.usage.revision,
            range,
            inputs: inputs.to_vec(),
            issued_at_ms: now_ms,
        })
    }

    pub fn apply_compaction(
        &mut self,
        context: &mut RequestContext,
        task: &CompactionTask,
        summary: impl Into<String>,
        now_ms: u64,
    ) -> Result<(), MemoryError> {
        self.context_view(context, now_ms)?;
        if !Arc::ptr_eq(&context.owner, &task.owner) || now_ms < task.issued_at_ms {
            return Err(MemoryError::TaskCancelled);
        }
        if context.view.usage.revision != task.revision {
            return Err(MemoryError::ContextVersionConflict);
        }
        let summary = summary.into();
        validation::text(&summary)?;
        let input_chars: usize = task
            .inputs
            .iter()
            .map(|block| block.text.chars().count())
            .sum();
        let summary_chars = summary.chars().count();
        if summary_chars >= input_chars {
            return Err(MemoryError::InvalidInput(
                "summary must reduce context size",
            ));
        }
        let revision = task
            .revision
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)?;
        let evidence = task
            .inputs
            .iter()
            .flat_map(|block| block.evidence.clone())
            .collect();
        context.view.blocks.splice(
            task.range.clone(),
            [ContextBlock {
                text: summary,
                kind: ContextBlockKind::UnverifiedSummary,
                evidence,
            }],
        );
        context.view.usage.used_chars -= input_chars - summary_chars;
        context.view.usage.revision = revision;
        context.updated_at_ms = now_ms;
        Ok(())
    }
}
