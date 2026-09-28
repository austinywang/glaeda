//! CMUX app-host product transport projection for the adaptive verification compiler.
//!
//! This module consumes one already-validated CMUX workload observation batch plus the closed JSON
//! payload emitted by CMUX's `CMUX_TEST_PRODUCT_RESTORE` receipt. It turns measured peer transfer
//! and canonical restore work into bounded #547 observations without treating compressed transport
//! bytes as equivalent to unpacked consumer bytes.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::adaptive_verification_compiler::{
    AdaptiveVerificationCompilerAuthority, AdaptiveVerificationCompilerError,
    SemanticValidationResult, VerificationObservation, VerificationReuseClass, VerificationStage,
};
use crate::cmux_workload_verification_adapter::CmuxWorkloadVerificationObservationBatch;

pub const CMUX_PRODUCT_TRANSPORT_ADAPTER_SCHEMA_VERSION: u8 = 1;
pub const MAX_CMUX_PRODUCT_RESTORE_RECEIPT_BYTES: usize = 8 * 1024;

const MAX_TEXT_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CmuxProductTransportDocumentType {
    CmuxProductTransportObservationBatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CmuxProductLookupSource {
    Local,
    Peer,
    R2,
    Github,
}

impl CmuxProductLookupSource {
    fn parse(value: &str) -> Result<Self, CmuxProductTransportAdapterError> {
        match value {
            "local" => Ok(Self::Local),
            "peer" => Ok(Self::Peer),
            "r2" => Ok(Self::R2),
            "github" => Ok(Self::Github),
            _ => Err(error(
                "cmux_product_lookup_source_invalid",
                "CMUX product lookup source is invalid",
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Peer => "peer",
            Self::R2 => "r2",
            Self::Github => "github",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CmuxProductTransportObservationBatch {
    document_type: CmuxProductTransportDocumentType,
    schema_version: u8,
    authority: AdaptiveVerificationCompilerAuthority,
    attempt_id: String,
    profile: String,
    semantic_result_sha256: String,
    restore_receipt_sha256: String,
    archive_identity: String,
    lookup_source: CmuxProductLookupSource,
    archive_bytes: u64,
    consumer_unpacked_bytes: u64,
    local_lookup_millis: u64,
    peer_lookup_millis: u64,
    peer_transfer_millis: u64,
    peer_bytes_transferred: u64,
    restore_millis: u64,
    split_comparable_bytes_available: bool,
    observations: Vec<VerificationObservation>,
}

impl CmuxProductTransportObservationBatch {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn lookup_source(&self) -> CmuxProductLookupSource {
        self.lookup_source
    }

    #[must_use]
    pub const fn split_comparable_bytes_available(&self) -> bool {
        self.split_comparable_bytes_available
    }

    #[must_use]
    pub fn observations(&self) -> &[VerificationObservation] {
        &self.observations
    }

    /// Render the typed projection as deterministic pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns only if serialization of the fixed model fails.
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Render the same projection as a compact human explanation.
    #[must_use]
    pub fn render_human(&self) -> String {
        format!(
            "cmux product transport observation\nattempt: {}\nprofile: {}\nsource: {}\narchive identity: {}\narchive bytes: {}\nconsumer unpacked bytes: {}\nlocal lookup: {} ms\npeer lookup: {} ms\npeer transfer: {} ms\npeer bytes transferred: {}\nrestore: {} ms\nsplit-comparable bytes: {}\nobservations: {}\n",
            self.attempt_id,
            self.profile,
            self.lookup_source.as_str(),
            self.archive_identity,
            self.archive_bytes,
            self.consumer_unpacked_bytes,
            self.local_lookup_millis,
            self.peer_lookup_millis,
            self.peer_transfer_millis,
            self.peer_bytes_transferred,
            self.restore_millis,
            self.split_comparable_bytes_available,
            self.observations.len(),
        )
    }
}

/// Project one CMUX product restore receipt into deterministic #547 observations.
///
/// `semantic_batch` must come from `project_cmux_workload_result`, which already binds the exact
/// CMUX semantic result to its Glaeda correlation receipt. This function therefore uses only the
/// exact artifact/consumer identity retained by that batch and the measured transport receipt.
///
/// Current CMUX receipts report compressed archive/transfer bytes while the semantic workload
/// reports unpacked runtime-input bytes. Those quantities are deliberately kept separate and this
/// adapter does not populate `required_consumer_bytes`; doing so would let incomparable units
/// manufacture a `split_consumer_artifact` suggestion.
///
/// # Errors
///
/// Returns a bounded adapter error for malformed receipt bytes, inconsistent source flags,
/// unsupported workload projections, or observations that exceed the #547 bounds.
pub fn project_cmux_product_transport(
    attempt_id: &str,
    semantic_batch: &CmuxWorkloadVerificationObservationBatch,
    restore_receipt_bytes: &[u8],
) -> Result<CmuxProductTransportObservationBatch, CmuxProductTransportAdapterError> {
    validate_token(attempt_id)?;
    if restore_receipt_bytes.is_empty()
        || restore_receipt_bytes.len() > MAX_CMUX_PRODUCT_RESTORE_RECEIPT_BYTES
    {
        return Err(error(
            "cmux_product_restore_receipt_size",
            "CMUX product restore receipt exceeds its bounded range",
        ));
    }
    if !semantic_batch
        .profile()
        .starts_with("cmux.macos.app-host-test-shard@")
    {
        return Err(error(
            "cmux_product_transport_profile_unsupported",
            "CMUX product transport projection requires an app-host test-shard semantic batch",
        ));
    }

    let receipt: RawCmuxProductRestoreReceipt = serde_json::from_slice(restore_receipt_bytes)
        .map_err(|_| {
            error(
                "cmux_product_restore_receipt_invalid",
                "CMUX product restore receipt is invalid closed-schema JSON",
            )
        })?;
    validate_restore_receipt(&receipt)?;
    validate_semantic_binding(&receipt, semantic_batch)?;

    let context = semantic_transport_context(semantic_batch)?;
    let archive_identity = archive_identity(&receipt);
    let lookup_source = CmuxProductLookupSource::parse(&receipt.lookup_source)?;
    let local_lookup_millis = seconds_to_millis(receipt.lookup_seconds)?;
    let peer_lookup_millis = seconds_to_millis(receipt.peer_lookup_seconds)?;
    let peer_transfer_millis = seconds_to_millis(receipt.peer_transfer_seconds)?;
    let restore_millis = seconds_to_millis(receipt.elapsed_seconds)?;
    let restore_observation_millis = local_lookup_millis.saturating_add(restore_millis);
    let restore_reuse_class = match lookup_source {
        CmuxProductLookupSource::Local => VerificationReuseClass::Reuse,
        CmuxProductLookupSource::Peer
        | CmuxProductLookupSource::R2
        | CmuxProductLookupSource::Github => VerificationReuseClass::Cold,
    };

    let mut observations = Vec::new();
    if lookup_source == CmuxProductLookupSource::Peer {
        let transfer_millis = peer_lookup_millis.saturating_add(peer_transfer_millis);
        let transfer_id = observation_id(attempt_id, "peer-artifact-transfer");
        let transfer = VerificationObservation::new(
            &transfer_id,
            attempt_id,
            "cmux",
            semantic_batch.profile(),
            1,
            VerificationStage::ArtifactTransfer,
            transfer_millis,
            VerificationReuseClass::Cold,
            context.semantic_validation,
            &context.resource_profile,
        )
        .map_err(compiler_projection_error)?
        .with_bytes(None, None, Some(receipt.peer_bytes_transferred))
        .map_err(compiler_projection_error)?
        .with_artifact_transfer_backend("peer")
        .map_err(compiler_projection_error)?
        .with_validity_inputs(context.validity_inputs)
        .with_artifact(
            &archive_identity,
            context.consumer_identity.as_deref(),
            None,
        )
        .map_err(compiler_projection_error)?;
        observations.push(transfer);
    }

    let restore_id = observation_id(attempt_id, "canonical-product-restore");
    let restore = VerificationObservation::new(
        &restore_id,
        attempt_id,
        "cmux",
        semantic_batch.profile(),
        2,
        VerificationStage::Restore,
        restore_observation_millis,
        restore_reuse_class,
        context.semantic_validation,
        &context.resource_profile,
    )
    .map_err(compiler_projection_error)?
    .with_bytes(Some(receipt.archive_bytes), None, None)
    .map_err(compiler_projection_error)?
    .with_validity_inputs(context.validity_inputs)
    .with_artifact(
        &archive_identity,
        context.consumer_identity.as_deref(),
        None,
    )
    .map_err(compiler_projection_error)?;
    observations.push(restore);

    Ok(CmuxProductTransportObservationBatch {
        document_type: CmuxProductTransportDocumentType::CmuxProductTransportObservationBatch,
        schema_version: CMUX_PRODUCT_TRANSPORT_ADAPTER_SCHEMA_VERSION,
        authority: AdaptiveVerificationCompilerAuthority::ObservationOnly,
        attempt_id: attempt_id.to_owned(),
        profile: semantic_batch.profile().to_owned(),
        semantic_result_sha256: semantic_batch.cmux_semantic_result_sha256().to_owned(),
        restore_receipt_sha256: sha256(restore_receipt_bytes),
        archive_identity,
        lookup_source,
        archive_bytes: receipt.archive_bytes,
        consumer_unpacked_bytes: context.consumer_unpacked_bytes,
        local_lookup_millis,
        peer_lookup_millis,
        peer_transfer_millis,
        peer_bytes_transferred: receipt.peer_bytes_transferred,
        restore_millis,
        split_comparable_bytes_available: false,
        observations,
    })
}

struct SemanticTransportContext<'a> {
    semantic_validation: SemanticValidationResult,
    resource_profile: String,
    consumer_identity: Option<String>,
    consumer_unpacked_bytes: u64,
    validity_inputs: &'a [crate::adaptive_verification_compiler::ValidityInput],
}

fn semantic_transport_context(
    batch: &CmuxWorkloadVerificationObservationBatch,
) -> Result<SemanticTransportContext<'_>, CmuxProductTransportAdapterError> {
    let test = batch
        .observations()
        .iter()
        .find(|observation| observation.stage() == VerificationStage::TestExecution)
        .ok_or_else(|| {
            error(
                "cmux_product_transport_context_missing",
                "CMUX semantic batch lacks its app-host test observation",
            )
        })?;
    let value = serde_json::to_value(test).map_err(|_| {
        error(
            "cmux_product_transport_context_invalid",
            "CMUX semantic observation cannot be projected into transport context",
        )
    })?;

    let _runtime_artifact_identity = string_field(&value, "artifact_identity")?;
    let consumer_identity = optional_string_field(&value, "consumer_identity")?;
    let consumer_unpacked_bytes = value
        .get("required_consumer_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            error(
                "cmux_product_transport_context_invalid",
                "CMUX semantic observation lacks exact consumer byte evidence",
            )
        })?;
    let resource_profile = string_field(&value, "resource_profile")?;
    let semantic_validation = match string_field(&value, "semantic_validation")?.as_str() {
        "passed" => SemanticValidationResult::Passed,
        "failed" => SemanticValidationResult::Failed,
        "timed_out" => SemanticValidationResult::TimedOut,
        "unknown" => SemanticValidationResult::Unknown,
        _ => {
            return Err(error(
                "cmux_product_transport_context_invalid",
                "CMUX semantic observation has an invalid semantic result",
            ));
        }
    };

    Ok(SemanticTransportContext {
        semantic_validation,
        resource_profile,
        consumer_identity,
        consumer_unpacked_bytes,
        validity_inputs: test.validity_inputs(),
    })
}

fn string_field(value: &Value, name: &str) -> Result<String, CmuxProductTransportAdapterError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            error(
                "cmux_product_transport_context_invalid",
                "CMUX semantic observation lacks required transport identity",
            )
        })
}

