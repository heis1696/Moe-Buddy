//! G4 步骤 2 外部回归：原文快照导出/导入纯运行契约。
//!
//! 覆盖隔离（含同入口双凭据归属双向隔离）、当前鉴权、无执行权、重复/冲突
//! （精确规范内容 CAS 与校验和碰撞）、时间/容量/编码预算、失效授权、撤销
//! 不维护直接导出、跨重启稳定归属映射、快照过期维护与失败无半更新；并逐
//! 阻塞闭环：稳定片段键撤销栅栏（B01）、收紧政策有效最终截止（B02）、语义
//! 坏档发布前拒绝（B03）、确认前缀/父事件一致性（B07）、栅栏累计预算整体
//! 拒绝（B05）、极端计数不毒化（B06）、导出编码失败不发布键（B04/B09）、
//! 授权修订绑定、提交时间完整性与历史闸门复用。全部使用合成正文与内存
//! 快照，不做文件或网络 I/O。

use moe_agent_runtime::{
    CallerSubject, ChannelId, ChannelScope, GatewayError, HostEventId, IngressToken,
    ManagementPermissions, RawDomainKey, RawHistoryGrant, RawIngressCall, RawRetention,
    RawSnapshotV1, ReplyTarget, RetentionZone, SessionAccess, SessionLimits, SessionRuntime,
    SessionTiming, TerminalState, TrustedIngress, TrustedRequest, UserEvent, UserEventKind,
};
use moe_memory_core::{Namespace, PetIdentity};

const ALIAS: &str = "buddy";
const TOKEN: &str = "synthetic-persistence-token";
const ATTRIBUTION: &str = "cred-1";
const SUBJECT: &str = "caller";
const CHANNEL: &str = "channel";
const SCOPE: &str = "scope";
const HINT: &str = "conversation";
const REPLY_TARGET: &str = "reply-target";
const IDENTITY_WORKSPACE: &str = "workspace";
const IDENTITY_USER: &str = "owner";
const IDENTITY_INSTANCE: &str = "pet";
const BODY: &str = "需要留存的合成原文";
const REPLY: &str = "宿主登记的合成实际回复";

fn identity() -> PetIdentity {
    PetIdentity::new(
        Namespace::new(IDENTITY_WORKSPACE, IDENTITY_USER).unwrap(),
        IDENTITY_INSTANCE,
    )
    .unwrap()
}

fn retention(normal_ms: u64, recycle_ms: u64) -> RawRetention {
    RawRetention::new(normal_ms, recycle_ms, 8, 128, 8_192).unwrap()
}

fn limits() -> SessionLimits {
    SessionLimits::new(8, 16, 64, 1_024, 8_192, 2).unwrap()
}

fn install_credential(ingress: &mut TrustedIngress, token: &str, attribution: &str) {
    let credential = ingress
        .install_credential(
            IngressToken::new(token).unwrap(),
            CallerSubject::new(SUBJECT).unwrap(),
            [ALIAS.to_owned()],
            ChannelId::new(CHANNEL).unwrap(),
            ChannelScope::new(SCOPE).unwrap(),
            ReplyTarget::new(REPLY_TARGET).unwrap(),
        )
        .unwrap();
    ingress
        .bind_stable_credential(credential, attribution)
        .unwrap();
}

fn ingress_with_bound_credentials(tokens: &[(&str, &str)]) -> TrustedIngress {
    let mut ingress = TrustedIngress::new();
    ingress.register_route(ALIAS, identity()).unwrap();
    for (token, attribution) in tokens {
        install_credential(&mut ingress, token, attribution);
    }
    ingress
}

fn fixture(
    retention: RawRetention,
    limits: SessionLimits,
    now_ms: u64,
) -> (TrustedIngress, SessionRuntime) {
    let ingress = ingress_with_bound_credentials(&[(TOKEN, ATTRIBUTION)]);
    let runtime = SessionRuntime::with_retention(
        &ingress,
        limits,
        SessionTiming::default(),
        retention,
        now_ms,
    );
    (ingress, runtime)
}

fn admit(ingress: &mut TrustedIngress, hint: &str, text: &str) -> TrustedRequest {
    admit_token(ingress, TOKEN, hint, text)
}

fn admit_token(
    ingress: &mut TrustedIngress,
    token: &str,
    hint: &str,
    text: &str,
) -> TrustedRequest {
    ingress
        .admit(
            RawIngressCall::new(IngressToken::new(token).unwrap(), ALIAS, text)
                .with_session_hint(hint),
        )
        .unwrap()
}

fn register(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    now_ms: u64,
    event: UserEvent,
) -> moe_agent_runtime::Registration {
    runtime
        .register_user(SessionAccess::new(ingress, request, now_ms), event)
        .unwrap()
}

fn grant_for(
    ingress: &mut TrustedIngress,
    runtime: &mut SessionRuntime,
    request: &TrustedRequest,
) -> RawHistoryGrant {
    let domain = RawDomainKey::from_request(ingress, request).unwrap();
    runtime.install_raw_history_grant(ingress, domain).unwrap()
}

fn install_view(
    runtime: &mut SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    session: &moe_agent_runtime::SessionId,
) -> moe_agent_runtime::ManagementCapability {
    runtime
        .install_management_capability(
            ingress,
            request.identity().clone(),
            request.isolation().clone(),
            vec![session.clone()],
            ManagementPermissions::default().with_view(),
        )
        .unwrap()
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[test]
fn persistence_apis_require_retention_and_hinted_domain() {
    let (mut unbound_ingress, mut plain) = {
        let mut ingress = TrustedIngress::new();
        ingress.register_route(ALIAS, identity()).unwrap();
        ingress
            .install_credential(
                IngressToken::new(TOKEN).unwrap(),
                CallerSubject::new(SUBJECT).unwrap(),
                [ALIAS.to_owned()],
                ChannelId::new(CHANNEL).unwrap(),
                ChannelScope::new(SCOPE).unwrap(),
                ReplyTarget::new(REPLY_TARGET).unwrap(),
            )
            .unwrap();
        let runtime = SessionRuntime::new(&ingress, limits(), SessionTiming::default(), 0);
        (ingress, runtime)
    };
    let hinted = admit(&mut unbound_ingress, HINT, BODY);
    let access = SessionAccess::new(&unbound_ingress, &hinted, 1);
    assert_eq!(
        plain.export_raw_snapshot(access, 1).unwrap_err(),
        GatewayError::RetentionNotConfigured
    );
    assert_eq!(
        plain
            .install_raw_history_grant(
                &unbound_ingress,
                RawDomainKey::new(
                    identity(),
                    CallerSubject::new(SUBJECT).unwrap(),
                    ChannelId::new(CHANNEL).unwrap(),
                    ChannelScope::new(SCOPE).unwrap(),
                    ATTRIBUTION.to_owned(),
                    HINT.to_owned(),
                )
                .unwrap()
            )
            .unwrap_err(),
        GatewayError::RetentionNotConfigured
    );

    let (mut enabled_ingress, mut enabled) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let hinted = admit(&mut enabled_ingress, HINT, BODY);
    let grant = grant_for(&mut enabled_ingress, &mut enabled, &hinted);
    let snapshot = enabled
        .export_raw_snapshot(SessionAccess::new(&enabled_ingress, &hinted, 1), 1)
        .unwrap();
    let fresh = enabled_ingress
        .admit(RawIngressCall::new(
            IngressToken::new(TOKEN).unwrap(),
            ALIAS,
            BODY,
        ))
        .unwrap();
    assert!(matches!(
        RawDomainKey::from_request(&enabled_ingress, &fresh),
        Err(GatewayError::InvalidInput(_))
    ));
    assert_eq!(
        enabled
            .export_raw_snapshot(SessionAccess::new(&enabled_ingress, &fresh, 1), 1)
            .unwrap_err(),
        GatewayError::InvalidInput("request has no stable session hint")
    );
    assert_eq!(
        enabled
            .import_raw_snapshot(
                SessionAccess::new(&enabled_ingress, &fresh, 1),
                &grant,
                &snapshot
            )
            .unwrap_err(),
        GatewayError::InvalidInput("request has no stable session hint")
    );
    assert_eq!(
        RawDomainKey::new(
            identity(),
            CallerSubject::new(SUBJECT).unwrap(),
            ChannelId::new(CHANNEL).unwrap(),
            ChannelScope::new(SCOPE).unwrap(),
            ATTRIBUTION.to_owned(),
            String::new(),
        )
        .unwrap_err(),
        GatewayError::InvalidInput("invalid identifier")
    );
    assert_eq!(
        RawDomainKey::new(
            identity(),
            CallerSubject::new(SUBJECT).unwrap(),
            ChannelId::new(CHANNEL).unwrap(),
            ChannelScope::new(SCOPE).unwrap(),
            String::new(),
            HINT.to_owned(),
        )
        .unwrap_err(),
        GatewayError::InvalidInput("invalid identifier")
    );
    let ghost_domain = RawDomainKey::new(
        identity(),
        CallerSubject::new(SUBJECT).unwrap(),
        ChannelId::new(CHANNEL).unwrap(),
        ChannelScope::new(SCOPE).unwrap(),
        "cred-never-bound".to_owned(),
        HINT.to_owned(),
    )
    .unwrap();
    assert_eq!(
        enabled
            .install_raw_history_grant(&enabled_ingress, ghost_domain)
            .unwrap_err(),
        GatewayError::InvalidInput(
            "domain credential attribution is not bound to a live credential"
        )
    );
}

#[test]
fn export_import_roundtrip_preserves_history_without_execution_rights() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    let registration = register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New)
            .with_id(HostEventId::new("host-1").unwrap())
            .with_source_order(100)
            .with_source_timestamp_ms(7),
    );
    runtime_a
        .finish_interaction(
            SessionAccess::new(&ingress_a, &request_a, 1),
            &registration.interaction,
            TerminalState::Completed,
            Some(REPLY),
        )
        .unwrap();
    let ticket = runtime_a
        .issue_idle_batch(
            SessionAccess::new(&ingress_a, &request_a, 180_000),
            &registration.session,
            true,
        )
        .unwrap();
    for event in runtime_a
        .read_batch(SessionAccess::new(&ingress_a, &request_a, 180_000), &ticket)
        .unwrap()
    {
        runtime_a
            .acknowledge_batch(
                SessionAccess::new(&ingress_a, &request_a, 180_000),
                &ticket,
                event.id(),
                moe_agent_runtime::BatchAcknowledgement::Confirmed,
            )
            .unwrap();
    }
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 200_000), 200_000)
        .unwrap();
    let bytes = snapshot.encode().unwrap();
    let decoded = RawSnapshotV1::decode(&bytes).unwrap();
    assert_eq!(decoded, snapshot);
    assert_eq!(decoded.fragment_count(), 1);

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    let receipt = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 200_001),
            &grant,
            &decoded,
        )
        .unwrap();
    assert!(!receipt.duplicate);
    assert!(!receipt.replay_protection_exhausted);
    assert_eq!(receipt.fragments.len(), 1);
    let restored = &receipt.fragments[0];
    assert_eq!(restored.zone, RetentionZone::Normal);
    assert_eq!(restored.normal_deadline_ms, 1_000_000);
    assert_eq!(restored.final_deadline_ms, None);

    let capability = install_view(&mut runtime_b, &ingress_b, &request_b, &restored.session);
    let view = runtime_b
        .inspect_fragment(
            SessionAccess::new(&ingress_b, &request_b, 200_001),
            &capability,
            &restored.session,
        )
        .unwrap();
    assert_eq!(view.processed_through, 2);
    assert_eq!(view.events.len(), 2);
    let user = &view.events[0];
    assert_eq!(user.role(), moe_agent_runtime::EventRole::User);
    assert_eq!(user.body(), BODY);
    assert_eq!(user.source_timestamp_ms(), Some(7));
    assert_eq!(user.source_order(), Some(100));
    assert_eq!(user.terminal(), Some(TerminalState::Completed));
    assert_eq!(
        user.acknowledgement(),
        Some(moe_agent_runtime::BatchAcknowledgement::Confirmed)
    );
    let assistant = &view.events[1];
    assert_eq!(assistant.role(), moe_agent_runtime::EventRole::Assistant);
    assert_eq!(assistant.body(), REPLY);

    assert_eq!(
        runtime_b
            .read_active(
                SessionAccess::new(&ingress_b, &request_b, 200_001),
                &restored.session
            )
            .unwrap_err(),
        GatewayError::SessionExpired
    );
    assert_eq!(
        runtime_b
            .issue_idle_batch(
                SessionAccess::new(&ingress_b, &request_b, 200_001),
                &restored.session,
                true,
            )
            .unwrap_err(),
        GatewayError::NotReady
    );
    let redelivery = register(
        &mut runtime_b,
        &ingress_b,
        &request_b,
        200_002,
        UserEvent::new(UserEventKind::History)
            .with_id(HostEventId::new("host-1").unwrap())
            .with_source_order(100)
            .with_source_timestamp_ms(7),
    );
    assert!(redelivery.duplicate);
    assert_eq!(redelivery.session, restored.session);
    assert_eq!(
        runtime_b
            .finish_interaction(
                SessionAccess::new(&ingress_b, &request_b, 200_003),
                &redelivery.interaction,
                TerminalState::Completed,
                Some(REPLY),
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    assert_eq!(
        runtime_b
            .cancel_interaction(
                SessionAccess::new(&ingress_b, &request_b, 200_003),
                &redelivery.interaction,
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    assert_eq!(
        runtime_b
            .read_active(
                SessionAccess::new(&ingress_b, &request_a, 200_003),
                &registration.session
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
}

#[test]
fn import_requires_current_request_and_exact_grant() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 1), 1)
        .unwrap();

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    runtime_b.revoke_raw_history_grant(&grant).unwrap();
    runtime_b.revoke_raw_history_grant(&grant).unwrap();
    assert_eq!(
        runtime_b
            .import_raw_snapshot(
                SessionAccess::new(&ingress_b, &request_b, 1),
                &grant,
                &snapshot
            )
            .unwrap_err(),
        GatewayError::Revoked
    );

    let (mut ingress_c, mut runtime_c) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_c = admit(&mut ingress_c, HINT, BODY);
    let foreign_grant = grant_for(&mut ingress_c, &mut runtime_c, &request_c);
    let (mut ingress_d, mut runtime_d) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_d = admit(&mut ingress_d, HINT, BODY);
    assert_eq!(
        runtime_d
            .import_raw_snapshot(
                SessionAccess::new(&ingress_d, &request_d, 1),
                &foreign_grant,
                &snapshot,
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    let mismatched_domain = {
        let request = admit(&mut ingress_d, "other-hint", BODY);
        RawDomainKey::from_request(&ingress_d, &request).unwrap()
    };
    let mismatched_grant = runtime_d
        .install_raw_history_grant(&ingress_d, mismatched_domain)
        .unwrap();
    assert_eq!(
        runtime_d
            .import_raw_snapshot(
                SessionAccess::new(&ingress_d, &request_d, 1),
                &mismatched_grant,
                &snapshot,
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );

    let (mut ingress_e, mut runtime_e) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_e = admit(&mut ingress_e, HINT, BODY);
    let grant_e = grant_for(&mut ingress_e, &mut runtime_e, &request_e);
    ingress_e.register_route("second", identity()).unwrap();
    assert_eq!(
        runtime_e
            .import_raw_snapshot(
                SessionAccess::new(&ingress_e, &request_e, 1),
                &grant_e,
                &snapshot
            )
            .unwrap_err(),
        GatewayError::Revoked
    );

    let (mut ingress_f, mut runtime_f) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_f = admit(&mut ingress_f, HINT, BODY);
    let grant_f = grant_for(&mut ingress_f, &mut runtime_f, &request_f);
    let other_domain = {
        let request = admit(&mut ingress_f, "other-hint", BODY);
        RawDomainKey::from_request(&ingress_f, &request).unwrap()
    };
    let wrong_grant = runtime_f
        .install_raw_history_grant(&ingress_f, other_domain)
        .unwrap();
    assert_eq!(
        runtime_f
            .import_raw_snapshot(
                SessionAccess::new(&ingress_f, &request_f, 1),
                &wrong_grant,
                &snapshot
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
    let wrong_projection = admit(&mut ingress_f, "other-hint", BODY);
    assert_eq!(
        runtime_f
            .import_raw_snapshot(
                SessionAccess::new(&ingress_f, &wrong_projection, 1),
                &grant_f,
                &snapshot,
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
}

#[test]
fn duplicate_import_is_idempotent_and_conflicting_content_is_rejected() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let snapshot_one = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 10), 10)
        .unwrap();

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    let first = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 11),
            &grant,
            &snapshot_one,
        )
        .unwrap();
    let again = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 12),
            &grant,
            &snapshot_one,
        )
        .unwrap();
    assert!(again.duplicate);
    assert_eq!(again.fragments, first.fragments);
    let resealed = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 20), 20)
        .unwrap();
    let resealed_receipt = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 21),
            &grant,
            &resealed,
        )
        .unwrap();
    assert!(resealed_receipt.duplicate);
    assert_eq!(resealed_receipt.fragments, first.fragments);

    let request_a2 = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a2,
        25,
        UserEvent::new(UserEventKind::History).with_id(HostEventId::new("host-2").unwrap()),
    );
    let snapshot_two = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 30), 30)
        .unwrap();
    assert_eq!(
        runtime_b
            .import_raw_snapshot(
                SessionAccess::new(&ingress_b, &request_b, 31),
                &grant,
                &snapshot_two
            )
            .unwrap_err(),
        GatewayError::VersionConflict
    );
    let capability = install_view(
        &mut runtime_b,
        &ingress_b,
        &request_b,
        &first.fragments[0].session,
    );
    let view = runtime_b
        .inspect_fragment(
            SessionAccess::new(&ingress_b, &request_b, 31),
            &capability,
            &first.fragments[0].session,
        )
        .unwrap();
    assert_eq!(view.events.len(), 1);
}

