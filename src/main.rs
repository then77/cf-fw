mod cli;
mod cloudflare_config;
mod cloudflared;
mod config;
mod daemon;
mod error;
mod ipc;
mod metrics;
mod platform;
mod proxy;
mod registry;
mod slug;
mod ui;

use std::io;
use std::process::ExitCode;
use std::time::Instant;

use tokio::net::windows::named_pipe::NamedPipeClient;

use cli::{Cli, Invocation};
use config::DAEMON_START_TIMEOUT;
use error::{FwError, Result};
use ipc::framing::{read_frame, write_frame};
use ipc::protocol::{
    ClientMessage, Envelope, ServerMessage, StopSelector as ProtocolStopSelector, TerminateReason,
};
use ipc::windows;
use platform::windows::{NamedMutex, UserObjectNames, spawn_daemon};
use ui::{
    PanelLayout, StartupMode, StartupProgress, Statistics, StatisticsRenderer, terminal_width,
    write_info,
};

#[tokio::main]
async fn main() -> ExitCode {
    let invocation = match Cli::try_parse_normalized_from(std::env::args_os()) {
        Ok(invocation) => invocation,
        Err(error) => error.exit(),
    };

    if matches!(invocation, Invocation::Daemon) {
        init_tracing();
    }

    match dispatch(invocation).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .try_init();
}

async fn dispatch(invocation: Invocation) -> Result<()> {
    match invocation {
        Invocation::Daemon => daemon::run().await,
        Invocation::Start { port, slug } => run_start(port, slug).await,
        Invocation::List => run_list().await,
        Invocation::Stop { selector } => {
            let selector = match selector {
                cli::StopSelector::Slug(slug) => ProtocolStopSelector::Slug(slug),
                cli::StopSelector::Port(port) => ProtocolStopSelector::Port(port),
            };
            run_stop(selector).await
        }
        Invocation::Kill => run_kill().await,
    }
}

async fn run_start(port: u16, slug: Option<String>) -> Result<()> {
    let names = UserObjectNames::current()?;
    let initial = windows::connect(&names.pipe);
    let mode = if initial.is_ok() {
        StartupMode::ConnectingToDaemon
    } else {
        StartupMode::StartingDaemon
    };
    let mut progress = StartupProgress::new(mode);

    let result = async {
        let mut pipe = match initial {
            Ok(pipe) => pipe,
            Err(_) => start_and_connect(&names).await?,
        };
        write_frame(
            &mut pipe,
            &Envelope::new(
                Some(1),
                ClientMessage::Register {
                    port,
                    slug,
                    pid: std::process::id(),
                },
            ),
        )
        .await?;
        let response: Envelope<ServerMessage> = read_frame(&mut pipe).await?;
        match response.message {
            ServerMessage::Registered {
                session_id,
                public_url,
                ..
            } => Ok((pipe, session_id, public_url)),
            ServerMessage::Error { message, .. } | ServerMessage::Fatal { message, .. } => {
                Err(FwError::Other(message))
            }
            other => Err(FwError::Protocol(format!(
                "unexpected registration response: {other:?}"
            ))),
        }
    }
    .await;

    let (pipe, session_id, public_url) = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = progress.error(&error.to_string());
            return Err(error);
        }
    };

    progress.success("Successfully started forwarding!")?;
    run_owner_session(pipe, session_id, port, &public_url).await
}

async fn start_and_connect(names: &UserObjectNames) -> Result<NamedPipeClient> {
    // Validate the exact sibling installation before creating any background
    // service process, preserving actionable path errors for the foreground.
    let paths = cloudflare_config::InstallPaths::resolve()?;
    let _startup_mutex = NamedMutex::acquire_startup(&names.startup_mutex)?;
    if let Ok(pipe) = windows::connect(&names.pipe) {
        return Ok(pipe);
    }

    let _child = spawn_daemon(&paths.fw_executable)?;
    let deadline = Instant::now() + DAEMON_START_TIMEOUT;
    loop {
        match windows::connect(&names.pipe) {
            Ok(pipe) => return Ok(pipe),
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(_) => return Err(FwError::DaemonStartTimeout),
        }
    }
}

