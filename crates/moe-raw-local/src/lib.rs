//! Host-owned local storage for `RawSnapshotV1`.
//!
//! `RawLocalStore::read_projection` authorizes and imports a bounded retained
//! view without changing the committed frame. `maintain_retention` separately
//! sweeps one segment and advances its CAS revision only when expiry changes the
//! snapshot. `preview_retention` reports the exact encoded candidate frame size and
//! latest user-interaction terminal evidence without importing bodies or writing.
//! `open_projection` includes the cold open in the same bounded frame read and
//! returns a reusable plan. Retirement gates bind the complete stable domain and
//! authorization revision; a continuation guard rechecks its borrowed access at
//! every deletion step.
//! Multi-segment directory coordination belongs to the host adapter.

use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use moe_agent_runtime::{
    GatewayError, RawDomainKey, RawHistoryGrant, RawImportReceipt, RawRetention, RawSnapshotV1,
    SessionAccess, SessionRuntime, TrustedIngress, TrustedRequest,
};

const FRAME_MAGIC: &[u8; 8] = b"MOERLOC1";
const FRAME_VERSION: u32 = 1;
const FRAME_HEADER_BYTES: usize = 36;
const FRAME_TRAILER_BYTES: usize = 8;
const FRAME_OVERHEAD_BYTES: usize = FRAME_HEADER_BYTES + FRAME_TRAILER_BYTES;
const CURRENT_FILE: &str = "raw-local.v1";
const LOCK_FILE: &str = "raw-local.lock";
const PENDING_FILE: &str = "raw-local.pending";
const RETIRING_FILE: &str = "raw-local.retiring";
const GATE_SUFFIX: &str = ".raw-local-gate";
const GATE_MAGIC: &[u8; 8] = b"MOERLG02";
const GATE_HEADER_BYTES: usize = 37;
const MAX_GATE_BYTES: usize = 16_384;

pub const RAW_LOCAL_MAX_FRAME_BYTES: usize =
    moe_agent_runtime::RAW_SNAPSHOT_MAX_TOTAL_BYTES + FRAME_OVERHEAD_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    CreateNew,
    OpenExisting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitDurability {
    FileAndDirectorySynced,
    FileSyncedDirectorySyncUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitStage {
    CreatePending,
    WritePending,
    SyncPending,
    ReplaceCurrent,
    SyncDirectory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitReceipt {
    pub revision: u64,
    pub committed_at_ms: u64,
    pub durability: CommitDurability,
}

pub struct OpenedStore {
    pub store: RawLocalStore,
    pub create_receipt: Option<CommitReceipt>,
}

struct ProjectionRead<'a> {
    retention: &'a RawRetention,
    frame_bytes: u64,
    events: usize,
    body_bytes: usize,
}

struct OpenResult {
    opened: OpenedStore,
    projection: Option<RetentionPlan>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RawLocalError {
    Gateway(GatewayError),
    Io(io::ErrorKind),
    Busy,
    NotFound,
    AlreadyExists,
    PendingRecoveryRequired,
    CorruptFrame,
    UnsupportedVersion,
    CapacityExceeded,
    BaselineNotRestored,
    CompareAndSwap,
    RevisionExhausted,
    ClockWentBackwards,
    DomainMismatch,
    RetirementNotReady,
    Retiring,
    Poisoned,
    CommitOutcomeUnknown(CommitStage),
}

impl fmt::Display for RawLocalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gateway(error) => write!(
                formatter,
                "runtime authorization or snapshot error: {error}"
            ),
            Self::Io(kind) => write!(formatter, "local raw store I/O failed ({kind:?})"),
            Self::Busy => formatter.write_str("local raw store is locked by another process"),
            Self::NotFound => formatter.write_str("local raw store does not exist"),
            Self::AlreadyExists => formatter.write_str("local raw store already exists"),
            Self::PendingRecoveryRequired => {
                formatter.write_str("pending local raw frame requires explicit recovery")
            }
            Self::CorruptFrame => formatter.write_str("local raw frame is corrupt or malformed"),
            Self::UnsupportedVersion => {
                formatter.write_str("local raw frame version is unsupported")
            }
            Self::CapacityExceeded => {
                formatter.write_str("local raw frame exceeds its byte budget")
            }
            Self::BaselineNotRestored => {
                formatter.write_str("local raw runtime baseline has not been restored")
            }
            Self::CompareAndSwap => formatter.write_str("local raw store revision conflict"),
            Self::RevisionExhausted => {
                formatter.write_str("local raw store revision space exhausted")
            }
            Self::ClockWentBackwards => {
                formatter.write_str("trusted epoch precedes the stored commit")
            }
            Self::DomainMismatch => {
                formatter.write_str("local raw store belongs to another domain")
            }
            Self::RetirementNotReady => {
                formatter.write_str("local raw frame still contains retained history")
            }
            Self::Retiring => formatter.write_str("local raw store is being retired"),
            Self::Poisoned => {
                formatter.write_str("local raw store requires close and verified reopen")
            }
            Self::CommitOutcomeUnknown(stage) => write!(
                formatter,
                "local raw commit outcome is unknown after {stage:?}; reopen required"
            ),
        }
    }
}

impl Error for RawLocalError {}

impl From<GatewayError> for RawLocalError {
    fn from(error: GatewayError) -> Self {
        Self::Gateway(error)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryError {
    Store(RawLocalError),
    PostCheckpoint {
        checkpoint: CommitReceipt,
        source: RawLocalError,
    },
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::PostCheckpoint { checkpoint, source } => write!(
                formatter,
                "snapshot checkpoint {} was published but post-checkpoint work failed: {source}",
                checkpoint.revision
            ),
        }
    }
}

impl Error for RecoveryError {}

impl From<RawLocalError> for RecoveryError {
    fn from(error: RawLocalError) -> Self {
        Self::Store(error)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct RecoveryReceipt {
    pub checkpoint: CommitReceipt,
    pub import: RawImportReceipt,
    pub removed: usize,
    pub transferred: usize,
    pub retained: usize,
}

/// Authorized bounded-history projection without publishing retention changes.
#[derive(Debug, PartialEq, Eq)]
pub struct ProjectionReceipt {
    pub import: RawImportReceipt,
    pub removed: usize,
    pub transferred: usize,
    pub retained: usize,
}

/// Authorized dry-run for one retention sweep. The candidate size includes the
/// exact encoded RawLocal frame and is zero when no commit would be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPreview {
    pub changed: bool,
    pub candidate_frame_bytes: u64,
    pub retained_fragments: usize,
    pub retained_event_count: usize,
    pub retained_body_bytes: usize,
    pub latest_user_interaction: Option<(u64, Option<moe_agent_runtime::TerminalState>)>,
    pub has_unsettled_user_interaction: bool,
}

/// Opaque, authorized result of reading and sweeping one locked frame. The
/// plan can be consumed by a projection or retention commit on the same store,
/// avoiding a second file read and decode without bypassing CAS or authorization.
pub struct RetentionPlan {
    owner: Arc<()>,
    revision: u64,
    committed_at_ms: u64,
    now_ms: u64,
    preview: RetentionPreview,
    swept: Option<moe_agent_runtime::RawSnapshotSweep>,
    frame_bytes_read: u64,
    decoded_event_count: usize,
    decoded_body_bytes: usize,
}

impl RetentionPlan {
    pub fn preview(&self) -> RetentionPreview {
        self.preview
    }

