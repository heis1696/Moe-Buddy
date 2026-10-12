use moe_agent_runtime::{
    CallerSubject, ChannelId, ChannelScope, GatewayError, HookBindingConfig, HookContext,
    HookEvent, HookLimits, HookPermissions, HookReplyRole, HookRuntime, HookTransportOutcome,
    IngressToken, ReplyTarget, TrustedIngress,
};
use moe_memory_core::{Namespace, PetIdentity};

const TOKEN: &str = "synthetic-hook-secret";
const PROMPT: &str = "synthetic-unverified-prompt";
const CONTEXT: &str = "synthetic-reviewed-context";

fn identity(workspace: &str, user: &str, pet: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new(workspace, user).unwrap(), pet).unwrap()
}

fn ingress() -> TrustedIngress {
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route("pet/default", identity("workspace", "owner", "stable-pet"))
        .unwrap();
    ingress
        .install_credential(
            IngressToken::new(TOKEN).unwrap(),
            CallerSubject::new("trusted-host").unwrap(),
            ["pet/default".to_owned()],
            ChannelId::new("desktop-channel").unwrap(),
            ChannelScope::new("private-scope").unwrap(),
            ReplyTarget::new("fixed-reply-target").unwrap(),
        )
        .unwrap();
    ingress
}

fn limits() -> HookLimits {
    HookLimits::new(8, 8, 16, 16, 256, 256, 1_000).unwrap()
}

fn config(permissions: HookPermissions, expiry: u64) -> HookBindingConfig {
    HookBindingConfig::new(permissions, expiry).with_cli_session_id("cli-session")
}

fn context(runtime: &mut HookRuntime) {
    runtime
        .register_context(HookContext::new("reviewed-source", 1, CONTEXT), None)
        .unwrap();
}

fn bind(
    runtime: &mut HookRuntime,
    ingress: &TrustedIngress,
    permissions: HookPermissions,
) -> moe_agent_runtime::HookBinding {
    let binding_config = config(permissions, 500);
    let binding_config = if permissions.return_context {
        binding_config.with_context_source("reviewed-source")
    } else {
        binding_config
    };
    runtime
        .bind(
            ingress,
            &IngressToken::new(TOKEN).unwrap(),
            "pet/default",
            binding_config,
            0,
        )
        .unwrap()
}

#[test]
fn independent_permissions_default_to_denial_and_keep_roles_distinct() {
    let ingress = ingress();
    let mut runtime = HookRuntime::new(limits());
    context(&mut runtime);

    let denied = bind(&mut runtime, &ingress, HookPermissions::default());
    assert_eq!(
        runtime
            .accept_event(
                &ingress,
                &denied,
                HookEvent::prompt_submitted("cli-session", PROMPT),
                0,
            )
            .unwrap_err(),
        GatewayError::Unauthorized
    );

    assert!(matches!(
        runtime.bind(
            &ingress,
            &IngressToken::new(PROMPT).unwrap(),
            "pet/default",
            config(HookPermissions::new(true, false, false), 500),
            0,
        ),
        Err(GatewayError::Unauthorized)
    ));

    let observe_only = bind(
        &mut runtime,
        &ingress,
        HookPermissions::new(true, false, false),
    );
    let observed = runtime
        .accept_event(
            &ingress,
            &observe_only,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            0,
        )
        .unwrap();
    assert_eq!(
        runtime
            .observe_prompt(&ingress, &observe_only, &observed, 0)
            .unwrap()
            .text(),
        PROMPT
    );
    assert_eq!(
        runtime
            .prepare_reply(&ingress, &observe_only, &observed, 0)
            .unwrap_err(),
        GatewayError::Unauthorized
    );

    let return_only = bind(
        &mut runtime,
        &ingress,
        HookPermissions::new(false, true, false),
    );
    let unobserved = runtime
        .accept_event(
            &ingress,
            &return_only,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            0,
        )
        .unwrap();
    assert_eq!(
        runtime
            .observe_prompt(&ingress, &return_only, &unobserved, 0)
            .unwrap_err(),
        GatewayError::Unauthorized
    );
    assert_eq!(
        runtime
            .prepare_reply(&ingress, &return_only, &unobserved, 0)
            .unwrap_err(),
        GatewayError::Unauthorized
    );

    let model_only = bind(
        &mut runtime,
        &ingress,
        HookPermissions::new(false, false, true),
    );
    assert_eq!(
        runtime
            .accept_event(
                &ingress,
                &model_only,
                HookEvent::prompt_submitted("cli-session", PROMPT),
                0,
            )
            .unwrap_err(),
        GatewayError::Unauthorized
    );

    let context_to_model = bind(
        &mut runtime,
        &ingress,
        HookPermissions::new(false, true, true),
    );
    let invocation = runtime
        .accept_event(
            &ingress,
            &context_to_model,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            0,
        )
        .unwrap();
    let reply = runtime
        .prepare_reply(&ingress, &context_to_model, &invocation, 0)
        .unwrap();
    assert_eq!(reply.text(), CONTEXT);
    assert_eq!(reply.role(), HookReplyRole::InjectedContext);
    assert_eq!(reply.reply_target(), "fixed-reply-target");
    assert_eq!(reply.cli_session_id(), "cli-session");
    runtime
        .authorize_before_write(&ingress, &context_to_model, &invocation, &reply, 0)
        .unwrap();
}

