use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_agent_runtime::{
    BatchAcknowledgement, CallerSubject, ChannelId, ChannelScope, GatewayError, HostEventId,
    IngressToken, RawDomainKey, RawIngressCall, RawRetention, SessionAccess, SessionLimits,
    SessionRuntime, SessionTiming, TerminalState, TrustedIngress, UserEvent, UserEventKind,
};
use moe_memory_core::{Namespace, PetIdentity};

use crate::{
    CommitDurability, CommitReceipt, CommitStage, FailurePoint, OpenMode,
    RAW_LOCAL_MAX_FRAME_BYTES, RawLocalAccess, RawLocalError, RawLocalStore,
};

const ALIAS: &str = "buddy";
const TOKEN: &str = "synthetic-local-token";
const ATTRIBUTION: &str = "stable-credential";
const BODY: &str = "synthetic-original-body";
const REPLY: &str = "synthetic-reply";
const SUBJECT: &str = "caller";
const CHANNEL: &str = "channel";
const SCOPE: &str = "scope";
const HINT: &str = "conversation";
const START_MS: u64 = 1_000;
const LONG_NORMAL_MS: u64 = 400_000;
const LONG_RECYCLE_MS: u64 = 400_000;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("moe-raw-local-{}-{sequence}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        if let Ok(gate) = crate::retirement_gate_path(&self.0) {
            let _ = fs::remove_file(gate);
        }
    }
}

struct Context {
    ingress: TrustedIngress,
    runtime: SessionRuntime,
    request: moe_agent_runtime::TrustedRequest,
    grant: moe_agent_runtime::RawHistoryGrant,
    retention: RawRetention,
    session: Option<moe_agent_runtime::SessionId>,
}

fn identity(owner: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new("workspace", owner).unwrap(), "pet-instance").unwrap()
}

fn make_context(
    now_ms: u64,
    normal_ms: u64,
    recycle_ms: u64,
    seed_history: bool,
    confirm_history: bool,
    owner: &str,
) -> Context {
    let mut ingress = TrustedIngress::new();
    ingress.register_route(ALIAS, identity(owner)).unwrap();
    let credential = ingress
        .install_credential(
            IngressToken::new(TOKEN).unwrap(),
            CallerSubject::new(SUBJECT).unwrap(),
            [ALIAS.to_owned()],
            ChannelId::new(CHANNEL).unwrap(),
            ChannelScope::new(SCOPE).unwrap(),
            moe_agent_runtime::ReplyTarget::new("reply-target").unwrap(),
        )
        .unwrap();
    ingress
        .bind_stable_credential(credential, ATTRIBUTION)
        .unwrap();
    let retention = RawRetention::new(normal_ms, recycle_ms, 8, 128, 8_192).unwrap();
    let mut runtime = SessionRuntime::with_retention(
        &ingress,
        SessionLimits::new(8, 16, 64, 1_024, 8_192, 2).unwrap(),
        SessionTiming::default(),
        retention,
        now_ms,
    );
    let request = ingress
        .admit(
            RawIngressCall::new(IngressToken::new(TOKEN).unwrap(), ALIAS, BODY)
                .with_session_hint(HINT),
        )
        .unwrap();
    let session = if seed_history {
        let registration = runtime
            .register_user(
                SessionAccess::new(&ingress, &request, START_MS),
                UserEvent::new(UserEventKind::New)
                    .with_id(HostEventId::new("event-1").unwrap())
                    .with_source_order(7)
                    .with_source_timestamp_ms(900),
            )
            .unwrap();
        if confirm_history {
            runtime
                .finish_interaction(
                    SessionAccess::new(&ingress, &request, START_MS + 1),
                    &registration.interaction,
                    TerminalState::Completed,
                    Some(REPLY),
                )
                .unwrap();
            let batch_now = START_MS + 180_000;
            let ticket = runtime
                .issue_idle_batch(
                    SessionAccess::new(&ingress, &request, batch_now),
                    &registration.session,
                    true,
                )
                .unwrap();
            for event in runtime
                .read_batch(SessionAccess::new(&ingress, &request, batch_now), &ticket)
                .unwrap()
            {
                runtime
                    .acknowledge_batch(
                        SessionAccess::new(&ingress, &request, batch_now),
                        &ticket,
                        event.id(),
                        BatchAcknowledgement::Confirmed,
                    )
                    .unwrap();
            }
        }
        Some(registration.session)
    } else {
        None
    };
    let domain = RawDomainKey::from_request(&ingress, &request).unwrap();
    let grant = runtime.install_raw_history_grant(&ingress, domain).unwrap();
    Context {
        ingress,
        runtime,
        request,
        grant,
        retention,
        session,
    }
}

fn seeded_context() -> Context {
    make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        true,
        true,
        "owner",
    )
}

fn exhausted_context() -> Context {
    let mut ingress = TrustedIngress::new();
    ingress.register_route(ALIAS, identity("owner")).unwrap();
    let credential = ingress
        .install_credential(
            IngressToken::new(TOKEN).unwrap(),
            CallerSubject::new(SUBJECT).unwrap(),
            [ALIAS.to_owned()],
            ChannelId::new(CHANNEL).unwrap(),
            ChannelScope::new(SCOPE).unwrap(),
            moe_agent_runtime::ReplyTarget::new("reply-target").unwrap(),
        )
        .unwrap();
    ingress
        .bind_stable_credential(credential, ATTRIBUTION)
        .unwrap();
    let retention = RawRetention::new(LONG_NORMAL_MS, LONG_RECYCLE_MS, 8, 128, 8_192).unwrap();
    let mut runtime = SessionRuntime::with_retention(
        &ingress,
        SessionLimits::new(8, 16, 2, 1_024, 8_192, 2).unwrap(),
        SessionTiming::default(),
        retention,
        START_MS,
    );
    let mut request = None;
    for order in 1_u64..=3 {
        let now_ms = START_MS + order * 2;
        let admitted = ingress
            .admit(
                RawIngressCall::new(IngressToken::new(TOKEN).unwrap(), ALIAS, BODY)
                    .with_session_hint(HINT),
            )
            .unwrap();
        let registration = runtime
            .register_user(
                SessionAccess::new(&ingress, &admitted, now_ms),
                UserEvent::new(UserEventKind::New)
                    .with_id(HostEventId::new(format!("event-{order}")).unwrap())
                    .with_source_order(order),
            )
            .unwrap();
        runtime
            .revoke_session(
                SessionAccess::new(&ingress, &admitted, now_ms + 1),
                &registration.session,
            )
            .unwrap();
        request = Some(admitted);
    }
    let request = request.unwrap();
    let domain = RawDomainKey::from_request(&ingress, &request).unwrap();
    let grant = runtime.install_raw_history_grant(&ingress, domain).unwrap();
    Context {
        ingress,
        runtime,
        request,
        grant,
        retention,
        session: None,
    }
}