    pub fn within_projection_budget(&self) -> bool {
        self.swept.is_some()
    }

    pub fn frame_bytes_read(&self) -> u64 {
        self.frame_bytes_read
    }

    pub fn decoded_event_count(&self) -> usize {
        self.decoded_event_count
    }

    pub fn decoded_body_bytes(&self) -> usize {
        self.decoded_body_bytes
    }
}

/// Retention result for one independently stored segment.
#[derive(Debug, PartialEq, Eq)]
pub struct RetentionReceipt {
    pub checkpoint: Option<CommitReceipt>,
    pub removed: usize,
    pub transferred: usize,
    pub retained: usize,
}

/// Durable progress of a host-authorized RawLocal segment retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawRetirementStage {
    /// The sanitized frame is still present and its segment lock is held.
    Retiring,
    /// The sanitized frame has been removed and synced under the segment lock.
    PayloadRemoved,
    /// The segment lock file has been removed under the stable path gate.
    LockRemoved,
    /// The segment directory is gone; the sibling gate remains as its receipt.
    DirectoryRemoved,
}

/// Receipt fields persisted by RawLocal's sibling gate for one retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawRetirementState {
    pub stage: RawRetirementStage,
    pub revision: u64,
    pub committed_at_ms: u64,
}

/// A path gate held while an authorized retirement removes the segment lock
/// and directory. The segment lock is acquired when the payload still exists.
pub struct RawRetirementGuard<'a> {
    root: PathBuf,
    gate: File,
    segment_lock: Option<File>,
    state: RawRetirementState,
    binding: RetirementBinding,
    access: RawLocalAccess<'a>,
}

#[derive(PartialEq, Eq)]
struct RetirementBinding {
    domain: Vec<u8>,
    authorization_revision: u64,
}

struct GateRecord {
    state: RawRetirementState,
    binding: RetirementBinding,
}

impl RetirementBinding {
    fn from_access(access: &RawLocalAccess<'_>) -> Result<Self, RawLocalError> {
        access.validate()?;
        let authorization_revision = access
            .ingress
            .stable_credential_revision(access.request.credential())
            .ok_or(RawLocalError::Gateway(GatewayError::Revoked))?;
        Ok(Self {
            domain: encode_gate_domain(&access.domain)?,
            authorization_revision,
        })
    }

    fn validate(&self, access: &RawLocalAccess<'_>) -> Result<(), RawLocalError> {
        let current = Self::from_access(access)?;
        if self.domain != current.domain {
            return Err(RawLocalError::DomainMismatch);
        }
        if self.authorization_revision != current.authorization_revision {
            return Err(RawLocalError::Gateway(GatewayError::Revoked));
        }
        Ok(())
    }
}

pub struct RawLocalAccess<'a> {
    runtime: &'a mut SessionRuntime,
    ingress: &'a TrustedIngress,
    request: &'a TrustedRequest,
    grant: &'a RawHistoryGrant,
    domain: RawDomainKey,
    now_ms: u64,
}

impl<'a> RawLocalAccess<'a> {
    pub fn new(
        runtime: &'a mut SessionRuntime,
        ingress: &'a TrustedIngress,
        request: &'a TrustedRequest,
        grant: &'a RawHistoryGrant,
        bound_domain: RawDomainKey,
        now_ms: u64,
    ) -> Result<Self, RawLocalError> {
        let current_domain = RawDomainKey::from_request(ingress, request)?;
        if current_domain != bound_domain {
            return Err(RawLocalError::DomainMismatch);
        }
        Ok(Self {
            runtime,
            ingress,
            request,
            grant,
            domain: bound_domain,
            now_ms,
        })
    }

    fn validate(&self) -> Result<(), RawLocalError> {
        validate_access(
            self.runtime,
            self.ingress,
            self.request,
            self.grant,
            &self.domain,
            self.now_ms,
        )
    }
}

impl fmt::Debug for RawLocalAccess<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawLocalAccess")
            .field("now_ms", &self.now_ms)
            .finish_non_exhaustive()
    }
}

pub struct RawLocalStore {
    root: PathBuf,
    domain: RawDomainKey,
    owner: Arc<()>,
    _lock: File,
    revision: u64,
    committed_at_ms: u64,
    baseline_grant: Option<RawHistoryGrant>,
    poisoned: bool,
    #[cfg(test)]
    fail_at: Option<FailurePoint>,
}

