use super::*;
use crate::{CallerSubject, ChannelId, ChannelScope, ReplyTarget};
use moe_memory_core::{Namespace, PetIdentity};

fn fixture() -> (TrustedIngress, HookRuntime, IngressToken) {
    let mut ingress = TrustedIngress::new();
    let identity = PetIdentity::new(
        Namespace::new("synthetic-workspace", "synthetic-user").unwrap(),
        "synthetic-pet",
    )
    .unwrap();
    ingress.register_route("buddy", identity).unwrap();
    ingress
        .install_credential(
            IngressToken::new("synthetic-token").unwrap(),
            CallerSubject::new("synthetic-host").unwrap(),
            ["buddy".to_owned()],
            ChannelId::new("hook").unwrap(),
            ChannelScope::new("private").unwrap(),
            ReplyTarget::new("fixed-target").unwrap(),
        )
        .unwrap();
    (
        ingress,
        HookRuntime::new(HookLimits::default()),
        IngressToken::new("synthetic-token").unwrap(),
    )
}

fn config(expiry: u64) -> HookBindingConfig {
    HookBindingConfig::new(HookPermissions::new(true, false, false), expiry)
        .with_cli_session_id("cli-session")
}

#[test]
fn binding_sequence_exhaustion_has_no_partial_publication() {
    let (ingress, mut runtime, token) = fixture();
    runtime.binding_sequence = u64::MAX;
    assert!(matches!(
        runtime.bind(&ingress, &token, "buddy", config(100), 0),
        Err(GatewayError::IdentifierExhausted)
    ));
    assert!(runtime.bindings.is_empty());
    assert_eq!(runtime.issued_bindings, 0);
    assert_eq!(runtime.binding_sequence, u64::MAX);
}

#[test]
fn invocation_sequence_exhaustion_preserves_all_issuance_budgets() {
    let (ingress, mut runtime, token) = fixture();
    let binding = runtime
        .bind(&ingress, &token, "buddy", config(100), 0)
        .unwrap();
    runtime.invocation_sequence = u64::MAX;
    assert!(matches!(
        runtime.accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", "synthetic-prompt"),
            1
        ),
        Err(GatewayError::IdentifierExhausted)
    ));
    assert!(runtime.invocations.is_empty());
    assert_eq!(runtime.issued_invocations, 0);
    assert_eq!(runtime.issued_prompt_chars, 0);
}

#[test]
fn binding_revision_overflow_keeps_original_permission_and_prompt() {
    let (ingress, mut runtime, token) = fixture();
    let binding = runtime
        .bind(&ingress, &token, "buddy", config(100), 0)
        .unwrap();
    runtime.bindings.get_mut(&binding.id).unwrap().revision = u64::MAX;
    let binding = runtime.binding_handle(binding.id, u64::MAX);
    let invocation = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", "synthetic-prompt"),
            1,
        )
        .unwrap();
    let denied =
        HookBindingConfig::new(HookPermissions::default(), 100).with_cli_session_id("cli-session");
    assert!(matches!(
        runtime.update_binding(&ingress, &binding, u64::MAX, denied, 2),
        Err(GatewayError::IdentifierExhausted)
    ));
    assert_eq!(
        runtime
            .observe_prompt(&ingress, &binding, &invocation, 2)
            .unwrap()
            .text(),
        "synthetic-prompt"
    );
    assert_eq!(runtime.bindings[&binding.id].revision, u64::MAX);
}

#[test]
fn exhausted_source_revision_cannot_recreate_revoked_context() {
    let (_, mut runtime, _) = fixture();
    runtime
        .register_context(
            HookContext::new("source", u64::MAX - 1, "synthetic-context"),
            None,
        )
        .unwrap();
    runtime
        .revoke_context("source", u64::MAX - 1, u64::MAX)
        .unwrap();
    for revision in [0, 1, u64::MAX - 1, u64::MAX] {
        assert_eq!(
            runtime.register_context(
                HookContext::new("source", revision, "resurrected-context"),
                Some(u64::MAX)
            ),
            Err(GatewayError::VersionConflict)
        );
    }
    assert!(!runtime.contexts.contains_key("source"));
    assert_eq!(runtime.context_revisions["source"], u64::MAX);
}

#[test]
fn deadline_overflow_does_not_issue_an_invocation() {
    let (ingress, mut runtime, token) = fixture();
    let binding = runtime
        .bind(&ingress, &token, "buddy", config(u64::MAX), u64::MAX - 10)
        .unwrap();
    assert!(matches!(
        runtime.accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", "synthetic-prompt"),
            u64::MAX - 9
        ),
        Err(GatewayError::IdentifierExhausted)
    ));
    assert_eq!(runtime.issued_invocations, 0);
    assert!(runtime.invocations.is_empty());
}
