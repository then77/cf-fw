use std::path::{Path, PathBuf};

use crate::error::{FwError, Result};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
pub(crate) mod windows;

#[cfg(unix)]
pub(crate) use unix::ChildSupervisor;
#[cfg(unix)]
pub use unix::{RuntimeScope, atomic_replace, spawn_daemon};
#[cfg(windows)]
pub(crate) use windows::ChildSupervisor;
#[cfg(windows)]
pub use windows::{RuntimeScope, atomic_replace, spawn_daemon};

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

#[cfg(unix)]
pub(crate) fn installation_directory_hash_bytes(executable: &Path) -> Result<Vec<u8>> {
    let directory = executable_directory_from(executable)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(directory.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        Ok(directory.as_os_str().to_string_lossy().as_bytes().to_vec())
    }
}

pub(crate) fn fnv1a_hex(bytes: impl IntoIterator<Item = u8>) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = bytes.into_iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    });
    format!("{hash:016x}")
}

pub(crate) fn executable_name_from(executable: &Path) -> Result<String> {
    executable
        .file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or(FwError::InvalidExecutableDirectory)
}