impl fmt::Debug for RawLocalStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawLocalStore")
            .field("revision", &self.revision)
            .field("committed_at_ms", &self.committed_at_ms)
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl RawLocalStore {
    pub fn open(
        root: impl AsRef<Path>,
        mode: OpenMode,
        access: RawLocalAccess<'_>,
    ) -> Result<OpenedStore, RawLocalError> {
        Ok(Self::open_inner(root.as_ref(), mode, access, None, None)?.opened)
    }

    /// Open an existing cold segment and prepare its bounded projection in one
    /// frame read. Opening does not perform a separate unbudgeted decode, and
    /// the plan retains the same store owner, lock, revision and authorization.
    pub fn open_projection(
        root: impl AsRef<Path>,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        max_frame_bytes: u64,
        max_events: usize,
        max_body_bytes: usize,
    ) -> Result<(Self, RetentionPlan), RawLocalError> {
        let result = Self::open_inner(
            root.as_ref(),
            OpenMode::OpenExisting,
            access,
            None,
            Some(ProjectionRead {
                retention,
                frame_bytes: max_frame_bytes,
                events: max_events,
                body_bytes: max_body_bytes,
            }),
        )?;
        Ok((
            result.opened.store,
            result
                .projection
                .expect("projection open prepares its frame"),
        ))
    }

    /// Reopen a marked segment while its retirement marker is pending. The
    /// caller still needs the current domain authorization and exact frame receipt.
    pub fn open_for_retirement(
        root: impl AsRef<Path>,
        access: RawLocalAccess<'_>,
        expected_revision: u64,
        expected_committed_at_ms: u64,
    ) -> Result<OpenedStore, RawLocalError> {
        Ok(Self::open_inner(
            root.as_ref(),
            OpenMode::OpenExisting,
            access,
            Some((expected_revision, expected_committed_at_ms)),
            None,
        )?
        .opened)
    }

    fn open_inner(
        root: &Path,
        mode: OpenMode,
        access: RawLocalAccess<'_>,
        retirement: Option<(u64, u64)>,
        projection: Option<ProjectionRead<'_>>,
    ) -> Result<OpenResult, RawLocalError> {
        let domain = access.domain.clone();
        let now_ms = access.now_ms;
        access.validate()?;
        let root = root.to_path_buf();

        match mode {
            OpenMode::CreateNew => {
                let parent = root.parent().ok_or(RawLocalError::NotFound)?;
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            OpenMode::OpenExisting => {
                if !root.is_dir() {
                    return Err(RawLocalError::NotFound);
                }
            }
        }

        let mut gate = open_gate(&root)?;
        let gate_record = read_gate_record(&mut gate)?;
        if retirement.is_some()
            && let Some(record) = &gate_record
        {
            record.binding.validate(&access)?;
        }
        let gate_state = gate_record.map(|record| record.state);
        match (retirement, gate_state) {
            (None, Some(_)) => return Err(RawLocalError::Retiring),
            (Some((revision, committed_at_ms)), Some(state))
                if state.stage == RawRetirementStage::Retiring
                    && state.revision == revision
                    && state.committed_at_ms == committed_at_ms => {}
            (Some(_), None) => {}
            (Some(_), _) => return Err(RawLocalError::Retiring),
            (None, None) => {}
        }
        let local_retiring = root.join(RETIRING_FILE);
        if path_exists(&local_retiring)? {
            let Some((revision, committed_at_ms)) = retirement else {
                return Err(RawLocalError::Retiring);
            };
            if read_local_retirement(&local_retiring)? != (revision, committed_at_ms) {
                return Err(RawLocalError::Retiring);
            }
        }

        if mode == OpenMode::CreateNew {
            fs::create_dir_all(&root).map_err(io_error)?;
        }

        let lock_path = root.join(LOCK_FILE);
        if mode == OpenMode::CreateNew && path_exists(&lock_path)? {
            return Err(RawLocalError::AlreadyExists);
        }
        let lock = match mode {
            OpenMode::CreateNew => OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(lock_path),
            OpenMode::OpenExisting => OpenOptions::new().read(true).write(true).open(lock_path),
        }
        .map_err(|error| {
            if mode == OpenMode::CreateNew && error.kind() == io::ErrorKind::AlreadyExists {
                RawLocalError::AlreadyExists
            } else {
                io_error(error)
            }
        })?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => RawLocalError::Busy,
            std::fs::TryLockError::Error(error) => io_error(error),
        })?;

        if retirement.is_none()
            && (read_gate_state(&mut gate)?.is_some() || path_exists(&local_retiring)?)
        {
            return Err(RawLocalError::Retiring);
        }

        let current_path = root.join(CURRENT_FILE);
        let pending_path = root.join(PENDING_FILE);
        if path_exists(&pending_path)? {
            return Err(RawLocalError::PendingRecoveryRequired);
        }

        let mut store = Self {
            root,
            domain: domain.clone(),
            owner: Arc::new(()),
            _lock: lock,
            revision: 0,
            committed_at_ms: 0,
            baseline_grant: None,
            poisoned: false,
            #[cfg(test)]
            fail_at: None,
        };

        match mode {
            OpenMode::CreateNew => {
                if path_exists(&current_path)? {
                    return Err(RawLocalError::AlreadyExists);
                }
                access.validate()?;
                let snapshot = access.runtime.export_raw_snapshot(
                    SessionAccess::new(access.ingress, access.request, now_ms),
                    now_ms,
                )?;
                if snapshot.domain() != &domain {
                    return Err(RawLocalError::DomainMismatch);
                }
                access.validate()?;
                let receipt = store.persist_snapshot(&snapshot, 0, now_ms, true)?;
                store.baseline_grant = Some(access.grant.clone());
                Ok(OpenResult {
                    opened: OpenedStore {
                        store,
                        create_receipt: Some(receipt),
                    },
                    projection: None,
                })
            }
            OpenMode::OpenExisting => {
                if let Some(projection) = projection {
                    let bytes = read_frame_bytes(&current_path, projection.frame_bytes)?;
                    let plan = store.plan_from_frame_bytes(
                        access,
                        projection.retention,
                        None,
                        projection.events,
                        projection.body_bytes,
                        bytes,
                    )?;
                    return Ok(OpenResult {
                        opened: OpenedStore {
                            store,
                            create_receipt: None,
                        },
                        projection: Some(plan),
                    });
                }
                access.validate()?;
                let frame = read_frame(&current_path, &domain)?;
                if let Some((revision, committed_at_ms)) = retirement
                    && (frame.revision != revision || frame.committed_at_ms != committed_at_ms)
                {
                    return Err(RawLocalError::CompareAndSwap);
                }
                if now_ms < frame.committed_at_ms {
                    return Err(RawLocalError::ClockWentBackwards);
                }
                store.revision = frame.revision;
                store.committed_at_ms = frame.committed_at_ms;
                Ok(OpenResult {
                    opened: OpenedStore {
                        store,
                        create_receipt: None,
                    },
                    projection: None,
                })
            }
        }
    }

    /// Confirm that the locked current frame is exactly the persisted, fully
    /// expired frame which may be retired. The returned receipt follows a fresh
    /// file and directory sync of those bytes.
    pub fn verify_retirable(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        expected_revision: u64,
    ) -> Result<CommitReceipt, RawLocalError> {
        self.ensure_ready()?;
        let frame = self.load_current(&access)?;
        if frame.revision != expected_revision {
            return Err(RawLocalError::CompareAndSwap);
        }
        let swept = frame
            .snapshot
            .sweep_expired(access.now_ms, retention)
            .map_err(RawLocalError::Gateway)?;
        if swept.snapshot != frame.snapshot
            || frame.snapshot.fragment_count() != 0
            || frame.snapshot.retained_body_bytes() != 0
        {
            return Err(RawLocalError::RetirementNotReady);
        }
        access.validate()?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.root.join(CURRENT_FILE))
            .and_then(|file| file.sync_all())
            .map_err(io_error)?;
        let directory_synced = sync_directory(&self.root).map_err(io_error)?;
        Ok(CommitReceipt {
            revision: frame.revision,
            committed_at_ms: frame.committed_at_ms,
            durability: if directory_synced {
                CommitDurability::FileAndDirectorySynced
            } else {
                CommitDurability::FileSyncedDirectorySyncUnavailable
            },
        })
    }

    /// Under the existing segment lock, persist a local retirement fence and
    /// remove the verified body-free frame. Standard opens reject this path
    /// before they can recreate its lock file.
    pub fn remove_retired_payload(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        receipt: CommitReceipt,
    ) -> Result<(), RawLocalError> {
        self.ensure_ready()?;
        let binding = RetirementBinding::from_access(&access)?;
        let frame = self.load_current(&access)?;
        if frame.revision != receipt.revision || frame.committed_at_ms != receipt.committed_at_ms {
            return Err(RawLocalError::CompareAndSwap);
        }
        let verified = self.verify_retirable(access, retention, receipt.revision)?;
        if verified != receipt {
            return Err(RawLocalError::CompareAndSwap);
        }
        let marker = self.root.join(RETIRING_FILE);
        if path_exists(&marker)? {
            if read_local_retirement(&marker)? != (receipt.revision, receipt.committed_at_ms) {
                return Err(RawLocalError::Retiring);
            }
        } else {
            let mut marker_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .map_err(io_error)?;
            write_local_retirement(&mut marker_file, receipt)?;
            marker_file.sync_all().map_err(io_error)?;
            sync_directory(&self.root).map_err(io_error)?;
        }

        let mut gate = open_gate(&self.root)?;
        match read_gate_record(&mut gate)? {
            None => write_gate_state(
                &mut gate,
                RawRetirementState {
                    stage: RawRetirementStage::Retiring,
                    revision: receipt.revision,
                    committed_at_ms: receipt.committed_at_ms,
                },
                &binding,
            )?,
            Some(record)
                if record.state.stage == RawRetirementStage::Retiring
                    && record.state.revision == receipt.revision
                    && record.state.committed_at_ms == receipt.committed_at_ms
                    && record.binding == binding => {}
            Some(_) => return Err(RawLocalError::Retiring),
        }
        fs::remove_file(self.root.join(CURRENT_FILE)).map_err(io_error)?;
        sync_directory(&self.root).map_err(io_error)?;
        write_gate_state(
            &mut gate,
            RawRetirementState {
                stage: RawRetirementStage::PayloadRemoved,
                revision: receipt.revision,
                committed_at_ms: receipt.committed_at_ms,
            },
            &binding,
        )?;
        Ok(())
    }

    /// Read the durable receipt associated with a RawLocal root without creating it.
    pub fn inspect_retirement(
        root: impl AsRef<Path>,
    ) -> Result<Option<RawRetirementState>, RawLocalError> {
        let path = retirement_gate_path(root.as_ref())?;
        match OpenOptions::new().read(true).open(path) {
            Ok(mut file) => read_gate_state(&mut file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(error)),
        }
    }

    /// Acquire the path gate and, when requested, the exact RawLocal segment lock.
    /// The current access must match the gate's original stable domain and
    /// authorization revision. It is revalidated after both locks are acquired
    /// and retained by the guard for every subsequent deletion step.
    pub fn lock_retirement<'a>(
        root: impl AsRef<Path>,
        access: RawLocalAccess<'a>,
        receipt: CommitReceipt,
        lock_file_expected: bool,
    ) -> Result<RawRetirementGuard<'a>, RawLocalError> {
        access.validate()?;
        let root = root.as_ref().to_path_buf();
        let mut gate = open_gate(&root)?;
        let record = read_gate_record(&mut gate)?.ok_or(RawLocalError::Retiring)?;
        record.binding.validate(&access)?;
        let state = record.state;
        if state.revision != receipt.revision
            || state.committed_at_ms != receipt.committed_at_ms
            || !matches!(
                state.stage,
                RawRetirementStage::PayloadRemoved | RawRetirementStage::LockRemoved
            )
            || !root.is_dir()
        {
            return Err(RawLocalError::Retiring);
        }
        let local_marker = root.join(RETIRING_FILE);
        let local_marker_valid = if path_exists(&local_marker)? {
            read_local_retirement(&local_marker)? == (receipt.revision, receipt.committed_at_ms)
        } else {
            state.stage == RawRetirementStage::LockRemoved
                && fs::read_dir(&root).map_err(io_error)?.next().is_none()
        };
        if !local_marker_valid
            || path_exists(&root.join(CURRENT_FILE))?
            || path_exists(&root.join(PENDING_FILE))?
        {
            return Err(RawLocalError::Retiring);
        }
        let lock_path = root.join(LOCK_FILE);
        let segment_lock = if lock_file_expected {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(io_error)?;
            lock.try_lock().map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => RawLocalError::Busy,
                std::fs::TryLockError::Error(error) => io_error(error),
            })?;
            Some(lock)
        } else {
            if path_exists(&lock_path)? {
                return Err(RawLocalError::Retiring);
            }
            None
        };
        record.binding.validate(&access)?;
        Ok(RawRetirementGuard {
            root,
            gate,
            segment_lock,
            state,
            binding: record.binding,
            access,
        })
    }

    /// Exact size of the body-free retirement gate, including its complete
    /// stable-domain binding and authorization revision. Used for peak budgets.
    pub fn retirement_gate_size(domain: &RawDomainKey) -> Result<u64, RawLocalError> {
        Ok((GATE_HEADER_BYTES + encode_gate_domain(domain)?.len()) as u64)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn committed_at_ms(&self) -> u64 {
        self.committed_at_ms
    }

    pub fn checkpoint(
        &mut self,
        access: RawLocalAccess<'_>,
        expected_revision: u64,
    ) -> Result<CommitReceipt, RawLocalError> {
        self.ensure_ready()?;
        self.authorize_checkpoint(&access)?;
        let now_ms = access.now_ms;
        self.check_revision_and_time(expected_revision, now_ms)?;
        let current = self.load_current(&access)?;
        if current.revision != expected_revision {
            self.poisoned = true;
            return Err(RawLocalError::CompareAndSwap);
        }
        self.authorize_checkpoint(&access)?;
        let snapshot = access.runtime.export_raw_snapshot(
            SessionAccess::new(access.ingress, access.request, now_ms),
            now_ms,
        )?;
        if snapshot.domain() != &self.domain {
            return Err(RawLocalError::DomainMismatch);
        }
        self.authorize_checkpoint(&access)?;
        self.persist_snapshot(&snapshot, expected_revision, now_ms, false)
    }

    pub fn restore(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        expected_revision: u64,
    ) -> Result<RecoveryReceipt, RecoveryError> {
        self.ensure_ready()?;
        self.authorize(&access)?;
        let now_ms = access.now_ms;
        self.check_revision_and_time(expected_revision, now_ms)?;
        let frame = self.load_current(&access)?;
        if frame.revision != expected_revision {
            self.poisoned = true;
            return Err(RawLocalError::CompareAndSwap.into());
        }
        self.authorize(&access)?;
        let swept = frame
            .snapshot
            .sweep_expired(now_ms, retention)
            .map_err(RawLocalError::Gateway)?;
        self.authorize(&access)?;
        let checkpoint =
            self.persist_snapshot(&swept.snapshot, expected_revision, now_ms, false)?;
        self.authorize(&access)
            .map_err(|source| RecoveryError::PostCheckpoint { checkpoint, source })?;
        let import = access
            .runtime
            .import_raw_snapshot(
                SessionAccess::new(access.ingress, access.request, now_ms),
                access.grant,
                &swept.snapshot,
            )
            .map_err(|source| RecoveryError::PostCheckpoint {
                checkpoint,
                source: RawLocalError::Gateway(source),
            })?;
        self.baseline_grant = Some(access.grant.clone());
        Ok(RecoveryReceipt {
            checkpoint,
            import,
            removed: swept.removed,
            transferred: swept.transferred,
            retained: swept.retained,
        })
    }

    /// Import an authorized, currently retained projection without writing the
    /// swept snapshot. Call `maintain_retention` separately to publish expiry.
    pub fn read_projection(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        expected_revision: u64,
    ) -> Result<ProjectionReceipt, RawLocalError> {
        let RawLocalAccess {
            runtime,
            ingress,
            request,
            grant,
            domain,
            now_ms,
        } = access;
        let plan = self.prepare_retention(
            RawLocalAccess {
                runtime: &mut *runtime,
                ingress,
                request,
                grant,
                domain: domain.clone(),
                now_ms,
            },
            retention,
            expected_revision,
        )?;
        self.read_projection_prepared(
            RawLocalAccess {
                runtime,
                ingress,
                request,
                grant,
                domain,
                now_ms,
            },
            plan,
        )
    }

    /// Read and validate one frame once, applying the event and body budgets
    /// before allocating any event bodies. The returned opaque plan can only be
    /// consumed on this same locked store and at the same revision/time.
    pub fn prepare_projection(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        expected_revision: u64,
        max_frame_bytes: u64,
        max_events: usize,
        max_body_bytes: usize,
    ) -> Result<RetentionPlan, RawLocalError> {
        self.prepare_retention_with_budget(
            access,
            retention,
            expected_revision,
            max_frame_bytes,
            max_events,
            max_body_bytes,
        )
    }

    /// Import a retained projection from its validated plan without rereading or
    /// redecoding the frame.
    pub fn read_projection_prepared(
        &mut self,
        access: RawLocalAccess<'_>,
        plan: RetentionPlan,
    ) -> Result<ProjectionReceipt, RawLocalError> {
        self.validate_plan(&access, &plan)?;
        let swept = plan.swept.ok_or(RawLocalError::CapacityExceeded)?;
        self.authorize(&access)?;
        let import = access.runtime.import_raw_snapshot(
            SessionAccess::new(access.ingress, access.request, access.now_ms),
            access.grant,
            &swept.snapshot,
        )?;
        Ok(ProjectionReceipt {
            import,
            removed: swept.removed,
            transferred: swept.transferred,
            retained: swept.retained,
        })
    }

    /// Validate authorization and CAS state, then calculate the exact frame that
    /// retention would publish without importing bodies or writing a pending file.
    pub fn preview_retention(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        expected_revision: u64,
    ) -> Result<RetentionPreview, RawLocalError> {
        Ok(self
            .prepare_retention(access, retention, expected_revision)?
            .preview())
    }

    /// Prepare an authorized retention sweep without publishing it. The plan
    /// retains the exact locked frame so a subsequent preview, projection or
    /// commit does not repeat the file read and decode.
    pub fn prepare_retention(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        expected_revision: u64,
    ) -> Result<RetentionPlan, RawLocalError> {
        self.prepare_retention_with_budget(
            access,
            retention,
            expected_revision,
            RAW_LOCAL_MAX_FRAME_BYTES as u64,
            usize::MAX,
            usize::MAX,
        )
    }

    fn prepare_retention_with_budget(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        expected_revision: u64,
        max_frame_bytes: u64,
        max_events: usize,
        max_body_bytes: usize,
    ) -> Result<RetentionPlan, RawLocalError> {
        self.ensure_ready()?;
        self.authorize(&access)?;
        let now_ms = access.now_ms;
        self.check_revision_and_time(expected_revision, now_ms)?;
        let frame_bytes = read_frame_bytes(&self.root.join(CURRENT_FILE), max_frame_bytes)?;
        self.plan_from_frame_bytes(
            access,
            retention,
            Some(expected_revision),
            max_events,
            max_body_bytes,
            frame_bytes,
        )
    }

    fn plan_from_frame_bytes(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        expected_revision: Option<u64>,
        max_events: usize,
        max_body_bytes: usize,
        frame_bytes: Vec<u8>,
    ) -> Result<RetentionPlan, RawLocalError> {
        self.ensure_ready()?;
        self.authorize(&access)?;
        let now_ms = access.now_ms;
        let frame_bytes_read =
            u64::try_from(frame_bytes.len()).map_err(|_| RawLocalError::CapacityExceeded)?;
        let parts = decode_frame_parts(&frame_bytes)?;
        if expected_revision.is_some_and(|revision| {
            parts.revision != revision
                || parts.revision != self.revision
                || parts.committed_at_ms != self.committed_at_ms
        }) {
            self.poisoned = true;
            return Err(RawLocalError::CompareAndSwap);
        }
        if now_ms < parts.committed_at_ms {
            return Err(RawLocalError::ClockWentBackwards);
        }
        let decoded = RawSnapshotV1::decode_bounded(parts.payload, max_events, max_body_bytes)
            .map_err(RawLocalError::Gateway)?;
        if decoded.domain() != &self.domain {
            return Err(RawLocalError::DomainMismatch);
        }
        let latest_user_interaction = decoded.latest_user_interaction();
        let has_unsettled_user_interaction = decoded.has_unsettled_user_interaction();
        let event_count = decoded.event_count();
        let decoded_event_count = decoded.decoded_event_count();
        let body_bytes = decoded.body_bytes();
        let decoded_body_bytes = decoded.decoded_body_bytes();
        access.validate()?;
        if expected_revision.is_none() {
            self.revision = parts.revision;
            self.committed_at_ms = parts.committed_at_ms;
        }
        let Some(snapshot) = decoded.into_snapshot() else {
            return Ok(RetentionPlan {
                owner: self.owner.clone(),
                revision: parts.revision,
                committed_at_ms: parts.committed_at_ms,
                now_ms,
                preview: RetentionPreview {
                    changed: false,
                    candidate_frame_bytes: 0,
                    retained_fragments: 0,
                    retained_event_count: event_count,
                    retained_body_bytes: body_bytes,
                    latest_user_interaction,
                    has_unsettled_user_interaction,
                },
                swept: None,
                frame_bytes_read,
                decoded_event_count,
                decoded_body_bytes,
            });
        };
        let swept = snapshot
            .sweep_expired(now_ms, retention)
            .map_err(RawLocalError::Gateway)?;
        self.authorize(&access)?;
        let changed = swept.snapshot != snapshot;
        let retained_event_count = swept.snapshot.retained_event_count();
        let retained_body_bytes = swept.snapshot.retained_body_bytes();
        let candidate_frame_bytes = if changed {
            let next_revision = parts
                .revision
                .checked_add(1)
                .ok_or(RawLocalError::RevisionExhausted)?;
            let payload = swept.snapshot.encode().map_err(RawLocalError::Gateway)?;
            u64::try_from(encode_frame(next_revision, now_ms, &payload)?.len())
                .map_err(|_| RawLocalError::CapacityExceeded)?
        } else {
            0
        };
        Ok(RetentionPlan {
            owner: self.owner.clone(),
            revision: parts.revision,
            committed_at_ms: parts.committed_at_ms,
            now_ms,
            preview: RetentionPreview {
                changed,
                candidate_frame_bytes,
                retained_fragments: swept.snapshot.fragment_count(),
                retained_event_count,
                retained_body_bytes,
                latest_user_interaction,
                has_unsettled_user_interaction,
            },
            swept: Some(swept),
            frame_bytes_read,
            decoded_event_count,
            decoded_body_bytes,
        })
    }

    /// Apply the existing bounded retention rules to one segment. A no-op sweep
    /// does not rewrite the frame or advance its revision.
    pub fn maintain_retention(
        &mut self,
        access: RawLocalAccess<'_>,
        retention: &RawRetention,
        expected_revision: u64,
    ) -> Result<RetentionReceipt, RawLocalError> {
        let RawLocalAccess {
            runtime,
            ingress,
            request,
            grant,
            domain,
            now_ms,
        } = access;
        let plan = self.prepare_retention(
            RawLocalAccess {
                runtime: &mut *runtime,
                ingress,
                request,
                grant,
                domain: domain.clone(),
                now_ms,
            },
            retention,
            expected_revision,
        )?;
        self.maintain_retention_prepared(
            RawLocalAccess {
                runtime,
                ingress,
                request,
                grant,
                domain,
                now_ms,
            },
            plan,
        )
    }

    /// Commit a previously prepared retention sweep using its same locked CAS
    /// revision and authorization recheck.
    pub fn maintain_retention_prepared(
        &mut self,
        access: RawLocalAccess<'_>,
        plan: RetentionPlan,
    ) -> Result<RetentionReceipt, RawLocalError> {
        self.validate_plan(&access, &plan)?;
        let swept = plan.swept.ok_or(RawLocalError::CapacityExceeded)?;
        let checkpoint = if plan.preview.changed {
            Some(self.persist_snapshot(&swept.snapshot, plan.revision, plan.now_ms, false)?)
        } else {
            None
        };
        Ok(RetentionReceipt {
            checkpoint,
            removed: swept.removed,
            transferred: swept.transferred,
            retained: swept.retained,
        })
    }

    fn validate_plan(
        &self,
        access: &RawLocalAccess<'_>,
        plan: &RetentionPlan,
    ) -> Result<(), RawLocalError> {
        self.ensure_ready()?;
        self.authorize(access)?;
        if !Arc::ptr_eq(&self.owner, &plan.owner)
            || self.revision != plan.revision
            || self.committed_at_ms != plan.committed_at_ms
            || access.now_ms != plan.now_ms
        {
            return Err(RawLocalError::CompareAndSwap);
        }
        self.check_revision_and_time(plan.revision, plan.now_ms)?;
        access.validate()
    }

    fn ensure_ready(&self) -> Result<(), RawLocalError> {
        if self.poisoned {
            Err(RawLocalError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn authorize(&self, access: &RawLocalAccess<'_>) -> Result<(), RawLocalError> {
        if access.domain != self.domain {
            return Err(RawLocalError::DomainMismatch);
        }
        access.validate()
    }

    fn authorize_checkpoint(&self, access: &RawLocalAccess<'_>) -> Result<(), RawLocalError> {
        self.authorize(access)?;
        let grant = self
            .baseline_grant
            .as_ref()
            .ok_or(RawLocalError::BaselineNotRestored)?;
        validate_access(
            access.runtime,
            access.ingress,
            access.request,
            grant,
            &self.domain,
            access.now_ms,
        )
    }

    fn check_revision_and_time(
        &self,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<(), RawLocalError> {
        if expected_revision != self.revision {
            return Err(RawLocalError::CompareAndSwap);
        }
        if now_ms < self.committed_at_ms {
            return Err(RawLocalError::ClockWentBackwards);
        }
        Ok(())
    }

    fn load_current(&mut self, access: &RawLocalAccess<'_>) -> Result<Frame, RawLocalError> {
        self.authorize(access)?;
        let frame = read_frame(&self.root.join(CURRENT_FILE), &self.domain)?;
        if frame.revision != self.revision || frame.committed_at_ms != self.committed_at_ms {
            self.poisoned = true;
            return Err(RawLocalError::CompareAndSwap);
        }
        if access.now_ms < frame.committed_at_ms {
            return Err(RawLocalError::ClockWentBackwards);
        }
        Ok(frame)
    }

    fn persist_snapshot(
        &mut self,
        snapshot: &RawSnapshotV1,
        expected_revision: u64,
        committed_at_ms: u64,
        create_new: bool,
    ) -> Result<CommitReceipt, RawLocalError> {
        self.ensure_ready()?;
        if snapshot.domain() != &self.domain {
            return Err(RawLocalError::DomainMismatch);
        }
        if !create_new {
            self.check_revision_and_time(expected_revision, committed_at_ms)?;
        } else if expected_revision != 0 || self.revision != 0 {
            return Err(RawLocalError::CompareAndSwap);
        }
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or(RawLocalError::RevisionExhausted)?;
        let payload = snapshot.encode().map_err(RawLocalError::Gateway)?;
        let frame = encode_frame(next_revision, committed_at_ms, &payload)?;
        let pending_path = self.root.join(PENDING_FILE);
        let current_path = self.root.join(CURRENT_FILE);
        self.poisoned = true;
        let mut pending = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending_path)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    RawLocalError::PendingRecoveryRequired
                } else {
                    RawLocalError::CommitOutcomeUnknown(CommitStage::CreatePending)
                }
            })?;
        #[cfg(test)]
        if self.fail_at == Some(FailurePoint::PartialWrite) {
            let partial_len = (frame.len() / 2).max(1);
            pending
                .write_all(&frame[..partial_len])
                .map_err(|_| RawLocalError::CommitOutcomeUnknown(CommitStage::WritePending))?;
            return Err(RawLocalError::CommitOutcomeUnknown(
                CommitStage::WritePending,
            ));
        }
        #[cfg(test)]
        if self.fail_at == Some(FailurePoint::Write) {
            return Err(RawLocalError::CommitOutcomeUnknown(
                CommitStage::WritePending,
            ));
        }
        pending
            .write_all(&frame)
            .map_err(|_| RawLocalError::CommitOutcomeUnknown(CommitStage::WritePending))?;
        #[cfg(test)]
        if self.fail_at == Some(FailurePoint::FileSync) {
            return Err(RawLocalError::CommitOutcomeUnknown(
                CommitStage::SyncPending,
            ));
        }
        pending
            .sync_all()
            .map_err(|_| RawLocalError::CommitOutcomeUnknown(CommitStage::SyncPending))?;
        drop(pending);
        #[cfg(test)]
        if self.fail_at == Some(FailurePoint::Replace) {
            return Err(RawLocalError::CommitOutcomeUnknown(
                CommitStage::ReplaceCurrent,
            ));
        }
        fs::rename(&pending_path, &current_path)
            .map_err(|_| RawLocalError::CommitOutcomeUnknown(CommitStage::ReplaceCurrent))?;
        #[cfg(test)]
        if self.fail_at == Some(FailurePoint::DirectorySync) {
            return Err(RawLocalError::CommitOutcomeUnknown(
                CommitStage::SyncDirectory,
            ));
        }
        let directory_synced = sync_directory(&self.root)
            .map_err(|_| RawLocalError::CommitOutcomeUnknown(CommitStage::SyncDirectory))?;
        self.revision = next_revision;
        self.committed_at_ms = committed_at_ms;
        self.poisoned = false;
        Ok(CommitReceipt {
            revision: next_revision,
            committed_at_ms,
            durability: if directory_synced {
                CommitDurability::FileAndDirectorySynced
            } else {
                CommitDurability::FileSyncedDirectorySyncUnavailable
            },
        })
    }
}

