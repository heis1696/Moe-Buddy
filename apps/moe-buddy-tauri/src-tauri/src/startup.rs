//! Host-only portable layout. Explicit data roots always take precedence.

use std::path::{Path, PathBuf};

pub(crate) struct StartupPaths {
    pub data_root: PathBuf,
    pub webview_data: Option<PathBuf>,
}

pub(crate) fn resolve(
    explicit: Option<PathBuf>,
    executable: &Path,
) -> Result<StartupPaths, String> {
    select(explicit, executable, moe_desktop_adapters::storage::root)
}

fn select(
    explicit: Option<PathBuf>,
    executable: &Path,
    default: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<StartupPaths, String> {
    if let Some(data_root) = explicit {
        return Ok(StartupPaths {
            data_root,
            webview_data: None,
        });
    }
    let directory = executable
        .parent()
        .ok_or("executable directory unavailable")?;
    let marker = directory.join("moe-buddy.portable");
    let portable = match std::fs::metadata(&marker) {
        Ok(metadata) if metadata.is_file() && metadata.len() == 2 => {
            std::fs::read(&marker).map_err(|_| "unable to read portable marker")? == b"1\n"
        }
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StartupPaths {
                data_root: default()?,
                webview_data: None,
            });
        }
        Err(_) => return Err("unable to inspect portable marker".into()),
    };
    if !portable {
        return Err("invalid portable marker; refusing fallback to daily data".into());
    }
    let data_root = directory.join("data");
    Ok(StartupPaths {
        webview_data: Some(data_root.join("webview")),
        data_root,
    })
}

#[cfg(all(windows, not(debug_assertions)))]
pub(crate) fn show_startup_failure() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let title: Vec<u16> = "Moe Buddy\0".encode_utf16().collect();
    let message: Vec<u16> = concat!(
        "无法启动 Moe Buddy。\n\n",
        "请先检查是否已在运行（任务栏或托盘），并确认解压目录可写、WebView2 已安装。\n",
        "若仍失败，请保留 data 目录排查；不要删除存档重试。\0"
    )
    .encode_utf16()
    .collect();
    // Both UTF-16 buffers are NUL-terminated and remain alive during this call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_layout_is_executable_relative_and_explicit_root_wins() {
        let root = std::env::temp_dir().join(format!("moe-portable-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("Moe Buddy.exe");
        std::fs::write(root.join("moe-buddy.portable"), b"1\n").unwrap();
        let selected = select(None, &executable, || panic!("must not open daily")).unwrap();
        assert_eq!(selected.data_root, root.join("data"));
        assert_eq!(selected.webview_data, Some(root.join("data/webview")));
        let explicit = root.join("synthetic-explicit");
        let selected = select(Some(explicit.clone()), &executable, || panic!()).unwrap();
        assert_eq!(selected.data_root, explicit);
        assert!(selected.webview_data.is_none());
        std::fs::write(root.join("moe-buddy.portable"), b"invalid").unwrap();
        assert!(select(None, &executable, || panic!("no silent fallback")).is_err());
        std::fs::remove_file(root.join("moe-buddy.portable")).unwrap();
        let selected = select(None, &executable, || Ok(root.join("synthetic-default"))).unwrap();
        assert_eq!(selected.data_root, root.join("synthetic-default"));
        assert!(selected.webview_data.is_none());
        std::fs::remove_dir(root).unwrap();
    }
}
