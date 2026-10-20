use moe_workflow_core::{
    MAX_WORKFLOW_STEPS, WorkflowDefinition, WorkflowError, WorkflowOutcome, WorkflowRuntime,
    WorkflowStatus, WorkflowStep, WorkflowStepProgress, WorkflowStepStatus,
};

fn chain(id: &'static str, version: u64) -> WorkflowDefinition {
    WorkflowDefinition::new(
        id,
        version,
        vec![
            WorkflowStep::new("prepare", &[]),
            WorkflowStep::new("process", &["prepare"]),
            WorkflowStep::new("report", &["process"]),
        ],
    )
    .unwrap()
}

fn boundary_steps(count: usize) -> Vec<WorkflowStep> {
    (0..count)
        .map(|index| WorkflowStep::new(Box::leak(format!("s{index}").into_boxed_str()), &[]))
        .collect()
}

#[test]
fn definitions_reject_invalid_ids_versions_and_step_counts() {
    let empty = vec![WorkflowStep::new("only", &[])];
    let too_long: &'static str = Box::leak("a".repeat(129).into_boxed_str());
    let boundary: &'static str = Box::leak("a".repeat(128).into_boxed_str());
    let unicode: &'static str = "流程";
    for (id, version, steps) in [
        ("", 1, empty),
        ("space not allowed", 1, vec![WorkflowStep::new("only", &[])]),
        ("slash/rejected", 1, vec![WorkflowStep::new("only", &[])]),
        (unicode, 1, vec![WorkflowStep::new("only", &[])]),
        (too_long, 1, vec![WorkflowStep::new("only", &[])]),
        ("synthetic", 0, vec![WorkflowStep::new("only", &[])]),
        ("synthetic", 1, Vec::new()),
        ("synthetic", 1, vec![WorkflowStep::new("", &[])]),
        ("synthetic", 1, vec![WorkflowStep::new("bad id", &[])]),
    ] {
        assert!(matches!(
            WorkflowDefinition::new(id, version, steps),
            Err(WorkflowError::InvalidDefinition)
        ));
    }
    assert!(WorkflowDefinition::new(boundary, 1, vec![WorkflowStep::new("only", &[])]).is_ok());
    assert!(matches!(
        WorkflowDefinition::new("synthetic", 1, boundary_steps(MAX_WORKFLOW_STEPS + 1)),
        Err(WorkflowError::InvalidDefinition)
    ));
    let definition =
        WorkflowDefinition::new("synthetic", 1, boundary_steps(MAX_WORKFLOW_STEPS)).unwrap();
    assert_eq!(definition.steps().len(), MAX_WORKFLOW_STEPS);
}

#[test]
fn definitions_reject_duplicate_missing_and_cyclic_wiring() {
    assert!(matches!(
        WorkflowDefinition::new(
            "synthetic",
            1,
            vec![
                WorkflowStep::new("same", &[]),
                WorkflowStep::new("same", &[]),
            ],
        ),
        Err(WorkflowError::DuplicateStep)
    ));
    assert!(matches!(
        WorkflowDefinition::new(
            "synthetic",
            1,
            vec![
                WorkflowStep::new("real", &[]),
                WorkflowStep::new("later", &["ghost"]),
            ],
        ),
        Err(WorkflowError::MissingPrerequisite)
    ));
    assert!(matches!(
        WorkflowDefinition::new(
            "synthetic",
            1,
            vec![
                WorkflowStep::new("first", &[]),
                WorkflowStep::new("second", &["first", "first"]),
            ],
        ),
        Err(WorkflowError::DuplicatePrerequisite)
    ));
    assert!(matches!(
        WorkflowDefinition::new(
            "synthetic",
            1,
            vec![
                WorkflowStep::new("a", &["c"]),
                WorkflowStep::new("b", &["a"]),
                WorkflowStep::new("c", &["b"]),
            ],
        ),
        Err(WorkflowError::CyclicDependency)
    ));
    assert!(matches!(
        WorkflowDefinition::new("synthetic", 1, vec![WorkflowStep::new("self", &["self"])]),
        Err(WorkflowError::CyclicDependency)
    ));
}