#[test]
fn binding_freezes_credential_route_and_fixed_reply_destination() {
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route("pet/alpha", identity("workspace-a", "owner-a", "pet-a"))
        .unwrap();
    ingress
        .register_route("pet/beta", identity("workspace-b", "owner-b", "pet-b"))
        .unwrap();
    for (token, subject, alias, channel, scope, target) in [
        (
            "host-alpha",
            "subject-alpha",
            "pet/alpha",
            "channel-a",
            "scope-a",
            "reply-a",
        ),
        (
            "host-beta",
            "subject-beta",
            "pet/beta",
            "channel-b",
            "scope-b",
            "reply-b",
        ),
    ] {
        ingress
            .install_credential(
                IngressToken::new(token).unwrap(),
                CallerSubject::new(subject).unwrap(),
                [alias.to_owned()],
                ChannelId::new(channel).unwrap(),
                ChannelScope::new(scope).unwrap(),
                ReplyTarget::new(target).unwrap(),
            )
            .unwrap();
    }
    let mut runtime = HookRuntime::new(limits());
    for source in ["context-alpha", "context-beta"] {
        runtime
            .register_context(HookContext::new(source, 1, source), None)
            .unwrap();
    }
    let permissions = HookPermissions::new(true, true, true);
    let alpha = runtime
        .bind(
            &ingress,
            &IngressToken::new("host-alpha").unwrap(),
            "pet/alpha",
            config(permissions, 500).with_context_source("context-alpha"),
            0,
        )
        .unwrap();
    let beta = runtime
        .bind(
            &ingress,
            &IngressToken::new("host-beta").unwrap(),
            "pet/beta",
            config(permissions, 500).with_context_source("context-beta"),
            0,
        )
        .unwrap();
    assert_eq!(
        runtime
            .bind(
                &ingress,
                &IngressToken::new("host-alpha").unwrap(),
                "pet/beta",
                config(permissions, 500).with_context_source("context-alpha"),
                0,
            )
            .unwrap_err(),
        GatewayError::RouteDenied
    );
    let alpha_call = runtime
        .accept_event(
            &ingress,
            &alpha,
            HookEvent::prompt_submitted("cli-session", "same prompt"),
            1,
        )
        .unwrap();
    let beta_call = runtime
        .accept_event(
            &ingress,
            &beta,
            HookEvent::prompt_submitted("cli-session", "same prompt"),
            1,
        )
        .unwrap();
    let alpha_reply = runtime
        .prepare_reply(&ingress, &alpha, &alpha_call, 1)
        .unwrap();
    let beta_reply = runtime
        .prepare_reply(&ingress, &beta, &beta_call, 1)
        .unwrap();
    assert_eq!(alpha_reply.reply_target(), "reply-a");
    assert_eq!(beta_reply.reply_target(), "reply-b");
    assert_eq!(alpha_reply.cli_session_id(), beta_reply.cli_session_id());
    assert_eq!(alpha_reply.text(), "context-alpha");
    assert_eq!(beta_reply.text(), "context-beta");
    let repeated_call = runtime
        .accept_event(
            &ingress,
            &alpha,
            HookEvent::prompt_submitted("cli-session", "same prompt"),
            2,
        )
        .unwrap();
    let repeated_reply = runtime
        .prepare_reply(&ingress, &alpha, &repeated_call, 2)
        .unwrap();
    assert_eq!(repeated_reply.reply_target(), "reply-a");
}

