use std::collections::BTreeMap;
use std::format;

use moe_agent_runtime::{
    CredentialRef, ExtensionValue, GatewayError, PresetConfig, PresetId, PresetPurpose,
    PresetRegistry,
};

fn config(name: &str, endpoint: &str) -> PresetConfig {
    PresetConfig::new(
        name,
        "openai-compatible",
        endpoint,
        Some(CredentialRef::new("vault://synthetic/test-key").unwrap()),
        "synthetic-model",
        BTreeMap::new(),
    )
    .unwrap()
}

fn id(value: &str) -> PresetId {
    PresetId::new(value).unwrap()
}

fn extensions(entries: &[(&str, ExtensionValue)]) -> BTreeMap<String, ExtensionValue> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

#[test]
fn resolves_only_explicit_purpose_binding_and_reports_missing_or_dangling() {
    let mut registry = PresetRegistry::new(4).unwrap();
    assert_eq!(
        registry.resolve(PresetPurpose::Chat).unwrap_err(),
        GatewayError::MissingBinding
    );
    let first_id = id("first");
    let second_id = id("second");
    let first_version = registry
        .create(
            first_id.clone(),
            config("First", "https://api.example.com/v1"),
        )
        .unwrap();
    registry
        .create(
            second_id.clone(),
            config("Second", "https://other.example.com/v1"),
        )
        .unwrap();
    registry
        .bind(PresetPurpose::Chat, first_id.clone())
        .unwrap();
    registry
        .bind(PresetPurpose::Vision, second_id.clone())
        .unwrap();
    assert_eq!(
        registry.resolve(PresetPurpose::Chat).unwrap().id(),
        &first_id
    );
    assert_eq!(
        registry.resolve(PresetPurpose::Vision).unwrap().id(),
        &second_id
    );
    registry.delete(&first_id, first_version).unwrap();
    assert_eq!(
        registry.resolve(PresetPurpose::Chat).unwrap_err(),
        GatewayError::PresetNotFound
    );
    assert_eq!(
        registry.bind(PresetPurpose::Voice, id("missing")),
        Err(GatewayError::PresetNotFound)
    );
}

#[test]
fn supports_all_eight_independent_purpose_bindings() {
    let purposes = [
        PresetPurpose::Chat,
        PresetPurpose::ImageGeneration,
        PresetPurpose::PersonaInterview,
        PresetPurpose::Vision,
        PresetPurpose::Voice,
        PresetPurpose::MemoryExtraction,
        PresetPurpose::Embedding,
        PresetPurpose::Rerank,
    ];
    let mut registry = PresetRegistry::new(8).unwrap();
    for (index, purpose) in purposes.into_iter().enumerate() {
        let preset_id = id(&format!("preset-{index}"));
        registry
            .create(
                preset_id.clone(),
                config("Synthetic", "https://api.example.com"),
            )
            .unwrap();
        registry.bind(purpose, preset_id.clone()).unwrap();
        assert_eq!(registry.resolve(purpose).unwrap().id(), &preset_id);
    }
}

#[test]
fn chat_binding_and_credential_do_not_implicitly_bind_embedding_or_rerank() {
    let mut registry = PresetRegistry::new(3).unwrap();
    let chat_id = id("chat");
    let embedding_id = id("embedding");
    let rerank_id = id("rerank");
    registry
        .create(
            chat_id.clone(),
            config("Chat", "https://chat.example.com/v1"),
        )
        .unwrap();
    registry
        .create(
            embedding_id.clone(),
            config("Embedding", "https://embedding.example.com/v1"),
        )
        .unwrap();
    registry
        .create(
            rerank_id.clone(),
            config("Rerank", "https://rerank.example.com/v1"),
        )
        .unwrap();
    registry.bind(PresetPurpose::Chat, chat_id.clone()).unwrap();

    assert_eq!(
        registry.resolve(PresetPurpose::Embedding).unwrap_err(),
        GatewayError::MissingBinding
    );
    assert_eq!(
        registry.resolve(PresetPurpose::Rerank).unwrap_err(),
        GatewayError::MissingBinding
    );
    assert!(
        registry
            .resolve(PresetPurpose::Chat)
            .unwrap()
            .config()
            .credential_ref()
            .is_some()
    );

    registry
        .bind(PresetPurpose::Embedding, embedding_id.clone())
        .unwrap();
    registry
        .bind(PresetPurpose::Rerank, rerank_id.clone())
        .unwrap();
    assert_eq!(
        registry.resolve(PresetPurpose::Chat).unwrap().id(),
        &chat_id
    );
    assert_eq!(
        registry.resolve(PresetPurpose::Embedding).unwrap().id(),
        &embedding_id
    );
    assert_eq!(
        registry.resolve(PresetPurpose::Rerank).unwrap().id(),
        &rerank_id
    );
}