#[test]
fn forward_dependencies_run_in_deterministic_ready_order() {
    let definition = WorkflowDefinition::new(
        "synthetic",
        1,
        vec![
            WorkflowStep::new("aggregate", &["alpha", "beta"]),
            WorkflowStep::new("beta", &[]),
            WorkflowStep::new("alpha", &[]),
        ],
    )
    .unwrap();
    let mut runtime = WorkflowRuntime::new(2).unwrap();
    let run = runtime.start(&definition).unwrap();
    assert_eq!(runtime.progress(&run).unwrap().next_step, Some("beta"));
    assert!(matches!(
        runtime.begin(&run, "alpha"),
        Err(WorkflowError::StepNotReady)
    ));
    let beta = runtime.begin(&run, "beta").unwrap();
    assert_eq!(beta.step_id(), "beta");
    assert!(matches!(
        runtime.begin(&run, "aggregate"),
        Err(WorkflowError::ExecutionInFlight)
    ));
    runtime.complete(&beta, WorkflowOutcome::Succeeded).unwrap();
    assert!(matches!(
        runtime.begin(&run, "aggregate"),
        Err(WorkflowError::StepNotReady)
    ));
    let alpha = runtime.begin(&run, "alpha").unwrap();
    runtime
        .complete(&alpha, WorkflowOutcome::Succeeded)
        .unwrap();
    let aggregate = runtime.begin(&run, "aggregate").unwrap();
    runtime
        .complete(&aggregate, WorkflowOutcome::Succeeded)
        .unwrap();
    let progress = runtime.progress(&run).unwrap();
    assert_eq!(progress.status, WorkflowStatus::Succeeded);
    assert_eq!(progress.next_step, None);
}

#[test]
fn unmet_prerequisites_never_allow_stepping_over_the_order() {
    let mut runtime = WorkflowRuntime::new(2).unwrap();
    let run = runtime.start(&chain("synthetic", 1)).unwrap();
    assert!(matches!(
        runtime.begin(&run, "process"),
        Err(WorkflowError::StepNotReady)
    ));
    assert!(matches!(
        runtime.begin(&run, "report"),
        Err(WorkflowError::StepNotReady)
    ));
    assert!(matches!(
        runtime.begin(&run, "unknown-step"),
        Err(WorkflowError::StepNotReady)
    ));
    let prepare = runtime.begin(&run, "prepare").unwrap();
    runtime
        .complete(&prepare, WorkflowOutcome::Succeeded)
        .unwrap();
    let progress = runtime.progress(&run).unwrap();
    assert_eq!(progress.next_step, Some("process"));
    assert_eq!(
        progress.steps,
        vec![
            WorkflowStepProgress {
                id: "prepare",
                status: WorkflowStepStatus::Succeeded,
            },
            WorkflowStepProgress {
                id: "process",
                status: WorkflowStepStatus::Pending,
            },
            WorkflowStepProgress {
                id: "report",
                status: WorkflowStepStatus::Pending,
            },
        ]
    );
    let process = runtime.begin(&run, "process").unwrap();
    runtime.complete(&process, WorkflowOutcome::Failed).unwrap();
    let progress = runtime.progress(&run).unwrap();
    assert_eq!(progress.status, WorkflowStatus::Failed);
    assert_eq!(progress.next_step, None);
    assert!(matches!(
        runtime.begin(&run, "report"),
        Err(WorkflowError::RunStopped)
    ));
}