#[test]
fn ingress_generation_change_and_route_aba_revoke_old_calls() {
    let mut ingress = ingress();
    let mut runtime = HookRuntime::new(limits());
    runtime
        .register_context(HookContext::new("reviewed-source", 1, "text"), None)
        .unwrap();
    let binding = runtime
        .bind(
            &ingress,
            &IngressToken::new(TOKEN).unwrap(),
            "pet/default",
            config(HookPermissions::new(true, true, true), 500)
                .with_context_source("reviewed-source"),
            0,
        )
        .unwrap();
    let invocation = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            1,
        )
        .unwrap();
    let original_identity = ingress.route_identity("pet/default").unwrap().clone();
    ingress.unregister_route("pet/default").unwrap();
    ingress
        .register_route("pet/default", original_identity)
        .unwrap();
    assert_eq!(
        runtime
            .prepare_reply(&ingress, &binding, &invocation, 2)
            .unwrap_err(),
        GatewayError::Revoked
    );
}

#[test]
fn context_revision_and_binding_cas_prevent_aba_revival() {
    let ingress = ingress();
    let mut runtime = HookRuntime::new(limits());
    context(&mut runtime);
    let permissions = HookPermissions::new(true, true, true);
    let binding = runtime
        .bind(
            &ingress,
            &IngressToken::new(TOKEN).unwrap(),
            "pet/default",
            config(permissions, 500).with_context_source("reviewed-source"),
            0,
        )
        .unwrap();
    let old_call = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            1,
        )
        .unwrap();
    runtime
        .register_context(
            HookContext::new("reviewed-source", 2, "new-context"),
            Some(1),
        )
        .unwrap();
    assert_eq!(
        runtime
            .prepare_reply(&ingress, &binding, &old_call, 2)
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime.register_context(HookContext::new("reviewed-source", 1, CONTEXT), Some(2)),
        Err(GatewayError::VersionConflict)
    );

    let updated = runtime
        .update_binding(
            &ingress,
            &binding,
            1,
            config(HookPermissions::new(false, true, true), 500)
                .with_context_source("reviewed-source"),
            2,
        )
        .unwrap();
    assert_eq!(updated.revision(), 2);
    let restored = runtime
        .update_binding(
            &ingress,
            &updated,
            2,
            config(permissions, 500).with_context_source("reviewed-source"),
            3,
        )
        .unwrap();
    assert_eq!(restored.revision(), 3);
    assert!(matches!(
        runtime.accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            4,
        ),
        Err(GatewayError::StaleHandle)
    ));
    assert!(matches!(
        runtime.update_binding(
            &ingress,
            &restored,
            1,
            config(permissions, 500).with_context_source("reviewed-source"),
            4,
        ),
        Err(GatewayError::VersionConflict)
    ));
}

#[test]
fn context_revocation_tombstone_rejects_old_revision() {
    let mut runtime = HookRuntime::new(limits());
    runtime
        .register_context(HookContext::new("reviewed-source", 7, CONTEXT), None)
        .unwrap();
    runtime.revoke_context("reviewed-source", 7, 8).unwrap();
    assert_eq!(
        runtime.register_context(HookContext::new("reviewed-source", 7, CONTEXT), Some(8)),
        Err(GatewayError::VersionConflict)
    );
    runtime
        .register_context(
            HookContext::new("reviewed-source", 9, "fresh context"),
            Some(8),
        )
        .unwrap();

    let mut bounded = HookRuntime::new(HookLimits::new(1, 1, 1, 1, 16, 16, 10).unwrap());
    bounded
        .register_context(HookContext::new("retired-source", 1, "text"), None)
        .unwrap();
    bounded.revoke_context("retired-source", 1, 2).unwrap();
    assert_eq!(
        bounded.register_context(HookContext::new("new-source", 1, "text"), None),
        Err(GatewayError::CapacityExceeded)
    );
}

