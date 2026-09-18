use std::ffi::OsStr;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::RawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_FAILED,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateMutexW, GetCurrentProcess, INFINITE, OpenProcessToken, ReleaseMutex,
    WaitForSingleObject,
};

use crate::error::{FwError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserObjectNames {
    pub sid_hash: String,
    pub directory_hash: String,
    pub daemon_mutex: String,
    pub startup_mutex: String,
    pub pipe: String,
}

impl UserObjectNames {
    pub fn current() -> Result<Self> {
        let sid_hash = current_user_sid_hash()?;
        let executable = crate::platform::executable_path()?;
        let directory_hash = installation_directory_hash(&executable)?;
        Ok(Self::from_hashes(sid_hash, directory_hash))
    }

    fn from_hashes(sid_hash: String, directory_hash: String) -> Self {
        Self {
            daemon_mutex: format!(r"Local\fw-daemon-{sid_hash}-{directory_hash}"),
            startup_mutex: format!(r"Local\fw-start-{sid_hash}-{directory_hash}"),
            pipe: format!(r"\\.\pipe\fw-{sid_hash}-{directory_hash}"),
            sid_hash,
            directory_hash,
        }
    }
}

pub fn current_user_sid_hash() -> Result<String> {
    let sid = current_user_sid_bytes()?;
    Ok(crate::platform::fnv1a_hex(sid.iter().copied()))
}

fn installation_directory_hash(executable: &Path) -> Result<String> {
    let directory = crate::platform::executable_directory_from(executable)?;
    let normalized = directory
        .as_os_str()
        .to_string_lossy()
        .replace('/', r"\")
        .to_lowercase();
    Ok(crate::platform::fnv1a_hex(
        normalized.encode_utf16().flat_map(u16::to_le_bytes),
    ))
}

fn current_user_sid_bytes() -> Result<Vec<u8>> {
    let mut token = null_mut();

    // SAFETY: GetCurrentProcess returns a valid pseudo-handle. `token` is a valid
    // out pointer and is closed below on every path after successful creation.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let token = OwnedHandle::new(token);

    let mut required = 0;
    // SAFETY: The initial null-buffer call is the documented way to obtain the
    // required TOKEN_USER buffer size.
    unsafe {
        GetTokenInformation(token.raw(), TokenUser, null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(io::Error::last_os_error().into());
    }

    let mut buffer = vec![0_u8; required as usize];
    // SAFETY: `buffer` is writable for `required` bytes and the token remains
    // alive. A successful TokenUser query initializes a TOKEN_USER at its start.
    if unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }

    // SAFETY: The successful query above initialized TOKEN_USER, whose SID
    // pointer remains valid while `buffer` is alive. GetLengthSid provides the
    // exact byte extent copied immediately into an owned Vec.
    let sid = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    if sid.is_null() {
        return Err(FwError::Other("the current user token has no SID".into()));
    }
    let sid_len = unsafe { GetLengthSid(sid) } as usize;
    if sid_len == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), sid_len) }.to_vec())
}

#[derive(Debug)]
pub struct NamedMutex {
    handle: OwnedHandle,
    owned: bool,
}

