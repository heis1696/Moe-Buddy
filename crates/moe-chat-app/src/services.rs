//! 受信业务组合：可信入口、会话运行时、离线 Agent 流程、记忆服务、
//! API 预设注册表与纯内存诊断日志的窄门面。
//!
//! 本模块只组合底层 crate 的既有受信接口，不复制业务规则、不接文件/
//! 网络/系统时钟；全部时间由宿主经 `ChatApplication::dispatch` 注入。
//! 诊断采集失败不改变业务结果，健康计数经授权查询可见。

use std::collections::BTreeMap;
use std::error::Error;

mod external;

use moe_agent_runtime::{
    AgentAccess, AgentLimits, AgentRuntime, AgentServices, AgentWorkflow, BackgroundCode,
    CallerSubject, CaptureStrategy, CategorySet, ChannelId, ChannelScope, ClockReading,
    CredentialId, DetailLevel, DiagnosticKind, DiagnosticsConfig, DiagnosticsService, EventCode,
    EventInput, Gap, HardBudgets, IngressToken, LogAccess, LogEntry, LogFilter, ModelOutcome,
    OfflineInput, OfflineModel, OpaqueAlias, PageDirection, PresentationEvent, PresetConfig,
    PresetId, PresetPurpose, PresetRegistry, RawIngressCall, Registration, ReplyTarget,
    RunObservation, SessionAccess, SessionLimits, SessionRuntime, SessionTiming, StableDomain,
    Step, TerminalState, TrustedIngress, TrustedRequest, UserEvent, UserEventKind, ViewerGrant,
    ViewerSubject,
};
use moe_memory_core::{
    Category, Epistemic, MemoryDraft, MemoryScope, Namespace, PetIdentity, ReadPolicy,
    RetrievalQuery, SearchScope, Sensitivity, SessionId, SourceRef,
};

use crate::contract::{
    ChatMessage, ChatSnapshot, ChatStatus, DiagnosticSnapshot, MemorySnapshot, MessageRole,
    PresetSnapshot,
};
use crate::error::AppError;

/// 稳定伙伴标识；同一离线应用实例内不变。
pub(crate) const COMPANION: &str = "offline-companion";
/// 应用层输入字节上限（与 `contract::MAX_TEXT_BYTES` 一致）与记忆正文字数上限。
pub(crate) const REMEMBER_MAX_CHARS: usize = 4_096;
/// 诊断面板单页与记忆列展示的有界条数。
pub(crate) const DIAGNOSTICS_PAGE: usize = 8;
pub(crate) const MEMORY_LIST_LIMIT: usize = 100;

const SESSION_EVENTS_BUDGET: usize = 64;
const SESSION_CHARS_BUDGET: usize = 65_536;
const REPLY_CHARS_BUDGET: usize = 4_096;

const ROUTE_ALIAS: &str = "buddy/offline";
const SESSION_HINT: &str = "offline-acceptance-session";
const CALLER: &str = "local-trusted-host";
const CHANNEL: &str = "local-chat";
const SCOPE: &str = "private-window";
const REPLY_TARGET: &str = "chat-window";
const TOKEN_VALUE: &str = "offline-acceptance-token-not-for-production";
const PERSONA_TEXT: &str = "离线验收人设：本机离线验收伙伴；回答必须标明离线验收，不冒充真实模型。";
const DIAGNOSTICS_DOMAIN: &str = "moe-chat-offline-companion";
const DIAGNOSTICS_VIEWER: &str = "local-authorized-viewer";
const DIAGNOSTICS_GRANT_VERSION: u64 = 1;
/// 授权记忆读取绑定的记忆会话标识（应用自有，不从客户端输入拼接）。
const MEMORY_READER_SESSION: &str = "offline-memory-view";

