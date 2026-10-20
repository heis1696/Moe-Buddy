//! One chat turn advanced through the real `WorkflowRuntime` contract.
//!
//! The application owns the definition and drives each phase explicitly. Agent,
//! session, memory and UI facades remain authoritative; this module only orders
//! their existing calls and keeps the host transport ticket bound to the same
//! turn and workflow run.

use super::*;

use moe_agent_runtime::{
    AgentBinding, AgentDelivery, AgentResult, AgentTask, ModelOutcome, OfflineModel,
    WorkflowDefinition, WorkflowExecution, WorkflowOutcome, WorkflowRun, WorkflowRuntime,
    WorkflowStep,
};

use crate::contract::{ExternalRequest, ExternalRequestTicket, RawModelReply};

/// The model result captured while `AgentRuntime` still owns the opaque result.
#[derive(Clone, Debug)]
enum ReceivedReply {
    Text {
        text: String,
        presentation: Vec<PresentationEvent>,
    },
    Failed,
    Cancelled,
    Unknown,
}

impl ReceivedReply {
    fn from_outcome(outcome: &ModelOutcome) -> Self {
        match outcome {
            ModelOutcome::Text { text, presentation } => Self::Text {
                text: text.clone(),
                presentation: presentation.clone(),
            },
            ModelOutcome::Failed(_) => Self::Failed,
            ModelOutcome::Cancelled => Self::Cancelled,
            ModelOutcome::Unknown => Self::Unknown,
        }
    }
}

/// Result of one external completion callback before the application decides
/// whether to persist it or refresh the display.
///
/// `Rejected` means the ticket never belonged to the in-flight request, so the
/// callback performed no business, storage or display side effect. `Admitted`
/// means the pending turn was consumed and the business phases actually ran.
pub(crate) enum ExternalCallback {
    Rejected(AppError),
    Admitted { result: Result<(), AppError> },
}

/// Records the exact model result before the runtime consumes it. The raw text
/// stays here for parsing; it never replaces the runtime's session ledger body.
struct RecordingOfflineModel<'a> {
    inner: OfflineVerifierModel,
    captured: &'a mut Option<ReceivedReply>,
}

impl OfflineModel for RecordingOfflineModel<'_> {
    fn infer(&mut self, input: OfflineInput<'_>) -> ModelOutcome {
        let outcome = self.inner.infer(input);
        *self.captured = Some(ReceivedReply::from_outcome(&outcome));
        outcome
    }
}

pub(super) struct ChatTurn {
    id: u64,
    user_text: String,
    runtime: WorkflowRuntime,
    run: WorkflowRun,
    wait: Option<WorkflowExecution>,
    request: Option<TrustedRequest>,
    registration: Option<Registration>,
    binding: Option<AgentBinding>,
    task: Option<AgentTask>,
    result: Option<AgentResult>,
    received: Option<ReceivedReply>,
    parsed: Option<ParsedReply>,
    delivery: Option<AgentDelivery>,
    emitter: Option<moe_agent_runtime::LogEmitter>,
    committed: bool,
    published: bool,
    publish: Option<WorkflowExecution>,
    pub(super) user_visible: bool,
    external_unknown: bool,
}

impl ChatTurn {
    fn start(id: u64, user_text: &str) -> Result<Self, AppError> {
        let definition = WorkflowDefinition::new(
            "chat-turn",
            1,
            vec![
                WorkflowStep::new("admission", &[]),
                WorkflowStep::new("prepare_input", &["admission"]),
                WorkflowStep::new("request_wait", &["prepare_input"]),
                WorkflowStep::new("receive", &["request_wait"]),
                WorkflowStep::new("parse", &["receive"]),
                WorkflowStep::new("commit", &["parse"]),
                WorkflowStep::new("publish", &["commit"]),
            ],
        )
        .map_err(AppError::from_workflow_core)?;
        let mut runtime = WorkflowRuntime::new(1).map_err(AppError::from_workflow_core)?;
        let run = runtime
            .start(&definition)
            .map_err(AppError::from_workflow_core)?;
        Ok(Self {
            id,
            user_text: user_text.to_owned(),
            runtime,
            run,
            wait: None,
            request: None,
            registration: None,
            binding: None,
            task: None,
            result: None,
            received: None,
            parsed: None,
            delivery: None,
            emitter: None,
            committed: false,
            published: false,
            publish: None,
            user_visible: false,
            external_unknown: false,
        })
    }

