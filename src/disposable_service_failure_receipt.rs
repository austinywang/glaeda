//! Pure, content-minimised diagnostic receipt for one disposable-worker service failure.
//!
//! This document is diagnostic evidence only. It contains no filesystem location, process identity,
//! command, environment, process output, credential, or lifecycle authority. Persistence, clearing,
//! and status integration are deliberately separate boundaries.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::artifact::Sha256Digest;

pub const DISPOSABLE_SERVICE_FAILURE_RECEIPT_SCHEMA_VERSION: u8 = 1;
const MAX_FAILURE_CODE_BYTES: usize = 96;
const MAX_RECEIPT_DOCUMENT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableServiceFailureKind {
    DurableState,
    Supervisor,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DisposableServiceFailureCode(String);

impl DisposableServiceFailureCode {
    /// Validate one compile-time machine-only service failure code for a new receipt.
    ///
    /// Requiring `&'static str` preserves the existing disposable-service error boundary: normal
    /// producers can record reviewed fixed codes, not runtime diagnostics or attacker-controlled
    /// text. Strict document decoding validates the same grammar separately for retained receipts.
    ///
    /// # Errors
    ///
    /// Returns a fixed public error unless the code begins with a lowercase ASCII letter and then
    /// contains only lowercase ASCII letters, digits, or underscores within the v1 size bound.
    pub fn from_static(value: &'static str) -> Result<Self, DisposableServiceFailureReceiptError> {
        Self::parse_document(value)
    }

    fn parse_document(value: &str) -> Result<Self, DisposableServiceFailureReceiptError> {
        let mut bytes = value.bytes();
        if value.len() > MAX_FAILURE_CODE_BYTES
            || !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(receipt_error(
                DisposableServiceFailureReceiptErrorKind::InvalidFailureCode,
                "disposable_service_failure_code_invalid",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DisposableServiceFailureCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DisposableServiceFailureCode")
            .field(&self.0)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableServiceFailureReceipt {
    schema_version: u8,
    program_digest: Sha256Digest,
    enrollment_digest: Sha256Digest,
    service_plan_identity: Sha256Digest,
    failure_kind: DisposableServiceFailureKind,
    failure_code: DisposableServiceFailureCode,
    process_started_at_epoch_ms: u64,
    failed_at_epoch_ms: u64,
    restart_generation: u64,
    durable_recovery_present: bool,
}

impl DisposableServiceFailureReceipt {
    /// Construct one validated v1 diagnostic receipt.
    ///
    /// # Errors
    ///
    /// Returns a fixed public error when the failure timestamp predates the retained process-start
    /// timestamp. All identities and failure-code syntax must already be typed and validated.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        program_digest: Sha256Digest,
        enrollment_digest: Sha256Digest,
        service_plan_identity: Sha256Digest,
        failure_kind: DisposableServiceFailureKind,
        failure_code: DisposableServiceFailureCode,
        process_started_at_epoch_ms: u64,
        failed_at_epoch_ms: u64,
        restart_generation: u64,
        durable_recovery_present: bool,
    ) -> Result<Self, DisposableServiceFailureReceiptError> {
        if failed_at_epoch_ms < process_started_at_epoch_ms {
            return Err(receipt_error(
                DisposableServiceFailureReceiptErrorKind::InvalidTimeline,
                "disposable_service_failure_timeline_invalid",
            ));
        }
        Ok(Self {
            schema_version: DISPOSABLE_SERVICE_FAILURE_RECEIPT_SCHEMA_VERSION,
            program_digest,
            enrollment_digest,
            service_plan_identity,
            failure_kind,
            failure_code,
            process_started_at_epoch_ms,
            failed_at_epoch_ms,
            restart_generation,
            durable_recovery_present,
        })
    }

    /// Decode one strict bounded v1 JSON receipt.
    ///
    /// # Errors
    ///
    /// Rejects oversized, malformed, unknown-field, duplicate-field, unsupported-version,
    /// non-canonical identity, invalid-code, and invalid-timeline documents with fixed public errors.
    pub fn from_json(bytes: &[u8]) -> Result<Self, DisposableServiceFailureReceiptError> {
        if bytes.len() > MAX_RECEIPT_DOCUMENT_BYTES {
            return Err(receipt_error(
                DisposableServiceFailureReceiptErrorKind::DocumentTooLarge,
                "disposable_service_failure_document_too_large",
            ));
        }
        let raw: RawDisposableServiceFailureReceipt =
            serde_json::from_slice(bytes).map_err(|_| {
                receipt_error(
                    DisposableServiceFailureReceiptErrorKind::InvalidDocument,
                    "disposable_service_failure_document_invalid",
                )
            })?;
        if raw.schema_version != DISPOSABLE_SERVICE_FAILURE_RECEIPT_SCHEMA_VERSION {
            return Err(receipt_error(
                DisposableServiceFailureReceiptErrorKind::UnsupportedSchema,
                "disposable_service_failure_schema_unsupported",
            ));
        }
        let program_digest = parse_digest(&raw.program_digest)?;
        let enrollment_digest = parse_digest(&raw.enrollment_digest)?;
        let service_plan_identity = parse_digest(&raw.service_plan_identity)?;
        let failure_code = DisposableServiceFailureCode::parse_document(&raw.failure_code)?;
        Self::new(
            program_digest,
            enrollment_digest,
            service_plan_identity,
            raw.failure_kind,
            failure_code,
            raw.process_started_at_epoch_ms,
            raw.failed_at_epoch_ms,
            raw.restart_generation,
            raw.durable_recovery_present,
        )
    }

    /// Render deterministic compact JSON for the v1 fixed-field document.
    #[must_use]
    pub fn canonical_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("fixed service failure receipt must serialize")
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn program_digest(&self) -> &Sha256Digest {
        &self.program_digest
    }

    #[must_use]
    pub fn enrollment_digest(&self) -> &Sha256Digest {
        &self.enrollment_digest
    }

    #[must_use]
    pub fn service_plan_identity(&self) -> &Sha256Digest {
        &self.service_plan_identity
    }

    #[must_use]
    pub const fn failure_kind(&self) -> DisposableServiceFailureKind {
        self.failure_kind
    }

    #[must_use]
    pub fn failure_code(&self) -> &DisposableServiceFailureCode {
        &self.failure_code
    }

    #[must_use]
    pub const fn process_started_at_epoch_ms(&self) -> u64 {
        self.process_started_at_epoch_ms
    }

    #[must_use]
    pub const fn failed_at_epoch_ms(&self) -> u64 {
        self.failed_at_epoch_ms
    }

    #[must_use]
    pub const fn restart_generation(&self) -> u64 {
        self.restart_generation
    }

    #[must_use]
    pub const fn durable_recovery_present(&self) -> bool {
        self.durable_recovery_present
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDisposableServiceFailureReceipt {
    schema_version: u8,
    program_digest: String,
    enrollment_digest: String,
    service_plan_identity: String,
    failure_kind: DisposableServiceFailureKind,
    failure_code: String,
    process_started_at_epoch_ms: u64,
    failed_at_epoch_ms: u64,
    restart_generation: u64,
    durable_recovery_present: bool,
}

fn parse_digest(value: &str) -> Result<Sha256Digest, DisposableServiceFailureReceiptError> {
    Sha256Digest::parse(value).map_err(|_| {
        receipt_error(
            DisposableServiceFailureReceiptErrorKind::InvalidIdentity,
            "disposable_service_failure_identity_invalid",
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableServiceFailureReceiptErrorKind {
    DocumentTooLarge,
    InvalidDocument,
    UnsupportedSchema,
    InvalidIdentity,
    InvalidFailureCode,
    InvalidTimeline,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableServiceFailureReceiptError {
    kind: DisposableServiceFailureReceiptErrorKind,
    code: &'static str,
}

impl DisposableServiceFailureReceiptError {
    #[must_use]
    pub const fn kind(self) -> DisposableServiceFailureReceiptErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableServiceFailureReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableServiceFailureReceiptError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableServiceFailureReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the disposable service failure receipt is invalid")
    }
}

impl std::error::Error for DisposableServiceFailureReceiptError {}

const fn receipt_error(
    kind: DisposableServiceFailureReceiptErrorKind,
    code: &'static str,
) -> DisposableServiceFailureReceiptError {
    DisposableServiceFailureReceiptError { kind, code }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(hex: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn receipt() -> DisposableServiceFailureReceipt {
        DisposableServiceFailureReceipt::new(
            digest('1'),
            digest('2'),
            digest('3'),
            DisposableServiceFailureKind::Supervisor,
            DisposableServiceFailureCode::from_static("disposable_worker_bridge_unavailable")
                .unwrap(),
            1_725_000_000_000,
            1_725_000_001_234,
            7,
            true,
        )
        .unwrap()
    }

    #[test]
    fn exact_receipt_round_trips_with_deterministic_json() {
        let receipt = receipt();
        let json = receipt.canonical_json();
        let decoded = DisposableServiceFailureReceipt::from_json(&json).unwrap();
        assert_eq!(decoded, receipt);
        assert_eq!(decoded.canonical_json(), json);
        assert!(json.len() < MAX_RECEIPT_DOCUMENT_BYTES);
    }

    #[test]
    fn unknown_and_duplicate_fields_fail_closed() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&receipt().canonical_json()).unwrap();
        value.as_object_mut().unwrap().insert(
            "private_path".to_owned(),
            serde_json::Value::String("/Users/operator".to_owned()),
        );
        let error =
            DisposableServiceFailureReceipt::from_json(&serde_json::to_vec(&value).unwrap())
                .unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableServiceFailureReceiptErrorKind::InvalidDocument
        );

        let valid = String::from_utf8(receipt().canonical_json()).unwrap();
        let duplicate = valid.replacen(
            "{\"schema_version\":1,",
            "{\"schema_version\":1,\"schema_version\":1,",
            1,
        );
        let error = DisposableServiceFailureReceipt::from_json(duplicate.as_bytes()).unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableServiceFailureReceiptErrorKind::InvalidDocument
        );
    }

    #[test]
    fn unsupported_schema_is_distinct() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&receipt().canonical_json()).unwrap();
        value["schema_version"] = serde_json::Value::from(2);
        let error =
            DisposableServiceFailureReceipt::from_json(&serde_json::to_vec(&value).unwrap())
                .unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableServiceFailureReceiptErrorKind::UnsupportedSchema
        );
    }

    #[test]
    fn invalid_digest_is_rejected_without_echoing_input() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&receipt().canonical_json()).unwrap();
        value["program_digest"] = serde_json::Value::String("/Users/operator/private".to_owned());
        let error =
            DisposableServiceFailureReceipt::from_json(&serde_json::to_vec(&value).unwrap())
                .unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableServiceFailureReceiptErrorKind::InvalidIdentity
        );
        let debug = format!("{error:?}");
        assert!(!debug.contains("/Users/operator/private"));
    }

    #[test]
    fn failure_code_is_bounded_machine_vocabulary() {
        let empty_error = DisposableServiceFailureCode::parse_document("").unwrap_err();
        assert_eq!(
            empty_error.kind(),
            DisposableServiceFailureReceiptErrorKind::InvalidFailureCode
        );

        for invalid in [
            "1starts_with_digit",
            "UPPERCASE",
            "contains-dash",
            "contains/slash",
            "/Users/operator/private",
        ] {
            let error = DisposableServiceFailureCode::parse_document(invalid).unwrap_err();
            assert_eq!(
                error.kind(),
                DisposableServiceFailureReceiptErrorKind::InvalidFailureCode
            );
            assert!(!format!("{error:?}").contains(invalid));
        }
        let oversized = format!("a{}", "b".repeat(MAX_FAILURE_CODE_BYTES));
        assert_eq!(
            DisposableServiceFailureCode::parse_document(&oversized)
                .unwrap_err()
                .kind(),
            DisposableServiceFailureReceiptErrorKind::InvalidFailureCode
        );
    }

    #[test]
    fn failure_cannot_predate_process_start() {
        let error = DisposableServiceFailureReceipt::new(
            digest('1'),
            digest('2'),
            digest('3'),
            DisposableServiceFailureKind::DurableState,
            DisposableServiceFailureCode::from_static("disposable_worker_recovery_required")
                .unwrap(),
            10,
            9,
            0,
            false,
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableServiceFailureReceiptErrorKind::InvalidTimeline
        );
    }

    #[test]
    fn oversized_document_is_rejected_before_decode() {
        let bytes = vec![b' '; MAX_RECEIPT_DOCUMENT_BYTES + 1];
        let error = DisposableServiceFailureReceipt::from_json(&bytes).unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableServiceFailureReceiptErrorKind::DocumentTooLarge
        );
    }

    #[test]
    fn public_document_has_no_slot_for_private_process_material() {
        let json = String::from_utf8(receipt().canonical_json()).unwrap();
        for forbidden in [
            "path",
            "argv",
            "environment",
            "stdout",
            "stderr",
            "pid",
            "credential",
            "token",
            "jit",
            "message",
        ] {
            assert!(
                !json.contains(forbidden),
                "forbidden public field fragment: {forbidden}"
            );
        }
    }
}
