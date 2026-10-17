use std::error::Error;
use std::fmt;
use std::process::ExitCode;

use moe_chat_app::{Action, AppSnapshot, ChatApplication, PanelKind};
use moe_chat_host::{ApplicationPort, Assets, LoopbackServer, PublicError};

struct ApplicationAdapter(ChatApplication);

impl ApplicationPort for ApplicationAdapter {
    fn snapshot(&self) -> AppSnapshot {
        self.0.snapshot()
    }

    fn dispatch(&mut self, action: Action, now_ms: u64) -> Result<(), PublicError> {
        self.0
            .dispatch(action, now_ms)
            .map_err(|error| PublicError::new(error.public_code(), error.public_message()))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Launch {
    Help,
    Start(u16),
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Launch, &'static str> {
    let arguments: Vec<_> = arguments.into_iter().collect();
    match arguments.as_slice() {
        [] => Ok(Launch::Start(0)),
        [help] if help == "--help" || help == "-h" => Ok(Launch::Help),
        [flag, port] if flag == "--port" => {
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("端口必须是 0 到 65535 之间的整数");
            }
            port.parse()
                .map(Launch::Start)
                .map_err(|_| "端口必须是 0 到 65535 之间的整数")
        }
        _ => Err("仅支持 --port <端口> 或 --help；不能指定外网监听地址"),
    }
}

#[derive(Debug)]
struct StartupError {
    message: &'static str,
    source: Box<dyn Error>,
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for StartupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn run(port: u16) -> Result<(), StartupError> {
    let assets = Assets::new(
        include_bytes!("../assets/index.html"),
        include_bytes!("../assets/app.js"),
        include_bytes!("../assets/style.css"),
    );
    let mut server = LoopbackServer::bind(port, assets).map_err(|error| StartupError {
        message: "无法启动本机宿主：请检查端口是否被占用及本机监听权限",
        source: Box::new(error),
    })?;
    let mut application =
        ChatApplication::new(server.process_generation()).map_err(|error| StartupError {
            message: error.public_message(),
            source: Box::new(error),
        })?;
    application
        .dispatch(Action::Open(PanelKind::Chat), 0)
        .map_err(|error| StartupError {
            message: error.public_message(),
            source: Box::new(error),
        })?;
    println!("Moe Buddy：{}", server.origin());
    println!("离线验收 · 未连接真实模型 · 仅内存，关闭进程后清空 · 未授权外传");
    println!("在浏览器打开上方地址；按 Ctrl+C 结束。");
    server
        .run(&mut ApplicationAdapter(application))
        .map_err(|error| StartupError {
            message: "本机宿主监听已停止，请重新启动",
            source: Box::new(error),
        })
}

fn main() -> ExitCode {
    match parse_arguments(std::env::args().skip(1)) {
        Ok(Launch::Help) => {
            println!("Moe Buddy 本机文字聊天界面");
            println!("用法：moe-chat-host [--port <0..65535>]");
            println!("默认使用系统分配端口，只监听 127.0.0.1。");
            ExitCode::SUCCESS
        }
        Ok(Launch::Start(port)) => match run(port) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Result<Launch, &'static str> {
        parse_arguments(values.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn default_is_loopback_ephemeral_and_help_is_side_effect_free() {
        assert_eq!(arguments(&[]), Ok(Launch::Start(0)));
        assert_eq!(arguments(&["--help"]), Ok(Launch::Help));
        assert_eq!(arguments(&["-h"]), Ok(Launch::Help));
    }

    #[test]
    fn explicit_port_accepts_the_u16_range() {
        assert_eq!(arguments(&["--port", "8787"]), Ok(Launch::Start(8787)));
        assert_eq!(arguments(&["--port", "65535"]), Ok(Launch::Start(65535)));
        assert_eq!(arguments(&["--port", "0"]), Ok(Launch::Start(0)));
    }

    #[test]
    fn ambiguous_arguments_and_external_bindings_are_rejected() {
        for values in [
            vec!["--port"],
            vec!["--port", "65536"],
            vec!["--port", "-1"],
            vec!["--port", "+8787"],
            vec!["--port", " 8787"],
            vec!["--port", "8787", "--port", "8788"],
            vec!["--host", "0.0.0.0"],
            vec!["--help", "--host", "0.0.0.0"],
        ] {
            assert!(arguments(&values).is_err());
        }
    }
}
