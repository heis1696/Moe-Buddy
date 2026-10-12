use moe_agent_runtime::{
    CallerSubject, ChannelId, ChannelScope, CredentialId, GatewayError, IngressLimits,
    IngressToken, RawIngressCall, ReplyTarget, RequestIsolation, TrustedIngress, TrustedRequest,
    UnsupportedFeature,
};
use moe_memory_core::{Namespace, PetIdentity};

fn identity(workspace: &str, user: &str, pet: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new(workspace, user).unwrap(), pet).unwrap()
}

fn raw_call(token: &str, alias: &str, text: &str) -> RawIngressCall {
    RawIngressCall::new(IngressToken::new(token).unwrap(), alias, text)
}

fn install_full(
    ingress: &mut TrustedIngress,
    token: &str,
    subject: &str,
    alias: &str,
    channel: &str,
    scope: &str,
    reply: &str,
) -> CredentialId {
    ingress
        .install_credential(
            IngressToken::new(token).unwrap(),
            CallerSubject::new(subject).unwrap(),
            [alias.to_owned()],
            ChannelId::new(channel).unwrap(),
            ChannelScope::new(scope).unwrap(),
            ReplyTarget::new(reply).unwrap(),
        )
        .unwrap()
}

fn install_simple(
    ingress: &mut TrustedIngress,
    token: &str,
    subject: &str,
    alias: &str,
) -> CredentialId {
    install_full(
        ingress,
        token,
        subject,
        alias,
        "channel-a",
        "scope-a",
        "reply-a",
    )
}

fn admit_hinted(
    ingress: &mut TrustedIngress,
    keys: &mut Vec<RequestIsolation>,
    token: &str,
    alias: &str,
    channel: &str,
    scope: &str,
    reply: &str,
) -> CredentialId {
    let credential = install_full(ingress, token, "subject-a", alias, channel, scope, reply);
    let request = ingress
        .admit(
            raw_call(token, alias, "hello")
                .with_session_hint("conv-1")
                .with_client_request_id("cid-1"),
        )
        .unwrap();
    assert_eq!(request.credential(), credential);
    keys.push(request.isolation().clone());
    credential
}

struct Fixture {
    ingress: TrustedIngress,
}

impl Fixture {
    fn new() -> Self {
        let mut ingress = TrustedIngress::new();
        ingress
            .register_route("route-main", identity("ws", "user", "pet-a"))
            .unwrap();
        Self { ingress }
    }

    fn install(&mut self, token: &str, subject: &str, alias: &str) -> CredentialId {
        install_simple(&mut self.ingress, token, subject, alias)
    }

    fn admit(&mut self, call: RawIngressCall) -> TrustedRequest {
        self.ingress.admit(call).unwrap()
    }
}

#[test]
fn authentication_precedes_route_and_input_validation() {
    let mut fixture = Fixture::new();
    let garbled = RawIngressCall::new(IngressToken::new("no-such-token").unwrap(), "\u{7}x", "");
    assert_eq!(
        fixture.ingress.admit(garbled).unwrap_err(),
        GatewayError::Unauthorized
    );
    fixture.install("tok-a", "subject-a", "route-main");
    assert_eq!(
        fixture
            .ingress
            .admit(raw_call("tok-a", "route-none", ""))
            .unwrap_err(),
        GatewayError::UnknownRoute
    );
    assert_eq!(
        fixture
            .ingress
            .admit(raw_call("tok-a", "route-main", ""))
            .unwrap_err(),
        GatewayError::InvalidInput("invalid text")
    );
}

#[test]
fn malformed_alias_rejected_after_authentication() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    assert_eq!(
        fixture
            .ingress
            .admit(raw_call("tok-a", "\u{1}route", "hello"))
            .unwrap_err(),
        GatewayError::InvalidInput("invalid identifier")
    );
}

#[test]
fn route_without_allowlist_entry_rejected() {
    let mut fixture = Fixture::new();
    fixture
        .ingress
        .register_route("route-side", identity("ws", "user", "pet-z"))
        .unwrap();
    fixture.install("tok-a", "subject-a", "route-side");
    assert_eq!(
        fixture
            .ingress
            .admit(raw_call("tok-a", "route-main", "hello"))
            .unwrap_err(),
        GatewayError::RouteDenied
    );
    assert!(fixture.ingress.route_identity("route-main").is_some());
}

