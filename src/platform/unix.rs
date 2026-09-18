use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::error::{FwError, Result};
use crate::platform::{executable_path, fnv1a_hex, installation_directory_hash_bytes};

#[derive(Debug)]
pub struct DaemonGuard {
    _file: File,
}

#[derive(Debug)]
pub struct StartupGuard {
    _file: File,
}

#[derive(Debug)]
pub struct RuntimeScope {
    endpoint: PathBuf,
    daemon_lock: PathBuf,
    startup_lock: PathBuf,
}

impl RuntimeScope {
    pub fn current() -> Result<Self> {
        let executable = executable_path()?;
        let directory_bytes = installation_directory_hash_bytes(&executable)?;
        let directory_hash = fnv1a_hex(directory_bytes);
        let runtime_directory = runtime_directory()?;
        Self::from_directory_and_hash(runtime_directory, &directory_hash)
    }

    fn from_directory_and_hash(runtime_directory: PathBuf, directory_hash: &str) -> Result<Self> {
        validate_runtime_directory(&runtime_directory)?;
        Ok(Self {
            endpoint: runtime_directory.join(format!("fw-{directory_hash}.sock")),
            daemon_lock: runtime_directory.join(format!("fw-daemon-{directory_hash}.lock")),
            startup_lock: runtime_directory.join(format!("fw-start-{directory_hash}.lock")),
        })
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    pub fn acquire_daemon(&self) -> Result<DaemonGuard> {
        let file = open_lock_file(&self.daemon_lock)?;
        let operation = libc::LOCK_EX | libc::LOCK_NB;
        // SAFETY: `file` owns a live file descriptor for the duration of the call.
        // `operation` is a valid combination of flock flags.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
            let error = io::Error::last_os_error();
            let raw = error.raw_os_error();
            if error.kind() == io::ErrorKind::WouldBlock
                || raw == Some(libc::EWOULDBLOCK)
                || raw == Some(libc::EAGAIN)
            {
                return Err(FwError::DaemonAlreadyRunning);
            }
            return Err(error.into());
        }
        Ok(DaemonGuard { _file: file })
    }

    pub fn acquire_startup(&self) -> Result<StartupGuard> {
        let file = open_lock_file(&self.startup_lock)?;
        // SAFETY: `file` owns a live file descriptor for the duration of the call,
        // and LOCK_EX is a valid flock operation. The descriptor is retained by
        // the returned guard so the lock remains held.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(StartupGuard { _file: file })
    }
}

#[cfg(target_os = "linux")]
fn runtime_directory() -> Result<PathBuf> {
    let value = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        FwError::Other("XDG_RUNTIME_DIR is not set; cannot create a secure runtime scope".into())
    })?;
    runtime_directory_from(value)
}

#[cfg(target_os = "macos")]
fn runtime_directory() -> Result<PathBuf> {
    match std::env::var_os("TMPDIR") {
        Some(value) => runtime_directory_from(value),
        None => {
            let path = std::env::temp_dir();
            validate_runtime_directory(&path)?;
            Ok(path)
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn runtime_directory() -> Result<PathBuf> {
    Err(FwError::Other(
        "Unix runtime scopes are supported only on Linux and macOS".into(),
    ))
}

fn runtime_directory_from(value: OsString) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    validate_runtime_directory(&path)?;
    Ok(path)
}

fn validate_runtime_directory(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(FwError::Other(format!(
            "runtime directory must be absolute: {}",
            path.display()
        )));
    }

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(FwError::Other(format!(
            "runtime directory must not be a symbolic link: {}",
            path.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(FwError::Other(format!(
            "runtime path is not a directory: {}",
            path.display()
        )));
    }

    // SAFETY: geteuid has no arguments, dereferences no pointers, and has no
    // preconditions. It simply returns the effective user ID of this process.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(FwError::Other(format!(
            "runtime directory is not owned by the current user: {}",
            path.display()
        )));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(FwError::Other(format!(
            "runtime directory is group- or world-writable: {}",
            path.display()
        )));
    }

    Ok(())
}

