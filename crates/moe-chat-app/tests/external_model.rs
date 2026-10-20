use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::error::Error;
use std::rc::Rc;

use moe_agent_runtime::{
    ExternalInput, ModelFailure, ModelOutcome, PresentationEvent, RawRetention, WorkflowStatus,
    WorkflowStepStatus,
};
use moe_chat_app::{
    Action, AppError, BusinessOutcome, ChatApplication, ChatHistory, ChatMessage, ChatStatus,
    ColdHistoryMaintenance, ExecutionDisposition, ExternalRequestTicket, HistoryAccess,
    HistoryChange, HistoryCommitReceipts, HistoryRecovery, HistoryRequest, HistorySession,
    ModelSettings, PanelKind, ParsedReply, RawHistoryCommitReceipt, RawModelReply, ReplyParseError,
    ReplyParser,
};

fn app(allowed: bool) -> ChatApplication {
    let mut app = ChatApplication::new(91).unwrap();
    app.dispatch(Action::Open(PanelKind::Chat), 0).unwrap();
    app.configure_model(&ModelSettings {
        endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
        model: "synthetic".into(),
        persona: "synthetic persona".into(),
        allow_transmission: allowed,
    })
    .unwrap();
    app.dispatch(Action::SetDraft("synthetic question".into()), 1)
        .unwrap();
    app
}

fn session_guard(app: &ChatApplication) -> (String, u64) {
    let snapshot = app.snapshot();
    (
        snapshot.active_session_id.expect("active logical session"),
        snapshot.sessions_revision,
    )
}

fn take_ticket(app: &mut ChatApplication) -> (ExternalInput, ExternalRequestTicket) {
    app.take_external_request()
        .expect("prepared external request")
        .into_parts()
}

fn text_outcome(text: &str) -> ModelOutcome {
    ModelOutcome::Text {
        text: text.into(),
        presentation: Vec::new(),
    }
}

fn step_status(app: &ChatApplication, id: &str) -> WorkflowStepStatus {
    app.chat_progress()
        .expect("turn progress exists")
        .steps
        .iter()
        .find(|step| step.id == id)
        .map(|step| step.status)
        .expect("step id is part of the fixed definition")
}

#[test]
fn explicit_permission_single_dispatch_and_bound_progress() {
    let mut denied = app(false);
    assert!(denied.dispatch(Action::Send, 2).is_err());
    assert!(denied.take_external_request().is_none());
    assert!(!denied.snapshot().external_transmission_allowed);

    let mut allowed = app(true);
    allowed.dispatch(Action::Send, 2).unwrap();
    assert_eq!(allowed.snapshot().chat.status, ChatStatus::Sending);
    let progress = allowed.chat_progress().expect("waiting progress");
    assert_eq!(progress.status, WorkflowStatus::Running);
    assert_eq!(progress.next_step, None);
    assert_eq!(
        step_status(&allowed, "admission"),
        WorkflowStepStatus::Succeeded
    );
    assert_eq!(
        step_status(&allowed, "prepare_input"),
        WorkflowStepStatus::Succeeded
    );
    assert_eq!(
        step_status(&allowed, "request_wait"),
        WorkflowStepStatus::Running
    );
    assert_eq!(
        step_status(&allowed, "receive"),
        WorkflowStepStatus::Pending
    );
    assert_eq!(
        step_status(&allowed, "publish"),
        WorkflowStepStatus::Pending
    );
    assert!(!progress.committed && !progress.published);

    let (input, ticket) = take_ticket(&mut allowed);
    assert_eq!(input.persona(), "synthetic persona");
    assert!(
        input
            .conversation()
            .iter()
            .any(|event| event.body() == "synthetic question")
    );
    assert!(allowed.take_external_request().is_none());

    let duplicate = ticket.clone();
    allowed
        .complete_external(ticket, text_outcome("synthetic reply"), 3)
        .unwrap();
    let snapshot = allowed.snapshot();
    assert_eq!(snapshot.chat.messages.len(), 2);
    assert_eq!(snapshot.chat.status, ChatStatus::Ready);
    assert_eq!(
        allowed.last_parsed_reply().unwrap().display_text(),
        "synthetic reply"
    );
    let progress = allowed.chat_progress().expect("final progress");
    assert_eq!(progress.status, WorkflowStatus::Succeeded);
    assert!(progress.committed && progress.published);
    for id in [
        "admission",
        "prepare_input",
        "request_wait",
        "receive",
        "parse",
        "commit",
        "publish",
    ] {
        assert_eq!(step_status(&allowed, id), WorkflowStepStatus::Succeeded);
    }

    let error = allowed
        .complete_external(duplicate, ModelOutcome::Unknown, 4)
        .expect_err("duplicate completion is rejected");
    assert_eq!(error.public_code(), "no_request");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    assert_eq!(allowed.snapshot().chat.messages.len(), 2);
}

