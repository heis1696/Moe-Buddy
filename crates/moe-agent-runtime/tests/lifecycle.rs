//! G3 原文生命周期与本机回收区外部回归。
//!
//! 覆盖显式留存配置、正常/回收/清理转换、独立管理能力、查看/软删除/
//! 恢复/清空、容量背压与 G1/G2 全路径失效联动；全部使用合成夹具。

use std::collections::BTreeMap;

use moe_agent_runtime::*;
use moe_memory_core::{
    Category, Epistemic, MemoryDraft, MemoryScope, MemoryService, Namespace, PetIdentity,
    ReadPolicy, RetrievalQuery, SourceRef,
};

fn identity(workspace: &str, user: &str, pet: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new(workspace, user).unwrap(), pet).unwrap()
}

fn retention(normal_ms: u64, recycle_ms: u64) -> RawRetention {
    RawRetention::new(normal_ms, recycle_ms, 8, 128, 8_192).unwrap()
}

fn limits() -> SessionLimits {
    SessionLimits::new(8, 16, 64, 1_024, 8_192, 2).unwrap()
}

fn install(
    ingress: &mut TrustedIngress,
    token: &str,
    subject: &str,
    channel: &str,
    scope: &str,
) -> CredentialId {
    ingress
        .install_credential(
            IngressToken::new(token).unwrap(),
            CallerSubject::new(subject).unwrap(),
            ["buddy".to_owned(), "other".to_owned()],
            ChannelId::new(channel).unwrap(),
            ChannelScope::new(scope).unwrap(),
            ReplyTarget::new("original-target").unwrap(),
        )
        .unwrap()
}

fn fixture() -> (TrustedIngress, SessionRuntime) {
    fixture_with(retention(1_000, 2_000), limits(), SessionTiming::default())
}

fn fixture_with(
    retention: RawRetention,
    limits: SessionLimits,
    timing: SessionTiming,
) -> (TrustedIngress, SessionRuntime) {
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route("buddy", identity("ws", "owner", "pet"))
        .unwrap();
    ingress
        .register_route("other", identity("ws", "owner", "other-pet"))
        .unwrap();
    install(&mut ingress, "token-api", "caller", "api", "private");
    let runtime = SessionRuntime::with_retention(&ingress, limits, timing, retention, 0);
    (ingress, runtime)
}

fn admit(
    ingress: &mut TrustedIngress,
    token: &str,
    alias: &str,
    body: &str,
    hint: &str,
) -> TrustedRequest {
    ingress
        .admit(
            RawIngressCall::new(IngressToken::new(token).unwrap(), alias, body)
                .with_session_hint(hint),
        )
        .unwrap()
}

fn access<'a>(
    ingress: &'a TrustedIngress,
    request: &'a TrustedRequest,
    now: u64,
) -> SessionAccess<'a> {
    SessionAccess::new(ingress, request, now)
}

fn full_permissions() -> ManagementPermissions {
    ManagementPermissions::default()
        .with_view()
        .with_soft_delete()
        .with_restore()
        .with_purge()
}

fn install_capability(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    fragments: Vec<SessionId>,
    permissions: ManagementPermissions,
) -> ManagementCapability {
    runtime
        .install_management_capability(
            ingress,
            request.identity().clone(),
            request.isolation().clone(),
            fragments,
            permissions,
        )
        .unwrap()
}

fn register(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    now: u64,
) -> Registration {
    runtime
        .register_user(
            access(ingress, request, now),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap()
}

struct CountingModel {
    calls: usize,
}

impl OfflineModel for CountingModel {
    fn infer(&mut self, _input: OfflineInput<'_>) -> ModelOutcome {
        self.calls += 1;
        ModelOutcome::Text {
            text: "合成回复".to_owned(),
            presentation: Vec::new(),
        }
    }
}

type AgentFixture = (
    TrustedIngress,
    SessionRuntime,
    MemoryService,
    AgentRuntime,
    PresetRegistry,
    Vec<SourceRef>,
);

fn g2_fixture(retention: RawRetention) -> AgentFixture {
    let mut ingress = TrustedIngress::new();
    let pet = identity("ws", "owner", "pet");
    ingress.register_route("buddy", pet.clone()).unwrap();
    ingress
        .install_credential(
            IngressToken::new("token-api").unwrap(),
            CallerSubject::new("caller").unwrap(),
            ["buddy".to_owned()],
            ChannelId::new("api").unwrap(),
            ChannelScope::new("private").unwrap(),
            ReplyTarget::new("original-target").unwrap(),
        )
        .unwrap();
    let sessions =
        SessionRuntime::with_retention(&ingress, limits(), SessionTiming::default(), retention, 0);
    let mut memory = MemoryService::new();
    let authority = memory
        .register_source(
            MemoryScope::Private(pet.clone()),
            "persona",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    let source = memory
        .register_source(
            MemoryScope::Private(pet.clone()),
            "fact",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    memory
        .set_identity_view(&pet, 1, "合成人设", vec![authority.clone()])
        .unwrap();
    memory
        .for_pet(&pet)
        .create(
            MemoryDraft::new(
                "合成获准事实",
                Category::Episode,
                Epistemic::UserStatement,
                vec![source.clone()],
            ),
            0,
        )
        .unwrap();
    memory
        .set_read_policy(
            &pet,
            ReadPolicy::offline(1, vec![authority.clone(), source.clone()]),
        )
        .unwrap();
    let grant = vec![authority, source];
    let mut presets = PresetRegistry::new(4).unwrap();
    let preset_id = PresetId::new("fixture-chat").unwrap();
    presets
        .create(
            preset_id.clone(),
            PresetConfig::new(
                "fixture",
                "offline-fixture",
                "https://example.invalid/v1",
                None,
                "fixture-model",
                BTreeMap::new(),
            )
            .unwrap(),
        )
        .unwrap();
    presets.bind(PresetPurpose::Chat, preset_id).unwrap();
    let agent =
        AgentRuntime::new(&ingress, AgentLimits::new(8, 64, 4_096, 4_096).unwrap()).unwrap();
    (ingress, sessions, memory, agent, presets, grant)
}

#[test]
fn retention_configuration_requires_explicit_positive_values() {
    let expected =
        GatewayError::InvalidInput("retention durations and recycle capacities must be positive");
    assert_eq!(RawRetention::new(0, 1, 1, 1, 1).unwrap_err(), expected);
    assert_eq!(RawRetention::new(1, 0, 1, 1, 1).unwrap_err(), expected);
    assert_eq!(RawRetention::new(1, 1, 0, 1, 1).unwrap_err(), expected);
    assert_eq!(RawRetention::new(1, 1, 1, 0, 1).unwrap_err(), expected);
    assert_eq!(RawRetention::new(1, 1, 1, 1, 0).unwrap_err(), expected);
}

#[test]
fn without_retention_lifecycle_apis_reject_and_g1_behaviour_is_unchanged() {
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route("buddy", identity("ws", "owner", "pet"))
        .unwrap();
    ingress
        .install_credential(
            IngressToken::new("token-api").unwrap(),
            CallerSubject::new("caller").unwrap(),
            ["buddy".to_owned()],
            ChannelId::new("api").unwrap(),
            ChannelScope::new("private").unwrap(),
            ReplyTarget::new("original-target").unwrap(),
        )
        .unwrap();
    let mut runtime = SessionRuntime::new(&ingress, limits(), SessionTiming::default(), 0);
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    assert!(runtime.retention().is_none());
    assert_eq!(
        runtime.maintain_retention(&ingress, 10).unwrap_err(),
        GatewayError::RetentionNotConfigured
    );
    assert_eq!(
        runtime
            .install_management_capability(
                &ingress,
                request.identity().clone(),
                request.isolation().clone(),
                vec![registration.session.clone()],
                full_permissions(),
            )
            .unwrap_err(),
        GatewayError::RetentionNotConfigured
    );
    assert!(
        runtime
            .read_active(access(&ingress, &request, 1_799_999), &registration.session)
            .is_ok()
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 1_800_000), &registration.session)
            .unwrap_err(),
        GatewayError::SessionExpired
    );
}

#[test]
fn normal_deadline_boundary_is_inclusive_across_all_usage_paths() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let summary = &runtime
        .list_managed_fragments(access(&ingress, &request, 0), &capability)
        .unwrap()[0];
    assert_eq!(summary.created_at_ms, 0);
    assert_eq!(summary.normal_deadline_ms, 1_000);
    assert_eq!(summary.final_deadline_ms, None);
    assert!(!summary.logically_expired);

    assert!(
        runtime
            .read_active(access(&ingress, &request, 999), &registration.session)
            .is_ok()
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 1_000), &registration.session)
            .unwrap_err(),
        GatewayError::RawExpired
    );
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &request, 1_000),
                &registration.interaction,
                TerminalState::Completed,
                None,
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
    assert_eq!(
        runtime
            .issue_idle_batch(
                access(&ingress, &request, 1_000),
                &registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
    let later = admit(&mut ingress, "token-api", "buddy", "新原文", "hint");
    let second = register(&mut runtime, &ingress, &later, 1_000);
    assert_ne!(second.session, registration.session);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &later, 1_000), &registration.session)
            .unwrap_err(),
        GatewayError::RawExpired
    );
    assert!(
        runtime
            .read_active(access(&ingress, &later, 1_000), &second.session)
            .is_ok()
    );
}