#[test]
fn import_rejects_time_rollback() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 100), 100)
        .unwrap();

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    assert_eq!(
        runtime_b
            .import_raw_snapshot(
                SessionAccess::new(&ingress_b, &request_b, 50),
                &grant,
                &snapshot
            )
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );
    let (mut ingress_c, mut runtime_c) = fixture(retention(1_000_000, 1_000_000), limits(), 500);
    let request_c = admit(&mut ingress_c, HINT, BODY);
    let grant_c = grant_for(&mut ingress_c, &mut runtime_c, &request_c);
    assert_eq!(
        runtime_c
            .import_raw_snapshot(
                SessionAccess::new(&ingress_c, &request_c, 300),
                &grant_c,
                &snapshot
            )
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );
}

#[test]
fn final_deadline_equality_blocks_import_and_recycle_keeps_original_deadline() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000, 1_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 0), 0)
        .unwrap();

    let build = |now: u64| {
        let (mut ingress, mut runtime) = fixture(retention(1_000, 1_000), limits(), 0);
        let request = admit(&mut ingress, HINT, BODY);
        let grant = grant_for(&mut ingress, &mut runtime, &request);
        let result = runtime.import_raw_snapshot(
            SessionAccess::new(&ingress, &request, now),
            &grant,
            &snapshot,
        );
        (ingress, runtime, request, result)
    };
    let (_, _, _, late) = build(2_000);
    assert_eq!(late.unwrap_err(), GatewayError::RawExpired);
    let (ingress_ok, mut runtime_ok, request_ok, ok) = build(1_500);
    let receipt = ok.unwrap();
    assert_eq!(receipt.fragments[0].normal_deadline_ms, 1_000);
    let capability = install_view(
        &mut runtime_ok,
        &ingress_ok,
        &request_ok,
        &receipt.fragments[0].session,
    );
    let summaries = runtime_ok
        .list_managed_fragments(
            SessionAccess::new(&ingress_ok, &request_ok, 1_500),
            &capability,
        )
        .unwrap();
    assert!(summaries[0].logically_expired);
    assert!(!summaries[0].final_expired);
    let report = runtime_ok.maintain_retention(&ingress_ok, 1_500).unwrap();
    assert_eq!(report.transferred, 1);
    let view = runtime_ok
        .inspect_fragment(
            SessionAccess::new(&ingress_ok, &request_ok, 1_500),
            &capability,
            &receipt.fragments[0].session,
        )
        .unwrap();
    assert_eq!(view.zone, RetentionZone::Recycle);
    assert_eq!(view.final_deadline_ms, Some(2_000));

    let (mut ingress_e, mut runtime_e) = fixture(retention(100, 100), limits(), 0);
    let request_e = admit(&mut ingress_e, HINT, BODY);
    register(
        &mut runtime_e,
        &ingress_e,
        &request_e,
        0,
        UserEvent::new(UserEventKind::New),
    );
    assert_eq!(
        runtime_e
            .export_raw_snapshot(SessionAccess::new(&ingress_e, &request_e, 250), 250)
            .unwrap_err(),
        GatewayError::RawExpired
    );
}

#[test]
fn stricter_policy_tightens_and_looser_policy_never_renews() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(100_000, 100_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 0), 0)
        .unwrap();

    let (mut ingress_b, mut runtime_b) = fixture(retention(50_000, 50_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant_b = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    let tight = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 1),
            &grant_b,
            &snapshot,
        )
        .unwrap();
    assert_eq!(tight.fragments[0].normal_deadline_ms, 50_000);

    let (mut ingress_c, mut runtime_c) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_c = admit(&mut ingress_c, HINT, BODY);
    let grant_c = grant_for(&mut ingress_c, &mut runtime_c, &request_c);
    let loose = runtime_c
        .import_raw_snapshot(
            SessionAccess::new(&ingress_c, &request_c, 1),
            &grant_c,
            &snapshot,
        )
        .unwrap();
    assert_eq!(loose.fragments[0].normal_deadline_ms, 100_000);
    assert_eq!(loose.fragments[0].final_deadline_ms, None);
}

