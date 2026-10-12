use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use crate::{GatewayError, validation};

const MAX_PRESETS: usize = 256;
const MAX_EXTENSIONS: usize = 16;

pub type PresetVersion = u64;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PresetId(String);

impl PresetId {
    pub fn new(value: impl Into<String>) -> Result<Self, GatewayError> {
        let value = value.into();
        validation::identifier(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PresetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("PresetId").field(&self.0).finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CredentialRef(String);

impl CredentialRef {
    pub fn new(value: impl Into<String>) -> Result<Self, GatewayError> {
        let value = value.into();
        validation::text(&value, 256)?;
        if value.trim() != value || value.chars().any(char::is_control) {
            return Err(GatewayError::InvalidInput("invalid credential reference"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialRef([redacted])")
    }
}

#[derive(Clone, PartialEq)]
pub enum ExtensionValue {
    Number(f64),
    Integer(i64),
    Boolean(bool),
    Text(String),
}

impl fmt::Debug for ExtensionValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExtensionValue([redacted])")
    }
}

pub struct PresetConfig {
    display_name: String,
    protocol: String,
    endpoint: String,
    credential_ref: Option<CredentialRef>,
    default_model: String,
    extensions: BTreeMap<String, ExtensionValue>,
}

impl PresetConfig {
    pub fn new(
        display_name: impl Into<String>,
        protocol: impl Into<String>,
        endpoint: impl Into<String>,
        credential_ref: Option<CredentialRef>,
        default_model: impl Into<String>,
        extensions: BTreeMap<String, ExtensionValue>,
    ) -> Result<Self, GatewayError> {
        let display_name = display_name.into();
        let protocol = protocol.into();
        let endpoint = endpoint.into();
        let default_model = default_model.into();
        validation::text(&display_name, 128)?;
        validation::identifier(&protocol)?;
        validation::text(&default_model, 256)?;
        validate_endpoint(&endpoint)?;
        validate_extensions(&extensions)?;
        Ok(Self {
            display_name,
            protocol,
            endpoint,
            credential_ref,
            default_model,
            extensions,
        })
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn credential_ref(&self) -> Option<&CredentialRef> {
        self.credential_ref.as_ref()
    }

    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    pub fn extensions(&self) -> &BTreeMap<String, ExtensionValue> {
        &self.extensions
    }
}

impl fmt::Debug for PresetConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PresetConfig([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetPurpose {
    Chat,
    ImageGeneration,
    PersonaInterview,
    Vision,
    Voice,
    MemoryExtraction,
    Embedding,
    Rerank,
}

impl PresetPurpose {
    const fn index(self) -> usize {
        match self {
            Self::Chat => 0,
            Self::ImageGeneration => 1,
            Self::PersonaInterview => 2,
            Self::Vision => 3,
            Self::Voice => 4,
            Self::MemoryExtraction => 5,
            Self::Embedding => 6,
            Self::Rerank => 7,
        }
    }
}

struct Preset {
    id: PresetId,
    version: PresetVersion,
    config: PresetConfig,
}

pub struct PresetRegistry {
    configuration: Arc<()>,
    capacity: usize,
    next_version: PresetVersion,
    presets: BTreeMap<PresetId, Preset>,
    bindings: [Option<PresetId>; 8],
}

impl PresetRegistry {
    pub fn new(capacity: usize) -> Result<Self, GatewayError> {
        if !(1..=MAX_PRESETS).contains(&capacity) {
            return Err(GatewayError::InvalidInput("invalid preset capacity"));
        }
        Ok(Self {
            configuration: Arc::new(()),
            capacity,
            next_version: 1,
            presets: BTreeMap::new(),
            bindings: std::array::from_fn(|_| None),
        })
    }

    pub fn create(
        &mut self,
        id: PresetId,
        config: PresetConfig,
    ) -> Result<PresetVersion, GatewayError> {
        if self.presets.contains_key(&id) {
            return Err(GatewayError::VersionConflict);
        }
        if self.presets.len() == self.capacity {
            return Err(GatewayError::CapacityExceeded);
        }
        let (version, next_version) = self.allocate_version()?;
        self.presets.insert(
            id.clone(),
            Preset {
                id,
                version,
                config,
            },
        );
        self.next_version = next_version;
        self.configuration = Arc::new(());
        Ok(version)
    }

    pub fn update(
        &mut self,
        id: &PresetId,
        expected_version: PresetVersion,
        config: PresetConfig,
    ) -> Result<PresetVersion, GatewayError> {
        let preset = self.presets.get(id).ok_or(GatewayError::PresetNotFound)?;
        if preset.version != expected_version {
            return Err(GatewayError::VersionConflict);
        }
        let (version, next_version) = self.allocate_version()?;
        let preset = self
            .presets
            .get_mut(id)
            .ok_or(GatewayError::PresetNotFound)?;
        preset.version = version;
        preset.config = config;
        self.next_version = next_version;
        self.configuration = Arc::new(());
        Ok(version)
    }

    pub fn delete(
        &mut self,
        id: &PresetId,
        expected_version: PresetVersion,
    ) -> Result<(), GatewayError> {
        let preset = self.presets.get(id).ok_or(GatewayError::PresetNotFound)?;
        if preset.version != expected_version {
            return Err(GatewayError::VersionConflict);
        }
        let (_, next_version) = self.allocate_version()?;
        self.presets.remove(id);
        self.next_version = next_version;
        self.configuration = Arc::new(());
        Ok(())
    }

    pub fn bind(&mut self, purpose: PresetPurpose, id: PresetId) -> Result<(), GatewayError> {
        if !self.presets.contains_key(&id) {
            return Err(GatewayError::PresetNotFound);
        }
        self.bindings[purpose.index()] = Some(id);
        self.configuration = Arc::new(());
        Ok(())
    }

    pub fn unbind(&mut self, purpose: PresetPurpose) {
        self.bindings[purpose.index()] = None;
        self.configuration = Arc::new(());
    }

    pub(crate) fn configuration(&self) -> &Arc<()> {
        &self.configuration
    }

    pub fn resolve(&self, purpose: PresetPurpose) -> Result<ResolvedPreset<'_>, GatewayError> {
        let id = self.bindings[purpose.index()]
            .as_ref()
            .ok_or(GatewayError::MissingBinding)?;
        let preset = self.presets.get(id).ok_or(GatewayError::PresetNotFound)?;
        Ok(ResolvedPreset { preset })
    }

    pub fn len(&self) -> usize {
        self.presets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    fn allocate_version(&self) -> Result<(PresetVersion, PresetVersion), GatewayError> {
        let version = self.next_version;
        let next_version = version
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        Ok((version, next_version))
    }
}

impl fmt::Debug for PresetRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PresetRegistry")
            .field("capacity", &self.capacity)
            .field("preset_count", &self.presets.len())
            .finish()
    }
}

#[derive(Clone, Copy)]
pub struct ResolvedPreset<'a> {
    preset: &'a Preset,
}

impl ResolvedPreset<'_> {
    pub fn id(&self) -> &PresetId {
        &self.preset.id
    }

    pub fn version(&self) -> PresetVersion {
        self.preset.version
    }

    pub fn config(&self) -> &PresetConfig {
        &self.preset.config
    }
}

impl fmt::Debug for ResolvedPreset<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedPreset")
            .field("id", &self.preset.id)
            .field("version", &self.preset.version)
            .finish()
    }
}

fn validate_extensions(extensions: &BTreeMap<String, ExtensionValue>) -> Result<(), GatewayError> {
    if extensions.len() > MAX_EXTENSIONS {
        return Err(GatewayError::CapacityExceeded);
    }
    for (name, value) in extensions {
        validation::identifier(name)?;
        match (name.as_str(), value) {
            ("temperature", ExtensionValue::Number(number))
                if number.is_finite() && (0.0..=2.0).contains(number) => {}
            ("top_p", ExtensionValue::Number(number))
                if number.is_finite() && (0.0..=1.0).contains(number) => {}
            ("frequency_penalty" | "presence_penalty", ExtensionValue::Number(number))
                if number.is_finite() && (-2.0..=2.0).contains(number) => {}
            ("max_tokens", ExtensionValue::Integer(number)) if (1..=131_072).contains(number) => {}
            ("seed", ExtensionValue::Integer(number)) if *number >= 0 => {}
            ("reasoning_effort", ExtensionValue::Text(value))
                if matches!(value.as_str(), "low" | "medium" | "high") => {}
            _ => return Err(GatewayError::UnsupportedParameter),
        }
    }
    Ok(())
}

fn validate_endpoint(endpoint: &str) -> Result<(), GatewayError> {
    validation::text(endpoint, 2048)?;
    if !endpoint.is_ascii()
        || endpoint.chars().any(char::is_whitespace)
        || endpoint
            .chars()
            .any(|character| matches!(character, '?' | '#' | '\\'))
    {
        return Err(GatewayError::InvalidInput("invalid endpoint"));
    }

    let (scheme, remainder) = if let Some(remainder) = endpoint.strip_prefix("https://") {
        ("https", remainder)
    } else if let Some(remainder) = endpoint.strip_prefix("http://") {
        ("http", remainder)
    } else {
        return Err(GatewayError::InvalidInput("invalid endpoint"));
    };
    let authority_end = remainder.find('/').unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let path = &remainder[authority_end..];
    if authority.is_empty() || authority.contains(['@', '%']) {
        return Err(GatewayError::InvalidInput("invalid endpoint"));
    }
    validate_path(path)?;

    let (host, port) = parse_authority(authority)?;
    if scheme == "http" {
        let address = host
            .parse::<IpAddr>()
            .map_err(|_| GatewayError::InvalidInput("invalid endpoint"))?;
        if !address.is_loopback() {
            return Err(GatewayError::InvalidInput("invalid endpoint"));
        }
    }
    if port.is_some_and(|port| port == 0) {
        return Err(GatewayError::InvalidInput("invalid endpoint"));
    }
    Ok(())
}

fn parse_authority(authority: &str) -> Result<(&str, Option<u16>), GatewayError> {
    if let Some(ipv6) = authority.strip_prefix('[') {
        let closing = ipv6
            .find(']')
            .ok_or(GatewayError::InvalidInput("invalid endpoint"))?;
        let address = &ipv6[..closing];
        address
            .parse::<Ipv6Addr>()
            .map_err(|_| GatewayError::InvalidInput("invalid endpoint"))?;
        let remainder = &ipv6[closing + 1..];
        let port = if remainder.is_empty() {
            None
        } else if let Some(port) = remainder.strip_prefix(':') {
            Some(parse_port(port)?)
        } else {
            return Err(GatewayError::InvalidInput("invalid endpoint"));
        };
        return Ok((address, port));
    }

    let (host, port) = match authority.split_once(':') {
        Some((host, port)) if !host.contains(':') => (host, Some(parse_port(port)?)),
        Some(_) => return Err(GatewayError::InvalidInput("invalid endpoint")),
        None => (authority, None),
    };
    if host.is_empty() || host.len() > 253 || host.ends_with('.') {
        return Err(GatewayError::InvalidInput("invalid endpoint"));
    }
    if let Ok(address) = host.parse::<Ipv4Addr>() {
        if address.to_string() != host {
            return Err(GatewayError::InvalidInput("invalid endpoint"));
        }
        return Ok((host, port));
    }
    let mut numeric_address = true;
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(GatewayError::InvalidInput("invalid endpoint"));
        }
        let numeric_or_hex = label.bytes().all(|byte| byte.is_ascii_digit())
            || (label
                .get(..2)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("0x"))
                && !label[2..].is_empty()
                && label[2..].bytes().all(|byte| byte.is_ascii_hexdigit()));
        numeric_address &= numeric_or_hex;
    }
    if numeric_address {
        return Err(GatewayError::InvalidInput("invalid endpoint"));
    }
    Ok((host, port))
}

