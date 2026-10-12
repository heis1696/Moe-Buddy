use std::collections::BTreeMap;

use moe_agent_runtime::{
    AgentAccess, AgentBinding, AgentDelivery, AgentError, AgentLimits, AgentResult, AgentRuntime,
    AgentServices, AgentTask, CallerSubject, ChannelId, ChannelScope, EventRole, GatewayError,
    IngressToken, ManagementCapability, ManagementPermissions, ModelOutcome, OfflineInput,
    OfflineModel, PresentationEvent, PresetConfig, PresetId, PresetPurpose, PresetRegistry,
    RawIngressCall, Registration, ReplyTarget, SessionAccess, SessionLimits, SessionRuntime,
    SessionTiming, TerminalState, TrustedIngress, TrustedRequest, UserEvent, UserEventKind,
};
use moe_memory_core::{
    Category, Epistemic, MemoryDraft, MemoryScope, MemoryService, Namespace, PetIdentity,
    ReadPolicy, RetrievalQuery,
};

const MODEL_OUTPUT: &str = "synthetic-private-model-output";
const USER_INPUT: &str = "synthetic-private-user-input";

#[derive(Clone, Copy)]
enum CleanupPath {
    SoftDelete,
    Maintenance,
    Revoke,
    Purge,
    EmptyRecycle,
}

const CLEANUP_PATHS: [CleanupPath; 5] = [
    CleanupPath::SoftDelete,
    CleanupPath::Maintenance,
    CleanupPath::Revoke,
    CleanupPath::Purge,
    CleanupPath::EmptyRecycle,
];

fn identity(workspace: &str, user: &str, pet: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new(workspace, user).unwrap(), pet).unwrap()
}

fn retention() -> moe_agent_runtime::RawRetention {
    moe_agent_runtime::RawRetention::new(1_000, 2_000, 16, 256, 16_384).unwrap()
}

fn session_limits() -> SessionLimits {
    SessionLimits::new(16, 128, 512, 8_192, 65_536, 4).unwrap()
}

fn install(ingress: &mut TrustedIngress, token: &str, subject: &str, channel: &str, scope: &str) {
    ingress
        .install_credential(
            IngressToken::new(token).unwrap(),
            CallerSubject::new(subject).unwrap(),
            ["buddy".to_owned(), "other".to_owned()],
            ChannelId::new(channel).unwrap(),
            ChannelScope::new(scope).unwrap(),
            ReplyTarget::new("synthetic-reply-target").unwrap(),
        )
        .unwrap();
}

fn ordering_fixture() -> (TrustedIngress, SessionRuntime) {
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route("buddy", identity("workspace", "owner", "pet"))
        .unwrap();
    ingress
        .register_route("other", identity("workspace", "owner", "other-pet"))
        .unwrap();
    install(&mut ingress, "token-api", "caller", "api", "private");
    let runtime = SessionRuntime::with_retention(
        &ingress,
        session_limits(),
        SessionTiming::new(10, 180_000, 10, 10).unwrap(),
        retention(),
        0,
    );
    (ingress, runtime)
}

fn admit(
    ingress: &mut TrustedIngress,
    token: &str,
    alias: &str,
    body: &str,
    hint: Option<&str>,
) -> TrustedRequest {
    let call = RawIngressCall::new(IngressToken::new(token).unwrap(), alias, body);
    ingress
        .admit(match hint {
            Some(hint) => call.with_session_hint(hint),
            None => call,
        })
        .unwrap()
}

fn register_order(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    now_ms: u64,
    source_order: Option<u64>,
) -> Registration {
    let event = match source_order {
        Some(source_order) => UserEvent::new(UserEventKind::New).with_source_order(source_order),
        None => UserEvent::new(UserEventKind::New),
    };
    runtime
        .register_user(SessionAccess::new(ingress, request, now_ms), event)
        .unwrap()
}

fn full_permissions() -> ManagementPermissions {
    ManagementPermissions::default()
        .with_view()
        .with_soft_delete()
        .with_restore()
        .with_purge()
}

fn capability(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    fragments: Vec<moe_agent_runtime::SessionId>,
) -> ManagementCapability {
    runtime
        .install_management_capability(
            ingress,
            request.identity().clone(),
            request.isolation().clone(),
            fragments,
            full_permissions(),
        )
        .unwrap()
}

