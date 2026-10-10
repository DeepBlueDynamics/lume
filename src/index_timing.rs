//! Opt-in index diagnostics; no index bytes or query behavior change.
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub fn enabled() -> bool {
    std::env::var("LUME_TIMING").is_ok_and(|value| value == "1")
}

pub fn emit(phase: &str, file: Option<&Path>, elapsed: Duration) {
    let row = serde_json::json!({
        "phase": phase, "file": file.map(|path| path.to_string_lossy()),
        "ms": elapsed.as_secs_f64() * 1000.0,
    });
    // Diagnostic failures must not turn successful indexing into an error.
    let _ = writeln!(io::stderr().lock(), "LUME_TIMING {row}");
}

pub struct Span {
    phase: &'static str,
    start: Option<Instant>,
}
impl Span {
    pub fn new(phase: &'static str) -> Self {
        Self { phase, start: enabled().then(Instant::now) }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            emit(self.phase, None, start.elapsed());
        }
    }
}

/// Time actual underlying I/O, leaving the existing BufReader/BufWriter in place.
pub struct TimedIo<T> {
    pub inner: T,
    enabled: bool,
    pub elapsed: Duration,
}
impl<T> TimedIo<T> {
    pub fn new(inner: T, enabled: bool) -> Self {
        Self { inner, enabled, elapsed: Duration::ZERO }
    }
    fn measure<R>(&mut self, operation: impl FnOnce(&mut T) -> R) -> R {
        let start = self.enabled.then(Instant::now);
        let result = operation(&mut self.inner);
        if let Some(start) = start {
            self.elapsed += start.elapsed();
        }
        result
    }
}
impl<T: Read> Read for TimedIo<T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.measure(|inner| inner.read(buffer))
    }
}
impl<T: Write> Write for TimedIo<T> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.measure(|inner| inner.write(buffer))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.measure(Write::flush)
    }
}
impl TimedIo<std::fs::File> {
    pub fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timed_io_preserves_bytes_and_errors() {
        let mut writer = TimedIo::new(Vec::new(), true);
        writer.write_all(b"index bytes").unwrap();
        writer.flush().unwrap();
        assert_eq!(writer.inner, b"index bytes");
        let mut reader = TimedIo::new(io::Cursor::new(writer.inner), true);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"index bytes");
        let mut disabled = TimedIo::new(io::empty(), false);
        assert_eq!(disabled.read(&mut [0]).unwrap(), 0);
        assert_eq!(disabled.elapsed, Duration::ZERO);
    }
}
