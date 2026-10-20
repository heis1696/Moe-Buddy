#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod contract;
mod hit_mask;
pub mod material;
mod prototype;
mod startup;

fn main() -> std::process::ExitCode {
    let code = prototype::run().clamp(0, 255) as u8;
    #[cfg(all(windows, not(debug_assertions)))]
    if code != 0 {
        startup::show_startup_failure();
    }
    std::process::ExitCode::from(code)
}
