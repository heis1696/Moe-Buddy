//! Trusted, bounded companion profiles. Persistence and identifier entropy stay
//! in the host adapter; this module owns validation and revision transitions.

use crate::{MAX_SAFE_INTEGER, ModelSettings};
use std::error::Error;
use std::fmt;

pub const MAX_BUDDIES: usize = 8;
pub const MAX_BUDDY_ID_BYTES: usize = 64;
pub const MAX_BUDDY_LABEL_CHARS: usize = 64;
pub const MAX_PERSONA_CHARS: usize = 4_096;

const DEFAULT_BUDDY_ID: &str = "offline-companion";
const DEFAULT_LABEL: &str = "离线验收伙伴";
const DEFAULT_PERSONA: &str =
    "离线验收人设：本机离线验收伙伴；回答必须标明离线验收，不冒充真实模型。";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BuddyId(String);

impl BuddyId {
    pub fn new(value: impl Into<String>) -> Result<Self, BuddyError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_BUDDY_ID_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            || !value.as_bytes()[0].is_ascii_alphanumeric()
            || value == "."
            || value == ".."
        {
            return Err(BuddyError::InvalidId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuddyModelConfiguration {
    endpoint: String,
    model: String,
    preset_id: String,
}

impl BuddyModelConfiguration {
    pub fn new(
        endpoint: impl Into<String>,
        model: impl Into<String>,
        preset_id: impl Into<String>,
    ) -> Result<Self, BuddyError> {
        let endpoint = endpoint.into();
        let model = model.into();
        let preset_id = preset_id.into();
        if endpoint.is_empty()
            || endpoint.len() > 2_048
            || endpoint.chars().any(char::is_control)
            || model.is_empty()
            || model.chars().count() > 128
            || model.chars().any(char::is_control)
        {
            return Err(BuddyError::InvalidConfiguration);
        }
        validate_identifier(&preset_id, 64).map_err(|_| BuddyError::InvalidConfiguration)?;
        Ok(Self {
            endpoint,
            model,
            preset_id,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn preset_id(&self) -> &str {
        &self.preset_id
    }

    pub fn to_model_settings(&self, persona: &str) -> ModelSettings {
        ModelSettings {
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            persona: persona.to_owned(),
            allow_transmission: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuddyProfile {
    id: BuddyId,
    label: String,
    profile_revision: u64,
    persona_revision: u64,
    model_revision: u64,
    persona: String,
    model: Option<BuddyModelConfiguration>,
}

impl BuddyProfile {
    pub fn new(
        id: BuddyId,
        label: impl Into<String>,
        persona: impl Into<String>,
    ) -> Result<Self, BuddyError> {
        let label = validate_label(label.into())?;
        let persona = validate_persona(persona.into())?;
        Ok(Self {
            id,
            label,
            profile_revision: 1,
            persona_revision: 1,
            model_revision: 1,
            persona,
            model: None,
        })
    }

    pub fn id(&self) -> &BuddyId {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn profile_revision(&self) -> u64 {
        self.profile_revision
    }

    pub fn persona_revision(&self) -> u64 {
        self.persona_revision
    }

    pub fn model_revision(&self) -> u64 {
        self.model_revision
    }

    pub fn persona(&self) -> &str {
        &self.persona
    }

    pub fn model(&self) -> Option<&BuddyModelConfiguration> {
        self.model.as_ref()
    }

    pub fn with_persisted_state(
        mut self,
        profile_revision: u64,
        persona_revision: u64,
        model_revision: u64,
        model: Option<BuddyModelConfiguration>,
    ) -> Result<Self, BuddyError> {
        validate_revision(profile_revision)?;
        validate_revision(persona_revision)?;
        validate_revision(model_revision)?;
        if persona_revision > model_revision
            || model_revision > profile_revision
            || (model.is_some() && model_revision < 2)
            || (model.is_none() && (model_revision != 1 || persona_revision != 1))
        {
            return Err(BuddyError::InvalidRegistry);
        }
        self.profile_revision = profile_revision;
        self.persona_revision = persona_revision;
        self.model_revision = model_revision;
        self.model = model;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuddySummary {
    pub id: String,
    pub label: String,
    pub profile_revision: u64,
    pub persona_revision: u64,
    pub model_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuddyRegistrySnapshot {
    pub active_buddy_id: Option<String>,
    pub revision: u64,
    pub buddies: Vec<BuddySummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuddyRegistry {
    active_buddy_id: BuddyId,
    revision: u64,
    buddies: Vec<BuddyProfile>,
}

impl BuddyRegistry {
    pub fn offline_default() -> Self {
        let profile = BuddyProfile::new(
            BuddyId::new(DEFAULT_BUDDY_ID).expect("the built-in identity is valid"),
            DEFAULT_LABEL,
            DEFAULT_PERSONA,
        )
        .expect("the built-in profile is valid");
        Self {
            active_buddy_id: profile.id.clone(),
            revision: 1,
            buddies: vec![profile],
        }
    }

    pub fn restore(
        active_buddy_id: BuddyId,
        revision: u64,
        buddies: Vec<BuddyProfile>,
    ) -> Result<Self, BuddyError> {
        validate_revision(revision)?;
        if buddies.is_empty() || buddies.len() > MAX_BUDDIES {
            return Err(BuddyError::InvalidRegistry);
        }
        let mut found_active = false;
        for (index, profile) in buddies.iter().enumerate() {
            validate_profile(profile)?;
            if profile.profile_revision > revision
                || profile.persona_revision > revision
                || profile.model_revision > revision
            {
                return Err(BuddyError::InvalidRegistry);
            }
            if profile.id == active_buddy_id {
                found_active = true;
            }
            if buddies[..index]
                .iter()
                .any(|previous| previous.id == profile.id)
            {
                return Err(BuddyError::InvalidRegistry);
            }
        }
        if !found_active {
            return Err(BuddyError::InvalidRegistry);
        }
        Ok(Self {
            active_buddy_id,
            revision,
            buddies,
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn active_buddy_id(&self) -> &BuddyId {
        &self.active_buddy_id
    }

    pub fn buddies(&self) -> &[BuddyProfile] {
        &self.buddies
    }

    pub fn profile(&self, id: &BuddyId) -> Option<&BuddyProfile> {
        self.buddies.iter().find(|profile| profile.id == *id)
    }

    pub fn snapshot(&self) -> BuddyRegistrySnapshot {
        BuddyRegistrySnapshot {
            active_buddy_id: Some(self.active_buddy_id.0.clone()),
            revision: self.revision,
            buddies: self
                .buddies
                .iter()
                .map(|profile| BuddySummary {
                    id: profile.id.0.clone(),
                    label: profile.label.clone(),
                    profile_revision: profile.profile_revision,
                    persona_revision: profile.persona_revision,
                    model_revision: profile.model_revision,
                })
                .collect(),
        }
    }

    pub fn create(
        &mut self,
        expected_revision: u64,
        id: BuddyId,
        label: impl Into<String>,
        persona: impl Into<String>,
    ) -> Result<&BuddyProfile, BuddyError> {
        self.check_revision(expected_revision)?;
        if self.buddies.len() >= MAX_BUDDIES {
            return Err(BuddyError::CapacityExceeded);
        }
        if self.profile(&id).is_some() {
            return Err(BuddyError::DuplicateId);
        }
        let profile = BuddyProfile::new(id, label, persona)?;
        let next_revision = next_revision(self.revision)?;
        self.active_buddy_id = profile.id.clone();
        self.buddies.push(profile);
        self.revision = next_revision;
        Ok(self.buddies.last().expect("profile was appended"))
    }

    pub fn switch(&mut self, expected_revision: u64, id: &BuddyId) -> Result<(), BuddyError> {
        self.check_revision(expected_revision)?;
        if self.profile(id).is_none() {
            return Err(BuddyError::NotFound);
        }
        let next_revision = next_revision(self.revision)?;
        self.active_buddy_id = id.clone();
        self.revision = next_revision;
        Ok(())
    }

    pub fn rename(
        &mut self,
        expected_revision: u64,
        id: &BuddyId,
        expected_profile_revision: u64,
        label: impl Into<String>,
    ) -> Result<(), BuddyError> {
        self.check_revision(expected_revision)?;
        let label = validate_label(label.into())?;
        let index = self.profile_index(id)?;
        let profile = &self.buddies[index];
        if profile.profile_revision != expected_profile_revision {
            return Err(BuddyError::StaleProfile);
        }
        let next_registry_revision = next_revision(self.revision)?;
        let next_profile_revision = next_revision(profile.profile_revision)?;
        self.buddies[index].label = label;
        self.buddies[index].profile_revision = next_profile_revision;
        self.revision = next_registry_revision;
        Ok(())
    }

    pub fn commit_configuration(
        &mut self,
        expected_revision: u64,
        id: &BuddyId,
        expected_profile_revision: u64,
        persona: impl Into<String>,
        model: BuddyModelConfiguration,
    ) -> Result<(), BuddyError> {
        self.check_revision(expected_revision)?;
        let persona = validate_persona(persona.into())?;
        let index = self.profile_index(id)?;
        let profile = &self.buddies[index];
        if profile.profile_revision != expected_profile_revision {
            return Err(BuddyError::StaleProfile);
        }
        let next_registry_revision = next_revision(self.revision)?;
        let next_profile_revision = next_revision(profile.profile_revision)?;
        let next_model_revision = next_revision(profile.model_revision)?;
        let next_persona_revision = if profile.persona != persona {
            next_revision(profile.persona_revision)?
        } else {
            profile.persona_revision
        };
        let profile = &mut self.buddies[index];
        profile.persona = persona;
        profile.model = Some(model);
        profile.profile_revision = next_profile_revision;
        profile.persona_revision = next_persona_revision;
        profile.model_revision = next_model_revision;
        self.revision = next_registry_revision;
        Ok(())
    }

    fn check_revision(&self, expected_revision: u64) -> Result<(), BuddyError> {
        if self.revision == expected_revision {
            Ok(())
        } else {
            Err(BuddyError::StaleRegistry)
        }
    }

    fn profile_index(&self, id: &BuddyId) -> Result<usize, BuddyError> {
        self.buddies
            .iter()
            .position(|profile| profile.id == *id)
            .ok_or(BuddyError::NotFound)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuddyError {
    InvalidId,
    InvalidLabel,
    InvalidPersona,
    InvalidConfiguration,
    InvalidRegistry,
    DuplicateId,
    NotFound,
    StaleRegistry,
    StaleProfile,
    CapacityExceeded,
    RevisionExhausted,
}

impl fmt::Display for BuddyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidId => "buddy identifier is invalid",
            Self::InvalidLabel => "buddy label is invalid",
            Self::InvalidPersona => "buddy persona is invalid",
            Self::InvalidConfiguration => "buddy model configuration is invalid",
            Self::InvalidRegistry => "buddy registry is invalid",
            Self::DuplicateId => "buddy identifier already exists",
            Self::NotFound => "buddy was not found",
            Self::StaleRegistry => "buddy registry revision is stale",
            Self::StaleProfile => "buddy profile revision is stale",
            Self::CapacityExceeded => "buddy registry capacity is exhausted",
            Self::RevisionExhausted => "buddy revision is exhausted",
        })
    }
}

impl Error for BuddyError {}

pub(crate) fn default_persona() -> &'static str {
    DEFAULT_PERSONA
}

fn validate_profile(profile: &BuddyProfile) -> Result<(), BuddyError> {
    BuddyId::new(profile.id.0.clone())?;
    validate_label(profile.label.clone())?;
    validate_persona(profile.persona.clone())?;
    validate_revision(profile.profile_revision)?;
    validate_revision(profile.persona_revision)?;
    validate_revision(profile.model_revision)?;
    if profile.persona_revision > profile.model_revision
        || profile.model_revision > profile.profile_revision
        || (profile.model.is_some() && profile.model_revision < 2)
        || (profile.model.is_none()
            && (profile.model_revision != 1 || profile.persona_revision != 1))
    {
        return Err(BuddyError::InvalidRegistry);
    }
    Ok(())
}

fn validate_label(value: String) -> Result<String, BuddyError> {
    let count = value.chars().count();
    if value.trim().is_empty()
        || count > MAX_BUDDY_LABEL_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(BuddyError::InvalidLabel);
    }
    Ok(value)
}

fn validate_persona(value: String) -> Result<String, BuddyError> {
    if !valid_persona(&value) {
        return Err(BuddyError::InvalidPersona);
    }
    Ok(value)
}

pub(crate) fn valid_persona(value: &str) -> bool {
    let count = value.chars().count();
    !value.chars().all(char::is_whitespace)
        && count <= MAX_PERSONA_CHARS
        && !value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}

fn validate_identifier(value: &str, max_bytes: usize) -> Result<(), BuddyError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || !value.as_bytes()[0].is_ascii_alphanumeric()
    {
        return Err(BuddyError::InvalidConfiguration);
    }
    Ok(())
}

fn validate_revision(revision: u64) -> Result<(), BuddyError> {
    if revision == 0 || revision > MAX_SAFE_INTEGER {
        Err(BuddyError::InvalidRegistry)
    } else {
        Ok(())
    }
}

fn next_revision(revision: u64) -> Result<u64, BuddyError> {
    revision
        .checked_add(1)
        .filter(|next| *next <= MAX_SAFE_INTEGER)
        .ok_or(BuddyError::RevisionExhausted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_profile_rejects_impossible_version_ordering() {
        let profile = BuddyProfile::new(
            BuddyId::new("version-order-fixture").unwrap(),
            "synthetic",
            "synthetic persona",
        )
        .unwrap();
        let model = BuddyModelConfiguration::new(
            "http://127.0.0.1:9/v1/chat/completions",
            "synthetic-model",
            "synthetic-preset",
        )
        .unwrap();
        for (profile_revision, persona_revision, model_revision) in
            [(1, 3, 2), (2, 1, 3), (3, 3, 2)]
        {
            assert_eq!(
                profile.clone().with_persisted_state(
                    profile_revision,
                    persona_revision,
                    model_revision,
                    Some(model.clone()),
                ),
                Err(BuddyError::InvalidRegistry),
            );
        }
        let renamed = profile.with_persisted_state(4, 2, 3, Some(model)).unwrap();
        let restored = BuddyRegistry::restore(renamed.id().clone(), 5, vec![renamed]).unwrap();
        assert_eq!(restored.buddies()[0].profile_revision(), 4);
    }

    #[test]
    fn registry_rejects_duplicate_and_stale_creation_without_advancing() {
        let mut registry = BuddyRegistry::offline_default();
        let original = registry.snapshot();
        let id = BuddyId::new("buddy-test-1").unwrap();
        registry
            .create(original.revision, id.clone(), "first", "first persona")
            .unwrap();
        let committed = registry.snapshot();
        assert_eq!(committed.revision, original.revision + 1);
        assert_eq!(registry.active_buddy_id(), &id);

        assert_eq!(
            registry.create(committed.revision, id, "duplicate", "persona"),
            Err(BuddyError::DuplicateId)
        );
        assert_eq!(
            registry.create(
                original.revision,
                BuddyId::new("buddy-test-2").unwrap(),
                "stale",
                "persona"
            ),
            Err(BuddyError::StaleRegistry)
        );
        assert_eq!(registry.snapshot(), committed);
    }

    #[test]
    fn selection_and_configuration_advance_independent_profile_versions() {
        let mut registry = BuddyRegistry::offline_default();
        let offline_id = registry.active_buddy_id().clone();
        let second = BuddyId::new("buddy-test-2").unwrap();
        registry
            .create(1, second.clone(), "second", "persona one")
            .unwrap();
        let after_create = registry.revision();
        registry.switch(after_create, &offline_id).unwrap();
        let after_switch = registry.revision();
        assert!(after_switch > after_create);

        let profile_revision = registry.profile(&second).unwrap().profile_revision();
        registry
            .commit_configuration(
                after_switch,
                &second,
                profile_revision,
                "persona two",
                BuddyModelConfiguration::new(
                    "http://127.0.0.1:8080/v1/chat/completions",
                    "synthetic-model",
                    "synthetic-preset",
                )
                .unwrap(),
            )
            .unwrap();
        let configured = registry.profile(&second).unwrap().clone();
        assert_eq!(configured.profile_revision(), profile_revision + 1);
        assert_eq!(configured.persona_revision(), 2);
        assert_eq!(configured.model_revision(), 2);
        assert_eq!(configured.persona(), "persona two");
        assert_eq!(configured.model().unwrap().preset_id(), "synthetic-preset");

        assert_eq!(
            registry.rename(after_switch, &second, profile_revision, "stale"),
            Err(BuddyError::StaleRegistry)
        );
        assert_eq!(
            registry.rename(
                registry.revision(),
                &second,
                profile_revision,
                "stale profile"
            ),
            Err(BuddyError::StaleProfile)
        );
        assert_eq!(registry.profile(&second).unwrap(), &configured);
    }

    #[test]
    fn persona_validation_preserves_multiline_text_and_rejects_other_controls() {
        for persona in ["first\nsecond", "first\rsecond", "first\tsecond", "a\r\nb"] {
            assert_eq!(validate_persona(persona.to_owned()).unwrap(), persona);
        }
        assert_eq!(
            validate_persona(" \r\n\t".to_owned()),
            Err(BuddyError::InvalidPersona)
        );
        assert_eq!(
            validate_persona("a\u{000b}b".to_owned()),
            Err(BuddyError::InvalidPersona)
        );
        assert_eq!(
            validate_persona("x".repeat(MAX_PERSONA_CHARS + 1)),
            Err(BuddyError::InvalidPersona)
        );
    }

    #[test]
    fn registry_capacity_is_fixed_and_new_profiles_start_without_a_model() {
        let mut registry = BuddyRegistry::offline_default();
        for index in 1..MAX_BUDDIES {
            let revision = registry.revision();
            registry
                .create(
                    revision,
                    BuddyId::new(format!("buddy-{index}")).unwrap(),
                    format!("buddy {index}"),
                    format!("persona {index}"),
                )
                .unwrap();
        }
        let new_revision = registry.revision();
        assert_eq!(
            registry.create(
                new_revision,
                BuddyId::new("buddy-over-limit").unwrap(),
                "extra",
                "persona"
            ),
            Err(BuddyError::CapacityExceeded)
        );
        assert_eq!(registry.buddies().len(), MAX_BUDDIES);
        assert!(
            registry
                .buddies()
                .iter()
                .all(|profile| profile.model().is_none()
                    || profile.id() == registry.active_buddy_id())
        );
    }
}
