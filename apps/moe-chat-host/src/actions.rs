//! Validation of the `POST /api/action` JSON envelope.
//!
//! The body is turned into a whitelisted [`Action`] only when every field is known, typed
//! and in range. Nothing here executes an action: the caller receives a [`Prepared`] value
//! or a safe public error and never dispatches on a rejected request.

use moe_chat_app::{Action, MAX_TEXT_BYTES, PanelKind};
use serde_json::{Map, Value};

use crate::PublicError;

/// Largest accepted request id, in ASCII bytes.
pub(crate) const MAX_ID_BYTES: usize = 64;
/// Largest accepted preset identifier, in bytes.
const MAX_PRESET_BYTES: usize = 128;

pub(crate) const ERR_BODY: PublicError =
    PublicError::new("malformed_body", "request body must be a JSON object");
pub(crate) const ERR_FIELD: PublicError = PublicError::new(
    "unknown_field",
    "request contains a field that is not permitted",
);
pub(crate) const ERR_ACTION: PublicError =
    PublicError::new("unknown_action", "action is not in the permitted set");
pub(crate) const ERR_SHAPE: PublicError = PublicError::new(
    "invalid_request",
    "request field is missing, mistyped or blank",
);
pub(crate) const ERR_TEXT: PublicError =
    PublicError::new("invalid_text", "text exceeds the UTF-8 byte limit");
pub(crate) const ERR_BLANK: PublicError =
    PublicError::new("invalid_text", "text must be non-blank");

/// A validated request id paired with the action it authorises.
pub(crate) struct Prepared {
    pub(crate) id: String,
    pub(crate) action: Action,
}

/// Validates the JSON body. A parse or schema failure never yields an action.
pub(crate) fn prepare(body: &[u8]) -> Result<Prepared, PublicError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ERR_BODY)?;
    let object = value.as_object().ok_or(ERR_BODY)?;
    let id = string_field(object, "id")?.to_owned();
    if !valid_id(&id) {
        return Err(ERR_SHAPE);
    }
    let action = match string_field(object, "action")? {
        "open" => {
            only_fields(object, &["id", "action", "panel"])?;
            Action::Open(panel_field(object)?)
        }
        "hide" => {
            only_fields(object, &["id", "action", "panel"])?;
            Action::Hide(panel_field(object)?)
        }
        "close" => {
            only_fields(object, &["id", "action", "panel"])?;
            Action::Close(panel_field(object)?)
        }
        "confirm_close" => {
            only_fields(object, &["id", "action", "panel", "discard"])?;
            let panel = panel_field(object)?;
            let discard = object
                .get("discard")
                .and_then(Value::as_bool)
                .ok_or(ERR_SHAPE)?;
            Action::ConfirmClose { panel, discard }
        }
        "set_draft" => {
            only_fields(object, &["id", "action", "text"])?;
            Action::SetDraft(bounded_text(object)?)
        }
        "send" => {
            only_fields(object, &["id", "action"])?;
            Action::Send
        }
        "select_preset" => {
            only_fields(object, &["id", "action", "preset"])?;
            Action::SelectPreset(preset_field(object)?)
        }
        "remember" => {
            only_fields(object, &["id", "action", "text"])?;
            Action::Remember(memory_text(object)?)
        }
        "refresh_memories" => {
            only_fields(object, &["id", "action"])?;
            Action::RefreshMemories
        }
        "refresh_diagnostics" => {
            only_fields(object, &["id", "action"])?;
            Action::RefreshDiagnostics
        }
        "revoke" => {
            only_fields(object, &["id", "action"])?;
            Action::Revoke
        }
        _ => return Err(ERR_ACTION),
    };
    Ok(Prepared { id, action })
}

/// Rejects any key outside the fixed envelope vocabulary.
fn only_fields(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), PublicError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(ERR_FIELD);
        }
    }
    Ok(())
}

fn string_field<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a str, PublicError> {
    object.get(name).and_then(Value::as_str).ok_or(ERR_SHAPE)
}

