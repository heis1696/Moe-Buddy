use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use moe_agent_runtime::{
    EventRole, ManagementPermissions, RawDomainKey, SessionAccess, SessionLimits, SessionRuntime,
    SessionTiming, TerminalState,
};
use moe_chat_app::{
    AppError, ChatHistory, ChatMessage, ChatStatus, ColdHistoryMaintenance, ColdMaintenanceBlock,
    DirectoryPublishReceipt, HistoryAccess, HistoryChange, HistoryCommitReceipts, HistoryRecovery,
    HistoryRequest, HistorySendReservation, HistorySession, MessageRole, RawHistoryCommitReceipt,
};
use moe_raw_local::{
    CommitDurability, CommitReceipt, OpenMode, RawLocalAccess, RawLocalStore,
    RawRetirementStage as RawStage, RawRetirementState, RetentionPreview,
};

const DIRECTORY_FILE: &str = "sessions.v1";
const DIRECTORY_PENDING: &str = "sessions.pending";
const DIRECTORY_LOCK: &str = ".sessions.lock";
const GLOBAL_BUDDY_LOCK: &str = ".buddy-history.lock";
const DIRECTORY_VERSION: u64 = 2;
const SEGMENTS_DIR: &str = "segments";
const MAX_SESSIONS: usize = 64;
const MAX_SEGMENTS: usize = 256;
const MAX_ROOT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DIRECTORY_BYTES: u64 = 1024 * 1024;
const MAX_HISTORY_MESSAGES: usize = 128;
const MAX_HISTORY_SEGMENTS_PER_READ: usize = 64;
const MAX_HISTORY_EVENTS_PER_READ: usize = 512;
const MAX_HISTORY_BODY_BYTES_PER_READ: usize = 1_048_576;
const MAX_HISTORY_FRAME_BYTES_PER_READ: u64 = 1_048_576;
const RAW_FRAME_OVERHEAD: u64 = 44;
const CANDIDATE_MARKER_BYTES: u64 = 20;
const LOCAL_RETIREMENT_MARKER_BYTES: u64 = 24;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone)]
struct Segment {
    id: String,
    path: String,
    sealed: bool,
}

#[derive(Clone)]
struct Session {
    id: String,
    label: String,
    raw_hint: String,
    archived: bool,
    status: ChatStatus,
    external_unknown: bool,
    recovered_unsettled: bool,
    active_segment: Option<String>,
    segments: Vec<Segment>,
}

#[derive(Clone)]
struct Directory {
    revision: u64,
    active_session_id: String,
    next_segment_sequence: u64,
    retiring_segments: Vec<Retirement>,
    sessions: Vec<Session>,
}

#[derive(Clone, PartialEq, Eq)]
struct StableDomainRecord {
    workspace_id: String,
    user_id: String,
    instance_id: String,
    subject: String,
    channel: String,
    scope: String,
    credential: String,
    hint: String,
}

impl StableDomainRecord {
    fn from_domain(domain: &RawDomainKey) -> Self {
        Self {
            workspace_id: domain.identity().namespace().workspace_id().to_owned(),
            user_id: domain.identity().namespace().user_id().to_owned(),
            instance_id: domain.identity().instance_id().to_owned(),
            subject: domain.subject().as_str().to_owned(),
            channel: domain.channel().as_str().to_owned(),
            scope: domain.scope().as_str().to_owned(),
            credential: domain.credential().to_owned(),
            hint: domain.hint().to_owned(),
        }
    }

    fn encode(&self) -> serde_json::Value {
        serde_json::json!({
            "workspaceId": self.workspace_id,
            "userId": self.user_id,
            "instanceId": self.instance_id,
            "subject": self.subject,
            "channel": self.channel,
            "scope": self.scope,
            "credential": self.credential,
            "hint": self.hint,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetirementStage {
    Marked,
    PayloadRemoved,
    LockRemoved,
    DirectoryRemoved,
}

impl RetirementStage {
    fn text(self) -> &'static str {
        match self {
            Self::Marked => "marked",
            Self::PayloadRemoved => "payload_removed",
            Self::LockRemoved => "lock_removed",
            Self::DirectoryRemoved => "directory_removed",
        }
    }

    fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "marked" => Ok(Self::Marked),
            "payload_removed" => Ok(Self::PayloadRemoved),
            "lock_removed" => Ok(Self::LockRemoved),
            "directory_removed" => Ok(Self::DirectoryRemoved),
            _ => Err(AppError::from_storage(HistoryFailure(
                "retirement stage is invalid",
            ))),
        }
    }
}

#[derive(Clone)]
struct Retirement {
    id: String,
    session_id: String,
    domain: StableDomainRecord,
    authorization_revision: u64,
    frame: RawHistoryCommitReceipt,
    stage: RetirementStage,
}

struct SegmentProjection {
    events: Vec<moe_agent_runtime::StagedEvent>,
    latest_user_interaction: Option<(u64, Option<TerminalState>)>,
    has_unsettled_user_interaction: bool,
    frame_bytes_read: u64,
    decoded_event_count: usize,
    decoded_body_bytes: usize,
    body_loaded: bool,
}

struct SendPeakContext<'a> {
    directory: &'a Directory,
    session_id: &'a str,
    current_total: u64,
    current_raw_bytes: u64,
    current_directory_bytes: u64,
    base_frame_bytes: u64,
    online: bool,
}

struct RetirementTarget<'a> {
    session_id: &'a str,
    segment: &'a Segment,
}

pub(super) struct DiskHistory {
    root: PathBuf,
    budget_root: PathBuf,
    lock: Option<File>,
    global_lock: Option<File>,
    shared_global_lock: bool,
    directory: Option<Directory>,
    active_store: Option<RawLocalStore>,
    active_segment_id: Option<String>,
    cold_cursor: usize,
    poisoned: bool,
    last_receipts: HistoryCommitReceipts,
}

impl DiskHistory {
    pub(super) fn new(root: PathBuf) -> Self {
        Self::new_scoped(root.clone(), root)
    }

    pub(super) fn new_scoped(root: PathBuf, budget_root: PathBuf) -> Self {
        Self {
            root,
            budget_root,
            lock: None,
            global_lock: None,
            shared_global_lock: false,
            directory: None,
            active_store: None,
            active_segment_id: None,
            cold_cursor: 0,
            poisoned: false,
            last_receipts: HistoryCommitReceipts::default(),
        }
    }

    pub(super) fn new_scoped_with_shared_root_lock(root: PathBuf, budget_root: PathBuf) -> Self {
        let mut history = Self::new_scoped(root, budget_root);
        history.shared_global_lock = true;
        history
    }

    fn ready(&self) -> Result<(), AppError> {
        if self.poisoned
            || self.directory.is_none()
            || self.lock.is_none()
            || (self.global_lock.is_none() && !self.shared_global_lock)
        {
            Err(AppError::from_storage(HistoryFailure(
                "session directory requires recovery",
            )))
        } else {
            Ok(())
        }
    }

    fn directory(&self) -> Result<&Directory, AppError> {
        self.directory
            .as_ref()
            .ok_or_else(|| AppError::from_storage(HistoryFailure("session directory is not open")))
    }

    fn recovery(&self, active_history: Vec<ChatMessage>) -> Result<HistoryRecovery, AppError> {
        let directory = self.directory()?;
        Ok(HistoryRecovery {
            sessions_revision: directory.revision,
            active_session_id: directory.active_session_id.clone(),
            active_storage_hint: directory
                .sessions
                .iter()
                .find(|session| session.id == directory.active_session_id)
                .map(|session| session.raw_hint.clone())
                .ok_or_else(|| {
                    AppError::from_storage(HistoryFailure("active session is missing"))
                })?,
            active_external_unknown: directory
                .sessions
                .iter()
                .find(|session| session.id == directory.active_session_id)
                .is_some_and(|session| session.external_unknown),
            active_recovered_unsettled: directory
                .sessions
                .iter()
                .find(|session| session.id == directory.active_session_id)
                .is_some_and(|session| session.recovered_unsettled),
            sessions: directory
                .sessions
                .iter()
                .map(|session| HistorySession {
                    id: session.id.clone(),
                    label: session.label.clone(),
                    archived: session.archived,
                    status: session.status,
                })
                .collect(),
            active_history,
            receipts: HistoryCommitReceipts::default(),
        })
    }

    fn open_directory_inner(&mut self) -> Result<HistoryRecovery, AppError> {
        self.last_receipts = HistoryCommitReceipts::default();
        fs::create_dir_all(&self.budget_root).map_err(AppError::from_storage)?;
        fs::create_dir_all(&self.root).map_err(AppError::from_storage)?;
        if !self.shared_global_lock {
            let global_lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.budget_root.join(GLOBAL_BUDDY_LOCK))
                .map_err(AppError::from_storage)?;
            global_lock.try_lock().map_err(|error| {
                AppError::from_storage(HistoryFailure(match error {
                    std::fs::TryLockError::WouldBlock => "buddy history root is locked",
                    std::fs::TryLockError::Error(_) => "buddy history root lock failed",
                }))
            })?;
            self.global_lock = Some(global_lock);
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(DIRECTORY_LOCK))
            .map_err(AppError::from_storage)?;
        lock.try_lock().map_err(|error| {
            AppError::from_storage(HistoryFailure(match error {
                std::fs::TryLockError::WouldBlock => "session directory is locked",
                std::fs::TryLockError::Error(_) => "session directory lock failed",
            }))
        })?;
        self.lock = Some(lock);