#[test]
fn capacity_precheck_rejects_whole_snapshot_without_partial_state() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(100, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    let first_fragment = register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-1").unwrap()),
    )
    .session;
    let request_a_mid = admit(&mut ingress_a, HINT, BODY);
    let second_fragment = register(
        &mut runtime_a,
        &ingress_a,
        &request_a_mid,
        500,
        UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-2").unwrap()),
    )
    .session;
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 600), 600)
        .unwrap();
    assert_eq!(snapshot.fragment_count(), 2);

    let tight_limits = SessionLimits::new(1, 16, 64, 1_024, 8_192, 2).unwrap();
    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), tight_limits, 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    assert_eq!(
        runtime_b
            .import_raw_snapshot(
                SessionAccess::new(&ingress_b, &request_b, 600),
                &grant,
                &snapshot
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    let fresh = register(
        &mut runtime_b,
        &ingress_b,
        &request_b,
        601,
        UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-1").unwrap()),
    );
    assert!(!fresh.duplicate);

    let tight_events = SessionLimits::new(8, 16, 1, 1_024, 8_192, 2).unwrap();
    let (mut ingress_c, mut runtime_c) = fixture(retention(1_000_000, 1_000_000), tight_events, 0);
    let request_c = admit(&mut ingress_c, HINT, BODY);
    let grant_c = grant_for(&mut ingress_c, &mut runtime_c, &request_c);
    assert_eq!(
        runtime_c
            .import_raw_snapshot(
                SessionAccess::new(&ingress_c, &request_c, 600),
                &grant_c,
                &snapshot
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );

    let request_a2 = admit(&mut ingress_a, HINT, BODY);
    let purge_all = ManagementPermissions::default().with_soft_delete();
    for session in [first_fragment.clone(), second_fragment.clone()] {
        let capability = runtime_a
            .install_management_capability(
                &ingress_a,
                request_a2.identity().clone(),
                request_a2.isolation().clone(),
                vec![session.clone()],
                purge_all,
            )
            .unwrap();
        runtime_a
            .soft_delete_fragment(
                SessionAccess::new(&ingress_a, &request_a2, 700),
                &capability,
                &session,
                0,
            )
            .unwrap();
    }
    let recycled_snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a2, 800), 800)
        .unwrap();
    assert_eq!(recycled_snapshot.fragment_count(), 2);
    let tight_recycle = RawRetention::new(1_000_000, 1_000_000, 8, 1, 8_192).unwrap();
    let (mut ingress_d, mut runtime_d) = fixture(tight_recycle, limits(), 0);
    let request_d = admit(&mut ingress_d, HINT, BODY);
    let grant_d = grant_for(&mut ingress_d, &mut runtime_d, &request_d);
    assert_eq!(
        runtime_d
            .import_raw_snapshot(
                SessionAccess::new(&ingress_d, &request_d, 800),
                &grant_d,
                &recycled_snapshot,
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
}

#[test]
fn revoked_history_becomes_fence_not_restorable_content() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    let registration = register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-gone").unwrap()),
    );
    runtime_a
        .revoke_session(
            SessionAccess::new(&ingress_a, &request_a, 1),
            &registration.session,
        )
        .unwrap();
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 2), 2)
        .unwrap();
    assert_eq!(snapshot.fragment_count(), 0);

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    let receipt = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 3),
            &grant,
            &snapshot,
        )
        .unwrap();
    assert!(receipt.fragments.is_empty());
    assert_eq!(
        runtime_b
            .register_user(
                SessionAccess::new(&ingress_b, &request_b, 4),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-gone").unwrap()),
            )
            .unwrap_err(),
        GatewayError::Revoked
    );

    let (mut ingress_c, mut runtime_c) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_c = admit(&mut ingress_c, HINT, BODY);
    let stale_registration = register(
        &mut runtime_c,
        &ingress_c,
        &request_c,
        0,
        UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-stale").unwrap()),
    );
    ingress_c.register_route("second", identity()).unwrap();
    assert_eq!(
        runtime_c
            .export_raw_snapshot(SessionAccess::new(&ingress_c, &request_c, 1), 1)
            .unwrap_err(),
        GatewayError::Revoked
    );
    let _ = stale_registration;
    let report = runtime_c.maintain_retention(&ingress_c, 2).unwrap();
    assert_eq!(report.authorization_removed, 1);
    let request_c2 = admit(&mut ingress_c, HINT, BODY);
    let cleaned_snapshot = runtime_c
        .export_raw_snapshot(SessionAccess::new(&ingress_c, &request_c2, 3), 3)
        .unwrap();
    assert_eq!(cleaned_snapshot.fragment_count(), 0);
    let (mut ingress_d, mut runtime_d) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_d = admit(&mut ingress_d, HINT, BODY);
    let grant_d = grant_for(&mut ingress_d, &mut runtime_d, &request_d);
    runtime_d
        .import_raw_snapshot(
            SessionAccess::new(&ingress_d, &request_d, 4),
            &grant_d,
            &cleaned_snapshot,
        )
        .unwrap();
    assert_eq!(
        runtime_d
            .register_user(
                SessionAccess::new(&ingress_d, &request_d, 5),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-stale").unwrap()),
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
}

#[test]
fn source_order_watermark_and_exhaustion_survive_restore() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New).with_source_order(100),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 1), 1)
        .unwrap();

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    let receipt = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 2),
            &grant,
            &snapshot,
        )
        .unwrap();
    let late = register(
        &mut runtime_b,
        &ingress_b,
        &request_b,
        3,
        UserEvent::new(UserEventKind::New).with_source_order(50),
    );
    assert!(!late.renewed);
    assert_eq!(late.session, receipt.fragments[0].session);

    let (mut ingress_c, mut runtime_c) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_c = admit(&mut ingress_c, HINT, BODY);
    let grant_c = grant_for(&mut ingress_c, &mut runtime_c, &request_c);
    runtime_c
        .import_raw_snapshot(
            SessionAccess::new(&ingress_c, &request_c, 2),
            &grant_c,
            &snapshot,
        )
        .unwrap();
    let newer = register(
        &mut runtime_c,
        &ingress_c,
        &request_c,
        3,
        UserEvent::new(UserEventKind::New).with_source_order(101),
    );
    assert!(newer.renewed);
    assert_ne!(newer.session, receipt.fragments[0].session);

    let drained = SessionLimits::new(8, 16, 2, 1_024, 8_192, 2).unwrap();
    let (mut ingress_x, mut runtime_x) = fixture(retention(1_000_000, 1_000_000), drained, 0);
    for (index, now) in [(0usize, 0u64), (1, 2), (2, 4)] {
        let request_x = admit(&mut ingress_x, HINT, BODY);
        let registration = register(
            &mut runtime_x,
            &ingress_x,
            &request_x,
            now,
            UserEvent::new(UserEventKind::New)
                .with_id(HostEventId::new(format!("host-drain-{index}")).unwrap()),
        );
        let capability = runtime_x
            .install_management_capability(
                &ingress_x,
                request_x.identity().clone(),
                request_x.isolation().clone(),
                vec![registration.session.clone()],
                ManagementPermissions::default().with_purge(),
            )
            .unwrap();
        runtime_x
            .purge_fragment(
                SessionAccess::new(&ingress_x, &request_x, now + 1),
                &capability,
                &registration.session,
                0,
            )
            .unwrap();
    }
    let request_x = admit(&mut ingress_x, HINT, BODY);
    let exhausted_snapshot = runtime_x
        .export_raw_snapshot(SessionAccess::new(&ingress_x, &request_x, 7), 7)
        .unwrap();
    let (mut ingress_y, mut runtime_y) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_y = admit(&mut ingress_y, HINT, BODY);
    let grant_y = grant_for(&mut ingress_y, &mut runtime_y, &request_y);
    let receipt_y = runtime_y
        .import_raw_snapshot(
            SessionAccess::new(&ingress_y, &request_y, 8),
            &grant_y,
            &exhausted_snapshot,
        )
        .unwrap();
    assert!(receipt_y.replay_protection_exhausted);
    assert!(receipt_y.fragments.is_empty());
    assert_eq!(
        runtime_y
            .register_user(
                SessionAccess::new(&ingress_y, &request_y, 9),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap_err(),
        GatewayError::ReplayProtectionExhausted
    );
}

#[test]
fn restored_interactions_stay_inert_and_merge_with_existing_fences() {
    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let local = register(
        &mut runtime_b,
        &ingress_b,
        &request_b,
        0,
        UserEvent::new(UserEventKind::New).with_id(HostEventId::new("b-local").unwrap()),
    );
    runtime_b
        .revoke_session(
            SessionAccess::new(&ingress_b, &request_b, 1),
            &local.session,
        )
        .unwrap();
    let request_b2 = admit(&mut ingress_b, HINT, BODY);

    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New).with_id(HostEventId::new("a-snap").unwrap()),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 2), 2)
        .unwrap();

    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b2);
    let receipt = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b2, 3),
            &grant,
            &snapshot,
        )
        .unwrap();
    assert_eq!(receipt.fragments.len(), 1);
    assert_eq!(
        runtime_b
            .register_user(
                SessionAccess::new(&ingress_b, &request_b2, 4),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("b-local").unwrap()),
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    assert_eq!(
        runtime_b
            .register_user(
                SessionAccess::new(&ingress_b, &request_b2, 4),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("a-snap").unwrap()),
            )
            .unwrap_err(),
        GatewayError::EventConflict
    );
    let redelivery = register(
        &mut runtime_b,
        &ingress_b,
        &request_b2,
        5,
        UserEvent::new(UserEventKind::History).with_id(HostEventId::new("a-snap").unwrap()),
    );
    assert!(redelivery.duplicate);
    assert_eq!(redelivery.session, receipt.fragments[0].session);
    let unrelated = register(
        &mut runtime_b,
        &ingress_b,
        &request_b2,
        6,
        UserEvent::new(UserEventKind::History),
    );
    assert!(!unrelated.duplicate && !unrelated.renewed);
}

#[test]
fn idempotent_reimport_never_resurrects_cleaned_fragments() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 1), 1)
        .unwrap();

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    let first = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 2),
            &grant,
            &snapshot,
        )
        .unwrap();
    let capability = runtime_b
        .install_management_capability(
            &ingress_b,
            request_b.identity().clone(),
            request_b.isolation().clone(),
            vec![first.fragments[0].session.clone()],
            ManagementPermissions::default().with_purge(),
        )
        .unwrap();
    runtime_b
        .purge_fragment(
            SessionAccess::new(&ingress_b, &request_b, 3),
            &capability,
            &first.fragments[0].session,
            0,
        )
        .unwrap();
    let again = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 4),
            &grant,
            &snapshot,
        )
        .unwrap();
    assert!(again.duplicate);
    assert!(again.fragments.is_empty());
    let fresh = register(
        &mut runtime_b,
        &ingress_b,
        &request_b,
        5,
        UserEvent::new(UserEventKind::New),
    );
    assert!(!fresh.duplicate && fresh.renewed);
}

#[test]
fn grant_ledger_is_bounded_and_revocation_is_idempotent() {
    let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let mut grants = Vec::new();
    for _ in 0..64 {
        grants.push(
            runtime
                .install_raw_history_grant(
                    &ingress,
                    RawDomainKey::from_request(&ingress, &request).unwrap(),
                )
                .unwrap(),
        );
    }
    assert_eq!(
        runtime
            .install_raw_history_grant(
                &ingress,
                RawDomainKey::from_request(&ingress, &request).unwrap()
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    runtime.revoke_raw_history_grant(&grants[0]).unwrap();
    runtime.revoke_raw_history_grant(&grants[0]).unwrap();
    let foreign = {
        let (mut other_ingress, mut other_runtime) =
            fixture(retention(1_000_000, 1_000_000), limits(), 0);
        let other_request = admit(&mut other_ingress, HINT, BODY);
        grant_for(&mut other_ingress, &mut other_runtime, &other_request)
    };
    assert_eq!(
        runtime.revoke_raw_history_grant(&foreign).unwrap_err(),
        GatewayError::ForeignRequest
    );
}

#[test]
fn corrupt_bytes_fail_decoding() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 0), 0)
        .unwrap();
    let bytes = snapshot.encode().unwrap();

    let mut magic = bytes.clone();
    magic[0] = b"X"[0];
    assert!(matches!(
        RawSnapshotV1::decode(&magic),
        Err(GatewayError::RawSnapshotInvalid(_))
    ));
    let mut version = bytes.clone();
    version[8] = 9;
    assert!(matches!(
        RawSnapshotV1::decode(&version),
        Err(GatewayError::RawSnapshotInvalid(_))
    ));
    let mut tampered = bytes.clone();
    tampered[30] ^= 0xff;
    assert!(matches!(
        RawSnapshotV1::decode(&tampered),
        Err(GatewayError::RawSnapshotInvalid(_))
    ));
    assert!(matches!(
        RawSnapshotV1::decode(&bytes[..10]),
        Err(GatewayError::RawSnapshotInvalid(_))
    ));
    assert_eq!(
        RawSnapshotV1::decode(&[]).unwrap_err(),
        GatewayError::RawSnapshotInvalid("truncated header")
    );
    assert!(matches!(
        RawSnapshotV1::decode(&vec![
            0u8;
            moe_agent_runtime::RAW_SNAPSHOT_MAX_TOTAL_BYTES + 1
        ]),
        Err(GatewayError::RawSnapshotInvalid(_))
    ));
    let decoded = RawSnapshotV1::decode(&bytes).unwrap();
    assert_eq!(decoded, snapshot);
    let debug_output = format!("{:?}", decoded);
    assert!(!debug_output.contains(BODY));
}

#[test]
fn oversized_reply_bodies_still_roundtrip() {
    let wide = SessionLimits::new(8, 16, 64, 20_000, 65_536, 2).unwrap();
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), wide, 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    let registration = register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let reply = "长".repeat(17_000);
    runtime_a
        .finish_interaction(
            SessionAccess::new(&ingress_a, &request_a, 1),
            &registration.interaction,
            TerminalState::Completed,
            Some(&reply),
        )
        .unwrap();
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 2), 2)
        .unwrap();
    let decoded = RawSnapshotV1::decode(&snapshot.encode().unwrap()).unwrap();
    assert_eq!(decoded, snapshot);

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), wide, 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    let receipt = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 3),
            &grant,
            &decoded,
        )
        .unwrap();
    let capability = install_view(
        &mut runtime_b,
        &ingress_b,
        &request_b,
        &receipt.fragments[0].session,
    );
    let view = runtime_b
        .inspect_fragment(
            SessionAccess::new(&ingress_b, &request_b, 3),
            &capability,
            &receipt.fragments[0].session,
        )
        .unwrap();
    assert_eq!(view.events[1].body(), reply.as_str());
}

