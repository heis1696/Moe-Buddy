//! LocalMemory is the authoritative journal; no alternate note snapshot format.
use moe_chat_app::{AppError, ChatMemory};
use moe_memory_core::*;
use moe_memory_local::{LocalMemory, OperationId, PersistenceError};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[path = "history.rs"]
mod history;
use history::DiskHistory;

const BUDDY_REGISTRY_FILE: &str = "buddies.v1";
const BUDDY_REGISTRY_PENDING: &str = "buddies.pending";
const BUDDY_CREATE_CANDIDATE: &str = "buddies.create-candidate";
const BUDDY_REGISTRY_LOCK: &str = ".buddies.lock";
const BUDDY_ONBOARDING_EVIDENCE: &str = "buddies.onboarded";
const BUDDY_ONBOARDING_EVIDENCE_BYTES: &[u8] = b"moe-buddy-buddies-onboarded-v1\n";
const MAX_BUDDY_REGISTRY_BYTES: u64 = 262_144;
const BUDDY_HISTORY_LOCK_FILE: &str = ".buddy-history.lock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuddyRegistryIoStage {
    CreateRoot,
    GenerateId,
    CreateOnboardingEvidence,
    WriteOnboardingEvidence,
    SyncOnboardingEvidence,
    OpenLock,
    AcquireLock,
    AcquireHistoryLock,
    Read,
    CreatePending,
    WritePending,
    SyncPending,
    CreateCandidate,
    WriteCandidate,
    SyncCandidate,
    SyncCandidateDirectory,
    RemoveCandidate,
    SyncCandidateCleanupDirectory,
    Publish,
    SyncDirectory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuddyPublicationState {
    NotPublished,
    Published,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuddyRegistryCommitReceipt {
    pub revision: u64,
    pub file_synced: bool,
    pub directory_synced: bool,
}

/// Candidate-marker synchronization completed before target preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuddyCandidateDurability {
    /// The marker file and its parent directory were synchronized.
    FileAndDirectorySynced,
    /// The marker file was synchronized; this platform has no supported directory sync.
    FileSyncedDirectorySyncUnavailable,
}

/// Successful buddy command result. Creation includes its earlier candidate-marker receipt;
/// registry, memory, and raw-history commits remain separate without a power-loss transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuddyCommitReceipt {
    pub registry: moe_chat_app::BuddyRegistrySnapshot,
    pub publication: BuddyRegistryCommitReceipt,
    /// Present only for creation; reports the candidate marker's sync result.
    pub candidate: Option<BuddyCandidateDurability>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuddyDraftRevision {
    pub buddy_id: String,
    pub revision: u64,
    pub has_unsubmitted_drafts: bool,
    pub sessions: Vec<BuddyDraftWatermark>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuddyDraftWatermark {
    pub session_id: String,
    pub revision: u64,
    pub discarded_revision: Option<u64>,
}

#[derive(Debug)]
pub enum BuddyStorageError {
    Io {
        stage: BuddyRegistryIoStage,
        source: std::io::Error,
    },
    PendingRegistry,
    PendingLegacySettings,
    InvalidOnboardingEvidence,
    RegistryMissingAfterOnboarding,
    InvalidArchive,
    MissingMemory,
    RevisionConflict,
    DraftRevisionConflict,
    BuddyNotFound,
    InactiveBuddy,
    Registry(moe_chat_app::BuddyError),
    ModelConfiguration(moe_agent_runtime::GatewayError),
    Application(AppError),
    Preparation(Box<BuddyStorageError>),
    /// A creation stage failed after candidate-marker synchronization completed.
    CandidateCommit {
        /// Candidate-marker durability already achieved before the failure.
        durability: BuddyCandidateDurability,
        /// The original preparation, publication, or cleanup failure.
        source: Box<BuddyStorageError>,
    },
    RegistryCommit {
        disposition: BuddyPublicationState,
        source: Box<BuddyStorageError>,
    },
    ApplicationsFull,
    GenerationExhausted,
}

impl BuddyStorageError {
    fn io(stage: BuddyRegistryIoStage, source: std::io::Error) -> Self {
        Self::Io { stage, source }
    }
}

impl std::fmt::Display for BuddyStorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Io { stage, .. } => match stage {
                BuddyRegistryIoStage::CreateRoot => "伙伴档案目录无法创建",
                BuddyRegistryIoStage::GenerateId => "无法签发伙伴实例 ID",
                BuddyRegistryIoStage::CreateOnboardingEvidence => "伙伴首次接入证据无法创建",
                BuddyRegistryIoStage::WriteOnboardingEvidence => {
                    "伙伴首次接入证据写入失败，现场已保留"
                }
                BuddyRegistryIoStage::SyncOnboardingEvidence => {
                    "伙伴首次接入证据同步失败，现场已保留"
                }
                BuddyRegistryIoStage::OpenLock => "伙伴档案锁无法打开",
                BuddyRegistryIoStage::AcquireLock => "伙伴档案当前被占用",
                BuddyRegistryIoStage::AcquireHistoryLock => "伙伴原文根当前被占用",
                BuddyRegistryIoStage::Read => "伙伴档案无法读取",
                BuddyRegistryIoStage::CreatePending => "伙伴档案待发布文件无法创建",
                BuddyRegistryIoStage::WritePending => "伙伴档案待发布文件写入失败，现场已保留",
                BuddyRegistryIoStage::SyncPending => "伙伴档案待发布文件同步失败，现场已保留",
                BuddyRegistryIoStage::CreateCandidate => "伙伴候选身份标记无法创建",
                BuddyRegistryIoStage::WriteCandidate => "伙伴候选身份标记写入失败，现场已保留",
                BuddyRegistryIoStage::SyncCandidate => "伙伴候选身份标记同步失败，现场已保留",
                BuddyRegistryIoStage::SyncCandidateDirectory => {
                    "伙伴候选身份目录同步失败，现场已保留"
                }
                BuddyRegistryIoStage::RemoveCandidate => "伙伴档案已发布，但候选身份标记无法清理",
                BuddyRegistryIoStage::SyncCandidateCleanupDirectory => {
                    "伙伴档案已发布，但候选身份清理后的目录同步失败"
                }
                BuddyRegistryIoStage::Publish => "伙伴档案未完成发布，现场已保留",
                BuddyRegistryIoStage::SyncDirectory => "伙伴档案已发布，但目录同步失败",
            },
            Self::PendingRegistry => "检测到未完成的伙伴档案发布，已保留现场",
            Self::PendingLegacySettings => "检测到未完成的旧模型设置保存，已保留现场",
            Self::InvalidOnboardingEvidence => "伙伴首次接入证据损坏，原文件已保留",
            Self::RegistryMissingAfterOnboarding => {
                "伙伴档案曾接入或已有实例数据，但注册表缺失；拒绝重建"
            }
            Self::InvalidArchive => "伙伴档案损坏或版本不受支持，原文件已保留",
            Self::MissingMemory => "伙伴私密记忆存档缺失或尚未初始化，未创建空档案",
            Self::RevisionConflict => "伙伴选择或档案版本已变化，本次操作未执行",
            Self::DraftRevisionConflict => "伙伴会话草稿版本已变化，请重新确认退出",
            Self::BuddyNotFound => "伙伴档案不存在，本次操作未执行",
            Self::InactiveBuddy => "伙伴选择已变化，本次操作未执行",
            Self::Registry(error) => return write!(formatter, "伙伴档案操作拒绝：{error}"),
            Self::ModelConfiguration(_) => "模型配置无效，本次档案提交未执行",
            Self::Application(error) => return formatter.write_str(error.public_message()),
            Self::Preparation(source) => {
                return write!(formatter, "目标伙伴准备未完成：{source}");
            }
            Self::CandidateCommit { durability, source } => {
                return write!(
                    formatter,
                    "候选同步结果 {durability:?}；后续创建阶段失败：{source}"
                );
            }
            Self::RegistryCommit {
                disposition,
                source,
            } => return write!(formatter, "伙伴档案提交状态 {:?}：{source}", disposition),
            Self::ApplicationsFull => "伙伴应用缓存容量已满",
            Self::GenerationExhausted => "伙伴应用代际已耗尽",
        })
    }
}

impl std::error::Error for BuddyStorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Registry(error) => Some(error),
            Self::ModelConfiguration(error) => Some(error),
            Self::Application(error) => Some(error),
            Self::Preparation(source) => Some(source),
            Self::CandidateCommit { source, .. } => Some(source),
            Self::RegistryCommit { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl BuddyStorageError {
    /// Returns the registry publication state, including through a candidate error wrapper.
    pub fn publication_state(&self) -> Option<BuddyPublicationState> {
        match self {
            Self::RegistryCommit { disposition, .. } => Some(*disposition),
            Self::CandidateCommit { source, .. } => source.publication_state(),
            Self::Io { stage, .. } => match stage {
                BuddyRegistryIoStage::CreatePending
                | BuddyRegistryIoStage::WritePending
                | BuddyRegistryIoStage::SyncPending => Some(BuddyPublicationState::NotPublished),
                BuddyRegistryIoStage::Publish => Some(BuddyPublicationState::Unknown),
                BuddyRegistryIoStage::SyncDirectory => Some(BuddyPublicationState::Published),
                _ => None,
            },
            _ => None,
        }
    }

    /// Returns the completed candidate-marker sync result for a later creation failure.
    pub fn candidate_durability(&self) -> Option<BuddyCandidateDurability> {
        match self {
            Self::CandidateCommit { durability, .. } => Some(*durability),
            _ => None,
        }
    }
}

impl From<moe_chat_app::BuddyError> for BuddyStorageError {
    fn from(error: moe_chat_app::BuddyError) -> Self {
        Self::Registry(error)
    }
}

/// Return the current bounded buddy summary, creating the known legacy profile
/// on first access while preserving its old memory and raw-history roots.
pub fn load_buddy_registry_at(
    root: &std::path::Path,
) -> Result<moe_chat_app::BuddyRegistrySnapshot, BuddyStorageError> {
    with_buddy_registry(root, |_| Ok(()))
}

/// Load a trusted profile's persona and non-secret model binding for host UI.
pub fn load_buddy_profile_at(
    root: &std::path::Path,
    id: &str,
) -> Result<moe_chat_app::BuddyProfile, BuddyStorageError> {
    let id = moe_chat_app::BuddyId::new(id).map_err(BuddyStorageError::Registry)?;
    with_buddy_profile(root, |registry| {
        registry
            .profile(&id)
            .cloned()
            .ok_or(BuddyStorageError::BuddyNotFound)
    })
}

/// Create a fresh identity and select it in one expected-revision commit.
pub fn create_buddy_at(
    root: &std::path::Path,
    expected_buddies_revision: u64,
    label: &str,
    persona: &str,
) -> Result<moe_chat_app::BuddyRegistrySnapshot, BuddyStorageError> {
    let mut seed = [0u8; 16];
    getrandom::fill(&mut seed).map_err(|error| {
        BuddyStorageError::io(
            BuddyRegistryIoStage::GenerateId,
            std::io::Error::other(error),
        )
    })?;
    let id = moe_chat_app::BuddyId::new(format!("buddy-{:032x}", u128::from_le_bytes(seed)))
        .map_err(BuddyStorageError::Registry)?;
    with_buddy_registry(root, |registry| {
        registry
            .create(expected_buddies_revision, id, label, persona)
            .map_err(BuddyStorageError::Registry)?;
        Ok(())
    })
}

/// Select an existing profile with an optimistic registry revision check.
pub fn switch_buddy_at(
    root: &std::path::Path,
    expected_buddies_revision: u64,
    id: &str,
) -> Result<moe_chat_app::BuddyRegistrySnapshot, BuddyStorageError> {
    let id = moe_chat_app::BuddyId::new(id).map_err(BuddyStorageError::Registry)?;
    with_buddy_registry(root, |registry| {
        registry
            .switch(expected_buddies_revision, &id)
            .map_err(BuddyStorageError::Registry)
    })
}

/// Rename the selected profile without changing its stable identity.
pub fn rename_buddy_at(
    root: &std::path::Path,
    expected_buddy_id: &str,
    expected_buddies_revision: u64,
    expected_profile_revision: u64,
    label: &str,
) -> Result<moe_chat_app::BuddyRegistrySnapshot, BuddyStorageError> {
    let id = moe_chat_app::BuddyId::new(expected_buddy_id).map_err(BuddyStorageError::Registry)?;
    with_buddy_registry(root, |registry| {
        if registry.revision() != expected_buddies_revision {
            return Err(BuddyStorageError::RevisionConflict);
        }
        ensure_active(registry, &id)?;
        registry
            .rename(
                expected_buddies_revision,
                &id,
                expected_profile_revision,
                label,
            )
            .map_err(BuddyStorageError::Registry)
    })
}

/// Atomically submit persona and non-secret model configuration and versions.
pub fn commit_buddy_configuration_at(
    root: &std::path::Path,
    expected_buddy_id: &str,
    expected_buddies_revision: u64,
    expected_profile_revision: u64,
    persona: &str,
    model: moe_chat_app::BuddyModelConfiguration,
) -> Result<moe_chat_app::BuddyRegistrySnapshot, BuddyStorageError> {
    let id = moe_chat_app::BuddyId::new(expected_buddy_id).map_err(BuddyStorageError::Registry)?;
    with_buddy_registry(root, |registry| {
        if registry.revision() != expected_buddies_revision {
            return Err(BuddyStorageError::RevisionConflict);
        }
        ensure_active(registry, &id)?;
        let profile = registry
            .profile(&id)
            .ok_or(BuddyStorageError::BuddyNotFound)?;
        if profile.profile_revision() != expected_profile_revision {
            return Err(BuddyStorageError::Registry(
                moe_chat_app::BuddyError::StaleProfile,
            ));
        }
        validate_model_configuration(&model)?;
        registry
            .commit_configuration(
                expected_buddies_revision,
                &id,
                expected_profile_revision,
                persona,
                model.clone(),
            )
            .map_err(BuddyStorageError::Registry)
    })
}

/// Restore the selected profile at the captured registry revision.
///
/// Memory remains in the existing shared LocalMemory journal and is scoped by
/// PetIdentity. Raw histories use sibling directories under one globally
/// locked and budgeted `chat/` root.
pub fn open_application_for_buddy_at(
    root: &std::path::Path,
    expected_buddy_id: &str,
    expected_buddies_revision: u64,
    generation: u64,
    now: u64,
) -> Result<moe_chat_app::ChatApplication, BuddyStorageError> {
    let id = moe_chat_app::BuddyId::new(expected_buddy_id).map_err(BuddyStorageError::Registry)?;
    with_buddy_profile(root, |registry| {
        if registry.revision() != expected_buddies_revision {
            return Err(BuddyStorageError::RevisionConflict);
        }
        ensure_active(registry, &id)?;
        let profile = registry
            .profile(&id)
            .cloned()
            .ok_or(BuddyStorageError::BuddyNotFound)?;
        open_profile_at(root, generation, now, &profile)
    })
}

/// Host-owned storage boundary for all buddy applications attached to one archive root.
/// It holds the registry and raw-history root locks for its lifetime and shares one
/// LocalMemory handle across the bounded application cache.
pub struct BuddyStorage {
    root: PathBuf,
    registry: moe_chat_app::BuddyRegistry,
    active_buddy_id: String,
    apps: BTreeMap<String, moe_chat_app::ChatApplication>,
    memory: Rc<RefCell<DiskMemory>>,
    _registry_lock: std::fs::File,
    _history_root_lock: std::fs::File,
    next_generation: u64,
    generation_exhausted: bool,
    registry_unknown: bool,
    #[cfg(test)]
    registry_fault: Option<BuddyRegistryIoStage>,
    #[cfg(test)]
    candidate_fault: Option<BuddyRegistryIoStage>,
    #[cfg(test)]
    candidate_cleanup_fault: Option<BuddyRegistryIoStage>,
}

impl BuddyStorage {
    /// Open one shared host storage owner and restore its selected buddy.
    pub fn open(root: &Path, process_generation: u64, now: u64) -> Result<Self, BuddyStorageError> {
        if process_generation == 0 {
            return Err(BuddyStorageError::GenerationExhausted);
        }
        std::fs::create_dir_all(root)
            .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::CreateRoot, error))?;
        let registry_lock = acquire_buddy_registry_lock(root)?;
        let registry = load_or_initialize_registry_locked(root)?;
        let chat_root = root.join("chat");
        std::fs::create_dir_all(&chat_root)
            .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::CreateRoot, error))?;
        let history_root_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(chat_root.join(BUDDY_HISTORY_LOCK_FILE))
            .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::OpenLock, error))?;
        history_root_lock.try_lock().map_err(|error| {
            BuddyStorageError::io(BuddyRegistryIoStage::AcquireHistoryLock, error.into())
        })?;
        let memory_path = root.join("memory.v1");
        let local_memory = LocalMemory::open(&memory_path)
            .map_err(AppError::from_storage)
            .map_err(BuddyStorageError::Application)?;
        let active_buddy_id = registry.active_buddy_id().as_str().to_owned();
        let profile = registry
            .profile(registry.active_buddy_id())
            .cloned()
            .ok_or(BuddyStorageError::BuddyNotFound)?;
        let mut storage = Self {
            root: root.to_path_buf(),
            registry,
            active_buddy_id: active_buddy_id.clone(),
            apps: BTreeMap::new(),
            memory: Rc::new(RefCell::new(DiskMemory(local_memory))),
            _registry_lock: registry_lock,
            _history_root_lock: history_root_lock,
            next_generation: process_generation,
            generation_exhausted: false,
            registry_unknown: false,
            #[cfg(test)]
            registry_fault: None,
            #[cfg(test)]
            candidate_fault: None,
            #[cfg(test)]
            candidate_cleanup_fault: None,
        };
        let app = open_shared_profile_at(
            &storage.root,
            process_generation,
            now,
            &profile,
            storage.memory.clone(),
        )?;
        storage.apps.insert(active_buddy_id, app);
        storage.next_generation = process_generation.saturating_add(1);
        if storage.next_generation == process_generation {
            storage.generation_exhausted = true;
        }
        Ok(storage)
    }

    pub fn registry_snapshot(
        &self,
    ) -> Result<moe_chat_app::BuddyRegistrySnapshot, BuddyStorageError> {
        self.ensure_registry_known()?;
        Ok(self.registry.snapshot())
    }

    pub fn active_buddy_id(&self) -> Result<&str, BuddyStorageError> {
        self.ensure_registry_known()?;
        Ok(&self.active_buddy_id)
    }

    pub fn registry_is_unknown(&self) -> bool {
        self.registry_unknown
    }

    /// Inspect one already-loaded instance without granting ordinary business access.
    pub fn inspect_known_buddy(
        &self,
        buddy_id: &str,
    ) -> Result<moe_chat_app::AppSnapshot, BuddyStorageError> {
        self.apps
            .get(buddy_id)
            .map(moe_chat_app::ChatApplication::snapshot)
            .ok_or(BuddyStorageError::BuddyNotFound)
    }

    pub fn buddy_profile(
        &self,
        buddy_id: &str,
    ) -> Result<moe_chat_app::BuddyProfile, BuddyStorageError> {
        self.ensure_registry_known()?;
        let id = moe_chat_app::BuddyId::new(buddy_id).map_err(BuddyStorageError::Registry)?;
        self.registry
            .profile(&id)
            .cloned()
            .ok_or(BuddyStorageError::BuddyNotFound)
    }

    /// Return the active application without releasing or rebuilding it.
    pub fn active_application_mut(
        &mut self,
    ) -> Result<&mut moe_chat_app::ChatApplication, BuddyStorageError> {
        self.ensure_registry_known()?;
        self.apps
            .get_mut(&self.active_buddy_id)
            .ok_or(BuddyStorageError::BuddyNotFound)
    }

    /// Hide an already loaded management panel while registry publication is
    /// unknown. This presentation-only recovery path cannot touch chat or data.
    pub fn hide_management_panel_during_unknown(
        &mut self,
        expected_buddy_id: &str,
        expected_buddies_revision: u64,
        panel: moe_chat_app::PanelKind,
        now_ms: u64,
    ) -> Result<(), String> {
        if !self.registry_is_unknown()
            || expected_buddy_id != self.active_buddy_id
            || expected_buddies_revision != self.registry.revision()
            || panel == moe_chat_app::PanelKind::Chat
        {
            return Err("restricted management close context rejected".into());
        }
        self.apps
            .get_mut(expected_buddy_id)
            .ok_or_else(|| "known active buddy application is unavailable".to_owned())?
            .dispatch(moe_chat_app::Action::Hide(panel), now_ms)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn active_application(&self) -> Result<&moe_chat_app::ChatApplication, BuddyStorageError> {
        self.ensure_registry_known()?;
        self.apps
            .get(&self.active_buddy_id)
            .ok_or(BuddyStorageError::BuddyNotFound)
    }

    pub fn draft_revision_set(&self) -> Result<Vec<BuddyDraftRevision>, BuddyStorageError> {
        Ok(self
            .apps
            .iter()
            .map(|(buddy_id, app)| {
                let draft = app.chat_draft_snapshot();
                BuddyDraftRevision {
                    buddy_id: buddy_id.clone(),
                    revision: draft.revision,
                    has_unsubmitted_drafts: app.has_unsubmitted_chat_drafts(),
                    sessions: draft
                        .sessions
                        .into_iter()
                        .map(|session| BuddyDraftWatermark {
                            session_id: session.id,
                            revision: session.revision,
                            discarded_revision: session.discarded_revision,
                        })
                        .collect(),
                }
            })
            .collect())
    }

    pub fn preflight_quit_draft_discard(
        &self,
        expected: &[BuddyDraftRevision],
        active_close_discards_active: bool,
    ) -> Result<(), BuddyStorageError> {
        if self.draft_revision_set()? != expected {
            return Err(BuddyStorageError::DraftRevisionConflict);
        }
        for (buddy_id, app) in &self.apps {
            app.preflight_quit_draft_discard(
                active_close_discards_active && buddy_id == &self.active_buddy_id,
            )
            .map_err(BuddyStorageError::Application)?;
        }
        Ok(())
    }

    pub fn discard_all_chat_drafts(
        &mut self,
        expected: &[BuddyDraftRevision],
    ) -> Result<(), BuddyStorageError> {
        if self.draft_revision_set()? != expected {
            return Err(BuddyStorageError::DraftRevisionConflict);
        }
        for (buddy_id, app) in &self.apps {
            app.preflight_quit_draft_discard(false)
                .map_err(BuddyStorageError::Application)?;
            let _ = buddy_id;
        }
        for (buddy_id, app) in &mut self.apps {
            let expected_revision = expected
                .iter()
                .find(|entry| entry.buddy_id == *buddy_id)
                .ok_or(BuddyStorageError::BuddyNotFound)?
                .revision;
            app.discard_all_chat_drafts(expected_revision)
                .map_err(BuddyStorageError::Application)?;
        }
        Ok(())
    }

    /// Access the instance captured by a worker request, even after another buddy is selected.
    pub fn application_mut(
        &mut self,
        buddy_id: &str,
    ) -> Result<&mut moe_chat_app::ChatApplication, BuddyStorageError> {
        self.ensure_registry_known()?;
        self.apps
            .get_mut(buddy_id)
            .ok_or(BuddyStorageError::BuddyNotFound)
    }

    /// Complete only the private request ticket against its captured, already-loaded owner.
    /// This narrow recovery path remains available while registry publication is unknown.
    pub fn complete_external_for_buddy(
        &mut self,
        buddy_id: &str,
        ticket: moe_chat_app::ExternalRequestTicket,
        outcome: moe_agent_runtime::ModelOutcome,
        now: u64,
    ) -> Result<(), BuddyStorageError> {
        let app = self
            .apps
            .get_mut(buddy_id)
            .ok_or(BuddyStorageError::BuddyNotFound)?;
        app.complete_external(ticket, outcome, now)
            .map_err(BuddyStorageError::Application)
    }

    /// Revoke permissions for one already-loaded, known instance during registry uncertainty.
    pub fn revoke_known_buddy(
        &mut self,
        buddy_id: &str,
        now: u64,
    ) -> Result<(), BuddyStorageError> {
        let app = self
            .apps
            .get_mut(buddy_id)
            .ok_or(BuddyStorageError::BuddyNotFound)?;
        app.dispatch(moe_chat_app::Action::Revoke, now)
            .map_err(BuddyStorageError::Application)?;
        Ok(())
    }

    /// Open and retain a target before changing the persisted active selection.
    pub fn prepare_buddy(
        &mut self,
        buddy_id: &str,
        expected_buddies_revision: u64,
        now: u64,
    ) -> Result<(), BuddyStorageError> {
        self.ensure_registry_known()?;
        if expected_buddies_revision != self.registry.revision() {
            return Err(BuddyStorageError::RevisionConflict);
        }
        let id = moe_chat_app::BuddyId::new(buddy_id).map_err(BuddyStorageError::Registry)?;
        if self.apps.contains_key(buddy_id) {
            let app = self
                .apps
                .get_mut(buddy_id)
                .ok_or(BuddyStorageError::BuddyNotFound)?;
            let chat_visible = app.snapshot().panels.iter().any(|panel| {
                panel.kind == moe_chat_app::PanelKind::Chat
                    && panel.visibility == moe_chat_app::PanelVisibility::Visible
            });
            if !chat_visible {
                app.dispatch(
                    moe_chat_app::Action::Open(moe_chat_app::PanelKind::Chat),
                    now,
                )
                .map_err(|error| {
                    BuddyStorageError::Preparation(Box::new(BuddyStorageError::Application(error)))
                })?;
            }
            return Ok(());
        }
        if self.apps.len() >= moe_chat_app::MAX_BUDDIES {
            return Err(BuddyStorageError::ApplicationsFull);
        }
        let profile = self
            .registry
            .profile(&id)
            .cloned()
            .ok_or(BuddyStorageError::BuddyNotFound)?;
        let generation = self.allocate_generation()?;
        let mut app =
            open_shared_profile_at(&self.root, generation, now, &profile, self.memory.clone())
                .map_err(|error| BuddyStorageError::Preparation(Box::new(error)))?;
        app.dispatch(
            moe_chat_app::Action::Open(moe_chat_app::PanelKind::Chat),
            now,
        )
        .map_err(|error| {
            BuddyStorageError::Preparation(Box::new(BuddyStorageError::Application(error)))
        })?;
        self.apps.insert(buddy_id.to_owned(), app);
        Ok(())
    }

    /// Create a new profile and select it after its isolated application is prepared.
    pub fn create_buddy(
        &mut self,
        expected_buddies_revision: u64,
        label: &str,
        persona: &str,
        now: u64,
    ) -> Result<BuddyCommitReceipt, BuddyStorageError> {
        self.ensure_registry_known()?;
        ensure_no_create_candidate(&self.root)?;
        if self.registry.revision() != expected_buddies_revision {
            return Err(not_published(BuddyStorageError::RevisionConflict));
        }
        self.active_application_mut()?
            .preflight_buddy_switch()
            .map_err(|error| not_published(BuddyStorageError::Application(error)))?;
        if self.registry.buddies().len() >= moe_chat_app::MAX_BUDDIES {
            return Err(BuddyStorageError::Registry(
                moe_chat_app::BuddyError::CapacityExceeded,
            ));
        }
        let id = generate_buddy_id()?;
        let mut candidate = self.registry.clone();
        candidate
            .create(expected_buddies_revision, id.clone(), label, persona)
            .map_err(|error| not_published(BuddyStorageError::Registry(error)))?;
        let profile = candidate.profile(&id).unwrap().clone();
        self.preflight_generation().map_err(not_published)?;
        #[cfg(test)]
        let candidate_durability = persist_create_candidate_with_fault(
            &self.root,
            id.as_str(),
            self.candidate_fault.take(),
        )?;
        #[cfg(not(test))]
        let candidate_durability = persist_create_candidate(&self.root, id.as_str())?;
        let generation = self
            .allocate_generation()
            .map_err(|error| candidate_commit(candidate_durability, error))?;
        let mut app =
            open_shared_profile_at(&self.root, generation, now, &profile, self.memory.clone())
                .map_err(|error| {
                    candidate_commit(
                        candidate_durability,
                        BuddyStorageError::Preparation(Box::new(error)),
                    )
                })?;
        app.dispatch(
            moe_chat_app::Action::Open(moe_chat_app::PanelKind::Chat),
            now,
        )
        .map_err(|error| {
            candidate_commit(
                candidate_durability,
                BuddyStorageError::Preparation(Box::new(BuddyStorageError::Application(error))),
            )
        })?;
        self.finish_prepared_commit(candidate, id.as_str().to_owned(), app, candidate_durability)
    }

    /// Change selection only after the destination app is available and recovered.
    pub fn switch_buddy(
        &mut self,
        buddy_id: &str,
        expected_buddies_revision: u64,
        now: u64,
    ) -> Result<BuddyCommitReceipt, BuddyStorageError> {
        self.active_application_mut()?
            .preflight_buddy_switch()
            .map_err(|error| not_published(BuddyStorageError::Application(error)))?;
        self.prepare_buddy(buddy_id, expected_buddies_revision, now)
            .map_err(not_published)?;
        let id = moe_chat_app::BuddyId::new(buddy_id).map_err(BuddyStorageError::Registry)?;
        let mut candidate = self.registry.clone();
        candidate
            .switch(expected_buddies_revision, &id)
            .map_err(|error| not_published(BuddyStorageError::Registry(error)))?;
        match self.publish_candidate(candidate) {
            Ok(publication) => {
                self.active_buddy_id = buddy_id.to_owned();
                Ok(BuddyCommitReceipt {
                    registry: self.registry.snapshot(),
                    publication,
                    candidate: None,
                })
            }
            Err(error) => {
                if error.publication_state() == Some(BuddyPublicationState::Published) {
                    self.active_buddy_id = buddy_id.to_owned();
                }
                Err(error)
            }
        }
    }

    /// Persist a label change against exact registry and profile revisions.
    pub fn rename_buddy(
        &mut self,
        buddy_id: &str,
        expected_buddies_revision: u64,
        expected_profile_revision: u64,
        label: &str,
    ) -> Result<BuddyCommitReceipt, BuddyStorageError> {
        self.ensure_registry_known()?;
        let id = moe_chat_app::BuddyId::new(buddy_id).map_err(BuddyStorageError::Registry)?;
        ensure_active(&self.registry, &id)?;
        let mut candidate = self.registry.clone();
        candidate
            .rename(
                expected_buddies_revision,
                &id,
                expected_profile_revision,
                label,
            )
            .map_err(BuddyStorageError::Registry)?;
        let publication = self.publish_candidate(candidate)?;
        Ok(BuddyCommitReceipt {
            registry: self.registry.snapshot(),
            publication,
            candidate: None,
        })
    }

    /// Commit a profile/model binding, then apply exactly that published version to its app.
    pub fn commit_configuration(
        &mut self,
        buddy_id: &str,
        expected_buddies_revision: u64,
        expected_profile_revision: u64,
        persona: &str,
        model: moe_chat_app::BuddyModelConfiguration,
        now: u64,
    ) -> Result<BuddyCommitReceipt, BuddyStorageError> {
        let settings = model.to_model_settings(persona);
        self.commit_configuration_with_runtime(
            buddy_id,
            expected_buddies_revision,
            expected_profile_revision,
            model,
            settings,
            now,
        )
    }

    /// Persist the non-secret profile and apply the user's explicit runtime consent
    /// in the same profile-version commit. Credentials remain owned by the host.
    pub fn commit_configuration_with_runtime(
        &mut self,
        buddy_id: &str,
        expected_buddies_revision: u64,
        expected_profile_revision: u64,
        model: moe_chat_app::BuddyModelConfiguration,
        runtime_settings: moe_chat_app::ModelSettings,
        now: u64,
    ) -> Result<BuddyCommitReceipt, BuddyStorageError> {
        self.ensure_registry_known()?;
        if self.active_buddy_id != buddy_id {
            return Err(not_published(BuddyStorageError::InactiveBuddy));
        }
        if runtime_settings.endpoint != model.endpoint() || runtime_settings.model != model.model()
        {
            return Err(not_published(BuddyStorageError::Registry(
                moe_chat_app::BuddyError::InvalidConfiguration,
            )));
        }
        let id = moe_chat_app::BuddyId::new(buddy_id).map_err(BuddyStorageError::Registry)?;
        let mut candidate = self.registry.clone();
        candidate
            .commit_configuration(
                expected_buddies_revision,
                &id,
                expected_profile_revision,
                &runtime_settings.persona,
                model.clone(),
            )
            .map_err(|error| not_published(BuddyStorageError::Registry(error)))?;
        validate_model_configuration(&model).map_err(not_published)?;
        self.prepare_buddy(buddy_id, expected_buddies_revision, now)
            .map_err(not_published)?;
        let profile = candidate.profile(&id).unwrap();
        self.apps
            .get(buddy_id)
            .ok_or(BuddyStorageError::BuddyNotFound)?
            .preflight_model_binding(
                &runtime_settings,
                profile.model_revision(),
                profile.persona_revision(),
                model.preset_id(),
            )
            .map_err(|error| not_published(BuddyStorageError::Application(error)))?;
        let publication = match self.publish_candidate(candidate) {
            Ok(publication) => publication,
            Err(error) if error.publication_state() == Some(BuddyPublicationState::Published) => {
                self.apps
                    .get_mut(buddy_id)
                    .expect("prepared application remains cached")
                    .mark_buddy_configuration_unknown();
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let profile = self.registry.profile(&id).unwrap();
        let result = self
            .apps
            .get_mut(buddy_id)
            .expect("prepared application remains cached")
            .configure_model_binding(
                &runtime_settings,
                profile.model_revision(),
                profile.persona_revision(),
                model.preset_id(),
            );
        if let Err(error) = result {
            self.apps
                .get_mut(buddy_id)
                .expect("prepared application remains cached")
                .mark_buddy_configuration_unknown();
            return Err(BuddyStorageError::RegistryCommit {
                disposition: BuddyPublicationState::Published,
                source: Box::new(BuddyStorageError::Application(error)),
            });
        }
        Ok(BuddyCommitReceipt {
            registry: self.registry.snapshot(),
            publication,
            candidate: None,
        })
    }

    fn finish_prepared_commit(
        &mut self,
        candidate: moe_chat_app::BuddyRegistry,
        buddy_id: String,
        app: moe_chat_app::ChatApplication,
        candidate_durability: BuddyCandidateDurability,
    ) -> Result<BuddyCommitReceipt, BuddyStorageError> {
        let publication = match self.publish_candidate(candidate) {
            Ok(publication) => publication,
            Err(error) => {
                if error.publication_state() == Some(BuddyPublicationState::Published) {
                    self.apps.insert(buddy_id.clone(), app);
                    self.active_buddy_id = buddy_id;
                }
                return Err(candidate_commit(candidate_durability, error));
            }
        };
        self.apps.insert(buddy_id.clone(), app);
        self.active_buddy_id = buddy_id;
        #[cfg(test)]
        let cleanup =
            clear_create_candidate_with_fault(&self.root, self.candidate_cleanup_fault.take());
        #[cfg(not(test))]
        let cleanup = clear_create_candidate(&self.root);
        cleanup.map_err(|source| {
            candidate_commit(
                candidate_durability,
                BuddyStorageError::RegistryCommit {
                    disposition: BuddyPublicationState::Published,
                    source: Box::new(source),
                },
            )
        })?;
        Ok(BuddyCommitReceipt {
            registry: self.registry.snapshot(),
            publication,
            candidate: Some(candidate_durability),
        })
    }

    fn publish_candidate(
        &mut self,
        candidate: moe_chat_app::BuddyRegistry,
    ) -> Result<BuddyRegistryCommitReceipt, BuddyStorageError> {
        self.ensure_registry_known()?;
        #[cfg(test)]
        let publication =
            write_buddy_registry_with_fault(&self.root, &candidate, self.registry_fault.take());
        #[cfg(not(test))]
        let publication = write_buddy_registry(&self.root, &candidate);
        match publication {
            Ok(receipt) => {
                self.registry = candidate;
                Ok(receipt)
            }
            Err(error) => {
                let disposition = error
                    .publication_state()
                    .unwrap_or(BuddyPublicationState::NotPublished);
                if disposition == BuddyPublicationState::Published {
                    self.registry = candidate;
                } else if disposition == BuddyPublicationState::Unknown {
                    self.registry_unknown = true;
                }
                Err(BuddyStorageError::RegistryCommit {
                    disposition,
                    source: Box::new(error),
                })
            }
        }
    }

    fn allocate_generation(&mut self) -> Result<u64, BuddyStorageError> {
        if self.generation_exhausted || self.next_generation == 0 {
            return Err(BuddyStorageError::GenerationExhausted);
        }
        let generation = self.next_generation;
        match generation.checked_add(1) {
            Some(next) if next != 0 => self.next_generation = next,
            _ => self.generation_exhausted = true,
        }
        Ok(generation)
    }

    fn preflight_generation(&self) -> Result<(), BuddyStorageError> {
        if self.generation_exhausted || self.next_generation == 0 {
            Err(BuddyStorageError::GenerationExhausted)
        } else {
            Ok(())
        }
    }

    fn ensure_registry_known(&self) -> Result<(), BuddyStorageError> {
        if self.registry_unknown {
            Err(BuddyStorageError::RegistryCommit {
                disposition: BuddyPublicationState::Unknown,
                source: Box::new(BuddyStorageError::InvalidArchive),
            })
        } else {
            Ok(())
        }
    }
}

impl Drop for BuddyStorage {
    fn drop(&mut self) {
        self.apps.clear();
    }
}

fn generate_buddy_id() -> Result<moe_chat_app::BuddyId, BuddyStorageError> {
    let mut seed = [0u8; 16];
    getrandom::fill(&mut seed).map_err(|error| {
        BuddyStorageError::io(
            BuddyRegistryIoStage::GenerateId,
            std::io::Error::other(error),
        )
    })?;
    moe_chat_app::BuddyId::new(format!("buddy-{:032x}", u128::from_le_bytes(seed)))
        .map_err(BuddyStorageError::Registry)
}

fn ensure_no_create_candidate(root: &Path) -> Result<(), BuddyStorageError> {
    if root
        .join(BUDDY_REGISTRY_PENDING)
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
        || root
            .join(BUDDY_CREATE_CANDIDATE)
            .try_exists()
            .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        Err(BuddyStorageError::PendingRegistry)
    } else {
        Ok(())
    }
}

fn persist_create_candidate(
    root: &Path,
    buddy_id: &str,
) -> Result<BuddyCandidateDurability, BuddyStorageError> {
    persist_create_candidate_with_fault(root, buddy_id, None)
}

fn persist_create_candidate_with_fault(
    root: &Path,
    buddy_id: &str,
    fault: Option<BuddyRegistryIoStage>,
) -> Result<BuddyCandidateDurability, BuddyStorageError> {
    use std::io::Write;
    if buddy_id.is_empty() || buddy_id.len() > 64 {
        return Err(BuddyStorageError::GenerationExhausted);
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(BUDDY_CREATE_CANDIDATE))
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                BuddyStorageError::PendingRegistry
            } else {
                BuddyStorageError::io(BuddyRegistryIoStage::CreateCandidate, error)
            }
        })?;
    if fault == Some(BuddyRegistryIoStage::WriteCandidate) {
        return Err(BuddyStorageError::io(
            BuddyRegistryIoStage::WriteCandidate,
            std::io::Error::other("synthetic candidate write failure"),
        ));
    }
    file.write_all(buddy_id.as_bytes())
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::WriteCandidate, error))?;
    if fault == Some(BuddyRegistryIoStage::SyncCandidate) {
        return Err(BuddyStorageError::io(
            BuddyRegistryIoStage::SyncCandidate,
            std::io::Error::other("synthetic candidate sync failure"),
        ));
    }
    file.sync_all()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::SyncCandidate, error))?;
    if fault == Some(BuddyRegistryIoStage::SyncCandidateDirectory) {
        return Err(BuddyStorageError::io(
            BuddyRegistryIoStage::SyncCandidateDirectory,
            std::io::Error::other("synthetic candidate directory sync failure"),
        ));
    }
    sync_registry_directory(root)
        .map(|directory_synced| {
            if directory_synced {
                BuddyCandidateDurability::FileAndDirectorySynced
            } else {
                BuddyCandidateDurability::FileSyncedDirectorySyncUnavailable
            }
        })
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::SyncCandidateDirectory, error))
}