fn assert_history(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    registration: &Registration,
    now_ms: u64,
) {
    assert!(!registration.duplicate);
    assert!(!registration.renewed);
    let capability = capability(
        runtime,
        ingress,
        request,
        vec![registration.session.clone()],
    );
    let snapshot = runtime
        .inspect_fragment(
            SessionAccess::new(ingress, request, now_ms),
            &capability,
            &registration.session,
        )
        .unwrap();
    let event = snapshot.events.last().unwrap();
    assert_eq!(event.role(), EventRole::UserHistory);
    assert_eq!(event.terminal(), Some(TerminalState::Unknown));
}

fn cleanup_fragments(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    fragments: &[(&TrustedRequest, &Registration)],
    cleanup: CleanupPath,
    now_ms: u64,
) {
    match cleanup {
        CleanupPath::Maintenance => {
            let report = runtime.maintain_retention(ingress, now_ms).unwrap();
            assert_eq!(report.removed, fragments.len());
        }
        CleanupPath::Revoke => {
            for (offset, (request, registration)) in fragments.iter().rev().enumerate() {
                runtime
                    .revoke_session(
                        SessionAccess::new(ingress, request, now_ms + offset as u64),
                        &registration.session,
                    )
                    .unwrap();
            }
        }
        CleanupPath::SoftDelete | CleanupPath::Purge | CleanupPath::EmptyRecycle => {
            let ids = fragments
                .iter()
                .map(|(_, registration)| registration.session.clone())
                .collect();
            let capability = capability(runtime, ingress, fragments[0].0, ids);
            for (offset, (request, registration)) in fragments.iter().rev().enumerate() {
                let at = now_ms + offset as u64;
                match cleanup {
                    CleanupPath::SoftDelete | CleanupPath::EmptyRecycle => {
                        runtime
                            .soft_delete_fragment(
                                SessionAccess::new(ingress, request, at),
                                &capability,
                                &registration.session,
                                0,
                            )
                            .unwrap();
                    }
                    CleanupPath::Purge => {
                        runtime
                            .purge_fragment(
                                SessionAccess::new(ingress, request, at),
                                &capability,
                                &registration.session,
                                0,
                            )
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            if matches!(cleanup, CleanupPath::EmptyRecycle) {
                let report = runtime
                    .empty_recycle(
                        SessionAccess::new(
                            ingress,
                            fragments[0].0,
                            now_ms + fragments.len() as u64,
                        ),
                        &capability,
                    )
                    .unwrap();
                assert_eq!(report.purged, fragments.len());
            }
        }
    }
}

fn cleanup_time(cleanup: CleanupPath) -> u64 {
    match cleanup {
        CleanupPath::Maintenance => 4_000,
        _ => 100,
    }
}

#[test]
fn source_order_zero_max_equality_and_missing_host_ids_survive_cleanup() {
    for cleanup in CLEANUP_PATHS {
        let (mut ingress, mut runtime) = ordering_fixture();
        let zero = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "zero",
            Some("same-domain"),
        );
        let zero_registration = register_order(&mut runtime, &ingress, &zero, 0, Some(0));
        assert!(zero_registration.renewed);

        let equal_zero = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "equal-zero",
            Some("same-domain"),
        );
        let equal_zero_registration =
            register_order(&mut runtime, &ingress, &equal_zero, 1, Some(0));
        assert_history(
            &mut runtime,
            &ingress,
            &equal_zero,
            &equal_zero_registration,
            1,
        );

        let maximum = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "maximum",
            Some("same-domain"),
        );
        let maximum_registration =
            register_order(&mut runtime, &ingress, &maximum, 11, Some(u64::MAX));
        assert!(maximum_registration.renewed);

        let equal_maximum = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "equal-maximum",
            Some("same-domain"),
        );
        let equal_maximum_registration =
            register_order(&mut runtime, &ingress, &equal_maximum, 12, Some(u64::MAX));
        assert_history(
            &mut runtime,
            &ingress,
            &equal_maximum,
            &equal_maximum_registration,
            12,
        );

        let fragments = [
            (&zero, &zero_registration),
            (&maximum, &maximum_registration),
        ];
        let cleaned_at = cleanup_time(cleanup);
        cleanup_fragments(&mut runtime, &ingress, &fragments, cleanup, cleaned_at);

        for (offset, order) in [u64::MAX, 1].into_iter().enumerate() {
            let late = admit(
                &mut ingress,
                "token-api",
                "buddy",
                "late-order",
                Some("same-domain"),
            );
            let registration = register_order(
                &mut runtime,
                &ingress,
                &late,
                cleaned_at + 10 + offset as u64,
                Some(order),
            );
            assert_history(
                &mut runtime,
                &ingress,
                &late,
                &registration,
                cleaned_at + 10 + offset as u64,
            );
        }
    }
}

