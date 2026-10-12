use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

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
        let domain = access.domain.clone();
        let now_ms = access.now_ms;
        access.validate()?;
        let root = root.as_ref().to_path_buf();

        match mode {
            OpenMode::CreateNew => fs::create_dir_all(&root).map_err(io_error)?,
            OpenMode::OpenExisting => {
                if !root.is_dir() {
                    return Err(RawLocalError::NotFound);
                }
            }
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

        let current_path = root.join(CURRENT_FILE);
        let pending_path = root.join(PENDING_FILE);
        if path_exists(&pending_path)? {
            return Err(RawLocalError::PendingRecoveryRequired);
        }

        let mut store = Self {
            root,
            domain: domain.clone(),
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
                Ok(OpenedStore {
                    store,
                    create_receipt: Some(receipt),
                })
            }
            OpenMode::OpenExisting => {
                access.validate()?;
                let frame = read_frame(&current_path, &domain)?;
                if now_ms < frame.committed_at_ms {
                    return Err(RawLocalError::ClockWentBackwards);
                }
                store.revision = frame.revision;
                store.committed_at_ms = frame.committed_at_ms;
                Ok(OpenedStore {
                    store,
                    create_receipt: None,
                })
            }
        }
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
    let snapshot = RawSnapshotV1::decode(&bytes[FRAME_HEADER_BYTES..checksum_offset])
        .map_err(RawLocalError::Gateway)?;
    if snapshot.domain() != domain {
        return Err(RawLocalError::DomainMismatch);
    }
    if snapshot.committed_at_ms() > committed_at_ms {
        return Err(RawLocalError::CorruptFrame);
    }
    Ok(Frame {
        revision,
        committed_at_ms,
        snapshot,
    })
}

fn read_frame(path: &Path, domain: &RawDomainKey) -> Result<Frame, RawLocalError> {
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RawLocalError::NotFound
        } else {
            io_error(error)
        }
    })?;
    let file_len = usize::try_from(metadata.len()).map_err(|_| RawLocalError::CapacityExceeded)?;
    if file_len > RAW_LOCAL_MAX_FRAME_BYTES {
        return Err(RawLocalError::CapacityExceeded);
    }
    let mut file = File::open(path).map_err(io_error)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(file_len)
        .map_err(|_| RawLocalError::CapacityExceeded)?;
    let read_limit = u64::try_from(RAW_LOCAL_MAX_FRAME_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    Read::by_ref(&mut file)
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > RAW_LOCAL_MAX_FRAME_BYTES {
        return Err(RawLocalError::CapacityExceeded);
    }
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
