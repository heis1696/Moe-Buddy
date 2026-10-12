//! 外部合成测试：单对象上下文输出的字节精确性、转义与失败脱敏。

use std::io::{self, Write};

use moe_cli_hooks::{
    HookWriteError, MAX_ADDITIONAL_CONTEXT_BYTES, write_user_prompt_submit_context,
};

#[test]
fn writes_exact_single_object_bytes() {
    let mut out = Vec::new();
    write_user_prompt_submit_context(&mut out, "hello").unwrap();
    assert_eq!(
        out,
        &b"{\"hookSpecificOutput\":{\"hookEventName\":\"UserPromptSubmit\",\"additionalContext\":\"hello\"}}\n"[..]
    );
}

#[test]
fn special_characters_are_escaped_and_round_trip() {
    let context = "quote:\" back:\\ newline:\n tab:\t ctrl:\u{1} emoji:🦀 slash:/ del:\u{7f}";
    let mut out = Vec::new();
    write_user_prompt_submit_context(&mut out, context).unwrap();

    let expected = "{\"hookSpecificOutput\":{\"hookEventName\":\"UserPromptSubmit\",\"additionalContext\":\"quote:\\\" back:\\\\ newline:\\n tab:\\t ctrl:\\u0001 emoji:🦀 slash:/ del:\u{7f}\"}}\n";
    assert_eq!(String::from_utf8(out.clone()).unwrap(), expected);

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
        context
    );
    assert!(out[..out.len() - 1].iter().all(|byte| *byte >= 0x20));
}

#[test]
fn empty_context_is_accepted() {
    let mut out = Vec::new();
    write_user_prompt_submit_context(&mut out, "").unwrap();
    assert_eq!(
        out,
        &b"{\"hookSpecificOutput\":{\"hookEventName\":\"UserPromptSubmit\",\"additionalContext\":\"\"}}\n"[..]
    );
}

#[test]
fn context_past_limit_is_rejected_without_touching_the_writer() {
    let context = "a".repeat(MAX_ADDITIONAL_CONTEXT_BYTES + 1);
    let mut out = Vec::new();
    let err = write_user_prompt_submit_context(&mut out, &context).unwrap_err();
    assert_eq!(
        err,
        HookWriteError::ContextTooLong {
            limit: MAX_ADDITIONAL_CONTEXT_BYTES
        }
    );
    assert!(out.is_empty());
}

struct AlwaysFail;

impl Write for AlwaysFail {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "boom"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn write_failure_is_redacted() {
    let context = "TOP_SECRET_CONTEXT_🦀 must not leak";
    let err = write_user_prompt_submit_context(AlwaysFail, context).unwrap_err();
    assert!(matches!(err, HookWriteError::WriteFailed { .. }));
    assert!(!err.to_string().contains("TOP_SECRET"));
    assert!(!err.to_string().contains("leak"));
}

struct FailAfter {
    budget: usize,
    written: Vec<u8>,
}

impl Write for FailAfter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.budget == 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stop"));
        }
        let count = buf.len().min(self.budget);
        self.written.extend_from_slice(&buf[..count]);
        self.budget -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn partial_write_failure_writes_payload_prefix_only() {
    let context = "REDACTION_CHECK_secret_context";
    let mut writer = FailAfter {
        budget: 20,
        written: Vec::new(),
    };
    let err = write_user_prompt_submit_context(&mut writer, context).unwrap_err();
    assert!(matches!(err, HookWriteError::WriteFailed { .. }));
    assert!(!err.to_string().contains("REDACTION_CHECK"));
    let full = format!(
        "{{\"hookSpecificOutput\":{{\"hookEventName\":\"UserPromptSubmit\",\"additionalContext\":\"{context}\"}}}}\n"
    );
    let written = std::str::from_utf8(&writer.written).unwrap();
    assert!(full.starts_with(written), "{written:?}");
    assert!(writer.written.len() <= 20);
}

struct ZeroProgress;

impl Write for ZeroProgress {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn zero_progress_writer_terminates_with_error() {
    let err = write_user_prompt_submit_context(ZeroProgress, "ctx").unwrap_err();
    assert!(matches!(
        err,
        HookWriteError::WriteFailed {
            kind: io::ErrorKind::WriteZero
        }
    ));
}

#[test]
fn token_and_error_types_are_sealed() {
    fn assert_error<T: std::error::Error + 'static>() {}
    assert_error::<HookWriteError>();
    assert_error::<moe_cli_hooks::HookInputError>();
    assert_error::<moe_cli_hooks::HookInputReadError>();
    assert_error::<moe_cli_hooks::UnsupportedCliVersion>();

    let mut out = Vec::new();
    let token = write_user_prompt_submit_context(&mut out, "x").unwrap();
    let copied = token;
    assert_eq!(token, copied); // Copy + PartialEq 可比
}