fn open_lock_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(no_follow_flag());
    let file = options.open(path)?;
    let metadata = file.metadata()?;

    if !metadata.is_file() {
        return Err(FwError::Other(format!(
            "lock path is not a regular file: {}",
            path.display()
        )));
    }

    // SAFETY: geteuid has no arguments, dereferences no pointers, and has no
    // preconditions. It simply returns the effective user ID of this process.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(FwError::Other(format!(
            "lock file is not owned by the current user: {}",
            path.display()
        )));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(FwError::Other(format!(
            "lock file is accessible by group or other users: {}",
            path.display()
        )));
    }

    Ok(file)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn no_follow_flag() -> i32 {
    libc::O_NOFOLLOW
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn no_follow_flag() -> i32 {
    0
}

pub fn spawn_daemon(executable: &Path) -> Result<Child> {
    if !executable.is_absolute() {
        return Err(FwError::InvalidExecutableDirectory);
    }
    let metadata = fs::metadata(executable)?;
    if !metadata.is_file() {
        return Err(FwError::Other(format!(
            "fw executable is not a regular file: {}",
            executable.display()
        )));
    }

    let mut command = Command::new(executable);
    command
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    // SAFETY: The closure runs after fork and before exec, performs only the
    // async-signal-safe setsid system call, does not access captured state, and
    // reports failure through an io::Error as required by pre_exec.
    unsafe {
        command.pre_exec(|| {
            // SAFETY: setsid takes no pointers or arguments. In the post-fork
            // child it either creates a new session or returns -1 with errno set.
            if unsafe { libc::setsid() } == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }

    Ok(command.spawn()?)
}

pub fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("fw-unix-tests-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn installation_hash(executable: &Path) -> String {
        fnv1a_hex(installation_directory_hash_bytes(executable).unwrap())
    }

    fn scope(directory: &Path, hash: &str) -> RuntimeScope {
        RuntimeScope::from_directory_and_hash(directory.to_path_buf(), hash).unwrap()
    }

    #[test]
    fn installation_hash_uses_raw_case_sensitive_directory_bytes() {
        let upper = Path::new("/opt/FW/fw");
        let lower = Path::new("/opt/fw/fw");

        assert_ne!(installation_hash(upper), installation_hash(lower));
        assert_eq!(
            installation_directory_hash_bytes(upper).unwrap(),
            b"/opt/FW"
        );
    }

    #[test]
    fn installation_hash_preserves_non_utf8_directory_bytes() {
        let mut bytes = b"/opt/fw-".to_vec();
        bytes.push(0xff);
        let directory = PathBuf::from(OsString::from_vec(bytes.clone()));
        let executable = directory.join("fw");

        assert_eq!(
            installation_directory_hash_bytes(&executable).unwrap(),
            bytes
        );
        assert_eq!(installation_hash(&executable), fnv1a_hex(bytes));
    }

    #[test]
    fn installation_hash_ignores_executable_filename() {
        let first = installation_hash(Path::new("/opt/fw/fw"));
        let second = installation_hash(Path::new("/opt/fw/renamed-fw"));

        assert_eq!(first, second);
    }

    #[test]
    fn generated_paths_use_the_directory_hash() {
        let directory = TestDirectory::new();
        let scope = scope(directory.path(), "0123456789abcdef");

        assert_eq!(
            scope.endpoint(),
            directory.path().join("fw-0123456789abcdef.sock")
        );
        assert_eq!(
            scope.daemon_lock,
            directory.path().join("fw-daemon-0123456789abcdef.lock")
        );
        assert_eq!(
            scope.startup_lock,
            directory.path().join("fw-start-0123456789abcdef.lock")
        );
    }

    #[test]
    fn daemon_lock_is_exclusive_and_released_with_guard() {
        let directory = TestDirectory::new();
        let scope = scope(directory.path(), "exclusive");
        let guard = scope.acquire_daemon().unwrap();

        assert!(matches!(
            scope.acquire_daemon(),
            Err(FwError::DaemonAlreadyRunning)
        ));

        drop(guard);
        scope.acquire_daemon().unwrap();
    }

    #[test]
    fn new_lock_files_are_private_regular_files() {
        let directory = TestDirectory::new();
        let scope = scope(directory.path(), "private");
        let _guard = scope.acquire_daemon().unwrap();
        let metadata = fs::symlink_metadata(&scope.daemon_lock).unwrap();

        assert!(metadata.is_file());
        assert_eq!(metadata.mode() & 0o777, 0o600);
    }

    #[test]
    fn existing_group_accessible_lock_file_is_rejected() {
        let directory = TestDirectory::new();
        let scope = scope(directory.path(), "insecure-lock");
        fs::write(&scope.daemon_lock, b"").unwrap();
        fs::set_permissions(&scope.daemon_lock, fs::Permissions::from_mode(0o640)).unwrap();

        assert!(scope.acquire_daemon().is_err());
    }

    #[test]
    fn relative_runtime_directory_is_rejected() {
        assert!(validate_runtime_directory(Path::new("relative/runtime")).is_err());
    }

    #[test]
    fn non_directory_runtime_path_is_rejected() {
        let directory = TestDirectory::new();
        let file = directory.path().join("not-a-directory");
        fs::write(&file, b"").unwrap();

        assert!(validate_runtime_directory(&file).is_err());
    }

    #[test]
    fn symlink_runtime_directory_is_rejected() {
        let directory = TestDirectory::new();
        let link = directory.path().with_extension("link");
        symlink(directory.path(), &link).unwrap();

        assert!(validate_runtime_directory(&link).is_err());
        fs::remove_file(link).unwrap();
    }

    #[test]
    fn group_or_world_writable_runtime_directory_is_rejected() {
        let directory = TestDirectory::new();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o720)).unwrap();

        assert!(validate_runtime_directory(directory.path()).is_err());

        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o702)).unwrap();
        assert!(validate_runtime_directory(directory.path()).is_err());
    }

    #[test]
    fn runtime_directory_accepts_raw_absolute_os_string() {
        let directory = TestDirectory::new();
        let value = OsString::from_vec(directory.path().as_os_str().as_bytes().to_vec());

        assert_eq!(runtime_directory_from(value).unwrap(), directory.path());
    }
}