#[test]
fn unknown_cancelled_and_failed_results_are_not_retried_automatically() {
    let mut unknown = app(true);
    unknown.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut unknown);
    unknown
        .complete_external(ticket, ModelOutcome::Unknown, 3)
        .unwrap();
    assert_eq!(unknown.snapshot().chat.status, ChatStatus::Unknown);
    assert_eq!(
        unknown.snapshot().chat.failure.as_deref(),
        Some("send_unknown")
    );
    let progress = unknown.chat_progress().expect("unknown turn progress");
    assert_eq!(
        progress.status,
        WorkflowStatus::Succeeded,
        "the process finished even though the model result is unknown"
    );
    assert!(progress.committed && progress.published);
    assert_eq!(
        progress.business_outcome,
        Some(BusinessOutcome::Unknown),
        "read-only progress must expose the unknown business terminal state"
    );
    assert_eq!(unknown.snapshot().chat.messages.len(), 1);
    unknown
        .dispatch(Action::SetDraft("do not replay".into()), 4)
        .unwrap();
    let error = unknown
        .dispatch(Action::Send, 5)
        .expect_err("unknown blocks");
    assert_eq!(error.public_code(), "send_unknown");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    assert!(unknown.take_external_request().is_none());

    let mut cancelled = app(true);
    cancelled.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut cancelled);
    cancelled
        .complete_external(ticket, ModelOutcome::Cancelled, 3)
        .unwrap();
    assert_eq!(cancelled.snapshot().chat.status, ChatStatus::Failed);
    assert!(!cancelled.snapshot().session_isolation_allowed);
    assert_eq!(
        cancelled.snapshot().chat.failure.as_deref(),
        Some("send_cancelled")
    );
    assert_eq!(cancelled.snapshot().chat.messages.len(), 1);
    let progress = cancelled.chat_progress().expect("cancelled turn progress");
    assert_eq!(progress.status, WorkflowStatus::Succeeded);
    assert!(progress.committed && progress.published);
    assert_eq!(progress.business_outcome, Some(BusinessOutcome::Cancelled));

    let mut failed = app(true);
    failed.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut failed);
    failed
        .complete_external(ticket, ModelOutcome::Failed(ModelFailure::Unavailable), 3)
        .unwrap();
    assert_eq!(failed.snapshot().chat.status, ChatStatus::Failed);
    assert_eq!(
        failed.snapshot().chat.failure.as_deref(),
        Some("send_failed")
    );
    assert_eq!(failed.snapshot().chat.draft, "synthetic question");
    let progress = failed.chat_progress().expect("failed turn progress");
    assert_eq!(progress.status, WorkflowStatus::Succeeded);
    assert!(progress.committed && progress.published);
    assert_eq!(progress.business_outcome, Some(BusinessOutcome::Failed));
}

#[test]
fn external_unknown_at_deadline_equality_stays_unknown_and_local_cancel_allows_persistent_isolation()
 {
    let (mut app, _, _) = app_with_failable_history(90);
    let old_id = app.snapshot().active_session_id.clone().unwrap();
    app.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut app);

    let error = app
        .complete_external(ticket, ModelOutcome::Unknown, 3_600_002)
        .expect_err("the interaction deadline equality cannot prove remote cancellation");
    assert_eq!(error.public_code(), "send_unknown");
    assert_eq!(error.execution_disposition(), ExecutionDisposition::Unknown);
    assert_eq!(app.snapshot().chat.status, ChatStatus::Unknown);
    assert_eq!(app.snapshot().chat.failure.as_deref(), Some("send_unknown"));
    assert!(app.snapshot().session_isolation_allowed);
    assert!(app.take_external_request().is_none());

    let (expected_session_id, expected_sessions_revision) = session_guard(&app);
    app.dispatch(
        Action::NewSession {
            expected_session_id,
            expected_sessions_revision,
        },
        3_600_003,
    )
    .unwrap();
    let snapshot = app.snapshot();
    assert_ne!(snapshot.active_session_id.as_deref(), Some(old_id.as_str()));
    assert_eq!(snapshot.chat.status, ChatStatus::Ready);
    assert!(!snapshot.session_isolation_allowed);
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == old_id)
            .unwrap()
            .status,
        ChatStatus::Unknown
    );
}