fn optional_string_field(
    value: &Value,
    name: &str,
) -> Result<Option<String>, CmuxProductTransportAdapterError> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(error(
            "cmux_product_transport_context_invalid",
            "CMUX semantic observation has invalid optional transport identity",
        )),
    }
}

fn validate_semantic_binding(
    receipt: &RawCmuxProductRestoreReceipt,
    semantic_batch: &CmuxWorkloadVerificationObservationBatch,
) -> Result<(), CmuxProductTransportAdapterError> {
    if receipt.source_revision != semantic_batch.source_commit() {
        return Err(error(
            "cmux_product_restore_source_mismatch",
            "CMUX product restore source revision differs from the semantic workload",
        ));
    }
    let expected_shard = semantic_batch.semantic_parameter("shard").ok_or_else(|| {
        error(
            "cmux_product_restore_shard_missing",
            "CMUX app-host semantic workload lacks its shard parameter",
        )
    })?;
    let actual_shard = receipt
        .shard
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| {
            error(
                "cmux_product_restore_shard_invalid",
                "CMUX product restore receipt lacks a numeric shard identity",
            )
        })?;
    if actual_shard != expected_shard {
        return Err(error(
            "cmux_product_restore_shard_mismatch",
            "CMUX product restore shard differs from the semantic workload",
        ));
    }
    Ok(())
}