/// 一次发送经真实链路后的结果；结论只来自实际 `AgentDelivery`。
pub(crate) struct SendOutcome {
    pub(crate) user_text: String,
    pub(crate) state: TerminalState,
    pub(crate) reply: Option<String>,
}

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
    pub(crate) online: bool,
    pub(crate) transmission_allowed: bool,
    pending_external: Option<(TrustedRequest, Registration, moe_agent_runtime::AgentTask)>,
    pub(crate) external_input: Option<moe_agent_runtime::ExternalInput>,
    settings_version: u64,
    history_store: Option<(
        Box<dyn crate::ChatHistory>,
        TrustedRequest,
        moe_agent_runtime::RawHistoryGrant,
        moe_agent_runtime::RawRetention,
    )>,
    ingress: TrustedIngress,
    credential: CredentialId,
    presets: PresetRegistry,
    preset_ids: Vec<PresetId>,
    preset_catalog: Vec<PresetSnapshot>,
    sessions: SessionRuntime,
    memory: Box<dyn crate::ChatMemory>,
    agent: AgentRuntime,
    diagnostics: DiagnosticsService,
    domain: StableDomain,
    viewer: ViewerSubject,
    base_emitter: moe_agent_runtime::LogEmitter,
    identity: PetIdentity,
    persona_source: SourceRef,
    notes_source: SourceRef,
    reader_session: SessionId,
    chat_history: ChatSnapshot,
    admitted_events: usize,
    admitted_chars: usize,
    pending_disposition: Option<Registration>,
    revoked: bool,
    note_ids: BTreeMap<String, moe_memory_core::MemoryId>,
}

