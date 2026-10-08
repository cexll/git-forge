//! Bounded capture of a CI plan's combined stdout/stderr: a reader thread
//! drains one pipe and keeps only the last [`TAIL_BYTES`] (+1) bytes, so a
//! high-volume plan neither blocks on a full pipe nor grows git-forge's heap.

use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Byte bound of the stored and reported CI output tail (spec #33: 4 KiB).
pub(crate) const TAIL_BYTES: usize = 4096;

/// The text tail of `output`: at most [`TAIL_BYTES`] bytes of valid UTF-8.
/// The raw cut takes the last `TAIL_BYTES` bytes; when that cut lands inside
/// a UTF-8 sequence, its leading continuation bytes are dropped. Bytes that
/// are not valid UTF-8 decode to U+FFFD, and because that replacement can
/// grow the text, the result is trimmed again at a character boundary.
pub(crate) fn bounded_tail(output: &[u8]) -> String {
    let start = output.len().saturating_sub(TAIL_BYTES);
    let mut cut = &output[start..];
    if start > 0 {
        let partial = cut
            .iter()
            .take(3)
            .take_while(|b| **b & 0xC0 == 0x80)
            .count();
        cut = &cut[partial..];
    }
    let text = String::from_utf8_lossy(cut);
    let mut from = text.len().saturating_sub(TAIL_BYTES);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    text[from..].to_string()
}

/// A running capture: the pipe reader thread and the bytes it kept.
pub(crate) struct Capture {
    kept: Arc<Mutex<Vec<u8>>>,
    reader: std::thread::JoinHandle<()>,
}

impl Capture {
    /// Open the pipe and start draining it. The returned writer is the plan's
    /// stdout and stderr; every copy of it must be closed for the reader to
    /// see end of file.
    pub(crate) fn start() -> std::io::Result<(Self, std::io::PipeWriter)> {
        let (mut pipe, writer) = std::io::pipe()?;
        let kept = Arc::new(Mutex::new(Vec::with_capacity(TAIL_BYTES + 1)));
        let sink = Arc::clone(&kept);
        let reader = std::thread::Builder::new()
            .name("ci-output".into())
            .spawn(move || {
                let mut chunk = [0u8; 8192];
                loop {
                    let n = match pipe.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    };
                    let mut kept = sink.lock().unwrap_or_else(|p| p.into_inner());
                    kept.extend_from_slice(&chunk[..n]);
                    // One byte beyond the bound records that output was cut,
                    // so `bounded_tail` knows a sequence may be split.
                    let excess = kept.len().saturating_sub(TAIL_BYTES + 1);
                    kept.drain(..excess);
                }
            })?;
        Ok((Self { kept, reader }, writer))
    }

    /// Bytes currently kept in memory (the F-016 heap bound).
    #[cfg(test)]
    pub(crate) fn kept_len(&self) -> usize {
        self.kept.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// The bounded tail of everything read. The wait for end of file is
    /// bounded: a descendant that left the process group can hold the pipe
    /// open, and the tail then holds what was read so far.
    pub(crate) fn finish(self) -> String {
        for _ in 0..50 {
            if self.reader.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let kept = self.kept.lock().unwrap_or_else(|p| p.into_inner());
        bounded_tail(&kept)
    }
}
