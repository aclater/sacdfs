// SPDX-License-Identifier: GPL-3.0-or-later

//! The read trace (#456): one short log line per open, read and close of a file, and per start
//! and stop of the start-of-track decode-ahead, under the log target [`TARGET`] at `debug`.
//!
//! `-vv` turns it on with the rest of the debug log; `RUST_LOG=info,sacdfs::trace=debug` turns
//! on the trace alone. Off, each place costs one comparison with the log's level
//! ([`enabled`]): nothing is timed or formatted.
//!
//! Each line starts with the milliseconds since the mount started, so that a log shows a
//! client's reads in the order and at the pace they came. Times in a line are milliseconds too.
//! *Why:* a player that glitched on a slow server (#456) read in a way our model of it did not;
//! the trace shows what it read, when, and what each read waited for.

use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The log target of the trace.
pub const TARGET: &str = "sacdfs::trace";

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Marks the start of the mount, which the lines' times count from. Later calls change nothing.
pub fn mark_start() {
    EPOCH.get_or_init(Instant::now);
}

/// Whether the trace is on.
#[inline]
pub fn enabled() -> bool {
    log::log_enabled!(target: TARGET, log::Level::Debug)
}

/// Milliseconds from the mount's start to `t`.
pub fn at(t: Instant) -> Ms {
    Ms(t.saturating_duration_since(*EPOCH.get_or_init(Instant::now)))
}

/// A duration, shown in milliseconds with one decimal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ms(pub Duration);

impl fmt::Display for Ms {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1}", self.0.as_secs_f64() * 1000.0)
    }
}

/// Logs a trace line, with the time since the mount's start in front, if the trace is on.
#[macro_export]
macro_rules! trace_line {
    ($($arg:tt)+) => {
        if $crate::trace::enabled() {
            log::debug!(
                target: $crate::trace::TARGET,
                "{} {}",
                $crate::trace::at(std::time::Instant::now()),
                format_args!($($arg)+)
            );
        }
    };
}

/// What a read of a track file did, for its trace line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadTrace {
    pub fh: u64,
    pub offset: u64,
    pub size: u32,
    /// Bytes answered; `None` if the read failed, or was answered late (the watchdog failed it).
    pub got: Option<usize>,
    /// Whether all its audio was in the cache when it came (a read of no audio counts as one).
    pub hit: bool,
    /// From its arrival until a request thread took it.
    pub queued: Duration,
    /// Waiting for the start-of-track decode-ahead to reach its frames.
    pub start_wait: Duration,
    /// The rest of its time on the thread: decoding, or waiting for another decoding its frames.
    pub decode: Duration,
    /// The frames (file indices `[first, end)`) it had decoded ahead, if any; `start` for the
    /// start-of-track decode-ahead.
    pub ahead: Option<(u32, u32, bool)>,
}

impl fmt::Display for ReadTrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "read fh={} off={} len={}", self.fh, self.offset, self.size)?;
        match self.got {
            Some(n) if n as u64 == u64::from(self.size) => {}
            Some(n) => write!(f, " got={n}")?,
            None => f.write_str(" failed")?,
        }
        f.write_str(if self.hit { " hit" } else { " miss" })?;
        write!(f, " q={} sw={} dec={}", Ms(self.queued), Ms(self.start_wait), Ms(self.decode))?;
        match self.ahead {
            Some((a, b, true)) => write!(f, " ahead=start {a}-{b}"),
            Some((a, b, false)) => write!(f, " ahead={a}-{b}"),
            None => Ok(()),
        }
    }
}

/// A short name of a track file for the trace: its image's file name, cut to 32 characters, its
/// track number, channels and format.
pub fn short_name(file: &crate::track::TrackFile) -> String {
    let image = file.disc.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let mut cut: String = image.chars().take(32).collect();
    if cut.len() < image.len() {
        cut.push('~');
    }
    format!("{cut} #{} {}ch .{}", file.number, file.layout.channels, file.format().extension())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logger(filter: &str) -> env_logger::Logger {
        env_logger::Builder::new().parse_filters(filter).build()
    }

    fn traces(l: &env_logger::Logger) -> bool {
        use log::Log;
        l.enabled(&log::Metadata::builder().target(TARGET).level(log::Level::Debug).build())
    }

    /// `-vv` (debug) and `RUST_LOG=…,sacdfs::trace=debug` turn it on; the default levels and
    /// `-v` don't.
    #[test]
    fn the_trace_is_on_at_debug_or_by_its_target() {
        assert!(!traces(&logger("warn")));
        assert!(!traces(&logger("info")));
        assert!(traces(&logger("debug")));
        assert!(traces(&logger("info,sacdfs::trace=debug")));
        assert!(traces(&logger("sacdfs=debug")));
        assert!(!traces(&logger("debug,sacdfs::trace=info")));
    }

    /// Off by default: the tests install no logger, so the level is off.
    #[test]
    fn off_without_a_logger() {
        assert!(!enabled());
    }

    #[test]
    fn a_read_line_is_short_and_says_what_it_waited_for() {
        let ms = Duration::from_micros;
        let r = ReadTrace {
            fh: 7,
            offset: 1 << 20,
            size: 131072,
            got: Some(131072),
            hit: false,
            queued: ms(50),
            start_wait: ms(212_340),
            decode: ms(1_960),
            ahead: Some((40, 72, true)),
        };
        let line = r.to_string();
        assert_eq!(line, "read fh=7 off=1048576 len=131072 miss q=0.1 sw=212.3 dec=2.0 ahead=start 40-72");
        assert!(line.len() < 100, "{line}");
        let short = ReadTrace { got: Some(100), hit: true, ahead: Some((3, 9, false)), ..r };
        assert_eq!(
            short.to_string(),
            "read fh=7 off=1048576 len=131072 got=100 hit q=0.1 sw=212.3 dec=2.0 ahead=3-9"
        );
        let failed = ReadTrace { got: None, ahead: None, ..r };
        assert!(failed.to_string().contains(" failed miss "), "{failed}");
    }

    #[test]
    fn times_count_from_the_mark() {
        mark_start();
        let t0 = *EPOCH.get().unwrap();
        assert_eq!(at(t0 + Duration::from_millis(1234)).to_string(), "1234.0");
        assert_eq!(at(t0).to_string(), "0.0");
    }
}
