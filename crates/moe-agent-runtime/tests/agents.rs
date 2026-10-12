use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_agent_runtime::{
    AgentAccess, AgentBinding, AgentDelivery, AgentError, AgentLimits, AgentResult, AgentRuntime,
    AgentServices, AgentTask, CallerSubject, ChannelId, ChannelScope, EventRole, GatewayError,
    IngressToken, ModelFailure, ModelOutcome, OfflineInput, OfflineModel, PresentationEvent,
    PresetConfig, PresetId, PresetPurpose, PresetRegistry, RawIngressCall, Registration,
    RelatedEvent, ReplyTarget, SessionAccess, SessionLimits, SessionRuntime, SessionTiming,
    TerminalState, TrustedIngress, TrustedRequest, UserEvent, UserEventKind,
};
use moe_memory_core::{
    Category, ContextView, EmbeddingProfile, Epistemic, Lifecycle, MemoryContextAccess,
    MemoryDraft, MemoryError, MemoryId, MemoryScope, MemoryService, MemoryTask, Namespace,
    PetIdentity, ReadPolicy, ReadPurpose, RequestContext, RerankProfile, RetrievalPlan,
    RetrievalQuery, SourceRef, TransmissionPolicy, TransmissionUsage,
};
use moe_memory_local::{LocalMemory, OperationId, PersistenceError};

const USER: &str = "synthetic-private-user-text";
const PERSONA: &str = "synthetic-protected-persona";
const FACT: &str = "synthetic-authorized-fact";

fn config(model: &str) -> PresetConfig {
    PresetConfig::new(
        "synthetic",
        "offline-fixture",
        "https://example.invalid/v1",
        None,
        model,
        BTreeMap::new(),
    )
    .unwrap()
}

fn limits() -> AgentLimits {
    AgentLimits::new(16, 64, 2048, 4096).unwrap()
}

struct Fixture {
    ingress: TrustedIngress,
    presets: PresetRegistry,
    sessions: SessionRuntime,
    memory: MemoryService,
    agent: AgentRuntime,
    request: TrustedRequest,
    registration: Registration,
    binding: AgentBinding,
    authority: SourceRef,
    source: SourceRef,
    record: MemoryId,
}

impl Fixture {
    fn new(limits: AgentLimits) -> Self {
        let identity = PetIdentity::new(
            Namespace::new("synthetic-workspace", "synthetic-owner").unwrap(),
            "stable-pet",
        )
        .unwrap();
        let mut ingress = TrustedIngress::new();
        ingress
            .register_route("buddy/synthetic", identity.clone())
            .unwrap();
        for (token, channel, target) in [
            ("desktop-token", "desktop", "desktop-target"),
            ("qq-token", "qq-fixture", "qq-target"),
        ] {
            ingress
                .install_credential(
                    IngressToken::new(token).unwrap(),
                    CallerSubject::new("synthetic-caller").unwrap(),
                    ["buddy/synthetic".to_owned()],
                    ChannelId::new(channel).unwrap(),
                    ChannelScope::new("private-thread").unwrap(),
                    ReplyTarget::new(target).unwrap(),
                )
                .unwrap();
        }
        let mut presets = PresetRegistry::new(4).unwrap();
        let id = PresetId::new("synthetic-chat").unwrap();
        presets
            .create(id.clone(), config("synthetic-model"))
            .unwrap();
        presets.bind(PresetPurpose::Chat, id).unwrap();
        let request = ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new("desktop-token").unwrap(),
                    "buddy/synthetic",
                    USER,
                )
                .with_session_hint("same-client-hint"),
            )
            .unwrap();
        let mut sessions = SessionRuntime::new(
            &ingress,
            SessionLimits::new(16, 128, 512, 8192, 65536, 4).unwrap(),
            SessionTiming::default(),
            0,
        );
        let registration = sessions
            .register_user(
                SessionAccess::new(&ingress, &request, 0),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        let mut memory = MemoryService::new();
        let source = memory
            .register_source(
                MemoryScope::Private(identity.clone()),
                "synthetic-source",
                1,
                Epistemic::UserStatement,
            )
            .unwrap();
        let authority = memory
            .register_source(
                MemoryScope::Private(identity.clone()),
                "synthetic-persona-source",
                1,
                Epistemic::AuthoritativeState,
            )
            .unwrap();
        let record = memory
            .for_pet(&identity)
            .create(
                MemoryDraft::new(
                    FACT,
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![source.clone()],
                ),
                0,
            )
            .unwrap()
            .id();
        memory
            .set_identity_view(&identity, 1, PERSONA, vec![authority.clone()])
            .unwrap();
        memory
            .set_read_policy(
                &identity,
                ReadPolicy::offline(1, vec![source.clone(), authority.clone()]),
            )
            .unwrap();
        let mut agent = AgentRuntime::new(&ingress, limits).unwrap();
        let binding = agent
            .bind_session(
                &AgentAccess::new(&ingress, &request, &presets, 0),
                &mut sessions,
                &registration.session,
                vec![source.clone(), authority.clone()],
            )
            .unwrap();
        Self {
            ingress,
            presets,
            sessions,
            memory,
            agent,
            request,
            registration,
            binding,
            authority,
            source,
            record,
        }
    }

    fn prepare_result(&mut self, now: u64) -> Result<AgentTask, AgentError> {
        self.agent.prepare(
            &AgentAccess::new(&self.ingress, &self.request, &self.presets, now),
            AgentServices::new(&mut self.sessions, &mut self.memory),
            &self.binding,
            &self.registration.interaction,
            &RetrievalQuery::new(""),
        )
    }

    fn prepare(&mut self) -> AgentTask {
        self.prepare_result(0).unwrap()
    }

    fn infer_result(
        &mut self,
        task: &mut AgentTask,
        model: &mut Probe,
        now: u64,
    ) -> Result<AgentResult, AgentError> {
        self.agent.infer(
            &AgentAccess::new(&self.ingress, &self.request, &self.presets, now),
            AgentServices::new(&mut self.sessions, &mut self.memory),
            task,
            model,
        )
    }

    fn finish(
        &mut self,
        task: &mut AgentTask,
        result: &AgentResult,
        now: u64,
    ) -> Result<AgentDelivery, AgentError> {
        self.agent.finish(
            &AgentAccess::new(&self.ingress, &self.request, &self.presets, now),
            AgentServices::new(&mut self.sessions, &mut self.memory),
            task,
            result,
        )
    }

    fn snapshot(&mut self, now: u64) -> moe_agent_runtime::SessionSnapshot {
        self.sessions
            .read_active(
                SessionAccess::new(&self.ingress, &self.request, now),
                &self.registration.session,
            )
            .unwrap()
    }

    fn working(&mut self, text: &str, revision: u64, now: u64) {
        self.memory
            .put_working_context(
                self.request.identity(),
                self.binding.memory_session(),
                "synthetic-working",
                revision,
                MemoryDraft::new(
                    text,
                    Category::WorkingContext,
                    Epistemic::UserStatement,
                    vec![self.source.clone()],
                )
                .with_lifecycle(Lifecycle::ShortTerm),
                now,
            )
            .unwrap();
    }

    fn other_request(&mut self, token: &str, text: &str) -> TrustedRequest {
        self.ingress
            .admit(
                RawIngressCall::new(IngressToken::new(token).unwrap(), "buddy/synthetic", text)
                    .with_session_hint("same-client-hint"),
            )
            .unwrap()
    }
}