    fn begin(&mut self, step: &str) -> Result<WorkflowExecution, AppError> {
        self.runtime
            .begin(&self.run, step)
            .map_err(AppError::from_workflow_core)
    }

    fn complete(
        &mut self,
        execution: &WorkflowExecution,
        outcome: WorkflowOutcome,
    ) -> Result<(), AppError> {
        self.runtime
            .complete(execution, outcome)
            .map_err(AppError::from_workflow_core)
    }

    fn progress(&self) -> ChatTurnProgress {
        let progress = self
            .runtime
            .progress(&self.run)
            .expect("owned chat workflow run");
        ChatTurnProgress {
            turn: self.id,
            definition_id: progress.definition_id,
            definition_version: progress.definition_version,
            status: progress.status,
            next_step: progress.next_step,
            steps: progress.steps,
            committed: self.committed,
            published: self.published,
            business_outcome: self
                .delivery
                .as_ref()
                .map(|delivery| match delivery.state() {
                    TerminalState::Completed => crate::contract::BusinessOutcome::Completed,
                    TerminalState::Failed => crate::contract::BusinessOutcome::Failed,
                    TerminalState::Cancelled => crate::contract::BusinessOutcome::Cancelled,
                    TerminalState::Unknown => crate::contract::BusinessOutcome::Unknown,
                }),
        }
    }

    pub(super) fn is_registered(&self) -> bool {
        self.registration.is_some()
    }
}

fn internal_phase(detail: &'static str) -> AppError {
    AppError::rejection(
        "chat_phase_failed",
        "聊天阶段缺少必要的受信状态，已停止",
        detail,
    )
}

fn operation_state(state: TerminalState) -> moe_agent_runtime::OperationState {
    match state {
        TerminalState::Completed => moe_agent_runtime::OperationState::Completed,
        TerminalState::Failed => moe_agent_runtime::OperationState::Failed,
        TerminalState::Cancelled => moe_agent_runtime::OperationState::Cancelled,
        TerminalState::Unknown => moe_agent_runtime::OperationState::Unknown,
    }
}

impl ChatCore {
    pub(super) fn start_turn(&mut self, text: &str) -> Result<ChatTurn, AppError> {
        let id = self.turn_sequence.checked_add(1).ok_or_else(|| {
            AppError::rejection(
                "turn_exhausted",
                "聊天轮次编号已耗尽，请重启宿主",
                "chat turn sequence exhausted",
            )
            .not_executed()
        })?;
        self.turn_sequence = id;
        self.last_parsed_reply = None;
        let mut turn = ChatTurn::start(id, text)?;
        turn.emitter = self.open_run();
        Ok(turn)
    }

    pub(super) fn record_progress(&mut self, turn: &ChatTurn) {
        self.last_progress = Some(turn.progress());
    }

    /// Whether a committed turn still awaits the application display result.
    pub(crate) fn has_pending_publish(&self) -> bool {
        self.pending_publish.is_some()
    }

    /// Identity of the committed turn whose `publish` node is still running.
    pub(crate) fn pending_publication(&self) -> Option<u64> {
        self.pending_publish.as_ref().map(|turn| turn.id)
    }

    pub(crate) fn chat_progress(&self) -> Option<ChatTurnProgress> {
        self.last_progress.clone()
    }

    pub(crate) fn last_parsed_reply(&self) -> Option<ParsedReply> {
        self.last_parsed_reply.clone()
    }

