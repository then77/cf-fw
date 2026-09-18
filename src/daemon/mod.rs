pub mod lifecycle;
pub mod state;

use std::collections::HashMap;
#[cfg(any(unix, test))]
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, Notify, RwLock, mpsc};
#[cfg(unix)]
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cloudflare_config::{self, InstallPaths};
use crate::cloudflared::CloudflaredProcess;
use crate::config::{DAEMON_IDLE_TIMEOUT, REMOTE_STOP_TIMEOUT, STATS_INTERVAL};
use crate::error::{FwError, Result};
use crate::ipc::framing::{read_frame, write_frame};
use crate::ipc::protocol::{
    ClientMessage, Envelope, ErrorCode, ServerMessage, StopSelector, TerminateReason,
};
use crate::metrics::MetricsSampler;
use crate::platform::RuntimeScope;
use crate::proxy;
use crate::registry::{Route, RouteRegistry};

use lifecycle::RegistryLookup;

const CHANNEL_CAPACITY: usize = 32;

#[derive(Clone)]
struct Shared {
    registry: Arc<RwLock<RouteRegistry>>,
    owners: Arc<RwLock<HashMap<u64, mpsc::Sender<Envelope<ServerMessage>>>>>,
    acknowledgements: Arc<Mutex<HashMap<u64, Arc<Notify>>>>,
    next_session: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
    idle_since: Arc<Mutex<Option<Instant>>>,
    base_domain: Arc<str>,
    shutdown: CancellationToken,
}

impl Shared {
    fn new(shutdown: CancellationToken, base_domain: Arc<str>) -> Self {
        Self {
            registry: Arc::new(RwLock::new(RouteRegistry::new())),
            owners: Arc::new(RwLock::new(HashMap::new())),
            acknowledgements: Arc::new(Mutex::new(HashMap::new())),
            next_session: Arc::new(AtomicU64::new(1)),
            shutting_down: Arc::new(AtomicBool::new(false)),
            idle_since: Arc::new(Mutex::new(None)),
            base_domain,
            shutdown,
        }
    }

    async fn mark_active(&self) {
        *self.idle_since.lock().await = None;
    }

    async fn mark_idle_if_empty(&self) {
        if self.registry.read().await.is_empty() {
            let mut idle = self.idle_since.lock().await;
            if idle.is_none() {
                *idle = Some(Instant::now());
            }
        }
    }

    async fn remove_session(&self, session_id: u64) {
        self.registry.write().await.remove_session(session_id);
        self.owners.write().await.remove(&session_id);
        if let Some(notify) = self.acknowledgements.lock().await.remove(&session_id) {
            notify.notify_waiters();
        }
        self.mark_idle_if_empty().await;
    }

    async fn send_owner(&self, session_id: u64, message: ServerMessage) -> bool {
        let sender = self.owners.read().await.get(&session_id).cloned();
        match sender {
            Some(sender) => sender.send(Envelope::new(None, message)).await.is_ok(),
            None => false,
        }
    }
}

