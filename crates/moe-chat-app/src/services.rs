//! 受信业务组合：可信入口、会话运行时、离线 Agent 流程、记忆服务、
//! API 预设注册表与纯内存诊断日志的窄门面。
//!
//! 本模块只组合底层 crate 的既有受信接口，不复制业务规则、不接文件/
//! 网络/系统时钟；全部时间由宿主经 `ChatApplication::dispatch` 注入。
//! 诊断采集失败不改变业务结果，健康计数经授权查询可见。

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;

mod external;
mod turn;

use moe_agent_runtime::{
    AgentAccess, AgentLimits, AgentRuntime, AgentServices, BackgroundCode, CallerSubject,
    CaptureStrategy, CategorySet, ChannelId, ChannelScope, ClockReading, CredentialId, DetailLevel,
    DiagnosticKind, DiagnosticsConfig, DiagnosticsService, EventCode, EventInput, Gap, HardBudgets,
    IngressToken, LogAccess, LogEntry, LogFilter, ModelOutcome, OfflineInput, OfflineModel,
    OpaqueAlias, PageDirection, PresentationEvent, PresetConfig, PresetId, PresetPurpose,
    PresetRegistry, RawIngressCall, Registration, ReplyTarget, RunObservation, SessionAccess,
    SessionLimits, SessionRuntime, SessionTiming, StableDomain, Step, TerminalState,
    TrustedIngress, TrustedRequest, UserEvent, UserEventKind, ViewerGrant, ViewerSubject,
};
#[cfg(test)]
use moe_memory_core::Namespace;
use moe_memory_core::{
    Category, Epistemic, MemoryDraft, MemoryScope, PetIdentity, ReadPolicy, RetrievalQuery,
    SearchScope, Sensitivity, SessionId, SourceRef,
};

use crate::contract::{
    ChatMessage, ChatSnapshot, ChatStatus, ChatTurnProgress, DiagnosticSnapshot, MemorySnapshot,
    MessageRole, ParsedReply, PlainTextReplyParser, PresetSnapshot, ReplyParser,
};
use crate::error::AppError;
use turn::ChatTurn;
pub(crate) use turn::ExternalCallback;

/// 稳定伙伴标识；同一离线应用实例内不变。
pub(crate) const COMPANION: &str = "offline-companion";
/// 应用层输入字节上限（与 `contract::MAX_TEXT_BYTES` 一致）与记忆正文字数上限。
pub(crate) const REMEMBER_MAX_CHARS: usize = 4_096;
/// 诊断面板单页与记忆列展示的有界条数。
pub(crate) const DIAGNOSTICS_PAGE: usize = 8;
pub(crate) const MEMORY_LIST_LIMIT: usize = 100;
const ISSUED_NOTE_ID_LIMIT: usize = MEMORY_LIST_LIMIT;

const SESSION_EVENTS_BUDGET: usize = 64;
const SESSION_FRAGMENTS_BUDGET: usize = 8;
const SESSION_CHARS_BUDGET: usize = 65_536;
pub(crate) const REPLY_CHARS_BUDGET: usize = 4_096;

const ROUTE_ALIAS: &str = "buddy/offline";
const SESSION_HINT: &str = "offline-acceptance-session";
const CALLER: &str = "local-trusted-host";
const CHANNEL: &str = "local-chat";
const SCOPE: &str = "private-window";
const REPLY_TARGET: &str = "chat-window";
const TOKEN_VALUE: &str = "offline-acceptance-token-not-for-production";
const DIAGNOSTICS_DOMAIN: &str = "moe-chat-offline-companion";
const DIAGNOSTICS_VIEWER: &str = "local-authorized-viewer";
const DIAGNOSTICS_GRANT_VERSION: u64 = 1;
/// 授权记忆读取绑定的记忆会话标识（应用自有，不从客户端输入拼接）。
const MEMORY_READER_SESSION: &str = "offline-memory-view";

/// 诊断面板一次有界查询的投影结果。
pub(crate) struct DiagnosticsPage {
    pub(crate) entries: Vec<DiagnosticSnapshot>,
    pub(crate) gap_possible: bool,
}

/// 离线验收模型：确定性地组合人设、获准记忆与用户输入，并明确标注
/// 非真实模型。它只是受信宿主夹具边界，不是网络模型适配。
struct OfflineVerifierModel;

