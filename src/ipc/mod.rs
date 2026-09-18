pub mod framing;
pub mod protocol;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{ClientConnection, Listener, connect};
#[cfg(windows)]
pub use windows::{ClientConnection, Listener, connect};
