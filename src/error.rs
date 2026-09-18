use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum FwError {
    #[error("Invalid port \"{0}\"")]
    InvalidPort(String),
    #[error("Invalid slug \"{0}\": {1}")]
    InvalidSlug(String, String),
    #[error("Slug \"{0}\" is already active")]
    DuplicateSlug(String),
    #[error("Port {port} is already forwarded as \"{slug}\"")]
    DuplicatePort { port: u16, slug: String },

    #[error("No active forwards")]
    NoActiveForwards,
    #[error(
        "The cf directory was not found beside {executable_name}\nexpected: {path}{setup_hint}",
        path = .path.display(),
        setup_hint = format_setup_hint(*.setup_eligible, .executable_name)
    )]
    MissingCloudflareDirectory {
        executable_name: String,
        path: PathBuf,
        setup_eligible: bool,
    },
    #[error("{name} was not found in the cf directory beside {executable_name}\nexpected: {path}", path = .path.display())]
    MissingSibling {
        name: &'static str,
        executable_name: String,
        path: PathBuf,
    },
    #[error("The executable directory could not be determined")]
    InvalidExecutableDirectory,
    #[error("No available proxy port was found in 10000..=65535")]
    NoAvailableProxyPort,
    #[error("Error parsing Cloudflare config.yml\n{0}")]
    InvalidIngress(String),
    #[error("cloudflared rejected the normalized ingress configuration{details}", details = format_details(.0))]
    CloudflaredValidation(String),
    #[error("cloudflared exited unexpectedly{details}", details = format_details(.0))]
    CloudflaredExited(String),
    #[error("Daemon did not become ready within 10 seconds")]
    DaemonStartTimeout,
    #[error("Daemon exited with code {code}:\n{details}")]
    DaemonExited { code: String, details: String },
    #[error("Daemon is already running")]
    DaemonAlreadyRunning,
    #[error("IPC protocol error: {0}")]
    Protocol(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yml::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("{0}")]
    Other(String),
    #[error("{0}")]
    Reported(Box<FwError>),
}

impl FwError {
    pub fn reported(self) -> Self {
        Self::Reported(Box::new(self))
    }

    pub fn is_reported(&self) -> bool {
        matches!(self, Self::Reported(_))
    }
}

fn format_details(details: &str) -> String {
    if details.is_empty() {
        String::new()
    } else {
        format!("\n{details}")
    }
}

fn format_setup_hint(setup_eligible: bool, executable_name: &str) -> String {
    if setup_eligible {
        format!(
            "\n\nIf you don't know what you're doing, consider running {executable_name} --setup first."
        )
    } else {
        String::new()
    }
}

pub type Result<T> = std::result::Result<T, FwError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cloudflare_directory_includes_setup_hint_when_eligible() {
        let error = FwError::MissingCloudflareDirectory {
            executable_name: "renamed-fw.exe".into(),
            path: PathBuf::from(r"C:\Tools\FW\cf"),
            setup_eligible: true,
        };

        assert!(
            error
                .to_string()
                .ends_with("consider running renamed-fw.exe --setup first.")
        );
    }

    #[test]
    fn missing_cloudflare_directory_omits_unavailable_setup_hint() {
        let error = FwError::MissingCloudflareDirectory {
            executable_name: "fw.exe".into(),
            path: PathBuf::from(r"C:\Tools\FW\cf"),
            setup_eligible: false,
        };

        assert!(!error.to_string().contains("--setup"));
    }

    #[test]
    fn missing_sibling_uses_the_actual_executable_name() {
        let error = FwError::MissingSibling {
            name: "config.yml",
            executable_name: "renamed-fw.exe".into(),
            path: PathBuf::from(r"C:\Tools\FW\cf\config.yml"),
        };

        assert!(
            error
                .to_string()
                .contains("cf directory beside renamed-fw.exe")
        );
    }
}