#[test]
fn unsupported_capabilities_explicitly_rejected() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    let features = [
        UnsupportedFeature::SystemDeveloperMessage,
        UnsupportedFeature::ToolDeclarations,
        UnsupportedFeature::MultimodalContent,
        UnsupportedFeature::StructuredOutput,
        UnsupportedFeature::Streaming,
    ];
    for feature in features {
        let call = raw_call("tok-a", "route-main", "hello").declares_unsupported(feature);
        assert_eq!(
            fixture.ingress.admit(call).unwrap_err(),
            GatewayError::UnsupportedCapability
        );
    }
    assert!(
        fixture
            .ingress
            .admit(raw_call("tok-a", "route-main", "hello"))
            .is_ok()
    );
}

#[test]
fn text_bounds_enforced() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    let max_text = "x".repeat(moe_agent_runtime::INGRESS_MAX_TEXT_CHARS);
    let over_text = "x".repeat(moe_agent_runtime::INGRESS_MAX_TEXT_CHARS + 1);
    assert!(
        fixture
            .ingress
            .admit(raw_call("tok-a", "route-main", &max_text))
            .is_ok()
    );
    for text in ["", "   ", "bad\u{0}text", over_text.as_str()] {
        assert_eq!(
            fixture
                .ingress
                .admit(raw_call("tok-a", "route-main", text))
                .unwrap_err(),
            GatewayError::InvalidInput("invalid text")
        );
    }
}

#[test]
fn optional_field_bounds_enforced() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    let long = "h".repeat(129);
    let ok_long = "h".repeat(128);
    for hint in ["", " spaced ", "nul\u{0}hint", long.as_str()] {
        let call = raw_call("tok-a", "route-main", "hello").with_session_hint(hint);
        assert_eq!(
            fixture.ingress.admit(call).unwrap_err(),
            GatewayError::InvalidInput("invalid identifier")
        );
    }
    for client_request_id in ["", long.as_str()] {
        let call =
            raw_call("tok-a", "route-main", "hello").with_client_request_id(client_request_id);
        assert_eq!(
            fixture.ingress.admit(call).unwrap_err(),
            GatewayError::InvalidInput("invalid identifier")
        );
    }
    let call = raw_call("tok-a", "route-main", "hello").with_session_hint(&ok_long);
    assert!(fixture.ingress.admit(call).is_ok());
    assert!(IngressToken::new("t".repeat(257).as_str()).is_err());
    assert!(IngressToken::new("t".repeat(256).as_str()).is_ok());
}

#[test]
fn hintless_repeats_stay_isolated() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    let first =
        fixture.admit(raw_call("tok-a", "route-main", "same text").with_client_request_id("cid-1"));
    let second =
        fixture.admit(raw_call("tok-a", "route-main", "same text").with_client_request_id("cid-1"));
    assert_eq!(first.client_request_id(), Some("cid-1"));
    assert_eq!(second.client_request_id(), Some("cid-1"));
    assert_eq!(first.sequence() + 1, second.sequence());
    assert_ne!(first.isolation(), second.isolation());
    match (first.isolation(), second.isolation()) {
        (
            RequestIsolation::Fresh {
                request: first_sequence,
                ..
            },
            RequestIsolation::Fresh {
                request: second_sequence,
                ..
            },
        ) => {
            assert_ne!(first_sequence, second_sequence);
        }
        _ => panic!("expected fresh isolation"),
    }
    assert!(first.validate(&fixture.ingress).is_ok());
    assert!(second.validate(&fixture.ingress).is_ok());
}

