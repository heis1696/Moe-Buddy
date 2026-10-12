use moe_agent_runtime::*;
use moe_memory_core::{Namespace, PetIdentity};

fn identity(workspace: &str, user: &str, pet: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new(workspace, user).unwrap(), pet).unwrap()
}

#[test]
fn complete_identity_and_every_ingress_dimension_remain_isolated_in_staging() {
    let (mut ingress, _, _) = fixture();
    for (alias, pet) in [
        ("workspace", identity("different", "owner", "pet")),
        ("owner", identity("workspace", "different", "pet")),
    ] {
        ingress.register_route(alias, pet).unwrap();
    }
    for (token, subject, channel, scope) in [
        ("same-dimensions", "caller", "api", "private"),
        ("other-subject", "other", "api", "private"),
        ("other-channel", "caller", "desktop", "private"),
        ("group-a", "caller", "api", "group-a"),
        ("group-b", "caller", "api", "group-b"),
    ] {
        install(&mut ingress, token, subject, channel, scope, token);
    }
    ingress
        .install_credential(
            IngressToken::new("full-owner").unwrap(),
            CallerSubject::new("caller").unwrap(),
            [
                "buddy".to_owned(),
                "other".to_owned(),
                "workspace".to_owned(),
                "owner".to_owned(),
            ],
            ChannelId::new("api").unwrap(),
            ChannelScope::new("private").unwrap(),
            ReplyTarget::new("full-target").unwrap(),
        )
        .unwrap();
    let mut runtime = SessionRuntime::new(&ingress, limits(), SessionTiming::default(), 0);
    let mut requests = Vec::new();
    for token in [
        "synthetic-token",
        "same-dimensions",
        "other-subject",
        "other-channel",
        "group-a",
        "group-b",
    ] {
        requests.push(
            ingress
                .admit(
                    RawIngressCall::new(IngressToken::new(token).unwrap(), "buddy", "same raw")
                        .with_session_hint("same-hint"),
                )
                .unwrap(),
        );
    }
    for alias in ["buddy", "other", "workspace", "owner"] {
        requests.push(
            ingress
                .admit(
                    RawIngressCall::new(
                        IngressToken::new("full-owner").unwrap(),
                        alias,
                        "same raw",
                    )
                    .with_session_hint("same-hint"),
                )
                .unwrap(),
        );
    }
    let registrations: Vec<_> = requests
        .iter()
        .map(|request| {
            runtime
                .register_user(
                    access(&ingress, request, 0),
                    UserEvent::new(UserEventKind::New)
                        .with_id(HostEventId::new("same-host-id").unwrap()),
                )
                .unwrap()
        })
        .collect();
    for (first_index, first) in registrations.iter().enumerate() {
        for (second_index, second) in registrations.iter().enumerate() {
            if first_index != second_index {
                assert_ne!(first.session, second.session);
                assert_eq!(
                    runtime
                        .read_active(access(&ingress, &requests[second_index], 0), &first.session)
                        .unwrap_err(),
                    GatewayError::Revoked
                );
                assert!(
                    runtime
                        .finish_interaction(
                            access(&ingress, &requests[second_index], 0),
                            &first.interaction,
                            TerminalState::Completed,
                            None
                        )
                        .is_err()
                );
            }
        }
    }
}

#[test]
fn hintless_admissions_never_merge_even_with_identical_text_client_and_host_ids() {
    let (mut ingress, mut runtime, _) = fixture();
    let mut sessions = Vec::new();
    for _ in 0..2 {
        let request = ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new("synthetic-token").unwrap(),
                    "buddy",
                    "same raw",
                )
                .with_client_request_id("same-client"),
            )
            .unwrap();
        sessions.push(
            runtime
                .register_user(
                    access(&ingress, &request, 0),
                    UserEvent::new(UserEventKind::New)
                        .with_id(HostEventId::new("same-source").unwrap()),
                )
                .unwrap()
                .session,
        );
    }
    assert_ne!(sessions[0], sessions[1]);
}

#[test]
fn original_reply_target_does_not_follow_newest_channel_or_fragment() {
    let (mut ingress, _, _) = fixture();
    install(
        &mut ingress,
        "desktop-token",
        "caller",
        "desktop",
        "private",
        "desktop-target",
    );
    let mut runtime = SessionRuntime::new(&ingress, limits(), SessionTiming::default(), 0);
    let original = call(&mut ingress, "api request");
    let registered = register(&mut runtime, &ingress, &original, 0);
    let desktop = ingress
        .admit(
            RawIngressCall::new(
                IngressToken::new("desktop-token").unwrap(),
                "buddy",
                "desktop request",
            )
            .with_session_hint("conversation"),
        )
        .unwrap();
    let desktop_registered = register(&mut runtime, &ingress, &desktop, 1_800_000);
    let next = call(&mut ingress, "new fragment");
    let next_registration = register(&mut runtime, &ingress, &next, 1_800_000);
    assert_ne!(registered.session, next_registration.session);
    let receipt = runtime
        .finish_interaction(
            access(&ingress, &original, 1_800_001),
            &registered.interaction,
            TerminalState::Completed,
            Some("actual old reply"),
        )
        .unwrap();
    assert_eq!(receipt.reply_target.as_str(), "original");
    assert_eq!(
        runtime
            .read_active(access(&ingress, &original, 1_800_001), &registered.session)
            .unwrap_err(),
        GatewayError::SessionExpired
    );
    assert_eq!(
        runtime
            .read_active(
                access(&ingress, &next, 1_800_001),
                &next_registration.session
            )
            .unwrap()
            .events
            .len(),
        1
    );
    assert_eq!(
        runtime
            .read_active(
                access(&ingress, &desktop, 1_800_001),
                &desktop_registered.session
            )
            .unwrap()
            .events
            .len(),
        1
    );
}

