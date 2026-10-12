use std::fmt;

/// 本适配器取得过核验证据的唯一 Claude Code 契约版本
/// （docs/GATEWAY.md §G5.3：本机 `claude --version` 为 2.1.286，
/// 只读核验了包元数据与 bin/claude.exe 的字节区间；滚动官方文档不构成
/// 本机版本保证）。升级 CLI 后必须重新核验并显式修改代码，其余版本一律拒绝。
pub const VERIFIED_CLAUDE_CODE_VERSION: &str = "2.1.286";

/// 显式固定的 CLI 契约版本。
///
/// Hook 载荷本身不含版本字段，版本只能在流之外建立。宿主必须通过本类型
/// 显式指名版本；它只能为 [`VERIFIED_CLAUDE_CODE_VERSION`] 构造，未知版本
/// 无法到达 codec。支持其它版本需要新的契约核验与有意的代码修改，
/// 而不是新的输入取值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedCliVersion(&'static str);

impl VerifiedCliVersion {
    /// 唯一经核验的版本：Claude Code 2.1.286。
    pub const CLAUDE_CODE_2_1_286: Self = Self(VERIFIED_CLAUDE_CODE_VERSION);

    /// 仅当宿主报告的版本字符串精确等于 `"2.1.286"` 时接受。
    ///
    /// `"2.1.286 (Claude Code)"`、`"v2.1.286"`、任何更新或更旧的构建都被
    /// 拒绝；宿主应传入已核验的版本 token。对其它 CLI 构建使用本适配器
    /// 之前必须先按 docs/GATEWAY.md §G5.3 重新核验。
    pub fn from_host_report(candidate: &str) -> Result<Self, UnsupportedCliVersion> {
        if candidate == VERIFIED_CLAUDE_CODE_VERSION {
            Ok(Self::CLAUDE_CODE_2_1_286)
        } else {
            Err(UnsupportedCliVersion)
        }
    }

    /// 被固定的版本 token。
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

/// 拒绝未经核验的 CLI 版本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedCliVersion;

impl fmt::Display for UnsupportedCliVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported CLI version: this adapter is verified only against Claude Code {}",
            VERIFIED_CLAUDE_CODE_VERSION
        )
    }
}

impl std::error::Error for UnsupportedCliVersion {}