#[test]
fn hinted_isolation_separates_every_dimension() {
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route("route-main", identity("ws", "user", "pet-a"))
        .unwrap();
    ingress
        .register_route("route-pet", identity("ws", "user", "pet-b"))
        .unwrap();
    ingress
        .register_route("route-workspace", identity("ws-2", "user", "pet-a"))
        .unwrap();
    ingress
        .register_route("route-user", identity("ws", "user-2", "pet-a"))
        .unwrap();

    let mut keys = Vec::new();
    admit_hinted(
        &mut ingress,
        &mut keys,
        "tok-main",
        "route-main",
        "channel-a",
        "scope-a",
        "reply-a",
    );
    admit_hinted(
        &mut ingress,
        &mut keys,
        "tok-subject-b",
        "route-main",
        "channel-a",
        "scope-a",
        "reply-a",
    );
    admit_hinted(
        &mut ingress,
        &mut keys,
        "tok-subject-same",
        "route-main",
        "channel-a",
        "scope-a",
        "reply-a",
    );
    admit_hinted(
        &mut ingress,
        &mut keys,
        "tok-pet",
        "route-pet",
        "channel-a",
        "scope-a",
        "reply-a",
    );
    admit_hinted(
        &mut ingress,
        &mut keys,
        "tok-workspace",
        "route-workspace",
        "channel-a",
        "scope-a",
        "reply-a",
    );
    admit_hinted(
        &mut ingress,
        &mut keys,
        "tok-user",
        "route-user",
        "channel-a",
        "scope-a",
        "reply-a",
    );
    admit_hinted(
        &mut ingress,
        &mut keys,
        "tok-channel",
        "route-main",
        "channel-b",
        "scope-a",
        "reply-a",
    );
    admit_hinted(
        &mut ingress,
        &mut keys,
        "tok-scope",
        "route-main",
        "channel-a",
        "scope-b",
        "reply-a",
    );
    admit_hinted(
        &mut ingress,
        &mut keys,
        "tok-reply",
        "route-main",
        "channel-a",
        "scope-a",
        "reply-b",
    );

    assert_eq!(keys.len(), 9);
    for first_index in 0..keys.len() {
        for second_index in (first_index + 1)..keys.len() {
            assert_ne!(
                keys[first_index], keys[second_index],
                "isolation keys {first_index} and {second_index} merged"
            );
        }
    }

    let base = keys[0].clone();
    match &base {
        RequestIsolation::Hinted(key) => {
            assert_eq!(key.hint(), "conv-1");
            assert_eq!(key.identity(), &identity("ws", "user", "pet-a"));
            assert_eq!(key.channel().as_str(), "channel-a");
            assert_eq!(key.scope().as_str(), "scope-a");
        }
        RequestIsolation::Fresh { .. } => panic!("expected hinted isolation"),
    }

    let repeat = ingress
        .admit(
            raw_call("tok-main", "route-main", "different text")
                .with_session_hint("conv-1")
                .with_client_request_id("cid-2"),
        )
        .unwrap();
    assert_eq!(repeat.isolation(), &base);
    assert_eq!(repeat.client_request_id(), Some("cid-2"));
}

#[test]
fn client_request_id_is_metadata_only() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    let first =
        fixture.admit(raw_call("tok-a", "route-main", "body one").with_client_request_id("dup-1"));
    let second =
        fixture.admit(raw_call("tok-a", "route-main", "body two").with_client_request_id("dup-1"));
    assert_ne!(first.isolation(), second.isolation());
    assert_ne!(first.text(), second.text());
    assert_eq!(first.client_request_id(), second.client_request_id());
    assert!(second.validate(&fixture.ingress).is_ok());
}

#[test]
fn reply_target_frozen_from_original_request() {
    let mut fixture = Fixture::new();
    let credential = fixture.install("tok-a", "subject-a", "route-main");
    let original = fixture.admit(raw_call("tok-a", "route-main", "hello"));
    assert_eq!(original.reply_target().as_str(), "reply-a");
    install_full(
        &mut fixture.ingress,
        "tok-b",
        "subject-b",
        "route-main",
        "channel-a",
        "scope-a",
        "reply-b",
    );
    let later = fixture.admit(raw_call("tok-b", "route-main", "hello"));
    assert_eq!(later.reply_target().as_str(), "reply-b");
    assert_eq!(original.reply_target().as_str(), "reply-a");
    fixture.ingress.revoke_credential(credential).unwrap();
    assert_eq!(original.reply_target().as_str(), "reply-a");
    assert_eq!(
        original.validate(&fixture.ingress).unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(later.reply_target().as_str(), "reply-b");
}

#[test]
fn route_change_forces_readmission() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    let old = fixture.admit(raw_call("tok-a", "route-main", "hello"));
    assert_eq!(old.identity(), &identity("ws", "user", "pet-a"));
    fixture.ingress.unregister_route("route-main").unwrap();
    assert_eq!(
        old.validate(&fixture.ingress).unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        fixture
            .ingress
            .admit(raw_call("tok-a", "route-main", "hello"))
            .unwrap_err(),
        GatewayError::UnknownRoute
    );
    fixture
        .ingress
        .register_route("route-main", identity("ws", "user", "pet-b"))
        .unwrap();
    assert_eq!(
        old.validate(&fixture.ingress).unwrap_err(),
        GatewayError::Revoked
    );
    let renewed = fixture.admit(raw_call("tok-a", "route-main", "hello"));
    assert_eq!(renewed.identity(), &identity("ws", "user", "pet-b"));
}