        if exists(&self.root.join(DIRECTORY_PENDING)).map_err(AppError::from_storage)? {
            return Err(AppError::from_storage(HistoryFailure(
                "pending session directory publication requires recovery",
            )));
        }
        let directory_path = self.root.join(DIRECTORY_FILE);
        let directory = if exists(&directory_path).map_err(AppError::from_storage)? {
            read_directory(&directory_path)?
        } else {
            self.discover_legacy_or_first_use()?
        };
        validate_directory(&directory)?;
        self.directory = Some(directory);
        let (session_count, segment_count) = self.root_usage()?;
        if session_count > MAX_SESSIONS || segment_count > MAX_SEGMENTS {
            return Err(capacity_error("root buddy history budget exceeded"));
        }
        self.validate_references_and_candidates()?;
        self.recovery(Vec::new())
    }

    fn discover_legacy_or_first_use(&self) -> Result<Directory, AppError> {
        let raw_current = self.root.join("raw-local.v1");
        let raw_pending = self.root.join("raw-local.pending");
        let raw_lock = self.root.join("raw-local.lock");
        if exists(&raw_pending).map_err(AppError::from_storage)? {
            return Err(AppError::from_storage(HistoryFailure(
                "legacy raw history has an unresolved pending frame",
            )));
        }
        let has_current = exists(&raw_current).map_err(AppError::from_storage)?;
        let has_lock = exists(&raw_lock).map_err(AppError::from_storage)?;
        let segments_root = self.root.join(SEGMENTS_DIR);
        if exists(&segments_root).map_err(AppError::from_storage)?
            && fs::read_dir(&segments_root)
                .map_err(AppError::from_storage)?
                .next()
                .is_some()
        {
            return Err(AppError::from_storage(HistoryFailure(
                "session directory is missing while managed segments exist",
            )));
        }
        if !has_current && has_lock {
            return Err(AppError::from_storage(HistoryFailure(
                "legacy raw history has a lock but no committed frame",
            )));
        }
        let legacy = has_current.then(|| Segment {
            id: String::from("segment-legacy"),
            path: String::from("."),
            sealed: false,
        });
        Ok(Directory {
            revision: 0,
            active_session_id: String::from("session-1"),
            next_segment_sequence: 1,
            retiring_segments: Vec::new(),
            sessions: vec![Session {
                id: String::from("session-1"),
                label: String::from("会话 1"),
                raw_hint: if has_current {
                    String::from("offline-acceptance-session")
                } else {
                    String::from("session-1")
                },
                archived: false,
                status: ChatStatus::Ready,
                external_unknown: false,
                recovered_unsettled: false,
                active_segment: legacy.as_ref().map(|segment| segment.id.clone()),
                segments: legacy.into_iter().collect(),
            }],
        })
    }

    fn validate_references_and_candidates(&mut self) -> Result<(), AppError> {
        let directory = self.directory()?.clone();
        validate_directory(&directory)?;
        let mut referenced = std::collections::BTreeSet::new();
        for retirement in &directory.retiring_segments {
            referenced.insert(format!("{SEGMENTS_DIR}/{}", retirement.id));
            let path = self.root.join(SEGMENTS_DIR).join(&retirement.id);
            let present = exists(&path).map_err(AppError::from_storage)?;
            let current = exists(&path.join("raw-local.v1")).map_err(AppError::from_storage)?;
            let lock = exists(&path.join("raw-local.lock")).map_err(AppError::from_storage)?;
            if present
                && (exists(&path.join("raw-local.pending")).map_err(AppError::from_storage)?
                    || exists(&path.join(".candidate")).map_err(AppError::from_storage)?)
            {
                return Err(AppError::from_storage(HistoryFailure(
                    "retiring segment has pending or candidate state",
                )));
            }
            let raw_state =
                RawLocalStore::inspect_retirement(&path).map_err(AppError::from_storage)?;
            validate_retirement_shape(retirement, raw_state, present, current, lock)?;
        }
        for session in &directory.sessions {
            for segment in &session.segments {
                let path = self.segment_path(segment);
                if !exists(&path.join("raw-local.v1")).map_err(AppError::from_storage)? {
                    return Err(AppError::from_storage(HistoryFailure(
                        "managed raw segment is missing its committed frame",
                    )));
                }
                if exists(&path.join("raw-local.pending")).map_err(AppError::from_storage)? {
                    return Err(AppError::from_storage(HistoryFailure(
                        "managed raw segment has an unresolved pending frame",
                    )));
                }
                let candidate = path.join(".candidate");
                if exists(&candidate).map_err(AppError::from_storage)? {
                    if session.active_segment.as_deref() != Some(segment.id.as_str()) {
                        return Err(AppError::from_storage(HistoryFailure(
                            "unpublished raw segment candidate requires recovery",
                        )));
                    }
                    fs::remove_file(candidate).map_err(AppError::from_storage)?;
                }
                referenced.insert(segment.path.clone());
            }
        }
        let segments_root = self.root.join(SEGMENTS_DIR);
        if exists(&segments_root).map_err(AppError::from_storage)? {
            for entry in fs::read_dir(&segments_root).map_err(AppError::from_storage)? {
                let entry = entry.map_err(AppError::from_storage)?;
                let file_type = entry.file_type().map_err(AppError::from_storage)?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if file_type.is_file() {
                    let Some(segment_id) = parse_retirement_gate_name(&name) else {
                        return Err(AppError::from_storage(HistoryFailure(
                            "unexpected file in managed segment directory",
                        )));
                    };
                    if !segment_sequence(&segment_id)
                        .is_some_and(|sequence| sequence < directory.next_segment_sequence)
                    {
                        return Err(AppError::from_storage(HistoryFailure(
                            "raw retirement gate exceeds the segment sequence watermark",
                        )));
                    }
                    let path = segments_root.join(&segment_id);
                    let state =
                        RawLocalStore::inspect_retirement(&path).map_err(AppError::from_storage)?;
                    let known = directory
                        .retiring_segments
                        .iter()
                        .any(|retirement| retirement.id == segment_id)
                        || directory.sessions.iter().any(|session| {
                            session
                                .segments
                                .iter()
                                .any(|segment| segment.id == segment_id)
                        });
                    let retired = state.is_some_and(|state| {
                        state.stage == RawStage::DirectoryRemoved
                            && segment_sequence(&segment_id)
                                .is_some_and(|sequence| sequence < directory.next_segment_sequence)
                    });
                    if !known && !retired {
                        return Err(AppError::from_storage(HistoryFailure(
                            "unreferenced raw retirement gate requires recovery",
                        )));
                    }
                    if let Some(state) = state
                        && state.stage == RawStage::DirectoryRemoved
                        && exists(&path).map_err(AppError::from_storage)?
                    {
                        return Err(AppError::from_storage(HistoryFailure(
                            "retired raw segment directory reappeared",
                        )));
                    }
                    continue;
                }
                if !file_type.is_dir() {
                    return Err(AppError::from_storage(HistoryFailure(
                        "unexpected entry in managed segment directory",
                    )));
                }
                let relative = format!("segments/{name}");
                if !referenced.contains(&relative) {
                    return Err(AppError::from_storage(HistoryFailure(
                        "unreferenced managed segment requires recovery",
                    )));
                }
            }
        }
        if exists(&self.root.join("raw-local.v1")).map_err(AppError::from_storage)?
            && !referenced.contains(".")
        {
            return Err(AppError::from_storage(HistoryFailure(
                "unreferenced legacy raw segment requires recovery",
            )));
        }
        Ok(())
    }

    fn segment_path(&self, segment: &Segment) -> PathBuf {
        if segment.path == "." {
            self.root.clone()
        } else {
            self.root.join(&segment.path)
        }
    }

    fn load_session_history(
        &mut self,
        access: &HistoryAccess<'_>,
        session: &Session,
        max_messages: usize,
    ) -> Result<(Vec<ChatMessage>, ChatStatus), AppError> {
        let max_messages = max_messages.min(MAX_HISTORY_MESSAGES);
        if max_messages == 0 {
            return Ok((Vec::new(), session.status));
        }
        let mut messages = Vec::new();
        let mut frame_bytes = 0_u64;
        let mut event_count = 0_usize;
        let mut body_bytes = 0_usize;
        let mut latest = None;
        let mut has_unsettled_user_interaction = false;
        let mut reached_window = false;
        for segment_index in (0..session.segments.len())
            .rev()
            .take(MAX_HISTORY_SEGMENTS_PER_READ)
        {
            let segment = &session.segments[segment_index];
            let path = self.segment_path(segment);
            let segment_bytes = fs::metadata(path.join("raw-local.v1"))
                .map_err(AppError::from_storage)?
                .len();
            if frame_bytes
                .checked_add(segment_bytes)
                .is_none_or(|total| total > MAX_HISTORY_FRAME_BYTES_PER_READ)
            {
                break;
            }
            let frame_budget = MAX_HISTORY_FRAME_BYTES_PER_READ - frame_bytes;
            let projected = if self.active_segment_id.as_deref() == Some(segment.id.as_str()) {
                let store = self.active_store.as_mut().ok_or_else(|| {
                    AppError::from_storage(HistoryFailure("active raw segment is unavailable"))
                })?;
                project_segment(
                    store,
                    access,
                    MAX_HISTORY_EVENTS_PER_READ - event_count,
                    MAX_HISTORY_BODY_BYTES_PER_READ - body_bytes,
                    frame_budget,
                )?
            } else {
                let mut runtime = scratch_runtime(access)?;
                let domain = RawDomainKey::from_request(access.ingress, access.request)
                    .map_err(AppError::from_storage)?;
                let grant = runtime
                    .install_raw_history_grant(access.ingress, domain.clone())
                    .map_err(AppError::from_storage)?;
                let raw_access = RawLocalAccess::new(
                    &mut runtime,
                    access.ingress,
                    access.request,
                    &grant,
                    domain,
                    access.now_ms,
                )
                .map_err(AppError::from_storage)?;
                let (mut store, plan) = RawLocalStore::open_projection(
                    &path,
                    raw_access,
                    access.retention,
                    frame_budget,
                    MAX_HISTORY_EVENTS_PER_READ - event_count,
                    MAX_HISTORY_BODY_BYTES_PER_READ - body_bytes,
                )
                .map_err(AppError::from_storage)?;
                project_prepared_segment(&mut store, access, runtime, grant, plan)?
            };
            frame_bytes = frame_bytes.saturating_add(projected.frame_bytes_read);
            if let Some(evidence) = projected.latest_user_interaction
                && latest.is_none_or(|(received_ms, latest_segment_index, _)| {
                    (evidence.0, segment_index) >= (received_ms, latest_segment_index)
                })
            {
                latest = Some((evidence.0, segment_index, evidence.1));
            }
            has_unsettled_user_interaction |= projected.has_unsettled_user_interaction;
            event_count = event_count.saturating_add(projected.decoded_event_count);
            body_bytes = body_bytes.saturating_add(projected.decoded_body_bytes);
            if !projected.body_loaded {
                break;
            }
            for (event_index, event) in projected.events.iter().enumerate().rev() {
                let role = match event.role() {
                    EventRole::User | EventRole::UserHistory => MessageRole::User,
                    EventRole::Assistant => MessageRole::Assistant,
                    _ => continue,
                };
                messages.push((
                    event.received_ms(),
                    segment_index,
                    event_index,
                    ChatMessage {
                        role,
                        text: event.body().to_owned(),
                    },
                ));
                if messages.len() >= max_messages {
                    reached_window = true;
                    break;
                }
            }
            if reached_window {
                break;
            }
        }
        messages.sort_by_key(|(received_ms, segment_index, event_index, _)| {
            (*received_ms, *segment_index, *event_index)
        });
        let start = messages.len().saturating_sub(max_messages);
        let messages = messages
            .drain(start..)
            .map(|(_, _, _, message)| message)
            .collect();
        let status =
            restored_status_evidence(session.status, latest, has_unsettled_user_interaction);
        Ok((messages, status))
    }

    fn raw_frame_estimate(access: &mut HistoryAccess<'_>) -> Result<u64, AppError> {
        let snapshot = access
            .runtime
            .export_raw_snapshot(
                SessionAccess::new(access.ingress, access.request, access.now_ms),
                access.now_ms,
            )
            .map_err(AppError::from_storage)?;
        let payload = snapshot.encode().map_err(AppError::from_storage)?;
        u64::try_from(payload.len())
            .ok()
            .and_then(|bytes| bytes.checked_add(RAW_FRAME_OVERHEAD))
            .ok_or_else(|| capacity_error("raw segment size overflow"))
    }

    fn tree_bytes(&self) -> Result<u64, AppError> {
        fn visit(path: &Path, total: &mut u64) -> std::io::Result<()> {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_symlink() {
                    return Err(std::io::Error::other("symlink in managed chat root"));
                }
                if kind.is_dir() {
                    visit(&entry.path(), total)?;
                } else if kind.is_file() {
                    *total = total
                        .checked_add(entry.metadata()?.len())
                        .ok_or_else(|| std::io::Error::other("managed byte count overflow"))?;
                } else {
                    return Err(std::io::Error::other("unsupported managed file type"));
                }
            }
            Ok(())
        }
        let mut total = 0;
        visit(&self.budget_root, &mut total).map_err(AppError::from_storage)?;
        Ok(total)
    }

    fn root_usage(&self) -> Result<(usize, usize), AppError> {
        let mut roots = vec![self.budget_root.clone()];
        let instances = self.budget_root.join("instances");
        if exists(&instances).map_err(AppError::from_storage)? {
            for entry in fs::read_dir(&instances).map_err(AppError::from_storage)? {
                let entry = entry.map_err(AppError::from_storage)?;
                let kind = entry.file_type().map_err(AppError::from_storage)?;
                if !kind.is_dir() || kind.is_symlink() {
                    return Err(AppError::from_storage(HistoryFailure(
                        "unexpected entry in buddy history root",
                    )));
                }
                roots.push(entry.path());
            }
        }
        let mut sessions = 0usize;
        let mut segments = 0usize;
        for root in roots {
            if exists(&root.join(DIRECTORY_PENDING)).map_err(AppError::from_storage)? {
                return Err(AppError::from_storage(HistoryFailure(
                    "a buddy session directory has unresolved pending state",
                )));
            }
            let directory_path = root.join(DIRECTORY_FILE);
            if exists(&directory_path).map_err(AppError::from_storage)? {
                let directory = read_directory(&directory_path)?;
                sessions = sessions
                    .checked_add(directory.sessions.len())
                    .ok_or_else(|| capacity_error("root session count overflow"))?;
                segments = segments
                    .checked_add(directory.retiring_segments.len())
                    .and_then(|count| {
                        directory.sessions.iter().try_fold(count, |sum, session| {
                            sum.checked_add(session.segments.len())
                        })
                    })
                    .ok_or_else(|| capacity_error("root segment count overflow"))?;
            } else if root == self.root {
                if let Some(directory) = &self.directory {
                    sessions = sessions
                        .checked_add(directory.sessions.len())
                        .ok_or_else(|| capacity_error("root session count overflow"))?;
                    segments = segments
                        .checked_add(directory.retiring_segments.len())
                        .and_then(|count| {
                            directory.sessions.iter().try_fold(count, |sum, session| {
                                sum.checked_add(session.segments.len())
                            })
                        })
                        .ok_or_else(|| capacity_error("root segment count overflow"))?;
                } else if exists(&root.join("raw-local.pending")).map_err(AppError::from_storage)? {
                    return Err(AppError::from_storage(HistoryFailure(
                        "legacy raw history has unresolved pending state",
                    )));
                } else if exists(&root.join("raw-local.v1")).map_err(AppError::from_storage)? {
                    sessions = sessions
                        .checked_add(1)
                        .ok_or_else(|| capacity_error("root session count overflow"))?;
                    segments = segments
                        .checked_add(1)
                        .ok_or_else(|| capacity_error("root segment count overflow"))?;
                } else if exists(&root.join(SEGMENTS_DIR)).map_err(AppError::from_storage)?
                    && fs::read_dir(root.join(SEGMENTS_DIR))
                        .map_err(AppError::from_storage)?
                        .next()
                        .is_some()
                {
                    return Err(AppError::from_storage(HistoryFailure(
                        "managed raw segments exist without a session directory",
                    )));
                }
            } else if exists(&root.join("raw-local.pending")).map_err(AppError::from_storage)?
                || exists(&root.join("raw-local.v1")).map_err(AppError::from_storage)?
                || (exists(&root.join(SEGMENTS_DIR)).map_err(AppError::from_storage)?
                    && fs::read_dir(root.join(SEGMENTS_DIR))
                        .map_err(AppError::from_storage)?
                        .next()
                        .is_some())
            {
                return Err(AppError::from_storage(HistoryFailure(
                    "managed buddy history is missing its session directory",
                )));
            }
        }
        Ok((sessions, segments))
    }

    fn preflight_peak(&self, bytes: u64) -> Result<(), AppError> {
        let total = self
            .tree_bytes()?
            .checked_add(bytes)
            .ok_or_else(|| capacity_error("managed root byte count overflow"))?;
        if total > MAX_ROOT_BYTES {
            Err(capacity_error("managed root peak byte budget exceeded"))
        } else {
            Ok(())
        }
    }

    fn encode_directory(directory: &Directory) -> Result<Vec<u8>, AppError> {
        let value = serde_json::json!({
            "version": DIRECTORY_VERSION,
            "revision": directory.revision,
            "activeSessionId": directory.active_session_id,
            "nextSegmentSequence": directory.next_segment_sequence,
            "retiringSegments": directory.retiring_segments.iter().map(encode_retirement).collect::<Vec<_>>(),
            "sessions": directory.sessions.iter().map(|session| serde_json::json!({
                "id": session.id,
                "label": session.label,
                "rawHint": session.raw_hint,
                "archived": session.archived,
                "status": status_text(session.status),
                "externalUnknown": session.external_unknown,
                "recoveredUnsettled": session.recovered_unsettled,
                "activeSegment": session.active_segment,
                "segments": session.segments.iter().map(|segment| serde_json::json!({
                    "id": segment.id,
                    "path": segment.path,
                    "sealed": segment.sealed,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        let bytes = serde_json::to_vec(&value).map_err(AppError::from_storage)?;
        if bytes.len() as u64 > MAX_DIRECTORY_BYTES {
            return Err(capacity_error("session directory byte budget exceeded"));
        }
        Ok(bytes)
    }

    fn publish_directory(
        &mut self,
        directory: Directory,
    ) -> Result<DirectoryPublishReceipt, AppError> {
        self.poisoned = true;
        let revision = directory.revision;
        let bytes = Self::encode_directory(&directory)?;
        let pending_path = self.root.join(DIRECTORY_PENDING);
        let current_path = self.root.join(DIRECTORY_FILE);
        #[cfg(test)]
        if injected_failure(FailurePoint::PublishCreate) {
            return Err(AppError::from_storage(HistoryFailure(
                "injected directory pending create failure",
            )));
        }
        let mut pending = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending_path)
            .map_err(AppError::from_storage)?;
        #[cfg(test)]
        if injected_failure(FailurePoint::PublishWrite) {
            pending
                .write_all(&bytes[..bytes.len().max(1).min(bytes.len() / 2 + 1)])
                .map_err(AppError::from_storage)?;
            return Err(AppError::from_storage(HistoryFailure(
                "injected directory pending write failure",
            )));
        }
        pending.write_all(&bytes).map_err(AppError::from_storage)?;
        #[cfg(test)]
        if injected_failure(FailurePoint::PublishSync) {
            return Err(AppError::from_storage(HistoryFailure(
                "injected directory pending sync failure",
            )));
        }
        pending.sync_all().map_err(AppError::from_storage)?;
        drop(pending);
        #[cfg(test)]
        if injected_failure(FailurePoint::PublishRename) {
            return Err(AppError::from_storage(HistoryFailure(
                "injected directory publish failure",
            )));
        }
        fs::rename(&pending_path, &current_path).map_err(AppError::from_storage)?;
        self.directory = Some(directory);
        let mut receipt = DirectoryPublishReceipt {
            revision,
            file_synced: true,
            directory_synced: false,
        };
        self.last_receipts.directory = Some(receipt);
        #[cfg(test)]
        if injected_failure(FailurePoint::PublishDirectorySync) {
            return Err(AppError::from_storage(HistoryFailure(
                "injected directory sync failure",
            )));
        }
        let directory_synced = sync_directory(&self.root).map_err(AppError::from_storage)?;
        receipt.directory_synced = directory_synced;
        self.last_receipts.directory = Some(receipt);
        self.poisoned = false;
        Ok(receipt)
    }

    fn next_segment(&self, directory: &Directory) -> Result<(u64, Segment), AppError> {
        let (_, segment_count) = self.root_usage()?;
        if segment_count >= MAX_SEGMENTS {
            return Err(capacity_error("root managed raw segment count exceeded"));
        }
        let sequence = directory.next_segment_sequence;
        let next_sequence = sequence
            .checked_add(1)
            .ok_or_else(|| capacity_error("raw segment sequence exhausted"))?;
        let id = format!("segment-{sequence:08}");
        Ok((
            next_sequence,
            Segment {
                id: id.clone(),
                path: format!("{SEGMENTS_DIR}/{id}"),
                sealed: false,
            },
        ))
    }

    fn begin_cold_retirement(
        &mut self,
        access: &mut HistoryAccess<'_>,
        request: &HistoryRequest,
        target: RetirementTarget<'_>,
        store: &mut RawLocalStore,
        scratch: &mut SessionRuntime,
        receipt: CommitReceipt,
    ) -> Result<(), AppError> {
        self.ready()?;
        let segment_id = target.segment.id.as_str();
        let session_id = target.session_id;
        let current = self.directory()?.clone();
        let session = current
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| {
                AppError::from_storage(HistoryFailure("retirement session is missing"))
            })?;
        let segment = session
            .segments
            .iter()
            .find(|segment| segment.id == segment_id)
            .ok_or_else(|| {
                AppError::from_storage(HistoryFailure("retirement segment is missing"))
            })?;
        if !segment.sealed
            || session.active_segment.as_deref() == Some(segment_id)
            || segment.path != format!("{SEGMENTS_DIR}/{segment_id}")
        {
            return Err(AppError::from_storage(HistoryFailure(
                "only a sealed managed cold segment can be retired",
            )));
        }
        if current.retiring_segments.len() >= MAX_SEGMENTS {
            return Err(capacity_error(
                "raw segment retirement fence count exceeded",
            ));
        }
        let domain = RawDomainKey::from_request(access.ingress, &request.request)
            .map_err(AppError::from_storage)?;
        access
            .runtime
            .validate_raw_history_access(
                SessionAccess::new(access.ingress, &request.request, access.now_ms),
                &request.grant,
                &domain,
            )
            .map_err(AppError::from_storage)?;
        let authorization_revision = access
            .ingress
            .stable_credential_revision(request.request.credential())
            .ok_or_else(|| {
                AppError::from_storage(HistoryFailure(
                    "retirement authorization revision is unavailable",
                ))
            })?;
        let grant = scratch
            .install_raw_history_grant(access.ingress, domain.clone())
            .map_err(AppError::from_storage)?;
        #[cfg(test)]
        if injected_failure(FailurePoint::RetireBeforeMarker) {
            self.poisoned = true;
            return Err(AppError::from_storage(HistoryFailure(
                "injected pre-retirement failure",
            )));
        }

        let retirement = Retirement {
            id: segment_id.to_owned(),
            session_id: session_id.to_owned(),
            domain: StableDomainRecord::from_domain(&domain),
            authorization_revision,
            frame: raw_receipt(receipt),
            stage: RetirementStage::Marked,
        };
        let mut staged = current;
        staged.retiring_segments.push(retirement);
        let session = staged
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("retirement session was checked");
        session.segments.retain(|segment| segment.id != segment_id);
        self.publish_new_retirement_directory(staged, &domain)?;

        #[cfg(test)]
        if injected_failure(FailurePoint::RetireAfterMarker) {
            self.poisoned = true;
            return Err(AppError::from_storage(HistoryFailure(
                "injected post-marker retirement failure",
            )));
        }
        let raw_access = RawLocalAccess::new(
            scratch,
            access.ingress,
            &request.request,
            &grant,
            domain,
            access.now_ms,
        )
        .map_err(AppError::from_storage)?;
        store
            .remove_retired_payload(raw_access, access.retention, receipt)
            .map_err(AppError::from_storage)?;
        #[cfg(test)]
        if injected_failure(FailurePoint::RetireAfterCurrentRemoval) {
            self.poisoned = true;
            return Err(AppError::from_storage(HistoryFailure(
                "injected post-frame retirement failure",
            )));
        }
        self.set_retirement_stage(segment_id, RetirementStage::PayloadRemoved)?;
        Ok(())
    }

    fn finish_pending_retirements(
        &mut self,
        access: &mut HistoryAccess<'_>,
        requests: &[HistoryRequest],
    ) -> Result<(), AppError> {
        let pending = self.directory()?.retiring_segments.clone();
        for mut retirement in pending {
            let request = requests
                .iter()
                .find(|request| request.logical_session_id == retirement.session_id)
                .ok_or_else(|| {
                    AppError::from_storage(HistoryFailure(
                        "retirement recovery is missing its trusted session request",
                    ))
                })?;
            let domain = RawDomainKey::from_request(access.ingress, &request.request)
                .map_err(AppError::from_storage)?;
            if StableDomainRecord::from_domain(&domain) != retirement.domain
                || access
                    .ingress
                    .stable_credential_revision(request.request.credential())
                    != Some(retirement.authorization_revision)
            {
                return Err(AppError::from_storage(HistoryFailure(
                    "retirement domain or authorization revision changed",
                )));
            }
            access
                .runtime
                .validate_raw_history_access(
                    SessionAccess::new(access.ingress, &request.request, access.now_ms),
                    &request.grant,
                    &domain,
                )
                .map_err(AppError::from_storage)?;

            loop {
                let segment_path = self.root.join(SEGMENTS_DIR).join(&retirement.id);
                let raw_state = RawLocalStore::inspect_retirement(&segment_path)
                    .map_err(AppError::from_storage)?;
                if let Some(state) = raw_state {
                    if state.revision != retirement.frame.revision
                        || state.committed_at_ms != retirement.frame.committed_at_ms
                        || retirement_rank(raw_stage_to_history(state.stage))
                            > retirement_rank(retirement.stage) + 1
                    {
                        return Err(AppError::from_storage(HistoryFailure(
                            "raw retirement stage is inconsistent with its directory fence",
                        )));
                    }
                    let confirmed_stage = raw_stage_to_history(state.stage);
                    if retirement_rank(confirmed_stage) > retirement_rank(retirement.stage) {
                        self.set_retirement_stage(&retirement.id, confirmed_stage)?;
                        retirement.stage = confirmed_stage;
                    }
                }

                match retirement.stage {
                    RetirementStage::Marked => {
                        let mut scratch = scratch_runtime(access)?;
                        let grant = scratch
                            .install_raw_history_grant(access.ingress, domain.clone())
                            .map_err(AppError::from_storage)?;
                        let raw_access = RawLocalAccess::new(
                            &mut scratch,
                            access.ingress,
                            &request.request,
                            &grant,
                            domain.clone(),
                            access.now_ms,
                        )
                        .map_err(AppError::from_storage)?;
                        let mut store = RawLocalStore::open_for_retirement(
                            &segment_path,
                            raw_access,
                            retirement.frame.revision,
                            retirement.frame.committed_at_ms,
                        )
                        .map_err(AppError::from_storage)?
                        .store;
                        let verified = store
                            .verify_retirable(
                                RawLocalAccess::new(
                                    &mut scratch,
                                    access.ingress,
                                    &request.request,
                                    &grant,
                                    domain.clone(),
                                    access.now_ms,
                                )
                                .map_err(AppError::from_storage)?,
                                access.retention,
                                retirement.frame.revision,
                            )
                            .map_err(AppError::from_storage)?;
                        if raw_receipt(verified) != retirement.frame {
                            return Err(AppError::from_storage(HistoryFailure(
                                "retirement frame receipt changed",
                            )));
                        }
                        store
                            .remove_retired_payload(
                                RawLocalAccess::new(
                                    &mut scratch,
                                    access.ingress,
                                    &request.request,
                                    &grant,
                                    domain.clone(),
                                    access.now_ms,
                                )
                                .map_err(AppError::from_storage)?,
                                access.retention,
                                verified,
                            )
                            .map_err(AppError::from_storage)?;
                        #[cfg(test)]
                        if injected_failure(FailurePoint::RetireAfterCurrentRemoval) {
                            return Err(AppError::from_storage(HistoryFailure(
                                "injected post-frame retirement failure",
                            )));
                        }
                        self.set_retirement_stage(&retirement.id, RetirementStage::PayloadRemoved)?;
                        retirement.stage = RetirementStage::PayloadRemoved;
                    }
                    RetirementStage::PayloadRemoved => {
                        let mut scratch = scratch_runtime(access)?;
                        let grant = scratch
                            .install_raw_history_grant(access.ingress, domain.clone())
                            .map_err(AppError::from_storage)?;
                        let mut guard = RawLocalStore::lock_retirement(
                            &segment_path,
                            RawLocalAccess::new(
                                &mut scratch,
                                access.ingress,
                                &request.request,
                                &grant,
                                domain.clone(),
                                access.now_ms,
                            )
                            .map_err(AppError::from_storage)?,
                            raw_commit_receipt(retirement.frame),
                            true,
                        )
                        .map_err(AppError::from_storage)?;
                        guard.remove_lock_file().map_err(AppError::from_storage)?;
                        guard.mark_lock_removed().map_err(AppError::from_storage)?;
                        #[cfg(test)]
                        if injected_failure(FailurePoint::RetireAfterLockRemoval) {
                            return Err(AppError::from_storage(HistoryFailure(
                                "injected post-lock retirement failure",
                            )));
                        }
                        self.set_retirement_stage(&retirement.id, RetirementStage::LockRemoved)?;
                        retirement.stage = RetirementStage::LockRemoved;
                    }
                    RetirementStage::LockRemoved => {
                        let mut scratch = scratch_runtime(access)?;
                        let grant = scratch
                            .install_raw_history_grant(access.ingress, domain.clone())
                            .map_err(AppError::from_storage)?;
                        let mut guard = RawLocalStore::lock_retirement(
                            &segment_path,
                            RawLocalAccess::new(
                                &mut scratch,
                                access.ingress,
                                &request.request,
                                &grant,
                                domain.clone(),
                                access.now_ms,
                            )
                            .map_err(AppError::from_storage)?,
                            raw_commit_receipt(retirement.frame),
                            false,
                        )
                        .map_err(AppError::from_storage)?;
                        guard
                            .remove_segment_directory()
                            .map_err(AppError::from_storage)?;
                        #[cfg(test)]
                        if injected_failure(FailurePoint::RetireAfterDirectoryRemoval) {
                            return Err(AppError::from_storage(HistoryFailure(
                                "injected post-directory retirement failure",
                            )));
                        }
                        self.set_retirement_stage(
                            &retirement.id,
                            RetirementStage::DirectoryRemoved,
                        )?;
                        retirement.stage = RetirementStage::DirectoryRemoved;
                    }
                    RetirementStage::DirectoryRemoved => {
                        #[cfg(test)]
                        if injected_failure(FailurePoint::RetireBeforeFinalMarker) {
                            return Err(AppError::from_storage(HistoryFailure(
                                "injected pre-final-marker retirement failure",
                            )));
                        }
                        self.finish_retirement_marker(&retirement.id)?;
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn finish_retirement_marker(&mut self, segment_id: &str) -> Result<(), AppError> {
        let mut directory = self.directory()?.clone();
        if !directory
            .retiring_segments
            .iter()
            .any(|retiring| retiring.id == segment_id)
        {
            return Err(AppError::from_storage(HistoryFailure(
                "retirement marker is missing",
            )));
        }
        directory
            .retiring_segments
            .retain(|retiring| retiring.id != segment_id);
        self.publish_retirement_directory(directory)?;
        Ok(())
    }

    fn set_retirement_stage(
        &mut self,
        segment_id: &str,
        stage: RetirementStage,
    ) -> Result<(), AppError> {
        let mut directory = self.directory()?.clone();
        let retirement = directory
            .retiring_segments
            .iter_mut()
            .find(|retirement| retirement.id == segment_id)
            .ok_or_else(|| {
                AppError::from_storage(HistoryFailure("retirement marker is missing"))
            })?;
        if retirement_rank(stage) < retirement_rank(retirement.stage)
            || retirement_rank(stage) > retirement_rank(retirement.stage) + 1
        {
            return Err(AppError::from_storage(HistoryFailure(
                "retirement stage cannot skip or reverse a deletion receipt",
            )));
        }
        retirement.stage = stage;
        self.publish_retirement_directory(directory)
    }

    fn publish_retirement_directory(&mut self, directory: Directory) -> Result<(), AppError> {
        let bytes = Self::encode_directory(&directory)?.len() as u64;
        self.preflight_peak(bytes)?;
        self.publish_directory(directory)?;
        Ok(())
    }

    fn publish_new_retirement_directory(
        &mut self,
        directory: Directory,
        domain: &RawDomainKey,
    ) -> Result<(), AppError> {
        let bytes = Self::encode_directory(&directory)?.len() as u64;
        let future_sidecars = LOCAL_RETIREMENT_MARKER_BYTES
            .checked_add(
                RawLocalStore::retirement_gate_size(domain).map_err(AppError::from_storage)?,
            )
            .ok_or_else(|| capacity_error("retirement sidecar peak overflow"))?;
        self.preflight_peak(
            bytes
                .checked_add(future_sidecars)
                .ok_or_else(|| capacity_error("retirement peak byte count overflow"))?,
        )?;
        self.publish_directory(directory)?;
        Ok(())
    }

    fn create_segment(
        &mut self,
        access: &mut HistoryAccess<'_>,
        segment: &Segment,
    ) -> Result<(RawLocalStore, RawHistoryCommitReceipt), AppError> {
        let path = self.segment_path(segment);
        #[cfg(test)]
        if injected_failure(FailurePoint::SegmentBeforeMarker) {
            return Err(AppError::from_storage(HistoryFailure(
                "injected segment creation failure",
            )));
        }
        fs::create_dir_all(&path).map_err(AppError::from_storage)?;
        let marker = path.join(".candidate");
        let mut marker_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
            .map_err(AppError::from_storage)?;
        marker_file
            .write_all(b"segment-candidate-v1")
            .map_err(AppError::from_storage)?;
        marker_file.sync_all().map_err(AppError::from_storage)?;
        drop(marker_file);
        #[cfg(test)]
        if injected_failure(FailurePoint::SegmentAfterMarker) {
            return Err(AppError::from_storage(HistoryFailure(
                "injected segment candidate failure",
            )));
        }
        let domain = RawDomainKey::from_request(access.ingress, access.request)
            .map_err(AppError::from_storage)?;
        let raw_access = RawLocalAccess::new(
            access.runtime,
            access.ingress,
            access.request,
            access.grant,
            domain,
            access.now_ms,
        )
        .map_err(AppError::from_storage)?;
        let opened = RawLocalStore::open(&path, OpenMode::CreateNew, raw_access)
            .map_err(AppError::from_storage)?;
        let receipt = opened.create_receipt.ok_or_else(|| {
            AppError::from_storage(HistoryFailure("new raw segment omitted its commit receipt"))
        })?;
        let receipt = raw_receipt(receipt);
        self.last_receipts.raw.push(receipt);
        Ok((opened.store, receipt))
    }

    fn check_revision(&self, expected: u64) -> Result<(), AppError> {
        if self.directory()?.revision != expected {
            Err(AppError::session_directory_conflict(HistoryFailure(
                "session directory revision changed",
            )))
        } else {
            Ok(())
        }
    }

    fn preflight_retention_peak(
        current_total: u64,
        current_frame: u64,
        preview: RetentionPreview,
        directory_candidate_bytes: u64,
    ) -> Result<(), AppError> {
        let pending_raw_peak = current_total
            .checked_add(preview.candidate_frame_bytes)
            .ok_or_else(|| capacity_error("retention pending-frame peak overflow"))?;
        let after_raw = current_total
            .checked_sub(current_frame)
            .and_then(|bytes| bytes.checked_add(preview.candidate_frame_bytes))
            .ok_or_else(|| capacity_error("retention committed-frame size overflow"))?;
        let pending_directory_peak = after_raw
            .checked_add(directory_candidate_bytes)
            .ok_or_else(|| capacity_error("retention directory peak overflow"))?;
        if pending_raw_peak.max(pending_directory_peak) > MAX_ROOT_BYTES {
            return Err(capacity_error("retention peak byte budget exceeded"));
        }
        Ok(())
    }

    fn retirement_directory_candidate_bytes(
        &self,
        session_id: &str,
        segment_id: &str,
        domain: &RawDomainKey,
        authorization_revision: u64,
        frame_revision: u64,
        committed_at_ms: u64,
    ) -> Result<u64, AppError> {
        let mut directory = self.directory()?.clone();
        if directory.retiring_segments.len() >= MAX_SEGMENTS {
            return Err(capacity_error(
                "raw segment retirement fence count exceeded",
            ));
        }
        let session = directory
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| {
                AppError::from_storage(HistoryFailure("retirement session is missing"))
            })?;
        let segment = session
            .segments
            .iter()
            .find(|segment| segment.id == segment_id)
            .ok_or_else(|| {
                AppError::from_storage(HistoryFailure("retirement segment is missing"))
            })?;
        if !segment.sealed
            || session.active_segment.as_deref() == Some(segment_id)
            || segment.path != format!("{SEGMENTS_DIR}/{segment_id}")
        {
            return Err(AppError::from_storage(HistoryFailure(
                "only a sealed managed cold segment can be retired",
            )));
        }
        directory.retiring_segments.push(Retirement {
            id: segment_id.to_owned(),
            session_id: session_id.to_owned(),
            domain: StableDomainRecord::from_domain(domain),
            authorization_revision,
            frame: RawHistoryCommitReceipt {
                revision: frame_revision,
                committed_at_ms,
                file_synced: true,
                directory_synced: false,
            },
            stage: RetirementStage::Marked,
        });
        let session = directory
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("retirement session validated");
        session.segments.retain(|segment| segment.id != segment_id);
        let gate_bytes =
            RawLocalStore::retirement_gate_size(domain).map_err(AppError::from_storage)?;
        u64::try_from(Self::encode_directory(&directory)?.len())
            .ok()
            .and_then(|bytes| bytes.checked_add(LOCAL_RETIREMENT_MARKER_BYTES + gate_bytes))
            .ok_or_else(|| capacity_error("retirement directory size overflow"))
    }

    fn maintain_session_retention(
        &mut self,
        access: &mut HistoryAccess<'_>,
        request: &HistoryRequest,
        session: &mut Session,
    ) -> Result<(), AppError> {
        let domain = RawDomainKey::from_request(access.ingress, &request.request)
            .map_err(AppError::from_storage)?;
        let _validated = RawLocalAccess::new(
            access.runtime,
            access.ingress,
            &request.request,
            &request.grant,
            domain.clone(),
            access.now_ms,
        )
        .map_err(AppError::from_storage)?;
        drop(_validated);

        let mut latest = None;
        let mut has_unsettled_user_interaction = false;
        for (segment_index, segment) in session.segments.iter().enumerate() {
            let active = self.active_segment_id.as_deref() == Some(segment.id.as_str());
            let path = self.segment_path(segment);
            let current_total = self.tree_bytes()?;
            let current_frame = fs::metadata(path.join("raw-local.v1"))
                .map_err(AppError::from_storage)?
                .len();
            let mut scratch = scratch_runtime(access)?;
            let grant = scratch
                .install_raw_history_grant(access.ingress, domain.clone())
                .map_err(AppError::from_storage)?;
            let raw_access = RawLocalAccess::new(
                &mut scratch,
                access.ingress,
                &request.request,
                &grant,
                domain.clone(),
                access.now_ms,
            )
            .map_err(AppError::from_storage)?;
            let mut cold_store;
            let store = if active {
                self.active_store.as_mut().ok_or_else(|| {
                    AppError::from_storage(HistoryFailure("active raw segment is unavailable"))
                })?
            } else {
                cold_store = RawLocalStore::open(&path, OpenMode::OpenExisting, raw_access)
                    .map_err(AppError::from_storage)?
                    .store;
                &mut cold_store
            };
            let revision = store.revision();
            let raw_access = RawLocalAccess::new(
                &mut scratch,
                access.ingress,
                &request.request,
                &grant,
                domain.clone(),
                access.now_ms,
            )
            .map_err(AppError::from_storage)?;
            let plan = store
                .prepare_retention(raw_access, access.retention, revision)
                .map_err(AppError::from_storage)?;
            let preview = plan.preview();
            if let Some(evidence) = preview.latest_user_interaction
                && latest.is_none_or(|(received_ms, latest_segment_index, _)| {
                    (evidence.0, segment_index) >= (received_ms, latest_segment_index)
                })
            {
                latest = Some((evidence.0, segment_index, evidence.1));
            }
            has_unsettled_user_interaction |= preview.has_unsettled_user_interaction;
            Self::preflight_retention_peak(current_total, current_frame, preview, 0)?;
            if preview.changed {
                let raw_access = RawLocalAccess::new(
                    &mut scratch,
                    access.ingress,
                    &request.request,
                    &grant,
                    domain.clone(),
                    access.now_ms,
                )
                .map_err(AppError::from_storage)?;
                let receipt = store
                    .maintain_retention_prepared(raw_access, plan)
                    .map_err(AppError::from_storage)?;
                if let Some(receipt) = receipt.checkpoint {
                    self.last_receipts.raw.push(raw_receipt(receipt));
                }
            }
        }
        session.status =
            restored_status_evidence(session.status, latest, has_unsettled_user_interaction);
        if session.status != ChatStatus::Unknown {
            session.external_unknown = false;
        }
        Ok(())
    }

    fn maintain_directory_retention(
        &mut self,
        access: &mut HistoryAccess<'_>,
        requests: &[HistoryRequest],
        directory: &mut Directory,
    ) -> Result<(), AppError> {
        for session in &mut directory.sessions {
            let request = requests
                .iter()
                .find(|request| request.logical_session_id == session.id)
                .ok_or_else(|| {
                    AppError::from_storage(HistoryFailure(
                        "startup recovery is missing a trusted session request",
                    ))
                })?;
            self.maintain_session_retention(access, request, session)?;
        }
        Ok(())
    }

    fn publish_unsettled_summary(
        &mut self,
        access: &mut HistoryAccess<'_>,
        requests: &[HistoryRequest],
    ) -> Result<(), AppError> {
        let mut directory = self.directory()?.clone();
        let mut changed = false;
        for session in &mut directory.sessions {
            let request = requests
                .iter()
                .find(|request| request.logical_session_id == session.id)
                .ok_or_else(|| {
                    AppError::from_storage(HistoryFailure(
                        "recovery summary is missing its trusted session request",
                    ))
                })?;
            let domain = RawDomainKey::from_request(access.ingress, &request.request)
                .map_err(AppError::from_storage)?;
            let mut unresolved = false;
            for segment in &session.segments {
                let path = self.segment_path(segment);
                if segment.path != "."
                    && RawLocalStore::inspect_retirement(&path)
                        .map_err(AppError::from_storage)?
                        .is_some()
                {
                    continue;
                }
                let mut scratch = scratch_runtime(access)?;
                let grant = scratch
                    .install_raw_history_grant(access.ingress, domain.clone())
                    .map_err(AppError::from_storage)?;
                let raw_access = RawLocalAccess::new(
                    &mut scratch,
                    access.ingress,
                    &request.request,
                    &grant,
                    domain.clone(),
                    access.now_ms,
                )
                .map_err(AppError::from_storage)?;
                let mut store = RawLocalStore::open(&path, OpenMode::OpenExisting, raw_access)
                    .map_err(AppError::from_storage)?
                    .store;
                let revision = store.revision();
                let preview = store
                    .preview_retention(
                        RawLocalAccess::new(
                            &mut scratch,
                            access.ingress,
                            &request.request,
                            &grant,
                            domain.clone(),
                            access.now_ms,
                        )
                        .map_err(AppError::from_storage)?,
                        access.retention,
                        revision,
                    )
                    .map_err(AppError::from_storage)?;
                unresolved |= preview.has_unsettled_user_interaction;
            }
            if unresolved && (session.status != ChatStatus::Unknown || !session.recovered_unsettled)
            {
                session.status = ChatStatus::Unknown;
                session.recovered_unsettled = true;
                changed = true;
            }
        }
        if changed {
            directory.revision = Self::next_directory_revision(self.directory()?)?;
            let bytes = Self::encode_directory(&directory)?.len() as u64;
            self.preflight_peak(bytes)?;
            self.publish_directory(directory)?;
        }
        Ok(())
    }

    fn preflight_reserved_send(
        context: SendPeakContext<'_>,
        reservation: HistorySendReservation,
    ) -> Result<(), AppError> {
        let SendPeakContext {
            directory,
            session_id,
            current_total,
            current_raw_bytes,
            current_directory_bytes,
            base_frame_bytes,
            online,
        } = context;
        let user_growth = u64::try_from(reservation.user_characters)
            .ok()
            .and_then(|chars| chars.checked_mul(4))
            .and_then(|bytes| bytes.checked_add(1_024))
            .ok_or_else(|| capacity_error("reserved raw history size overflow"))?;
        let full_growth = reservation
            .user_characters
            .checked_add(reservation.reserved_reply_characters)
            .and_then(|chars| u64::try_from(chars).ok())
            .and_then(|chars| chars.checked_mul(4))
            .and_then(|bytes| bytes.checked_add(2_048))
            .ok_or_else(|| capacity_error("reserved raw history size overflow"))?;
        let user_frame = base_frame_bytes
            .checked_add(user_growth)
            .ok_or_else(|| capacity_error("reserved raw history size overflow"))?;
        let full_frame = base_frame_bytes
            .checked_add(full_growth)
            .ok_or_else(|| capacity_error("reserved raw history size overflow"))?;
        let directory_size_for = |status: ChatStatus, external_unknown: bool| {
            let mut next = directory.clone();
            let session = next
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
                .ok_or_else(|| {
                    AppError::session_directory_conflict(HistoryFailure(
                        "send reservation session is missing",
                    ))
                })?;
            let changed = session.status != status || session.external_unknown != external_unknown;
            session.status = status;
            session.external_unknown = external_unknown;
            if changed {
                u64::try_from(Self::encode_directory(&next)?.len())
                    .map_err(|_| capacity_error("send reservation directory size overflow"))
            } else {
                Ok(0)
            }
        };
        let final_directory_candidate = directory_size_for(ChatStatus::Unknown, online)?;
        let peak = if online {
            let send_directory_candidate = directory_size_for(ChatStatus::Sending, true)?;
            let peak_send_raw = current_total
                .checked_add(user_frame)
                .ok_or_else(|| capacity_error("send peak byte count overflow"))?;
            let peak_send_directory = current_total
                .checked_sub(current_raw_bytes)
                .and_then(|bytes| bytes.checked_add(user_frame))
                .and_then(|bytes| bytes.checked_add(send_directory_candidate))
                .ok_or_else(|| capacity_error("send peak byte count overflow"))?;
            let after_send = current_total
                .checked_sub(current_raw_bytes)
                .and_then(|bytes| bytes.checked_sub(current_directory_bytes))
                .and_then(|bytes| bytes.checked_add(user_frame))
                .and_then(|bytes| bytes.checked_add(send_directory_candidate))
                .ok_or_else(|| capacity_error("completion peak byte count overflow"))?;
            let peak_completion_raw = after_send
                .checked_add(full_frame)
                .ok_or_else(|| capacity_error("completion peak byte count overflow"))?;
            let peak_completion_directory = after_send
                .checked_sub(user_frame)
                .and_then(|bytes| bytes.checked_add(full_frame))
                .and_then(|bytes| bytes.checked_add(final_directory_candidate))
                .ok_or_else(|| capacity_error("completion peak byte count overflow"))?;
            peak_send_raw
                .max(peak_send_directory)
                .max(peak_completion_raw)
                .max(peak_completion_directory)
        } else {
            let peak_raw = current_total
                .checked_add(full_frame)
                .ok_or_else(|| capacity_error("send peak byte count overflow"))?;
            let peak_directory = current_total
                .checked_sub(current_raw_bytes)
                .and_then(|bytes| bytes.checked_add(full_frame))
                .and_then(|bytes| bytes.checked_add(final_directory_candidate))
                .ok_or_else(|| capacity_error("send peak byte count overflow"))?;
            peak_raw.max(peak_directory)
        };
        if peak > MAX_ROOT_BYTES {
            Err(capacity_error("send peak byte budget exceeded"))
        } else {
            Ok(())
        }
    }

    fn next_directory_revision(directory: &Directory) -> Result<u64, AppError> {
        directory
            .revision
            .checked_add(1)
            .filter(|revision| *revision <= MAX_SAFE_INTEGER)
            .ok_or_else(|| capacity_error("session directory revision exhausted"))
    }
}

impl ChatHistory for DiskHistory {
    fn open_directory(&mut self) -> Result<Option<HistoryRecovery>, AppError> {
        let recovery = self.open_directory_inner()?;
        Ok(Some(recovery))
    }

    fn last_commit_receipts(&self) -> HistoryCommitReceipts {
        self.last_receipts.clone()
    }

    fn session_storage_hint(&self, session_id: &str) -> Result<String, AppError> {
        self.directory()?
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.raw_hint.clone())
            .ok_or_else(|| {
                AppError::session_directory_conflict(HistoryFailure(
                    "session storage hint is unavailable",
                ))
            })
    }

    fn open(
        &mut self,
        mut access: HistoryAccess<'_>,
        requests: &[HistoryRequest],
        expected_sessions_revision: u64,
    ) -> Result<HistoryRecovery, AppError> {
        self.ready()?;
        self.check_revision(expected_sessions_revision)?;
        self.last_receipts = HistoryCommitReceipts::default();
        self.publish_unsettled_summary(&mut access, requests)
            .map_err(|error| stage_error("publish unresolved recovery summary", error))?;
        let previous_directory = Self::encode_directory(self.directory()?)?;
        self.finish_pending_retirements(&mut access, requests)
            .map_err(|error| stage_error("pending retirement recovery", error))?;
        let mut directory = self.directory()?.clone();
        self.maintain_directory_retention(&mut access, requests, &mut directory)
            .map_err(|error| stage_error("startup retention commit", error))?;
        let recovery_directory_changed = previous_directory != Self::encode_directory(&directory)?;
        self.directory = Some(directory.clone());
        let active = directory
            .sessions
            .iter()
            .find(|session| session.id == directory.active_session_id)
            .ok_or_else(|| AppError::from_storage(HistoryFailure("active session is missing")))?;
        if active.id != access.logical_session_id {
            return Err(AppError::session_directory_conflict(HistoryFailure(
                "application and directory active session differ",
            )));
        }
        let (history, status) = self
            .load_session_history(&access, active, MAX_HISTORY_MESSAGES)
            .map_err(|error| stage_error("startup projection", error))?;
        if active.archived {
            let mut next = directory.clone();
            let active = next
                .sessions
                .iter_mut()
                .find(|session| session.id == next.active_session_id)
                .expect("active session validated");
            let mut changed = recovery_directory_changed
                || active.status != status
                || !self.last_receipts.raw.is_empty();
            active.status = status;
            if status != ChatStatus::Unknown && active.external_unknown {
                active.external_unknown = false;
                changed = true;
            }
            if let Some(previous) = active.active_segment.take()
                && let Some(previous) = active.segments.iter_mut().find(|item| item.id == previous)
            {
                previous.sealed = true;
                changed = true;
            }
            let directory_receipt = if changed {
                next.revision = Self::next_directory_revision(&directory)?;
                let bytes = Self::encode_directory(&next)?.len() as u64;
                self.preflight_peak(bytes)?;
                Some(self.publish_directory(next)?)
            } else {
                None
            };
            self.active_store = None;
            self.active_segment_id = None;
            let mut recovery = self.recovery(history)?;
            recovery.receipts.directory = directory_receipt;
            recovery.receipts.raw = self.last_receipts.raw.clone();
            return Ok(recovery);
        }
        let (next_sequence, segment) = self.next_segment(&directory)?;
        let mut next = directory.clone();
        next.next_segment_sequence = next_sequence;
        next.revision = Self::next_directory_revision(&directory)?;
        let active = next
            .sessions
            .iter_mut()
            .find(|session| session.id == next.active_session_id)
            .expect("active session validated");
        active.status = status;
        if status != ChatStatus::Unknown {
            active.external_unknown = false;
        }
        if let Some(previous) = active.active_segment.take()
            && let Some(previous) = active.segments.iter_mut().find(|item| item.id == previous)
        {
            previous.sealed = true;
        }
        active.segments.push(segment.clone());
        active.active_segment = Some(segment.id.clone());
        if next
            .sessions
            .iter()
            .map(|session| session.segments.len())
            .sum::<usize>()
            > MAX_SEGMENTS
        {
            return Err(capacity_error("managed raw segment count exceeded"));
        }
        let directory_bytes = Self::encode_directory(&next)?.len() as u64;
        let new_frame = Self::raw_frame_estimate(&mut access)?;
        self.preflight_peak(
            new_frame
                .checked_add(directory_bytes)
                .and_then(|bytes| bytes.checked_add(CANDIDATE_MARKER_BYTES))
                .ok_or_else(|| capacity_error("initial segment peak byte count overflow"))?,
        )?;
        let (store, _raw_receipt) = self.create_segment(&mut access, &segment)?;
        let directory_receipt = self.publish_directory(next).inspect_err(|_| {
            self.poisoned = true;
        })?;
        fs::remove_file(self.segment_path(&segment).join(".candidate")).map_err(|error| {
            self.poisoned = true;
            AppError::from_storage(error)
        })?;
        self.active_store = Some(store);
        self.active_segment_id = Some(segment.id);
        let mut recovery = self.recovery(history)?;
        if let Some(session) = recovery
            .sessions
            .iter_mut()
            .find(|session| session.id == recovery.active_session_id)
        {
            session.status = status;
        }
        recovery.receipts = self.last_receipts.clone();
        debug_assert_eq!(recovery.receipts.directory, Some(directory_receipt));
        Ok(recovery)
    }

    fn checkpoint(
        &mut self,
        mut access: HistoryAccess<'_>,
        expected_sessions_revision: u64,
    ) -> Result<HistoryCommitReceipts, AppError> {
        self.last_receipts = HistoryCommitReceipts::default();
        self.ready()?;
        self.check_revision(expected_sessions_revision)?;
        if access.logical_session_id != self.directory()?.active_session_id {
            return Err(AppError::session_directory_conflict(HistoryFailure(
                "checkpoint targets a non-active session",
            )));
        }
        let mut next = self.directory()?.clone();
        let next_external_unknown = access.external_unknown
            || (access.online
                && access.external_request_pending
                && access.status == ChatStatus::Sending);
        let session = next
            .sessions
            .iter_mut()
            .find(|session| session.id == access.logical_session_id)
            .expect("active session validated");
        let directory_changed =
            session.status != access.status || session.external_unknown != next_external_unknown;
        session.status = access.status;
        session.external_unknown = next_external_unknown;
        let directory_candidate = if directory_changed {
            Self::encode_directory(&next)?.len() as u64
        } else {
            0
        };
        let estimate = Self::raw_frame_estimate(&mut access)?;
        self.preflight_peak(
            estimate
                .checked_add(directory_candidate)
                .ok_or_else(|| capacity_error("checkpoint peak size overflow"))?,
        )?;
        let store = self.active_store.as_mut().ok_or_else(|| {
            AppError::from_storage(HistoryFailure("active raw segment is unavailable"))
        })?;
        let domain = RawDomainKey::from_request(access.ingress, access.request)
            .map_err(AppError::from_storage)?;
        let raw_access = RawLocalAccess::new(
            access.runtime,
            access.ingress,
            access.request,
            access.grant,
            domain,
            access.now_ms,
        )
        .map_err(AppError::from_storage)?;
        let raw_commit = store
            .checkpoint(raw_access, store.revision())
            .map_err(AppError::from_storage)?;
        self.last_receipts.raw.push(raw_receipt(raw_commit));
        let directory = if directory_changed {
            Some(self.publish_directory(next).inspect_err(|_| {
                self.poisoned = true;
            })?)
        } else {
            None
        };
        if let Some(directory) = directory {
            self.last_receipts.directory = Some(directory);
        }
        Ok(self.last_receipts.clone())
    }

    fn preflight_checkpoint(
        &mut self,
        mut access: HistoryAccess<'_>,
        expected_sessions_revision: u64,
        user_characters: usize,
        reserved_reply_characters: usize,
    ) -> Result<(), AppError> {
        self.ready()?;
        self.check_revision(expected_sessions_revision)?;
        if access.logical_session_id != self.directory()?.active_session_id {
            return Err(AppError::session_directory_conflict(HistoryFailure(
                "preflight targets a non-active session",
            )));
        }
        let base = Self::raw_frame_estimate(&mut access)?;
        let current_total = self.tree_bytes()?;
        let current_directory_bytes = fs::metadata(self.root.join(DIRECTORY_FILE))
            .map_err(AppError::from_storage)?
            .len();
        let current_session = self
            .directory()?
            .sessions
            .iter()
            .find(|session| session.id == access.logical_session_id)
            .expect("active session validated");
        let current_segment = current_session
            .active_segment
            .as_ref()
            .and_then(|id| {
                current_session
                    .segments
                    .iter()
                    .find(|segment| segment.id == *id)
            })
            .ok_or_else(|| {
                AppError::session_directory_conflict(HistoryFailure(
                    "preflight has no writable segment",
                ))
            })?;
        let current_raw_bytes =
            fs::metadata(self.segment_path(current_segment).join("raw-local.v1"))
                .map_err(AppError::from_storage)?
                .len();
        Self::preflight_reserved_send(
            SendPeakContext {
                directory: self.directory()?,
                session_id: access.logical_session_id,
                current_total,
                current_raw_bytes,
                current_directory_bytes,
                base_frame_bytes: base,
                online: access.online,
            },
            HistorySendReservation {
                user_characters,
                reserved_reply_characters,
            },
        )
    }

    fn rotate(
        &mut self,
        mut current: HistoryAccess<'_>,
        mut next_access: HistoryAccess<'_>,
        expected_sessions_revision: u64,
        change: HistoryChange,
        send_reservation: Option<HistorySendReservation>,
    ) -> Result<HistoryRecovery, AppError> {
        self.last_receipts = HistoryCommitReceipts::default();
        self.ready()?;
        self.check_revision(expected_sessions_revision)?;
        let mut directory = self.directory()?.clone();
        if directory.active_session_id != current.logical_session_id {
            return Err(AppError::session_directory_conflict(HistoryFailure(
                "rotation source is not the directory active session",
            )));
        }
        let target_id = next_access.logical_session_id.to_owned();
        if let HistoryChange::Switch(id) | HistoryChange::Renew(id) = &change {
            let request = HistoryRequest {
                logical_session_id: id.clone(),
                request: next_access.request.clone(),
                grant: next_access.grant.clone(),
            };
            let target = directory
                .sessions
                .iter_mut()
                .find(|session| session.id == *id)
                .ok_or_else(|| {
                    AppError::session_directory_conflict(HistoryFailure(
                        "rotation target is not in the directory",
                    ))
                })?;
            self.maintain_session_retention(&mut next_access, &request, target)
                .map_err(|error| stage_error("rotation target retention commit", error))?;
        }
        let (mut proposed, target_status, history) = match change {
            HistoryChange::Create(new_session) => {
                if new_session.id != target_id
                    || directory.sessions.iter().any(|s| s.id == target_id)
                {
                    return Err(AppError::session_directory_conflict(HistoryFailure(
                        "new session identifier conflicts with the directory",
                    )));
                }
                let (root_sessions, _) = self.root_usage()?;
                if root_sessions >= MAX_SESSIONS {
                    return Err(capacity_error("logical session count exceeded"));
                }
                let mut proposed = directory.clone();
                proposed.sessions.push(Session {
                    id: new_session.id,
                    label: new_session.label,
                    raw_hint: target_id.clone(),
                    archived: false,
                    status: ChatStatus::Ready,
                    external_unknown: false,
                    recovered_unsettled: false,
                    active_segment: None,
                    segments: Vec::new(),
                });
                proposed.active_session_id = target_id.clone();
                (proposed, ChatStatus::Ready, Vec::new())
            }
            HistoryChange::Switch(id) => {
                if id != target_id {
                    return Err(AppError::session_directory_conflict(HistoryFailure(
                        "switch target differs from trusted request scope",
                    )));
                }
                let target = directory
                    .sessions
                    .iter()
                    .find(|session| session.id == id)
                    .ok_or_else(|| {
                        AppError::session_directory_conflict(HistoryFailure(
                            "switch target is not in the directory",
                        ))
                    })?;
                let (history, status) = self
                    .load_session_history(&next_access, target, MAX_HISTORY_MESSAGES)
                    .map_err(|error| stage_error("switch history projection", error))?;
                let mut proposed = directory.clone();
                proposed.active_session_id = id;
                (proposed, status, history)
            }
            HistoryChange::Renew(id) => {
                if id != target_id || id != directory.active_session_id {
                    return Err(AppError::session_directory_conflict(HistoryFailure(
                        "renewal target is not the active session",
                    )));
                }
                let target = directory
                    .sessions
                    .iter()
                    .find(|session| session.id == id)
                    .expect("active session validated");
                let (history, status) = self
                    .load_session_history(&next_access, target, MAX_HISTORY_MESSAGES)
                    .map_err(|error| stage_error("renew history projection", error))?;
                (directory.clone(), status, history)
            }
        };
        proposed.revision = Self::next_directory_revision(&directory)?;
        let target_archived = proposed
            .sessions
            .iter()
            .find(|session| session.id == target_id)
            .expect("target session prepared")
            .archived;
        let new_segment = if target_archived {
            None
        } else {
            let (next_sequence, segment) = self.next_segment(&proposed)?;
            proposed.next_segment_sequence = next_sequence;
            Some(segment)
        };
        let target = proposed
            .sessions
            .iter_mut()
            .find(|session| session.id == target_id)
            .expect("target session prepared");
        target.status = target_status;
        if target_status != ChatStatus::Unknown {
            target.external_unknown = false;
        }
        if let Some(previous) = target.active_segment.take()
            && let Some(previous) = target.segments.iter_mut().find(|item| item.id == previous)
        {
            previous.sealed = true;
        }
        if let Some(segment) = &new_segment {
            target.segments.push(segment.clone());
            target.active_segment = Some(segment.id.clone());
        }
        let source = proposed
            .sessions
            .iter_mut()
            .find(|session| session.id == directory.active_session_id)
            .expect("source session validated");
        source.status = current.status;
        source.external_unknown = current.external_unknown;
        if directory.active_session_id != target_id
            && let Some(previous) = source.active_segment.take()
            && let Some(previous) = source.segments.iter_mut().find(|item| item.id == previous)
        {
            previous.sealed = true;
        }
        if proposed
            .sessions
            .iter()
            .map(|session| session.segments.len())
            .sum::<usize>()
            > MAX_SEGMENTS
        {
            return Err(capacity_error("managed raw segment count exceeded"));
        }
        let directory_candidate = Self::encode_directory(&proposed)?.len() as u64;
        let new_candidate = if new_segment.is_some() {
            Self::raw_frame_estimate(&mut next_access)?
        } else {
            0
        };
        let source_session = directory
            .sessions
            .iter()
            .find(|session| session.id == directory.active_session_id)
            .expect("source session validated");
        let old_segment = source_session.active_segment.as_ref().and_then(|id| {
            source_session
                .segments
                .iter()
                .find(|segment| segment.id == *id)
        });
        let old_current_bytes = if let Some(segment) = old_segment {
            fs::metadata(self.segment_path(segment).join("raw-local.v1"))
                .map_err(AppError::from_storage)?
                .len()
        } else {
            0
        };
        let old_candidate = if old_segment.is_some() {
            Self::raw_frame_estimate(&mut current)?
        } else {
            0
        };
        let current_total = self.tree_bytes()?;
        let current_directory_bytes = fs::metadata(self.root.join(DIRECTORY_FILE))
            .map_err(AppError::from_storage)?
            .len();
        let seal_peak = current_total
            .checked_add(old_candidate)
            .ok_or_else(|| capacity_error("segment seal peak byte count overflow"))?;
        let publish_peak = current_total
            .checked_sub(old_current_bytes)
            .and_then(|bytes| bytes.checked_add(old_candidate))
            .and_then(|bytes| bytes.checked_add(new_candidate))
            .and_then(|bytes| bytes.checked_add(directory_candidate))
            .and_then(|bytes| {
                bytes.checked_add(if new_segment.is_some() {
                    CANDIDATE_MARKER_BYTES
                } else {
                    0
                })
            })
            .ok_or_else(|| capacity_error("rotation peak byte count overflow"))?;
        let rotation_peak = seal_peak.max(publish_peak);
        if rotation_peak > MAX_ROOT_BYTES {
            return Err(capacity_error("rotation peak byte budget exceeded"));
        }
        if let Some(reservation) = send_reservation {
            if new_segment.is_none() {
                return Err(AppError::session_directory_conflict(HistoryFailure(
                    "send reservation requires a writable rotated segment",
                )));
            }
            let post_rotation_total = current_total
                .checked_sub(old_current_bytes)
                .and_then(|bytes| bytes.checked_sub(current_directory_bytes))
                .and_then(|bytes| bytes.checked_add(old_candidate))
                .and_then(|bytes| bytes.checked_add(new_candidate))
                .and_then(|bytes| bytes.checked_add(directory_candidate))
                .ok_or_else(|| capacity_error("rotated send reservation size overflow"))?;
            Self::preflight_reserved_send(
                SendPeakContext {
                    directory: &proposed,
                    session_id: &target_id,
                    current_total: post_rotation_total,
                    current_raw_bytes: new_candidate,
                    current_directory_bytes: directory_candidate,
                    base_frame_bytes: new_candidate,
                    online: next_access.online,
                },
                reservation,
            )?;
        }
        if old_segment.is_some() {
            #[cfg(test)]
            if injected_failure(FailurePoint::Seal) {
                self.poisoned = true;
                return Err(AppError::from_storage(HistoryFailure(
                    "injected segment seal failure",
                )));
            }
            let store = self.active_store.as_mut().ok_or_else(|| {
                AppError::from_storage(HistoryFailure("active raw segment is unavailable"))
            })?;
            let domain = RawDomainKey::from_request(current.ingress, current.request)
                .map_err(AppError::from_storage)?;
            let raw_access = RawLocalAccess::new(
                current.runtime,
                current.ingress,
                current.request,
                current.grant,
                domain,
                current.now_ms,
            )
            .map_err(AppError::from_storage)?;
            let receipt = store
                .checkpoint(raw_access, store.revision())
                .map_err(AppError::from_storage)
                .map_err(|error| stage_error("seal current segment", error))?;
            self.last_receipts.raw.push(raw_receipt(receipt));
            #[cfg(test)]
            if injected_failure(FailurePoint::AfterSeal) {
                self.poisoned = true;
                return Err(AppError::from_storage(HistoryFailure(
                    "injected post-seal failure",
                )));
            }
        }
        let _old_store = self.active_store.take();
        self.active_segment_id = None;
        self.poisoned = true;
        let new_store = if let Some(segment) = &new_segment {
            let (store, _receipt) = self
                .create_segment(&mut next_access, segment)
                .map_err(|error| stage_error("create new segment", error))?;
            Some(store)
        } else {
            None
        };
        let directory_receipt = self.publish_directory(proposed)?;
        if let Some(segment) = &new_segment
            && let Err(error) = fs::remove_file(self.segment_path(segment).join(".candidate"))
        {
            self.poisoned = true;
            return Err(AppError::from_storage(error));
        }
        self.active_store = new_store;
        self.active_segment_id = new_segment.map(|segment| segment.id);
        let mut recovery = self.recovery(history)?;
        if let Some(session) = recovery
            .sessions
            .iter_mut()
            .find(|session| session.id == recovery.active_session_id)
        {
            session.status = target_status;
        }
        recovery.receipts = self.last_receipts.clone();
        debug_assert_eq!(recovery.receipts.directory, Some(directory_receipt));
        Ok(recovery)
    }

    fn archive(
        &mut self,
        access: HistoryAccess<'_>,
        session_id: &str,
        expected_sessions_revision: u64,
    ) -> Result<HistoryRecovery, AppError> {
        self.last_receipts = HistoryCommitReceipts::default();
        self.ready()?;
        self.check_revision(expected_sessions_revision)?;
        if session_id != access.logical_session_id
            || session_id != self.directory()?.active_session_id
        {
            return Err(AppError::session_directory_conflict(HistoryFailure(
                "archive target is not the active session",
            )));
        }
        let mut next = self.directory()?.clone();
        next.revision = Self::next_directory_revision(&next)?;
        let session = next
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| {
                AppError::session_directory_conflict(HistoryFailure("archive target is missing"))
            })?;
        session.archived = true;
        session.status = access.status;
        session.external_unknown = access.external_unknown;
        let bytes = Self::encode_directory(&next)?.len() as u64;
        self.preflight_peak(bytes)?;
        let directory = self.publish_directory(next)?;
        let mut recovery = self.recovery(Vec::new())?;
        recovery.receipts.directory = Some(directory);
        Ok(recovery)
    }

    fn load_history(
        &mut self,
        access: HistoryAccess<'_>,
        session_id: &str,
        max_messages: usize,
    ) -> Result<Vec<ChatMessage>, AppError> {
        self.ready()?;
        if session_id != access.logical_session_id {
            return Err(AppError::session_directory_conflict(HistoryFailure(
                "history request scope differs from the requested session",
            )));
        }
        let domain = RawDomainKey::from_request(access.ingress, access.request)
            .map_err(AppError::from_storage)?;
        let _validated = RawLocalAccess::new(
            access.runtime,
            access.ingress,
            access.request,
            access.grant,
            domain,
            access.now_ms,
        )
        .map_err(AppError::from_storage)?;
        drop(_validated);
        let session = self
            .directory()?
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
            .ok_or_else(|| {
                AppError::session_directory_conflict(HistoryFailure(
                    "history session is not in the directory",
                ))
            })?;
        let (mut messages, _) = self.load_session_history(&access, &session, max_messages)?;
        let start = messages.len().saturating_sub(max_messages);
        if start > 0 {
            messages.drain(..start);
        }
        Ok(messages)
    }

    fn cold_session_ids(&self, max_sessions: usize) -> Vec<String> {
        let Some(directory) = self.directory.as_ref() else {
            return Vec::new();
        };
        let work = directory
            .sessions
            .iter()
            .flat_map(|session| {
                session
                    .segments
                    .iter()
                    .filter(|segment| {
                        segment.sealed
                            && Some(segment.id.as_str()) != self.active_segment_id.as_deref()
                    })
                    .map(move |_| session.id.clone())
            })
            .collect::<Vec<_>>();
        if work.is_empty() || max_sessions == 0 {
            return Vec::new();
        }
        let mut ids = Vec::new();
        for offset in 0..work.len().min(max_sessions) {
            let id = &work[(self.cold_cursor + offset) % work.len()];
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids
    }

    fn cold_segment_count(&self) -> usize {
        self.directory
            .as_ref()
            .into_iter()
            .flat_map(|directory| directory.sessions.iter())
            .flat_map(|session| session.segments.iter())
            .filter(|segment| {
                segment.sealed && Some(segment.id.as_str()) != self.active_segment_id.as_deref()
            })
            .count()
    }

    fn maintain_cold(
        &mut self,
        access: HistoryAccess<'_>,
        requests: &[HistoryRequest],
        max_segments: usize,
    ) -> Result<ColdHistoryMaintenance, AppError> {
        self.last_receipts = HistoryCommitReceipts::default();
        self.ready()?;
        let mut access = access;
        let directory = self.directory()?.clone();
        let work = directory
            .sessions
            .iter()
            .flat_map(|session| {
                session
                    .segments
                    .iter()
                    .filter(|segment| segment.sealed)
                    .map(move |segment| (session.id.clone(), segment.clone()))
            })
            .collect::<Vec<_>>();
        if work.is_empty() || max_segments == 0 {
            return Ok(ColdHistoryMaintenance::default());
        }
        let count = work.len().min(max_segments);
        let mut maintained = 0;
        let mut raw_commits = Vec::new();
        let mut blocked = None;
        for offset in 0..count {
            let position = (self.cold_cursor + offset) % work.len();
            let (session_id, segment) = &work[position];
            let Some(request) = requests
                .iter()
                .find(|request| request.logical_session_id == *session_id)
            else {
                continue;
            };
            let domain = RawDomainKey::from_request(access.ingress, &request.request)
                .map_err(AppError::from_storage)?;
            let _validated = RawLocalAccess::new(
                access.runtime,
                access.ingress,
                &request.request,
                &request.grant,
                domain.clone(),
                access.now_ms,
            )
            .map_err(AppError::from_storage)?;
            drop(_validated);
            let mut scratch = scratch_runtime(&access)?;
            let grant = scratch
                .install_raw_history_grant(access.ingress, domain.clone())
                .map_err(AppError::from_storage)?;
            let raw_access = RawLocalAccess::new(
                &mut scratch,
                access.ingress,
                &request.request,
                &grant,
                domain.clone(),
                access.now_ms,
            )
            .map_err(AppError::from_storage)?;
            let path = self.segment_path(segment);
            let current_total = self.tree_bytes()?;
            let current_frame = fs::metadata(path.join("raw-local.v1"))
                .map_err(AppError::from_storage)?
                .len();
            let mut store = RawLocalStore::open(&path, OpenMode::OpenExisting, raw_access)
                .map_err(AppError::from_storage)
                .map_err(|error| stage_error("open cold segment", error))?
                .store;
            let revision = store.revision();
            let plan = store
                .prepare_retention(
                    RawLocalAccess::new(
                        &mut scratch,
                        access.ingress,
                        &request.request,
                        &grant,
                        domain.clone(),
                        access.now_ms,
                    )
                    .map_err(AppError::from_storage)?,
                    access.retention,
                    revision,
                )
                .map_err(AppError::from_storage)
                .map_err(|error| stage_error("preview cold retention", error))?;
            let preview = plan.preview();
            let retirement_directory_bytes =
                if segment.path != "." && preview.retained_fragments == 0 {
                    let frame_revision = revision
                        .checked_add(u64::from(preview.changed))
                        .ok_or_else(|| capacity_error("retirement frame revision exhausted"))?;
                    let committed_at_ms = if preview.changed {
                        access.now_ms
                    } else {
                        store.committed_at_ms()
                    };
                    let authorization_revision = access
                        .ingress
                        .stable_credential_revision(request.request.credential())
                        .ok_or_else(|| {
                            AppError::from_storage(HistoryFailure(
                                "retirement authorization revision is unavailable",
                            ))
                        })?;
                    self.retirement_directory_candidate_bytes(
                        session_id,
                        &segment.id,
                        &domain,
                        authorization_revision,
                        frame_revision,
                        committed_at_ms,
                    )?
                } else {
                    0
                };
            if let Err(error) = Self::preflight_retention_peak(
                current_total,
                current_frame,
                preview,
                retirement_directory_bytes,
            ) {
                if error
                    .source()
                    .is_some_and(|source| source.downcast_ref::<CapacityFailure>().is_some())
                {
                    blocked = Some(ColdMaintenanceBlock::CapacityExceeded);
                    break;
                }
                return Err(error);
            }
            let raw_access = RawLocalAccess::new(
                &mut scratch,
                access.ingress,
                &request.request,
                &grant,
                RawDomainKey::from_request(access.ingress, &request.request)
                    .map_err(AppError::from_storage)?,
                access.now_ms,
            )
            .map_err(AppError::from_storage)?;
            let mut verify_runtime = scratch_runtime(&access)?;
            let verify_grant = verify_runtime
                .install_raw_history_grant(access.ingress, domain.clone())
                .map_err(AppError::from_storage)?;
            let verify_access = RawLocalAccess::new(
                &mut verify_runtime,
                access.ingress,
                &request.request,
                &verify_grant,
                domain.clone(),
                access.now_ms,
            )
            .map_err(AppError::from_storage)?;
            let (checkpoint, verified) = if preview.changed {
                let receipt = store
                    .maintain_retention_prepared(raw_access, plan)
                    .map_err(AppError::from_storage)
                    .map_err(|error| stage_error("maintain cold segment", error))?;
                let verify_revision = store.revision();
                let verified = store
                    .read_projection(verify_access, access.retention, verify_revision)
                    .map_err(AppError::from_storage)
                    .map_err(|error| stage_error("verify cold segment expiry", error))?;
                (receipt.checkpoint, verified)
            } else {
                let verified = store
                    .read_projection_prepared(verify_access, plan)
                    .map_err(AppError::from_storage)
                    .map_err(|error| stage_error("verify cold segment expiry", error))?;
                (None, verified)
            };
            let retire = segment.path != "."
                && preview.retained_fragments == 0
                && verified.import.fragments.is_empty();
            if let Some(receipt) = checkpoint {
                let receipt = raw_receipt(receipt);
                raw_commits.push(receipt);
                self.last_receipts.raw.push(receipt);
            }
            maintained += 1;
            if retire {
                let verified_frame = store
                    .verify_retirable(
                        RawLocalAccess::new(
                            &mut scratch,
                            access.ingress,
                            &request.request,
                            &grant,
                            domain.clone(),
                            access.now_ms,
                        )
                        .map_err(AppError::from_storage)?,
                        access.retention,
                        store.revision(),
                    )
                    .map_err(AppError::from_storage)?;
                self.begin_cold_retirement(
                    &mut access,
                    request,
                    RetirementTarget {
                        session_id,
                        segment,
                    },
                    &mut store,
                    &mut scratch,
                    verified_frame,
                )?;
            }
            drop(store);
            if retire {
                self.finish_pending_retirements(&mut access, requests)?;
            }
        }
        self.cold_cursor = (self.cold_cursor + maintained) % work.len();
        Ok(ColdHistoryMaintenance {
            segments_processed: maintained,
            raw_commits,
            blocked,
        })
    }
}

fn project_segment(
    store: &mut RawLocalStore,
    access: &HistoryAccess<'_>,
    event_budget: usize,
    body_byte_budget: usize,
    frame_byte_budget: u64,
) -> Result<SegmentProjection, AppError> {
    let mut runtime = scratch_runtime(access)?;
    let domain = RawDomainKey::from_request(access.ingress, access.request)
        .map_err(AppError::from_storage)?;
    let grant = runtime
        .install_raw_history_grant(access.ingress, domain.clone())
        .map_err(AppError::from_storage)?;
    let revision = store.revision();
    let plan = store
        .prepare_projection(
            RawLocalAccess::new(
                &mut runtime,
                access.ingress,
                access.request,
                &grant,
                domain.clone(),
                access.now_ms,
            )
            .map_err(AppError::from_storage)?,
            access.retention,
            revision,
            frame_byte_budget,
            event_budget,
            body_byte_budget,
        )
        .map_err(AppError::from_storage)?;
    project_prepared_segment(store, access, runtime, grant, plan)
}

fn project_prepared_segment(
    store: &mut RawLocalStore,
    access: &HistoryAccess<'_>,
    mut runtime: SessionRuntime,
    grant: moe_agent_runtime::RawHistoryGrant,
    plan: moe_raw_local::RetentionPlan,
) -> Result<SegmentProjection, AppError> {
    let domain = RawDomainKey::from_request(access.ingress, access.request)
        .map_err(AppError::from_storage)?;
    let preview = plan.preview();
    let frame_bytes_read = plan.frame_bytes_read();
    let decoded_event_count = plan.decoded_event_count();
    let decoded_body_bytes = plan.decoded_body_bytes();
    if !plan.within_projection_budget() {
        return Ok(SegmentProjection {
            events: Vec::new(),
            latest_user_interaction: preview.latest_user_interaction,
            has_unsettled_user_interaction: preview.has_unsettled_user_interaction,
            frame_bytes_read,
            decoded_event_count,
            decoded_body_bytes,
            body_loaded: false,
        });
    }
    let raw_access = RawLocalAccess::new(
        &mut runtime,
        access.ingress,
        access.request,
        &grant,
        domain,
        access.now_ms,
    )
    .map_err(AppError::from_storage)?;
    let receipt = store
        .read_projection_prepared(raw_access, plan)
        .map_err(AppError::from_storage)?;
    let mut events = Vec::new();
    for fragment in receipt.import.fragments {
        if fragment.zone != moe_agent_runtime::RetentionZone::Normal {
            continue;
        }
        let capability = runtime
            .install_management_capability(
                access.ingress,
                access.request.identity().clone(),
                access.request.isolation().clone(),
                vec![fragment.session.clone()],
                ManagementPermissions::default().with_view(),
            )
            .map_err(AppError::from_storage)?;
        let snapshot = runtime
            .inspect_fragment(
                SessionAccess::new(access.ingress, access.request, access.now_ms),
                &capability,
                &fragment.session,
            )
            .map_err(AppError::from_storage)?;
        events.extend(snapshot.events);
    }
    Ok(SegmentProjection {
        events,
        latest_user_interaction: preview.latest_user_interaction,
        has_unsettled_user_interaction: preview.has_unsettled_user_interaction,
        frame_bytes_read,
        decoded_event_count,
        decoded_body_bytes,
        body_loaded: true,
    })
}

fn scratch_runtime(access: &HistoryAccess<'_>) -> Result<SessionRuntime, AppError> {
    let limits = SessionLimits::new(
        8,
        MAX_HISTORY_EVENTS_PER_READ,
        MAX_HISTORY_EVENTS_PER_READ,
        8_192,
        MAX_HISTORY_BODY_BYTES_PER_READ,
        2,
    )
    .map_err(AppError::from_storage)?;
    Ok(SessionRuntime::with_retention(
        access.ingress,
        limits,
        SessionTiming::default(),
        *access.retention,
        access.now_ms,
    ))
}

fn restored_status_evidence(
    saved: ChatStatus,
    latest: Option<(u64, usize, Option<TerminalState>)>,
    has_unsettled_user_interaction: bool,
) -> ChatStatus {
    if saved == ChatStatus::Unknown || has_unsettled_user_interaction {
        return ChatStatus::Unknown;
    }
    if let Some((_, _, terminal)) = latest {
        return match terminal {
            None | Some(TerminalState::Unknown | TerminalState::Cancelled) => ChatStatus::Unknown,
            Some(TerminalState::Failed) => ChatStatus::Failed,
            Some(TerminalState::Completed) => ChatStatus::Ready,
        };
    }
    if saved == ChatStatus::Sending {
        ChatStatus::Unknown
    } else {
        saved
    }
}

fn status_text(status: ChatStatus) -> &'static str {
    match status {
        ChatStatus::Ready => "ready",
        ChatStatus::Sending => "sending",
        ChatStatus::Failed => "failed",
        ChatStatus::Unknown => "unknown",
    }
}

fn raw_receipt(receipt: CommitReceipt) -> RawHistoryCommitReceipt {
    RawHistoryCommitReceipt {
        revision: receipt.revision,
        committed_at_ms: receipt.committed_at_ms,
        file_synced: true,
        directory_synced: receipt.durability == CommitDurability::FileAndDirectorySynced,
    }
}

fn raw_commit_receipt(receipt: RawHistoryCommitReceipt) -> CommitReceipt {
    CommitReceipt {
        revision: receipt.revision,
        committed_at_ms: receipt.committed_at_ms,
        durability: if receipt.directory_synced {
            CommitDurability::FileAndDirectorySynced
        } else {
            CommitDurability::FileSyncedDirectorySyncUnavailable
        },
    }
}

fn retirement_rank(stage: RetirementStage) -> u8 {
    match stage {
        RetirementStage::Marked => 0,
        RetirementStage::PayloadRemoved => 1,
        RetirementStage::LockRemoved => 2,
        RetirementStage::DirectoryRemoved => 3,
    }
}

fn raw_stage_to_history(stage: RawStage) -> RetirementStage {
    match stage {
        RawStage::Retiring => RetirementStage::Marked,
        RawStage::PayloadRemoved => RetirementStage::PayloadRemoved,
        RawStage::LockRemoved => RetirementStage::LockRemoved,
        RawStage::DirectoryRemoved => RetirementStage::DirectoryRemoved,
    }
}

fn parse_status(value: &str) -> Result<ChatStatus, AppError> {
    match value {
        "ready" => Ok(ChatStatus::Ready),
        "sending" => Ok(ChatStatus::Sending),
        "failed" => Ok(ChatStatus::Failed),
        "unknown" => Ok(ChatStatus::Unknown),
        _ => Err(AppError::from_storage(HistoryFailure(
            "session directory contains an unknown status",
        ))),
    }
}

fn encode_retirement(retirement: &Retirement) -> serde_json::Value {
    serde_json::json!({
        "id": retirement.id,
        "sessionId": retirement.session_id,
        "domain": retirement.domain.encode(),
        "authorizationRevision": retirement.authorization_revision,
        "frame": {
            "revision": retirement.frame.revision,
            "committedAtMs": retirement.frame.committed_at_ms,
            "fileSynced": retirement.frame.file_synced,
            "directorySynced": retirement.frame.directory_synced,
        },
        "stage": retirement.stage.text(),
    })
}

fn parse_retirement(value: &serde_json::Value) -> Result<Retirement, AppError> {
    let string = |object: &serde_json::Value, name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| AppError::from_storage(HistoryFailure("retirement field is invalid")))
    };
    let number = |object: &serde_json::Value, name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| AppError::from_storage(HistoryFailure("retirement receipt is invalid")))
    };
    let boolean = |object: &serde_json::Value, name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| AppError::from_storage(HistoryFailure("retirement receipt is invalid")))
    };
    let domain_value = value
        .get("domain")
        .ok_or_else(|| AppError::from_storage(HistoryFailure("retirement domain is missing")))?;
    let domain = StableDomainRecord {
        workspace_id: string(domain_value, "workspaceId")?,
        user_id: string(domain_value, "userId")?,
        instance_id: string(domain_value, "instanceId")?,
        subject: string(domain_value, "subject")?,
        channel: string(domain_value, "channel")?,
        scope: string(domain_value, "scope")?,
        credential: string(domain_value, "credential")?,
        hint: string(domain_value, "hint")?,
    };
    let frame_value = value
        .get("frame")
        .ok_or_else(|| AppError::from_storage(HistoryFailure("retirement frame is missing")))?;
    let receipt = RawHistoryCommitReceipt {
        revision: number(frame_value, "revision")?,
        committed_at_ms: number(frame_value, "committedAtMs")?,
        file_synced: boolean(frame_value, "fileSynced")?,
        directory_synced: boolean(frame_value, "directorySynced")?,
    };
    Ok(Retirement {
        id: string(value, "id")?,
        session_id: string(value, "sessionId")?,
        domain,
        authorization_revision: number(value, "authorizationRevision")?,
        frame: receipt,
        stage: RetirementStage::parse(&string(value, "stage")?)?,
    })
}

fn read_directory(path: &Path) -> Result<Directory, AppError> {
    let metadata = fs::metadata(path).map_err(AppError::from_storage)?;
    if metadata.len() > MAX_DIRECTORY_BYTES {
        return Err(AppError::from_storage(HistoryFailure(
            "session directory exceeds its format budget",
        )));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(AppError::from_storage)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(AppError::from_storage)?;
    let field = |object: &serde_json::Value, name: &str| {
        object.get(name).cloned().ok_or_else(|| {
            AppError::from_storage(HistoryFailure("session directory field is missing"))
        })
    };
    let version = field(&value, "version")?.as_u64().ok_or_else(|| {
        AppError::from_storage(HistoryFailure("session directory version is invalid"))
    })?;
    if version != 1 && version != DIRECTORY_VERSION {
        return Err(AppError::from_storage(HistoryFailure(
            "session directory version is unsupported",
        )));
    }
    let revision = field(&value, "revision")?.as_u64().ok_or_else(|| {
        AppError::from_storage(HistoryFailure("session directory revision is invalid"))
    })?;
    let active_session_id = field(&value, "activeSessionId")?
        .as_str()
        .ok_or_else(|| AppError::from_storage(HistoryFailure("active session ID is invalid")))?
        .to_owned();
    let next_segment_sequence = field(&value, "nextSegmentSequence")?
        .as_u64()
        .ok_or_else(|| AppError::from_storage(HistoryFailure("segment sequence is invalid")))?;
    let retiring_segments = match value.get("retiringSegments") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| {
                AppError::from_storage(HistoryFailure("retirement fence list is invalid"))
            })?
            .iter()
            .map(parse_retirement)
            .collect::<Result<Vec<_>, _>>()?,
    };
    let records_value = field(&value, "sessions")?;
    let records = records_value.as_array().ok_or_else(|| {
        AppError::from_storage(HistoryFailure("session directory list is invalid"))
    })?;
    let mut sessions = Vec::with_capacity(records.len());
    for record in records {
        let id = field(record, "id")?
            .as_str()
            .ok_or_else(|| AppError::from_storage(HistoryFailure("session ID is invalid")))?
            .to_owned();
        let label = field(record, "label")?
            .as_str()
            .ok_or_else(|| AppError::from_storage(HistoryFailure("session label is invalid")))?
            .to_owned();
        let raw_hint = field(record, "rawHint")?
            .as_str()
            .ok_or_else(|| AppError::from_storage(HistoryFailure("session raw hint is invalid")))?
            .to_owned();
        let archived = field(record, "archived")?
            .as_bool()
            .ok_or_else(|| AppError::from_storage(HistoryFailure("archive flag is invalid")))?;
        let status =
            parse_status(field(record, "status")?.as_str().ok_or_else(|| {
                AppError::from_storage(HistoryFailure("session status is invalid"))
            })?)?;
        let external_unknown = field(record, "externalUnknown")?.as_bool().ok_or_else(|| {
            AppError::from_storage(HistoryFailure("external unknown evidence is invalid"))
        })?;
        let recovered_unsettled = if version == DIRECTORY_VERSION {
            field(record, "recoveredUnsettled")?
                .as_bool()
                .ok_or_else(|| {
                    AppError::from_storage(HistoryFailure(
                        "recovered unresolved evidence is invalid",
                    ))
                })?
        } else {
            false
        };
        let active_segment = field(record, "activeSegment")?.as_str().map(str::to_owned);
        let segments_owned = field(record, "segments")?;
        let segments_value = segments_owned
            .as_array()
            .ok_or_else(|| AppError::from_storage(HistoryFailure("segment list is invalid")))?;
        let mut segments = Vec::with_capacity(segments_value.len());
        for item in segments_value {
            segments.push(Segment {
                id: field(item, "id")?
                    .as_str()
                    .ok_or_else(|| AppError::from_storage(HistoryFailure("segment ID is invalid")))?
                    .to_owned(),
                path: field(item, "path")?
                    .as_str()
                    .ok_or_else(|| {
                        AppError::from_storage(HistoryFailure("segment path is invalid"))
                    })?
                    .to_owned(),
                sealed: field(item, "sealed")?.as_bool().ok_or_else(|| {
                    AppError::from_storage(HistoryFailure("segment seal flag is invalid"))
                })?,
            });
        }
        sessions.push(Session {
            id,
            label,
            raw_hint,
            archived,
            status,
            external_unknown,
            recovered_unsettled,
            active_segment,
            segments,
        });
    }
    let directory = Directory {
        revision,
        active_session_id,
        next_segment_sequence,
        retiring_segments,
        sessions,
    };
    validate_directory(&directory)?;
    Ok(directory)
}

