use std::io::{self, Write};

use moe_agent_runtime::{
    CallerSubject, ChannelId, ChannelScope, GatewayError, HookBinding, HookBindingConfig,
    HookContext, HookLimits, HookPermissions, HookRuntime, HookTransportOutcome, IngressToken,
    ReplyTarget, TrustedIngress,
};
use moe_cli_hooks::{
    ClaudeHookError, VerifiedCliVersion, accept_claude_submission, parse_user_prompt_submit,
    read_bounded, write_claude_context,
};
use moe_memory_core::{Namespace, PetIdentity};

fn fixture(permissions: HookPermissions) -> (TrustedIngress, HookRuntime, HookBinding) {
    let mut ingress = TrustedIngress::new();
    let identity = PetIdentity::new(
        Namespace::new("synthetic-workspace", "synthetic-user").unwrap(),
        "synthetic-pet",
    )
    .unwrap();
    ingress.register_route("buddy", identity).unwrap();
    let token = IngressToken::new("synthetic-hook-token").unwrap();
    ingress
        .install_credential(
            IngressToken::new("synthetic-hook-token").unwrap(),
            CallerSubject::new("synthetic-host").unwrap(),
            ["buddy".to_owned()],
            ChannelId::new("claude-hook").unwrap(),
            ChannelScope::new("synthetic-private").unwrap(),
            ReplyTarget::new("original-cli-hook").unwrap(),
        )
        .unwrap();
    let mut runtime = HookRuntime::new(HookLimits::default());
    runtime
        .register_context(
            HookContext::new("approved-source", 1, "synthetic-approved-context"),
            None,
        )
        .unwrap();
    let binding = runtime
        .bind(
            &ingress,
            &token,
            "buddy",
            HookBindingConfig::new(permissions, 100)
                .with_cli_session_id("synthetic-session")
                .with_context_source("approved-source"),
            0,
        )
        .unwrap();
    (ingress, runtime, binding)
}

fn submission(session: &str) -> moe_cli_hooks::ClaudePromptSubmission {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "session_id": session, "hook_event_name": "UserPromptSubmit", "prompt": "synthetic-unverified-prompt",
        "role": "user", "owner": "other-pet", "allow_model_input": true,
        "reply_target": "attacker", "cancelled": false, "transcript_path": "must-not-open",
    })).unwrap();
    let bytes = read_bounded(bytes.as_slice()).unwrap();
    parse_user_prompt_submit(VerifiedCliVersion::CLAUDE_CODE_2_1_286, &bytes).unwrap()
}

fn accept(
    runtime: &mut HookRuntime,
    ingress: &TrustedIngress,
    binding: &HookBinding,
) -> moe_agent_runtime::HookInvocation {
    accept_claude_submission(
        runtime,
        ingress,
        binding,
        submission("synthetic-session"),
        1,
    )
    .unwrap()
}

#[test]
fn authenticated_host_round_trip_preserves_unverified_role_and_only_emits_approved_context() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(true, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    let observed = runtime
        .observe_prompt(&ingress, &binding, &invocation, 1)
        .unwrap();
    assert_eq!(observed.text(), "synthetic-unverified-prompt");
    assert_eq!(format!("{observed:?}"), "UnverifiedPrompt(REDACTED)");
    let mut output = Vec::new();
    let receipt = write_claude_context(
        &mut runtime,
        &ingress,
        &binding,
        &invocation,
        &mut output,
        || 2,
    )
    .unwrap();
    assert_eq!(receipt.outcome(), HookTransportOutcome::Written);
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit", "additionalContext": "synthetic-approved-context"
        }})
    );
    assert_eq!(runtime.receipts().len(), 1);
}

#[test]
fn missing_trusted_cli_session_binding_fails_closed() {
    let (ingress, mut runtime, _) = fixture(HookPermissions::default());
    assert!(matches!(
        runtime.bind(
            &ingress,
            &IngressToken::new("synthetic-hook-token").unwrap(),
            "buddy",
            HookBindingConfig::new(HookPermissions::default(), 100),
            1,
        ),
        Err(GatewayError::MissingBinding)
    ));
}

#[test]
fn untrusted_cli_session_cannot_redirect_the_binding() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(true, true, true));
    let result = accept_claude_submission(
        &mut runtime,
        &ingress,
        &binding,
        submission("other-session"),
        1,
    );
    assert!(matches!(
        result,
        Err(ClaudeHookError::Runtime(GatewayError::Unauthorized))
    ));
    assert!(runtime.receipts().is_empty());
}

#[test]
fn json_permission_claims_cannot_enable_default_denied_binding() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::default());
    assert!(matches!(
        accept_claude_submission(
            &mut runtime,
            &ingress,
            &binding,
            submission("synthetic-session"),
            1
        ),
        Err(ClaudeHookError::Runtime(GatewayError::Unauthorized))
    ));
}