impl OfflineModel for OfflineVerifierModel {
    fn infer(&mut self, input: OfflineInput<'_>) -> ModelOutcome {
        let mut reply = format!(
            "【离线验收·非真实模型】预设 {}（v{}）收到：「{}」。",
            input.preset_id().as_str(),
            input.preset_version(),
            truncate_chars(input.user_text(), 120),
        );
        if input.memory().is_empty() {
            reply.push_str("本轮没有装配获准记忆。");
        } else {
            reply.push_str(&format!(
                "已按授权装配获准记忆 {} 条，最近一条：{}",
                input.memory().len(),
                truncate_chars(input.memory()[0].text(), 120),
            ));
        }
        ModelOutcome::Text {
            text: reply,
            presentation: vec![PresentationEvent::Neutral],
        }
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

/// 五系统组合的核心；由 [`ChatApplication`](crate::ChatApplication) 独占持有。
pub(crate) struct ChatCore {
    process_generation: u64,
    pub(crate) online: bool,
    pub(crate) transmission_allowed: bool,
    pending_external: Option<ChatTurn>,
    pub(crate) external_input: Option<moe_agent_runtime::ExternalInput>,
    ticket_owner: Arc<()>,
    turn_sequence: u64,
    session_turns: usize,
    /// Actual fragment handles returned by this execution generation's ledger.
    admitted_fragments: Vec<moe_agent_runtime::SessionId>,
    parser: Box<dyn ReplyParser>,
    last_progress: Option<ChatTurnProgress>,
    last_parsed_reply: Option<ParsedReply>,
    /// 已提交但尚未取得应用展示发布结果的轮次；仅应用层可收尾。
    pending_publish: Option<ChatTurn>,
    settings_version: u64,
    persona_version: u64,
    persona_text: String,
    history_store: Option<(
        Box<dyn crate::ChatHistory>,
        TrustedRequest,
        moe_agent_runtime::RawHistoryGrant,
        moe_agent_runtime::RawRetention,
    )>,
    last_storage_receipts: crate::HistoryCommitReceipts,
    cold_history_grants: BTreeMap<String, moe_agent_runtime::RawHistoryGrant>,
    ingress: TrustedIngress,
    credential: CredentialId,
    presets: PresetRegistry,
    preset_ids: Vec<PresetId>,
    preset_catalog: Vec<PresetSnapshot>,
    sessions: SessionRuntime,
    memory: Box<dyn crate::ChatMemory>,
    agent: AgentRuntime,
    diagnostics: DiagnosticsService,
    diagnostics_history_gap: bool,
    domain: StableDomain,
    viewer: ViewerSubject,
    base_emitter: moe_agent_runtime::LogEmitter,
    identity: PetIdentity,
    persona_source: SourceRef,
    notes_source: SourceRef,
    reader_session: SessionId,
    chat_history: ChatSnapshot,
    active_logical_session_id: String,
    active_session_hint: String,
    inactive_sessions: BTreeMap<String, InactiveSession>,
    storage_unknown: bool,
    buddy_configuration_unknown: bool,
    external_unknown_evidence: bool,
    recovered_unsettled_evidence: bool,
    admitted_events: usize,
    admitted_chars: usize,
    pending_disposition: Option<Registration>,
    revoked: bool,
    note_ids: BTreeMap<String, moe_memory_core::MemoryId>,
}

#[derive(Clone)]
struct InactiveSession {
    hint: String,
    history: ChatSnapshot,
    external_unknown: bool,
    recovered_unsettled: bool,
}

struct PreparedExecution {
    ingress: TrustedIngress,
    credential: CredentialId,
    sessions: SessionRuntime,
    agent: AgentRuntime,
    diagnostics: DiagnosticsService,
    domain: StableDomain,
    viewer: ViewerSubject,
    base_emitter: moe_agent_runtime::LogEmitter,
    request: TrustedRequest,
    grant: Option<moe_agent_runtime::RawHistoryGrant>,
}

impl ChatCore {
    #[cfg(test)]
    pub(crate) fn new(
        process_generation: u64,
        memory: Box<dyn crate::ChatMemory>,
        initial_session_id: String,
    ) -> Result<Self, AppError> {
        let identity = PetIdentity::new(
            Namespace::new("moe-chat", "local")
                .map_err(|error| AppError::from_memory(error).not_executed())?,
            COMPANION,
        )
        .map_err(|error| AppError::from_memory(error).not_executed())?;
        Self::new_for_identity(
            process_generation,
            memory,
            initial_session_id,
            identity,
            crate::buddy::default_persona(),
            1,
            1,
        )
    }

    pub(crate) fn new_for_identity(
        process_generation: u64,
        mut memory: Box<dyn crate::ChatMemory>,
        initial_session_id: String,
        identity: PetIdentity,
        persona_text: &str,
        persona_version: u64,
        model_version: u64,
    ) -> Result<Self, AppError> {
        if persona_version == 0
            || model_version == 0
            || persona_version > crate::MAX_SAFE_INTEGER
            || model_version > crate::MAX_SAFE_INTEGER
            || !crate::buddy::valid_persona(persona_text)
        {
            return Err(AppError::rejection(
                "invalid_buddy_profile",
                "伙伴身份或配置版本无效",
                "invalid trusted buddy profile",
            )
            .not_executed());
        }

        let mut ingress = TrustedIngress::new();
        ingress
            .register_route(ROUTE_ALIAS, identity.clone())
            .map_err(AppError::from_gateway)?;
        let credential = ingress
            .install_credential(
                IngressToken::new(TOKEN_VALUE).map_err(AppError::from_gateway)?,
                CallerSubject::new(CALLER).map_err(AppError::from_gateway)?,
                [ROUTE_ALIAS.to_owned()],
                ChannelId::new(CHANNEL).map_err(AppError::from_gateway)?,
                ChannelScope::new(SCOPE).map_err(AppError::from_gateway)?,
                ReplyTarget::new(REPLY_TARGET).map_err(AppError::from_gateway)?,
            )
            .map_err(AppError::from_gateway)?;

        let (presets, preset_ids, preset_catalog) = build_presets()?;

        let sessions = SessionRuntime::new(
            &ingress,
            SessionLimits::new(
                SESSION_FRAGMENTS_BUDGET,
                SESSION_EVENTS_BUDGET,
                256,
                8_192,
                SESSION_CHARS_BUDGET,
                2,
            )
            .map_err(AppError::from_gateway)?,
            SessionTiming::default(),
            0,
        );

        let persona_source = memory.register_source(
            MemoryScope::Private(identity.clone()),
            "persona",
            1,
            Epistemic::AuthoritativeState,
        )?;
        let notes_source = memory.register_source(
            MemoryScope::Private(identity.clone()),
            "explicit-notes",
            1,
            Epistemic::UserStatement,
        )?;
        memory.set_identity_view(
            &identity,
            persona_version,
            persona_text,
            vec![persona_source.clone()],
        )?;
        memory.set_read_policy(
            &identity,
            ReadPolicy::offline(1, vec![persona_source.clone(), notes_source.clone()]),
        )?;

        let agent = AgentRuntime::new(
            &ingress,
            AgentLimits::new(16, 4_096, 4_096, REPLY_CHARS_BUDGET)
                .map_err(AppError::from_gateway)?,
        )
        .map_err(AppError::from_gateway)?;

        let domain = StableDomain::new(DIAGNOSTICS_DOMAIN).map_err(AppError::from_diagnostics)?;
        let viewer = ViewerSubject::new(DIAGNOSTICS_VIEWER).map_err(AppError::from_diagnostics)?;
        let grant = ViewerGrant::new(
            viewer,
            DIAGNOSTICS_GRANT_VERSION,
            u64::MAX,
            CategorySet::all(),
            DetailLevel::Essential,
        );
        let budgets = HardBudgets {
            max_domains: 1,
            max_grants_per_domain: 1,
            events_per_partition: 64,
            runs_per_partition: 64,
            aliases_per_partition: 128,
            cursors_per_grant: 2,
            max_page_size: DIAGNOSTICS_PAGE,
            max_query_work: 1_024,
            max_summaries_per_query: 4,
            max_health_slots: 9,
            max_concurrent_queries: 1,
            max_event_bytes: 2_048,
            max_allocated_bytes: 1_048_576,
        };
        let mut config = DiagnosticsConfig::new(u128::from(process_generation), budgets)
            .map_err(AppError::from_diagnostics)?;
        config
            .add_domain(
                domain,
                CaptureStrategy::all_through(DetailLevel::Essential),
                &[grant],
            )
            .map_err(AppError::from_diagnostics)?;
        let mut diagnostics = DiagnosticsService::default();
        diagnostics
            .enable(config)
            .map_err(AppError::from_diagnostics)?;
        let base_emitter = diagnostics
            .create_emitter(domain, None)
            .map_err(AppError::from_diagnostics)?;
        let _ = diagnostics.emit(
            &base_emitter,
            EventInput {
                source: moe_agent_runtime::Source::Host,
                detail: DetailLevel::Essential,
                elapsed_ms: None,
                kind: DiagnosticKind::Background(BackgroundCode::Initialized),
            },
            ClockReading {
                monotonic_ms: 0,
                wall_clock_ms: None,
            },
        );

        Ok(Self {
            process_generation,
            online: false,
            transmission_allowed: false,
            pending_external: None,
            external_input: None,
            ticket_owner: Arc::new(()),
            turn_sequence: 0,
            session_turns: 0,
            admitted_fragments: Vec::new(),
            parser: Box::new(PlainTextReplyParser),
            last_progress: None,
            last_parsed_reply: None,
            pending_publish: None,
            settings_version: model_version.saturating_sub(1).max(1),
            persona_version,
            persona_text: persona_text.to_owned(),
            history_store: None,
            last_storage_receipts: crate::HistoryCommitReceipts::default(),
            cold_history_grants: BTreeMap::new(),
            ingress,
            credential,
            presets,
            preset_ids,
            preset_catalog,
            sessions,
            memory,
            agent,
            diagnostics,
            diagnostics_history_gap: false,
            domain,
            viewer,
            base_emitter,
            identity,
            persona_source,
            notes_source,
            reader_session: SessionId::new(MEMORY_READER_SESSION)
                .map_err(|error| AppError::from_memory(error).not_executed())?,
            chat_history: ChatSnapshot {
                draft: String::new(),
                messages: Vec::new(),
                status: ChatStatus::Ready,
                failure: None,
            },
            active_logical_session_id: initial_session_id,
            active_session_hint: SESSION_HINT.to_owned(),
            inactive_sessions: BTreeMap::new(),
            storage_unknown: false,
            buddy_configuration_unknown: false,
            external_unknown_evidence: false,
            recovered_unsettled_evidence: false,
            admitted_events: 0,
            admitted_chars: 0,
            pending_disposition: None,
            revoked: false,
            note_ids: BTreeMap::new(),
        })
    }

    fn prepare_execution(
        &self,
        process_generation: u64,
        session_hint: &str,
        now_ms: u64,
    ) -> Result<PreparedExecution, AppError> {
        let mut ingress = TrustedIngress::new();
        ingress
            .register_route(ROUTE_ALIAS, self.identity.clone())
            .map_err(AppError::from_gateway)?;
        let credential = ingress
            .install_credential(
                IngressToken::new(TOKEN_VALUE).map_err(AppError::from_gateway)?,
                CallerSubject::new(CALLER).map_err(AppError::from_gateway)?,
                [ROUTE_ALIAS.to_owned()],
                ChannelId::new(CHANNEL).map_err(AppError::from_gateway)?,
                ChannelScope::new(SCOPE).map_err(AppError::from_gateway)?,
                ReplyTarget::new(REPLY_TARGET).map_err(AppError::from_gateway)?,
            )
            .map_err(AppError::from_gateway)?;
        let retention = self
            .history_store
            .as_ref()
            .map(|(_, _, _, retention)| *retention);
        if retention.is_some() {
            ingress
                .bind_stable_credential(credential, "daily-desktop-owner")
                .map_err(AppError::from_gateway)?;
        }
        let limits = SessionLimits::new(
            SESSION_FRAGMENTS_BUDGET,
            SESSION_EVENTS_BUDGET,
            256,
            8_192,
            SESSION_CHARS_BUDGET,
            2,
        )
        .map_err(AppError::from_gateway)?;
        let mut sessions = if let Some(retention) = retention {
            SessionRuntime::with_retention(
                &ingress,
                limits,
                SessionTiming::default(),
                retention,
                now_ms,
            )
        } else {
            SessionRuntime::new(&ingress, limits, SessionTiming::default(), now_ms)
        };
        let request = ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new(TOKEN_VALUE).map_err(AppError::from_gateway)?,
                    ROUTE_ALIAS,
                    "history-access",
                )
                .with_session_hint(session_hint.to_owned()),
            )
            .map_err(AppError::from_gateway)?;
        let grant = if retention.is_some() {
            let domain = moe_agent_runtime::RawDomainKey::from_request(&ingress, &request)
                .map_err(AppError::from_gateway)?;
            Some(
                sessions
                    .install_raw_history_grant(&ingress, domain.clone())
                    .map_err(AppError::from_gateway)?,
            )
        } else {
            None
        };
        let agent = AgentRuntime::new(
            &ingress,
            AgentLimits::new(16, 4_096, 4_096, REPLY_CHARS_BUDGET)
                .map_err(AppError::from_gateway)?,
        )
        .map_err(AppError::from_gateway)?;
        let domain = StableDomain::new(DIAGNOSTICS_DOMAIN).map_err(AppError::from_diagnostics)?;
        let viewer = ViewerSubject::new(DIAGNOSTICS_VIEWER).map_err(AppError::from_diagnostics)?;
        let (diagnostics, base_emitter) = build_diagnostics(process_generation, domain, viewer)?;
        Ok(PreparedExecution {
            ingress,
            credential,
            sessions,
            agent,
            diagnostics,
            domain,
            viewer,
            base_emitter,
            request,
            grant,
        })
    }

    pub(crate) fn is_revoked(&self) -> bool {
        self.revoked
    }

    pub(crate) fn history_enabled(&self) -> bool {
        self.history_store.is_some()
    }

    pub(crate) fn storage_commit_receipts(&self) -> crate::HistoryCommitReceipts {
        self.last_storage_receipts.clone()
    }

    pub(crate) fn mark_revoked(&mut self) {
        self.revoked = true;
    }

    pub(crate) fn create_logical_session(&mut self, id: &str, now_ms: u64) -> Result<(), AppError> {
        self.ensure_session_lifecycle_available()?;
        if self.inactive_sessions.contains_key(id) || id == self.active_logical_session_id {
            return Err(session_rejected("session_id_conflict"));
        }
        self.ensure_volatile_history_can_rotate()?;
        self.store_active_session();
        self.rotate_memory_execution(id, now_ms)?;
        self.activate_session(id.to_owned(), id.to_owned(), None);
        Ok(())
    }

    pub(crate) fn switch_logical_session(&mut self, id: &str, now_ms: u64) -> Result<(), AppError> {
        self.ensure_session_lifecycle_available()?;
        let Some(next) = self.inactive_sessions.get(id).cloned() else {
            return Err(session_rejected("unknown_session"));
        };
        self.ensure_volatile_history_can_rotate()?;
        self.store_active_session();
        self.rotate_memory_execution(id, now_ms)?;
        self.inactive_sessions.remove(id);
        self.activate_session(id.to_owned(), next.hint.clone(), Some(next));
        Ok(())
    }

