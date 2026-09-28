//! Strict decoder for Quarry's bounded parallel verification receipt v2.
//!
//! Decoding proves only that supplied bytes satisfy Quarry's versioned, content-addressed wire
//! contract. It does not prove that Quarry ran, that the source or toolchain existed, that an outer
//! process observed these bytes, or that Glaeda may publish, reuse, settle, or merge anything.

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

pub const QUARRY_PARALLEL_VERIFICATION_SCHEMA_VERSION: u8 = 2;
pub const MAX_QUARRY_PARALLEL_VERIFICATION_PLAN_BYTES: usize = 32_768;
pub const MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES: usize = 65_536;

const PLAN_KIND: &str = "quarry-parallel-verification-plan-v2";
const RECEIPT_KIND: &str = "quarry-parallel-verification-receipt-v2";
const PLAN_ID_PREFIX: &str = "quarry-parallel-verification-plan-v2:sha256:";
const RECEIPT_ID_PREFIX: &str = "quarry-parallel-verification-receipt-v2:sha256:";
const SCHEDULER_POLICY: &str = "static_longest_first_v1";
const SOURCE_ISOLATION: &str = "detached_worktree_per_shard";
const TEMPORARY_ISOLATION: &str = "external_task_private";
const MAX_SHARDS: usize = 64;
const MAX_TEST_COUNT: u64 = 1_000_000;
const MAX_WALL_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_CLEANUP_FAILURE_CODES: usize = 64;
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarryParallelVerificationReceiptAuthority {
    SuppliedReceiptOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarryParallelVerificationReceipt {
    wire: ReceiptWire,
    canonical_bytes: Vec<u8>,
}

impl QuarryParallelVerificationReceipt {
    #[must_use]
    pub const fn authority(&self) -> QuarryParallelVerificationReceiptAuthority {
        QuarryParallelVerificationReceiptAuthority::SuppliedReceiptOnly
    }

    #[must_use]
    pub fn receipt_id(&self) -> &str {
        &self.wire.receipt_id
    }

    #[must_use]
    pub fn plan(&self) -> &QuarryParallelVerificationPlan {
        &self.wire.plan
    }

    #[must_use]
    pub const fn result(&self) -> &QuarryParallelVerificationResult {
        &self.wire.result
    }

    #[must_use]
    pub fn outcomes(&self) -> &[QuarryParallelVerificationShardOutcome] {
        &self.wire.outcomes
    }

    #[must_use]
    pub const fn cleanup(&self) -> &QuarryParallelVerificationCleanup {
        &self.wire.cleanup
    }

    #[must_use]
    pub const fn evidence_scope(&self) -> QuarryParallelVerificationEvidenceScope {
        self.wire.evidence_scope
    }

    #[must_use]
    pub fn verified_head(&self) -> Option<&str> {
        self.wire.verified_head.as_deref()
    }

    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarryParallelVerificationPlan {
    schema_version: u8,
    plan_kind: String,
    plan_id: String,
    key: QuarryParallelVerificationPlanKey,
}

impl QuarryParallelVerificationPlan {
    #[must_use]
    pub fn plan_id(&self) -> &str {
        &self.plan_id
    }

    #[must_use]
    pub const fn key(&self) -> &QuarryParallelVerificationPlanKey {
        &self.key
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarryParallelVerificationPlanKey {
    source: QuarryParallelVerificationSource,
    toolchain_id: String,
    collection: QuarryParallelVerificationCollection,
    scheduler: QuarryParallelVerificationScheduler,
    isolation: QuarryParallelVerificationIsolation,
    shards: Vec<QuarryParallelVerificationShardPlan>,
}

impl QuarryParallelVerificationPlanKey {
    #[must_use]
    pub const fn source(&self) -> &QuarryParallelVerificationSource {
        &self.source
    }

    #[must_use]
    pub fn toolchain_id(&self) -> &str {
        &self.toolchain_id
    }

    #[must_use]
    pub const fn collection(&self) -> &QuarryParallelVerificationCollection {
        &self.collection
    }

    #[must_use]
    pub const fn workers(&self) -> u16 {
        self.scheduler.workers
    }

    #[must_use]
    pub fn shards(&self) -> &[QuarryParallelVerificationShardPlan] {
        &self.shards
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarryParallelVerificationSource {
    commit: String,
    tree: String,
}

impl QuarryParallelVerificationSource {
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }

    #[must_use]
    pub fn tree(&self) -> &str {
        &self.tree
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarryParallelVerificationCollection {
    sha256: String,
    test_count: u64,
}

impl QuarryParallelVerificationCollection {
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    #[must_use]
    pub const fn test_count(&self) -> u64 {
        self.test_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuarryParallelVerificationScheduler {
    policy: String,
    workers: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuarryParallelVerificationIsolation {
    source: String,
    temporary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarryParallelVerificationShardPlan {
    name: String,
    command_sha256: String,
    node_ids_sha256: String,
    test_count: u64,
}

impl QuarryParallelVerificationShardPlan {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn command_sha256(&self) -> &str {
        &self.command_sha256
    }

    #[must_use]
    pub fn node_ids_sha256(&self) -> &str {
        &self.node_ids_sha256
    }

    #[must_use]
    pub const fn test_count(&self) -> u64 {
        self.test_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarryParallelVerificationResult {
    class: QuarryParallelVerificationResultClass,
    termination_reason: QuarryParallelVerificationTerminationReason,
    aggregate_wall_millis: u64,
}

impl QuarryParallelVerificationResult {
    #[must_use]
    pub const fn class(&self) -> QuarryParallelVerificationResultClass {
        self.class
    }

    #[must_use]
    pub const fn termination_reason(&self) -> QuarryParallelVerificationTerminationReason {
        self.termination_reason
    }

    #[must_use]
    pub const fn aggregate_wall_millis(&self) -> u64 {
        self.aggregate_wall_millis
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarryParallelVerificationResultClass {
    Passed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarryParallelVerificationTerminationReason {
    Completed,
    ShardFailure,
    Cancelled,
    Interrupted,
    CleanupFailure,
    InternalFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarryParallelVerificationShardOutcome {
    name: String,
    state: QuarryParallelVerificationShardState,
    wall_millis: Option<u64>,
    exit_code: Option<i16>,
    output_sha256: Option<String>,
    collection_sha256: Option<String>,
}

impl QuarryParallelVerificationShardOutcome {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn state(&self) -> QuarryParallelVerificationShardState {
        self.state
    }

    #[must_use]
    pub const fn wall_millis(&self) -> Option<u64> {
        self.wall_millis
    }

    #[must_use]
    pub const fn exit_code(&self) -> Option<i16> {
        self.exit_code
    }

    #[must_use]
    pub fn output_sha256(&self) -> Option<&str> {
        self.output_sha256.as_deref()
    }

    #[must_use]
    pub fn collection_sha256(&self) -> Option<&str> {
        self.collection_sha256.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarryParallelVerificationShardState {
    Passed,
    Failed,
    Cancelled,
    NotStarted,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarryParallelVerificationCleanup {
    status: QuarryParallelVerificationCleanupStatus,
    failure_codes: Vec<String>,
}

impl QuarryParallelVerificationCleanup {
    #[must_use]
    pub const fn status(&self) -> QuarryParallelVerificationCleanupStatus {
        self.status
    }

    #[must_use]
    pub fn failure_codes(&self) -> &[String] {
        &self.failure_codes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarryParallelVerificationCleanupStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarryParallelVerificationEvidenceScope {
    ExactHead,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptWire {
    schema_version: u8,
    receipt_kind: String,
    receipt_id: String,
    plan: QuarryParallelVerificationPlan,
    result: QuarryParallelVerificationResult,
    outcomes: Vec<QuarryParallelVerificationShardOutcome>,
    cleanup: QuarryParallelVerificationCleanup,
    evidence_scope: QuarryParallelVerificationEvidenceScope,
    verified_head: Option<String>,
    hosted_ci_evidence: bool,
    merge_authority: bool,
}

/// Decode one canonical newline-framed Quarry receipt without granting it execution authority.
///
/// # Errors
///
/// Returns a bounded fixed-class error for malformed framing or JSON, unknown fields, incompatible
/// versions, invalid identities, noncanonical bytes, stale content IDs, or inconsistent terminal
/// semantics.
pub fn decode_quarry_parallel_verification_receipt(
    bytes: &[u8],
) -> Result<QuarryParallelVerificationReceipt, QuarryParallelVerificationReceiptError> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(receipt_error(
            QuarryParallelVerificationReceiptErrorKind::InvalidDocument,
            "Quarry parallel verification receipt framing is invalid",
        ));
    }
    if bytes.len() > MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES {
        return Err(receipt_error(
            QuarryParallelVerificationReceiptErrorKind::DocumentTooLarge,
            "Quarry parallel verification receipt exceeds the reviewed byte limit",
        ));
    }
    let document = &bytes[..bytes.len() - 1];
    let value: Value = serde_json::from_slice(document).map_err(|_| malformed())?;
    let wire: ReceiptWire = serde_json::from_slice(document).map_err(|_| malformed())?;
    if wire.schema_version != QUARRY_PARALLEL_VERIFICATION_SCHEMA_VERSION
        || wire.plan.schema_version != QUARRY_PARALLEL_VERIFICATION_SCHEMA_VERSION
    {
        return Err(receipt_error(
            QuarryParallelVerificationReceiptErrorKind::VersionIncompatible,
            "Quarry parallel verification receipt schema version is unsupported",
        ));
    }

    let mut canonical = serde_json::to_vec(&value).map_err(|_| malformed())?;
    canonical.push(b'\n');
    if canonical != bytes {
        return Err(receipt_error(
            QuarryParallelVerificationReceiptErrorKind::NonCanonical,
            "Quarry parallel verification receipt is not canonical newline-framed JSON",
        ));
    }
    validate_plan(&wire.plan, &value)?;
    validate_receipt(&wire, &value)?;
    Ok(QuarryParallelVerificationReceipt {
        wire,
        canonical_bytes: canonical,
    })
}

fn validate_plan(
    plan: &QuarryParallelVerificationPlan,
    document: &Value,
) -> Result<(), QuarryParallelVerificationReceiptError> {
    if plan.plan_kind != PLAN_KIND {
        return Err(invalid_plan());
    }
    let plan_value = document.get("plan").ok_or_else(malformed)?;
    if serde_json::to_vec(plan_value)
        .map_err(|_| malformed())?
        .len()
        > MAX_QUARRY_PARALLEL_VERIFICATION_PLAN_BYTES
    {
        return Err(receipt_error(
            QuarryParallelVerificationReceiptErrorKind::PlanTooLarge,
            "Quarry parallel verification plan exceeds the reviewed byte limit",
        ));
    }
    if !valid_git_object(&plan.key.source.commit)
        || !valid_git_object(&plan.key.source.tree)
        || !valid_toolchain_id(&plan.key.toolchain_id)
        || !valid_sha256(&plan.key.collection.sha256)
        || !(1..=MAX_TEST_COUNT).contains(&plan.key.collection.test_count)
        || plan.key.scheduler.policy != SCHEDULER_POLICY
        || plan.key.isolation.source != SOURCE_ISOLATION
        || plan.key.isolation.temporary != TEMPORARY_ISOLATION
        || plan.key.shards.is_empty()
        || plan.key.shards.len() > MAX_SHARDS
        || usize::from(plan.key.scheduler.workers) == 0
        || usize::from(plan.key.scheduler.workers) > plan.key.shards.len()
    {
        return Err(invalid_plan());
    }
    let mut names = BTreeSet::new();
    let mut test_count = 0_u64;
    for shard in &plan.key.shards {
        if !valid_name(&shard.name)
            || !names.insert(shard.name.as_str())
            || !valid_sha256(&shard.command_sha256)
            || !valid_sha256(&shard.node_ids_sha256)
            || !(1..=MAX_TEST_COUNT).contains(&shard.test_count)
        {
            return Err(invalid_plan());
        }
        test_count = test_count
            .checked_add(shard.test_count)
            .ok_or_else(invalid_plan)?;
    }
    if test_count != plan.key.collection.test_count {
        return Err(invalid_plan());
    }
    let key = plan_value.get("key").ok_or_else(malformed)?;
    let key_bytes = serde_json::to_vec(key).map_err(|_| malformed())?;
    if plan.plan_id != content_id(PLAN_ID_PREFIX, &key_bytes) {
        return Err(receipt_error(
            QuarryParallelVerificationReceiptErrorKind::IdentityMismatch,
            "Quarry parallel verification plan identity does not match its key",
        ));
    }
    Ok(())
}

fn validate_receipt(
    receipt: &ReceiptWire,
    document: &Value,
) -> Result<(), QuarryParallelVerificationReceiptError> {
    if receipt.receipt_kind != RECEIPT_KIND
        || receipt.hosted_ci_evidence
        || receipt.merge_authority
        || receipt.result.aggregate_wall_millis > MAX_WALL_MILLIS
        || receipt.outcomes.len() != receipt.plan.key.shards.len()
    {
        return Err(invalid_terminal());
    }
    let mut maximum_shard_wall = 0_u64;
    for (planned, outcome) in receipt.plan.key.shards.iter().zip(&receipt.outcomes) {
        if outcome.name != planned.name || !valid_outcome(outcome) {
            return Err(receipt_error(
                QuarryParallelVerificationReceiptErrorKind::InvalidOutcome,
                "Quarry parallel verification shard outcome is inconsistent",
            ));
        }
        maximum_shard_wall = maximum_shard_wall.max(outcome.wall_millis.unwrap_or(0));
    }
    if receipt.result.aggregate_wall_millis < maximum_shard_wall {
        return Err(invalid_terminal());
    }
    validate_cleanup(&receipt.cleanup)?;

    let all_passed = receipt
        .outcomes
        .iter()
        .all(|outcome| outcome.state == QuarryParallelVerificationShardState::Passed);
    let has_failed = receipt
        .outcomes
        .iter()
        .any(|outcome| outcome.state == QuarryParallelVerificationShardState::Failed);
    let has_unfinished = receipt.outcomes.iter().any(|outcome| {
        matches!(
            outcome.state,
            QuarryParallelVerificationShardState::Cancelled
                | QuarryParallelVerificationShardState::NotStarted
        )
    });
    match receipt.result.termination_reason {
        QuarryParallelVerificationTerminationReason::Completed
            if all_passed
                && receipt.cleanup.status == QuarryParallelVerificationCleanupStatus::Passed => {}
        QuarryParallelVerificationTerminationReason::ShardFailure if has_failed => {}
        QuarryParallelVerificationTerminationReason::Cancelled
        | QuarryParallelVerificationTerminationReason::Interrupted
            if has_unfinished => {}
        QuarryParallelVerificationTerminationReason::CleanupFailure
            if receipt.cleanup.status == QuarryParallelVerificationCleanupStatus::Failed => {}
        QuarryParallelVerificationTerminationReason::InternalFailure => {}
        _ => return Err(invalid_terminal()),
    }
    let completed =
        receipt.result.termination_reason == QuarryParallelVerificationTerminationReason::Completed;
    if receipt.result.class
        != if completed {
            QuarryParallelVerificationResultClass::Passed
        } else {
            QuarryParallelVerificationResultClass::Failed
        }
        || receipt.evidence_scope
            != if completed {
                QuarryParallelVerificationEvidenceScope::ExactHead
            } else {
                QuarryParallelVerificationEvidenceScope::None
            }
        || receipt.verified_head.as_deref()
            != if completed {
                Some(receipt.plan.key.source.commit.as_str())
            } else {
                None
            }
    {
        return Err(invalid_terminal());
    }

    let object = document.as_object().ok_or_else(malformed)?;
    let mut core = Map::new();
    for field in [
        "plan",
        "result",
        "outcomes",
        "cleanup",
        "evidence_scope",
        "verified_head",
        "hosted_ci_evidence",
        "merge_authority",
    ] {
        core.insert(
            field.to_owned(),
            object.get(field).cloned().ok_or_else(malformed)?,
        );
    }
    let core_bytes = serde_json::to_vec(&Value::Object(core)).map_err(|_| malformed())?;
    if receipt.receipt_id != content_id(RECEIPT_ID_PREFIX, &core_bytes) {
        return Err(receipt_error(
            QuarryParallelVerificationReceiptErrorKind::IdentityMismatch,
            "Quarry parallel verification receipt identity does not match its content",
        ));
    }
    Ok(())
}

fn valid_outcome(outcome: &QuarryParallelVerificationShardOutcome) -> bool {
    let no_measurements = outcome.wall_millis.is_none()
        && outcome.exit_code.is_none()
        && outcome.output_sha256.is_none()
        && outcome.collection_sha256.is_none();
    if outcome.state == QuarryParallelVerificationShardState::NotStarted {
        return no_measurements;
    }
    if outcome.state == QuarryParallelVerificationShardState::Cancelled && no_measurements {
        return true;
    }
    let (Some(wall), Some(exit), Some(output)) = (
        outcome.wall_millis,
        outcome.exit_code,
        outcome.output_sha256.as_deref(),
    ) else {
        return false;
    };
    if wall > MAX_WALL_MILLIS
        || !(-255..=255).contains(&exit)
        || !valid_sha256(output)
        || outcome
            .collection_sha256
            .as_deref()
            .is_some_and(|digest| !valid_sha256(digest))
    {
        return false;
    }
    match outcome.state {
        QuarryParallelVerificationShardState::Passed => {
            exit == 0 && outcome.collection_sha256.is_some()
        }
        QuarryParallelVerificationShardState::Failed
        | QuarryParallelVerificationShardState::Cancelled => exit != 0,
        QuarryParallelVerificationShardState::NotStarted => false,
    }
}

fn validate_cleanup(
    cleanup: &QuarryParallelVerificationCleanup,
) -> Result<(), QuarryParallelVerificationReceiptError> {
    if cleanup.failure_codes.len() > MAX_CLEANUP_FAILURE_CODES
        || (cleanup.status == QuarryParallelVerificationCleanupStatus::Passed
            && !cleanup.failure_codes.is_empty())
        || (cleanup.status == QuarryParallelVerificationCleanupStatus::Failed
            && cleanup.failure_codes.is_empty())
    {
        return Err(invalid_terminal());
    }
    let mut codes = BTreeSet::new();
    if cleanup
        .failure_codes
        .iter()
        .any(|code| !valid_failure_code(code) || !codes.insert(code.as_str()))
    {
        return Err(invalid_terminal());
    }
    Ok(())
}

fn content_id(prefix: &str, bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(prefix.len() + digest.len() * 2);
    value.push_str(prefix);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    value
}

fn valid_git_object(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(is_lower_hex)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(is_lower_hex)
}

fn valid_toolchain_id(value: &str) -> bool {
    let Some((name, digest)) = value.split_once(":sha256:") else {
        return false;
    };
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .skip(1)
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && valid_sha256(digest)
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .skip(1)
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_failure_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().skip(1).all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'.' | b':' | b'-')
        })
}

const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarryParallelVerificationReceiptErrorKind {
    InvalidDocument,
    VersionIncompatible,
    DocumentTooLarge,
    PlanTooLarge,
    NonCanonical,
    InvalidPlan,
    InvalidOutcome,
    InvalidTerminal,
    IdentityMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarryParallelVerificationReceiptError {
    kind: QuarryParallelVerificationReceiptErrorKind,
    message: &'static str,
}

impl QuarryParallelVerificationReceiptError {
    #[must_use]
    pub const fn kind(&self) -> QuarryParallelVerificationReceiptErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for QuarryParallelVerificationReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for QuarryParallelVerificationReceiptError {}

const fn receipt_error(
    kind: QuarryParallelVerificationReceiptErrorKind,
    message: &'static str,
) -> QuarryParallelVerificationReceiptError {
    QuarryParallelVerificationReceiptError { kind, message }
}

const fn malformed() -> QuarryParallelVerificationReceiptError {
    receipt_error(
        QuarryParallelVerificationReceiptErrorKind::InvalidDocument,
        "Quarry parallel verification receipt JSON or schema is invalid",
    )
}

const fn invalid_plan() -> QuarryParallelVerificationReceiptError {
    receipt_error(
        QuarryParallelVerificationReceiptErrorKind::InvalidPlan,
        "Quarry parallel verification plan is inconsistent",
    )
}

const fn invalid_terminal() -> QuarryParallelVerificationReceiptError {
    receipt_error(
        QuarryParallelVerificationReceiptErrorKind::InvalidTerminal,
        "Quarry parallel verification terminal semantics are inconsistent",
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const QUARRY_FIXTURE: &[u8] =
        include_bytes!("../tests/fixtures/quarry_parallel_verification_receipt_v2.json");

    fn fixture_value() -> Value {
        serde_json::from_slice(&QUARRY_FIXTURE[..QUARRY_FIXTURE.len() - 1]).expect("fixture JSON")
    }

    fn canonical(value: Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&value).expect("canonical JSON");
        bytes.push(b'\n');
        bytes
    }

    fn refresh_receipt_id(value: &mut Value) {
        let object = value.as_object().expect("receipt object");
        let mut core = Map::new();
        for field in [
            "plan",
            "result",
            "outcomes",
            "cleanup",
            "evidence_scope",
            "verified_head",
            "hosted_ci_evidence",
            "merge_authority",
        ] {
            core.insert(field.to_owned(), object[field].clone());
        }
        let id = content_id(
            RECEIPT_ID_PREFIX,
            &serde_json::to_vec(&Value::Object(core)).expect("core JSON"),
        );
        value["receipt_id"] = json!(id);
    }

    #[test]
    fn exact_quarry_generated_fixture_decodes_as_supplied_only() {
        let receipt =
            decode_quarry_parallel_verification_receipt(QUARRY_FIXTURE).expect("decode fixture");

        assert_eq!(
            receipt.authority(),
            QuarryParallelVerificationReceiptAuthority::SuppliedReceiptOnly
        );
        assert_eq!(
            receipt.receipt_id(),
            "quarry-parallel-verification-receipt-v2:sha256:b8305b6bb597489de3b32a47716e9e8e7dd5e522e9e0424ae7f7b9fcc5f7725e"
        );
        assert_eq!(receipt.plan().key().workers(), 2);
        assert_eq!(receipt.plan().key().collection().test_count(), 3);
        assert_eq!(receipt.outcomes().len(), 2);
        assert_eq!(
            receipt.outcomes()[0].state(),
            QuarryParallelVerificationShardState::Failed
        );
        assert_eq!(
            receipt.outcomes()[1].state(),
            QuarryParallelVerificationShardState::Cancelled
        );
        assert_eq!(
            receipt.evidence_scope(),
            QuarryParallelVerificationEvidenceScope::None
        );
        assert_eq!(receipt.verified_head(), None);
        assert_eq!(receipt.canonical_bytes(), QUARRY_FIXTURE);
    }

    #[test]
    fn valid_completed_receipt_claim_decodes_without_gaining_authority() {
        let mut value = fixture_value();
        value["outcomes"][0]["state"] = json!("passed");
        value["outcomes"][0]["exit_code"] = json!(0);
        value["outcomes"][1] = json!({
            "collection_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "exit_code": 0,
            "name": "regime",
            "output_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "state": "passed",
            "wall_millis": 100
        });
        value["result"]["class"] = json!("passed");
        value["result"]["termination_reason"] = json!("completed");
        value["evidence_scope"] = json!("exact_head");
        value["verified_head"] = json!("1111111111111111111111111111111111111111");
        refresh_receipt_id(&mut value);

        let receipt = decode_quarry_parallel_verification_receipt(&canonical(value))
            .expect("decode completed claim");

        assert_eq!(
            receipt.result().class(),
            QuarryParallelVerificationResultClass::Passed
        );
        assert_eq!(
            receipt.authority(),
            QuarryParallelVerificationReceiptAuthority::SuppliedReceiptOnly
        );
        assert_eq!(
            receipt.verified_head(),
            Some("1111111111111111111111111111111111111111")
        );
    }

    #[test]
    fn framing_schema_and_canonical_json_are_strict() {
        assert_eq!(
            decode_quarry_parallel_verification_receipt(
                &QUARRY_FIXTURE[..QUARRY_FIXTURE.len() - 1]
            )
            .unwrap_err()
            .kind(),
            QuarryParallelVerificationReceiptErrorKind::InvalidDocument
        );

        let pretty = serde_json::to_vec_pretty(&fixture_value()).expect("pretty fixture");
        let mut pretty_framed = pretty;
        pretty_framed.push(b'\n');
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&pretty_framed)
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::NonCanonical
        );

        let mut unknown = fixture_value();
        unknown["path"] = json!("/private/path");
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&canonical(unknown))
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::InvalidDocument
        );

        let duplicate = String::from_utf8(QUARRY_FIXTURE.to_vec())
            .unwrap()
            .replacen("{", "{\"schema_version\":2,", 1);
        assert_eq!(
            decode_quarry_parallel_verification_receipt(duplicate.as_bytes())
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::InvalidDocument
        );

        let mut oversized = vec![b' '; MAX_QUARRY_PARALLEL_VERIFICATION_RECEIPT_BYTES + 1];
        *oversized.last_mut().unwrap() = b'\n';
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&oversized)
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::DocumentTooLarge
        );
    }

    #[test]
    fn plan_and_receipt_content_ids_are_independently_reconstructed() {
        let mut plan_drift = fixture_value();
        plan_drift["plan"]["key"]["collection"]["sha256"] = json!("d".repeat(64));
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&canonical(plan_drift))
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::IdentityMismatch
        );

        let mut receipt_drift = fixture_value();
        receipt_drift["outcomes"][0]["output_sha256"] = json!("e".repeat(64));
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&canonical(receipt_drift))
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::IdentityMismatch
        );
    }

    #[test]
    fn plan_privacy_resource_and_inventory_constraints_fail_closed() {
        for mutation in [
            (json!(3), "workers"),
            (json!("/private/python"), "toolchain"),
        ] {
            let mut value = fixture_value();
            if mutation.1 == "workers" {
                value["plan"]["key"]["scheduler"]["workers"] = mutation.0;
            } else {
                value["plan"]["key"]["toolchain_id"] = mutation.0;
            }
            assert_eq!(
                decode_quarry_parallel_verification_receipt(&canonical(value))
                    .unwrap_err()
                    .kind(),
                QuarryParallelVerificationReceiptErrorKind::InvalidPlan
            );
        }

        let mut duplicate = fixture_value();
        duplicate["plan"]["key"]["shards"][1]["name"] = json!("routine");
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&canonical(duplicate))
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::InvalidPlan
        );
    }

    #[test]
    fn outcome_cleanup_and_terminal_contradictions_fail_closed() {
        let mut reordered = fixture_value();
        reordered["outcomes"].as_array_mut().unwrap().reverse();
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&canonical(reordered))
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::InvalidOutcome
        );

        let mut false_exact = fixture_value();
        false_exact["evidence_scope"] = json!("exact_head");
        false_exact["verified_head"] = json!("1111111111111111111111111111111111111111");
        refresh_receipt_id(&mut false_exact);
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&canonical(false_exact))
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::InvalidTerminal
        );

        let mut cleanup = fixture_value();
        cleanup["cleanup"]["status"] = json!("failed");
        refresh_receipt_id(&mut cleanup);
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&canonical(cleanup))
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::InvalidTerminal
        );

        let mut missing_collection = fixture_value();
        missing_collection["outcomes"][0]["state"] = json!("passed");
        missing_collection["outcomes"][0]["exit_code"] = json!(0);
        missing_collection["outcomes"][0]["collection_sha256"] = Value::Null;
        assert_eq!(
            decode_quarry_parallel_verification_receipt(&canonical(missing_collection))
                .unwrap_err()
                .kind(),
            QuarryParallelVerificationReceiptErrorKind::InvalidOutcome
        );
    }
}
