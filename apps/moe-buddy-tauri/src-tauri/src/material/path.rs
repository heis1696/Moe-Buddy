use std::fs::{File, OpenOptions};
use std::io::{Read, Take};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::manifest::MaterialError;

pub(super) struct PackageRoot {
    canonical: PathBuf,
    #[cfg(windows)]
    _directory_handles: Vec<File>,
}

impl std::ops::Deref for PackageRoot {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.canonical
    }
}

pub(super) fn canonical_package_root(path: &Path) -> Result<PackageRoot, MaterialError> {
    if !path.is_absolute() {
        return Err(MaterialError::Invalid("package path must be absolute"));
    }
    #[cfg(windows)]
    {
        use std::path::Prefix;
        let drive = match path.components().next() {
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)) => {
                format!("{}\\", prefix.as_os_str().to_string_lossy())
            }
            _ => String::new(),
        };
        if drive.is_empty() || !is_local_drive(&drive) {
            return Err(MaterialError::Invalid(
                "package path must be on a local disk",
            ));
        }
    }
    #[cfg(windows)]
    let directory_handles = lock_directory_chain(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if is_reparse(&metadata) || metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MaterialError::Invalid(
            "package root cannot be a symlink or reparse point",
        ));
    }
    let root = std::fs::canonicalize(path)?;
    if !std::fs::metadata(&root)?.is_dir() {
        return Err(MaterialError::Invalid("package root is not a directory"));
    }
    Ok(PackageRoot {
        canonical: root,
        #[cfg(windows)]
        _directory_handles: directory_handles,
    })
}

/// Keep every directory object fixed until the caller has finished using its path.
/// Denying write/delete sharing prevents rename and reparse changes during reads.
#[cfg(windows)]
fn lock_directory_chain(path: &Path) -> Result<Vec<File>, MaterialError> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Prefix;

    const MAX_DIRECTORY_HANDLES: usize = 128;
    if !path.is_absolute() || path.components().count() > MAX_DIRECTORY_HANDLES {
        return Err(MaterialError::Invalid(
            "material directory handle budget exceeded",
        ));
    }
    let mut current = PathBuf::new();
    let mut handles = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix)
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
            {
                current.push(component.as_os_str());
                continue;
            }
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            _ => {
                return Err(MaterialError::Invalid(
                    "unsafe material directory component",
                ));
            }
        }
        let directory = OpenOptions::new()
            .read(true)
            .access_mode(0x80) // FILE_READ_ATTRIBUTES
            .share_mode(0x1) // FILE_SHARE_READ: hold name and reparse metadata stable
            .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
            .open(&current)?;
        let metadata = directory.metadata()?;
        if !metadata.is_dir() || is_reparse(&metadata) {
            return Err(MaterialError::Invalid(
                "material directory cannot be a reparse point",
            ));
        }
        handles.push(directory);
    }
    Ok(handles)
}

#[cfg(windows)]
fn is_local_drive(root: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    }

    let mut wide: Vec<u16> = std::ffi::OsStr::new(root).encode_wide().collect();
    wide.push(0);
    // Only local removable, fixed, and RAM disks are accepted; mapped remote drives report 4.
    matches!(unsafe { GetDriveTypeW(wide.as_ptr()) }, 2 | 3 | 6)
}

#[cfg(test)]
pub(super) fn read_asset(
    root: &Path,
    relative: &str,
    limit: usize,
) -> Result<Vec<u8>, MaterialError> {
    read_asset_cancellable(root, relative, limit, None)
}