#[test]
fn active_ttl_expiry_does_not_trigger_raw_recycle_before_deadline() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(10_000, 2_000),
        limits(),
        SessionTiming::new(1_000, 180_000, 100_000, 300_000).unwrap(),
    );
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 2_000), &registration.session)
            .unwrap_err(),
        GatewayError::SessionExpired
    );
    let related = runtime
        .append_related(
            access(&ingress, &request, 2_000),
            &registration.interaction,
            RelatedEvent::new(EventRole::ToolReceipt, "工具回执"),
        )
        .unwrap();
    assert_eq!(related.ordinal(), 2);
    runtime
        .finish_interaction(
            access(&ingress, &request, 3_000),
            &registration.interaction,
            TerminalState::Completed,
            None,
        )
        .unwrap();
    assert_eq!(
        runtime
            .append_related(
                access(&ingress, &request, 10_000),
                &registration.interaction,
                RelatedEvent::new(EventRole::ToolReceipt, "迟到回执"),
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
    assert_eq!(
        runtime
            .finish_interaction(
                access(&ingress, &request, 10_000),
                &registration.interaction,
                TerminalState::Failed,
                None,
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
}

#[test]
fn soft_delete_fixes_final_deadline_from_deletion_time() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let deleted = runtime
        .soft_delete_fragment(
            access(&ingress, &request, 200),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    assert_eq!(deleted.zone, RetentionZone::Recycle);
    assert_eq!(deleted.lifecycle_version, 1);
    assert_eq!(deleted.final_deadline_ms, Some(2_200));
    let restored = runtime
        .restore_fragment(
            access(&ingress, &request, 300),
            &capability,
            &registration.session,
            1,
        )
        .unwrap();
    assert_eq!(restored.zone, RetentionZone::Normal);
    assert_eq!(restored.lifecycle_version, 2);
    assert_eq!(restored.normal_deadline_ms, 1_300);
    let deleted_again = runtime
        .soft_delete_fragment(
            access(&ingress, &request, 400),
            &capability,
            &registration.session,
            2,
        )
        .unwrap();
    assert_eq!(deleted_again.final_deadline_ms, Some(2_200));
    let restored_late = runtime
        .restore_fragment(
            access(&ingress, &request, 1_500),
            &capability,
            &registration.session,
            3,
        )
        .unwrap();
    assert_eq!(restored_late.normal_deadline_ms, 2_200);
    let deleted_last = runtime
        .soft_delete_fragment(
            access(&ingress, &request, 1_600),
            &capability,
            &registration.session,
            4,
        )
        .unwrap();
    assert_eq!(deleted_last.final_deadline_ms, Some(2_200));
    assert_eq!(
        runtime
            .restore_fragment(
                access(&ingress, &request, 2_200),
                &capability,
                &registration.session,
                5
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
}

#[test]
fn natural_expiry_uses_original_normal_deadline_not_maintenance_time() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 1_000), &registration.session)
            .unwrap_err(),
        GatewayError::RawExpired
    );
    let report = runtime.maintain_retention(&ingress, 2_900).unwrap();
    assert_eq!(report.transferred, 1);
    assert_eq!(report.pending_transfer, 0);
    assert_eq!(report.removed, 0);
    let snapshot = runtime
        .inspect_fragment(
            access(&ingress, &request, 2_900),
            &capability,
            &registration.session,
        )
        .unwrap();
    assert_eq!(snapshot.zone, RetentionZone::Recycle);
    assert_eq!(snapshot.final_deadline_ms, Some(3_000));
    assert_eq!(snapshot.lifecycle_version, 1);
}

#[test]
fn repeated_delete_and_restore_never_extends_final_deadline() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let mut version = 0u64;
    for cycle in 0..3u64 {
        let deleted = runtime
            .soft_delete_fragment(
                access(&ingress, &request, 100 + cycle * 100),
                &capability,
                &registration.session,
                version,
            )
            .unwrap();
        assert_eq!(deleted.final_deadline_ms, Some(2_100));
        version = deleted.lifecycle_version;
        let restored = runtime
            .restore_fragment(
                access(&ingress, &request, 150 + cycle * 100),
                &capability,
                &registration.session,
                version,
            )
            .unwrap();
        assert_eq!(
            restored.normal_deadline_ms,
            (150 + cycle * 100 + 1_000).min(2_100)
        );
        version = restored.lifecycle_version;
    }
    assert_eq!(version, 6);
}

#[test]
fn backwards_time_is_rejected_on_lifecycle_and_management_paths() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    assert!(runtime.maintain_retention(&ingress, 100).is_ok());
    assert_eq!(
        runtime.maintain_retention(&ingress, 50).unwrap_err(),
        GatewayError::ClockWentBackwards
    );
    runtime
        .soft_delete_fragment(
            access(&ingress, &request, 200),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    assert_eq!(
        runtime
            .inspect_fragment(
                access(&ingress, &request, 150),
                &capability,
                &registration.session
            )
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );
    assert_eq!(
        runtime
            .restore_fragment(
                access(&ingress, &request, 150),
                &capability,
                &registration.session,
                1
            )
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );
}

#[test]
fn management_permissions_are_adjudicated_separately() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let view_only = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        ManagementPermissions::default().with_view(),
    );
    assert!(
        runtime
            .inspect_fragment(
                access(&ingress, &request, 1),
                &view_only,
                &registration.session
            )
            .is_ok()
    );
    assert!(
        runtime
            .list_managed_fragments(access(&ingress, &request, 1), &view_only)
            .is_ok()
    );
    assert_eq!(
        runtime
            .soft_delete_fragment(
                access(&ingress, &request, 1),
                &view_only,
                &registration.session,
                0
            )
            .unwrap_err(),
        GatewayError::Unauthorized
    );
    assert_eq!(
        runtime
            .restore_fragment(
                access(&ingress, &request, 1),
                &view_only,
                &registration.session,
                0
            )
            .unwrap_err(),
        GatewayError::Unauthorized
    );
    assert_eq!(
        runtime
            .purge_fragment(
                access(&ingress, &request, 1),
                &view_only,
                &registration.session,
                0
            )
            .unwrap_err(),
        GatewayError::Unauthorized
    );
    assert_eq!(
        runtime
            .empty_recycle(access(&ingress, &request, 1), &view_only)
            .unwrap_err(),
        GatewayError::Unauthorized
    );
}

