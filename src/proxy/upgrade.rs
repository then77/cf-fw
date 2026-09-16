use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::metrics::RouteMetrics;

pub async fn bridge(
    downstream: hyper::upgrade::OnUpgrade,
    upstream: hyper::upgrade::OnUpgrade,
    metrics: Arc<RouteMetrics>,
) -> Result<(), hyper::Error> {
    let (downstream, upstream) = tokio::try_join!(downstream, upstream)?;
    let mut downstream = CountedIo::upload(TokioIo::new(downstream), metrics.clone());
    let mut upstream = CountedIo::download(TokioIo::new(upstream), metrics);

    if let Err(error) = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await {
        tracing::debug!(%error, "upgraded proxy stream closed with an I/O error");
    }
    Ok(())
}

struct CountedIo {
    inner: TokioIo<Upgraded>,
    metrics: Arc<RouteMetrics>,
    direction: Direction,
}

#[derive(Clone, Copy)]
enum Direction {
    Upload,
    Download,
}

impl CountedIo {
    fn upload(inner: TokioIo<Upgraded>, metrics: Arc<RouteMetrics>) -> Self {
        Self {
            inner,
            metrics,
            direction: Direction::Upload,
        }
    }

    fn download(inner: TokioIo<Upgraded>, metrics: Arc<RouteMetrics>) -> Self {
        Self {
            inner,
            metrics,
            direction: Direction::Download,
        }
    }
}

impl AsyncRead for CountedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(context, buffer);
        let read = buffer.filled().len().saturating_sub(before) as u64;
        if read != 0 {
            match self.direction {
                Direction::Upload => self.metrics.record_upload(read),
                Direction::Download => self.metrics.record_download(read),
            }
        }
        result
    }
}

impl AsyncWrite for CountedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}