fn archive_identity(receipt: &RawCmuxProductRestoreReceipt) -> String {
    let material = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        receipt.repository,
        receipt.artifact_id,
        normalized_digest(&receipt.provider_digest),
        normalized_digest(&receipt.archive_sha256),
        normalized_digest(&receipt.product_contract),
        receipt.source_revision,
        receipt.producer_run_id,
        receipt.producer_run_attempt,
    );
    let mut hasher = Sha256::new();
    hasher.update(b"cmux-immutable-product-archive-v1\0");
    hasher.update(material.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn normalized_digest(value: &str) -> &str {
    value.strip_prefix("sha256:").unwrap_or(value)
}

fn validate_digest_token(value: &str) -> Result<(), CmuxProductTransportAdapterError> {
    let digest = normalized_digest(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(error(
            "cmux_product_restore_digest_invalid",
            "CMUX product restore identity contains an invalid SHA-256 digest",
        ));
    }
    Ok(())
}

fn validate_restore_receipt(
    receipt: &RawCmuxProductRestoreReceipt,
) -> Result<(), CmuxProductTransportAdapterError> {
    let source = CmuxProductLookupSource::parse(&receipt.lookup_source)?;
    if receipt.repository != "manaflow-ai/cmux"
        || receipt.artifact_id == 0
        || receipt.producer_run_id == 0
        || receipt.producer_run_attempt == 0
        || receipt.source_revision.len() != 40
        || !receipt
            .source_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(error(
            "cmux_product_restore_identity_invalid",
            "CMUX product restore immutable identity is invalid",
        ));
    }
    validate_digest_token(&receipt.provider_digest)?;
    validate_digest_token(&receipt.archive_sha256)?;
    validate_digest_token(&receipt.product_contract)?;
    if receipt.archive_bytes == 0 {
        return Err(error(
            "cmux_product_restore_bytes_invalid",
            "CMUX product restore archive bytes must be positive",
        ));
    }
    seconds_to_millis(receipt.elapsed_seconds)?;
    seconds_to_millis(receipt.lookup_seconds)?;
    seconds_to_millis(receipt.peer_lookup_seconds)?;
    seconds_to_millis(receipt.peer_transfer_seconds)?;

    if let Some(run_id) = receipt.run_id.as_deref() {
        validate_token(run_id)?;
    }
    if let Some(job) = receipt.job.as_deref() {
        validate_token(job)?;
    }
    if let Some(shard) = receipt.shard.as_deref() {
        validate_token(shard)?;
    }
    if let Some(runner_name) = receipt.runner_name.as_deref() {
        validate_text(runner_name)?;
    }

    match source {
        CmuxProductLookupSource::Local => {
            if !receipt.local_hit
                || receipt.peer_hit
                || receipt.peer_bytes_transferred != 0
                || receipt.peer_transfer_seconds != 0.0
            {
                return Err(error(
                    "cmux_product_restore_source_inconsistent",
                    "local CMUX product restore has inconsistent peer/local evidence",
                ));
            }
        }
        CmuxProductLookupSource::Peer => {
            if receipt.local_hit
                || !receipt.peer_hit
                || receipt.peer_bytes_transferred == 0
                || receipt.peer_bytes_transferred != receipt.archive_bytes
            {
                return Err(error(
                    "cmux_product_restore_source_inconsistent",
                    "peer CMUX product restore has inconsistent transfer evidence",
                ));
            }
        }
        CmuxProductLookupSource::R2 | CmuxProductLookupSource::Github => {
            if receipt.local_hit
                || receipt.peer_hit
                || receipt.peer_bytes_transferred != 0
                || receipt.peer_transfer_seconds != 0.0
            {
                return Err(error(
                    "cmux_product_restore_source_inconsistent",
                    "fallback CMUX product restore has inconsistent peer/local evidence",
                ));
            }
        }
    }
    Ok(())
}