impl RawRetirementGuard<'_> {
    /// Release and remove the segment lock file while the stable path gate is held.
    pub fn remove_lock_file(&mut self) -> Result<(), RawLocalError> {
        self.binding.validate(&self.access)?;
        if self.state.stage != RawRetirementStage::PayloadRemoved || self.segment_lock.is_none() {
            return Err(RawLocalError::Retiring);
        }
        self.segment_lock.take();
        fs::remove_file(self.root.join(LOCK_FILE)).map_err(io_error)?;
        sync_directory(&self.root).map_err(io_error)?;
        Ok(())
    }

    /// Persist the completed lock-file removal in the sibling gate receipt.
    pub fn mark_lock_removed(&mut self) -> Result<(), RawLocalError> {
        self.binding.validate(&self.access)?;
        if self.state.stage != RawRetirementStage::PayloadRemoved
            || self.segment_lock.is_some()
            || path_exists(&self.root.join(LOCK_FILE))?
        {
            return Err(RawLocalError::Retiring);
        }
        self.state.stage = RawRetirementStage::LockRemoved;
        write_gate_state(&mut self.gate, self.state, &self.binding)
    }

    /// Remove a directory containing only its matching retirement fence, then
    /// persist the completed directory-removal receipt in the sibling gate.
    pub fn remove_segment_directory(&mut self) -> Result<(), RawLocalError> {
        self.binding.validate(&self.access)?;
        if self.state.stage != RawRetirementStage::LockRemoved
            || self.segment_lock.is_some()
            || path_exists(&self.root.join(LOCK_FILE))?
            || path_exists(&self.root.join(CURRENT_FILE))?
            || path_exists(&self.root.join(PENDING_FILE))?
        {
            return Err(RawLocalError::Retiring);
        }
        let marker = self.root.join(RETIRING_FILE);
        if path_exists(&marker)? {
            if read_local_retirement(&marker)? != (self.state.revision, self.state.committed_at_ms)
            {
                return Err(RawLocalError::Retiring);
            }
            let mut entries = fs::read_dir(&self.root).map_err(io_error)?;
            if entries.any(|entry| {
                entry
                    .map(|entry| entry.file_name() != RETIRING_FILE)
                    .unwrap_or(true)
            }) {
                return Err(RawLocalError::Retiring);
            }
            fs::remove_file(marker).map_err(io_error)?;
        } else if fs::read_dir(&self.root).map_err(io_error)?.next().is_some() {
            return Err(RawLocalError::Retiring);
        }
        fs::remove_dir(&self.root).map_err(io_error)?;
        if let Some(parent) = self.root.parent() {
            sync_directory(parent).map_err(io_error)?;
        }
        self.state.stage = RawRetirementStage::DirectoryRemoved;
        write_gate_state(&mut self.gate, self.state, &self.binding)
    }
}