    pub(crate) fn install_reply_parser(
        &mut self,
        parser: Box<dyn ReplyParser>,
    ) -> Result<(), AppError> {
        if self.pending_external.is_some() || self.chat_history.status == ChatStatus::Sending {
            return Err(AppError::rejection(
                "request_pending",
                "请等待当前请求结束再替换回复解析器",
                "cannot replace the reply parser during an in-flight external request",
            )
            .not_executed());
        }
        self.parser = parser;
        Ok(())
    }

    pub(crate) fn take_external_request(&mut self) -> Option<ExternalRequest> {
        let turn = self.pending_external.as_ref()?;
        let input = self.external_input.take()?;
        Some(ExternalRequest::new(
            input,
            ExternalRequestTicket::new(self.ticket_owner.clone(), turn.id),
        ))
    }

    /// Validate the ticket and consume the pending request.
    ///
    /// This step must stay free of storage, display and history-refresh side
    /// effects: a foreign, duplicate or revoked callback may not damage the
    /// correct in-flight request or quarantine later sends.
    fn claim_external(&mut self, ticket: ExternalRequestTicket) -> Result<ChatTurn, AppError> {
        self.ensure_active().map_err(AppError::not_executed)?;
        if self.pending_publish.is_some() {
            return Err(AppError::rejection(
                "request_pending",
                "上一轮已提交，正在等待展示发布结果",
                "a committed turn still awaits its display publication result",
            )
            .not_executed());
        }
        let Some(pending) = self.pending_external.as_ref() else {
            return Err(AppError::rejection(
                "no_request",
                "当前没有等待中的请求",
                "no external request",
            )
            .not_executed());
        };
        if !ticket.matches(&self.ticket_owner, pending.id) {
            return Err(AppError::rejection(
                "stale_external_ticket",
                "请求票据已失效或不属于当前请求，已拒绝",
                "external completion ticket does not match the in-flight request",
            )
            .not_executed());
        }
        let turn = self
            .pending_external
            .take()
            .expect("pending turn checked above");
        self.external_input = None;
        self.last_parsed_reply = None;
        Ok(turn)
    }

    /// Receive, parse and commit one admitted callback, then stage the publish
    /// node. The node is completed only after the application reports the real
    /// display publication result through [`Self::finish_display_publish`].
    pub(crate) fn complete_external(
        &mut self,
        ticket: ExternalRequestTicket,
        outcome: ModelOutcome,
        now_ms: u64,
    ) -> ExternalCallback {
        let mut turn = match self.claim_external(ticket) {
            Ok(turn) => turn,
            Err(error) => return ExternalCallback::Rejected(error),
        };
        turn.external_unknown = matches!(&outcome, ModelOutcome::Unknown);
        self.external_unknown_evidence = turn.external_unknown;
        let completed = (|| {
            self.external_receive_phase(&mut turn, outcome, now_ms)?;
            self.parse_phase(&mut turn)?;
            self.commit_phase(&mut turn, now_ms)?;
            self.stage_publish_phase(&mut turn)?;
            Ok::<(), AppError>(())
        })();
        self.record_progress(&turn);
        let result = match completed {
            Ok(()) => {
                self.refund_success(&turn);
                self.pending_publish = Some(turn);
                Ok(())
            }
            Err(primary) => {
                let error = self.finish_registered_failure(&mut turn, primary, now_ms);
                Err(error)
            }
        };
        ExternalCallback::Admitted { result }
    }

