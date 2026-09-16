use std::fmt;
use std::io::{self, IsTerminal, Write};

use console::{Alignment, Style, measure_text_width, pad_str, truncate_str};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::config::SPINNER_TICK;
const BRAILLE_TICKS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const HORIZONTAL_MARGIN: usize = 4;
const ELLIPSIS: &str = "…";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    StartingDaemon,
    ConnectingToDaemon,
}

impl StartupMode {
    pub const fn message(self) -> &'static str {
        match self {
            Self::StartingDaemon => "Starting up daemon...",
            Self::ConnectingToDaemon => "Connecting to daemon...",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Success,
    Error,
    Info,
}

impl StatusKind {
    const fn text(self) -> &'static str {
        match self {
            Self::Success => " SUCCESS ",
            Self::Error => " ERROR ",
            Self::Info => " INFO ",
        }
    }

    fn style(self) -> Style {
        match self {
            Self::Success => Style::new().black().on_green().bold(),
            Self::Error => Style::new().white().on_red().bold(),
            Self::Info => Style::new().white().on_blue().bold(),
        }
    }
}

/// Startup progress that animates on a terminal and emits exactly one plain
/// line when stdout is redirected.
pub struct StartupProgress {
    spinner: Option<ProgressBar>,
}

impl StartupProgress {
    pub fn new(mode: StartupMode) -> Self {
        Self::with_interactivity(mode, io::stdout().is_terminal())
    }

    /// Explicit interactivity is useful for deterministic integration tests.
    pub fn with_interactivity(mode: StartupMode, interactive: bool) -> Self {
        if !interactive {
            println!("⠷ {}", mode.message());
            return Self { spinner: None };
        }

        let spinner = ProgressBar::new_spinner();
        spinner.set_draw_target(ProgressDrawTarget::stdout());
        spinner.set_style(
            ProgressStyle::with_template("{spinner} {msg}")
                .expect("the static spinner template is valid")
                .tick_strings(BRAILLE_TICKS),
        );
        spinner.set_message(mode.message());
        spinner.enable_steady_tick(SPINNER_TICK);

        Self {
            spinner: Some(spinner),
        }
    }

    /// Clear any active animation before another view or final message is
    /// printed. Calling this more than once is harmless.
    pub fn finish_and_clear(&mut self) {
        if let Some(spinner) = self.spinner.take() {
            spinner.finish_and_clear();
        }
    }

    pub fn success(&mut self, message: &str) -> io::Result<()> {
        self.finish_and_clear();
        write_status(&mut io::stdout().lock(), StatusKind::Success, message)
    }

    pub fn error(&mut self, message: &str) -> io::Result<()> {
        self.finish_and_clear();
        write_status(&mut io::stdout().lock(), StatusKind::Error, message)
    }
}

impl Drop for StartupProgress {
    fn drop(&mut self) {
        self.finish_and_clear();
    }
}

/// Print a status label and message to any writer. Styling automatically
/// follows `console`'s terminal/color detection.
pub fn write_status(writer: &mut impl Write, kind: StatusKind, message: &str) -> io::Result<()> {
    writeln!(writer, "{} {message}", kind.style().apply_to(kind.text()))
}

#[cfg(test)]
pub fn write_success(writer: &mut impl Write, message: &str) -> io::Result<()> {
    write_status(writer, StatusKind::Success, message)
}

#[cfg(test)]
pub fn write_error(writer: &mut impl Write, message: &str) -> io::Result<()> {
    write_status(writer, StatusKind::Error, message)
}

pub fn write_info(writer: &mut impl Write, message: &str) -> io::Result<()> {
    write_status(writer, StatusKind::Info, message)
}

/// One metrics sample delivered by the daemon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Statistics {
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
    pub upload_bytes_per_second: f64,
    pub download_bytes_per_second: f64,
}

impl Default for Statistics {
    fn default() -> Self {
        Self {
            uploaded_bytes: 0,
            downloaded_bytes: 0,
            upload_bytes_per_second: 0.0,
            download_bytes_per_second: 0.0,
        }
    }
}