#[test]
fn separate_model_input_denial_emits_nothing_and_records_failure() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, false));
    let invocation = accept(&mut runtime, &ingress, &binding);
    let mut output = Vec::new();
    assert!(
        write_claude_context(
            &mut runtime,
            &ingress,
            &binding,
            &invocation,
            &mut output,
            || 2
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert_eq!(
        runtime.receipts()[0].outcome(),
        HookTransportOutcome::Failed
    );
}

#[test]
fn entry_revocation_after_acceptance_prevents_output() {
    let (mut ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    ingress.unregister_route("buddy").unwrap();
    let mut output = Vec::new();
    assert!(matches!(
        write_claude_context(
            &mut runtime,
            &ingress,
            &binding,
            &invocation,
            &mut output,
            || 2
        ),
        Err(ClaudeHookError::Runtime(GatewayError::Revoked))
    ));
    assert!(output.is_empty());
    assert_eq!(
        runtime.receipts()[0].outcome(),
        HookTransportOutcome::Failed
    );
}

#[test]
fn cancellation_before_output_is_not_overwritten_by_cleanup() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    runtime.cancel(&invocation, 2).unwrap();
    let mut output = Vec::new();
    assert!(
        write_claude_context(
            &mut runtime,
            &ingress,
            &binding,
            &invocation,
            &mut output,
            || 3
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert_eq!(runtime.receipts().len(), 1);
    assert_eq!(
        runtime.receipts()[0].outcome(),
        HookTransportOutcome::Cancelled
    );
}

#[test]
fn context_revision_change_prevents_stale_injection() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    runtime
        .register_context(
            HookContext::new("approved-source", 2, "new-context"),
            Some(1),
        )
        .unwrap();
    let mut output = Vec::new();
    assert!(
        write_claude_context(
            &mut runtime,
            &ingress,
            &binding,
            &invocation,
            &mut output,
            || 2
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert_eq!(
        runtime.receipts()[0].outcome(),
        HookTransportOutcome::Failed
    );
}

struct UnreliableWriter {
    budget: usize,
    bytes: Vec<u8>,
    fail_flush: bool,
}

impl Write for UnreliableWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.budget == 0 {
            return Err(io::Error::other("sensitive-transport-error"));
        }
        let count = bytes.len().min(self.budget);
        self.bytes.extend_from_slice(&bytes[..count]);
        self.budget -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::Error::other("sensitive-flush-error"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn zero_byte_write_failure_records_failed_without_diagnostics_on_stdout() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    let mut output = UnreliableWriter {
        budget: 0,
        bytes: Vec::new(),
        fail_flush: false,
    };
    let error = write_claude_context(
        &mut runtime,
        &ingress,
        &binding,
        &invocation,
        &mut output,
        || 2,
    )
    .unwrap_err();
    assert!(output.bytes.is_empty());
    assert!(!format!("{error:?} {error}").contains("sensitive-transport-error"));
    assert_eq!(
        runtime.receipts()[0].outcome(),
        HookTransportOutcome::Failed
    );
}

#[test]
fn partial_write_failure_records_unknown_and_never_appends_diagnostic_text() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    let mut output = UnreliableWriter {
        budget: 10,
        bytes: Vec::new(),
        fail_flush: false,
    };
    assert!(
        write_claude_context(
            &mut runtime,
            &ingress,
            &binding,
            &invocation,
            &mut output,
            || 2
        )
        .is_err()
    );
    assert_eq!(output.bytes, b"{\"hookSpec");
    assert_eq!(
        runtime.receipts()[0].outcome(),
        HookTransportOutcome::Unknown
    );
}

#[test]
fn valid_json_followed_by_flush_failure_is_unknown_not_no_injection() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    let mut output = UnreliableWriter {
        budget: usize::MAX,
        bytes: Vec::new(),
        fail_flush: true,
    };
    assert!(
        write_claude_context(
            &mut runtime,
            &ingress,
            &binding,
            &invocation,
            &mut output,
            || 2
        )
        .is_err()
    );
    assert!(serde_json::from_slice::<serde_json::Value>(&output.bytes).is_ok());
    assert_eq!(
        runtime.receipts()[0].outcome(),
        HookTransportOutcome::Unknown
    );
}

#[test]
fn completion_after_expiry_does_not_claim_cli_consumption() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    let mut times = [2, 3, 100].into_iter();
    let mut output = Vec::new();
    let receipt = write_claude_context(
        &mut runtime,
        &ingress,
        &binding,
        &invocation,
        &mut output,
        || times.next().unwrap(),
    )
    .unwrap();
    assert!(!output.is_empty());
    assert_eq!(receipt.outcome(), HookTransportOutcome::Unknown);
}

#[test]
fn backwards_clock_after_write_records_unknown_and_reports_clock_error() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    let mut times = [2, 3, 2].into_iter();
    let mut output = Vec::new();
    assert!(matches!(
        write_claude_context(
            &mut runtime,
            &ingress,
            &binding,
            &invocation,
            &mut output,
            || times.next().unwrap()
        ),
        Err(ClaudeHookError::Runtime(GatewayError::ClockWentBackwards))
    ));
    assert!(!output.is_empty());
    assert_eq!(
        runtime.receipts()[0].outcome(),
        HookTransportOutcome::Unknown
    );
}

#[test]
fn repeated_write_never_replays_an_already_terminal_call() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    let mut output = Vec::new();
    write_claude_context(
        &mut runtime,
        &ingress,
        &binding,
        &invocation,
        &mut output,
        || 2,
    )
    .unwrap();
    let expected = output.clone();
    assert!(
        write_claude_context(
            &mut runtime,
            &ingress,
            &binding,
            &invocation,
            &mut output,
            || 3
        )
        .is_err()
    );
    assert_eq!(output, expected);
    assert_eq!(runtime.receipts().len(), 1);
}

#[test]
fn failure_cleanup_cannot_forge_a_written_or_cancelled_receipt() {
    let (ingress, mut runtime, binding) = fixture(HookPermissions::new(false, true, true));
    let invocation = accept(&mut runtime, &ingress, &binding);
    for outcome in [
        HookTransportOutcome::Written,
        HookTransportOutcome::Cancelled,
    ] {
        assert!(runtime.fail_invocation(&invocation, outcome, 2).is_err());
    }
    assert!(runtime.receipts().is_empty());
    runtime
        .fail_invocation(&invocation, HookTransportOutcome::Failed, 2)
        .unwrap();
    assert!(
        runtime
            .fail_invocation(&invocation, HookTransportOutcome::Unknown, 2)
            .is_err()
    );
}
