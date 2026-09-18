use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use console::style;
use sha2::{Digest, Sha256};
use tokio::process::{Child, Command};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

use crate::error::{FwError, Result};
use crate::ui::{StartupProgress, StatusKind, write_status};

const PROJECT_URL: &str = "https://github.com/then77/cf-fw";
const RELEASE_BASE_URL: &str = "https://github.com/then77/cf-fw/releases/download";
const MAX_SETUP_SCRIPT_BYTES: u64 = 16 * 1024 * 1024;
const UNAVAILABLE_MESSAGE: &str = "This app version does not include setup flow.";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetupMetadata {
    version: String,
    sha256: String,
}

impl SetupMetadata {
    fn embedded() -> Result<Self> {
        Self::from_values(
            option_env!("FW_APP_VERSION"),
            option_env!("FW_SETUP_SCRIPT_SHA"),
        )
    }

    fn from_values(version: Option<&str>, sha256: Option<&str>) -> Result<Self> {
        let (Some(version), Some(sha256)) = (version, sha256) else {
            return Err(FwError::Other(UNAVAILABLE_MESSAGE.into()));
        };
        let version = version.trim();
        let sha256 = sha256.trim();
        if version.is_empty() || sha256.is_empty() {
            return Err(FwError::Other(UNAVAILABLE_MESSAGE.into()));
        }
        if !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
        {
            return Err(FwError::Other(
                "The embedded setup app version is invalid.".into(),
            ));
        }
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(FwError::Other(
                "The embedded setup script SHA-256 is invalid.".into(),
            ));
        }

        Ok(Self {
            version: version.into(),
            sha256: sha256.to_ascii_lowercase(),
        })
    }

    fn url(&self) -> String {
        format!("{RELEASE_BASE_URL}/{}/fw-setup.ps1", self.version)
    }

    fn temp_path(&self) -> PathBuf {
        std::env::temp_dir().join(format!("fw-setup-{}.ps1", self.version))
    }
}

pub(crate) fn is_eligible() -> bool {
    SetupMetadata::embedded().is_ok()
}

pub async fn run() -> Result<()> {
    match run_inner().await {
        Ok(()) => Ok(()),
        Err(error) => {
            if write_status(
                &mut io::stdout().lock(),
                StatusKind::Error,
                &error.to_string(),
            )
            .is_ok()
            {
                Err(error.reported())
            } else {
                Err(error)
            }
        }
    }
}

async fn run_inner() -> Result<()> {
    let metadata = SetupMetadata::embedded()?;
    let url = metadata.url();

    println!("\nSetup will download setup script from:");
    println!("{}\n", style(&url).green());
    println!(
        "If you use a custom Cloudflare Tunnel configuration, consider installing FW manually."
    );
    println!("More information: {}\n", PROJECT_URL);
    if !confirm_setup()? {
        return Ok(());
    }

    let script_path = metadata.temp_path();
    if !script_path.exists() {
        let mut progress = StartupProgress::for_message("Downloading setup script...");
        let result = download_script(&url, &script_path).await;
        progress.finish_and_clear();
        if let Err(error) = result {
            remove_if_present(&script_path);
            return Err(error);
        }
    }

    let mut progress = StartupProgress::for_message("Verifying script integrity...");
    let script = open_locked_script(&script_path);
    let mut script = match script {
        Ok(script) => script,
        Err(error) => {
            progress.finish_and_clear();
            remove_if_present(&script_path);
            return Err(error);
        }
    };
    let integrity = verify_script(&mut script, &metadata.sha256);
    progress.finish_and_clear();
    if let Err(error) = integrity {
        drop(script);
        remove_if_present(&script_path);
        return Err(error);
    }

    write_status(
        &mut io::stdout().lock(),
        StatusKind::Success,
        "Launching script...",
    )?;
    let fw_path = crate::platform::executable_path()?;
    let mut child = spawn_powershell(&script_path, &fw_path).await?;
    let status = child.wait().await?;

    if !status.success() {
        return Err(FwError::Other(format!(
            "Setup script exited with code {}.",
            status
                .code()
                .map_or_else(|| "unknown".into(), |code| code.to_string())
        )));
    }

    drop(script);
    fs::remove_file(&script_path)?;
    Ok(())
}

