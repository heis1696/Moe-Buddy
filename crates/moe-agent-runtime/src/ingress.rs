use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_memory_core::PetIdentity;

use crate::GatewayError;
use crate::validation;

pub const INGRESS_MAX_TEXT_CHARS: usize = 16_384;
const MAX_TOKEN_CHARS: usize = 256;
/// 稳定凭据归属的初始授权修订号；宿主声明授权撤销并换发新授权时必须
/// 显式推进，旧修订一经退役不能重新绑定。
const INITIAL_ATTRIBUTION_REVISION: u64 = 1;

macro_rules! trusted_label {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, GatewayError> {
                let value = value.into();
                validation::identifier(&value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

trusted_label!(CallerSubject);
trusted_label!(ChannelId);
trusted_label!(ChannelScope);
trusted_label!(ReplyTarget);

pub struct IngressToken(String);

impl IngressToken {
    pub fn new(value: impl Into<String>) -> Result<Self, GatewayError> {
        let value = value.into();
        if value.is_empty()
            || value.chars().count() > MAX_TOKEN_CHARS
            || value.chars().any(char::is_whitespace)
            || value.chars().any(char::is_control)
        {
            return Err(GatewayError::InvalidInput("invalid ingress token"));
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for IngressToken {
    #[allow(clippy::missing_fields_in_debug)]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IngressToken(REDACTED)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnsupportedFeature {
    SystemDeveloperMessage,
    ToolDeclarations,
    MultimodalContent,
    StructuredOutput,
    Streaming,
}

pub struct RawIngressCall {
    token: IngressToken,
    alias: String,
    text: String,
    session_hint: Option<String>,
    client_request_id: Option<String>,
    unsupported: Vec<UnsupportedFeature>,
}

impl RawIngressCall {
    pub fn new(token: IngressToken, alias: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            token,
            alias: alias.into(),
            text: text.into(),
            session_hint: None,
            client_request_id: None,
            unsupported: Vec::new(),
        }
    }

    pub fn with_session_hint(mut self, hint: impl Into<String>) -> Self {
        self.session_hint = Some(hint.into());
        self
    }

    pub fn with_client_request_id(mut self, client_request_id: impl Into<String>) -> Self {
        self.client_request_id = Some(client_request_id.into());
        self
    }

    pub fn declares_unsupported(mut self, feature: UnsupportedFeature) -> Self {
        self.unsupported.push(feature);
        self
    }
}

impl fmt::Debug for RawIngressCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawIngressCall")
            .field("token", &"REDACTED")
            .field("alias", &"REDACTED")
            .field("text_chars", &self.text.chars().count())
            .field(
                "session_hint",
                &self.session_hint.as_ref().map(|_| "present"),
            )
            .field(
                "client_request_id",
                &self.client_request_id.as_ref().map(|_| "present"),
            )
            .field("unsupported", &self.unsupported)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IngressLimits {
    max_routes: usize,
    max_credentials: usize,
    max_issued_credentials: usize,
}

impl IngressLimits {
    pub fn new(max_routes: usize, max_credentials: usize) -> Result<Self, GatewayError> {
        let max_issued_credentials = max_credentials
            .checked_mul(2)
            .ok_or(GatewayError::InvalidInput("credential limit too large"))?;
        Self::with_issuance_limit(max_routes, max_credentials, max_issued_credentials)
    }

    pub fn with_issuance_limit(
        max_routes: usize,
        max_credentials: usize,
        max_issued_credentials: usize,
    ) -> Result<Self, GatewayError> {
        if max_routes == 0 || max_credentials == 0 || max_issued_credentials < max_credentials {
            return Err(GatewayError::InvalidInput("limits must be positive"));
        }
        Ok(Self {
            max_routes,
            max_credentials,
            max_issued_credentials,
        })
    }

    pub fn max_routes(&self) -> usize {
        self.max_routes
    }

    pub fn max_credentials(&self) -> usize {
        self.max_credentials
    }

    pub fn max_issued_credentials(&self) -> usize {
        self.max_issued_credentials
    }
}

impl Default for IngressLimits {
    fn default() -> Self {
        Self {
            max_routes: 256,
            max_credentials: 256,
            max_issued_credentials: 512,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CredentialId {
    run_instance: u64,
    sequence: u64,
}

impl CredentialId {
    pub fn value(&self) -> u64 {
        self.sequence
    }

    pub fn run_instance(&self) -> u64 {
        self.run_instance
    }
}

#[derive(Clone)]
struct RunInstance {
    id: u64,
    marker: Arc<()>,
}

static NEXT_RUN_INSTANCE: AtomicU64 = AtomicU64::new(1);

impl RunInstance {
    fn fresh() -> Result<Self, GatewayError> {
        Ok(Self {
            id: allocate_run_id(&NEXT_RUN_INSTANCE)?,
            marker: Arc::new(()),
        })
    }

    fn owns(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.marker, &other.marker)
    }
}

struct CredentialRecord {
    token: IngressToken,
    subject: CallerSubject,
    aliases: BTreeSet<String>,
    channel: ChannelId,
    scope: ChannelScope,
    reply_target: ReplyTarget,
}

#[derive(Clone)]
pub(crate) struct HookIngressProof {
    instance: RunInstance,
    generation: u64,
    credential: CredentialId,
    alias: String,
    identity: PetIdentity,
    subject: CallerSubject,
    channel: ChannelId,
    scope: ChannelScope,
    reply_target: ReplyTarget,
}

impl HookIngressProof {
    pub(crate) fn reply_target(&self) -> &ReplyTarget {
        &self.reply_target
    }
}

impl fmt::Debug for HookIngressProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookIngressProof")
            .field("generation", &self.generation)
            .field("credential", &self.credential)
            .field("identity", &"REDACTED")
            .field("subject", &"REDACTED")
            .field("channel", &"REDACTED")
            .field("scope", &"REDACTED")
            .field("reply_target", &"REDACTED")
            .finish()
    }
}

fn allocate_run_id(counter: &AtomicU64) -> Result<u64, GatewayError> {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| GatewayError::IdentifierExhausted)
}

pub struct TrustedIngress {
    instance: RunInstance,
    limits: IngressLimits,
    generation: u64,
    sequence: u64,
    credential_sequence: u64,
    routes: HashMap<String, PetIdentity>,
    credentials: HashMap<CredentialId, CredentialRecord>,
    tokens: HashMap<String, CredentialId>,
    burned: HashSet<String>,
    stable_bindings: HashMap<CredentialId, String>,
    attribution_claims: HashMap<String, CredentialId>,
    attribution_revisions: HashMap<String, u64>,
    retired_revisions: HashMap<String, u64>,
}

impl TrustedIngress {
    pub fn new() -> Self {
        Self::with_limits(IngressLimits::default()).expect("default limits are valid")
    }

    pub fn with_limits(limits: IngressLimits) -> Result<Self, GatewayError> {
        Ok(Self {
            instance: RunInstance::fresh()?,
            limits,
            generation: 0,
            sequence: 0,
            credential_sequence: 0,
            routes: HashMap::new(),
            credentials: HashMap::new(),
            tokens: HashMap::new(),
            burned: HashSet::new(),
            stable_bindings: HashMap::new(),
            attribution_claims: HashMap::new(),
            attribution_revisions: HashMap::new(),
            retired_revisions: HashMap::new(),
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn run_instance(&self) -> u64 {
        self.instance.id
    }

    pub fn limits(&self) -> IngressLimits {
        self.limits
    }

    pub fn route_identity(&self, alias: &str) -> Option<&PetIdentity> {
        self.routes.get(alias)
    }

    pub(crate) fn prove_hook_authorization(
        &self,
        token: &IngressToken,
        alias: &str,
    ) -> Result<HookIngressProof, GatewayError> {
        validation::identifier(alias)?;
        let credential = *self
            .tokens
            .get(token.0.as_str())
            .ok_or(GatewayError::Unauthorized)?;
        let record = self
            .credentials
            .get(&credential)
            .expect("token map mirrors live credentials");
        let identity = self.routes.get(alias).ok_or(GatewayError::UnknownRoute)?;
        if !record.aliases.contains(alias) {
            return Err(GatewayError::RouteDenied);
        }
        Ok(HookIngressProof {
            instance: self.instance.clone(),
            generation: self.generation,
            credential,
            alias: alias.to_owned(),
            identity: identity.clone(),
            subject: record.subject.clone(),
            channel: record.channel.clone(),
            scope: record.scope.clone(),
            reply_target: record.reply_target.clone(),
        })
    }

    pub(crate) fn validate_hook_proof(&self, proof: &HookIngressProof) -> Result<(), GatewayError> {
        if !proof.instance.owns(&self.instance) {
            return Err(GatewayError::ForeignRequest);
        }
        if proof.generation != self.generation {
            return Err(GatewayError::Revoked);
        }
        let Some(record) = self.credentials.get(&proof.credential) else {
            return Err(GatewayError::Revoked);
        };
        if record.subject != proof.subject
            || record.channel != proof.channel
            || record.scope != proof.scope
            || record.reply_target != proof.reply_target
            || !record.aliases.contains(&proof.alias)
            || self.routes.get(&proof.alias) != Some(&proof.identity)
        {
            return Err(GatewayError::Revoked);
        }
        Ok(())
    }

    fn next_generation(&self) -> Result<u64, GatewayError> {
        self.generation
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)
    }

    pub fn register_route(
        &mut self,
        alias: &str,
        identity: PetIdentity,
    ) -> Result<(), GatewayError> {
        validation::identifier(alias)?;
        if self.routes.contains_key(alias) {
            return Err(GatewayError::InvalidInput("route alias already registered"));
        }
        if self.routes.len() >= self.limits.max_routes {
            return Err(GatewayError::CapacityExceeded);
        }
        let generation = self.next_generation()?;
        self.routes.insert(alias.to_owned(), identity);
        self.generation = generation;
        Ok(())
    }

    pub fn unregister_route(&mut self, alias: &str) -> Result<(), GatewayError> {
        if !self.routes.contains_key(alias) {
            return Err(GatewayError::UnknownRoute);
        }
        let generation = self.next_generation()?;
        self.routes.remove(alias);
        self.generation = generation;
        Ok(())
    }

    pub fn install_credential(
        &mut self,
        token: IngressToken,
        subject: CallerSubject,
        aliases: impl IntoIterator<Item = String>,
        channel: ChannelId,
        scope: ChannelScope,
        reply_target: ReplyTarget,
    ) -> Result<CredentialId, GatewayError> {
        let mut allow = BTreeSet::new();
        for alias in aliases {
            validation::identifier(&alias)?;
            if !self.routes.contains_key(&alias) {
                return Err(GatewayError::InvalidInput(
                    "allowlist references unregistered route",
                ));
            }
            allow.insert(alias);
        }
        if allow.is_empty() {
            return Err(GatewayError::InvalidInput("empty alias allowlist"));
        }
        if self.tokens.contains_key(&token.0) {
            return Err(GatewayError::InvalidInput(
                "credential token already in use",
            ));
        }
        if self.burned.contains(&token.0) {
            return Err(GatewayError::Revoked);
        }
        if self.credentials.len() >= self.limits.max_credentials {
            return Err(GatewayError::CapacityExceeded);
        }
        if self.burned.len() >= self.limits.max_issued_credentials - self.credentials.len() {
            return Err(GatewayError::CapacityExceeded);
        }
        let credential_value = self
            .credential_sequence
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let generation = self.next_generation()?;
        let token_string = token.0.clone();
        let credential_id = CredentialId {
            run_instance: self.instance.id,
            sequence: credential_value,
        };
        self.credential_sequence = credential_value;
        self.credentials.insert(
            credential_id,
            CredentialRecord {
                token,
                subject,
                aliases: allow,
                channel,
                scope,
                reply_target,
            },
        );
        self.tokens.insert(token_string, credential_id);
        self.generation = generation;
        Ok(credential_id)
    }

    /// 受信宿主为在册凭据显式声明稳定归属；原文持久化按该归属构建跨重启
    /// 可比对的稳定域，授权修订取该归属当前绑定的修订号（新归属为
    /// 初始修订 1）。运行层从不从 token、运行实例号或局部序号推导归属，
    /// 也不验证宿主所报字符串的业务稳定性，只保证：未声明即无稳定域、
    /// 声明在凭据在册期间不可改写、一条归属同时只属于一张在册凭据。
    /// 绑定不推进配置代际，不影响既有准入或权限。
    pub fn bind_stable_credential(
        &mut self,
        credential: CredentialId,
        attribution: impl Into<String>,
    ) -> Result<(), GatewayError> {
        let attribution = attribution.into();
        let revision = self
            .attribution_revisions
            .get(&attribution)
            .copied()
            .unwrap_or(INITIAL_ATTRIBUTION_REVISION);
        self.bind_stable_credential_revision(credential, attribution, revision)
    }

    /// 受信宿主声明稳定归属的授权修订：同一归属的新绑定必须不低于当前
    /// 修订（相等即凭据轮换续用；更高即宿主宣告旧授权撤销并换发，旧修订
    /// 随之退役），低于当前或已退役修订一律拒绝——被撤销的旧授权不能凭
    /// 相同局部序号的新凭据或重签的历史许可恢复。归属字符串的绑定与
    /// 改写规则与 `bind_stable_credential` 相同。
    pub fn bind_stable_credential_revision(
        &mut self,
        credential: CredentialId,
        attribution: impl Into<String>,
        revision: u64,
    ) -> Result<(), GatewayError> {
        if credential.run_instance != self.instance.id {
            return Err(GatewayError::ForeignRequest);
        }
        if !self.credentials.contains_key(&credential) {
            return Err(GatewayError::InvalidInput("unknown credential"));
        }
        let attribution = attribution.into();
        validation::identifier(&attribution)?;
        if revision == 0 {
            return Err(GatewayError::InvalidInput(
                "stable credential revision must be positive",
            ));
        }
        if let Some(existing) = self.stable_bindings.get(&credential) {
            if existing != &attribution {
                return Err(GatewayError::InvalidInput(
                    "credential is already bound to another stable attribution",
                ));
            }
            let current = self
                .attribution_revisions
                .get(&attribution)
                .copied()
                .expect("bound attribution carries its revision");
            if revision == current {
                return Ok(());
            }
            if revision > current {
                self.retired_revisions
                    .entry(attribution.clone())
                    .and_modify(|retired| *retired = (*retired).max(current))
                    .or_insert(current);
                self.attribution_revisions.insert(attribution, revision);
                return Ok(());
            }
            return Err(GatewayError::InvalidInput(
                "stable credential revision cannot move backwards",
            ));
        }
        if self.attribution_claims.contains_key(&attribution) {
            return Err(GatewayError::InvalidInput(
                "stable attribution is already bound to another live credential",
            ));
        }
        if let Some(retired) = self.retired_revisions.get(&attribution)
            && revision <= *retired
        {
            return Err(GatewayError::InvalidInput(
                "stable credential revision was revoked and cannot be rebound",
            ));
        }
        let current = self.attribution_revisions.get(&attribution).copied();
        match current {
            Some(bound) if revision < bound => {
                return Err(GatewayError::InvalidInput(
                    "stable credential revision cannot move backwards",
                ));
            }
            Some(bound) if revision > bound => {
                self.retired_revisions
                    .entry(attribution.clone())
                    .and_modify(|retired| *retired = (*retired).max(bound))
                    .or_insert(bound);
            }
            _ => {}
        }
        self.stable_bindings.insert(credential, attribution.clone());
        self.attribution_claims
            .insert(attribution.clone(), credential);
        self.attribution_revisions.insert(attribution, revision);
        Ok(())
    }

    /// 解析凭据稳定归属绑定的授权修订；凭据撤销后保留为历史事实供稳定域
    /// 与快照修订比对，但不恢复任何准入权。从未绑定的凭据返回 `None`。
    pub fn stable_credential_revision(&self, credential: CredentialId) -> Option<u64> {
        let attribution = self.stable_bindings.get(&credential)?;
        self.attribution_revisions.get(attribution).copied()
    }

    /// 解析凭据的稳定归属；凭据撤销后保留为历史事实供稳定域比对，但不
    /// 恢复任何准入权。从未绑定的凭据返回 `None`。
    pub fn credential_attribution(&self, credential: CredentialId) -> Option<&str> {
        self.stable_bindings.get(&credential).map(String::as_str)
    }

    pub(crate) fn has_live_attribution(&self, attribution: &str) -> bool {
        self.attribution_claims.contains_key(attribution)
    }

    pub fn revoke_credential(&mut self, credential: CredentialId) -> Result<(), GatewayError> {
        if credential.run_instance != self.instance.id {
            return Err(GatewayError::ForeignRequest);
        }
        if !self.credentials.contains_key(&credential) {
            return Err(GatewayError::InvalidInput("unknown credential"));
        }
        let generation = self.next_generation()?;
        let record = self
            .credentials
            .remove(&credential)
            .expect("credential existence checked");
        let _ = self
            .tokens
            .remove(&record.token.0)
            .expect("token map mirrors live credentials");
        self.burned.insert(record.token.0);
        if let Some(attribution) = self.stable_bindings.get(&credential).cloned() {
            self.attribution_claims.remove(&attribution);
        }
        self.generation = generation;
        Ok(())
    }

    pub fn admit(&mut self, call: RawIngressCall) -> Result<TrustedRequest, GatewayError> {
        let RawIngressCall {
            token,
            alias,
            text,
            session_hint,
            client_request_id,
            unsupported,
        } = call;
        let credential_id = *self
            .tokens
            .get(token.0.as_str())
            .ok_or(GatewayError::Unauthorized)?;
        let record = self
            .credentials
            .get(&credential_id)
            .expect("token map mirrors live credentials");

        validation::identifier(&alias)?;
        let Some(identity) = self.routes.get(&alias) else {
            return Err(GatewayError::UnknownRoute);
        };
        if !record.aliases.contains(alias.as_str()) {
            return Err(GatewayError::RouteDenied);
        }

        if !unsupported.is_empty() {
            return Err(GatewayError::UnsupportedCapability);
        }
        validation::text(&text, INGRESS_MAX_TEXT_CHARS)?;
        if let Some(hint) = &session_hint {
            validation::identifier(hint)?;
        }
        if let Some(client_request_id) = &client_request_id {
            validation::identifier(client_request_id)?;
        }

        let identity = identity.clone();
        let subject = record.subject.clone();
        let channel = record.channel.clone();
        let scope = record.scope.clone();
        let reply_target = record.reply_target.clone();
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        self.sequence = sequence;

        let isolation = match &session_hint {
            Some(hint) => RequestIsolation::Hinted(HintIsolation {
                run_instance: self.instance.id,
                credential: credential_id,
                subject: subject.clone(),
                identity: identity.clone(),
                channel: channel.clone(),
                scope: scope.clone(),
                hint: hint.clone(),
            }),
            None => RequestIsolation::Fresh {
                run_instance: self.instance.id,
                request: sequence,
            },
        };

        Ok(TrustedRequest {
            instance: self.instance.clone(),
            generation: self.generation,
            sequence,
            credential: credential_id,
            alias,
            identity,
            subject,
            channel,
            scope,
            reply_target,
            text,
            session_hint,
            client_request_id,
            isolation,
        })
    }
}

impl Default for TrustedIngress {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TrustedIngress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedIngress")
            .field("run_instance", &self.instance.id)
            .field("generation", &self.generation)
            .field("routes", &self.routes.len())
            .field("credentials", &self.credentials.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum RequestIsolation {
    Fresh { run_instance: u64, request: u64 },
    Hinted(HintIsolation),
}

#[derive(Clone, PartialEq, Eq)]
pub struct HintIsolation {
    run_instance: u64,
    credential: CredentialId,
    subject: CallerSubject,
    identity: PetIdentity,
    channel: ChannelId,
    scope: ChannelScope,
    hint: String,
}

impl HintIsolation {
    pub fn run_instance(&self) -> u64 {
        self.run_instance
    }

    pub fn credential(&self) -> CredentialId {
        self.credential
    }

    pub fn subject(&self) -> &CallerSubject {
        &self.subject
    }

    pub fn identity(&self) -> &PetIdentity {
        &self.identity
    }

    pub fn channel(&self) -> &ChannelId {
        &self.channel
    }

    pub fn scope(&self) -> &ChannelScope {
        &self.scope
    }

    pub fn hint(&self) -> &str {
        &self.hint
    }
}

impl fmt::Debug for HintIsolation {
    #[allow(clippy::missing_fields_in_debug)]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HintIsolation")
            .field("credential", &self.credential)
            .field("subject", &self.subject.as_str())
            .field(
                "identity_workspace",
                &self.identity.namespace().workspace_id(),
            )
            .field("identity_user", &self.identity.namespace().user_id())
            .field("identity_instance", &self.identity.instance_id())
            .field("channel", &self.channel.as_str())
            .field("scope", &self.scope.as_str())
            .field("hint", &"REDACTED")
            .finish()
    }
}

impl fmt::Debug for RequestIsolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fresh {
                run_instance,
                request,
            } => formatter
                .debug_struct("Fresh")
                .field("run_instance", run_instance)
                .field("request", request)
                .finish(),
            Self::Hinted(isolation) => fmt::Debug::fmt(isolation, formatter),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IngressAdmission {
    _private: (),
}

#[derive(Clone)]
pub struct TrustedRequest {
    instance: RunInstance,
    generation: u64,
    sequence: u64,
    credential: CredentialId,
    alias: String,
    identity: PetIdentity,
    subject: CallerSubject,
    channel: ChannelId,
    scope: ChannelScope,
    reply_target: ReplyTarget,
    text: String,
    session_hint: Option<String>,
    client_request_id: Option<String>,
    isolation: RequestIsolation,
}

impl TrustedRequest {
    pub fn run_instance(&self) -> u64 {
        self.instance.id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn credential(&self) -> CredentialId {
        self.credential
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn identity(&self) -> &PetIdentity {
        &self.identity
    }

    pub fn subject(&self) -> &CallerSubject {
        &self.subject
    }

    pub fn channel(&self) -> &ChannelId {
        &self.channel
    }

    pub fn scope(&self) -> &ChannelScope {
        &self.scope
    }

    pub fn reply_target(&self) -> &ReplyTarget {
        &self.reply_target
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn session_hint(&self) -> Option<&str> {
        self.session_hint.as_deref()
    }

    pub fn client_request_id(&self) -> Option<&str> {
        self.client_request_id.as_deref()
    }

    pub fn isolation(&self) -> &RequestIsolation {
        &self.isolation
    }

    pub fn admission(&self) -> IngressAdmission {
        IngressAdmission { _private: () }
    }

    pub fn validate(&self, ingress: &TrustedIngress) -> Result<(), GatewayError> {
        if !self.instance.owns(&ingress.instance) {
            return Err(GatewayError::ForeignRequest);
        }
        if self.generation != ingress.generation {
            return Err(GatewayError::Revoked);
        }
        match ingress.credentials.get(&self.credential) {
            Some(record) => {
                if record.subject != self.subject
                    || record.channel != self.channel
                    || record.scope != self.scope
                    || record.reply_target != self.reply_target
                {
                    return Err(GatewayError::Revoked);
                }
            }
            None => return Err(GatewayError::Revoked),
        }
        if ingress.routes.get(&self.alias) != Some(&self.identity) {
            return Err(GatewayError::Revoked);
        }
        Ok(())
    }
}

impl fmt::Debug for TrustedRequest {
    #[allow(clippy::missing_fields_in_debug)]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedRequest")
            .field("run_instance", &self.instance.id)
            .field("generation", &self.generation)
            .field("sequence", &self.sequence)
            .field("credential", &self.credential)
            .field("alias", &self.alias)
            .field(
                "identity_workspace",
                &self.identity.namespace().workspace_id(),
            )
            .field("identity_user", &self.identity.namespace().user_id())
            .field("identity_instance", &self.identity.instance_id())
            .field("subject", &self.subject.as_str())
            .field("channel", &self.channel.as_str())
            .field("scope", &self.scope.as_str())
            .field("reply_target", &self.reply_target.as_str())
            .field("text_chars", &self.text.chars().count())
            .field(
                "session_hint",
                &self.session_hint.as_deref().map(|_| "present"),
            )
            .field(
                "client_request_id",
                &self.client_request_id.as_ref().map(|_| "present"),
            )
            .field("isolation", &self.isolation)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moe_memory_core::Namespace;

    fn identity(workspace: &str, user: &str, pet: &str) -> PetIdentity {
        PetIdentity::new(Namespace::new(workspace, user).unwrap(), pet).unwrap()
    }

    fn prepared() -> TrustedIngress {
        let mut ingress = TrustedIngress::new();
        ingress
            .register_route("alias-a", identity("ws", "user", "pet-a"))
            .unwrap();
        ingress
    }

    fn call(token: &str) -> RawIngressCall {
        RawIngressCall::new(IngressToken::new(token).unwrap(), "alias-a", "hello")
    }

    fn installed(ingress: &mut TrustedIngress, token: &str) -> CredentialId {
        ingress
            .install_credential(
                IngressToken::new(token).unwrap(),
                CallerSubject::new("subject-a").unwrap(),
                ["alias-a".to_owned()],
                ChannelId::new("channel-a").unwrap(),
                ChannelScope::new("scope-a").unwrap(),
                ReplyTarget::new("reply-a").unwrap(),
            )
            .unwrap()
    }

    #[test]
    fn credential_ids_do_not_reuse_after_revocation() {
        let mut ingress = prepared();
        let first = installed(&mut ingress, "tok-a");
        assert_eq!(first.value(), 1);
        assert_eq!(first.run_instance(), ingress.run_instance());
        ingress.revoke_credential(first).unwrap();
        let second = installed(&mut ingress, "tok-b");
        assert_eq!(second.value(), 2);
        assert_eq!(second.run_instance(), ingress.run_instance());
        assert_ne!(first, second);
        assert!(ingress.admit(call("tok-b")).is_ok());
        assert_eq!(
            ingress.admit(call("tok-a")).unwrap_err(),
            GatewayError::Unauthorized
        );
    }

    #[test]
    fn sequence_overflow_rejects_without_publishing() {
        let mut ingress = prepared();
        installed(&mut ingress, "tok-a");
        ingress.sequence = u64::MAX;
        assert_eq!(
            ingress.admit(call("tok-a")).unwrap_err(),
            GatewayError::IdentifierExhausted
        );
        assert_eq!(ingress.sequence, u64::MAX);
        ingress.sequence = 0;
        assert!(ingress.admit(call("tok-a")).is_ok());
    }

    #[test]
    fn credential_overflow_rejects_without_installing() {
        let mut ingress = prepared();
        ingress.credential_sequence = u64::MAX;
        assert_eq!(
            ingress
                .install_credential(
                    IngressToken::new("tok-a").unwrap(),
                    CallerSubject::new("subject-a").unwrap(),
                    ["alias-a".to_owned()],
                    ChannelId::new("channel-a").unwrap(),
                    ChannelScope::new("scope-a").unwrap(),
                    ReplyTarget::new("reply-a").unwrap(),
                )
                .unwrap_err(),
            GatewayError::IdentifierExhausted
        );
        assert!(ingress.credentials.is_empty());
        assert!(ingress.tokens.is_empty());
        assert_eq!(ingress.credential_sequence, u64::MAX);
        assert_eq!(ingress.generation, 1);
    }

    #[test]
    fn generation_overflow_prevents_half_updates() {
        let mut ingress = prepared();
        ingress.generation = u64::MAX;
        assert_eq!(
            ingress
                .register_route("alias-b", identity("ws", "user", "pet-b"))
                .unwrap_err(),
            GatewayError::IdentifierExhausted
        );
        assert!(!ingress.routes.contains_key("alias-b"));
        assert_eq!(ingress.generation, u64::MAX);
        assert_eq!(
            ingress
                .install_credential(
                    IngressToken::new("tok-a").unwrap(),
                    CallerSubject::new("subject-a").unwrap(),
                    ["alias-a".to_owned()],
                    ChannelId::new("channel-a").unwrap(),
                    ChannelScope::new("scope-a").unwrap(),
                    ReplyTarget::new("reply-a").unwrap(),
                )
                .unwrap_err(),
            GatewayError::IdentifierExhausted
        );
        assert!(ingress.credentials.is_empty());
        assert!(ingress.tokens.is_empty());
        assert_eq!(ingress.generation, u64::MAX);
    }

    #[test]
    fn run_instance_overflow_does_not_wrap() {
        let counter = AtomicU64::new(u64::MAX);
        assert_eq!(
            allocate_run_id(&counter),
            Err(GatewayError::IdentifierExhausted)
        );
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