#[test]
fn cross_session_stale_ticket_does_not_consume_the_new_pending_request() {
    let (mut app, _, _) = app_with_failable_history(91);
    let old_id = app.snapshot().active_session_id.clone().unwrap();
    app.dispatch(Action::Send, 2).unwrap();
    let (_, old_ticket) = take_ticket(&mut app);
    app.complete_external(old_ticket.clone(), ModelOutcome::Unknown, 3)
        .unwrap();

    let (expected_session_id, expected_sessions_revision) = session_guard(&app);
    app.dispatch(
        Action::NewSession {
            expected_session_id,
            expected_sessions_revision,
        },
        4,
    )
    .unwrap();
    app.dispatch(Action::SetDraft("new session request".into()), 5)
        .unwrap();
    app.dispatch(Action::Send, 6).unwrap();
    let (_, current_ticket) = take_ticket(&mut app);

    let stale = app
        .complete_external(old_ticket, text_outcome("stale old reply"), 7)
        .expect_err("old session ticket cannot consume the current request");
    assert_eq!(stale.public_code(), "stale_external_ticket");
    assert_eq!(
        stale.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    assert_eq!(app.snapshot().chat.status, ChatStatus::Sending);
    app.complete_external(current_ticket, text_outcome("current reply"), 8)
        .unwrap();
    assert_eq!(app.snapshot().chat.status, ChatStatus::Ready);

    let (expected_session_id, expected_sessions_revision) = session_guard(&app);
    app.dispatch(
        Action::SwitchSession {
            id: old_id.clone(),
            expected_session_id,
            expected_sessions_revision,
        },
        9,
    )
    .unwrap();
    assert_eq!(app.snapshot().chat.status, ChatStatus::Unknown);
    assert_eq!(app.snapshot().chat.failure.as_deref(), Some("send_unknown"));
}

#[test]
fn stale_duplicate_and_cross_instance_tickets_do_not_consume_the_current_request() {
    let mut first = app(true);
    first.dispatch(Action::Send, 2).unwrap();
    let (_, first_ticket) = take_ticket(&mut first);
    let stale = first_ticket.clone();
    first
        .complete_external(first_ticket, text_outcome("first reply"), 3)
        .unwrap();

    first
        .dispatch(Action::SetDraft("second question".into()), 4)
        .unwrap();
    first.dispatch(Action::Send, 5).unwrap();
    let (_, second_ticket) = take_ticket(&mut first);
    let error = first
        .complete_external(stale.clone(), text_outcome("late reply"), 6)
        .expect_err("older turn ticket is rejected");
    assert_eq!(error.public_code(), "stale_external_ticket");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    assert_eq!(first.snapshot().chat.status, ChatStatus::Sending);
    assert_eq!(first.snapshot().chat.messages.len(), 3);

    first
        .complete_external(second_ticket, text_outcome("second reply"), 7)
        .unwrap();
    assert_eq!(first.snapshot().chat.messages.len(), 4);
    assert_eq!(first.snapshot().chat.status, ChatStatus::Ready);

    let mut other = app(true);
    other.dispatch(Action::Send, 2).unwrap();
    let (_, other_ticket) = take_ticket(&mut other);
    other
        .dispatch(Action::SetDraft("other question".into()), 3)
        .unwrap();
    let error = other
        .complete_external(stale, text_outcome("foreign reply"), 4)
        .expect_err("ticket from another application instance is rejected");
    assert_eq!(error.public_code(), "stale_external_ticket");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    assert_eq!(other.snapshot().chat.status, ChatStatus::Sending);
    other
        .complete_external(other_ticket, text_outcome("other reply"), 5)
        .unwrap();
    assert_eq!(other.snapshot().chat.messages.len(), 2);
    assert_eq!(other.snapshot().chat.status, ChatStatus::Ready);
}

#[test]
fn offline_turn_advances_the_same_workflow_definition() {
    let mut app = ChatApplication::new(96).unwrap();
    app.dispatch(Action::Open(PanelKind::Chat), 0).unwrap();
    app.dispatch(Action::SetDraft("offline question".into()), 1)
        .unwrap();
    app.dispatch(Action::Send, 2).unwrap();
    let progress = app.chat_progress().expect("offline turn progress");
    assert_eq!(progress.definition_id, "chat-turn");
    assert_eq!(progress.definition_version, 1);
    assert_eq!(progress.status, WorkflowStatus::Succeeded);
    assert!(progress.committed && progress.published);
    assert!(progress.next_step.is_none());
    for id in [
        "admission",
        "prepare_input",
        "request_wait",
        "receive",
        "parse",
        "commit",
        "publish",
    ] {
        assert_eq!(step_status(&app, id), WorkflowStepStatus::Succeeded);
    }
    assert_eq!(app.snapshot().chat.messages.len(), 2);
    let parsed = app.last_parsed_reply().expect("offline parse result");
    assert_eq!(parsed.display_text(), app.snapshot().chat.messages[1].text);
    assert_eq!(parsed.presentation(), &[PresentationEvent::Neutral]);
}

#[test]
fn parser_receives_raw_text_and_display_and_proposals_stay_separate() {
    struct RecordingParser {
        calls: Rc<Cell<usize>>,
        last_raw: Rc<RefCell<Option<String>>>,
        fail: bool,
    }

    impl ReplyParser for RecordingParser {
        fn parse(&self, raw: &RawModelReply<'_>) -> Result<ParsedReply, ReplyParseError> {
            self.calls.set(self.calls.get() + 1);
            *self.last_raw.borrow_mut() = Some(raw.text().to_owned());
            if self.fail {
                return Err(ReplyParseError::new("synthetic parse failure"));
            }
            ParsedReply::new(
                format!("display:{}", raw.text()),
                raw.presentation().to_vec(),
            )
        }
    }

    let calls = Rc::new(Cell::new(0));
    let raw = Rc::new(RefCell::new(None));
    let mut online = app(true);
    online
        .install_reply_parser(Box::new(RecordingParser {
            calls: calls.clone(),
            last_raw: raw.clone(),
            fail: false,
        }))
        .unwrap();
    online.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut online);
    online
        .complete_external(
            ticket,
            ModelOutcome::Text {
                text: "raw reply".into(),
                presentation: vec![PresentationEvent::Happy],
            },
            3,
        )
        .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(raw.borrow().as_deref(), Some("raw reply"));
    let snapshot = online.snapshot();
    assert_eq!(snapshot.chat.messages[1].text, "display:raw reply");
    let parsed = online.last_parsed_reply().unwrap();
    assert_eq!(parsed.display_text(), "display:raw reply");
    assert_eq!(parsed.presentation(), &[PresentationEvent::Happy]);

    // The next authorized model input reads the ledger's original reply, not
    // the parser's display-only transformation.
    online
        .dispatch(Action::SetDraft("verify original history".into()), 4)
        .unwrap();
    online.dispatch(Action::Send, 5).unwrap();
    let (next_input, next_ticket) = take_ticket(&mut online);
    assert!(next_input.conversation().iter().any(|event| {
        event.role() == moe_agent_runtime::EventRole::Assistant && event.body() == "raw reply"
    }));
    assert!(
        next_input
            .conversation()
            .iter()
            .all(|event| event.body() != "display:raw reply")
    );
    online
        .complete_external(next_ticket, text_outcome("next raw reply"), 6)
        .unwrap();

    let offline_calls = Rc::new(Cell::new(0));
    let offline_raw = Rc::new(RefCell::new(None));
    let mut offline = ChatApplication::new(92).unwrap();
    offline
        .install_reply_parser(Box::new(RecordingParser {
            calls: offline_calls.clone(),
            last_raw: offline_raw.clone(),
            fail: false,
        }))
        .unwrap();
    offline.dispatch(Action::Open(PanelKind::Chat), 0).unwrap();
    offline
        .dispatch(Action::SetDraft("offline question".into()), 1)
        .unwrap();
    offline.dispatch(Action::Send, 2).unwrap();
    assert_eq!(offline_calls.get(), 1);
    assert!(
        offline_raw
            .borrow()
            .as_deref()
            .is_some_and(|text| text.contains("离线验收·非真实模型"))
    );
    assert_eq!(offline.snapshot().chat.messages.len(), 2);
    assert!(
        offline.snapshot().chat.messages[1]
            .text
            .starts_with("display:【离线验收")
    );
    assert_eq!(
        offline.last_parsed_reply().unwrap().display_text(),
        offline.snapshot().chat.messages[1].text
    );
}

