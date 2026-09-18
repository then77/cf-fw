use std::ffi::OsString;
use std::fmt;

use clap::{CommandFactory, Parser, Subcommand, error::ErrorKind};

const APP_VERSION: &str = match option_env!("FW_APP_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Raw command-line arguments accepted by `fw`.
///
/// Call [`Cli::normalize`] (or [`Cli::try_parse_normalized_from`]) before
/// dispatching so shorthand and global-option semantics are applied in one
/// place.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(
    name = "fw",
    version = APP_VERSION,
    about = "Forward local HTTP services through Cloudflare"
)]
pub struct Cli {
    /// Internal daemon mode.
    #[arg(long, global = true)]
    pub daemon: bool,

    /// Download, verify, and run the setup script.
    #[arg(long, global = true)]
    pub setup: bool,

    /// Custom public slug. Used only when starting a forward.
    #[arg(short, long, global = true)]
    pub slug: Option<String>,

    /// Shorthand for `fw start <port>`.
    #[arg(value_parser = parse_port)]
    pub port: Option<u16>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Explicit user-facing subcommands before semantic normalization.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Start forwarding a local HTTP port.
    Start {
        #[arg(value_parser = parse_port)]
        port: u16,
    },
    /// List active forwards.
    List,
    /// Stop an active forward by slug or local port.
    Stop { selector: String },
    /// Stop every forward and the daemon.
    Kill,
}

/// A fully validated command ready for application dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Daemon,
    Setup,
    Start { port: u16, slug: Option<String> },
    List,
    Stop { selector: StopSelector },
    Kill,
}

/// The unambiguous selector sent by `fw stop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopSelector {
    Slug(String),
    Port(u16),
}

/// Semantic command-line validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    DaemonWithCommand,
    SetupWithCommand,
    ConflictingModes,
    ShorthandWithSubcommand,
    SlugWithoutStart,
    MissingCommand,
    InvalidPort(String),
    InvalidSlug { value: String, reason: &'static str },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DaemonWithCommand => {
                f.write_str("--daemon cannot be combined with a port or subcommand")
            }
            Self::SetupWithCommand => {
                f.write_str("--setup cannot be combined with a port or subcommand")
            }
            Self::ConflictingModes => f.write_str("--setup and --daemon cannot be used together"),
            Self::ShorthandWithSubcommand => {
                f.write_str("a shorthand port cannot be combined with a subcommand")
            }
            Self::SlugWithoutStart => {
                f.write_str("--slug can only be used when starting a forward")
            }
            Self::MissingCommand => f.write_str("a port or subcommand is required"),
            Self::InvalidPort(value) => write!(f, "invalid port \"{value}\""),
            Self::InvalidSlug { value, reason } => {
                write!(f, "invalid slug \"{value}\": {reason}")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

impl Cli {
    /// Parse with Clap, then apply the semantic rules from the public CLI
    /// contract. Semantic failures are returned as normal Clap usage errors.
    pub fn try_parse_normalized_from<I, T>(args: I) -> Result<Invocation, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        Self::try_parse_from(args)?.normalize().map_err(clap_error)
    }

    /// Convert raw arguments into one normalized dispatch value.
    pub fn normalize(self) -> Result<Invocation, ValidationError> {
        if self.daemon && self.setup {
            return Err(ValidationError::ConflictingModes);
        }

        if self.daemon {
            if self.port.is_some() || self.command.is_some() {
                return Err(ValidationError::DaemonWithCommand);
            }

            // Recognized non-command options are intentionally ignored in pure
            // daemon mode. Unknown options have already been rejected by Clap.
            return Ok(Invocation::Daemon);
        }

        if self.setup {
            if self.port.is_some() || self.command.is_some() {
                return Err(ValidationError::SetupWithCommand);
            }

            // As in daemon mode, recognized non-command options are ignored.
            return Ok(Invocation::Setup);
        }

        if self.port.is_some() && self.command.is_some() {
            return Err(ValidationError::ShorthandWithSubcommand);
        }

        if let Some(port) = self.port {
            return Ok(Invocation::Start {
                port,
                slug: normalize_optional_slug(self.slug)?,
            });
        }

        match self.command {
            Some(Command::Start { port }) => Ok(Invocation::Start {
                port,
                slug: normalize_optional_slug(self.slug)?,
            }),
            Some(Command::List) => {
                reject_unused_slug(self.slug)?;
                Ok(Invocation::List)
            }
            Some(Command::Stop { selector }) => {
                reject_unused_slug(self.slug)?;
                Ok(Invocation::Stop {
                    selector: parse_stop_selector(&selector)?,
                })
            }
            Some(Command::Kill) => {
                reject_unused_slug(self.slug)?;
                Ok(Invocation::Kill)
            }
            None => {
                reject_unused_slug(self.slug)?;
                Err(ValidationError::MissingCommand)
            }
        }
    }
}

