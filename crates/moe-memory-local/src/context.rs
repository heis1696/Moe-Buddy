use std::ops::Range;

use moe_memory_core::{CompactionTask, ContextView, MemoryTask, RequestContext};

use crate::{LocalMemory, PersistenceError};

impl LocalMemory {
    pub fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget_chars: usize,
        now_ms: u64,
    ) -> Result<RequestContext, PersistenceError> {
        self.ready()?;
        Ok(self.service.assemble_context(task, budget_chars, now_ms)?)
    }

    pub fn context_view<'context>(
        &mut self,
        context: &'context RequestContext,
        now_ms: u64,
    ) -> Result<&'context ContextView, PersistenceError> {
        self.ready()?;
        Ok(self.service.context_view(context, now_ms)?)
    }

    pub fn begin_compaction(
        &mut self,
        context: &RequestContext,
        range: Range<usize>,
        now_ms: u64,
    ) -> Result<CompactionTask, PersistenceError> {
        self.ready()?;
        Ok(self.service.begin_compaction(context, range, now_ms)?)
    }

    pub fn apply_compaction(
        &mut self,
        context: &mut RequestContext,
        task: &CompactionTask,
        summary: impl Into<String>,
        now_ms: u64,
    ) -> Result<(), PersistenceError> {
        self.ready()?;
        Ok(self
            .service
            .apply_compaction(context, task, summary, now_ms)?)
    }
}
