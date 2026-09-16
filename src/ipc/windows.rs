//! Windows named-pipe transport for the control-plane protocol.
//!
//! This module intentionally exposes byte streams only. The IPC framing and
//! protocol modules should wrap these streams once `ipc::framing` and
//! `ipc::protocol` are available; transport code must not duplicate framing.

use std::io;

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};

#[derive(Clone, Debug)]
pub struct PipeListener {
    name: String,
}

impl PipeListener {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    #[cfg(test)]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Creates the protected first instance used to establish daemon ownership.
    ///
    /// Call this before starting the proxy or `cloudflared`. Subsequent accept
    /// slots must be created with [`Self::create_additional_instance`], because
    /// Windows permits `FILE_FLAG_FIRST_PIPE_INSTANCE` only on the first handle.
    pub fn create_first_instance(&self) -> io::Result<NamedPipeServer> {
        server_options(true).create(&self.name)
    }

    /// Creates an additional local-only, duplex, byte-mode server instance.
    ///
    /// An accept loop should create the next instance before handing a connected
    /// instance to a session task, avoiding a window with no connectable pipe.
    pub fn create_additional_instance(&self) -> io::Result<NamedPipeServer> {
        server_options(false).create(&self.name)
    }
}

pub fn connect(name: &str) -> io::Result<NamedPipeClient> {
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

    #[test]
    fn listener_retains_exact_sid_derived_name() {
        let name = r"\\.\pipe\fw-0123456789abcdef";
        assert_eq!(PipeListener::new(name).name(), name);
    }
}
