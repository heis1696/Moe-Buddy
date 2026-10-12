use std::fmt;
use std::io::{self, Write};

use moe_agent_runtime::{
    GatewayError, HookBinding, HookEvent, HookInvocation, HookReceipt, HookRuntime,
    HookTransportOutcome, TrustedIngress,
};

use crate::{ClaudePromptSubmission, HookWriteError, write_user_prompt_submit_context};

#[derive(Debug, PartialEq, Eq)]
pub enum ClaudeHookError {
    Runtime(GatewayError),
    Output(HookWriteError),
}

impl fmt::Display for ClaudeHookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(formatter, "hook authorization failed: {error}"),
            Self::Output(error) => write!(formatter, "hook output failed: {error}"),
        }
    }
}

impl std::error::Error for ClaudeHookError {}

pub fn accept_claude_submission(
    runtime: &mut HookRuntime,
    ingress: &TrustedIngress,
    binding: &HookBinding,
    submission: ClaudePromptSubmission,
    now_ms: u64,
) -> Result<HookInvocation, ClaudeHookError> {
    runtime
        .accept_event(
            ingress,
            binding,
            HookEvent::prompt_submitted(submission.session_id(), submission.prompt()),
            now_ms,
        )
        .map_err(ClaudeHookError::Runtime)
}

struct CountingWriter<Writer> {
    inner: Writer,
    written: usize,
}

impl<Writer: Write> Write for CountingWriter<Writer> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.written += count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub fn write_claude_context<Writer: Write, Clock: FnMut() -> u64>(
    runtime: &mut HookRuntime,
    ingress: &TrustedIngress,
    binding: &HookBinding,
    invocation: &HookInvocation,
    writer: Writer,
    mut now_ms: Clock,
) -> Result<HookReceipt, ClaudeHookError> {
    let prepared_at = now_ms();
    let reply = match runtime.prepare_reply(ingress, binding, invocation, prepared_at) {
        Ok(reply) => reply,
        Err(error) => {
            let _ = runtime.fail_invocation(invocation, HookTransportOutcome::Failed, prepared_at);
            return Err(ClaudeHookError::Runtime(error));
        }
    };
    let write_started_at = now_ms();
    if let Err(error) =
        runtime.authorize_before_write(ingress, binding, invocation, &reply, write_started_at)
    {
        let _ = runtime.fail_invocation(
            invocation,
            HookTransportOutcome::Failed,
            prepared_at.max(write_started_at),
        );
        return Err(ClaudeHookError::Runtime(error));
    }
    let mut output = CountingWriter {
        inner: writer,
        written: 0,
    };
    let result = write_user_prompt_submit_context(&mut output, reply.text());
    let finished_at = now_ms();
    let outcome = match &result {
        Ok(_) => HookTransportOutcome::Written,
        Err(_) if output.written != 0 => HookTransportOutcome::Unknown,
        Err(_) => HookTransportOutcome::Failed,
    };
    let receipt = match runtime.record_transport_outcome(
        ingress,
        binding,
        invocation,
        &reply,
        finished_at,
        outcome,
    ) {
        Ok(receipt) => receipt,
        Err(error) => {
            let _ = runtime.fail_invocation(
                invocation,
                HookTransportOutcome::Unknown,
                finished_at.max(write_started_at),
            );
            return Err(ClaudeHookError::Runtime(error));
        }
    };
    result.map_err(ClaudeHookError::Output)?;
    Ok(receipt)
}