#[test]
fn cleanup_merges_max_watermarks_and_failed_transitions_are_atomic() {
    for cleanup in CLEANUP_PATHS {
        let (mut ingress, mut runtime) = ordering_fixture();
        let lower = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "lower",
            Some("merge-domain"),
        );
        let lower_registration = register_order(&mut runtime, &ingress, &lower, 0, Some(10));
        let higher = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "higher",
            Some("merge-domain"),
        );
        let higher_registration = register_order(&mut runtime, &ingress, &higher, 11, Some(20));
        assert!(higher_registration.renewed);
        let missing = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "missing-source-order",
            Some("merge-domain"),
        );
        let missing_registration = register_order(&mut runtime, &ingress, &missing, 22, None);
        assert!(missing_registration.renewed);

        let capability = capability(
            &mut runtime,
            &ingress,
            &lower,
            vec![lower_registration.session.clone()],
        );
        let before = runtime
            .inspect_fragment(
                SessionAccess::new(&ingress, &lower, 23),
                &capability,
                &lower_registration.session,
            )
            .unwrap();
        assert_eq!(
            runtime.soft_delete_fragment(
                SessionAccess::new(&ingress, &lower, 23),
                &capability,
                &lower_registration.session,
                u64::MAX,
            ),
            Err(GatewayError::LifecycleVersionConflict)
        );
        let after = runtime
            .inspect_fragment(
                SessionAccess::new(&ingress, &lower, 23),
                &capability,
                &lower_registration.session,
            )
            .unwrap();
        assert_eq!(after.zone, before.zone);
        assert_eq!(after.lifecycle_version, before.lifecycle_version);
        assert_eq!(after.normal_deadline_ms, before.normal_deadline_ms);
        assert_eq!(after.final_deadline_ms, before.final_deadline_ms);
        assert_eq!(after.processed_through, before.processed_through);
        assert_eq!(after.events.len(), before.events.len());

        let fragments = [
            (&lower, &lower_registration),
            (&higher, &higher_registration),
            (&missing, &missing_registration),
        ];
        let cleaned_at = cleanup_time(cleanup);
        cleanup_fragments(&mut runtime, &ingress, &fragments, cleanup, cleaned_at);

        for order in [15, 20] {
            let delayed = admit(
                &mut ingress,
                "token-api",
                "buddy",
                "delayed-order",
                Some("merge-domain"),
            );
            let probe_at = cleaned_at + 10;
            let registration =
                register_order(&mut runtime, &ingress, &delayed, probe_at, Some(order));
            assert_history(&mut runtime, &ingress, &delayed, &registration, probe_at);
        }
        let newest = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "newest-order",
            Some("merge-domain"),
        );
        assert!(register_order(&mut runtime, &ingress, &newest, cleaned_at + 11, Some(21)).renewed);
    }
}

#[test]
fn source_watermarks_do_not_cross_complete_isolation_domains() {
    for cleanup in CLEANUP_PATHS {
        let (mut ingress, mut runtime) = ordering_fixture();
        install(
            &mut ingress,
            "token-channel",
            "caller",
            "other-channel",
            "private",
        );
        install(&mut ingress, "token-scope", "caller", "api", "other-scope");
        install(
            &mut ingress,
            "token-subject",
            "other-caller",
            "api",
            "private",
        );
        install(&mut ingress, "token-credential", "caller", "api", "private");

        let original = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "original-domain",
            Some("same-hint"),
        );
        let original_registration =
            register_order(&mut runtime, &ingress, &original, 0, Some(u64::MAX));
        let cleaned_at = cleanup_time(cleanup);
        cleanup_fragments(
            &mut runtime,
            &ingress,
            &[(&original, &original_registration)],
            cleanup,
            cleaned_at,
        );

        let isolated = [
            admit(
                &mut ingress,
                "token-channel",
                "buddy",
                "isolated",
                Some("same-hint"),
            ),
            admit(
                &mut ingress,
                "token-scope",
                "buddy",
                "isolated",
                Some("same-hint"),
            ),
            admit(
                &mut ingress,
                "token-subject",
                "buddy",
                "isolated",
                Some("same-hint"),
            ),
            admit(
                &mut ingress,
                "token-credential",
                "buddy",
                "isolated",
                Some("same-hint"),
            ),
            admit(
                &mut ingress,
                "token-api",
                "other",
                "isolated",
                Some("same-hint"),
            ),
            admit(
                &mut ingress,
                "token-api",
                "buddy",
                "isolated",
                Some("different-hint"),
            ),
            admit(&mut ingress, "token-api", "buddy", "isolated", None),
        ];
        for (offset, request) in isolated.iter().enumerate() {
            assert!(
                register_order(
                    &mut runtime,
                    &ingress,
                    request,
                    cleaned_at + 10 + offset as u64,
                    Some(1),
                )
                .renewed
            );
        }

        let same_domain = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "same-domain-late",
            Some("same-hint"),
        );
        let probe_at = cleaned_at + 20;
        let registration = register_order(&mut runtime, &ingress, &same_domain, probe_at, Some(1));
        assert_history(
            &mut runtime,
            &ingress,
            &same_domain,
            &registration,
            probe_at,
        );
    }
}