const LOCAL_RETIREMENT_MAGIC: &[u8; 8] = b"MOERLT01";

fn retirement_gate_path(root: &Path) -> Result<PathBuf, RawLocalError> {
    let parent = root.parent().ok_or(RawLocalError::NotFound)?;
    let basename = root.file_name().ok_or(RawLocalError::NotFound)?;
    let mut name = std::ffi::OsString::from(".");
    name.push(basename);
    name.push(GATE_SUFFIX);
    Ok(parent.join(name))
}

fn open_gate(root: &Path) -> Result<File, RawLocalError> {
    let path = retirement_gate_path(root)?;
    let gate = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => {
            if let Some(parent) = path.parent() {
                sync_directory(parent).map_err(io_error)?;
            }
            file
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(io_error)?,
        Err(error) => return Err(io_error(error)),
    };
    gate.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => RawLocalError::Busy,
        std::fs::TryLockError::Error(error) => io_error(error),
    })?;
    Ok(gate)
}

fn read_gate_state(file: &mut File) -> Result<Option<RawRetirementState>, RawLocalError> {
    Ok(read_gate_record(file)?.map(|record| record.state))
}

fn encode_gate_domain(domain: &RawDomainKey) -> Result<Vec<u8>, RawLocalError> {
    let mut bytes = Vec::new();
    for field in [
        domain.identity().namespace().workspace_id(),
        domain.identity().namespace().user_id(),
        domain.identity().instance_id(),
        domain.subject().as_str(),
        domain.channel().as_str(),
        domain.scope().as_str(),
        domain.credential(),
        domain.hint(),
    ] {
        let length = u32::try_from(field.len()).map_err(|_| RawLocalError::CapacityExceeded)?;
        if bytes
            .len()
            .checked_add(4 + field.len())
            .is_none_or(|len| len > MAX_GATE_BYTES - GATE_HEADER_BYTES)
        {
            return Err(RawLocalError::CapacityExceeded);
        }
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(field.as_bytes());
    }
    Ok(bytes)
}