fn parse_port(value: &str) -> Result<u16, GatewayError> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(GatewayError::InvalidInput("invalid endpoint"));
    }
    value
        .parse::<u16>()
        .map_err(|_| GatewayError::InvalidInput("invalid endpoint"))
}

fn validate_path(path: &str) -> Result<(), GatewayError> {
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let valid = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'/' | b'-'
                    | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
            );
        if valid {
            index += 1;
            continue;
        }
        if byte == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            index += 3;
            continue;
        }
        return Err(GatewayError::InvalidInput("invalid endpoint"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PresetConfig {
        PresetConfig::new(
            "Synthetic",
            "chat-completions",
            "https://api.example.com/v1",
            None,
            "synthetic-model",
            BTreeMap::new(),
        )
        .unwrap()
    }

    #[test]
    fn version_exhaustion_never_publishes_mutations() {
        let mut registry = PresetRegistry::new(2).unwrap();
        let id = PresetId::new("synthetic").unwrap();
        let version = registry.create(id.clone(), config()).unwrap();
        registry.bind(PresetPurpose::Chat, id.clone()).unwrap();
        registry.next_version = u64::MAX;
        assert_eq!(
            registry.update(&id, version, config()),
            Err(GatewayError::IdentifierExhausted)
        );
        assert_eq!(
            registry.delete(&id, version),
            Err(GatewayError::IdentifierExhausted)
        );
        assert_eq!(
            registry.create(PresetId::new("other").unwrap(), config()),
            Err(GatewayError::IdentifierExhausted)
        );
        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.resolve(PresetPurpose::Chat).unwrap().version(),
            version
        );
        assert_eq!(registry.next_version, u64::MAX);
    }

    #[test]
    fn purpose_slots_preserve_legacy_indices() {
        let legacy_purposes = [
            PresetPurpose::Chat,
            PresetPurpose::ImageGeneration,
            PresetPurpose::PersonaInterview,
            PresetPurpose::Vision,
            PresetPurpose::Voice,
            PresetPurpose::MemoryExtraction,
        ];
        for (expected_index, purpose) in legacy_purposes.into_iter().enumerate() {
            assert_eq!(purpose.index(), expected_index);
        }
        assert_eq!(PresetPurpose::Embedding.index(), 6);
        assert_eq!(PresetPurpose::Rerank.index(), 7);
    }

    #[test]
    fn embedding_and_rerank_mutations_invalidate_configuration_generation() {
        let mut registry = PresetRegistry::new(2).unwrap();
        let first_id = PresetId::new("first").unwrap();
        let second_id = PresetId::new("second").unwrap();
        let first_version = registry.create(first_id.clone(), config()).unwrap();
        let second_version = registry.create(second_id.clone(), config()).unwrap();

        let before_embedding_binding = registry.configuration().clone();
        registry
            .bind(PresetPurpose::Embedding, first_id.clone())
            .unwrap();
        assert!(!std::sync::Arc::ptr_eq(
            &before_embedding_binding,
            registry.configuration()
        ));

        let before_rerank_binding = registry.configuration().clone();
        registry
            .bind(PresetPurpose::Rerank, first_id.clone())
            .unwrap();
        assert!(!std::sync::Arc::ptr_eq(
            &before_rerank_binding,
            registry.configuration()
        ));

        let before_switch = registry.configuration().clone();
        registry
            .bind(PresetPurpose::Embedding, second_id.clone())
            .unwrap();
        assert!(!std::sync::Arc::ptr_eq(
            &before_switch,
            registry.configuration()
        ));

        let before_update = registry.configuration().clone();
        let updated_version = registry
            .update(&second_id, second_version, config())
            .unwrap();
        assert!(!std::sync::Arc::ptr_eq(
            &before_update,
            registry.configuration()
        ));
        assert_ne!(updated_version, second_version);

        let before_delete = registry.configuration().clone();
        registry.delete(&second_id, updated_version).unwrap();
        assert!(!std::sync::Arc::ptr_eq(
            &before_delete,
            registry.configuration()
        ));
        assert_eq!(
            registry.resolve(PresetPurpose::Embedding).unwrap_err(),
            GatewayError::PresetNotFound
        );
        assert_eq!(
            registry.resolve(PresetPurpose::Rerank).unwrap().version(),
            first_version
        );
    }
}