fn preset_config() -> PresetConfig {
    PresetConfig::new(
        "synthetic",
        "offline-fixture",
        "https://example.invalid/v1",
        None,
        "synthetic-model",
        BTreeMap::new(),
    )
    .unwrap()
}

struct AgentFixture {
    ingress: TrustedIngress,
    presets: PresetRegistry,
    sessions: SessionRuntime,
    memory: MemoryService,
    agent: AgentRuntime,
    request: TrustedRequest,
    registration: Registration,
    binding: AgentBinding,
}

impl AgentFixture {
    fn new() -> Self {
        let pet = identity("agent-workspace", "agent-owner", "stable-pet");
        let mut ingress = TrustedIngress::new();
        ingress.register_route("buddy", pet.clone()).unwrap();
        ingress
            .register_route(
                "other",
                identity("agent-workspace", "agent-owner", "other-pet"),
            )
            .unwrap();
        install(
            &mut ingress,
            "agent-token",
            "agent-caller",
            "desktop",
            "private",
        );
        let request = ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new("agent-token").unwrap(),
                    "buddy",
                    USER_INPUT,
                )
                .with_session_hint("agent-hint"),
            )
            .unwrap();

        let mut presets = PresetRegistry::new(4).unwrap();
        let preset_id = PresetId::new("synthetic-chat").unwrap();
        presets.create(preset_id.clone(), preset_config()).unwrap();
        presets.bind(PresetPurpose::Chat, preset_id).unwrap();

        let mut sessions = SessionRuntime::with_retention(
            &ingress,
            session_limits(),
            SessionTiming::default(),
            retention(),
            0,
        );
        let registration = sessions
            .register_user(
                SessionAccess::new(&ingress, &request, 0),
                UserEvent::new(UserEventKind::New).with_source_order(10),
            )
            .unwrap();

        let mut memory = MemoryService::new();
        let source = memory
            .register_source(
                MemoryScope::Private(pet.clone()),
                "synthetic-user-source",
                1,
                Epistemic::UserStatement,
            )
            .unwrap();
        let authority = memory
            .register_source(
                MemoryScope::Private(pet.clone()),
                "synthetic-persona-source",
                1,
                Epistemic::AuthoritativeState,
            )
            .unwrap();
        memory
            .for_pet(&pet)
            .create(
                MemoryDraft::new(
                    "synthetic-authorized-fact",
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![source.clone()],
                ),
                0,
            )
            .unwrap();
        memory
            .set_identity_view(
                &pet,
                1,
                "synthetic-protected-persona",
                vec![authority.clone()],
            )
            .unwrap();
        memory
            .set_read_policy(
                &pet,
                ReadPolicy::offline(1, vec![source.clone(), authority.clone()]),
            )
            .unwrap();

        let agent_limits = AgentLimits::new(16, 64, 2_048, 4_096).unwrap();
        let mut agent = AgentRuntime::new(&ingress, agent_limits).unwrap();
        let binding = agent
            .bind_session(
                &AgentAccess::new(&ingress, &request, &presets, 0),
                &mut sessions,
                &registration.session,
                vec![source, authority],
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
        }
    }

    fn prepare(&mut self, now_ms: u64) -> AgentTask {
        self.agent
            .prepare(
                &AgentAccess::new(&self.ingress, &self.request, &self.presets, now_ms),
                AgentServices::new(&mut self.sessions, &mut self.memory),
                &self.binding,
                &self.registration.interaction,
                &RetrievalQuery::new(""),
            )
            .unwrap()
    }

    fn infer(
        &mut self,
        task: &mut AgentTask,
        model: &mut CountingModel,
        now_ms: u64,
    ) -> Result<AgentResult, AgentError> {
        self.agent.infer(
            &AgentAccess::new(&self.ingress, &self.request, &self.presets, now_ms),
            AgentServices::new(&mut self.sessions, &mut self.memory),
            task,
            model,
        )
    }

    fn finish(
        &mut self,
        task: &mut AgentTask,
        result: &AgentResult,
        now_ms: u64,
    ) -> Result<AgentDelivery, AgentError> {
        self.agent.finish(
            &AgentAccess::new(&self.ingress, &self.request, &self.presets, now_ms),
            AgentServices::new(&mut self.sessions, &mut self.memory),
            task,
            result,
        )
    }
}