#[cfg(not(test))]
fn clear_create_candidate(root: &Path) -> Result<(), BuddyStorageError> {
    clear_create_candidate_with_fault(root, None)
}

fn clear_create_candidate_with_fault(
    root: &Path,
    fault: Option<BuddyRegistryIoStage>,
) -> Result<(), BuddyStorageError> {
    if fault == Some(BuddyRegistryIoStage::RemoveCandidate) {
        return Err(BuddyStorageError::io(
            BuddyRegistryIoStage::RemoveCandidate,
            std::io::Error::other("synthetic candidate removal failure"),
        ));
    }
    std::fs::remove_file(root.join(BUDDY_CREATE_CANDIDATE))
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::RemoveCandidate, error))?;
    if fault == Some(BuddyRegistryIoStage::SyncCandidateCleanupDirectory) {
        return Err(BuddyStorageError::io(
            BuddyRegistryIoStage::SyncCandidateCleanupDirectory,
            std::io::Error::other("synthetic candidate cleanup directory sync failure"),
        ));
    }
    sync_registry_directory(root).map(|_| ()).map_err(|error| {
        BuddyStorageError::io(BuddyRegistryIoStage::SyncCandidateCleanupDirectory, error)
    })
}

fn not_published(error: BuddyStorageError) -> BuddyStorageError {
    if error.publication_state() == Some(BuddyPublicationState::NotPublished) {
        error
    } else {
        BuddyStorageError::RegistryCommit {
            disposition: BuddyPublicationState::NotPublished,
            source: Box::new(error),
        }
    }
}

fn candidate_commit(
    durability: BuddyCandidateDurability,
    source: BuddyStorageError,
) -> BuddyStorageError {
    BuddyStorageError::CandidateCommit {
        durability,
        source: Box::new(source),
    }
}

fn ensure_active(
    registry: &moe_chat_app::BuddyRegistry,
    id: &moe_chat_app::BuddyId,
) -> Result<(), BuddyStorageError> {
    if registry.active_buddy_id() == id {
        Ok(())
    } else {
        Err(BuddyStorageError::InactiveBuddy)
    }
}

fn validate_model_configuration(
    model: &moe_chat_app::BuddyModelConfiguration,
) -> Result<(), BuddyStorageError> {
    moe_agent_runtime::PresetConfig::new(
        "伙伴模型",
        "openai-compatible",
        model.endpoint(),
        None,
        model.model(),
        std::collections::BTreeMap::new(),
    )
    .map_err(BuddyStorageError::ModelConfiguration)?;
    moe_agent_runtime::PresetId::new(model.preset_id())
        .map_err(BuddyStorageError::ModelConfiguration)?;
    Ok(())
}

fn with_buddy_registry<T>(
    root: &std::path::Path,
    update: impl FnOnce(&mut moe_chat_app::BuddyRegistry) -> Result<T, BuddyStorageError>,
) -> Result<moe_chat_app::BuddyRegistrySnapshot, BuddyStorageError> {
    let (_, snapshot) = with_buddy_profile(root, |registry| {
        let previous_revision = registry.revision();
        let value = update(registry)?;
        if registry.revision() != previous_revision {
            write_buddy_registry(root, registry)?;
        }
        Ok((value, registry.snapshot()))
    })?;
    Ok(snapshot)
}

fn acquire_buddy_registry_lock(root: &Path) -> Result<std::fs::File, BuddyStorageError> {
    std::fs::create_dir_all(root)
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::CreateRoot, error))?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(BUDDY_REGISTRY_LOCK))
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::OpenLock, error))?;
    lock.try_lock()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::AcquireLock, error.into()))?;
    Ok(lock)
}

fn load_or_initialize_registry_locked(
    root: &Path,
) -> Result<moe_chat_app::BuddyRegistry, BuddyStorageError> {
    let path = root.join(BUDDY_REGISTRY_FILE);
    if root
        .join(BUDDY_REGISTRY_PENDING)
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        return Err(BuddyStorageError::PendingRegistry);
    }
    if root
        .join(BUDDY_CREATE_CANDIDATE)
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        return Err(BuddyStorageError::PendingRegistry);
    }
    if path
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        ensure_onboarding_evidence(root)?;
        read_buddy_registry(&path)
    } else {
        if onboarding_evidence_exists(root)? || buddy_instance_data_exists(root)? {
            return Err(BuddyStorageError::RegistryMissingAfterOnboarding);
        }
        let registry = legacy_buddy_registry(root)?;
        write_onboarding_evidence(root)?;
        initialize_memory_for_first_registry(root)?;
        write_buddy_registry(root, &registry)?;
        Ok(registry)
    }
}

fn onboarding_evidence_exists(root: &Path) -> Result<bool, BuddyStorageError> {
    let path = root.join(BUDDY_ONBOARDING_EVIDENCE);
    if !path
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        return Ok(false);
    }
    let bytes = read_bounded_frame(&path, BUDDY_ONBOARDING_EVIDENCE_BYTES.len() as u64)?;
    if bytes != BUDDY_ONBOARDING_EVIDENCE_BYTES {
        return Err(BuddyStorageError::InvalidOnboardingEvidence);
    }
    Ok(true)
}

fn ensure_onboarding_evidence(root: &Path) -> Result<(), BuddyStorageError> {
    if onboarding_evidence_exists(root)? {
        Ok(())
    } else {
        write_onboarding_evidence(root)
    }
}

fn write_onboarding_evidence(root: &Path) -> Result<(), BuddyStorageError> {
    use std::io::Write;
    let path = root.join(BUDDY_ONBOARDING_EVIDENCE);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                BuddyStorageError::InvalidOnboardingEvidence
            } else {
                BuddyStorageError::io(BuddyRegistryIoStage::CreateOnboardingEvidence, error)
            }
        })?;
    file.write_all(BUDDY_ONBOARDING_EVIDENCE_BYTES)
        .map_err(|error| {
            BuddyStorageError::io(BuddyRegistryIoStage::WriteOnboardingEvidence, error)
        })?;
    file.sync_all().map_err(|error| {
        BuddyStorageError::io(BuddyRegistryIoStage::SyncOnboardingEvidence, error)
    })?;
    sync_registry_directory(root).map_err(|error| {
        BuddyStorageError::io(BuddyRegistryIoStage::SyncOnboardingEvidence, error)
    })?;
    Ok(())
}

fn buddy_instance_data_exists(root: &Path) -> Result<bool, BuddyStorageError> {
    root.join("chat/instances")
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))
}

fn with_buddy_profile<T>(
    root: &std::path::Path,
    action: impl FnOnce(&mut moe_chat_app::BuddyRegistry) -> Result<T, BuddyStorageError>,
) -> Result<T, BuddyStorageError> {
    let _lock = acquire_buddy_registry_lock(root)?;
    let mut registry = load_or_initialize_registry_locked(root)?;
    action(&mut registry)
}

fn initialize_memory_for_first_registry(root: &std::path::Path) -> Result<(), BuddyStorageError> {
    let memory_path = root.join("memory.v1");
    if memory_path
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        return Ok(());
    }
    ensure_memory_creation_is_fresh(root)?;
    LocalMemory::create(&memory_path)
        .map(|_| ())
        .map_err(|error| BuddyStorageError::Application(AppError::from_storage(error)))
}

fn ensure_memory_creation_is_fresh(root: &Path) -> Result<(), BuddyStorageError> {
    let chat_root = root.join("chat");
    let has_chat_state = chat_root
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
        && std::fs::read_dir(&chat_root)
            .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
            .next()
            .transpose()
            .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
            .is_some();
    if root
        .join("memory.pending")
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
        || root
            .join("settings.json")
            .try_exists()
            .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
        || has_chat_state
    {
        return Err(BuddyStorageError::MissingMemory);
    }
    Ok(())
}

fn legacy_buddy_registry(
    root: &std::path::Path,
) -> Result<moe_chat_app::BuddyRegistry, BuddyStorageError> {
    if root
        .join("settings.pending")
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        return Err(BuddyStorageError::PendingLegacySettings);
    }
    let mut registry = moe_chat_app::BuddyRegistry::offline_default();
    if let Some(settings) = read_legacy_settings(root)? {
        let model = moe_chat_app::BuddyModelConfiguration::new(
            settings.endpoint,
            settings.model,
            "daily-chat",
        )
        .map_err(BuddyStorageError::Registry)?;
        validate_model_configuration(&model)?;
        let id = registry.active_buddy_id().clone();
        let profile_revision = registry.profile(&id).unwrap().profile_revision();
        let persona = settings.persona;
        registry
            .commit_configuration(1, &id, profile_revision, persona, model)
            .map_err(BuddyStorageError::Registry)?;
    }
    Ok(registry)
}

fn read_legacy_settings(
    root: &std::path::Path,
) -> Result<Option<moe_chat_app::ModelSettings>, BuddyStorageError> {
    let path = root.join("settings.json");
    if !path
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        return Ok(None);
    }
    let bytes = read_bounded_frame(&path, 32_768)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| BuddyStorageError::InvalidArchive)?;
    let get_string = |name: &str| {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or(BuddyStorageError::InvalidArchive)
    };
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(BuddyStorageError::InvalidArchive);
    }
    Ok(Some(moe_chat_app::ModelSettings {
        endpoint: get_string("endpoint")?,
        model: get_string("model")?,
        persona: get_string("persona")?,
        allow_transmission: false,
    }))
}

fn write_buddy_registry(
    root: &std::path::Path,
    registry: &moe_chat_app::BuddyRegistry,
) -> Result<BuddyRegistryCommitReceipt, BuddyStorageError> {
    write_buddy_registry_with_fault(root, registry, None)
}