    fn ensure_volatile_history_can_rotate(&self) -> Result<(), AppError> {
        if self.history_store.is_none() && self.admitted_events != 0 {
            return Err(AppError::rejection(
                "conversation_full",
                "内存会话原文仍由当前执行代际持有，不能在丢弃账本的情况下轮转",
                "volatile session rotation would discard authoritative raw history",
            )
            .not_executed());
        }
        Ok(())
    }

    fn rotate_memory_execution(&mut self, session_id: &str, now_ms: u64) -> Result<(), AppError> {
        let next_generation = self
            .process_generation
            .checked_add(1)
            .ok_or_else(|| session_rejected("execution_generation_exhausted").not_executed())?;
        let next = self.prepare_execution(next_generation, session_id, now_ms)?;
        self.process_generation = next_generation;
        self.ingress = next.ingress;
        self.credential = next.credential;
        self.sessions = next.sessions;
        self.agent = next.agent;
        self.diagnostics = next.diagnostics;
        self.diagnostics_history_gap = true;
        self.domain = next.domain;
        self.viewer = next.viewer;
        self.base_emitter = next.base_emitter;
        self.ticket_owner = Arc::new(());
        self.turn_sequence = 0;
        self.session_turns = 0;
        self.admitted_fragments.clear();
        self.cold_history_grants.clear();
        self.external_unknown_evidence = false;
        self.recovered_unsettled_evidence = false;
        self.pending_external = None;
        self.external_input = None;
        self.pending_publish = None;
        self.pending_disposition = None;
        self.last_progress = None;
        self.last_parsed_reply = None;
        self.admitted_events = 0;
        self.admitted_chars = 0;
        Ok(())
    }

    pub(crate) fn ensure_session_lifecycle_available(&self) -> Result<(), AppError> {
        self.ensure_active()?;
        if self.storage_unknown {
            return Err(session_rejected("storage_failed"));
        }
        if self.pending_external.is_some()
            || self.pending_publish.is_some()
            || self.pending_disposition.is_some()
        {
            return Err(session_rejected("session_execution_pending"));
        }
        Ok(())
    }

    pub(crate) fn session_isolation_allowed(&self) -> bool {
        !self.revoked
            && !self.storage_unknown
            && (self.history_store.is_some() || self.admitted_events == 0)
            && self.chat_history.status == ChatStatus::Unknown
            && self.chat_history.failure.as_deref() == Some("send_unknown")
            && (self.external_unknown_evidence || self.recovered_unsettled_evidence)
            && self.pending_external.is_none()
            && self.pending_publish.is_none()
            && self.pending_disposition.is_none()
    }

    pub(crate) fn rotate_persistent_session(
        &mut self,
        session_id: &str,
        expected_sessions_revision: u64,
        change: crate::HistoryChange,
        now_ms: u64,
        send_reservation: Option<crate::HistorySendReservation>,
    ) -> Result<crate::HistoryRecovery, AppError> {
        self.ensure_session_lifecycle_available()?;
        let prepared = (|| {
            let next_generation = self
                .process_generation
                .checked_add(1)
                .ok_or_else(|| session_rejected("execution_generation_exhausted").not_executed())?;
            let next_storage_hint = match &change {
                crate::HistoryChange::Create(session) => session.id.clone(),
                crate::HistoryChange::Switch(id) | crate::HistoryChange::Renew(id) => self
                    .history_store
                    .as_ref()
                    .ok_or_else(|| session_rejected("history_not_enabled").not_executed())?
                    .0
                    .session_storage_hint(id)?,
            };
            let next = self.prepare_execution(next_generation, &next_storage_hint, now_ms)?;
            Ok::<_, AppError>((next_generation, next))
        })();
        let (next_generation, mut next) = prepared?;
        let maintenance_receipts = self
            .maintain_retention(now_ms, expected_sessions_revision, 1)?
            .map_or_else(crate::HistoryCommitReceipts::default, |report| {
                report.receipts
            });
        let Some((mut store, request, grant, retention)) = self.history_store.take() else {
            return Err(committed_maintenance_error(
                session_rejected("history_not_enabled").not_executed(),
                &maintenance_receipts,
            ));
        };
        let result = {
            let next_grant = next
                .grant
                .as_ref()
                .expect("persistent execution has raw grant");
            store.rotate(
                crate::HistoryAccess {
                    runtime: &mut self.sessions,
                    ingress: &self.ingress,
                    request: &request,
                    grant: &grant,
                    retention: &retention,
                    now_ms,
                    logical_session_id: &self.active_logical_session_id,
                    status: self.chat_history.status,
                    online: self.online && self.transmission_allowed,
                    external_request_pending: self.pending_external.is_some(),
                    external_unknown: self.external_unknown_evidence,
                },
                crate::HistoryAccess {
                    runtime: &mut next.sessions,
                    ingress: &next.ingress,
                    request: &next.request,
                    grant: next_grant,
                    retention: &retention,
                    now_ms,
                    logical_session_id: session_id,
                    status: self.chat_history.status,
                    online: self.online && self.transmission_allowed,
                    external_request_pending: false,
                    external_unknown: false,
                },
                expected_sessions_revision,
                change,
                send_reservation,
            )
        };
        let mut recovery = match result {
            Ok(recovery) => recovery,
            Err(error) => {
                self.last_storage_receipts =
                    merge_commit_receipts(maintenance_receipts, store.last_commit_receipts());
                self.history_store = Some((store, request, grant, retention));
                if error.execution_disposition() == crate::ExecutionDisposition::Unknown {
                    self.mark_storage_unknown();
                }
                return Err(committed_maintenance_error(
                    error,
                    &self.last_storage_receipts,
                ));
            }
        };
        recovery.receipts = merge_commit_receipts(maintenance_receipts, recovery.receipts);
        self.process_generation = next_generation;
        self.ingress = next.ingress;
        self.credential = next.credential;
        self.sessions = next.sessions;
        self.agent = next.agent;
        self.diagnostics = next.diagnostics;
        self.diagnostics_history_gap = true;
        self.domain = next.domain;
        self.viewer = next.viewer;
        self.base_emitter = next.base_emitter;
        self.history_store = Some((
            store,
            next.request,
            next.grant.expect("persistent execution has raw grant"),
            retention,
        ));
        self.active_logical_session_id = recovery.active_session_id.clone();
        self.active_session_hint = recovery.active_storage_hint.clone();
        self.external_unknown_evidence = recovery.active_external_unknown;
        self.recovered_unsettled_evidence = recovery.active_recovered_unsettled;
        self.ticket_owner = Arc::new(());
        self.turn_sequence = 0;
        self.session_turns = 0;
        self.admitted_fragments.clear();
        self.cold_history_grants.clear();
        self.pending_external = None;
        self.external_input = None;
        self.pending_publish = None;
        self.pending_disposition = None;
        self.last_progress = None;
        self.last_parsed_reply = None;
        self.admitted_events = 0;
        self.admitted_chars = 0;
        self.chat_history = history_snapshot(&recovery);
        self.last_storage_receipts = recovery.receipts.clone();
        Ok(recovery)
    }

    pub(crate) fn archive_persistent_session(
        &mut self,
        session_id: &str,
        expected_sessions_revision: u64,
        now_ms: u64,
    ) -> Result<crate::HistoryRecovery, AppError> {
        self.ensure_session_lifecycle_available()?;
        let Some((store, request, grant, retention)) = self.history_store.as_mut() else {
            return Err(session_rejected("history_not_enabled").not_executed());
        };
        let result = store.archive(
            crate::HistoryAccess {
                runtime: &mut self.sessions,
                ingress: &self.ingress,
                request,
                grant,
                retention,
                now_ms,
                logical_session_id: &self.active_logical_session_id,
                status: self.chat_history.status,
                online: self.online && self.transmission_allowed,
                external_request_pending: self.pending_external.is_some(),
                external_unknown: self.external_unknown_evidence,
            },
            session_id,
            expected_sessions_revision,
        );
        self.last_storage_receipts = store.last_commit_receipts();
        match result {
            Ok(recovery) => {
                self.last_storage_receipts = recovery.receipts.clone();
                Ok(recovery)
            }
            Err(error) => {
                if error.execution_disposition() == crate::ExecutionDisposition::Unknown {
                    self.mark_storage_unknown();
                }
                Err(error)
            }
        }
    }

    pub(crate) fn needs_generation_rotation(&self, user_text: &str) -> bool {
        self.session_turns >= 31
            || self.admitted_fragments.len() >= SESSION_FRAGMENTS_BUDGET
            || self.admitted_events.saturating_add(2) > SESSION_EVENTS_BUDGET
            || self
                .admitted_chars
                .saturating_add(user_text.chars().count())
                .saturating_add(REPLY_CHARS_BUDGET)
                > SESSION_CHARS_BUDGET
    }

    pub(crate) fn preflight_send(
        &mut self,
        text: &str,
        sessions_revision: u64,
        now_ms: u64,
    ) -> Result<(), AppError> {
        self.ensure_active()?;
        if self.storage_unknown {
            return Err(session_rejected("storage_failed").not_executed());
        }
        if self.pending_external.is_some() || self.chat_history.status == ChatStatus::Unknown {
            return Err(session_rejected("send_unknown").not_executed());
        }
        if self.pending_publish.is_some() {
            return Err(session_rejected("session_execution_pending").not_executed());
        }
        if self.online && !self.transmission_allowed {
            return Err(AppError::rejection(
                "transmission_denied",
                "请在模型设置中明确允许发送内容",
                "model transmission denied",
            )
            .not_executed());
        }
        let Some((store, request, grant, retention)) = self.history_store.as_mut() else {
            return Ok(());
        };
        store.preflight_checkpoint(
            crate::HistoryAccess {
                runtime: &mut self.sessions,
                ingress: &self.ingress,
                request,
                grant,
                retention,
                now_ms,
                logical_session_id: &self.active_logical_session_id,
                status: self.chat_history.status,
                online: self.online && self.transmission_allowed,
                external_request_pending: self.pending_external.is_some(),
                external_unknown: self.external_unknown_evidence,
            },
            sessions_revision,
            text.chars().count(),
            REPLY_CHARS_BUDGET,
        )
    }