impl Context {
    fn create(&mut self, root: &Path, now_ms: u64) -> (RawLocalStore, CommitReceipt) {
        let opened = RawLocalStore::open(root, OpenMode::CreateNew, self.access(now_ms)).unwrap();
        (opened.store, opened.create_receipt.unwrap())
    }

    fn access(&mut self, now_ms: u64) -> RawLocalAccess<'_> {
        let domain = RawDomainKey::from_request(&self.ingress, &self.request).unwrap();
        RawLocalAccess::new(
            &mut self.runtime,
            &self.ingress,
            &self.request,
            &self.grant,
            domain,
            now_ms,
        )
        .unwrap()
    }

    fn access_for_domain(
        &mut self,
        bound_domain: RawDomainKey,
        now_ms: u64,
    ) -> Result<RawLocalAccess<'_>, RawLocalError> {
        RawLocalAccess::new(
            &mut self.runtime,
            &self.ingress,
            &self.request,
            &self.grant,
            bound_domain,
            now_ms,
        )
    }
}

fn open_existing(context: &mut Context, root: &Path, now_ms: u64) -> RawLocalStore {
    RawLocalStore::open(root, OpenMode::OpenExisting, context.access(now_ms))
        .unwrap()
        .store
}

fn try_open(
    context: &mut Context,
    root: &Path,
    mode: OpenMode,
    now_ms: u64,
) -> Result<crate::OpenedStore, RawLocalError> {
    RawLocalStore::open(root, mode, context.access(now_ms))
}

fn checkpoint(
    context: &mut Context,
    store: &mut RawLocalStore,
    expected_revision: u64,
    now_ms: u64,
) -> Result<CommitReceipt, RawLocalError> {
    store.checkpoint(context.access(now_ms), expected_revision)
}

fn restore(
    context: &mut Context,
    store: &mut RawLocalStore,
    expected_revision: u64,
    now_ms: u64,
) -> Result<crate::RecoveryReceipt, crate::RecoveryError> {
    let retention = context.retention;
    store.restore(context.access(now_ms), &retention, expected_revision)
}

fn current_bytes(root: &Path) -> Vec<u8> {
    fs::read(root.join("raw-local.v1")).unwrap()
}

fn frame_payload(frame: &[u8]) -> &[u8] {
    let payload_len = u64::from_le_bytes(frame[28..36].try_into().unwrap()) as usize;
    &frame[36..36 + payload_len]
}

