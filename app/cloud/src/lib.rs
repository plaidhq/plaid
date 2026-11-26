//! plaid-cloud: accounts, usage and the dictation websocket (docs/cloud.md).
pub mod admin;
pub mod config;
pub mod db;
pub mod llm;
pub mod server;

/// A log line: ids, sizes and timings only, never text or audio.
#[macro_export]
macro_rules! log {
    ($($t:tt)*) => {
        eprintln!("{} {}", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), format_args!($($t)*))
    };
}
