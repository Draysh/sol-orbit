use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Logs to stdout; set `ORBIT_LOG_FORMAT=json` for structured logs and `RUST_LOG` to filter.
/// Colours only on a terminal, so `docker logs` stays greppable.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stdout().is_terminal());
    if std::env::var("ORBIT_LOG_FORMAT").is_ok_and(|v| v == "json") {
        builder.json().init();
    } else {
        builder.init();
    }
}
