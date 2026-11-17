//! Lossless output: budgets limit inline text, never the captured data.
use anyhow::{Context, Result};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

pub(crate) fn character_budget(tokens: usize) -> usize {
    tokens.min(100_000) * 4
}

struct Spill {
    file: File,
    path: PathBuf,
    bytes: u64,
}
impl Spill {
    fn new() -> Result<Self> {
        let path = PathBuf::from(format!("/tmp/pico-evo-output-{}", uuid::Uuid::new_v4()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("Could not create output file {}", path.display()))?;
        Ok(Self {
            file,
            path,
            bytes: 0,
        })
    }
    fn notice(&self) -> String {
        format!(
            "Output saved in full to a temporary file ({} bytes).\nPath: {}\nRead or search this file with shell commands to retrieve the content you need.",
            self.bytes,
            self.path.display()
        )
    }
}

#[derive(Default)]
pub(crate) struct OutputBuffer {
    pending: Vec<u8>,
    spill: Option<Spill>,
    // Keep unwritten bytes in memory on failure and retry when the caller reads.
    spill_failed: bool,
}
impl OutputBuffer {
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.spill.is_none()
    }

    pub(crate) fn append(&mut self, bytes: &[u8], memory_limit: usize) {
        self.pending.extend_from_slice(bytes);
        if !self.spill_failed
            && (self.spill.is_some() || self.pending.len() > memory_limit)
            && self.flush().is_err()
        {
            self.spill_failed = true;
        }
    }

    fn flush(&mut self) -> Result<()> {
        if self.spill.is_none() {
            self.spill = Some(Spill::new()?);
        }
        let spill = self.spill.as_mut().expect("created spill file");
        let mut written = 0;
        let result = (|| -> io::Result<()> {
            while written < self.pending.len() {
                match spill.file.write(&self.pending[written..]) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(n) => {
                        written += n;
                        spill.bytes += n as u64;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        // Even a partial write is accounted for exactly; retries never duplicate bytes.
        self.pending.drain(..written);
        result.with_context(|| {
            format!("Could not save complete output to {}", spill.path.display())
        })?;
        self.spill_failed = false;
        Ok(())
    }

    /// Consume only after successful preservation. A returned file is a closed,
    /// immutable snapshot of this read, not a live log. Files are left to the OS.
    pub(crate) fn read(&mut self, characters: usize) -> Result<String> {
        let result = self.prepare_read(characters)?;
        *self = Self::default();
        Ok(result)
    }
    /// Prepare without consuming: ordered multimodal batches commit all buffers together.
    pub(crate) fn prepare_read(&mut self, characters: usize) -> Result<String> {
        let text = std::str::from_utf8(&self.pending);
        let result = if self.spill.is_some()
            || self.spill_failed
            || text
                .as_ref()
                .map_or(true, |text| text.chars().count() > characters)
        {
            self.flush().context("Output is retained for retry. Check /tmp disk space and permissions, then poll again; do not rerun the command")?;
            self.spill.as_ref().expect("flushed spill file").notice()
        } else {
            text.expect("validated UTF-8").to_owned()
        };
        Ok(result)
    }
}

/// Already-materialized diagnostics/summaries cannot be polled again. If saving
/// fails, return all their text inline with an explicit warning, never a fragment.
pub(crate) fn preserve_text(text: &str, characters: usize) -> String {
    preserve_bytes(text.as_bytes(), characters)
}

pub(crate) fn preserve_bytes(bytes: &[u8], characters: usize) -> String {
    let mut output = OutputBuffer::default();
    output.append(bytes, usize::MAX);
    match output.read(characters) {
        Ok(result) => result,
        Err(error) => {
            let complete = match std::str::from_utf8(bytes) {
                Ok(text) => text.to_owned(),
                Err(_) => {
                    use std::fmt::Write;
                    let mut hex = String::from("Complete raw bytes in hexadecimal:\n");
                    for byte in bytes {
                        write!(&mut hex, "{byte:02x}").expect("write to String");
                    }
                    hex
                }
            };
            format!(
                "[Could not preserve oversized data in /tmp: {error:#}. Returning the complete data inline.]\n{complete}"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::saved_output;

    #[test]
    fn budgets_preserve_unicode_boundaries_and_zero_budget_output() {
        let text = "first λ\nimportant middle 界\nlast";
        let characters = text.chars().count();
        assert_eq!(preserve_text(text, characters), text);
        for budget in [0, characters - 1] {
            let notice = preserve_text(text, budget);
            assert_eq!(saved_output(&notice), text.as_bytes());
            assert!(!notice.contains("important middle"));
        }
        assert_eq!(preserve_text("", 0), "");
        assert_eq!(character_budget(usize::MAX), 400_000);
    }

    #[test]
    fn streamed_spills_are_complete_immutable_and_consume_only_new_bytes() {
        let mut buffer = OutputBuffer::default();
        let bytes: Vec<_> = (0..2 * 1024 * 1024 + 17777)
            .map(|i| ((i * 17 + i / 29) % 251) as u8)
            .collect();
        for chunk in bytes.chunks(8191) {
            buffer.append(chunk, 1024 * 1024);
            assert!(buffer.pending.len() <= 1024 * 1024);
        }
        let notice = buffer.read(usize::MAX).unwrap();
        assert_eq!(saved_output(&notice), bytes);
        buffer.append(b"new bytes", 1024);
        assert_eq!(buffer.read(100).unwrap(), "new bytes");
        assert_eq!(buffer.read(0).unwrap(), "");
        drop(buffer);
        assert_eq!(saved_output(&notice), bytes);

        // Even tiny non-UTF-8 output must not be decoded with replacement chars.
        let mut binary = OutputBuffer::default();
        binary.append(b"before\xffafter", 1024);
        assert_eq!(saved_output(&binary.read(100).unwrap()), b"before\xffafter");
    }

    #[test]
    fn failed_spill_keeps_unwritten_bytes_and_can_be_retried() {
        let mut buffer = OutputBuffer {
            spill: Some(Spill {
                file: OpenOptions::new().write(true).open("/dev/full").unwrap(),
                path: PathBuf::from("/dev/full"),
                bytes: 0,
            }),
            ..Default::default()
        };
        buffer.append(b"before", 0);
        buffer.append(b"\xffafter", 0);
        let error = buffer.read(100).unwrap_err();
        assert!(format!("{error:#}").contains("retained for retry"));
        buffer.spill = Some(Spill::new().unwrap());
        assert_eq!(saved_output(&buffer.read(100).unwrap()), b"before\xffafter");
        assert_eq!(buffer.read(100).unwrap(), "");
    }
}