#[derive(Default)]
struct Probe {
    outcome: Option<ModelOutcome>,
    calls: usize,
    identity: Option<PetIdentity>,
    channel: String,
    memory: Vec<String>,
    conversation: Vec<(EventRole, String)>,
    debug: String,
}

impl OfflineModel for Probe {
    fn infer(&mut self, input: OfflineInput<'_>) -> ModelOutcome {
        self.calls += 1;
        self.identity = Some(input.identity().clone());
        self.channel = input.channel().as_str().to_owned();
        self.memory = input
            .memory()
            .iter()
            .map(|block| block.text().to_owned())
            .collect();
        self.conversation = input
            .conversation()
            .iter()
            .map(|event| (event.role(), event.body().to_owned()))
            .collect();
        self.debug = format!("{input:?}");
        assert!(!input.tools_enabled());
        assert_eq!(input.scope().as_str(), "private-thread");
        assert_eq!(input.preset_id(), &PresetId::new("synthetic-chat").unwrap());
        assert!(input.preset_version() > 0);
        self.outcome.take().unwrap_or_else(|| ModelOutcome::Text {
            text: format!(
                "{} | {} | {}",
                input.persona(),
                self.memory.join(";"),
                input.user_text()
            ),
            presentation: vec![PresentationEvent::Happy],
        })
    }
}

#[test]
fn persona_memory_and_raw_context_participate_before_one_inference() {
    let mut fixture = Fixture::new(limits());
    fixture.working("synthetic-session-context", 1, 0);
    fixture
        .sessions
        .append_related(
            SessionAccess::new(&fixture.ingress, &fixture.request, 0),
            &fixture.registration.interaction,
            RelatedEvent::new(EventRole::ExternalAgent, "synthetic-agent-quote"),
        )
        .unwrap();
    let generation = fixture.memory.generation();
    let mut task = fixture.prepare();
    let mut model = Probe::default();
    let result = fixture.infer_result(&mut task, &mut model, 1).unwrap();
    let delivery = fixture.finish(&mut task, &result, 2).unwrap();
    assert_eq!(model.calls, 1);
    assert_eq!(model.identity.as_ref(), Some(fixture.request.identity()));
    assert_eq!(
        model.conversation,
        vec![
            (EventRole::User, USER.to_owned()),
            (EventRole::ExternalAgent, "synthetic-agent-quote".to_owned())
        ]
    );
    assert!(delivery.text().unwrap().contains(PERSONA));
    assert!(delivery.text().unwrap().contains(FACT));
    assert!(
        delivery
            .text()
            .unwrap()
            .contains("synthetic-session-context")
    );
    assert_eq!(delivery.state(), TerminalState::Completed);
    assert_eq!(delivery.channel().as_str(), "desktop");
    assert_eq!(delivery.reply_target().as_str(), "desktop-target");
    assert_eq!(delivery.presentation(), &[PresentationEvent::Happy]);
    assert_eq!(fixture.memory.generation(), generation);
    let snapshot = fixture.snapshot(2);
    assert_eq!(
        snapshot.events.last().unwrap().body(),
        delivery.text().unwrap()
    );
    assert_eq!(snapshot.events.last().unwrap().role(), EventRole::Assistant);
}

#[test]
fn text_and_presentation_are_separate_without_postprocessing() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let mut model = Probe {
        outcome: Some(ModelOutcome::Text {
            text: "  code\nprint(1)\n  ".to_owned(),
            presentation: vec![PresentationEvent::Thinking],
        }),
        ..Probe::default()
    };
    let result = fixture.infer_result(&mut task, &mut model, 0).unwrap();
    let delivery = fixture.finish(&mut task, &result, 0).unwrap();
    assert_eq!(delivery.text(), Some("  code\nprint(1)\n  "));
    assert_eq!(delivery.presentation(), &[PresentationEvent::Thinking]);
}

#[test]
fn core_read_denial_is_explainable_and_never_calls_model() {
    let mut fixture = Fixture::new(limits());
    fixture
        .memory
        .set_read_policy(fixture.request.identity(), ReadPolicy::deny_all(2))
        .unwrap();
    assert!(matches!(
        fixture.prepare_result(0),
        Err(AgentError::Memory(MemoryError::AccessDenied))
    ));
    assert_eq!(
        AgentError::Memory(MemoryError::AccessDenied).to_string(),
        "read purpose is not authorized"
    );
    assert_eq!(fixture.snapshot(0).events.len(), 1);
}

#[test]
fn absent_or_unauthorized_persona_does_not_run_bare_model() {
    for remove in [true, false] {
        let mut fixture = Fixture::new(limits());
        if remove {
            fixture
                .memory
                .remove_identity_view(fixture.request.identity(), 1)
                .unwrap();
        } else {
            fixture
                .memory
                .set_read_policy(
                    fixture.request.identity(),
                    ReadPolicy::offline(2, vec![fixture.source.clone()]),
                )
                .unwrap();
        }
        assert!(matches!(
            fixture.prepare_result(0),
            Err(AgentError::PersonaUnavailable)
        ));
    }
}

#[test]
fn channel_grant_checks_the_entire_actual_read_set_before_budget_omissions() {
    let mut fixture =
        Fixture::new(AgentLimits::new(16, 64, PERSONA.chars().count(), 4096).unwrap());
    let other = fixture.other_request("qq-token", "qq-private-text");
    let registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &other, 0),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    let binding = fixture
        .agent
        .bind_session(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            &mut fixture.sessions,
            &registration.session,
            vec![fixture.authority.clone()],
        )
        .unwrap();
    let error = fixture
        .agent
        .prepare(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &binding,
            &registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap_err();
    assert_eq!(error, AgentError::MemoryScopeDenied);
    assert!(!error.to_string().contains(FACT));
}

