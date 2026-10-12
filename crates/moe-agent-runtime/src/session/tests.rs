use super::*;
use crate::{CallerSubject, ChannelId, ChannelScope, IngressToken, RawIngressCall};
use moe_memory_core::{Namespace, PetIdentity};

fn fixture() -> (TrustedIngress, SessionRuntime, TrustedRequest) {
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route(
            "buddy",
            PetIdentity::new(Namespace::new("workspace", "owner").unwrap(), "pet").unwrap(),
        )
        .unwrap();
    ingress
        .install_credential(
            IngressToken::new("synthetic-token").unwrap(),
            CallerSubject::new("caller").unwrap(),
            ["buddy".to_owned()],
            ChannelId::new("channel").unwrap(),
            ChannelScope::new("scope").unwrap(),
            ReplyTarget::new("original").unwrap(),
        )
        .unwrap();
    let request = ingress
        .admit(
            RawIngressCall::new(
                IngressToken::new("synthetic-token").unwrap(),
                "buddy",
                "raw",
            )
            .with_session_hint("conversation"),
        )
        .unwrap();
    let runtime = SessionRuntime::new(
        &ingress,
        SessionLimits::new(4, 8, 16, 64, 256, 1).unwrap(),
        SessionTiming::default(),
        0,
    );
    (ingress, runtime, request)
}

fn completed(
    ingress: &TrustedIngress,
    runtime: &mut SessionRuntime,
    request: &TrustedRequest,
) -> Registration {
    let registration = runtime
        .register_user(
            SessionAccess::new(ingress, request, 0),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    runtime
        .finish_interaction(
            SessionAccess::new(ingress, request, 0),
            &registration.interaction,
            TerminalState::Completed,
            Some("reply"),
        )
        .unwrap();
    registration
}

#[test]
fn session_id_exhaustion_preserves_all_state() {
    let (ingress, mut runtime, request) = fixture();
    runtime.session_sequence = u64::MAX;
    assert_eq!(
        runtime
            .register_user(
                SessionAccess::new(&ingress, &request, 1),
                UserEvent::new(UserEventKind::New)
            )
            .unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert!(runtime.sessions.is_empty());
    assert_eq!(runtime.events_total, 0);
    assert_eq!(runtime.chars_total, 0);
    assert_eq!(runtime.last_now_ms, 0);
    assert_eq!(runtime.session_sequence, u64::MAX);
}

#[test]
fn batch_id_and_deadline_exhaustion_preserve_watermark_and_partial_state() {
    let (ingress, mut runtime, request) = fixture();
    let registration = completed(&ingress, &mut runtime, &request);
    runtime.batch_sequence = u64::MAX;
    assert_eq!(
        runtime
            .issue_idle_batch(
                SessionAccess::new(&ingress, &request, 180_000),
                &registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert_eq!(runtime.batch_sequence, u64::MAX);
    assert_eq!(runtime.last_now_ms, 0);
    assert!(runtime.sessions[0].batch.is_none());
    runtime.batch_sequence = 0;
    assert_eq!(
        runtime
            .issue_idle_batch(
                SessionAccess::new(&ingress, &request, u64::MAX),
                &registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert_eq!(runtime.batch_sequence, 0);
    assert_eq!(runtime.last_now_ms, 0);
    assert_eq!(runtime.sessions[0].processed_through, 0);
    assert!(runtime.sessions[0].batch.is_none());
    assert!(
        runtime
            .issue_idle_batch(
                SessionAccess::new(&ingress, &request, 180_000),
                &registration.session,
                true
            )
            .is_ok()
    );
}

#[test]
fn revision_exhaustion_does_not_half_revoke_or_cancel_batch() {
    let (ingress, mut runtime, request) = fixture();
    let registration = completed(&ingress, &mut runtime, &request);
    runtime.sessions[0].revision = u64::MAX;
    let ticket = runtime
        .issue_idle_batch(
            SessionAccess::new(&ingress, &request, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    assert_eq!(
        runtime
            .revoke_session(
                SessionAccess::new(&ingress, &request, 180_001),
                &registration.session
            )
            .unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert_eq!(
        runtime
            .cancel_batch(SessionAccess::new(&ingress, &request, 180_001), &ticket)
            .unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert!(!runtime.sessions[0].revoked);
    assert_eq!(runtime.last_now_ms, 180_000);
    assert_eq!(
        runtime
            .read_batch(SessionAccess::new(&ingress, &request, 180_000), &ticket)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn revision_exhaustion_does_not_half_cancel_interaction() {
    let (ingress, mut runtime, request) = fixture();
    let registration = runtime
        .register_user(
            SessionAccess::new(&ingress, &request, 0),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    runtime.sessions[0].revision = u64::MAX;
    assert_eq!(
        runtime
            .cancel_interaction(
                SessionAccess::new(&ingress, &request, 1),
                &registration.interaction
            )
            .unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert_eq!(runtime.sessions[0].events[0].staged.terminal, None);
    assert_eq!(runtime.last_now_ms, 0);
    assert!(
        runtime
            .finish_interaction(
                SessionAccess::new(&ingress, &request, 0),
                &registration.interaction,
                TerminalState::Completed,
                None
            )
            .is_ok()
    );
}

#[test]
fn interaction_deadline_overflow_does_not_publish_active_state() {
    let (ingress, mut runtime, request) = fixture();
    runtime.timing = SessionTiming::new(1, 180_000, u64::MAX, 1).unwrap();
    assert_eq!(
        runtime
            .register_user(
                SessionAccess::new(&ingress, &request, 1),
                UserEvent::new(UserEventKind::New)
            )
            .unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert!(runtime.sessions.is_empty());
    assert_eq!(runtime.session_sequence, 0);
    assert_eq!(runtime.events_total, 0);
    assert_eq!(runtime.chars_total, 0);
    assert_eq!(runtime.last_now_ms, 0);
}
