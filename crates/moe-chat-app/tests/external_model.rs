use moe_agent_runtime::{ModelFailure, ModelOutcome};
use moe_chat_app::{Action, ChatApplication, ChatStatus, ModelSettings, PanelKind};

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

#[test]
fn explicit_permission_and_single_dispatch_are_required() {
    let mut denied = app(false);
    assert!(denied.dispatch(Action::Send, 2).is_err());
    assert!(denied.take_external_request().is_none());
    assert!(!denied.snapshot().external_transmission_allowed);
    let mut allowed = app(true);
    allowed.dispatch(Action::Send, 2).unwrap();
    assert_eq!(allowed.snapshot().chat.status, ChatStatus::Sending);
    let input = allowed.take_external_request().unwrap();
    assert_eq!(input.persona(), "synthetic persona");
    assert!(
        input
            .conversation()
            .iter()
            .any(|event| event.body() == "synthetic question")
    );
    assert!(allowed.take_external_request().is_none());
    allowed
        .complete_external(
            ModelOutcome::Text {
                text: "synthetic reply".into(),
                presentation: vec![],
            },
            3,
        )
        .unwrap();
    assert_eq!(allowed.snapshot().chat.messages.len(), 2);
    assert_eq!(allowed.snapshot().chat.status, ChatStatus::Ready);
    assert!(allowed.complete_external(ModelOutcome::Unknown, 4).is_err());
}

#[test]
fn unknown_is_not_retried_and_known_failure_is_visible() {
    let mut app = app(true);
    app.dispatch(Action::Send, 2).unwrap();
    app.take_external_request().unwrap();
    app.complete_external(ModelOutcome::Unknown, 3).unwrap();
    app.dispatch(Action::SetDraft("do not replay".into()), 4)
        .unwrap();
    assert!(app.dispatch(Action::Send, 5).is_err());
    assert!(app.take_external_request().is_none());
    assert_eq!(app.snapshot().chat.status, ChatStatus::Unknown);
    let mut failed = self::app(true);
    failed.dispatch(Action::Send, 2).unwrap();
    failed.take_external_request().unwrap();
    failed
        .complete_external(ModelOutcome::Failed(ModelFailure::Unavailable), 3)
        .unwrap();
    assert_eq!(failed.snapshot().chat.status, ChatStatus::Failed);
    assert_eq!(failed.snapshot().chat.draft, "synthetic question");
}

#[test]
fn late_failure_does_not_replace_a_new_draft() {
    let mut app = app(true);
    app.dispatch(Action::Send, 2).unwrap();
    app.take_external_request().unwrap();
    app.dispatch(Action::SetDraft("next message".into()), 3)
        .unwrap();
    app.complete_external(ModelOutcome::Failed(ModelFailure::Unavailable), 4)
        .unwrap();
    assert_eq!(app.snapshot().chat.draft, "next message");
}

#[test]
fn revocation_and_memory_revision_reject_late_output() {
    let mut revoked = app(true);
    revoked.dispatch(Action::Send, 2).unwrap();
    revoked.take_external_request().unwrap();
    revoked.dispatch(Action::Revoke, 3).unwrap();
    assert!(
        revoked
            .complete_external(
                ModelOutcome::Text {
                    text: "late".into(),
                    presentation: vec![]
                },
                4
            )
            .is_err()
    );
    assert!(revoked.snapshot().chat.messages.is_empty());
    let mut changed = app(true);
    changed
        .dispatch(Action::Open(PanelKind::Memory), 1)
        .unwrap();
    changed
        .dispatch(Action::Remember("synthetic note".into()), 1)
        .unwrap();
    changed.dispatch(Action::Send, 2).unwrap();
    changed.take_external_request().unwrap();
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
    assert!(
        changed
            .complete_external(
                ModelOutcome::Text {
                    text: "stale reply".into(),
                    presentation: vec![]
                },
                4
            )
            .is_err()
    );
    assert_eq!(changed.snapshot().chat.messages.len(), 1);
    assert_eq!(changed.snapshot().chat.status, ChatStatus::Unknown);
}
