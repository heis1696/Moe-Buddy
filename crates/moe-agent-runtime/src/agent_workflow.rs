use std::fmt;

use moe_memory_core::{MemoryContextAccess, MemoryError, RetrievalQuery};

use crate::{
    AgentAccess, AgentBinding, AgentDelivery, AgentError, AgentResult, AgentRuntime, AgentServices,
    AgentTask, InteractionId, OfflineModel, SessionRuntime, TerminalState, WorkflowDefinition,
    WorkflowError, WorkflowExecution, WorkflowOutcome, WorkflowProgress, WorkflowRun,
    WorkflowRuntime, WorkflowStatus, WorkflowStep,
};

#[derive(Debug)]
pub enum AgentWorkflowError<E = MemoryError> {
    Workflow(WorkflowError),
    Agent(AgentError<E>),
}

impl<E: fmt::Display> fmt::Display for AgentWorkflowError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workflow(error) => error.fmt(formatter),
            Self::Agent(error) => error.fmt(formatter),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for AgentWorkflowError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Workflow(error) => Some(error),
            Self::Agent(error) => Some(error),
        }
    }
}

impl<E> From<WorkflowError> for AgentWorkflowError<E> {
    fn from(error: WorkflowError) -> Self {
        Self::Workflow(error)
    }
}

pub struct AgentWorkflow {
    runtime: WorkflowRuntime,
    run: WorkflowRun,
    interaction: Option<InteractionId>,
    task: Option<AgentTask>,
    result: Option<AgentResult>,
}

impl fmt::Debug for AgentWorkflow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AgentWorkflow(REDACTED)")
    }
}

impl AgentWorkflow {
    pub fn start() -> Result<Self, WorkflowError> {
        let definition = WorkflowDefinition::new(
            "offline-agent",
            1,
            vec![
                WorkflowStep::new("prepare", &[]),
                WorkflowStep::new("infer", &["prepare"]),
                WorkflowStep::new("finish", &["infer"]),
            ],
        )?;
        let mut runtime = WorkflowRuntime::new(1)?;
        let run = runtime.start(&definition)?;
        Ok(Self {
            runtime,
            run,
            interaction: None,
            task: None,
            result: None,
        })
    }

    pub fn progress(&self) -> WorkflowProgress {
        self.runtime
            .progress(&self.run)
            .expect("owned workflow run")
    }

    fn failed<E, Output>(
        &mut self,
        execution: &WorkflowExecution,
        error: AgentError<E>,
    ) -> Result<Output, AgentWorkflowError<E>> {
        self.runtime.complete(execution, WorkflowOutcome::Failed)?;
        self.task = None;
        self.result = None;
        Err(AgentWorkflowError::Agent(error))
    }

    pub fn prepare<M: MemoryContextAccess + ?Sized>(
        &mut self,
        agent: &mut AgentRuntime,
        access: &AgentAccess<'_>,
        services: AgentServices<'_, M>,
        binding: &AgentBinding,
        interaction: &InteractionId,
        query: &RetrievalQuery,
    ) -> Result<(), AgentWorkflowError<M::Error>> {
        let execution = self.runtime.begin(&self.run, "prepare")?;
        match agent.prepare(access, services, binding, interaction, query) {
            Ok(task) => {
                self.task = Some(task);
                self.interaction = Some(interaction.clone());
                self.runtime
                    .complete(&execution, WorkflowOutcome::Succeeded)?;
                Ok(())
            }
            Err(error) => self.failed(&execution, error),
        }
    }

    pub fn infer<M: MemoryContextAccess + ?Sized>(
        &mut self,
        agent: &AgentRuntime,
        access: &AgentAccess<'_>,
        services: AgentServices<'_, M>,
        model: &mut dyn OfflineModel,
    ) -> Result<(), AgentWorkflowError<M::Error>> {
        let execution = self.runtime.begin(&self.run, "infer")?;
        match agent.infer(
            access,
            services,
            self.task.as_mut().expect("prepared workflow task"),
            model,
        ) {
            Ok(result) => {
                self.result = Some(result);
                self.runtime
                    .complete(&execution, WorkflowOutcome::Succeeded)?;
                Ok(())
            }
            Err(error) => self.failed(&execution, error),
        }
    }

    pub fn finish<M: MemoryContextAccess + ?Sized>(
        &mut self,
        agent: &AgentRuntime,
        access: &AgentAccess<'_>,
        services: AgentServices<'_, M>,
    ) -> Result<AgentDelivery, AgentWorkflowError<M::Error>> {
        let execution = self.runtime.begin(&self.run, "finish")?;
        match agent.finish(
            access,
            services,
            self.task.as_mut().expect("prepared workflow task"),
            self.result.as_ref().expect("inferred workflow result"),
        ) {
            Ok(delivery) => {
                let outcome = match delivery.state() {
                    TerminalState::Completed => WorkflowOutcome::Succeeded,
                    TerminalState::Failed => WorkflowOutcome::Failed,
                    TerminalState::Cancelled => WorkflowOutcome::Cancelled,
                    TerminalState::Unknown => WorkflowOutcome::Unknown,
                };
                self.runtime.complete(&execution, outcome)?;
                self.task = None;
                self.result = None;
                Ok(delivery)
            }
            Err(error) => self.failed(&execution, error),
        }
    }

    pub fn cancel(
        &mut self,
        access: &AgentAccess<'_>,
        sessions: &mut SessionRuntime,
    ) -> Result<(), AgentWorkflowError> {
        match self.progress().status {
            WorkflowStatus::Cancelled => return Ok(()),
            WorkflowStatus::Ready | WorkflowStatus::Running => (),
            _ => return Err(WorkflowError::RunStopped.into()),
        }
        if let Some(interaction) = &self.interaction {
            sessions
                .cancel_interaction(access.session(), interaction)
                .map_err(|error| AgentWorkflowError::Agent(AgentError::Gateway(error)))?;
        }
        self.runtime.cancel(&self.run)?;
        self.task = None;
        self.result = None;
        Ok(())
    }
}
