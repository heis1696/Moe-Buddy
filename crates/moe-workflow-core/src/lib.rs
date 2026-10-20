#![doc = include_str!("../README.md")]

use std::fmt;
use std::sync::Arc;

pub const MAX_WORKFLOW_STEPS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowError {
    InvalidDefinition,
    DuplicateStep,
    MissingPrerequisite,
    DuplicatePrerequisite,
    CyclicDependency,
    CapacityExceeded,
    ForeignRun,
    StepNotReady,
    ExecutionInFlight,
    StaleExecution,
    RunStopped,
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidDefinition => "invalid workflow definition or capacity",
            Self::DuplicateStep => "workflow contains duplicate steps",
            Self::MissingPrerequisite => "workflow prerequisite does not exist",
            Self::DuplicatePrerequisite => "workflow contains duplicate prerequisites",
            Self::CyclicDependency => "workflow contains cyclic dependencies",
            Self::CapacityExceeded => "workflow run capacity exhausted",
            Self::ForeignRun => "workflow handle belongs to another runtime",
            Self::StepNotReady => "workflow step is not next in execution order",
            Self::ExecutionInFlight => "workflow already has an execution in flight",
            Self::StaleExecution => "workflow execution is no longer current",
            Self::RunStopped => "workflow is already terminal",
        })
    }
}

impl std::error::Error for WorkflowError {}

#[derive(Clone, Debug)]
pub struct WorkflowStep {
    id: &'static str,
    prerequisites: &'static [&'static str],
}

impl WorkflowStep {
    pub fn new(id: &'static str, prerequisites: &'static [&'static str]) -> Self {
        Self { id, prerequisites }
    }

    pub fn id(&self) -> &'static str {
        self.id
    }

    pub fn prerequisites(&self) -> &[&'static str] {
        self.prerequisites
    }
}

#[derive(Debug)]
struct Definition {
    id: &'static str,
    version: u64,
    steps: Vec<WorkflowStep>,
    prerequisites: Vec<Vec<usize>>,
}

#[derive(Clone, Debug)]
pub struct WorkflowDefinition(Arc<Definition>);

impl WorkflowDefinition {
    pub fn new(
        id: &'static str,
        version: u64,
        steps: Vec<WorkflowStep>,
    ) -> Result<Self, WorkflowError> {
        let valid_id = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        };
        if !valid_id(id)
            || version == 0
            || steps.is_empty()
            || steps.len() > MAX_WORKFLOW_STEPS
            || steps.iter().any(|step| !valid_id(step.id))
        {
            return Err(WorkflowError::InvalidDefinition);
        }
        for (index, step) in steps.iter().enumerate() {
            if steps[..index].iter().any(|other| other.id == step.id) {
                return Err(WorkflowError::DuplicateStep);
            }
        }
        let mut prerequisites = Vec::with_capacity(steps.len());
        for step in &steps {
            let mut indices = Vec::new();
            for dependency in step.prerequisites {
                let index = steps
                    .iter()
                    .position(|candidate| candidate.id == *dependency)
                    .ok_or(WorkflowError::MissingPrerequisite)?;
                if indices.contains(&index) {
                    return Err(WorkflowError::DuplicatePrerequisite);
                }
                indices.push(index);
            }
            prerequisites.push(indices);
        }
        let mut visited = vec![false; steps.len()];
        for _ in &steps {
            let index = prerequisites
                .iter()
                .enumerate()
                .position(|(index, dependencies)| {
                    !visited[index] && dependencies.iter().all(|dependency| visited[*dependency])
                })
                .ok_or(WorkflowError::CyclicDependency)?;
            visited[index] = true;
        }
        Ok(Self(Arc::new(Definition {
            id,
            version,
            steps,
            prerequisites,
        })))
    }

    pub fn id(&self) -> &'static str {
        self.0.id
    }

    pub fn version(&self) -> u64 {
        self.0.version
    }

    pub fn steps(&self) -> &[WorkflowStep] {
        &self.0.steps
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowStatus {
    Ready,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowStepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone)]
pub struct WorkflowRun {
    owner: Arc<()>,
    index: usize,
}

impl fmt::Debug for WorkflowRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorkflowRun(REDACTED)")
    }
}

#[derive(Clone, Debug)]
pub struct WorkflowExecution {
    run: WorkflowRun,
    owner: Arc<()>,
    step: usize,
    step_id: &'static str,
}