impl NamedMutex {
    pub fn acquire_daemon(name: &str) -> Result<Self> {
        let wide = wide_null(name);
        // SAFETY: `wide` is NUL-terminated and lives for the duration of the call.
        let handle = unsafe { CreateMutexW(null(), 1, wide.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: GetLastError must be read immediately after CreateMutexW.
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mutex = Self {
            handle: OwnedHandle::new(handle),
            // Initial ownership is ignored when the named mutex already exists.
            owned: !already_exists,
        };
        if already_exists {
            return Err(FwError::DaemonAlreadyRunning);
        }
        Ok(mutex)
    }

    pub fn acquire_startup(name: &str) -> Result<Self> {
        let wide = wide_null(name);
        // SAFETY: `wide` is NUL-terminated and lives for the duration of the call.
        let handle = unsafe { CreateMutexW(null(), 0, wide.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let mut mutex = Self {
            handle: OwnedHandle::new(handle),
            owned: false,
        };
        // SAFETY: The handle is a live mutex handle owned by this value.
        match unsafe { WaitForSingleObject(mutex.handle.raw(), INFINITE) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => {
                mutex.owned = true;
                Ok(mutex)
            }
            WAIT_FAILED => Err(io::Error::last_os_error().into()),
            result => Err(FwError::Other(format!(
                "unexpected startup mutex wait result: {result}"
            ))),
        }
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: This process owns the mutex after a successful create/wait.
            unsafe {
                ReleaseMutex(self.handle.raw());
            }
        }
    }
}

pub type DaemonGuard = NamedMutex;
pub type StartupGuard = NamedMutex;

#[derive(Debug)]
pub struct RuntimeScope {
    names: UserObjectNames,
    endpoint: PathBuf,
}

impl RuntimeScope {
    pub fn current() -> Result<Self> {
        let names = UserObjectNames::current()?;
        let endpoint = PathBuf::from(&names.pipe);
        Ok(Self { names, endpoint })
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    pub fn acquire_daemon(&self) -> Result<DaemonGuard> {
        NamedMutex::acquire_daemon(&self.names.daemon_mutex)
    }

    pub fn acquire_startup(&self) -> Result<StartupGuard> {
        NamedMutex::acquire_startup(&self.names.startup_mutex)
    }
}

pub fn spawn_daemon(executable: &Path) -> Result<Child> {
    if !executable.is_absolute() {
        return Err(FwError::InvalidExecutableDirectory);
    }
    if !executable.is_file() {
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
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    Ok(command.spawn()?)
}

pub fn configure_no_window(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    command.creation_flags(CREATE_NO_WINDOW)
}

pub fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    let source = wide_path(source);
    let destination = wide_path(destination);
    // SAFETY: Both path buffers are NUL-terminated and remain alive for the call.
    // MoveFileExW performs the required same-volume replacement with write-through.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

#[derive(Debug)]
pub struct JobObject {
    handle: OwnedHandle,
}

impl JobObject {
    pub fn kill_on_close() -> Result<Self> {
        // SAFETY: Null security attributes and name request an unnamed job object.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let job = Self {
            handle: OwnedHandle::new(handle),
        };

        // SAFETY: Zero is a valid initial state. We initialize the documented
        // LimitFlags field and pass the exact structure size to the API.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.handle.raw(),
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok(job)
    }

    pub fn assign_raw_handle(&self, process: RawHandle) -> Result<()> {
        // SAFETY: The caller supplies a live process handle. The job handle remains
        // valid for this call and is retained by `self` afterward.
        if unsafe { AssignProcessToJobObject(self.handle.raw(), process.cast()) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub fn terminate(&self, exit_code: u32) -> Result<()> {
        // SAFETY: `self.handle` is a live job handle.
        if unsafe { TerminateJobObject(self.handle.raw(), exit_code) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
}

#[derive(Debug)]
struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> Self {
        debug_assert!(!handle.is_null());
        Self(handle)
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: OwnedHandle is constructed only from owned, non-null Win32
        // handles and is not Clone, so this is the unique close.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn wide_path(value: &Path) -> Vec<u16> {
    value.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_names_are_scoped_to_the_user_and_executable() {
        let names =
            UserObjectNames::from_hashes("0123456789abcdef".into(), "fedcba9876543210".into());
        assert_eq!(names.sid_hash, "0123456789abcdef");
        assert_eq!(names.directory_hash, "fedcba9876543210");
        assert_eq!(
            names.daemon_mutex,
            r"Local\fw-daemon-0123456789abcdef-fedcba9876543210"
        );
        assert_eq!(
            names.startup_mutex,
            r"Local\fw-start-0123456789abcdef-fedcba9876543210"
        );
        assert_eq!(names.pipe, r"\\.\pipe\fw-0123456789abcdef-fedcba9876543210");
    }

    #[test]
    fn directory_hash_is_case_and_separator_insensitive() {
        let first = installation_directory_hash(Path::new(r"C:\Tools\FW\fw.exe")).unwrap();
        let second = installation_directory_hash(Path::new("c:/tools/fw/FW.EXE")).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn directory_hash_ignores_the_executable_filename() {
        let first = installation_directory_hash(Path::new(r"C:\Tools\FW\fw.exe")).unwrap();
        let second = installation_directory_hash(Path::new(r"C:\Tools\FW\renamed-fw.exe")).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn directory_hash_distinguishes_different_installations() {
        let first = installation_directory_hash(Path::new(r"C:\Tools\First\fw.exe")).unwrap();
        let second = installation_directory_hash(Path::new(r"C:\Tools\Second\fw.exe")).unwrap();
        assert_ne!(first, second);
    }
}