#[test]
fn interaction_deadline_is_strict_but_expiration_alone_does_not_cancel_authorized_tasks() {
    let (mut ingress, mut runtime, _) =
        fixture_with(limits(), SessionTiming::new(10, 180_000, 20, 100).unwrap());
    let request = call(&mut ingress, "raw");
    let registration = register(&mut runtime, &ingress, &request, 0);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 10), &registration.session)
            .unwrap_err(),
        GatewayError::SessionExpired
    );
    runtime
        .append_related(
            access(&ingress, &request, 19),
            &registration.interaction,
            RelatedEvent::new(EventRole::Assistant, "actual reply"),
        )
        .unwrap();
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &request, 20),
                &registration.interaction,
                TerminalState::Completed,
                None
            )
            .unwrap_err(),
        GatewayError::DeadlineExceeded
    );
    assert_eq!(
        runtime
            .append_related(
                access(&ingress, &request, 20),
                &registration.interaction,
                RelatedEvent::new(EventRole::Background, "late")
            )
            .unwrap_err(),
        GatewayError::DeadlineExceeded
    );
    runtime
        .cancel_interaction(access(&ingress, &request, 20), &registration.interaction)
        .unwrap();
    assert!(
        runtime
            .issue_idle_batch(
                access(&ingress, &request, 180_000),
                &registration.session,
                true
            )
            .is_ok()
    );
}

#[test]
fn cancellation_is_idempotent_and_prevents_all_late_task_output() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "raw");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let cancelled = runtime
        .cancel_interaction(access(&ingress, &request, 1), &registration.interaction)
        .unwrap();
    assert_eq!(
        runtime
            .cancel_interaction(access(&ingress, &request, 2), &registration.interaction)
            .unwrap(),
        cancelled
    );
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &request, 3),
                &registration.interaction,
                TerminalState::Completed,
                Some("late")
            )
            .unwrap_err(),
        GatewayError::InteractionClosed
    );
    assert_eq!(
        runtime
            .append_related(
                access(&ingress, &request, 3),
                &registration.interaction,
                RelatedEvent::new(EventRole::Assistant, "late")
            )
            .unwrap_err(),
        GatewayError::InteractionClosed
    );
    let snapshot = runtime
        .read_active(access(&ingress, &request, 3), &registration.session)
        .unwrap();
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(
        snapshot.events[0].terminal(),
        Some(TerminalState::Cancelled)
    );
    assert_eq!(snapshot.active_until_ms, Some(1_800_000));
}

#[test]
fn session_revocation_blocks_read_and_write_and_never_resurrects_old_ids() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "raw");
    let input = UserEvent::new(UserEventKind::New).with_id(HostEventId::new("source").unwrap());
    let original = runtime
        .register_user(access(&ingress, &request, 0), input.clone())
        .unwrap();
    finish(&mut runtime, &ingress, &request, &original.interaction, 0);
    let ticket = runtime
        .issue_idle_batch(access(&ingress, &request, 180_000), &original.session, true)
        .unwrap();
    let event = runtime
        .read_batch(access(&ingress, &request, 180_000), &ticket)
        .unwrap()
        .remove(0);
    runtime
        .revoke_session(access(&ingress, &request, 180_001), &original.session)
        .unwrap();
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 180_001), &original.session)
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &request, 180_001), &ticket)
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &request, 180_001),
                &ticket,
                event.id(),
                BatchAcknowledgement::Confirmed
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &request, 180_001),
                &original.interaction,
                TerminalState::Completed,
                None
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    let duplicate = call(&mut ingress, "raw");
    assert_eq!(
        runtime
            .register_user(access(&ingress, &duplicate, 180_001), input)
            .unwrap_err(),
        GatewayError::Revoked
    );
    let new = call(&mut ingress, "new source");
    assert_ne!(
        register(&mut runtime, &ingress, &new, 180_001).session,
        original.session
    );
}

#[test]
fn credential_revocation_is_revalidated_on_every_operation() {
    let (mut ingress, mut runtime, credential) = fixture();
    let request = call(&mut ingress, "raw");
    let original = register(&mut runtime, &ingress, &request, 0);
    finish(&mut runtime, &ingress, &request, &original.interaction, 0);
    let ticket = runtime
        .issue_idle_batch(access(&ingress, &request, 180_000), &original.session, true)
        .unwrap();
    let event = runtime
        .read_batch(access(&ingress, &request, 180_000), &ticket)
        .unwrap()
        .remove(0);
    ingress.revoke_credential(credential).unwrap();
    assert_eq!(
        runtime
            .register_user(
                access(&ingress, &request, 180_000),
                UserEvent::new(UserEventKind::New)
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 180_000), &original.session)
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .append_related(
                access(&ingress, &request, 180_000),
                &original.interaction,
                RelatedEvent::new(EventRole::Assistant, "late")
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &request, 180_000),
                &original.interaction,
                TerminalState::Completed,
                None
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .cancel_interaction(access(&ingress, &request, 180_000), &original.interaction)
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .revoke_session(access(&ingress, &request, 180_000), &original.session)
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .issue_idle_batch(access(&ingress, &request, 180_000), &original.session, true)
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &request, 180_000), &ticket)
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &request, 180_000),
                &ticket,
                event.id(),
                BatchAcknowledgement::Confirmed
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .cancel_batch(access(&ingress, &request, 180_000), &ticket)
            .unwrap_err(),
        GatewayError::Revoked
    );
}

#[test]
fn configuration_change_requires_new_fragment_and_rejects_old_source_replay() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "raw");
    let input = UserEvent::new(UserEventKind::New).with_id(HostEventId::new("old-source").unwrap());
    let original = runtime
        .register_user(access(&ingress, &request, 0), input.clone())
        .unwrap();
    finish(&mut runtime, &ingress, &request, &original.interaction, 0);
    let ticket = runtime
        .issue_idle_batch(access(&ingress, &request, 180_000), &original.session, true)
        .unwrap();
    ingress.unregister_route("buddy").unwrap();
    ingress
        .register_route("buddy", identity("workspace", "owner", "pet"))
        .unwrap();
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &request, 180_001), &ticket)
            .unwrap_err(),
        GatewayError::Revoked
    );
    let repeated = call(&mut ingress, "raw");
    assert_eq!(
        runtime
            .register_user(access(&ingress, &repeated, 180_001), input)
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &repeated, 180_001), &original.session)
            .unwrap_err(),
        GatewayError::Revoked
    );
    let new = call(&mut ingress, "new");
    assert_ne!(
        register(&mut runtime, &ingress, &new, 180_001).session,
        original.session
    );
}

