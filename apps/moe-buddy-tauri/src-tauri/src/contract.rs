use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize)]
pub(super) struct Snapshot {
    pub revision: u64,
    #[serde(rename = "activeBuddyId")]
    pub active_buddy_id: Option<String>,
    #[serde(rename = "buddiesRevision")]
    pub buddies_revision: u64,
    pub buddies: Vec<BuddyView>,
    #[serde(rename = "activeBuddyPersona")]
    pub active_buddy_persona: Option<String>,
    #[serde(rename = "buddyCommit")]
    pub buddy_commit: Option<BuddyCommitView>,
    #[serde(rename = "activeSessionId")]
    pub active_session_id: Option<String>,
    #[serde(rename = "sessionsRevision")]
    pub sessions_revision: u64,
    #[serde(rename = "sessionIsolationAllowed")]
    pub session_isolation_allowed: bool,
    pub sessions: Vec<SessionView>,
    #[serde(rename = "presentationRevision")]
    pub presentation_revision: u64,
    pub mode: String,
    pub revoked: bool,
    pub chat: ChatView,
    #[serde(rename = "closeConfirmation")]
    pub close_confirmation: bool,
    #[serde(rename = "closeIntent")]
    pub close_intent: Option<CloseIntent>,
    #[serde(rename = "managementDraft")]
    pub management_draft: ManagementDraftView,
    #[serde(rename = "managementExit")]
    pub management_exit: Option<ManagementExitView>,
    #[serde(rename = "managementDiscardedVersion")]
    pub management_discarded_version: Option<u64>,
    #[serde(rename = "chatDraft")]
    pub chat_draft: ChatDraftView,
    #[serde(rename = "activePreset")]
    pub active_preset: String,
    #[serde(rename = "externalTransmissionAllowed")]
    pub external_transmission_allowed: bool,
    pub presets: Vec<PresetView>,
    pub memories: Vec<MemoryView>,
    pub diagnostics: Vec<DiagnosticView>,
    pub notice: Option<String>,
    #[serde(rename = "modelSettings")]
    pub model_settings: Option<ModelSettingsView>,
    #[serde(rename = "persistenceEnabled")]
    pub persistence_enabled: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BuddyCommitView {
    pub kind: String,
    pub from_buddy_id: String,
    pub from_revision: u64,
    pub to_buddy_id: String,
    pub to_revision: u64,
    pub management_version: u64,
}

#[derive(Clone, Serialize)]
pub(super) struct SessionView {
    pub id: String,
    pub label: String,
    pub archived: bool,
    pub status: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BuddyView {
    pub id: String,
    pub label: String,
    pub profile_revision: u64,
    pub persona_revision: u64,
    pub model_revision: u64,
}

#[derive(Clone, Serialize)]
pub(super) struct PresetView {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
}

#[derive(Clone, Serialize)]
pub(super) struct MemoryView {
    pub id: String,
    pub revision: u64,
    pub text: String,
}

#[derive(Clone, Serialize)]
pub(super) struct DiagnosticView {
    pub sequence: u64,
    pub run: String,
    pub event: String,
    pub status: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModelSettingsView {
    pub endpoint: String,
    pub model: String,
    pub persona: String,
    pub allow_transmission: bool,
    pub has_api_key: bool,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ManagementDraftView {
    pub version: u64,
    pub dirty: bool,
    pub kind: ManagementDraftKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ManagementDraftKind {
    #[default]
    None,
    General,
    BuddyCreate,
    BuddyRename,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ManagementExitView {
    pub version: u64,
    pub intent: CloseIntent,
}

/// Body-free chat draft projection: the current revision plus the latest
/// authoritative discard receipt and bounded per-session watermarks. It never
/// carries draft text or queue state.
#[derive(Clone, Serialize)]
pub(super) struct ChatDraftView {
    pub revision: u64,
    pub discard: Option<ChatDraftDiscardView>,
    pub sessions: Vec<ChatDraftSessionView>,
}

/// Latest draft-change revision and latest explicit discard revision for one
/// live logical session. `discardedRevision` is null until an explicit discard
/// consumes that session's unsent text.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChatDraftSessionView {
    pub id: String,
    pub revision: u64,
    pub discarded_revision: Option<u64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChatDraftDiscardView {
    pub revision: u64,
    pub session_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CloseIntent {
    CloseChat,
    QuitApplication,
}

#[derive(Clone, Serialize)]
pub(super) struct ChatView {
    pub draft: String,
    pub messages: Vec<MessageView>,
    pub status: String,
    pub failure: Option<String>,
}

#[derive(Clone, Serialize)]
pub(super) struct MessageView {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Serialize)]
pub(super) struct Failure {
    pub code: String,
    pub message: String,
    pub disposition: String,
}

#[derive(Clone, Serialize)]
pub(super) struct DispatchResult {
    pub snapshot: Option<Snapshot>,
    pub error: Option<Failure>,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FrameSnapshot {
    pub resource_generation: u64,
    pub playback_generation: u64,
    pub presentation_sequence: u64,
    pub source: String,
    pub clip_id: Option<String>,
    pub frame_index: Option<u8>,
    pub width: u32,
    pub height: u32,
    pub paused: bool,
    pub faulted: bool,
}

impl FrameSnapshot {
    pub(super) fn builtin(frame_index: u8, paused: bool, faulted: bool) -> Self {
        Self::builtin_with_sequence(frame_index, paused, faulted, 1)
    }

    pub(super) fn builtin_with_sequence(
        frame_index: u8,
        paused: bool,
        faulted: bool,
        presentation_sequence: u64,
    ) -> Self {
        Self {
            resource_generation: 1,
            playback_generation: 1,
            presentation_sequence,
            source: "builtin".into(),
            clip_id: Some("idle-four-frame".into()),
            frame_index: Some(frame_index),
            width: 320,
            height: 320,
            paused,
            faulted,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MaterialDescriptor {
    pub frame_count: u8,
    pub frame_duration_ms: u64,
    pub clip_duration_ms: u64,
}

pub(super) fn material_descriptor() -> &'static MaterialDescriptor {
    static DESCRIPTOR: std::sync::OnceLock<MaterialDescriptor> = std::sync::OnceLock::new();
    DESCRIPTOR.get_or_init(|| {
        serde_json::from_str(include_str!("../../src/core/contract-fixture.json"))
            .expect("valid shared IPC/material contract fixture")
    })
}