#[test]
fn channels_share_stable_identity_not_raw_text_working_context_or_grants() {
    let mut fixture = Fixture::new(limits());
    fixture.working("desktop-only-working", 1, 0);
    let other = fixture.other_request("qq-token", "qq-only-raw-text");
    let registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &other, 0),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    let binding = fixture
        .agent
        .bind_session(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            &mut fixture.sessions,
            &registration.session,
            vec![fixture.authority.clone(), fixture.source.clone()],
        )
        .unwrap();
    assert_ne!(fixture.binding.memory_session(), binding.memory_session());
    let mut desktop_task = fixture.prepare();
    let mut desktop_model = Probe::default();
    fixture
        .infer_result(&mut desktop_task, &mut desktop_model, 0)
        .unwrap();
    let mut other_task = fixture
        .agent
        .prepare(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &binding,
            &registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let mut other_model = Probe::default();
    fixture
        .agent
        .infer(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &mut other_task,
            &mut other_model,
        )
        .unwrap();
    assert_eq!(desktop_model.identity, other_model.identity);
    assert!(
        desktop_model
            .memory
            .contains(&"desktop-only-working".to_owned())
    );
    assert!(
        !other_model
            .memory
            .contains(&"desktop-only-working".to_owned())
    );
    assert_eq!(
        other_model.conversation,
        vec![(EventRole::User, "qq-only-raw-text".to_owned())]
    );
    assert_eq!(
        desktop_model.conversation,
        vec![(EventRole::User, USER.to_owned())]
    );
    assert_eq!(other_model.channel, "qq-fixture");
    assert!(matches!(
        fixture.agent.prepare(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &fixture.binding,
            &registration.interaction,
            &RetrievalQuery::new("")
        ),
        Err(AgentError::Gateway(GatewayError::Revoked))
    ));
}

#[test]
fn original_target_survives_other_channel_activity_and_fragment_expiry() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 1)
        .unwrap();
    let other = fixture.other_request("qq-token", "newer-qq");
    fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &other, 1_800_000),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    let newer = fixture.other_request("desktop-token", "new-desktop-fragment");
    let new_registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &newer, 1_800_000),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    assert_ne!(new_registration.session, fixture.registration.session);
    let delivery = fixture.finish(&mut task, &result, 1_800_001).unwrap();
    assert_eq!(delivery.reply_target().as_str(), "desktop-target");
    assert_eq!(delivery.channel().as_str(), "desktop");
    assert!(matches!(
        fixture.sessions.read_active(
            SessionAccess::new(&fixture.ingress, &fixture.request, 1_800_001),
            &fixture.registration.session
        ),
        Err(GatewayError::SessionExpired)
    ));
    let snapshot = fixture
        .sessions
        .read_active(
            SessionAccess::new(&fixture.ingress, &newer, 1_800_001),
            &new_registration.session,
        )
        .unwrap();
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.events[0].body(), "new-desktop-fragment");
}

#[test]
fn new_events_do_not_enter_already_captured_context() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let newer = fixture.other_request("desktop-token", "newer-user-text");
    fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &newer, 1),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    let mut model = Probe::default();
    fixture.infer_result(&mut task, &mut model, 2).unwrap();
    assert_eq!(model.conversation, vec![(EventRole::User, USER.to_owned())]);
}

#[test]
fn source_revocation_rejects_late_result() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 0)
        .unwrap();
    fixture.memory.revoke_source(&fixture.source).unwrap();
    assert!(matches!(
        fixture.finish(&mut task, &result, 1),
        Err(AgentError::Memory(MemoryError::TaskCancelled))
    ));
    assert_eq!(fixture.snapshot(1).events.len(), 1);
}

#[test]
fn policy_revocation_rejects_late_result() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 0)
        .unwrap();
    fixture
        .memory
        .set_read_policy(fixture.request.identity(), ReadPolicy::deny_all(2))
        .unwrap();
    assert!(matches!(
        fixture.finish(&mut task, &result, 1),
        Err(AgentError::Memory(MemoryError::TaskCancelled))
    ));
}

#[test]
fn binding_revocation_blocks_inference_and_late_delivery() {
    for inferred in [false, true] {
        let mut fixture = Fixture::new(limits());
        let mut task = fixture.prepare();
        let result = inferred.then(|| {
            fixture
                .infer_result(&mut task, &mut Probe::default(), 0)
                .unwrap()
        });
        fixture.agent.revoke_binding(&fixture.binding).unwrap();
        let error = if let Some(result) = result {
            fixture.finish(&mut task, &result, 1).unwrap_err()
        } else {
            let mut model = Probe::default();
            let error = fixture.infer_result(&mut task, &mut model, 1).unwrap_err();
            assert_eq!(model.calls, 0);
            error
        };
        assert_eq!(error, AgentError::Gateway(GatewayError::Revoked));
    }
}

#[test]
fn cancellation_rejects_late_result_without_success() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 0)
        .unwrap();
    fixture
        .sessions
        .cancel_interaction(
            SessionAccess::new(&fixture.ingress, &fixture.request, 1),
            &fixture.registration.interaction,
        )
        .unwrap();
    assert!(matches!(
        fixture.finish(&mut task, &result, 2),
        Err(AgentError::Gateway(GatewayError::StaleHandle))
    ));
    let snapshot = fixture.snapshot(2);
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(
        snapshot.events[0].terminal(),
        Some(TerminalState::Cancelled)
    );
}

#[test]
fn session_revocation_rejects_late_result() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 0)
        .unwrap();
    fixture
        .sessions
        .revoke_session(
            SessionAccess::new(&fixture.ingress, &fixture.request, 1),
            &fixture.registration.session,
        )
        .unwrap();
    assert!(matches!(
        fixture.finish(&mut task, &result, 2),
        Err(AgentError::Gateway(GatewayError::Revoked))
    ));
}

#[test]
fn core_global_cancel_blocks_actual_model_use() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    fixture
        .memory
        .cancel_tasks(fixture.memory.generation())
        .unwrap();
    let mut model = Probe::default();
    assert!(matches!(
        fixture.infer_result(&mut task, &mut model, 1),
        Err(AgentError::Memory(MemoryError::TaskCancelled))
    ));
    assert_eq!(model.calls, 0);
}

#[test]
fn changed_persona_record_and_working_versions_reject_late_result() {
    for change in [0, 1, 2] {
        let mut fixture = Fixture::new(limits());
        fixture.working("working-version-one", 1, 0);
        let mut task = fixture.prepare();
        let result = fixture
            .infer_result(&mut task, &mut Probe::default(), 0)
            .unwrap();
        match change {
            0 => fixture
                .memory
                .set_identity_view(
                    fixture.request.identity(),
                    2,
                    "changed-persona",
                    vec![fixture.authority.clone()],
                )
                .unwrap(),
            1 => {
                fixture
                    .memory
                    .for_pet(fixture.request.identity())
                    .delete(fixture.record, 1, 1)
                    .unwrap();
            }
            _ => fixture.working("working-version-two", 2, 1),
        }
        assert!(matches!(
            fixture.finish(&mut task, &result, 2),
            Err(AgentError::Memory(MemoryError::TaskCancelled))
        ));
    }
}