#[test]
fn parse_failure_has_an_authoritative_receipt_and_no_reply_is_committed() {
    struct FailingParser;
    impl ReplyParser for FailingParser {
        fn parse(&self, _raw: &RawModelReply<'_>) -> Result<ParsedReply, ReplyParseError> {
            Err(ReplyParseError::new("synthetic parse failure"))
        }
    }

    let mut online = app(true);
    online
        .install_reply_parser(Box::new(FailingParser))
        .unwrap();
    online.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut online);
    let error = online
        .complete_external(ticket, text_outcome("raw reply"), 3)
        .expect_err("parse failure rejects the turn");
    assert_eq!(error.public_code(), "send_cancelled");
    assert_eq!(error.execution_disposition(), ExecutionDisposition::Settled);
    assert_eq!(online.snapshot().chat.messages.len(), 1);
    assert!(online.last_parsed_reply().is_none());
    assert!(error_chain_contains::<ReplyParseError>(&error));
    assert_eq!(online.snapshot().chat.status, ChatStatus::Failed);
    assert_eq!(
        online.snapshot().chat.failure.as_deref(),
        Some("send_cancelled")
    );
    assert!(online.take_external_request().is_none());

    // A confirmed local cancellation must not become an unresolved request.
    // A new, explicit turn is allowed; the failed request is never replayed.
    online
        .install_reply_parser(Box::new(moe_chat_app::PlainTextReplyParser))
        .unwrap();
    online
        .dispatch(Action::SetDraft("a new explicit turn".into()), 4)
        .unwrap();
    online.dispatch(Action::Send, 5).unwrap();
    let (_, next_ticket) = take_ticket(&mut online);
    online
        .complete_external(next_ticket, text_outcome("new turn reply"), 6)
        .unwrap();
    assert_eq!(online.snapshot().chat.status, ChatStatus::Ready);
    assert_eq!(online.snapshot().chat.messages.len(), 3);
    assert_eq!(online.snapshot().chat.messages[2].text, "new turn reply");

    let mut offline = ChatApplication::new(93).unwrap();
    offline
        .install_reply_parser(Box::new(FailingParser))
        .unwrap();
    offline.dispatch(Action::Open(PanelKind::Chat), 0).unwrap();
    offline
        .dispatch(Action::SetDraft("offline question".into()), 1)
        .unwrap();
    let error = offline
        .dispatch(Action::Send, 2)
        .expect_err("offline parse failure is reported");
    assert_eq!(error.public_code(), "send_cancelled");
    assert_eq!(error.execution_disposition(), ExecutionDisposition::Settled);
    assert_eq!(offline.snapshot().chat.messages.len(), 1);
    assert_eq!(offline.snapshot().chat.status, ChatStatus::Failed);
    assert!(offline.last_parsed_reply().is_none());
}

fn error_chain_contains<T: Error + 'static>(error: &dyn Error) -> bool {
    let mut source = error.source();
    while let Some(current) = source {
        if current.downcast_ref::<T>().is_some() {
            return true;
        }
        source = current.source();
    }
    false
}

#[derive(Debug)]
struct InjectedStorageFailure;

impl std::fmt::Display for InjectedStorageFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("synthetic checkpoint failure")
    }
}

impl Error for InjectedStorageFailure {}

struct FailableHistory {
    fail: Rc<Cell<bool>>,
    checkpoints: Rc<Cell<usize>>,
    revision: u64,
    active_session_id: String,
    sessions: Vec<HistorySession>,
    unknown_sessions: BTreeSet<String>,
}

impl ChatHistory for FailableHistory {
    fn open_directory(&mut self) -> Result<Option<HistoryRecovery>, AppError> {
        Ok(None)
    }

    fn last_commit_receipts(&self) -> HistoryCommitReceipts {
        HistoryCommitReceipts::default()
    }

    fn session_storage_hint(&self, session_id: &str) -> Result<String, AppError> {
        Ok(session_id.to_owned())
    }

    fn open(
        &mut self,
        _access: HistoryAccess<'_>,
        _requests: &[HistoryRequest],
        expected_sessions_revision: u64,
    ) -> Result<HistoryRecovery, AppError> {
        self.revision = expected_sessions_revision + 1;
        self.active_session_id = String::from("session-1");
        self.sessions = vec![HistorySession {
            id: self.active_session_id.clone(),
            label: String::from("会话 1"),
            archived: false,
            status: ChatStatus::Ready,
        }];
        self.unknown_sessions.clear();
        Ok(self.recovery(Vec::new()))
    }

    fn checkpoint(
        &mut self,
        _access: HistoryAccess<'_>,
        expected_sessions_revision: u64,
    ) -> Result<HistoryCommitReceipts, AppError> {
        assert_eq!(self.revision, expected_sessions_revision);
        self.checkpoints.set(self.checkpoints.get() + 1);
        if self.fail.get() {
            Err(AppError::from_storage(InjectedStorageFailure))
        } else {
            Ok(HistoryCommitReceipts {
                raw: vec![RawHistoryCommitReceipt {
                    revision: self.checkpoints.get() as u64,
                    committed_at_ms: 0,
                    file_synced: true,
                    directory_synced: false,
                }],
                directory: None,
            })
        }
    }