#[test]
fn updates_use_cas_and_failures_leave_resolved_state_unchanged() {
    let mut registry = PresetRegistry::new(2).unwrap();
    let preset_id = id("chat");
    let version = registry
        .create(
            preset_id.clone(),
            config("Before", "https://api.example.com"),
        )
        .unwrap();
    registry
        .bind(PresetPurpose::Chat, preset_id.clone())
        .unwrap();
    let rejected = registry.update(
        &preset_id,
        version + 1,
        config("Rejected", "https://reject.example.com"),
    );
    assert_eq!(rejected, Err(GatewayError::VersionConflict));
    let resolved = registry.resolve(PresetPurpose::Chat).unwrap();
    assert_eq!(resolved.version(), version);
    assert_eq!(resolved.config().display_name(), "Before");
    let new_version = registry
        .update(
            &preset_id,
            version,
            config("After", "https://api.example.com"),
        )
        .unwrap();
    assert_ne!(new_version, version);
    assert_eq!(
        registry.resolve(PresetPurpose::Chat).unwrap().version(),
        new_version
    );
}

#[test]
fn delete_and_recreate_never_reuses_a_version() {
    let mut registry = PresetRegistry::new(1).unwrap();
    let preset_id = id("stable-id");
    let old_version = registry
        .create(preset_id.clone(), config("Old", "https://api.example.com"))
        .unwrap();
    registry
        .bind(PresetPurpose::Chat, preset_id.clone())
        .unwrap();
    registry.delete(&preset_id, old_version).unwrap();
    let new_version = registry
        .create(preset_id.clone(), config("New", "https://api.example.com"))
        .unwrap();
    assert!(new_version > old_version);
    assert_eq!(
        registry.resolve(PresetPurpose::Chat).unwrap().version(),
        new_version
    );
    assert_eq!(
        registry.update(
            &preset_id,
            old_version,
            config("Stale", "https://api.example.com")
        ),
        Err(GatewayError::VersionConflict)
    );
    assert_eq!(
        registry.resolve(PresetPurpose::Chat).unwrap().version(),
        new_version
    );
}

#[test]
fn registry_capacity_is_bounded_and_rejection_is_atomic() {
    assert!(PresetRegistry::new(0).is_err());
    assert!(PresetRegistry::new(257).is_err());
    let mut registry = PresetRegistry::new(1).unwrap();
    registry
        .create(id("one"), config("One", "https://api.example.com"))
        .unwrap();
    assert_eq!(
        registry.create(id("two"), config("Two", "https://api.example.com")),
        Err(GatewayError::CapacityExceeded)
    );
    assert_eq!(registry.len(), 1);
}

#[test]
fn endpoint_validation_accepts_https_and_literal_loopback_http_only() {
    for endpoint in [
        "https://api.example.com/v1/chat/completions",
        "https://127.0.0.1:8443/v1",
        "http://127.0.0.1:8080/v1",
        "http://[::1]:8080/v1",
    ] {
        assert!(
            PresetConfig::new(
                "Test",
                "test-protocol",
                endpoint,
                None,
                "model",
                BTreeMap::new(),
            )
            .is_ok(),
            "{endpoint}"
        );
    }
}

#[test]
fn endpoint_validation_rejects_confusing_or_unsafe_urls() {
    for endpoint in [
        "http://example.com",
        "http://localhost",
        "http://192.168.1.4",
        "https://user:secret@example.com",
        "https://example.com?query=1",
        "https://example.com#fragment",
        "https://example.com/path with spaces",
        "https://example.com\\@127.0.0.1",
        "https://2130706433",
        "https://127.1",
        "https://0177.0.0.1",
        "https://0x7f.0.0.1",
        "https://0X7F.0.0.1",
        "https://example.com:00080",
        "https://example.com:65536",
        "https://example.com:443:444",
        "https://[::1",
        "https://example..com",
        "https://example.com/%Q0",
        "https://example.com/line\nbreak",
    ] {
        assert!(
            PresetConfig::new(
                "Test",
                "test-protocol",
                endpoint,
                None,
                "model",
                BTreeMap::new(),
            )
            .is_err(),
            "{endpoint:?}"
        );
    }
}