    pub(crate) fn maintain_retention(
        &mut self,
        now_ms: u64,
        sessions_revision: u64,
        cold_segment_budget: usize,
    ) -> Result<Option<crate::HistoryMaintenanceReport>, AppError> {
        if self.history_store.is_none() {
            return Ok(None);
        }
        let live = match self.sessions.maintain_retention(&self.ingress, now_ms) {
            Ok(report) => report,
            Err(error) => {
                self.mark_storage_unknown();
                return Err(AppError::from_gateway(error));
            }
        };
        let mut live_checkpointed = false;
        let mut receipts = crate::HistoryCommitReceipts::default();
        if live.transferred > 0
            || live.removed > 0
            || live.authorization_removed > 0
            || live.released_events > 0
            || live.released_chars > 0
        {
            let checkpoint =
                if let Some((store, request, grant, retention)) = self.history_store.as_mut() {
                    store.checkpoint(
                        crate::HistoryAccess {
                            runtime: &mut self.sessions,
                            ingress: &self.ingress,
                            request,
                            grant,
                            retention,
                            now_ms,
                            logical_session_id: &self.active_logical_session_id,
                            status: self.chat_history.status,
                            online: self.online && self.transmission_allowed,
                            external_request_pending: self.pending_external.is_some(),
                            external_unknown: self.external_unknown_evidence,
                        },
                        sessions_revision,
                    )
                } else {
                    Ok(crate::HistoryCommitReceipts::default())
                };
            match checkpoint {
                Ok(commits) => {
                    receipts = commits;
                    self.last_storage_receipts = receipts.clone();
                    live_checkpointed = true;
                }
                Err(error) => {
                    if let Some((store, _, _, _)) = self.history_store.as_ref() {
                        self.last_storage_receipts = store.last_commit_receipts();
                    }
                    self.mark_storage_unknown();
                    return Err(error);
                }
            }
            if let Some((store, request, grant, retention)) = self.history_store.as_mut() {
                let messages = store.load_history(
                    crate::HistoryAccess {
                        runtime: &mut self.sessions,
                        ingress: &self.ingress,
                        request,
                        grant,
                        retention,
                        now_ms,
                        logical_session_id: &self.active_logical_session_id,
                        status: self.chat_history.status,
                        online: self.online && self.transmission_allowed,
                        external_request_pending: self.pending_external.is_some(),
                        external_unknown: self.external_unknown_evidence,
                    },
                    &self.active_logical_session_id,
                    128,
                );
                match messages {
                    Ok(messages) => self.chat_history.messages = messages,
                    Err(error) => {
                        self.mark_storage_unknown();
                        return Err(error);
                    }
                }
            }
        }

        let (cold_count, cold_ids) = self
            .history_store
            .as_ref()
            .map(|(store, _, _, _)| {
                (
                    store.cold_segment_count(),
                    store.cold_session_ids(cold_segment_budget),
                )
            })
            .unwrap_or_default();
        let mut requests = Vec::with_capacity(cold_ids.len());
        let active_session_id = self.active_logical_session_id.clone();
        let active_grant = self
            .history_store
            .as_ref()
            .expect("history store remains enabled")
            .2
            .clone();
        for id in cold_ids {
            let hint = self
                .history_store
                .as_ref()
                .expect("history store remains enabled")
                .0
                .session_storage_hint(&id)?;
            let request = self
                .ingress
                .admit(
                    RawIngressCall::new(
                        IngressToken::new(TOKEN_VALUE).map_err(AppError::from_gateway)?,
                        ROUTE_ALIAS,
                        "cold-history-maintenance",
                    )
                    .with_session_hint(hint),
                )
                .map_err(AppError::from_gateway)?;
            let grant = if id == active_session_id {
                active_grant.clone()
            } else if let Some(grant) = self.cold_history_grants.get(&id) {
                grant.clone()
            } else {
                let domain = moe_agent_runtime::RawDomainKey::from_request(&self.ingress, &request)
                    .map_err(AppError::from_gateway)?;
                let grant = self
                    .sessions
                    .install_raw_history_grant(&self.ingress, domain)
                    .map_err(AppError::from_gateway)?;
                self.cold_history_grants.insert(id.clone(), grant.clone());
                grant
            };
            requests.push(crate::HistoryRequest {
                logical_session_id: id,
                request,
                grant,
            });
        }
        let cold = if let Some((store, request, grant, retention)) = self.history_store.as_mut() {
            match store.maintain_cold(
                crate::HistoryAccess {
                    runtime: &mut self.sessions,
                    ingress: &self.ingress,
                    request,
                    grant,
                    retention,
                    now_ms,
                    logical_session_id: &self.active_logical_session_id,
                    status: self.chat_history.status,
                    online: self.online && self.transmission_allowed,
                    external_request_pending: self.pending_external.is_some(),
                    external_unknown: self.external_unknown_evidence,
                },
                &requests,
                cold_segment_budget,
            ) {
                Ok(processed) => {
                    receipts = merge_commit_receipts(receipts, store.last_commit_receipts());
                    processed
                }
                Err(error) => {
                    receipts = merge_commit_receipts(receipts, store.last_commit_receipts());
                    self.last_storage_receipts = receipts.clone();
                    if error.execution_disposition() == crate::ExecutionDisposition::Unknown {
                        self.mark_storage_unknown();
                    }
                    let has_committed_work =
                        !receipts.raw.is_empty() || receipts.directory.is_some();
                    return Err(
                        if has_committed_work
                            && error.execution_disposition()
                                == crate::ExecutionDisposition::NotExecuted
                        {
                            error.with_execution_disposition(crate::ExecutionDisposition::Settled)
                        } else {
                            error
                        },
                    );
                }
            }
        } else {
            crate::ColdHistoryMaintenance::default()
        };
        self.last_storage_receipts = receipts.clone();
        Ok(Some(crate::HistoryMaintenanceReport {
            live,
            live_checkpointed,
            cold_segments_processed: cold.segments_processed,
            cold_segments_remaining: cold_count.saturating_sub(cold.segments_processed),
            cold_blocked: cold.blocked,
            receipts,
        }))
    }

    pub(crate) fn mark_storage_unknown(&mut self) {
        self.storage_unknown = true;
        self.external_unknown_evidence = false;
        self.recovered_unsettled_evidence = false;
        self.external_input = None;
        self.chat_history.status = ChatStatus::Unknown;
        self.chat_history.failure = Some(String::from("storage_failed"));
    }

    pub(crate) fn mark_buddy_configuration_unknown(&mut self) {
        self.buddy_configuration_unknown = true;
        self.transmission_allowed = false;
    }

    pub(crate) fn ensure_usable(&self) -> Result<(), AppError> {
        self.ensure_active()
    }

    fn store_active_session(&mut self) {
        self.inactive_sessions.insert(
            self.active_logical_session_id.clone(),
            InactiveSession {
                hint: self.active_session_hint.clone(),
                history: self.chat_history.clone(),
                external_unknown: self.external_unknown_evidence,
                recovered_unsettled: self.recovered_unsettled_evidence,
            },
        );
    }

    fn activate_session(&mut self, id: String, hint: String, stored: Option<InactiveSession>) {
        self.active_logical_session_id = id;
        self.active_session_hint = hint;
        if let Some(stored) = stored {
            self.chat_history = stored.history;
            self.external_unknown_evidence = stored.external_unknown;
            self.recovered_unsettled_evidence = stored.recovered_unsettled;
            self.admitted_events = 0;
            self.admitted_chars = 0;
            self.last_progress = None;
            self.last_parsed_reply = None;
        } else {
            self.chat_history = ChatSnapshot {
                draft: String::new(),
                messages: Vec::new(),
                status: ChatStatus::Ready,
                failure: None,
            };
            self.recovered_unsettled_evidence = false;
            self.admitted_events = 0;
            self.admitted_chars = 0;
            self.last_progress = None;
            self.last_parsed_reply = None;
        }
    }

    /// 撤权服务侧收口：撤销入口凭据并停用诊断采集（清空全部诊断缓冲）。
    pub(crate) fn revoke_services(&mut self) -> Result<(), AppError> {
        self.pending_external = None;
        self.external_input = None;
        self.pending_publish = None;
        self.last_progress = None;
        self.last_parsed_reply = None;
        self.transmission_allowed = false;
        self.pending_disposition = None;
        self.chat_history.messages.clear();
        self.chat_history.status = ChatStatus::Ready;
        self.chat_history.failure = None;
        self.inactive_sessions.clear();
        self.cold_history_grants.clear();
        self.storage_unknown = false;
        self.external_unknown_evidence = false;
        self.recovered_unsettled_evidence = false;
        self.ingress
            .revoke_credential(self.credential)
            .map_err(AppError::from_gateway)?;
        self.diagnostics
            .disable()
            .map_err(AppError::from_diagnostics)?;
        Ok(())
    }

    pub(crate) fn active_preset_id(&self) -> String {
        self.presets
            .resolve(PresetPurpose::Chat)
            .map(|resolved| resolved.id().as_str().to_owned())
            .unwrap_or_default()
    }

    /// 预设目录展示：注册表无按 ID 读取入口，目录使用创建预设时的同一份
    /// 配置；当前绑定与解析始终走真实注册表。
    pub(crate) fn preset_catalog(&self) -> Vec<PresetSnapshot> {
        self.preset_catalog.clone()
    }

