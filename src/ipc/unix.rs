//! Unix-domain socket transport for the control-plane protocol.

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

pub type ClientConnection = UnixStream;
pub type ServerConnection = UnixStream;

#[derive(Debug)]
pub struct Listener {
    inner: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        prepare_endpoint(path)?;
        let inner = UnixListener::bind(path)?;
        if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(path);
            return Err(error);
        }
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_socket() {
            let _ = fs::remove_file(path);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bound IPC endpoint is not a Unix socket",
            ));
        }
        Ok(Self {
            inner,
            path: path.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub async fn accept(&mut self) -> io::Result<ServerConnection> {
        self.inner.accept().await.map(|(stream, _)| stream)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub async fn connect(path: &Path) -> io::Result<ClientConnection> {
    UnixStream::connect(path).await
}

fn prepare_endpoint(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "refusing to replace non-socket IPC endpoint: {}",
                path.display()
            ),
        ));
    }

    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("IPC endpoint is already active: {}", path.display()),
        )),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "fw-ipc-{label}-{}-{nonce}.sock",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn connects_and_accepts_clients() {
        let path = temporary_path("round-trip");
        let mut listener = Listener::bind(&path).unwrap();
        let client = connect(&path);
        let (client, server) = tokio::join!(client, listener.accept());
        assert!(client.is_ok());
        assert!(server.is_ok());
    }

    #[tokio::test]
    async fn socket_is_private_and_removed_on_drop() {
        let path = temporary_path("permissions");
        let listener = Listener::bind(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(listener);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn stale_socket_is_recovered() {
        let path = temporary_path("stale");
        let stale = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(stale);
        let listener = Listener::bind(&path).unwrap();
        drop(listener);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn active_socket_is_not_replaced() {
        let path = temporary_path("active");
        let listener = Listener::bind(&path).unwrap();
        let error = Listener::bind(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        drop(listener);
    }

    #[test]
    fn non_socket_endpoint_is_never_removed() {
        let path = temporary_path("regular-file");
        fs::write(&path, b"keep").unwrap();
        let error = Listener::bind(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        fs::remove_file(path).unwrap();
    }
}