fn read_gate_record(file: &mut File) -> Result<Option<GateRecord>, RawLocalError> {
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take((MAX_GATE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() < GATE_HEADER_BYTES || bytes.len() > MAX_GATE_BYTES || &bytes[..8] != GATE_MAGIC
    {
        return Err(RawLocalError::Retiring);
    }
    let domain_length =
        u32::from_le_bytes(bytes[33..37].try_into().expect("fixed gate field")) as usize;
    if domain_length == 0 || domain_length != bytes.len() - GATE_HEADER_BYTES {
        return Err(RawLocalError::Retiring);
    }
    let stage = match bytes[8] {
        1 => RawRetirementStage::Retiring,
        2 => RawRetirementStage::PayloadRemoved,
        3 => RawRetirementStage::LockRemoved,
        4 => RawRetirementStage::DirectoryRemoved,
        _ => return Err(RawLocalError::Retiring),
    };
    Ok(Some(GateRecord {
        state: RawRetirementState {
            stage,
            revision: u64::from_le_bytes(bytes[9..17].try_into().expect("fixed gate field")),
            committed_at_ms: u64::from_le_bytes(
                bytes[17..25].try_into().expect("fixed gate field"),
            ),
        },
        binding: RetirementBinding {
            authorization_revision: u64::from_le_bytes(
                bytes[25..33].try_into().expect("fixed gate field"),
            ),
            domain: bytes[GATE_HEADER_BYTES..].to_vec(),
        },
    }))
}

fn write_gate_state(
    file: &mut File,
    state: RawRetirementState,
    binding: &RetirementBinding,
) -> Result<(), RawLocalError> {
    let stage = match state.stage {
        RawRetirementStage::Retiring => 1,
        RawRetirementStage::PayloadRemoved => 2,
        RawRetirementStage::LockRemoved => 3,
        RawRetirementStage::DirectoryRemoved => 4,
    };
    file.set_len(0).map_err(io_error)?;
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    file.write_all(GATE_MAGIC).map_err(io_error)?;
    file.write_all(&[stage]).map_err(io_error)?;
    file.write_all(&state.revision.to_le_bytes())
        .map_err(io_error)?;
    file.write_all(&state.committed_at_ms.to_le_bytes())
        .map_err(io_error)?;
    file.write_all(&binding.authorization_revision.to_le_bytes())
        .map_err(io_error)?;
    file.write_all(&(binding.domain.len() as u32).to_le_bytes())
        .map_err(io_error)?;
    file.write_all(&binding.domain).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

fn write_local_retirement(file: &mut File, receipt: CommitReceipt) -> Result<(), RawLocalError> {
    file.write_all(LOCAL_RETIREMENT_MAGIC).map_err(io_error)?;
    file.write_all(&receipt.revision.to_le_bytes())
        .map_err(io_error)?;
    file.write_all(&receipt.committed_at_ms.to_le_bytes())
        .map_err(io_error)
}

fn read_local_retirement(path: &Path) -> Result<(u64, u64), RawLocalError> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(io_error)?;
    if bytes.len() != 24 || &bytes[..8] != LOCAL_RETIREMENT_MAGIC {
        return Err(RawLocalError::Retiring);
    }
    Ok((
        u64::from_le_bytes(bytes[8..16].try_into().expect("fixed retirement field")),
        u64::from_le_bytes(bytes[16..24].try_into().expect("fixed retirement field")),
    ))
}

fn validate_access(
    runtime: &SessionRuntime,
    ingress: &TrustedIngress,
    request: &TrustedRequest,
    grant: &RawHistoryGrant,
    domain: &RawDomainKey,
    now_ms: u64,
) -> Result<(), RawLocalError> {
    runtime
        .validate_raw_history_access(SessionAccess::new(ingress, request, now_ms), grant, domain)
        .map_err(RawLocalError::Gateway)
}

struct Frame {
    revision: u64,
    committed_at_ms: u64,
    snapshot: RawSnapshotV1,
}

struct FrameParts<'a> {
    revision: u64,
    committed_at_ms: u64,
    payload: &'a [u8],
}

fn encode_frame(
    revision: u64,
    committed_at_ms: u64,
    payload: &[u8],
) -> Result<Vec<u8>, RawLocalError> {
    if payload.len() > moe_agent_runtime::RAW_SNAPSHOT_MAX_TOTAL_BYTES {
        return Err(RawLocalError::CapacityExceeded);
    }
    let payload_len = u64::try_from(payload.len()).map_err(|_| RawLocalError::CapacityExceeded)?;
    let total_len = FRAME_OVERHEAD_BYTES
        .checked_add(payload.len())
        .ok_or(RawLocalError::CapacityExceeded)?;
    if total_len > RAW_LOCAL_MAX_FRAME_BYTES {
        return Err(RawLocalError::CapacityExceeded);
    }
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(total_len)
        .map_err(|_| RawLocalError::CapacityExceeded)?;
    frame.extend_from_slice(FRAME_MAGIC);
    frame.extend_from_slice(&FRAME_VERSION.to_le_bytes());
    frame.extend_from_slice(&revision.to_le_bytes());
    frame.extend_from_slice(&committed_at_ms.to_le_bytes());
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.extend_from_slice(payload);
    let checksum = fnv1a64(&frame);
    frame.extend_from_slice(&checksum.to_le_bytes());
    Ok(frame)
}

fn decode_frame(bytes: &[u8], domain: &RawDomainKey) -> Result<Frame, RawLocalError> {
    let parts = decode_frame_parts(bytes)?;
    let snapshot = RawSnapshotV1::decode(parts.payload).map_err(RawLocalError::Gateway)?;
    if snapshot.domain() != domain {
        return Err(RawLocalError::DomainMismatch);
    }
    if snapshot.committed_at_ms() > parts.committed_at_ms {
        return Err(RawLocalError::CorruptFrame);
    }
    Ok(Frame {
        revision: parts.revision,
        committed_at_ms: parts.committed_at_ms,
        snapshot,
    })
}

fn decode_frame_parts(bytes: &[u8]) -> Result<FrameParts<'_>, RawLocalError> {
    if bytes.len() > RAW_LOCAL_MAX_FRAME_BYTES {
        return Err(RawLocalError::CapacityExceeded);
    }
    if bytes.len() < FRAME_OVERHEAD_BYTES {
        return Err(RawLocalError::CorruptFrame);
    }
    if &bytes[..8] != FRAME_MAGIC {
        return Err(RawLocalError::CorruptFrame);
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("fixed-width frame field"));
    if version != FRAME_VERSION {
        return Err(RawLocalError::UnsupportedVersion);
    }
    let revision = u64::from_le_bytes(bytes[12..20].try_into().expect("fixed-width frame field"));
    if revision == 0 {
        return Err(RawLocalError::CorruptFrame);
    }
    let committed_at_ms =
        u64::from_le_bytes(bytes[20..28].try_into().expect("fixed-width frame field"));
    let payload_len_u64 =
        u64::from_le_bytes(bytes[28..36].try_into().expect("fixed-width frame field"));
    let payload_len =
        usize::try_from(payload_len_u64).map_err(|_| RawLocalError::CapacityExceeded)?;
    let expected_len = FRAME_OVERHEAD_BYTES
        .checked_add(payload_len)
        .ok_or(RawLocalError::CapacityExceeded)?;
    if payload_len > moe_agent_runtime::RAW_SNAPSHOT_MAX_TOTAL_BYTES || expected_len != bytes.len()
    {
        return Err(RawLocalError::CorruptFrame);
    }
    let checksum_offset = FRAME_HEADER_BYTES + payload_len;
    let stored_checksum = u64::from_le_bytes(
        bytes[checksum_offset..checksum_offset + FRAME_TRAILER_BYTES]
            .try_into()
            .expect("frame length was validated"),
    );
    if fnv1a64(&bytes[..checksum_offset]) != stored_checksum {
        return Err(RawLocalError::CorruptFrame);
    }
    let payload = &bytes[FRAME_HEADER_BYTES..checksum_offset];
    if payload.len() < 24 {
        return Err(RawLocalError::Gateway(
            moe_agent_runtime::GatewayError::RawSnapshotInvalid("truncated header"),
        ));
    }
    let snapshot_committed_at_ms = u64::from_le_bytes(
        payload[16..24]
            .try_into()
            .expect("snapshot header was validated"),
    );
    if snapshot_committed_at_ms > committed_at_ms {
        return Err(RawLocalError::CorruptFrame);
    }
    Ok(FrameParts {
        revision,
        committed_at_ms,
        payload,
    })
}