#[test]
fn execution_tickets_reject_duplicate_late_cross_instance_and_foreign_use() {
    let definition = chain("synthetic", 1);
    let mut runtime = WorkflowRuntime::new(4).unwrap();
    let run = runtime.start(&definition).unwrap();
    let other = runtime.start(&definition).unwrap();
    let first = runtime.begin(&run, "prepare").unwrap();
    assert!(matches!(
        runtime.begin(&run, "prepare"),
        Err(WorkflowError::ExecutionInFlight)
    ));
    let parallel = runtime.begin(&other, "prepare").unwrap();
    runtime
        .complete(&parallel, WorkflowOutcome::Succeeded)
        .unwrap();
    let progress = runtime.progress(&run).unwrap();
    assert_eq!(progress.status, WorkflowStatus::Running);
    assert_eq!(progress.steps[0].status, WorkflowStepStatus::Running);
    assert_eq!(runtime.progress(&other).unwrap().next_step, Some("process"));
    runtime
        .complete(&first, WorkflowOutcome::Succeeded)
        .unwrap();
    assert!(matches!(
        runtime.complete(&first, WorkflowOutcome::Succeeded),
        Err(WorkflowError::StaleExecution)
    ));
    assert!(matches!(
        runtime.complete(&first, WorkflowOutcome::Failed),
        Err(WorkflowError::StaleExecution)
    ));
    let stale = runtime.begin(&run, "process").unwrap();
    let before = runtime.progress(&run).unwrap();
    assert!(matches!(
        runtime.complete(&first, WorkflowOutcome::Failed),
        Err(WorkflowError::StaleExecution)
    ));
    assert_eq!(runtime.progress(&run).unwrap(), before);
    runtime.cancel(&run).unwrap();
    assert!(matches!(
        runtime.complete(&stale, WorkflowOutcome::Succeeded),
        Err(WorkflowError::StaleExecution)
    ));
    let mut foreign = WorkflowRuntime::new(4).unwrap();
    assert!(matches!(
        foreign.begin(&run, "prepare"),
        Err(WorkflowError::ForeignRun)
    ));
    assert!(matches!(
        foreign.progress(&run),
        Err(WorkflowError::ForeignRun)
    ));
    assert!(matches!(
        foreign.cancel(&run),
        Err(WorkflowError::ForeignRun)
    ));
    let foreign_run = foreign.start(&definition).unwrap();
    let foreign_execution = foreign.begin(&foreign_run, "prepare").unwrap();
    assert!(matches!(
        runtime.complete(&foreign_execution, WorkflowOutcome::Succeeded),
        Err(WorkflowError::ForeignRun)
    ));
}

#[test]
fn failure_unknown_and_cancellation_stop_the_run() {
    for outcome in [
        WorkflowOutcome::Failed,
        WorkflowOutcome::Cancelled,
        WorkflowOutcome::Unknown,
    ] {
        let mut runtime = WorkflowRuntime::new(2).unwrap();
        let run = runtime.start(&chain("synthetic", 1)).unwrap();
        let execution = runtime.begin(&run, "prepare").unwrap();
        runtime.complete(&execution, outcome).unwrap();
        let progress = runtime.progress(&run).unwrap();
        assert_eq!(
            progress.status,
            match outcome {
                WorkflowOutcome::Failed => WorkflowStatus::Failed,
                WorkflowOutcome::Cancelled => WorkflowStatus::Cancelled,
                WorkflowOutcome::Unknown => WorkflowStatus::Unknown,
                WorkflowOutcome::Succeeded => WorkflowStatus::Ready,
            }
        );
        assert_eq!(progress.next_step, None);
        assert_eq!(
            progress.steps[0].status,
            match outcome {
                WorkflowOutcome::Failed => WorkflowStepStatus::Failed,
                WorkflowOutcome::Cancelled => WorkflowStepStatus::Cancelled,
                WorkflowOutcome::Unknown => WorkflowStepStatus::Unknown,
                WorkflowOutcome::Succeeded => WorkflowStepStatus::Succeeded,
            }
        );
        if outcome == WorkflowOutcome::Cancelled {
            assert!(
                progress
                    .steps
                    .iter()
                    .all(|step| step.status == WorkflowStepStatus::Cancelled)
            );
            runtime.cancel(&run).unwrap();
        } else {
            assert!(matches!(
                runtime.begin(&run, "process"),
                Err(WorkflowError::RunStopped)
            ));
            assert!(matches!(
                runtime.cancel(&run),
                Err(WorkflowError::RunStopped)
            ));
        }
    }
}