pub async fn run() -> Result<()> {
    let scope = RuntimeScope::current()?;
    let _daemon_guard = scope.acquire_daemon()?;
    let paths = InstallPaths::resolve()?;

    let listener = proxy::reserve_listener()
        .await
        .map_err(|error| match error.kind() {
            io::ErrorKind::AddrNotAvailable => FwError::NoAvailableProxyPort,
            _ => error.into(),
        })?;
    let proxy_port = listener.local_addr()?.port();
    let base_domain: Arc<str> =
        cloudflare_config::normalize_validate_and_replace(&paths, proxy_port)
            .await?
            .into();

    let ipc_listener = crate::ipc::Listener::bind(scope.endpoint())?;

    let root = CancellationToken::new();
    #[cfg(unix)]
    let signal_task = spawn_unix_signal_task(root.clone())?;
    let shared = Shared::new(root.clone(), base_domain.clone());
    let lookup = Arc::new(RegistryLookup {
        registry: shared.registry.clone(),
    });
    let proxy_shutdown = root.child_token();
    let proxy_task = tokio::spawn(proxy::serve(listener, lookup, base_domain, proxy_shutdown));

    let mut cloudflared = match CloudflaredProcess::spawn(
        &paths.cloudflared,
        &paths.cloudflare_config,
        &paths.cloudflare_dir,
    )
    .await
    {
        Ok(process) => process,
        Err(error) => {
            root.cancel();
            let _ = proxy_task.await;
            #[cfg(unix)]
            let _ = signal_task.await;
            return Err(error);
        }
    };
    if let Err(error) = cloudflared.stabilize().await {
        root.cancel();
        let _ = cloudflared.shutdown().await;
        let _ = proxy_task.await;
        #[cfg(unix)]
        let _ = signal_task.await;
        return Err(error);
    }

    shared.mark_idle_if_empty().await;
    let accept_shared = shared.clone();
    let accept_shutdown = root.child_token();
    let accept_task =
        tokio::spawn(
            async move { accept_loop(ipc_listener, accept_shared, accept_shutdown).await },
        );

    let idle_shared = shared.clone();
    let idle_task = tokio::spawn(async move { idle_monitor(idle_shared).await });

    let mut fatal_error = None;
    while !root.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(100)).await;
        match cloudflared.try_wait_exit().await {
            Ok(Some(exit)) => {
                let details = if exit.recent_output.is_empty() {
                    exit.message()
                } else {
                    format!("{}\n{}", exit.message(), exit.recent_output)
                };
                broadcast_fatal(&shared, details.clone()).await;
                fatal_error = Some(FwError::CloudflaredExited(details));
                root.cancel();
            }
            Ok(None) => {}
            Err(error) => {
                fatal_error = Some(error);
                root.cancel();
            }
        }
    }

    shared.shutting_down.store(true, Ordering::SeqCst);
    let _ = cloudflared.shutdown().await;
    let _ = accept_task.await;
    let _ = idle_task.await;
    #[cfg(unix)]
    match signal_task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) if fatal_error.is_none() => fatal_error = Some(error.into()),
        Err(error) if fatal_error.is_none() => fatal_error = Some(error.into()),
        _ => {}
    }
    match proxy_task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) if fatal_error.is_none() => fatal_error = Some(error.into()),
        Err(error) if fatal_error.is_none() => fatal_error = Some(error.into()),
        _ => {}
    }

    if let Some(error) = fatal_error {
        Err(error)
    } else {
        Ok(())
    }
}

#[cfg(any(unix, test))]
async fn cancel_on_trigger<F>(shutdown: CancellationToken, trigger: F)
where
    F: Future<Output = ()>,
{
    tokio::pin!(trigger);
    tokio::select! {
        _ = shutdown.cancelled() => {}
        _ = &mut trigger => shutdown.cancel(),
    }
}

#[cfg(unix)]
fn spawn_unix_signal_task(shutdown: CancellationToken) -> Result<JoinHandle<io::Result<()>>> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(tokio::spawn(async move {
        let received = async move {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        };
        cancel_on_trigger(shutdown, received).await;
        Ok(())
    }))
}

async fn accept_loop(
    mut listener: crate::ipc::Listener,
    shared: Shared,
    shutdown: CancellationToken,
) -> Result<()> {
    loop {
        let connected = tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            connected = listener.accept() => connected?,
        };
        let session_shared = shared.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(connected, session_shared).await {
                tracing::debug!(%error, "IPC session ended");
            }
        });
    }
}