/// Parse a port with a stable, concise error instead of exposing an integer
/// parser implementation detail such as overflow or an invalid digit.
pub fn parse_port(value: &str) -> Result<u16, String> {
    match value.parse::<u16>() {
        Ok(port @ 1..=u16::MAX) => Ok(port),
        _ => Err(ValidationError::InvalidPort(value.to_owned()).to_string()),
    }
}

/// Normalize and validate a user-provided slug according to the v1 DNS-label
/// contract.
pub fn normalize_slug(value: &str) -> Result<String, ValidationError> {
    let normalized = value.to_ascii_lowercase();

    if normalized.is_empty() {
        return Err(invalid_slug(value, "must be between 1 and 63 characters"));
    }
    if normalized.len() > 63 {
        return Err(invalid_slug(value, "must be between 1 and 63 characters"));
    }
    if !normalized.is_ascii() {
        return Err(invalid_slug(
            value,
            "must contain only lowercase ASCII letters, digits, and hyphens",
        ));
    }
    if normalized.starts_with('-') || normalized.ends_with('-') {
        return Err(invalid_slug(value, "must not start or end with a hyphen"));
    }
    if !normalized
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid_slug(
            value,
            "must contain only lowercase ASCII letters, digits, and hyphens",
        ));
    }
    if normalized.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_slug(value, "must not be all numeric"));
    }

    Ok(normalized)
}

/// Parse the `stop` operand. An all-ASCII-digit value is always interpreted as
/// a port, including invalid numeric values such as `0` and `99999`.
pub fn parse_stop_selector(value: &str) -> Result<StopSelector, ValidationError> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return parse_port(value)
            .map(StopSelector::Port)
            .map_err(|_| ValidationError::InvalidPort(value.to_owned()));
    }

    normalize_slug(value).map(StopSelector::Slug)
}

fn normalize_optional_slug(slug: Option<String>) -> Result<Option<String>, ValidationError> {
    slug.as_deref().map(normalize_slug).transpose()
}

fn reject_unused_slug(slug: Option<String>) -> Result<(), ValidationError> {
    if slug.is_some() {
        Err(ValidationError::SlugWithoutStart)
    } else {
        Ok(())
    }
}

fn invalid_slug(value: &str, reason: &'static str) -> ValidationError {
    ValidationError::InvalidSlug {
        value: value.to_owned(),
        reason,
    }
}