fn confirm_setup() -> Result<bool> {
    loop {
        print!("Continue setup? (Y/n) ");
        io::stdout().flush()?;

        let mut answer = String::new();
        if io::stdin().read_line(&mut answer)? == 0 {
            return Ok(false);
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "" | "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please answer yes or no."),
        }
    }
}

async fn download_script(url: &str, path: &Path) -> Result<()> {
    let response = reqwest::Client::builder()
        .https_only(true)
        .build()
        .map_err(|error| FwError::Other(format!("Could not initialize setup download: {error}")))?
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| FwError::Other(format!("Could not download setup script: {error}")))?;

    if response
        .content_length()
        .is_some_and(|length| length > MAX_SETUP_SCRIPT_BYTES)
    {
        return Err(FwError::Other(
            "The setup script is unexpectedly large.".into(),
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| FwError::Other(format!("Could not download setup script: {error}")))?;
    if bytes.len() as u64 > MAX_SETUP_SCRIPT_BYTES {
        return Err(FwError::Other(
            "The setup script is unexpectedly large.".into(),
        ));
    }

    fs::write(path, bytes)?;
    Ok(())
}

fn open_locked_script(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)?)
}

fn verify_script(script: &mut File, expected_sha256: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    let bytes = io::copy(script, &mut hasher)?;
    if bytes > MAX_SETUP_SCRIPT_BYTES {
        return Err(FwError::Other(
            "The setup script is unexpectedly large.".into(),
        ));
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_sha256 {
        return Err(FwError::Other(
            "Setup script integrity verification failed.".into(),
        ));
    }
    Ok(())
}

async fn spawn_powershell(script: &Path, fw_path: &Path) -> Result<Child> {
    match powershell_command("pwsh.exe", script, fw_path).spawn() {
        Ok(child) => Ok(child),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(powershell_command("powershell.exe", script, fw_path).spawn()?)
        }
        Err(error) => Err(error.into()),
    }
}

fn powershell_command(program: &str, script: &Path, fw_path: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(script)
        .arg("-FWPath")
        .arg(fw_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

fn remove_if_present(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "could not remove setup script");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;

    #[test]
    fn metadata_requires_both_build_values() {
        let sha = "a".repeat(64);
        for values in [
            (None, None),
            (Some("v1.2.3"), None),
            (None, Some(sha.as_str())),
            (Some(""), Some(sha.as_str())),
        ] {
            let error = SetupMetadata::from_values(values.0, values.1).unwrap_err();
            assert_eq!(error.to_string(), UNAVAILABLE_MESSAGE);
        }
    }

    #[test]
    fn metadata_builds_release_url_and_safe_temp_name() {
        let metadata = SetupMetadata::from_values(Some("v1.2.3"), Some(&"A".repeat(64))).unwrap();
        assert_eq!(
            metadata.url(),
            "https://github.com/then77/cf-fw/releases/download/v1.2.3/fw-setup.ps1"
        );
        assert!(metadata.temp_path().ends_with("fw-setup-v1.2.3.ps1"));
        assert_eq!(metadata.sha256, "a".repeat(64));
        assert!(SetupMetadata::from_values(Some("../bad"), Some(&"a".repeat(64))).is_err());
    }

    #[test]
    fn powershell_receives_the_absolute_fw_executable_path() {
        let arguments = powershell_command(
            "pwsh.exe",
            Path::new(r"C:\Temp\fw-setup-v1.ps1"),
            Path::new(r"D:\Programs\FW\renamed-fw.exe"),
        )
        .as_std()
        .get_args()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();

        assert_eq!(
            arguments,
            [
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                r"C:\Temp\fw-setup-v1.ps1",
                "-FWPath",
                r"D:\Programs\FW\renamed-fw.exe",
            ]
            .map(std::ffi::OsString::from)
        );
    }

    #[test]
    fn verifies_sha256_and_rejects_mismatch() {
        let path = std::env::temp_dir().join(format!(
            "fw-setup-hash-test-{}-{}.ps1",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::write(&path, b"Write-Output 'setup'").unwrap();
        let mut file = File::open(&path).unwrap();
        verify_script(
            &mut file,
            "ec12dbb43b188304b188895cc7348b85912425507c44ed32e7daa25e86f94522",
        )
        .unwrap();
        file.rewind().unwrap();
        assert!(verify_script(&mut file, &"0".repeat(64)).is_err());
        drop(file);
        fs::remove_file(path).unwrap();
    }
}
