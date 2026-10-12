use std::ops::Range;

use moe_memory_core::{
    CompactionTask, ContextView, MemoryContextAccess, MemoryTask, PetIdentity, ReadPurpose,
    RequestContext, RetrievalQuery, SessionId,
};

use crate::{LocalMemory, PersistenceError};

impl MemoryContextAccess for LocalMemory {
    type Error = PersistenceError;

    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<MemoryTask, Self::Error> {
        self.ready()?;
        Ok(self
            .service
            .for_session(identity, session, purpose)
            .begin_task(query, now_ms)?)
    }

    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget_chars: usize,
        now_ms: u64,
    ) -> Result<RequestContext, Self::Error> {
        LocalMemory::assemble_context(self, task, budget_chars, now_ms)
    }

    fn context_view<'context>(
        &mut self,
        context: &'context RequestContext,
        now_ms: u64,
    ) -> Result<&'context ContextView, Self::Error> {
        LocalMemory::context_view(self, context, now_ms)
    }
}

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