#[test]
fn handles_are_service_instance_bound() {
    let ingress = ingress();
    let mut first = HookRuntime::new(limits());
    let mut second = HookRuntime::new(limits());
    context(&mut first);
    context(&mut second);
    let binding = bind(&mut first, &ingress, HookPermissions::new(true, true, true));
    let invocation = first
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            1,
        )
        .unwrap();
    assert!(matches!(
        second.observe_prompt(&ingress, &binding, &invocation, 1),
        Err(GatewayError::ForeignRequest)
    ));

    let other_ingress = TrustedIngress::new();
    assert!(matches!(
        first.observe_prompt(&other_ingress, &binding, &invocation, 1),
        Err(GatewayError::ForeignRequest)
    ));
}

#[test]
fn cancellation_wins_over_late_completion_and_duplicate_receipts() {
    let ingress = ingress();
    let mut runtime = HookRuntime::new(limits());
    context(&mut runtime);
    let binding = bind(
        &mut runtime,
        &ingress,
        HookPermissions::new(true, true, true),
    );
    let invocation = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            1,
        )
        .unwrap();
    let reply = runtime
        .prepare_reply(&ingress, &binding, &invocation, 1)
        .unwrap();
    let cancelled = runtime.cancel(&invocation, 2).unwrap();
    assert_eq!(cancelled.outcome(), HookTransportOutcome::Cancelled);
    assert_eq!(
        runtime.authorize_before_write(&ingress, &binding, &invocation, &reply, 3),
        Err(GatewayError::InteractionClosed)
    );
    assert_eq!(
        runtime.record_transport_outcome(
            &ingress,
            &binding,
            &invocation,
            &reply,
            3,
            HookTransportOutcome::Written,
        ),
        Err(GatewayError::InteractionClosed)
    );
    assert_eq!(runtime.receipts(), &[cancelled]);
}

#[test]
fn transport_ledger_is_bounded_to_host_outcomes_only() {
    let ingress = ingress();
    let mut runtime = HookRuntime::new(limits());
    context(&mut runtime);
    let binding = bind(
        &mut runtime,
        &ingress,
        HookPermissions::new(false, true, true),
    );
    let invocation = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            1,
        )
        .unwrap();
    let reply = runtime
        .prepare_reply(&ingress, &binding, &invocation, 1)
        .unwrap();
    assert_eq!(
        runtime
            .record_transport_outcome(
                &ingress,
                &binding,
                &invocation,
                &reply,
                2,
                HookTransportOutcome::Written,
            )
            .unwrap_err(),
        GatewayError::Unauthorized
    );
    runtime
        .authorize_before_write(&ingress, &binding, &invocation, &reply, 2)
        .unwrap();
    let receipt = runtime
        .record_transport_outcome(
            &ingress,
            &binding,
            &invocation,
            &reply,
            3,
            HookTransportOutcome::Written,
        )
        .unwrap();
    assert_eq!(receipt.outcome(), HookTransportOutcome::Written);
    assert_eq!(runtime.receipts(), &[receipt]);

    let failed_call = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            4,
        )
        .unwrap();
    let failed_reply = runtime
        .prepare_reply(&ingress, &binding, &failed_call, 4)
        .unwrap();
    runtime
        .authorize_before_write(&ingress, &binding, &failed_call, &failed_reply, 4)
        .unwrap();
    let failed_receipt = runtime
        .record_transport_outcome(
            &ingress,
            &binding,
            &failed_call,
            &failed_reply,
            5,
            HookTransportOutcome::Failed,
        )
        .unwrap();
    assert_eq!(failed_receipt.outcome(), HookTransportOutcome::Failed);
}

