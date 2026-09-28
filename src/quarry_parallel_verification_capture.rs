//! Bounded exact-byte capture for one already-open Quarry verification receipt channel.
//!
//! This module owns neither end of the channel. It does not open pipes, execute work, impose a
//! deadline, settle a process, or prove that EOF is authoritative. The caller must arrange those
//! boundaries and may treat the result as terminal evidence only after the independent execution
//! and cleanup contracts are satisfied.

use std::fmt;
use std::io::{self, Read};

use crate::quarry_parallel_verification_adapter::QuarryParallelVerificationCapture;
use crate::quarry_parallel_verification_receipt::MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES;

const READ_BUFFER_BYTES: usize = 8 * 1024;

/// One complete, bounded observation from an already-open receipt channel.
///
/// Retained bytes are deliberately private and are exposed only by borrowing them into the
/// existing pure Quarry adapter input. `Debug` reports the class and bounded size, never content.
pub struct CapturedQuarryParallelVerificationReceipt {
    observation: CaptureObservation,
}

enum CaptureObservation {
    Missing,
    Bytes(Vec<u8>),
    Overflow { bytes_observed_at_least: u64 },
}

impl CapturedQuarryParallelVerificationReceipt {
    /// Borrow this completed observation as the existing pure adapter input.
    ///
    /// This conversion grants no execution, cleanup, persistence, or publication authority.
    #[must_use]
    pub fn as_adapter_capture(&self) -> QuarryParallelVerificationCapture<'_> {
        match &self.observation {
            CaptureObservation::Missing => QuarryParallelVerificationCapture::Missing,
            CaptureObservation::Bytes(bytes) => {
                QuarryParallelVerificationCapture::Bytes(bytes.as_slice())
            }
            CaptureObservation::Overflow {
                bytes_observed_at_least,
            } => QuarryParallelVerificationCapture::Overflow {
                bytes_observed_at_least: *bytes_observed_at_least,
            },
        }
    }
}

impl fmt::Debug for CapturedQuarryParallelVerificationReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("CapturedQuarryParallelVerificationReceipt");
        match &self.observation {
            CaptureObservation::Missing => debug.field("class", &"missing"),
            CaptureObservation::Bytes(bytes) => debug
                .field("class", &"bytes")
                .field("retained_bytes", &bytes.len()),
            CaptureObservation::Overflow {
                bytes_observed_at_least,
            } => debug
                .field("class", &"overflow")
                .field("bytes_observed_at_least", bytes_observed_at_least),
        }
        .finish()
    }
}

/// Capture exact bytes from one already-open Quarry receipt channel through EOF.
///
/// At most [`MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES`] are retained. Once the stream
/// exceeds that limit, retained content is discarded and the reader is still drained through EOF
/// so classification alone does not leave a bounded writer blocked. The overflow observation is
/// intentionally threshold-only rather than an exact output-size disclosure.
///
/// This call can wait for EOF. Its caller must independently provide deadline, cancellation,
/// reader-thread settlement, and whole-execution cleanup behavior.
///
/// # Errors
///
/// Returns a fixed-class error when the channel cannot be read through EOF. The underlying I/O
/// error and any captured content are not retained or exposed.
pub fn capture_quarry_parallel_verification_receipt<R: Read + ?Sized>(
    reader: &mut R,
) -> Result<CapturedQuarryParallelVerificationReceipt, QuarryReceiptCaptureError> {
    let mut retained = Vec::with_capacity(MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES);
    let mut overflow = false;
    let mut buffer = [0_u8; READ_BUFFER_BYTES];

    loop {
        let read = match reader.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(QuarryReceiptCaptureError::read_failed()),
        };
        if read == 0 {
            break;
        }

        if !overflow {
            let remaining = MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES - retained.len();
            if read <= remaining {
                retained.extend_from_slice(&buffer[..read]);
            } else {
                retained.clear();
                retained.shrink_to_fit();
                overflow = true;
            }
        }
    }

    let observation = if overflow {
        CaptureObservation::Overflow {
            bytes_observed_at_least: u64::try_from(
                MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES.saturating_add(1),
            )
            .unwrap_or(u64::MAX),
        }
    } else if retained.is_empty() {
        CaptureObservation::Missing
    } else {
        CaptureObservation::Bytes(retained)
    };
    Ok(CapturedQuarryParallelVerificationReceipt { observation })
}

/// Stable failure class for bounded receipt capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarryReceiptCaptureErrorKind {
    ReadFailed,
}

/// Content-free failure from reading a receipt channel through EOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuarryReceiptCaptureError {
    kind: QuarryReceiptCaptureErrorKind,
}

impl QuarryReceiptCaptureError {
    const fn read_failed() -> Self {
        Self {
            kind: QuarryReceiptCaptureErrorKind::ReadFailed,
        }
    }

    #[must_use]
    pub const fn kind(self) -> QuarryReceiptCaptureErrorKind {
        self.kind
    }
}

impl fmt::Display for QuarryReceiptCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Quarry receipt channel could not be read through EOF")
    }
}

impl std::error::Error for QuarryReceiptCaptureError {}

#[cfg(test)]
mod tests {
    use std::cmp;
    use std::io::{self, Cursor, Read};

    use super::*;

    #[test]
    fn empty_channel_is_missing() {
        let captured = capture_quarry_parallel_verification_receipt(&mut io::empty())
            .expect("empty channel is a complete observation");

        assert!(matches!(
            captured.as_adapter_capture(),
            QuarryParallelVerificationCapture::Missing
        ));
    }