    /// 一轮文字聊天：由应用层组合器通过 `WorkflowRuntime` 实际推进
    /// 受理、准备输入、请求/等待、接收、解析、提交与发布七阶段；
    /// Agent、会话、记忆与 UI 门面仍是权威业务入口。
    pub(crate) fn send(&mut self, text: &str, now_ms: u64) -> Result<(), AppError> {
        self.ensure_active().map_err(AppError::not_executed)?;
        if self.storage_unknown {
            return Err(session_rejected("storage_failed"));
        }
        if self.pending_publish.is_some() {
            return Err(AppError::rejection(
                "request_pending",
                "上一轮已提交，正在等待展示发布结果",
                "a committed turn still awaits its display publication result",
            )
            .not_executed());
        }
        if self.online {
            return self.begin_external(text, now_ms);
        }
        if self.pending_external.is_some() || self.chat_history.status == ChatStatus::Unknown {
            return Err(AppError::rejection(
                "send_unknown",
                "上一轮登记后的结果未能确认，已隔离会话；请勿重发，请重启宿主",
                "new admission blocked by a quarantined interaction",
            )
            .not_executed());
        }
        if self.history_store.is_none() && self.chat_history.messages.len() > 126 {
            return Err(AppError::rejection(
                "conversation_full",
                "本次内存会话已达到容量上限，请重启宿主开始新会话",
                "conversation display budget exhausted before admission",
            )
            .not_executed());
        }
        let user_chars = text.chars().count();
        if self.admitted_events + 2 > SESSION_EVENTS_BUDGET
            || self.admitted_chars + user_chars + REPLY_CHARS_BUDGET > SESSION_CHARS_BUDGET
        {
            return Err(AppError::rejection(
                "conversation_full",
                "会话容量不足以容纳输入和回复，该轮尚未登记；请重启宿主开始新会话",
                "admission refused before registration: reply budget unavailable",
            )
            .not_executed());
        }
        let mut turn = self.start_turn(text)?;
        if let Err(error) = self.admission_phase(&mut turn, text, now_ms) {
            self.record_progress(&turn);
            if turn.is_registered() {
                return Err(self.finish_registered_failure(&mut turn, error, now_ms));
            }
            self.chat_history.status = ChatStatus::Failed;
            self.chat_history.failure = Some(error.public_code().to_owned());
            return Err(error);
        }
        let outcome = (|| {
            self.prepare_input_phase(&mut turn, false, now_ms)?;
            self.offline_request_phase(&mut turn, now_ms)?;
            self.receive_recorded_phase(&mut turn, now_ms)?;
            self.parse_phase(&mut turn)?;
            self.commit_phase(&mut turn, now_ms)?;
            self.stage_publish_phase(&mut turn)?;
            Ok::<(), AppError>(())
        })();
        self.record_progress(&turn);
        match outcome {
            Ok(()) => {
                self.refund_success(&turn);
                // The publish node stays Running until the application layer
                // reports whether the real display facade published this turn.
                self.pending_publish = Some(turn);
                Ok(())
            }
            Err(primary) => Err(self.finish_registered_failure(&mut turn, primary, now_ms)),
        }
    }

    pub(crate) fn chat_history(&self) -> Result<ChatSnapshot, AppError> {
        self.ensure_active()?;
        Ok(self.chat_history.clone())
    }

    pub(crate) fn push_message(&mut self, message: ChatMessage) {
        self.chat_history.messages.push(message);
        if self.chat_history.messages.len() > 128 {
            let excess = self.chat_history.messages.len() - 128;
            self.chat_history.messages.drain(..excess);
        }
    }

    fn settle_after_failure(
        &mut self,
        request: &TrustedRequest,
        registration: &Registration,
        primary: AppError,
        emitter: Option<&moe_agent_runtime::LogEmitter>,
        now_ms: u64,
    ) -> AppError {
        let disposal = self.sessions.cancel_interaction(
            SessionAccess::new(&self.ingress, request, now_ms),
            &registration.interaction,
        );
        match disposal {
            Ok(receipt) if receipt.state == TerminalState::Cancelled => {
                self.admitted_chars -= REPLY_CHARS_BUDGET;
                self.admitted_events -= 1;
                mark(
                    &mut self.diagnostics,
                    emitter,
                    now_ms,
                    DiagnosticKind::Run(RunObservation::Operation(
                        moe_agent_runtime::OperationState::Cancelled,
                    )),
                );
                AppError::cancelled_after_admission(primary)
            }
            failure => {
                let cleanup = match failure {
                    Err(error) => error,
                    Ok(_) => moe_agent_runtime::GatewayError::InvalidInput(
                        "unexpected cancellation receipt",
                    ),
                };
                self.pending_disposition = Some(registration.clone());
                self.chat_history.status = ChatStatus::Unknown;
                self.chat_history.failure = Some(String::from("send_unknown"));
                mark(
                    &mut self.diagnostics,
                    emitter,
                    now_ms,
                    DiagnosticKind::Run(RunObservation::Operation(
                        moe_agent_runtime::OperationState::Unknown,
                    )),
                );
                AppError::quarantined_after_admission(primary, cleanup)
            }
        }
    }

    fn settle_external_unknown(
        &mut self,
        request: &TrustedRequest,
        registration: &Registration,
        primary: AppError,
        emitter: Option<&moe_agent_runtime::LogEmitter>,
        now_ms: u64,
    ) -> AppError {
        let disposal = self.sessions.cancel_interaction(
            SessionAccess::new(&self.ingress, request, now_ms),
            &registration.interaction,
        );
        let error = match disposal {
            Ok(receipt) if receipt.state == TerminalState::Cancelled => {
                self.admitted_chars -= REPLY_CHARS_BUDGET;
                self.admitted_events -= 1;
                AppError::unknown_after_admission(primary)
            }
            failure => {
                let cleanup = match failure {
                    Err(error) => error,
                    Ok(_) => moe_agent_runtime::GatewayError::InvalidInput(
                        "unexpected cancellation receipt",
                    ),
                };
                self.pending_disposition = Some(registration.clone());
                AppError::quarantined_after_admission(primary, cleanup)
            }
        };
        self.chat_history.status = ChatStatus::Unknown;
        self.chat_history.failure = Some(String::from("send_unknown"));
        mark(
            &mut self.diagnostics,
            emitter,
            now_ms,
            DiagnosticKind::Run(RunObservation::Operation(
                moe_agent_runtime::OperationState::Unknown,
            )),
        );
        error
    }

