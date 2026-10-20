//! Moe Buddy 原生桌面宿主入口；Windows 上承载事件循环，其他平台明确退出。

#![cfg_attr(windows, allow(unsafe_code, unsafe_op_in_unsafe_fn))]
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod chatlog;
mod layout;
mod presenter;
mod theme;

use moe_desktop_adapters::{storage, transport};

#[cfg(windows)]
mod host;

#[cfg(not(windows))]
fn main() {
    eprintln!("moe-buddy-desktop: unsupported_platform (Windows desktop host only)");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match host::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            host::report_startup_failure(&message);
            std::process::ExitCode::FAILURE
        }
    }
}