#[test]
fn stale_or_expired_late_transport_is_recorded_as_unknown() {
    let ingress = ingress();
    let mut runtime = HookRuntime::new(limits());
    context(&mut runtime);
    let binding = bind(
        &mut runtime,
        &ingress,
        HookPermissions::new(false, true, true),
    );
    let invocation = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            1,
        )
        .unwrap();
    let reply = runtime
        .prepare_reply(&ingress, &binding, &invocation, 1)
        .unwrap();
    runtime
        .authorize_before_write(&ingress, &binding, &invocation, &reply, 1)
        .unwrap();
    runtime
        .register_context(
            HookContext::new("reviewed-source", 2, "changed context"),
            Some(1),
        )
        .unwrap();
    let receipt = runtime
        .record_transport_outcome(
            &ingress,
            &binding,
            &invocation,
            &reply,
            2,
            HookTransportOutcome::Written,
        )
        .unwrap();
    assert_eq!(receipt.outcome(), HookTransportOutcome::Unknown);
}

#[test]
fn context_and_binding_revocation_invalidate_prepared_output() {
    let ingress = ingress();
    let mut runtime = HookRuntime::new(limits());
    context(&mut runtime);
    let binding = bind(
        &mut runtime,
        &ingress,
        HookPermissions::new(false, true, true),
    );
    let invocation = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            1,
        )
        .unwrap();
    let reply = runtime
        .prepare_reply(&ingress, &binding, &invocation, 1)
        .unwrap();
    runtime
        .authorize_before_write(&ingress, &binding, &invocation, &reply, 1)
        .unwrap();
    runtime.revoke_context("reviewed-source", 1, 2).unwrap();
    let context_receipt = runtime
        .record_transport_outcome(
            &ingress,
            &binding,
            &invocation,
            &reply,
            2,
            HookTransportOutcome::Written,
        )
        .unwrap();
    assert_eq!(context_receipt.outcome(), HookTransportOutcome::Unknown);

    let mut second = HookRuntime::new(limits());
    context(&mut second);
    let second_binding = bind(
        &mut second,
        &ingress,
        HookPermissions::new(false, true, true),
    );
    let second_call = second
        .accept_event(
            &ingress,
            &second_binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            1,
        )
        .unwrap();
    let second_reply = second
        .prepare_reply(&ingress, &second_binding, &second_call, 1)
        .unwrap();
    second
        .authorize_before_write(&ingress, &second_binding, &second_call, &second_reply, 1)
        .unwrap();
    second.revoke_binding(&second_binding, 1, 2).unwrap();
    assert!(matches!(
        second.authorize_before_write(&ingress, &second_binding, &second_call, &second_reply, 3),
        Err(GatewayError::StaleHandle)
    ));
    let binding_receipt = second
        .record_transport_outcome(
            &ingress,
            &second_binding,
            &second_call,
            &second_reply,
            3,
            HookTransportOutcome::Written,
        )
        .unwrap();
    assert_eq!(binding_receipt.outcome(), HookTransportOutcome::Unknown);
}

#[test]
fn bounded_budgets_expiry_and_time_monotonicity_fail_closed() {
    assert_eq!(
        HookLimits::new(0, 1, 1, 1, 1, 1, 1),
        Err(GatewayError::InvalidInput("invalid hook limits"))
    );
    assert_eq!(
        HookLimits::new(usize::MAX, 1, usize::MAX, 1, 1, 1, 1),
        Err(GatewayError::InvalidInput("invalid hook limits"))
    );
    assert_eq!(
        HookLimits::new(1, 1, 1, usize::MAX, 1, 1, 1),
        Err(GatewayError::InvalidInput("invalid hook limits"))
    );

    let ingress = ingress();
    let tight_limits = HookLimits::new(1, 1, 1, 1, 4, 4, 10).unwrap();
    let mut runtime = HookRuntime::new(tight_limits);
    runtime
        .register_context(HookContext::new("source", 1, "text"), None)
        .unwrap();
    let binding = runtime
        .bind(
            &ingress,
            &IngressToken::new(TOKEN).unwrap(),
            "pet/default",
            config(HookPermissions::new(false, true, true), 10).with_context_source("source"),
            0,
        )
        .unwrap();
    let invocation = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", "1234"),
            1,
        )
        .unwrap();
    assert!(matches!(
        runtime.accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", "extra"),
            2,
        ),
        Err(GatewayError::InvalidInput("invalid text"))
    ));
    assert!(matches!(
        runtime.prepare_reply(&ingress, &binding, &invocation, 0),
        Err(GatewayError::ClockWentBackwards)
    ));
    assert!(matches!(
        runtime.prepare_reply(&ingress, &binding, &invocation, 10),
        Err(GatewayError::DeadlineExceeded)
    ));
    assert!(matches!(
        runtime.accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", "ok"),
            11,
        ),
        Err(GatewayError::DeadlineExceeded)
    ));
}