#[test]
fn concurrent_and_rebuilt_runtimes_reject_foreign_handles_even_with_same_ingress() {
    let (mut ingress, mut first, _) = fixture();
    let request = call(&mut ingress, "raw");
    let original = register(&mut first, &ingress, &request, 0);
    finish(&mut first, &ingress, &request, &original.interaction, 0);
    let ticket = first
        .issue_idle_batch(access(&ingress, &request, 180_000), &original.session, true)
        .unwrap();
    let mut second = SessionRuntime::new(&ingress, limits(), SessionTiming::default(), 0);
    let other = register(&mut second, &ingress, &request, 0);
    assert_ne!(original.session, other.session);
    assert_eq!(
        second
            .read_active(access(&ingress, &request, 0), &original.session)
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    assert_eq!(
        second
            .read_batch(access(&ingress, &request, 0), &ticket)
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    assert_eq!(
        second
            .finish_interaction(
                access(&ingress, &request, 0),
                &original.interaction,
                TerminalState::Completed,
                None
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    drop(first);
    let mut rebuilt = SessionRuntime::new(&ingress, limits(), SessionTiming::default(), 0);
    assert_eq!(
        rebuilt
            .read_active(access(&ingress, &request, 0), &original.session)
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    assert_eq!(
        rebuilt
            .read_batch(access(&ingress, &request, 0), &ticket)
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
}

#[test]
fn restart_drops_all_active_state_and_does_not_compare_old_process_time() {
    let (mut old_ingress, mut old_runtime, _) = fixture();
    let old_request = call(&mut old_ingress, "old raw");
    let old = register(&mut old_runtime, &old_ingress, &old_request, 9_000_000);
    let (mut new_ingress, mut new_runtime, _) = fixture();
    assert_eq!(
        new_runtime
            .register_user(
                access(&old_ingress, &old_request, 9_000_000),
                UserEvent::new(UserEventKind::New)
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    assert_eq!(
        new_runtime
            .register_user(
                access(&new_ingress, &old_request, 0),
                UserEvent::new(UserEventKind::New)
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    let request = call(&mut new_ingress, "new raw");
    assert_eq!(
        new_runtime
            .read_active(access(&new_ingress, &request, 0), &old.session)
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    let new = register(&mut new_runtime, &new_ingress, &request, 0);
    let snapshot = new_runtime
        .read_active(access(&new_ingress, &request, 0), &new.session)
        .unwrap();
    assert_eq!(snapshot.active_until_ms, Some(1_800_000));
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.events[0].body(), "new raw");
}

#[test]
fn deadline_addition_overflow_never_publishes_a_session_or_advances_time() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "raw");
    assert_eq!(
        runtime
            .register_user(
                access(&ingress, &request, u64::MAX),
                UserEvent::new(UserEventKind::New)
            )
            .unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert!(register(&mut runtime, &ingress, &request, 0).renewed);
}

#[test]
fn session_debug_redacts_bodies_host_ids_and_client_hints() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "sensitive-body");
    let registration = runtime
        .register_user(
            access(&ingress, &request, 0),
            UserEvent::new(UserEventKind::New).with_id(HostEventId::new("sensitive-id").unwrap()),
        )
        .unwrap();
    let snapshot = runtime
        .read_active(access(&ingress, &request, 0), &registration.session)
        .unwrap();
    let debug = format!(
        "{runtime:?} {snapshot:?} {:?} {:?}",
        HostEventId::new("sensitive-id").unwrap(),
        RelatedEvent::new(EventRole::ExternalAgent, "sensitive-body")
    );
    for secret in [
        "sensitive-body",
        "sensitive-id",
        "client-retry-hint",
        "synthetic-token",
    ] {
        assert!(!debug.contains(secret));
    }
}

fn limits() -> SessionLimits {
    SessionLimits::new(32, 64, 256, 1024, 8192, 2).unwrap()
}

fn install(
    ingress: &mut TrustedIngress,
    token: &str,
    subject: &str,
    channel: &str,
    scope: &str,
    target: &str,
) -> CredentialId {
    ingress
        .install_credential(
            IngressToken::new(token).unwrap(),
            CallerSubject::new(subject).unwrap(),
            ["buddy".to_owned(), "other".to_owned()],
            ChannelId::new(channel).unwrap(),
            ChannelScope::new(scope).unwrap(),
            ReplyTarget::new(target).unwrap(),
        )
        .unwrap()
}

fn fixture_with(
    capacity: SessionLimits,
    timing: SessionTiming,
) -> (TrustedIngress, SessionRuntime, CredentialId) {
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route("buddy", identity("workspace", "owner", "pet"))
        .unwrap();
    ingress
        .register_route("other", identity("workspace", "owner", "other-pet"))
        .unwrap();
    let credential = install(
        &mut ingress,
        "synthetic-token",
        "caller",
        "api",
        "private",
        "original",
    );
    let runtime = SessionRuntime::new(&ingress, capacity, timing, 0);
    (ingress, runtime, credential)
}

fn fixture() -> (TrustedIngress, SessionRuntime, CredentialId) {
    fixture_with(limits(), SessionTiming::default())
}

fn call(ingress: &mut TrustedIngress, text: &str) -> TrustedRequest {
    ingress
        .admit(
            RawIngressCall::new(IngressToken::new("synthetic-token").unwrap(), "buddy", text)
                .with_session_hint("conversation")
                .with_client_request_id("client-retry-hint"),
        )
        .unwrap()
}

fn access<'a>(
    ingress: &'a TrustedIngress,
    request: &'a TrustedRequest,
    now: u64,
) -> SessionAccess<'a> {
    SessionAccess::new(ingress, request, now)
}

fn register(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    now: u64,
) -> Registration {
    runtime
        .register_user(
            access(ingress, request, now),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap()
}

fn finish(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    interaction: &InteractionId,
    now: u64,
) {
    runtime
        .finish_interaction(
            access(ingress, request, now),
            interaction,
            TerminalState::Completed,
            None,
        )
        .unwrap();
}

#[test]
fn default_timing_and_configuration_validation() {
    assert_eq!(SessionTiming::default().active_ms(), 1_800_000);
    assert_eq!(SessionTiming::default().quiet_ms(), 180_000);
    for timing in [
        (0, 180_000, 1, 1),
        (1, 179_999, 1, 1),
        (1, 180_000, 0, 1),
        (1, 180_000, 1, 0),
    ] {
        assert!(SessionTiming::new(timing.0, timing.1, timing.2, timing.3).is_err());
    }
    for position in 0..6 {
        let mut values = [1; 6];
        values[position] = 0;
        assert!(
            SessionLimits::new(
                values[0], values[1], values[2], values[3], values[4], values[5]
            )
            .is_err()
        );
    }
    assert!(HostEventId::new("").is_err());
    assert!(HostEventId::new("x".repeat(129)).is_err());
}

#[test]
fn strict_deadline_and_new_fragment_do_not_restore_old_context() {
    let (mut ingress, mut runtime, _) = fixture();
    let first = call(&mut ingress, "old raw");
    let old = register(&mut runtime, &ingress, &first, 0);
    assert!(
        runtime
            .read_active(access(&ingress, &first, 1_799_999), &old.session)
            .is_ok()
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &first, 1_800_000), &old.session)
            .unwrap_err(),
        GatewayError::SessionExpired
    );
    let second = call(&mut ingress, "new raw");
    let new = register(&mut runtime, &ingress, &second, 1_800_000);
    assert_ne!(old.session, new.session);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &second, 1_800_000), &new.session)
            .unwrap()
            .events
            .len(),
        1
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &second, 1_800_000), &old.session)
            .unwrap_err(),
        GatewayError::SessionExpired
    );
}