#[test]
fn omitted_actual_read_items_remain_fenced() {
    let mut fixture =
        Fixture::new(AgentLimits::new(16, 64, PERSONA.chars().count(), 4096).unwrap());
    let mut task = fixture.prepare();
    let mut model = Probe::default();
    let result = fixture.infer_result(&mut task, &mut model, 0).unwrap();
    assert!(model.memory.is_empty());
    fixture
        .memory
        .for_pet(fixture.request.identity())
        .delete(fixture.record, 1, 1)
        .unwrap();
    assert!(matches!(
        fixture.finish(&mut task, &result, 2),
        Err(AgentError::Memory(MemoryError::TaskCancelled))
    ));
}

#[test]
fn preset_update_delete_rebind_aba_and_registry_replacement_reject_results() {
    for change in [0, 1, 2, 3] {
        let mut fixture = Fixture::new(limits());
        let mut task = fixture.prepare();
        let result = fixture
            .infer_result(&mut task, &mut Probe::default(), 0)
            .unwrap();
        let id = PresetId::new("synthetic-chat").unwrap();
        let version = fixture
            .presets
            .resolve(PresetPurpose::Chat)
            .unwrap()
            .version();
        match change {
            0 => {
                fixture
                    .presets
                    .update(&id, version, config("replacement-model"))
                    .unwrap();
            }
            1 => {
                fixture.presets.delete(&id, version).unwrap();
                fixture
                    .presets
                    .create(id.clone(), config("synthetic-model"))
                    .unwrap();
            }
            2 => {
                fixture.presets.unbind(PresetPurpose::Chat);
                fixture.presets.bind(PresetPurpose::Chat, id).unwrap();
            }
            _ => {
                let mut registry = PresetRegistry::new(4).unwrap();
                registry
                    .create(id.clone(), config("synthetic-model"))
                    .unwrap();
                registry.bind(PresetPurpose::Chat, id).unwrap();
                fixture.presets = registry;
            }
        }
        assert!(matches!(
            fixture.finish(&mut task, &result, 1),
            Err(AgentError::Gateway(GatewayError::VersionConflict))
        ));
    }
}

#[test]
fn preset_binding_aba_is_rejected_before_model_use() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let id = fixture
        .presets
        .resolve(PresetPurpose::Chat)
        .unwrap()
        .id()
        .clone();
    fixture.presets.unbind(PresetPurpose::Chat);
    fixture.presets.bind(PresetPurpose::Chat, id).unwrap();
    let mut model = Probe::default();
    assert!(matches!(
        fixture.infer_result(&mut task, &mut model, 1),
        Err(AgentError::Gateway(GatewayError::VersionConflict))
    ));
    assert_eq!(model.calls, 0);
    let snapshot = fixture.snapshot(1);
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.events[0].terminal(), None);
}

#[test]
fn prepared_task_can_start_inference_after_fragment_expiry_without_revival() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let newer = fixture.other_request("desktop-token", "new-fragment-after-expiry");
    let registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &newer, 1_800_000),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    assert_ne!(registration.session, fixture.registration.session);
    let mut model = Probe::default();
    let result = fixture
        .infer_result(&mut task, &mut model, 1_800_001)
        .unwrap();
    assert_eq!(model.calls, 1);
    assert_eq!(model.conversation, vec![(EventRole::User, USER.to_owned())]);
    let delivery = fixture.finish(&mut task, &result, 1_800_002).unwrap();
    assert_eq!(delivery.state(), TerminalState::Completed);
    assert_eq!(delivery.reply_target().as_str(), "desktop-target");
    assert!(matches!(
        fixture.sessions.read_active(
            SessionAccess::new(&fixture.ingress, &fixture.request, 1_800_002),
            &fixture.registration.session,
        ),
        Err(GatewayError::SessionExpired)
    ));
    let snapshot = fixture
        .sessions
        .read_active(
            SessionAccess::new(&fixture.ingress, &newer, 1_800_002),
            &registration.session,
        )
        .unwrap();
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.events[0].body(), "new-fragment-after-expiry");
}

#[test]
fn failed_preset_mutation_does_not_invalidate_task() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    assert_eq!(
        fixture.presets.update(
            &PresetId::new("synthetic-chat").unwrap(),
            999,
            config("other")
        ),
        Err(GatewayError::VersionConflict)
    );
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 0)
        .unwrap();
    assert_eq!(
        fixture.finish(&mut task, &result, 0).unwrap().state(),
        TerminalState::Completed
    );
}

#[test]
fn missing_binding_does_not_fallback_or_consume_task_budget() {
    let mut fixture = Fixture::new(AgentLimits::new(16, 1, 2048, 4096).unwrap());
    fixture.presets.unbind(PresetPurpose::Chat);
    assert!(matches!(
        fixture.prepare_result(0),
        Err(AgentError::Gateway(GatewayError::MissingBinding))
    ));
    fixture
        .presets
        .bind(
            PresetPurpose::Chat,
            PresetId::new("synthetic-chat").unwrap(),
        )
        .unwrap();
    fixture.prepare();
}

#[test]
fn ingress_configuration_change_blocks_use_and_late_result() {
    for inferred in [false, true] {
        let mut fixture = Fixture::new(limits());
        let mut task = fixture.prepare();
        let result = inferred.then(|| {
            fixture
                .infer_result(&mut task, &mut Probe::default(), 0)
                .unwrap()
        });
        fixture
            .ingress
            .revoke_credential(fixture.request.credential())
            .unwrap();
        let error = if let Some(result) = result {
            fixture.finish(&mut task, &result, 1).unwrap_err()
        } else {
            fixture
                .infer_result(&mut task, &mut Probe::default(), 1)
                .unwrap_err()
        };
        assert_eq!(error, AgentError::Gateway(GatewayError::Revoked));
    }
}

#[test]
fn foreign_runtime_service_request_and_result_cannot_cross_bind() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 0)
        .unwrap();
    let foreign = AgentRuntime::new(&fixture.ingress, limits()).unwrap();
    assert!(matches!(
        foreign.finish(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &mut task,
            &result
        ),
        Err(AgentError::Gateway(GatewayError::ForeignRequest))
    ));
    let mut other_memory = fixture.memory.clone();
    other_memory
        .cancel_tasks(other_memory.generation())
        .unwrap();
    assert!(matches!(
        fixture.agent.finish(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut other_memory),
            &mut task,
            &result
        ),
        Err(AgentError::Memory(MemoryError::TaskCancelled))
    ));
    let other = fixture.other_request("desktop-token", "other-request");
    assert!(matches!(
        fixture.agent.finish(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &mut task,
            &result
        ),
        Err(AgentError::Gateway(GatewayError::ForeignRequest))
    ));
    assert_eq!(
        fixture.finish(&mut task, &result, 0).unwrap().state(),
        TerminalState::Completed
    );
}

