use std::fmt;
use std::io::Write;

use serde::ser::{Serialize, SerializeMap, Serializer};

/// 已登记上下文字符串的硬上限，防御宿主侧误用并保持输出有界。
pub const MAX_ADDITIONAL_CONTEXT_BYTES: usize = 1_048_576;

const KEY_HOOK_SPECIFIC_OUTPUT: &str = "hookSpecificOutput";
const KEY_HOOK_EVENT_NAME: &str = "hookEventName";
const KEY_ADDITIONAL_CONTEXT: &str = "additionalContext";
const OUTPUT_EVENT_NAME: &str = "UserPromptSubmit";
const TRAILING_NEWLINE: &[u8] = b"\n";

/// 传输完成的凭据 token：仅证明目的地写入器已接受编码字节且 flush 完成。
///
/// 它不表示 CLI 已解析、显示或采纳该上下文，不表示 prompt 被阻断或放行，
/// 也不构成任何外部系统已消费的证明（docs/GATEWAY.md §G5.2：
/// 写后账本至多证明宿主输出传输完成）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextTransferred {
    _sealed: (),
}

/// 写出 Hook 输出失败。错误经过脱敏：不携带上下文文本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookWriteError {
    /// 上下文超过 [`MAX_ADDITIONAL_CONTEXT_BYTES`]。
    ContextTooLong { limit: usize },
    /// 输出对象编码失败（固定说明，不含正文，也不含写入器信息）。
    EncodeFailed,
    /// 写入器失败；只保留 OS 错误类别。本 crate 不会向输出流追加任何
    /// 诊断正文。
    WriteFailed { kind: std::io::ErrorKind },
}

impl fmt::Display for HookWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            HookWriteError::ContextTooLong { limit } => {
                write!(
                    formatter,
                    "additionalContext exceeds the {}-byte limit",
                    limit
                )
            }
            HookWriteError::EncodeFailed => {
                formatter.write_str("encoding the hook output object failed")
            }
            HookWriteError::WriteFailed { kind } => {
                write!(
                    formatter,
                    "writing the hook output object failed (io kind: {})",
                    kind
                )
            }
        }
    }
}

impl std::error::Error for HookWriteError {}

struct OutputObject<'a> {
    additional_context: &'a str,
}

impl Serialize for OutputObject<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut root = serializer.serialize_map(Some(1))?;
        root.serialize_entry(
            KEY_HOOK_SPECIFIC_OUTPUT,
            &HookSpecificOutput {
                additional_context: self.additional_context,
            },
        )?;
        root.end()
    }
}

struct HookSpecificOutput<'a> {
    additional_context: &'a str,
}

impl Serialize for HookSpecificOutput<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry(KEY_HOOK_EVENT_NAME, OUTPUT_EVENT_NAME)?;
        map.serialize_entry(KEY_ADDITIONAL_CONTEXT, self.additional_context)?;
        map.end()
    }
}

/// 编码并写出本适配器对 `UserPromptSubmit` 唯一允许的输出：单个紧凑
/// JSON 对象
/// `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":…}}`
/// 加一个换行，字符串转义由 serde_json 按 JSON 规范完成。
///
/// 输出是 **InjectedContext**：不是用户发言、不是可见助手回复、不是工具
/// 回执。上下文字符串必须是宿主显式审核并登记的内容；本 crate 不从记忆
/// 读取、不调用模型、不自行生成正文，也不输出该对象之外的任何语义字段。
///
/// 实现先把对象编码进内存缓冲，再一次交给写入器。失败时返回脱敏错误，
/// 且此后不向输出流追加任何诊断正文；中途失败时已交给写入器的字节
/// （失败前缀）无法撤回。无论成败，本函数都不宣称 CLI 已消费输出，
/// 也不宣称 prompt 被阻断。
///
/// # Examples
///
/// ```
/// use moe_cli_hooks::write_user_prompt_submit_context;
///
/// let mut out: Vec<u8> = Vec::new();
/// write_user_prompt_submit_context(&mut out, "remembered: user likes tea").unwrap();
/// assert_eq!(
///     String::from_utf8(out).unwrap(),
///     "{\"hookSpecificOutput\":{\"hookEventName\":\"UserPromptSubmit\",\
///       \"additionalContext\":\"remembered: user likes tea\"}}\n"
/// );
/// ```
pub fn write_user_prompt_submit_context<W: Write>(
    mut writer: W,
    additional_context: &str,
) -> Result<ContextTransferred, HookWriteError> {
    if additional_context.len() > MAX_ADDITIONAL_CONTEXT_BYTES {
        return Err(HookWriteError::ContextTooLong {
            limit: MAX_ADDITIONAL_CONTEXT_BYTES,
        });
    }
    let mut encoded = Vec::new();
    serde_json::to_writer(&mut encoded, &OutputObject { additional_context })
        .map_err(|_| HookWriteError::EncodeFailed)?;
    encoded.extend_from_slice(TRAILING_NEWLINE);
    writer
        .write_all(&encoded)
        .and_then(|()| writer.flush())
        .map_err(|error| HookWriteError::WriteFailed { kind: error.kind() })?;
    Ok(ContextTransferred { _sealed: () })
}