#[cfg(test)]
thread_local! {
    static FRAME_BYTES_READ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn read_frame_bytes(path: &Path, max_bytes: u64) -> Result<Vec<u8>, RawLocalError> {
    let mut file = File::open(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RawLocalError::NotFound
        } else {
            io_error(error)
        }
    })?;
    let metadata = file.metadata().map_err(io_error)?;
    let file_len = usize::try_from(metadata.len()).map_err(|_| RawLocalError::CapacityExceeded)?;
    let max_bytes = usize::try_from(max_bytes)
        .unwrap_or(usize::MAX)
        .min(RAW_LOCAL_MAX_FRAME_BYTES);
    if file_len > max_bytes {
        return Err(RawLocalError::CapacityExceeded);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(file_len)
        .map_err(|_| RawLocalError::CapacityExceeded)?;
    Read::by_ref(&mut file)
        .take(file_len as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    #[cfg(test)]
    FRAME_BYTES_READ.with(|count| count.set(count.get() + bytes.len() as u64));
    if bytes.len() != file_len || file.metadata().map_err(io_error)?.len() != file_len as u64 {
        return Err(RawLocalError::CorruptFrame);
    }
    Ok(bytes)
}

fn read_frame(path: &Path, domain: &RawDomainKey) -> Result<Frame, RawLocalError> {
    let bytes = read_frame_bytes(path, RAW_LOCAL_MAX_FRAME_BYTES as u64)?;
    decode_frame(&bytes, domain)
}

fn path_exists(path: &Path) -> Result<bool, RawLocalError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(error)),
    }
}

fn sync_directory(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(false)
    }
}

fn io_error(error: io::Error) -> RawLocalError {
    RawLocalError::Io(error.kind())
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailurePoint {
    PartialWrite,
    Write,
    FileSync,
    Replace,
    DirectorySync,
}

#[cfg(test)]
mod tests;