    fn preflight_checkpoint(
        &mut self,
        _access: HistoryAccess<'_>,
        expected_sessions_revision: u64,
        _user_characters: usize,
        _reserved_reply_characters: usize,
    ) -> Result<(), AppError> {
        assert_eq!(self.revision, expected_sessions_revision);
        Ok(())
    }

    fn rotate(
        &mut self,
        current: HistoryAccess<'_>,
        _next: HistoryAccess<'_>,
        expected_sessions_revision: u64,
        change: HistoryChange,
        _send_reservation: Option<moe_chat_app::HistorySendReservation>,
    ) -> Result<HistoryRecovery, AppError> {
        assert_eq!(self.revision, expected_sessions_revision);
        if let Some(source) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == current.logical_session_id)
        {
            source.status = current.status;
        }
        if current.external_unknown {
            self.unknown_sessions
                .insert(current.logical_session_id.to_owned());
        } else {
            self.unknown_sessions.remove(current.logical_session_id);
        }
        match change {
            HistoryChange::Create(session) => {
                self.active_session_id = session.id.clone();
                self.unknown_sessions.remove(&session.id);
                self.sessions.push(session);
            }
            HistoryChange::Switch(id) | HistoryChange::Renew(id) => {
                self.active_session_id = id;
            }
        }
        self.revision += 1;
        Ok(self.recovery(Vec::new()))
    }

    fn archive(
        &mut self,
        _access: HistoryAccess<'_>,
        session_id: &str,
        expected_sessions_revision: u64,
    ) -> Result<HistoryRecovery, AppError> {
        assert_eq!(self.revision, expected_sessions_revision);
        self.sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .unwrap()
            .archived = true;
        self.revision += 1;
        Ok(self.recovery(Vec::new()))
    }

    fn cold_session_ids(&self, _max_sessions: usize) -> Vec<String> {
        Vec::new()
    }

    fn cold_segment_count(&self) -> usize {
        0
    }

    fn load_history(
        &mut self,
        _access: HistoryAccess<'_>,
        _session_id: &str,
        _max_messages: usize,
    ) -> Result<Vec<ChatMessage>, AppError> {
        Ok(Vec::new())
    }

    fn maintain_cold(
        &mut self,
        _access: HistoryAccess<'_>,
        _requests: &[HistoryRequest],
        _max_segments: usize,
    ) -> Result<ColdHistoryMaintenance, AppError> {
        Ok(ColdHistoryMaintenance::default())
    }
}

impl FailableHistory {
    fn recovery(&self, active_history: Vec<ChatMessage>) -> HistoryRecovery {
        HistoryRecovery {
            sessions_revision: self.revision,
            active_session_id: self.active_session_id.clone(),
            active_storage_hint: self.active_session_id.clone(),
            sessions: self.sessions.clone(),
            active_external_unknown: self.unknown_sessions.contains(&self.active_session_id),
            active_recovered_unsettled: false,
            active_history,
            receipts: Default::default(),
        }
    }
}

fn app_with_failable_history(
    generation: u64,
) -> (ChatApplication, Rc<Cell<bool>>, Rc<Cell<usize>>) {
    let mut app = ChatApplication::new(generation).unwrap();
    let fail = Rc::new(Cell::new(false));
    let checkpoints = Rc::new(Cell::new(0));
    app.enable_history(
        Box::new(FailableHistory {
            fail: fail.clone(),
            checkpoints: checkpoints.clone(),
            revision: 0,
            active_session_id: String::new(),
            sessions: Vec::new(),
            unknown_sessions: BTreeSet::new(),
        }),
        RawRetention::new(30 * 86_400_000, 7 * 86_400_000, 8, 256, 65_536).unwrap(),
        0,
    )
    .unwrap();
    app.dispatch(Action::Open(PanelKind::Chat), 0).unwrap();
    app.configure_model(&ModelSettings {
        endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
        model: "synthetic".into(),
        persona: "synthetic persona".into(),
        allow_transmission: true,
    })
    .unwrap();
    app.dispatch(Action::SetDraft("synthetic question".into()), 1)
        .unwrap();
    (app, fail, checkpoints)
}

#[test]
fn lifecycle_actions_route_through_the_history_contract() {
    let (mut app, _, _) = app_with_failable_history(309);
    let before = app.snapshot();
    let (expected_session_id, expected_sessions_revision) = session_guard(&app);
    app.dispatch(
        Action::NewSession {
            expected_session_id: expected_session_id.clone(),
            expected_sessions_revision,
        },
        2,
    )
    .unwrap();
    let active = app.snapshot();
    assert_eq!(active.active_session_id.as_deref(), Some("session-2"));
    assert_eq!(active.sessions_revision, expected_sessions_revision + 1);

    app.dispatch(
        Action::ArchiveSession {
            id: String::from("session-2"),
            expected_session_id: String::from("session-2"),
            expected_sessions_revision: active.sessions_revision,
        },
        3,
    )
    .unwrap();
    let archived = app.snapshot();
    assert!(
        archived
            .sessions
            .iter()
            .find(|s| s.id == "session-2")
            .unwrap()
            .archived
    );

    app.dispatch(
        Action::SwitchSession {
            id: expected_session_id,
            expected_session_id: String::from("session-2"),
            expected_sessions_revision: archived.sessions_revision,
        },
        4,
    )
    .unwrap();
    assert_eq!(
        app.snapshot().active_session_id.as_deref(),
        Some("session-1")
    );
    assert_eq!(
        app.snapshot().sessions_revision,
        before.sessions_revision + 3
    );
}

