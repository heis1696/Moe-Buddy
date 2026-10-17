//! Narrow commands and read-only snapshots for the trusted text-chat host.

pub const MAX_TEXT_BYTES: usize = 8_000;

/// Trusted desktop settings. Consent covers persona, authorized private notes and
/// the current conversation sent to this exact endpoint. Credentials stay in the host.
#[derive(Clone)]
pub struct ModelSettings {
    pub endpoint: String,
    pub model: String,
    pub persona: String,
    pub allow_transmission: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Chat,
    Presets,
    Memory,
    Diagnostics,
}

impl PanelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Presets => "presets",
            Self::Memory => "memory",
            Self::Diagnostics => "diagnostics",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "chat" => Some(Self::Chat),
            "presets" => Some(Self::Presets),
            "memory" => Some(Self::Memory),
            "diagnostics" => Some(Self::Diagnostics),
            _ => None,
        }
    }
}

pub enum Action {
    Open(PanelKind),
    Hide(PanelKind),
    Close(PanelKind),
    ConfirmClose {
        panel: PanelKind,
        discard: bool,
    },
    SetDraft(String),
    Send,
    SelectPreset(String),
    Remember(String),
    /// Search authorized private notes; an empty query lists recent notes.
    SearchMemories(String),
    /// Replace an explicitly selected note, rejecting stale revisions.
    UpdateMemory {
        id: String,
        revision: u64,
        text: String,
    },
    /// Delete an explicitly confirmed note, rejecting stale revisions.
    DeleteMemory {
        id: String,
        revision: u64,
    },
    RefreshMemories,
    RefreshDiagnostics,
    Revoke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelVisibility {
    Visible,
    Hidden,
    Closed,
}

impl PanelVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Hidden => "hidden",
            Self::Closed => "closed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatStatus {
    Ready,
    Sending,
    Failed,
    Unknown,
}

impl ChatStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Sending => "sending",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone)]
pub struct AppSnapshot {
    pub revision: u64,
    pub mode: &'static str,
    pub companion: &'static str,
    pub active_preset: String,
    pub external_transmission_allowed: bool,
    pub revoked: bool,
    pub panels: Vec<PanelSnapshot>,
    pub chat: ChatSnapshot,
    pub presets: Vec<PresetSnapshot>,
    pub memories: Vec<MemorySnapshot>,
    pub diagnostics: Vec<DiagnosticSnapshot>,
    pub notice: Option<String>,
}

#[derive(Clone)]
pub struct PanelSnapshot {
    pub kind: PanelKind,
    pub visibility: PanelVisibility,
    pub confirmation: bool,
    pub mounts: u64,
    pub renders: u64,
    pub unmounts: u64,
}

#[derive(Clone)]
pub struct ChatSnapshot {
    pub draft: String,
    pub messages: Vec<ChatMessage>,
    pub status: ChatStatus,
    pub failure: Option<String>,
}

#[derive(Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Clone)]
pub struct PresetSnapshot {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
}

#[derive(Clone)]
pub struct MemorySnapshot {
    pub id: String,
    pub revision: u64,
    pub text: String,
}

#[derive(Clone)]
pub struct DiagnosticSnapshot {
    pub sequence: u64,
    pub run: String,
    pub event: String,
    pub status: String,
}