fn clap_error(error: ValidationError) -> clap::Error {
    let kind = match error {
        ValidationError::DaemonWithCommand
        | ValidationError::SetupWithCommand
        | ValidationError::ConflictingModes
        | ValidationError::ShorthandWithSubcommand
        | ValidationError::SlugWithoutStart => ErrorKind::ArgumentConflict,
        ValidationError::MissingCommand => ErrorKind::MissingSubcommand,
        ValidationError::InvalidPort(_) | ValidationError::InvalidSlug { .. } => {
            ErrorKind::InvalidValue
        }
    };

    Cli::command().error(kind, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    fn parse(args: &[&str]) -> Result<Invocation, clap::Error> {
        Cli::try_parse_normalized_from(args)
    }

    #[test]
    fn reports_the_embedded_application_version() {
        assert_eq!(Cli::command().get_version(), Some(APP_VERSION));
    }

    #[test]
    fn parses_start_and_shorthand_to_the_same_invocation() {
        let explicit = parse(&["fw", "start", "4321"]).unwrap();
        let shorthand = parse(&["fw", "4321"]).unwrap();

        assert_eq!(
            explicit,
            Invocation::Start {
                port: 4321,
                slug: None
            }
        );
        assert_eq!(explicit, shorthand);
    }

    #[test]
    fn accepts_global_slug_before_or_after_start_and_normalizes_case() {
        let before = parse(&["fw", "--slug", "Green-Apple", "start", "8080"]).unwrap();
        let after = parse(&["fw", "start", "8080", "-s", "Green-Apple"]).unwrap();
        let shorthand = parse(&["fw", "8080", "--slug", "Green-Apple"]).unwrap();
        let expected = Invocation::Start {
            port: 8080,
            slug: Some("green-apple".into()),
        };

        assert_eq!(before, expected);
        assert_eq!(after, expected);
        assert_eq!(shorthand, expected);
    }

    #[test]
    fn parses_control_commands() {
        assert_eq!(parse(&["fw", "list"]).unwrap(), Invocation::List);
        assert_eq!(parse(&["fw", "kill"]).unwrap(), Invocation::Kill);
        assert_eq!(
            parse(&["fw", "stop", "Green-Apple"]).unwrap(),
            Invocation::Stop {
                selector: StopSelector::Slug("green-apple".into())
            }
        );
        assert_eq!(
            parse(&["fw", "stop", "8080"]).unwrap(),
            Invocation::Stop {
                selector: StopSelector::Port(8080)
            }
        );
    }

    #[test]
    fn pure_daemon_mode_ignores_slug_in_either_position() {
        assert_eq!(parse(&["fw", "--daemon"]).unwrap(), Invocation::Daemon);
        assert_eq!(
            parse(&["fw", "--daemon", "--slug", "anything"]).unwrap(),
            Invocation::Daemon
        );
        assert_eq!(
            parse(&["fw", "--slug", "anything", "--daemon"]).unwrap(),
            Invocation::Daemon
        );
    }

    #[test]
    fn daemon_rejects_every_command_form() {
        for args in [
            &["fw", "8080", "--daemon"][..],
            &["fw", "start", "8080", "--daemon"],
            &["fw", "list", "--daemon"],
            &["fw", "stop", "apple-pen", "--daemon"],
            &["fw", "kill", "--daemon"],
        ] {
            let error = parse(args).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{args:?}");
            assert!(error.to_string().contains("--daemon cannot be combined"));
        }
    }

    #[test]
    fn unknown_daemon_option_is_still_rejected_by_clap() {
        let error = parse(&["fw", "--daemon", "--unknown"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn setup_matches_daemon_validation_and_ignores_slug() {
        assert_eq!(parse(&["fw", "--setup"]).unwrap(), Invocation::Setup);
        assert_eq!(
            parse(&["fw", "--slug", "ignored", "--setup"]).unwrap(),
            Invocation::Setup
        );

        for args in [
            &["fw", "8080", "--setup"][..],
            &["fw", "start", "8080", "--setup"],
            &["fw", "list", "--setup"],
            &["fw", "stop", "apple-pen", "--setup"],
            &["fw", "kill", "--setup"],
        ] {
            let error = parse(args).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{args:?}");
            assert!(error.to_string().contains("--setup cannot be combined"));
        }
    }

    #[test]
    fn setup_and_daemon_are_mutually_exclusive() {
        let error = parse(&["fw", "--setup", "--daemon"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn shorthand_and_subcommand_are_mutually_exclusive() {
        let error = parse(&["fw", "8080", "list"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn slug_is_rejected_without_a_start() {
        for args in [
            &["fw", "--slug", "apple-pen"][..],
            &["fw", "list", "--slug", "apple-pen"],
            &["fw", "stop", "apple-pen", "--slug", "other"],
            &["fw", "kill", "--slug", "apple-pen"],
        ] {
            let error = parse(args).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{args:?}");
        }
    }

    #[test]
    fn no_arguments_is_a_usage_error() {
        let error = parse(&["fw"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MissingSubcommand);
        let rendered = error.to_string();
        assert!(rendered.contains("Usage:"));
        assert!(rendered.contains("a port or subcommand is required"));
    }

    #[test]
    fn invalid_ports_have_the_exact_domain_error() {
        for value in ["0", "65536", "99999", "-1", "abc"] {
            assert_eq!(
                parse_port(value).unwrap_err(),
                format!("invalid port \"{value}\"")
            );
        }

        let error = parse(&["fw", "99999"]).unwrap_err().to_string();
        assert!(error.contains("invalid port \"99999\""));
        let stop_error = parse(&["fw", "stop", "99999"]).unwrap_err().to_string();
        assert!(stop_error.contains("invalid port \"99999\""));
    }

    #[test]
    fn accepts_port_boundaries() {
        assert_eq!(parse_port("1"), Ok(1));
        assert_eq!(parse_port("65535"), Ok(65535));
    }

    #[test]
    fn normalizes_and_validates_slugs() {
        assert_eq!(normalize_slug("Apple-Pen").unwrap(), "apple-pen");
        assert_eq!(normalize_slug("a1").unwrap(), "a1");
        assert_eq!(normalize_slug(&"a".repeat(63)).unwrap().len(), 63);

        for invalid in [
            "",
            "-apple",
            "apple-",
            "apple_pen",
            "apple.pen",
            "two words",
            "a/b",
            "café",
        ] {
            assert!(normalize_slug(invalid).is_err(), "{invalid:?}");
        }
        assert!(normalize_slug(&"a".repeat(64)).is_err());
    }

    #[test]
    fn numeric_stop_selectors_are_never_slugs() {
        assert_eq!(parse_stop_selector("1").unwrap(), StopSelector::Port(1));
        assert_eq!(
            parse_stop_selector("65535").unwrap(),
            StopSelector::Port(65535)
        );
        assert_eq!(
            parse_stop_selector("0"),
            Err(ValidationError::InvalidPort("0".into()))
        );
        assert_eq!(
            parse_stop_selector("99999"),
            Err(ValidationError::InvalidPort("99999".into()))
        );
        assert!(matches!(
            parse_stop_selector("123-abc"),
            Ok(StopSelector::Slug(slug)) if slug == "123-abc"
        ));
    }
}