fn write_buddy_registry_with_fault(
    root: &std::path::Path,
    registry: &moe_chat_app::BuddyRegistry,
    fault: Option<BuddyRegistryIoStage>,
) -> Result<BuddyRegistryCommitReceipt, BuddyStorageError> {
    use std::io::Write;
    let pending = root.join(BUDDY_REGISTRY_PENDING);
    let bytes = encode_buddy_registry(registry)?;
    if fault == Some(BuddyRegistryIoStage::CreatePending) {
        return Err(BuddyStorageError::io(
            BuddyRegistryIoStage::CreatePending,
            std::io::Error::other("synthetic create-pending failure"),
        ));
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                BuddyStorageError::PendingRegistry
            } else {
                BuddyStorageError::io(BuddyRegistryIoStage::CreatePending, error)
            }
        })?;
    if fault == Some(BuddyRegistryIoStage::WritePending) {
        return Err(BuddyStorageError::io(
            BuddyRegistryIoStage::WritePending,
            std::io::Error::other("synthetic write-pending failure"),
        ));
    }
    file.write_all(&bytes)
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::WritePending, error))?;
    if fault == Some(BuddyRegistryIoStage::SyncPending) {
        return Err(BuddyStorageError::io(
            BuddyRegistryIoStage::SyncPending,
            std::io::Error::other("synthetic sync-pending failure"),
        ));
    }
    file.sync_all()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::SyncPending, error))?;
    drop(file);
    if fault == Some(BuddyRegistryIoStage::Publish) {
        return Err(BuddyStorageError::io(
            BuddyRegistryIoStage::Publish,
            std::io::Error::other("synthetic publish failure"),
        ));
    }
    std::fs::rename(pending, root.join(BUDDY_REGISTRY_FILE))
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Publish, error))
        .and_then(|()| {
            if fault == Some(BuddyRegistryIoStage::SyncDirectory) {
                return Err(BuddyStorageError::RegistryCommit {
                    disposition: BuddyPublicationState::Published,
                    source: Box::new(BuddyStorageError::io(
                        BuddyRegistryIoStage::SyncDirectory,
                        std::io::Error::other("synthetic directory-sync failure"),
                    )),
                });
            }
            sync_registry_directory(root)
                .map(|directory_synced| BuddyRegistryCommitReceipt {
                    revision: registry.revision(),
                    file_synced: true,
                    directory_synced,
                })
                .map_err(|error| BuddyStorageError::RegistryCommit {
                    disposition: BuddyPublicationState::Published,
                    source: Box::new(BuddyStorageError::io(
                        BuddyRegistryIoStage::SyncDirectory,
                        error,
                    )),
                })
        })
}

fn sync_registry_directory(path: &Path) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)?.sync_all()?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(false)
    }
}

fn encode_buddy_registry(
    registry: &moe_chat_app::BuddyRegistry,
) -> Result<Vec<u8>, BuddyStorageError> {
    let buddies = registry
        .buddies()
        .iter()
        .map(|profile| {
            let model = profile.model().map(|model| {
                serde_json::json!({
                    "endpoint": model.endpoint(),
                    "model": model.model(),
                    "presetId": model.preset_id(),
                })
            });
            serde_json::json!({
                "id": profile.id().as_str(),
                "label": profile.label(),
                "profileRevision": profile.profile_revision(),
                "personaRevision": profile.persona_revision(),
                "modelRevision": profile.model_revision(),
                "persona": profile.persona(),
                "model": model,
            })
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "revision": registry.revision(),
        "activeBuddyId": registry.active_buddy_id().as_str(),
        "buddies": buddies,
    }))
    .map_err(|_| BuddyStorageError::InvalidArchive)?;
    if bytes.len() as u64 > MAX_BUDDY_REGISTRY_BYTES {
        return Err(BuddyStorageError::Registry(
            moe_chat_app::BuddyError::CapacityExceeded,
        ));
    }
    Ok(bytes)
}

fn read_buddy_registry(
    path: &std::path::Path,
) -> Result<moe_chat_app::BuddyRegistry, BuddyStorageError> {
    let bytes = read_bounded_frame(path, MAX_BUDDY_REGISTRY_BYTES)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| BuddyStorageError::InvalidArchive)?;
    if !exact_keys(&value, &["version", "revision", "activeBuddyId", "buddies"])
        || value.get("version").and_then(serde_json::Value::as_u64) != Some(1)
    {
        return Err(BuddyStorageError::InvalidArchive);
    }
    let active_id = json_field(&value, "activeBuddyId")?
        .as_str()
        .ok_or(BuddyStorageError::InvalidArchive)?;
    let active_id = moe_chat_app::BuddyId::new(active_id).map_err(BuddyStorageError::Registry)?;
    let revision = json_field(&value, "revision")?
        .as_u64()
        .ok_or(BuddyStorageError::InvalidArchive)?;
    let entries = json_field(&value, "buddies")?
        .as_array()
        .ok_or(BuddyStorageError::InvalidArchive)?;
    if entries.len() > moe_chat_app::MAX_BUDDIES {
        return Err(BuddyStorageError::InvalidArchive);
    }
    let mut profiles = Vec::with_capacity(entries.len());
    for entry in entries {
        if !exact_keys(
            entry,
            &[
                "id",
                "label",
                "profileRevision",
                "personaRevision",
                "modelRevision",
                "persona",
                "model",
            ],
        ) {
            return Err(BuddyStorageError::InvalidArchive);
        }
        let string = |key: &str| {
            json_field(entry, key)?
                .as_str()
                .ok_or(BuddyStorageError::InvalidArchive)
        };
        let number = |key: &str| {
            json_field(entry, key)?
                .as_u64()
                .ok_or(BuddyStorageError::InvalidArchive)
        };
        let id = moe_chat_app::BuddyId::new(string("id")?).map_err(BuddyStorageError::Registry)?;
        let profile = moe_chat_app::BuddyProfile::new(id, string("label")?, string("persona")?)
            .map_err(BuddyStorageError::Registry)?;
        let model = match json_field(entry, "model")? {
            serde_json::Value::Null => None,
            value => {
                if !exact_keys(value, &["endpoint", "model", "presetId"]) {
                    return Err(BuddyStorageError::InvalidArchive);
                }
                let get = |key: &str| {
                    value
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .ok_or(BuddyStorageError::InvalidArchive)
                };
                Some(
                    moe_chat_app::BuddyModelConfiguration::new(
                        get("endpoint")?,
                        get("model")?,
                        get("presetId")?,
                    )
                    .map_err(BuddyStorageError::Registry)?,
                )
            }
        };
        profiles.push(
            profile
                .with_persisted_state(
                    number("profileRevision")?,
                    number("personaRevision")?,
                    number("modelRevision")?,
                    model,
                )
                .map_err(BuddyStorageError::Registry)?,
        );
    }
    moe_chat_app::BuddyRegistry::restore(active_id, revision, profiles)
        .map_err(BuddyStorageError::Registry)
}

fn read_bounded_frame(path: &Path, max_bytes: u64) -> Result<Vec<u8>, BuddyStorageError> {
    use std::io::Read;
    let file = std::fs::File::open(path)
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?;
    if file
        .metadata()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
        .len()
        > max_bytes
    {
        return Err(BuddyStorageError::InvalidArchive);
    }
    let mut bytes = Vec::with_capacity(max_bytes.min(32_768) as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?;
    if bytes.len() as u64 > max_bytes {
        return Err(BuddyStorageError::InvalidArchive);
    }
    Ok(bytes)
}

fn exact_keys(value: &serde_json::Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    })
}

fn json_field<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a serde_json::Value, BuddyStorageError> {
    value.get(key).ok_or(BuddyStorageError::InvalidArchive)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSaveStage {
    AcquireRegistryLock,
    PendingRegistry,
    BuddyRegistryPresent,
    CreatePending,
    WritePending,
    SyncPending,
    Publish,
}

#[derive(Debug)]
pub struct SettingsSaveError {
    stage: SettingsSaveStage,
    source: std::io::Error,
}

impl SettingsSaveError {
    fn new(stage: SettingsSaveStage, source: std::io::Error) -> Self {
        Self { stage, source }
    }

    pub fn stage(&self) -> SettingsSaveStage {
        self.stage
    }
}

impl std::fmt::Display for SettingsSaveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self.stage {
            SettingsSaveStage::AcquireRegistryLock => "设置未保存：本地档案存储当前被占用",
            SettingsSaveStage::PendingRegistry => "设置未保存：伙伴档案发布待恢复",
            SettingsSaveStage::BuddyRegistryPresent => "设置未保存：buddies.v1 已成为配置权威",
            SettingsSaveStage::CreatePending => "设置未保存：无法创建临时文件",
            SettingsSaveStage::WritePending => "设置临时文件写入失败，现场已保留",
            SettingsSaveStage::SyncPending => "设置临时文件同步失败，保存结果未知且现场已保留",
            SettingsSaveStage::Publish => "设置未发布，临时文件已保留",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SettingsSaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

struct DiskMemory(LocalMemory);

fn map_memory_change_error(error: PersistenceError) -> AppError {
    match &error {
        PersistenceError::Memory(MemoryError::RevisionConflict { .. }) => {
            AppError::memory_revision_conflict(error)
        }
        PersistenceError::CapacityExceeded => AppError::memory_capacity_exceeded(error),
        _ => AppError::from_storage(error),
    }
}

/// Return the default daily archive root for the current Windows user.
pub fn root() -> Result<std::path::PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|path| {
            std::path::PathBuf::from(path)
                .join("MoeBuddy")
                .join("daily")
        })
        .ok_or_else(|| "无法确定当前用户的本地保存目录".into())
}

/// Load model settings from the default archive root.
pub fn load_settings() -> Result<Option<moe_chat_app::ModelSettings>, String> {
    load_settings_at(&root()?)
}

/// Load model settings from an explicit host-owned archive root.
///
/// Restored settings always deny transmission; API keys are never read from disk.
pub fn load_settings_at(
    root: &std::path::Path,
) -> Result<Option<moe_chat_app::ModelSettings>, String> {
    if !root.exists() {
        return Ok(None);
    }
    let _lock = acquire_buddy_registry_lock(root).map_err(|error| error.to_string())?;
    validate_legacy_entry_locked(root).map_err(|error| error.to_string())?;
    if root.join(BUDDY_REGISTRY_FILE).exists() {
        let registry = read_buddy_registry(&root.join(BUDDY_REGISTRY_FILE))
            .map_err(|error| error.to_string())?;
        let profile = registry
            .profile(registry.active_buddy_id())
            .ok_or_else(|| "当前伙伴档案不存在".to_owned())?;
        return Ok(profile
            .model()
            .map(|model| model.to_model_settings(profile.persona())));
    }
    read_legacy_settings(root).map_err(|error| error.to_string())
}

// Legacy entry points remain usable only before any durable onboarding or
// interrupted B migration evidence exists. Never infer a fresh root from a
// missing registry alone.
fn validate_legacy_entry_locked(root: &Path) -> Result<(), BuddyStorageError> {
    if root
        .join(BUDDY_REGISTRY_PENDING)
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
        || root
            .join(BUDDY_CREATE_CANDIDATE)
            .try_exists()
            .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
        || root
            .join("settings.pending")
            .try_exists()
            .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        return Err(BuddyStorageError::PendingRegistry);
    }
    if root
        .join(BUDDY_REGISTRY_FILE)
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        if !onboarding_evidence_exists(root)? {
            return Err(BuddyStorageError::InvalidOnboardingEvidence);
        }
        return Ok(());
    }
    if onboarding_evidence_exists(root)? || buddy_instance_data_exists(root)? {
        return Err(BuddyStorageError::RegistryMissingAfterOnboarding);
    }
    Ok(())
}

/// Save the non-secret model settings to the default archive root.
pub fn save_settings(settings: &moe_chat_app::ModelSettings) -> Result<(), String> {
    let root = root()?;
    save_settings_detailed_at(&root, settings).map_err(|error| error.to_string())
}

/// Save endpoint, model, and persona to an explicit host-owned archive root.
///
/// Transmission permission and API keys are intentionally omitted.
pub fn save_settings_at(
    root: &std::path::Path,
    settings: &moe_chat_app::ModelSettings,
) -> Result<(), String> {
    save_settings_detailed_at(root, settings).map_err(|error| error.to_string())
}

/// Save settings while preserving the failing I/O stage and its source error.
pub fn save_settings_detailed_at(
    root: &std::path::Path,
    settings: &moe_chat_app::ModelSettings,
) -> Result<(), SettingsSaveError> {
    use std::io::Write;
    let lock = acquire_buddy_registry_lock(root).map_err(|error| {
        let (stage, source) = match error {
            BuddyStorageError::Io { source, .. } => {
                (SettingsSaveStage::AcquireRegistryLock, source)
            }
            other => (
                SettingsSaveStage::AcquireRegistryLock,
                std::io::Error::other(other.to_string()),
            ),
        };
        SettingsSaveError::new(stage, source)
    })?;
    if let Err(error) = validate_legacy_entry_locked(root) {
        return Err(SettingsSaveError::new(
            if matches!(error, BuddyStorageError::PendingRegistry) {
                SettingsSaveStage::PendingRegistry
            } else {
                SettingsSaveStage::BuddyRegistryPresent
            },
            std::io::Error::other(error.to_string()),
        ));
    }
    if root.join(BUDDY_REGISTRY_FILE).exists() {
        return Err(SettingsSaveError::new(
            SettingsSaveStage::BuddyRegistryPresent,
            std::io::Error::other("buddies.v1 is authoritative"),
        ));
    }
    let pending = root.join("settings.pending");
    let bytes = serde_json::json!({"version":1,"endpoint":settings.endpoint,"model":settings.model,"persona":settings.persona}).to_string();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|error| SettingsSaveError::new(SettingsSaveStage::CreatePending, error))?;
    file.write_all(bytes.as_bytes())
        .map_err(|error| SettingsSaveError::new(SettingsSaveStage::WritePending, error))?;
    file.sync_all()
        .map_err(|error| SettingsSaveError::new(SettingsSaveStage::SyncPending, error))?;
    drop(file);
    std::fs::rename(pending, root.join("settings.json"))
        .map_err(|error| SettingsSaveError::new(SettingsSaveStage::Publish, error))?;
    drop(lock);
    Ok(())
}

/// Open the application using the default daily archive root.
pub fn open_application(
    generation: u64,
    now: u64,
) -> Result<moe_chat_app::ChatApplication, String> {
    open_application_at(&root()?, generation, now)
}

/// Restore an application from an explicit host-owned archive root.
///
/// Saved settings are loaded with transmission permission disabled. Restore errors
/// are returned without replacing damaged or incomplete archive files.
pub fn open_application_at(
    root: &std::path::Path,
    generation: u64,
    now: u64,
) -> Result<moe_chat_app::ChatApplication, String> {
    let _lock = acquire_buddy_registry_lock(root).map_err(|error| error.to_string())?;
    validate_legacy_entry_locked(root).map_err(|error| error.to_string())?;
    if root.join(BUDDY_REGISTRY_FILE).exists() {
        drop(_lock);
        let registry = load_buddy_registry_at(root).map_err(|error| error.to_string())?;
        let buddy_id = registry
            .active_buddy_id
            .as_deref()
            .ok_or_else(|| "当前伙伴身份缺失".to_owned())?;
        return open_application_for_buddy_at(root, buddy_id, registry.revision, generation, now)
            .map_err(|error| error.to_string());
    }
    let settings = read_legacy_settings(root).map_err(|error| error.to_string())?;
    let mut app = open_at(root, generation, now)?;
    if let Some(settings) = settings {
        app.configure_model(&settings)
            .map_err(|error| error.public_message().to_owned())?;
    }
    Ok(app)
}

fn open_at(
    root: &std::path::Path,
    generation: u64,
    now: u64,
) -> Result<moe_chat_app::ChatApplication, String> {
    let retention =
        moe_agent_runtime::RawRetention::new(30 * 86_400_000, 7 * 86_400_000, 8, 256, 65_536)
            .map_err(|_| "无法建立聊天保存策略")?;
    open_at_with_retention(root, generation, now, retention)
}

fn open_at_with_retention(
    root: &std::path::Path,
    generation: u64,
    now: u64,
    retention: moe_agent_runtime::RawRetention,
) -> Result<moe_chat_app::ChatApplication, String> {
    std::fs::create_dir_all(root).map_err(|_| "无法创建本地保存目录")?;
    let path = root.join("memory.v1");
    let memory = if path.exists() {
        LocalMemory::open(&path)
    } else {
        ensure_memory_creation_is_fresh(root).map_err(|error| error.to_string())?;
        LocalMemory::create(&path)
    }
    .map_err(|_| "私密记忆无法打开，原存档已保留；请检查文件与占用")?;
    let mut app =
        moe_chat_app::ChatApplication::with_memory(generation, Box::new(DiskMemory(memory)))
            .map_err(|error| error.public_message().to_owned())?;
    app.enable_history(
        Box::new(DiskHistory::new(root.join("chat"))),
        retention,
        now,
    )
    .map_err(|error| error.public_message().to_owned())?;
    Ok(app)
}

fn open_profile_at(
    root: &std::path::Path,
    generation: u64,
    now: u64,
    profile: &moe_chat_app::BuddyProfile,
) -> Result<moe_chat_app::ChatApplication, BuddyStorageError> {
    std::fs::create_dir_all(root)
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::CreateRoot, error))?;
    let path = root.join("memory.v1");
    if !path
        .try_exists()
        .map_err(|error| BuddyStorageError::io(BuddyRegistryIoStage::Read, error))?
    {
        return Err(BuddyStorageError::MissingMemory);
    }
    let memory = LocalMemory::open(&path)
        .map_err(AppError::from_storage)
        .map_err(BuddyStorageError::Application)?;
    let chat_root = root.join("chat");
    let history_root = buddy_history_root(&chat_root, profile.id().as_str());
    open_profile_with_adapters(
        generation,
        now,
        profile,
        Box::new(DiskMemory(memory)),
        Box::new(DiskHistory::new_scoped(history_root, chat_root)),
    )
}

fn open_shared_profile_at(
    root: &Path,
    generation: u64,
    now: u64,
    profile: &moe_chat_app::BuddyProfile,
    memory: Rc<RefCell<DiskMemory>>,
) -> Result<moe_chat_app::ChatApplication, BuddyStorageError> {
    let chat_root = root.join("chat");
    let history_root = buddy_history_root(&chat_root, profile.id().as_str());
    open_profile_with_adapters(
        generation,
        now,
        profile,
        Box::new(SharedDiskMemory(memory)),
        Box::new(DiskHistory::new_scoped_with_shared_root_lock(
            history_root,
            chat_root,
        )),
    )
}

fn buddy_history_root(chat_root: &Path, id: &str) -> PathBuf {
    if id == "offline-companion" {
        chat_root.to_path_buf()
    } else {
        chat_root.join("instances").join(id)
    }
}

fn open_profile_with_adapters(
    generation: u64,
    now: u64,
    profile: &moe_chat_app::BuddyProfile,
    memory: Box<dyn ChatMemory>,
    history: Box<dyn moe_chat_app::ChatHistory>,
) -> Result<moe_chat_app::ChatApplication, BuddyStorageError> {
    let namespace = Namespace::new("moe-chat", "local")
        .map_err(|error| BuddyStorageError::Application(AppError::from_storage(error)))?;
    let identity = PetIdentity::new(namespace, profile.id().as_str())
        .map_err(|error| BuddyStorageError::Application(AppError::from_storage(error)))?;
    let mut app = moe_chat_app::ChatApplication::with_memory_and_identity(
        generation,
        memory,
        identity,
        profile.persona_revision(),
        profile.model_revision(),
        profile.persona(),
    )
    .map_err(BuddyStorageError::Application)?;
    if let Some(model) = profile.model() {
        let settings = model.to_model_settings(profile.persona());
        app.configure_model_binding(
            &settings,
            profile.model_revision(),
            profile.persona_revision(),
            model.preset_id(),
        )
        .map_err(BuddyStorageError::Application)?;
    }
    let retention =
        moe_agent_runtime::RawRetention::new(30 * 86_400_000, 7 * 86_400_000, 8, 256, 65_536)
            .map_err(|_| BuddyStorageError::InvalidArchive)?;
    app.enable_history(history, retention, now)
        .map_err(BuddyStorageError::Application)?;
    Ok(app)
}

impl DiskMemory {
    fn operation() -> Result<OperationId, AppError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(AppError::from_storage)?;
        OperationId::new(format!("daily-{:032x}", u128::from_le_bytes(bytes)))
            .map_err(AppError::from_storage)
    }
}
impl MemoryContextAccess for DiskMemory {
    type Error = AppError;
    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now: u64,
    ) -> Result<MemoryTask, AppError> {
        self.0
            .begin_memory_task(identity, session, purpose, query, now)
            .map_err(AppError::from_storage)
    }
    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget: usize,
        now: u64,
    ) -> Result<RequestContext, AppError> {
        self.0
            .assemble_context(task, budget, now)
            .map_err(AppError::from_storage)
    }
    fn context_view<'a>(
        &mut self,
        context: &'a RequestContext,
        now: u64,
    ) -> Result<&'a ContextView, AppError> {
        self.0
            .context_view(context, now)
            .map_err(AppError::from_storage)
    }
}
impl ChatMemory for DiskMemory {
    fn register_source(
        &mut self,
        scope: MemoryScope,
        name: &str,
        revision: u64,
        epistemic: Epistemic,
    ) -> Result<SourceRef, AppError> {
        let identity = match &scope {
            MemoryScope::Private(identity) => identity.instance_id(),
            MemoryScope::Shared(_) => "shared",
        };
        let operation_id = if identity == "offline-companion" && revision == 1 {
            format!("daily-source-{name}")
        } else {
            format!("daily-source-{identity}-{name}-{revision}")
        };
        self.0
            .register_source(
                OperationId::new(operation_id).map_err(AppError::from_storage)?,
                scope,
                name,
                revision,
                epistemic,
            )
            .map(|receipt| receipt.value().clone())
            .map_err(map_memory_change_error)
    }
    fn set_identity_view(
        &mut self,
        identity: &PetIdentity,
        version: u64,
        text: &str,
        sources: Vec<SourceRef>,
    ) -> Result<(), AppError> {
        self.0
            .set_identity_view(identity, version, text, sources)
            .map_err(map_memory_change_error)
    }
    fn set_read_policy(
        &mut self,
        identity: &PetIdentity,
        policy: ReadPolicy,
    ) -> Result<(), AppError> {
        self.0
            .set_read_policy(identity, policy)
            .map_err(map_memory_change_error)
    }
    fn create(
        &mut self,
        identity: &PetIdentity,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.0
            .for_pet(identity)
            .create(Self::operation()?, draft, now)
            .map(|receipt| receipt.value().clone())
            .map_err(map_memory_change_error)
    }
    fn update(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.0
            .for_pet(identity)
            .update(Self::operation()?, id, revision, draft, now)
            .map(|receipt| receipt.value().clone())
            .map_err(map_memory_change_error)
    }
    fn delete(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.0
            .for_pet(identity)
            .delete(Self::operation()?, id, revision, now)
            .map(|receipt| receipt.value().clone())
            .map_err(map_memory_change_error)
    }
    fn retrieve(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        query: &RetrievalQuery,
        now: u64,
    ) -> Result<RetrievalResult, AppError> {
        self.0
            .for_session(identity, session, ReadPurpose::OfflineReasoning)
            .retrieve(query, now)
            .map_err(AppError::from_storage)
    }
}

struct SharedDiskMemory(Rc<RefCell<DiskMemory>>);

impl SharedDiskMemory {
    fn with<T>(&mut self, action: impl FnOnce(&mut DiskMemory) -> T) -> T {
        action(&mut self.0.borrow_mut())
    }
}

impl MemoryContextAccess for SharedDiskMemory {
    type Error = AppError;

    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now: u64,
    ) -> Result<MemoryTask, AppError> {
        self.with(|memory| memory.begin_memory_task(identity, session, purpose, query, now))
    }

    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget: usize,
        now: u64,
    ) -> Result<RequestContext, AppError> {
        self.with(|memory| memory.assemble_context(task, budget, now))
    }

    fn context_view<'a>(
        &mut self,
        context: &'a RequestContext,
        now: u64,
    ) -> Result<&'a ContextView, AppError> {
        self.with(|memory| memory.context_view(context, now))
    }
}

impl ChatMemory for SharedDiskMemory {
    fn register_source(
        &mut self,
        scope: MemoryScope,
        name: &str,
        revision: u64,
        epistemic: Epistemic,
    ) -> Result<SourceRef, AppError> {
        self.with(|memory| memory.register_source(scope, name, revision, epistemic))
    }

    fn set_identity_view(
        &mut self,
        identity: &PetIdentity,
        version: u64,
        text: &str,
        sources: Vec<SourceRef>,
    ) -> Result<(), AppError> {
        self.with(|memory| memory.set_identity_view(identity, version, text, sources))
    }