#[test]
fn revocation_burns_token_for_every_subject() {
    let mut fixture = Fixture::new();
    let credential = fixture.install("tok-a", "subject-a", "route-main");
    let old = fixture.admit(raw_call("tok-a", "route-main", "hello"));
    assert!(old.validate(&fixture.ingress).is_ok());
    fixture.ingress.revoke_credential(credential).unwrap();
    assert_eq!(
        old.validate(&fixture.ingress).unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        fixture
            .ingress
            .admit(raw_call("tok-a", "route-main", "hello"))
            .unwrap_err(),
        GatewayError::Unauthorized
    );
    for subject in ["subject-a", "subject-b"] {
        assert_eq!(
            fixture
                .ingress
                .install_credential(
                    IngressToken::new("tok-a").unwrap(),
                    CallerSubject::new(subject).unwrap(),
                    ["route-main".to_owned()],
                    ChannelId::new("channel-a").unwrap(),
                    ChannelScope::new("scope-a").unwrap(),
                    ReplyTarget::new("reply-a").unwrap(),
                )
                .unwrap_err(),
            GatewayError::Revoked
        );
    }
    let mut foreign = TrustedIngress::new();
    foreign
        .register_route("route-other", identity("ws", "user", "pet-x"))
        .unwrap();
    let minted_elsewhere = install_simple(&mut foreign, "tok-x", "subject-x", "route-other");
    assert_eq!(
        fixture
            .ingress
            .revoke_credential(minted_elsewhere)
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    assert_eq!(
        fixture.ingress.revoke_credential(credential).unwrap_err(),
        GatewayError::InvalidInput("unknown credential")
    );
}

#[test]
fn foreign_instance_rejects_old_request() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    let request = fixture.admit(raw_call("tok-a", "route-main", "hello"));
    let mut twin = TrustedIngress::new();
    twin.register_route("route-main", identity("ws", "user", "pet-a"))
        .unwrap();
    install_simple(&mut twin, "tok-a", "subject-a", "route-main");
    assert_eq!(
        request.validate(&twin).unwrap_err(),
        GatewayError::ForeignRequest
    );
    assert!(request.validate(&fixture.ingress).is_ok());
    let third = TrustedIngress::new();
    assert_ne!(request.run_instance(), third.run_instance());
    assert_ne!(request.run_instance(), twin.run_instance());
}

#[test]
fn duplicate_token_never_reassigned() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    let generation = fixture.ingress.generation();
    for subject in ["subject-a", "subject-b"] {
        assert_eq!(
            fixture
                .ingress
                .install_credential(
                    IngressToken::new("tok-a").unwrap(),
                    CallerSubject::new(subject).unwrap(),
                    ["route-main".to_owned()],
                    ChannelId::new("channel-a").unwrap(),
                    ChannelScope::new("scope-a").unwrap(),
                    ReplyTarget::new("reply-a").unwrap(),
                )
                .unwrap_err(),
            GatewayError::InvalidInput("credential token already in use")
        );
    }
    assert_eq!(fixture.ingress.generation(), generation);
    install_simple(&mut fixture.ingress, "tok-b", "subject-b", "route-main");
    assert_eq!(fixture.ingress.generation(), generation + 1);
}

