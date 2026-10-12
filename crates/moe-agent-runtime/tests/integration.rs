use std::collections::BTreeMap;

use moe_agent_runtime::{
    CallerSubject, ChannelId, ChannelScope, GatewayError, IngressLimits, IngressToken,
    PresetConfig, PresetId, PresetPurpose, PresetRegistry, RawIngressCall, ReplyTarget,
    TrustedIngress,
};
use moe_memory_core::{Namespace, PetIdentity};

fn ingress(limits: IngressLimits) -> TrustedIngress {
    let mut ingress = TrustedIngress::with_limits(limits).unwrap();
    ingress
        .register_route(
            "buddy/synthetic",
            PetIdentity::new(
                Namespace::new("workspace", "owner").unwrap(),
                "pet-instance",
            )
            .unwrap(),
        )
        .unwrap();
    ingress
}

fn install(
    ingress: &mut TrustedIngress,
    token: &str,
) -> Result<moe_agent_runtime::CredentialId, GatewayError> {
    ingress.install_credential(
        IngressToken::new(token).unwrap(),
        CallerSubject::new("external-caller-not-owner").unwrap(),
        ["buddy/synthetic".to_owned()],
        ChannelId::new("external-api").unwrap(),
        ChannelScope::new("private-thread").unwrap(),
        ReplyTarget::new("original-target").unwrap(),
    )
}

fn call(token: &str) -> RawIngressCall {
    RawIngressCall::new(
        IngressToken::new(token).unwrap(),
        "buddy/synthetic",
        "synthetic input",
    )
}

#[test]
fn retired_tokens_are_bounded_and_revocation_remains_available() {
    let limits = IngressLimits::with_issuance_limit(1, 1, 2).unwrap();
    let mut ingress = ingress(limits);
    let first = install(&mut ingress, "synthetic-token-one").unwrap();
    ingress.revoke_credential(first).unwrap();
    let second = install(&mut ingress, "synthetic-token-two").unwrap();
    let request = ingress.admit(call("synthetic-token-two")).unwrap();
    let generation = ingress.generation();
    assert_eq!(
        install(&mut ingress, "synthetic-token-three"),
        Err(GatewayError::CapacityExceeded)
    );
    assert_eq!(ingress.generation(), generation);
    assert!(request.validate(&ingress).is_ok());
    ingress.revoke_credential(second).unwrap();
    let generation = ingress.generation();
    assert_eq!(
        install(&mut ingress, "synthetic-token-three"),
        Err(GatewayError::CapacityExceeded)
    );
    assert_eq!(ingress.generation(), generation);
    assert_eq!(
        install(&mut ingress, "synthetic-token-one"),
        Err(GatewayError::Revoked)
    );
    assert_eq!(request.validate(&ingress), Err(GatewayError::Revoked));
}

#[test]
fn issuance_limits_reject_invalid_and_overflowing_configurations() {
    assert_eq!(
        IngressLimits::new(1, usize::MAX),
        Err(GatewayError::InvalidInput("credential limit too large"))
    );
    assert!(IngressLimits::with_issuance_limit(1, 2, 1).is_err());
    assert!(IngressLimits::with_issuance_limit(1, 1, 0).is_err());
    assert_eq!(IngressLimits::default().max_issued_credentials(), 512);
}

#[test]
fn isolation_keys_include_the_runtime_for_hinted_and_fresh_requests() {
    let mut first = ingress(IngressLimits::default());
    let mut second = ingress(IngressLimits::default());
    install(&mut first, "same-synthetic-token").unwrap();
    install(&mut second, "same-synthetic-token").unwrap();
    let first_fresh = first.admit(call("same-synthetic-token")).unwrap();
    let second_fresh = second.admit(call("same-synthetic-token")).unwrap();
    assert_eq!(first_fresh.sequence(), second_fresh.sequence());
    assert_ne!(first_fresh.isolation(), second_fresh.isolation());
    let first_hinted = first
        .admit(call("same-synthetic-token").with_session_hint("same-hint"))
        .unwrap();
    let second_hinted = second
        .admit(call("same-synthetic-token").with_session_hint("same-hint"))
        .unwrap();
    assert_ne!(first_hinted.isolation(), second_hinted.isolation());
    assert_eq!(
        first_hinted.validate(&second),
        Err(GatewayError::ForeignRequest)
    );
}

