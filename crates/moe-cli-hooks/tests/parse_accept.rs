//! 外部合成测试：应被接纳的载荷，以及未知数据只检查不保留的行为。

use moe_cli_hooks::{VerifiedCliVersion, parse_user_prompt_submit};

const MINIMAL: &str = r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;

fn parse(raw: &[u8]) -> moe_cli_hooks::ClaudePromptSubmission {
    let version = VerifiedCliVersion::from_host_report("2.1.286").unwrap();
    parse_user_prompt_submit(version, raw).expect("payload should be accepted")
}

#[test]
fn minimal_valid_payload_parses() {
    let submission = parse(MINIMAL.as_bytes());
    assert_eq!(submission.session_id(), "s-1");
    assert_eq!(submission.prompt(), "hi");
}

#[test]
fn documented_cli_fields_are_syntax_checked_and_dropped() {
    let raw = r#"{"session_id":"8f0c1a2b","transcript_path":"C:\\Users\\x\\t.jsonl","cwd":"D:\\work","permission_mode":"default","hook_event_name":"UserPromptSubmit","prompt":"你好，🦀！","scratchpad_dir":"C:\\sp","prompt_id":"p-9","agent_type":"main","session_title":"title"}"#;
    let submission = parse(raw.as_bytes());
    assert_eq!(submission.session_id(), "8f0c1a2b");
    assert_eq!(submission.prompt(), "你好，🦀！");
}

#[test]
fn arbitrary_unknown_values_are_checked_and_dropped() {
    let raw = br#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"p","extra":{"a":[1,2,{"b":null}],"c":1.5e300},"big":123456789012345678901234567890,"neg":-17,"flag":false,"none":null,"text":"x"}"#;
    let submission = parse(raw);
    assert_eq!(submission.prompt(), "p");
}

#[test]
fn escape_sequences_decode_exactly() {
    let raw = r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"[\"q\"] a\\b\nc\td\u0001é🦀"}"#;
    let submission = parse(raw.as_bytes());
    assert_eq!(submission.prompt(), "[\"q\"] a\\b\nc\td\u{1}é🦀");
}

#[test]
fn escaped_spelling_of_event_name_is_recognized() {
    let raw = r#"{"session_id":"s","hook_event_name":"\u0055serPromptSubmit","prompt":"p"}"#;
    let submission = parse(raw.as_bytes());
    assert_eq!(submission.session_id(), "s");
    assert_eq!(submission.prompt(), "p");
}

#[test]
fn trailing_whitespace_and_newlines_are_allowed() {
    let raw = format!("{MINIMAL}\n \t\r\n");
    parse(raw.as_bytes());
}

#[test]
fn empty_prompt_is_accepted_as_empty_string() {
    let raw = br#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":""}"#;
    assert_eq!(parse(raw).prompt(), "");
}

#[test]
fn session_id_at_byte_limit_is_accepted() {
    let raw = format!(
        r#"{{"session_id":"{}","hook_event_name":"UserPromptSubmit","prompt":"p"}}"#,
        "a".repeat(moe_cli_hooks::MAX_SESSION_ID_BYTES)
    );
    assert_eq!(parse(raw.as_bytes()).session_id().len(), 256);
}

#[test]
fn unknown_nesting_at_depth_limit_is_accepted() {
    let raw = format!(
        r#"{{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"p","u":{}{}}}"#,
        "[".repeat(63),
        "]".repeat(63)
    );
    parse(raw.as_bytes());
}

#[test]
fn same_key_in_sibling_objects_is_not_a_duplicate() {
    let raw = br#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"p","u":[{"k":1},{"k":2}]}"#;
    parse(raw);
}

#[test]
fn large_bounded_prompt_is_accepted() {
    let raw = format!(
        r#"{{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"{}"}}"#,
        "a".repeat(1_000_000)
    );
    assert_eq!(parse(raw.as_bytes()).prompt().len(), 1_000_000);
}
