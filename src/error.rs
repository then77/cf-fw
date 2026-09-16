use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum FwError {
    #[error("invalid port \"{0}\"")]
    InvalidPort(String),
    #[error("invalid slug \"{0}\": {1}")]
    InvalidSlug(String, String),
    #[error("slug \"{0}\" is already active")]
    DuplicateSlug(String),
    #[error("port {port} is already forwarded as \"{slug}\"")]
    DuplicatePort { port: u16, slug: String },

    #[error("no active forwards")]
    NoActiveForwards,
    #[error("{name} was not found in the cf directory beside fw.exe\nexpected: {path}", path = .path.display())]
    MissingSibling { name: &'static str, path: PathBuf },
    #[error("the executable directory could not be determined")]
    InvalidExecutableDirectory,
    #[error("no available proxy port was found in 10000..=65535")]
    NoAvailableProxyPort,
    #[error("cf-config.yml contains an invalid ingress rule\n{0}")]
    InvalidIngress(String),
    #[error("cloudflared rejected the normalized ingress configuration{details}", details = format_details(.0))]
    CloudflaredValidation(String),
    #[error("cloudflared exited unexpectedly{details}", details = format_details(.0))]
    CloudflaredExited(String),
    #[error("fw daemon did not become ready within 5 seconds")]
    DaemonStartTimeout,
    #[error("fw daemon is already running")]
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
}

fn format_details(details: &str) -> String {
    if details.is_empty() {
        String::new()
    } else {
        format!("\n{details}")
    }
}

pub type Result<T> = std::result::Result<T, FwError>;