    fn set_read_policy(
        &mut self,
        identity: &PetIdentity,
        policy: ReadPolicy,
    ) -> Result<(), AppError> {
        self.with(|memory| memory.set_read_policy(identity, policy))
    }

    fn create(
        &mut self,
        identity: &PetIdentity,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.with(|memory| memory.create(identity, draft, now))
    }

    fn update(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.with(|memory| memory.update(identity, id, revision, draft, now))
    }

    fn delete(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.with(|memory| memory.delete(identity, id, revision, now))
    }

    fn retrieve(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        query: &RetrievalQuery,
        now: u64,
    ) -> Result<RetrievalResult, AppError> {
        self.with(|memory| memory.retrieve(identity, session, query, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moe_agent_runtime::{
        CallerSubject, ChannelId, ChannelScope, IngressToken, RawDomainKey, RawIngressCall,
        RawRetention, ReplyTarget, SessionAccess, SessionLimits, SessionRuntime, SessionTiming,
        TrustedIngress, UserEvent, UserEventKind,
    };
    use moe_chat_app::{
        Action, ChatHistory, ChatStatus, ExecutionDisposition, ModelSettings, PanelKind,
    };
    use moe_memory_core::{Namespace, PetIdentity};
    use std::error::Error;
    fn directory() -> std::path::PathBuf {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).unwrap();
        std::env::temp_dir().join(format!(
            "buddy-synthetic-{:032x}",
            u128::from_le_bytes(bytes)
        ))
    }

    fn tree_bytes(root: &std::path::Path) -> u64 {
        fn visit(path: &std::path::Path, bytes: &mut u64) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                if metadata.is_dir() {
                    visit(&entry.path(), bytes);
                } else {
                    *bytes += metadata.len();
                }
            }
        }
        let mut bytes = 0;
        visit(root, &mut bytes);
        bytes
    }

