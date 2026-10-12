use crate::{
    ContextView, MemoryError, MemoryService, MemoryTask, PetIdentity, ReadPurpose, RequestContext,
    RetrievalQuery, SessionId,
};

pub trait MemoryContextAccess {
    type Error: std::error::Error;

    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<MemoryTask, Self::Error>;

    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget_chars: usize,
        now_ms: u64,
    ) -> Result<RequestContext, Self::Error>;

    fn context_view<'context>(
        &mut self,
        context: &'context RequestContext,
        now_ms: u64,
    ) -> Result<&'context ContextView, Self::Error>;
}

impl MemoryContextAccess for MemoryService {
    type Error = MemoryError;

    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<MemoryTask, Self::Error> {
        self.for_session(identity, session, purpose)
            .begin_task(query, now_ms)
    }

    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget_chars: usize,
        now_ms: u64,
    ) -> Result<RequestContext, Self::Error> {
        MemoryService::assemble_context(self, task, budget_chars, now_ms)
    }

    fn context_view<'context>(
        &mut self,
        context: &'context RequestContext,
        now_ms: u64,
    ) -> Result<&'context ContextView, Self::Error> {
        MemoryService::context_view(self, context, now_ms)
    }
}