impl fmt::Display for Statistics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "↑ {}/s ({}) ↓ {}/s ({})",
            format_decimal_rate(self.upload_bytes_per_second),
            format_decimal_bytes(self.uploaded_bytes),
            format_decimal_rate(self.download_bytes_per_second),
            format_decimal_bytes(self.downloaded_bytes),
        )
    }
}

/// A display-width-safe, already-truncated layout for the start view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelLayout {
    pub width: usize,
    pub destination: String,
    pub source: String,
    pub statistics: String,
}

impl PanelLayout {
    /// Build a panel using the exact terminal width supplied by the caller.
    /// `None` leaves the natural panel width unclamped.
    pub fn new(
        destination: impl Into<String>,
        source: impl Into<String>,
        statistics: &Statistics,
        terminal_width: Option<usize>,
    ) -> Self {
        let destination = destination.into();
        let source = source.into();
        let statistics = statistics.to_string();
        let source_presentation = format!("→ {source}");
        let natural_width = [
            measure_text_width(&destination),
            measure_text_width(&source_presentation),
            measure_text_width(&statistics),
        ]
        .into_iter()
        .max()
        .unwrap_or_default()
        .saturating_add(HORIZONTAL_MARGIN);
        let width = terminal_width
            .map(|available| natural_width.min(available))
            .unwrap_or(natural_width);
        let content_width = width.saturating_sub(HORIZONTAL_MARGIN);

        let destination = truncate_to_width(&destination, content_width);
        let source_width = content_width.saturating_sub(measure_text_width("→ "));
        let source = truncate_to_width(&source, source_width);

        Self {
            width,
            destination,
            source,
            statistics,
        }
    }

    pub fn for_port(
        destination: impl Into<String>,
        port: u16,
        statistics: &Statistics,
        terminal_width: Option<usize>,
    ) -> Self {
        Self::new(
            destination,
            format!("http://localhost:{port}"),
            statistics,
            terminal_width,
        )
    }

    /// Render the two URL rows and the statistics row. Styles are applied only
    /// after display-width-aware truncation and padding have been calculated.
    pub fn render(&self) -> RenderedPanel {
        self.render_with_styles(true)
    }

    /// Render without ANSI styling, useful for redirected output and snapshots.
    #[cfg(test)]
    pub fn render_plain(&self) -> RenderedPanel {
        self.render_with_styles(false)
    }