#[test]
fn capacity_failures_leave_state_intact() {
    let mut ingress = TrustedIngress::with_limits(IngressLimits::new(1, 1).unwrap()).unwrap();
    ingress
        .register_route("route-main", identity("ws", "user", "pet-a"))
        .unwrap();
    let generation = ingress.generation();
    assert_eq!(
        ingress
            .register_route("route-extra", identity("ws", "user", "pet-b"))
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    assert_eq!(ingress.generation(), generation);
    assert!(ingress.route_identity("route-main").is_some());
    assert!(ingress.route_identity("route-extra").is_none());
    install_simple(&mut ingress, "tok-a", "subject-a", "route-main");
    assert_eq!(
        ingress
            .install_credential(
                IngressToken::new("tok-b").unwrap(),
                CallerSubject::new("subject-b").unwrap(),
                ["route-main".to_owned()],
                ChannelId::new("channel-a").unwrap(),
                ChannelScope::new("scope-a").unwrap(),
                ReplyTarget::new("reply-a").unwrap(),
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    assert_eq!(ingress.generation(), generation + 1);
    assert!(
        ingress
            .admit(raw_call("tok-a", "route-main", "hello"))
            .is_ok()
    );
    assert!(
        ingress
            .admit(raw_call("tok-b", "route-main", "hello"))
            .is_err()
    );
}

#[test]
fn limit_construction_validated() {
    assert_eq!(
        IngressLimits::new(0, 1).unwrap_err(),
        GatewayError::InvalidInput("limits must be positive")
    );
    assert_eq!(
        IngressLimits::new(1, 0).unwrap_err(),
        GatewayError::InvalidInput("limits must be positive")
    );
    assert_eq!(
        IngressLimits::default(),
        IngressLimits::new(256, 256).unwrap()
    );
}

#[test]
fn allowlist_construction_validated() {
    let mut fixture = Fixture::new();
    let generation = fixture.ingress.generation();
    let attempts: [(GatewayError, Vec<String>); 2] = [
        (
            GatewayError::InvalidInput("empty alias allowlist"),
            Vec::new(),
        ),
        (
            GatewayError::InvalidInput("allowlist references unregistered route"),
            vec!["route-missing".to_owned()],
        ),
    ];
    for (expected, aliases) in attempts {
        assert_eq!(
            fixture
                .ingress
                .install_credential(
                    IngressToken::new("tok-a").unwrap(),
                    CallerSubject::new("subject-a").unwrap(),
                    aliases,
                    ChannelId::new("channel-a").unwrap(),
                    ChannelScope::new("scope-a").unwrap(),
                    ReplyTarget::new("reply-a").unwrap(),
                )
                .unwrap_err(),
            expected
        );
    }
    assert_eq!(
        CallerSubject::new(" padded ").unwrap_err(),
        GatewayError::InvalidInput("invalid identifier")
    );
    assert_eq!(fixture.ingress.generation(), generation);
}

#[test]
fn duplicate_route_registration_rejected() {
    let mut fixture = Fixture::new();
    let generation = fixture.ingress.generation();
    assert_eq!(
        fixture
            .ingress
            .register_route("route-main", identity("ws", "user", "pet-a"))
            .unwrap_err(),
        GatewayError::InvalidInput("route alias already registered")
    );
    assert_eq!(fixture.ingress.generation(), generation);
    assert_eq!(
        fixture.ingress.unregister_route("route-none").unwrap_err(),
        GatewayError::UnknownRoute
    );
    assert_eq!(fixture.ingress.generation(), generation);
}

#[test]
fn full_identity_bound_to_request() {
    let mut fixture = Fixture::new();
    fixture.install("tok-a", "subject-a", "route-main");
    let request = fixture.admit(raw_call("tok-a", "route-main", "hello"));
    assert_eq!(request.identity(), &identity("ws", "user", "pet-a"));
    assert_eq!(request.identity().namespace().workspace_id(), "ws");
    assert_eq!(request.identity().namespace().user_id(), "user");
    assert_eq!(request.identity().instance_id(), "pet-a");
    assert_eq!(request.alias(), "route-main");
    assert_eq!(request.subject().as_str(), "subject-a");
    assert_eq!(request.channel().as_str(), "channel-a");
    assert_eq!(request.scope().as_str(), "scope-a");
    assert_eq!(request.text(), "hello");
    assert_eq!(request.session_hint(), None);
    assert_eq!(request.generation(), fixture.ingress.generation());
    let admission = request.admission();
    assert_eq!(admission, request.admission());
}

#[test]
fn debug_output_redacts_secrets() {
    let mut fixture = Fixture::new();
    fixture.install("token-Secret-99", "subject-a", "route-main");
    let prepared = raw_call("token-Secret-99", "route-main", "BODY-secret-正文")
        .with_session_hint("hint-secret-7")
        .with_client_request_id("cid-debug");
    let debug_call = format!("{prepared:?}");
    let request = fixture.admit(prepared);
    let debug_request = format!("{request:?}");
    let debug_ingress = format!("{:?}", fixture.ingress);
    let debug_isolation = format!("{:?}", request.isolation());
    let debug_token = format!("{:?}", IngressToken::new("token-Secret-99").unwrap());
    for debug_output in [
        debug_call,
        debug_request,
        debug_ingress,
        debug_isolation,
        debug_token,
    ] {
        assert!(!debug_output.contains("token-Secret-99"));
        assert!(!debug_output.contains("BODY-secret-正文"));
        assert!(!debug_output.contains("hint-secret-7"));
    }
}

#[test]
fn error_display_contains_no_secrets() {
    let secrets = ["token-Secret-99", "BODY-secret-正文", "hint-secret-7"];
    let errors = [
        GatewayError::InvalidInput("invalid input"),
        GatewayError::Unauthorized,
        GatewayError::RouteDenied,
        GatewayError::UnknownRoute,
        GatewayError::MissingBinding,
        GatewayError::PresetNotFound,
        GatewayError::UnsupportedCapability,
        GatewayError::UnsupportedParameter,
        GatewayError::VersionConflict,
        GatewayError::CapacityExceeded,
        GatewayError::IdentifierExhausted,
        GatewayError::Revoked,
        GatewayError::ForeignRequest,
    ];
    for error in errors {
        let rendered = error.to_string();
        for secret in secrets {
            assert!(!rendered.contains(secret));
        }
    }
}