#[test]
fn only_successful_new_users_slide_the_window() {
    let (mut ingress, mut runtime, _) = fixture();
    let first = call(&mut ingress, "first");
    let registered = register(&mut runtime, &ingress, &first, 0);
    runtime
        .append_related(
            access(&ingress, &first, 100),
            &registered.interaction,
            RelatedEvent::new(EventRole::Assistant, "actual reply"),
        )
        .unwrap();
    runtime
        .append_related(
            access(&ingress, &first, 200),
            &registered.interaction,
            RelatedEvent::new(EventRole::Background, "background"),
        )
        .unwrap();
    finish(&mut runtime, &ingress, &first, &registered.interaction, 300);
    let history = call(&mut ingress, "history");
    let historical = runtime
        .register_user(
            access(&ingress, &history, 400),
            UserEvent::new(UserEventKind::History),
        )
        .unwrap();
    assert!(!historical.renewed);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &history, 400), &registered.session)
            .unwrap()
            .active_until_ms,
        Some(1_800_000)
    );
    let next = call(&mut ingress, "new");
    let next_registration = register(&mut runtime, &ingress, &next, 500);
    assert!(next_registration.renewed);
    assert_eq!(next_registration.session, registered.session);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &next, 500), &registered.session)
            .unwrap()
            .active_until_ms,
        Some(1_800_500)
    );
}

#[test]
fn proof_replay_and_trusted_source_id_are_idempotent_but_client_hints_are_not() {
    let (mut ingress, mut runtime, _) = fixture();
    let first = call(&mut ingress, "same raw");
    let input = UserEvent::new(UserEventKind::New)
        .with_id(HostEventId::new("trusted-platform-event").unwrap());
    let original = runtime
        .register_user(access(&ingress, &first, 0), input.clone())
        .unwrap();
    let replay = runtime
        .register_user(access(&ingress, &first, 100), input.clone())
        .unwrap();
    assert!(replay.duplicate);
    assert!(!replay.renewed);
    assert_eq!(original.interaction, replay.interaction);
    let retransmission = call(&mut ingress, "same raw");
    let repeated = runtime
        .register_user(access(&ingress, &retransmission, 200), input)
        .unwrap();
    assert!(repeated.duplicate);
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &retransmission, 200),
                &repeated.interaction,
                TerminalState::Completed,
                None
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &first, 200), &original.session)
            .unwrap()
            .active_until_ms,
        Some(1_800_000)
    );
    let next = call(&mut ingress, "same raw");
    let fresh = register(&mut runtime, &ingress, &next, 300);
    assert!(!fresh.duplicate);
    assert!(fresh.renewed);
    let snapshot = runtime
        .read_active(access(&ingress, &next, 300), &fresh.session)
        .unwrap();
    assert_eq!(snapshot.events.len(), 2);
    assert_eq!(
        snapshot.events[0].client_request_hint(),
        snapshot.events[1].client_request_hint()
    );
}

#[test]
fn duplicate_after_expiry_never_reactivates_or_opens_fragment() {
    let (mut ingress, mut runtime, _) = fixture();
    let first = call(&mut ingress, "old");
    let old = register(&mut runtime, &ingress, &first, 0);
    let replay = register(&mut runtime, &ingress, &first, 1_800_000);
    assert!(replay.duplicate);
    assert_eq!(old.session, replay.session);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &first, 1_800_000), &old.session)
            .unwrap_err(),
        GatewayError::SessionExpired
    );
}