#[test]
fn retention_is_explicit_and_a_failed_pre_checkpoint_maintenance_never_writes() {
    let (mut app, _, checkpoints) = app_with_failable_history(310);
    let report = app
        .maintain_retention(10)
        .expect("existing retention service is reachable")
        .expect("history-enabled app has configured retention");
    assert_eq!(report.live.pending_transfer, 0);
    assert_eq!(report.cold_segments_processed, 0);

    app.dispatch(Action::Send, 11).unwrap();
    let (_, ticket) = take_ticket(&mut app);
    let before = checkpoints.get();
    let error = app
        .complete_external(ticket, text_outcome("must not checkpoint"), 9)
        .expect_err("clock regression makes the local result unknown");
    assert_eq!(error.execution_disposition(), ExecutionDisposition::Unknown);
    assert_eq!(checkpoints.get(), before);
    assert_eq!(app.snapshot().chat.status, ChatStatus::Unknown);
    assert!(!app.snapshot().session_isolation_allowed);
    assert_eq!(
        app.snapshot().chat.failure.as_deref(),
        Some("storage_failed")
    );
}

#[test]
fn maintenance_failure_keeps_the_snapshot_unknown_and_blocks_isolation() {
    let (mut app, _, checkpoints) = app_with_failable_history(311);
    app.maintain_retention(10).unwrap();
    let before = checkpoints.get();
    let error = app
        .maintain_retention(9)
        .expect_err("trusted maintenance rejects a clock rollback");
    assert_eq!(error.execution_disposition(), ExecutionDisposition::Unknown);
    assert_eq!(checkpoints.get(), before);
    let snapshot = app.snapshot();
    assert_eq!(snapshot.chat.status, ChatStatus::Unknown);
    assert_eq!(snapshot.chat.failure.as_deref(), Some("storage_failed"));
    assert!(!snapshot.session_isolation_allowed);
}

#[test]
fn rejected_send_does_not_checkpoint_a_previous_committed_turn() {
    let (mut subject, fail, checkpoints) = app_with_failable_history(304);
    subject.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut subject);
    subject
        .complete_external(ticket, text_outcome("previous reply"), 3)
        .unwrap();
    subject
        .dispatch(Action::SetDraft(String::new()), 4)
        .unwrap();
    let before = checkpoints.get();
    fail.set(true);

    let error = subject.dispatch(Action::Send, 5).unwrap_err();
    assert_eq!(error.public_code(), "empty_input");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    assert!(error.retained_error().is_none());
    assert_eq!(checkpoints.get(), before);
    assert_eq!(subject.snapshot().chat.status, ChatStatus::Ready);
    assert!(subject.take_external_request().is_none());
}

#[test]
fn rejected_completion_does_not_checkpoint_or_quarantine_the_inflight_request() {
    let (mut subject, fail, checkpoints) = app_with_failable_history(301);
    subject.dispatch(Action::Send, 2).unwrap();
    let after_send = checkpoints.get();
    assert!(after_send > 0, "the accepted Send is checkpointed");
    let (_, ticket) = take_ticket(&mut subject);

    let mut other = app(true);
    other.dispatch(Action::Send, 2).unwrap();
    let (_, foreign_ticket) = take_ticket(&mut other);

    // A rejected callback would trigger a failing checkpoint and a display
    // refresh in the old flow, quarantining the correct request's conversation.
    let renders_before = subject
        .snapshot()
        .panels
        .iter()
        .find(|panel| panel.kind == PanelKind::Chat)
        .expect("chat panel is registered")
        .renders;
    fail.set(true);
    let error = subject
        .complete_external(foreign_ticket, text_outcome("foreign reply"), 3)
        .expect_err("a foreign ticket is rejected");
    assert_eq!(error.public_code(), "stale_external_ticket");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    assert_eq!(
        checkpoints.get(),
        after_send,
        "a rejected ticket must not run storage side effects"
    );
    let renders_after = subject
        .snapshot()
        .panels
        .iter()
        .find(|panel| panel.kind == PanelKind::Chat)
        .expect("chat panel is registered")
        .renders;
    assert_eq!(
        renders_after, renders_before,
        "a rejected ticket must not refresh the display"
    );
    assert_eq!(subject.snapshot().chat.status, ChatStatus::Sending);
    assert_eq!(subject.snapshot().chat.messages.len(), 1);
    let waiting = subject.chat_progress().expect("still waiting");
    assert_eq!(waiting.status, WorkflowStatus::Running);
    assert_eq!(
        step_status(&subject, "request_wait"),
        WorkflowStepStatus::Running
    );

    fail.set(false);
    subject
        .complete_external(ticket, text_outcome("real reply"), 4)
        .expect("the correct in-flight request still completes");
    assert_eq!(subject.snapshot().chat.status, ChatStatus::Ready);
    assert_eq!(subject.snapshot().chat.messages.len(), 2);
    assert_eq!(subject.snapshot().chat.messages[1].text, "real reply");
    assert!(checkpoints.get() > after_send);
    assert!(subject.chat_progress().unwrap().published);
}