#[test]
fn create_checkpoint_reopen_and_raw_payload_are_exact() {
    let temp = TempRoot::new();
    let mut context = seeded_context();
    let expected = context
        .runtime
        .export_raw_snapshot(
            SessionAccess::new(&context.ingress, &context.request, 200_000),
            200_000,
        )
        .unwrap()
        .encode()
        .unwrap();
    let (mut store, created) = context.create(temp.path(), 200_000);
    assert_eq!(created.revision, 1);
    #[cfg(windows)]
    assert_eq!(
        created.durability,
        CommitDurability::FileSyncedDirectorySyncUnavailable
    );
    #[cfg(unix)]
    assert_eq!(created.durability, CommitDurability::FileAndDirectorySynced);
    let debug = format!("{store:?}");
    assert!(!debug.contains(BODY));
    assert!(!debug.contains(TOKEN));
    assert_eq!(frame_payload(&current_bytes(temp.path())), expected);
    let checkpoint = checkpoint(&mut context, &mut store, 1, 200_001).unwrap();
    assert_eq!(checkpoint.revision, 2);
    assert_eq!(store.revision(), 2);
    drop(store);
    let mut reopened_context = make_context(
        200_002,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let reopened = open_existing(&mut reopened_context, temp.path(), 200_002);
    assert_eq!(reopened.revision(), 2);
    assert!(temp.path().join("raw-local.lock").exists());
}

#[test]
fn read_projection_is_read_only_and_explicit_maintenance_publishes_expiry() {
    let temp = TempRoot::new();
    let mut writer = seeded_context();
    let created_at = START_MS + 200_000;
    let (store, _) = writer.create(temp.path(), created_at);
    drop(store);
    let original = current_bytes(temp.path());

    let mut reader = make_context(
        created_at + 1,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let mut store = open_existing(&mut reader, temp.path(), created_at + 1);
    let revision = store.revision();
    let retention = reader.retention;
    let projected = store
        .read_projection(reader.access(created_at + 1), &retention, revision)
        .unwrap();
    assert_eq!(projected.import.fragments.len(), 1);
    assert_eq!(current_bytes(temp.path()), original);

    let normal_deadline = START_MS + LONG_NORMAL_MS;
    let revision = store.revision();
    let maintained = store
        .maintain_retention(reader.access(normal_deadline), &retention, revision)
        .unwrap();
    assert_eq!(maintained.transferred, 1);
    assert_eq!(maintained.checkpoint.unwrap().revision, 2);
    assert_ne!(current_bytes(temp.path()), original);
    drop(store);
}

#[test]
fn retention_preview_reports_unresolved_control_and_exact_transfer_frame_size() {
    let temp = TempRoot::new();
    let mut writer = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        true,
        false,
        "owner",
    );
    let (store, _) = writer.create(temp.path(), START_MS);
    drop(store);
    let original = current_bytes(temp.path());

    let normal_ms = 100;
    let mut strict = make_context(START_MS + normal_ms, normal_ms, 20, false, false, "owner");
    let mut store = open_existing(&mut strict, temp.path(), START_MS + normal_ms);
    let revision = store.revision();
    let retention = strict.retention;
    let preview = store
        .preview_retention(strict.access(START_MS + normal_ms), &retention, revision)
        .unwrap();

    assert!(preview.changed);
    assert_eq!(preview.retained_fragments, 1);
    assert_eq!(preview.candidate_frame_bytes as usize, original.len() + 8);
    assert_eq!(
        preview.latest_user_interaction,
        Some((START_MS, None)),
        "the unresolved interaction survives a projection that will enter recycle"
    );
    assert_eq!(current_bytes(temp.path()), original, "preview is read-only");
}

#[test]
fn bounded_projection_stops_body_materialization_and_reuses_its_frame_plan() {
    let temp = TempRoot::new();
    let mut writer = seeded_context();
    let created_at = START_MS + 200_000;
    let (store, _) = writer.create(temp.path(), created_at);
    drop(store);
    let frame_len = current_bytes(temp.path()).len() as u64;

    let mut reader = make_context(
        created_at + 1,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let mut store = open_existing(&mut reader, temp.path(), created_at + 1);
    let revision = store.revision();
    let retention = reader.retention;
    let over_budget = store
        .prepare_projection(
            reader.access(created_at + 1),
            &retention,
            revision,
            frame_len,
            1,
            usize::MAX,
        )
        .unwrap();
    assert!(!over_budget.within_projection_budget());
    assert_eq!(over_budget.frame_bytes_read(), frame_len);
    assert_eq!(over_budget.decoded_event_count(), 1);
    assert!(over_budget.decoded_body_bytes() <= BODY.len());
    assert_eq!(current_bytes(temp.path()).len() as u64, frame_len);

    let body_over_budget = store
        .prepare_projection(
            reader.access(created_at + 1),
            &retention,
            revision,
            frame_len,
            128,
            BODY.len(),
        )
        .unwrap();
    assert!(!body_over_budget.within_projection_budget());
    assert_eq!(body_over_budget.decoded_event_count(), 1);
    assert_eq!(body_over_budget.decoded_body_bytes(), BODY.len());

    let plan = store
        .prepare_projection(
            reader.access(created_at + 1),
            &retention,
            revision,
            frame_len,
            128,
            8_192,
        )
        .unwrap();
    assert!(plan.within_projection_budget());
    assert_eq!(plan.frame_bytes_read(), frame_len);
    let receipt = store
        .read_projection_prepared(reader.access(created_at + 1), plan)
        .unwrap();
    assert_eq!(receipt.import.fragments.len(), 1);
    assert_eq!(current_bytes(temp.path()).len() as u64, frame_len);
}

#[test]
fn cold_projection_open_counts_its_only_frame_read_and_checks_budget_before_decode() {
    let temp = TempRoot::new();
    let mut writer = seeded_context();
    let now = START_MS + 200_000;
    let (store, _) = writer.create(temp.path(), now);
    drop(store);
    let frame = current_bytes(temp.path());
    let frame_len = frame.len() as u64;
    let mut reader = make_context(
        now + 1,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let retention = reader.retention;

    // A corrupt over-budget file must be rejected without its first decode.
    fs::write(temp.path().join("raw-local.v1"), vec![0; frame.len()]).unwrap();
    crate::FRAME_BYTES_READ.with(|count| count.set(0));
    assert!(matches!(
        RawLocalStore::open_projection(
            temp.path(),
            reader.access(now + 1),
            &retention,
            frame_len - 1,
            128,
            8_192,
        ),
        Err(RawLocalError::CapacityExceeded)
    ));
    assert_eq!(crate::FRAME_BYTES_READ.with(|count| count.get()), 0);
    fs::write(temp.path().join("raw-local.v1"), &frame).unwrap();

    let (mut store, limited) = RawLocalStore::open_projection(
        temp.path(),
        reader.access(now + 1),
        &retention,
        frame_len,
        1,
        8_192,
    )
    .unwrap();
    assert!(!limited.within_projection_budget());
    assert_eq!(limited.decoded_event_count(), 1);
    assert_eq!(crate::FRAME_BYTES_READ.with(|count| count.get()), frame_len);
    drop(limited);
    // The cold open is a read-only baseline, not permission to overwrite it.
    let revision = store.revision();
    assert!(matches!(
        store.checkpoint(reader.access(now + 1), revision),
        Err(RawLocalError::BaselineNotRestored)
    ));
    drop(store);

    crate::FRAME_BYTES_READ.with(|count| count.set(0));
    let (mut store, plan) = RawLocalStore::open_projection(
        temp.path(),
        reader.access(now + 1),
        &retention,
        frame_len,
        128,
        8_192,
    )
    .unwrap();
    assert!(plan.within_projection_budget());
    assert_eq!(plan.frame_bytes_read(), frame_len);
    assert_eq!(plan.decoded_event_count(), 2);
    let imported = store
        .read_projection_prepared(reader.access(now + 1), plan)
        .unwrap();
    assert_eq!(imported.import.fragments.len(), 1);
    assert_eq!(
        crate::FRAME_BYTES_READ.with(|count| count.get()),
        frame_len,
        "opening and projecting a cold segment must share one bounded frame read"
    );
}

#[test]
fn retirement_continuation_rechecks_the_original_domain_and_authorization_revision() {
    let temp = TempRoot::new();
    let mut owner = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let (mut store, _) = owner.create(temp.path(), START_MS);
    let retention = owner.retention;
    let revision = store.revision();
    let receipt = store
        .verify_retirable(owner.access(START_MS + 1), &retention, revision)
        .unwrap();
    store
        .remove_retired_payload(owner.access(START_MS + 1), &retention, receipt)
        .unwrap();
    drop(store);
    let gate_path = crate::retirement_gate_path(temp.path()).unwrap();
    let gate_before = fs::read(&gate_path).unwrap();

    let mut unrelated = make_context(
        START_MS + 2,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "other-owner",
    );
    assert!(matches!(
        RawLocalStore::lock_retirement(temp.path(), unrelated.access(START_MS + 2), receipt, true,),
        Err(RawLocalError::DomainMismatch)
    ));

    let mut changed = make_context(
        START_MS + 2,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    changed
        .ingress
        .bind_stable_credential_revision(changed.request.credential(), ATTRIBUTION, 2)
        .unwrap();
    assert!(matches!(
        RawLocalStore::lock_retirement(temp.path(), changed.access(START_MS + 2), receipt, true,),
        Err(RawLocalError::Gateway(GatewayError::Revoked))
    ));
    assert_eq!(fs::read(&gate_path).unwrap(), gate_before);
    assert!(temp.path().join("raw-local.lock").exists());

    let mut fresh = make_context(
        START_MS + 2,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let domain = RawDomainKey::from_request(&fresh.ingress, &fresh.request).unwrap();
    assert_eq!(
        RawLocalStore::retirement_gate_size(&domain).unwrap(),
        gate_before.len() as u64
    );
    let mut guard =
        RawLocalStore::lock_retirement(temp.path(), fresh.access(START_MS + 2), receipt, true)
            .unwrap();
    guard.remove_lock_file().unwrap();
    guard.mark_lock_removed().unwrap();
    drop(guard);
    assert!(matches!(
        RawLocalStore::lock_retirement(temp.path(), unrelated.access(START_MS + 3), receipt, false,),
        Err(RawLocalError::DomainMismatch)
    ));
    let mut guard =
        RawLocalStore::lock_retirement(temp.path(), fresh.access(START_MS + 3), receipt, false)
            .unwrap();
    guard.remove_segment_directory().unwrap();
    drop(guard);
    assert!(!temp.path().exists());
}

#[test]
fn process_cold_starts_reauthorize_recover_and_deduplicate() {
    let temp = TempRoot::new();
    let mut parent = seeded_context();
    let old_session = parent.session.clone().unwrap();
    let (store, _) = parent.create(temp.path(), 200_000);
    drop(store);
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::child_process_entry")
        .arg("--nocapture")
        .env("MOE_RAW_LOCAL_CHILD_ROOT", temp.path())
        .env("MOE_RAW_LOCAL_CHILD_MODE", "restore")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut second_cold_start = make_context(
        200_003,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let mut reopened = open_existing(&mut second_cold_start, temp.path(), 200_003);
    let revision = reopened.revision();
    let receipt = restore(&mut second_cold_start, &mut reopened, revision, 200_003).unwrap();
    assert!(!receipt.import.duplicate);
    assert_eq!(receipt.import.fragments.len(), 1);
    assert!(matches!(
        second_cold_start.runtime.read_active(
            SessionAccess::new(
                &second_cold_start.ingress,
                &second_cold_start.request,
                200_003,
            ),
            &old_session,
        ),
        Err(GatewayError::ForeignRequest | GatewayError::StaleHandle)
    ));
}

#[test]
fn child_process_entry() {
    let Ok(root) = std::env::var("MOE_RAW_LOCAL_CHILD_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    match std::env::var("MOE_RAW_LOCAL_CHILD_MODE").as_deref() {
        Ok("restore") => {
            let mut child = make_context(
                200_001,
                LONG_NORMAL_MS,
                LONG_RECYCLE_MS,
                false,
                false,
                "owner",
            );
            let mut store = open_existing(&mut child, &root, 200_001);
            let first = restore(&mut child, &mut store, 1, 200_001).unwrap();
            assert!(!first.import.duplicate);
            assert_eq!(first.import.fragments.len(), 1);
            let second = restore(&mut child, &mut store, 2, 200_002).unwrap();
            assert!(second.import.duplicate);
            assert_eq!(second.import.fragments.len(), 1);
            assert_eq!(
                second.import.fragments[0].session,
                first.import.fragments[0].session
            );
        }
        Ok("lock") => {
            let mut child = make_context(
                200_001,
                LONG_NORMAL_MS,
                LONG_RECYCLE_MS,
                false,
                false,
                "owner",
            );
            let result = try_open(&mut child, &root, OpenMode::OpenExisting, 200_001);
            assert!(matches!(result, Err(RawLocalError::Busy)));
        }
        Ok("recycle") => {
            let mut child = make_context(1_010, 10, 20, false, false, "owner");
            let mut store = open_existing(&mut child, &root, 1_010);
            let first = restore(&mut child, &mut store, 1, 1_010).unwrap();
            assert_eq!(first.transferred, 1);
            assert_eq!(first.import.fragments[0].final_deadline_ms, Some(1_030));
            let second = restore(&mut child, &mut store, 2, 1_011).unwrap();
            assert!(second.import.duplicate);
        }
        _ => panic!("unknown child test mode"),
    }
}

#[test]
fn cross_process_file_lock_rejects_second_writer() {
    let temp = TempRoot::new();
    let mut parent = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let (store, _) = parent.create(temp.path(), START_MS);
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::child_process_entry")
        .arg("--nocapture")
        .env("MOE_RAW_LOCAL_CHILD_ROOT", temp.path())
        .env("MOE_RAW_LOCAL_CHILD_MODE", "lock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(store);
    assert!(temp.path().join("raw-local.lock").exists());
}

#[test]
fn retirement_uses_the_segment_lock_and_prevents_lock_file_recreation() {
    let temp = TempRoot::new();
    let mut context = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let (mut store, _) = context.create(temp.path(), START_MS);
    let retention = context.retention;
    let revision = store.revision();
    let receipt = store
        .verify_retirable(context.access(START_MS + 1), &retention, revision)
        .unwrap();

    assert!(matches!(
        RawLocalStore::open_for_retirement(
            temp.path(),
            context.access(START_MS + 1),
            receipt.revision,
            receipt.committed_at_ms,
        ),
        Err(RawLocalError::Busy)
    ));

    store
        .remove_retired_payload(context.access(START_MS + 1), &retention, receipt)
        .unwrap();
    assert_eq!(
        RawLocalStore::inspect_retirement(temp.path())
            .unwrap()
            .unwrap()
            .stage,
        crate::RawRetirementStage::PayloadRemoved
    );
    drop(store);

    assert!(matches!(
        try_open(
            &mut context,
            temp.path(),
            OpenMode::OpenExisting,
            START_MS + 2
        ),
        Err(RawLocalError::Retiring)
    ));
    assert!(matches!(
        try_open(&mut context, temp.path(), OpenMode::CreateNew, START_MS + 2),
        Err(RawLocalError::Retiring)
    ));
    assert!(!temp.path().join("raw-local.v1").exists());
    assert!(temp.path().join("raw-local.lock").exists());

    let mut guard =
        RawLocalStore::lock_retirement(temp.path(), context.access(START_MS + 2), receipt, true)
            .unwrap();
    guard.remove_lock_file().unwrap();
    guard.mark_lock_removed().unwrap();
    guard.remove_segment_directory().unwrap();
    drop(guard);
    assert!(!temp.path().exists());
    assert_eq!(
        RawLocalStore::inspect_retirement(temp.path())
            .unwrap()
            .unwrap()
            .stage,
        crate::RawRetirementStage::DirectoryRemoved
    );
    assert!(matches!(
        try_open(&mut context, temp.path(), OpenMode::CreateNew, START_MS + 3),
        Err(RawLocalError::Retiring)
    ));
    assert!(!temp.path().exists());
}

#[test]
fn checkpoint_replaces_the_current_frame_while_holding_the_real_segment_lock() {
    let temp = TempRoot::new();
    let mut context = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let (mut store, created) = context.create(temp.path(), START_MS);
    let first = current_bytes(temp.path());
    let registered = context
        .runtime
        .register_user(
            SessionAccess::new(&context.ingress, &context.request, START_MS + 1),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    context
        .runtime
        .finish_interaction(
            SessionAccess::new(&context.ingress, &context.request, START_MS + 2),
            &registered.interaction,
            TerminalState::Completed,
            Some("synthetic replacement body"),
        )
        .unwrap();
    let replacement = checkpoint(&mut context, &mut store, created.revision, START_MS + 3).unwrap();
    let second = current_bytes(temp.path());
    assert_ne!(first, second);
    assert!(!temp.path().join("raw-local.pending").exists());
    assert_eq!(replacement.revision, created.revision + 1);
    assert!(matches!(
        try_open(
            &mut context,
            temp.path(),
            OpenMode::OpenExisting,
            START_MS + 4
        ),
        Err(RawLocalError::Busy)
    ));
    drop(store);

    let reopened = open_existing(&mut context, temp.path(), START_MS + 4);
    assert_eq!(reopened.revision(), replacement.revision);
    assert_eq!(current_bytes(temp.path()), second);
}

#[test]
fn parent_child_cold_starts_persist_recycle_and_final_cleanup() {
    let temp = TempRoot::new();
    let mut parent = make_context(START_MS, 10, 20, true, false, "owner");
    let (store, _) = parent.create(temp.path(), START_MS);
    drop(store);
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::child_process_entry")
        .arg("--nocapture")
        .env("MOE_RAW_LOCAL_CHILD_ROOT", temp.path())
        .env("MOE_RAW_LOCAL_CHILD_MODE", "recycle")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut recycle_start = make_context(1_020, 10, 20, false, false, "owner");
    let mut reopened = open_existing(&mut recycle_start, temp.path(), 1_020);
    let revision = reopened.revision();
    let receipt = restore(&mut recycle_start, &mut reopened, revision, 1_020).unwrap();
    assert_eq!(receipt.retained, 1);
    assert_eq!(
        receipt.import.fragments[0].zone,
        moe_agent_runtime::RetentionZone::Recycle
    );
    drop(reopened);

    let mut final_cutoff = make_context(1_030, 10, 20, false, false, "owner");
    let mut reopened = open_existing(&mut final_cutoff, temp.path(), 1_030);
    let revision = reopened.revision();
    let receipt = restore(&mut final_cutoff, &mut reopened, revision, 1_030).unwrap();
    assert_eq!(receipt.removed, 1);
    drop(reopened);
    assert!(!String::from_utf8_lossy(&current_bytes(temp.path())).contains(BODY));
}

#[test]
fn create_new_refuses_existing_and_orphaned_store_state() {
    let temp = TempRoot::new();
    let mut first = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let (store, _) = first.create(temp.path(), START_MS);
    drop(store);
    let original = current_bytes(temp.path());
    let mut second = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    assert!(matches!(
        try_open(&mut second, temp.path(), OpenMode::CreateNew, START_MS),
        Err(RawLocalError::AlreadyExists)
    ));
    assert_eq!(current_bytes(temp.path()), original);
    fs::remove_file(temp.path().join("raw-local.v1")).unwrap();
    assert!(matches!(
        try_open(&mut second, temp.path(), OpenMode::CreateNew, START_MS),
        Err(RawLocalError::AlreadyExists)
    ));
}

#[test]
fn normal_recycle_and_final_deadline_equality_are_enforced() {
    let recycled_root = TempRoot::new();
    let mut recycled = make_context(START_MS, 10, 20, true, false, "owner");
    let (store, _) = recycled.create(recycled_root.path(), START_MS);
    drop(store);
    let mut fresh = make_context(1_010, 10, 20, false, false, "owner");
    let mut store = open_existing(&mut fresh, recycled_root.path(), 1_010);
    let receipt = restore(&mut fresh, &mut store, 1, 1_010).unwrap();
    assert_eq!(receipt.transferred, 1);
    assert_eq!(
        receipt.import.fragments[0].zone,
        moe_agent_runtime::RetentionZone::Recycle
    );
    drop(store);

    let expired_root = TempRoot::new();
    let mut expired = make_context(START_MS, 10, 20, true, false, "owner");
    let (store, _) = expired.create(expired_root.path(), START_MS);
    drop(store);
    let mut fresh = make_context(1_030, 10, 20, false, false, "owner");
    let mut store = open_existing(&mut fresh, expired_root.path(), 1_030);
    let receipt = restore(&mut fresh, &mut store, 1, 1_030).unwrap();
    assert_eq!(receipt.removed, 1);
    assert!(receipt.import.fragments.is_empty());
    drop(store);
    let files = fs::read_dir(expired_root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    assert!(!String::from_utf8_lossy(&current_bytes(expired_root.path())).contains(BODY));
}

fn check_tightened_retention_survives_reopen(recycled: bool, reopen_ms: u64) {
    let temp = TempRoot::new();
    let mut original = make_context(START_MS, 1_000, 2_000, true, false, "owner");
    if recycled {
        let session = original.session.clone().unwrap();
        let capability = original
            .runtime
            .install_management_capability(
                &original.ingress,
                original.request.identity().clone(),
                original.request.isolation().clone(),
                vec![session.clone()],
                moe_agent_runtime::ManagementPermissions::default().with_soft_delete(),
            )
            .unwrap();
        original
            .runtime
            .soft_delete_fragment(
                SessionAccess::new(&original.ingress, &original.request, 1_010),
                &capability,
                &session,
                0,
            )
            .unwrap();
    }
    let (store, _) = original.create(temp.path(), 1_010);
    drop(store);
    drop(original);

    let mut strict = make_context(1_050, 100, 200, false, false, "owner");
    let mut store = open_existing(&mut strict, temp.path(), 1_050);
    let restored = restore(&mut strict, &mut store, 1, 1_050).unwrap();
    assert_eq!(restored.import.fragments[0].normal_deadline_ms, 1_100);
    if recycled {
        assert_eq!(restored.import.fragments[0].final_deadline_ms, Some(1_300));
    }
    drop(store);
    drop(strict);

    let mut wide = make_context(reopen_ms, 1_000, 2_000, false, false, "owner");
    let mut store = open_existing(&mut wide, temp.path(), reopen_ms);
    let restored = restore(&mut wide, &mut store, 2, reopen_ms).unwrap();
    assert_eq!(
        restored.removed, 1,
        "恢复成功本身必须固化更严格期限，无需再 checkpoint"
    );
    assert!(restored.import.fragments.is_empty());
    assert!(!String::from_utf8_lossy(&current_bytes(temp.path())).contains(BODY));
}

#[test]
fn tightened_normal_retention_survives_two_reopens_without_checkpoint() {
    for now_ms in [1_300, 1_400] {
        check_tightened_retention_survives_reopen(false, now_ms);
    }
}

#[test]
fn tightened_recycle_retention_survives_two_reopens_without_checkpoint() {
    for now_ms in [1_300, 1_400] {
        check_tightened_retention_survives_reopen(true, now_ms);
    }
}

#[test]
fn stale_cas_clock_rollback_reauthorization_and_domain_binding_reject() {
    let temp = TempRoot::new();
    let mut owner = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let (mut store, _) = owner.create(temp.path(), START_MS);
    assert_eq!(
        checkpoint(&mut owner, &mut store, 0, START_MS + 1),
        Err(RawLocalError::CompareAndSwap)
    );
    assert!(matches!(
        checkpoint(&mut owner, &mut store, 1, START_MS - 1),
        Err(RawLocalError::Gateway(GatewayError::ClockWentBackwards))
    ));
    owner
        .runtime
        .revoke_raw_history_grant(&owner.grant)
        .unwrap();
    assert!(matches!(
        checkpoint(&mut owner, &mut store, 1, START_MS + 1),
        Err(RawLocalError::Gateway(GatewayError::Revoked))
    ));
    drop(store);

    let unauthorized_root = temp.path().join("absent");
    let mut other = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "other-owner",
    );
    let owner_domain = RawDomainKey::from_request(&owner.ingress, &owner.request).unwrap();
    assert!(matches!(
        other.access_for_domain(owner_domain, START_MS),
        Err(RawLocalError::DomainMismatch)
    ));
    assert!(!unauthorized_root.exists());

    let mut rolled_back = make_context(
        START_MS - 1,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    assert!(matches!(
        try_open(
            &mut rolled_back,
            temp.path(),
            OpenMode::OpenExisting,
            START_MS - 1
        ),
        Err(RawLocalError::ClockWentBackwards)
    ));
}

#[test]
fn corrupt_unknown_oversized_length_and_pending_frames_are_preserved() {
    let temp = TempRoot::new();
    let mut owner = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let (store, _) = owner.create(temp.path(), START_MS);
    drop(store);
    let good = current_bytes(temp.path());

    for (mutation, expected) in [
        ("magic", RawLocalError::CorruptFrame),
        ("unknown", RawLocalError::UnsupportedVersion),
        ("length", RawLocalError::CapacityExceeded),
        ("revision", RawLocalError::CorruptFrame),
        ("epoch", RawLocalError::CorruptFrame),
        ("checksum", RawLocalError::CorruptFrame),
        ("truncated", RawLocalError::CorruptFrame),
    ] {
        fs::write(temp.path().join("raw-local.v1"), &good).unwrap();
        let mut bad = good.clone();
        match mutation {
            "magic" => bad[0] ^= 1,
            "unknown" => bad[8..12].copy_from_slice(&2_u32.to_le_bytes()),
            "length" => bad[28..36].copy_from_slice(&u64::MAX.to_le_bytes()),
            "revision" => bad[12] ^= 1,
            "epoch" => bad[20] ^= 1,
            "checksum" => bad[36] ^= 1,
            "truncated" => bad.truncate(18),
            _ => unreachable!(),
        }
        fs::write(temp.path().join("raw-local.v1"), &bad).unwrap();
        let result = try_open(&mut owner, temp.path(), OpenMode::OpenExisting, START_MS);
        assert!(matches!(result, Err(actual) if actual == expected));
        assert_eq!(current_bytes(temp.path()), bad);
    }

    fs::write(temp.path().join("raw-local.v1"), &good).unwrap();
    let pending = temp.path().join("raw-local.pending");
    fs::write(&pending, &good[..good.len() / 2]).unwrap();
    assert!(matches!(
        try_open(&mut owner, temp.path(), OpenMode::OpenExisting, START_MS),
        Err(RawLocalError::PendingRecoveryRequired)
    ));
    assert_eq!(current_bytes(temp.path()), good);
    assert!(pending.exists());
}

#[test]
fn capacity_and_injected_failures_preserve_exact_durable_state() {
    let oversized = vec![0; moe_agent_runtime::RAW_SNAPSHOT_MAX_TOTAL_BYTES + 1];
    assert_eq!(
        crate::encode_frame(1, START_MS, &oversized),
        Err(RawLocalError::CapacityExceeded)
    );
    assert!(RAW_LOCAL_MAX_FRAME_BYTES > oversized.len());

    for point in [
        FailurePoint::PartialWrite,
        FailurePoint::Write,
        FailurePoint::FileSync,
        FailurePoint::Replace,
        FailurePoint::DirectorySync,
    ] {
        let temp = TempRoot::new();
        let mut context = make_context(
            START_MS,
            LONG_NORMAL_MS,
            LONG_RECYCLE_MS,
            false,
            false,
            "owner",
        );
        let (mut store, _) = context.create(temp.path(), START_MS);
        let old_frame = current_bytes(temp.path());
        store.fail_at = Some(point);
        let error = checkpoint(&mut context, &mut store, 1, START_MS + 1).unwrap_err();
        let expected_stage = match point {
            FailurePoint::PartialWrite | FailurePoint::Write => CommitStage::WritePending,
            FailurePoint::FileSync => CommitStage::SyncPending,
            FailurePoint::Replace => CommitStage::ReplaceCurrent,
            FailurePoint::DirectorySync => CommitStage::SyncDirectory,
        };
        assert_eq!(error, RawLocalError::CommitOutcomeUnknown(expected_stage));
        assert_eq!(store.revision(), 1);
        assert_eq!(
            checkpoint(&mut context, &mut store, 1, START_MS + 1),
            Err(RawLocalError::Poisoned)
        );
        let current = current_bytes(temp.path());
        if point == FailurePoint::DirectorySync {
            assert_ne!(current, old_frame);
            assert!(!temp.path().join("raw-local.pending").exists());
        } else {
            assert_eq!(current, old_frame);
            assert!(temp.path().join("raw-local.pending").exists());
        }
        drop(store);
        let reopened = try_open(
            &mut context,
            temp.path(),
            OpenMode::OpenExisting,
            START_MS + 1,
        );
        if point == FailurePoint::DirectorySync {
            assert_eq!(reopened.unwrap().store.revision(), 2);
        } else {
            assert!(matches!(
                reopened,
                Err(RawLocalError::PendingRecoveryRequired)
            ));
        }
    }
}

#[test]
fn denied_reopen_does_not_touch_missing_store_and_format_budget_is_bounded() {
    let temp = TempRoot::new();
    let mut context = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let denied = context.runtime.revoke_raw_history_grant(&context.grant);
    assert!(denied.is_ok());
    let absent = temp.path().join("not-created");
    assert!(matches!(
        try_open(&mut context, &absent, OpenMode::OpenExisting, START_MS),
        Err(RawLocalError::Gateway(GatewayError::Revoked))
    ));
    assert!(!absent.exists());
    assert_eq!(RAW_LOCAL_MAX_FRAME_BYTES, 16_777_216 + 44);
}

#[test]
fn persisted_source_watermarks_survive_without_host_event_ids() {
    let temp = TempRoot::new();
    let mut original = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let registration = original
        .runtime
        .register_user(
            SessionAccess::new(&original.ingress, &original.request, START_MS),
            UserEvent::new(UserEventKind::New).with_source_order(7),
        )
        .unwrap();
    original
        .runtime
        .revoke_session(
            SessionAccess::new(&original.ingress, &original.request, START_MS + 1),
            &registration.session,
        )
        .unwrap();
    let (store, _) = original.create(temp.path(), START_MS + 2);
    drop(store);

    let mut fresh = make_context(
        START_MS + 3,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let mut reopened = open_existing(&mut fresh, temp.path(), START_MS + 3);
    restore(&mut fresh, &mut reopened, 1, START_MS + 3).unwrap();
    for (index, order) in [0, 7].into_iter().enumerate() {
        let now_ms = START_MS + 4 + index as u64;
        fresh.request = fresh
            .ingress
            .admit(
                RawIngressCall::new(IngressToken::new(TOKEN).unwrap(), ALIAS, BODY)
                    .with_session_hint(HINT),
            )
            .unwrap();
        let delayed = fresh
            .runtime
            .register_user(
                SessionAccess::new(&fresh.ingress, &fresh.request, now_ms),
                UserEvent::new(UserEventKind::New).with_source_order(order),
            )
            .unwrap();
        assert!(!delayed.renewed);
        assert!(
            fresh
                .runtime
                .read_active(
                    SessionAccess::new(&fresh.ingress, &fresh.request, now_ms),
                    &delayed.session,
                )
                .is_err()
        );
        assert!(
            fresh
                .runtime
                .finish_interaction(
                    SessionAccess::new(&fresh.ingress, &fresh.request, now_ms),
                    &delayed.interaction,
                    TerminalState::Completed,
                    None,
                )
                .is_err()
        );
    }
    fresh.request = fresh
        .ingress
        .admit(
            RawIngressCall::new(IngressToken::new(TOKEN).unwrap(), ALIAS, BODY)
                .with_session_hint(HINT),
        )
        .unwrap();
    let newer = fresh
        .runtime
        .register_user(
            SessionAccess::new(&fresh.ingress, &fresh.request, START_MS + 6),
            UserEvent::new(UserEventKind::New).with_source_order(8),
        )
        .unwrap();
    assert!(newer.renewed);
}

#[test]
fn cold_checkpoint_cannot_reset_persisted_replay_fences() {
    let temp = TempRoot::new();
    let mut original = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        true,
        false,
        "owner",
    );
    let session = original.session.clone().unwrap();
    original
        .runtime
        .revoke_session(
            SessionAccess::new(&original.ingress, &original.request, START_MS + 1),
            &session,
        )
        .unwrap();
    let (store, _) = original.create(temp.path(), START_MS + 2);
    drop(store);
    let before = current_bytes(temp.path());
    let mut fresh = make_context(
        START_MS + 3,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let mut reopened = open_existing(&mut fresh, temp.path(), START_MS + 3);
    assert!(checkpoint(&mut fresh, &mut reopened, 1, START_MS + 3).is_err());
    assert_eq!(current_bytes(temp.path()), before);
    let restored = restore(&mut fresh, &mut reopened, 1, START_MS + 3).unwrap();
    assert!(restored.import.fragments.is_empty());
    assert!(
        fresh
            .runtime
            .register_user(
                SessionAccess::new(&fresh.ingress, &fresh.request, START_MS + 4),
                UserEvent::new(UserEventKind::History)
                    .with_id(HostEventId::new("event-1").unwrap())
                    .with_source_order(7),
            )
            .is_err()
    );

    let checkpoint = checkpoint(
        &mut fresh,
        &mut reopened,
        restored.checkpoint.revision,
        START_MS + 5,
    )
    .unwrap();
    drop(reopened);

    let mut rebound = make_context(
        START_MS + 6,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let mut reopened = open_existing(&mut rebound, temp.path(), START_MS + 6);
    let restored_again = restore(
        &mut rebound,
        &mut reopened,
        checkpoint.revision,
        START_MS + 6,
    )
    .unwrap();
    assert!(restored_again.import.fragments.is_empty());
    assert!(
        rebound
            .runtime
            .register_user(
                SessionAccess::new(&rebound.ingress, &rebound.request, START_MS + 7),
                UserEvent::new(UserEventKind::History)
                    .with_id(HostEventId::new("event-1").unwrap())
                    .with_source_order(7),
            )
            .is_err()
    );
}

#[test]
fn cold_open_cannot_checkpoint_over_body_and_restore_can_checkpoint() {
    let temp = TempRoot::new();
    let mut original = seeded_context();
    let (store, _) = original.create(temp.path(), 200_000);
    drop(store);
    let before = current_bytes(temp.path());

    let mut fresh = make_context(
        200_001,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let mut reopened = open_existing(&mut fresh, temp.path(), 200_001);
    assert_eq!(
        checkpoint(&mut fresh, &mut reopened, 1, 200_001),
        Err(RawLocalError::BaselineNotRestored)
    );
    assert_eq!(current_bytes(temp.path()), before);
    assert_eq!(reopened.revision(), 1);

    let restored = restore(&mut fresh, &mut reopened, 1, 200_001).unwrap();
    assert_eq!(restored.import.fragments.len(), 1);
    let receipt = checkpoint(
        &mut fresh,
        &mut reopened,
        restored.checkpoint.revision,
        200_002,
    )
    .unwrap();
    assert_eq!(receipt.revision, 3);
    assert!(String::from_utf8_lossy(&current_bytes(temp.path())).contains(BODY));
}

#[test]
fn checkpoint_cas_and_revoked_grant_fail_atomically_until_restore_rebinds() {
    let temp = TempRoot::new();
    let mut context = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let (mut store, _) = context.create(temp.path(), START_MS);
    let original = current_bytes(temp.path());

    let mut swapped_runtime = make_context(
        START_MS + 1,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    assert!(matches!(
        checkpoint(&mut swapped_runtime, &mut store, 1, START_MS + 1),
        Err(RawLocalError::Gateway(GatewayError::ForeignRequest))
    ));
    assert_eq!(current_bytes(temp.path()), original);
    assert_eq!(store.revision(), 1);

    assert_eq!(
        checkpoint(&mut context, &mut store, 0, START_MS + 1),
        Err(RawLocalError::CompareAndSwap)
    );
    assert_eq!(current_bytes(temp.path()), original);
    assert_eq!(store.revision(), 1);

    context
        .runtime
        .revoke_raw_history_grant(&context.grant)
        .unwrap();
    assert!(matches!(
        checkpoint(&mut context, &mut store, 1, START_MS + 1),
        Err(RawLocalError::Gateway(GatewayError::Revoked))
    ));
    assert_eq!(current_bytes(temp.path()), original);
    assert_eq!(store.revision(), 1);

    let domain = RawDomainKey::from_request(&context.ingress, &context.request).unwrap();
    context.grant = context
        .runtime
        .install_raw_history_grant(&context.ingress, domain)
        .unwrap();
    assert!(matches!(
        checkpoint(&mut context, &mut store, 1, START_MS + 1),
        Err(RawLocalError::Gateway(GatewayError::Revoked))
    ));
    assert_eq!(current_bytes(temp.path()), original);
    assert_eq!(store.revision(), 1);

    let restored = restore(&mut context, &mut store, 1, START_MS + 1).unwrap();
    assert!(restored.import.fragments.is_empty());
    let checkpoint = checkpoint(
        &mut context,
        &mut store,
        restored.checkpoint.revision,
        START_MS + 2,
    )
    .unwrap();
    assert_eq!(checkpoint.revision, 3);
}

#[test]
fn post_checkpoint_import_rejection_preserves_receipt_without_binding_baseline() {
    let temp = TempRoot::new();
    let mut original = make_context(
        START_MS,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        true,
        false,
        "owner",
    );
    let (store, _) = original.create(temp.path(), START_MS);
    drop(store);
    let mut fresh = make_context(
        START_MS + 1,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    fresh
        .ingress
        .bind_stable_credential_revision(fresh.request.credential(), ATTRIBUTION, 2)
        .unwrap();
    let mut reopened = open_existing(&mut fresh, temp.path(), START_MS + 1);
    for revision in [1, 2] {
        let failure = restore(&mut fresh, &mut reopened, revision, START_MS + 1).unwrap_err();
        let crate::RecoveryError::PostCheckpoint {
            checkpoint: receipt,
            source,
        } = failure
        else {
            panic!("导入拒绝必须携带已发布检查点回执");
        };
        assert_eq!(source, RawLocalError::Gateway(GatewayError::Revoked));
        assert_eq!(receipt.revision, revision + 1);
        assert_eq!(reopened.revision(), receipt.revision);
        assert!(!reopened.poisoned);
        assert!(reopened.baseline_grant.is_none());
        let domain = RawDomainKey::from_request(&fresh.ingress, &fresh.request).unwrap();
        let frame = crate::decode_frame(&current_bytes(temp.path()), &domain).unwrap();
        assert_eq!(frame.revision, receipt.revision);
        assert_eq!(frame.snapshot.fragment_count(), 1);
        let committed = current_bytes(temp.path());
        assert!(matches!(
            checkpoint(&mut fresh, &mut reopened, receipt.revision, START_MS + 1),
            Err(RawLocalError::BaselineNotRestored)
        ));
        assert_eq!(current_bytes(temp.path()), committed);
        assert_eq!(
            fresh
                .runtime
                .export_raw_snapshot(
                    SessionAccess::new(&fresh.ingress, &fresh.request, START_MS + 1),
                    START_MS + 1,
                )
                .unwrap()
                .fragment_count(),
            0
        );
    }
}

#[test]
fn replay_protection_exhaustion_survives_cold_restore_rebind_and_checkpoint() {
    let temp = TempRoot::new();
    let mut original = exhausted_context();
    let (store, _) = original.create(temp.path(), START_MS + 10);
    drop(store);

    let mut fresh = make_context(
        START_MS + 11,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let mut reopened = open_existing(&mut fresh, temp.path(), START_MS + 11);
    let restored = restore(&mut fresh, &mut reopened, 1, START_MS + 11).unwrap();
    assert!(restored.import.replay_protection_exhausted);
    assert_eq!(
        fresh.runtime.register_user(
            SessionAccess::new(&fresh.ingress, &fresh.request, START_MS + 12),
            UserEvent::new(UserEventKind::New),
        ),
        Err(GatewayError::ReplayProtectionExhausted)
    );
    let checkpoint = checkpoint(
        &mut fresh,
        &mut reopened,
        restored.checkpoint.revision,
        START_MS + 12,
    )
    .unwrap();
    drop(reopened);

    let mut rebound = make_context(
        START_MS + 13,
        LONG_NORMAL_MS,
        LONG_RECYCLE_MS,
        false,
        false,
        "owner",
    );
    let mut reopened = open_existing(&mut rebound, temp.path(), START_MS + 13);
    let restored_again = restore(
        &mut rebound,
        &mut reopened,
        checkpoint.revision,
        START_MS + 13,
    )
    .unwrap();
    assert!(restored_again.import.replay_protection_exhausted);
    assert_eq!(
        rebound.runtime.register_user(
            SessionAccess::new(&rebound.ingress, &rebound.request, START_MS + 14),
            UserEvent::new(UserEventKind::New),
        ),
        Err(GatewayError::ReplayProtectionExhausted)
    );
}