#[test]
fn conflicting_trusted_id_does_not_change_raw_or_deadline() {
    let (mut ingress, mut runtime, _) = fixture();
    let first = call(&mut ingress, "original");
    let input = UserEvent::new(UserEventKind::New).with_id(HostEventId::new("source").unwrap());
    let registration = runtime
        .register_user(access(&ingress, &first, 0), input.clone())
        .unwrap();
    let changed = call(&mut ingress, "changed");
    assert_eq!(
        runtime
            .register_user(access(&ingress, &changed, 100), input)
            .unwrap_err(),
        GatewayError::EventConflict
    );
    let snapshot = runtime
        .read_active(access(&ingress, &first, 0), &registration.session)
        .unwrap();
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.events[0].body(), "original");
    assert_eq!(snapshot.active_until_ms, Some(1_800_000));
}

#[test]
fn source_order_and_admission_order_prevent_late_events_from_renewing() {
    let (mut ingress, mut runtime, _) = fixture();
    let early = call(&mut ingress, "admitted early");
    let newest = call(&mut ingress, "newest");
    let original = runtime
        .register_user(
            access(&ingress, &newest, 0),
            UserEvent::new(UserEventKind::New)
                .with_source_order(10)
                .with_source_timestamp_ms(u64::MAX),
        )
        .unwrap();
    let late_admission = runtime
        .register_user(
            access(&ingress, &early, 100),
            UserEvent::new(UserEventKind::New).with_source_order(11),
        )
        .unwrap();
    assert!(!late_admission.renewed);
    let late = call(&mut ingress, "late source");
    let late_source = runtime
        .register_user(
            access(&ingress, &late, 200),
            UserEvent::new(UserEventKind::New).with_source_order(9),
        )
        .unwrap();
    assert!(!late_source.renewed);
    let snapshot = runtime
        .read_active(access(&ingress, &late, 200), &original.session)
        .unwrap();
    assert_eq!(snapshot.active_until_ms, Some(1_800_000));
    assert_eq!(snapshot.events[0].source_timestamp_ms(), Some(u64::MAX));
    assert_eq!(snapshot.events[1].role(), EventRole::UserHistory);
    assert_eq!(snapshot.events[2].received_ms(), 200);
    assert_eq!(snapshot.events[2].source_order(), Some(9));
}

#[test]
fn history_alone_does_not_establish_active_state_or_idle_work() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "history");
    let registration = runtime
        .register_user(
            access(&ingress, &request, 0),
            UserEvent::new(UserEventKind::History),
        )
        .unwrap();
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 0), &registration.session)
            .unwrap_err(),
        GatewayError::SessionExpired
    );
    assert_eq!(
        runtime
            .issue_idle_batch(
                access(&ingress, &request, 180_000),
                &registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::NotReady
    );
    let new = call(&mut ingress, "new");
    assert_ne!(
        register(&mut runtime, &ingress, &new, 180_000).session,
        registration.session
    );
}

#[test]
fn role_fidelity_body_fidelity_and_related_event_replay() {
    let (mut ingress, mut runtime, _) = fixture();
    let text = "  \u{4f60}\u{597d}\nraw\ttext  ";
    let request = call(&mut ingress, text);
    let registration = register(&mut runtime, &ingress, &request, 0);
    for (offset, role) in [
        EventRole::Assistant,
        EventRole::ExternalAgent,
        EventRole::ToolReceipt,
        EventRole::InjectedContext,
        EventRole::Background,
    ]
    .into_iter()
    .enumerate()
    {
        let source = HostEventId::new(format!("source-{offset}")).unwrap();
        let first = runtime
            .append_related(
                access(&ingress, &request, 1),
                &registration.interaction,
                RelatedEvent::new(role, text).with_id(source.clone()),
            )
            .unwrap();
        let replay = runtime
            .append_related(
                access(&ingress, &request, 1),
                &registration.interaction,
                RelatedEvent::new(role, text).with_id(source),
            )
            .unwrap();
        assert_eq!(first, replay);
    }
    assert!(
        runtime
            .append_related(
                access(&ingress, &request, 1),
                &registration.interaction,
                RelatedEvent::new(EventRole::User, text)
            )
            .is_err()
    );
    let snapshot = runtime
        .read_active(access(&ingress, &request, 1), &registration.session)
        .unwrap();
    assert_eq!(snapshot.events.len(), 6);
    assert!(snapshot.events.iter().all(|event| event.body() == text));
    assert_eq!(snapshot.active_until_ms, Some(1_800_000));
}

#[test]
fn related_source_ids_cannot_be_relabelled_or_reused_as_user_evidence() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "user");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let source = HostEventId::new("agent-source").unwrap();
    runtime
        .append_related(
            access(&ingress, &request, 1),
            &registration.interaction,
            RelatedEvent::new(EventRole::ExternalAgent, "agent").with_id(source.clone()),
        )
        .unwrap();
    assert_eq!(
        runtime
            .append_related(
                access(&ingress, &request, 2),
                &registration.interaction,
                RelatedEvent::new(EventRole::Assistant, "agent").with_id(source.clone())
            )
            .unwrap_err(),
        GatewayError::EventConflict
    );
    let next = call(&mut ingress, "agent");
    assert_eq!(
        runtime
            .register_user(
                access(&ingress, &next, 2),
                UserEvent::new(UserEventKind::New).with_id(source)
            )
            .unwrap_err(),
        GatewayError::EventConflict
    );
}

#[test]
fn reply_and_terminal_are_atomic_and_replays_do_not_duplicate_output() {
    let (mut ingress, mut runtime, _) = fixture_with(
        SessionLimits::new(2, 3, 3, 8, 12, 1).unwrap(),
        SessionTiming::default(),
    );
    let request = call(&mut ingress, "12345678");
    let registration = register(&mut runtime, &ingress, &request, 0);
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &request, 100),
                &registration.interaction,
                TerminalState::Completed,
                Some("12345")
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    let snapshot = runtime
        .read_active(access(&ingress, &request, 0), &registration.session)
        .unwrap();
    assert_eq!(snapshot.events[0].terminal(), None);
    assert_eq!(snapshot.events.len(), 1);
    let receipt = runtime
        .finish_interaction(
            access(&ingress, &request, 1),
            &registration.interaction,
            TerminalState::Failed,
            Some("1234"),
        )
        .unwrap();
    assert_eq!(receipt.state, TerminalState::Failed);
    assert_eq!(receipt.reply_target.as_str(), "original");
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &request, 2),
                &registration.interaction,
                TerminalState::Failed,
                Some("1234")
            )
            .unwrap(),
        receipt
    );
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &request, 2),
                &registration.interaction,
                TerminalState::Completed,
                Some("1234")
            )
            .unwrap_err(),
        GatewayError::EventConflict
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 2), &registration.session)
            .unwrap()
            .events
            .len(),
        2
    );
}