#[test]
fn accepted_callback_retains_storage_and_display_failures_independently() {
    let (mut app, fail, checkpoints) = app_with_failable_history(302);
    app.dispatch(Action::Send, 2).unwrap();
    let after_send = checkpoints.get();
    let (_, ticket) = take_ticket(&mut app);
    let duplicate = ticket.clone();
    app.dispatch(Action::Hide(PanelKind::Chat), 3).unwrap();

    fail.set(true);
    let error = app
        .complete_external(ticket, text_outcome("synthetic reply"), 4)
        .expect_err("storage and display both fail");
    assert_eq!(error.public_code(), "storage_failed");
    assert_eq!(error.execution_disposition(), ExecutionDisposition::Unknown);
    let retained = error
        .retained_error()
        .expect("the display failure is retained next to the storage failure");
    assert_eq!(retained.public_code(), "display_failed");
    assert!(
        error_chain_contains::<InjectedStorageFailure>(&error),
        "the adapter error chain is preserved"
    );
    assert_eq!(checkpoints.get(), after_send + 1);

    let progress = app.chat_progress().expect("committed turn progress");
    assert!(progress.committed);
    assert!(!progress.published);
    assert_eq!(progress.business_outcome, Some(BusinessOutcome::Completed));
    assert_eq!(progress.status, WorkflowStatus::Failed);
    assert_eq!(step_status(&app, "commit"), WorkflowStepStatus::Succeeded);
    assert_eq!(step_status(&app, "publish"), WorkflowStepStatus::Failed);
    // Actual display facade never received the turn.
    assert!(app.snapshot().chat.messages.is_empty());

    let error = app
        .complete_external(duplicate, text_outcome("must not re-register"), 5)
        .expect_err("the turn is already terminal");
    assert_eq!(error.public_code(), "no_request");

    // Restoring the view reads the committed projection; it does not re-run the
    // flow or flip the terminated publish node.
    app.dispatch(Action::Open(PanelKind::Chat), 6).unwrap();
    let snapshot = app.snapshot();
    assert_eq!(snapshot.chat.messages.len(), 2);
    assert_eq!(snapshot.chat.messages[1].text, "synthetic reply");
    let restored = app.chat_progress().expect("progress survives restore");
    assert_eq!(
        restored.turn, progress.turn,
        "restore must not start a turn"
    );
    assert_eq!(restored.status, WorkflowStatus::Failed);
    assert!(restored.committed && !restored.published);
    assert!(app.take_external_request().is_none());
    assert_eq!(step_status(&app, "publish"), WorkflowStepStatus::Failed);
}

#[test]
fn external_completion_never_invents_transport_evidence() {
    for (outcome, label) in [
        (text_outcome("synthetic reply"), "text"),
        (ModelOutcome::Cancelled, "cancelled"),
        (ModelOutcome::Failed(ModelFailure::Unavailable), "failed"),
    ] {
        let mut app = app(true);
        app.dispatch(Action::Open(PanelKind::Diagnostics), 1)
            .unwrap();
        app.dispatch(Action::Send, 2).unwrap();
        let (_, ticket) = take_ticket(&mut app);
        app.complete_external(ticket, outcome, 3).unwrap();
        app.dispatch(Action::RefreshDiagnostics, 4).unwrap();
        let entries = app.snapshot().diagnostics;
        assert!(
            entries
                .iter()
                .any(|entry| entry.event == "run.transport.unknown"),
            "{label}: transport state must stay explicitly unknown"
        );
        for forbidden in [
            "run.transport.written",
            "run.transport.failed",
            "run.transport.cancelled",
        ] {
            assert!(
                !entries.iter().any(|entry| entry.event == forbidden),
                "{label}: {forbidden} has no transport receipt at the application boundary"
            );
        }
    }
}

#[test]
fn revocation_and_memory_revision_reject_late_output() {
    let mut revoked = app(true);
    revoked.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut revoked);
    revoked.dispatch(Action::Revoke, 3).unwrap();
    let error = revoked
        .complete_external(ticket, text_outcome("late"), 4)
        .expect_err("revoked completion is rejected");
    assert_eq!(error.public_code(), "revoked");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    assert!(revoked.snapshot().chat.messages.is_empty());
    assert!(revoked.snapshot().active_session_id.is_none());
    assert!(revoked.snapshot().sessions.is_empty());

    let mut changed = app(true);
    changed
        .dispatch(Action::Open(PanelKind::Memory), 1)
        .unwrap();
    changed
        .dispatch(Action::Remember("synthetic note".into()), 1)
        .unwrap();
    changed.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut changed);
    let note = changed.snapshot().memories[0].clone();
    changed
        .dispatch(
            Action::UpdateMemory {
                id: note.id,
                revision: note.revision,
                text: "corrected note".into(),
            },
            3,
        )
        .unwrap();
    let error = changed
        .complete_external(ticket, text_outcome("stale reply"), 4)
        .expect_err("memory revision change rejects late output");
    assert_eq!(error.public_code(), "send_cancelled");
    assert_eq!(error.execution_disposition(), ExecutionDisposition::Settled);
    assert_eq!(changed.snapshot().chat.messages.len(), 1);
    assert_eq!(changed.snapshot().chat.status, ChatStatus::Failed);
    assert_eq!(
        changed.snapshot().chat.failure.as_deref(),
        Some("send_cancelled")
    );
    assert!(changed.take_external_request().is_none());
}

#[test]
fn preset_rebinding_during_the_wait_rejects_late_output() {
    let mut app = app(true);
    app.dispatch(Action::Open(PanelKind::Presets), 1).unwrap();
    app.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut app);
    app.dispatch(Action::SelectPreset("daily-chat".into()), 3)
        .unwrap();
    let error = app
        .complete_external(ticket, text_outcome("late reply"), 4)
        .expect_err("preset configuration change rejects late output");
    assert_eq!(error.public_code(), "send_cancelled");
    assert_eq!(error.execution_disposition(), ExecutionDisposition::Settled);
    assert_eq!(app.snapshot().chat.messages.len(), 1);
    assert_eq!(app.snapshot().chat.status, ChatStatus::Failed);
    assert_eq!(
        app.snapshot().chat.failure.as_deref(),
        Some("send_cancelled")
    );
    assert!(app.take_external_request().is_none());
}

