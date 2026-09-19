use std::collections::VecDeque;
#[cfg(test)]
use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::config::{
    CHILD_SHUTDOWN_TIMEOUT, CLOUDFLARED_STABILIZATION, MAX_CLOUDFLARED_OUTPUT_BYTES,
    MAX_CLOUDFLARED_OUTPUT_LINES,
};
use crate::error::{FwError, Result};
use crate::platform::ChildSupervisor;
#[cfg(windows)]
use crate::platform::windows::configure_no_window;

const READ_CHUNK_BYTES: usize = 4096;

#[derive(Clone, Debug)]
pub struct RecentOutput {
    inner: Arc<Mutex<OutputState>>,
}

#[derive(Debug, Default)]
struct OutputState {
    lines: VecDeque<String>,
    bytes: usize,
}

impl RecentOutput {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(OutputState::default())),
        }
    }

    pub fn snapshot(&self) -> String {
        self.lock()
            .lines
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn push_bytes(&self, source: &'static str, bytes: &[u8]) {
        let prefix = format!("[{source}] ");
        let maximum_payload = MAX_CLOUDFLARED_OUTPUT_BYTES.saturating_sub(prefix.len() + 1);
        let bytes = if bytes.len() > maximum_payload {
            &bytes[bytes.len() - maximum_payload..]
        } else {
            bytes
        };
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_end_matches(['\r', '\n']);
        let mut start = text.len().saturating_sub(maximum_payload);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        let line = format!("{prefix}{}", &text[start..]);
        let line_bytes = line.len() + 1;

        let mut state = self.lock();
        state.bytes = state.bytes.saturating_add(line_bytes);
        state.lines.push_back(line);
        while state.lines.len() > MAX_CLOUDFLARED_OUTPUT_LINES
            || state.bytes > MAX_CLOUDFLARED_OUTPUT_BYTES
        {
            if let Some(removed) = state.lines.pop_front() {
                state.bytes = state.bytes.saturating_sub(removed.len() + 1);
            } else {
                break;
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, OutputState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for RecentOutput {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct CloudflaredExit {
    pub status: ExitStatus,
    pub recent_output: String,
}

impl CloudflaredExit {
    pub fn message(&self) -> String {
        match self.status.code() {
            Some(code) => format!("Cloudflared process exited with code: {code}"),
            None => "Cloudflared process terminated unexpectedly.".into(),
        }
    }
}

#[derive(Debug)]
pub struct CloudflaredProcess {
    child: Child,
    output: RecentOutput,
    output_tasks: Vec<JoinHandle<io::Result<()>>>,
    supervisor: ChildSupervisor,
}

impl CloudflaredProcess {
    pub async fn spawn(executable: &Path, config: &Path, install_dir: &Path) -> Result<Self> {
        require_absolute(executable, "cloudflared executable")?;
        require_absolute(config, "cloudflared configuration")?;
        require_absolute(install_dir, "installation directory")?;

        let mut command = tunnel_command(executable, config, install_dir);
        let mut supervisor = ChildSupervisor::prepare(&mut command)?;
        let mut child = command.spawn()?;
        if let Err(error) = supervisor.attach(&child) {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(error);
        }

        let output = RecentOutput::new();
        let output_tasks = capture_child_output(&mut child, output.clone())?;
        Ok(Self {
            child,
            output,
            output_tasks,
            supervisor,
        })
    }

    pub async fn stabilize(&mut self) -> Result<()> {
        tokio::time::sleep(CLOUDFLARED_STABILIZATION).await;
        if let Some(status) = self.child.try_wait()? {
            self.supervisor.finish_shutdown()?;
            self.finish_output_tasks().await?;
            return Err(FwError::Other(format_exit_details(
                status,
                &self.output.snapshot(),
            )));
        }
        Ok(())
    }

    pub async fn try_wait_exit(&mut self) -> Result<Option<CloudflaredExit>> {
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        self.supervisor.finish_shutdown()?;
        self.finish_output_tasks().await?;
        Ok(Some(CloudflaredExit {
            status,
            recent_output: self.output.snapshot(),
        }))
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_some() {
            self.supervisor.finish_shutdown()?;
            self.finish_output_tasks().await?;
            return Ok(());
        }

        // Unix requests an orderly process-group shutdown with SIGTERM. A
        // CREATE_NO_WINDOW Windows child has no console signal, so its supervisor
        // preserves the existing bounded wait followed by Job Object termination.
        self.supervisor.begin_shutdown()?;
        match tokio::time::timeout(CHILD_SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(status) => {
                status?;
            }
            Err(_) => {
                self.supervisor.force_shutdown()?;
                self.child.wait().await?;
            }
        }
        self.supervisor.finish_shutdown()?;
        self.finish_output_tasks().await
    }

    async fn finish_output_tasks(&mut self) -> Result<()> {
        for task in self.output_tasks.drain(..) {
            task.await??;
        }
        Ok(())
    }
}

pub async fn validate_config(
    executable: &Path,
    candidate_config: &Path,
    install_dir: &Path,
) -> Result<()> {
    require_absolute(executable, "cloudflared executable")?;
    require_absolute(candidate_config, "temporary cloudflared configuration")?;
    require_absolute(install_dir, "installation directory")?;

    let command = validation_command(executable, candidate_config, install_dir);
    #[cfg(windows)]
    let mut command = command;
    #[cfg(windows)]
    configure_no_window(&mut command);
    let (status, output) = run_bounded(command).await?;
    if !status.success() {
        return Err(FwError::CloudflaredValidation(output.snapshot()));
    }
    Ok(())
}

fn validation_command(executable: &Path, config: &Path, install_dir: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("tunnel")
        .arg("--config")
        .arg(config)
        .arg("ingress")
        .arg("validate")
        .current_dir(install_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn tunnel_command(executable: &Path, config: &Path, install_dir: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("tunnel")
        .arg("--config")
        .arg(config)
        .arg("run")
        .current_dir(install_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

async fn run_bounded(mut command: Command) -> Result<(ExitStatus, RecentOutput)> {
    let mut child = command.spawn()?;
    let output = RecentOutput::new();
    let tasks = capture_child_output(&mut child, output.clone())?;
    let status = child.wait().await?;
    for task in tasks {
        task.await??;
    }
    Ok((status, output))
}

fn capture_child_output(
    child: &mut Child,
    output: RecentOutput,
) -> Result<Vec<JoinHandle<io::Result<()>>>> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| FwError::Other("cloudflared stdout was not piped".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| FwError::Other("cloudflared stderr was not piped".into()))?;

    Ok(vec![
        tokio::spawn(capture_stream(stdout, "stdout", output.clone())),
        tokio::spawn(capture_stream(stderr, "stderr", output)),
    ])
}

async fn capture_stream<R>(
    mut reader: R,
    source: &'static str,
    output: RecentOutput,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut pending = Vec::new();
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            if !pending.is_empty() {
                output.push_bytes(source, &pending);
            }
            return Ok(());
        }
        pending.extend_from_slice(&chunk[..read]);

        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline).collect::<Vec<_>>();
            output.push_bytes(source, &line);
        }

        // Bound memory even if cloudflared emits an unterminated line.
        if pending.len() >= READ_CHUNK_BYTES * 2 {
            let line = pending.drain(..READ_CHUNK_BYTES).collect::<Vec<_>>();
            output.push_bytes(source, &line);
        }
    }
}

fn require_absolute(path: &Path, description: &str) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(FwError::Other(format!(
            "{description} path must be absolute: {}",
            path.display()
        )))
    }
}

fn format_exit_details(status: ExitStatus, output: &str) -> String {
    let status = match status.code() {
        Some(code) => format!("Cloudflared process exited with code: {code}"),
        None => "Cloudflared process terminated unexpectedly.".into(),
    };
    if output.is_empty() {
        status
    } else {
        format!("{status}\n{output}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_output_is_bounded_by_lines() {
        let output = RecentOutput::new();
        for line in 0..(MAX_CLOUDFLARED_OUTPUT_LINES + 20) {
            output.push_bytes("stderr", format!("line-{line}").as_bytes());
        }
        let snapshot = output.snapshot();
        assert_eq!(snapshot.lines().count(), MAX_CLOUDFLARED_OUTPUT_LINES);
        assert!(!snapshot.contains("line-0\n"));
        assert!(snapshot.contains(&format!("line-{}", MAX_CLOUDFLARED_OUTPUT_LINES + 19)));
    }

    #[test]
    fn recent_output_is_bounded_by_bytes() {
        let output = RecentOutput::new();
        output.push_bytes("stdout", &vec![b'x'; MAX_CLOUDFLARED_OUTPUT_BYTES * 2]);
        assert!(output.snapshot().len() <= MAX_CLOUDFLARED_OUTPUT_BYTES);
        assert!(!output.snapshot().is_empty());
    }

    #[test]
    fn command_arguments_match_cloudflared_contract() {
        let executable = Path::new(r"C:\FW\cf\cloudflared.exe");
        let config = Path::new(r"C:\FW\cf\config.yml");
        let cloudflare_dir = Path::new(r"C:\FW\cf");

        let validation = validation_command(executable, config, cloudflare_dir)
            .as_std()
            .get_args()
            .map(OsString::from)
            .collect::<Vec<_>>();
        assert_eq!(
            validation,
            [
                "tunnel",
                "--config",
                r"C:\FW\cf\config.yml",
                "ingress",
                "validate"
            ]
            .map(OsString::from)
        );

        let run = tunnel_command(executable, config, cloudflare_dir)
            .as_std()
            .get_args()
            .map(OsString::from)
            .collect::<Vec<_>>();
        assert_eq!(
            run,
            ["tunnel", "--config", r"C:\FW\cf\config.yml", "run"].map(OsString::from)
        );
    }
}