#[test]
fn domain_and_grant_debug_redacts_hint() {
    let domain = RawDomainKey::new(
        identity(),
        CallerSubject::new(SUBJECT).unwrap(),
        ChannelId::new(CHANNEL).unwrap(),
        ChannelScope::new(SCOPE).unwrap(),
        ATTRIBUTION.to_owned(),
        HINT.to_owned(),
    )
    .unwrap();
    let debug_output = format!("{:?}", domain);
    assert!(!debug_output.contains(HINT));
    assert!(debug_output.contains(ATTRIBUTION));
    let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let grant = grant_for(&mut ingress, &mut runtime, &request);
    assert_eq!(format!("{:?}", grant), "RawHistoryGrant(REDACTED)");
}

#[test]
fn stable_credential_binding_rules_are_precise() {
    let mut ingress = TrustedIngress::new();
    ingress.register_route(ALIAS, identity()).unwrap();
    let install = |ingress: &mut TrustedIngress, token: &str| {
        ingress
            .install_credential(
                IngressToken::new(token).unwrap(),
                CallerSubject::new(SUBJECT).unwrap(),
                [ALIAS.to_owned()],
                ChannelId::new(CHANNEL).unwrap(),
                ChannelScope::new(SCOPE).unwrap(),
                ReplyTarget::new(REPLY_TARGET).unwrap(),
            )
            .unwrap()
    };
    let first = install(&mut ingress, "token-1");
    assert_eq!(
        ingress.bind_stable_credential(first, "").unwrap_err(),
        GatewayError::InvalidInput("invalid identifier")
    );
    ingress.bind_stable_credential(first, "cred-a").unwrap();
    ingress.bind_stable_credential(first, "cred-a").unwrap();
    assert_eq!(
        ingress.bind_stable_credential(first, "cred-b").unwrap_err(),
        GatewayError::InvalidInput("credential is already bound to another stable attribution")
    );
    let second = install(&mut ingress, "token-2");
    assert_eq!(
        ingress
            .bind_stable_credential(second, "cred-a")
            .unwrap_err(),
        GatewayError::InvalidInput(
            "stable attribution is already bound to another live credential"
        )
    );
    ingress.bind_stable_credential(second, "cred-b").unwrap();

    let request_a = admit_token(&mut ingress, "token-1", HINT, BODY);
    let request_b = admit_token(&mut ingress, "token-2", HINT, BODY);
    let domain_a = RawDomainKey::from_request(&ingress, &request_a).unwrap();
    let domain_b = RawDomainKey::from_request(&ingress, &request_b).unwrap();
    assert_ne!(domain_a, domain_b);
    assert_eq!(domain_a.credential(), "cred-a");
    assert_eq!(domain_b.credential(), "cred-b");

    ingress.revoke_credential(first).unwrap();
    assert_eq!(ingress.credential_attribution(first), Some("cred-a"));
    let third = install(&mut ingress, "token-3");
    ingress.bind_stable_credential(third, "cred-a").unwrap();
    let request_rotated = admit_token(&mut ingress, "token-3", HINT, BODY);
    assert_eq!(
        RawDomainKey::from_request(&ingress, &request_rotated).unwrap(),
        domain_a
    );
    assert_eq!(
        ingress.bind_stable_credential(first, "cred-z").unwrap_err(),
        GatewayError::InvalidInput("unknown credential")
    );
}

#[test]
fn two_live_credentials_stay_isolated_in_body_and_watermarks() {
    let mut ingress =
        ingress_with_bound_credentials(&[("token-a", "cred-a"), ("token-b", "cred-b")]);
    let mut runtime = SessionRuntime::with_retention(
        &ingress,
        limits(),
        SessionTiming::default(),
        retention(1_000_000, 1_000_000),
        0,
    );

    let request_a = admit_token(&mut ingress, "token-a", HINT, BODY);
    let registration_a = register(
        &mut runtime,
        &ingress,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New)
            .with_id(HostEventId::new("host-a").unwrap())
            .with_source_order(100),
    );
    let request_b = admit_token(&mut ingress, "token-b", HINT, BODY);
    let registration_b = register(
        &mut runtime,
        &ingress,
        &request_b,
        1,
        UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-b").unwrap()),
    );

    let capability_a = runtime
        .install_management_capability(
            &ingress,
            request_a.identity().clone(),
            request_a.isolation().clone(),
            vec![registration_a.session.clone()],
            ManagementPermissions::default().with_purge(),
        )
        .unwrap();
    runtime
        .purge_fragment(
            SessionAccess::new(&ingress, &request_a, 2),
            &capability_a,
            &registration_a.session,
            0,
        )
        .unwrap();

    let snapshot_a = runtime
        .export_raw_snapshot(SessionAccess::new(&ingress, &request_a, 3), 3)
        .unwrap();
    assert_eq!(snapshot_a.fragment_count(), 0);
    let snapshot_b = runtime
        .export_raw_snapshot(SessionAccess::new(&ingress, &request_b, 3), 3)
        .unwrap();
    assert_eq!(snapshot_b.fragment_count(), 1);
    let snapshot_b_again = runtime
        .export_raw_snapshot(SessionAccess::new(&ingress, &request_b, 3), 3)
        .unwrap();
    assert_eq!(
        snapshot_b_again.encode().unwrap(),
        snapshot_b.encode().unwrap()
    );

    let mut ingress_restored_a = ingress_with_bound_credentials(&[(TOKEN, "cred-a")]);
    let mut runtime_restored_a = SessionRuntime::with_retention(
        &ingress_restored_a,
        limits(),
        SessionTiming::default(),
        retention(1_000_000, 1_000_000),
        0,
    );
    let request_restored_a = admit(&mut ingress_restored_a, HINT, BODY);
    let grant_a = grant_for(
        &mut ingress_restored_a,
        &mut runtime_restored_a,
        &request_restored_a,
    );
    let receipt_a = runtime_restored_a
        .import_raw_snapshot(
            SessionAccess::new(&ingress_restored_a, &request_restored_a, 3),
            &grant_a,
            &snapshot_a,
        )
        .unwrap();
    assert!(receipt_a.fragments.is_empty());
    assert_eq!(
        runtime_restored_a
            .register_user(
                SessionAccess::new(&ingress_restored_a, &request_restored_a, 4),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-a").unwrap()),
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    let suppressed = register(
        &mut runtime_restored_a,
        &ingress_restored_a,
        &request_restored_a,
        5,
        UserEvent::new(UserEventKind::New).with_source_order(50),
    );
    assert!(!suppressed.renewed);

    let mut ingress_restored_b = ingress_with_bound_credentials(&[(TOKEN, "cred-b")]);
    let mut runtime_restored_b = SessionRuntime::with_retention(
        &ingress_restored_b,
        limits(),
        SessionTiming::default(),
        retention(1_000_000, 1_000_000),
        0,
    );
    let request_restored_b = admit(&mut ingress_restored_b, HINT, BODY);
    let grant_b = grant_for(
        &mut ingress_restored_b,
        &mut runtime_restored_b,
        &request_restored_b,
    );
    let receipt_b = runtime_restored_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_restored_b, &request_restored_b, 3),
            &grant_b,
            &snapshot_b,
        )
        .unwrap();
    assert_eq!(receipt_b.fragments.len(), 1);
    assert_ne!(
        receipt_b.fragments[0].session, registration_b.session,
        "restored handle is newly signed, never the source run id"
    );
    let request_b2 = admit(&mut ingress_restored_b, HINT, BODY);
    let foreign_fence = register(
        &mut runtime_restored_b,
        &ingress_restored_b,
        &request_b2,
        4,
        UserEvent::new(UserEventKind::History).with_id(HostEventId::new("host-a").unwrap()),
    );
    assert!(!foreign_fence.duplicate && !foreign_fence.renewed);
    let request_b3 = admit(&mut ingress_restored_b, HINT, BODY);
    let renewed = register(
        &mut runtime_restored_b,
        &ingress_restored_b,
        &request_b3,
        5,
        UserEvent::new(UserEventKind::New).with_source_order(50),
    );
    assert!(renewed.renewed);
}

#[test]
fn revoked_without_maintenance_exports_only_fences() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    let registration = register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New)
            .with_id(HostEventId::new("host-r").unwrap())
            .with_source_order(70),
    );
    runtime_a
        .revoke_session(
            SessionAccess::new(&ingress_a, &request_a, 1),
            &registration.session,
        )
        .unwrap();
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 2), 2)
        .unwrap();
    assert_eq!(snapshot.fragment_count(), 0);

    assert_eq!(
        runtime_a
            .install_management_capability(
                &ingress_a,
                request_a.identity().clone(),
                request_a.isolation().clone(),
                vec![registration.session.clone()],
                ManagementPermissions::default().with_view(),
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    let receipt = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 3),
            &grant,
            &snapshot,
        )
        .unwrap();
    assert!(receipt.fragments.is_empty());
    assert!(!receipt.replay_protection_exhausted);
    assert_eq!(
        runtime_b
            .register_user(
                SessionAccess::new(&ingress_b, &request_b, 4),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-r").unwrap()),
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    let suppressed = register(
        &mut runtime_b,
        &ingress_b,
        &request_b,
        5,
        UserEvent::new(UserEventKind::New).with_source_order(50),
    );
    assert!(!suppressed.renewed);
    assert_eq!(
        runtime_b
            .install_management_capability(
                &ingress_b,
                request_b.identity().clone(),
                request_b.isolation().clone(),
                vec![registration.session.clone()],
                ManagementPermissions::default().with_view(),
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );
}

#[test]
fn same_local_numbers_across_runs_do_not_map_domains() {
    let mut ingress_a = ingress_with_bound_credentials(&[(TOKEN, "stable-alpha")]);
    let mut runtime_a = SessionRuntime::with_retention(
        &ingress_a,
        limits(),
        SessionTiming::default(),
        retention(1_000_000, 1_000_000),
        0,
    );
    let request_a = admit(&mut ingress_a, HINT, BODY);
    let local_number = request_a.credential().value();
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-x").unwrap()),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 1), 1)
        .unwrap();

    let mut ingress_b = ingress_with_bound_credentials(&[(TOKEN, "stable-beta")]);
    let mut runtime_b = SessionRuntime::with_retention(
        &ingress_b,
        limits(),
        SessionTiming::default(),
        retention(1_000_000, 1_000_000),
        0,
    );
    let request_b = admit(&mut ingress_b, HINT, BODY);
    assert_eq!(local_number, request_b.credential().value());
    let grant_b = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    assert_eq!(
        runtime_b
            .import_raw_snapshot(
                SessionAccess::new(&ingress_b, &request_b, 2),
                &grant_b,
                &snapshot,
            )
            .unwrap_err(),
        GatewayError::ForeignRequest
    );

    install_credential(&mut ingress_b, "token-rotated", "stable-alpha");
    let request_rotated = admit_token(&mut ingress_b, "token-rotated", HINT, BODY);
    let grant_alpha = grant_for(&mut ingress_b, &mut runtime_b, &request_rotated);
    let receipt = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_rotated, 3),
            &grant_alpha,
            &snapshot,
        )
        .unwrap();
    assert!(!receipt.duplicate);
    assert_eq!(receipt.fragments.len(), 1);
}

struct WireEvent {
    role: u8,
    body: &'static str,
    received: u64,
    parent: u32,
    host_id: Option<&'static str>,
    terminal: u8,
    acknowledgement: u8,
}

impl WireEvent {
    fn history(body: &'static str) -> Self {
        Self {
            role: 2,
            body,
            received: 0,
            parent: 0,
            host_id: None,
            terminal: 3,
            acknowledgement: 0,
        }
    }
}

struct WireFragment {
    key: u64,
    zone: u8,
    created: u64,
    normal: u64,
    hard: u64,
    final_deadline: Option<u64>,
    version: u64,
    processed: u32,
    events: Vec<WireEvent>,
}

fn put_wire_str(content: &mut Vec<u8>, value: &str) {
    content.extend_from_slice(&(value.len() as u32).to_le_bytes());
    content.extend_from_slice(value.as_bytes());
}