#[test]
fn cumulative_binding_and_invocation_budgets_do_not_reissue() {
    let ingress = ingress();
    let tight_limits = HookLimits::new(1, 1, 1, 1, 16, 16, 10).unwrap();
    let mut runtime = HookRuntime::new(tight_limits);
    runtime
        .register_context(HookContext::new("reviewed-source", 1, "text"), None)
        .unwrap();
    let binding = runtime
        .bind(
            &ingress,
            &IngressToken::new(TOKEN).unwrap(),
            "pet/default",
            config(HookPermissions::new(true, false, false), 10),
            0,
        )
        .unwrap();
    runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", "one"),
            1,
        )
        .unwrap();
    assert!(matches!(
        runtime.accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", "two"),
            2,
        ),
        Err(GatewayError::ReplayProtectionExhausted)
    ));
    runtime.revoke_binding(&binding, 1, 3).unwrap();
    assert!(matches!(
        runtime.bind(
            &ingress,
            &IngressToken::new(TOKEN).unwrap(),
            "pet/default",
            config(HookPermissions::new(true, false, false), 10),
            4,
        ),
        Err(GatewayError::ReplayProtectionExhausted)
    ));
}

#[test]
fn debug_output_redacts_prompt_context_and_credentials() {
    let ingress = ingress();
    let token = IngressToken::new(TOKEN).unwrap();
    let mut runtime = HookRuntime::new(limits());
    let sensitive_context = "sensitive-context-body";
    let sensitive_prompt = "sensitive-prompt-body";
    let context = HookContext::new("private-source-id", 1, sensitive_context);
    let context_debug = format!("{context:?}");
    let config_debug = format!(
        "{:?}",
        config(HookPermissions::new(true, true, true), 500)
            .with_context_source("private-source-id")
            .with_cli_session_id("private-session")
    );
    runtime.register_context(context, None).unwrap();
    let binding = runtime
        .bind(
            &ingress,
            &token,
            "pet/default",
            config(HookPermissions::new(true, true, true), 500)
                .with_context_source("private-source-id")
                .with_cli_session_id("private-session"),
            0,
        )
        .unwrap();
    let event = HookEvent::prompt_submitted("private-session", sensitive_prompt);
    let invocation = runtime
        .accept_event(&ingress, &binding, event.clone(), 1)
        .unwrap();
    let prompt = runtime
        .observe_prompt(&ingress, &binding, &invocation, 1)
        .unwrap();
    let reply = runtime
        .prepare_reply(&ingress, &binding, &invocation, 1)
        .unwrap();
    let output = format!(
        "{token:?} {event:?} {context_debug} {config_debug} {prompt:?} {reply:?} {binding:?} {invocation:?} {runtime:?}"
    );
    assert!(!output.contains(TOKEN));
    assert!(!output.contains(sensitive_context));
    assert!(!output.contains(sensitive_prompt));
    assert!(!output.contains("private-session"));
    assert!(!output.contains("private-source-id"));
}

#[test]
fn ordinary_prompt_never_becomes_a_user_source_or_assistant_reply() {
    let ingress = ingress();
    let mut runtime = HookRuntime::new(limits());
    context(&mut runtime);
    let binding = bind(
        &mut runtime,
        &ingress,
        HookPermissions::new(true, true, true),
    );
    let invocation = runtime
        .accept_event(
            &ingress,
            &binding,
            HookEvent::prompt_submitted("cli-session", PROMPT),
            1,
        )
        .unwrap();
    let prompt = runtime
        .observe_prompt(&ingress, &binding, &invocation, 1)
        .unwrap();
    let reply = runtime
        .prepare_reply(&ingress, &binding, &invocation, 1)
        .unwrap();
    assert_eq!(prompt.text(), PROMPT);
    assert_eq!(reply.role(), HookReplyRole::InjectedContext);
    assert_eq!(reply.text(), CONTEXT);
    assert!(runtime.receipts().is_empty());
}