#[test]
fn results_are_private_and_tied_to_one_task() {
    let mut fixture = Fixture::new(limits());
    let mut first = fixture.prepare();
    let first_result = fixture
        .infer_result(&mut first, &mut Probe::default(), 0)
        .unwrap();
    let other = fixture.other_request("desktop-token", "second-interaction");
    let registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &other, 0),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    let mut second = fixture
        .agent
        .prepare(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &fixture.binding,
            &registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let second_result = fixture
        .agent
        .infer(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &mut second,
            &mut Probe::default(),
        )
        .unwrap();
    assert!(matches!(
        fixture.finish(&mut first, &second_result, 0),
        Err(AgentError::Gateway(GatewayError::ForeignRequest))
    ));
    assert_eq!(
        fixture
            .finish(&mut first, &first_result, 0)
            .unwrap()
            .state(),
        TerminalState::Completed
    );
}

#[test]
fn repeated_preparation_and_inference_do_not_repeat_model_use() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    assert!(matches!(
        fixture.prepare_result(0),
        Err(AgentError::TaskAlreadyIssued)
    ));
    let mut model = Probe::default();
    let result = fixture.infer_result(&mut task, &mut model, 0).unwrap();
    assert!(matches!(
        fixture.infer_result(&mut task, &mut model, 0),
        Err(AgentError::InvalidTaskState)
    ));
    fixture.finish(&mut task, &result, 0).unwrap();
    assert!(fixture.finish(&mut task, &result, 0).is_err());
    assert_eq!(model.calls, 1);
    assert_eq!(fixture.snapshot(0).events.len(), 2);
}

#[test]
fn deadline_boundary_and_backwards_time_reject_without_reply() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 2)
        .unwrap();
    assert!(matches!(
        fixture.finish(&mut task, &result, 1),
        Err(AgentError::Gateway(GatewayError::ClockWentBackwards))
    ));
    assert!(matches!(
        fixture.finish(&mut task, &result, 3_600_000),
        Err(AgentError::Gateway(GatewayError::DeadlineExceeded))
    ));
}

#[test]
fn protected_persona_budget_failure_does_not_issue_task() {
    let mut fixture = Fixture::new(AgentLimits::new(16, 1, 1, 4096).unwrap());
    assert!(matches!(
        fixture.prepare_result(0),
        Err(AgentError::Memory(
            MemoryError::ContextBudgetExceeded { .. }
        ))
    ));
    assert!(matches!(
        fixture.prepare_result(0),
        Err(AgentError::Memory(
            MemoryError::ContextBudgetExceeded { .. }
        ))
    ));
}

#[test]
fn bounded_bindings_and_cumulative_task_budget_fail_explicitly() {
    let mut fixture = Fixture::new(AgentLimits::new(1, 1, 2048, 4096).unwrap());
    fixture.prepare();
    let other = fixture.other_request("desktop-token", "second-user-event");
    let registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &other, 0),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    assert!(matches!(
        fixture.agent.prepare(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &fixture.binding,
            &registration.interaction,
            &RetrievalQuery::new("")
        ),
        Err(AgentError::Gateway(GatewayError::CapacityExceeded))
    ));
    let qq = fixture.other_request("qq-token", "qq-user-event");
    let registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &qq, 0),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    assert!(matches!(
        fixture.agent.bind_session(
            &AgentAccess::new(&fixture.ingress, &qq, &fixture.presets, 0),
            &mut fixture.sessions,
            &registration.session,
            vec![fixture.authority.clone()]
        ),
        Err(AgentError::Gateway(GatewayError::CapacityExceeded))
    ));
    fixture.agent.revoke_binding(&fixture.binding).unwrap();
    fixture.agent.revoke_binding(&fixture.binding).unwrap();
}

#[test]
fn same_fragment_binding_is_idempotent_but_cannot_widen_grants() {
    let mut fixture = Fixture::new(limits());
    let binding = fixture
        .agent
        .bind_session(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            &mut fixture.sessions,
            &fixture.registration.session,
            vec![fixture.source.clone(), fixture.authority.clone()],
        )
        .unwrap();
    assert_eq!(binding.memory_session(), fixture.binding.memory_session());
    assert!(matches!(
        fixture.agent.bind_session(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            &mut fixture.sessions,
            &fixture.registration.session,
            vec![fixture.authority.clone()]
        ),
        Err(AgentError::Gateway(GatewayError::EventConflict))
    ));
    let mut foreign = AgentRuntime::new(&fixture.ingress, limits()).unwrap();
    assert_eq!(
        foreign.revoke_binding(&fixture.binding),
        Err(GatewayError::ForeignRequest)
    );
}

#[test]
fn task_with_working_context_still_cannot_persist_derived_memory() {
    let mut fixture = Fixture::new(limits());
    fixture.working("synthetic-working-evidence", 1, 0);
    let task = fixture
        .memory
        .for_session(
            fixture.request.identity(),
            fixture.binding.memory_session(),
            ReadPurpose::OfflineReasoning,
        )
        .begin_task(&RetrievalQuery::new(""), 0)
        .unwrap();
    assert_eq!(
        task.propose_create(
            MemoryDraft::new(
                "derived",
                Category::Episode,
                Epistemic::UserStatement,
                vec![fixture.source.clone()]
            ),
            0
        )
        .unwrap_err(),
        MemoryError::InvalidInput("session-derived output cannot be persisted")
    );
}

#[test]
fn debug_and_errors_never_print_sensitive_text_or_model_output() {
    let mut fixture = Fixture::new(limits());
    fixture.working("synthetic-private-working", 1, 0);
    let mut task = fixture.prepare();
    let mut model = Probe::default();
    let result = fixture.infer_result(&mut task, &mut model, 0).unwrap();
    let delivery = fixture.finish(&mut task, &result, 0).unwrap();
    let debug = format!(
        "{:?} {:?} {task:?} {result:?} {delivery:?} {} {:?}",
        fixture.agent,
        fixture.binding,
        model.debug,
        AgentError::<MemoryError>::MemoryScopeDenied
    );
    for secret in [
        USER,
        PERSONA,
        FACT,
        "synthetic-private-working",
        "desktop-token",
        "qq-token",
        "desktop-target",
    ] {
        assert!(!debug.contains(secret), "debug leaked a synthetic sentinel");
    }
}

#[test]
fn source_revision_and_authority_revocation_block_actual_inference() {
    for change in [0, 1] {
        let mut fixture = Fixture::new(limits());
        let mut task = fixture.prepare();
        if change == 0 {
            fixture
                .memory
                .register_source(
                    MemoryScope::Private(fixture.request.identity().clone()),
                    "synthetic-source",
                    2,
                    Epistemic::UserStatement,
                )
                .unwrap();
        } else {
            fixture.memory.revoke_source(&fixture.authority).unwrap();
        }
        let mut model = Probe::default();
        assert!(matches!(
            fixture.infer_result(&mut task, &mut model, 1),
            Err(AgentError::Memory(MemoryError::TaskCancelled))
        ));
        assert_eq!(model.calls, 0);
    }
}