impl WorkflowExecution {
    pub fn step_id(&self) -> &'static str {
        self.step_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowStepProgress {
    pub id: &'static str,
    pub status: WorkflowStepStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowProgress {
    pub definition_id: &'static str,
    pub definition_version: u64,
    pub status: WorkflowStatus,
    pub next_step: Option<&'static str>,
    pub steps: Vec<WorkflowStepProgress>,
}

#[derive(Debug)]
struct Run {
    definition: WorkflowDefinition,
    status: WorkflowStatus,
    steps: Vec<WorkflowStepStatus>,
    active: Option<WorkflowExecution>,
}

impl Run {
    fn next_step(&self) -> Option<usize> {
        if self.status != WorkflowStatus::Ready {
            return None;
        }
        self.steps.iter().enumerate().position(|(index, state)| {
            *state == WorkflowStepStatus::Pending
                && self.definition.0.prerequisites[index]
                    .iter()
                    .all(|dependency| self.steps[*dependency] == WorkflowStepStatus::Succeeded)
        })
    }

    fn cancel(&mut self) {
        self.status = WorkflowStatus::Cancelled;
        self.active = None;
        for state in &mut self.steps {
            if matches!(
                state,
                WorkflowStepStatus::Pending | WorkflowStepStatus::Running
            ) {
                *state = WorkflowStepStatus::Cancelled;
            }
        }
    }
}

#[derive(Debug)]
pub struct WorkflowRuntime {
    owner: Arc<()>,
    capacity: usize,
    runs: Vec<Run>,
}

impl WorkflowRuntime {
    pub fn new(capacity: usize) -> Result<Self, WorkflowError> {
        if capacity == 0 {
            return Err(WorkflowError::InvalidDefinition);
        }
        Ok(Self {
            owner: Arc::new(()),
            capacity,
            runs: Vec::new(),
        })
    }

    pub fn start(&mut self, definition: &WorkflowDefinition) -> Result<WorkflowRun, WorkflowError> {
        if self.runs.len() >= self.capacity {
            return Err(WorkflowError::CapacityExceeded);
        }
        let handle = WorkflowRun {
            owner: self.owner.clone(),
            index: self.runs.len(),
        };
        self.runs.push(Run {
            definition: definition.clone(),
            status: WorkflowStatus::Ready,
            steps: vec![WorkflowStepStatus::Pending; definition.steps().len()],
            active: None,
        });
        Ok(handle)
    }

    fn index(&self, run: &WorkflowRun) -> Result<usize, WorkflowError> {
        if !Arc::ptr_eq(&self.owner, &run.owner) || run.index >= self.runs.len() {
            return Err(WorkflowError::ForeignRun);
        }
        Ok(run.index)
    }

    pub fn begin(
        &mut self,
        handle: &WorkflowRun,
        step_id: &str,
    ) -> Result<WorkflowExecution, WorkflowError> {
        let index = self.index(handle)?;
        let run = &mut self.runs[index];
        if run.status == WorkflowStatus::Running {
            return Err(WorkflowError::ExecutionInFlight);
        }
        if run.status != WorkflowStatus::Ready {
            return Err(WorkflowError::RunStopped);
        }
        let step = run.next_step().ok_or(WorkflowError::StepNotReady)?;
        if run.definition.0.steps[step].id != step_id {
            return Err(WorkflowError::StepNotReady);
        }
        let execution = WorkflowExecution {
            run: handle.clone(),
            owner: Arc::new(()),
            step,
            step_id: run.definition.0.steps[step].id,
        };
        run.steps[step] = WorkflowStepStatus::Running;
        run.status = WorkflowStatus::Running;
        run.active = Some(execution.clone());
        Ok(execution)
    }

    pub fn complete(
        &mut self,
        execution: &WorkflowExecution,
        outcome: WorkflowOutcome,
    ) -> Result<(), WorkflowError> {
        let index = self.index(&execution.run)?;
        let run = &mut self.runs[index];
        if !run.active.as_ref().is_some_and(|active| {
            active.step == execution.step && Arc::ptr_eq(&active.owner, &execution.owner)
        }) {
            return Err(WorkflowError::StaleExecution);
        }
        run.active = None;
        let (step_status, status) = match outcome {
            WorkflowOutcome::Succeeded => (WorkflowStepStatus::Succeeded, WorkflowStatus::Ready),
            WorkflowOutcome::Failed => (WorkflowStepStatus::Failed, WorkflowStatus::Failed),
            WorkflowOutcome::Cancelled => {
                (WorkflowStepStatus::Cancelled, WorkflowStatus::Cancelled)
            }
            WorkflowOutcome::Unknown => (WorkflowStepStatus::Unknown, WorkflowStatus::Unknown),
        };
        run.steps[execution.step] = step_status;
        run.status = status;
        if status == WorkflowStatus::Cancelled {
            run.cancel();
        } else if run
            .steps
            .iter()
            .all(|state| *state == WorkflowStepStatus::Succeeded)
        {
            run.status = WorkflowStatus::Succeeded;
        }
        Ok(())
    }

    pub fn cancel(&mut self, handle: &WorkflowRun) -> Result<(), WorkflowError> {
        let index = self.index(handle)?;
        let run = &mut self.runs[index];
        match run.status {
            WorkflowStatus::Ready | WorkflowStatus::Running => run.cancel(),
            WorkflowStatus::Cancelled => (),
            _ => return Err(WorkflowError::RunStopped),
        }
        Ok(())
    }

    pub fn progress(&self, handle: &WorkflowRun) -> Result<WorkflowProgress, WorkflowError> {
        let run = &self.runs[self.index(handle)?];
        Ok(WorkflowProgress {
            definition_id: run.definition.id(),
            definition_version: run.definition.version(),
            status: run.status,
            next_step: run
                .next_step()
                .map(|index| run.definition.0.steps[index].id),
            steps: run
                .definition
                .steps()
                .iter()
                .zip(&run.steps)
                .map(|(step, status)| WorkflowStepProgress {
                    id: step.id,
                    status: *status,
                })
                .collect(),
        })
    }
}