    /// 明确记住：经 `MemoryService` 的受控私密写入；聊天原文永不自动入库。
    pub(crate) fn remember(&mut self, text: &str, now_ms: u64) -> Result<u64, AppError> {
        self.ensure_active()?;
        let emitter = self.open_run();
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::MemoryQuery)),
        );
        let draft = MemoryDraft::new(
            text,
            Category::UserProfile,
            Epistemic::UserStatement,
            vec![self.notes_source.clone()],
        )
        .with_sensitivity(Sensitivity::Private);
        let receipt = match self.memory.create(&self.identity, draft, now_ms) {
            Ok(receipt) => receipt,
            Err(error) => {
                mark_error(
                    &mut self.diagnostics,
                    emitter.as_ref(),
                    now_ms,
                    Step::MemoryQuery,
                    &error,
                );
                return Err(error);
            }
        };
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::MemoryQuery)),
        );
        self.retain_note_id(receipt.id());
        Ok(receipt.id().as_u64())
    }

    /// 经授权检索列出私密长期记忆（离线用途，与 Agent 同一读取边界）。
    pub(crate) fn search_memories(
        &mut self,
        text: &str,
        now_ms: u64,
    ) -> Result<Vec<MemorySnapshot>, AppError> {
        self.ensure_active()?;
        let emitter = self.open_run();
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::MemoryQuery)),
        );
        let query = RetrievalQuery::new(text)
            .with_scope(SearchScope::Private)
            .with_limit(MEMORY_LIST_LIMIT);
        let result =
            match self
                .memory
                .retrieve(&self.identity, &self.reader_session, &query, now_ms)
            {
                Ok(result) => result,
                Err(error) => {
                    mark_error(
                        &mut self.diagnostics,
                        emitter.as_ref(),
                        now_ms,
                        Step::MemoryQuery,
                        &error,
                    );
                    return Err(error);
                }
            };
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::MemoryQuery)),
        );
        for item in result.items() {
            if let moe_memory_core::RetrievalItem::LongTerm(record) = item
                && record.sources() == [self.notes_source.clone()]
            {
                self.retain_note_id(record.id());
            }
        }
        Ok(result
            .items()
            .iter()
            .map(|item| MemorySnapshot {
                revision: match item {
                    moe_memory_core::RetrievalItem::LongTerm(record) => record.revision(),
                    _ => 0,
                },
                id: match item {
                    moe_memory_core::RetrievalItem::LongTerm(record) => {
                        record.id().as_u64().to_string()
                    }
                    moe_memory_core::RetrievalItem::WorkingContext(context) => {
                        context.id().to_owned()
                    }
                },
                text: item.text().to_owned(),
            })
            .collect())
    }

    pub(crate) fn change_memory(
        &mut self,
        id: &str,
        revision: u64,
        text: Option<&str>,
        now_ms: u64,
    ) -> Result<(), AppError> {
        self.ensure_active()?;
        let note_key = id.to_owned();
        let id = *self.note_ids.get(&note_key).ok_or_else(|| {
            AppError::rejection(
                "memory_not_found",
                "这条记忆已不可用，请刷新列表",
                "note is not an issued private note",
            )
            .not_executed()
        })?;
        let emitter = self.open_run();
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::MemoryQuery)),
        );
        // Only explicit notes issued by this application are editable. The memory
        // service still verifies ownership, sources, protection and revision.
        let result = if let Some(text) = text {
            let draft = MemoryDraft::new(
                text,
                Category::UserProfile,
                Epistemic::UserStatement,
                vec![self.notes_source.clone()],
            )
            .with_sensitivity(Sensitivity::Private);
            self.memory
                .update(&self.identity, id, revision, draft, now_ms)
                .map(|_| ())
        } else {
            self.memory
                .delete(&self.identity, id, revision, now_ms)
                .map(|_| ())
        };
        if let Err(error) = result {
            mark_error(
                &mut self.diagnostics,
                emitter.as_ref(),
                now_ms,
                Step::MemoryQuery,
                &error,
            );
            return Err(error);
        }
        if text.is_none() {
            self.note_ids.remove(&note_key);
        }
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::MemoryQuery)),
        );
        Ok(())
    }

    fn retain_note_id(&mut self, id: moe_memory_core::MemoryId) {
        let key = id.as_u64().to_string();
        if !self.note_ids.contains_key(&key) && self.note_ids.len() >= ISSUED_NOTE_ID_LIMIT {
            let oldest = self
                .note_ids
                .keys()
                .min_by_key(|value| value.parse::<u64>().unwrap_or(u64::MAX))
                .cloned();
            if let Some(oldest) = oldest {
                self.note_ids.remove(&oldest);
            }
        }
        self.note_ids.insert(key, id);
    }

    /// 切换聊天预设：真实 `PresetRegistry` 逐用途重绑，下一轮解析即生效。
    pub(crate) fn select_preset(
        &mut self,
        id: &str,
        now_ms: u64,
    ) -> Result<PresetSnapshot, AppError> {
        self.ensure_active()?;
        let target = self
            .preset_ids
            .iter()
            .find(|preset| preset.as_str() == id)
            .cloned()
            .ok_or_else(|| {
                AppError::rejection(
                    "unknown_preset",
                    "预设不存在，已拒绝绑定",
                    "unknown preset id",
                )
                .not_executed()
            })?;
        self.presets
            .bind(PresetPurpose::Chat, target)
            .map_err(|error| AppError::from_gateway(error).settled())?;
        let emitter = self.open_run();
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Lifecycle)),
        );
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::Lifecycle)),
        );
        let resolved = self
            .presets
            .resolve(PresetPurpose::Chat)
            .map_err(AppError::from_gateway)?;
        Ok(PresetSnapshot {
            id: resolved.id().as_str().to_owned(),
            label: resolved.config().display_name().to_owned(),
            provider: resolved.config().protocol().to_owned(),
            model: resolved.config().default_model().to_owned(),
            endpoint: resolved.config().endpoint().to_owned(),
        })
    }

    /// 独立授权查询：先认证主体再取一页有界安全投影，游标及时关闭；
    /// 条目不含提示词、回复正文、记忆正文、凭据或原始错误。
    pub(crate) fn query_diagnostics(&mut self, now_ms: u64) -> Result<DiagnosticsPage, AppError> {
        self.ensure_active()?;
        let emitter = self.open_run();
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Lifecycle)),
        );
        let clock = ClockReading {
            monotonic_ms: now_ms,
            wall_clock_ms: None,
        };
        let access: LogAccess = self
            .diagnostics
            .authorize(self.domain, self.viewer, DIAGNOSTICS_GRANT_VERSION, clock)
            .map_err(AppError::from_diagnostics)?;
        let filter = LogFilter::all().direction(PageDirection::NewestFirst);
        let mut output: [Option<LogEntry>; DIAGNOSTICS_PAGE] = std::array::from_fn(|_| None);
        let page = self
            .diagnostics
            .query_into(&access, filter, None, clock, &mut output)
            .map_err(AppError::from_diagnostics)?;
        if let Some(cursor) = page.next_cursor {
            self.diagnostics
                .close_cursor(cursor)
                .map_err(AppError::from_diagnostics)?;
        }
        let mut run_names: Vec<(OpaqueAlias, String)> = Vec::new();
        let entries = output
            .iter()
            .flatten()
            .enumerate()
            .map(|(index, entry)| DiagnosticSnapshot {
                sequence: (index + 1) as u64,
                run: run_display_name(entry.run_alias(), &mut run_names),
                event: String::from(event_code_text(entry.event_code())),
                status: entry_status_text(entry),
            })
            .collect();
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::Lifecycle)),
        );
        Ok(DiagnosticsPage {
            entries,
            gap_possible: self.diagnostics_history_gap || matches!(page.gap, Gap::Possible),
        })
    }

    fn ensure_active(&self) -> Result<(), AppError> {
        if self.buddy_configuration_unknown {
            return Err(AppError::rejection(
                "storage_failed",
                "伙伴配置已提交但当前运行未能对齐，应用暂不可继续使用",
                "persisted buddy configuration is not applied to this runtime",
            )
            .not_executed());
        }
        if self.revoked {
            return Err(AppError::rejection(
                "revoked",
                "本次授权已撤销，应用不再接受任何操作",
                "authorization revoked for this process",
            )
            .not_executed());
        }
        Ok(())
    }

    fn admit(&mut self, text: &str) -> Result<TrustedRequest, AppError> {
        self.admit_with_hint(text, self.active_session_hint.clone())
    }

    fn admit_with_hint(
        &mut self,
        text: &str,
        session_hint: String,
    ) -> Result<TrustedRequest, AppError> {
        let token = IngressToken::new(TOKEN_VALUE).map_err(AppError::from_gateway)?;
        self.ingress
            .admit(
                RawIngressCall::new(token, ROUTE_ALIAS, text.to_owned())
                    .with_session_hint(session_hint),
            )
            .map_err(AppError::from_gateway)
    }

    fn grant_sources(&self) -> Vec<SourceRef> {
        vec![self.persona_source.clone(), self.notes_source.clone()]
    }

    /// 为一次业务动作申请运行与采集句柄；任一失败都降级为无采集，
    /// 不阻塞业务（健康计数经授权查询可见）。
    fn open_run(&mut self) -> Option<moe_agent_runtime::LogEmitter> {
        let run = match self.diagnostics.create_run(self.domain) {
            Ok(run) => run,
            Err(moe_agent_runtime::DiagnosticsError::CapacityExceeded) => {
                self.rotate_diagnostics().ok()?;
                self.diagnostics.create_run(self.domain).ok()?
            }
            Err(_) => return None,
        };
        self.diagnostics
            .create_emitter(self.domain, Some(&run))
            .ok()
    }

    fn rotate_diagnostics(&mut self) -> Result<(), AppError> {
        let next_generation = self.process_generation.checked_add(1).ok_or_else(|| {
            AppError::rejection(
                "diagnostics_generation_exhausted",
                "诊断代际已耗尽，历史缺口已保留",
                "diagnostics process generation exhausted",
            )
            .not_executed()
        })?;
        let (diagnostics, emitter) = build_diagnostics(next_generation, self.domain, self.viewer)?;
        self.process_generation = next_generation;
        self.diagnostics = diagnostics;
        self.base_emitter = emitter;
        self.diagnostics_history_gap = true;
        Ok(())
    }
}

fn build_diagnostics(
    process_generation: u64,
    domain: StableDomain,
    viewer: ViewerSubject,
) -> Result<(DiagnosticsService, moe_agent_runtime::LogEmitter), AppError> {
    let grant = ViewerGrant::new(
        viewer,
        DIAGNOSTICS_GRANT_VERSION,
        u64::MAX,
        CategorySet::all(),
        DetailLevel::Essential,
    );
    let budgets = HardBudgets {
        max_domains: 1,
        max_grants_per_domain: 1,
        events_per_partition: 64,
        runs_per_partition: 64,
        aliases_per_partition: 128,
        cursors_per_grant: 2,
        max_page_size: DIAGNOSTICS_PAGE,
        max_query_work: 1_024,
        max_summaries_per_query: 4,
        max_health_slots: 9,
        max_concurrent_queries: 1,
        max_event_bytes: 2_048,
        max_allocated_bytes: 1_048_576,
    };
    let mut config = DiagnosticsConfig::new(u128::from(process_generation), budgets)
        .map_err(AppError::from_diagnostics)?;
    config
        .add_domain(
            domain,
            CaptureStrategy::all_through(DetailLevel::Essential),
            &[grant],
        )
        .map_err(AppError::from_diagnostics)?;
    let mut service = DiagnosticsService::default();
    service.enable(config).map_err(AppError::from_diagnostics)?;
    let emitter = service
        .create_emitter(domain, None)
        .map_err(AppError::from_diagnostics)?;
    let _ = service.emit(
        &emitter,
        EventInput {
            source: moe_agent_runtime::Source::Host,
            detail: DetailLevel::Essential,
            elapsed_ms: None,
            kind: DiagnosticKind::Background(BackgroundCode::Initialized),
        },
        ClockReading {
            monotonic_ms: 0,
            wall_clock_ms: None,
        },
    );
    Ok((service, emitter))
}

fn history_snapshot(recovery: &crate::HistoryRecovery) -> ChatSnapshot {
    let status = recovery
        .sessions
        .iter()
        .find(|session| session.id == recovery.active_session_id)
        .map_or(ChatStatus::Ready, |session| session.status);
    let start = recovery.active_history.len().saturating_sub(128);
    ChatSnapshot {
        draft: String::new(),
        messages: recovery.active_history[start..].to_vec(),
        status,
        failure: (status == ChatStatus::Unknown).then(|| String::from("send_unknown")),
    }
}

fn merge_commit_receipts(
    mut earlier: crate::HistoryCommitReceipts,
    later: crate::HistoryCommitReceipts,
) -> crate::HistoryCommitReceipts {
    earlier.raw.extend(later.raw);
    if later.directory.is_some() {
        earlier.directory = later.directory;
    }
    earlier
}