fn wire_snapshot(
    committed: u64,
    revision: u64,
    exhausted: bool,
    revoked: &[&str],
    revoked_keys: &[u64],
    watermark: Option<u64>,
    fragments: &[WireFragment],
) -> Vec<u8> {
    let mut content = Vec::new();
    put_wire_str(&mut content, IDENTITY_WORKSPACE);
    put_wire_str(&mut content, IDENTITY_USER);
    put_wire_str(&mut content, IDENTITY_INSTANCE);
    put_wire_str(&mut content, SUBJECT);
    put_wire_str(&mut content, CHANNEL);
    put_wire_str(&mut content, SCOPE);
    put_wire_str(&mut content, ATTRIBUTION);
    put_wire_str(&mut content, HINT);
    content.extend_from_slice(&revision.to_le_bytes());
    content.push(u8::from(exhausted));
    content.extend_from_slice(&(revoked.len() as u32).to_le_bytes());
    for id in revoked {
        put_wire_str(&mut content, id);
    }
    content.extend_from_slice(&(revoked_keys.len() as u32).to_le_bytes());
    for key in revoked_keys {
        content.extend_from_slice(&key.to_le_bytes());
    }
    match watermark {
        None => content.push(0),
        Some(order) => {
            content.push(1);
            content.extend_from_slice(&order.to_le_bytes());
        }
    }
    content.extend_from_slice(&(fragments.len() as u32).to_le_bytes());
    for fragment in fragments {
        content.extend_from_slice(&fragment.key.to_le_bytes());
        content.push(fragment.zone);
        content.extend_from_slice(&fragment.created.to_le_bytes());
        content.extend_from_slice(&fragment.normal.to_le_bytes());
        content.extend_from_slice(&fragment.hard.to_le_bytes());
        match fragment.final_deadline {
            None => content.push(0),
            Some(deadline) => {
                content.push(1);
                content.extend_from_slice(&deadline.to_le_bytes());
            }
        }
        content.extend_from_slice(&fragment.version.to_le_bytes());
        content.extend_from_slice(&fragment.processed.to_le_bytes());
        content.extend_from_slice(&(fragment.events.len() as u32).to_le_bytes());
        for event in &fragment.events {
            content.push(event.role);
            put_wire_str(&mut content, event.body);
            content.extend_from_slice(&event.received.to_le_bytes());
            content.extend_from_slice(&event.parent.to_le_bytes());
            content.push(0);
            content.push(0);
            content.push(0);
            match event.host_id {
                None => content.push(0),
                Some(id) => {
                    content.push(1);
                    put_wire_str(&mut content, id);
                }
            }
            content.push(event.terminal);
            content.push(event.acknowledgement);
        }
    }
    let mut bytes = Vec::with_capacity(content.len() + 24);
    bytes.extend_from_slice(b"MOERAW1\0");
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&committed.to_le_bytes());
    bytes.extend_from_slice(&content);
    let checksum = fnv1a(&bytes[16..]);
    bytes[12..16].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn craft_snapshot(
    committed: u64,
    revision: u64,
    exhausted: bool,
    revoked: &[&str],
    revoked_keys: &[u64],
    watermark: Option<u64>,
    fragments: Vec<WireFragment>,
) -> RawSnapshotV1 {
    RawSnapshotV1::decode(&wire_snapshot(
        committed,
        revision,
        exhausted,
        revoked,
        revoked_keys,
        watermark,
        &fragments,
    ))
    .unwrap()
}

fn wire_fragment(
    key: u64,
    created: u64,
    normal: u64,
    hard: u64,
    body: &'static str,
) -> WireFragment {
    let mut fragment = WireFragment {
        key,
        zone: 0,
        created,
        normal,
        hard,
        final_deadline: None,
        version: 0,
        processed: 0,
        events: vec![WireEvent::history(body)],
    };
    fragment.events[0].received = created;
    fragment
}

#[test]
fn sweep_expired_enables_partial_import_and_full_expiry_fences() {
    let valid_body = "仍有效正文";
    let recycling_body = "到期回收正文";
    let expired_body = "过期全文";
    let mut fragments = vec![
        {
            let mut fragment = wire_fragment(1, 0, 100, 2_000, expired_body);
            fragment.events[0].host_id = Some("host-1");
            fragment
        },
        {
            let mut fragment = wire_fragment(2, 1_500, 2_000, 3_000, recycling_body);
            fragment.events[0].host_id = Some("host-2");
            fragment
        },
        {
            let mut fragment = wire_fragment(3, 1_800, 2_000_000, 3_000_000, valid_body);
            fragment.events[0].host_id = Some("host-3");
            fragment
        },
    ];
    fragments[2].version = 7;
    let snapshot = craft_snapshot(2_700, 1, false, &[], &[], None, fragments);
    let policy = retention(1_000, 1_000);

    let (mut ingress, mut runtime) = fixture(retention(1_000, 1_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let grant = grant_for(&mut ingress, &mut runtime, &request);
    assert_eq!(
        runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request, 2_700),
                &grant,
                &snapshot
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );

    let swept = snapshot.sweep_expired(2_700, &policy).unwrap();
    assert_eq!(
        (swept.removed, swept.transferred, swept.retained),
        (1, 1, 1)
    );
    assert_eq!(snapshot.fragment_count(), 3);
    let purified = &swept.snapshot;
    assert_eq!(purified.fragment_count(), 2);
    let purified_decoded = RawSnapshotV1::decode(&purified.encode().unwrap()).unwrap();
    assert_eq!(*purified, purified_decoded);

    let receipt = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 2_700),
            &grant,
            &purified_decoded,
        )
        .unwrap();
    assert!(!receipt.duplicate);
    assert_eq!(
        receipt
            .fragments
            .iter()
            .map(|fragment| (fragment.key, fragment.zone, fragment.final_deadline_ms))
            .collect::<Vec<_>>(),
        vec![
            (2, RetentionZone::Recycle, Some(3_000)),
            (3, RetentionZone::Normal, None),
        ],
    );
    assert_eq!(
        receipt
            .fragments
            .iter()
            .find(|fragment| fragment.key == 3)
            .map(|fragment| fragment.normal_deadline_ms),
        Some(2_800)
    );
    assert_eq!(
        runtime
            .register_user(
                SessionAccess::new(&ingress, &request, 2_800),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-1").unwrap()),
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    for entry in &receipt.fragments {
        let capability = install_view(&mut runtime, &ingress, &request, &entry.session);
        let view = runtime
            .inspect_fragment(
                SessionAccess::new(&ingress, &request, 2_800),
                &capability,
                &entry.session,
            )
            .unwrap();
        assert_eq!(
            view.events[0].body(),
            if entry.key == 2 {
                recycling_body
            } else {
                valid_body
            }
        );
    }

    let swept_final = snapshot.sweep_expired(4_000, &policy).unwrap();
    assert_eq!(
        (
            swept_final.removed,
            swept_final.transferred,
            swept_final.retained
        ),
        (3, 0, 0)
    );
    let fences_only = RawSnapshotV1::decode(&swept_final.snapshot.encode().unwrap()).unwrap();
    let (mut ingress_all, mut runtime_all) = fixture(retention(1_000, 1_000), limits(), 0);
    let request_all = admit(&mut ingress_all, HINT, BODY);
    let grant_all = grant_for(&mut ingress_all, &mut runtime_all, &request_all);
    let receipt_all = runtime_all
        .import_raw_snapshot(
            SessionAccess::new(&ingress_all, &request_all, 4_000),
            &grant_all,
            &fences_only,
        )
        .unwrap();
    assert!(receipt_all.fragments.is_empty());
    assert_eq!(
        runtime_all
            .register_user(
                SessionAccess::new(&ingress_all, &request_all, 4_100),
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new("host-3").unwrap()),
            )
            .unwrap_err(),
        GatewayError::Revoked
    );

    assert_eq!(
        snapshot.sweep_expired(2_699, &policy).unwrap_err(),
        GatewayError::ClockWentBackwards
    );
    assert_eq!(snapshot.fragment_count(), 3);

    let untouched_archive = craft_snapshot(
        0,
        1,
        false,
        &[],
        &[],
        None,
        vec![wire_fragment(3, 0, 2_000_000, 3_000_000, valid_body)],
    );
    let untouched = untouched_archive.sweep_expired(0, &policy).unwrap();
    assert_eq!(
        (untouched.removed, untouched.transferred, untouched.retained),
        (0, 0, 1)
    );
    assert_eq!(
        untouched.snapshot.encode().unwrap(),
        craft_snapshot(
            0,
            1,
            false,
            &[],
            &[],
            None,
            vec![wire_fragment(3, 0, 1_000, 2_000, valid_body)],
        )
        .encode()
        .unwrap()
    );
}

#[test]
fn time_and_arithmetic_extremes_fail_closed() {
    let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let grant = grant_for(&mut ingress, &mut runtime, &request);

    let far_snapshot = runtime
        .export_raw_snapshot(SessionAccess::new(&ingress, &request, 0), u64::MAX)
        .unwrap();
    assert_eq!(far_snapshot.committed_at_ms(), u64::MAX);
    assert_eq!(
        runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request, 0),
                &grant,
                &far_snapshot
            )
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );

    let committed_max = craft_snapshot(
        u64::MAX,
        1,
        false,
        &[],
        &[],
        None,
        vec![wire_fragment(1, 0, 10_000_000, 20_000_000, "极端正文")],
    );
    assert_eq!(
        runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request, 1),
                &grant,
                &committed_max
            )
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );

    let created_overflow = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        &[wire_fragment(1, u64::MAX, 10, 20, "极端正文")],
    );
    assert_eq!(
        RawSnapshotV1::decode(&created_overflow).unwrap_err(),
        GatewayError::RawSnapshotInvalid("fragment created after commit time")
    );
    let normal_after_hard = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        &[wire_fragment(1, 0, 30, 20, "极端正文")],
    );
    assert_eq!(
        RawSnapshotV1::decode(&normal_after_hard).unwrap_err(),
        GatewayError::RawSnapshotInvalid("normal deadline after hard deadline")
    );
    let mut final_after_hard = wire_fragment(1, 0, 10, 20, "极端正文");
    final_after_hard.final_deadline = Some(50);
    let final_after_hard = wire_snapshot(100, 1, false, &[], &[], None, &[final_after_hard]);
    assert_eq!(
        RawSnapshotV1::decode(&final_after_hard).unwrap_err(),
        GatewayError::RawSnapshotInvalid("final deadline after hard deadline")
    );
    let mut processed_beyond = wire_fragment(1, 0, 10_000_000, 20_000_000, "极端正文");
    processed_beyond.processed = 2;
    let processed_beyond = wire_snapshot(100, 1, false, &[], &[], None, &[processed_beyond]);
    assert_eq!(
        RawSnapshotV1::decode(&processed_beyond).unwrap_err(),
        GatewayError::RawSnapshotInvalid("processed watermark beyond events")
    );
    let mut unknown_zone = wire_fragment(1, 0, 10_000_000, 20_000_000, "极端正文");
    unknown_zone.zone = 7;
    assert_eq!(
        RawSnapshotV1::decode(&wire_snapshot(
            100,
            1,
            false,
            &[],
            &[],
            None,
            &[unknown_zone]
        ))
        .unwrap_err(),
        GatewayError::RawSnapshotInvalid("unknown retention zone")
    );

    let huge = RawRetention::new(u64::MAX, u64::MAX, 1, 1, 1).unwrap();
    let extreme = craft_snapshot(
        300,
        1,
        false,
        &[],
        &[],
        None,
        vec![wire_fragment(1, 10, 100, 200, "极端正文")],
    );
    assert_eq!(
        extreme.sweep_expired(150, &huge).unwrap_err(),
        GatewayError::ClockWentBackwards
    );
    assert_eq!(
        extreme.sweep_expired(350, &huge).unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert_eq!(
        extreme.sweep_expired(u64::MAX, &huge).unwrap_err(),
        GatewayError::IdentifierExhausted
    );
    assert_eq!(extreme.fragment_count(), 1);
    let swept_final = extreme
        .sweep_expired(u64::MAX, &retention(1_000, 1_000))
        .unwrap();
    assert_eq!(
        (
            swept_final.removed,
            swept_final.transferred,
            swept_final.retained
        ),
        (1, 0, 0)
    );
    let mut max_version = wire_fragment(1, 10, 100, 200, "极端正文");
    max_version.version = u64::MAX;
    let max_version = craft_snapshot(300, 1, false, &[], &[], None, vec![max_version]);
    assert_eq!(
        max_version.sweep_expired(350, &huge).unwrap_err(),
        GatewayError::IdentifierExhausted
    );

    let max_key = craft_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        vec![wire_fragment(
            u64::MAX,
            0,
            10_000_000,
            20_000_000,
            "极端正文",
        )],
    );
    let receipt = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 100),
            &grant,
            &max_key,
        )
        .unwrap();
    assert_eq!(receipt.fragments[0].key, u64::MAX);
    register(
        &mut runtime,
        &ingress,
        &request,
        101,
        UserEvent::new(UserEventKind::New),
    );
    let exported = runtime
        .export_raw_snapshot(SessionAccess::new(&ingress, &request, 102), 102)
        .unwrap();
    assert_eq!(exported.fragment_count(), 2);
    assert_eq!(
        exported.encode().unwrap(),
        runtime
            .export_raw_snapshot(SessionAccess::new(&ingress, &request, 102), 102)
            .unwrap()
            .encode()
            .unwrap()
    );
    let (mut ingress_r, mut runtime_r) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_r = admit(&mut ingress_r, HINT, BODY);
    let grant_r = grant_for(&mut ingress_r, &mut runtime_r, &request_r);
    let receipt_r = runtime_r
        .import_raw_snapshot(
            SessionAccess::new(&ingress_r, &request_r, 103),
            &grant_r,
            &exported,
        )
        .unwrap();
    assert_eq!(
        receipt_r
            .fragments
            .iter()
            .map(|fragment| fragment.key)
            .collect::<Vec<_>>(),
        vec![u64::MAX, 1]
    );
}