#[derive(Default)]
struct CountingModel {
    calls: usize,
}

impl OfflineModel for CountingModel {
    fn infer(&mut self, _input: OfflineInput<'_>) -> ModelOutcome {
        self.calls += 1;
        ModelOutcome::Text {
            text: MODEL_OUTPUT.to_owned(),
            presentation: vec![PresentationEvent::Happy],
        }
    }
}

#[derive(Clone, Copy)]
enum AgentTransition {
    SoftDelete,
    Maintenance,
    Purge,
    Restore,
    Revoke,
}

const AGENT_TRANSITIONS: [AgentTransition; 5] = [
    AgentTransition::SoftDelete,
    AgentTransition::Maintenance,
    AgentTransition::Purge,
    AgentTransition::Restore,
    AgentTransition::Revoke,
];

fn apply_agent_transition(fixture: &mut AgentFixture, transition: AgentTransition) -> u64 {
    match transition {
        AgentTransition::Maintenance => {
            let report = fixture
                .sessions
                .maintain_retention(&fixture.ingress, 1_000)
                .unwrap();
            assert_eq!(report.transferred, 1);
            1_001
        }
        AgentTransition::Revoke => {
            fixture
                .sessions
                .revoke_session(
                    SessionAccess::new(&fixture.ingress, &fixture.request, 1),
                    &fixture.registration.session,
                )
                .unwrap();
            2
        }
        AgentTransition::SoftDelete | AgentTransition::Purge | AgentTransition::Restore => {
            let capability = capability(
                &mut fixture.sessions,
                &fixture.ingress,
                &fixture.request,
                vec![fixture.registration.session.clone()],
            );
            match transition {
                AgentTransition::SoftDelete => {
                    fixture
                        .sessions
                        .soft_delete_fragment(
                            SessionAccess::new(&fixture.ingress, &fixture.request, 1),
                            &capability,
                            &fixture.registration.session,
                            0,
                        )
                        .unwrap();
                    2
                }
                AgentTransition::Purge => {
                    fixture
                        .sessions
                        .purge_fragment(
                            SessionAccess::new(&fixture.ingress, &fixture.request, 1),
                            &capability,
                            &fixture.registration.session,
                            0,
                        )
                        .unwrap();
                    2
                }
                AgentTransition::Restore => {
                    let recycled = fixture
                        .sessions
                        .soft_delete_fragment(
                            SessionAccess::new(&fixture.ingress, &fixture.request, 1),
                            &capability,
                            &fixture.registration.session,
                            0,
                        )
                        .unwrap();
                    fixture
                        .sessions
                        .restore_fragment(
                            SessionAccess::new(&fixture.ingress, &fixture.request, 2),
                            &capability,
                            &fixture.registration.session,
                            recycled.lifecycle_version,
                        )
                        .unwrap();
                    3
                }
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn old_g2_tasks_and_results_stay_fenced_across_lifecycle_matrix() {
    for transition in AGENT_TRANSITIONS {
        for result_exists in [false, true] {
            let mut fixture = AgentFixture::new();
            let mut task = fixture.prepare(0);
            let result = if result_exists {
                let mut model = CountingModel::default();
                let result = fixture.infer(&mut task, &mut model, 0).unwrap();
                assert_eq!(model.calls, 1);
                Some(result)
            } else {
                None
            };

            let now_ms = apply_agent_transition(&mut fixture, transition);
            match result {
                Some(result) => {
                    let error = fixture.finish(&mut task, &result, now_ms).unwrap_err();
                    let diagnostic = format!("{error:?}");
                    assert!(!diagnostic.contains(USER_INPUT));
                    assert!(!diagnostic.contains(MODEL_OUTPUT));
                }
                None => {
                    let mut model = CountingModel::default();
                    assert!(fixture.infer(&mut task, &mut model, now_ms).is_err());
                    assert_eq!(model.calls, 0);
                }
            }
        }
    }
}