fn seconds_to_millis(seconds: f64) -> Result<u64, CmuxProductTransportAdapterError> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(error(
            "cmux_product_restore_duration_invalid",
            "CMUX product restore duration must be finite and non-negative",
        ));
    }
    let millis = (seconds * 1_000.0).round();
    if millis > crate::adaptive_verification_compiler::MAX_VERIFICATION_DURATION_MILLIS as f64 {
        return Err(error(
            "cmux_product_restore_duration_out_of_range",
            "CMUX product restore duration exceeds the #547 observation range",
        ));
    }
    Ok(millis as u64)
}

fn observation_id(attempt_id: &str, stage: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"cmux-product-transport-observation-v1\0");
    hasher.update(attempt_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(stage.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("cmux-transport-{}", &digest[..16])
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn validate_token(value: &str) -> Result<(), CmuxProductTransportAdapterError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@' | b'+')
        })
    {
        return Err(error(
            "cmux_product_transport_token_invalid",
            "CMUX product transport token is invalid",
        ));
    }
    Ok(())
}

fn validate_text(value: &str) -> Result<(), CmuxProductTransportAdapterError> {
    if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.contains('\0') {
        return Err(error(
            "cmux_product_transport_text_invalid",
            "CMUX product transport text exceeds its bounded range",
        ));
    }
    Ok(())
}