#[test]
fn repeated_rejections_are_deterministic() {
    let mut app = app(true);
    app.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut app);
    let duplicate = ticket.clone();
    app.complete_external(ticket, text_outcome("synthetic reply"), 3)
        .unwrap();
    let stale_ticket = duplicate.clone();
    let first = app
        .complete_external(duplicate.clone(), ModelOutcome::Unknown, 4)
        .unwrap_err();
    let second = app
        .complete_external(duplicate, ModelOutcome::Unknown, 5)
        .unwrap_err();
    assert_eq!(first.public_code(), "no_request");
    assert_eq!(first.public_code(), second.public_code());
    assert_eq!(
        first.execution_disposition(),
        second.execution_disposition()
    );
    assert_eq!(
        first.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );

    app.dispatch(Action::SetDraft("again".into()), 6).unwrap();
    app.dispatch(Action::Send, 7).unwrap();
    let (_, second_ticket) = take_ticket(&mut app);
    let stale_first = app
        .complete_external(stale_ticket.clone(), text_outcome("late"), 8)
        .unwrap_err();
    let stale_second = app
        .complete_external(stale_ticket, text_outcome("late"), 9)
        .unwrap_err();
    assert_eq!(stale_first.public_code(), "stale_external_ticket");
    assert_eq!(stale_first.public_code(), stale_second.public_code());
    assert_eq!(
        stale_first.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    assert_eq!(app.snapshot().chat.status, ChatStatus::Sending);
    app.complete_external(second_ticket, text_outcome("reply"), 10)
        .unwrap();
    assert_eq!(app.snapshot().chat.status, ChatStatus::Ready);
}

#[test]
fn display_publish_failure_keeps_the_commit_but_does_not_claim_published() {
    let mut app = app(true);
    app.dispatch(Action::Send, 2).unwrap();
    let (_, ticket) = take_ticket(&mut app);
    let duplicate = ticket.clone();
    app.dispatch(Action::Hide(PanelKind::Chat), 3).unwrap();
    let error = app
        .complete_external(ticket, text_outcome("synthetic reply"), 4)
        .expect_err("hidden panel reports a display failure");
    assert_eq!(error.public_code(), "display_failed");
    assert_eq!(error.execution_disposition(), ExecutionDisposition::Settled);

    // The business commit is authoritative and survives independently.
    let progress = app.chat_progress().expect("completed turn progress");
    assert!(progress.committed);
    assert_eq!(progress.business_outcome, Some(BusinessOutcome::Completed));
    assert!(
        !progress.published,
        "the display facade never accepted the reply"
    );
    assert_eq!(progress.status, WorkflowStatus::Failed);
    assert_eq!(step_status(&app, "commit"), WorkflowStepStatus::Succeeded);
    assert_eq!(
        step_status(&app, "publish"),
        WorkflowStepStatus::Failed,
        "publish covers the real display result, not an internal projection"
    );
    // Actual display state: nothing was published to the hidden facade.
    assert!(app.snapshot().chat.messages.is_empty());
    assert!(app.take_external_request().is_none());

    let error = app
        .complete_external(duplicate, text_outcome("must not re-register"), 5)
        .expect_err("the completed turn has no pending request");
    assert_eq!(error.public_code(), "no_request");
    assert!(app.snapshot().chat.messages.is_empty());

    // Reopening restores the projection without resending, re-registering or
    // reversing the already terminated flow.
    app.dispatch(Action::Open(PanelKind::Chat), 6).unwrap();
    let snapshot = app.snapshot();
    assert_eq!(snapshot.chat.messages.len(), 2);
    assert_eq!(snapshot.chat.messages[1].text, "synthetic reply");
    assert_eq!(snapshot.chat.status, ChatStatus::Ready);
    let restored = app.chat_progress().expect("progress survives restore");
    assert_eq!(restored.status, WorkflowStatus::Failed);
    assert!(restored.committed && !restored.published);
    assert_eq!(
        step_status(&app, "publish"),
        WorkflowStepStatus::Failed,
        "restoring the view cannot rewrite the terminated flow"
    );
}

#[test]
fn disposition_is_recorded_at_operation_boundaries() {
    let error = ChatApplication::new(0).err().unwrap();
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );

    let mut app = app(true);
    app.dispatch(Action::SetDraft(String::new()), 1).unwrap();
    let error = app.dispatch(Action::Send, 2).unwrap_err();
    assert_eq!(error.public_code(), "empty_input");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );

    let mut closed = ChatApplication::new(94).unwrap();
    let error = closed.dispatch(Action::Send, 0).unwrap_err();
    assert_eq!(error.public_code(), "panel_not_open");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );

    let mut presets = ChatApplication::new(95).unwrap();
    presets
        .dispatch(Action::Open(PanelKind::Presets), 0)
        .unwrap();
    let error = presets
        .dispatch(Action::SelectPreset("missing".into()), 1)
        .unwrap_err();
    assert_eq!(error.public_code(), "unknown_preset");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
}

#[test]
fn parser_replacement_is_refused_while_a_request_is_pending() {
    let mut app = app(true);
    app.dispatch(Action::Send, 2).unwrap();
    let error = app
        .install_reply_parser(Box::new(moe_chat_app::PlainTextReplyParser))
        .unwrap_err();
    assert_eq!(error.public_code(), "request_pending");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    let (_, ticket) = take_ticket(&mut app);
    let error = app
        .install_reply_parser(Box::new(moe_chat_app::PlainTextReplyParser))
        .unwrap_err();
    assert_eq!(error.public_code(), "request_pending");
    assert_eq!(
        error.execution_disposition(),
        ExecutionDisposition::NotExecuted
    );
    app.complete_external(ticket, text_outcome("synthetic reply"), 3)
        .unwrap();
    app.install_reply_parser(Box::new(moe_chat_app::PlainTextReplyParser))
        .expect("replacement allowed between requests");
    assert_eq!(app.snapshot().chat.status, ChatStatus::Ready);
}