#[test]
fn registration_capacity_failures_preserve_clock_raw_and_window() {
    for capacity in [
        SessionLimits::new(1, 1, 8, 8, 64, 1).unwrap(),
        SessionLimits::new(4, 8, 1, 8, 64, 1).unwrap(),
        SessionLimits::new(4, 8, 8, 8, 4, 1).unwrap(),
    ] {
        let (mut ingress, mut runtime, _) = fixture_with(capacity, SessionTiming::default());
        let request = call(&mut ingress, "1234");
        let registration = register(&mut runtime, &ingress, &request, 0);
        let next = call(&mut ingress, "1234");
        assert_eq!(
            runtime
                .register_user(
                    access(&ingress, &next, 100),
                    UserEvent::new(UserEventKind::New)
                )
                .unwrap_err(),
            GatewayError::CapacityExceeded
        );
        let snapshot = runtime
            .read_active(access(&ingress, &request, 0), &registration.session)
            .unwrap();
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.active_until_ms, Some(1_800_000));
        assert!(register(&mut runtime, &ingress, &request, 0).duplicate);
    }
}

#[test]
fn session_capacity_counts_retained_fragments_and_does_not_evict_evidence() {
    let (mut ingress, mut runtime, _) = fixture_with(
        SessionLimits::new(1, 8, 8, 8, 64, 1).unwrap(),
        SessionTiming::default(),
    );
    let first = call(&mut ingress, "first");
    let original = register(&mut runtime, &ingress, &first, 0);
    finish(&mut runtime, &ingress, &first, &original.interaction, 0);
    let next = call(&mut ingress, "next");
    assert_eq!(
        runtime
            .register_user(
                access(&ingress, &next, 1_800_000),
                UserEvent::new(UserEventKind::New)
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    let ticket = runtime
        .issue_idle_batch(access(&ingress, &first, 1_800_000), &original.session, true)
        .unwrap();
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &first, 1_800_000), &ticket)
            .unwrap()[0]
            .body(),
        "first"
    );
}

#[test]
fn body_capacity_uses_unicode_characters_and_failed_first_registration_is_atomic() {
    let (mut ingress, mut runtime, _) = fixture_with(
        SessionLimits::new(1, 3, 3, 2, 4, 1).unwrap(),
        SessionTiming::default(),
    );
    let too_big = call(&mut ingress, "abc");
    assert_eq!(
        runtime
            .register_user(
                access(&ingress, &too_big, 100),
                UserEvent::new(UserEventKind::New)
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    let valid = call(&mut ingress, "\u{4f60}\u{597d}");
    let registration = register(&mut runtime, &ingress, &valid, 0);
    runtime
        .append_related(
            access(&ingress, &valid, 0),
            &registration.interaction,
            RelatedEvent::new(EventRole::Assistant, "\u{518d}\u{89c1}"),
        )
        .unwrap();
    assert_eq!(
        runtime
            .append_related(
                access(&ingress, &valid, 10),
                &registration.interaction,
                RelatedEvent::new(EventRole::Background, "a")
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &valid, 0), &registration.session)
            .unwrap()
            .events
            .len(),
        2
    );
}

#[test]
fn monotonic_time_is_checked_for_reads_writes_and_failures_do_not_advance_it() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "raw");
    let registration = register(&mut runtime, &ingress, &request, 10);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 9), &registration.session)
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );
    assert_eq!(
        runtime
            .register_user(
                access(&ingress, &request, 9),
                UserEvent::new(UserEventKind::New)
            )
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );
    assert_eq!(
        runtime
            .append_related(
                access(&ingress, &request, 9),
                &registration.interaction,
                RelatedEvent::new(EventRole::Assistant, "reply")
            )
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );
    assert!(
        runtime
            .read_active(access(&ingress, &request, 10), &registration.session)
            .is_ok()
    );
}

#[test]
fn idle_requires_quiet_boundary_terminal_interactions_and_explicit_budget() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "raw");
    let registration = register(&mut runtime, &ingress, &request, 0);
    assert_eq!(
        runtime
            .issue_idle_batch(
                access(&ingress, &request, 180_000),
                &registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::NotReady
    );
    finish(
        &mut runtime,
        &ingress,
        &request,
        &registration.interaction,
        1,
    );
    assert_eq!(
        runtime
            .issue_idle_batch(
                access(&ingress, &request, 179_999),
                &registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::NotReady
    );
    assert_eq!(
        runtime
            .issue_idle_batch(
                access(&ingress, &request, 180_000),
                &registration.session,
                false
            )
            .unwrap_err(),
        GatewayError::NotReady
    );
    let ticket = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    assert_eq!(ticket.first_ordinal(), 1);
    assert_eq!(ticket.last_ordinal(), 1);
    assert_eq!(ticket.watermark(), 0);
    assert_eq!(
        runtime
            .issue_idle_batch(
                access(&ingress, &request, 180_000),
                &registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::BatchInFlight
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 180_000), &registration.session)
            .unwrap()
            .processed_through,
        0
    );
}