#[test]
fn raw_debug_does_not_echo_unvalidated_identifiers() {
    let raw = RawIngressCall::new(
        IngressToken::new("synthetic-secret-token").unwrap(),
        "unvalidated-secret-alias",
        "synthetic-secret-body",
    )
    .with_session_hint("synthetic-secret-hint")
    .with_client_request_id("synthetic-secret-correlation");
    let debug = format!("{raw:?}");
    for secret in [
        "synthetic-secret-token",
        "unvalidated-secret-alias",
        "synthetic-secret-body",
        "synthetic-secret-hint",
        "synthetic-secret-correlation",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn whitespace_tokens_cannot_be_installed() {
    for token in [
        " ",
        " leading",
        "trailing ",
        "embedded space",
        "embedded\tcontrol",
    ] {
        assert!(IngressToken::new(token).is_err());
    }
}

#[test]
fn switching_upstream_presets_does_not_change_pet_or_caller_identity() {
    let mut ingress = ingress(IngressLimits::default());
    install(&mut ingress, "synthetic-token").unwrap();
    let request = ingress.admit(call("synthetic-token")).unwrap();
    let mut presets = PresetRegistry::new(2).unwrap();
    let first = PresetId::new("first-provider").unwrap();
    let second = PresetId::new("second-provider").unwrap();
    for id in [&first, &second] {
        let config = PresetConfig::new(
            "Synthetic",
            "chat-completions",
            "https://api.example.com/v1",
            None,
            "synthetic-model",
            BTreeMap::new(),
        )
        .unwrap();
        presets.create(id.clone(), config).unwrap();
    }
    presets.bind(PresetPurpose::Chat, first.clone()).unwrap();
    assert_eq!(presets.resolve(PresetPurpose::Chat).unwrap().id(), &first);
    let pet = request.identity().clone();
    presets.bind(PresetPurpose::Chat, second.clone()).unwrap();
    assert_eq!(presets.resolve(PresetPurpose::Chat).unwrap().id(), &second);
    assert_eq!(request.identity(), &pet);
    assert_eq!(request.subject().as_str(), "external-caller-not-owner");
    assert_ne!(request.subject().as_str(), pet.namespace().user_id());
    assert_eq!(request.reply_target().as_str(), "original-target");
    request.validate(&ingress).unwrap();
}

#[test]
fn credential_handles_cannot_revoke_a_different_live_runtime() {
    let mut first = ingress(IngressLimits::default());
    let mut second = ingress(IngressLimits::default());
    let first_credential = install(&mut first, "first-runtime-token").unwrap();
    let second_credential = second
        .install_credential(
            IngressToken::new("second-runtime-token").unwrap(),
            CallerSubject::new("different-caller").unwrap(),
            ["buddy/synthetic".to_owned()],
            ChannelId::new("different-channel").unwrap(),
            ChannelScope::new("different-scope").unwrap(),
            ReplyTarget::new("different-reply-target").unwrap(),
        )
        .unwrap();
    let first_request = first.admit(call("first-runtime-token")).unwrap();
    let second_request = second.admit(call("second-runtime-token")).unwrap();
    assert_eq!(first_credential.value(), second_credential.value());
    let first_generation = first.generation();
    let second_generation = second.generation();
    assert_eq!(
        second.revoke_credential(first_credential),
        Err(GatewayError::ForeignRequest)
    );
    assert_eq!(
        first.revoke_credential(second_credential),
        Err(GatewayError::ForeignRequest)
    );
    assert_eq!(first.generation(), first_generation);
    assert_eq!(second.generation(), second_generation);
    assert_ne!(first_credential, second_credential);
    first_request.validate(&first).unwrap();
    second_request.validate(&second).unwrap();
    assert_eq!(
        second
            .admit(call("second-runtime-token"))
            .unwrap()
            .credential(),
        second_credential
    );
    assert_eq!(
        first
            .admit(call("first-runtime-token"))
            .unwrap()
            .credential(),
        first_credential
    );
}

#[test]
fn a_retired_handle_cannot_revoke_credentials_after_runtime_rebuild() {
    let old_credential = {
        let mut old = ingress(IngressLimits::with_issuance_limit(1, 1, 1).unwrap());
        let credential = install(&mut old, "retired-runtime-token").unwrap();
        old.revoke_credential(credential).unwrap();
        assert_eq!(
            install(&mut old, "replacement-token"),
            Err(GatewayError::CapacityExceeded)
        );
        credential
    };
    let mut rebuilt = ingress(IngressLimits::new(1, 1).unwrap());
    let new_credential = install(&mut rebuilt, "rebuilt-runtime-token").unwrap();
    let request = rebuilt.admit(call("rebuilt-runtime-token")).unwrap();
    let generation = rebuilt.generation();
    assert_eq!(old_credential.value(), new_credential.value());
    assert_eq!(
        rebuilt.revoke_credential(old_credential),
        Err(GatewayError::ForeignRequest)
    );
    assert_eq!(rebuilt.generation(), generation);
    request.validate(&rebuilt).unwrap();
    rebuilt.revoke_credential(new_credential).unwrap();
    assert_eq!(request.validate(&rebuilt), Err(GatewayError::Revoked));
    install(&mut rebuilt, "another-valid-token").unwrap();
}
