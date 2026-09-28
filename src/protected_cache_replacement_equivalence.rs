//! Strict, path-free replacement-equivalence receipt vocabulary for protected cache generations.
//!
//! A decoded receipt is caller-supplied equality evidence only. It cannot adopt physical state,
//! publish a catalog generation, construct transition authorization, authorize cache reuse, or
//! authorize reclamation. A later protected producer must bind this vocabulary to fresh physical
//! materialization and semantic-validation evidence before the catalog transition gate can open.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::cache_inventory::CacheStateId;
use crate::personal_worker_queue::PersonalWorkerSourceIdentity;
use crate::protected_cache_generation_catalog::{
    ProtectedCacheGenerationFamily, ProtectedCacheGenerationIdentity,
    ProtectedCacheNamespaceIdentity,
};

pub const PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE_SCHEMA_VERSION: u8 = 1;
pub const MAX_PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE_BYTES: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheReplacementEquivalenceAuthority {
    SuppliedReceiptOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheReplacementEquivalenceCorrelation {
    ExactSuppliedReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedCacheReplacementTarget {
    family: ProtectedCacheGenerationFamily,
    namespace_identity: ProtectedCacheNamespaceIdentity,
    state_id: CacheStateId,
    materialized_generation_identity: ProtectedCacheGenerationIdentity,
}

impl ProtectedCacheReplacementTarget {
    #[must_use]
    pub const fn new(
        family: ProtectedCacheGenerationFamily,
        namespace_identity: ProtectedCacheNamespaceIdentity,
        state_id: CacheStateId,
        materialized_generation_identity: ProtectedCacheGenerationIdentity,
    ) -> Self {
        Self {
            family,
            namespace_identity,
            state_id,
            materialized_generation_identity,
        }
    }

    #[must_use]
    pub const fn family(&self) -> ProtectedCacheGenerationFamily {
        self.family
    }

    #[must_use]
    pub const fn namespace_identity(&self) -> &ProtectedCacheNamespaceIdentity {
        &self.namespace_identity
    }

    #[must_use]
    pub const fn state_id(&self) -> &CacheStateId {
        &self.state_id
    }

    #[must_use]
    pub const fn materialized_generation_identity(&self) -> &ProtectedCacheGenerationIdentity {
        &self.materialized_generation_identity
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedCacheReconstructionBinding {
    source: PersonalWorkerSourceIdentity,
    canonical_inputs_digest: Sha256Digest,
    plan_generation_digest: Sha256Digest,
    validator_generation_digest: Sha256Digest,
    toolchain_envelope_digest: Sha256Digest,
}

impl ProtectedCacheReconstructionBinding {
    #[must_use]
    pub const fn new(
        source: PersonalWorkerSourceIdentity,
        canonical_inputs_digest: Sha256Digest,
        plan_generation_digest: Sha256Digest,
        validator_generation_digest: Sha256Digest,
        toolchain_envelope_digest: Sha256Digest,
    ) -> Self {
        Self {
            source,
            canonical_inputs_digest,
            plan_generation_digest,
            validator_generation_digest,
            toolchain_envelope_digest,
        }
    }

    #[must_use]
    pub const fn source(&self) -> &PersonalWorkerSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn canonical_inputs_digest(&self) -> &Sha256Digest {
        &self.canonical_inputs_digest
    }

    #[must_use]
    pub const fn plan_generation_digest(&self) -> &Sha256Digest {
        &self.plan_generation_digest
    }

    #[must_use]
    pub const fn validator_generation_digest(&self) -> &Sha256Digest {
        &self.validator_generation_digest
    }

    #[must_use]
    pub const fn toolchain_envelope_digest(&self) -> &Sha256Digest {
        &self.toolchain_envelope_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedCacheReplacementEquivalenceBinding {
    target: ProtectedCacheReplacementTarget,
    reconstruction: ProtectedCacheReconstructionBinding,
    family_semantic_digest: Sha256Digest,
}

impl ProtectedCacheReplacementEquivalenceBinding {
    #[must_use]
    pub const fn new(
        target: ProtectedCacheReplacementTarget,
        reconstruction: ProtectedCacheReconstructionBinding,
        family_semantic_digest: Sha256Digest,
    ) -> Self {
        Self {
            target,
            reconstruction,
            family_semantic_digest,
        }
    }

    #[must_use]
    pub const fn target(&self) -> &ProtectedCacheReplacementTarget {
        &self.target
    }

    #[must_use]
    pub const fn reconstruction(&self) -> &ProtectedCacheReconstructionBinding {
        &self.reconstruction
    }

    #[must_use]
    pub const fn family_semantic_digest(&self) -> &Sha256Digest {
        &self.family_semantic_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedCacheReplacementEquivalenceReceipt {
    schema_version: u8,
    binding: ProtectedCacheReplacementEquivalenceBinding,
}

impl ProtectedCacheReplacementEquivalenceReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn authority(&self) -> ProtectedCacheReplacementEquivalenceAuthority {
        ProtectedCacheReplacementEquivalenceAuthority::SuppliedReceiptOnly
    }

    #[must_use]
    pub const fn binding(&self) -> &ProtectedCacheReplacementEquivalenceBinding {
        &self.binding
    }

    /// Compare every receipt field with one caller-owned expected binding.
    ///
    /// Exact correlation remains supplied equality evidence. It grants no physical cache,
    /// transition, reuse, lease, reset, eviction, or cleanup authority.
    ///
    /// # Errors
    ///
    /// Returns an error when any expected identity or generation differs.
    pub fn correlate(
        &self,
        expected: &ProtectedCacheReplacementEquivalenceBinding,
    ) -> Result<
        ProtectedCacheReplacementEquivalenceCorrelation,
        ProtectedCacheReplacementEquivalenceError,
    > {
        if &self.binding != expected {
            return Err(replacement_error(
                ProtectedCacheReplacementEquivalenceErrorKind::BindingMismatch,
                "protected cache replacement-equivalence binding does not match",
            ));
        }
        Ok(ProtectedCacheReplacementEquivalenceCorrelation::ExactSuppliedReceipt)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptWire {
    schema_version: u8,
    receipt_type: ReceiptTypeWire,
    family: FamilyWire,
    namespace_identity: String,
    state_id: String,
    materialized_generation_identity: String,
    source: SourceWire,
    canonical_reconstruction_inputs_digest: String,
    reconstruction_plan_generation_digest: String,
    validator_generation_digest: String,
    toolchain_envelope_digest: String,
    family_semantic_digest: String,
    outcome: OutcomeWire,
}

#[derive(Debug, Deserialize)]
struct ReceiptVersionWire {
    schema_version: u8,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptTypeWire {
    CargoTargetReplacementEquivalenceV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FamilyWire {
    CargoTargetV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceWire {
    repository: String,
    commit: String,
    tree: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutcomeWire {
    Equivalent,
}

/// Encode one already-decoded supplied receipt as canonical bounded JSON.
///
/// This encoder does not upgrade the receipt's `supplied_receipt_only` authority.
///
/// # Errors
///
/// Returns an error if the fixed receipt cannot be encoded within the reviewed byte bound.
pub fn encode_protected_cache_replacement_equivalence_receipt(
    receipt: &ProtectedCacheReplacementEquivalenceReceipt,
) -> Result<Vec<u8>, ProtectedCacheReplacementEquivalenceError> {
    let target = receipt.binding.target();
    let reconstruction = receipt.binding.reconstruction();
    let source = reconstruction.source();
    let wire = ReceiptWire {
        schema_version: receipt.schema_version,
        receipt_type: ReceiptTypeWire::CargoTargetReplacementEquivalenceV1,
        family: match target.family() {
            ProtectedCacheGenerationFamily::CargoTargetV1 => FamilyWire::CargoTargetV1,
        },
        namespace_identity: target.namespace_identity().as_str().to_owned(),
        state_id: target.state_id().as_str().to_owned(),
        materialized_generation_identity: target
            .materialized_generation_identity()
            .as_str()
            .to_owned(),
        source: SourceWire {
            repository: source.repository.as_str().to_owned(),
            commit: source.commit.as_str().to_owned(),
            tree: source.tree.as_str().to_owned(),
        },
        canonical_reconstruction_inputs_digest: reconstruction
            .canonical_inputs_digest()
            .as_str()
            .to_owned(),
        reconstruction_plan_generation_digest: reconstruction
            .plan_generation_digest()
            .as_str()
            .to_owned(),
        validator_generation_digest: reconstruction
            .validator_generation_digest()
            .as_str()
            .to_owned(),
        toolchain_envelope_digest: reconstruction
            .toolchain_envelope_digest()
            .as_str()
            .to_owned(),
        family_semantic_digest: receipt.binding.family_semantic_digest().as_str().to_owned(),
        outcome: OutcomeWire::Equivalent,
    };
    let bytes = serde_json::to_vec(&wire).map_err(|_| {
        replacement_error(
            ProtectedCacheReplacementEquivalenceErrorKind::InvalidDocument,
            "protected cache replacement-equivalence receipt cannot encode",
        )
    })?;
    if bytes.len() > MAX_PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

/// Decode and strictly canonicalize one supplied replacement-equivalence receipt.
///
/// The receipt's `equivalent` outcome is a claim in supplied bytes, not trusted success evidence.
/// The returned value therefore remains `supplied_receipt_only` even after exact correlation.
///
/// # Errors
///
/// Returns an error for empty, malformed, oversized, future-version, unknown-field,
/// noncanonical, or invalid-identity documents.
pub fn decode_protected_cache_replacement_equivalence_receipt(
    bytes: &[u8],
) -> Result<ProtectedCacheReplacementEquivalenceReceipt, ProtectedCacheReplacementEquivalenceError>
{
    if bytes.is_empty() {
        return Err(replacement_error(
            ProtectedCacheReplacementEquivalenceErrorKind::InvalidDocument,
            "protected cache replacement-equivalence receipt is empty",
        ));
    }
    if bytes.len() > MAX_PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE_BYTES {
        return Err(too_large());
    }

    let version: ReceiptVersionWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if version.schema_version != PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE_SCHEMA_VERSION {
        return Err(replacement_error(
            ProtectedCacheReplacementEquivalenceErrorKind::VersionIncompatible,
            "protected cache replacement-equivalence schema version is unsupported",
        ));
    }
    let wire: ReceiptWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let source = PersonalWorkerSourceIdentity::new(
        RepositoryRef::parse(&wire.source.repository).map_err(|_| corrupt())?,
        CommitId::parse(&wire.source.commit).map_err(|_| corrupt())?,
        GitTreeId::parse(&wire.source.tree).map_err(|_| corrupt())?,
    );
    let target = ProtectedCacheReplacementTarget::new(
        match wire.family {
            FamilyWire::CargoTargetV1 => ProtectedCacheGenerationFamily::CargoTargetV1,
        },
        ProtectedCacheNamespaceIdentity::parse(&wire.namespace_identity).map_err(|_| corrupt())?,
        CacheStateId::parse(&wire.state_id).map_err(|_| corrupt())?,
        ProtectedCacheGenerationIdentity::parse(&wire.materialized_generation_identity)
            .map_err(|_| corrupt())?,
    );
    let reconstruction = ProtectedCacheReconstructionBinding::new(
        source,
        parse_digest(&wire.canonical_reconstruction_inputs_digest)?,
        parse_digest(&wire.reconstruction_plan_generation_digest)?,
        parse_digest(&wire.validator_generation_digest)?,
        parse_digest(&wire.toolchain_envelope_digest)?,
    );
    let receipt = ProtectedCacheReplacementEquivalenceReceipt {
        schema_version: PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE_SCHEMA_VERSION,
        binding: ProtectedCacheReplacementEquivalenceBinding::new(
            target,
            reconstruction,
            parse_digest(&wire.family_semantic_digest)?,
        ),
    };
    if encode_protected_cache_replacement_equivalence_receipt(&receipt)? != bytes {
        return Err(replacement_error(
            ProtectedCacheReplacementEquivalenceErrorKind::NonCanonical,
            "protected cache replacement-equivalence receipt is not canonical JSON",
        ));
    }
    Ok(receipt)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheReplacementEquivalenceErrorKind {
    InvalidDocument,
    VersionIncompatible,
    DocumentTooLarge,
    NonCanonical,
    CorruptReceipt,
    BindingMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedCacheReplacementEquivalenceError {
    kind: ProtectedCacheReplacementEquivalenceErrorKind,
    message: &'static str,
}

impl ProtectedCacheReplacementEquivalenceError {
    #[must_use]
    pub const fn kind(&self) -> ProtectedCacheReplacementEquivalenceErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for ProtectedCacheReplacementEquivalenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProtectedCacheReplacementEquivalenceError {}

const fn replacement_error(
    kind: ProtectedCacheReplacementEquivalenceErrorKind,
    message: &'static str,
) -> ProtectedCacheReplacementEquivalenceError {
    ProtectedCacheReplacementEquivalenceError { kind, message }
}

fn parse_digest(value: &str) -> Result<Sha256Digest, ProtectedCacheReplacementEquivalenceError> {
    Sha256Digest::parse(value).map_err(|_| corrupt())
}

const fn malformed() -> ProtectedCacheReplacementEquivalenceError {
    replacement_error(
        ProtectedCacheReplacementEquivalenceErrorKind::InvalidDocument,
        "protected cache replacement-equivalence receipt JSON is invalid",
    )
}

const fn corrupt() -> ProtectedCacheReplacementEquivalenceError {
    replacement_error(
        ProtectedCacheReplacementEquivalenceErrorKind::CorruptReceipt,
        "protected cache replacement-equivalence receipt identity is invalid",
    )
}

const fn too_large() -> ProtectedCacheReplacementEquivalenceError {
    replacement_error(
        ProtectedCacheReplacementEquivalenceErrorKind::DocumentTooLarge,
        "protected cache replacement-equivalence receipt exceeds the reviewed byte limit",
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn digest(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    fn binding() -> ProtectedCacheReplacementEquivalenceBinding {
        ProtectedCacheReplacementEquivalenceBinding::new(
            ProtectedCacheReplacementTarget::new(
                ProtectedCacheGenerationFamily::CargoTargetV1,
                ProtectedCacheNamespaceIdentity::parse(&digest('a')).expect("namespace"),
                CacheStateId::parse("cargo-target-main-rust-1-97").expect("state"),
                ProtectedCacheGenerationIdentity::parse(&digest('b')).expect("generation"),
            ),
            ProtectedCacheReconstructionBinding::new(
                PersonalWorkerSourceIdentity::new(
                    RepositoryRef::parse("teamleaderleo/glaeda").expect("repository"),
                    CommitId::parse(&"1a".repeat(20)).expect("commit"),
                    GitTreeId::parse(&"2b".repeat(20)).expect("tree"),
                ),
                Sha256Digest::parse(&digest('c')).expect("inputs"),
                Sha256Digest::parse(&digest('d')).expect("plan"),
                Sha256Digest::parse(&digest('e')).expect("validator"),
                Sha256Digest::parse(&digest('f')).expect("toolchain"),
            ),
            Sha256Digest::parse(&digest('0')).expect("semantics"),
        )
    }

    fn receipt() -> ProtectedCacheReplacementEquivalenceReceipt {
        ProtectedCacheReplacementEquivalenceReceipt {
            schema_version: PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE_SCHEMA_VERSION,
            binding: binding(),
        }
    }

    #[test]
    fn canonical_receipt_round_trips_as_supplied_equality_only() {
        let expected = binding();
        let bytes = encode_protected_cache_replacement_equivalence_receipt(&receipt())
            .expect("encode receipt");
        let decoded =
            decode_protected_cache_replacement_equivalence_receipt(&bytes).expect("decode receipt");

        assert_eq!(decoded.schema_version(), 1);
        assert_eq!(decoded.binding(), &expected);
        assert_eq!(
            decoded.authority(),
            ProtectedCacheReplacementEquivalenceAuthority::SuppliedReceiptOnly
        );
        assert_eq!(
            decoded.correlate(&expected),
            Ok(ProtectedCacheReplacementEquivalenceCorrelation::ExactSuppliedReceipt)
        );
        assert_eq!(
            encode_protected_cache_replacement_equivalence_receipt(&decoded).unwrap(),
            bytes
        );
    }

    #[test]
    fn canonical_wire_shape_is_fixed_and_path_free() {
        let bytes = encode_protected_cache_replacement_equivalence_receipt(&receipt()).unwrap();
        let rendered = String::from_utf8(bytes).expect("UTF-8");
        let expected = format!(
            concat!(
                "{{\"schema_version\":1,",
                "\"receipt_type\":\"cargo_target_replacement_equivalence_v1\",",
                "\"family\":\"cargo_target_v1\",",
                "\"namespace_identity\":\"{}\",",
                "\"state_id\":\"cargo-target-main-rust-1-97\",",
                "\"materialized_generation_identity\":\"{}\",",
                "\"source\":{{\"repository\":\"teamleaderleo/glaeda\",",
                "\"commit\":\"{}\",\"tree\":\"{}\"}},",
                "\"canonical_reconstruction_inputs_digest\":\"{}\",",
                "\"reconstruction_plan_generation_digest\":\"{}\",",
                "\"validator_generation_digest\":\"{}\",",
                "\"toolchain_envelope_digest\":\"{}\",",
                "\"family_semantic_digest\":\"{}\",",
                "\"outcome\":\"equivalent\"}}"
            ),
            digest('a'),
            digest('b'),
            "1a".repeat(20),
            "2b".repeat(20),
            digest('c'),
            digest('d'),
            digest('e'),
            digest('f'),
            digest('0'),
        );
        assert_eq!(rendered, expected);
        for forbidden in ["/home/", "/tmp/", "target/", "command_output", "argv"] {
            assert!(!rendered.contains(forbidden));
        }
    }

    #[test]
    fn every_binding_dimension_participates_in_correlation() {
        let expected = binding();
        let mut mutations = Vec::new();

        let mut alternate = expected.clone();
        alternate.target.namespace_identity =
            ProtectedCacheNamespaceIdentity::parse(&digest('1')).unwrap();
        mutations.push(("namespace_identity", alternate));

        let mut alternate = expected.clone();
        alternate.target.state_id = CacheStateId::parse("cargo-target-other").unwrap();
        mutations.push(("state_id", alternate));

        let mut alternate = expected.clone();
        alternate.target.materialized_generation_identity =
            ProtectedCacheGenerationIdentity::parse(&digest('2')).unwrap();
        mutations.push(("materialized_generation_identity", alternate));

        let mut alternate = expected.clone();
        alternate.reconstruction.source.repository =
            RepositoryRef::parse("teamleaderleo/other").unwrap();
        mutations.push(("source.repository", alternate));

        let mut alternate = expected.clone();
        alternate.reconstruction.source.commit = CommitId::parse(&"3c".repeat(20)).unwrap();
        mutations.push(("source.commit", alternate));

        let mut alternate = expected.clone();
        alternate.reconstruction.source.tree = GitTreeId::parse(&"4d".repeat(20)).unwrap();
        mutations.push(("source.tree", alternate));

        let mut alternate = expected.clone();
        alternate.reconstruction.canonical_inputs_digest =
            Sha256Digest::parse(&digest('5')).unwrap();
        mutations.push(("canonical_reconstruction_inputs_digest", alternate));

        let mut alternate = expected.clone();
        alternate.reconstruction.plan_generation_digest =
            Sha256Digest::parse(&digest('6')).unwrap();
        mutations.push(("reconstruction_plan_generation_digest", alternate));

        let mut alternate = expected.clone();
        alternate.reconstruction.validator_generation_digest =
            Sha256Digest::parse(&digest('7')).unwrap();
        mutations.push(("validator_generation_digest", alternate));

        let mut alternate = expected.clone();
        alternate.reconstruction.toolchain_envelope_digest =
            Sha256Digest::parse(&digest('8')).unwrap();
        mutations.push(("toolchain_envelope_digest", alternate));

        let mut alternate = expected.clone();
        alternate.family_semantic_digest = Sha256Digest::parse(&digest('9')).unwrap();
        mutations.push(("family_semantic_digest", alternate));

        for (field, alternate) in mutations {
            assert_eq!(
                receipt().correlate(&alternate).unwrap_err().kind(),
                ProtectedCacheReplacementEquivalenceErrorKind::BindingMismatch,
                "field {field} must participate in correlation"
            );
        }
    }

    #[test]
    fn decoder_rejects_noncanonical_unknown_future_and_invalid_identity_documents() {
        let canonical = encode_protected_cache_replacement_equivalence_receipt(&receipt()).unwrap();
        let value: Value = serde_json::from_slice(&canonical).unwrap();

        let pretty = serde_json::to_vec_pretty(&value).unwrap();
        assert_eq!(
            decode_protected_cache_replacement_equivalence_receipt(&pretty)
                .unwrap_err()
                .kind(),
            ProtectedCacheReplacementEquivalenceErrorKind::NonCanonical
        );

        let mut unknown = value.clone();
        unknown["path"] = json!("/home/leo/Projects/glaeda/target");
        assert_eq!(
            decode_protected_cache_replacement_equivalence_receipt(
                &serde_json::to_vec(&unknown).unwrap()
            )
            .unwrap_err()
            .kind(),
            ProtectedCacheReplacementEquivalenceErrorKind::InvalidDocument
        );

        let mut future = value.clone();
        future["schema_version"] = json!(2);
        assert_eq!(
            decode_protected_cache_replacement_equivalence_receipt(
                &serde_json::to_vec(&future).unwrap()
            )
            .unwrap_err()
            .kind(),
            ProtectedCacheReplacementEquivalenceErrorKind::VersionIncompatible
        );

        let mut path = value;
        path["state_id"] = json!("target/private");
        assert_eq!(
            decode_protected_cache_replacement_equivalence_receipt(
                &serde_json::to_vec(&path).unwrap()
            )
            .unwrap_err()
            .kind(),
            ProtectedCacheReplacementEquivalenceErrorKind::CorruptReceipt
        );
    }

    #[test]
    fn decoder_rejects_fixed_vocabulary_drift_and_duplicate_keys() {
        let canonical = encode_protected_cache_replacement_equivalence_receipt(&receipt()).unwrap();
        let value: Value = serde_json::from_slice(&canonical).unwrap();
        for (field, replacement) in [
            ("receipt_type", "future_receipt_v2"),
            ("family", "future_family_v2"),
            ("outcome", "failed"),
        ] {
            let mut changed = value.clone();
            changed[field] = json!(replacement);
            assert_eq!(
                decode_protected_cache_replacement_equivalence_receipt(
                    &serde_json::to_vec(&changed).unwrap()
                )
                .unwrap_err()
                .kind(),
                ProtectedCacheReplacementEquivalenceErrorKind::InvalidDocument
            );
        }

        let duplicate =
            String::from_utf8(canonical)
                .unwrap()
                .replacen("{", "{\"schema_version\":1,", 1);
        assert_eq!(
            decode_protected_cache_replacement_equivalence_receipt(duplicate.as_bytes())
                .unwrap_err()
                .kind(),
            ProtectedCacheReplacementEquivalenceErrorKind::InvalidDocument
        );
    }

    #[test]
    fn decoder_rejects_empty_and_oversized_input_before_use() {
        assert_eq!(
            decode_protected_cache_replacement_equivalence_receipt(&[])
                .unwrap_err()
                .kind(),
            ProtectedCacheReplacementEquivalenceErrorKind::InvalidDocument
        );
        let oversized = vec![b' '; MAX_PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE_BYTES + 1];
        assert_eq!(
            decode_protected_cache_replacement_equivalence_receipt(&oversized)
                .unwrap_err()
                .kind(),
            ProtectedCacheReplacementEquivalenceErrorKind::DocumentTooLarge
        );
    }
}
