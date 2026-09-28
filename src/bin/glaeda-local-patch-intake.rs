//! Content-only admission for one sandbox-authored unified diff.
//!
//! A controller may fetch patch bytes through an authenticated provider transport, then pipe only
//! those bytes here together with identities already bound by Stensibly. This process performs no
//! network or filesystem mutation and grants no source, execution, cleanup, or publication
//! authority.

use std::fmt;
use std::io::{self, Read as _};

use clap::Parser;
use serde::Serialize;
use sha1::{Digest as _, Sha1};
use sha2::Sha256;

const SCHEMA_VERSION: u8 = 1;
const MAX_PATCH_BYTES: usize = 1024 * 1024;
const SHA1_HEX_BYTES: usize = 40;
const SHA256_PREFIX: &str = "sha256:";
const SHA256_HEX_BYTES: usize = 64;

#[derive(Debug, Parser)]
#[command(about = "Verify exact sandbox patch bytes without applying them")]
struct Args {
    /// Exact lowercase SHA-1 Git blob object ID expected from provider transport.
    #[arg(long)]
    git_blob_sha1: String,

    /// Exact canonical sha256:<hex> digest expected for the raw patch bytes.
    #[arg(long)]
    sha256: String,

    /// Exact raw patch byte count expected from transport metadata.
    #[arg(long)]
    bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchExpectation {
    git_blob_sha1: String,
    sha256: String,
    bytes: usize,
}

impl PatchExpectation {
    fn new(
        git_blob_sha1: impl Into<String>,
        sha256: impl Into<String>,
        bytes: usize,
    ) -> Result<Self, PatchAdmissionError> {
        let git_blob_sha1 = git_blob_sha1.into();
        let sha256 = sha256.into();
        if !is_lower_hex(&git_blob_sha1, SHA1_HEX_BYTES) {
            return Err(invalid_expectation());
        }
        let Some(sha256_hex) = sha256.strip_prefix(SHA256_PREFIX) else {
            return Err(invalid_expectation());
        };
        if !is_lower_hex(sha256_hex, SHA256_HEX_BYTES) || bytes == 0 || bytes > MAX_PATCH_BYTES {
            return Err(invalid_expectation());
        }
        Ok(Self {
            git_blob_sha1,
            sha256,
            bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PatchAdmissionReport {
    schema_version: u8,
    document_type: &'static str,
    authority: &'static str,
    transport_identity: &'static str,
    format: &'static str,
    git_blob_sha1: String,
    sha256: String,
    bytes: usize,
    line_count: usize,
    contains_patch_content: bool,
    authorizes_source_mutation: bool,
    authorizes_execution: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PatchAdmissionErrorKind {
    InvalidExpectation,
    InputTooLarge,
    ByteCountMismatch,
    Sha256Mismatch,
    GitBlobMismatch,
    InvalidUtf8,
    ContainsNul,
    NotUnifiedDiff,
    InputUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PatchAdmissionError {
    kind: PatchAdmissionErrorKind,
    code: &'static str,
    problem: &'static str,
}

impl fmt::Display for PatchAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for PatchAdmissionError {}

#[derive(Debug, Serialize)]
struct RefusalReceipt<'a> {
    schema_version: u8,
    document_type: &'static str,
    admitted: bool,
    code: &'a str,
    problem: &'a str,
    contains_patch_content: bool,
    authorizes_source_mutation: bool,
    authorizes_execution: bool,
}

fn main() {
    let args = Args::parse();
    match run(args) {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string(&report).expect("patch admission report is serializable")
            );
        }
        Err(error) => {
            let receipt = RefusalReceipt {
                schema_version: SCHEMA_VERSION,
                document_type: "glaeda-local-patch-admission-refusal",
                admitted: false,
                code: error.code,
                problem: error.problem,
                contains_patch_content: false,
                authorizes_source_mutation: false,
                authorizes_execution: false,
            };
            eprintln!(
                "{}",
                serde_json::to_string(&receipt).expect("patch refusal receipt is serializable")
            );
            std::process::exit(2);
        }
    }
}

fn run(args: Args) -> Result<PatchAdmissionReport, PatchAdmissionError> {
    let expectation = PatchExpectation::new(args.git_blob_sha1, args.sha256, args.bytes)?;
    let mut input = Vec::with_capacity(expectation.bytes.min(MAX_PATCH_BYTES));
    io::stdin()
        .take((MAX_PATCH_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|_| input_unavailable())?;
    admit_patch(&expectation, &input)
}

fn admit_patch(
    expectation: &PatchExpectation,
    patch: &[u8],
) -> Result<PatchAdmissionReport, PatchAdmissionError> {
    if patch.len() > MAX_PATCH_BYTES {
        return Err(input_too_large());
    }
    if patch.len() != expectation.bytes {
        return Err(byte_count_mismatch());
    }
    let actual_sha256 = sha256(patch);
    if actual_sha256 != expectation.sha256 {
        return Err(sha256_mismatch());
    }
    let actual_blob_sha1 = git_blob_sha1(patch);
    if actual_blob_sha1 != expectation.git_blob_sha1 {
        return Err(git_blob_mismatch());
    }
    let text = std::str::from_utf8(patch).map_err(|_| invalid_utf8())?;
    if text.as_bytes().contains(&0) {
        return Err(contains_nul());
    }
    validate_unified_diff(text)?;

    Ok(PatchAdmissionReport {
        schema_version: SCHEMA_VERSION,
        document_type: "glaeda-local-patch-admission",
        authority: "content_identity_only",
        transport_identity: "git_blob_sha1_plus_sha256",
        format: "unified_diff_utf8",
        git_blob_sha1: actual_blob_sha1,
        sha256: actual_sha256,
        bytes: patch.len(),
        line_count: text.lines().count(),
        contains_patch_content: false,
        authorizes_source_mutation: false,
        authorizes_execution: false,
    })
}

fn validate_unified_diff(text: &str) -> Result<(), PatchAdmissionError> {
    if text.is_empty() {
        return Err(not_unified_diff());
    }
    let mut old_header = false;
    let mut new_header = false;
    let mut hunk = false;
    for line in text.lines() {
        if line.starts_with("--- ") {
            old_header = true;
        } else if line.starts_with("+++ ") && old_header {
            new_header = true;
        } else if line.starts_with("@@ ") && old_header && new_header {
            hunk = true;
        }
    }
    if !old_header || !new_header || !hunk {
        return Err(not_unified_diff());
    }
    Ok(())
}

fn git_blob_sha1(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    lower_hex(&hasher.finalize())
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{SHA256_PREFIX}{}", lower_hex(&hasher.finalize()))
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

const fn error(
    kind: PatchAdmissionErrorKind,
    code: &'static str,
    problem: &'static str,
) -> PatchAdmissionError {
    PatchAdmissionError {
        kind,
        code,
        problem,
    }
}

const fn invalid_expectation() -> PatchAdmissionError {
    error(
        PatchAdmissionErrorKind::InvalidExpectation,
        "patch_expectation_invalid",
        "patch expectation is outside the reviewed content boundary",
    )
}

const fn input_too_large() -> PatchAdmissionError {
    error(
        PatchAdmissionErrorKind::InputTooLarge,
        "patch_input_too_large",
        "patch input exceeds the one-mebibyte limit",
    )
}

const fn byte_count_mismatch() -> PatchAdmissionError {
    error(
        PatchAdmissionErrorKind::ByteCountMismatch,
        "patch_byte_count_mismatch",
        "patch byte count does not match the expected content identity",
    )
}

const fn sha256_mismatch() -> PatchAdmissionError {
    error(
        PatchAdmissionErrorKind::Sha256Mismatch,
        "patch_sha256_mismatch",
        "patch SHA-256 does not match the expected content identity",
    )
}

const fn git_blob_mismatch() -> PatchAdmissionError {
    error(
        PatchAdmissionErrorKind::GitBlobMismatch,
        "patch_git_blob_mismatch",
        "patch Git blob identity does not match the expected provider object",
    )
}

const fn invalid_utf8() -> PatchAdmissionError {
    error(
        PatchAdmissionErrorKind::InvalidUtf8,
        "patch_utf8_invalid",
        "patch input is not valid UTF-8",
    )
}

const fn contains_nul() -> PatchAdmissionError {
    error(
        PatchAdmissionErrorKind::ContainsNul,
        "patch_nul_forbidden",
        "patch input contains a NUL byte",
    )
}

const fn not_unified_diff() -> PatchAdmissionError {
    error(
        PatchAdmissionErrorKind::NotUnifiedDiff,
        "patch_unified_diff_invalid",
        "patch input does not contain an ordinary unified-diff hunk",
    )
}

const fn input_unavailable() -> PatchAdmissionError {
    error(
        PatchAdmissionErrorKind::InputUnavailable,
        "patch_input_unavailable",
        "patch input could not be read",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &[u8] = b"diff --git a/example.txt b/example.txt\n--- a/example.txt\n+++ b/example.txt\n@@ -1 +1 @@\n-old\n+new\n";

    fn expectation(patch: &[u8]) -> PatchExpectation {
        PatchExpectation::new(git_blob_sha1(patch), sha256(patch), patch.len())
            .expect("expectation")
    }

    fn assert_kind(
        result: Result<PatchAdmissionReport, PatchAdmissionError>,
        expected: PatchAdmissionErrorKind,
    ) {
        assert_eq!(result.expect_err("refusal").kind, expected);
    }

    #[test]
    fn admits_exact_patch_without_retaining_content_or_authority() {
        let report = admit_patch(&expectation(PATCH), PATCH).expect("admitted");
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.format, "unified_diff_utf8");
        assert_eq!(report.bytes, PATCH.len());
        assert_eq!(report.sha256, sha256(PATCH));
        assert_eq!(report.git_blob_sha1, git_blob_sha1(PATCH));
        assert!(!report.contains_patch_content);
        assert!(!report.authorizes_source_mutation);
        assert!(!report.authorizes_execution);
        let encoded = serde_json::to_string(&report).expect("json");
        assert!(!encoded.contains("example.txt"));
        assert!(!encoded.contains("-old"));
        assert!(!encoded.contains("+new"));
    }

    #[test]
    fn git_blob_identity_matches_git_object_framing() {
        assert_eq!(
            git_blob_sha1(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn refuses_changed_byte_count_and_content_digests() {
        let expected = expectation(PATCH);
        let shorter = &PATCH[..PATCH.len() - 1];
        assert_kind(
            admit_patch(&expected, shorter),
            PatchAdmissionErrorKind::ByteCountMismatch,
        );

        let wrong_sha = PatchExpectation::new(
            expected.git_blob_sha1.clone(),
            format!("sha256:{}", "0".repeat(64)),
            PATCH.len(),
        )
        .expect("expectation");
        assert_kind(
            admit_patch(&wrong_sha, PATCH),
            PatchAdmissionErrorKind::Sha256Mismatch,
        );

        let wrong_blob =
            PatchExpectation::new("0".repeat(40), expected.sha256.clone(), PATCH.len())
                .expect("expectation");
        assert_kind(
            admit_patch(&wrong_blob, PATCH),
            PatchAdmissionErrorKind::GitBlobMismatch,
        );
    }

    #[test]
    fn refuses_oversized_invalid_utf8_nul_and_non_diff_input() {
        let oversized = vec![b'a'; MAX_PATCH_BYTES + 1];
        assert_kind(
            admit_patch(&expectation(PATCH), &oversized),
            PatchAdmissionErrorKind::InputTooLarge,
        );

        let invalid_utf8 = [0xff_u8];
        assert_kind(
            admit_patch(&expectation(&invalid_utf8), &invalid_utf8),
            PatchAdmissionErrorKind::InvalidUtf8,
        );

        let nul_patch = b"--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\0\n+b\n";
        assert_kind(
            admit_patch(&expectation(nul_patch), nul_patch),
            PatchAdmissionErrorKind::ContainsNul,
        );

        let text = b"not a diff\n";
        assert_kind(
            admit_patch(&expectation(text), text),
            PatchAdmissionErrorKind::NotUnifiedDiff,
        );
    }

    #[test]
    fn expectation_refuses_noncanonical_or_unbounded_identities() {
        assert!(PatchExpectation::new("A".repeat(40), sha256(PATCH), PATCH.len()).is_err());
        assert!(PatchExpectation::new(git_blob_sha1(PATCH), "abcd", PATCH.len()).is_err());
        assert!(PatchExpectation::new(git_blob_sha1(PATCH), sha256(PATCH), 0).is_err());
        assert!(
            PatchExpectation::new(git_blob_sha1(PATCH), sha256(PATCH), MAX_PATCH_BYTES + 1)
                .is_err()
        );
    }
}
