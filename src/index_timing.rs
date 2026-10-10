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
        "memory": resident_memory(),
    });
    // Diagnostic failures must not turn successful indexing into an error.
    let _ = writeln!(io::stderr().lock(), "LUME_TIMING {row}");
}

// Linux reports both live RSS and the process high-water mark. These opt-in
// diagnostics distinguish retained buffers from peaks inside an operation.
fn resident_memory() -> Option<(u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| parse_resident_memory(&status))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_resident_memory(status: &str) -> Option<(u64, u64)> {
    let bytes = |name: &str| {
        let value = status.lines().find_map(|line| line.strip_prefix(name))?;
        let mut fields = value.split_whitespace();
        let kib = fields.next()?.parse::<u64>().ok()?;
        if fields.next()? != "kB" || fields.next().is_some() {
            return None;
        }
        kib.checked_mul(1024)
    };
    Some((bytes("VmRSS:")?, bytes("VmHWM:")?))
}

pub fn memory_checkpoint(phase: &'static str) {
    if enabled() {
        emit(phase, None, Duration::ZERO);
    }
}

pub struct Span {
    phase: &'static str,
    start: Option<Instant>,
}
impl Span {
    pub fn new(phase: &'static str) -> Self {
        Self {
            phase,
            start: enabled().then(Instant::now),
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            emit(self.phase, None, start.elapsed());
        }
    }
}

/// A named segment operation; records file attribution without changing data.
pub struct FileSpan<'a> {
    phase: &'static str,
    file: &'a Path,
    start: Option<Instant>,
}
impl<'a> FileSpan<'a> {
    pub fn new(phase: &'static str, file: &'a Path) -> Self {
        Self {
            phase,
            file,
            start: enabled().then(Instant::now),
        }
    }
}
impl Drop for FileSpan<'_> {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            emit(self.phase, Some(self.file), start.elapsed());
        }
    }
}

/// Aggregate repeated operations without emitting one diagnostic per file.
pub struct Aggregate {
    phase: &'static str,
    enabled: bool,
    elapsed: Duration,
}
impl Aggregate {
    pub fn new(phase: &'static str) -> Self {
        Self {
            phase,
            enabled: enabled(),
            elapsed: Duration::ZERO,
        }
    }
    pub fn measure<R>(&mut self, operation: impl FnOnce() -> R) -> R {
        let start = self.enabled.then(Instant::now);
        let result = operation();
        if let Some(start) = start {
            self.elapsed += start.elapsed();
        }
        result
    }
}
impl Drop for Aggregate {
    fn drop(&mut self) {
        if self.enabled {
            emit(self.phase, None, self.elapsed);
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
        Self {
            inner,
            enabled,
            elapsed: Duration::ZERO,
        }
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
    fn resident_memory_requires_exact_units_and_checked_sizes() {
        assert_eq!(
            parse_resident_memory("Name: lume\nVmRSS: 123 kB\nVmHWM: 456 kB\n"),
            Some((123 * 1024, 456 * 1024))
        );
        for invalid in [
            "VmRSS: 123 kB",
            "VmRSS: 123 MB\nVmHWM: 456 kB",
            "VmRSS: 123 kB extra\nVmHWM: 456 kB",
            "VmRSS: 18446744073709551615 kB\nVmHWM: 456 kB",
        ] {
            assert_eq!(parse_resident_memory(invalid), None);
        }
    }

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
