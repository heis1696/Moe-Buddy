#![doc = include_str!("../README.md")]

mod codec;
mod host;
mod output;
mod reader;
mod version;

pub use crate::codec::{
    ClaudePromptSubmission, HookInputError, MAX_CONTAINER_DEPTH, MAX_SESSION_ID_BYTES,
    parse_user_prompt_submit,
};
pub use crate::host::{ClaudeHookError, accept_claude_submission, write_claude_context};
pub use crate::output::{
    ContextTransferred, HookWriteError, MAX_ADDITIONAL_CONTEXT_BYTES,
    write_user_prompt_submit_context,
};
pub use crate::reader::{HookInputReadError, MAX_HOOK_INPUT_BYTES, read_bounded};
pub use crate::version::{UnsupportedCliVersion, VERIFIED_CLAUDE_CODE_VERSION, VerifiedCliVersion};