impl ChatCore {
    pub(crate) fn new(
        process_generation: u64,
        mut memory: Box<dyn crate::ChatMemory>,
    ) -> Result<Self, AppError> {
        let identity = PetIdentity::new(
            Namespace::new("moe-chat", "local").map_err(AppError::from_memory)?,
            COMPANION,
        )
        .map_err(AppError::from_memory)?;

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
                8,
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

        let persona_source = memory
            .register_source(
                MemoryScope::Private(identity.clone()),
                "persona",
                1,
                Epistemic::AuthoritativeState,
            )
            .map_err(AppError::from_memory)?;
        let notes_source = memory
            .register_source(
                MemoryScope::Private(identity.clone()),
                "explicit-notes",
                1,
                Epistemic::UserStatement,
            )
            .map_err(AppError::from_memory)?;
        memory
            .set_identity_view(&identity, 1, PERSONA_TEXT, vec![persona_source.clone()])
            .map_err(AppError::from_memory)?;
        memory
            .set_read_policy(
                &identity,
                ReadPolicy::offline(1, vec![persona_source.clone(), notes_source.clone()]),
            )
            .map_err(AppError::from_memory)?;

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
            online: false,
            transmission_allowed: false,
            pending_external: None,
            external_input: None,
            settings_version: 1,
            history_store: None,
            ingress,
            credential,
            presets,
            preset_ids,
            preset_catalog,
            sessions,
            memory,
            agent,
            diagnostics,
            domain,
            viewer,
            base_emitter,
            identity,
            persona_source,
            notes_source,
            reader_session: SessionId::new(MEMORY_READER_SESSION).map_err(AppError::from_memory)?,
            chat_history: ChatSnapshot {
                draft: String::new(),
                messages: Vec::new(),
                status: ChatStatus::Ready,
                failure: None,
            },
            admitted_events: 0,
            admitted_chars: 0,
            pending_disposition: None,
            revoked: false,
            note_ids: BTreeMap::new(),
        })
    }

    pub(crate) fn is_revoked(&self) -> bool {
        self.revoked
    }

    pub(crate) fn mark_revoked(&mut self) {
        self.revoked = true;
    }

    /// 撤权服务侧收口：撤销入口凭据并停用诊断采集（清空全部诊断缓冲）。
    pub(crate) fn revoke_services(&mut self) -> Result<(), AppError> {
        self.pending_external = None;
        self.external_input = None;
        self.transmission_allowed = false;
        self.pending_disposition = None;
        self.chat_history.messages.clear();
        self.chat_history.status = ChatStatus::Ready;
        self.chat_history.failure = None;
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

    /// 一轮文字聊天：可信入口接纳 → 会话登记 → 授权绑定 →
    /// `AgentWorkflow::prepare/infer/finish` → 以实际交付判定结果。
    pub(crate) fn send(&mut self, text: &str, now_ms: u64) -> Result<(), AppError> {
        self.ensure_active()?;
        if self.online {
            return self.begin_external(text, now_ms);
        }
        if self.pending_disposition.is_some() || self.chat_history.status == ChatStatus::Unknown {
            return Err(AppError::rejection(
                "send_unknown",
                "上一轮登记后的结果未能确认，已隔离会话；请勿重发，请重启宿主",
                "new admission blocked by a quarantined interaction",
            ));
        }
        if self.chat_history.messages.len() > 126 {
            return Err(AppError::rejection(
                "conversation_full",
                "本次内存会话已达到容量上限，请重启宿主开始新会话",
                "conversation display budget exhausted before admission",
            ));
        }
        let outcome = self.send_inner(text, now_ms);
        match &outcome {
            Ok(delivery) => {
                self.chat_history.messages.push(ChatMessage {
                    role: MessageRole::User,
                    text: delivery.user_text.clone(),
                });
                let (status, failure) = match delivery.state {
                    TerminalState::Completed => {
                        if let Some(reply) = &delivery.reply {
                            self.chat_history.messages.push(ChatMessage {
                                role: MessageRole::Assistant,
                                text: reply.clone(),
                            });
                        }
                        (ChatStatus::Ready, None)
                    }
                    TerminalState::Failed => (ChatStatus::Failed, Some("send_failed")),
                    TerminalState::Cancelled => (ChatStatus::Failed, Some("send_cancelled")),
                    TerminalState::Unknown => (ChatStatus::Unknown, Some("send_unknown")),
                };
                self.chat_history.status = status;
                self.chat_history.failure = failure.map(str::to_owned);
            }
            Err(error) => {
                if matches!(error.public_code(), "send_cancelled" | "send_unknown") {
                    self.chat_history.messages.push(ChatMessage {
                        role: MessageRole::User,
                        text: text.to_owned(),
                    });
                }
                self.chat_history.status = if error.public_code() == "send_unknown" {
                    ChatStatus::Unknown
                } else {
                    ChatStatus::Failed
                };
                self.chat_history.failure = Some(error.public_code().to_owned());
            }
        }
        outcome.map(|_| ())
    }

    pub(crate) fn chat_history(&self) -> Result<ChatSnapshot, AppError> {
        self.ensure_active()?;
        Ok(self.chat_history.clone())
    }

    fn send_inner(&mut self, text: &str, now_ms: u64) -> Result<SendOutcome, AppError> {
        self.ensure_active()?;
        let user_chars = text.chars().count();
        if self.admitted_events + 2 > SESSION_EVENTS_BUDGET
            || self.admitted_chars + user_chars + REPLY_CHARS_BUDGET > SESSION_CHARS_BUDGET
        {
            return Err(AppError::rejection(
                "conversation_full",
                "会话容量不足以容纳输入和回复，该轮尚未登记；请重启宿主开始新会话",
                "admission refused before registration: reply budget unavailable",
            ));
        }
        let workflow = AgentWorkflow::start()
            .map_err(|error| AppError::from_workflow::<AppError>(error.into()))?;
        let emitter = self.open_run();
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Admission)),
        );
        let request = match self.admit(text) {
            Ok(request) => request,
            Err(error) => {
                mark_error_gateway(
                    &mut self.diagnostics,
                    emitter.as_ref(),
                    now_ms,
                    Step::Admission,
                    &error,
                );
                return Err(error);
            }
        };
        mark(
            &mut self.diagnostics,
            emitter.as_ref(),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::Admission)),
        );

        let registration = match self.sessions.register_user(
            SessionAccess::new(&self.ingress, &request, now_ms),
            UserEvent::new(UserEventKind::New),
        ) {
            Ok(registration) => registration,
            Err(error) => {
                let error = AppError::from_gateway(error);
                mark_error_gateway(
                    &mut self.diagnostics,
                    emitter.as_ref(),
                    now_ms,
                    Step::Admission,
                    &error,
                );
                return Err(error);
            }
        };
        self.admitted_events += 2;
        self.admitted_chars += user_chars + REPLY_CHARS_BUDGET;
        match self.run_registered(&request, &registration, workflow, emitter.as_ref(), now_ms) {
            Ok(outcome) => {
                let reply_chars = outcome
                    .reply
                    .as_ref()
                    .map_or(0, |reply| reply.chars().count());
                self.admitted_chars -= REPLY_CHARS_BUDGET - reply_chars;
                if outcome.reply.is_none() {
                    self.admitted_events -= 1;
                }
                Ok(outcome)
            }
            Err(primary) => Err(self.settle_after_failure(
                &request,
                &registration,
                primary,
                emitter.as_ref(),
                now_ms,
            )),
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

    fn run_registered(
        &mut self,
        request: &TrustedRequest,
        registration: &Registration,
        mut workflow: AgentWorkflow,
        emitter: Option<&moe_agent_runtime::LogEmitter>,
        now_ms: u64,
    ) -> Result<SendOutcome, AppError> {
        let access = AgentAccess::new(&self.ingress, request, &self.presets, now_ms);
        let sources = self.grant_sources();
        let binding = match self.agent.bind_session(
            &access,
            &mut self.sessions,
            &registration.session,
            sources,
        ) {
            Ok(binding) => binding,
            Err(error) => {
                let error = AppError::from(error);
                mark_error(
                    &mut self.diagnostics,
                    emitter,
                    now_ms,
                    Step::Prepare,
                    &error,
                );
                return Err(error);
            }
        };

        mark(
            &mut self.diagnostics,
            emitter,
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Prepare)),
        );
        let query = RetrievalQuery::new("");
        if let Err(error) = workflow.prepare(
            &mut self.agent,
            &access,
            AgentServices::new(&mut self.sessions, self.memory.as_mut()),
            &binding,
            &registration.interaction,
            &query,
        ) {
            let error = AppError::from_workflow(error);
            mark_error(
                &mut self.diagnostics,
                emitter,
                now_ms,
                Step::Prepare,
                &error,
            );
            return Err(error);
        }
        mark(
            &mut self.diagnostics,
            emitter,
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::Prepare)),
        );

        mark(
            &mut self.diagnostics,
            emitter,
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Infer)),
        );
        let mut model = OfflineVerifierModel;
        if let Err(error) = workflow.infer(
            &self.agent,
            &access,
            AgentServices::new(&mut self.sessions, self.memory.as_mut()),
            &mut model,
        ) {
            let error = AppError::from_workflow(error);
            mark_error(&mut self.diagnostics, emitter, now_ms, Step::Infer, &error);
            return Err(error);
        }
        mark(
            &mut self.diagnostics,
            emitter,
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::Infer)),
        );

        mark(
            &mut self.diagnostics,
            emitter,
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Finish)),
        );
        let delivery = match workflow.finish(
            &self.agent,
            &access,
            AgentServices::new(&mut self.sessions, self.memory.as_mut()),
        ) {
            Ok(delivery) => delivery,
            Err(error) => {
                let error = AppError::from_workflow(error);
                mark_error(&mut self.diagnostics, emitter, now_ms, Step::Finish, &error);
                return Err(error);
            }
        };
        let state = delivery.state();
        mark(
            &mut self.diagnostics,
            emitter,
            now_ms,
            DiagnosticKind::Run(RunObservation::Operation(match state {
                TerminalState::Completed => moe_agent_runtime::OperationState::Completed,
                TerminalState::Failed => moe_agent_runtime::OperationState::Failed,
                TerminalState::Cancelled => moe_agent_runtime::OperationState::Cancelled,
                TerminalState::Unknown => moe_agent_runtime::OperationState::Unknown,
            })),
        );
        mark(
            &mut self.diagnostics,
            emitter,
            now_ms,
            DiagnosticKind::Run(RunObservation::Transport(
                moe_agent_runtime::TransportState::NotAttempted,
            )),
        );
        mark(
            &mut self.diagnostics,
            emitter,
            now_ms,
            DiagnosticKind::Run(RunObservation::StageReturned(Step::Finish)),
        );
        Ok(SendOutcome {
            user_text: request.text().to_owned(),
            state,
            reply: delivery.text().map(str::to_owned),
        })
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
                let error = AppError::from_memory(error);
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
        self.note_ids
            .insert(receipt.id().as_u64().to_string(), receipt.id());
        Ok(receipt.id().as_u64())
    }

    /// 经授权检索列出私密长期记忆（离线用途，与 Agent 同一读取边界）。
    pub(crate) fn search_memories(
        &mut self,
        text: &str,
        now_ms: u64,
    ) -> Result<Vec<MemorySnapshot>, AppError> {
        self.ensure_active()?;
        let query = RetrievalQuery::new(text)
            .with_scope(SearchScope::Private)
            .with_limit(MEMORY_LIST_LIMIT);
        let result = self
            .memory
            .retrieve(&self.identity, &self.reader_session, &query, now_ms)?;
        for item in result.items() {
            if let moe_memory_core::RetrievalItem::LongTerm(record) = item
                && record.sources() == [self.notes_source.clone()]
            {
                self.note_ids
                    .insert(record.id().as_u64().to_string(), record.id());
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
        let id = *self.note_ids.get(id).ok_or_else(|| {
            AppError::rejection(
                "memory_not_found",
                "这条记忆已不可用，请刷新列表",
                "note is not an issued private note",
            )
        })?;
        // Only explicit notes issued by this application are editable. The memory
        // service still verifies ownership, sources, protection and revision.
        if let Some(text) = text {
            let draft = MemoryDraft::new(
                text,
                Category::UserProfile,
                Epistemic::UserStatement,
                vec![self.notes_source.clone()],
            )
            .with_sensitivity(Sensitivity::Private);
            self.memory
                .update(&self.identity, id, revision, draft, now_ms)
                .map_err(AppError::from_memory)?;
        } else {
            self.memory
                .delete(&self.identity, id, revision, now_ms)
                .map_err(AppError::from_memory)?;
        }
        Ok(())
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
            })?;
        self.presets
            .bind(PresetPurpose::Chat, target)
            .map_err(AppError::from_gateway)?;
        mark(
            &mut self.diagnostics,
            Some(&self.base_emitter),
            now_ms,
            DiagnosticKind::Run(RunObservation::StageStarted(Step::Lifecycle)),
        );
        mark(
            &mut self.diagnostics,
            Some(&self.base_emitter),
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
        Ok(DiagnosticsPage {
            entries,
            gap_possible: matches!(page.gap, Gap::Possible),
        })
    }

    fn ensure_active(&self) -> Result<(), AppError> {
        if self.revoked {
            return Err(AppError::rejection(
                "revoked",
                "本次授权已撤销，应用不再接受任何操作",
                "authorization revoked for this process",
            ));
        }
        Ok(())
    }

    fn admit(&mut self, text: &str) -> Result<TrustedRequest, AppError> {
        let token = IngressToken::new(TOKEN_VALUE).map_err(AppError::from_gateway)?;
        self.ingress
            .admit(
                RawIngressCall::new(token, ROUTE_ALIAS, text.to_owned())
                    .with_session_hint(SESSION_HINT),
            )
            .map_err(AppError::from_gateway)
    }

    fn grant_sources(&self) -> Vec<SourceRef> {
        vec![self.persona_source.clone(), self.notes_source.clone()]
    }

    /// 为一次业务动作申请运行与采集句柄；任一失败都降级为无采集，
    /// 不阻塞业务（健康计数经授权查询可见）。
    fn open_run(&mut self) -> Option<moe_agent_runtime::LogEmitter> {
        let run = self.diagnostics.create_run(self.domain).ok();
        self.diagnostics
            .create_emitter(self.domain, run.as_ref())
            .ok()
    }
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
        )
        .unwrap();
        let text = "x".repeat(7_000);
        let mut completed = 0;
        loop {
            match core.send(&text, completed as u64 + 1) {
                Ok(()) => {
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
        )
        .unwrap();
        core.memory
            .set_read_policy(&core.identity, ReadPolicy::deny_all(2))
            .unwrap();
        let error = core.send("denied", 1).err().unwrap();
        assert_eq!(error.public_code(), "send_cancelled");
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
    fn unexpected_backend_capacity_failure_during_finish_is_actually_cancelled_not_left_pending() {
        let mut core = ChatCore::new(
            3,
            Box::new(crate::storage::VolatileMemory(
                moe_memory_core::MemoryService::new(),
            )),
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