pub(super) fn read_asset_cancellable(
    root: &Path,
    relative: &str,
    limit: usize,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<u8>, MaterialError> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(MaterialError::Cancelled);
    }
    validate_relative(relative)?;
    #[cfg(windows)]
    let _directory_handles = lock_directory_chain(
        root.join(relative)
            .parent()
            .ok_or(MaterialError::Invalid("asset has no parent"))?,
    )?;
    let canonical_root = std::fs::canonicalize(root)?;
    let root = canonical_root.as_path();
    let mut current = root.to_path_buf();
    let components: Vec<_> = relative.split('/').collect();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current)?;
        if is_reparse(&metadata) || metadata.file_type().is_symlink() {
            return Err(MaterialError::Invalid(
                "package paths cannot traverse symlink or reparse points",
            ));
        }
        let is_last = index + 1 == components.len();
        if !is_last && !metadata.is_dir() {
            return Err(MaterialError::Invalid(
                "asset parent component is not a directory",
            ));
        }
    }
    let canonical = std::fs::canonicalize(&current)?;
    if !canonical.starts_with(root) {
        return Err(MaterialError::Invalid(
            "asset resolves outside package root",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
        options.share_mode(0x1); // prevent file replacement or writes until bounded read ends
    }
    let file = options.open(&current)?;
    let opened = file.metadata()?;
    if is_reparse(&opened) || !opened.is_file() {
        return Err(MaterialError::Invalid(
            "asset is not a regular non-reparse file",
        ));
    }
    // The handle already supplies the resolved object path. Re-resolving that
    // string through the filesystem would introduce another mutable name lookup.
    let opened_path = final_handle_path(&file)?;
    if !opened_path.starts_with(root) {
        return Err(MaterialError::Invalid(
            "opened asset target escaped package root",
        ));
    }
    let _ = verify_component_chain(root, relative)?;
    #[cfg(windows)]
    {
        if opened_path != canonical {
            return Err(MaterialError::Invalid(
                "opened asset target changed or escaped package path",
            ));
        }
    }
    if opened.len() > limit as u64 {
        return Err(MaterialError::Invalid("encoded asset exceeds byte budget"));
    }
    let mut reader = file.take(limit.saturating_add(1) as u64);
    let bytes = read_bounded(&mut reader, limit, cancelled)?;
    let after = std::fs::canonicalize(&current)?;
    if !after.starts_with(root) || after != canonical {
        return Err(MaterialError::Invalid("asset path changed during read"));
    }
    let _ = verify_component_chain(root, relative)?;
    Ok(bytes)
}

#[cfg(windows)]
fn final_handle_path(file: &File) -> Result<PathBuf, std::io::Error> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFinalPathNameByHandleW(
            file: *mut std::ffi::c_void,
            path: *mut u16,
            path_len: u32,
            flags: u32,
        ) -> u32;
    }

    let mut buffer = vec![0u16; 32_768];
    // The handle is owned by `file`; the buffer is writable for its declared length.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle().cast(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    };
    if length == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if length as usize >= buffer.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "opened asset path exceeds the supported path length",
        ));
    }
    Ok(PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..length as usize],
    )))
}

#[cfg(unix)]
fn final_handle_path(file: &File) -> Result<PathBuf, std::io::Error> {
    use std::os::fd::AsRawFd;
    std::fs::canonicalize(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(not(any(windows, unix)))]
fn final_handle_path(_file: &File) -> Result<PathBuf, std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opened handle path verification is unavailable",
    ))
}

fn read_bounded(
    reader: &mut Take<File>,
    limit: usize,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<u8>, MaterialError> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(MaterialError::Cancelled);
        }
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > limit {
            return Err(MaterialError::Invalid(
                "encoded asset grew past byte budget while reading",
            ));
        }
    }
    if bytes.len() > limit {
        return Err(MaterialError::Invalid(
            "encoded asset grew past byte budget while reading",
        ));
    }
    Ok(bytes)
}

fn verify_component_chain(root: &Path, relative: &str) -> Result<PathBuf, MaterialError> {
    let mut current = root.to_path_buf();
    for component in relative.split('/') {
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current)?;
        if is_reparse(&metadata) || metadata.file_type().is_symlink() {
            return Err(MaterialError::Invalid(
                "package path changed to a reparse point",
            ));
        }
    }
    let canonical = std::fs::canonicalize(&current)?;
    if !canonical.starts_with(root) {
        return Err(MaterialError::Invalid(
            "asset resolves outside package root",
        ));
    }
    Ok(canonical)
}

fn validate_relative(relative: &str) -> Result<(), MaterialError> {
    if relative.is_empty()
        || relative.contains('\\')
        || relative.contains(':')
        || relative.starts_with('/')
        || relative.chars().any(char::is_control)
    {
        return Err(MaterialError::Invalid(
            "asset path must be a safe relative slash path",
        ));
    }
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(MaterialError::Invalid(
            "asset path contains an unsafe component",
        ));
    }
    if relative
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(MaterialError::Invalid(
            "asset path contains an unsafe component",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
}

#[cfg(not(windows))]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}
