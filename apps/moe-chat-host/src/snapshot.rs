//! Read-only rendering of [`AppSnapshot`] into the browser JSON shape.
//!
//! Field names follow `moe-chat-app/src/contract.rs` and every enum is rendered through its
//! `as_str()` spelling, so the browser never sees a Rust variant name.

use moe_chat_app::{
    AppSnapshot, ChatSnapshot, DiagnosticSnapshot, MemorySnapshot, PanelSnapshot, PresetSnapshot,
};
use serde_json::{Value, json};

pub(crate) fn to_json(snapshot: &AppSnapshot) -> Value {
    json!({
        "revision": snapshot.revision,
        "mode": snapshot.mode,
        "companion": snapshot.companion,
        "active_preset": snapshot.active_preset,
        "external_transmission_allowed": snapshot.external_transmission_allowed,
        "revoked": snapshot.revoked,
        "notice": snapshot.notice,
        "panels": snapshot.panels.iter().map(panel_json).collect::<Vec<_>>(),
        "chat": chat_json(&snapshot.chat),
        "presets": snapshot.presets.iter().map(preset_json).collect::<Vec<_>>(),
        "memories": snapshot.memories.iter().map(memory_json).collect::<Vec<_>>(),
        "diagnostics": snapshot.diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>(),
    })
}

fn panel_json(panel: &PanelSnapshot) -> Value {
    json!({
        "kind": panel.kind.as_str(),
        "visibility": panel.visibility.as_str(),
        "confirmation": panel.confirmation,
        "mounts": panel.mounts,
        "renders": panel.renders,
        "unmounts": panel.unmounts,
    })
}

fn chat_json(chat: &ChatSnapshot) -> Value {
    json!({
        "draft": chat.draft,
        "status": chat.status.as_str(),
        "failure": chat.failure,
        "messages": chat
            .messages
            .iter()
            .map(|message| json!({"role": message.role.as_str(), "text": message.text}))
            .collect::<Vec<_>>(),
    })
}

fn preset_json(preset: &PresetSnapshot) -> Value {
    json!({
        "id": preset.id,
        "label": preset.label,
        "provider": preset.provider,
        "model": preset.model,
        "endpoint": preset.endpoint,
    })
}

fn memory_json(memory: &MemorySnapshot) -> Value {
    json!({"id": memory.id, "text": memory.text})
}

fn diagnostic_json(diagnostic: &DiagnosticSnapshot) -> Value {
    json!({
        "sequence": diagnostic.sequence,
        "run": diagnostic.run,
        "event": diagnostic.event,
        "status": diagnostic.status,
    })
}

#[cfg(test)]
mod tests {
    use moe_chat_app::{ChatMessage, ChatStatus, MessageRole, PanelKind, PanelVisibility};

    use super::*;

    fn fixture() -> AppSnapshot {
        AppSnapshot {
            revision: 7,
            mode: "offline",
            companion: "moe",
            active_preset: "offline-default".to_owned(),
            external_transmission_allowed: false,
            revoked: false,
            notice: None,
            panels: vec![PanelSnapshot {
                kind: PanelKind::Chat,
                visibility: PanelVisibility::Visible,
                confirmation: false,
                mounts: 1,
                renders: 3,
                unmounts: 0,
            }],
            chat: ChatSnapshot {
                draft: "hi".to_owned(),
                messages: vec![ChatMessage {
                    role: MessageRole::User,
                    text: "hello".to_owned(),
                }],
                status: ChatStatus::Ready,
                failure: None,
            },
            presets: vec![PresetSnapshot {
                id: "p1".to_owned(),
                label: "Offline".to_owned(),
                provider: "none".to_owned(),
                model: "fixture".to_owned(),
                endpoint: "memory".to_owned(),
            }],
            memories: vec![MemorySnapshot {
                revision: 1,
                id: "m1".to_owned(),
                text: "likes tea".to_owned(),
            }],
            diagnostics: vec![DiagnosticSnapshot {
                sequence: 4,
                run: "run-1".to_owned(),
                event: "dispatch".to_owned(),
                status: "completed".to_owned(),
            }],
        }
    }

    #[test]
    fn snapshot_uses_contract_field_names_and_enum_spellings() {
        let rendered = to_json(&fixture());
        assert_eq!(rendered["revision"], 7);
        assert_eq!(rendered["mode"], "offline");
        assert_eq!(rendered["companion"], "moe");
        assert_eq!(rendered["external_transmission_allowed"], false);
        assert_eq!(rendered["revoked"], false);
        assert!(rendered["notice"].is_null());
        assert_eq!(rendered["panels"][0]["kind"], "chat");
        assert_eq!(rendered["panels"][0]["visibility"], "visible");
        assert_eq!(rendered["panels"][0]["mounts"], 1);
        assert_eq!(rendered["chat"]["status"], "ready");
        assert!(rendered["chat"]["failure"].is_null());
        assert_eq!(rendered["chat"]["messages"][0]["role"], "user");
        assert_eq!(rendered["presets"][0]["endpoint"], "memory");
        assert_eq!(rendered["memories"][0]["text"], "likes tea");
        assert_eq!(rendered["diagnostics"][0]["event"], "dispatch");
    }

    #[test]
    fn snapshot_renders_notice_and_failure_when_present() {
        let mut snapshot = fixture();
        snapshot.notice = Some("offline".to_owned());
        snapshot.chat.failure = Some("unknown".to_owned());
        let rendered = to_json(&snapshot);
        assert_eq!(rendered["notice"], "offline");
        assert_eq!(rendered["chat"]["failure"], "unknown");
    }
}
