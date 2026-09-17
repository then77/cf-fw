use std::path::{Path, PathBuf};

use crate::error::{FwError, Result};

#[cfg(windows)]
pub mod windows;

pub fn executable_path() -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    if !executable.is_absolute() {
        return Err(FwError::InvalidExecutableDirectory);
    }
    Ok(executable)
}

pub(crate) fn executable_directory_from(executable: &Path) -> Result<PathBuf> {
    if !executable.is_absolute() {
        return Err(FwError::InvalidExecutableDirectory);
    }

    executable
        .parent()
        .filter(|parent| parent.is_absolute())
        .map(Path::to_path_buf)
        .ok_or(FwError::InvalidExecutableDirectory)
}
