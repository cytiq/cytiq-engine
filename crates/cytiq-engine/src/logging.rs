use std::env;
use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;

use log::{LevelFilter, Log, Metadata, Record, SetLoggerError};

pub(crate) static LOGGER: SimpleLogger = SimpleLogger::new();

pub(crate) struct SimpleLogger {
    level: OnceLock<LevelFilter>,
    format: OnceLock<LogFormat>,
}

/// `CYTIQ_LOG_FORMAT`: `text` (default) or `json`, one object per line for
/// log collectors (Argo Workflows, Prefect, Loki).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogFormat {
    Text,
    Json,
}

impl SimpleLogger {
    const fn new() -> Self {
        Self {
            level: OnceLock::new(),
            format: OnceLock::new(),
        }
    }
}

impl Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.level.get().copied().unwrap_or(LevelFilter::Off) >= metadata.level().to_level_filter()
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let target = record.module_path_static().unwrap_or(record.target());
        // The binary logs as `cytiq` from every one of its modules.
        let target = if target.starts_with("cytiq::") {
            "cytiq"
        } else {
            target
        };
        let mut stderr = io::stderr();
        let _ = match self.format.get().copied().unwrap_or(LogFormat::Text) {
            LogFormat::Text => {
                writeln!(stderr, "[{}] {}: {}", record.level(), target, record.args())
            }
            LogFormat::Json => writeln!(
                stderr,
                "{}",
                serde_json::json!({
                    "level": record.level().as_str(),
                    "target": target,
                    "message": record.args().to_string(),
                })
            ),
        };
    }

    fn flush(&self) {}
}

pub(crate) fn init_logging() -> Result<(), SetLoggerError> {
    let level = configured_log_level();
    let _ = LOGGER.level.set(level);
    let _ = LOGGER.format.set(parse_log_format(
        env::var("CYTIQ_LOG_FORMAT").ok().as_deref(),
    ));
    log::set_logger(&LOGGER)?;
    log::set_max_level(level);
    Ok(())
}

pub(crate) fn configured_log_level() -> LevelFilter {
    env::var("CYTIQ_LOG")
        .ok()
        .or_else(|| env::var("RUST_LOG").ok())
        .as_deref()
        .and_then(parse_log_level)
        .unwrap_or_else(default_log_level)
}

pub(crate) fn default_log_level() -> LevelFilter {
    if io::stderr().is_terminal() {
        LevelFilter::Info
    } else {
        LevelFilter::Warn
    }
}

/// `json` (any case) selects JSON lines; anything else keeps the text format.
pub(crate) fn parse_log_format(raw: Option<&str>) -> LogFormat {
    match raw {
        Some(value) if value.trim().eq_ignore_ascii_case("json") => LogFormat::Json,
        _ => LogFormat::Text,
    }
}

pub(crate) fn parse_log_level(raw: &str) -> Option<LevelFilter> {
    raw.split(',')
        .rev()
        .find_map(|token| parse_log_level_token(token.trim()))
}

pub(crate) fn parse_log_level_token(token: &str) -> Option<LevelFilter> {
    let candidate = token.rsplit('=').next().unwrap_or(token).trim();
    match candidate.to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_json_selects_json_lines() {
        assert_eq!(parse_log_format(Some("json")), LogFormat::Json);
        assert_eq!(parse_log_format(Some(" JSON ")), LogFormat::Json);
        assert_eq!(parse_log_format(Some("text")), LogFormat::Text);
        assert_eq!(parse_log_format(Some("yaml")), LogFormat::Text);
        assert_eq!(parse_log_format(None), LogFormat::Text);
    }

    #[test]
    fn recognizes_each_level_and_directive_suffix() {
        for (name, level) in [
            ("off", LevelFilter::Off),
            ("error", LevelFilter::Error),
            ("warn", LevelFilter::Warn),
            ("warning", LevelFilter::Warn),
            ("info", LevelFilter::Info),
            ("debug", LevelFilter::Debug),
            ("trace", LevelFilter::Trace),
        ] {
            assert_eq!(parse_log_level_token(name), Some(level));
            assert_eq!(
                parse_log_level_token(&format!("cytiq= {} ", name.to_uppercase())),
                Some(level)
            );
        }
        for name in ["", "cytiq=", "verbose", "info extra"] {
            assert_eq!(parse_log_level_token(name), None);
        }
        assert_eq!(
            parse_log_level("info, cytiq=trace,unknown"),
            Some(LevelFilter::Trace)
        );
    }
}