async fn handle_connection<S>(stream: S, shared: Shared) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (outgoing, mut receiver) = mpsc::channel::<Envelope<ServerMessage>>(CHANNEL_CAPACITY);
    let writer_task = tokio::spawn(async move {
        while let Some(envelope) = receiver.recv().await {
            write_frame(&mut writer, &envelope).await?;
        }
        Result::<()>::Ok(())
    });
    let mut owned_session = None;

    loop {
        let envelope = match read_frame::<_, ClientMessage>(&mut reader).await {
            Ok(envelope) => envelope,
            Err(error) if is_disconnect(&error) => break,
            Err(error) => return Err(error),
        };
        let request_id = envelope.request_id;
        match envelope.message {
            ClientMessage::Register { port, slug, pid } => {
                if owned_session.is_some() || shared.shutting_down.load(Ordering::SeqCst) {
                    send_response(
                        &outgoing,
                        request_id,
                        ServerMessage::Error {
                            code: ErrorCode::DaemonShuttingDown,
                            message: "fw daemon is shutting down".into(),
                        },
                    )
                    .await?;
                    continue;
                }
                let session_id = shared.next_session.fetch_add(1, Ordering::Relaxed);
                let registration =
                    shared
                        .registry
                        .write()
                        .await
                        .register(port, slug.as_deref(), session_id, pid);
                match registration {
                    Ok(route) => {
                        owned_session = Some(session_id);
                        shared
                            .owners
                            .write()
                            .await
                            .insert(session_id, outgoing.clone());
                        shared.mark_active().await;
                        let view = route.view(&shared.base_domain);
                        send_response(
                            &outgoing,
                            request_id,
                            ServerMessage::Registered {
                                session_id,
                                slug: view.slug,
                                local_url: view.local_url,
                                public_url: view.public_url,
                            },
                        )
                        .await?;
                        spawn_stats(shared.clone(), route, outgoing.clone());
                    }
                    Err(error) => {
                        send_response(&outgoing, request_id, error_message(&error)).await?;
                    }
                }
            }
            ClientMessage::Unregister { session_id } => {
                if owned_session == Some(session_id) {
                    shared.remove_session(session_id).await;
                    send_response(
                        &outgoing,
                        request_id,
                        ServerMessage::Unregistered { session_id },
                    )
                    .await?;
                    owned_session = None;
                } else {
                    send_response(
                        &outgoing,
                        request_id,
                        ServerMessage::Error {
                            code: ErrorCode::InvalidRequest,
                            message: "session is not owned by this connection".into(),
                        },
                    )
                    .await?;
                }
            }
            ClientMessage::TerminateAcknowledged { session_id } => {
                if owned_session == Some(session_id) {
                    shared.remove_session(session_id).await;
                    owned_session = None;
                }
            }
            ClientMessage::List => {
                let routes = shared.registry.read().await.list(&shared.base_domain);
                send_response(&outgoing, request_id, ServerMessage::RouteList { routes }).await?;
            }
            ClientMessage::Stop { selector } => {
                stop_route(&shared, &outgoing, request_id, selector).await?;
            }
            ClientMessage::Kill => {
                kill_daemon(&shared, &outgoing, request_id).await?;
            }
        }
    }

    if let Some(session_id) = owned_session {
        shared.remove_session(session_id).await;
    }
    drop(outgoing);
    writer_task.await??;
    Ok(())
}

async fn stop_route(
    shared: &Shared,
    outgoing: &mpsc::Sender<Envelope<ServerMessage>>,
    request_id: Option<u64>,
    selector: StopSelector,
) -> Result<()> {
    let route = shared.registry.read().await.resolve(&selector).cloned();
    let Some(route) = route else {
        let selector = match selector {
            StopSelector::Slug(value) => value,
            StopSelector::Port(value) => value.to_string(),
        };
        return send_response(
            outgoing,
            request_id,
            ServerMessage::Error {
                code: ErrorCode::RouteNotFound,
                message: format!("no active forward matches \"{selector}\""),
            },
        )
        .await;
    };

    let notify = Arc::new(Notify::new());
    shared
        .acknowledgements
        .lock()
        .await
        .insert(route.owner_session, notify.clone());
    let _ = shared
        .send_owner(
            route.owner_session,
            ServerMessage::Terminate {
                reason: TerminateReason::RemoteStop,
            },
        )
        .await;
    if tokio::time::timeout(REMOTE_STOP_TIMEOUT, notify.notified())
        .await
        .is_err()
    {
        shared.remove_session(route.owner_session).await;
    }
    send_response(
        outgoing,
        request_id,
        ServerMessage::Stopped {
            route: route.view(&shared.base_domain),
        },
    )
    .await
}