#[test]
fn runtime_cancel_is_idempotent_and_preserves_completed_steps() {
    let mut runtime = WorkflowRuntime::new(4).unwrap();
    let run = runtime.start(&chain("synthetic", 1)).unwrap();
    runtime.cancel(&run).unwrap();
    assert_eq!(
        runtime.progress(&run).unwrap().status,
        WorkflowStatus::Cancelled
    );
    runtime.cancel(&run).unwrap();
    assert!(matches!(
        runtime.begin(&run, "prepare"),
        Err(WorkflowError::RunStopped)
    ));

    let run = runtime.start(&chain("synthetic", 1)).unwrap();
    let prepare = runtime.begin(&run, "prepare").unwrap();
    runtime
        .complete(&prepare, WorkflowOutcome::Succeeded)
        .unwrap();
    let process = runtime.begin(&run, "process").unwrap();
    runtime.cancel(&run).unwrap();
    assert!(matches!(
        runtime.complete(&process, WorkflowOutcome::Succeeded),
        Err(WorkflowError::StaleExecution)
    ));
    assert_eq!(
        runtime.progress(&run).unwrap().steps,
        vec![
            WorkflowStepProgress {
                id: "prepare",
                status: WorkflowStepStatus::Succeeded,
            },
            WorkflowStepProgress {
                id: "process",
                status: WorkflowStepStatus::Cancelled,
            },
            WorkflowStepProgress {
                id: "report",
                status: WorkflowStepStatus::Cancelled,
            },
        ]
    );

    let run = runtime.start(&chain("synthetic", 1)).unwrap();
    for step in ["prepare", "process", "report"] {
        let execution = runtime.begin(&run, step).unwrap();
        runtime
            .complete(&execution, WorkflowOutcome::Succeeded)
            .unwrap();
    }
    assert_eq!(
        runtime.progress(&run).unwrap().status,
        WorkflowStatus::Succeeded
    );
    assert!(matches!(
        runtime.cancel(&run),
        Err(WorkflowError::RunStopped)
    ));
}

#[test]
fn instances_advance_independently() {
    let definition = chain("synthetic", 1);
    let mut runtime = WorkflowRuntime::new(4).unwrap();
    let stalled = runtime.start(&definition).unwrap();
    let healthy = runtime.start(&definition).unwrap();
    let first = runtime.begin(&stalled, "prepare").unwrap();
    let second = runtime.begin(&healthy, "prepare").unwrap();
    runtime.complete(&first, WorkflowOutcome::Failed).unwrap();
    assert_eq!(
        runtime.progress(&healthy).unwrap().status,
        WorkflowStatus::Running
    );
    runtime
        .complete(&second, WorkflowOutcome::Succeeded)
        .unwrap();
    for step in ["process", "report"] {
        let execution = runtime.begin(&healthy, step).unwrap();
        runtime
            .complete(&execution, WorkflowOutcome::Succeeded)
            .unwrap();
    }
    assert_eq!(
        runtime.progress(&healthy).unwrap().status,
        WorkflowStatus::Succeeded
    );
    assert_eq!(
        runtime.progress(&stalled).unwrap().status,
        WorkflowStatus::Failed
    );
}

#[test]
fn unrelated_pipeline_can_progress_while_another_instance_waits() {
    let document_pipeline = WorkflowDefinition::new(
        "document-pipeline",
        1,
        vec![
            WorkflowStep::new("extract", &[]),
            WorkflowStep::new("index", &["extract"]),
            WorkflowStep::new("publish", &["index"]),
        ],
    )
    .unwrap();
    let mut runtime = WorkflowRuntime::new(2).unwrap();
    let waiting = runtime.start(&chain("offline-agent", 1)).unwrap();
    let pipeline = runtime.start(&document_pipeline).unwrap();
    let waiting_ticket = runtime.begin(&waiting, "prepare").unwrap();
    assert_eq!(
        runtime.progress(&waiting).unwrap().status,
        WorkflowStatus::Running
    );

    for step in ["extract", "index", "publish"] {
        let ticket = runtime.begin(&pipeline, step).unwrap();
        runtime
            .complete(&ticket, WorkflowOutcome::Succeeded)
            .unwrap();
    }

    assert_eq!(
        runtime.progress(&pipeline).unwrap().status,
        WorkflowStatus::Succeeded
    );
    assert_eq!(
        runtime.progress(&waiting).unwrap().status,
        WorkflowStatus::Running
    );
    runtime
        .complete(&waiting_ticket, WorkflowOutcome::Succeeded)
        .unwrap();
    assert_eq!(
        runtime.progress(&waiting).unwrap().next_step,
        Some("process")
    );
}