fn validate_directory(directory: &Directory) -> Result<(), AppError> {
    if directory.revision > MAX_SAFE_INTEGER
        || directory.sessions.is_empty()
        || directory.sessions.len() > MAX_SESSIONS
        || directory.next_segment_sequence == 0
        || !valid_id(&directory.active_session_id)
    {
        return Err(AppError::from_storage(HistoryFailure(
            "session directory limits or active entry are invalid",
        )));
    }
    let mut session_ids = std::collections::BTreeSet::new();
    let mut segment_ids = std::collections::BTreeSet::new();
    let mut total_segments = 0usize;
    let mut active_found = false;
    if directory.retiring_segments.len() > MAX_SEGMENTS {
        return Err(capacity_error(
            "raw segment retirement fence count exceeded",
        ));
    }
    for retirement in &directory.retiring_segments {
        let domain = &retirement.domain;
        if !valid_segment_id(&retirement.id)
            || retirement.id == "segment-legacy"
            || !valid_id(&retirement.session_id)
            || !valid_id(&domain.workspace_id)
            || !valid_id(&domain.user_id)
            || !valid_id(&domain.instance_id)
            || !valid_id(&domain.subject)
            || !valid_id(&domain.channel)
            || !valid_id(&domain.scope)
            || !valid_id(&domain.credential)
            || !valid_id(&domain.hint)
            || retirement.authorization_revision == 0
            || retirement.frame.revision == 0
            || !retirement.frame.file_synced
            || !segment_sequence(&retirement.id)
                .is_some_and(|sequence| sequence < directory.next_segment_sequence)
            || !segment_ids.insert(retirement.id.as_str())
        {
            return Err(AppError::from_storage(HistoryFailure(
                "session directory contains an invalid retirement fence",
            )));
        }
    }
    for session in &directory.sessions {
        if !valid_id(&session.id)
            || !valid_id(&session.raw_hint)
            || (session.external_unknown
                && !matches!(session.status, ChatStatus::Unknown | ChatStatus::Sending))
            || (session.recovered_unsettled && session.status != ChatStatus::Unknown)
            || session.label.is_empty()
            || session.label.len() > 128
            || session.label.chars().any(char::is_control)
            || !session_ids.insert(session.id.as_str())
        {
            return Err(AppError::from_storage(HistoryFailure(
                "session directory contains an invalid or duplicate session",
            )));
        }
        if directory.retiring_segments.iter().any(|retirement| {
            retirement.session_id == session.id && retirement.domain.hint != session.raw_hint
        }) {
            return Err(AppError::from_storage(HistoryFailure(
                "retirement fence is bound to another session hint",
            )));
        }
        active_found |= session.id == directory.active_session_id;
        if session.segments_len_exceeds_limit() {
            return Err(capacity_error("session segment count exceeded"));
        }
        total_segments += session.segments.len();
        if let Some(active) = &session.active_segment
            && !session
                .segments
                .iter()
                .any(|segment| segment.id == *active && !segment.sealed)
        {
            return Err(AppError::from_storage(HistoryFailure(
                "active raw segment reference is invalid",
            )));
        }
        for segment in &session.segments {
            if !valid_segment_id(&segment.id)
                || !valid_segment_path(&segment.id, &segment.path)
                || (segment.id != "segment-legacy"
                    && !segment_sequence(&segment.id)
                        .is_some_and(|sequence| sequence < directory.next_segment_sequence))
                || !segment_ids.insert(segment.id.as_str())
            {
                return Err(AppError::from_storage(HistoryFailure(
                    "session directory contains an invalid or duplicate segment",
                )));
            }
            if session.active_segment.as_deref() == Some(segment.id.as_str()) && segment.sealed {
                return Err(AppError::from_storage(HistoryFailure(
                    "active raw segment is marked sealed",
                )));
            }
        }
    }
    if directory
        .retiring_segments
        .iter()
        .any(|retirement| !session_ids.contains(retirement.session_id.as_str()))
    {
        return Err(AppError::from_storage(HistoryFailure(
            "retirement fence references a missing session",
        )));
    }
    if !active_found
        || total_segments
            .checked_add(directory.retiring_segments.len())
            .is_none_or(|count| count > MAX_SEGMENTS)
    {
        return Err(AppError::from_storage(HistoryFailure(
            "session directory references are inconsistent",
        )));
    }
    Ok(())
}