#[test]
fn read_item_expiry_at_boundary_rejects_late_result() {
    let mut fixture = Fixture::new(limits());
    fixture
        .memory
        .for_pet(fixture.request.identity())
        .create(
            MemoryDraft::new(
                "expiring-fact",
                Category::Episode,
                Epistemic::UserStatement,
                vec![fixture.source.clone()],
            )
            .with_expiration(2),
            0,
        )
        .unwrap();
    let mut task = fixture.prepare();
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 1)
        .unwrap();
    assert!(matches!(
        fixture.finish(&mut task, &result, 2),
        Err(AgentError::Memory(MemoryError::TaskCancelled))
    ));
}

#[test]
fn session_backpressure_does_not_publish_reply_or_terminal_success() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let mut model = Probe {
        outcome: Some(ModelOutcome::Text {
            text: "x".repeat(2048),
            presentation: vec![],
        }),
        ..Probe::default()
    };
    let result = fixture.infer_result(&mut task, &mut model, 0).unwrap();
    for size in [8192, 8192, 8192, 8192, 8192, 8192, 8192, 7000] {
        fixture
            .sessions
            .append_related(
                SessionAccess::new(&fixture.ingress, &fixture.request, 0),
                &fixture.registration.interaction,
                RelatedEvent::new(EventRole::ExternalAgent, "x".repeat(size)),
            )
            .unwrap();
    }
    let before = fixture.snapshot(0).events.len();
    for _ in 0..2 {
        assert!(matches!(
            fixture.finish(&mut task, &result, 0),
            Err(AgentError::Gateway(GatewayError::CapacityExceeded))
        ));
        let snapshot = fixture.snapshot(0);
        assert_eq!(snapshot.events.len(), before);
        assert_eq!(snapshot.events[0].terminal(), None);
    }
    assert_eq!(model.calls, 1);
}

#[test]
fn new_fragment_and_recreated_runtime_do_not_reuse_working_session_keys() {
    let mut fixture = Fixture::new(limits());
    fixture.working("old-working-context", 1, 0);
    let other = fixture.other_request("desktop-token", "new-fragment-user");
    let registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &other, 1_800_000),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    let binding = fixture
        .agent
        .bind_session(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 1_800_000),
            &mut fixture.sessions,
            &registration.session,
            vec![fixture.source.clone(), fixture.authority.clone()],
        )
        .unwrap();
    let mut recreated = AgentRuntime::new(&fixture.ingress, limits()).unwrap();
    let recreated_binding = recreated
        .bind_session(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 1_800_000),
            &mut fixture.sessions,
            &registration.session,
            vec![fixture.source.clone(), fixture.authority.clone()],
        )
        .unwrap();
    assert_ne!(binding.memory_session(), fixture.binding.memory_session());
    assert_ne!(binding.memory_session(), recreated_binding.memory_session());
    let mut task = fixture
        .agent
        .prepare(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 1_800_000),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &binding,
            &registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let mut model = Probe::default();
    fixture
        .agent
        .infer(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 1_800_000),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &mut task,
            &mut model,
        )
        .unwrap();
    assert!(!model.memory.contains(&"old-working-context".to_owned()));
    assert_eq!(
        model.conversation,
        vec![(EventRole::User, "new-fragment-user".to_owned())]
    );
}

#[test]
fn history_events_are_not_new_agent_tasks() {
    let mut fixture = Fixture::new(limits());
    let other = fixture.other_request("desktop-token", "historical-input");
    let registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &other, 0),
            UserEvent::new(UserEventKind::History),
        )
        .unwrap();
    assert!(matches!(
        fixture.agent.prepare(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &fixture.binding,
            &registration.interaction,
            &RetrievalQuery::new("")
        ),
        Err(AgentError::Gateway(GatewayError::InteractionClosed))
    ));
}

#[test]
fn grant_must_include_protected_persona_sources() {
    let mut fixture = Fixture::new(limits());
    let other = fixture.other_request("qq-token", "restricted-qq");
    let registration = fixture
        .sessions
        .register_user(
            SessionAccess::new(&fixture.ingress, &other, 0),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    let binding = fixture
        .agent
        .bind_session(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            &mut fixture.sessions,
            &registration.session,
            vec![fixture.source.clone()],
        )
        .unwrap();
    assert!(matches!(
        fixture.agent.prepare(
            &AgentAccess::new(&fixture.ingress, &other, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut fixture.memory),
            &binding,
            &registration.interaction,
            &RetrievalQuery::new("")
        ),
        Err(AgentError::MemoryScopeDenied)
    ));
}

#[test]
fn foreign_memory_epoch_and_session_coordinator_reject_late_results() {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let result = fixture
        .infer_result(&mut task, &mut Probe::default(), 0)
        .unwrap();
    let mut memory = MemoryService::new();
    assert!(matches!(
        fixture.agent.finish(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut memory),
            &mut task,
            &result
        ),
        Err(AgentError::Memory(MemoryError::TaskCancelled))
    ));
    let mut sessions = SessionRuntime::new(
        &fixture.ingress,
        SessionLimits::new(1, 1, 1, 32, 32, 1).unwrap(),
        SessionTiming::default(),
        0,
    );
    assert!(matches!(
        fixture.agent.finish(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            AgentServices::new(&mut sessions, &mut fixture.memory),
            &mut task,
            &result
        ),
        Err(AgentError::Gateway(GatewayError::ForeignRequest))
    ));
}

#[test]
fn invalid_limits_and_excess_source_grants_fail_without_publishing() {
    for values in [[0, 1, 1, 1], [1, 0, 1, 1], [1, 1, 0, 1], [1, 1, 1, 0]] {
        assert!(AgentLimits::new(values[0], values[1], values[2], values[3]).is_err());
    }
    let mut fixture = Fixture::new(limits());
    assert!(matches!(
        fixture.agent.bind_session(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            &mut fixture.sessions,
            &fixture.registration.session,
            vec![fixture.source.clone(); 257]
        ),
        Err(AgentError::Gateway(GatewayError::CapacityExceeded))
    ));
    fixture.prepare();
}

fn nonsuccess(outcome: ModelOutcome, state: TerminalState) {
    let mut fixture = Fixture::new(limits());
    let mut task = fixture.prepare();
    let mut model = Probe {
        outcome: Some(outcome),
        ..Probe::default()
    };
    let result = fixture.infer_result(&mut task, &mut model, 0).unwrap();
    let delivery = fixture.finish(&mut task, &result, 1).unwrap();
    assert_eq!(delivery.state(), state);
    assert!(delivery.text().is_none());
    assert!(delivery.presentation().is_empty());
    let snapshot = fixture.snapshot(1);
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.events[0].terminal(), Some(state));
}

