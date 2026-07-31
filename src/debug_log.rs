//! Debug logging infrastructure.
//!
//! Provides a lock-free bounded queue that captures `tracing` output and
//! feeds it into the TUI debug pane when `DEBUG=1` is set. Also handles
//! stderr redirection to prevent TUI corruption.

use crossbeam_queue::ArrayQueue;
use std::collections::VecDeque;
use std::io::Write;
use std::sync::OnceLock;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;
use tracing_subscriber::prelude::*;

/// Shared bounded queue for debug log messages (capacity: 256).
static LOG_QUEUE: OnceLock<ArrayQueue<String>> = OnceLock::new();

/// Drains all queued messages into the provided deque.
pub fn drain_log_queue(buf: &mut VecDeque<String>) {
    if let Some(queue) = LOG_QUEUE.get() {
        while let Some(msg) = queue.pop() {
            buf.push_back(msg);
            if buf.len() > 500 {
                buf.pop_front();
            }
        }
    }
}

struct DebugLayer;

impl<S: Subscriber> Layer<S> for DebugLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = QueueVisitor;
        event.record(&mut visitor);
    }
}

struct QueueVisitor;

impl tracing::field::Visit for QueueVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let queue = LOG_QUEUE.get_or_init(|| ArrayQueue::new(256));
        let target = field.name();
        let msg = format!("[{}] {:?}", target, value);
        let _ = queue.push(msg);
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        let queue = LOG_QUEUE.get_or_init(|| ArrayQueue::new(256));
        let msg = format!("[{}] {}", field.name(), value);
        let _ = queue.push(msg);
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        let queue = LOG_QUEUE.get_or_init(|| ArrayQueue::new(256));
        let msg = format!("[{}] {}", field.name(), value);
        let _ = queue.push(msg);
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        let queue = LOG_QUEUE.get_or_init(|| ArrayQueue::new(256));
        let msg = format!("[{}] {}", field.name(), value);
        let _ = queue.push(msg);
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        let queue = LOG_QUEUE.get_or_init(|| ArrayQueue::new(256));
        let msg = format!("[{}] {}", field.name(), value);
        let _ = queue.push(msg);
    }
}

/// Initialize the tracing subscriber to route logs to the debug pane.
/// Also redirects stderr to a log file to prevent TUI corruption while
/// preserving crash diagnostics.
pub fn init_logging() {
    use tracing_subscriber::EnvFilter;

    LOG_QUEUE.get_or_init(|| ArrayQueue::new(256));

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(DebugLayer);

    // The global default can only be set once; ignore errors from repeated calls.
    let _ = tracing::subscriber::set_global_default(subscriber);

    // Install panic hook that writes to the log file before the stderr redirect.
    install_panic_hook();

    // Redirect stderr to log file to prevent library warnings from corrupting TUI.
    redirect_stderr_to_logfile();
}

/// Install a panic hook that writes panic info to `/tmp/termixer.log`.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let payload = info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<dyn Any>".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();

        let _ = std::fs::write(
            "/tmp/termixer-panic.log",
            format!(
                "PANIC on thread '{}': {}\nLocation: {}\n\nBacktrace:\n{}\n",
                thread_name, msg, location, backtrace
            ),
        );

        // Also call the default hook (prints to stderr if available).
        default_hook(info);
    }));
}

/// Redirect stderr (fd 2) to a log file so crash reports are diagnosable.
fn redirect_stderr_to_logfile() {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if let Ok(logfile) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/termixer.log")
        {
            let _ = writeln!(
                &logfile,
                "\n--- termixer started at {:?} ---",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            );
            let fd = logfile.as_raw_fd();
            unsafe {
                libc::dup2(fd, 2);
            }
        }
    }
}