impl Session {
    fn segments_len_exceeds_limit(&self) -> bool {
        self.segments.len() > MAX_SEGMENTS
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_segment_id(value: &str) -> bool {
    value == "segment-legacy"
        || value.strip_prefix("segment-").is_some_and(|suffix| {
            suffix.len() == 8 && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn segment_sequence(value: &str) -> Option<u64> {
    value
        .strip_prefix("segment-")
        .and_then(|suffix| suffix.parse::<u64>().ok())
}

fn parse_retirement_gate_name(value: &str) -> Option<String> {
    let id = value.strip_prefix('.')?.strip_suffix(".raw-local-gate")?;
    (valid_segment_id(id) && id != "segment-legacy").then(|| id.to_owned())
}

fn validate_retirement_shape(
    retirement: &Retirement,
    raw: Option<RawRetirementState>,
    directory_exists: bool,
    frame_exists: bool,
    lock_exists: bool,
) -> Result<(), AppError> {
    if raw.is_some_and(|state| {
        state.revision != retirement.frame.revision
            || state.committed_at_ms != retirement.frame.committed_at_ms
    }) {
        return Err(AppError::from_storage(HistoryFailure(
            "raw retirement receipt does not match the directory fence",
        )));
    }
    let allowed = match (retirement.stage, raw.map(|state| state.stage)) {
        (RetirementStage::Marked, None | Some(RawStage::Retiring)) => {
            directory_exists && frame_exists && lock_exists
        }
        (RetirementStage::Marked, Some(RawStage::PayloadRemoved)) => {
            directory_exists && !frame_exists && lock_exists
        }
        (RetirementStage::PayloadRemoved, Some(RawStage::PayloadRemoved)) => {
            directory_exists && !frame_exists && lock_exists
        }
        (RetirementStage::PayloadRemoved, Some(RawStage::LockRemoved)) => {
            directory_exists && !frame_exists && !lock_exists
        }
        (RetirementStage::LockRemoved, Some(RawStage::LockRemoved)) => {
            directory_exists && !frame_exists && !lock_exists
        }
        (RetirementStage::LockRemoved, Some(RawStage::DirectoryRemoved))
        | (RetirementStage::DirectoryRemoved, Some(RawStage::DirectoryRemoved)) => {
            !directory_exists && !frame_exists && !lock_exists
        }
        _ => false,
    };
    if !allowed {
        return Err(AppError::from_storage(HistoryFailure(
            "retirement files do not match the last confirmed deletion stage",
        )));
    }
    Ok(())
}

fn valid_segment_path(id: &str, path: &str) -> bool {
    (id == "segment-legacy" && path == ".") || path == format!("{SEGMENTS_DIR}/{id}")
}

fn exists(path: &Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(std::io::Error::other("managed path is a symlink"))
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn sync_directory(path: &Path) -> std::io::Result<bool> {
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

fn capacity_error(message: &'static str) -> AppError {
    AppError::storage_capacity_exceeded(CapacityFailure(message))
}

fn stage_error(stage: &'static str, source: AppError) -> AppError {
    AppError::from_storage(HistoryStageFailure { stage, source })
}

#[derive(Debug)]
struct HistoryFailure(&'static str);

impl fmt::Display for HistoryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for HistoryFailure {}

#[derive(Debug)]
struct CapacityFailure(&'static str);

impl fmt::Display for CapacityFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for CapacityFailure {}

#[derive(Debug)]
struct HistoryStageFailure {
    stage: &'static str,
    source: AppError,
}

impl fmt::Display for HistoryStageFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "chat history {} failed", self.stage)
    }
}

impl Error for HistoryStageFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FailurePoint {
    Seal,
    AfterSeal,
    SegmentBeforeMarker,
    SegmentAfterMarker,
    RetireBeforeMarker,
    RetireAfterMarker,
    RetireAfterCurrentRemoval,
    RetireAfterLockRemoval,
    RetireAfterDirectoryRemoval,
    RetireBeforeFinalMarker,
    PublishCreate,
    PublishWrite,
    PublishSync,
    PublishRename,
    PublishDirectorySync,
}

#[cfg(test)]
std::thread_local! {
    static FAILURE_POINT: std::cell::Cell<Option<FailurePoint>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn inject_failure(point: FailurePoint) {
    FAILURE_POINT.with(|slot| slot.set(Some(point)));
}

#[cfg(test)]
pub(super) fn clear_failure() {
    FAILURE_POINT.with(|slot| slot.set(None));
}

#[cfg(test)]
fn injected_failure(point: FailurePoint) -> bool {
    FAILURE_POINT.with(|slot| slot.get() == Some(point))
}

#[cfg(test)]
mod buddy_budget_tests {
    use super::*;

    fn temporary_root() -> PathBuf {
        let mut seed = [0u8; 16];
        getrandom::fill(&mut seed).unwrap();
        std::env::temp_dir().join(format!("buddy-budget-{:032x}", u128::from_le_bytes(seed)))
    }

    fn directory(segment_count: u64) -> Directory {
        let segments = (1..=segment_count)
            .map(|sequence| {
                let id = format!("segment-{sequence:08}");
                Segment {
                    path: format!("{SEGMENTS_DIR}/{id}"),
                    sealed: sequence != segment_count,
                    id,
                }
            })
            .collect::<Vec<_>>();
        let active_segment = (segment_count > 0).then(|| format!("segment-{segment_count:08}"));
        Directory {
            revision: 1,
            active_session_id: String::from("session-1"),
            next_segment_sequence: segment_count + 1,
            retiring_segments: Vec::new(),
            sessions: vec![Session {
                id: String::from("session-1"),
                label: String::from("session 1"),
                raw_hint: String::from("session-1"),
                archived: false,
                status: ChatStatus::Ready,
                external_unknown: false,
                recovered_unsettled: false,
                active_segment,
                segments,
            }],
        }
    }

    #[test]
    fn sibling_buddy_roots_share_segment_and_byte_limits() {
        let root = temporary_root();
        let budget_root = root.join("chat");
        let legacy = budget_root.clone();
        let buddy_root = budget_root.join("instances/buddy-synthetic");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&buddy_root).unwrap();
        for path in [&legacy, &buddy_root] {
            let bytes = DiskHistory::encode_directory(&directory(128)).unwrap();
            fs::write(path.join(DIRECTORY_FILE), bytes).unwrap();
        }

        let history = DiskHistory::new_scoped(buddy_root.clone(), budget_root.clone());
        assert_eq!(history.root_usage().unwrap(), (2, MAX_SEGMENTS));
        assert!(matches!(
            history.next_segment(&directory(128)),
            Err(error) if error.public_code() == "storage_capacity_exceeded"
        ));

        let oversized = buddy_root.join("synthetic-over-budget.bin");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_ROOT_BYTES + 1)
            .unwrap();
        assert_eq!(
            history.preflight_peak(0).unwrap_err().public_code(),
            "storage_capacity_exceeded"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