#[test]
fn model_failure_has_no_reply_or_success_receipt() {
    nonsuccess(
        ModelOutcome::Failed(ModelFailure::Unavailable),
        TerminalState::Failed,
    );
}

#[test]
fn model_cancellation_is_not_success() {
    nonsuccess(ModelOutcome::Cancelled, TerminalState::Cancelled);
}

#[test]
fn unknown_model_outcome_is_not_success() {
    nonsuccess(ModelOutcome::Unknown, TerminalState::Unknown);
}

#[test]
fn malformed_and_oversized_model_outputs_fail_closed() {
    for (text, presentation) in [
        ("".to_owned(), vec![]),
        (" \n".to_owned(), vec![]),
        ("bad\0text".to_owned(), vec![]),
        ("x".repeat(4097), vec![]),
        ("text".to_owned(), vec![PresentationEvent::Neutral; 17]),
    ] {
        let mut fixture = Fixture::new(limits());
        let mut task = fixture.prepare();
        let mut model = Probe {
            outcome: Some(ModelOutcome::Text { text, presentation }),
            ..Probe::default()
        };
        let result = fixture.infer_result(&mut task, &mut model, 0).unwrap();
        let delivery = fixture.finish(&mut task, &result, 0).unwrap();
        assert_eq!(delivery.state(), TerminalState::Failed);
        assert_eq!(delivery.failure(), Some(ModelFailure::InvalidOutput));
        assert!(delivery.text().is_none());
        assert!(delivery.presentation().is_empty());
    }
}

static NEXT_LOCAL_JOURNAL: AtomicU64 = AtomicU64::new(1);

struct JournalPath(PathBuf);

impl JournalPath {
    fn new() -> Self {
        let suffix = NEXT_LOCAL_JOURNAL.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "moe-agent-local-memory-{}-{suffix}.journal",
            std::process::id()
        )))
    }
}

impl Drop for JournalPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct RecoveryRequiredAccess;

impl MemoryContextAccess for RecoveryRequiredAccess {
    type Error = PersistenceError;

    fn begin_memory_task(
        &mut self,
        _: &PetIdentity,
        _: &moe_memory_core::SessionId,
        _: ReadPurpose,
        _: &RetrievalQuery,
        _: u64,
    ) -> Result<MemoryTask, Self::Error> {
        Err(PersistenceError::RecoveryRequired)
    }

    fn assemble_context(
        &mut self,
        _: &MemoryTask,
        _: usize,
        _: u64,
    ) -> Result<RequestContext, Self::Error> {
        Err(PersistenceError::RecoveryRequired)
    }

    fn context_view<'context>(
        &mut self,
        _: &'context RequestContext,
        _: u64,
    ) -> Result<&'context ContextView, Self::Error> {
        Err(PersistenceError::RecoveryRequired)
    }
}

struct FaultableMemory {
    memory: MemoryService,
    faulted: bool,
}

impl FaultableMemory {
    fn ready(&self) -> Result<(), PersistenceError> {
        if self.faulted {
            Err(PersistenceError::RecoveryRequired)
        } else {
            Ok(())
        }
    }
}

impl MemoryContextAccess for FaultableMemory {
    type Error = PersistenceError;

    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &moe_memory_core::SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<MemoryTask, Self::Error> {
        self.ready()?;
        self.memory
            .begin_memory_task(identity, session, purpose, query, now_ms)
            .map_err(PersistenceError::from)
    }

    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget_chars: usize,
        now_ms: u64,
    ) -> Result<RequestContext, Self::Error> {
        self.ready()?;
        self.memory
            .assemble_context(task, budget_chars, now_ms)
            .map_err(PersistenceError::from)
    }

    fn context_view<'context>(
        &mut self,
        context: &'context RequestContext,
        now_ms: u64,
    ) -> Result<&'context ContextView, Self::Error> {
        self.ready()?;
        self.memory
            .context_view(context, now_ms)
            .map_err(PersistenceError::from)
    }
}

struct HybridAccess {
    memory: MemoryService,
    query_source: SourceRef,
    selected_source: SourceRef,
    endpoint: &'static str,
}

impl MemoryContextAccess for HybridAccess {
    type Error = MemoryError;

    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &moe_memory_core::SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<MemoryTask, Self::Error> {
        let plan = RetrievalPlan::new(EmbeddingProfile::new("synthetic", "embedding", 4)?)
            .with_rerank(RerankProfile::new("synthetic", "reranker")?);
        let mut ticket = self
            .memory
            .for_session(identity, session, purpose)
            .begin_candidates(query, plan, now_ms)?;
        let selected = ticket
            .candidates()
            .iter()
            .find(|candidate| candidate.item().sources().contains(&self.selected_source))
            .ok_or(MemoryError::NotFound)?
            .candidate_ref();
        let references: Vec<_> = ticket
            .candidates()
            .iter()
            .map(|candidate| candidate.candidate_ref())
            .collect();
        self.memory.bind_query_provenance(
            &mut ticket,
            vec![self.query_source.clone()],
            &[],
            now_ms,
        )?;
        let sent = self.memory.prepare_transmission(
            &ticket,
            TransmissionUsage::Rerank,
            self.endpoint,
            &references,
            now_ms,
        )?;
        self.memory
            .record_transmission(&mut ticket, &sent, now_ms)?;
        self.memory
            .for_session(identity, session, purpose)
            .complete_retrieval(&ticket, &[(selected, 1.0)], now_ms)
    }

    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget_chars: usize,
        now_ms: u64,
    ) -> Result<RequestContext, Self::Error> {
        self.memory.assemble_context(task, budget_chars, now_ms)
    }

    fn context_view<'context>(
        &mut self,
        context: &'context RequestContext,
        now_ms: u64,
    ) -> Result<&'context ContextView, Self::Error> {
        self.memory.context_view(context, now_ms)
    }
}

