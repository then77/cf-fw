use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

#[derive(Debug, Default)]
pub struct RouteMetrics {
    uploaded_bytes: AtomicU64,
    downloaded_bytes: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ByteTotals {
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MetricsSnapshot {
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
    pub upload_bytes_per_second: f64,
    pub download_bytes_per_second: f64,
}

impl RouteMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_upload(&self, bytes: u64) {
        saturating_fetch_add(&self.uploaded_bytes, bytes);
    }

    pub fn record_download(&self, bytes: u64) {
        saturating_fetch_add(&self.downloaded_bytes, bytes);
    }

    pub fn totals(&self) -> ByteTotals {
        ByteTotals {
            uploaded_bytes: self.uploaded_bytes.load(Ordering::Relaxed),
            downloaded_bytes: self.downloaded_bytes.load(Ordering::Relaxed),
        }
    }
}

fn saturating_fetch_add(counter: &AtomicU64, bytes: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(bytes))
    });
}

/// Stateful sampler used by the daemon's periodic statistics task.
#[derive(Debug)]
pub struct MetricsSampler {
    sampled_at: Instant,
    totals: ByteTotals,
}

impl MetricsSampler {
    /// Establish a zero-rate baseline at the current totals.
    pub fn new(metrics: &RouteMetrics) -> Self {
        Self::at(metrics, Instant::now())
    }

    pub fn at(metrics: &RouteMetrics, sampled_at: Instant) -> Self {
        Self {
            sampled_at,
            totals: metrics.totals(),
        }
    }

    pub fn sample(&mut self, metrics: &RouteMetrics) -> MetricsSnapshot {
        self.sample_at(metrics, Instant::now())
    }

    /// Sample totals and calculate rates using the actual elapsed duration.
    pub fn sample_at(&mut self, metrics: &RouteMetrics, sampled_at: Instant) -> MetricsSnapshot {
        let totals = metrics.totals();
        let elapsed = sampled_at.saturating_duration_since(self.sampled_at);
        let elapsed_seconds = elapsed.as_secs_f64();
        let uploaded_delta = totals
            .uploaded_bytes
            .saturating_sub(self.totals.uploaded_bytes);
        let downloaded_delta = totals
            .downloaded_bytes
            .saturating_sub(self.totals.downloaded_bytes);

        self.sampled_at = sampled_at;
        self.totals = totals;

        MetricsSnapshot {
            uploaded_bytes: totals.uploaded_bytes,
            downloaded_bytes: totals.downloaded_bytes,
            upload_bytes_per_second: rate(uploaded_delta, elapsed_seconds),
            download_bytes_per_second: rate(downloaded_delta, elapsed_seconds),
        }
    }
}

fn rate(delta: u64, elapsed_seconds: f64) -> f64 {
    if elapsed_seconds > 0.0 {
        delta as f64 / elapsed_seconds
    } else {
        0.0
    }
}

/// Format a byte total with decimal units for the foreground statistics view.
#[cfg(test)]
pub fn format_bytes(bytes: u64) -> String {
    format_decimal(bytes as f64, &["B", "kB", "MB", "GB", "TB"])
}

/// Format a byte rate with decimal units, excluding the `/s` suffix.
#[cfg(test)]
pub fn format_bytes_per_second(bytes_per_second: f64) -> String {
    format_decimal(bytes_per_second.max(0.0), &["b", "kb", "Mb", "Gb", "Tb"])
}

#[cfg(test)]
fn format_decimal(mut value: f64, units: &[&str]) -> String {
    let mut unit = units[0];
    for next_unit in &units[1..] {
        if value < 1_000.0 {
            break;
        }
        value /= 1_000.0;
        unit = next_unit;
    }

    if value < 100.0 && value.fract() != 0.0 {
        format!("{value:.1}{unit}")
    } else {
        format!("{value:.0}{unit}")
    }
}

#[cfg(test)]
pub fn snapshot_after(
    previous: ByteTotals,
    current: ByteTotals,
    elapsed: Duration,
) -> MetricsSnapshot {
    let seconds = elapsed.as_secs_f64();
    MetricsSnapshot {
        uploaded_bytes: current.uploaded_bytes,
        downloaded_bytes: current.downloaded_bytes,
        upload_bytes_per_second: rate(
            current
                .uploaded_bytes
                .saturating_sub(previous.uploaded_bytes),
            seconds,
        ),
        download_bytes_per_second: rate(
            current
                .downloaded_bytes
                .saturating_sub(previous.downloaded_bytes),
            seconds,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_monotonic_totals() {
        let metrics = RouteMetrics::new();
        metrics.record_upload(10);
        metrics.record_upload(5);
        metrics.record_download(7);
        assert_eq!(
            metrics.totals(),
            ByteTotals {
                uploaded_bytes: 15,
                downloaded_bytes: 7,
            }
        );
    }

    #[test]
    fn rates_use_actual_elapsed_time() {
        let metrics = RouteMetrics::new();
        let start = Instant::now();
        let mut sampler = MetricsSampler::at(&metrics, start);
        metrics.record_upload(900);
        metrics.record_download(300);

        let snapshot = sampler.sample_at(&metrics, start + Duration::from_millis(1500));

        assert_eq!(snapshot.uploaded_bytes, 900);
        assert_eq!(snapshot.downloaded_bytes, 300);
        assert_eq!(snapshot.upload_bytes_per_second, 600.0);
        assert_eq!(snapshot.download_bytes_per_second, 200.0);
    }

    #[test]
    fn zero_elapsed_time_has_zero_rates() {
        let snapshot = snapshot_after(
            ByteTotals::default(),
            ByteTotals {
                uploaded_bytes: 10,
                downloaded_bytes: 20,
            },
            Duration::ZERO,
        );
        assert_eq!(snapshot.upload_bytes_per_second, 0.0);
        assert_eq!(snapshot.download_bytes_per_second, 0.0);
    }

    #[test]
    fn formats_decimal_unit_boundaries() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(999), "999B");
        assert_eq!(format_bytes(1_000), "1kB");
        assert_eq!(format_bytes(1_500), "1.5kB");
        assert_eq!(format_bytes(83_000_000), "83MB");
        assert_eq!(format_bytes_per_second(0.0), "0b");
        assert_eq!(format_bytes_per_second(45_100.0), "45.1kb");
        assert_eq!(format_bytes_per_second(254_000.0), "254kb");
    }
}
