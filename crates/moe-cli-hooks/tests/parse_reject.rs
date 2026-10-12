//! 外部合成测试：必须拒绝的输入，以及错误信息的脱敏。

use moe_cli_hooks::MAX_HOOK_INPUT_BYTES;

const MINIMAL: &str = r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;
const SECRET: &str = "SECRET_MARKER_7f3a9c";

fn parse_err(raw: &[u8]) -> String {
    let version = moe_cli_hooks::VerifiedCliVersion::from_host_report("2.1.286").unwrap();
    moe_cli_hooks::parse_user_prompt_submit(version, raw)
        .expect_err("payload must be rejected")
        .to_string()
}

fn nested_unknown(depth: usize) -> Vec<u8> {
    format!(
        r#"{{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"p","u":{}{}}}"#,
        "[".repeat(depth),
        "]".repeat(depth)
    )
    .into_bytes()
}

#[test]
fn duplicate_known_top_level_key_is_rejected() {
    let raw = br#"{"session_id":"a","hook_event_name":"UserPromptSubmit","prompt":"p1","session_id":"b"}"#;
    assert!(parse_err(raw).contains("duplicate object key"));
}

#[test]
fn duplicate_unknown_top_level_key_is_rejected() {
    let raw = br#"{"session_id":"a","hook_event_name":"UserPromptSubmit","prompt":"p","unknown":1,"unknown":2}"#;
    assert!(parse_err(raw).contains("duplicate object key"));
}

#[test]
fn escape_equivalent_duplicate_of_known_key_is_rejected() {
    let raw = br#"{"session_id":"a","hook_event_name":"UserPromptSubmit","prompt":"p","\u0070rompt":"other"}"#;
    assert!(parse_err(raw).contains("duplicate object key"));
}

#[test]
fn escape_equivalent_duplicate_of_unknown_key_is_rejected() {
    let raw = br#"{"session_id":"a","hook_event_name":"UserPromptSubmit","prompt":"p","unknown":1,"\u0075nknown":2}"#;
    assert!(parse_err(raw).contains("duplicate object key"));
}

#[test]
fn duplicate_key_inside_nested_unknown_object_is_rejected() {
    let raw = br#"{"session_id":"a","hook_event_name":"UserPromptSubmit","prompt":"p","u":{"k":1,"k":2}}"#;
    assert!(parse_err(raw).contains("duplicate object key"));
}

#[test]
fn duplicate_key_inside_array_element_object_is_rejected() {
    let raw = br#"{"session_id":"a","hook_event_name":"UserPromptSubmit","prompt":"p","u":[{"k":1,"k":2}]}"#;
    assert!(parse_err(raw).contains("duplicate object key"));
}

#[test]
fn trailing_object_is_rejected() {
    let raw = format!("{MINIMAL} {{}}");
    assert!(parse_err(raw.as_bytes()).contains("invalid JSON"));
}

#[test]
fn trailing_garbage_is_rejected() {
    let raw = format!("{MINIMAL} trailing");
    assert!(parse_err(raw.as_bytes()).contains("invalid JSON"));
}

#[test]
fn invalid_utf8_is_rejected() {
    let raw = [b'{', b'"', 0xFF, b'"', b':', b'1', b'}'];
    assert!(parse_err(&raw).contains("not valid UTF-8"));
}

#[test]
fn nesting_one_past_depth_limit_is_rejected() {
    let err = parse_err(&nested_unknown(64));
    assert!(err.contains("depth limit"), "{err}");
}

#[test]
fn input_past_byte_limit_is_rejected() {
    let raw = vec![b'a'; MAX_HOOK_INPUT_BYTES + 1];
    assert!(parse_err(&raw).contains("input limit"));
}

#[test]
fn missing_session_id_is_rejected() {
    let raw = br#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;
    assert!(parse_err(raw).contains("`session_id` is missing"));
}

#[test]
fn missing_prompt_is_rejected() {
    let raw = br#"{"session_id":"s","hook_event_name":"UserPromptSubmit"}"#;
    assert!(parse_err(raw).contains("`prompt` is missing"));
}

#[test]
fn missing_hook_event_name_is_rejected() {
    let raw = br#"{"session_id":"s","prompt":"hi"}"#;
    assert!(parse_err(raw).contains("`hook_event_name` is missing"));
}

#[test]
fn other_event_name_is_rejected() {
    let raw = br#"{"session_id":"s","hook_event_name":"SessionStart","prompt":"hi"}"#;
    assert!(parse_err(raw).contains("hook_event_name is not"));
}

#[test]
fn non_string_known_fields_are_rejected() {
    for raw in [
        r#"{"session_id":42,"hook_event_name":"UserPromptSubmit","prompt":"p"}"#,
        r#"{"session_id":null,"hook_event_name":"UserPromptSubmit","prompt":"p"}"#,
        r#"{"session_id":{},"hook_event_name":"UserPromptSubmit","prompt":"p"}"#,
        r#"{"session_id":"s","hook_event_name":5,"prompt":"p"}"#,
        r#"{"session_id":"s","hook_event_name":null,"prompt":"p"}"#,
        r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":["x"]}"#,
        r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":3.5}"#,
    ] {
        assert!(
            parse_err(raw.as_bytes()).contains("not a JSON string"),
            "{:?}",
            raw
        );
    }
}

#[test]
fn session_id_past_byte_limit_is_rejected() {
    let raw = format!(
        r#"{{"session_id":"{}","hook_event_name":"UserPromptSubmit","prompt":"p"}}"#,
        "y".repeat(257)
    );
    assert!(parse_err(raw.as_bytes()).contains("session_id exceeds"));
}

#[test]
fn non_object_roots_are_rejected() {
    for raw in [
        br#"[]"#.as_slice(),
        br#""just a string""#.as_slice(),
        br#"42"#.as_slice(),
        br#"null"#.as_slice(),
    ] {
        assert!(parse_err(raw).contains("not a JSON object"), "{:?}", raw);
    }
}

#[test]
fn empty_input_is_rejected() {
    assert!(parse_err(b"").contains("invalid JSON"));
}

#[test]
fn truncated_json_is_rejected() {
    let raw = br#"{"session_id":"s-1""#;
    assert!(parse_err(raw).contains("invalid JSON"));
}

#[test]
fn byte_order_mark_is_rejected() {
    let raw = format!("\u{feff}{MINIMAL}");
    assert!(parse_err(raw.as_bytes()).contains("invalid JSON"));
}

#[test]
fn unpaired_surrogate_escape_is_rejected() {
    let raw = br#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"\uD800"}"#;
    assert!(parse_err(raw).contains("invalid JSON"));
}

#[test]
fn error_display_is_redacted_across_failure_modes() {
    let duplicate = format!(
        r#"{{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"{SECRET}","prompt":"{SECRET}"}}"#
    );
    let broken = format!(r#"{{"session_id":"{SECRET}","broken"#);
    let mut oversized = b"{\"prompt\":\"".to_vec();
    oversized.resize(MAX_HOOK_INPUT_BYTES + oversized.len(), b'S');
    let long_id = format!(
        r#"{{"session_id":"{}","hook_event_name":"UserPromptSubmit","prompt":"p"}}"#,
        SECRET.repeat(80)
    );
    for raw in [
        duplicate.into_bytes(),
        broken.into_bytes(),
        oversized,
        long_id.into_bytes(),
    ] {
        let display = parse_err(&raw);
        assert!(!display.contains(SECRET), "leaked input: {display}");
        assert!(!display.contains("SSSS"), "leaked filler: {display}");
    }
}