#[test]
fn local_memory_can_be_injected_through_the_context_trait_object() {
    let mut fixture = Fixture::new(limits());
    let journal = JournalPath::new();
    let mut memory = LocalMemory::create(&journal.0).unwrap();
    let identity = fixture.request.identity().clone();
    let source = memory
        .register_source(
            OperationId::new("local-fact-source").unwrap(),
            MemoryScope::Private(identity.clone()),
            "synthetic-source",
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone();
    let authority = memory
        .register_source(
            OperationId::new("local-persona-source").unwrap(),
            MemoryScope::Private(identity.clone()),
            "synthetic-persona-source",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap()
        .value()
        .clone();
    memory
        .for_pet(&identity)
        .create(
            OperationId::new("local-fact").unwrap(),
            MemoryDraft::new(
                FACT,
                Category::Episode,
                Epistemic::UserStatement,
                vec![source.clone()],
            ),
            0,
        )
        .unwrap();
    memory
        .set_identity_view(&identity, 1, PERSONA, vec![authority.clone()])
        .unwrap();
    memory
        .set_read_policy(
            &identity,
            ReadPolicy::offline(1, vec![source.clone(), authority.clone()]),
        )
        .unwrap();
    let mut agent = AgentRuntime::new(&fixture.ingress, limits()).unwrap();
    let access = AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0);
    let binding = agent
        .bind_session(
            &access,
            &mut fixture.sessions,
            &fixture.registration.session,
            vec![source, authority],
        )
        .unwrap();
    let memory_access: &mut dyn MemoryContextAccess<Error = PersistenceError> = &mut memory;
    let mut task = agent
        .prepare(
            &access,
            AgentServices::new(&mut fixture.sessions, memory_access),
            &binding,
            &fixture.registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let mut model = Probe::default();
    let result = agent
        .infer(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 1),
            AgentServices::new(&mut fixture.sessions, &mut memory),
            &mut task,
            &mut model,
        )
        .unwrap();
    let delivery = agent
        .finish(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 2),
            AgentServices::new(&mut fixture.sessions, &mut memory),
            &mut task,
            &result,
        )
        .unwrap();
    assert_eq!(delivery.state(), TerminalState::Completed);
    assert!(delivery.text().unwrap().contains(PERSONA));
    assert!(delivery.text().unwrap().contains(FACT));
    assert_eq!(model.calls, 1);
}

#[test]
fn backend_health_failure_is_preserved_and_never_reaches_the_model() {
    let mut fixture = Fixture::new(limits());
    let mut memory = RecoveryRequiredAccess;
    let error = fixture
        .agent
        .prepare(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut memory),
            &fixture.binding,
            &fixture.registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        AgentError::Memory(PersistenceError::RecoveryRequired)
    ));
    assert_eq!(
        std::error::Error::source(&error).unwrap().to_string(),
        "journal failed; close and reopen before any further access"
    );
    let model = Probe::default();
    assert_eq!(model.calls, 0);
}

#[test]
fn backend_failure_after_prepare_is_preserved_and_the_model_is_never_called() {
    let mut fixture = Fixture::new(limits());
    let memory = std::mem::replace(&mut fixture.memory, MemoryService::new());
    let mut backend = FaultableMemory {
        memory,
        faulted: false,
    };
    let mut task = fixture
        .agent
        .prepare(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut backend),
            &fixture.binding,
            &fixture.registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    backend.faulted = true;
    let mut model = Probe::default();
    let error = fixture
        .agent
        .infer(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 1),
            AgentServices::new(&mut fixture.sessions, &mut backend),
            &mut task,
            &mut model,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        AgentError::Memory(PersistenceError::RecoveryRequired)
    ));
    assert_eq!(
        std::error::Error::source(&error).unwrap().to_string(),
        "journal failed; close and reopen before any further access"
    );
    assert_eq!(model.calls, 0);
}

#[test]
fn backend_failure_after_inference_is_preserved_and_registers_no_terminal_state() {
    let mut fixture = Fixture::new(limits());
    let memory = std::mem::replace(&mut fixture.memory, MemoryService::new());
    let mut backend = FaultableMemory {
        memory,
        faulted: false,
    };
    let mut task = fixture
        .agent
        .prepare(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut backend),
            &fixture.binding,
            &fixture.registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let mut model = Probe::default();
    let result = fixture
        .agent
        .infer(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 1),
            AgentServices::new(&mut fixture.sessions, &mut backend),
            &mut task,
            &mut model,
        )
        .unwrap();
    assert_eq!(model.calls, 1);
    assert!(model.memory.iter().any(|block| block.contains(FACT)));
    let events_before = fixture.snapshot(2).events.len();
    backend.faulted = true;
    let error = fixture
        .agent
        .finish(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 2),
            AgentServices::new(&mut fixture.sessions, &mut backend),
            &mut task,
            &result,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        AgentError::Memory(PersistenceError::RecoveryRequired)
    ));
    assert_eq!(
        std::error::Error::source(&error).unwrap().to_string(),
        "journal failed; close and reopen before any further access"
    );
    let snapshot = fixture.snapshot(3);
    assert_eq!(snapshot.events.len(), events_before);
    assert_eq!(snapshot.events[0].terminal(), None);
    assert!(
        snapshot
            .events
            .iter()
            .all(|event| event.terminal() != Some(TerminalState::Completed))
    );
}

#[test]
fn unselected_transmission_sources_must_fit_the_session_grant() {
    let mut fixture = Fixture::new(limits());
    let mut memory = std::mem::replace(&mut fixture.memory, MemoryService::new());
    let identity = fixture.request.identity().clone();
    let ungranted_source = memory
        .register_source(
            MemoryScope::Private(identity.clone()),
            "synthetic-unselected-source",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    memory
        .for_pet(&identity)
        .create(
            MemoryDraft::new(
                "synthetic-unselected-entry",
                Category::Episode,
                Epistemic::UserStatement,
                vec![ungranted_source.clone()],
            ),
            0,
        )
        .unwrap();
    memory
        .set_read_policy(
            &identity,
            ReadPolicy::offline(
                2,
                vec![
                    fixture.authority.clone(),
                    fixture.source.clone(),
                    ungranted_source.clone(),
                ],
            ),
        )
        .unwrap();
    const ENDPOINT: &str = "https://example.invalid/rerank";
    memory
        .set_transmission_policy(
            &identity,
            TransmissionPolicy::new(
                1,
                TransmissionUsage::Rerank,
                ENDPOINT,
                vec![fixture.source.clone(), ungranted_source.clone()],
            )
            .unwrap()
            .transmit_query()
            .with_scope(MemoryScope::Private(identity.clone())),
        )
        .unwrap();
    let mut hybrid = HybridAccess {
        memory,
        query_source: fixture.source.clone(),
        selected_source: fixture.source.clone(),
        endpoint: ENDPOINT,
    };
    let task = hybrid
        .begin_memory_task(
            &identity,
            fixture.binding.memory_session(),
            ReadPurpose::OfflineReasoning,
            &RetrievalQuery::new(""),
            0,
        )
        .unwrap();
    assert_eq!(task.result().items().len(), 1);
    assert!(task.result().items()[0].sources().contains(&fixture.source));
    assert!(
        !task.result().items()[0]
            .sources()
            .contains(&ungranted_source)
    );
    assert!(
        task.transmissions()
            .iter()
            .flat_map(|evidence| evidence.sent_sources())
            .any(|source| source == &ungranted_source)
    );
    let error = fixture
        .agent
        .prepare(
            &AgentAccess::new(&fixture.ingress, &fixture.request, &fixture.presets, 0),
            AgentServices::new(&mut fixture.sessions, &mut hybrid),
            &fixture.binding,
            &fixture.registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap_err();
    assert_eq!(error, AgentError::MemoryScopeDenied);
}