    #[test]
    fn buddies_keep_private_memory_and_raw_sessions_isolated_across_restarts() {
        let root = directory();
        let initial = load_buddy_registry_at(&root).unwrap();
        let offline_id = initial.active_buddy_id.unwrap();
        let mut offline =
            open_application_for_buddy_at(&root, &offline_id, initial.revision, 70, 1_000).unwrap();
        offline
            .dispatch(Action::Open(PanelKind::Memory), 1_000)
            .unwrap();
        offline
            .dispatch(Action::Remember("private offline memory".into()), 1_001)
            .unwrap();
        offline.dispatch(Action::RefreshMemories, 1_002).unwrap();
        offline
            .dispatch(Action::Open(PanelKind::Chat), 1_003)
            .unwrap();
        send_offline(&mut offline, "private offline session", 1_004);
        drop(offline);

        let created =
            create_buddy_at(&root, initial.revision, "same label", "new persona").unwrap();
        let buddy_id = created.active_buddy_id.clone().unwrap();
        let mut buddy =
            open_application_for_buddy_at(&root, &buddy_id, created.revision, 71, 2_000).unwrap();
        assert_eq!(buddy.identity().instance_id(), buddy_id);
        buddy
            .dispatch(Action::Open(PanelKind::Memory), 2_000)
            .unwrap();
        buddy.dispatch(Action::RefreshMemories, 2_001).unwrap();
        assert!(buddy.snapshot().memories.is_empty());
        buddy
            .dispatch(Action::Remember("private new-buddy memory".into()), 2_002)
            .unwrap();
        buddy.dispatch(Action::RefreshMemories, 2_003).unwrap();
        let buddy_memories = buddy.snapshot().memories;
        assert_eq!(buddy_memories.len(), 1);
        assert_eq!(buddy_memories[0].text, "private new-buddy memory");
        buddy
            .dispatch(Action::Open(PanelKind::Chat), 2_004)
            .unwrap();
        send_offline(&mut buddy, "private new-buddy session", 2_005);
        drop(buddy);

        let switched = switch_buddy_at(&root, created.revision, &offline_id).unwrap();
        let mut offline =
            open_application_for_buddy_at(&root, &offline_id, switched.revision, 72, 3_000)
                .unwrap();
        offline
            .dispatch(Action::Open(PanelKind::Memory), 3_000)
            .unwrap();
        offline.dispatch(Action::RefreshMemories, 3_001).unwrap();
        let offline_memories = offline.snapshot().memories;
        assert_eq!(offline_memories.len(), 1);
        assert_eq!(offline_memories[0].text, "private offline memory");
        offline
            .dispatch(Action::Open(PanelKind::Chat), 3_002)
            .unwrap();
        let offline_messages = offline.snapshot().chat.messages;
        assert!(
            offline_messages
                .iter()
                .any(|message| message.text.contains("private offline session"))
        );
        assert!(
            !offline_messages
                .iter()
                .any(|message| message.text.contains("private new-buddy session"))
        );
        drop(offline);

        let switched = switch_buddy_at(&root, switched.revision, &buddy_id).unwrap();
        let mut buddy =
            open_application_for_buddy_at(&root, &buddy_id, switched.revision, 73, 4_000).unwrap();
        buddy
            .dispatch(Action::Open(PanelKind::Memory), 4_000)
            .unwrap();
        buddy.dispatch(Action::RefreshMemories, 4_001).unwrap();
        let buddy_memories = buddy.snapshot().memories;
        assert_eq!(buddy_memories.len(), 1);
        assert_eq!(buddy_memories[0].text, "private new-buddy memory");
        buddy
            .dispatch(Action::Open(PanelKind::Chat), 4_002)
            .unwrap();
        let buddy_messages = buddy.snapshot().chat.messages;
        assert!(
            buddy_messages
                .iter()
                .any(|message| message.text.contains("private new-buddy session"))
        );
        assert!(
            !buddy_messages
                .iter()
                .any(|message| message.text.contains("private offline session"))
        );
        drop(buddy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn profile_and_model_versions_restore_together_without_transmission_consent() {
        let root = directory();
        let initial = load_buddy_registry_at(&root).unwrap();
        let created =
            create_buddy_at(&root, initial.revision, "configured", "first persona").unwrap();
        let id = created.active_buddy_id.clone().unwrap();
        let profile = load_buddy_profile_at(&root, &id).unwrap();
        let model = moe_chat_app::BuddyModelConfiguration::new(
            "http://127.0.0.1:9/v1/chat/completions",
            "synthetic-model",
            "synthetic-profile-preset",
        )
        .unwrap();
        let committed = commit_buddy_configuration_at(
            &root,
            &id,
            created.revision,
            profile.profile_revision(),
            "committed persona",
            model,
        )
        .unwrap();
        assert!(committed.revision > created.revision);
        let stale_open =
            match open_application_for_buddy_at(&root, &id, created.revision, 80, 5_000) {
                Err(error) => error,
                Ok(app) => {
                    drop(app);
                    panic!("opening with a stale buddy revision must fail")
                }
            };
        assert!(matches!(stale_open, BuddyStorageError::RevisionConflict));

        let profile = load_buddy_profile_at(&root, &id).unwrap();
        assert_eq!(profile.profile_revision(), 2);
        assert_eq!(profile.persona_revision(), 2);
        assert_eq!(profile.model_revision(), 2);
        assert_eq!(profile.persona(), "committed persona");
        assert_eq!(
            profile.model().unwrap().preset_id(),
            "synthetic-profile-preset"
        );
        let persisted = std::fs::read_to_string(root.join(BUDDY_REGISTRY_FILE)).unwrap();
        assert!(!persisted.contains("apiKey"));
        assert!(!persisted.contains("allowTransmission"));
        assert!(!persisted.contains("transmissionAllowed"));

        let app = open_application_for_buddy_at(&root, &id, committed.revision, 81, 5_001).unwrap();
        let snapshot = app.snapshot();
        assert_eq!(snapshot.companion, id);
        assert_eq!(snapshot.mode, "online");
        assert_eq!(snapshot.active_preset, "synthetic-profile-preset");
        assert!(!snapshot.external_transmission_allowed);
        drop(app);
        let mut restored =
            open_application_for_buddy_at(&root, &id, committed.revision, 82, 5_002).unwrap();
        assert_eq!(restored.identity().instance_id(), id);
        assert_eq!(
            restored.snapshot().active_preset,
            "synthetic-profile-preset"
        );
        assert!(!restored.snapshot().external_transmission_allowed);
        restored
            .dispatch(Action::Open(PanelKind::Presets), 5_003)
            .unwrap();
        let catalog = restored.snapshot().presets;
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].id, "synthetic-profile-preset");
        restored
            .dispatch(Action::SelectPreset(catalog[0].id.clone()), 5_004)
            .unwrap();
        assert_eq!(restored.snapshot().active_preset, catalog[0].id);
        assert!(!restored.snapshot().external_transmission_allowed);
        drop(restored);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_consent_is_applied_with_the_published_profile_version_and_cleared_on_restart() {
        let root = directory();
        let mut storage = BuddyStorage::open(&root, 700, 10_000).unwrap();
        let registry = storage.registry_snapshot().unwrap();
        let buddy_id = storage.active_buddy_id().unwrap().to_owned();
        let profile = storage.buddy_profile(&buddy_id).unwrap();
        let model = moe_chat_app::BuddyModelConfiguration::new(
            "http://127.0.0.1:9/v1/chat/completions",
            "synthetic-model",
            "synthetic-selected-preset",
        )
        .unwrap();
        let mut runtime = model.to_model_settings("committed persona");
        runtime.allow_transmission = true;
        let receipt = storage
            .commit_configuration_with_runtime(
                &buddy_id,
                registry.revision,
                profile.profile_revision(),
                model,
                runtime,
                10_001,
            )
            .unwrap();
        assert_eq!(receipt.registry.revision, registry.revision + 1);
        assert!(
            storage
                .active_application()
                .unwrap()
                .snapshot()
                .external_transmission_allowed
        );
        assert_eq!(
            storage
                .active_application()
                .unwrap()
                .snapshot()
                .active_preset,
            "synthetic-selected-preset"
        );
        let encoded = std::fs::read_to_string(root.join(BUDDY_REGISTRY_FILE)).unwrap();
        assert!(!encoded.contains("apiKey"));
        assert!(!encoded.contains("allowTransmission"));
        drop(storage);

        let restored = BuddyStorage::open(&root, 701, 10_002).unwrap();
        let restored_app = restored.active_application().unwrap().snapshot();
        assert_eq!(restored_app.companion, buddy_id);
        assert_eq!(restored_app.active_preset, "synthetic-selected-preset");
        assert!(!restored_app.external_transmission_allowed);
        drop(restored);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_commands_and_incomplete_or_bad_archives_leave_files_untouched() {
        let root = directory();
        let initial = load_buddy_registry_at(&root).unwrap();
        let original = std::fs::read(root.join(BUDDY_REGISTRY_FILE)).unwrap();
        let stale = create_buddy_at(&root, initial.revision + 1, "stale", "persona").unwrap_err();
        assert!(matches!(
            stale,
            BuddyStorageError::Registry(moe_chat_app::BuddyError::StaleRegistry)
        ));
        assert_eq!(
            std::fs::read(root.join(BUDDY_REGISTRY_FILE)).unwrap(),
            original
        );

        let created = create_buddy_at(&root, initial.revision, "created once", "persona").unwrap();
        let committed = std::fs::read(root.join(BUDDY_REGISTRY_FILE)).unwrap();
        assert!(matches!(
            create_buddy_at(&root, initial.revision, "retry", "persona"),
            Err(BuddyStorageError::Registry(
                moe_chat_app::BuddyError::StaleRegistry
            ))
        ));
        assert_eq!(created.buddies.len(), 2);
        assert_eq!(
            std::fs::read(root.join(BUDDY_REGISTRY_FILE)).unwrap(),
            committed
        );

        let pending = b"synthetic pending profile publication";
        std::fs::write(root.join(BUDDY_REGISTRY_PENDING), pending).unwrap();
        assert!(matches!(
            switch_buddy_at(&root, initial.revision, "offline-companion"),
            Err(BuddyStorageError::PendingRegistry)
        ));
        assert_eq!(
            std::fs::read(root.join(BUDDY_REGISTRY_PENDING)).unwrap(),
            pending
        );
        assert_eq!(
            std::fs::read(root.join(BUDDY_REGISTRY_FILE)).unwrap(),
            committed
        );
        std::fs::remove_file(root.join(BUDDY_REGISTRY_PENDING)).unwrap();
        std::fs::remove_dir_all(root).unwrap();

        let corrupt_root = directory();
        let corrupt = b"{synthetic malformed registry";
        std::fs::create_dir_all(&corrupt_root).unwrap();
        std::fs::write(corrupt_root.join(BUDDY_REGISTRY_FILE), corrupt).unwrap();
        assert!(matches!(
            load_buddy_registry_at(&corrupt_root),
            Err(BuddyStorageError::InvalidArchive)
        ));
        assert_eq!(
            std::fs::read(corrupt_root.join(BUDDY_REGISTRY_FILE)).unwrap(),
            corrupt
        );
        std::fs::remove_dir_all(corrupt_root).unwrap();

        let missing_root = directory();
        let initial = load_buddy_registry_at(&missing_root).unwrap();
        let buddy_id = initial.active_buddy_id.unwrap();
        std::fs::remove_file(missing_root.join("memory.v1")).unwrap();
        assert!(matches!(
            open_application_for_buddy_at(&missing_root, &buddy_id, initial.revision, 85, 5_500),
            Err(BuddyStorageError::MissingMemory)
        ));
        assert!(!missing_root.join("memory.v1").exists());
        std::fs::remove_dir_all(missing_root).unwrap();

        let legacy_root = directory();
        std::fs::create_dir_all(legacy_root.join("chat")).unwrap();
        std::fs::write(legacy_root.join("chat/.sessions.lock"), b"").unwrap();
        assert!(matches!(
            load_buddy_registry_at(&legacy_root),
            Err(BuddyStorageError::MissingMemory)
        ));
        assert!(!legacy_root.join(BUDDY_REGISTRY_FILE).exists());
        assert!(!legacy_root.join("memory.v1").exists());
        std::fs::remove_dir_all(legacy_root).unwrap();
    }

    #[test]
    fn missing_buddy_registry_after_first_access_never_reinitializes_profiles() {
        let default_root = directory();
        let default = load_buddy_registry_at(&default_root).unwrap();
        assert_eq!(default.buddies.len(), 1);
        let default_memory = std::fs::read(default_root.join("memory.v1")).unwrap();
        std::fs::remove_file(default_root.join(BUDDY_REGISTRY_FILE)).unwrap();
        assert!(matches!(
            load_buddy_registry_at(&default_root),
            Err(BuddyStorageError::RegistryMissingAfterOnboarding)
        ));
        assert_eq!(
            std::fs::read(default_root.join("memory.v1")).unwrap(),
            default_memory
        );
        assert!(default_root.join(BUDDY_ONBOARDING_EVIDENCE).exists());
        std::fs::remove_dir_all(default_root).unwrap();

        let two_buddy_root = directory();
        let initial = load_buddy_registry_at(&two_buddy_root).unwrap();
        let created = create_buddy_at(
            &two_buddy_root,
            initial.revision,
            "second",
            "second persona",
        )
        .unwrap();
        assert_eq!(created.buddies.len(), 2);
        let memory_before = std::fs::read(two_buddy_root.join("memory.v1")).unwrap();
        std::fs::remove_file(two_buddy_root.join(BUDDY_REGISTRY_FILE)).unwrap();
        assert!(matches!(
            load_buddy_registry_at(&two_buddy_root),
            Err(BuddyStorageError::RegistryMissingAfterOnboarding)
        ));
        assert_eq!(
            std::fs::read(two_buddy_root.join("memory.v1")).unwrap(),
            memory_before
        );
        std::fs::remove_dir_all(two_buddy_root).unwrap();

        let instance_only_root = directory();
        std::fs::create_dir_all(instance_only_root.join("chat/instances/synthetic-buddy")).unwrap();
        assert!(matches!(
            load_buddy_registry_at(&instance_only_root),
            Err(BuddyStorageError::RegistryMissingAfterOnboarding)
        ));
        assert!(!instance_only_root.join("memory.v1").exists());
        std::fs::remove_dir_all(instance_only_root).unwrap();

        let interrupted_root = directory();
        std::fs::create_dir_all(&interrupted_root).unwrap();
        std::fs::write(
            interrupted_root.join(BUDDY_ONBOARDING_EVIDENCE),
            BUDDY_ONBOARDING_EVIDENCE_BYTES,
        )
        .unwrap();
        assert!(matches!(
            load_buddy_registry_at(&interrupted_root),
            Err(BuddyStorageError::RegistryMissingAfterOnboarding)
        ));
        assert!(!interrupted_root.join("memory.v1").exists());
        assert!(!interrupted_root.join(BUDDY_REGISTRY_FILE).exists());
        std::fs::remove_dir_all(interrupted_root).unwrap();
    }

    #[test]
    fn legacy_settings_migrate_to_the_original_identity_without_transmission_consent() {
        let root = directory();
        std::fs::create_dir_all(&root).unwrap();
        let persona = "first line\r\nsecond\tline";
        let settings = ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic legacy model".into(),
            persona: persona.into(),
            allow_transmission: true,
        };
        save_settings_at(&root, &settings).unwrap();
        LocalMemory::create(root.join("memory.v1")).unwrap();
        let registry = load_buddy_registry_at(&root).unwrap();
        assert_eq!(
            registry.active_buddy_id.as_deref(),
            Some("offline-companion")
        );
        assert_eq!(registry.buddies.len(), 1);
        let profile = load_buddy_profile_at(&root, "offline-companion").unwrap();
        assert_eq!(profile.persona(), persona);
        assert_eq!(profile.persona_revision(), 2);
        assert_eq!(profile.model_revision(), 2);

        let app =
            open_application_for_buddy_at(&root, "offline-companion", registry.revision, 86, 5_600)
                .unwrap();
        assert_eq!(app.snapshot().companion, "offline-companion");
        assert_eq!(app.snapshot().active_preset, "daily-chat");
        assert!(!app.snapshot().external_transmission_allowed);
        drop(app);

        let resumed = BuddyStorage::open(&root, 87, 5_601).unwrap();
        assert_eq!(resumed.active_buddy_id().unwrap(), "offline-companion");
        assert_eq!(
            resumed
                .buddy_profile("offline-companion")
                .unwrap()
                .persona(),
            persona
        );
        drop(resumed);
        assert_eq!(
            load_settings_at(&root).unwrap().unwrap().persona,
            persona,
            "legacy read compatibility follows the B registry after migration"
        );
        assert!(matches!(
            save_settings_at(&root, &settings),
            Err(message) if message.contains("buddies.v1")
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn buddy_registry_frame_over_limit_is_rejected_without_rewriting_it() {
        let root = directory();
        std::fs::create_dir_all(&root).unwrap();
        let oversized = vec![b'x'; MAX_BUDDY_REGISTRY_BYTES as usize + 1];
        std::fs::write(root.join(BUDDY_REGISTRY_FILE), &oversized).unwrap();
        assert!(matches!(
            load_buddy_registry_at(&root),
            Err(BuddyStorageError::InvalidArchive)
        ));
        assert_eq!(
            std::fs::metadata(root.join(BUDDY_REGISTRY_FILE))
                .unwrap()
                .len(),
            oversized.len() as u64
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn buddy_storage_keeps_two_live_apps_and_their_runtime_state_isolated() {
        let root = directory();
        let mut storage = BuddyStorage::open(&root, 500, 10_000).unwrap();
        let original = storage.registry_snapshot().unwrap();
        let first_id = original.active_buddy_id.unwrap();
        let created = storage
            .create_buddy(original.revision, "second", "second persona", 10_001)
            .unwrap();
        assert!(created.publication.file_synced);
        assert_eq!(
            created.candidate,
            Some(if cfg!(windows) {
                BuddyCandidateDurability::FileSyncedDirectorySyncUnavailable
            } else {
                BuddyCandidateDurability::FileAndDirectorySynced
            })
        );
        assert_eq!(created.publication.revision, created.registry.revision);
        let second_id = created.registry.active_buddy_id.unwrap();
        assert_ne!(first_id, second_id);

        for id in [&first_id, &second_id] {
            let app = storage.application_mut(id).unwrap();
            app.dispatch(Action::Open(PanelKind::Chat), 10_002).unwrap();
            app.dispatch(Action::Open(PanelKind::Memory), 10_002)
                .unwrap();
            app.dispatch(Action::Open(PanelKind::Diagnostics), 10_002)
                .unwrap();
        }
        storage
            .application_mut(&first_id)
            .unwrap()
            .dispatch(Action::SetDraft("first draft".into()), 10_003)
            .unwrap();
        storage
            .application_mut(&first_id)
            .unwrap()
            .dispatch(Action::Remember("first private note".into()), 10_004)
            .unwrap();
        let (expected_session_id, expected_sessions_revision) =
            session_guard(storage.application_mut(&first_id).unwrap());
        storage
            .application_mut(&first_id)
            .unwrap()
            .dispatch(
                Action::NewSession {
                    expected_session_id,
                    expected_sessions_revision,
                },
                10_005,
            )
            .unwrap();

        storage
            .application_mut(&second_id)
            .unwrap()
            .dispatch(Action::SetDraft("second draft".into()), 10_006)
            .unwrap();
        storage
            .application_mut(&first_id)
            .unwrap()
            .dispatch(Action::SetDraft("first session two".into()), 10_006)
            .unwrap();

        let before_second_config = storage.registry_snapshot().unwrap();
        let second_profile = storage.buddy_profile(&second_id).unwrap();
        let model = moe_chat_app::BuddyModelConfiguration::new(
            "http://127.0.0.1:9/v1/chat/completions",
            "synthetic-second-model",
            "synthetic-second-preset",
        )
        .unwrap();
        let configured = storage
            .commit_configuration(
                &second_id,
                before_second_config.revision,
                second_profile.profile_revision(),
                "second\nconfigured persona",
                model,
                10_007,
            )
            .unwrap();
        assert_eq!(
            configured.publication.revision,
            configured.registry.revision
        );
        let second_app = storage.application_mut(&second_id).unwrap();
        second_app
            .dispatch(Action::Remember("second private note".into()), 10_008)
            .unwrap();
        second_app
            .dispatch(Action::SearchMemories(String::new()), 10_009)
            .unwrap();
        second_app
            .dispatch(Action::RefreshDiagnostics, 10_009)
            .unwrap();
        assert!(
            second_app
                .snapshot()
                .memories
                .iter()
                .any(|note| note.text == "second private note")
        );
        assert_eq!(second_app.snapshot().companion, second_id);
        assert!(!second_app.snapshot().external_transmission_allowed);
        assert_eq!(second_app.snapshot().sessions.len(), 1);
        assert_eq!(second_app.snapshot().chat.draft, "second draft");
        assert!(storage.buddy_profile(&first_id).unwrap().model().is_none());
        assert!(storage.buddy_profile(&second_id).unwrap().model().is_some());

        let first_app = storage.application_mut(&first_id).unwrap();
        first_app
            .dispatch(Action::SearchMemories(String::new()), 10_010)
            .unwrap();
        first_app
            .dispatch(Action::RefreshDiagnostics, 10_010)
            .unwrap();
        assert!(
            first_app
                .snapshot()
                .memories
                .iter()
                .any(|note| note.text == "first private note")
        );
        assert!(
            first_app
                .snapshot()
                .memories
                .iter()
                .all(|note| note.text != "second private note")
        );
        assert_eq!(first_app.snapshot().sessions.len(), 2);
        assert_eq!(first_app.snapshot().chat.draft, "first session two");
        let first_diagnostics = first_app.snapshot().diagnostics.len();
        let first_revision = first_app.snapshot().revision;

        for target in [&first_id, &second_id, &first_id, &second_id] {
            let revision = storage.registry_snapshot().unwrap().revision;
            let result = storage.switch_buddy(target, revision, 10_011).unwrap();
            assert_eq!(
                result.registry.active_buddy_id.as_deref(),
                Some(target.as_str())
            );
            assert_eq!(storage.active_buddy_id().unwrap(), target);
        }
        let first_app = storage.application_mut(&first_id).unwrap();
        assert_eq!(first_app.snapshot().revision, first_revision);
        assert_eq!(first_app.snapshot().chat.draft, "first session two");
        assert_eq!(first_app.snapshot().diagnostics.len(), first_diagnostics);
        let second_app = storage.application_mut(&second_id).unwrap();
        second_app
            .dispatch(Action::SearchMemories(String::new()), 10_012)
            .unwrap();
        assert!(
            second_app
                .snapshot()
                .memories
                .iter()
                .all(|note| note.text != "first private note")
        );
        assert_eq!(second_app.snapshot().chat.draft, "second draft");

        assert!(matches!(
            BuddyStorage::open(&root, 900, 10_013),
            Err(BuddyStorageError::Io {
                stage: BuddyRegistryIoStage::AcquireLock,
                ..
            })
        ));
        drop(storage);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn buddy_storage_prepares_targets_and_reports_configuration_publication_state() {
        let root = directory();
        let initial = load_buddy_registry_at(&root).unwrap();
        let first_id = initial.active_buddy_id.unwrap();
        let created = create_buddy_at(&root, initial.revision, "second", "second persona").unwrap();
        let second_id = created.active_buddy_id.unwrap();
        let selected_first = switch_buddy_at(&root, created.revision, &first_id).unwrap();
        let mut storage = BuddyStorage::open(&root, 700, 20_000).unwrap();
        let second_history = buddy_history_root(&root.join("chat"), &second_id);
        std::fs::create_dir_all(&second_history).unwrap();
        std::fs::write(
            second_history.join("sessions.pending"),
            b"synthetic pending",
        )
        .unwrap();

        let before = storage.registry_snapshot().unwrap();
        assert_eq!(before.revision, selected_first.revision);
        let open_error = storage
            .switch_buddy(&second_id, before.revision, 20_001)
            .unwrap_err();
        assert_eq!(
            open_error.publication_state(),
            Some(BuddyPublicationState::NotPublished)
        );
        assert!(matches!(
            open_error,
            BuddyStorageError::RegistryCommit {
                source,
                disposition: BuddyPublicationState::NotPublished,
            } if matches!(*source, BuddyStorageError::Preparation(_))
        ));
        assert_eq!(storage.active_buddy_id().unwrap(), first_id);
        assert_eq!(
            storage.registry_snapshot().unwrap().revision,
            before.revision
        );

        std::fs::remove_file(second_history.join("sessions.pending")).unwrap();
        let profile = storage.buddy_profile(&first_id).unwrap();
        let invalid_model = moe_chat_app::BuddyModelConfiguration::new(
            "not an endpoint",
            "synthetic-model",
            "synthetic-preset",
        )
        .unwrap();
        let invalid = storage
            .commit_configuration(
                &first_id,
                before.revision,
                profile.profile_revision(),
                "valid persona",
                invalid_model,
                20_002,
            )
            .unwrap_err();
        assert_eq!(
            invalid.publication_state(),
            Some(BuddyPublicationState::NotPublished)
        );
        assert!(matches!(
            invalid,
            BuddyStorageError::RegistryCommit {
                source,
                disposition: BuddyPublicationState::NotPublished,
            } if matches!(*source, BuddyStorageError::ModelConfiguration(_))
        ));
        assert_eq!(
            storage.registry_snapshot().unwrap().revision,
            before.revision
        );

        let profile = storage.buddy_profile(&first_id).unwrap();
        let model = moe_chat_app::BuddyModelConfiguration::new(
            "http://127.0.0.1:9/v1/chat/completions",
            "synthetic-model",
            "synthetic-preset",
        )
        .unwrap();
        std::fs::write(
            root.join(BUDDY_REGISTRY_PENDING),
            b"synthetic pending registry",
        )
        .unwrap();
        let failed = storage
            .commit_configuration(
                &first_id,
                before.revision,
                profile.profile_revision(),
                "valid persona",
                model,
                20_003,
            )
            .unwrap_err();
        assert_eq!(
            failed.publication_state(),
            Some(BuddyPublicationState::NotPublished)
        );
        assert_eq!(
            storage.registry_snapshot().unwrap().revision,
            before.revision
        );
        assert!(root.join(BUDDY_REGISTRY_PENDING).exists());
        drop(storage);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn buddy_registry_publish_failure_is_reported_as_unknown_and_freezes_storage() {
        let root = directory();
        let mut storage = BuddyStorage::open(&root, 800, 30_000).unwrap();
        let registry = storage.registry_snapshot().unwrap();
        let id = registry.active_buddy_id.unwrap();
        let profile = storage.buddy_profile(&id).unwrap();
        let model = moe_chat_app::BuddyModelConfiguration::new(
            "http://127.0.0.1:9/v1/chat/completions",
            "synthetic-model",
            "synthetic-preset",
        )
        .unwrap();
        let current = root.join(BUDDY_REGISTRY_FILE);
        let backup = root.join("synthetic-registry-backup");
        std::fs::rename(&current, &backup).unwrap();
        std::fs::create_dir(&current).unwrap();
        let error = storage
            .commit_configuration(
                &id,
                registry.revision,
                profile.profile_revision(),
                "configured persona",
                model,
                30_001,
            )
            .unwrap_err();
        assert_eq!(
            error.publication_state(),
            Some(BuddyPublicationState::Unknown)
        );
        assert!(matches!(
            storage.registry_snapshot(),
            Err(BuddyStorageError::RegistryCommit {
                disposition: BuddyPublicationState::Unknown,
                ..
            })
        ));
        assert!(storage.application_mut(&id).is_err());
        let drafts = storage.draft_revision_set().unwrap();
        storage
            .preflight_quit_draft_discard(&drafts, false)
            .unwrap();
        storage.discard_all_chat_drafts(&drafts).unwrap();
        assert!(
            storage
                .draft_revision_set()
                .unwrap()
                .iter()
                .all(|entry| { !entry.has_unsubmitted_drafts })
        );
        storage.revoke_known_buddy(&id, 30_002).unwrap();
        assert!(storage.inspect_known_buddy(&id).unwrap().revoked);
        assert!(storage.application_mut(&id).is_err());
        assert!(root.join(BUDDY_REGISTRY_PENDING).exists());
        drop(storage);
        std::fs::remove_dir(current).unwrap();
        std::fs::rename(backup, root.join(BUDDY_REGISTRY_FILE)).unwrap();
        std::fs::remove_file(root.join(BUDDY_REGISTRY_PENDING)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_buddy_creation_candidate_is_bounded_and_blocks_reissue_after_restart() {
        let root = directory();
        let initial = load_buddy_registry_at(&root).unwrap();
        let _active = initial.active_buddy_id.unwrap();
        let candidate_id = "buddy-interrupted-candidate";
        persist_create_candidate(&root, candidate_id).unwrap();
        let candidate_dir = root.join("chat").join("instances").join(candidate_id);
        std::fs::create_dir_all(&candidate_dir).unwrap();
        std::fs::write(candidate_dir.join("synthetic-evidence"), b"prepared").unwrap();
        let marker = root.join(BUDDY_CREATE_CANDIDATE);
        let marker_bytes = std::fs::read(&marker).unwrap();
        let initial_frame = std::fs::read(root.join(BUDDY_REGISTRY_FILE)).unwrap();

        assert!(matches!(
            BuddyStorage::open(&root, 1, 10),
            Err(BuddyStorageError::PendingRegistry)
        ));
        assert!(matches!(
            BuddyStorage::open(&root, 2, 11),
            Err(BuddyStorageError::PendingRegistry)
        ));
        assert_eq!(std::fs::read(&marker).unwrap(), marker_bytes);
        assert_eq!(
            std::fs::read(root.join(BUDDY_REGISTRY_FILE)).unwrap(),
            initial_frame
        );
        assert_eq!(
            std::fs::read(candidate_dir.join("synthetic-evidence")).unwrap(),
            b"prepared"
        );
        assert!(matches!(
            load_buddy_registry_at(&root),
            Err(BuddyStorageError::PendingRegistry)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn buddy_creation_unknown_publish_retains_candidate_and_rejects_retries() {
        let root = directory();
        let mut storage = BuddyStorage::open(&root, 3, 12).unwrap();
        let revision = storage.registry_snapshot().unwrap().revision;
        let registry = root.join(BUDDY_REGISTRY_FILE);
        let backup = root.join("synthetic-registry-before-create");
        std::fs::rename(&registry, &backup).unwrap();
        std::fs::create_dir(&registry).unwrap();

        let error = storage
            .create_buddy(revision, "candidate", "synthetic persona", 13)
            .unwrap_err();
        assert_eq!(
            error.publication_state(),
            Some(BuddyPublicationState::Unknown)
        );
        assert_eq!(
            error.candidate_durability(),
            Some(if cfg!(windows) {
                BuddyCandidateDurability::FileSyncedDirectorySyncUnavailable
            } else {
                BuddyCandidateDurability::FileAndDirectorySynced
            })
        );
        let candidate_id = std::fs::read_to_string(root.join(BUDDY_CREATE_CANDIDATE)).unwrap();
        assert!(moe_chat_app::BuddyId::new(candidate_id.clone()).is_ok());
        assert!(buddy_history_root(&root.join("chat"), &candidate_id).exists());
        assert!(storage.inspect_known_buddy(&candidate_id).is_err());
        let before_retry = std::fs::read_dir(root.join("chat").join("instances"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        let retry = storage.create_buddy(revision, "retry", "another persona", 14);
        assert!(matches!(
            retry,
            Err(BuddyStorageError::RegistryCommit { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(root.join(BUDDY_CREATE_CANDIDATE)).unwrap(),
            candidate_id
        );
        assert_eq!(
            std::fs::read_dir(root.join("chat").join("instances"))
                .map(|entries| entries.count())
                .unwrap_or(0),
            before_retry
        );
        drop(storage);
        assert!(matches!(
            BuddyStorage::open(&root, 4, 15),
            Err(BuddyStorageError::PendingRegistry)
        ));
        assert!(root.join(BUDDY_REGISTRY_PENDING).exists());
        assert!(root.join(BUDDY_CREATE_CANDIDATE).exists());
        std::fs::remove_dir(registry).unwrap();
        std::fs::rename(backup, root.join(BUDDY_REGISTRY_FILE)).unwrap();
        std::fs::remove_file(root.join(BUDDY_REGISTRY_PENDING)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn candidate_write_and_sync_failures_stop_before_history_preparation() {
        for stage in [
            BuddyRegistryIoStage::WriteCandidate,
            BuddyRegistryIoStage::SyncCandidate,
            BuddyRegistryIoStage::SyncCandidateDirectory,
        ] {
            let root = directory();
            let mut storage = BuddyStorage::open(&root, 30, 12).unwrap();
            let revision = storage.registry_snapshot().unwrap().revision;
            let instances = root.join("chat/instances");
            let existing_instances = std::fs::read_dir(&instances)
                .map(|entries| entries.count())
                .unwrap_or(0);
            storage.candidate_fault = Some(stage);

            let error = storage
                .create_buddy(revision, "candidate", "synthetic persona", 13)
                .unwrap_err();
            assert!(matches!(
                error,
                BuddyStorageError::Io { stage: actual, .. } if actual == stage
            ));
            assert_eq!(
                std::fs::read_dir(&instances)
                    .map(|entries| entries.count())
                    .unwrap_or(0),
                existing_instances,
                "candidate failure must stop before opening a history root"
            );
            assert!(root.join(BUDDY_CREATE_CANDIDATE).exists());
            assert_eq!(storage.registry_snapshot().unwrap().revision, revision);
            drop(storage);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn candidate_cleanup_failures_are_reported_after_publication() {
        for stage in [
            BuddyRegistryIoStage::RemoveCandidate,
            BuddyRegistryIoStage::SyncCandidateCleanupDirectory,
        ] {
            let root = directory();
            let mut storage = BuddyStorage::open(&root, 31, 12).unwrap();
            let revision = storage.registry_snapshot().unwrap().revision;
            storage.candidate_cleanup_fault = Some(stage);

            let error = storage
                .create_buddy(revision, "candidate", "synthetic persona", 13)
                .unwrap_err();
            assert_eq!(
                error.publication_state(),
                Some(BuddyPublicationState::Published)
            );
            assert_eq!(
                error.candidate_durability(),
                Some(if cfg!(windows) {
                    BuddyCandidateDurability::FileSyncedDirectorySyncUnavailable
                } else {
                    BuddyCandidateDurability::FileAndDirectorySynced
                })
            );
            assert!(matches!(
                &error,
                BuddyStorageError::CandidateCommit { source, .. }
                    if matches!(source.as_ref(), BuddyStorageError::RegistryCommit {
                        disposition: BuddyPublicationState::Published,
                        source,
                    } if matches!(source.as_ref(), BuddyStorageError::Io { stage: actual, .. } if *actual == stage))
            ));
            assert_eq!(storage.registry_snapshot().unwrap().revision, revision + 1);
            assert_eq!(
                root.join(BUDDY_CREATE_CANDIDATE).exists(),
                stage == BuddyRegistryIoStage::RemoveCandidate
            );
            drop(storage);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn prepared_buddy_candidate_survives_each_registry_publish_stage() {
        for (index, stage, expected_state, leaves_pending) in [
            (
                0,
                BuddyRegistryIoStage::CreatePending,
                BuddyPublicationState::NotPublished,
                false,
            ),
            (
                1,
                BuddyRegistryIoStage::WritePending,
                BuddyPublicationState::NotPublished,
                true,
            ),
            (
                2,
                BuddyRegistryIoStage::SyncPending,
                BuddyPublicationState::NotPublished,
                true,
            ),
            (
                3,
                BuddyRegistryIoStage::Publish,
                BuddyPublicationState::Unknown,
                true,
            ),
            (
                4,
                BuddyRegistryIoStage::SyncDirectory,
                BuddyPublicationState::Published,
                false,
            ),
        ] {
            let root = directory();
            let mut storage = BuddyStorage::open(&root, 5, 20).unwrap();
            let revision = storage.registry_snapshot().unwrap().revision;
            let candidate_text = format!("buddy-prepared-{index}");
            let candidate_id = moe_chat_app::BuddyId::new(&candidate_text).unwrap();
            let mut candidate_registry = storage.registry.clone();
            candidate_registry
                .create(
                    revision,
                    candidate_id.clone(),
                    "candidate",
                    "synthetic persona",
                )
                .unwrap();
            let profile = candidate_registry.profile(&candidate_id).unwrap().clone();
            let generation = storage.allocate_generation().unwrap();
            persist_create_candidate(&root, candidate_id.as_str()).unwrap();
            let mut app =
                open_shared_profile_at(&root, generation, 21, &profile, storage.memory.clone())
                    .unwrap();
            app.dispatch(
                moe_chat_app::Action::Open(moe_chat_app::PanelKind::Chat),
                21,
            )
            .unwrap();
            let evidence = buddy_history_root(&root.join("chat"), candidate_id.as_str());
            assert!(evidence.exists());
            storage.registry_fault = Some(stage);

            let error = storage
                .finish_prepared_commit(
                    candidate_registry,
                    candidate_id.as_str().to_owned(),
                    app,
                    BuddyCandidateDurability::FileAndDirectorySynced,
                )
                .unwrap_err();
            assert_eq!(error.publication_state(), Some(expected_state));
            assert_eq!(root.join(BUDDY_REGISTRY_PENDING).exists(), leaves_pending);
            assert!(root.join(BUDDY_CREATE_CANDIDATE).exists());
            assert!(evidence.exists());
            match expected_state {
                BuddyPublicationState::Published => {
                    assert_eq!(storage.registry_snapshot().unwrap().revision, revision + 1)
                }
                BuddyPublicationState::Unknown => assert!(matches!(
                    storage.registry_snapshot(),
                    Err(BuddyStorageError::RegistryCommit {
                        disposition: BuddyPublicationState::Unknown,
                        ..
                    })
                )),
                BuddyPublicationState::NotPublished => {
                    assert_eq!(storage.registry_snapshot().unwrap().revision, revision)
                }
            }
            assert!(
                storage
                    .create_buddy(revision, "retry", "another persona", 22)
                    .is_err()
            );
            drop(storage);
            assert!(matches!(
                BuddyStorage::open(&root, 6, 23),
                Err(BuddyStorageError::PendingRegistry)
            ));
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn persisted_binding_that_cannot_apply_is_reported_published_and_quarantines_app() {
        let root = directory();
        let mut storage = BuddyStorage::open(&root, 850, 40_000).unwrap();
        let registry = storage.registry_snapshot().unwrap();
        let id = registry.active_buddy_id.unwrap();
        let profile = storage.buddy_profile(&id).unwrap();
        let identity = PetIdentity::new(Namespace::new("moe-chat", "local").unwrap(), &id).unwrap();
        let source = storage
            .memory
            .borrow_mut()
            .0
            .register_source(
                OperationId::new("daily-source-persona").unwrap(),
                MemoryScope::Private(identity.clone()),
                "persona",
                1,
                Epistemic::AuthoritativeState,
            )
            .unwrap()
            .value()
            .clone();
        storage
            .memory
            .borrow_mut()
            .0
            .set_identity_view(&identity, 99, "synthetic newer memory view", vec![source])
            .unwrap();

        let model = moe_chat_app::BuddyModelConfiguration::new(
            "http://127.0.0.1:9/v1/chat/completions",
            "synthetic-model",
            "synthetic-preset",
        )
        .unwrap();
        let error = storage
            .commit_configuration(
                &id,
                registry.revision,
                profile.profile_revision(),
                "new persisted persona",
                model,
                40_001,
            )
            .unwrap_err();
        assert_eq!(
            error.publication_state(),
            Some(BuddyPublicationState::Published)
        );
        assert_eq!(
            storage.registry_snapshot().unwrap().revision,
            registry.revision + 1
        );
        assert_eq!(
            storage.buddy_profile(&id).unwrap().persona(),
            "new persisted persona"
        );
        let app_error = storage
            .application_mut(&id)
            .unwrap()
            .dispatch(Action::SetDraft("must not run".into()), 40_002)
            .unwrap_err();
        assert_eq!(app_error.public_code(), "storage_failed");
        assert!(
            !storage
                .application_mut(&id)
                .unwrap()
                .has_unsubmitted_chat_drafts()
        );
        drop(storage);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn logical_session_limit_is_shared_across_buddy_history_roots() {
        let root = directory();
        let initial = load_buddy_registry_at(&root).unwrap();
        let offline_id = initial.active_buddy_id.unwrap();
        let created = create_buddy_at(&root, initial.revision, "second", "persona").unwrap();
        let buddy_id = created.active_buddy_id.unwrap();
        let switched = switch_buddy_at(&root, created.revision, &offline_id).unwrap();
        let mut offline =
            open_application_for_buddy_at(&root, &offline_id, switched.revision, 90, 6_000)
                .unwrap();
        offline
            .dispatch(Action::Open(PanelKind::Chat), 6_000)
            .unwrap();
        for sequence in 0..62_u64 {
            let (expected_session_id, expected_sessions_revision) = session_guard(&offline);
            offline
                .dispatch(
                    Action::NewSession {
                        expected_session_id,
                        expected_sessions_revision,
                    },
                    6_001 + sequence,
                )
                .unwrap();
        }
        assert_eq!(offline.snapshot().sessions.len(), 63);
        drop(offline);

        let selected = switch_buddy_at(&root, switched.revision, &buddy_id).unwrap();
        let mut buddy =
            open_application_for_buddy_at(&root, &buddy_id, selected.revision, 91, 7_000).unwrap();
        buddy
            .dispatch(Action::Open(PanelKind::Chat), 7_000)
            .unwrap();
        assert_eq!(buddy.snapshot().sessions.len(), 1);
        let (expected_session_id, expected_sessions_revision) = session_guard(&buddy);
        let error = buddy
            .dispatch(
                Action::NewSession {
                    expected_session_id,
                    expected_sessions_revision,
                },
                7_001,
            )
            .unwrap_err();
        assert_eq!(error.public_code(), "storage_capacity_exceeded");
        assert_eq!(
            error.execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        assert_eq!(buddy.snapshot().sessions.len(), 1);
        drop(buddy);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detailed_settings_save_preserves_stage_and_io_error_source() {
        let root = directory();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("settings.pending"), b"synthetic pending").unwrap();
        let settings = ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "synthetic persona".into(),
            allow_transmission: false,
        };

        let error = save_settings_detailed_at(&root, &settings).unwrap_err();
        assert_eq!(error.stage(), SettingsSaveStage::PendingRegistry);
        assert_eq!(
            error
                .source()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::Other
        );
        assert_eq!(
            save_settings_at(&root, &settings).unwrap_err(),
            error.to_string(),
            "the legacy String facade should retain its safe display message"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_chat_without_settings_never_recreates_missing_memory() {
        let root = directory();
        let mut app = open_application_at(&root, 190, 10_000).unwrap();
        app.dispatch(Action::Open(PanelKind::Chat), 10_000).unwrap();
        app.dispatch(
            Action::SetDraft("synthetic retained history".into()),
            10_001,
        )
        .unwrap();
        app.dispatch(Action::Send, 10_002).unwrap();
        drop(app);
        assert!(!root.join("settings.json").exists());
        let directory_before = std::fs::read(root.join("chat/sessions.v1")).unwrap();
        std::fs::remove_file(root.join("memory.v1")).unwrap();
        let rejected = open_application_at(&root, 191, 10_003).is_err();
        let recreated = root.join("memory.v1").exists();
        let directory_after = std::fs::read(root.join("chat/sessions.v1")).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            rejected,
            "existing chat history requires its original memory journal"
        );
        assert!(
            !recreated,
            "a missing journal must never be replaced by an empty one"
        );
        assert_eq!(directory_after, directory_before);
    }

    #[test]
    fn legacy_settings_entries_reject_every_onboarding_or_interruption_trace() {
        let settings = ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "legacy fallback must not load".into(),
            allow_transmission: false,
        };
        for trace in ["onboarded", "candidate", "instance", "pending"] {
            let root = directory();
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("settings.json"),
                br#"{"version":1,"endpoint":"http://127.0.0.1:9/v1/chat/completions","model":"legacy","persona":"legacy fallback must not load"}"#,
            ).unwrap();
            match trace {
                "onboarded" => std::fs::write(
                    root.join(BUDDY_ONBOARDING_EVIDENCE),
                    BUDDY_ONBOARDING_EVIDENCE_BYTES,
                )
                .unwrap(),
                "candidate" => {
                    std::fs::write(root.join(BUDDY_CREATE_CANDIDATE), b"buddy-interrupted").unwrap()
                }
                "instance" => {
                    std::fs::create_dir_all(root.join("chat/instances/buddy-old")).unwrap()
                }
                "pending" => {
                    std::fs::write(root.join(BUDDY_REGISTRY_PENDING), b"synthetic pending").unwrap()
                }
                _ => unreachable!(),
            }
            assert!(
                load_settings_at(&root).is_err(),
                "{trace}: legacy load must fail closed"
            );
            let save = save_settings_detailed_at(&root, &settings).unwrap_err();
            assert!(
                matches!(
                    save.stage(),
                    SettingsSaveStage::PendingRegistry | SettingsSaveStage::BuddyRegistryPresent
                ),
                "{trace}: {save}"
            );
            assert!(
                open_application_at(&root, 77, 9_000).is_err(),
                "{trace}: open must fail closed"
            );
            assert!(
                !root.join("memory.v1").exists(),
                "{trace}: must not create an empty memory archive"
            );
            assert!(
                std::fs::read_to_string(root.join("settings.json"))
                    .unwrap()
                    .contains("legacy")
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn detailed_settings_save_identifies_publish_failure_and_retains_source() {
        let root = directory();
        std::fs::create_dir_all(root.join("settings.json")).unwrap();
        let settings = ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "synthetic persona".into(),
            allow_transmission: false,
        };

        let error = save_settings_detailed_at(&root, &settings).unwrap_err();
        assert_eq!(error.stage(), SettingsSaveStage::Publish);
        assert!(error.source().is_some());
        assert!(root.join("settings.pending").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispatched_disk_memory_revision_conflicts_do_not_append_and_remain_editable() {
        let root = directory();
        let mut app = open_memory_application(&root, 60, 1000);
        app.dispatch(Action::Remember("synthetic first note".into()), 1002)
            .unwrap();
        app.dispatch(Action::Remember("synthetic second note".into()), 1003)
            .unwrap();

        let initial = app.snapshot().memories;
        assert_eq!(initial.len(), 2);
        let first_id = initial[0].id.clone();
        let second_id = initial[1].id.clone();
        app.dispatch(
            Action::UpdateMemory {
                id: first_id.clone(),
                revision: 1,
                text: "synthetic first note revised".into(),
            },
            1004,
        )
        .unwrap();
        app.dispatch(
            Action::UpdateMemory {
                id: second_id.clone(),
                revision: 1,
                text: "synthetic second note revised".into(),
            },
            1005,
        )
        .unwrap();

        let journal = root.join("memory.v1");
        drop(app);
        let committed_bytes = std::fs::read(&journal).unwrap();
        let mut app = open_memory_application(&root, 61, 1006);
        let update_error = app
            .dispatch(
                Action::UpdateMemory {
                    id: first_id.clone(),
                    revision: 1,
                    text: "synthetic stale update".into(),
                },
                1007,
            )
            .expect_err("stale update must be rejected");
        assert_eq!(update_error.public_code(), "memory_revision_conflict");
        assert_eq!(
            update_error.execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        assert_persistence_revision_conflict(&update_error, 1, 2);
        assert_eq!(memory_revision(&app.snapshot().memories, &first_id), 2);
        drop(app);
        assert_eq!(std::fs::read(&journal).unwrap(), committed_bytes);

        let mut app = open_memory_application(&root, 62, 1008);
        let delete_error = app
            .dispatch(
                Action::DeleteMemory {
                    id: second_id.clone(),
                    revision: 1,
                },
                1009,
            )
            .expect_err("stale delete must be rejected");
        assert_eq!(delete_error.public_code(), "memory_revision_conflict");
        assert_eq!(
            delete_error.execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        assert_persistence_revision_conflict(&delete_error, 1, 2);
        assert_eq!(memory_revision(&app.snapshot().memories, &second_id), 2);
        drop(app);
        assert_eq!(std::fs::read(&journal).unwrap(), committed_bytes);

        let mut app = open_memory_application(&root, 63, 1010);
        app.dispatch(
            Action::UpdateMemory {
                id: first_id.clone(),
                revision: 2,
                text: "synthetic edit after conflict".into(),
            },
            1011,
        )
        .expect("the memory remains editable after a definite conflict");
        assert_eq!(memory_revision(&app.snapshot().memories, &first_id), 3);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn indeterminate_disk_memory_errors_remain_unknown() {
        for error in [
            PersistenceError::RecoveryRequired,
            PersistenceError::Io(std::io::Error::other("synthetic write failure")),
        ] {
            let mapped = map_memory_change_error(error);
            assert_eq!(mapped.public_code(), "storage_failed");
            assert_eq!(
                mapped.execution_disposition(),
                ExecutionDisposition::Unknown
            );
            assert!(mapped.source().is_some());
        }
    }

    #[test]
    fn local_memory_capacity_before_append_is_typed_not_executed_with_source() {
        let error = map_memory_change_error(PersistenceError::CapacityExceeded);
        assert_eq!(error.public_code(), "memory_capacity_exceeded");
        assert_eq!(
            error.execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        assert!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<PersistenceError>())
                .is_some_and(|source| matches!(source, PersistenceError::CapacityExceeded))
        );
    }

    fn memory_revision(memories: &[moe_chat_app::MemorySnapshot], id: &str) -> u64 {
        memories
            .iter()
            .find(|memory| memory.id == id)
            .expect("memory remains in the displayed list")
            .revision
    }

    fn open_memory_application(
        root: &std::path::Path,
        generation: u64,
        now: u64,
    ) -> moe_chat_app::ChatApplication {
        let mut app = open_application_at(root, generation, now).unwrap();
        app.dispatch(Action::Open(PanelKind::Memory), now).unwrap();
        app.dispatch(Action::RefreshMemories, now + 1).unwrap();
        app
    }

    fn open_chat_application(
        root: &std::path::Path,
        generation: u64,
        now: u64,
    ) -> moe_chat_app::ChatApplication {
        let mut app = open_at(root, generation, now).unwrap();
        app.dispatch(Action::Open(PanelKind::Chat), now).unwrap();
        app
    }

    fn open_chat_application_with_retention(
        root: &std::path::Path,
        generation: u64,
        now: u64,
        normal_ms: u64,
        recycle_ms: u64,
    ) -> moe_chat_app::ChatApplication {
        let retention = RawRetention::new(normal_ms, recycle_ms, 8, 256, 65_536).unwrap();
        let mut app = open_at_with_retention(root, generation, now, retention).unwrap();
        app.dispatch(Action::Open(PanelKind::Chat), now).unwrap();
        app
    }

    fn marked_retirement_fixture(
        root: &std::path::Path,
        start: u64,
        generation: u64,
    ) -> (String, u64) {
        use super::history::{self, FailurePoint};

        let final_at = start + 37 * 86_400_000 + 20;
        let mut app = open_chat_application(root, generation, start);
        send_offline(&mut app, "synthetic expired body", start + 1);
        let (session_id, revision) = session_guard(&app);
        app.dispatch(
            Action::NewSession {
                expected_session_id: session_id,
                expected_sessions_revision: revision,
            },
            start + 3,
        )
        .unwrap();
        let directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("chat").join("sessions.v1")).unwrap())
                .unwrap();
        let segment_id = directory["sessions"][0]["segments"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        history::inject_failure(FailurePoint::RetireAfterMarker);
        assert!(app.maintain_retention(final_at).is_err());
        history::clear_failure();
        drop(app);
        (segment_id, final_at)
    }

    // A legacy desktop archive includes its original memory journal even when
    // this fixture only populates raw chat history. Missing-memory rejection is
    // tested separately; recovery must not create a replacement for this file.
    fn create_legacy_memory_fixture(chat_root: &std::path::Path) {
        let memory_path = chat_root.parent().unwrap().join("memory.v1");
        if !memory_path.exists() {
            std::fs::create_dir_all(memory_path.parent().unwrap()).unwrap();
            drop(LocalMemory::create(&memory_path).unwrap());
        }
    }

    fn create_legacy_raw_baseline(chat_root: &std::path::Path) {
        let retention = RawRetention::new(100, 20, 8, 256, 65_536).unwrap();
        create_legacy_raw_fixture(chat_root, 1_000, retention, None);
    }

    fn create_legacy_unresolved_raw(
        chat_root: &std::path::Path,
        created_at: u64,
        retention: RawRetention,
        text: &str,
    ) {
        create_legacy_raw_fixture(chat_root, created_at, retention, Some(text));
    }

    fn create_legacy_raw_fixture(
        chat_root: &std::path::Path,
        now: u64,
        retention: RawRetention,
        text: Option<&str>,
    ) {
        create_legacy_memory_fixture(chat_root);
        let identity = PetIdentity::new(
            Namespace::new("moe-chat", "local").unwrap(),
            "offline-companion",
        )
        .unwrap();
        let mut ingress = TrustedIngress::new();
        ingress.register_route("buddy/offline", identity).unwrap();
        let credential = ingress
            .install_credential(
                IngressToken::new("offline-acceptance-token-not-for-production").unwrap(),
                CallerSubject::new("local-trusted-host").unwrap(),
                [String::from("buddy/offline")],
                ChannelId::new("local-chat").unwrap(),
                ChannelScope::new("private-window").unwrap(),
                ReplyTarget::new("chat-window").unwrap(),
            )
            .unwrap();
        ingress
            .bind_stable_credential(credential, "daily-desktop-owner")
            .unwrap();
        let request = ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new("offline-acceptance-token-not-for-production").unwrap(),
                    "buddy/offline",
                    text.unwrap_or("legacy baseline"),
                )
                .with_session_hint("offline-acceptance-session"),
            )
            .unwrap();
        let mut runtime = SessionRuntime::with_retention(
            &ingress,
            SessionLimits::new(8, 64, 256, 8_192, 65_536, 2).unwrap(),
            SessionTiming::default(),
            retention,
            now,
        );
        if text.is_some() {
            runtime
                .register_user(
                    SessionAccess::new(&ingress, &request, now),
                    UserEvent::new(UserEventKind::New),
                )
                .unwrap();
        }
        let domain = RawDomainKey::from_request(&ingress, &request).unwrap();
        let grant = runtime
            .install_raw_history_grant(&ingress, domain.clone())
            .unwrap();
        let access = moe_raw_local::RawLocalAccess::new(
            &mut runtime,
            &ingress,
            &request,
            &grant,
            domain,
            now,
        )
        .unwrap();
        drop(
            moe_raw_local::RawLocalStore::open(
                chat_root,
                moe_raw_local::OpenMode::CreateNew,
                access,
            )
            .unwrap(),
        );
    }

    fn create_legacy_with_older_unsettled_interaction(chat_root: &std::path::Path) {
        create_legacy_memory_fixture(chat_root);
        let identity = PetIdentity::new(
            Namespace::new("moe-chat", "local").unwrap(),
            "offline-companion",
        )
        .unwrap();
        let mut ingress = TrustedIngress::new();
        ingress.register_route("buddy/offline", identity).unwrap();
        let credential = ingress
            .install_credential(
                IngressToken::new("offline-acceptance-token-not-for-production").unwrap(),
                CallerSubject::new("local-trusted-host").unwrap(),
                [String::from("buddy/offline")],
                ChannelId::new("local-chat").unwrap(),
                ChannelScope::new("private-window").unwrap(),
                ReplyTarget::new("chat-window").unwrap(),
            )
            .unwrap();
        ingress
            .bind_stable_credential(credential, "daily-desktop-owner")
            .unwrap();
        let retention = RawRetention::new(10_000, 10_000, 8, 256, 65_536).unwrap();
        let mut runtime = SessionRuntime::with_retention(
            &ingress,
            SessionLimits::new(8, 256, 256, 8_192, 65_536, 2).unwrap(),
            SessionTiming::default(),
            retention,
            1_000,
        );
        let first = ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new("offline-acceptance-token-not-for-production").unwrap(),
                    "buddy/offline",
                    "synthetic earliest unresolved interaction",
                )
                .with_session_hint("offline-acceptance-session"),
            )
            .unwrap();
        runtime
            .register_user(
                SessionAccess::new(&ingress, &first, 1_000),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        for index in 0..64_u64 {
            let now_ms = 1_001 + index;
            let request = ingress
                .admit(
                    RawIngressCall::new(
                        IngressToken::new("offline-acceptance-token-not-for-production").unwrap(),
                        "buddy/offline",
                        format!("synthetic completed interaction {index}"),
                    )
                    .with_session_hint("offline-acceptance-session"),
                )
                .unwrap();
            let registration = runtime
                .register_user(
                    SessionAccess::new(&ingress, &request, now_ms),
                    UserEvent::new(UserEventKind::New),
                )
                .unwrap();
            runtime
                .finish_interaction(
                    SessionAccess::new(&ingress, &request, now_ms + 1),
                    &registration.interaction,
                    moe_agent_runtime::TerminalState::Completed,
                    Some("synthetic completed reply"),
                )
                .unwrap();
        }
        let domain = RawDomainKey::from_request(&ingress, &first).unwrap();
        let grant = runtime
            .install_raw_history_grant(&ingress, domain.clone())
            .unwrap();
        let access = moe_raw_local::RawLocalAccess::new(
            &mut runtime,
            &ingress,
            &first,
            &grant,
            domain,
            1_066,
        )
        .unwrap();
        drop(
            moe_raw_local::RawLocalStore::open(
                chat_root,
                moe_raw_local::OpenMode::CreateNew,
                access,
            )
            .unwrap(),
        );
    }

    fn send_offline(app: &mut moe_chat_app::ChatApplication, text: &str, now: u64) {
        app.dispatch(Action::SetDraft(text.to_owned()), now)
            .unwrap();
        app.dispatch(Action::Send, now + 1).unwrap();
    }

    fn session_guard(app: &moe_chat_app::ChatApplication) -> (String, u64) {
        let snapshot = app.snapshot();
        (
            snapshot.active_session_id.unwrap(),
            snapshot.sessions_revision,
        )
    }

    fn assert_persistence_revision_conflict(error: &AppError, expected: u64, actual: u64) {
        let mut source = error.source();
        while let Some(cause) = source {
            if let Some(PersistenceError::Memory(MemoryError::RevisionConflict {
                expected: found_expected,
                actual: found_actual,
            })) = cause.downcast_ref::<PersistenceError>()
            {
                assert_eq!((*found_expected, *found_actual), (expected, actual));
                return;
            }
            source = cause.source();
        }
        panic!("full PersistenceError::Memory(RevisionConflict) source chain was lost");
    }

    #[test]
    fn journal_and_raw_history_reopen_without_replaying_requests() {
        let root = directory();
        {
            let mut app = open_at(&root, 51, 1000).unwrap();
            app.dispatch(Action::Open(PanelKind::Chat), 1000).unwrap();
            app.dispatch(Action::Open(PanelKind::Memory), 1000).unwrap();
            app.dispatch(Action::Remember("synthetic blue tea".into()), 1001)
                .unwrap();
            app.dispatch(Action::SetDraft("synthetic hello".into()), 1002)
                .unwrap();
            app.dispatch(Action::Send, 1003).unwrap();
        }
        {
            let mut app = open_at(&root, 52, 2000).unwrap();
            app.dispatch(Action::Open(PanelKind::Chat), 2000).unwrap();
            app.dispatch(Action::Open(PanelKind::Memory), 2000).unwrap();
            app.dispatch(Action::RefreshMemories, 2000).unwrap();
            let snapshot = app.snapshot();
            assert_eq!(snapshot.chat.messages.len(), 2);
            assert_eq!(snapshot.memories.len(), 1);
            let note = &snapshot.memories[0];
            app.dispatch(
                Action::UpdateMemory {
                    id: note.id.clone(),
                    revision: note.revision,
                    text: "synthetic green tea".into(),
                },
                2001,
            )
            .unwrap();
            app.configure_model(&ModelSettings {
                endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
                model: "synthetic".into(),
                persona: "synthetic persona".into(),
                allow_transmission: true,
            })
            .unwrap();
            app.dispatch(Action::SetDraft("pending synthetic request".into()), 2002)
                .unwrap();
            app.dispatch(Action::Send, 2003).unwrap();
            let request = app.take_external_request().unwrap();
            let (input, _ticket) = request.into_parts();
            assert!(input.memory().iter().any(|text| text.contains("green tea")));
        }
        {
            let mut app = open_at(&root, 53, 3000).unwrap();
            app.dispatch(Action::Open(PanelKind::Chat), 3000).unwrap();
            assert!(app.take_external_request().is_none());
            assert_eq!(app.snapshot().chat.status, ChatStatus::Unknown);
            app.dispatch(Action::SetDraft("must not replay".into()), 3001)
                .unwrap();
            assert!(app.dispatch(Action::Send, 3002).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_lifecycle_archive_and_session_history_survive_restart() {
        let root = directory();
        {
            let mut app = open_chat_application(&root, 71, 1_000);
            send_offline(&mut app, "synthetic session one", 1_001);
            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::NewSession {
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                1_010,
            )
            .unwrap();
            send_offline(&mut app, "synthetic archived session", 1_011);
            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::ArchiveSession {
                    id: current_id.clone(),
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                1_020,
            )
            .unwrap();
            assert!(
                app.snapshot()
                    .sessions
                    .iter()
                    .find(|session| session.id == "session-2")
                    .unwrap()
                    .archived
            );
        }
        {
            let mut app = open_chat_application(&root, 72, 2_000);
            let reopened = app.snapshot();
            assert_eq!(reopened.active_session_id.as_deref(), Some("session-2"));
            assert!(
                reopened
                    .sessions
                    .iter()
                    .find(|s| s.id == "session-2")
                    .unwrap()
                    .archived
            );
            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::SwitchSession {
                    id: String::from("session-1"),
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                2_010,
            )
            .unwrap();
            let history = app.snapshot().chat.messages;
            assert!(
                history
                    .iter()
                    .any(|message| message.text == "synthetic session one")
            );
            assert!(
                !history
                    .iter()
                    .any(|message| message.text == "synthetic archived session")
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archived_restarts_do_not_allocate_new_writable_segments() {
        let root = directory();
        {
            let mut app = open_chat_application(&root, 73, 1_000);
            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::ArchiveSession {
                    id: current_id.clone(),
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                1_001,
            )
            .unwrap();
        }
        let mut segment_count = None;
        let mut directory_revision = None;
        for generation in 74..76 {
            let app = open_chat_application(&root, generation, 1_100 + generation);
            assert!(app.snapshot().sessions[0].archived);
            let directory: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join("chat").join("sessions.v1")).unwrap(),
            )
            .unwrap();
            let count = directory["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|session| session["segments"].as_array().unwrap().len())
                .sum::<usize>();
            if let Some(previous) = segment_count {
                assert_eq!(count, previous);
                assert_eq!(directory["revision"].as_u64(), directory_revision);
            }
            segment_count = Some(count);
            directory_revision = directory["revision"].as_u64();
            drop(app);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_raw_local_v1_is_managed_under_its_original_authorization_hint() {
        let root = directory();
        let chat_root = root.join("chat");
        create_legacy_raw_baseline(&chat_root);
        let legacy_path = chat_root.join("raw-local.v1");
        let legacy_before = std::fs::read(&legacy_path).unwrap();
        {
            let mut app = open_chat_application(&root, 75, 1_001);
            assert_eq!(
                app.snapshot().active_session_id.as_deref(),
                Some("session-1")
            );
            send_offline(&mut app, "synthetic after legacy migration", 1_002);
            assert!(std::fs::read(&legacy_path).unwrap() == legacy_before);
        }
        let directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(chat_root.join("sessions.v1")).unwrap()).unwrap();
        assert_eq!(
            directory["sessions"][0]["rawHint"],
            "offline-acceptance-session"
        );
        assert_eq!(directory["sessions"][0]["segments"][0]["path"], ".");
        {
            let app = open_chat_application(&root, 76, 2_000);
            assert!(
                app.snapshot()
                    .chat
                    .messages
                    .iter()
                    .any(|message| message.text == "synthetic after legacy migration")
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_single_writer_lock_and_pending_recovery_fail_closed() {
        let root = directory();
        let chat_root = root.join("chat");
        let mut first = super::history::DiskHistory::new(chat_root.clone());
        first.open_directory().unwrap();
        let mut second = super::history::DiskHistory::new(chat_root.clone());
        let concurrent = match second.open_directory() {
            Err(error) => error,
            Ok(_) => panic!("a second session directory writer acquired the lock"),
        };
        assert_eq!(concurrent.public_code(), "storage_failed");
        drop(first);

        let pending = chat_root.join("sessions.pending");
        std::fs::write(&pending, b"synthetic pending directory publication").unwrap();
        let preserved = std::fs::read(&pending).unwrap();
        assert!(open_at(&root, 79, 1_002).is_err());
        assert_eq!(std::fs::read(&pending).unwrap(), preserved);

        std::fs::remove_file(&pending).unwrap();
        let directory_path = chat_root.join("sessions.v1");
        std::fs::write(&directory_path, b"{synthetic damaged directory").unwrap();
        let damaged = std::fs::read(&directory_path).unwrap();
        assert!(open_at(&root, 80, 1_003).is_err());
        assert_eq!(std::fs::read(&directory_path).unwrap(), damaged);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_root_peak_budget_rejects_before_admitting_or_losing_text() {
        let root = directory();
        drop(open_chat_application(&root, 81, 1_000));
        let padding = root.join("chat").join("capacity-padding.bin");
        let file = std::fs::File::create(&padding).unwrap();
        file.set_len(16 * 1024 * 1024 - 8_192).unwrap();
        drop(file);

        let mut app = open_chat_application(&root, 82, 2_000);
        app.configure_model(&ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "synthetic persona".into(),
            allow_transmission: true,
        })
        .unwrap();
        app.dispatch(Action::SetDraft(String::from("x")), 2_001)
            .unwrap();
        let error = app.dispatch(Action::Send, 2_002).unwrap_err();
        assert_eq!(error.public_code(), "storage_capacity_exceeded");
        assert_eq!(
            error.execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        assert_eq!(app.snapshot().chat.draft, "x");
        assert!(app.take_external_request().is_none());
        drop(app);
        assert!(
            padding.exists(),
            "capacity backpressure retains all old content"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rotation_failures_preserve_or_expose_the_exact_pending_boundary() {
        use super::history::{self, FailurePoint};

        for (index, point) in [
            FailurePoint::Seal,
            FailurePoint::AfterSeal,
            FailurePoint::SegmentBeforeMarker,
            FailurePoint::SegmentAfterMarker,
            FailurePoint::PublishCreate,
            FailurePoint::PublishWrite,
            FailurePoint::PublishSync,
            FailurePoint::PublishRename,
            FailurePoint::PublishDirectorySync,
        ]
        .into_iter()
        .enumerate()
        {
            let root = directory();
            let mut app = open_chat_application(&root, 140 + index as u64, 1_000);
            send_offline(&mut app, "synthetic failure boundary", 1_001);
            let (current_id, revision) = session_guard(&app);
            history::inject_failure(point);
            let error = app
                .dispatch(
                    Action::NewSession {
                        expected_session_id: current_id.clone(),
                        expected_sessions_revision: revision,
                    },
                    1_010,
                )
                .unwrap_err();
            history::clear_failure();
            assert_eq!(error.execution_disposition(), ExecutionDisposition::Unknown);
            assert_eq!(
                app.snapshot().active_session_id.as_deref(),
                Some(current_id.as_str())
            );
            drop(app);

            if point == FailurePoint::PublishDirectorySync {
                let recovered = open_chat_application(&root, 200 + index as u64, 2_000);
                assert_eq!(
                    recovered.snapshot().active_session_id.as_deref(),
                    Some("session-2")
                );
                drop(recovered);
            } else if matches!(
                point,
                FailurePoint::SegmentAfterMarker | FailurePoint::PublishCreate
            ) {
                assert!(open_at(&root, 200 + index as u64, 2_000).is_err());
                assert!(root.join("chat").join("segments").exists());
            } else if matches!(
                point,
                FailurePoint::PublishWrite
                    | FailurePoint::PublishSync
                    | FailurePoint::PublishRename
            ) {
                assert!(root.join("chat").join("sessions.pending").exists());
                assert!(open_at(&root, 200 + index as u64, 2_000).is_err());
            } else {
                let recovered = open_chat_application(&root, 200 + index as u64, 2_000);
                assert_eq!(
                    recovered.snapshot().active_session_id.as_deref(),
                    Some("session-1")
                );
                drop(recovered);
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn sixteen_reopens_load_bounded_read_only_history_and_continue() {
        let root = directory();
        for run in 0..16_u64 {
            let now = 10_000 + run * 100;
            let mut app = open_chat_application(&root, 80 + run, now);
            assert_eq!(app.snapshot().chat.messages.len(), run as usize * 2);
            send_offline(&mut app, &format!("synthetic reopen {run}"), now + 1);
            assert!(app.snapshot().chat.messages.len() <= 128);
        }
        let app = open_chat_application(&root, 99, 12_000);
        let snapshot = app.snapshot();
        assert_eq!(snapshot.chat.messages.len(), 32);
        assert!(
            snapshot
                .chat
                .messages
                .iter()
                .any(|message| { message.text == "synthetic reopen 15" })
        );
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zero_message_history_page_does_not_open_or_read_raw_segment_bodies() {
        let root = directory();
        {
            let mut app = open_chat_application(&root, 149, 70_000);
            send_offline(
                &mut app,
                "synthetic body intentionally corrupted later",
                70_001,
            );
        }
        let chat_root = root.join("chat");
        let directory_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(chat_root.join("sessions.v1")).unwrap()).unwrap();
        let segment_path = chat_root.join(
            directory_json["sessions"][0]["segments"][0]["path"]
                .as_str()
                .unwrap(),
        );
        let mut history = super::history::DiskHistory::new(chat_root);
        history.open_directory().unwrap();
        std::fs::write(segment_path.join("raw-local.v1"), b"intentionally unread").unwrap();

        let identity = PetIdentity::new(
            Namespace::new("moe-chat", "local").unwrap(),
            "offline-companion",
        )
        .unwrap();
        let mut ingress = TrustedIngress::new();
        ingress.register_route("buddy/offline", identity).unwrap();
        let credential = ingress
            .install_credential(
                IngressToken::new("offline-acceptance-token-not-for-production").unwrap(),
                CallerSubject::new("local-trusted-host").unwrap(),
                [String::from("buddy/offline")],
                ChannelId::new("local-chat").unwrap(),
                ChannelScope::new("private-window").unwrap(),
                ReplyTarget::new("chat-window").unwrap(),
            )
            .unwrap();
        ingress
            .bind_stable_credential(credential, "daily-desktop-owner")
            .unwrap();
        let request = ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new("offline-acceptance-token-not-for-production").unwrap(),
                    "buddy/offline",
                    "history page authorization",
                )
                .with_session_hint("session-1"),
            )
            .unwrap();
        let retention = RawRetention::new(30 * 86_400_000, 7 * 86_400_000, 8, 256, 65_536).unwrap();
        let mut runtime = SessionRuntime::with_retention(
            &ingress,
            SessionLimits::new(8, 64, 256, 8_192, 65_536, 2).unwrap(),
            SessionTiming::default(),
            retention,
            70_010,
        );
        let domain = RawDomainKey::from_request(&ingress, &request).unwrap();
        let grant = runtime.install_raw_history_grant(&ingress, domain).unwrap();
        let messages = history
            .load_history(
                moe_chat_app::HistoryAccess {
                    runtime: &mut runtime,
                    ingress: &ingress,
                    request: &request,
                    grant: &grant,
                    retention: &retention,
                    now_ms: 70_010,
                    logical_session_id: "session-1",
                    status: ChatStatus::Ready,
                    online: false,
                    external_request_pending: false,
                    external_unknown: false,
                },
                "session-1",
                0,
            )
            .unwrap();
        assert!(messages.is_empty());
        drop(history);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restored_external_unknown_allows_only_a_new_isolated_persistent_session() {
        let root = directory();
        let (old_ticket, old_session) = {
            let mut app = open_chat_application(&root, 101, 20_000);
            app.configure_model(&ModelSettings {
                endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
                model: "synthetic".into(),
                persona: "synthetic persona".into(),
                allow_transmission: true,
            })
            .unwrap();
            app.dispatch(
                Action::SetDraft("synthetic unresolved request".into()),
                20_001,
            )
            .unwrap();
            app.dispatch(Action::Send, 20_002).unwrap();
            let receipts = app.history_commit_receipts();
            assert_eq!(receipts.raw.len(), 1);
            assert!(receipts.raw[0].file_synced);
            assert!(
                receipts
                    .directory
                    .is_some_and(|receipt| receipt.file_synced)
            );
            let request = app.take_external_request().unwrap();
            let (_, ticket) = request.into_parts();
            (ticket, app.snapshot().active_session_id.unwrap())
        };

        let mut app = open_chat_application(&root, 102, 21_000);
        let restored = app.snapshot();
        assert_eq!(restored.chat.status, ChatStatus::Unknown);
        assert!(restored.session_isolation_allowed);
        let revision = restored.sessions_revision;
        app.dispatch(
            Action::NewSession {
                expected_session_id: old_session,
                expected_sessions_revision: revision,
            },
            21_001,
        )
        .unwrap();
        let receipts = app.history_commit_receipts();
        assert_eq!(
            receipts.raw.len(),
            2,
            "old seal and new baseline are distinct commits"
        );
        assert_eq!(
            receipts.directory.map(|receipt| receipt.revision),
            Some(app.snapshot().sessions_revision)
        );
        assert!(!app.snapshot().session_isolation_allowed);
        assert_eq!(
            app.snapshot()
                .sessions
                .iter()
                .find(|session| session.id == "session-1")
                .unwrap()
                .status,
            ChatStatus::Unknown
        );

        app.configure_model(&ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "synthetic persona".into(),
            allow_transmission: true,
        })
        .unwrap();
        app.dispatch(
            Action::SetDraft("synthetic isolated request".into()),
            21_002,
        )
        .unwrap();
        app.dispatch(Action::Send, 21_003).unwrap();
        let (_, current_ticket) = app.take_external_request().unwrap().into_parts();
        let stale = app
            .complete_external(
                old_ticket,
                moe_agent_runtime::ModelOutcome::Text {
                    text: "must be rejected".into(),
                    presentation: vec![moe_agent_runtime::PresentationEvent::Neutral],
                },
                21_004,
            )
            .unwrap_err();
        assert_eq!(stale.public_code(), "stale_external_ticket");
        assert_eq!(app.snapshot().chat.status, ChatStatus::Sending);
        app.complete_external(
            current_ticket,
            moe_agent_runtime::ModelOutcome::Text {
                text: "synthetic current reply".into(),
                presentation: vec![moe_agent_runtime::PresentationEvent::Neutral],
            },
            21_005,
        )
        .unwrap();
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovered_legacy_unsettled_raw_allows_isolation_without_replaying_old_ticket() {
        let root = directory();
        let chat_root = root.join("chat");
        let old_ticket = {
            let mut app = open_at(&root, 151, 30_000)
                .unwrap_or_else(|error| panic!("initial archive open failed at {root:?}: {error}"));
            app.dispatch(Action::Open(PanelKind::Chat), 30_000).unwrap();
            app.configure_model(&ModelSettings {
                endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
                model: "synthetic".into(),
                persona: "synthetic persona".into(),
                allow_transmission: true,
            })
            .unwrap();
            app.dispatch(
                Action::SetDraft("synthetic legacy unresolved request".into()),
                30_001,
            )
            .unwrap();
            app.dispatch(Action::Send, 30_002).unwrap();
            let request = app.take_external_request().unwrap();
            let (_, ticket) = request.into_parts();
            ticket
        };

        std::fs::remove_dir_all(&chat_root).unwrap();
        std::fs::create_dir_all(&chat_root).unwrap();
        create_legacy_unresolved_raw(
            &chat_root,
            30_002,
            RawRetention::new(30 * 86_400_000, 7 * 86_400_000, 8, 256, 65_536).unwrap(),
            "synthetic legacy unresolved request",
        );

        let mut app = open_at(&root, 152, 31_000)
            .unwrap_or_else(|error| panic!("legacy recovery failed at {root:?}: {error}"));
        app.dispatch(Action::Open(PanelKind::Chat), 31_000).unwrap();
        let restored = app.snapshot();
        assert_eq!(restored.chat.status, ChatStatus::Unknown);
        assert!(restored.session_isolation_allowed);
        assert!(app.take_external_request().is_none());
        let directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(chat_root.join("sessions.v1")).unwrap()).unwrap();
        assert_eq!(directory["version"], 2);
        assert_eq!(directory["sessions"][0]["status"], "unknown");
        assert_eq!(directory["sessions"][0]["externalUnknown"], false);
        assert_eq!(directory["sessions"][0]["recoveredUnsettled"], true);

        let old_session = restored.active_session_id.unwrap();
        app.dispatch(
            Action::NewSession {
                expected_session_id: old_session.clone(),
                expected_sessions_revision: restored.sessions_revision,
            },
            31_001,
        )
        .unwrap();
        assert!(app.take_external_request().is_none());
        assert_eq!(
            app.snapshot()
                .sessions
                .iter()
                .find(|session| session.id == old_session)
                .unwrap()
                .status,
            ChatStatus::Unknown
        );

        app.configure_model(&ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "synthetic persona".into(),
            allow_transmission: true,
        })
        .unwrap();
        app.dispatch(
            Action::SetDraft("synthetic new session request".into()),
            31_002,
        )
        .unwrap();
        app.dispatch(Action::Send, 31_003).unwrap();
        let (_, new_ticket) = app.take_external_request().unwrap().into_parts();
        let stale = app
            .complete_external(
                old_ticket,
                moe_agent_runtime::ModelOutcome::Text {
                    text: "must not be replayed".into(),
                    presentation: vec![moe_agent_runtime::PresentationEvent::Neutral],
                },
                31_004,
            )
            .unwrap_err();
        assert_eq!(stale.public_code(), "stale_external_ticket");
        assert_eq!(app.snapshot().chat.status, ChatStatus::Sending);
        app.complete_external(
            new_ticket,
            moe_agent_runtime::ModelOutcome::Text {
                text: "synthetic isolated reply".into(),
                presentation: vec![moe_agent_runtime::PresentationEvent::Neutral],
            },
            31_005,
        )
        .unwrap();
        assert!(app.take_external_request().is_none());
        assert_eq!(app.snapshot().chat.status, ChatStatus::Ready);
        assert_eq!(
            app.snapshot()
                .sessions
                .iter()
                .find(|session| session.id == old_session)
                .unwrap()
                .status,
            ChatStatus::Unknown
        );
        let directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(chat_root.join("sessions.v1")).unwrap()).unwrap();
        let old = directory["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["id"] == old_session)
            .unwrap();
        assert_eq!(old["status"], "unknown");
        assert_eq!(old["externalUnknown"], false);
        assert_eq!(old["recoveredUnsettled"], true);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restored_unknown_without_external_evidence_cannot_open_an_isolated_session() {
        let root = directory();
        {
            let app = open_chat_application(&root, 103, 22_000);
            drop(app);
        }
        let directory_path = root.join("chat").join("sessions.v1");
        let mut directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&directory_path).unwrap()).unwrap();
        directory["version"] = serde_json::Value::from(1);
        directory["sessions"][0]["status"] = serde_json::Value::String("unknown".into());
        directory["sessions"][0]["externalUnknown"] = serde_json::Value::Bool(false);
        directory["sessions"][0]
            .as_object_mut()
            .unwrap()
            .remove("recoveredUnsettled");
        std::fs::write(&directory_path, serde_json::to_vec(&directory).unwrap()).unwrap();

        let mut app = open_chat_application(&root, 104, 23_000);
        assert_eq!(app.snapshot().chat.status, ChatStatus::Unknown);
        assert!(!app.snapshot().session_isolation_allowed);
        let (expected_session_id, expected_sessions_revision) = session_guard(&app);
        let error = app
            .dispatch(
                Action::NewSession {
                    expected_session_id,
                    expected_sessions_revision,
                },
                23_001,
            )
            .unwrap_err();
        assert_eq!(error.public_code(), "session_isolation_denied");
        assert_eq!(
            error.execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_or_pending_legacy_raw_never_publishes_unsettled_evidence() {
        for (generation, pending) in [(107_u64, false), (108, true)] {
            let root = directory();
            let chat_root = root.join("chat");
            std::fs::create_dir_all(&chat_root).unwrap();
            create_legacy_unresolved_raw(
                &chat_root,
                1_000,
                RawRetention::new(100_000, 200_000, 8, 256, 65_536).unwrap(),
                "synthetic unsettled legacy request",
            );
            let raw_path = chat_root.join("raw-local.v1");
            if pending {
                std::fs::write(
                    chat_root.join("raw-local.pending"),
                    b"synthetic pending frame",
                )
                .unwrap();
            } else {
                std::fs::write(&raw_path, b"synthetic corrupt RawSnapshotV1").unwrap();
            }
            let raw_before = std::fs::read(&raw_path).unwrap();
            let pending_before =
                pending.then(|| std::fs::read(chat_root.join("raw-local.pending")).unwrap());

            assert!(open_at(&root, generation, 1_001).is_err());
            assert_eq!(std::fs::read(&raw_path).unwrap(), raw_before);
            assert!(!chat_root.join("sessions.v1").exists());
            if let Some(pending_before) = pending_before {
                assert_eq!(
                    std::fs::read(chat_root.join("raw-local.pending")).unwrap(),
                    pending_before
                );
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn session_directory_v2_requires_the_recovered_unsettled_evidence_field() {
        let root = directory();
        let app = open_chat_application(&root, 105, 24_000);
        drop(app);
        let directory_path = root.join("chat").join("sessions.v1");
        let mut directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&directory_path).unwrap()).unwrap();
        assert_eq!(directory["version"], 2);
        directory["sessions"][0]
            .as_object_mut()
            .unwrap()
            .remove("recoveredUnsettled");
        std::fs::write(&directory_path, serde_json::to_vec(&directory).unwrap()).unwrap();

        assert!(open_at(&root, 106, 24_001).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_unknown_in_recycle_is_hidden_but_stays_isolated_and_tightened_expiry_is_durable() {
        let root = directory();
        let chat_root = root.join("chat");
        std::fs::create_dir_all(&chat_root).unwrap();
        let wide = RawRetention::new(1_000, 2_000, 8, 256, 65_536).unwrap();
        create_legacy_unresolved_raw(
            &chat_root,
            1_000,
            wide,
            "synthetic legacy unresolved request",
        );
        let original = std::fs::read(chat_root.join("raw-local.v1")).unwrap();
        let original_revision = u64::from_le_bytes(original[12..20].try_into().unwrap());

        {
            let mut app = open_chat_application_with_retention(&root, 141, 1_101, 100, 20);
            let snapshot = app.snapshot();
            assert_eq!(snapshot.chat.status, ChatStatus::Unknown);
            assert!(snapshot.session_isolation_allowed);
            assert!(snapshot.chat.messages.is_empty());
            let receipts = app.history_commit_receipts();
            assert!(
                receipts
                    .raw
                    .iter()
                    .any(|receipt| receipt.revision == original_revision + 1)
            );
            let directory: serde_json::Value =
                serde_json::from_slice(&std::fs::read(chat_root.join("sessions.v1")).unwrap())
                    .unwrap();
            assert_eq!(directory["sessions"][0]["externalUnknown"], false);
            assert_eq!(directory["sessions"][0]["recoveredUnsettled"], true);
            assert_eq!(directory["sessions"][0]["status"], "unknown");

            app.dispatch(Action::SetDraft("do not resend".into()), 1_102)
                .unwrap();
            let rejected = app.dispatch(Action::Send, 1_103).unwrap_err();
            assert_eq!(rejected.public_code(), "send_unknown");
            assert_eq!(
                rejected.execution_disposition(),
                ExecutionDisposition::NotExecuted
            );
            assert!(app.take_external_request().is_none());
        }
        let tightened = std::fs::read(chat_root.join("raw-local.v1")).unwrap();
        assert_ne!(
            original, tightened,
            "the strict policy is committed on recovery"
        );

        let mut app = open_chat_application_with_retention(&root, 142, 1_150, 1_000, 2_000);
        let reopened = app.snapshot();
        assert_eq!(reopened.chat.status, ChatStatus::Unknown);
        assert!(reopened.session_isolation_allowed);
        assert!(reopened.chat.messages.is_empty());
        let directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(chat_root.join("sessions.v1")).unwrap()).unwrap();
        assert_eq!(directory["sessions"][0]["externalUnknown"], false);
        assert_eq!(directory["sessions"][0]["recoveredUnsettled"], true);
        app.dispatch(Action::Revoke, 1_151).unwrap();
        assert!(!app.snapshot().session_isolation_allowed);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_publishes_unknown_before_cleanup_and_keeps_it_after_segment_failure() {
        let blocked_root = directory();
        let blocked_chat = blocked_root.join("chat");
        std::fs::create_dir_all(&blocked_chat).unwrap();
        create_legacy_unresolved_raw(
            &blocked_chat,
            1_000,
            RawRetention::new(100, 20, 8, 256, 65_536).unwrap(),
            "synthetic unresolved legacy request",
        );
        let original = std::fs::read(blocked_chat.join("raw-local.v1")).unwrap();
        history::inject_failure(history::FailurePoint::PublishCreate);
        assert!(
            open_at_with_retention(
                &blocked_root,
                243,
                1_120,
                RawRetention::new(100, 20, 8, 256, 65_536).unwrap(),
            )
            .is_err()
        );
        history::clear_failure();
        assert_eq!(
            std::fs::read(blocked_chat.join("raw-local.v1")).unwrap(),
            original,
            "failed summary publication must not sanitize the source frame"
        );
        assert!(!blocked_chat.join("sessions.v1").exists());
        std::fs::remove_dir_all(&blocked_root).unwrap();

        let retry_root = directory();
        let retry_chat = retry_root.join("chat");
        std::fs::create_dir_all(&retry_chat).unwrap();
        create_legacy_unresolved_raw(
            &retry_chat,
            1_000,
            RawRetention::new(100, 20, 8, 256, 65_536).unwrap(),
            "synthetic unresolved legacy request",
        );
        let original = std::fs::read(retry_chat.join("raw-local.v1")).unwrap();
        history::inject_failure(history::FailurePoint::SegmentBeforeMarker);
        assert!(
            open_at_with_retention(
                &retry_root,
                244,
                1_120,
                RawRetention::new(100, 20, 8, 256, 65_536).unwrap(),
            )
            .is_err()
        );
        history::clear_failure();
        assert_ne!(
            std::fs::read(retry_chat.join("raw-local.v1")).unwrap(),
            original,
            "the strict final deadline removes the source interaction"
        );
        let directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(retry_chat.join("sessions.v1")).unwrap())
                .unwrap();
        assert_eq!(directory["sessions"][0]["status"], "unknown");
        assert_eq!(directory["sessions"][0]["recoveredUnsettled"], true);
        let mut retry = open_chat_application_with_retention(&retry_root, 245, 1_121, 100, 20);
        assert_eq!(retry.snapshot().chat.status, ChatStatus::Unknown);
        assert!(retry.snapshot().session_isolation_allowed);
        let (old_session, sessions_revision) = session_guard(&retry);
        retry
            .dispatch(
                Action::NewSession {
                    expected_session_id: old_session.clone(),
                    expected_sessions_revision: sessions_revision,
                },
                1_122,
            )
            .unwrap();
        assert_eq!(
            retry
                .snapshot()
                .sessions
                .iter()
                .find(|session| session.id == old_session)
                .unwrap()
                .status,
            ChatStatus::Unknown
        );
        drop(retry);
        std::fs::remove_dir_all(retry_root).unwrap();
    }

    #[test]
    fn older_unsettled_legacy_interaction_survives_newer_completion_and_final_expiry() {
        let root = directory();
        let chat_root = root.join("chat");
        std::fs::create_dir_all(&chat_root).unwrap();
        create_legacy_with_older_unsettled_interaction(&chat_root);

        let mut app = open_chat_application_with_retention(&root, 241, 1_100, 10_000, 10_000);
        assert_eq!(app.snapshot().chat.status, ChatStatus::Unknown);
        assert_eq!(app.snapshot().chat.messages.len(), 128);
        assert!(
            app.snapshot()
                .chat
                .messages
                .iter()
                .all(|message| message.text != "synthetic earliest unresolved interaction")
        );
        let directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(chat_root.join("sessions.v1")).unwrap()).unwrap();
        assert_eq!(directory["sessions"][0]["status"], "unknown");
        assert_eq!(directory["sessions"][0]["externalUnknown"], false);

        app.maintain_retention(21_100).unwrap();
        drop(app);
        let reopened = open_chat_application_with_retention(&root, 242, 21_101, 10_000, 10_000);
        assert_eq!(reopened.snapshot().chat.status, ChatStatus::Unknown);
        assert!(reopened.snapshot().chat.messages.is_empty());
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_commits_tight_retention_for_archived_cold_sessions() {
        let root = directory();
        let send_at = 5_001_u64;
        let old_segment_path;
        {
            let mut app = open_chat_application_with_retention(&root, 143, 5_000, 1_000, 2_000);
            send_offline(&mut app, "synthetic archived cold content", send_at - 1);
            let initial: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join("chat").join("sessions.v1")).unwrap(),
            )
            .unwrap();
            old_segment_path = initial["sessions"][0]["segments"][0]["path"]
                .as_str()
                .unwrap()
                .to_owned();
            let raw_path = root
                .join("chat")
                .join(&old_segment_path)
                .join("raw-local.v1");
            assert!(!std::fs::read(&raw_path).unwrap().is_empty());

            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::NewSession {
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                5_003,
            )
            .unwrap();
            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::SwitchSession {
                    id: String::from("session-1"),
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                5_004,
            )
            .unwrap();
            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::ArchiveSession {
                    id: current_id.clone(),
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                5_006,
            )
            .unwrap();
            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::SwitchSession {
                    id: String::from("session-2"),
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                5_007,
            )
            .unwrap();
            let directory: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join("chat").join("sessions.v1")).unwrap(),
            )
            .unwrap();
            assert_eq!(directory["sessions"][0]["archived"], true);
            assert_eq!(directory["sessions"][0]["segments"][0]["sealed"], true);
        }
        let old_segment_bytes = std::fs::read(
            root.join("chat")
                .join(&old_segment_path)
                .join("raw-local.v1"),
        )
        .unwrap();
        let old_segment_revision =
            u64::from_le_bytes(old_segment_bytes[12..20].try_into().unwrap());

        {
            let app = open_chat_application_with_retention(&root, 144, 5_010, 100, 20);
            let receipts = app.history_commit_receipts();
            assert!(
                receipts
                    .raw
                    .iter()
                    .any(|receipt| receipt.revision == old_segment_revision + 1)
            );
            assert!(
                !std::fs::read(
                    root.join("chat")
                        .join(&old_segment_path)
                        .join("raw-local.v1")
                )
                .unwrap()
                .is_empty()
            );
        }

        let mut app = open_chat_application_with_retention(&root, 145, 5_150, 1_000, 2_000);
        let (current_id, revision) = session_guard(&app);
        app.dispatch(
            Action::SwitchSession {
                id: String::from("session-1"),
                expected_session_id: current_id,
                expected_sessions_revision: revision,
            },
            5_151,
        )
        .unwrap();
        assert!(app.snapshot().chat.messages.is_empty());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hot_and_cold_segments_follow_normal_and_recycle_deadlines() {
        let root = directory();
        let retention_normal = 30 * 86_400_000_u64;
        let retention_recycle = 7 * 86_400_000_u64;
        let normal_at = 30_002 + retention_normal;
        let final_at = normal_at + retention_recycle;
        let old_segment = {
            let mut app = open_chat_application(&root, 111, 30_000);
            send_offline(&mut app, "synthetic expiring history", 30_001);
            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::NewSession {
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                30_010,
            )
            .unwrap();
            let directory: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join("chat").join("sessions.v1")).unwrap(),
            )
            .unwrap();
            directory["sessions"][0]["segments"][0]["path"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let segment_root = root.join("chat").join(old_segment).join("raw-local.v1");
        let before = std::fs::read(&segment_root).unwrap();
        let before_revision = u64::from_le_bytes(before[12..20].try_into().unwrap());
        {
            let mut app = open_chat_application(&root, 112, normal_at);
            let restore_receipts = app.history_commit_receipts();
            assert!(
                restore_receipts
                    .raw
                    .iter()
                    .any(|receipt| receipt.revision == before_revision + 1)
            );
            let progress = app.maintain_retention(normal_at).unwrap().unwrap();
            assert!(progress.cold_segments_processed >= 1);
            assert_eq!(
                progress.cold_segments_remaining, 0,
                "all cold segments fit in this bounded pass"
            );
        }
        let in_recycle = std::fs::read(&segment_root).unwrap();
        assert_ne!(
            before, in_recycle,
            "normal expiry is persisted to the cold segment"
        );
        {
            let mut app = open_chat_application(&root, 113, final_at);
            app.maintain_retention(final_at).unwrap();
            let (current_id, revision) = session_guard(&app);
            app.dispatch(
                Action::SwitchSession {
                    id: String::from("session-1"),
                    expected_session_id: current_id,
                    expected_sessions_revision: revision,
                },
                final_at + 1,
            )
            .unwrap();
            assert!(app.snapshot().chat.messages.is_empty());
        }
        assert!(
            !segment_root.exists(),
            "confirmed empty cold segment is retired"
        );
        assert_ne!(before, in_recycle, "normal expiry is persisted");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cold_capacity_backpressure_returns_partial_progress_and_exact_receipts() {
        let root = directory();
        let send_at = 20_001_u64;
        let mut app = open_chat_application_with_retention(&root, 146, 20_000, 100, 20);
        send_offline(&mut app, "x", send_at - 1);
        let first_directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("chat").join("sessions.v1")).unwrap())
                .unwrap();
        let small = first_directory["sessions"][0]["segments"][0]["path"]
            .as_str()
            .unwrap()
            .to_owned();
        let (current_id, revision) = session_guard(&app);
        app.dispatch(
            Action::NewSession {
                expected_session_id: current_id,
                expected_sessions_revision: revision,
            },
            20_003,
        )
        .unwrap();
        send_offline(&mut app, &"y".repeat(6_000), 20_004);
        let second_directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("chat").join("sessions.v1")).unwrap())
                .unwrap();
        let large = second_directory["sessions"][1]["segments"][0]["path"]
            .as_str()
            .unwrap()
            .to_owned();
        let (current_id, revision) = session_guard(&app);
        app.dispatch(
            Action::NewSession {
                expected_session_id: current_id,
                expected_sessions_revision: revision,
            },
            20_007,
        )
        .unwrap();
        let large_before =
            std::fs::read(root.join("chat").join(&large).join("raw-local.v1")).unwrap();

        let chat_root = root.join("chat");
        let current_total = tree_bytes(&chat_root);
        let reserve = (16 * 1024 * 1024_u64)
            .checked_sub(current_total + 4_096)
            .expect("fixture begins below the root byte limit");
        std::fs::write(
            chat_root.join("retention-capacity-fixture"),
            vec![b'x'; reserve as usize],
        )
        .unwrap();

        let report = app.maintain_retention(20_121).unwrap().unwrap();
        assert_eq!(report.cold_segments_processed, 1);
        assert_eq!(report.cold_segments_remaining, 1);
        assert_eq!(
            report.cold_blocked,
            Some(moe_chat_app::ColdMaintenanceBlock::CapacityExceeded)
        );
        assert_eq!(report.receipts.raw.len(), 1);
        assert!(report.receipts.raw[0].revision > 1);
        assert!(report.receipts.directory.is_some());
        assert_eq!(app.history_commit_receipts(), report.receipts);
        assert!(
            !chat_root.join(&small).exists(),
            "the first cold segment completed retirement before the second was blocked"
        );
        assert_eq!(
            std::fs::read(chat_root.join(&large).join("raw-local.v1")).unwrap(),
            large_before,
            "the blocked segment retains its prior frame"
        );
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cold_error_after_retirement_preserves_raw_and_directory_receipts() {
        let root = directory();
        let mut app = open_chat_application_with_retention(&root, 147, 20_000, 100, 20);
        send_offline(&mut app, "synthetic expired first segment", 20_000);
        let first_directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("chat").join("sessions.v1")).unwrap())
                .unwrap();
        let first = first_directory["sessions"][0]["segments"][0]["path"]
            .as_str()
            .unwrap()
            .to_owned();
        let (current, revision) = session_guard(&app);
        app.dispatch(
            Action::NewSession {
                expected_session_id: current,
                expected_sessions_revision: revision,
            },
            20_003,
        )
        .unwrap();
        send_offline(&mut app, &"y".repeat(6_000), 20_004);
        let second_directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("chat").join("sessions.v1")).unwrap())
                .unwrap();
        let second = second_directory["sessions"][1]["segments"][0]["path"]
            .as_str()
            .unwrap()
            .to_owned();
        let (current, revision) = session_guard(&app);
        app.dispatch(
            Action::NewSession {
                expected_session_id: current,
                expected_sessions_revision: revision,
            },
            20_007,
        )
        .unwrap();
        let second_frame = root.join("chat").join(&second).join("raw-local.v1");
        let mut bytes = std::fs::read(&second_frame).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&second_frame, bytes).unwrap();

        let error = app
            .maintain_retention(20_121)
            .expect_err("the corrupt second cold frame fails after first retirement");
        assert_eq!(error.execution_disposition(), ExecutionDisposition::Unknown);
        assert!(!root.join("chat").join(&first).exists());
        let receipts = app.history_commit_receipts();
        assert!(!receipts.raw.is_empty());
        assert!(receipts.directory.is_some());
        assert_eq!(app.snapshot().chat.status, ChatStatus::Unknown);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn send_capacity_rejection_precedes_generation_rotation_and_external_request() {
        let root = directory();
        let mut app = open_chat_application(&root, 148, 60_000);
        app.configure_model(&ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "synthetic persona".into(),
            allow_transmission: true,
        })
        .unwrap();
        let mut request_count = 0;
        for turn in 0..31_u64 {
            let now = 60_001 + turn * 10;
            app.dispatch(Action::SetDraft(format!("synthetic turn {turn}")), now)
                .unwrap();
            app.dispatch(Action::Send, now + 1).unwrap();
            let (_, ticket) = app.take_external_request().unwrap().into_parts();
            request_count += 1;
            app.complete_external(
                ticket,
                moe_agent_runtime::ModelOutcome::Text {
                    text: String::from("synthetic response"),
                    presentation: vec![moe_agent_runtime::PresentationEvent::Neutral],
                },
                now + 2,
            )
            .unwrap();
        }
        assert_eq!(request_count, 31);
        let before_snapshot = app.snapshot();
        let directory_path = root.join("chat").join("sessions.v1");
        let directory_before = std::fs::read(&directory_path).unwrap();
        let directory_json: serde_json::Value = serde_json::from_slice(&directory_before).unwrap();
        let active_session = &directory_json["sessions"][0];
        let segment_count = active_session["segments"].as_array().unwrap().len();
        let active_segment_id = active_session["activeSegment"].as_str().unwrap();
        let active_segment = active_session["segments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|segment| segment["id"] == active_segment_id)
            .unwrap();
        let active_frame_path = root
            .join("chat")
            .join(active_segment["path"].as_str().unwrap())
            .join("raw-local.v1");
        let active_frame_before = std::fs::read(&active_frame_path).unwrap();
        let root_bytes = tree_bytes(&root.join("chat"));
        let remaining = 16 * 1024 * 1024_u64 - root_bytes;
        assert!(
            remaining > 1_024,
            "fixture needs room for its capacity marker"
        );
        std::fs::write(
            root.join("chat").join("rotation-capacity-fixture"),
            vec![b'x'; (remaining - 1_024) as usize],
        )
        .unwrap();

        let now = 60_001 + 31 * 10;
        app.dispatch(Action::SetDraft("synthetic turn 31".into()), now)
            .unwrap();
        let rejected = app.dispatch(Action::Send, now + 1).unwrap_err();
        assert_eq!(rejected.public_code(), "storage_capacity_exceeded");
        assert_eq!(
            rejected.execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        assert_eq!(request_count, 31);
        assert!(app.take_external_request().is_none());
        let after_snapshot = app.snapshot();
        assert_eq!(
            after_snapshot.sessions_revision,
            before_snapshot.sessions_revision
        );
        assert_eq!(
            after_snapshot.chat.messages.len(),
            before_snapshot.chat.messages.len()
        );
        assert_eq!(std::fs::read(&directory_path).unwrap(), directory_before);
        let after_directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&directory_path).unwrap()).unwrap();
        assert_eq!(
            after_directory["sessions"][0]["segments"]
                .as_array()
                .unwrap()
                .len(),
            segment_count
        );
        assert_eq!(
            std::fs::read(&active_frame_path).unwrap(),
            active_frame_before
        );
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn send_denial_precedes_rotation_and_later_committed_rotation_error_keeps_receipt() {
        let root = directory();
        let mut app = open_chat_application(&root, 246, 60_000);
        app.configure_model(&ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "synthetic persona".into(),
            allow_transmission: true,
        })
        .unwrap();
        for turn in 0..31_u64 {
            let now = 60_001 + turn * 10;
            app.dispatch(Action::SetDraft(format!("synthetic turn {turn}")), now)
                .unwrap();
            app.dispatch(Action::Send, now + 1).unwrap();
            let (_, ticket) = app.take_external_request().unwrap().into_parts();
            app.complete_external(
                ticket,
                moe_agent_runtime::ModelOutcome::Text {
                    text: String::from("synthetic response"),
                    presentation: vec![moe_agent_runtime::PresentationEvent::Neutral],
                },
                now + 2,
            )
            .unwrap();
        }

        app.configure_model(&ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "synthetic persona".into(),
            allow_transmission: false,
        })
        .unwrap();
        let directory_path = root.join("chat").join("sessions.v1");
        let directory_before = std::fs::read(&directory_path).unwrap();
        let directory: serde_json::Value = serde_json::from_slice(&directory_before).unwrap();
        let mut raw_before = Vec::new();
        for session in directory["sessions"].as_array().unwrap() {
            for segment in session["segments"].as_array().unwrap() {
                let relative = segment["path"].as_str().unwrap();
                let path = root.join("chat").join(relative).join("raw-local.v1");
                raw_before.push((path.clone(), std::fs::read(path).unwrap()));
            }
        }
        let now = 60_001 + 31 * 10;
        app.dispatch(Action::SetDraft("synthetic turn 31".into()), now)
            .unwrap();
        let denied = app.dispatch(Action::Send, now + 1).unwrap_err();
        assert_eq!(denied.public_code(), "transmission_denied");
        assert_eq!(
            denied.execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        assert!(app.take_external_request().is_none());
        assert_eq!(std::fs::read(&directory_path).unwrap(), directory_before);
        for (path, bytes) in &raw_before {
            assert_eq!(std::fs::read(path).unwrap(), *bytes);
        }

        app.configure_model(&ModelSettings {
            endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
            model: "synthetic".into(),
            persona: "synthetic persona".into(),
            allow_transmission: true,
        })
        .unwrap();
        history::inject_failure(history::FailurePoint::SegmentBeforeMarker);
        let rotation_error = app.dispatch(Action::Send, now + 2).unwrap_err();
        history::clear_failure();
        assert_ne!(
            rotation_error.execution_disposition(),
            ExecutionDisposition::NotExecuted
        );
        let receipts = app.history_commit_receipts();
        assert!(!receipts.raw.is_empty());
        let active_directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&directory_path).unwrap()).unwrap();
        let active = &active_directory["sessions"][0];
        let active_segment_id = active["activeSegment"].as_str().unwrap();
        let active_segment = active["segments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|segment| segment["id"] == active_segment_id)
            .unwrap();
        let active_frame = root
            .join("chat")
            .join(active_segment["path"].as_str().unwrap())
            .join("raw-local.v1");
        let active_bytes = std::fs::read(active_frame).unwrap();
        let revision = u64::from_le_bytes(active_bytes[12..20].try_into().unwrap());
        assert_eq!(receipts.raw.last().unwrap().revision, revision);
        assert!(app.take_external_request().is_none());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cold_retirement_releases_segment_budget_without_reusing_old_sequences() {
        let root = directory();
        let retention_total = 37 * 86_400_000_u64;
        let mut now = 100_000_u64;
        let old_segment_path;
        {
            let mut app = open_chat_application(&root, 131, now);
            send_offline(&mut app, "synthetic generation zero", now + 1);
            let initial_directory: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join("chat").join("sessions.v1")).unwrap(),
            )
            .unwrap();
            old_segment_path = initial_directory["sessions"][0]["segments"][0]["path"]
                .as_str()
                .unwrap()
                .to_owned();
            let old_segment_bytes = std::fs::read(
                root.join("chat")
                    .join(&old_segment_path)
                    .join("raw-local.v1"),
            )
            .unwrap();
            assert!(!old_segment_bytes.is_empty());

            let (expected_session_id, expected_sessions_revision) = session_guard(&app);
            app.dispatch(
                Action::NewSession {
                    expected_session_id,
                    expected_sessions_revision,
                },
                now + 3,
            )
            .unwrap();
            now += retention_total + 10;
            app.maintain_retention(now).unwrap();

            for generation in 1..=260_u64 {
                let active = app.snapshot().active_session_id.unwrap();
                let target = if active == "session-1" {
                    String::from("session-2")
                } else {
                    String::from("session-1")
                };
                send_offline(
                    &mut app,
                    &format!("synthetic generation {generation}"),
                    now + 1,
                );
                let (expected_session_id, expected_sessions_revision) = session_guard(&app);
                app.dispatch(
                    Action::SwitchSession {
                        id: target,
                        expected_session_id,
                        expected_sessions_revision,
                    },
                    now + 3,
                )
                .unwrap_or_else(|error| panic!("generation {generation} rotation: {error:?}"));
                now += retention_total + 10;
                app.maintain_retention(now).unwrap_or_else(|error| {
                    panic!("generation {generation} maintenance: {error:?}")
                });
            }

            let directory: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join("chat").join("sessions.v1")).unwrap(),
            )
            .unwrap();
            assert!(directory["nextSegmentSequence"].as_u64().unwrap() > 256);
            let active_segments = directory["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|session| session["segments"].as_array().unwrap().len())
                .sum::<usize>();
            assert!(
                active_segments < 16,
                "expired segment references were retired"
            );
            send_offline(&mut app, "synthetic after retirement", now + 1);
            assert!(
                app.snapshot()
                    .chat
                    .messages
                    .iter()
                    .any(|message| { message.text == "synthetic after retirement" })
            );
        }

        let old_path = root.join("chat").join(old_segment_path);
        assert!(!old_path.exists());
        std::fs::create_dir_all(&old_path).unwrap();
        std::fs::write(
            old_path.join("raw-local.v1"),
            b"synthetic stale retired frame",
        )
        .unwrap();
        assert!(
            open_at(&root, 132, now + 10).is_err(),
            "a frame with a retired sequence is not reactivated"
        );
        assert_eq!(
            std::fs::read(old_path.join("raw-local.v1")).unwrap(),
            b"synthetic stale retired frame"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cold_retirement_failures_leave_a_retryable_fence_or_the_original_segment() {
        use super::history::{self, FailurePoint};

        let retention_total = 37 * 86_400_000_u64;
        for (index, point) in [
            FailurePoint::RetireBeforeMarker,
            FailurePoint::RetireAfterMarker,
            FailurePoint::RetireAfterCurrentRemoval,
            FailurePoint::RetireAfterLockRemoval,
            FailurePoint::RetireAfterDirectoryRemoval,
            FailurePoint::RetireBeforeFinalMarker,
        ]
        .into_iter()
        .enumerate()
        {
            let root = directory();
            let start = 10_000_u64;
            let final_at = start + retention_total + 20;
            let segment_id;
            {
                let mut app = open_chat_application(&root, 150 + index as u64, start);
                send_offline(&mut app, "synthetic expiring segment", start + 1);
                let (session_id, revision) = session_guard(&app);
                app.dispatch(
                    Action::NewSession {
                        expected_session_id: session_id,
                        expected_sessions_revision: revision,
                    },
                    start + 3,
                )
                .unwrap();
                let directory: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(root.join("chat").join("sessions.v1")).unwrap(),
                )
                .unwrap();
                segment_id = directory["sessions"][0]["segments"][0]["id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                history::inject_failure(point);
                let error = app
                    .maintain_retention(final_at)
                    .expect_err("the injected retirement point must fail");
                history::clear_failure();
                assert_eq!(error.execution_disposition(), ExecutionDisposition::Unknown);
            }

            let directory_path = root.join("chat").join("sessions.v1");
            let directory: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&directory_path).unwrap()).unwrap();
            let retired = directory["retiringSegments"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["id"].as_str() == Some(segment_id.as_str()));
            if point == FailurePoint::RetireBeforeMarker {
                assert!(!retired);
                assert!(
                    directory["sessions"][0]["segments"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|segment| segment["id"].as_str() == Some(segment_id.as_str()))
                );
            } else {
                assert!(retired, "durable fence identifies interrupted retirement");
            }

            let mut recovered = open_chat_application(&root, 170 + index as u64, final_at + 1);
            if point == FailurePoint::RetireBeforeMarker {
                recovered.maintain_retention(final_at + 1).unwrap();
            }
            assert_eq!(recovered.snapshot().sessions.len(), 2);
            assert!(
                !root
                    .join("chat")
                    .join("segments")
                    .join(&segment_id)
                    .exists(),
                "restart cleanup finishes only the explicitly fenced segment"
            );
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn marked_retirement_rejects_a_legal_writer_and_preserves_its_new_frame() {
        let root = directory();
        let (segment_id, final_at) = marked_retirement_fixture(&root, 20_000, 181);

        let segment_path = root.join("chat").join("segments").join(&segment_id);
        let before = std::fs::read(segment_path.join("raw-local.v1")).unwrap();
        let identity = PetIdentity::new(
            Namespace::new("moe-chat", "local").unwrap(),
            "offline-companion",
        )
        .unwrap();
        let mut ingress = TrustedIngress::new();
        ingress.register_route("buddy/offline", identity).unwrap();
        let credential = ingress
            .install_credential(
                IngressToken::new("offline-acceptance-token-not-for-production").unwrap(),
                CallerSubject::new("local-trusted-host").unwrap(),
                [String::from("buddy/offline")],
                ChannelId::new("local-chat").unwrap(),
                ChannelScope::new("private-window").unwrap(),
                ReplyTarget::new("chat-window").unwrap(),
            )
            .unwrap();
        ingress
            .bind_stable_credential(credential, "daily-desktop-owner")
            .unwrap();
        let request = ingress
            .admit(
                RawIngressCall::new(
                    IngressToken::new("offline-acceptance-token-not-for-production").unwrap(),
                    "buddy/offline",
                    "history-access",
                )
                .with_session_hint("session-1"),
            )
            .unwrap();
        let retention = RawRetention::new(30 * 86_400_000, 7 * 86_400_000, 8, 256, 65_536).unwrap();
        let mut runtime = SessionRuntime::with_retention(
            &ingress,
            SessionLimits::new(8, 64, 256, 8_192, 65_536, 2).unwrap(),
            SessionTiming::default(),
            retention,
            final_at + 1,
        );
        let domain = RawDomainKey::from_request(&ingress, &request).unwrap();
        let grant = runtime
            .install_raw_history_grant(&ingress, domain.clone())
            .unwrap();
        let mut store = moe_raw_local::RawLocalStore::open(
            &segment_path,
            moe_raw_local::OpenMode::OpenExisting,
            moe_raw_local::RawLocalAccess::new(
                &mut runtime,
                &ingress,
                &request,
                &grant,
                domain.clone(),
                final_at + 1,
            )
            .unwrap(),
        )
        .unwrap()
        .store;
        let revision = store.revision();
        store
            .restore(
                moe_raw_local::RawLocalAccess::new(
                    &mut runtime,
                    &ingress,
                    &request,
                    &grant,
                    domain.clone(),
                    final_at + 1,
                )
                .unwrap(),
                &retention,
                revision,
            )
            .unwrap();
        let registered = runtime
            .register_user(
                SessionAccess::new(&ingress, &request, final_at + 2),
                UserEvent::new(UserEventKind::New),
            )
            .unwrap();
        runtime
            .finish_interaction(
                SessionAccess::new(&ingress, &request, final_at + 3),
                &registered.interaction,
                moe_agent_runtime::TerminalState::Completed,
                Some("synthetic unexpired body"),
            )
            .unwrap();
        store
            .checkpoint(
                moe_raw_local::RawLocalAccess::new(
                    &mut runtime,
                    &ingress,
                    &request,
                    &grant,
                    domain,
                    final_at + 4,
                )
                .unwrap(),
                store.revision(),
            )
            .unwrap();
        let updated = std::fs::read(segment_path.join("raw-local.v1")).unwrap();
        assert_ne!(updated, before, "the legal writer published a new frame");
        assert!(open_at(&root, 182, final_at + 5).is_err());
        assert_eq!(
            std::fs::read(segment_path.join("raw-local.v1")).unwrap(),
            updated,
            "failed recovery preserves the new body and retirement fence"
        );
        drop(store);
        assert!(open_at(&root, 183, final_at + 6).is_err());
        assert_eq!(
            std::fs::read(segment_path.join("raw-local.v1")).unwrap(),
            updated
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pending_retirement_rejects_changed_domain_authorization_bad_frames_and_candidates() {
        for (index, mutation) in ["domain", "authorization", "frame", "pending", "candidate"]
            .into_iter()
            .enumerate()
        {
            let root = directory();
            let (segment_id, final_at) = marked_retirement_fixture(
                &root,
                30_000 + index as u64 * 100_000,
                191 + index as u64,
            );
            let segment_path = root.join("chat").join("segments").join(&segment_id);
            let directory_path = root.join("chat").join("sessions.v1");
            let affected_path = match mutation {
                "domain" | "authorization" => {
                    let mut directory: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&directory_path).unwrap()).unwrap();
                    if mutation == "domain" {
                        directory["retiringSegments"][0]["domain"]["credential"] =
                            serde_json::Value::String("another-owner".to_owned());
                    } else {
                        directory["retiringSegments"][0]["authorizationRevision"] =
                            serde_json::Value::from(2_u64);
                    }
                    std::fs::write(&directory_path, serde_json::to_vec(&directory).unwrap())
                        .unwrap();
                    directory_path.clone()
                }
                "frame" => {
                    let path = segment_path.join("raw-local.v1");
                    std::fs::write(&path, b"synthetic corrupt retired frame").unwrap();
                    path
                }
                "pending" => {
                    let path = segment_path.join("raw-local.pending");
                    std::fs::write(&path, b"synthetic pending frame").unwrap();
                    path
                }
                _ => {
                    let path = segment_path.join(".candidate");
                    std::fs::write(&path, b"synthetic candidate marker").unwrap();
                    path
                }
            };
            let preserved = std::fs::read(&affected_path).unwrap();
            assert!(open_at(&root, 201 + index as u64, final_at + 1).is_err());
            assert_eq!(
                std::fs::read(&affected_path).unwrap(),
                preserved,
                "{mutation}"
            );
            assert!(directory_path.exists());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn open_directory_reads_a_retirement_fence_without_deleting_its_segment() {
        let root = directory();
        let (segment_id, final_at) = marked_retirement_fixture(&root, 900_000, 211);
        let segment_path = root.join("chat").join("segments").join(&segment_id);
        let frame = std::fs::read(segment_path.join("raw-local.v1")).unwrap();
        let directory_path = root.join("chat").join("sessions.v1");
        let directory_before = std::fs::read(&directory_path).unwrap();

        let mut history = super::history::DiskHistory::new(root.join("chat"));
        let recovery = history.open_directory().unwrap().unwrap();
        assert_eq!(recovery.sessions.len(), 2);
        assert_eq!(
            std::fs::read(segment_path.join("raw-local.v1")).unwrap(),
            frame
        );
        assert_eq!(std::fs::read(&directory_path).unwrap(), directory_before);
        drop(history);

        let recovered = open_chat_application(&root, 212, final_at + 1);
        assert!(!segment_path.exists());
        drop(recovered);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_segment_retention_checkpoints_transfer_and_final_cleanup() {
        let root = directory();
        let sent_at = 50_002_u64;
        let normal_at = sent_at + 30 * 86_400_000;
        let final_at = normal_at + 7 * 86_400_000;
        let mut app = open_chat_application(&root, 116, 50_000);
        send_offline(&mut app, "synthetic hot retention", sent_at - 1);
        let active_path = {
            let directory: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join("chat").join("sessions.v1")).unwrap(),
            )
            .unwrap();
            directory["sessions"][0]["segments"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["path"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let frame_path = root.join("chat").join(active_path).join("raw-local.v1");
        let before = std::fs::read(&frame_path).unwrap();

        let transferred = app.maintain_retention(normal_at).unwrap().unwrap();
        assert_eq!(transferred.live.transferred, 1);
        assert_eq!(transferred.receipts.raw.len(), 1);
        assert!(transferred.receipts.raw[0].file_synced);
        assert!(app.snapshot().chat.messages.is_empty());
        let recycled = std::fs::read(&frame_path).unwrap();
        assert_ne!(before, recycled);

        let removed = app.maintain_retention(final_at).unwrap().unwrap();
        assert_eq!(removed.live.removed, 1);
        assert!(removed.live.released_events > 0);
        assert!(app.snapshot().chat.messages.is_empty());
        assert_ne!(recycled, std::fs::read(&frame_path).unwrap());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn idle_deadline_fragments_rotate_before_the_normal_zone_fills() {
        let root = directory();
        let mut app = open_chat_application(&root, 1_600, 1_000);
        let interval = moe_agent_runtime::SessionTiming::default().active_ms();
        let mut failure = None;
        for turn in 0..24_u64 {
            let now = 1_001 + turn * interval;
            let result = app
                .dispatch(
                    moe_chat_app::Action::SetDraft(format!("synthetic idle turn {turn}")),
                    now,
                )
                .and_then(|()| app.dispatch(moe_chat_app::Action::Send, now + 1));
            if let Err(error) = result {
                failure = Some((turn, error.public_code().to_owned()));
                break;
            }
        }
        let messages = app.snapshot().chat.messages.len();
        let directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("chat/sessions.v1")).unwrap()).unwrap();
        let segments = directory["sessions"][0]["segments"]
            .as_array()
            .unwrap()
            .len();
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            failure.is_none(),
            "idle-fragment admission failed: {failure:?}"
        );
        assert_eq!(messages, 48);
        assert_eq!(
            segments, 3,
            "the original eight-fragment capacity remains unchanged"
        );
    }

    #[test]
    fn one_persistent_instance_rotates_past_4096_short_requests_with_bounded_state() {
        let root = directory();
        let mut app = open_chat_application(&root, 121, 40_000);
        app.dispatch(Action::Open(PanelKind::Memory), 40_000)
            .unwrap();
        app.dispatch(
            Action::Remember(String::from("synthetic persistent note")),
            40_001,
        )
        .unwrap();
        for turn in 0..4_101_u64 {
            let now = 40_002 + turn * 2;
            app.dispatch(Action::SetDraft(String::from("x")), now)
                .unwrap();
            app.dispatch(Action::Send, now + 1)
                .unwrap_or_else(|error| panic!("turn {turn} failed: {error:?}"));
        }
        let snapshot = app.snapshot();
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.chat.messages.len(), 128);
        assert_eq!(snapshot.chat.status, ChatStatus::Ready);
        app.dispatch(Action::RefreshMemories, 48_204).unwrap();
        assert!(
            app.snapshot()
                .memories
                .iter()
                .any(|memory| memory.text == "synthetic persistent note")
        );
        let directory: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("chat").join("sessions.v1")).unwrap())
                .unwrap();
        let segments = directory["sessions"][0]["segments"].as_array().unwrap();
        assert!(segments.len() > 64);
        assert!(segments.len() <= 256);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn equal_timestamp_messages_keep_old_segment_before_new_segment() {
        let root = directory();
        {
            let mut app = open_chat_application(&root, 150, 80_000);
            app.configure_model(&ModelSettings {
                endpoint: "http://127.0.0.1:9/v1/chat/completions".into(),
                model: "synthetic".into(),
                persona: "synthetic persona".into(),
                allow_transmission: true,
            })
            .unwrap();
            for turn in 0..31_u64 {
                let now = 80_000 + turn * 10;
                app.dispatch(Action::SetDraft(format!("synthetic user {turn}")), now)
                    .unwrap();
                app.dispatch(Action::Send, now + 1).unwrap();
                let (_, ticket) = app.take_external_request().unwrap().into_parts();
                app.complete_external(
                    ticket,
                    moe_agent_runtime::ModelOutcome::Text {
                        text: format!("synthetic reply {turn}"),
                        presentation: vec![moe_agent_runtime::PresentationEvent::Neutral],
                    },
                    now + 2,
                )
                .unwrap();
            }
            let tie_timestamp = 80_302_u64;
            app.dispatch(
                Action::SetDraft(String::from("synthetic tie turn")),
                tie_timestamp - 1,
            )
            .unwrap();
            app.dispatch(Action::Send, tie_timestamp).unwrap();
            let (_, ticket) = app.take_external_request().unwrap().into_parts();
            app.complete_external(
                ticket,
                moe_agent_runtime::ModelOutcome::Text {
                    text: String::from("synthetic tie reply"),
                    presentation: vec![moe_agent_runtime::PresentationEvent::Neutral],
                },
                tie_timestamp + 1,
            )
            .unwrap();
        }
        let app = open_chat_application(&root, 151, 80_400);
        let messages = app.snapshot().chat.messages;
        let previous_reply = messages
            .iter()
            .position(|message| message.text == "synthetic reply 30")
            .unwrap();
        let tied_user = messages
            .iter()
            .position(|message| message.text == "synthetic tie turn")
            .unwrap();
        assert!(previous_reply < tied_user);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
