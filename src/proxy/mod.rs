pub mod headers;
pub mod not_found;
pub mod server_error;
pub mod upgrade;

use std::{
    convert::Infallible,
    error::Error,
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
};

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
    server::conn::auto,
};
use rand::seq::SliceRandom;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::config::{
    MAX_PROXY_PORT, MIN_PROXY_PORT, UPSTREAM_CONNECT_TIMEOUT, UPSTREAM_HEADER_TIMEOUT,
};
use crate::metrics::RouteMetrics;

pub type BoxError = Box<dyn Error + Send + Sync>;
pub type ProxyBody = UnsyncBoxBody<Bytes, BoxError>;
pub type ProxyResponse = Response<ProxyBody>;

pub const FALLBACK_HEADER: &str = "x-fw-fallback";
pub const FALLBACK_HEADER_VALUE: &str = "1";

#[derive(Clone, Debug)]
pub struct ProxyRoute {
    pub target: SocketAddr,
    pub metrics: Arc<RouteMetrics>,
}

/// The narrow registry interface needed by the proxy.
///
/// Implementations should clone the route data while holding their read lock and
/// release that lock before this future completes; proxy network I/O must never
/// hold a registry lock.
pub trait RouteLookup: Send + Sync + 'static {
    fn lookup<'a>(
        &'a self,
        slug: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<ProxyRoute>> + Send + 'a>>;
}

#[derive(Clone)]
struct Proxy {
    lookup: Arc<dyn RouteLookup>,
    client: Client<HttpConnector, ProxyBody>,
}

/// Bind and retain the real proxy listener, eliminating the allocation race that
/// would occur if a candidate port were probed and then released.
pub async fn reserve_listener() -> io::Result<TcpListener> {
    let mut ports: Vec<u16> = (MIN_PROXY_PORT..=MAX_PROXY_PORT).collect();
    ports.shuffle(&mut rand::rng());

    let mut last_error = None;
    for port in ports {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no loopback proxy port is available",
        )
    }))
}

pub async fn serve(
    listener: TcpListener,
    lookup: Arc<dyn RouteLookup>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let local_addr = listener.local_addr()?;
    if local_addr.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "proxy listener must be bound to 127.0.0.1",
        ));
    }

    let mut connector = HttpConnector::new();
    connector.enforce_http(true);
    connector.set_connect_timeout(Some(UPSTREAM_CONNECT_TIMEOUT));
    let client = Client::builder(TokioExecutor::new()).build(connector);
    let proxy = Proxy { lookup, client };

    loop {
        let (stream, peer) = tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            accepted = listener.accept() => accepted?,
        };

        let proxy = proxy.clone();
        let connection_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let proxy = proxy.clone();
                async move { Ok::<_, Infallible>(proxy.forward(request).await) }
            });
            let builder = auto::Builder::new(TokioExecutor::new());
            let connection = builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
            tokio::pin!(connection);

            let result = tokio::select! {
                result = &mut connection => result,
                _ = connection_shutdown.cancelled() => {
                    connection.as_mut().graceful_shutdown();
                    connection.await
                }
            };
            if let Err(error) = result {
                tracing::debug!(%peer, %error, "proxy client connection ended with an error");
            }
        });
    }
}

impl Proxy {
    async fn forward(&self, mut request: Request<Incoming>) -> ProxyResponse {
        let public_host = match headers::extract_public_host(&request) {
            Some(host) => host,
            None => return generated_error(StatusCode::BAD_REQUEST, "Invalid host"),
        };

        let route = match self.lookup.lookup(&public_host.slug).await {
            Some(route) => route,
            None => return boxed_response(not_found::response(request.headers())),
        };

        if route.target.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            tracing::error!(target = %route.target, "registry returned a non-loopback proxy target");
            return boxed_response(server_error::response(request.headers()));
        }

        let upgrading = headers::is_upgrade(request.headers());
        let downstream_upgrade = upgrading.then(|| hyper::upgrade::on(&mut request));

        if let Err(error) =
            headers::rewrite_for_upstream(&mut request, route.target, &public_host.hostname)
        {
            tracing::error!(target = %route.target, %error, "proxy request rewrite failed");
            return boxed_response(server_error::response(request.headers()));
        }

        let (parts, body) = request.into_parts();
        let request = Request::from_parts(
            parts,
            counted_body(body, route.metrics.clone(), Direction::Upload),
        );

        let result =
            tokio::time::timeout(UPSTREAM_HEADER_TIMEOUT, self.client.request(request)).await;

        let mut response = match result {
            Err(_) => {
                return generated_error(StatusCode::GATEWAY_TIMEOUT, "Local service timed out");
            }
            Ok(Err(error)) if error_is_timeout(&error) => {
                return generated_error(StatusCode::GATEWAY_TIMEOUT, "Local service timed out");
            }
            Ok(Err(error)) => {
                tracing::debug!(target = %route.target, %error, "local proxy request failed");
                return generated_error(StatusCode::BAD_GATEWAY, "Local service unavailable");
            }
            Ok(Ok(response)) => response,
        };

        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            if let Some(downstream_upgrade) = downstream_upgrade {
                let upstream_upgrade = hyper::upgrade::on(&mut response);
                let metrics = route.metrics.clone();
                tokio::spawn(async move {
                    if let Err(error) =
                        upgrade::bridge(downstream_upgrade, upstream_upgrade, metrics).await
                    {
                        tracing::debug!(%error, "HTTP upgrade failed");
                    }
                });
            }
        }

        response.map(|body| counted_body(body, route.metrics, Direction::Download))
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Upload,
    Download,
}

fn counted_body<B>(body: B, metrics: Arc<RouteMetrics>, direction: Direction) -> ProxyBody
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Error + Send + Sync + 'static,
{
    body.map_frame(move |frame| {
        if let Some(data) = frame.data_ref() {
            let count = data.len() as u64;
            match direction {
                Direction::Upload => metrics.record_upload(count),
                Direction::Download => metrics.record_download(count),
            }
        }
        frame
    })
    .map_err(|error| Box::new(error) as BoxError)
    .boxed_unsync()
}

fn generated_error(status: StatusCode, message: &'static str) -> ProxyResponse {
    let body = Full::new(Bytes::from_static(message.as_bytes()))
        .map_err(|never| match never {})
        .boxed_unsync();

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header(FALLBACK_HEADER, FALLBACK_HEADER_VALUE)
        .body(body)
        .expect("fixed response headers are valid")
}

fn boxed_response(response: Response<Full<Bytes>>) -> ProxyResponse {
    response.map(|body| body.map_err(|never| match never {}).boxed_unsync())
}

fn error_is_timeout(error: &(dyn Error + 'static)) -> bool {
    if error
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::TimedOut)
        || error.is::<tokio::time::error::Elapsed>()
    {
        return true;
    }

    error.source().is_some_and(error_is_timeout)
}

#[cfg(test)]
mod tests {
    use super::{FALLBACK_HEADER, FALLBACK_HEADER_VALUE, generated_error};
    use http::StatusCode;

    #[test]
    fn generated_proxy_errors_are_marked_as_fw_fallbacks() {
        let response = generated_error(StatusCode::BAD_GATEWAY, "Local service unavailable");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers()[FALLBACK_HEADER], FALLBACK_HEADER_VALUE);
    }
}