    /// Complete the staged `publish` node with the application display result.
    ///
    /// `publication` is `Ok` only when the real display facade rendered this
    /// turn's staged projection. A failure marks the node `Failed` and keeps
    /// `published` false while the committed business receipt is untouched.
    pub(crate) fn finish_display_publish(
        &mut self,
        publication: Result<(), AppError>,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let Some(mut turn) = self.pending_publish.take() else {
            return publication;
        };
        let Some(execution) = turn.publish.take() else {
            return Err(internal_phase(
                "publish completion requires a staged workflow execution",
            ));
        };
        let outcome = if publication.is_ok() {
            WorkflowOutcome::Succeeded
        } else {
            WorkflowOutcome::Failed
        };
        let completion = turn.complete(&execution, outcome);
        mark(
            &mut self.diagnostics,
            turn.emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::Finish)),
        );
        turn.published = publication.is_ok();
        self.record_progress(&turn);
        match (publication, completion) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(error),
            (Err(primary), Ok(())) => Err(AppError::display_after_commit(primary)),
            (Err(primary), Err(error)) => Err(AppError::with_retained(
                AppError::display_after_commit(primary),
                error,
            )),
        }
    }

    pub(super) fn admission_phase(
        &mut self,
        turn: &mut ChatTurn,
        text: &str,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let execution = turn.begin("admission")?;
        mark(
            &mut self.diagnostics,
            turn.emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Admission)),
        );
        let request = match self.admit(text) {
            Ok(request) => request,
            Err(error) => {
                let error = error.not_executed();
                mark_error_gateway(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    Step::Admission,
                    &error,
                );
                turn.complete(&execution, WorkflowOutcome::Failed)?;
                return Err(error);
            }
        };
        let registration = match self.sessions.register_user(
            SessionAccess::new(&self.ingress, &request, now_ms),
            UserEvent::new(UserEventKind::New),
        ) {
            Ok(registration) => registration,
            Err(error) => {
                let error = AppError::from_gateway(error).not_executed();
                mark_error_gateway(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    Step::Admission,
                    &error,
                );
                turn.complete(&execution, WorkflowOutcome::Failed)?;
                return Err(error);
            }
        };
        self.admitted_events += 2;
        if !self.admitted_fragments.contains(&registration.session) {
            self.admitted_fragments.push(registration.session.clone());
        }
        self.admitted_chars += text.chars().count() + REPLY_CHARS_BUDGET;
        self.session_turns = self.session_turns.saturating_add(1);
        turn.request = Some(request);
        turn.registration = Some(registration);
        turn.complete(&execution, WorkflowOutcome::Succeeded)?;
        mark(
            &mut self.diagnostics,
            turn.emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::Admission)),
        );
        Ok(())
    }

    pub(super) fn prepare_input_phase(
        &mut self,
        turn: &mut ChatTurn,
        online: bool,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let execution = turn.begin("prepare_input")?;
        mark(
            &mut self.diagnostics,
            turn.emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Prepare)),
        );
        let request = turn
            .request
            .as_ref()
            .ok_or_else(|| internal_phase("prepare_input requires an admitted trusted request"))?;
        let registration = turn.registration.as_ref().ok_or_else(|| {
            internal_phase("prepare_input requires a registered session interaction")
        })?;
        let access = AgentAccess::new(&self.ingress, request, &self.presets, now_ms);
        let sources = self.grant_sources();
        let binding = match self.agent.bind_session(
            &access,
            &mut self.sessions,
            &registration.session,
            sources,
        ) {
            Ok(binding) => binding,
            Err(error) => {
                let error = AppError::from(error);
                mark_error(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    Step::Prepare,
                    &error,
                );
                turn.complete(&execution, WorkflowOutcome::Failed)?;
                return Err(error);
            }
        };
        let query = RetrievalQuery::new("");
        let prepared = if online {
            self.agent.prepare_external(
                &access,
                AgentServices::new(&mut self.sessions, self.memory.as_mut()),
                &binding,
                &registration.interaction,
                &query,
            )
        } else {
            self.agent.prepare(
                &access,
                AgentServices::new(&mut self.sessions, self.memory.as_mut()),
                &binding,
                &registration.interaction,
                &query,
            )
        };
        turn.binding = Some(binding);
        match prepared {
            Ok(task) => {
                turn.task = Some(task);
                turn.complete(&execution, WorkflowOutcome::Succeeded)?;
                mark(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    DiagnosticKind::Run(RunObservation::StageReturned(Step::Prepare)),
                );
                Ok(())
            }
            Err(error) => {
                let error = AppError::from(error);
                mark_error(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    Step::Prepare,
                    &error,
                );
                turn.complete(&execution, WorkflowOutcome::Failed)?;
                Err(error)
            }
        }
    }

    pub(super) fn offline_request_phase(
        &mut self,
        turn: &mut ChatTurn,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let execution = turn.begin("request_wait")?;
        mark(
            &mut self.diagnostics,
            turn.emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Infer)),
        );
        let request = turn
            .request
            .as_ref()
            .ok_or_else(|| internal_phase("request_wait requires an admitted request"))?;
        let access = AgentAccess::new(&self.ingress, request, &self.presets, now_ms);
        let mut task = turn
            .task
            .take()
            .ok_or_else(|| internal_phase("request_wait requires a prepared task"))?;
        let mut received = None;
        let result = {
            let mut model = RecordingOfflineModel {
                inner: OfflineVerifierModel,
                captured: &mut received,
            };
            self.agent.infer(
                &access,
                AgentServices::new(&mut self.sessions, self.memory.as_mut()),
                &mut task,
                &mut model,
            )
        };
        turn.task = Some(task);
        match result {
            Ok(result) => {
                turn.result = Some(result);
                turn.received = received;
                turn.complete(&execution, WorkflowOutcome::Succeeded)?;
                mark(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    DiagnosticKind::Run(RunObservation::Transport(
                        moe_agent_runtime::TransportState::NotAttempted,
                    )),
                );
                Ok(())
            }
            Err(error) => {
                let error = AppError::from(error);
                mark_error(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    Step::Infer,
                    &error,
                );
                turn.complete(&execution, WorkflowOutcome::Failed)?;
                Err(error)
            }
        }
    }

    pub(super) fn receive_recorded_phase(
        &mut self,
        turn: &mut ChatTurn,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let execution = turn.begin("receive")?;
        if turn.received.is_none() {
            turn.complete(&execution, WorkflowOutcome::Failed)?;
            return Err(internal_phase(
                "receive requires the recorded offline model result",
            ));
        }
        turn.complete(&execution, WorkflowOutcome::Succeeded)?;
        mark(
            &mut self.diagnostics,
            turn.emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::Infer)),
        );
        Ok(())
    }

    pub(super) fn external_request_phase(
        &mut self,
        turn: &mut ChatTurn,
        now_ms: u64,
    ) -> Result<moe_agent_runtime::ExternalInput, AppError> {
        let execution = turn.begin("request_wait")?;
        mark(
            &mut self.diagnostics,
            turn.emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Infer)),
        );
        let request = turn
            .request
            .as_ref()
            .ok_or_else(|| internal_phase("request_wait requires an admitted request"))?;
        let access = AgentAccess::new(&self.ingress, request, &self.presets, now_ms);
        let mut task = turn
            .task
            .take()
            .ok_or_else(|| internal_phase("request_wait requires a prepared task"))?;
        let dispatched = self.agent.dispatch_external(
            &access,
            AgentServices::new(&mut self.sessions, self.memory.as_mut()),
            &mut task,
        );
        turn.task = Some(task);
        match dispatched {
            Ok(input) => {
                turn.wait = Some(execution);
                mark(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    DiagnosticKind::Background(BackgroundCode::Waiting),
                );
                Ok(input)
            }
            Err(error) => {
                let error = AppError::from(error);
                mark_error(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    Step::Infer,
                    &error,
                );
                turn.complete(&execution, WorkflowOutcome::Failed)?;
                Err(error)
            }
        }
    }

    fn external_receive_phase(
        &mut self,
        turn: &mut ChatTurn,
        outcome: ModelOutcome,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let received = ReceivedReply::from_outcome(&outcome);
        let wait = turn
            .wait
            .take()
            .ok_or_else(|| internal_phase("receive requires an in-flight request ticket"))?;
        turn.complete(&wait, WorkflowOutcome::Succeeded)?;
        // The application boundary only receives the host's business outcome
        // (`ModelOutcome`). It has no transport receipt proving the request was
        // written, failed before writing or was cancelled remotely, so it must
        // not derive `Written`/`Failed`/`Cancelled` from model text or business
        // status. Transport state stays explicitly unknown.
        mark(
            &mut self.diagnostics,
            turn.emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::Transport(
                moe_agent_runtime::TransportState::Unknown,
            )),
        );
        let execution = turn.begin("receive")?;
        let request = turn
            .request
            .as_ref()
            .ok_or_else(|| internal_phase("receive requires an admitted request"))?;
        let access = AgentAccess::new(&self.ingress, request, &self.presets, now_ms);
        let mut task = turn
            .task
            .take()
            .ok_or_else(|| internal_phase("receive requires a dispatched task"))?;
        let accepted = self.agent.complete_external(
            &access,
            AgentServices::new(&mut self.sessions, self.memory.as_mut()),
            &mut task,
            outcome,
        );
        turn.task = Some(task);
        match accepted {
            Ok(result) => {
                turn.result = Some(result);
                turn.received = Some(received);
                turn.complete(&execution, WorkflowOutcome::Succeeded)?;
                mark(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    DiagnosticKind::Run(RunObservation::StageReturned(Step::Infer)),
                );
                Ok(())
            }
            Err(error) => {
                let error = AppError::from(error);
                mark_error(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    Step::Infer,
                    &error,
                );
                turn.complete(&execution, WorkflowOutcome::Failed)?;
                Err(error)
            }
        }
    }

    pub(super) fn parse_phase(&mut self, turn: &mut ChatTurn) -> Result<(), AppError> {
        let execution = turn.begin("parse")?;
        let parsed = match turn.received.as_ref() {
            Some(ReceivedReply::Text { text, presentation }) => {
                let raw = RawModelReply::new(text, presentation);
                match self.parser.parse(&raw) {
                    Ok(parsed) => Some(parsed),
                    Err(error) => {
                        let error = AppError::from_parse(error);
                        turn.complete(&execution, WorkflowOutcome::Failed)?;
                        return Err(error);
                    }
                }
            }
            Some(ReceivedReply::Failed | ReceivedReply::Cancelled | ReceivedReply::Unknown) => None,
            None => {
                turn.complete(&execution, WorkflowOutcome::Failed)?;
                return Err(internal_phase(
                    "parse requires a received model result or a terminal outcome",
                ));
            }
        };
        turn.parsed = parsed;
        turn.complete(&execution, WorkflowOutcome::Succeeded)?;
        Ok(())
    }

    pub(super) fn commit_phase(
        &mut self,
        turn: &mut ChatTurn,
        now_ms: u64,
    ) -> Result<(), AppError> {
        let execution = turn.begin("commit")?;
        mark(
            &mut self.diagnostics,
            turn.emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Finish)),
        );
        let request = turn
            .request
            .as_ref()
            .ok_or_else(|| internal_phase("commit requires an admitted request"))?;
        let access = AgentAccess::new(&self.ingress, request, &self.presets, now_ms);
        let mut task = turn
            .task
            .take()
            .ok_or_else(|| internal_phase("commit requires a prepared task"))?;
        let result = turn
            .result
            .as_ref()
            .ok_or_else(|| internal_phase("commit requires an accepted model result"))?;
        let finished = self.agent.finish(
            &access,
            AgentServices::new(&mut self.sessions, self.memory.as_mut()),
            &mut task,
            result,
        );
        turn.task = Some(task);
        match finished {
            Ok(delivery) => {
                let state = delivery.state();
                turn.delivery = Some(delivery);
                turn.committed = true;
                mark(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    DiagnosticKind::Run(RunObservation::Operation(operation_state(state))),
                );
                turn.complete(&execution, WorkflowOutcome::Succeeded)?;
                Ok(())
            }
            Err(error) => {
                let error = AppError::from(error);
                mark_error(
                    &mut self.diagnostics,
                    turn.emitter.as_ref(),
                    now_ms,
                    Step::Finish,
                    &error,
                );
                turn.complete(&execution, WorkflowOutcome::Failed)?;
                Err(error)
            }
        }
    }

    /// Begin the `publish` node and stage the projection the application display
    /// will render. The node is intentionally left `Running`; only the
    /// application layer can observe the real display facade and finish it.
    pub(super) fn stage_publish_phase(&mut self, turn: &mut ChatTurn) -> Result<(), AppError> {
        let execution = turn.begin("publish")?;
        let delivery = turn
            .delivery
            .as_ref()
            .ok_or_else(|| internal_phase("publish requires a committed delivery"))?;
        let state = delivery.state();
        if !turn.user_visible {
            self.push_message(ChatMessage {
                role: MessageRole::User,
                text: turn.user_text.clone(),
            });
            turn.user_visible = true;
        }
        self.last_parsed_reply = turn.parsed.clone();
        let (status, failure) = match state {
            TerminalState::Completed => {
                if let Some(parsed) = &turn.parsed {
                    self.push_message(ChatMessage {
                        role: MessageRole::Assistant,
                        text: parsed.display_text().to_owned(),
                    });
                    (ChatStatus::Ready, None)
                } else {
                    (ChatStatus::Failed, Some("send_failed"))
                }
            }
            TerminalState::Failed => (ChatStatus::Failed, Some("send_failed")),
            TerminalState::Cancelled => (ChatStatus::Failed, Some("send_cancelled")),
            TerminalState::Unknown => (ChatStatus::Unknown, Some("send_unknown")),
        };
        self.chat_history.status = status;
        self.chat_history.failure = failure.map(str::to_owned);
        turn.publish = Some(execution);
        Ok(())
    }

    pub(super) fn refund_success(&mut self, turn: &ChatTurn) {
        let Some(delivery) = &turn.delivery else {
            return;
        };
        let reply_chars = delivery.text().map_or(0, |text| text.chars().count());
        self.admitted_chars = self
            .admitted_chars
            .saturating_sub(REPLY_CHARS_BUDGET - reply_chars);
        if delivery.text().is_none() {
            self.admitted_events = self.admitted_events.saturating_sub(1);
        }
    }

    pub(super) fn finish_registered_failure(
        &mut self,
        turn: &mut ChatTurn,
        primary: AppError,
        now_ms: u64,
    ) -> AppError {
        if turn.external_unknown {
            self.external_unknown_evidence = true;
            if !turn.user_visible {
                self.push_message(ChatMessage {
                    role: MessageRole::User,
                    text: turn.user_text.clone(),
                });
                turn.user_visible = true;
            }
            let request = turn
                .request
                .as_ref()
                .expect("registered unknown turn retains its trusted request");
            let registration = turn
                .registration
                .as_ref()
                .expect("registered unknown turn retains its interaction");
            return self.settle_external_unknown(
                request,
                registration,
                primary,
                turn.emitter.as_ref(),
                now_ms,
            );
        }
        let request = turn
            .request
            .as_ref()
            .expect("registered turn retains its trusted request");
        let registration = turn
            .registration
            .as_ref()
            .expect("registered turn retains its interaction");
        let error = self.settle_after_failure(
            request,
            registration,
            primary,
            turn.emitter.as_ref(),
            now_ms,
        );
        if !turn.user_visible && matches!(error.public_code(), "send_cancelled" | "send_unknown") {
            self.push_message(ChatMessage {
                role: MessageRole::User,
                text: turn.user_text.clone(),
            });
            turn.user_visible = true;
        }
        self.chat_history.status = if error.public_code() == "send_unknown" {
            ChatStatus::Unknown
        } else {
            ChatStatus::Failed
        };
        self.chat_history.failure = Some(error.public_code().to_owned());
        error
    }
}
