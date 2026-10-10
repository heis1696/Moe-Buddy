use moe_memory_core::{
    Category, CommandResult, Epistemic, Lifecycle, MemoryCommand, MemoryDraft, MemoryError,
    MemoryScope, MemoryService, Namespace, PetIdentity, Sensitivity,
};

fn identity() -> PetIdentity {
    PetIdentity::new(Namespace::new("home", "user").unwrap(), "alice").unwrap()
}

fn roundtrip(command: MemoryCommand) {
    let bytes = command.to_bytes().unwrap();
    let decoded = MemoryCommand::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, command);
    assert_eq!(decoded.to_bytes().unwrap(), bytes);
}

#[test]
fn all_command_variants_roundtrip_without_exposing_storage() {
    let pet = identity();
    let mut service = MemoryService::new();
    let source_command = MemoryCommand::register_source(
        MemoryScope::Private(pet.clone()),
        "message",
        1,
        Epistemic::UserStatement,
    );
    roundtrip(source_command.clone());
    roundtrip(MemoryCommand::register_source(
        MemoryScope::Shared(pet.namespace().clone()),
        "reference",
        u64::MAX,
        Epistemic::Reference,
    ));
    let CommandResult::Source(source) = source_command.execute(&mut service).unwrap() else {
        panic!()
    };
    let draft = MemoryDraft::new(
        "秘密 🐾",
        Category::UserProfile,
        Epistemic::UserStatement,
        vec![source],
    )
    .with_lifecycle(Lifecycle::ShortTerm)
    .with_sensitivity(Sensitivity::Private)
    .with_expiration(500);
    let create = MemoryCommand::create(pet.clone(), draft.clone(), 100);
    roundtrip(create.clone());
    assert!(!format!("{create:?}").contains("秘密"));
    let CommandResult::Write(receipt) = create.execute(&mut service).unwrap() else {
        panic!()
    };
    roundtrip(MemoryCommand::update(
        pet.clone(),
        receipt.id(),
        1,
        draft,
        200,
    ));
    roundtrip(MemoryCommand::delete(pet, receipt.id(), 1, 300));
}

#[test]
fn all_enum_values_have_explicit_lossless_wire_tags() {
    for category in [
        Category::Identity,
        Category::UserProfile,
        Category::Relationship,
        Category::Episode,
        Category::WorkingContext,
        Category::Knowledge,
        Category::Procedure,
        Category::ToolDefinition,
        Category::Protocol,
    ] {
        for epistemic in [
            Epistemic::UserStatement,
            Epistemic::Observation,
            Epistemic::Inference,
            Epistemic::Fiction,
            Epistemic::Reference,
            Epistemic::AuthoritativeState,
        ] {
            for lifecycle in [Lifecycle::ShortTerm, Lifecycle::LongTerm] {
                for sensitivity in [
                    Sensitivity::Normal,
                    Sensitivity::Private,
                    Sensitivity::Restricted,
                ] {
                    roundtrip(MemoryCommand::create(
                        identity(),
                        MemoryDraft::new("candidate", category, epistemic, vec![])
                            .with_lifecycle(lifecycle)
                            .with_sensitivity(sensitivity),
                        0,
                    ));
                }
            }
        }
    }
}

#[test]
fn decoder_rejects_every_truncated_prefix_trailing_bytes_and_unknown_tags() {
    let command = MemoryCommand::register_source(
        MemoryScope::Private(identity()),
        "message",
        1,
        Epistemic::UserStatement,
    );
    let valid = command.to_bytes().unwrap();
    for length in 0..valid.len() {
        assert!(MemoryCommand::from_bytes(&valid[..length]).is_err());
    }
    let mut trailing = valid.clone();
    trailing.push(0);
    assert!(MemoryCommand::from_bytes(&trailing).is_err());
    for offset in [0, 1, 2, valid.len() - 1] {
        let mut invalid = valid.clone();
        invalid[offset] = 255;
        assert!(MemoryCommand::from_bytes(&invalid).is_err());
    }
}

#[test]
fn decoder_bounds_lengths_before_allocating_and_rejects_invalid_utf8() {
    let mut excessive = vec![1, 0, 0];
    excessive.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(MemoryCommand::from_bytes(&excessive).is_err());
    assert!(MemoryCommand::from_bytes(&vec![0; 128 * 1024 + 1]).is_err());
    let mut invalid_utf8 = MemoryCommand::register_source(
        MemoryScope::Shared(Namespace::new("h", "u").unwrap()),
        "s",
        1,
        Epistemic::UserStatement,
    )
    .to_bytes()
    .unwrap();
    invalid_utf8[11] = 255;
    assert!(MemoryCommand::from_bytes(&invalid_utf8).is_err());
}

#[test]
fn wire_version_one_has_a_fixed_golden_representation() {
    let command = MemoryCommand::register_source(
        MemoryScope::Shared(Namespace::new("h", "u").unwrap()),
        "s",
        1,
        Epistemic::UserStatement,
    );
    let mut expected = vec![1, 0, 1];
    for value in *b"hus" {
        expected.extend_from_slice(&1_u64.to_le_bytes());
        expected.push(value);
    }
    expected.extend_from_slice(&1_u64.to_le_bytes());
    expected.push(0);
    assert_eq!(command.to_bytes().unwrap(), expected);
}

#[test]
fn command_execution_cannot_bypass_core_policy() {
    let mut service = MemoryService::new();
    let command = MemoryCommand::create(
        identity(),
        MemoryDraft::new(
            "protected",
            Category::Identity,
            Epistemic::Inference,
            vec![],
        ),
        100,
    );
    assert_eq!(
        command.execute(&mut service),
        Err(MemoryError::ProtectedContent)
    );
}
