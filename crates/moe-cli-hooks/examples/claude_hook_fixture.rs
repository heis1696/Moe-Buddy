use std::io;
use std::process::ExitCode;

use moe_agent_runtime::{
    CallerSubject, ChannelId, ChannelScope, HookBindingConfig, HookContext, HookLimits,
    HookPermissions, HookRuntime, IngressToken, ReplyTarget, TrustedIngress,
};
use moe_cli_hooks::{
    VerifiedCliVersion, accept_claude_submission, parse_user_prompt_submit, read_bounded,
    write_claude_context,
};
use moe_memory_core::{Namespace, PetIdentity};

fn run_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let mut ingress = TrustedIngress::new();
    let identity = PetIdentity::new(
        Namespace::new("synthetic-workspace", "synthetic-user")?,
        "synthetic-pet",
    )?;
    ingress.register_route("synthetic-buddy", identity)?;
    ingress.install_credential(
        IngressToken::new("synthetic-fixture-token")?,
        CallerSubject::new("synthetic-host")?,
        ["synthetic-buddy".to_owned()],
        ChannelId::new("synthetic-cli-hook")?,
        ChannelScope::new("synthetic-private")?,
        ReplyTarget::new("synthetic-stdout")?,
    )?;
    let mut runtime = HookRuntime::new(HookLimits::default());
    runtime.register_context(
        HookContext::new("synthetic-source", 1, "synthetic-reviewed-context"),
        None,
    )?;
    let binding = runtime.bind(
        &ingress,
        &IngressToken::new("synthetic-fixture-token")?,
        "synthetic-buddy",
        HookBindingConfig::new(HookPermissions::new(false, true, true), 100)
            .with_cli_session_id("synthetic-session")
            .with_context_source("synthetic-source"),
        0,
    )?;
    let bytes = read_bounded(io::stdin().lock())?;
    let submission = parse_user_prompt_submit(VerifiedCliVersion::CLAUDE_CODE_2_1_286, &bytes)?;
    let invocation = accept_claude_submission(&mut runtime, &ingress, &binding, submission, 1)?;
    write_claude_context(
        &mut runtime,
        &ingress,
        &binding,
        &invocation,
        io::stdout().lock(),
        || 2,
    )?;
    Ok(())
}

fn main() -> ExitCode {
    match run_fixture() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("synthetic Hook fixture failed; CLI continuation is not controlled");
            ExitCode::FAILURE
        }
    }
}
