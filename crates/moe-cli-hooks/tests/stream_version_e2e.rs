//! 外部合成测试：有界流读取、版本识别、Debug 脱敏与端到端链路。

use std::io::{self, Read};

use moe_cli_hooks::{
    HookInputReadError, MAX_HOOK_INPUT_BYTES, VerifiedCliVersion, parse_user_prompt_submit,
    read_bounded, write_user_prompt_submit_context,
};

const MINIMAL: &str = r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;

#[test]
fn reads_the_whole_stream_including_newline() {
    let payload = format!("{MINIMAL}\n");
    let bytes = read_bounded(payload.as_bytes()).unwrap();
    assert_eq!(bytes, payload.as_bytes());
}

#[test]
fn stream_one_past_limit_is_rejected() {
    let payload = vec![b' '; MAX_HOOK_INPUT_BYTES + 1];
    let err = read_bounded(&payload[..]).unwrap_err();
    assert!(matches!(err, HookInputReadError::TooLong { .. }));
    assert!(err.to_string().contains("input limit"));
}

#[test]
fn stream_exactly_at_limit_is_accepted() {
    let payload = vec![b' '; MAX_HOOK_INPUT_BYTES];
    read_bounded(&payload[..]).unwrap();
}

#[test]
fn io_failure_reports_only_the_error_kind() {
    struct Boom;

    impl Read for Boom {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "no"))
        }
    }

    let err = read_bounded(Boom).unwrap_err();
    assert!(matches!(
        err,
        HookInputReadError::ReadFailed {
            kind: io::ErrorKind::PermissionDenied
        }
    ));
}

#[test]
fn host_report_accepts_only_the_verified_token() {
    let version = VerifiedCliVersion::from_host_report("2.1.286").unwrap();
    assert_eq!(version.as_str(), "2.1.286");
    assert_eq!(VerifiedCliVersion::CLAUDE_CODE_2_1_286.as_str(), "2.1.286");

    for rejected in [
        "2.1.285",
        "2.1.287",
        "2.2.0",
        "2.1.286 (Claude Code)",
        "v2.1.286",
        " 2.1.286",
        "2.1.286\n",
        "",
    ] {
        let err = VerifiedCliVersion::from_host_report(rejected)
            .expect_err("unknown version must be rejected");
        assert!(err.to_string().contains("2.1.286"), "{err}");
    }
}

#[test]
fn debug_output_redacts_prompt_and_session_id() {
    let raw = r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"PROMPT_SECRET_TEXT_🦀"}"#;
    let version = VerifiedCliVersion::from_host_report("2.1.286").unwrap();
    let submission = parse_user_prompt_submit(version, raw.as_bytes()).unwrap();
    let debug = format!("{submission:?}");
    assert!(debug.contains("ClaudePromptSubmission"));
    assert!(!debug.contains("s-1"));
    assert!(debug.contains("redacted"));
    assert!(!debug.contains("PROMPT_SECRET"));
}

#[test]
fn end_to_end_read_parse_write_round_trip() {
    let payload = concat!(
        r#"{"session_id":"abc-123","hook_event_name":"UserPromptSubmit","prompt":"帮助我回忆上次的内容",""#,
        r#"transcript_path":"C:\\x\\t.jsonl","permission_mode":"bypassPermissions","role":"admin","cancel":true}"#,
        "\n"
    );
    let bytes = read_bounded(payload.as_bytes()).unwrap();
    let version = VerifiedCliVersion::from_host_report("2.1.286").unwrap();
    let submission = parse_user_prompt_submit(version, &bytes).unwrap();
    assert_eq!(submission.prompt(), "帮助我回忆上次的内容");

    let mut out = Vec::new();
    write_user_prompt_submit_context(&mut out, "【已登记上下文】仅注入，不代表发言").unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        value["hookSpecificOutput"]["hookEventName"]
            .as_str()
            .unwrap(),
        "UserPromptSubmit"
    );
    assert_eq!(
        value["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap(),
        "【已登记上下文】仅注入，不代表发言"
    );
}

#[test]
fn end_to_end_trailing_junk_after_stream_payload_fails_closed() {
    let payload = format!("{MINIMAL}\n{{}}\n");
    let bytes = read_bounded(payload.as_bytes()).unwrap();
    let version = VerifiedCliVersion::from_host_report("2.1.286").unwrap();
    let err = parse_user_prompt_submit(version, &bytes).unwrap_err();
    assert!(err.to_string().contains("invalid JSON"));
}