    #[test]
    fn exact_non_utf8_bytes_are_preserved() {
        let bytes = b"{\"receipt\":\xff}\n";
        let captured = capture_quarry_parallel_verification_receipt(&mut bytes.as_slice())
            .expect("raw bytes are captured");

        match captured.as_adapter_capture() {
            QuarryParallelVerificationCapture::Bytes(observed) => assert_eq!(observed, bytes),
            _ => panic!("expected retained bytes"),
        }
    }

    #[test]
    fn exact_budget_is_retained() {
        let bytes = vec![b'x'; MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES];
        let captured = capture_quarry_parallel_verification_receipt(&mut bytes.as_slice())
            .expect("boundary-sized channel is captured");

        match captured.as_adapter_capture() {
            QuarryParallelVerificationCapture::Bytes(observed) => assert_eq!(observed, bytes),
            _ => panic!("expected retained boundary bytes"),
        }
    }

    #[test]
    fn one_byte_over_budget_is_overflow() {
        let bytes = vec![b'x'; MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES + 1];
        let captured = capture_quarry_parallel_verification_receipt(&mut bytes.as_slice())
            .expect("overflowing channel is drained");

        assert_overflow(captured.as_adapter_capture());
    }

    #[test]
    fn multichunk_overflow_is_drained_through_eof() {
        let bytes = vec![b'x'; MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES * 3 + 17];
        let mut reader = ChunkedReader::new(&bytes, 997);
        let captured = capture_quarry_parallel_verification_receipt(&mut reader)
            .expect("overflowing channel is drained");

        assert_eq!(reader.position, bytes.len());
        assert!(reader.read_calls > bytes.len() / READ_BUFFER_BYTES);
        assert_overflow(captured.as_adapter_capture());
    }

    #[test]
    fn interrupted_read_is_retried_without_changing_bytes() {
        let bytes = b"exact receipt bytes\n";
        let mut reader = InterruptedOnce {
            interrupted: false,
            inner: Cursor::new(bytes),
        };
        let captured = capture_quarry_parallel_verification_receipt(&mut reader)
            .expect("interrupted read is retried");

        match captured.as_adapter_capture() {
            QuarryParallelVerificationCapture::Bytes(observed) => assert_eq!(observed, bytes),
            _ => panic!("expected retained bytes"),
        }
    }

    #[test]
    fn read_failure_exposes_only_a_fixed_class() {
        let private = "private-repository-output-and-/home/leo/path";
        let mut reader = FailingReader { private };
        let error = capture_quarry_parallel_verification_receipt(&mut reader)
            .expect_err("read failure must fail capture");

        assert_eq!(error.kind(), QuarryReceiptCaptureErrorKind::ReadFailed);
        assert!(!format!("{error:?}").contains(private));
        assert!(!error.to_string().contains(private));
    }

    #[test]
    fn failure_after_overflow_does_not_claim_complete_overflow() {
        let mut reader = BytesThenFailure {
            remaining: MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES + 1,
        };
        let error = capture_quarry_parallel_verification_receipt(&mut reader)
            .expect_err("EOF is required even after overflow");

        assert_eq!(error.kind(), QuarryReceiptCaptureErrorKind::ReadFailed);
    }

    #[test]
    fn debug_never_contains_retained_content() {
        let private = b"private-repository-receipt-body";
        let captured = capture_quarry_parallel_verification_receipt(&mut private.as_slice())
            .expect("private bytes are captured");
        let debug = format!("{captured:?}");

        assert!(debug.contains("retained_bytes"));
        assert!(!debug.contains("private-repository-receipt-body"));
    }

    fn assert_overflow(capture: QuarryParallelVerificationCapture<'_>) {
        match capture {
            QuarryParallelVerificationCapture::Overflow {
                bytes_observed_at_least,
            } => assert_eq!(
                bytes_observed_at_least,
                u64::try_from(MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES + 1)
                    .expect("receipt budget fits u64")
            ),
            _ => panic!("expected overflow"),
        }
    }

    struct ChunkedReader<'a> {
        bytes: &'a [u8],
        chunk_bytes: usize,
        position: usize,
        read_calls: usize,
    }

    impl<'a> ChunkedReader<'a> {
        const fn new(bytes: &'a [u8], chunk_bytes: usize) -> Self {
            Self {
                bytes,
                chunk_bytes,
                position: 0,
                read_calls: 0,
            }
        }
    }

    impl Read for ChunkedReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.read_calls += 1;
            if self.position == self.bytes.len() {
                return Ok(0);
            }
            let read = cmp::min(
                cmp::min(buffer.len(), self.chunk_bytes),
                self.bytes.len() - self.position,
            );
            buffer[..read].copy_from_slice(&self.bytes[self.position..self.position + read]);
            self.position += read;
            Ok(read)
        }
    }

    struct InterruptedOnce<R> {
        interrupted: bool,
        inner: R,
    }

    impl<R: Read> Read for InterruptedOnce<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            self.inner.read(buffer)
        }
    }

    struct FailingReader<'a> {
        private: &'a str,
    }

    impl Read for FailingReader<'_> {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other(self.private.to_owned()))
        }
    }

    struct BytesThenFailure {
        remaining: usize,
    }

    impl Read for BytesThenFailure {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::other("private post-overflow failure"));
            }
            let read = cmp::min(self.remaining, buffer.len());
            buffer[..read].fill(b'x');
            self.remaining -= read;
            Ok(read)
        }
    }
}