fn compiler_projection_error(
    _error: AdaptiveVerificationCompilerError,
) -> CmuxProductTransportAdapterError {
    error(
        "cmux_product_transport_projection_invalid",
        "CMUX product transport evidence cannot be represented by the bounded #547 model",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CmuxProductTransportAdapterError {
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for CmuxProductTransportAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.problem)
    }
}

impl std::error::Error for CmuxProductTransportAdapterError {}

const fn error(code: &'static str, problem: &'static str) -> CmuxProductTransportAdapterError {
    CmuxProductTransportAdapterError { code, problem }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawCmuxProductRestoreReceipt {
    repository: String,
    artifact_id: u64,
    provider_digest: String,
    archive_sha256: String,
    product_contract: String,
    source_revision: String,
    producer_run_id: u64,
    producer_run_attempt: u64,
    archive_bytes: u64,
    elapsed_seconds: f64,
    lookup_source: String,
    local_hit: bool,
    lookup_seconds: f64,
    peer_hit: bool,
    peer_lookup_seconds: f64,
    peer_transfer_seconds: f64,
    peer_bytes_transferred: u64,
    run_id: Option<String>,
    job: Option<String>,
    shard: Option<String>,
    runner_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;
    use crate::adaptive_verification_compiler::{
        OptimizationClass, ValidityFingerprintStatus, compile_verification_optimizations,
    };
    use crate::cmux_workload_verification_adapter::project_cmux_workload_result;

    fn digest(seed: char) -> String {
        format!("sha256:{}", seed.to_string().repeat(64))
    }

    fn semantic_batch(attempt_id: &str) -> CmuxWorkloadVerificationObservationBatch {
        let result = json!({
            "document_type": "cmux-workload-result",
            "schema_version": 1,
            "source": {
                "repository": "manaflow-ai/cmux",
                "commit": "1".repeat(40),
                "tree": "2".repeat(40)
            },
            "profile": {"id": "cmux.macos.app-host-test-shard", "generation": 1},
            "semantic_validator": "cmux.app-host-test-shard/v1",
            "environment_class": "isolated-console-test",
            "expected_result_class": "cmux.app-host-test-shard-result/v1",
            "result": "passed",
            "parameters": {"shard": 3},
            "runtime_input_identities": [{
                "name": "app_host_xctestrun",
                "class": "cmux.app-host-product-tree/v1",
                "identity": "parent-tree-sha256",
                "sha256": digest('7'),
                "bytes": 1_157_000_000_u64
            }],
            "artifact_identities": [],
            "validation": {"missing_required_artifact_classes": []},
            "stage_timings": [
                {"stage": "setup", "seconds": 12.0},
                {"stage": "test", "seconds": 180.0}
            ],
            "resource_summary": {
                "resource_class": "cmux-macos-app-host-test",
                "cpu_count": 6,
                "memory_bytes": 34_359_738_368_u64,
                "architecture": "arm64"
            },
            "toolchain": {
                "identity": digest('3'),
                "observations": {"xcode": "Xcode 26.6", "macos_sdk": "26.6"}
            },
            "benchmark": {
                "state_class": "exact-product-reuse",
                "semantic_comparison_key": digest('8'),
                "comparison_context_key": digest('9')
            },
            "network_class": "repository-test",
            "timeout_class": "macos-long",
            "cleanup": {"state": "complete", "process_group_settled": true},
            "exit_code": 0,
            "started_at_unix_millis": 1_000,
            "ended_at_unix_millis": 193_000
        });
        let result_bytes = serde_json::to_vec(&result).unwrap();
        let result_digest = sha256(&result_bytes);
        let outer = json!({
            "document_type": "glaeda-cmux-workload-observation",
            "schema_version": 1,
            "external_request_ref": "cmux-workload-fixture",
            "request_sha256": digest('a'),
            "execution_binding_sha256": digest('b'),
            "source": result["source"].clone(),
            "profile": result["profile"].clone(),
            "state": "succeeded",
            "cmux_semantic_result_sha256": result_digest,
            "cmux_semantic_validator": "cmux.app-host-test-shard/v1",
            "authority": {
                "authorizes_execution": false,
                "authorizes_host_selection": false,
                "authorizes_resource_override": false,
                "authorizes_redispatch": false
            },
            "correlation": {"work_ref": "cmux-work-fixture"}
        });
        project_cmux_workload_result(
            attempt_id,
            &serde_json::to_vec(&outer).unwrap(),
            &result_bytes,
        )
        .unwrap()
    }

    fn peer_receipt() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "repository": "manaflow-ai/cmux",
            "artifact_id": 10_610_975_375_u64,
            "provider_digest": digest('c'),
            "archive_sha256": digest('d'),
            "product_contract": digest('e'),
            "source_revision": "1".repeat(40),
            "producer_run_id": 35_645_388_943_u64,
            "producer_run_attempt": 1_u64,
            "archive_bytes": 606_055_356_u64,
            "elapsed_seconds": 13.0,
            "lookup_source": "peer",
            "local_hit": false,
            "lookup_seconds": 0.0,
            "peer_hit": true,
            "peer_lookup_seconds": 0.262123,
            "peer_transfer_seconds": 0.781782,
            "peer_bytes_transferred": 606_055_356_u64,
            "run_id": "35676384016",
            "job": "app-host-unit-tests",
            "shard": "3",
            "runner_name": "cmux-mac-1"
        }))
        .unwrap()
    }

    #[test]
    fn peer_receipt_projects_transfer_and_canonical_restore() {
        let semantic = semantic_batch("attempt-1");
        let batch =
            project_cmux_product_transport("attempt-1", &semantic, &peer_receipt()).unwrap();

        assert_eq!(batch.lookup_source(), CmuxProductLookupSource::Peer);
        assert_eq!(batch.observations().len(), 2);
        assert_eq!(
            batch.observations()[0].stage(),
            VerificationStage::ArtifactTransfer
        );
        assert_eq!(batch.observations()[0].duration_millis(), 1_044);
        let json = batch.render_json().unwrap();
        assert!(json.contains("\"stage\": \"artifact_transfer\""));
        assert!(json.contains("\"reuse_class\": \"cold\""));
        assert_eq!(batch.observations()[1].stage(), VerificationStage::Restore);
        assert_eq!(batch.observations()[1].duration_millis(), 13_000);
        assert!(!batch.split_comparable_bytes_available());
    }

    #[test]
    fn repeated_peer_transfers_discover_exact_local_retention_without_split_claim() {
        let mut observations = Vec::new();
        for index in 0..3 {
            let attempt = format!("attempt-{index}");
            let semantic = semantic_batch(&attempt);
            let batch =
                project_cmux_product_transport(&attempt, &semantic, &peer_receipt()).unwrap();
            observations.extend_from_slice(batch.observations());
        }

        let receipt = compile_verification_optimizations(
            "cmux",
            "cmux.macos.app-host-test-shard@1",
            &observations,
            &[],
        )
        .unwrap();
        let classes = receipt
            .candidates()
            .iter()
            .map(|candidate| candidate.class())
            .collect::<BTreeSet<_>>();

        assert!(classes.contains(&OptimizationClass::RetainLocalImmutableArtifact));
        assert!(!classes.contains(&OptimizationClass::SplitConsumerArtifact));
        let retention = receipt
            .candidates()
            .iter()
            .find(|candidate| candidate.class() == OptimizationClass::RetainLocalImmutableArtifact)
            .unwrap();
        assert_eq!(
            retention.validity().status(),
            ValidityFingerprintStatus::Exact
        );
        assert_eq!(retention.utility().hit_frequency_basis_points(), 0);
    }

    #[test]
    fn local_hit_emits_restore_without_manufacturing_transfer_bytes() {
        let semantic = semantic_batch("attempt-local");
        let raw = serde_json::to_vec(&json!({
            "repository": "manaflow-ai/cmux",
            "artifact_id": 10_610_975_375_u64,
            "provider_digest": digest('c'),
            "archive_sha256": digest('d'),
            "product_contract": digest('e'),
            "source_revision": "1".repeat(40),
            "producer_run_id": 35_645_388_943_u64,
            "producer_run_attempt": 1_u64,
            "archive_bytes": 606_055_356_u64,
            "elapsed_seconds": 8.0,
            "lookup_source": "local",
            "local_hit": true,
            "lookup_seconds": 0.025,
            "peer_hit": false,
            "peer_lookup_seconds": 0.0,
            "peer_transfer_seconds": 0.0,
            "peer_bytes_transferred": 0,
            "run_id": "35676384017",
            "job": "app-host-unit-tests",
            "shard": "3",
            "runner_name": "cmux-mac-1"
        }))
        .unwrap();
        let batch = project_cmux_product_transport("attempt-local", &semantic, &raw).unwrap();

        assert_eq!(batch.observations().len(), 1);
        assert_eq!(batch.observations()[0].stage(), VerificationStage::Restore);
        assert_eq!(batch.observations()[0].duration_millis(), 8_025);
        let json = batch.render_json().unwrap();
        assert!(json.contains("\"reuse_class\": \"reuse\""));
    }

    #[test]
    fn source_revision_and_shard_must_match_semantic_workload() {
        let semantic = semantic_batch("attempt-mismatch");
        let mut source: serde_json::Value = serde_json::from_slice(&peer_receipt()).unwrap();
        source["source_revision"] = serde_json::Value::String("f".repeat(40));
        let err = project_cmux_product_transport(
            "attempt-mismatch",
            &semantic,
            &serde_json::to_vec(&source).unwrap(),
        )
        .unwrap_err();
        assert_eq!(err.code, "cmux_product_restore_source_mismatch");

        let mut shard: serde_json::Value = serde_json::from_slice(&peer_receipt()).unwrap();
        shard["shard"] = serde_json::Value::String("4".to_owned());
        let err = project_cmux_product_transport(
            "attempt-mismatch",
            &semantic,
            &serde_json::to_vec(&shard).unwrap(),
        )
        .unwrap_err();
        assert_eq!(err.code, "cmux_product_restore_shard_mismatch");
    }

    #[test]
    fn retention_hit_frequency_uses_restore_source_classes() {
        let mut observations = Vec::new();
        for index in 0..3 {
            let attempt = format!("peer-attempt-{index}");
            let semantic = semantic_batch(&attempt);
            let batch =
                project_cmux_product_transport(&attempt, &semantic, &peer_receipt()).unwrap();
            observations.extend_from_slice(batch.observations());
        }

        let semantic = semantic_batch("local-attempt");
        let local = serde_json::to_vec(&json!({
            "repository": "manaflow-ai/cmux",
            "artifact_id": 10_610_975_375_u64,
            "provider_digest": digest('c'),
            "archive_sha256": digest('d'),
            "product_contract": digest('e'),
            "source_revision": "1".repeat(40),
            "producer_run_id": 35_645_388_943_u64,
            "producer_run_attempt": 1_u64,
            "archive_bytes": 606_055_356_u64,
            "elapsed_seconds": 8.0,
            "lookup_source": "local",
            "local_hit": true,
            "lookup_seconds": 0.025,
            "peer_hit": false,
            "peer_lookup_seconds": 0.0,
            "peer_transfer_seconds": 0.0,
            "peer_bytes_transferred": 0,
            "run_id": "35676384019",
            "job": "app-host-unit-tests",
            "shard": "3",
            "runner_name": "cmux-mac-1"
        }))
        .unwrap();
        let batch = project_cmux_product_transport("local-attempt", &semantic, &local).unwrap();
        observations.extend_from_slice(batch.observations());

        let receipt = compile_verification_optimizations(
            "cmux",
            "cmux.macos.app-host-test-shard@1",
            &observations,
            &[],
        )
        .unwrap();
        let retention = receipt
            .candidates()
            .iter()
            .find(|candidate| candidate.class() == OptimizationClass::RetainLocalImmutableArtifact)
            .unwrap();

        assert_eq!(retention.utility().hit_frequency_basis_points(), 2_500);
    }

    #[test]
    fn inconsistent_peer_source_evidence_is_rejected() {
        let semantic = semantic_batch("attempt-bad");
        let raw = serde_json::to_vec(&json!({
            "repository": "manaflow-ai/cmux",
            "artifact_id": 10_610_975_375_u64,
            "provider_digest": digest('c'),
            "archive_sha256": digest('d'),
            "product_contract": digest('e'),
            "source_revision": "1".repeat(40),
            "producer_run_id": 35_645_388_943_u64,
            "producer_run_attempt": 1_u64,
            "archive_bytes": 606_055_356_u64,
            "elapsed_seconds": 8.0,
            "lookup_source": "peer",
            "local_hit": true,
            "lookup_seconds": 0.0,
            "peer_hit": true,
            "peer_lookup_seconds": 0.2,
            "peer_transfer_seconds": 0.8,
            "peer_bytes_transferred": 606_055_356_u64,
            "run_id": "35676384018",
            "job": "app-host-unit-tests",
            "shard": "3",
            "runner_name": "cmux-mac-1"
        }))
        .unwrap();

        let err = project_cmux_product_transport("attempt-bad", &semantic, &raw).unwrap_err();
        assert_eq!(err.code, "cmux_product_restore_source_inconsistent");
    }

    #[test]
    fn human_and_json_explain_incomparable_split_bytes() {
        let semantic = semantic_batch("attempt-render");
        let batch =
            project_cmux_product_transport("attempt-render", &semantic, &peer_receipt()).unwrap();
        let human = batch.render_human();
        let json = batch.render_json().unwrap();

        assert!(human.contains("source: peer"));
        assert!(human.contains("split-comparable bytes: false"));
        assert!(json.contains("\"peer_bytes_transferred\": 606055356"));
        assert!(json.contains("\"split_comparable_bytes_available\": false"));
    }
}