#[test]
fn late_import_failure_leaves_no_partial_state() {
    let bad_snapshot = craft_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        vec![
            wire_fragment(5, 0, 5_000_000, 9_000_000, "存活片段"),
            wire_fragment(6, 0, 10, 20, "过期全文"),
        ],
    );
    let good_snapshot = craft_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        vec![wire_fragment(5, 0, 5_000_000, 9_000_000, "存活片段")],
    );

    let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let grant = grant_for(&mut ingress, &mut runtime, &request);

    assert_eq!(
        runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request, 100),
                &grant,
                &bad_snapshot
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
    assert_eq!(
        runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request, 100),
                &grant,
                &bad_snapshot
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
    let probe = runtime
        .export_raw_snapshot(SessionAccess::new(&ingress, &request, 100), 100)
        .unwrap();
    assert_eq!(probe.fragment_count(), 0);
    let receipt = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 101),
            &grant,
            &good_snapshot,
        )
        .unwrap();
    assert!(!receipt.duplicate);
    assert_eq!(receipt.fragments.len(), 1);
    assert_eq!(
        runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request, 102),
                &grant,
                &bad_snapshot
            )
            .unwrap_err(),
        GatewayError::VersionConflict
    );
}

#[test]
fn checksum_collision_is_not_treated_as_duplicate() {
    let build = |body: &str| {
        let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
        let request = admit(&mut ingress, HINT, body);
        register(
            &mut runtime,
            &ingress,
            &request,
            0,
            UserEvent::new(UserEventKind::New),
        );
        runtime
            .export_raw_snapshot(SessionAccess::new(&ingress, &request, 0), 0)
            .unwrap()
            .encode()
            .unwrap()
    };

    let probe_bytes = build("aaaa");
    let body_position = probe_bytes
        .windows(4)
        .position(|window| *window == *b"aaaa")
        .expect("body located exactly once");
    assert_eq!(
        probe_bytes
            .windows(4)
            .filter(|window| **window == *b"aaaa")
            .count(),
        1
    );
    let mut h_before = 0x811c_9dc5u32;
    for byte in &probe_bytes[16..body_position] {
        h_before ^= u32::from(*byte);
        h_before = h_before.wrapping_mul(0x0100_0193);
    }
    let mut buckets: std::collections::HashMap<u32, Vec<([u8; 3], u32)>> =
        std::collections::HashMap::new();
    for a in 0x21u8..=0x7e {
        for b in 0x21u8..=0x7e {
            for c in 0x21u8..=0x7e {
                let mut hash = h_before;
                for byte in [a, b, c] {
                    hash ^= u32::from(byte);
                    hash = hash.wrapping_mul(0x0100_0193);
                }
                buckets
                    .entry(hash >> 8)
                    .or_default()
                    .push(([a, b, c], hash));
            }
        }
    }
    let mut pairs: Vec<([u8; 3], [u8; 3], u8)> = Vec::new();
    for group in buckets.values() {
        for first in 0..group.len() {
            for second in first + 1..group.len() {
                let delta = group[first].1 ^ group[second].1;
                if delta != 0 {
                    pairs.push((group[first].0, group[second].0, delta as u8));
                }
            }
        }
    }
    let printable_fourth = |delta: u8| {
        (0x21u8..=0x7e).find_map(|first| {
            let second = first ^ delta;
            (0x21..=0x7e).contains(&second).then_some((first, second))
        })
    };
    let occurrences = |haystack: &[u8], needle: &[u8]| {
        haystack
            .windows(needle.len())
            .filter(|window| **window == *needle)
            .count()
    };
    let (x_prefix, y_prefix, delta) = pairs
        .iter()
        .find(|(x, y, delta)| {
            printable_fourth(*delta).is_some()
                && occurrences(&probe_bytes, x.as_slice()) == 0
                && occurrences(&probe_bytes, y.as_slice()) == 0
        })
        .copied()
        .expect("printable ASCII collision pair exists outside constant regions");
    let (x_last, y_last) =
        printable_fourth(delta).expect("fourth byte pair resolves within printable ASCII");
    let x = [x_prefix.as_slice(), &[x_last]].concat();
    let y = [y_prefix.as_slice(), &[y_last]].concat();
    let x_str = std::str::from_utf8(&x).unwrap().to_owned();
    let y_str = std::str::from_utf8(&y).unwrap().to_owned();
    assert_ne!(x_str, y_str);
    let x_str = x_str.as_str();
    let y_str = y_str.as_str();

    let bytes_a = build(x_str);
    let bytes_b = build(y_str);
    assert_ne!(bytes_a, bytes_b);
    assert_eq!(bytes_a[12..16], bytes_b[12..16]);
    assert_eq!(fnv1a(&bytes_a[16..]), fnv1a(&bytes_b[16..]));
    let snapshot_a = RawSnapshotV1::decode(&bytes_a).unwrap();
    let snapshot_b = RawSnapshotV1::decode(&bytes_b).unwrap();
    assert_ne!(snapshot_a, snapshot_b);

    let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let grant = grant_for(&mut ingress, &mut runtime, &request);
    runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 1),
            &grant,
            &snapshot_a,
        )
        .unwrap();
    assert_eq!(
        runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request, 2),
                &grant,
                &snapshot_b
            )
            .unwrap_err(),
        GatewayError::VersionConflict
    );
}

#[test]
fn exhausted_cleanup_blocks_first_import_with_or_without_host_ids() {
    for with_host_id in [false, true] {
        let drained = SessionLimits::new(8, 16, 1, 1_024, 8_192, 2).unwrap();
        let (mut ingress, mut runtime) = fixture(retention(1_000, 1_000), drained, 0);
        let cycles = if with_host_id { 1 } else { 2 };
        let mut stale = None;
        let mut latest_request = None;
        for cycle in 0..cycles {
            let now_ms = cycle * 3;
            let request = admit(&mut ingress, HINT, BODY);
            let mut event = UserEvent::new(UserEventKind::New).with_source_order(cycle);
            if with_host_id {
                event = event.with_id(HostEventId::new("exhausted-event").unwrap());
            }
            let registration = register(&mut runtime, &ingress, &request, now_ms, event);
            stale = Some(
                runtime
                    .export_raw_snapshot(
                        SessionAccess::new(&ingress, &request, now_ms + 1),
                        now_ms + 1,
                    )
                    .unwrap(),
            );
            runtime
                .revoke_session(
                    SessionAccess::new(&ingress, &request, now_ms + 2),
                    &registration.session,
                )
                .unwrap();
            latest_request = Some(request);
        }
        let request = latest_request.unwrap();
        let grant = grant_for(&mut ingress, &mut runtime, &request);
        let before = runtime
            .export_raw_snapshot(SessionAccess::new(&ingress, &request, 10), 10)
            .unwrap();
        assert_eq!(before.fragment_count(), 0);
        for _ in 0..2 {
            assert_eq!(
                runtime.import_raw_snapshot(
                    SessionAccess::new(&ingress, &request, 10),
                    &grant,
                    stale.as_ref().unwrap(),
                ),
                Err(GatewayError::ReplayProtectionExhausted)
            );
        }
        let after = runtime
            .export_raw_snapshot(SessionAccess::new(&ingress, &request, 10), 10)
            .unwrap();
        assert_eq!(before.encode().unwrap(), after.encode().unwrap());
        assert_eq!(
            runtime.register_user(
                SessionAccess::new(&ingress, &request, 10),
                UserEvent::new(UserEventKind::New),
            ),
            Err(GatewayError::ReplayProtectionExhausted)
        );
    }
}

#[test]
fn exhausted_restore_allows_exact_duplicates_but_no_new_fence_growth() {
    let exhausted = craft_snapshot(
        0,
        1,
        true,
        &[],
        &[],
        None,
        vec![wire_fragment(1, 0, 1_000, 3_000, BODY)],
    );
    let (mut ingress, mut runtime) = fixture(retention(1_000, 1_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let grant = grant_for(&mut ingress, &mut runtime, &request);
    let receipt = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 1),
            &grant,
            &exhausted,
        )
        .unwrap();
    assert!(receipt.replay_protection_exhausted);
    assert!(!receipt.duplicate);
    assert_eq!(receipt.fragments.len(), 1);
    let duplicate = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 1),
            &grant,
            &exhausted,
        )
        .unwrap();
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.fragments[0].session, receipt.fragments[0].session);
    runtime
        .revoke_session(
            SessionAccess::new(&ingress, &request, 2),
            &receipt.fragments[0].session,
        )
        .unwrap();
    let duplicate = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 2),
            &grant,
            &exhausted,
        )
        .unwrap();
    assert!(duplicate.duplicate);
    assert!(duplicate.replay_protection_exhausted);
    assert!(duplicate.fragments.is_empty(), "重复回执不得重建已清理正文");

    let other_hint = "exhausted-other-domain";
    let (mut source_ingress, mut source_runtime) = fixture(retention(1_000, 1_000), limits(), 0);
    let source_request = admit(&mut source_ingress, other_hint, BODY);
    let registered = register(
        &mut source_runtime,
        &source_ingress,
        &source_request,
        0,
        UserEvent::new(UserEventKind::New)
            .with_id(HostEventId::new("new-fence").unwrap())
            .with_source_order(99),
    );
    source_runtime
        .export_raw_snapshot(SessionAccess::new(&source_ingress, &source_request, 1), 1)
        .unwrap();
    source_runtime
        .revoke_session(
            SessionAccess::new(&source_ingress, &source_request, 2),
            &registered.session,
        )
        .unwrap();
    let fences = source_runtime
        .export_raw_snapshot(SessionAccess::new(&source_ingress, &source_request, 3), 3)
        .unwrap();
    let other_request = admit(&mut ingress, other_hint, BODY);
    let other_grant = grant_for(&mut ingress, &mut runtime, &other_request);
    let before = runtime
        .export_raw_snapshot(SessionAccess::new(&ingress, &other_request, 3), 3)
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            runtime.import_raw_snapshot(
                SessionAccess::new(&ingress, &other_request, 3),
                &other_grant,
                &fences,
            ),
            Err(GatewayError::ReplayProtectionExhausted)
        );
    }
    let after = runtime
        .export_raw_snapshot(SessionAccess::new(&ingress, &other_request, 3), 3)
        .unwrap();
    assert_eq!(
        before.encode().unwrap(),
        after.encode().unwrap(),
        "失败不得增长栅栏"
    );
    let duplicate = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 3),
            &grant,
            &exhausted,
        )
        .unwrap();
    assert!(duplicate.duplicate);
    assert!(duplicate.fragments.is_empty());
}

#[test]
fn export_commits_global_fragment_sequence_only_for_new_assignments() {
    let (mut ingress, mut runtime) = fixture(retention(1_000, 1_000), limits(), 0);
    for (hint, expected_key) in [("sequence-a", 1), ("sequence-b", 2)] {
        let request = admit(&mut ingress, hint, BODY);
        register(
            &mut runtime,
            &ingress,
            &request,
            0,
            UserEvent::new(UserEventKind::New),
        );
        let snapshot = runtime
            .export_raw_snapshot(SessionAccess::new(&ingress, &request, 0), 0)
            .unwrap();
        let repeated = runtime
            .export_raw_snapshot(SessionAccess::new(&ingress, &request, 0), 0)
            .unwrap();
        assert_eq!(snapshot.encode().unwrap(), repeated.encode().unwrap());
        let (mut reader_ingress, mut reader_runtime) =
            fixture(retention(1_000, 1_000), limits(), 0);
        let reader_request = admit(&mut reader_ingress, hint, BODY);
        let grant = grant_for(&mut reader_ingress, &mut reader_runtime, &reader_request);
        let receipt = reader_runtime
            .import_raw_snapshot(
                SessionAccess::new(&reader_ingress, &reader_request, 0),
                &grant,
                &snapshot,
            )
            .unwrap();
        assert_eq!(receipt.fragments[0].key, expected_key);
    }
}