fn committed_maintenance_error(
    error: AppError,
    receipts: &crate::HistoryCommitReceipts,
) -> AppError {
    if error.execution_disposition() == crate::ExecutionDisposition::NotExecuted
        && (!receipts.raw.is_empty() || receipts.directory.is_some())
    {
        error.with_execution_disposition(crate::ExecutionDisposition::Settled)
    } else {
        error
    }
}

fn session_rejected(code: &'static str) -> AppError {
    let (message, detail) = match code {
        "history_not_enabled" => (
            "当前应用未启用会话历史适配器",
            "persistent lifecycle method requires an enabled history adapter",
        ),
        "session_execution_pending" => (
            "当前本地执行或展示尚未结束，不能切换会话",
            "logical session change requires locally quiescent execution",
        ),
        "storage_failed" => (
            "本地原文状态未能确认，当前执行代际已封闭",
            "unresolved history maintenance or checkpoint outcome",
        ),
        "session_id_conflict" => (
            "会话标识冲突，操作未执行",
            "logical session id already exists",
        ),
        _ => (
            "目标会话不可用，操作未执行",
            "logical session is unavailable",
        ),
    };
    AppError::rejection(code, message, detail).not_executed()
}

fn build_presets() -> Result<(PresetRegistry, Vec<PresetId>, Vec<PresetSnapshot>), AppError> {
    let mut registry = PresetRegistry::new(8).map_err(AppError::from_gateway)?;
    let definitions = [
        ("offline-echo", "离线验收·回声预设", "offline-verifier-v1"),
        (
            "offline-reflect",
            "离线验收·回照预设",
            "offline-verifier-v2",
        ),
    ];
    let mut ids = Vec::new();
    let mut catalog = Vec::new();
    for (id, label, model) in definitions {
        let preset_id = PresetId::new(id).map_err(AppError::from_gateway)?;
        registry
            .create(
                preset_id.clone(),
                PresetConfig::new(
                    label,
                    "offline-fixture",
                    "https://offline.invalid/v1",
                    None,
                    model,
                    BTreeMap::new(),
                )
                .map_err(AppError::from_gateway)?,
            )
            .map_err(AppError::from_gateway)?;
        catalog.push(PresetSnapshot {
            id: id.to_owned(),
            label: label.to_owned(),
            provider: "offline-fixture".to_owned(),
            model: model.to_owned(),
            endpoint: "https://offline.invalid/v1".to_owned(),
        });
        ids.push(preset_id);
    }
    registry
        .bind(PresetPurpose::Chat, ids[0].clone())
        .map_err(AppError::from_gateway)?;
    Ok((registry, ids, catalog))
}

fn mark(
    diagnostics: &mut DiagnosticsService,
    emitter: Option<&moe_agent_runtime::LogEmitter>,
    now_ms: u64,
    kind: DiagnosticKind,
) {
    let Some(emitter) = emitter else {
        return;
    };
    let _ = diagnostics.emit(
        emitter,
        EventInput {
            source: moe_agent_runtime::Source::Host,
            detail: DetailLevel::Essential,
            elapsed_ms: None,
            kind,
        },
        ClockReading {
            monotonic_ms: now_ms,
            wall_clock_ms: None,
        },
    );
}

fn gateway_diagnostic_code(error: &AppError) -> moe_agent_runtime::ErrorCode {
    let Some(gateway) = error
        .source()
        .and_then(|source| source.downcast_ref::<moe_agent_runtime::GatewayError>())
    else {
        return moe_agent_runtime::ErrorCode::Unclassified;
    };
    match gateway {
        moe_agent_runtime::GatewayError::Unauthorized
        | moe_agent_runtime::GatewayError::RouteDenied
        | moe_agent_runtime::GatewayError::Revoked
        | moe_agent_runtime::GatewayError::ForeignRequest => {
            moe_agent_runtime::ErrorCode::AuthorizationDenied
        }
        _ => moe_agent_runtime::ErrorCode::Unclassified,
    }
}

fn mark_error_gateway(
    diagnostics: &mut DiagnosticsService,
    emitter: Option<&moe_agent_runtime::LogEmitter>,
    now_ms: u64,
    stage: Step,
    error: &AppError,
) {
    mark(
        diagnostics,
        emitter,
        now_ms,
        DiagnosticKind::Error {
            code: gateway_diagnostic_code(error),
            stage,
        },
    );
}

fn mark_error(
    diagnostics: &mut DiagnosticsService,
    emitter: Option<&moe_agent_runtime::LogEmitter>,
    now_ms: u64,
    stage: Step,
    _error: &AppError,
) {
    mark(
        diagnostics,
        emitter,
        now_ms,
        DiagnosticKind::Error {
            code: moe_agent_runtime::ErrorCode::Unclassified,
            stage,
        },
    );
}

fn run_display_name(alias: Option<OpaqueAlias>, names: &mut Vec<(OpaqueAlias, String)>) -> String {
    let Some(alias) = alias else {
        return String::from("unattributed");
    };
    if let Some((_, name)) = names.iter().find(|(existing, _)| *existing == alias) {
        return name.clone();
    }
    let name = format!("run-{}", names.len() + 1);
    names.push((alias, name.clone()));
    name
}

fn event_code_text(code: EventCode) -> &'static str {
    use moe_agent_runtime::{
        BackgroundCode as B, ErrorCode as E, OperationState as O, Step as S, TransportState as T,
    };
    match code {
        EventCode::Background(B::ServiceStarted) => "background.service_started",
        EventCode::Background(B::Waiting) => "background.waiting",
        EventCode::Background(B::Initialized) => "background.initialized",
        EventCode::Background(B::BackendUnavailable) => "background.backend_unavailable",
        EventCode::Background(B::ProcessCrashed) => "process.crashed",
        EventCode::Error(E::AuthorizationDenied) => "error.authorization_denied",
        EventCode::Error(E::VersionConflict) => "error.version_conflict",
        EventCode::Error(E::ModelUnavailable) => "error.model_unavailable",
        EventCode::Error(E::WriteOutcomeUnknown) => "error.write_outcome_unknown",
        EventCode::Error(E::Unclassified) => "error.unclassified",
        EventCode::StageStarted(S::Admission) => "run.stage_started.admission",
        EventCode::StageStarted(S::Prepare) => "run.stage_started.prepare",
        EventCode::StageStarted(S::Infer) => "run.stage_started.infer",
        EventCode::StageStarted(S::Finish) => "run.stage_started.finish",
        EventCode::StageStarted(S::MemoryQuery) => "run.stage_started.memory_query",
        EventCode::StageStarted(S::HookWrite) => "run.stage_started.hook_write",
        EventCode::StageStarted(S::Lifecycle) => "run.stage_started.lifecycle",
        EventCode::StageReturned(S::Admission) => "run.stage_returned.admission",
        EventCode::StageReturned(S::Prepare) => "run.stage_returned.prepare",
        EventCode::StageReturned(S::Infer) => "run.stage_returned.infer",
        EventCode::StageReturned(S::Finish) => "run.stage_returned.finish",
        EventCode::StageReturned(S::MemoryQuery) => "run.stage_returned.memory_query",
        EventCode::StageReturned(S::HookWrite) => "run.stage_returned.hook_write",
        EventCode::StageReturned(S::Lifecycle) => "run.stage_returned.lifecycle",
        EventCode::Operation(O::InProgress) => "run.operation.in_progress",
        EventCode::Operation(O::Completed) => "run.operation.completed",
        EventCode::Operation(O::Failed) => "run.operation.failed",
        EventCode::Operation(O::Cancelled) => "run.operation.cancelled",
        EventCode::Operation(O::Unknown) => "run.operation.unknown",
        EventCode::Transport(T::NotAttempted) => "run.transport.not_attempted",
        EventCode::Transport(T::Written) => "run.transport.written",
        EventCode::Transport(T::Failed) => "run.transport.failed",
        EventCode::Transport(T::Cancelled) => "run.transport.cancelled",
        EventCode::Transport(T::Unknown) => "run.transport.unknown",
    }
}

