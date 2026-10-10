use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::PersistenceError;

pub(crate) const HEADER: &[u8; 16] = b"MOEMEMJ\0\x01\0\0\0\0\0\0\0";
pub(crate) const MAX_JOURNAL_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_FRAME_BYTES: usize = 256 * 1024;

pub(crate) trait Journal: Send {
    fn read_all(&mut self) -> Result<Vec<u8>, PersistenceError>;
    fn append_synced(&mut self, frame: &[u8]) -> io::Result<()>;
    fn sync(&mut self) -> io::Result<()>;
}

pub(crate) struct FileJournal {
    file: File,
}

impl FileJournal {
    pub(crate) fn open(path: &Path, create: bool) -> Result<Self, PersistenceError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(create);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(PersistenceError::Locked),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let mut journal = Self { file };
        if create {
            journal.append_synced(HEADER)?;
            #[cfg(unix)]
            File::open(
                path.parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?
            .sync_all()?;
        }
        Ok(journal)
    }
}

impl Journal for FileJournal {
    fn read_all(&mut self) -> Result<Vec<u8>, PersistenceError> {
        if self.file.metadata()?.len() > MAX_JOURNAL_BYTES as u64 {
            return Err(PersistenceError::CapacityExceeded);
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut self.file)
            .take(MAX_JOURNAL_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(PersistenceError::CapacityExceeded);
        }
        Ok(bytes)
    }

    fn append_synced(&mut self, frame: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(frame)?;
        self.file.sync_all()
    }

    fn sync(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}

pub(crate) fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

pub(crate) fn frame(sequence: u64, previous: u64, operation: &str, payload: &[u8]) -> Vec<u8> {
    let body_length = 32 + operation.len() + payload.len();
    let mut bytes = Vec::with_capacity(body_length + 16);
    bytes.extend_from_slice(&(body_length as u64).to_le_bytes());
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&previous.to_le_bytes());
    bytes.extend_from_slice(&(operation.len() as u64).to_le_bytes());
    bytes.extend_from_slice(operation.as_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
    bytes
}

pub(crate) struct Reader<'bytes> {
    pub(crate) remaining: &'bytes [u8],
    pub(crate) offset: u64,
}

impl<'bytes> Reader<'bytes> {
    pub(crate) fn take(&mut self, length: usize) -> Result<&'bytes [u8], PersistenceError> {
        let (value, remaining) = self
            .remaining
            .split_at_checked(length)
            .ok_or_else(|| self.corrupt("truncated frame"))?;
        self.remaining = remaining;
        self.offset += length as u64;
        Ok(value)
    }

    pub(crate) fn number(&mut self) -> Result<u64, PersistenceError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| self.corrupt("invalid integer"))?,
        ))
    }

    pub(crate) fn sized(&mut self, limit: usize) -> Result<&'bytes [u8], PersistenceError> {
        let length = self.number()?;
        if length > limit as u64 {
            return Err(self.corrupt("length exceeds limit"));
        }
        self.take(length as usize)
    }

    pub(crate) fn corrupt(&self, reason: &'static str) -> PersistenceError {
        PersistenceError::Corrupt {
            offset: self.offset,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "moe-journal-m1-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(mut file) => {
                        file.write_all(HEADER).unwrap();
                        file.sync_all().unwrap();
                        return Self(path);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("{error}"),
                }
            }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn actual_os_write_failure_is_returned_without_changing_the_file() {
        let scratch = Scratch::new();
        let mut journal = FileJournal {
            file: File::open(&scratch.0).unwrap(),
        };
        assert!(journal.append_synced(b"not writable").is_err());
        drop(journal);
        assert_eq!(std::fs::read(&scratch.0).unwrap(), HEADER);
    }

    #[test]
    fn oversized_file_is_rejected_before_reading_or_allocating_it() {
        let scratch = Scratch::new();
        OpenOptions::new()
            .write(true)
            .open(&scratch.0)
            .unwrap()
            .set_len(MAX_JOURNAL_BYTES as u64 + 1)
            .unwrap();
        let mut journal = FileJournal::open(&scratch.0, false).unwrap();
        assert!(matches!(
            journal.read_all(),
            Err(PersistenceError::CapacityExceeded)
        ));
    }
}