#[test]
fn batch_range_is_fixed_and_watermark_waits_for_every_acknowledgement() {
    let (mut ingress, mut runtime, _) = fixture();
    let first = call(&mut ingress, "first");
    let registration = register(&mut runtime, &ingress, &first, 0);
    runtime
        .finish_interaction(
            access(&ingress, &first, 0),
            &registration.interaction,
            TerminalState::Completed,
            Some("reply"),
        )
        .unwrap();
    let ticket = runtime
        .issue_idle_batch(
            access(&ingress, &first, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    let expected = runtime
        .read_batch(access(&ingress, &first, 180_000), &ticket)
        .unwrap();
    let next = call(&mut ingress, "new arrival");
    let next_registration = register(&mut runtime, &ingress, &next, 180_001);
    assert_eq!(next_registration.session, registration.session);
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &first, 180_001), &ticket)
            .unwrap()
            .len(),
        2
    );
    let partial = runtime
        .acknowledge_batch(
            access(&ingress, &first, 180_001),
            &ticket,
            expected[1].id(),
            BatchAcknowledgement::Confirmed,
        )
        .unwrap();
    assert_eq!(partial.acknowledged, 1);
    assert_eq!(partial.processed_through, 0);
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &first, 180_001),
                &ticket,
                expected[1].id(),
                BatchAcknowledgement::Confirmed
            )
            .unwrap(),
        partial
    );
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &first, 180_001),
                &ticket,
                expected[1].id(),
                BatchAcknowledgement::Skipped
            )
            .unwrap_err(),
        GatewayError::EventConflict
    );
    let snapshot = runtime
        .read_active(access(&ingress, &next, 180_001), &registration.session)
        .unwrap();
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &first, 180_001),
                &ticket,
                snapshot.events[2].id(),
                BatchAcknowledgement::Confirmed
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    let complete = runtime
        .acknowledge_batch(
            access(&ingress, &first, 180_001),
            &ticket,
            expected[0].id(),
            BatchAcknowledgement::Skipped,
        )
        .unwrap();
    assert_eq!(complete.acknowledged, 2);
    assert_eq!(complete.processed_through, 2);
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &first, 180_001),
                &ticket,
                expected[0].id(),
                BatchAcknowledgement::Skipped
            )
            .unwrap(),
        complete
    );
    finish(
        &mut runtime,
        &ingress,
        &next,
        &next_registration.interaction,
        180_001,
    );
    let following = runtime
        .issue_idle_batch(
            access(&ingress, &next, 360_001),
            &registration.session,
            true,
        )
        .unwrap();
    assert_eq!(following.first_ordinal(), 3);
    assert_eq!(following.watermark(), 2);
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &first, 360_001), &ticket)
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &next, 360_001), &registration.session)
            .unwrap()
            .events[0]
            .body(),
        "first"
    );
}

#[test]
fn partial_acknowledgements_survive_cancel_and_retry_without_advancing_watermark() {
    let (mut ingress, mut runtime, _) = fixture();
    let request = call(&mut ingress, "raw");
    let registration = register(&mut runtime, &ingress, &request, 0);
    runtime
        .finish_interaction(
            access(&ingress, &request, 0),
            &registration.interaction,
            TerminalState::Unknown,
            Some("unknown result"),
        )
        .unwrap();
    let ticket = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    let events = runtime
        .read_batch(access(&ingress, &request, 180_000), &ticket)
        .unwrap();
    runtime
        .acknowledge_batch(
            access(&ingress, &request, 180_000),
            &ticket,
            events[0].id(),
            BatchAcknowledgement::Confirmed,
        )
        .unwrap();
    runtime
        .cancel_batch(access(&ingress, &request, 180_001), &ticket)
        .unwrap();
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &request, 180_001),
                &ticket,
                events[1].id(),
                BatchAcknowledgement::Confirmed
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    let retry = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_001),
            &registration.session,
            true,
        )
        .unwrap();
    assert_ne!(ticket, retry);
    assert!(retry.revision() > ticket.revision());
    let pending = runtime
        .read_batch(access(&ingress, &request, 180_001), &retry)
        .unwrap();
    assert_eq!(
        pending[0].acknowledgement(),
        Some(BatchAcknowledgement::Confirmed)
    );
    assert_eq!(retry.watermark(), 0);
    let partial = runtime
        .acknowledge_batch(
            access(&ingress, &request, 180_001),
            &retry,
            pending[0].id(),
            BatchAcknowledgement::Confirmed,
        )
        .unwrap();
    assert_eq!(partial.acknowledged, 1);
    assert_eq!(partial.processed_through, 0);
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &request, 180_001),
                &retry,
                pending[1].id(),
                BatchAcknowledgement::Skipped
            )
            .unwrap()
            .processed_through,
        2
    );
}

#[test]
fn batch_expiry_rejects_late_read_and_write_without_a_timer_and_keeps_partial_receipts() {
    let (mut ingress, mut runtime, _) = fixture_with(
        limits(),
        SessionTiming::new(1_800_000, 180_000, 3_600_000, 10).unwrap(),
    );
    let request = call(&mut ingress, "raw");
    let registration = register(&mut runtime, &ingress, &request, 0);
    runtime
        .finish_interaction(
            access(&ingress, &request, 0),
            &registration.interaction,
            TerminalState::Completed,
            Some("reply"),
        )
        .unwrap();
    let ticket = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    let events = runtime
        .read_batch(access(&ingress, &request, 180_009), &ticket)
        .unwrap();
    runtime
        .acknowledge_batch(
            access(&ingress, &request, 180_009),
            &ticket,
            events[0].id(),
            BatchAcknowledgement::Confirmed,
        )
        .unwrap();
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &request, 180_010), &ticket)
            .unwrap_err(),
        GatewayError::DeadlineExceeded
    );
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &request, 180_010),
                &ticket,
                events[1].id(),
                BatchAcknowledgement::Confirmed
            )
            .unwrap_err(),
        GatewayError::DeadlineExceeded
    );
    let retry = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_010),
            &registration.session,
            true,
        )
        .unwrap();
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &request, 180_010), &retry)
            .unwrap()[0]
            .acknowledgement(),
        Some(BatchAcknowledgement::Confirmed)
    );
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &request, 180_010), &ticket)
            .unwrap_err(),
        GatewayError::StaleHandle
    );
}

