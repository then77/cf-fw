//! Windows named-pipe transport for the control-plane protocol.
//!
//! This module intentionally exposes byte streams only. The IPC framing and
//! protocol modules should wrap these streams once `ipc::framing` and
//! `ipc::protocol` are available; transport code must not duplicate framing.

use std::io;
use std::path::{Path, PathBuf};

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};

pub type ClientConnection = NamedPipeClient;
pub type ServerConnection = NamedPipeServer;

#[derive(Debug)]
pub struct Listener {
    name: PathBuf,
    pending: NamedPipeServer,
}

impl Listener {
    pub fn bind(name: &Path) -> io::Result<Self> {
        let pending = server_options(true).create(name)?;
        Ok(Self {
            name: name.to_path_buf(),
            pending,
        })
    }

    pub async fn accept(&mut self) -> io::Result<ServerConnection> {
        self.pending.connect().await?;
        let next = server_options(false).create(&self.name)?;
        Ok(std::mem::replace(&mut self.pending, next))
    }

    #[cfg(test)]
    fn name(&self) -> &Path {
        &self.name
    }
}

pub async fn connect(name: &Path) -> io::Result<ClientConnection> {
    ClientOptions::new().open(name)
}

fn server_options(first_instance: bool) -> ServerOptions {
    let mut options = ServerOptions::new();
    options
        .pipe_mode(PipeMode::Byte)
        .reject_remote_clients(true)
        .first_pipe_instance(first_instance);
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn listener_retains_exact_sid_derived_name() {
        let name = Path::new(r"\\.\pipe\fw-test-listener-retains-name-0123456789abcdef");
        let listener = Listener::bind(name).unwrap();
        assert_eq!(listener.name(), name);
    }
}