fn panel_field(object: &Map<String, Value>) -> Result<PanelKind, PublicError> {
    PanelKind::parse(string_field(object, "panel")?).ok_or(ERR_SHAPE)
}

/// Request text: the type must be a string and the value inside the application byte limit.
///
/// Blank text is not refused here: `set_draft` uses this to clear a draft, and the blank
/// judgement for `send` belongs to the application controller.
fn bounded_text(object: &Map<String, Value>) -> Result<String, PublicError> {
    let text = string_field(object, "text")?;
    if text.len() > MAX_TEXT_BYTES {
        return Err(ERR_TEXT);
    }
    Ok(text.to_owned())
}

/// Memory text: bounded like every request text, and additionally never blank.
fn memory_text(object: &Map<String, Value>) -> Result<String, PublicError> {
    let text = bounded_text(object)?;
    if text.trim().is_empty() {
        return Err(ERR_BLANK);
    }
    Ok(text)
}

fn preset_field(object: &Map<String, Value>) -> Result<String, PublicError> {
    let preset = string_field(object, "preset")?;
    if preset.trim().is_empty()
        || preset.len() > MAX_PRESET_BYTES
        || preset.chars().any(char::is_control)
    {
        return Err(ERR_SHAPE);
    }
    Ok(preset.to_owned())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error_code(body: &str) -> &'static str {
        prepare(body.as_bytes())
            .err()
            .expect("body must be refused")
            .code
    }

    fn describe(action: &Action) -> String {
        match action {
            Action::Open(panel) => format!("open:{}", panel.as_str()),
            Action::Hide(panel) => format!("hide:{}", panel.as_str()),
            Action::Close(panel) => format!("close:{}", panel.as_str()),
            Action::ConfirmClose { panel, discard } => {
                format!("confirm_close:{}:{discard}", panel.as_str())
            }
            Action::SetDraft(text) => format!("set_draft:{text}"),
            Action::Send => "send".to_owned(),
            Action::SelectPreset(preset) => format!("select_preset:{preset}"),
            Action::Remember(text) => format!("remember:{text}"),
            Action::RefreshMemories => "refresh_memories".to_owned(),
            Action::RefreshDiagnostics => "refresh_diagnostics".to_owned(),
            Action::Revoke => "revoke".to_owned(),
            Action::SearchMemories(_) => "search_memories".to_owned(),
            Action::UpdateMemory { .. } => "update_memory".to_owned(),
            Action::DeleteMemory { .. } => "delete_memory".to_owned(),
        }
    }

    fn accepted(body: &str) -> (String, String) {
        let prepared = prepare(body.as_bytes()).expect("body must be accepted");
        (prepared.id, describe(&prepared.action))
    }

    #[test]
    fn every_whitelisted_action_is_accepted() {
        let cases = [
            (r#"{"id":"a1","action":"open","panel":"chat"}"#, "open:chat"),
            (
                r#"{"id":"a2","action":"hide","panel":"presets"}"#,
                "hide:presets",
            ),
            (
                r#"{"id":"a3","action":"close","panel":"memory"}"#,
                "close:memory",
            ),
            (
                r#"{"id":"a4","action":"confirm_close","panel":"diagnostics","discard":true}"#,
                "confirm_close:diagnostics:true",
            ),
            (
                r#"{"id":"a5","action":"set_draft","text":"hello"}"#,
                "set_draft:hello",
            ),
            (r#"{"id":"a6","action":"send"}"#, "send"),
            (
                r#"{"id":"a7","action":"select_preset","preset":"offline-default"}"#,
                "select_preset:offline-default",
            ),
            (
                r#"{"id":"a8","action":"remember","text":"likes tea"}"#,
                "remember:likes tea",
            ),
            (
                r#"{"id":"a9","action":"refresh_memories"}"#,
                "refresh_memories",
            ),
            (
                r#"{"id":"a10","action":"refresh_diagnostics"}"#,
                "refresh_diagnostics",
            ),
            (r#"{"id":"a11","action":"revoke"}"#, "revoke"),
        ];
        for (body, expected) in cases {
            let (id, described) = accepted(body);
            assert!(!id.is_empty());
            assert_eq!(described, expected, "body {body}");
        }
    }

    #[test]
    fn unknown_action_and_unknown_field_are_refused() {
        assert_eq!(
            error_code(r#"{"id":"a","action":"delete_everything"}"#),
            ERR_ACTION.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"send","text":"x"}"#),
            ERR_FIELD.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"send","secret":"x"}"#),
            ERR_FIELD.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"open","panel":"chat","text":"x"}"#),
            ERR_FIELD.code
        );
    }

    #[test]
    fn wrong_types_missing_fields_and_blank_memory_are_refused() {
        assert_eq!(
            error_code(r#"{"id":"a","action":"set_draft","text":5}"#),
            ERR_SHAPE.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"send","text":5}"#),
            ERR_FIELD.code
        );
        assert_eq!(error_code(r#"{"action":"send"}"#), ERR_SHAPE.code);
        assert_eq!(error_code(r#"{"id":"a"}"#), ERR_SHAPE.code);
        assert_eq!(
            error_code(r#"{"id":"a","action":"set_draft"}"#),
            ERR_SHAPE.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"remember","text":"\t\n"}"#),
            ERR_TEXT.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"open","panel":"unknown"}"#),
            ERR_SHAPE.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"confirm_close","panel":"chat","discard":"yes"}"#),
            ERR_SHAPE.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"select_preset","preset":" "}"#),
            ERR_SHAPE.code
        );
    }

    #[test]
    fn malformed_json_and_non_objects_are_refused() {
        assert_eq!(error_code("{"), ERR_BODY.code);
        assert_eq!(error_code("[]"), ERR_BODY.code);
        assert_eq!(error_code("\"send\""), ERR_BODY.code);
        assert_eq!(error_code(""), ERR_BODY.code);
    }

    #[test]
    fn request_ids_are_bounded_ascii_tokens() {
        assert_eq!(error_code(r#"{"id":"","action":"send"}"#), ERR_SHAPE.code);
        let long_id = "a".repeat(MAX_ID_BYTES + 1);
        assert_eq!(
            error_code(&format!(r#"{{"id":"{long_id}","action":"send"}}"#)),
            ERR_SHAPE.code
        );
        assert_eq!(
            error_code(r#"{"id":"bad id","action":"send"}"#),
            ERR_SHAPE.code
        );
        assert_eq!(
            error_code(r#"{"id":"a/b","action":"send"}"#),
            ERR_SHAPE.code
        );
        assert!(prepare(br#"{"id":"AZaz09_-","action":"send"}"#.as_slice()).is_ok());
    }

    #[test]
    fn text_at_the_utf8_byte_limit_is_accepted_and_one_byte_more_is_refused() {
        let exact = "あ".repeat(MAX_TEXT_BYTES / 3);
        assert!(exact.len() <= MAX_TEXT_BYTES);
        let body = format!(r#"{{"id":"a","action":"set_draft","text":"{exact}"}}"#);
        assert!(prepare(body.as_bytes()).is_ok());

        let over = "a".repeat(MAX_TEXT_BYTES + 1);
        let body = format!(r#"{{"id":"a","action":"set_draft","text":"{over}"}}"#);
        assert_eq!(error_code(&body), ERR_TEXT.code);
    }

    #[test]
    fn escaped_text_still_counts_as_utf8_bytes_not_json_characters() {
        let escaped = r"\u0041".repeat(MAX_TEXT_BYTES);
        let body = format!(r#"{{"id":"a","action":"remember","text":"{escaped}"}}"#);
        let prepared = prepare(body.as_bytes()).expect("escaped text is legal");
        match prepared.action {
            Action::Remember(text) => assert_eq!(text.len(), MAX_TEXT_BYTES),
            other => panic!("unexpected action {}", describe(&other)),
        }
    }

    #[test]
    fn set_draft_accepts_empty_and_whitespace_text_byte_for_byte() {
        for text in ["", " ", "   ", "\t\n", "\u{a0}", "\u{3000}\u{a0}"] {
            let body = serde_json::json!({"id": "a", "action": "set_draft", "text": text});
            let prepared = prepare(body.to_string().as_bytes())
                .unwrap_or_else(|error| panic!("blank draft {text:?} refused as {}", error.code));
            match prepared.action {
                Action::SetDraft(stored) => assert_eq!(stored, text, "draft {text:?}"),
                other => panic!("unexpected action {}", describe(&other)),
            }
        }

        let exact = "a".repeat(MAX_TEXT_BYTES);
        let body = serde_json::json!({"id": "a", "action": "set_draft", "text": exact});
        match prepare(body.to_string().as_bytes())
            .expect("exact byte budget")
            .action
        {
            Action::SetDraft(stored) => assert_eq!(stored.len(), MAX_TEXT_BYTES),
            other => panic!("unexpected action {}", describe(&other)),
        }
    }

    #[test]
    fn set_draft_still_refuses_wrong_types_unknown_fields_and_oversized_text() {
        assert_eq!(
            error_code(r#"{"id":"a","action":"set_draft","text":null}"#),
            ERR_SHAPE.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"set_draft","text":[]}"#),
            ERR_SHAPE.code
        );
        assert_eq!(
            error_code(r#"{"id":"a","action":"set_draft","text":"","extra":1}"#),
            ERR_FIELD.code
        );

        let over = serde_json::json!({
            "id": "a",
            "action": "set_draft",
            "text": "a".repeat(MAX_TEXT_BYTES + 1),
        });
        assert_eq!(error_code(&over.to_string()), ERR_TEXT.code);

        let escaped = format!(
            r#"{{"id":"a","action":"set_draft","text":"{}"}}"#,
            r"\u0041".repeat(MAX_TEXT_BYTES + 1)
        );
        assert_eq!(error_code(&escaped), ERR_TEXT.code);
    }

    #[test]
    fn remember_still_refuses_blank_text_and_keeps_the_byte_limit() {
        for body in [
            r#"{"id":"a","action":"remember","text":""}"#,
            r#"{"id":"a","action":"remember","text":"   "}"#,
            r#"{"id":"a","action":"remember","text":"\t\n"}"#,
        ] {
            assert_eq!(error_code(body), ERR_BLANK.code, "{body}");
        }

        let blank = serde_json::json!({"id": "a", "action": "remember", "text": "\u{a0}\u{3000}"});
        assert_eq!(error_code(&blank.to_string()), ERR_BLANK.code);
        assert_eq!(
            error_code(r#"{"id":"a","action":"remember","text":5}"#),
            ERR_SHAPE.code
        );

        let over = serde_json::json!({
            "id": "a",
            "action": "remember",
            "text": "a".repeat(MAX_TEXT_BYTES + 1),
        });
        assert_eq!(error_code(&over.to_string()), ERR_TEXT.code);

        let bounded = serde_json::json!({
            "id": "a",
            "action": "remember",
            "text": "a".repeat(MAX_TEXT_BYTES),
        });
        match prepare(bounded.to_string().as_bytes())
            .expect("bounded memory text")
            .action
        {
            Action::Remember(stored) => assert_eq!(stored.len(), MAX_TEXT_BYTES),
            other => panic!("unexpected action {}", describe(&other)),
        }
    }

    #[test]
    fn send_never_carries_text_so_blankness_stays_with_the_application() {
        assert_eq!(
            error_code(r#"{"id":"a","action":"send","text":""}"#),
            ERR_FIELD.code
        );
        match prepare(br#"{"id":"a","action":"send"}"#.as_slice())
            .expect("send carries no text")
            .action
        {
            Action::Send => {}
            other => panic!("unexpected action {}", describe(&other)),
        }
    }
}