async fn kill_daemon(
    shared: &Shared,
    outgoing: &mpsc::Sender<Envelope<ServerMessage>>,
    request_id: Option<u64>,
) -> Result<()> {
    if shared.shutting_down.swap(true, Ordering::SeqCst) {
        return send_response(
            outgoing,
            request_id,
            ServerMessage::Error {
                code: ErrorCode::DaemonShuttingDown,
                message: "fw daemon is shutting down".into(),
            },
        )
        .await;
    }

    let routes = shared.registry.write().await.clear();
    let owners = shared.owners.read().await.clone();
    for sender in owners.values() {
        let _ = sender
            .send(Envelope::new(
                None,
                ServerMessage::Terminate {
                    reason: TerminateReason::DaemonKill,
                },
            ))
            .await;
    }
    send_response(
        outgoing,
        request_id,
        ServerMessage::KillAccepted {
            route_count: routes.len(),
        },
    )
    .await?;
    let shutdown = shared.shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        shutdown.cancel();
    });
    Ok(())
}

fn spawn_stats(shared: Shared, route: Route, outgoing: mpsc::Sender<Envelope<ServerMessage>>) {
    tokio::spawn(async move {
        let mut sampler = MetricsSampler::new(&route.metrics);
        let mut interval = tokio::time::interval(STATS_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            if shared
                .registry
                .read()
                .await
                .route_for_slug(&route.slug)
                .is_none()
            {
                break;
            }
            let sample = sampler.sample(&route.metrics);
            if outgoing
                .send(Envelope::new(
                    None,
                    ServerMessage::Stats {
                        session_id: route.owner_session,
                        uploaded_bytes: sample.uploaded_bytes,
                        downloaded_bytes: sample.downloaded_bytes,
                        upload_bytes_per_second: sample.upload_bytes_per_second,
                        download_bytes_per_second: sample.download_bytes_per_second,
                    },
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });
}

async fn idle_monitor(shared: Shared) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = shared.shutdown.cancelled() => return,
            _ = interval.tick() => {
                let expired = shared.idle_since.lock().await
                    .is_some_and(|since| since.elapsed() >= DAEMON_IDLE_TIMEOUT);
                if expired {
                    shared.shutting_down.store(true, Ordering::SeqCst);
                    shared.shutdown.cancel();
                    return;
                }
            }
        }
    }
}

async fn broadcast_fatal(shared: &Shared, message: String) {
    shared.shutting_down.store(true, Ordering::SeqCst);
    let owners = shared.owners.read().await.clone();
    for sender in owners.values() {
        let _ = sender
            .send(Envelope::new(
                None,
                ServerMessage::Fatal {
                    code: ErrorCode::CloudflaredExited,
                    message: message.clone(),
                },
            ))
            .await;
    }
    shared.registry.write().await.clear();
}

async fn send_response(
    sender: &mpsc::Sender<Envelope<ServerMessage>>,
    request_id: Option<u64>,
    message: ServerMessage,
) -> Result<()> {
    sender
        .send(Envelope::new(request_id, message))
        .await
        .map_err(|_| FwError::Protocol("IPC writer closed".into()))
}

fn error_message(error: &FwError) -> ServerMessage {
    let code = match error {
        FwError::InvalidPort(_) => ErrorCode::InvalidPort,
        FwError::InvalidSlug(_, _) => ErrorCode::InvalidSlug,
        FwError::DuplicateSlug(_) => ErrorCode::DuplicateSlug,
        FwError::DuplicatePort { .. } => ErrorCode::DuplicatePort,

        FwError::NoActiveForwards => ErrorCode::NoActiveForwards,
        _ => ErrorCode::Internal,
    };
    ServerMessage::Error {
        code,
        message: error.to_string(),
    }
}

fn is_disconnect(error: &FwError) -> bool {
    matches!(error, FwError::Io(io_error) if matches!(
        io_error.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signal_waiter_exits_when_another_shutdown_path_wins() {
        let shutdown = CancellationToken::new();
        let waiter = tokio::spawn(cancel_on_trigger(
            shutdown.clone(),
            std::future::pending::<()>(),
        ));

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("signal waiter did not stop after cancellation")
            .expect("signal waiter task failed");
    }

    #[tokio::test]
    async fn signal_trigger_cancels_the_shared_shutdown_token() {
        let shutdown = CancellationToken::new();
        cancel_on_trigger(shutdown.clone(), std::future::ready(())).await;
        assert!(shutdown.is_cancelled());
    }
}