#[test]
fn revoked_fragment_key_fence_blocks_stale_snapshot_revival() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    let registration = register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New).with_source_order(100),
    );
    let stale_snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 1), 1)
        .unwrap();
    runtime_a
        .revoke_session(
            SessionAccess::new(&ingress_a, &request_a, 2),
            &registration.session,
        )
        .unwrap();
    let fences = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 3), 3)
        .unwrap();
    assert_eq!(fences.fragment_count(), 0);

    let grant_a = grant_for(&mut ingress_a, &mut runtime_a, &request_a);
    assert_eq!(
        runtime_a
            .import_raw_snapshot(
                SessionAccess::new(&ingress_a, &request_a, 4),
                &grant_a,
                &stale_snapshot,
            )
            .unwrap_err(),
        GatewayError::InvalidInput(
            "snapshot fragment key conflicts with an existing revocation fence"
        )
    );

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant_b = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    let receipt = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 3),
            &grant_b,
            &fences,
        )
        .unwrap();
    assert!(receipt.fragments.is_empty());
    let suppressed = register(
        &mut runtime_b,
        &ingress_b,
        &request_b,
        4,
        UserEvent::new(UserEventKind::New).with_source_order(50),
    );
    assert!(!suppressed.renewed);
    assert_eq!(
        runtime_b
            .import_raw_snapshot(
                SessionAccess::new(&ingress_b, &request_b, 5),
                &grant_b,
                &stale_snapshot,
            )
            .unwrap_err(),
        GatewayError::VersionConflict
    );
}

#[test]
fn tightened_policy_blocks_past_logical_final_immediately() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(50, 50), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let snapshot = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 0), 0)
        .unwrap();

    let build = |now: u64| {
        let (mut ingress, mut runtime) = fixture(retention(100, 10), limits(), 0);
        let request = admit(&mut ingress, HINT, BODY);
        let grant = grant_for(&mut ingress, &mut runtime, &request);
        let result = runtime.import_raw_snapshot(
            SessionAccess::new(&ingress, &request, now),
            &grant,
            &snapshot,
        );
        (ingress, runtime, request, result)
    };
    let (_, _, _, late) = build(65);
    assert_eq!(late.unwrap_err(), GatewayError::RawExpired);

    let (ingress_ok, mut runtime_ok, request_ok, ok) = build(55);
    let receipt = ok.unwrap();
    assert_eq!(receipt.fragments[0].normal_deadline_ms, 50);
    let capability = install_view(
        &mut runtime_ok,
        &ingress_ok,
        &request_ok,
        &receipt.fragments[0].session,
    );
    let view = runtime_ok
        .inspect_fragment(
            SessionAccess::new(&ingress_ok, &request_ok, 55),
            &capability,
            &receipt.fragments[0].session,
        )
        .unwrap();
    assert!(view.logically_expired);
    assert_eq!(
        runtime_ok
            .inspect_fragment(
                SessionAccess::new(&ingress_ok, &request_ok, 60),
                &capability,
                &receipt.fragments[0].session,
            )
            .unwrap_err(),
        GatewayError::RawExpired
    );
    assert_eq!(
        runtime_ok
            .export_raw_snapshot(SessionAccess::new(&ingress_ok, &request_ok, 61), 61)
            .unwrap_err(),
        GatewayError::RawExpired
    );
    let report = runtime_ok.maintain_retention(&ingress_ok, 65).unwrap();
    assert_eq!(
        (report.removed, report.transferred, report.pending_transfer),
        (1, 0, 0)
    );

    let policy = retention(100, 10);
    let swept = snapshot.sweep_expired(55, &policy).unwrap();
    assert_eq!(
        (swept.removed, swept.transferred, swept.retained),
        (0, 1, 0)
    );
    let (mut ingress_s, mut runtime_s) = fixture(retention(100, 10), limits(), 0);
    let request_s = admit(&mut ingress_s, HINT, BODY);
    let grant_s = grant_for(&mut ingress_s, &mut runtime_s, &request_s);
    let swept_receipt = runtime_s
        .import_raw_snapshot(
            SessionAccess::new(&ingress_s, &request_s, 55),
            &grant_s,
            &RawSnapshotV1::decode(&swept.snapshot.encode().unwrap()).unwrap(),
        )
        .unwrap();
    assert_eq!(swept_receipt.fragments[0].zone, RetentionZone::Recycle);
    assert_eq!(swept_receipt.fragments[0].final_deadline_ms, Some(60));
    let swept_late = snapshot.sweep_expired(65, &policy).unwrap();
    assert_eq!(
        (
            swept_late.removed,
            swept_late.transferred,
            swept_late.retained
        ),
        (1, 0, 0)
    );
}

#[test]
fn semantic_corruption_rejected_before_publication() {
    let recycle_without_final = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        &[{
            let mut fragment = wire_fragment(1, 0, 10_000_000, 20_000_000, "回收正文");
            fragment.zone = 1;
            fragment
        }],
    );
    assert_eq!(
        RawSnapshotV1::decode(&recycle_without_final).unwrap_err(),
        GatewayError::RawSnapshotInvalid("recycled fragment has no fixed final deadline")
    );
    let normal_before_creation = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        &[wire_fragment(1, 10, 5, 20, "倒挂正文")],
    );
    assert_eq!(
        RawSnapshotV1::decode(&normal_before_creation).unwrap_err(),
        GatewayError::RawSnapshotInvalid("normal deadline before fragment creation")
    );
    let event_before_creation = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        &[{
            let mut fragment = wire_fragment(1, 10, 20, 30, "早到正文");
            fragment.events[0].received = 5;
            fragment
        }],
    );
    assert_eq!(
        RawSnapshotV1::decode(&event_before_creation).unwrap_err(),
        GatewayError::RawSnapshotInvalid("event received before fragment creation")
    );
    let final_before_creation = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        &[{
            let mut fragment = wire_fragment(1, 10, 20, 30, "倒挂最终截止");
            fragment.final_deadline = Some(5);
            fragment
        }],
    );
    assert_eq!(
        RawSnapshotV1::decode(&final_before_creation).unwrap_err(),
        GatewayError::RawSnapshotInvalid("final deadline before fragment creation")
    );
    let key_both_live_and_revoked = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[5],
        None,
        &[wire_fragment(5, 0, 10_000_000, 20_000_000, "双重身份")],
    );
    assert_eq!(
        RawSnapshotV1::decode(&key_both_live_and_revoked).unwrap_err(),
        GatewayError::RawSnapshotInvalid("fragment key is both live and revoked")
    );

    let mut recycled = wire_fragment(9, 0, 500, 1_500, "合法回收正文");
    recycled.zone = 1;
    recycled.final_deadline = Some(1_200);
    let recycled_snapshot = craft_snapshot(100, 1, false, &[], &[], None, vec![recycled]);
    let (mut ingress, mut runtime) = fixture(retention(1_000, 1_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let grant = grant_for(&mut ingress, &mut runtime, &request);
    let receipt = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 100),
            &grant,
            &recycled_snapshot,
        )
        .unwrap();
    assert_eq!(receipt.fragments[0].zone, RetentionZone::Recycle);
    let report = runtime.maintain_retention(&ingress, 1_200).unwrap();
    assert_eq!(report.removed, 1);
}

#[test]
fn confirmation_prefix_parent_and_role_consistency_validated() {
    let mut prefix_gap = wire_fragment(1, 0, 10_000_000, 20_000_000, "已确认正文");
    prefix_gap.processed = 2;
    prefix_gap.events = vec![
        WireEvent::history("已确认正文"),
        WireEvent::history("未确认正文"),
    ];
    prefix_gap.events[1].acknowledgement = 0;
    let prefix_gap = wire_snapshot(100, 1, false, &[], &[], None, &[prefix_gap]);
    assert_eq!(
        RawSnapshotV1::decode(&prefix_gap).unwrap_err(),
        GatewayError::RawSnapshotInvalid("confirmed prefix contains an unacknowledged event")
    );

    let user_with_parent = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        &[{
            let mut fragment = wire_fragment(1, 0, 10_000_000, 20_000_000, "用户正文");
            fragment.events = vec![
                WireEvent::history("用户正文"),
                WireEvent::history("带父的用户事件"),
            ];
            fragment.events[1].parent = 1;
            fragment
        }],
    );
    assert_eq!(
        RawSnapshotV1::decode(&user_with_parent).unwrap_err(),
        GatewayError::RawSnapshotInvalid("user event carries a parent event")
    );

    let related_without_parent = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        &[{
            let mut fragment = wire_fragment(1, 0, 10_000_000, 20_000_000, "用户正文");
            fragment.events = vec![{
                let mut event = WireEvent::history("无父相关事件");
                event.role = 3;
                event
            }];
            fragment
        }],
    );
    assert_eq!(
        RawSnapshotV1::decode(&related_without_parent).unwrap_err(),
        GatewayError::RawSnapshotInvalid("non-user event has no parent event")
    );

    let nested_related_parent = wire_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        &[{
            let mut fragment = wire_fragment(1, 0, 10_000_000, 20_000_000, "用户正文");
            fragment.events = vec![
                WireEvent::history("用户正文"),
                {
                    let mut event = WireEvent::history("一级相关事件");
                    event.role = 3;
                    event.parent = 1;
                    event
                },
                {
                    let mut event = WireEvent::history("挂到相关事件上的孙事件");
                    event.role = 5;
                    event.parent = 2;
                    event
                },
            ];
            fragment
        }],
    );
    assert_eq!(
        RawSnapshotV1::decode(&nested_related_parent).unwrap_err(),
        GatewayError::RawSnapshotInvalid("event parent is not a user event")
    );

    let mut consistent = wire_fragment(1, 0, 10_000_000, 20_000_000, "用户正文");
    consistent.processed = 2;
    consistent.events = vec![
        {
            let mut event = WireEvent::history("用户正文");
            event.acknowledgement = 1;
            event
        },
        {
            let mut event = WireEvent::history("确认的相关事件");
            event.role = 5;
            event.parent = 1;
            event.acknowledgement = 1;
            event
        },
    ];
    let consistent = craft_snapshot(100, 1, false, &[], &[], None, vec![consistent]);
    let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let grant = grant_for(&mut ingress, &mut runtime, &request);
    let receipt = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request, 100),
            &grant,
            &consistent,
        )
        .unwrap();
    let capability = install_view(
        &mut runtime,
        &ingress,
        &request,
        &receipt.fragments[0].session,
    );
    let view = runtime
        .inspect_fragment(
            SessionAccess::new(&ingress, &request, 101),
            &capability,
            &receipt.fragments[0].session,
        )
        .unwrap();
    assert_eq!(view.processed_through, 2);
    assert_eq!(
        view.events[1].role(),
        moe_agent_runtime::EventRole::ToolReceipt
    );
}

