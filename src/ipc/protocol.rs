use serde::{Deserialize, Serialize};

use crate::config::{LOOPBACK_HOST, PROTOCOL_VERSION};
use crate::error::{FwError, Result};
#[cfg(test)]
use crate::slug::normalize_slug;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    pub version: u16,
    pub request_id: Option<u64>,
    pub message: T,
}

impl<T> Envelope<T> {
    pub fn new(request_id: Option<u64>, message: T) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            message,
        }
    }

    pub fn validate_version(&self) -> Result<()> {
        if self.version == PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(FwError::Protocol(format!(
                "unsupported protocol version {}; expected {PROTOCOL_VERSION}",
                self.version
            )))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Register {
        port: u16,
        slug: Option<String>,
        pid: u32,
    },
    Unregister {
        session_id: u64,
    },
    List,
    Stop {
        selector: StopSelector,
    },
    Kill,
    TerminateAcknowledged {
        session_id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
    Registered {
        session_id: u64,
        slug: String,
        local_url: String,
        public_url: String,
    },
    /// Acknowledges idempotent owner-session cleanup.
    Unregistered {
        session_id: u64,
    },
    RouteList {
        routes: Vec<RouteView>,
    },
    Stopped {
        route: RouteView,
    },
    KillAccepted {
        route_count: usize,
    },
    Terminate {
        reason: TerminateReason,
    },
    Stats {
        session_id: u64,
        uploaded_bytes: u64,
        downloaded_bytes: u64,
        upload_bytes_per_second: f64,
        download_bytes_per_second: f64,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
    /// An asynchronous unrecoverable daemon error, such as cloudflared exiting.
    Fatal {
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StopSelector {
    Slug(String),
    Port(u16),
}

impl StopSelector {
    /// Parse numeric input as a port and all other input as a normalized slug.
    #[cfg(test)]
    pub fn parse(input: &str) -> Result<Self> {
        if !input.is_empty() && input.bytes().all(|byte| byte.is_ascii_digit()) {
            let port = input
                .parse::<u16>()
                .map_err(|_| FwError::InvalidPort(input.to_owned()))?;
            if port == 0 {
                return Err(FwError::InvalidPort(input.to_owned()));
            }
            Ok(Self::Port(port))
        } else {
            Ok(Self::Slug(normalize_slug(input)?))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminateReason {
    RemoteStop,
    DaemonKill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    InvalidPort,
    InvalidSlug,
    DuplicateSlug,
    DuplicatePort,
    RouteNotFound,
    NoActiveForwards,
    DaemonShuttingDown,
    CloudflaredExited,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteView {
    pub slug: String,
    pub port: u16,
    pub local_url: String,
    pub public_url: String,
}

impl RouteView {
    pub fn new(slug: impl Into<String>, port: u16, base_domain: &str) -> Self {
        let slug = slug.into();
        Self {
            local_url: format!("http://{LOOPBACK_HOST}:{port}"),
            public_url: format!("https://{slug}.{base_domain}"),
            slug,
            port,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn protocol_has_stable_tagged_json_shape() {
        let envelope = Envelope::new(
            Some(42),
            ClientMessage::Register {
                port: 4321,
                slug: Some("green-apple".into()),
                pid: 99,
            },
        );
        let value = serde_json::to_value(envelope).unwrap();
        assert_eq!(
            value,
            json!({
                "version": 1,
                "request_id": 42,
                "message": {
                    "type": "register",
                    "port": 4321,
                    "slug": "green-apple",
                    "pid": 99
                }
            })
        );
    }

    #[test]
    fn round_trips_server_events() {
        let messages = [
            ServerMessage::Terminate {
                reason: TerminateReason::RemoteStop,
            },
            ServerMessage::Fatal {
                code: ErrorCode::CloudflaredExited,
                message: "connector exited".into(),
            },
            ServerMessage::Unregistered { session_id: 7 },
        ];

        for message in messages {
            let json = serde_json::to_vec(&Envelope::new(None, message.clone())).unwrap();
            let decoded: Envelope<ServerMessage> = serde_json::from_slice(&json).unwrap();
            assert_eq!(decoded.message, message);
        }
    }

    #[test]
    fn rejects_unsupported_versions() {
        let envelope = Envelope {
            version: PROTOCOL_VERSION + 1,
            request_id: None,
            message: ClientMessage::List,
        };
        assert!(envelope.validate_version().is_err());
    }

    #[test]
    fn parses_port_and_slug_selectors() {
        assert_eq!(
            StopSelector::parse("8080").unwrap(),
            StopSelector::Port(8080)
        );
        assert_eq!(
            StopSelector::parse("Green-Apple").unwrap(),
            StopSelector::Slug("green-apple".into())
        );
    }

    #[test]
    fn rejects_invalid_numeric_selectors() {
        assert!(matches!(
            StopSelector::parse("0"),
            Err(FwError::InvalidPort(_))
        ));
        assert!(matches!(
            StopSelector::parse("99999"),
            Err(FwError::InvalidPort(_))
        ));
    }

    #[test]
    fn route_view_uses_configured_addresses() {
        let route = RouteView::new("silent-panda", 4321, "mytunnel.me");
        assert_eq!(route.local_url, "http://127.0.0.1:4321");
        assert_eq!(route.public_url, "https://silent-panda.mytunnel.me");
    }
}
