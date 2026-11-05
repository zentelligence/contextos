//! Capturing what the code under test logs, so a test can observe a warning or an info line as an operator would.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, Once};

use tracing_subscriber::fmt::MakeWriter;

/// A log writer that keeps every line.
#[derive(Clone, Default)]
pub struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturingWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl CapturingWriter {
    /// Every line logged so far.
    pub fn lines(&self) -> Vec<String> {
        let bytes = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        String::from_utf8_lossy(&bytes).lines().map(str::to_owned).collect()
    }

    /// The lines at `level` that mention `needle`.
    pub fn at(&self, level: &str, needle: &str) -> Vec<String> {
        let marker = format!(" {level} ");
        self.lines()
            .into_iter()
            .filter(|line| line.contains(&marker) && line.contains(needle))
            .collect()
    }
}

thread_local! {
    /// Where this thread's log lines go while a capture is open.
    static SINK: RefCell<Option<CapturingWriter>> = const { RefCell::new(None) };
}

/// The one writer every event goes to: the open capture of the thread that logged it, and nowhere when there is none.
#[derive(Clone, Copy)]
struct RoutedWriter;

impl std::io::Write for RoutedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        SINK.with(|sink| match sink.borrow_mut().as_mut() {
            Some(writer) => writer.write(buf),
            None => Ok(buf.len()),
        })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for RoutedWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        *self
    }
}

/// Installs the process-wide subscriber once. It has to be global rather than set per test: tracing caches, for each
/// log call site, whether any subscriber wants it, and a test that runs a call site for the first time with no
/// subscriber of its own installed can cache "nobody wants it" just as another test's thread-scoped subscriber is being
/// installed, which silently drops that test's events for the rest of the process. A subscriber that is always there
/// leaves nothing to cache wrongly.
fn install() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_writer(RoutedWriter)
                .with_ansi(false)
                .finish(),
        );
    });
}

/// Ends the capture it was returned with when it is dropped.
pub struct CaptureGuard;

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        SINK.with(|sink| *sink.borrow_mut() = None);
    }
}

/// Starts capturing this thread's log. Keep the guard for as long as the capture should last.
pub fn capture() -> (CapturingWriter, CaptureGuard) {
    install();
    let writer = CapturingWriter::default();
    SINK.with(|sink| *sink.borrow_mut() = Some(writer.clone()));
    (writer, CaptureGuard)
}
