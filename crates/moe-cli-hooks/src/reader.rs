use std::fmt;
use std::io::{self, Read};

/// 单次 Hook 调用 stdin 载荷的硬上限。
///
/// docs/GATEWAY.md §G5.3：已核验 CLI 写入一个 UTF-8 JSON 对象加换行后
/// 关闭流。对 2.1.286 而言远超 1 MiB 的 prompt 提交不在预期内；无论生产方
/// 是否经过核验，适配器都必须保持内存有界。
pub const MAX_HOOK_INPUT_BYTES: usize = 1_048_576;

/// 有界读取 Hook 输入流失败的原因。错误不携带任何流内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookInputReadError {
    /// EOF 之前出现了超过 [`MAX_HOOK_INPUT_BYTES`] 的字节量。
    TooLong { limit: usize },
    /// 底层读取器失败；只保留 OS 错误类别。
    ReadFailed { kind: io::ErrorKind },
}

impl fmt::Display for HookInputReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            HookInputReadError::TooLong { limit } => {
                write!(
                    formatter,
                    "hook input stream exceeds the {}-byte input limit",
                    limit
                )
            }
            HookInputReadError::ReadFailed { kind } => {
                write!(
                    formatter,
                    "reading the hook input stream failed (io kind: {})",
                    kind
                )
            }
        }
    }
}

impl std::error::Error for HookInputReadError {}

/// 从 `reader`（通常为 Hook 的 stdin）读取至多 [`MAX_HOOK_INPUT_BYTES`]
/// 字节的原始输入。
///
/// 契约下 CLI 写完单个 JSON 对象与换行后关闭流，因此读到 EOF 即输入结束；
/// 字节内容与 UTF-8 有效性留待 [`crate::parse_user_prompt_submit`] 检查，
/// 本函数只负责读取有界。返回的错误经过脱敏：不含任何已读内容。
pub fn read_bounded<R: Read>(reader: R) -> Result<Vec<u8>, HookInputReadError> {
    let mut limited = reader.take(MAX_HOOK_INPUT_BYTES as u64 + 1);
    let mut buf = Vec::new();
    limited
        .read_to_end(&mut buf)
        .map_err(|error| HookInputReadError::ReadFailed { kind: error.kind() })?;
    if buf.len() > MAX_HOOK_INPUT_BYTES {
        return Err(HookInputReadError::TooLong {
            limit: MAX_HOOK_INPUT_BYTES,
        });
    }
    Ok(buf)
}