#[test]
fn global_batch_concurrency_is_bounded_and_expired_slots_are_reusable() {
    let (mut ingress, mut runtime, _) = fixture_with(
        SessionLimits::new(4, 8, 16, 64, 256, 1).unwrap(),
        SessionTiming::new(1_800_000, 180_000, 3_600_000, 10).unwrap(),
    );
    let first = call(&mut ingress, "first");
    let second = ingress
        .admit(
            RawIngressCall::new(
                IngressToken::new("synthetic-token").unwrap(),
                "buddy",
                "second",
            )
            .with_session_hint("different"),
        )
        .unwrap();
    let first_registration = register(&mut runtime, &ingress, &first, 0);
    let second_registration = register(&mut runtime, &ingress, &second, 0);
    finish(
        &mut runtime,
        &ingress,
        &first,
        &first_registration.interaction,
        0,
    );
    finish(
        &mut runtime,
        &ingress,
        &second,
        &second_registration.interaction,
        0,
    );
    runtime
        .issue_idle_batch(
            access(&ingress, &first, 180_000),
            &first_registration.session,
            true,
        )
        .unwrap();
    assert_eq!(
        runtime
            .issue_idle_batch(
                access(&ingress, &second, 180_001),
                &second_registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    assert!(
        runtime
            .issue_idle_batch(
                access(&ingress, &second, 180_010),
                &second_registration.session,
                true
            )
            .is_ok()
    );
}

#[test]
fn logical_session_serialization_and_quiet_window_span_fragments() {
    let (mut ingress, mut runtime, _) = fixture_with(
        limits(),
        SessionTiming::new(100, 180_000, 3_600_000, 1_000_000).unwrap(),
    );
    let first = call(&mut ingress, "first");
    let old = register(&mut runtime, &ingress, &first, 0);
    finish(&mut runtime, &ingress, &first, &old.interaction, 0);
    let second = call(&mut ingress, "second");
    let new = register(&mut runtime, &ingress, &second, 100);
    finish(&mut runtime, &ingress, &second, &new.interaction, 100);
    assert_ne!(old.session, new.session);
    assert_eq!(
        runtime
            .issue_idle_batch(access(&ingress, &first, 180_000), &old.session, true)
            .unwrap_err(),
        GatewayError::NotReady
    );
    let ticket = runtime
        .issue_idle_batch(access(&ingress, &first, 180_100), &old.session, true)
        .unwrap();
    assert_eq!(
        runtime
            .issue_idle_batch(access(&ingress, &second, 180_100), &new.session, true)
            .unwrap_err(),
        GatewayError::BatchInFlight
    );
    let old_events = runtime
        .read_batch(access(&ingress, &first, 180_100), &ticket)
        .unwrap();
    let new_ticket_event = runtime
        .read_batch(access(&ingress, &second, 180_100), &ticket)
        .unwrap_err();
    assert_eq!(new_ticket_event, GatewayError::ForeignRequest);
    runtime
        .acknowledge_batch(
            access(&ingress, &first, 180_100),
            &ticket,
            old_events[0].id(),
            BatchAcknowledgement::Skipped,
        )
        .unwrap();
    let new_ticket = runtime
        .issue_idle_batch(access(&ingress, &second, 180_100), &new.session, true)
        .unwrap();
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &second, 180_100),
                &new_ticket,
                old_events[0].id(),
                BatchAcknowledgement::Confirmed
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &second, 180_100), &new_ticket)
            .unwrap()[0]
            .body(),
        "second"
    );
}

#[test]
fn cancelling_new_interaction_invalidates_old_in_flight_ticket() {
    let (mut ingress, mut runtime, _) = fixture();
    let first = call(&mut ingress, "first");
    let original = register(&mut runtime, &ingress, &first, 0);
    finish(&mut runtime, &ingress, &first, &original.interaction, 0);
    let ticket = runtime
        .issue_idle_batch(access(&ingress, &first, 180_000), &original.session, true)
        .unwrap();
    let next = call(&mut ingress, "new");
    let new = register(&mut runtime, &ingress, &next, 180_001);
    runtime
        .cancel_interaction(access(&ingress, &next, 180_002), &new.interaction)
        .unwrap();
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &first, 180_002), &ticket)
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &next, 180_002), &new.session)
            .unwrap()
            .processed_through,
        0
    );
}

#[test]
fn completed_batches_release_concurrency_but_never_delete_raw() {
    let (mut ingress, mut runtime, _) = fixture_with(
        SessionLimits::new(4, 8, 16, 64, 256, 1).unwrap(),
        SessionTiming::default(),
    );
    let first = call(&mut ingress, "first");
    let second = ingress
        .admit(
            RawIngressCall::new(
                IngressToken::new("synthetic-token").unwrap(),
                "buddy",
                "second",
            )
            .with_session_hint("different"),
        )
        .unwrap();
    let original = register(&mut runtime, &ingress, &first, 0);
    let other = register(&mut runtime, &ingress, &second, 0);
    finish(&mut runtime, &ingress, &first, &original.interaction, 0);
    finish(&mut runtime, &ingress, &second, &other.interaction, 0);
    let ticket = runtime
        .issue_idle_batch(access(&ingress, &first, 180_000), &original.session, true)
        .unwrap();
    let event = runtime
        .read_batch(access(&ingress, &first, 180_000), &ticket)
        .unwrap()
        .remove(0);
    runtime
        .acknowledge_batch(
            access(&ingress, &first, 180_000),
            &ticket,
            event.id(),
            BatchAcknowledgement::Confirmed,
        )
        .unwrap();
    assert!(
        runtime
            .issue_idle_batch(access(&ingress, &second, 180_000), &other.session, true)
            .is_ok()
    );
    assert_eq!(
        runtime
            .issue_idle_batch(access(&ingress, &first, 180_000), &original.session, true)
            .unwrap_err(),
        GatewayError::NotReady
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &first, 180_000), &original.session)
            .unwrap()
            .events[0]
            .body(),
        "first"
    );
}