#[test]
fn revoked_capability_is_rejected_immediately_and_revocation_is_idempotent() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    assert!(runtime.revoke_management_capability(&capability).is_ok());
    assert!(runtime.revoke_management_capability(&capability).is_ok());
    assert_eq!(
        runtime
            .inspect_fragment(
                access(&ingress, &request, 1),
                &capability,
                &registration.session
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime
            .soft_delete_fragment(
                access(&ingress, &request, 1),
                &capability,
                &registration.session,
                0
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    let fresh = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    assert!(
        runtime
            .soft_delete_fragment(
                access(&ingress, &request, 2),
                &fresh,
                &registration.session,
                0
            )
            .is_ok()
    );
}

#[test]
fn capability_is_rejected_across_coordinators_and_ingress_runs() {
    let (mut ingress_a, mut runtime_a) = fixture();
    let request_a = admit(&mut ingress_a, "token-api", "buddy", "原文A", "hint");
    let registration_a = register(&mut runtime_a, &ingress_a, &request_a, 0);
    let capability_a = install_capability(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        vec![registration_a.session.clone()],
        full_permissions(),
    );
    let request_b = admit(&mut ingress_a, "token-api", "buddy", "原文B", "hint");
    let mut runtime_b = SessionRuntime::with_retention(
        &ingress_a,
        limits(),
        SessionTiming::default(),
        retention(1_000, 2_000),
        0,
    );
    assert_eq!(
        runtime_b
            .inspect_fragment(
                access(&ingress_a, &request_b, 0),
                &capability_a,
                &registration_a.session
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    let mut ingress_c = TrustedIngress::new();
    ingress_c
        .register_route("buddy", identity("ws", "owner", "pet"))
        .unwrap();
    ingress_c
        .install_credential(
            IngressToken::new("token-api").unwrap(),
            CallerSubject::new("caller").unwrap(),
            ["buddy".to_owned()],
            ChannelId::new("api").unwrap(),
            ChannelScope::new("private").unwrap(),
            ReplyTarget::new("original-target").unwrap(),
        )
        .unwrap();
    let request_c = admit(&mut ingress_c, "token-api", "buddy", "原文C", "hint");
    assert_eq!(
        runtime_a
            .inspect_fragment(
                access(&ingress_c, &request_c, 0),
                &capability_a,
                &registration_a.session
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
}

#[test]
fn capability_is_rejected_across_identity_channel_scope_credential_and_scope() {
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route("buddy", identity("ws", "owner", "pet"))
        .unwrap();
    ingress
        .register_route("other", identity("ws", "owner", "other-pet"))
        .unwrap();
    install(&mut ingress, "token-api", "caller", "api", "private");
    install(
        &mut ingress,
        "token-desktop",
        "caller",
        "desktop",
        "private",
    );
    install(&mut ingress, "token-group", "caller", "api", "group");
    install(
        &mut ingress,
        "token-subject",
        "other-subject",
        "api",
        "private",
    );
    let mut runtime = SessionRuntime::with_retention(
        &ingress,
        limits(),
        SessionTiming::default(),
        retention(1_000, 2_000),
        0,
    );
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    for token in ["token-desktop", "token-group", "token-subject"] {
        let foreign = admit(&mut ingress, token, "buddy", "原文", "hint");
        assert_eq!(
            runtime
                .inspect_fragment(
                    access(&ingress, &foreign, 1),
                    &capability,
                    &registration.session
                )
                .unwrap_err(),
            GatewayError::ForeignRequest,
            "isolation dimension mismatch for {token}"
        );
    }
    let cross_pet = admit(&mut ingress, "token-api", "other", "原文", "hint");
    assert_eq!(
        runtime
            .inspect_fragment(
                access(&ingress, &cross_pet, 1),
                &capability,
                &registration.session
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    let later = admit(&mut ingress, "token-api", "buddy", "第二片段", "hint");
    let second = register(&mut runtime, &ingress, &later, 1_000);
    assert_ne!(second.session, registration.session);
    assert_eq!(
        runtime
            .inspect_fragment(
                access(&ingress, &later, 1_001),
                &capability,
                &second.session
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
}

#[test]
fn capability_expires_on_configuration_generation_change() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    assert!(
        runtime
            .inspect_fragment(
                access(&ingress, &request, 1),
                &capability,
                &registration.session
            )
            .is_ok()
    );
    ingress
        .register_route("extra", identity("ws", "owner", "extra-pet"))
        .unwrap();
    let renewed = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    assert_eq!(
        runtime
            .inspect_fragment(
                access(&ingress, &renewed, 2),
                &capability,
                &registration.session
            )
            .unwrap_err(),
        GatewayError::VersionConflict
    );
}

#[test]
fn capability_install_validates_scope_and_ledger_is_bounded() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    assert_eq!(
        runtime
            .install_management_capability(
                &ingress,
                request.identity().clone(),
                request.isolation().clone(),
                Vec::new(),
                full_permissions(),
            )
            .unwrap_err(),
        GatewayError::InvalidInput("management capability must target an exact fragment scope")
    );
    assert_eq!(
        runtime
            .install_management_capability(
                &ingress,
                identity("ws", "owner", "other-pet"),
                request.isolation().clone(),
                vec![registration.session.clone()],
                full_permissions(),
            )
            .unwrap_err(),
        GatewayError::InvalidInput("capability identity must match the isolation domain")
    );
    let cleaning = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    runtime
        .purge_fragment(
            access(&ingress, &request, 1),
            &cleaning,
            &registration.session,
            0,
        )
        .unwrap();
    assert_eq!(
        runtime
            .install_management_capability(
                &ingress,
                request.identity().clone(),
                request.isolation().clone(),
                vec![registration.session.clone()],
                full_permissions(),
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    let later = admit(&mut ingress, "token-api", "buddy", "新原文", "hint");
    let second = register(&mut runtime, &ingress, &later, 2);
    for _ in 0..63 {
        runtime
            .install_management_capability(
                &ingress,
                later.identity().clone(),
                later.isolation().clone(),
                vec![second.session.clone()],
                full_permissions(),
            )
            .unwrap();
    }
    assert_eq!(
        runtime
            .install_management_capability(
                &ingress,
                later.identity().clone(),
                later.isolation().clone(),
                vec![second.session.clone()],
                full_permissions(),
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
}

#[test]
fn management_debug_output_never_leaks_bodies_tokens_or_credentials() {
    let (mut ingress, mut runtime) = fixture();
    let body = "机密正文-不应出现在调试输出";
    let request = admit(&mut ingress, "token-api", "buddy", body, "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    runtime
        .soft_delete_fragment(
            access(&ingress, &request, 1),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    let snapshot = runtime
        .inspect_fragment(
            access(&ingress, &request, 2),
            &capability,
            &registration.session,
        )
        .unwrap();
    let summaries = runtime
        .list_managed_fragments(access(&ingress, &request, 2), &capability)
        .unwrap();
    let report = format!(
        "{:?}{:?}{:?}{:?}{:?}{:?}",
        snapshot,
        summaries,
        capability,
        GatewayError::RawExpired,
        MaintenanceReport::default(),
        runtime
    );
    assert!(!report.contains(body));
    assert!(!report.contains("token-api"));
    assert_eq!(format!("{capability:?}"), "ManagementCapability(REDACTED)");
    assert_eq!(snapshot.events[0].body(), body);
    assert_eq!(snapshot.events[0].role(), EventRole::User);
    assert_eq!(snapshot.zone, RetentionZone::Recycle);
}

#[test]
fn full_recycle_zone_keeps_expired_fragment_pending_with_explicit_backpressure() {
    let (mut ingress, mut runtime) = fixture_with(
        RawRetention::new(1_000, 2_000, 1, 128, 8_192).unwrap(),
        limits(),
        SessionTiming::default(),
    );
    let request = admit(&mut ingress, "token-api", "buddy", "第一片段", "hint");
    let first = register(&mut runtime, &ingress, &request, 0);
    let later = admit(&mut ingress, "token-api", "buddy", "第二片段", "hint");
    let second = register(&mut runtime, &ingress, &later, 1_100);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![first.session.clone(), second.session.clone()],
        full_permissions(),
    );
    let report = runtime.maintain_retention(&ingress, 2_200).unwrap();
    assert_eq!(report.transferred, 1);
    assert_eq!(report.pending_transfer, 1);
    assert_eq!(report.removed, 0);
    let pending = runtime
        .inspect_fragment(
            access(&ingress, &request, 2_200),
            &capability,
            &first.session,
        )
        .unwrap();
    assert_eq!(pending.zone, RetentionZone::Normal);
    assert!(pending.logically_expired);
    assert_eq!(
        runtime
            .issue_idle_batch(access(&ingress, &request, 2_200), &first.session, true)
            .unwrap_err(),
        GatewayError::RawExpired
    );
    runtime
        .purge_fragment(
            access(&ingress, &request, 2_201),
            &capability,
            &second.session,
            1,
        )
        .unwrap();
    let report = runtime.maintain_retention(&ingress, 2_202).unwrap();
    assert_eq!(report.transferred, 1);
    assert_eq!(report.pending_transfer, 0);
    let transferred = runtime
        .inspect_fragment(
            access(&ingress, &request, 2_202),
            &capability,
            &first.session,
        )
        .unwrap();
    assert_eq!(transferred.zone, RetentionZone::Recycle);
}

#[test]
fn recycle_capacity_failure_rejects_soft_delete_atomically() {
    let (mut ingress, mut runtime) = fixture_with(
        RawRetention::new(1_000, 2_000, 8, 128, 9).unwrap(),
        limits(),
        SessionTiming::default(),
    );
    let request = admit(&mut ingress, "token-api", "buddy", "hello", "hint");
    let first = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![first.session.clone()],
        full_permissions(),
    );
    runtime
        .soft_delete_fragment(
            access(&ingress, &request, 10),
            &capability,
            &first.session,
            0,
        )
        .unwrap();
    let later = admit(&mut ingress, "token-api", "buddy", "worlds", "hint");
    let second = register(&mut runtime, &ingress, &later, 1_000);
    assert_ne!(second.session, first.session);
    let both = install_capability(
        &mut runtime,
        &ingress,
        &later,
        vec![first.session.clone(), second.session.clone()],
        full_permissions(),
    );
    let before = runtime
        .inspect_fragment(access(&ingress, &later, 1_000), &both, &second.session)
        .unwrap();
    assert_eq!(
        runtime
            .soft_delete_fragment(access(&ingress, &later, 1_001), &both, &second.session, 0)
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    let after = runtime
        .inspect_fragment(access(&ingress, &later, 1_001), &both, &second.session)
        .unwrap();
    assert_eq!(after.zone, RetentionZone::Normal);
    assert_eq!(after.lifecycle_version, before.lifecycle_version);
    assert_eq!(after.normal_deadline_ms, before.normal_deadline_ms);
    assert_eq!(after.final_deadline_ms, None);
    assert_eq!(after.processed_through, before.processed_through);
    assert_eq!(after.events.len(), before.events.len());
}

#[test]
fn restore_capacity_failure_is_atomic_and_recycle_content_is_unchanged() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(1_000, 2_000),
        SessionLimits::new(2, 16, 64, 1_024, 8_192, 2).unwrap(),
        SessionTiming::default(),
    );
    let request_a = admit(&mut ingress, "token-api", "buddy", "片段A", "hint-a");
    let request_b = admit(&mut ingress, "token-api", "buddy", "片段B", "hint-b");
    let request_c = admit(&mut ingress, "token-api", "buddy", "片段C", "hint-c");
    let a = register(&mut runtime, &ingress, &request_a, 0);
    let b = register(&mut runtime, &ingress, &request_b, 0);
    let capability_a = install_capability(
        &mut runtime,
        &ingress,
        &request_a,
        vec![a.session.clone()],
        full_permissions(),
    );
    let capability_b = install_capability(
        &mut runtime,
        &ingress,
        &request_b,
        vec![b.session.clone()],
        full_permissions(),
    );
    runtime
        .soft_delete_fragment(
            access(&ingress, &request_a, 2),
            &capability_a,
            &a.session,
            0,
        )
        .unwrap();
    register(&mut runtime, &ingress, &request_c, 3);
    let before = runtime
        .inspect_fragment(access(&ingress, &request_a, 4), &capability_a, &a.session)
        .unwrap();
    assert_eq!(
        runtime
            .restore_fragment(
                access(&ingress, &request_a, 4),
                &capability_a,
                &a.session,
                1
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    let after = runtime
        .inspect_fragment(access(&ingress, &request_a, 4), &capability_a, &a.session)
        .unwrap();
    assert_eq!(after.zone, RetentionZone::Recycle);
    assert_eq!(after.lifecycle_version, before.lifecycle_version);
    assert_eq!(after.normal_deadline_ms, before.normal_deadline_ms);
    assert_eq!(after.final_deadline_ms, before.final_deadline_ms);
    assert_eq!(after.processed_through, before.processed_through);
    runtime
        .purge_fragment(
            access(&ingress, &request_b, 5),
            &capability_b,
            &b.session,
            0,
        )
        .unwrap();
    let restored = runtime
        .restore_fragment(
            access(&ingress, &request_a, 6),
            &capability_a,
            &a.session,
            1,
        )
        .unwrap();
    assert_eq!(restored.zone, RetentionZone::Normal);
}

#[test]
fn cleanup_releases_capacity_and_identifiers_are_never_reused() {
    let (mut ingress, mut runtime) = fixture_with(
        RawRetention::new(10_000_000, 10_000_000, 1, 128, 8_192).unwrap(),
        limits(),
        SessionTiming::default(),
    );
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    runtime
        .finish_interaction(
            access(&ingress, &request, 1),
            &registration.interaction,
            TerminalState::Completed,
            Some("合成回复"),
        )
        .unwrap();
    let ticket = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    runtime
        .soft_delete_fragment(
            access(&ingress, &request, 180_001),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    runtime
        .purge_fragment(
            access(&ingress, &request, 180_002),
            &capability,
            &registration.session,
            1,
        )
        .unwrap();
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 180_002), &registration.session)
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    assert_eq!(
        runtime
            .inspect_fragment(
                access(&ingress, &request, 180_002),
                &capability,
                &registration.session
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    assert_eq!(
        runtime
            .purge_fragment(
                access(&ingress, &request, 180_002),
                &capability,
                &registration.session,
                1
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &request, 180_002), &ticket)
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    let later = admit(&mut ingress, "token-api", "buddy", "新原文", "hint");
    let second = register(&mut runtime, &ingress, &later, 180_003);
    assert_ne!(second.session, registration.session);
    let second_capability = install_capability(
        &mut runtime,
        &ingress,
        &later,
        vec![second.session.clone()],
        full_permissions(),
    );
    assert!(
        runtime
            .soft_delete_fragment(
                access(&ingress, &later, 180_004),
                &second_capability,
                &second.session,
                0
            )
            .is_ok()
    );
    let report = runtime
        .empty_recycle(access(&ingress, &later, 180_005), &second_capability)
        .unwrap();
    assert_eq!(report.purged, 1);
    assert_eq!(report.released_events, 1);
    let repeat = runtime
        .empty_recycle(access(&ingress, &later, 180_006), &second_capability)
        .unwrap();
    assert_eq!(repeat.purged, 0);
}

#[test]
fn maintenance_removes_final_expired_content_inclusively_and_frees_recycle() {
    let (mut ingress, mut runtime) = fixture_with(
        RawRetention::new(1_000, 1_200, 1, 128, 8_192).unwrap(),
        limits(),
        SessionTiming::default(),
    );
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let deleted = runtime
        .soft_delete_fragment(
            access(&ingress, &request, 200),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    assert_eq!(deleted.final_deadline_ms, Some(1_400));
    assert_eq!(
        runtime
            .issue_idle_batch(
                access(&ingress, &request, 1_399),
                &registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::Recycled
    );
    let report = runtime.maintain_retention(&ingress, 1_400).unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(
        runtime
            .inspect_fragment(
                access(&ingress, &request, 1_400),
                &capability,
                &registration.session
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    let later = admit(&mut ingress, "token-api", "buddy", "新原文", "hint");
    let second = register(&mut runtime, &ingress, &later, 1_401);
    let second_capability = install_capability(
        &mut runtime,
        &ingress,
        &later,
        vec![second.session.clone()],
        full_permissions(),
    );
    assert!(
        runtime
            .soft_delete_fragment(
                access(&ingress, &later, 1_402),
                &second_capability,
                &second.session,
                0
            )
            .is_ok()
    );
}

#[test]
fn revocation_with_retention_purges_raw_text_and_blocks_replay() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = runtime
        .register_user(
            access(&ingress, &request, 0),
            UserEvent::new(UserEventKind::New)
                .with_id(HostEventId::new("synthetic-host-id").unwrap()),
        )
        .unwrap();
    runtime
        .revoke_session(access(&ingress, &request, 1), &registration.session)
        .unwrap();
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 1), &registration.session)
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    let replay = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    assert_eq!(
        runtime.register_user(
            access(&ingress, &replay, 2),
            UserEvent::new(UserEventKind::New)
                .with_id(HostEventId::new("synthetic-host-id").unwrap()),
        ),
        Err(GatewayError::Revoked)
    );
    let fresh = admit(&mut ingress, "token-api", "buddy", "新原文", "hint");
    let next = register(&mut runtime, &ingress, &fresh, 3);
    assert_ne!(next.session, registration.session);
    assert!(
        runtime
            .read_active(access(&ingress, &fresh, 3), &next.session)
            .is_ok()
    );
}

#[test]
fn maintenance_is_repeatable_with_mixed_fragments() {
    let (mut ingress, mut runtime) = fixture();
    let request_a = admit(&mut ingress, "token-api", "buddy", "片段A", "hint-a");
    let request_b = admit(&mut ingress, "token-api", "buddy", "片段B", "hint-b");
    let a = register(&mut runtime, &ingress, &request_a, 0);
    let b = register(&mut runtime, &ingress, &request_b, 0);
    let capability_a = install_capability(
        &mut runtime,
        &ingress,
        &request_a,
        vec![a.session.clone()],
        full_permissions(),
    );
    runtime
        .soft_delete_fragment(
            access(&ingress, &request_a, 10),
            &capability_a,
            &a.session,
            0,
        )
        .unwrap();
    let report = runtime.maintain_retention(&ingress, 500).unwrap();
    assert_eq!(
        report.transferred + report.removed + report.pending_transfer,
        0
    );
    let report = runtime.maintain_retention(&ingress, 1_000).unwrap();
    assert_eq!(report.transferred, 1);
    assert_eq!(report.removed, 0);
    let report = runtime.maintain_retention(&ingress, 2_100).unwrap();
    assert_eq!(report.removed, 1);
    let report = runtime.maintain_retention(&ingress, 2_101).unwrap();
    assert_eq!(
        report.transferred + report.removed + report.pending_transfer,
        0
    );
    let _ = b;
}

#[test]
fn natural_expiry_immediately_invalidates_in_flight_batch_tickets() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(200_000, 200_000),
        limits(),
        SessionTiming::default(),
    );
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    runtime
        .finish_interaction(
            access(&ingress, &request, 1),
            &registration.interaction,
            TerminalState::Completed,
            None,
        )
        .unwrap();
    let ticket = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    let events = runtime
        .read_batch(access(&ingress, &request, 199_999), &ticket)
        .unwrap();
    let first = events[0].id().clone();
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &request, 200_000), &ticket)
            .unwrap_err(),
        GatewayError::RawExpired
    );
    assert_eq!(
        runtime
            .acknowledge_batch(
                access(&ingress, &request, 200_000),
                &ticket,
                &first,
                BatchAcknowledgement::Confirmed,
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
}

#[test]
fn soft_delete_invalidates_tickets_and_releases_batch_concurrency() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(1_000_000, 1_000_000),
        SessionLimits::new(8, 16, 64, 1_024, 8_192, 1).unwrap(),
        SessionTiming::default(),
    );
    let request_a = admit(&mut ingress, "token-api", "buddy", "片段A", "hint-a");
    let request_b = admit(&mut ingress, "token-api", "buddy", "片段B", "hint-b");
    let a = register(&mut runtime, &ingress, &request_a, 0);
    let b = register(&mut runtime, &ingress, &request_b, 0);
    for (request, registration) in [(&request_a, &a), (&request_b, &b)] {
        runtime
            .finish_interaction(
                access(&ingress, request, 1),
                &registration.interaction,
                TerminalState::Completed,
                None,
            )
            .unwrap();
    }
    let ticket_a = runtime
        .issue_idle_batch(access(&ingress, &request_a, 180_000), &a.session, true)
        .unwrap();
    assert_eq!(
        runtime
            .issue_idle_batch(access(&ingress, &request_b, 180_000), &b.session, true)
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    let capability_a = install_capability(
        &mut runtime,
        &ingress,
        &request_a,
        vec![a.session.clone()],
        full_permissions(),
    );
    runtime
        .soft_delete_fragment(
            access(&ingress, &request_a, 180_001),
            &capability_a,
            &a.session,
            0,
        )
        .unwrap();
    assert_eq!(
        runtime
            .read_batch(access(&ingress, &request_a, 180_002), &ticket_a)
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    assert!(
        runtime
            .issue_idle_batch(access(&ingress, &request_b, 180_002), &b.session, true)
            .is_ok()
    );
}

#[test]
fn agent_finish_rejects_after_recycle_and_returns_no_delivery() {
    let (mut ingress, mut sessions, mut memory, mut agent, presets, grant) =
        g2_fixture(retention(1_000_000, 1_000_000));
    let request = admit(&mut ingress, "token-api", "buddy", "合成用户输入", "hint");
    let registration = register(&mut sessions, &ingress, &request, 0);
    let mut model = CountingModel { calls: 0 };
    let agent_access = AgentAccess::new(&ingress, &request, &presets, 0);
    let binding = agent
        .bind_session(
            &agent_access,
            &mut sessions,
            &registration.session,
            grant.clone(),
        )
        .unwrap();
    let mut task = agent
        .prepare(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &binding,
            &registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let result = agent
        .infer(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &mut task,
            &mut model,
        )
        .unwrap();
    assert_eq!(model.calls, 1);
    let capability = install_capability(
        &mut sessions,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    sessions
        .soft_delete_fragment(
            access(&ingress, &request, 2),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    let finish_access = AgentAccess::new(&ingress, &request, &presets, 4);
    assert_eq!(
        agent
            .finish(
                &finish_access,
                AgentServices::new(&mut sessions, &mut memory),
                &mut task,
                &result,
            )
            .unwrap_err(),
        AgentError::Gateway(GatewayError::StaleHandle)
    );
    let snapshot = sessions
        .inspect_fragment(
            access(&ingress, &request, 3),
            &capability,
            &registration.session,
        )
        .unwrap();
    assert_eq!(
        snapshot.events[0].terminal(),
        Some(TerminalState::Cancelled)
    );
}

#[test]
fn agent_inference_is_rejected_before_model_callback_after_recycle() {
    let (mut ingress, mut sessions, mut memory, mut agent, presets, grant) =
        g2_fixture(retention(1_000_000, 1_000_000));
    let request = admit(&mut ingress, "token-api", "buddy", "合成用户输入", "hint");
    let registration = register(&mut sessions, &ingress, &request, 0);
    let mut model = CountingModel { calls: 0 };
    let agent_access = AgentAccess::new(&ingress, &request, &presets, 0);
    let binding = agent
        .bind_session(&agent_access, &mut sessions, &registration.session, grant)
        .unwrap();
    let mut task = agent
        .prepare(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &binding,
            &registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let capability = install_capability(
        &mut sessions,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    sessions
        .soft_delete_fragment(
            access(&ingress, &request, 1),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    assert!(
        agent
            .infer(
                &agent_access,
                AgentServices::new(&mut sessions, &mut memory),
                &mut task,
                &mut model,
            )
            .is_err()
    );
    assert_eq!(model.calls, 0);
}

#[test]
fn agent_inference_rejects_on_natural_expiry_with_zero_model_calls() {
    let (mut ingress, mut sessions, mut memory, mut agent, presets, grant) =
        g2_fixture(retention(100, 100));
    let request = admit(&mut ingress, "token-api", "buddy", "合成用户输入", "hint");
    let registration = register(&mut sessions, &ingress, &request, 0);
    let mut model = CountingModel { calls: 0 };
    let agent_access = AgentAccess::new(&ingress, &request, &presets, 50);
    let binding = agent
        .bind_session(&agent_access, &mut sessions, &registration.session, grant)
        .unwrap();
    let mut task = agent
        .prepare(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &binding,
            &registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let infer_access = AgentAccess::new(&ingress, &request, &presets, 100);
    assert_eq!(
        agent
            .infer(
                &infer_access,
                AgentServices::new(&mut sessions, &mut memory),
                &mut task,
                &mut model,
            )
            .unwrap_err(),
        AgentError::Gateway(GatewayError::RawExpired)
    );
    assert_eq!(model.calls, 0);
}

#[test]
fn restore_does_not_revive_agent_tasks_interactions_or_activity() {
    let (mut ingress, mut sessions, mut memory, mut agent, presets, grant) =
        g2_fixture(retention(1_000_000, 1_000_000));
    let request = admit(&mut ingress, "token-api", "buddy", "合成用户输入", "hint");
    let registration = register(&mut sessions, &ingress, &request, 0);
    let mut model = CountingModel { calls: 0 };
    let agent_access = AgentAccess::new(&ingress, &request, &presets, 0);
    let binding = agent
        .bind_session(&agent_access, &mut sessions, &registration.session, grant)
        .unwrap();
    let mut task = agent
        .prepare(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &binding,
            &registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let result = agent
        .infer(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &mut task,
            &mut model,
        )
        .unwrap();
    let capability = install_capability(
        &mut sessions,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    sessions
        .soft_delete_fragment(
            access(&ingress, &request, 2),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    sessions
        .restore_fragment(
            access(&ingress, &request, 3),
            &capability,
            &registration.session,
            1,
        )
        .unwrap();
    assert!(
        agent
            .infer(
                &agent_access,
                AgentServices::new(&mut sessions, &mut memory),
                &mut task,
                &mut model,
            )
            .is_err()
    );
    assert!(
        agent
            .finish(
                &agent_access,
                AgentServices::new(&mut sessions, &mut memory),
                &mut task,
                &result,
            )
            .is_err()
    );
    assert_eq!(model.calls, 1);
    assert_eq!(
        sessions
            .finish_interaction(
                access(&ingress, &request, 3),
                &registration.interaction,
                TerminalState::Completed,
                Some("迟到回复"),
            )
            .unwrap_err(),
        GatewayError::InteractionClosed
    );
    assert_eq!(
        sessions
            .read_active(access(&ingress, &request, 3), &registration.session)
            .unwrap_err(),
        GatewayError::SessionExpired
    );
    let later = admit(&mut ingress, "token-api", "buddy", "新输入", "hint");
    let second = register(&mut sessions, &ingress, &later, 4);
    assert_ne!(second.session, registration.session);
    assert!(
        sessions
            .read_active(access(&ingress, &later, 4), &second.session)
            .is_ok()
    );
}

#[test]
fn agent_binding_rejected_on_expired_and_restored_fragments() {
    let (mut ingress, mut sessions, mut _memory, mut agent, presets, grant) =
        g2_fixture(retention(1_000, 1_000_000));
    let request = admit(&mut ingress, "token-api", "buddy", "合成用户输入", "hint");
    let registration = register(&mut sessions, &ingress, &request, 0);
    let agent_access = AgentAccess::new(&ingress, &request, &presets, 1_000);
    assert_eq!(
        agent
            .bind_session(&agent_access, &mut sessions, &registration.session, grant)
            .unwrap_err(),
        AgentError::Gateway(GatewayError::RawExpired)
    );
    let capability = install_capability(
        &mut sessions,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    sessions
        .soft_delete_fragment(
            access(&ingress, &request, 1_001),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    sessions
        .restore_fragment(
            access(&ingress, &request, 1_002),
            &capability,
            &registration.session,
            1,
        )
        .unwrap();
    let agent_access = AgentAccess::new(&ingress, &request, &presets, 1_003);
    assert_eq!(
        agent
            .bind_session(
                &agent_access,
                &mut sessions,
                &registration.session,
                Vec::new()
            )
            .unwrap_err(),
        AgentError::Gateway(GatewayError::SessionExpired)
    );
}

#[test]
fn watermarks_and_acknowledgements_survive_recycle_and_restore() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(10_000_000, 10_000_000),
        limits(),
        SessionTiming::default(),
    );
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    runtime
        .finish_interaction(
            access(&ingress, &request, 1),
            &registration.interaction,
            TerminalState::Completed,
            Some("合成回复"),
        )
        .unwrap();
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let ticket = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    assert_eq!(ticket.first_ordinal(), 1);
    assert_eq!(ticket.last_ordinal(), 2);
    let events = runtime
        .read_batch(access(&ingress, &request, 180_000), &ticket)
        .unwrap();
    for event in &events {
        runtime
            .acknowledge_batch(
                access(&ingress, &request, 180_000),
                &ticket,
                event.id(),
                BatchAcknowledgement::Confirmed,
            )
            .unwrap();
    }
    runtime
        .soft_delete_fragment(
            access(&ingress, &request, 180_001),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    let recycled = runtime
        .inspect_fragment(
            access(&ingress, &request, 180_001),
            &capability,
            &registration.session,
        )
        .unwrap();
    assert_eq!(recycled.processed_through, 2);
    assert_eq!(recycled.events.len(), 2);
    runtime
        .restore_fragment(
            access(&ingress, &request, 180_002),
            &capability,
            &registration.session,
            1,
        )
        .unwrap();
    let restored = runtime
        .inspect_fragment(
            access(&ingress, &request, 180_002),
            &capability,
            &registration.session,
        )
        .unwrap();
    assert_eq!(restored.processed_through, 2);
    assert_eq!(
        restored
            .events
            .iter()
            .map(|event| event.body())
            .collect::<Vec<_>>(),
        vec!["原文", "合成回复"]
    );
    assert_eq!(restored.events[1].role(), EventRole::Assistant);
    assert_eq!(
        runtime
            .issue_idle_batch(
                access(&ingress, &request, 180_003),
                &registration.session,
                true
            )
            .unwrap_err(),
        GatewayError::NotReady
    );
}

#[test]
fn partial_acknowledgements_resume_after_restore_without_reprocessing() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(10_000_000, 10_000_000),
        limits(),
        SessionTiming::default(),
    );
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    runtime
        .append_related(
            access(&ingress, &request, 1),
            &registration.interaction,
            RelatedEvent::new(EventRole::ToolReceipt, "工具回执"),
        )
        .unwrap();
    runtime
        .finish_interaction(
            access(&ingress, &request, 2),
            &registration.interaction,
            TerminalState::Completed,
            None,
        )
        .unwrap();
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let ticket = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    let events = runtime
        .read_batch(access(&ingress, &request, 180_000), &ticket)
        .unwrap();
    runtime
        .acknowledge_batch(
            access(&ingress, &request, 180_000),
            &ticket,
            events[0].id(),
            BatchAcknowledgement::Confirmed,
        )
        .unwrap();
    runtime
        .soft_delete_fragment(
            access(&ingress, &request, 180_001),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    runtime
        .restore_fragment(
            access(&ingress, &request, 180_002),
            &capability,
            &registration.session,
            1,
        )
        .unwrap();
    let resumed = runtime
        .issue_idle_batch(
            access(&ingress, &request, 180_003),
            &registration.session,
            true,
        )
        .unwrap();
    assert_eq!(resumed.first_ordinal(), 1);
    assert_eq!(resumed.watermark(), 0);
    let events = runtime
        .read_batch(access(&ingress, &request, 180_003), &resumed)
        .unwrap();
    assert_eq!(
        events[0].acknowledgement(),
        Some(BatchAcknowledgement::Confirmed)
    );
    assert_eq!(events[1].acknowledgement(), None);
    runtime
        .acknowledge_batch(
            access(&ingress, &request, 180_003),
            &resumed,
            events[0].id(),
            BatchAcknowledgement::Confirmed,
        )
        .unwrap();
    let progress = runtime
        .acknowledge_batch(
            access(&ingress, &request, 180_003),
            &resumed,
            events[1].id(),
            BatchAcknowledgement::Skipped,
        )
        .unwrap();
    assert_eq!(progress.acknowledged, 2);
    assert_eq!(progress.processed_through, 2);
}

#[test]
fn cleaning_session_raw_text_keeps_long_term_memory_intact() {
    let (mut ingress, mut sessions, mut memory, mut agent, presets, grant) =
        g2_fixture(retention(1_000_000, 1_000_000));
    let request = admit(&mut ingress, "token-api", "buddy", "合成用户输入", "hint");
    let registration = register(&mut sessions, &ingress, &request, 0);
    let mut model = CountingModel { calls: 0 };
    let agent_access = AgentAccess::new(&ingress, &request, &presets, 0);
    let binding = agent
        .bind_session(
            &agent_access,
            &mut sessions,
            &registration.session,
            grant.clone(),
        )
        .unwrap();
    let mut task = agent
        .prepare(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &binding,
            &registration.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let result = agent
        .infer(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &mut task,
            &mut model,
        )
        .unwrap();
    let delivery = agent
        .finish(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &mut task,
            &result,
        )
        .unwrap();
    assert_eq!(delivery.state(), TerminalState::Completed);
    let capability = install_capability(
        &mut sessions,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let purged = sessions
        .purge_fragment(
            access(&ingress, &request, 4),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    assert_eq!(purged.released_events, 2);
    let later = admit(&mut ingress, "token-api", "buddy", "第二次输入", "hint");
    let second = register(&mut sessions, &ingress, &later, 5);
    let agent_access = AgentAccess::new(&ingress, &later, &presets, 5);
    let binding = agent
        .bind_session(&agent_access, &mut sessions, &second.session, grant)
        .unwrap();
    let mut task = agent
        .prepare(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &binding,
            &second.interaction,
            &RetrievalQuery::new(""),
        )
        .unwrap();
    let result = agent
        .infer(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &mut task,
            &mut model,
        )
        .unwrap();
    let delivery = agent
        .finish(
            &agent_access,
            AgentServices::new(&mut sessions, &mut memory),
            &mut task,
            &result,
        )
        .unwrap();
    assert_eq!(delivery.state(), TerminalState::Completed);
    assert_eq!(model.calls, 2);
}

#[test]
fn restored_fragment_events_are_preserved_verbatim() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(10_000_000, 10_000_000),
        limits(),
        SessionTiming::default(),
    );
    let request = admit(&mut ingress, "token-api", "buddy", "用户原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    runtime
        .append_related(
            access(&ingress, &request, 1),
            &registration.interaction,
            RelatedEvent::new(EventRole::ExternalAgent, "外部事件"),
        )
        .unwrap();
    runtime
        .finish_interaction(
            access(&ingress, &request, 2),
            &registration.interaction,
            TerminalState::Completed,
            Some("最终回复"),
        )
        .unwrap();
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let before = runtime
        .inspect_fragment(
            access(&ingress, &request, 3),
            &capability,
            &registration.session,
        )
        .unwrap();
    runtime
        .soft_delete_fragment(
            access(&ingress, &request, 4),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    runtime
        .restore_fragment(
            access(&ingress, &request, 5),
            &capability,
            &registration.session,
            1,
        )
        .unwrap();
    let after = runtime
        .inspect_fragment(
            access(&ingress, &request, 5),
            &capability,
            &registration.session,
        )
        .unwrap();
    assert_eq!(after.session, before.session);
    assert_eq!(after.events.len(), before.events.len());
    for (latest, original) in after.events.iter().zip(before.events.iter()) {
        assert_eq!(latest.id(), original.id());
        assert_eq!(latest.body(), original.body());
        assert_eq!(latest.role(), original.role());
        assert_eq!(latest.terminal(), original.terminal());
        assert_eq!(latest.acknowledgement(), original.acknowledgement());
        assert_eq!(latest.source_timestamp_ms(), original.source_timestamp_ms());
    }
    assert_eq!(after.processed_through, before.processed_through);
}

#[test]
fn empty_recycle_is_repeatable_and_scoped_to_capability() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "第一片段", "hint");
    let first = register(&mut runtime, &ingress, &request, 0);
    let later = admit(&mut ingress, "token-api", "buddy", "第二片段", "hint");
    let second = register(&mut runtime, &ingress, &later, 1_100);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![first.session.clone(), second.session.clone()],
        full_permissions(),
    );
    let report = runtime.maintain_retention(&ingress, 2_200).unwrap();
    assert_eq!(report.transferred, 2);
    let emptied = runtime
        .empty_recycle(access(&ingress, &request, 2_201), &capability)
        .unwrap();
    assert_eq!(emptied.purged, 2);
    assert_eq!(emptied.released_events, 2);
    let repeat = runtime
        .empty_recycle(access(&ingress, &request, 2_202), &capability)
        .unwrap();
    assert_eq!(repeat.purged, 0);
    let summaries = runtime
        .list_managed_fragments(access(&ingress, &request, 2_202), &capability)
        .unwrap();
    assert!(summaries.is_empty());
}

#[test]
fn expired_latest_fragment_routes_new_events_to_a_new_fragment() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let first = register(&mut runtime, &ingress, &request, 0);
    let later = admit(&mut ingress, "token-api", "buddy", "新原文", "hint");
    let second = register(&mut runtime, &ingress, &later, 1_000);
    assert_ne!(first.session, second.session);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &request, 1_000), &first.session)
            .unwrap_err(),
        GatewayError::RawExpired
    );
    let history_request = admit(&mut ingress, "token-api", "buddy", "历史原文", "hint");
    let history = runtime
        .register_user(
            access(&ingress, &history_request, 1_001),
            UserEvent::new(UserEventKind::History),
        )
        .unwrap();
    assert_eq!(history.session, second.session);
    assert!(!history.duplicate);
    assert!(!history.renewed);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &later,
        vec![first.session.clone(), second.session.clone()],
        full_permissions(),
    );
    let summaries = runtime
        .list_managed_fragments(access(&ingress, &later, 1_001), &capability)
        .unwrap();
    assert_eq!(summaries.len(), 2);
    let first_summary = summaries
        .iter()
        .find(|summary| summary.session == first.session)
        .unwrap();
    assert!(first_summary.logically_expired);
    assert_eq!(first_summary.event_count, 1);
    let second_summary = summaries
        .iter()
        .find(|summary| summary.session == second.session)
        .unwrap();
    assert!(!second_summary.logically_expired);
    assert_eq!(second_summary.event_count, 2);
}

#[test]
fn lifecycle_version_conflicts_are_rejected_without_state_change() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    assert_eq!(
        runtime
            .soft_delete_fragment(
                access(&ingress, &request, 1),
                &capability,
                &registration.session,
                7
            )
            .unwrap_err(),
        GatewayError::LifecycleVersionConflict
    );
    let deleted = runtime
        .soft_delete_fragment(
            access(&ingress, &request, 1),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    assert_eq!(
        runtime
            .soft_delete_fragment(
                access(&ingress, &request, 2),
                &capability,
                &registration.session,
                0
            )
            .unwrap_err(),
        GatewayError::InvalidInput("fragment is already in the recycle zone")
    );
    assert_eq!(
        runtime
            .restore_fragment(
                access(&ingress, &request, 2),
                &capability,
                &registration.session,
                0
            )
            .unwrap_err(),
        GatewayError::LifecycleVersionConflict
    );
    assert_eq!(
        runtime
            .restore_fragment(
                access(&ingress, &request, 2),
                &capability,
                &registration.session,
                deleted.lifecycle_version,
            )
            .unwrap()
            .zone,
        RetentionZone::Normal
    );
    assert_eq!(
        runtime
            .restore_fragment(
                access(&ingress, &request, 3),
                &capability,
                &registration.session,
                deleted.lifecycle_version,
            )
            .unwrap_err(),
        GatewayError::InvalidInput("fragment is not in the recycle zone")
    );
}

#[test]
fn management_paths_distinguish_pending_transfer_from_final_expiry() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "合成到期原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let pending = runtime
        .inspect_fragment(
            access(&ingress, &request, 1_000),
            &capability,
            &registration.session,
        )
        .unwrap();
    assert!(pending.logically_expired);
    let summaries = runtime
        .list_managed_fragments(access(&ingress, &request, 1_000), &capability)
        .unwrap();
    assert!(summaries[0].logically_expired);
    assert!(!summaries[0].final_expired);
    assert_eq!(
        runtime
            .inspect_fragment(
                access(&ingress, &request, 3_000),
                &capability,
                &registration.session,
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
    assert_eq!(
        runtime
            .soft_delete_fragment(
                access(&ingress, &request, 3_000),
                &capability,
                &registration.session,
                0,
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
    let summaries = runtime
        .list_managed_fragments(access(&ingress, &request, 3_000), &capability)
        .unwrap();
    assert!(summaries[0].logically_expired);
    assert!(summaries[0].final_expired);
    let purged = runtime
        .purge_fragment(
            access(&ingress, &request, 3_001),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    assert_eq!(purged.released_events, 1);
}

#[test]
fn capability_install_rejects_prior_generation_fragments_atomically() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "合成换代原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    ingress
        .register_route("extra", identity("ws", "owner", "extra-pet"))
        .unwrap();
    let current = admit(&mut ingress, "token-api", "buddy", "合成当前输入", "hint");
    assert_eq!(
        runtime
            .install_management_capability(
                &ingress,
                current.identity().clone(),
                current.isolation().clone(),
                vec![registration.session.clone()],
                full_permissions(),
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    let later = admit(&mut ingress, "token-api", "buddy", "合成新片段", "hint");
    let second = register(&mut runtime, &ingress, &later, 1);
    let fresh = install_capability(
        &mut runtime,
        &ingress,
        &later,
        vec![second.session.clone()],
        full_permissions(),
    );
    assert_eq!(
        runtime
            .install_management_capability(
                &ingress,
                later.identity().clone(),
                later.isolation().clone(),
                vec![registration.session.clone(), second.session.clone()],
                full_permissions(),
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert!(
        runtime
            .inspect_fragment(access(&ingress, &later, 2), &fresh, &second.session)
            .is_ok()
    );
}

#[test]
fn maintenance_with_current_ingress_removes_prior_generation_fragments() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "合成撤权原文", "hint");
    let registration = runtime
        .register_user(
            access(&ingress, &request, 0),
            UserEvent::new(UserEventKind::New)
                .with_id(HostEventId::new("synthetic-stale-host").unwrap()),
        )
        .unwrap();
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    runtime
        .soft_delete_fragment(
            access(&ingress, &request, 1),
            &capability,
            &registration.session,
            0,
        )
        .unwrap();
    let later = admit(&mut ingress, "token-api", "buddy", "合成新片段", "hint");
    let second = register(&mut runtime, &ingress, &later, 2);
    ingress
        .register_route("extra", identity("ws", "owner", "extra-pet"))
        .unwrap();
    let current = admit(&mut ingress, "token-api", "buddy", "合成换代输入", "hint");
    assert_eq!(
        runtime
            .inspect_fragment(access(&ingress, &current, 3), &capability, &second.session)
            .unwrap_err(),
        GatewayError::VersionConflict
    );
    let report = runtime.maintain_retention(&ingress, 3).unwrap();
    assert_eq!(report.authorization_removed, 2);
    assert_eq!(report.transferred, 0);
    assert_eq!(report.removed, 0);
    assert_eq!(
        runtime
            .read_active(access(&ingress, &current, 3), &second.session)
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    assert_eq!(
        runtime
            .install_management_capability(
                &ingress,
                current.identity().clone(),
                current.isolation().clone(),
                vec![registration.session.clone()],
                full_permissions(),
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );
    let replay = admit(&mut ingress, "token-api", "buddy", "合成撤权原文", "hint");
    assert_eq!(
        runtime.register_user(
            access(&ingress, &replay, 4),
            UserEvent::new(UserEventKind::New)
                .with_id(HostEventId::new("synthetic-stale-host").unwrap()),
        ),
        Err(GatewayError::Revoked)
    );
    let fresh = admit(&mut ingress, "token-api", "buddy", "合成新原文", "hint");
    let third = register(&mut runtime, &ingress, &fresh, 5);
    assert!(
        runtime
            .read_active(access(&ingress, &fresh, 5), &third.session)
            .is_ok()
    );
    let fresh_capability = install_capability(
        &mut runtime,
        &ingress,
        &fresh,
        vec![third.session.clone()],
        full_permissions(),
    );
    assert!(
        runtime
            .soft_delete_fragment(
                access(&ingress, &fresh, 6),
                &fresh_capability,
                &third.session,
                0,
            )
            .is_ok()
    );
}

#[test]
fn review_management_read_rejects_final_expiry_without_maintenance() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "合成到期原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let snapshot = runtime
        .inspect_fragment(
            access(&ingress, &request, 0),
            &capability,
            &registration.session,
        )
        .unwrap();
    let recycled = runtime
        .soft_delete_fragment(
            access(&ingress, &request, 1),
            &capability,
            &registration.session,
            snapshot.lifecycle_version,
        )
        .unwrap();
    let deadline = recycled.final_deadline_ms.unwrap();
    assert!(
        runtime
            .inspect_fragment(
                access(&ingress, &request, deadline),
                &capability,
                &registration.session,
            )
            .is_err()
    );
}

#[test]
fn review_new_capability_cannot_read_a_prior_generation_fragment() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "合成撤权原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    ingress
        .register_route("extra", identity("ws", "owner", "extra-pet"))
        .unwrap();
    let current = admit(&mut ingress, "token-api", "buddy", "合成当前输入", "hint");
    let result = runtime
        .install_management_capability(
            &ingress,
            current.identity().clone(),
            current.isolation().clone(),
            vec![registration.session.clone()],
            full_permissions(),
        )
        .and_then(|capability| {
            runtime.inspect_fragment(
                access(&ingress, &current, 1),
                &capability,
                &registration.session,
            )
        });
    assert!(result.is_err());
}

#[test]
fn review_new_capability_cannot_restore_a_prior_generation_fragment() {
    let (mut ingress, mut runtime) = fixture();
    let request = admit(&mut ingress, "token-api", "buddy", "合成恢复原文", "hint");
    let registration = register(&mut runtime, &ingress, &request, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let snapshot = runtime
        .inspect_fragment(
            access(&ingress, &request, 0),
            &capability,
            &registration.session,
        )
        .unwrap();
    let recycled = runtime
        .soft_delete_fragment(
            access(&ingress, &request, 1),
            &capability,
            &registration.session,
            snapshot.lifecycle_version,
        )
        .unwrap();
    ingress
        .register_route("extra", identity("ws", "owner", "extra-pet"))
        .unwrap();
    let current = admit(&mut ingress, "token-api", "buddy", "合成当前输入", "hint");
    let result = runtime
        .install_management_capability(
            &ingress,
            current.identity().clone(),
            current.isolation().clone(),
            vec![registration.session.clone()],
            full_permissions(),
        )
        .and_then(|capability| {
            runtime.restore_fragment(
                access(&ingress, &current, 2),
                &capability,
                &registration.session,
                recycled.lifecycle_version,
            )
        });
    assert!(result.is_err());
}

#[test]
fn review_cleaned_request_without_host_id_cannot_register_again() {
    for cleanup in ["revoke", "purge", "expiry"] {
        let (mut ingress, mut runtime) = fixture();
        let request = admit(&mut ingress, "token-api", "buddy", "合成清理原文", "hint");
        let registration = register(&mut runtime, &ingress, &request, 0);
        let capability = install_capability(
            &mut runtime,
            &ingress,
            &request,
            vec![registration.session.clone()],
            full_permissions(),
        );
        runtime
            .cancel_interaction(access(&ingress, &request, 0), &registration.interaction)
            .unwrap();
        let now_ms = match cleanup {
            "revoke" => {
                runtime
                    .revoke_session(access(&ingress, &request, 1), &registration.session)
                    .unwrap();
                1
            }
            "purge" => {
                runtime
                    .purge_fragment(
                        access(&ingress, &request, 1),
                        &capability,
                        &registration.session,
                        0,
                    )
                    .unwrap();
                1
            }
            "expiry" => {
                runtime.maintain_retention(&ingress, 3_000).unwrap();
                3_000
            }
            _ => unreachable!(),
        };
        assert_eq!(
            runtime.register_user(
                access(&ingress, &request, now_ms),
                UserEvent::new(UserEventKind::New)
            ),
            Err(GatewayError::Revoked),
            "{cleanup}",
        );
    }
}

#[test]
fn review_full_replay_ledger_blocks_ingestion_without_blocking_cleanup() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(1_000, 2_000),
        SessionLimits::new(8, 16, 1, 1_024, 8_192, 2).unwrap(),
        SessionTiming::default(),
    );
    let first = admit(&mut ingress, "token-api", "buddy", "first", "hint");
    let first_registration = runtime
        .register_user(
            access(&ingress, &first, 0),
            UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-a").unwrap()),
        )
        .unwrap();
    let first_capability = install_capability(
        &mut runtime,
        &ingress,
        &first,
        vec![first_registration.session.clone()],
        full_permissions(),
    );
    let first_recycled = runtime
        .soft_delete_fragment(
            access(&ingress, &first, 1),
            &first_capability,
            &first_registration.session,
            0,
        )
        .unwrap();
    let second = admit(&mut ingress, "token-api", "buddy", "second", "hint");
    let second_registration = runtime
        .register_user(
            access(&ingress, &second, 2),
            UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-b").unwrap()),
        )
        .unwrap();
    let second_capability = install_capability(
        &mut runtime,
        &ingress,
        &second,
        vec![second_registration.session.clone()],
        full_permissions(),
    );
    runtime
        .purge_fragment(
            access(&ingress, &first, 3),
            &first_capability,
            &first_registration.session,
            first_recycled.lifecycle_version,
        )
        .unwrap();
    let second_recycled = runtime
        .soft_delete_fragment(
            access(&ingress, &second, 4),
            &second_capability,
            &second_registration.session,
            0,
        )
        .unwrap();
    let third = admit(&mut ingress, "token-api", "buddy", "third", "hint");
    let third_registration = register(&mut runtime, &ingress, &third, 5);
    let third_capability = install_capability(
        &mut runtime,
        &ingress,
        &third,
        vec![third_registration.session.clone()],
        full_permissions(),
    );
    runtime
        .purge_fragment(
            access(&ingress, &second, 6),
            &second_capability,
            &second_registration.session,
            second_recycled.lifecycle_version,
        )
        .unwrap();
    let receipt = runtime
        .purge_fragment(
            access(&ingress, &third, 7),
            &third_capability,
            &third_registration.session,
            0,
        )
        .unwrap();
    assert_eq!(receipt.released_events, 1);
    let replay = admit(&mut ingress, "token-api", "buddy", "second", "hint");
    assert!(
        runtime
            .register_user(
                access(&ingress, &replay, 8),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-b").unwrap()),
            )
            .is_err()
    );
    let fresh = admit(&mut ingress, "token-api", "buddy", "fresh", "hint");
    assert!(
        runtime
            .register_user(
                access(&ingress, &fresh, 8),
                UserEvent::new(UserEventKind::New)
            )
            .is_err()
    );
    assert!(runtime.maintain_retention(&ingress, 9).is_ok());
}

#[test]
fn review_related_events_check_cleaned_host_ids_in_the_original_isolation() {
    let (mut ingress, mut runtime) = fixture();
    let original = admit(&mut ingress, "token-api", "buddy", "original", "hint");
    let original_registration = register(&mut runtime, &ingress, &original, 0);
    runtime
        .append_related(
            access(&ingress, &original, 0),
            &original_registration.interaction,
            RelatedEvent::new(EventRole::ToolReceipt, "synthetic receipt")
                .with_id(HostEventId::new("revoked-receipt").unwrap()),
        )
        .unwrap();
    runtime
        .revoke_session(
            access(&ingress, &original, 1),
            &original_registration.session,
        )
        .unwrap();
    let current = admit(&mut ingress, "token-api", "buddy", "current", "hint");
    let current_registration = register(&mut runtime, &ingress, &current, 2);
    assert_eq!(
        runtime.append_related(
            access(&ingress, &current, 2),
            &current_registration.interaction,
            RelatedEvent::new(EventRole::ToolReceipt, "synthetic receipt")
                .with_id(HostEventId::new("revoked-receipt").unwrap()),
        ),
        Err(GatewayError::Revoked)
    );
    let other = admit(
        &mut ingress,
        "token-api",
        "buddy",
        "other",
        "different-hint",
    );
    let other_registration = register(&mut runtime, &ingress, &other, 3);
    assert!(
        runtime
            .append_related(
                access(&ingress, &other, 3),
                &other_registration.interaction,
                RelatedEvent::new(EventRole::ToolReceipt, "synthetic receipt")
                    .with_id(HostEventId::new("revoked-receipt").unwrap()),
            )
            .is_ok()
    );
}

#[test]
fn review_duplicate_user_registration_rechecks_lifecycle() {
    for with_host_id in [false, true] {
        let (mut ingress, mut runtime) = fixture();
        let request = admit(
            &mut ingress,
            "token-api",
            "buddy",
            "synthetic original",
            "hint",
        );
        let input = if with_host_id {
            UserEvent::new(UserEventKind::New).with_id(HostEventId::new("original-id").unwrap())
        } else {
            UserEvent::new(UserEventKind::New)
        };
        runtime
            .register_user(access(&ingress, &request, 0), input.clone())
            .unwrap();
        assert_eq!(
            runtime.register_user(access(&ingress, &request, 1_000), input.clone()),
            Err(GatewayError::RawExpired)
        );
        runtime.maintain_retention(&ingress, 1_000).unwrap();
        assert_eq!(
            runtime.register_user(access(&ingress, &request, 1_001), input),
            Err(GatewayError::Recycled)
        );
    }
}

#[test]
fn review_maintenance_reports_released_normal_and_recycle_capacity() {
    for recycled in [false, true] {
        let (mut ingress, mut runtime) = fixture();
        let request = admit(&mut ingress, "token-api", "buddy", "原文四字", "hint");
        let registration = register(&mut runtime, &ingress, &request, 0);
        let deadline = if recycled {
            let capability = install_capability(
                &mut runtime,
                &ingress,
                &request,
                vec![registration.session.clone()],
                full_permissions(),
            );
            runtime
                .soft_delete_fragment(
                    access(&ingress, &request, 1),
                    &capability,
                    &registration.session,
                    0,
                )
                .unwrap()
                .final_deadline_ms
                .unwrap()
        } else {
            3_000
        };
        let report = runtime.maintain_retention(&ingress, deadline).unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(report.released_events, 1);
        assert_eq!(report.released_chars, 4);
    }
}

#[test]
fn review_maintenance_reports_authorization_cleanup_release() {
    let (mut ingress, mut runtime) = fixture();
    let first = admit(&mut ingress, "token-api", "buddy", "原文四字", "first");
    let first_registration = register(&mut runtime, &ingress, &first, 0);
    let second = admit(&mut ingress, "token-api", "buddy", "原文四字", "second");
    register(&mut runtime, &ingress, &second, 0);
    let capability = install_capability(
        &mut runtime,
        &ingress,
        &first,
        vec![first_registration.session.clone()],
        full_permissions(),
    );
    runtime
        .soft_delete_fragment(
            access(&ingress, &first, 1),
            &capability,
            &first_registration.session,
            0,
        )
        .unwrap();
    ingress
        .register_route("extra", identity("ws", "owner", "extra-pet"))
        .unwrap();
    let report = runtime.maintain_retention(&ingress, 2).unwrap();
    assert_eq!(report.authorization_removed, 2);
    assert_eq!(report.released_events, 2);
    assert_eq!(report.released_chars, 8);
}

#[test]
fn review_exhausted_replay_ledger_blocks_related_events_with_free_capacity() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(1_000, 2_000),
        SessionLimits::new(8, 16, 2, 1_024, 8_192, 2).unwrap(),
        SessionTiming::default(),
    );
    for (now_ms, hint) in [(0, "cleaned-a"), (2, "cleaned-b")] {
        let request = admit(&mut ingress, "token-api", "buddy", "cleaned", hint);
        let registration = register(&mut runtime, &ingress, &request, now_ms);
        runtime
            .revoke_session(
                access(&ingress, &request, now_ms + 1),
                &registration.session,
            )
            .unwrap();
    }
    let current = admit(&mut ingress, "token-api", "buddy", "current", "surviving");
    let current_registration = register(&mut runtime, &ingress, &current, 4);
    let overflowing = admit(
        &mut ingress,
        "token-api",
        "buddy",
        "overflowing",
        "overflowing",
    );
    let overflow_registration = register(&mut runtime, &ingress, &overflowing, 5);
    runtime
        .revoke_session(
            access(&ingress, &overflowing, 6),
            &overflow_registration.session,
        )
        .unwrap();
    assert!(
        runtime
            .append_related(
                access(&ingress, &current, 7),
                &current_registration.interaction,
                RelatedEvent::new(EventRole::ToolReceipt, "new receipt"),
            )
            .is_err()
    );
    let snapshot = runtime
        .read_active(access(&ingress, &current, 7), &current_registration.session)
        .unwrap();
    assert_eq!(snapshot.events.len(), 1);
}

fn cleanup_for_ordering(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    registration: &Registration,
    cleanup: &str,
) -> u64 {
    match cleanup {
        "present" => 1,
        "expiry" => {
            let report = runtime.maintain_retention(ingress, 3_000).unwrap();
            assert_eq!(report.removed, 1);
            3_000
        }
        "revoke" => {
            runtime
                .revoke_session(access(ingress, request, 1), &registration.session)
                .unwrap();
            1
        }
        "purge" | "empty" => {
            let capability = install_capability(
                runtime,
                ingress,
                request,
                vec![registration.session.clone()],
                full_permissions(),
            );
            if cleanup == "purge" {
                runtime
                    .purge_fragment(
                        access(ingress, request, 1),
                        &capability,
                        &registration.session,
                        0,
                    )
                    .unwrap();
            } else {
                runtime
                    .soft_delete_fragment(
                        access(ingress, request, 1),
                        &capability,
                        &registration.session,
                        0,
                    )
                    .unwrap();
                assert_eq!(
                    runtime
                        .empty_recycle(access(ingress, request, 1), &capability)
                        .unwrap()
                        .purged,
                    1
                );
            }
            1
        }
        _ => unreachable!(),
    }
}

fn assert_ordering_history(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    registration: &Registration,
    now_ms: u64,
) {
    assert!(!registration.duplicate);
    assert!(!registration.renewed);
    let capability = install_capability(
        runtime,
        ingress,
        request,
        vec![registration.session.clone()],
        full_permissions(),
    );
    let snapshot = runtime
        .inspect_fragment(
            access(ingress, request, now_ms),
            &capability,
            &registration.session,
        )
        .unwrap();
    let event = snapshot.events.last().unwrap();
    assert_eq!(event.role(), EventRole::UserHistory);
    assert_eq!(event.terminal(), Some(TerminalState::Unknown));
    assert_eq!(
        runtime.finish_interaction(
            access(ingress, request, now_ms),
            &registration.interaction,
            TerminalState::Completed,
            Some("must not deliver"),
        ),
        Err(GatewayError::DeadlineExceeded)
    );
}

#[test]
fn review_cleanup_ordering_preserves_source_watermark() {
    for cleanup in ["present", "expiry", "revoke", "purge", "empty"] {
        for source_order in [9, 10] {
            let (mut ingress, mut runtime) = fixture();
            let current = admit(&mut ingress, "token-api", "buddy", "current", "hint");
            let original = runtime
                .register_user(
                    access(&ingress, &current, 0),
                    UserEvent::new(UserEventKind::New).with_source_order(10),
                )
                .unwrap();
            assert!(original.renewed);
            let now_ms = cleanup_for_ordering(&mut runtime, &ingress, &current, &original, cleanup);
            let late = admit(&mut ingress, "token-api", "buddy", "late", "hint");
            let registration = runtime
                .register_user(
                    access(&ingress, &late, now_ms),
                    UserEvent::new(UserEventKind::New).with_source_order(source_order),
                )
                .unwrap();
            assert!(
                !registration.renewed,
                "{cleanup}, source_order={source_order}"
            );
            assert_ordering_history(&mut runtime, &ingress, &late, &registration, now_ms);
            let newest = admit(&mut ingress, "token-api", "buddy", "newest", "hint");
            assert!(
                runtime
                    .register_user(
                        access(&ingress, &newest, now_ms),
                        UserEvent::new(UserEventKind::New).with_source_order(11),
                    )
                    .unwrap()
                    .renewed
            );
        }
    }
}

#[test]
fn review_cleanup_ordering_preserves_admission_watermark() {
    for cleanup in ["present", "expiry", "revoke", "purge", "empty"] {
        for source_order in [None, Some(11)] {
            let (mut ingress, mut runtime) = fixture();
            let early = admit(&mut ingress, "token-api", "buddy", "early", "hint");
            let current = admit(&mut ingress, "token-api", "buddy", "current", "hint");
            let original = runtime
                .register_user(
                    access(&ingress, &current, 0),
                    UserEvent::new(UserEventKind::New).with_source_order(10),
                )
                .unwrap();
            let now_ms = cleanup_for_ordering(&mut runtime, &ingress, &current, &original, cleanup);
            let mut input = UserEvent::new(UserEventKind::New);
            if let Some(order) = source_order {
                input = input.with_source_order(order);
            }
            let registration = runtime
                .register_user(access(&ingress, &early, now_ms), input)
                .unwrap();
            assert!(
                !registration.renewed,
                "{cleanup}, source_order={source_order:?}"
            );
            assert_ordering_history(&mut runtime, &ingress, &early, &registration, now_ms);
        }
    }
}

#[test]
fn review_cleanup_ordering_watermarks_use_complete_isolation() {
    for cleanup in ["purge", "expiry"] {
        let (mut ingress, mut runtime) = fixture();
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
        let mut isolated = Vec::new();
        for (token, alias, hint) in [
            ("token-channel", "buddy", "hint"),
            ("token-scope", "buddy", "hint"),
            ("token-subject", "buddy", "hint"),
            ("token-credential", "buddy", "hint"),
            ("token-api", "other", "hint"),
            ("token-api", "buddy", "other-hint"),
        ] {
            isolated.push(admit(&mut ingress, token, alias, "isolated", hint));
        }
        isolated.push(
            ingress
                .admit(RawIngressCall::new(
                    IngressToken::new("token-api").unwrap(),
                    "buddy",
                    "fresh isolation",
                ))
                .unwrap(),
        );
        let current = admit(&mut ingress, "token-api", "buddy", "current", "hint");
        let original = runtime
            .register_user(
                access(&ingress, &current, 0),
                UserEvent::new(UserEventKind::New).with_source_order(10),
            )
            .unwrap();
        let now_ms = cleanup_for_ordering(&mut runtime, &ingress, &current, &original, cleanup);
        for request in isolated {
            assert!(
                runtime
                    .register_user(
                        access(&ingress, &request, now_ms),
                        UserEvent::new(UserEventKind::New).with_source_order(1),
                    )
                    .unwrap()
                    .renewed,
                "{cleanup}: {:?}",
                request.isolation(),
            );
        }
        let late = admit(&mut ingress, "token-api", "buddy", "late", "hint");
        assert!(
            !runtime
                .register_user(
                    access(&ingress, &late, now_ms),
                    UserEvent::new(UserEventKind::New).with_source_order(9),
                )
                .unwrap()
                .renewed
        );
    }
}

#[test]
fn review_cleanup_ordering_merges_maxima_without_resetting_missing_source() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(1_000, 2_000),
        limits(),
        SessionTiming::new(10, 180_000, 10, 10).unwrap(),
    );
    let first = admit(&mut ingress, "token-api", "buddy", "first", "hint");
    let first_registration = runtime
        .register_user(
            access(&ingress, &first, 0),
            UserEvent::new(UserEventKind::New).with_source_order(10),
        )
        .unwrap();
    let delayed = admit(&mut ingress, "token-api", "buddy", "delayed", "hint");
    let second = admit(&mut ingress, "token-api", "buddy", "second", "hint");
    let second_registration = runtime
        .register_user(
            access(&ingress, &second, 10),
            UserEvent::new(UserEventKind::New).with_source_order(20),
        )
        .unwrap();
    assert_ne!(first_registration.session, second_registration.session);
    runtime
        .revoke_session(access(&ingress, &second, 11), &second_registration.session)
        .unwrap();
    runtime
        .revoke_session(access(&ingress, &first, 12), &first_registration.session)
        .unwrap();
    let missing = admit(&mut ingress, "token-api", "buddy", "missing order", "hint");
    let missing_registration = register(&mut runtime, &ingress, &missing, 13);
    assert!(missing_registration.renewed);
    runtime
        .revoke_session(
            access(&ingress, &missing, 14),
            &missing_registration.session,
        )
        .unwrap();
    let late = admit(&mut ingress, "token-api", "buddy", "late", "hint");
    let late_registration = runtime
        .register_user(
            access(&ingress, &late, 15),
            UserEvent::new(UserEventKind::New).with_source_order(15),
        )
        .unwrap();
    assert_ordering_history(&mut runtime, &ingress, &late, &late_registration, 15);
    let delayed_registration = register(&mut runtime, &ingress, &delayed, 15);
    assert_ordering_history(&mut runtime, &ingress, &delayed, &delayed_registration, 15);
    let newest = admit(&mut ingress, "token-api", "buddy", "newest", "hint");
    assert!(
        runtime
            .register_user(
                access(&ingress, &newest, 15),
                UserEvent::new(UserEventKind::New).with_source_order(21),
            )
            .unwrap()
            .renewed
    );
}

#[test]
fn review_cleanup_ordering_budget_exhaustion_is_fail_closed() {
    let (mut ingress, mut runtime) = fixture_with(
        retention(1_000, 2_000),
        SessionLimits::new(8, 16, 2, 1_024, 8_192, 2).unwrap(),
        SessionTiming::default(),
    );
    for (now_ms, hint) in [(0, "domain-a"), (2, "domain-b"), (4, "domain-c")] {
        let request = admit(&mut ingress, "token-api", "buddy", "current", hint);
        let registration = runtime
            .register_user(
                access(&ingress, &request, now_ms),
                UserEvent::new(UserEventKind::New).with_source_order(10),
            )
            .unwrap();
        runtime
            .revoke_session(
                access(&ingress, &request, now_ms + 1),
                &registration.session,
            )
            .unwrap();
    }
    for hint in ["domain-a", "domain-c", "unseen-domain"] {
        let request = admit(&mut ingress, "token-api", "buddy", "late", hint);
        assert_eq!(
            runtime.register_user(
                access(&ingress, &request, 6),
                UserEvent::new(UserEventKind::New).with_source_order(9),
            ),
            Err(GatewayError::ReplayProtectionExhausted),
        );
    }
    assert!(runtime.maintain_retention(&ingress, 6).is_ok());
}