#[test]
fn started_instances_keep_their_definition_version() {
    let mut runtime = WorkflowRuntime::new(4).unwrap();
    let first = chain("synthetic", 7);
    let run = runtime.start(&first).unwrap();
    let in_flight = runtime.begin(&run, "prepare").unwrap();
    let second =
        WorkflowDefinition::new("synthetic", 8, vec![WorkflowStep::new("only", &[])]).unwrap();
    let next = runtime.start(&second).unwrap();
    let progress = runtime.progress(&next).unwrap();
    assert_eq!(progress.definition_version, 8);
    assert_eq!(progress.next_step, Some("only"));
    let progress = runtime.progress(&run).unwrap();
    assert_eq!(progress.definition_id, "synthetic");
    assert_eq!(progress.definition_version, 7);
    assert_eq!(progress.steps.len(), 3);
    assert_eq!(progress.status, WorkflowStatus::Running);
    assert_eq!(progress.next_step, None);
    runtime
        .complete(&in_flight, WorkflowOutcome::Succeeded)
        .unwrap();
    assert_eq!(runtime.progress(&run).unwrap().next_step, Some("process"));
    assert_eq!(first.id(), "synthetic");
    assert_eq!(first.version(), 7);
    assert_eq!(first.steps().len(), 3);
}

#[test]
fn capacity_is_cumulative_and_progress_snapshots_are_detached() {
    assert!(matches!(
        WorkflowRuntime::new(0),
        Err(WorkflowError::InvalidDefinition)
    ));
    let definition = chain("synthetic", 1);
    let mut runtime = WorkflowRuntime::new(2).unwrap();
    let first = runtime.start(&definition).unwrap();
    let second = runtime.start(&definition).unwrap();
    assert!(matches!(
        runtime.start(&definition),
        Err(WorkflowError::CapacityExceeded)
    ));
    for step in ["prepare", "process", "report"] {
        let execution = runtime.begin(&first, step).unwrap();
        runtime
            .complete(&execution, WorkflowOutcome::Succeeded)
            .unwrap();
    }
    assert!(matches!(
        runtime.start(&definition),
        Err(WorkflowError::CapacityExceeded)
    ));
    assert_eq!(format!("{first:?}"), "WorkflowRun(REDACTED)");

    let in_flight = runtime.begin(&second, "prepare").unwrap();
    let mut snapshot = runtime.progress(&second).unwrap();
    snapshot.status = WorkflowStatus::Succeeded;
    snapshot.next_step = Some("report");
    snapshot.steps[0].status = WorkflowStepStatus::Succeeded;
    snapshot.steps.push(WorkflowStepProgress {
        id: "forged",
        status: WorkflowStepStatus::Succeeded,
    });
    let fresh = runtime.progress(&second).unwrap();
    assert_eq!(fresh.status, WorkflowStatus::Running);
    assert_eq!(fresh.next_step, None);
    assert_eq!(fresh.steps.len(), 3);
    assert_eq!(fresh.steps[0].status, WorkflowStepStatus::Running);
    runtime
        .complete(&in_flight, WorkflowOutcome::Succeeded)
        .unwrap();
    assert_eq!(
        runtime.progress(&second).unwrap().next_step,
        Some("process")
    );
    assert_eq!(snapshot.steps.len(), 4);
}
