//! Logging set-up: Sol logs to stdout; a world's app also keeps a log file
//! of its own, so what a moon or an app started at login did can be read
//! afterwards (their output goes nowhere).

use std::{
    fs::File,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// How large a log file grows before it is started again (the one before
/// is kept as `<file>.1`): a few MB for the two together.
const ROTATE_AT: u64 = 2 * 1024 * 1024;

/// Logs to stdout; set `ORBIT_LOG_FORMAT=json` for structured logs and `RUST_LOG` to filter.
/// Colours only on a terminal, so `docker logs` stays greppable.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stdout().is_terminal());
    if std::env::var("ORBIT_LOG_FORMAT").is_ok_and(|v| v == "json") {
        builder.json().init();
    } else {
        builder.init();
    }
}

/// For a world's app: logs to stdout as [`init`] does and to `file` too,
/// which starts again at a couple of MB (keeping the one before as
/// `<file>.1`). A panic anywhere is logged, with where it happened, before
/// it runs its course. Nothing is written while there is nothing to say.
pub fn init_app(file: &Path) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stdout = fmt::layer().with_ansi(std::io::stdout().is_terminal());
    let registry = tracing_subscriber::registry().with(filter).with(stdout);
    match Rotating::open(file) {
        Some(log) => {
            let log = Arc::new(log);
            registry
                .with(
                    fmt::layer()
                        .with_ansi(false)
                        .with_writer(move || Handle(log.clone())),
                )
                .init();
        }
        None => registry.init(),
    }
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!(
            thread = thread.name().unwrap_or("unnamed"),
            backtrace = %std::backtrace::Backtrace::force_capture(),
            "panicked: {info}"
        );
        before(info);
    }));
}

/// A log file that starts again when it grows too large.
struct Rotating {
    path: PathBuf,
    file: Mutex<(File, u64)>,
}

impl Rotating {
    fn open(path: &Path) -> Option<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok()?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Some(Self {
            path: path.to_owned(),
            file: Mutex::new((file, size)),
        })
    }

    fn write(&self, buf: &[u8]) -> std::io::Result<usize> {
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let (file, size) = &mut *guard;
        if *size + buf.len() as u64 > ROTATE_AT {
            let mut older = self.path.clone().into_os_string();
            older.push(".1");
            let _ = std::fs::rename(&self.path, older);
            *file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            *size = 0;
        }
        file.write_all(buf)?;
        *size += buf.len() as u64;
        Ok(buf.len())
    }
}

/// What the file layer writes each line through.
struct Handle(Arc<Rotating>);

impl Write for Handle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// How often the same kind of damaged data is logged again.
const DAMAGED_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// Logs a warning that something stored or received didn't read (a
/// document's data, a date, a delivery's parameters), which the caller then
/// treats as empty. Each `what` is logged at most once a minute, so a
/// damaged table read on every request doesn't flood the log.
pub fn damaged(what: &'static str, err: impl std::fmt::Display) {
    if first_in_a_while(what, std::time::Instant::now()) {
        tracing::warn!(what, %err, "damaged data read as empty");
    }
}

fn first_in_a_while(what: &'static str, now: std::time::Instant) -> bool {
    static LAST: Mutex<Vec<(&str, std::time::Instant)>> = Mutex::new(Vec::new());
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    match last.iter_mut().find(|(w, _)| *w == what) {
        Some((_, at)) if now.duration_since(*at) < DAMAGED_EVERY => false,
        Some((_, at)) => {
            *at = now;
            true
        }
        None => {
            last.push((what, now));
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damaged_data_is_logged_once_a_minute() {
        let now = std::time::Instant::now();
        assert!(super::first_in_a_while("test row", now));
        assert!(!super::first_in_a_while(
            "test row",
            now + std::time::Duration::from_secs(5)
        ));
        assert!(super::first_in_a_while("another row", now));
        assert!(super::first_in_a_while(
            "test row",
            now + std::time::Duration::from_secs(61)
        ));
    }

    #[test]
    fn a_log_starts_again_when_it_grows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs/terra.log");
        let log = Rotating::open(&path).unwrap();
        let line = vec![b'x'; 1024];
        for _ in 0..(ROTATE_AT / 1024 + 10) {
            log.write(&line).unwrap();
        }
        let now = std::fs::metadata(&path).unwrap().len();
        let older = std::fs::metadata(dir.path().join("logs/terra.log.1"))
            .unwrap()
            .len();
        assert!(older <= ROTATE_AT && older > ROTATE_AT - 2048, "{older}");
        assert_eq!(now, 10 * 1024);
        // Opened again, it carries on where it was.
        drop(log);
        let again = Rotating::open(&path).unwrap();
        assert_eq!(again.file.lock().unwrap().1, now);
    }
}