    fn render_with_styles(&self, styled: bool) -> RenderedPanel {
        let destination = center(&self.destination, self.width);
        let source_prefix = "→ ";
        let source_content_width =
            measure_text_width(source_prefix) + measure_text_width(&self.source);
        let left = self.width.saturating_sub(source_content_width) / 2;
        let right = self
            .width
            .saturating_sub(source_content_width)
            .saturating_sub(left);
        let source = if self.width < measure_text_width(source_prefix) {
            center(&truncate_to_width(source_prefix, self.width), self.width)
        } else if styled {
            format!(
                "{}{}{}{}",
                " ".repeat(left),
                source_prefix,
                Style::new().dim().apply_to(&self.source),
                " ".repeat(right)
            )
        } else {
            format!(
                "{}{}{}{}",
                " ".repeat(left),
                source_prefix,
                self.source,
                " ".repeat(right)
            )
        };
        let statistics = center(&truncate_to_width(&self.statistics, self.width), self.width);
        let statistics = if styled {
            Style::new()
                .white()
                .on_black()
                .on_bright()
                .apply_to(statistics)
                .to_string()
        } else {
            statistics
        };

        RenderedPanel {
            width: self.width,
            destination,
            source,
            statistics,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPanel {
    pub width: usize,
    pub destination: String,
    pub source: String,
    pub statistics: String,
}

/// Current stdout width for callers constructing a [`PanelLayout`].
pub fn terminal_width() -> usize {
    console::Term::stdout().size().1 as usize
}

/// In-place statistics line backed by `indicatif`. It is hidden for redirected
/// stdout so updates never append an unbounded stream of lines.
pub struct StatisticsRenderer {
    progress: Option<ProgressBar>,
    width: usize,
}

impl StatisticsRenderer {
    pub fn new(width: usize) -> Self {
        Self::with_interactivity(width, io::stdout().is_terminal())
    }

    pub fn with_interactivity(width: usize, interactive: bool) -> Self {
        if !interactive {
            return Self {
                progress: None,
                width,
            };
        }

        let progress = ProgressBar::with_draw_target(None, ProgressDrawTarget::stdout());
        progress.set_style(
            ProgressStyle::with_template("{msg}").expect("the static statistics template is valid"),
        );
        Self {
            progress: Some(progress),
            width,
        }
    }

    pub fn update(&self, statistics: &Statistics) {
        let line = center(
            &truncate_to_width(&statistics.to_string(), self.width),
            self.width,
        );
        let styled = Style::new()
            .white()
            .on_black()
            .on_bright()
            .apply_to(line)
            .to_string();

        if let Some(progress) = &self.progress {
            progress.set_message(styled);
            progress.tick();
        }
    }

    pub fn finish_and_clear(&mut self) {
        if let Some(progress) = self.progress.take() {
            progress.finish_and_clear();
        }
    }
}

impl Drop for StatisticsRenderer {
    fn drop(&mut self) {
        self.finish_and_clear();
    }
}

/// Format byte totals with decimal (base-1000) units.
pub fn format_decimal_bytes(bytes: u64) -> String {
    format_decimal(bytes as f64, &["B", "kB", "MB", "GB", "TB", "PB"])
}

/// Format byte rates with the lowercase unit spelling specified by the CLI
/// contract. The `/s` suffix is supplied by the statistics line.
pub fn format_decimal_rate(bytes_per_second: f64) -> String {
    format_decimal(
        sanitize_rate(bytes_per_second),
        &["b", "kb", "Mb", "Gb", "Tb", "Pb"],
    )
}

fn sanitize_rate(rate: f64) -> f64 {
    if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        0.0
    }
}

fn format_decimal(mut value: f64, units: &[&str]) -> String {
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < units.len() {
        value /= 1000.0;
        unit += 1;
    }

    let number = if value >= 100.0 || value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        let one_decimal = format!("{value:.1}");
        one_decimal
            .strip_suffix(".0")
            .unwrap_or(&one_decimal)
            .to_owned()
    };

    format!("{number}{}", units[unit])
}

fn truncate_to_width(value: &str, width: usize) -> String {
    if width == 0 {
        String::new()
    } else if measure_text_width(value) <= width {
        value.to_owned()
    } else {
        truncate_str(value, width, ELLIPSIS).into_owned()
    }
}

fn center(value: &str, width: usize) -> String {
    pad_str(value, width, Alignment::Center, None).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_width(value: &str, width: usize) {
        assert_eq!(measure_text_width(value), width, "{value:?}");
    }

    #[test]
    fn status_labels_have_the_contract_text() {
        let mut output = Vec::new();
        write_success(&mut output, "Successfully started forwarding!").unwrap();
        write_error(&mut output, "Something failed").unwrap();
        write_info(&mut output, "Shutting down...").unwrap();
        let output = String::from_utf8(output).unwrap();
        let visible = console::strip_ansi_codes(&output);

        assert_eq!(
            visible,
            " SUCCESS  Successfully started forwarding!\n ERROR  Something failed\n INFO  Shutting down...\n"
        );
    }

    #[test]
    fn statistics_start_at_zero() {
        assert_eq!(Statistics::default().to_string(), "↑ 0b/s (0B) ↓ 0b/s (0B)");
    }

    #[test]
    fn statistics_match_the_plan_examples() {
        let statistics = Statistics {
            uploaded_bytes: 83_000_000,
            downloaded_bytes: 487_000_000,
            upload_bytes_per_second: 45_100.0,
            download_bytes_per_second: 254_000.0,
        };

        assert_eq!(
            statistics.to_string(),
            "↑ 45.1kb/s (83MB) ↓ 254kb/s (487MB)"
        );
    }

    #[test]
    fn decimal_byte_totals_respect_unit_boundaries() {
        for (value, expected) in [
            (0, "0B"),
            (999, "999B"),
            (1_000, "1kB"),
            (1_500, "1.5kB"),
            (10_000, "10kB"),
            (999_000, "999kB"),
            (1_000_000, "1MB"),
            (1_000_000_000, "1GB"),
            (1_000_000_000_000, "1TB"),
        ] {
            assert_eq!(format_decimal_bytes(value), expected, "{value}");
        }
    }

    #[test]
    fn decimal_rates_respect_unit_boundaries_and_bad_samples() {
        for (value, expected) in [
            (0.0, "0b"),
            (999.0, "999b"),
            (1_000.0, "1kb"),
            (45_100.0, "45.1kb"),
            (254_000.0, "254kb"),
            (1_000_000.0, "1Mb"),
        ] {
            assert_eq!(format_decimal_rate(value), expected, "{value}");
        }
        assert_eq!(format_decimal_rate(-1.0), "0b");
        assert_eq!(format_decimal_rate(f64::NAN), "0b");
        assert_eq!(format_decimal_rate(f64::INFINITY), "0b");
    }

    #[test]
    fn panel_uses_natural_display_width_plus_four() {
        let destination = "https://green-apple.fw.rlzy.me";
        let source = "http://localhost:8080";
        let stats = Statistics::default();
        let panel = PanelLayout::new(destination, source, &stats, None);
        let expected = [
            measure_text_width(destination),
            measure_text_width(&format!("→ {source}")),
            measure_text_width(&stats.to_string()),
        ]
        .into_iter()
        .max()
        .unwrap()
            + 4;

        assert_eq!(panel.width, expected);
        let rendered = panel.render_plain();
        assert_width(&rendered.destination, expected);
        assert_width(&rendered.source, expected);
        assert_width(&rendered.statistics, expected);
    }

    #[test]
    fn ansi_styling_does_not_change_visible_alignment() {
        let panel = PanelLayout::for_port(
            "https://silent-panda.fw.rlzy.me",
            4321,
            &Statistics::default(),
            Some(80),
        );
        let plain = panel.render_plain();
        let styled = panel.render();

        for (plain_line, styled_line) in [
            (&plain.destination, &styled.destination),
            (&plain.source, &styled.source),
            (&plain.statistics, &styled.statistics),
        ] {
            assert_eq!(
                measure_text_width(plain_line),
                measure_text_width(styled_line)
            );
            assert_width(styled_line, panel.width);
        }
    }

    #[test]
    fn long_unicode_presentations_are_truncated_to_terminal_width() {
        let panel = PanelLayout::new(
            "https://非常に長い名前-green-apple.fw.rlzy.me",
            "http://localhost:65535/a/very/long/source",
            &Statistics::default(),
            Some(30),
        );
        let rendered = panel.render_plain();

        assert_eq!(panel.width, 30);
        assert!(panel.destination.ends_with(ELLIPSIS));
        assert!(panel.source.ends_with(ELLIPSIS));
        assert_width(&rendered.destination, 30);
        assert_width(&rendered.source, 30);
        assert_width(&rendered.statistics, 30);
    }

    #[test]
    fn tiny_terminal_widths_do_not_underflow_or_overflow() {
        for width in 0..=4 {
            let panel = PanelLayout::for_port(
                "https://long.example",
                65535,
                &Statistics::default(),
                Some(width),
            );
            let rendered = panel.render_plain();
            assert!(measure_text_width(&rendered.destination) <= width);
            assert!(measure_text_width(&rendered.source) <= width);
            assert!(measure_text_width(&rendered.statistics) <= width);
        }
    }

    #[test]
    fn panel_uses_localhost_for_the_displayed_source() {
        let panel = PanelLayout::for_port(
            "https://apple-pen.fw.rlzy.me",
            4321,
            &Statistics::default(),
            None,
        );

        assert_eq!(panel.source, "http://localhost:4321");
        assert!(!panel.source.contains("127.0.0.1"));
    }
}