fn entry_status_text(entry: &LogEntry) -> String {
    use moe_agent_runtime::{OperationState, Severity, TransportState};
    let mut status = match entry.severity() {
        Severity::Trace => "trace",
        Severity::Debug => "debug",
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
    .to_owned();
    match (entry.operation_state(), entry.transport_state()) {
        (Some(state), _) => {
            status.push('/');
            status.push_str(match state {
                OperationState::InProgress => "in_progress",
                OperationState::Completed => "completed",
                OperationState::Failed => "failed",
                OperationState::Cancelled => "cancelled",
                OperationState::Unknown => "unknown",
            });
        }
        (None, Some(state)) => {
            status.push('/');
            status.push_str(match state {
                TransportState::NotAttempted => "not_attempted",
                TransportState::Written => "written",
                TransportState::Failed => "failed",
                TransportState::Cancelled => "cancelled",
                TransportState::Unknown => "unknown",
            });
        }
        (None, None) => {}
    }
    status
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn real_character_exhaustion_rejects_before_registration_and_has_no_hidden_user_event() {
        let mut core = ChatCore::new(
            1,
            Box::new(crate::storage::VolatileMemory(
                moe_memory_core::MemoryService::new(),
            )),
            String::from("session-1"),
        )
        .unwrap();
        let text = "x".repeat(7_000);
        let mut completed = 0;
        loop {
            match core.send(&text, completed as u64 + 1) {
                Ok(()) => {
                    core.finish_display_publish(Ok(()), completed as u64 + 1)
                        .expect("display publication completes the staged node");
                    assert_eq!(core.chat_history.status, ChatStatus::Ready);
                    completed += 1;
                }
                Err(error) => {
                    assert_eq!(error.public_code(), "conversation_full");
                    break;
                }
            }
            assert!(completed < 32);
        }
        assert!(completed > 0);
        assert_eq!(core.admitted_events, completed * 2);
        assert_eq!(core.chat_history.messages.len(), completed * 2);
        let request = core.admit("audit").unwrap();
        let registration = core
            .sessions
            .register_user(
                SessionAccess::new(&core.ingress, &request, 100),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        let ledger = core
            .sessions
            .read_active(
                SessionAccess::new(&core.ingress, &request, 100),
                &registration.session,
            )
            .unwrap();
        assert_eq!(ledger.events.len(), completed * 2 + 1);
        assert!(
            ledger.events[..ledger.events.len() - 1]
                .iter()
                .all(|event| event.terminal().is_some())
        );
        let characters: usize = ledger
            .events
            .iter()
            .map(|event| event.body().chars().count())
            .sum();
        assert_eq!(characters - 5, core.admitted_chars);
        core.sessions
            .cancel_interaction(
                SessionAccess::new(&core.ingress, &request, 100),
                &registration.interaction,
            )
            .unwrap();
    }

    #[test]
    fn prepare_failure_receives_an_actual_cancellation_and_refunds_only_the_reply_reservation() {
        let mut core = ChatCore::new(
            2,
            Box::new(crate::storage::VolatileMemory(
                moe_memory_core::MemoryService::new(),
            )),
            String::from("session-1"),
        )
        .unwrap();
        core.memory
            .set_read_policy(&core.identity, ReadPolicy::deny_all(2))
            .unwrap();
        let error = core.send("denied", 1).err().unwrap();
        assert_eq!(error.public_code(), "send_cancelled");
        assert_eq!(
            error.execution_disposition(),
            crate::ExecutionDisposition::Settled
        );
        assert!(error.source().is_some());
        assert_eq!(core.admitted_events, 1);
        assert_eq!(core.admitted_chars, 6);
        assert!(core.pending_disposition.is_none());
        assert_eq!(core.chat_history.messages.len(), 1);
        let request = core.admit("audit").unwrap();
        let registration = core
            .sessions
            .register_user(
                SessionAccess::new(&core.ingress, &request, 2),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        let ledger = core
            .sessions
            .read_active(
                SessionAccess::new(&core.ingress, &request, 2),
                &registration.session,
            )
            .unwrap();
        assert_eq!(ledger.events[0].terminal(), Some(TerminalState::Cancelled));
        assert_eq!(ledger.events.len(), 2);
        core.sessions
            .cancel_interaction(
                SessionAccess::new(&core.ingress, &request, 2),
                &registration.interaction,
            )
            .unwrap();
    }

    #[test]
    fn external_unknown_at_deadline_settles_local_interaction_without_claiming_remote_cancel() {
        let mut core = ChatCore::new(
            27,
            Box::new(crate::storage::VolatileMemory(
                moe_memory_core::MemoryService::new(),
            )),
            String::from("session-1"),
        )
        .unwrap();
        core.sessions = SessionRuntime::new(
            &core.ingress,
            SessionLimits::new(8, 64, 256, 8_192, SESSION_CHARS_BUDGET, 2).unwrap(),
            SessionTiming::new(7_200_000, 180_000, 3_600_000, 300_000).unwrap(),
            0,
        );
        let text = "deadline unknown";
        let request = core.admit(text).unwrap();
        let registration = core
            .sessions
            .register_user(
                SessionAccess::new(&core.ingress, &request, 1),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        core.admitted_events = 2;
        core.admitted_chars = text.chars().count() + REPLY_CHARS_BUDGET;
        core.external_unknown_evidence = true;

        let error = core.settle_external_unknown(
            &request,
            &registration,
            AppError::from_gateway(moe_agent_runtime::GatewayError::DeadlineExceeded),
            None,
            3_600_001,
        );
        assert_eq!(error.public_code(), "send_unknown");
        assert_eq!(
            error.execution_disposition(),
            crate::ExecutionDisposition::Unknown
        );
        assert!(
            error.source().is_some(),
            "the original deadline error remains chained"
        );
        assert_eq!(
            core.admitted_events, 1,
            "only the reply reservation is refunded"
        );
        assert_eq!(core.admitted_chars, text.chars().count());
        assert!(core.pending_disposition.is_none());
        assert_eq!(core.chat_history.status, ChatStatus::Unknown);
        assert!(
            !core.session_isolation_allowed(),
            "volatile mode cannot discard its ledger"
        );

        let ledger = core
            .sessions
            .read_active(
                SessionAccess::new(&core.ingress, &request, 3_600_001),
                &registration.session,
            )
            .unwrap();
        assert_eq!(ledger.events.len(), 1);
        assert_eq!(ledger.events[0].terminal(), Some(TerminalState::Cancelled));
    }

    #[test]
    fn external_unknown_keeps_reservation_when_local_terminal_receipt_is_denied() {
        let mut core = ChatCore::new(
            28,
            Box::new(crate::storage::VolatileMemory(
                moe_memory_core::MemoryService::new(),
            )),
            String::from("session-1"),
        )
        .unwrap();
        let request = core.admit("unknown").unwrap();
        let registration = core
            .sessions
            .register_user(
                SessionAccess::new(&core.ingress, &request, 1),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        core.admitted_events = 2;
        core.admitted_chars = 6 + REPLY_CHARS_BUDGET;
        core.external_unknown_evidence = true;
        core.ingress.revoke_credential(core.credential).unwrap();

        let error = core.settle_external_unknown(
            &request,
            &registration,
            AppError::from_gateway(moe_agent_runtime::GatewayError::DeadlineExceeded),
            None,
            3_600_001,
        );
        assert_eq!(error.public_code(), "send_unknown");
        assert_eq!(
            error.execution_disposition(),
            crate::ExecutionDisposition::Unknown
        );
        assert!(error.disposition_error().is_some());
        assert!(error.source().is_some());
        assert_eq!(core.admitted_events, 2);
        assert_eq!(core.admitted_chars, 6 + REPLY_CHARS_BUDGET);
        assert!(core.pending_disposition.is_some());
        assert!(!core.session_isolation_allowed());
    }

    #[test]
    fn unexpected_backend_capacity_failure_during_finish_is_actually_cancelled_not_left_pending() {
        let mut core = ChatCore::new(
            3,
            Box::new(crate::storage::VolatileMemory(
                moe_memory_core::MemoryService::new(),
            )),
            String::from("session-1"),
        )
        .unwrap();
        core.sessions = SessionRuntime::new(
            &core.ingress,
            SessionLimits::new(8, 64, 256, 8_192, 1_040, 2).unwrap(),
            SessionTiming::default(),
            0,
        );
        let error = core.send(&"x".repeat(1_000), 1).err().unwrap();
        assert_eq!(error.public_code(), "send_cancelled");
        assert_eq!(core.admitted_events, 1);
        assert_eq!(core.admitted_chars, 1_000);
        let request = core.admit("audit").unwrap();
        let registration = core
            .sessions
            .register_user(
                SessionAccess::new(&core.ingress, &request, 2),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        let ledger = core
            .sessions
            .read_active(
                SessionAccess::new(&core.ingress, &request, 2),
                &registration.session,
            )
            .unwrap();
        assert_eq!(ledger.events[0].terminal(), Some(TerminalState::Cancelled));
        assert_eq!(ledger.events.len(), 2);
        assert_eq!(core.chat_history.messages.len(), 1);
        core.sessions
            .cancel_interaction(
                SessionAccess::new(&core.ingress, &request, 2),
                &registration.interaction,
            )
            .unwrap();
    }

    #[test]
    fn denied_disposition_retains_both_errors_and_quarantines_new_sends() {
        let mut core = ChatCore::new(
            4,
            Box::new(crate::storage::VolatileMemory(
                moe_memory_core::MemoryService::new(),
            )),
            String::from("session-1"),
        )
        .unwrap();
        let request = core.admit("quarantine").unwrap();
        let registration = core
            .sessions
            .register_user(
                SessionAccess::new(&core.ingress, &request, 1),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        core.ingress.revoke_credential(core.credential).unwrap();
        let primary = AppError::from_gateway(moe_agent_runtime::GatewayError::Revoked);
        let error = core.settle_after_failure(&request, &registration, primary, None, 1);
        assert_eq!(error.public_code(), "send_unknown");
        assert_eq!(
            error.execution_disposition(),
            crate::ExecutionDisposition::Unknown
        );
        assert!(error.disposition_error().is_some());
        assert!(
            error
                .source()
                .unwrap()
                .source()
                .unwrap()
                .downcast_ref::<AppError>()
                .is_some()
        );
        assert!(core.pending_disposition.is_some());
        assert_eq!(core.chat_history.status, ChatStatus::Unknown);
        let before = core.chat_history.messages.len();
        assert_eq!(
            core.send("do not retry", 2).err().unwrap().public_code(),
            "send_unknown"
        );
        assert_eq!(core.chat_history.messages.len(), before);
    }

    #[test]
    fn gateway_diagnostic_classification_uses_the_original_error_variant() {
        use moe_agent_runtime::ErrorCode;

        for error in [
            moe_agent_runtime::GatewayError::Unauthorized,
            moe_agent_runtime::GatewayError::RouteDenied,
            moe_agent_runtime::GatewayError::Revoked,
            moe_agent_runtime::GatewayError::ForeignRequest,
        ] {
            assert_eq!(
                gateway_diagnostic_code(&AppError::from_gateway(error)),
                ErrorCode::AuthorizationDenied
            );
        }
        for error in [
            moe_agent_runtime::GatewayError::CapacityExceeded,
            moe_agent_runtime::GatewayError::ClockWentBackwards,
            moe_agent_runtime::GatewayError::SessionExpired,
            moe_agent_runtime::GatewayError::PresetNotFound,
        ] {
            assert_eq!(
                gateway_diagnostic_code(&AppError::from_gateway(error)),
                ErrorCode::Unclassified
            );
        }
        assert_eq!(
            gateway_diagnostic_code(&AppError::rejection(
                "gateway_rejected",
                "safe",
                "not a gateway error",
            )),
            ErrorCode::Unclassified
        );
    }
}
