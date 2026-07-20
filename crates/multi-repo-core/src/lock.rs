use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use crate::error::{Error, Result};

pub(crate) fn acquire_workspace_lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|source| Error::Write {
            path: path.to_path_buf(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(Error::WorkspaceLocked),
        Err(TryLockError::Error(source)) => {
            return Err(Error::Write {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    Ok(file)
}