#[test]
fn extensions_require_allowlisted_names_types_and_ranges() {
    for (key, value) in [
        ("temperature", ExtensionValue::Number(0.7)),
        ("top_p", ExtensionValue::Number(1.0)),
        ("max_tokens", ExtensionValue::Integer(4096)),
        ("reasoning_effort", ExtensionValue::Text("medium".into())),
    ] {
        assert!(
            PresetConfig::new(
                "Test",
                "protocol",
                "https://api.example.com",
                None,
                "model",
                extensions(&[(key, value)]),
            )
            .is_ok()
        );
    }
    for (key, value) in [
        ("temperature", ExtensionValue::Number(f64::NAN)),
        ("temperature", ExtensionValue::Number(f64::INFINITY)),
        ("temperature", ExtensionValue::Number(2.01)),
        ("top_p", ExtensionValue::Number(-0.01)),
        ("max_tokens", ExtensionValue::Integer(0)),
        ("reasoning_effort", ExtensionValue::Text("extreme".into())),
        ("model", ExtensionValue::Text("attacker-model".into())),
        ("messages", ExtensionValue::Text("attacker-input".into())),
        ("headers", ExtensionValue::Text("Authorization: x".into())),
        ("stream", ExtensionValue::Boolean(true)),
        ("tools", ExtensionValue::Boolean(true)),
        (
            "endpoint",
            ExtensionValue::Text("https://evil.example".into()),
        ),
        ("identity", ExtensionValue::Text("other-pet".into())),
    ] {
        assert!(
            PresetConfig::new(
                "Test",
                "protocol",
                "https://api.example.com",
                None,
                "model",
                extensions(&[(key, value)]),
            )
            .is_err(),
            "{key}"
        );
    }
}

#[test]
fn debug_redacts_configuration_and_credential_reference() {
    let secret_reference = "vault://synthetic/SECRET-REFERENCE";
    let config = PresetConfig::new(
        "PRIVATE-DISPLAY-NAME",
        "private-protocol",
        "https://private.example.com/private-path",
        Some(CredentialRef::new(secret_reference).unwrap()),
        "PRIVATE-MODEL-NAME",
        extensions(&[("reasoning_effort", ExtensionValue::Text("high".into()))]),
    )
    .unwrap();
    let config_debug = format!("{config:?}");
    let reference_debug = format!("{:?}", config.credential_ref().unwrap());
    let value_debug = format!("{:?}", config.extensions().get("reasoning_effort").unwrap());
    let mut registry = PresetRegistry::new(1).unwrap();
    let preset_id = id("safe-id");
    let version = registry.create(preset_id.clone(), config).unwrap();
    registry.bind(PresetPurpose::Chat, preset_id).unwrap();
    let resolved_debug = format!("{:?}", registry.resolve(PresetPurpose::Chat).unwrap());
    let registry_debug = format!("{registry:?}");
    for output in [
        &config_debug,
        &reference_debug,
        &value_debug,
        &resolved_debug,
        &registry_debug,
    ] {
        for private in [
            secret_reference,
            "PRIVATE-DISPLAY-NAME",
            "private-protocol",
            "private-path",
            "PRIVATE-MODEL-NAME",
        ] {
            assert!(!output.contains(private));
        }
    }
    assert!(resolved_debug.contains(&format!("{version}")));
}

#[test]
fn invalid_configuration_and_failed_mutations_do_not_change_state() {
    let mut registry = PresetRegistry::new(2).unwrap();
    let preset_id = id("atomic");
    let version = registry
        .create(
            preset_id.clone(),
            config("Stable", "https://api.example.com"),
        )
        .unwrap();
    registry
        .bind(PresetPurpose::Chat, preset_id.clone())
        .unwrap();
    let invalid = PresetConfig::new(
        "Rejected",
        "protocol",
        "http://8.8.8.8",
        None,
        "model",
        BTreeMap::new(),
    );
    assert!(invalid.is_err());
    assert_eq!(
        registry.resolve(PresetPurpose::Chat).unwrap().version(),
        version
    );
    assert_eq!(
        registry.delete(&preset_id, version + 1),
        Err(GatewayError::VersionConflict)
    );
    assert_eq!(
        registry
            .resolve(PresetPurpose::Chat)
            .unwrap()
            .config()
            .display_name(),
        "Stable"
    );
}