async fn run_owner_session(
    mut pipe: NamedPipeClient,
    session_id: u64,
    port: u16,
    public_url: &str,
) -> Result<()> {
    let statistics = Statistics::default();
    let panel = PanelLayout::for_port(public_url, port, &statistics, Some(terminal_width()));
    let rendered = panel.render();
    println!("\n{}\n{}\n", rendered.destination, rendered.source);
    let mut renderer = StatisticsRenderer::new(panel.width);
    renderer.update(&statistics);

    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal?;
                renderer.finish_and_clear();
                write_info(&mut io::stdout().lock(), "Shutting down...")?;
                unregister(&mut pipe, session_id).await?;
                return Ok(());
            }
            incoming = read_frame::<_, ServerMessage>(&mut pipe) => {
                let message = incoming?.message;
                match message {
                    ServerMessage::Stats {
                        session_id: event_session,
                        uploaded_bytes,
                        downloaded_bytes,
                        upload_bytes_per_second,
                        download_bytes_per_second,
                    } if event_session == session_id => {
                        renderer.update(&Statistics {
                            uploaded_bytes,
                            downloaded_bytes,
                            upload_bytes_per_second,
                            download_bytes_per_second,
                        });
                    }
                    ServerMessage::Terminate { reason } => {
                        renderer.finish_and_clear();
                        let message = match reason {
                            TerminateReason::RemoteStop => "Shut down remotely via stop command.",
                            TerminateReason::DaemonKill => "Shut down remotely via kill command.",
                        };
                        write_info(&mut io::stdout().lock(), message)?;
                        if reason == TerminateReason::RemoteStop {
                            unregister(&mut pipe, session_id).await?;
                        } else {
                            let _ = write_frame(
                                &mut pipe,
                                &Envelope::new(None, ClientMessage::TerminateAcknowledged { session_id }),
                            ).await;
                        }
                        return Ok(());
                    }
                    ServerMessage::Fatal { message, .. } => {
                        renderer.finish_and_clear();
                        return Err(FwError::CloudflaredExited(message));
                    }
                    ServerMessage::Error { message, .. } => {
                        renderer.finish_and_clear();
                        return Err(FwError::Other(message));
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn unregister(pipe: &mut NamedPipeClient, session_id: u64) -> Result<()> {
    write_frame(
        pipe,
        &Envelope::new(Some(2), ClientMessage::Unregister { session_id }),
    )
    .await?;
    loop {
        match read_frame::<_, ServerMessage>(pipe).await?.message {
            ServerMessage::Unregistered {
                session_id: acknowledged,
            } if acknowledged == session_id => return Ok(()),
            ServerMessage::Stats { .. } => continue,
            ServerMessage::Error { message, .. } => return Err(FwError::Other(message)),
            _ => continue,
        }
    }
}

async fn run_list() -> Result<()> {
    let names = UserObjectNames::current()?;
    let Ok(mut pipe) = windows::connect(&names.pipe) else {
        println!("No active forwards.");
        return Ok(());
    };
    let response = request(&mut pipe, ClientMessage::List).await?;
    match response {
        ServerMessage::RouteList { routes } if routes.is_empty() => {
            println!("No active forwards.");
            Ok(())
        }
        ServerMessage::RouteList { routes } => {
            let slug_width = routes
                .iter()
                .map(|route| route.slug.len())
                .max()
                .unwrap_or(4)
                .max(4);
            let local_width = routes
                .iter()
                .map(|route| route.local_url.len())
                .max()
                .unwrap_or(5)
                .max(5);
            println!("{:<slug_width$}  {:<local_width$}  PUBLIC", "SLUG", "LOCAL");
            for route in routes {
                println!(
                    "{:<slug_width$}  {:<local_width$}  {}",
                    route.slug, route.local_url, route.public_url
                );
            }
            Ok(())
        }
        ServerMessage::Error { message, .. } => Err(FwError::Other(message)),
        other => Err(FwError::Protocol(format!(
            "unexpected list response: {other:?}"
        ))),
    }
}

async fn run_stop(selector: ProtocolStopSelector) -> Result<()> {
    let names = UserObjectNames::current()?;
    let mut pipe = windows::connect(&names.pipe).map_err(|_| FwError::NoActiveForwards)?;
    match request(&mut pipe, ClientMessage::Stop { selector }).await? {
        ServerMessage::Stopped { route } => {
            println!(
                "Stopped {}\n  {}\n  → {}",
                route.slug, route.public_url, route.local_url
            );
            Ok(())
        }
        ServerMessage::Error { message, .. } => Err(FwError::Other(message)),
        other => Err(FwError::Protocol(format!(
            "unexpected stop response: {other:?}"
        ))),
    }
}

async fn run_kill() -> Result<()> {
    let names = UserObjectNames::current()?;
    let Ok(mut pipe) = windows::connect(&names.pipe) else {
        println!("Nothing to stop.");
        return Ok(());
    };
    match request(&mut pipe, ClientMessage::Kill).await? {
        ServerMessage::KillAccepted { route_count } => {
            let noun = if route_count == 1 {
                "forward"
            } else {
                "forwards"
            };
            println!("Stopped {route_count} {noun}.");
            println!("Stopped cloudflared.");
            println!("Stopped fw daemon.");
            Ok(())
        }
        ServerMessage::Error { message, .. } => Err(FwError::Other(message)),
        other => Err(FwError::Protocol(format!(
            "unexpected kill response: {other:?}"
        ))),
    }
}

async fn request(pipe: &mut NamedPipeClient, message: ClientMessage) -> Result<ServerMessage> {
    write_frame(pipe, &Envelope::new(Some(1), message)).await?;
    let response: Envelope<ServerMessage> = read_frame(pipe).await?;
    Ok(response.message)
}