#[test]
fn fence_ledger_cumulative_budget_rejects_whole_import() {
    let drained = SessionLimits::new(8, 16, 2, 1_024, 8_192, 2).unwrap();
    let build_fences = |token: &str, attribution: &str, host_ids: &[&str]| {
        let mut ingress = ingress_with_bound_credentials(&[(token, attribution)]);
        let mut runtime = SessionRuntime::with_retention(
            &ingress,
            limits(),
            SessionTiming::default(),
            retention(1_000_000, 1_000_000),
            0,
        );
        let request = admit_token(&mut ingress, token, HINT, BODY);
        let mut registration = None;
        for (index, id) in host_ids.iter().enumerate() {
            let event_request = admit_token(&mut ingress, token, HINT, BODY);
            let current = register(
                &mut runtime,
                &ingress,
                &event_request,
                index as u64,
                UserEvent::new(UserEventKind::New).with_id(HostEventId::new(*id).unwrap()),
            );
            registration = Some(current);
        }
        let capability = runtime
            .install_management_capability(
                &ingress,
                request.identity().clone(),
                request.isolation().clone(),
                vec![registration.as_ref().unwrap().session.clone()],
                ManagementPermissions::default().with_purge(),
            )
            .unwrap();
        let now = host_ids.len() as u64 + 1;
        runtime
            .purge_fragment(
                SessionAccess::new(&ingress, &request, now),
                &capability,
                &registration.as_ref().unwrap().session,
                0,
            )
            .unwrap();
        runtime
            .export_raw_snapshot(SessionAccess::new(&ingress, &request, now + 1), now + 1)
            .unwrap()
    };
    let fences_a = build_fences("token-src-a", "cred-src-a", &["fence-a1", "fence-a2"]);
    let fences_b = build_fences("token-src-b", "cred-src-b", &["fence-b1"]);

    let mut ingress = ingress_with_bound_credentials(&[
        ("token-dst-a", "cred-src-a"),
        ("token-dst-b", "cred-src-b"),
    ]);
    let mut runtime = SessionRuntime::with_retention(
        &ingress,
        drained,
        SessionTiming::default(),
        retention(1_000_000, 1_000_000),
        0,
    );
    let request_a = admit_token(&mut ingress, "token-dst-a", HINT, BODY);
    let grant_a = grant_for(&mut ingress, &mut runtime, &request_a);
    let request_b = admit_token(&mut ingress, "token-dst-b", HINT, BODY);
    let grant_b = grant_for(&mut ingress, &mut runtime, &request_b);

    runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request_a, 10),
            &grant_a,
            &fences_a,
        )
        .unwrap();
    assert_eq!(
        runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request_b, 11),
                &grant_b,
                &fences_b,
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded
    );
    assert_eq!(
        runtime
            .import_raw_snapshot(
                SessionAccess::new(&ingress, &request_b, 12),
                &grant_b,
                &fences_b,
            )
            .unwrap_err(),
        GatewayError::CapacityExceeded,
        "拒绝不得留下半记录把重试变成版本冲突"
    );
    let fresh = register(
        &mut runtime,
        &ingress,
        &request_b,
        13,
        UserEvent::new(UserEventKind::New),
    );
    assert!(fresh.renewed);
}

#[test]
fn archived_extreme_counters_do_not_poison_other_domains() {
    let mut extreme = wire_fragment(7, 0, 10_000_000, 20_000_000, "极端版本正文");
    extreme.version = u64::MAX;
    let extreme_snapshot = craft_snapshot(100, 1, false, &[], &[], None, vec![extreme]);
    let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress, HINT, BODY);
    let grant_a = grant_for(&mut ingress, &mut runtime, &request_a);
    let receipt = runtime
        .import_raw_snapshot(
            SessionAccess::new(&ingress, &request_a, 100),
            &grant_a,
            &extreme_snapshot,
        )
        .unwrap();

    let request_b = admit(&mut ingress, "hint-b", BODY);
    let registration_b = register(
        &mut runtime,
        &ingress,
        &request_b,
        101,
        UserEvent::new(UserEventKind::New),
    );
    let capability_b = runtime
        .install_management_capability(
            &ingress,
            request_b.identity().clone(),
            request_b.isolation().clone(),
            vec![registration_b.session.clone()],
            ManagementPermissions::default().with_soft_delete(),
        )
        .unwrap();
    let deleted = runtime
        .soft_delete_fragment(
            SessionAccess::new(&ingress, &request_b, 102),
            &capability_b,
            &registration_b.session,
            0,
        )
        .unwrap();
    assert_eq!(deleted.zone, RetentionZone::Recycle);

    let capability_a = runtime
        .install_management_capability(
            &ingress,
            request_a.identity().clone(),
            request_a.isolation().clone(),
            vec![receipt.fragments[0].session.clone()],
            ManagementPermissions::default().with_purge(),
        )
        .unwrap();
    runtime
        .purge_fragment(
            SessionAccess::new(&ingress, &request_a, 103),
            &capability_a,
            &receipt.fragments[0].session,
            u64::MAX,
        )
        .unwrap();
    assert_eq!(
        runtime
            .install_management_capability(
                &ingress,
                request_a.identity().clone(),
                request_a.isolation().clone(),
                vec![receipt.fragments[0].session.clone()],
                ManagementPermissions::default().with_view(),
            )
            .unwrap_err(),
        GatewayError::StaleHandle
    );
}

#[test]
fn export_encode_failure_commits_no_keys() {
    let wide = SessionLimits::new(8, 16, 64, 7_000_000, 30_000_000, 2).unwrap();
    let reply = "长".repeat(6_000_000);
    let build_small_export = |with_oversize_domain: bool| {
        let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), wide, 0);
        if with_oversize_domain {
            let request_a = admit(&mut ingress, "hint-a", BODY);
            let registration = register(
                &mut runtime,
                &ingress,
                &request_a,
                0,
                UserEvent::new(UserEventKind::New),
            );
            runtime
                .finish_interaction(
                    SessionAccess::new(&ingress, &request_a, 1),
                    &registration.interaction,
                    TerminalState::Completed,
                    Some(reply.as_str()),
                )
                .unwrap();
            assert_eq!(
                runtime
                    .export_raw_snapshot(SessionAccess::new(&ingress, &request_a, 2), 2)
                    .unwrap_err(),
                GatewayError::CapacityExceeded
            );
        }
        let request_b = admit(&mut ingress, "hint-b", BODY);
        register(
            &mut runtime,
            &ingress,
            &request_b,
            3,
            UserEvent::new(UserEventKind::New),
        );
        let snapshot = runtime
            .export_raw_snapshot(SessionAccess::new(&ingress, &request_b, 4), 4)
            .unwrap();
        snapshot.encode().unwrap()
    };
    let control = build_small_export(false);
    let after_failure = build_small_export(true);
    assert_eq!(control, after_failure);
}

#[test]
fn authorization_revision_binding_governs_cross_restart_import() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let revision_one = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 1), 1)
        .unwrap();

    ingress_a
        .bind_stable_credential_revision(request_a.credential(), ATTRIBUTION, 2)
        .unwrap();
    let revision_two = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 3), 3)
        .unwrap();

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant_b = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 1),
            &grant_b,
            &revision_one,
        )
        .unwrap();
    assert_eq!(
        runtime_b
            .import_raw_snapshot(
                SessionAccess::new(&ingress_b, &request_b, 2),
                &grant_b,
                &revision_two,
            )
            .unwrap_err(),
        GatewayError::Revoked
    );

    let mut ingress_c = ingress_with_bound_credentials(&[(TOKEN, ATTRIBUTION)]);
    let mut runtime_c = SessionRuntime::with_retention(
        &ingress_c,
        limits(),
        SessionTiming::default(),
        retention(1_000_000, 1_000_000),
        0,
    );
    let request_c = admit(&mut ingress_c, HINT, BODY);
    ingress_c
        .bind_stable_credential_revision(request_c.credential(), ATTRIBUTION, 2)
        .unwrap();
    let grant_c = grant_for(&mut ingress_c, &mut runtime_c, &request_c);
    assert_eq!(
        runtime_c
            .import_raw_snapshot(
                SessionAccess::new(&ingress_c, &request_c, 1),
                &grant_c,
                &revision_one,
            )
            .unwrap_err(),
        GatewayError::Revoked
    );
    runtime_c
        .import_raw_snapshot(
            SessionAccess::new(&ingress_c, &request_c, 3),
            &grant_c,
            &revision_two,
        )
        .unwrap();

    ingress_c.revoke_credential(request_c.credential()).unwrap();
    let rotated = ingress_c
        .install_credential(
            IngressToken::new("token-rotated").unwrap(),
            CallerSubject::new(SUBJECT).unwrap(),
            [ALIAS.to_owned()],
            ChannelId::new(CHANNEL).unwrap(),
            ChannelScope::new(SCOPE).unwrap(),
            ReplyTarget::new(REPLY_TARGET).unwrap(),
        )
        .unwrap();
    assert_eq!(
        ingress_c
            .bind_stable_credential_revision(rotated, ATTRIBUTION, 1)
            .unwrap_err(),
        GatewayError::InvalidInput("stable credential revision was revoked and cannot be rebound")
    );
    ingress_c
        .bind_stable_credential(rotated, ATTRIBUTION)
        .unwrap();
    let request_rotated = admit_token(&mut ingress_c, "token-rotated", HINT, BODY);
    let grant_rotated = grant_for(&mut ingress_c, &mut runtime_c, &request_rotated);
    let receipt = runtime_c
        .import_raw_snapshot(
            SessionAccess::new(&ingress_c, &request_rotated, 4),
            &grant_rotated,
            &revision_two,
        )
        .unwrap();
    assert!(receipt.duplicate);
}

#[test]
fn raw_history_access_gate_serves_adapter_precheck() {
    let (mut ingress, mut runtime) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request = admit(&mut ingress, HINT, BODY);
    let domain = RawDomainKey::from_request(&ingress, &request).unwrap();
    let grant = runtime
        .install_raw_history_grant(&ingress, domain.clone())
        .unwrap();
    assert_eq!(
        runtime.validate_raw_history_access(
            SessionAccess::new(&ingress, &request, 0),
            &grant,
            &domain,
        ),
        Ok(())
    );
    runtime.revoke_raw_history_grant(&grant).unwrap();
    assert_eq!(
        runtime.validate_raw_history_access(
            SessionAccess::new(&ingress, &request, 1),
            &grant,
            &domain,
        ),
        Err(GatewayError::Revoked)
    );
    let foreign_domain = {
        let other = admit(&mut ingress, "other-hint", BODY);
        RawDomainKey::from_request(&ingress, &other).unwrap()
    };
    let grant_again = runtime
        .install_raw_history_grant(&ingress, foreign_domain.clone())
        .unwrap();
    assert_eq!(
        runtime.validate_raw_history_access(
            SessionAccess::new(&ingress, &request, 2),
            &grant_again,
            &foreign_domain,
        ),
        Err(GatewayError::ForeignRequest)
    );
    let mut plain_ingress = ingress_with_bound_credentials(&[(TOKEN, ATTRIBUTION)]);
    let plain_request = admit(&mut plain_ingress, HINT, BODY);
    let plain = SessionRuntime::new(&plain_ingress, limits(), SessionTiming::default(), 0);
    assert_eq!(
        plain.validate_raw_history_access(
            SessionAccess::new(&plain_ingress, &plain_request, 0),
            &grant_again,
            &foreign_domain,
        ),
        Err(GatewayError::RetentionNotConfigured)
    );
}

#[test]
fn commit_time_is_integrity_checked_and_governs_sweep() {
    let (mut ingress_a, mut runtime_a) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_a = admit(&mut ingress_a, HINT, BODY);
    register(
        &mut runtime_a,
        &ingress_a,
        &request_a,
        0,
        UserEvent::new(UserEventKind::New),
    );
    let bytes = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 0), 0)
        .unwrap()
        .encode()
        .unwrap();
    let mut tampered = bytes.clone();
    tampered[20] ^= 0xff;
    assert!(matches!(
        RawSnapshotV1::decode(&tampered),
        Err(GatewayError::RawSnapshotInvalid(_))
    ));

    let archive = craft_snapshot(
        100,
        1,
        false,
        &[],
        &[],
        None,
        vec![wire_fragment(1, 0, 10_000_000, 20_000_000, "净化正文")],
    );
    assert_eq!(
        archive
            .sweep_expired(99, &retention(1_000, 1_000))
            .unwrap_err(),
        GatewayError::ClockWentBackwards
    );
    let swept = archive
        .sweep_expired(100, &retention(1_000, 1_000))
        .unwrap();
    assert_eq!(
        (swept.removed, swept.transferred, swept.retained),
        (0, 0, 1)
    );

    let (mut ingress_b, mut runtime_b) = fixture(retention(1_000_000, 1_000_000), limits(), 0);
    let request_b = admit(&mut ingress_b, HINT, BODY);
    let grant_b = grant_for(&mut ingress_b, &mut runtime_b, &request_b);
    runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 1),
            &grant_b,
            &RawSnapshotV1::decode(&bytes).unwrap(),
        )
        .unwrap();
    let resealed = runtime_a
        .export_raw_snapshot(SessionAccess::new(&ingress_a, &request_a, 9), 9)
        .unwrap();
    let again = runtime_b
        .import_raw_snapshot(
            SessionAccess::new(&ingress_b, &request_b, 10),
            &grant_b,
            &resealed,
        )
        .unwrap();
    assert!(again.duplicate);
}
