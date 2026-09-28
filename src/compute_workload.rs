//! Pure workload-family-neutral identity for declared compute.
//!
//! This module describes what useful computation is requested. It deliberately contains no
//! execution attempt, scheduler, process, path, command, persistence, backend, capacity, release,
//! or result-acceptance authority. Workload-family adapters own semantic input derivation and the
//! evidence rules for their output contracts.

/// Pure semantic currentness and result precedence, independent from physical settlement.
pub mod supersession;

use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;

pub const COMPUTE_WORKLOAD_SCHEMA_VERSION: u8 = 1;
pub const MAX_COMPUTE_CAPABILITIES: usize = 32;
pub const MAX_COMPUTE_SEMANTIC_GENERATION: u64 = 1_000_000_000_000;

const MAX_COMPUTE_IDENTIFIER_BYTES: usize = 96;

macro_rules! identifier_type {
    ($name:ident, $field:literal, $code:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded public compute identifier.
            ///
            /// # Errors
            ///
            /// Returns a bounded error for empty, oversized, path-shaped, command-shaped, or
            /// otherwise non-canonical text.
            pub fn parse(value: &str) -> Result<Self, ComputeWorkloadError> {
                validate_identifier($field, $code, value)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier_type!(
    ComputeWorkloadFamilyId,
    "family",
    "invalid_compute_workload_family"
);
identifier_type!(
    ComputeCapabilityId,
    "required_capabilities",
    "invalid_compute_capability"
);

/// One family-owned exact identity for all semantic inputs controlling a workload result.
///
/// The generic compute layer treats the digest as opaque. A repository adapter may bind source,
/// profile, and command identities; a dataset adapter may bind dataset, transform, and runtime
/// generations; another family may define a different exact input contract.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ComputeInputIdentity(Sha256Digest);

impl ComputeInputIdentity {
    #[must_use]
    pub const fn new(digest: Sha256Digest) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

/// Exact family-owned identity of the contract that decides whether produced output is accepted.
///
/// Possessing this identity grants no result authority. The workload-family adapter still owns
/// validation and interpretation of concrete output/evidence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ComputeOutputContractIdentity(Sha256Digest);

impl ComputeOutputContractIdentity {
    #[must_use]
    pub const fn new(digest: Sha256Digest) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ComputeSemanticGeneration(u64);

impl ComputeSemanticGeneration {
    pub fn new(value: u64) -> Result<Self, ComputeWorkloadError> {
        if !(1..=MAX_COMPUTE_SEMANTIC_GENERATION).contains(&value) {
            return Err(ComputeWorkloadError::new(
                "semantic_generation",
                "invalid_compute_semantic_generation",
                "compute semantic generation must be within the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Trust controls which separately authorized execution capabilities and residency policies may be
/// considered for a workload. This value itself grants none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeTrustClass {
    HostileUnknown,
    Trusted,
    UltraTrusted,
}

/// Canonical, bounded capability names required before a workload can be considered executable.
///
/// Capability names are declarative equality keys only. They contain no paths, commands, tokens,
/// environment values, device handles, or backend authority.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ComputeCapabilitySet(Vec<ComputeCapabilityId>);

impl ComputeCapabilitySet {
    pub fn new(capabilities: Vec<ComputeCapabilityId>) -> Result<Self, ComputeWorkloadError> {
        if capabilities.len() > MAX_COMPUTE_CAPABILITIES {
            return Err(ComputeWorkloadError::new(
                "required_capabilities",
                "too_many_compute_capabilities",
                "compute capability set exceeds the bounded maximum",
            ));
        }

        let mut canonical = capabilities;
        canonical.sort();
        if canonical.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ComputeWorkloadError::new(
                "required_capabilities",
                "duplicate_compute_capability",
                "compute capability set cannot contain duplicates",
            ));
        }
        Ok(Self(canonical))
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    #[must_use]
    pub fn as_slice(&self) -> &[ComputeCapabilityId] {
        &self.0
    }

    #[must_use]
    pub fn contains(&self, capability: &ComputeCapabilityId) -> bool {
        self.0.binary_search(capability).is_ok()
    }
}

/// Exact semantic identity of one requested compute workload.
///
/// This is intentionally independent of execution-attempt identity. Retries of unchanged semantic
/// work may share this identity while acquiring distinct attempt generations, capacity claims,
/// backends, and settlement state in their owning execution layers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ComputeWorkloadIdentity {
    schema_version: u8,
    family: ComputeWorkloadFamilyId,
    semantic_generation: ComputeSemanticGeneration,
    input_identity: ComputeInputIdentity,
    trust_class: ComputeTrustClass,
    required_capabilities: ComputeCapabilitySet,
    output_contract: ComputeOutputContractIdentity,
}

impl ComputeWorkloadIdentity {
    #[must_use]
    pub const fn new(
        family: ComputeWorkloadFamilyId,
        semantic_generation: ComputeSemanticGeneration,
        input_identity: ComputeInputIdentity,
        trust_class: ComputeTrustClass,
        required_capabilities: ComputeCapabilitySet,
        output_contract: ComputeOutputContractIdentity,
    ) -> Self {
        Self {
            schema_version: COMPUTE_WORKLOAD_SCHEMA_VERSION,
            family,
            semantic_generation,
            input_identity,
            trust_class,
            required_capabilities,
            output_contract,
        }
    }

    #[must_use]
    pub const fn family(&self) -> &ComputeWorkloadFamilyId {
        &self.family
    }

    #[must_use]
    pub const fn semantic_generation(&self) -> ComputeSemanticGeneration {
        self.semantic_generation
    }

    #[must_use]
    pub const fn input_identity(&self) -> &ComputeInputIdentity {
        &self.input_identity
    }

    #[must_use]
    pub const fn trust_class(&self) -> ComputeTrustClass {
        self.trust_class
    }

    #[must_use]
    pub const fn required_capabilities(&self) -> &ComputeCapabilitySet {
        &self.required_capabilities
    }

    #[must_use]
    pub const fn output_contract(&self) -> &ComputeOutputContractIdentity {
        &self.output_contract
    }
}

fn validate_identifier(
    field: &'static str,
    code: &'static str,
    value: &str,
) -> Result<(), ComputeWorkloadError> {
    let bytes = value.as_bytes();
    let edge_is_valid = bytes
        .first()
        .zip(bytes.last())
        .is_some_and(|(first, last)| is_lower_alphanumeric(*first) && is_lower_alphanumeric(*last));
    let body_is_valid = bytes
        .iter()
        .copied()
        .all(|byte| is_lower_alphanumeric(byte) || matches!(byte, b'.' | b'_' | b'-'));

    if value.is_empty()
        || value.len() > MAX_COMPUTE_IDENTIFIER_BYTES
        || !edge_is_valid
        || !body_is_valid
    {
        return Err(ComputeWorkloadError::new(
            field,
            code,
            "compute identifier must be bounded lowercase ASCII using letters, digits, '.', '_', or '-' with alphanumeric edges",
        ));
    }
    Ok(())
}

const fn is_lower_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ComputeWorkloadError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl ComputeWorkloadError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ComputeWorkloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ComputeWorkloadError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(hex: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn family(value: &str) -> ComputeWorkloadFamilyId {
        ComputeWorkloadFamilyId::parse(value).unwrap()
    }

    fn capability(value: &str) -> ComputeCapabilityId {
        ComputeCapabilityId::parse(value).unwrap()
    }

    fn generation(value: u64) -> ComputeSemanticGeneration {
        ComputeSemanticGeneration::new(value).unwrap()
    }

    fn workload(
        family_id: &str,
        generation_value: u64,
        input_hex: char,
        trust_class: ComputeTrustClass,
        capabilities: &[&str],
        output_hex: char,
    ) -> ComputeWorkloadIdentity {
        ComputeWorkloadIdentity::new(
            family(family_id),
            generation(generation_value),
            ComputeInputIdentity::new(digest(input_hex)),
            trust_class,
            ComputeCapabilitySet::new(capabilities.iter().map(|value| capability(value)).collect())
                .unwrap(),
            ComputeOutputContractIdentity::new(digest(output_hex)),
        )
    }

    #[test]
    fn identifiers_are_bounded_public_equality_keys() {
        for valid in [
            "repository_verification.v1",
            "dataset_transform.v1",
            "linux.arm64",
            "gpu.metal",
            "storage-local",
        ] {
            assert_eq!(
                ComputeWorkloadFamilyId::parse(valid).unwrap().as_str(),
                valid
            );
        }

        for invalid in [
            "",
            ".leading",
            "trailing.",
            "/private/state",
            "cargo test",
            "https://example.invalid",
            "UPPER",
            "gpu:metal",
        ] {
            assert_eq!(
                ComputeWorkloadFamilyId::parse(invalid).unwrap_err().code(),
                "invalid_compute_workload_family"
            );
        }
    }

    #[test]
    fn capability_sets_are_canonical_bounded_and_duplicate_free() {
        let set = ComputeCapabilitySet::new(vec![
            capability("storage.local"),
            capability("linux.arm64"),
            capability("cpu.general"),
        ])
        .unwrap();
        assert_eq!(
            set.as_slice()
                .iter()
                .map(ComputeCapabilityId::as_str)
                .collect::<Vec<_>>(),
            vec!["cpu.general", "linux.arm64", "storage.local"]
        );
        assert!(set.contains(&capability("linux.arm64")));

        assert_eq!(
            ComputeCapabilitySet::new(vec![capability("linux"), capability("linux")])
                .unwrap_err()
                .code(),
            "duplicate_compute_capability"
        );
        assert_eq!(
            ComputeCapabilitySet::new(
                (0..=MAX_COMPUTE_CAPABILITIES)
                    .map(|index| capability(&format!("capability{index}")))
                    .collect(),
            )
            .unwrap_err()
            .code(),
            "too_many_compute_capabilities"
        );
    }

    #[test]
    fn repository_and_dataset_workloads_share_one_generic_contract() {
        let repository = workload(
            "repository_verification.v1",
            7,
            'a',
            ComputeTrustClass::UltraTrusted,
            &["linux.arm64", "toolchain.rust"],
            'b',
        );
        let dataset = workload(
            "dataset_transform.v1",
            3,
            'c',
            ComputeTrustClass::Trusted,
            &["linux.arm64", "storage.local"],
            'd',
        );

        assert_eq!(repository.family().as_str(), "repository_verification.v1");
        assert_eq!(dataset.family().as_str(), "dataset_transform.v1");
        assert_ne!(repository, dataset);
        assert_eq!(repository.semantic_generation().get(), 7);
        assert_eq!(dataset.semantic_generation().get(), 3);
        assert_eq!(
            repository.input_identity().digest().as_str(),
            digest('a').as_str()
        );
        assert_eq!(
            dataset.output_contract().digest().as_str(),
            digest('d').as_str()
        );
    }

    #[test]
    fn each_semantic_axis_changes_workload_identity() {
        let base = workload(
            "dataset_transform.v1",
            1,
            '1',
            ComputeTrustClass::Trusted,
            &["linux.arm64"],
            '2',
        );
        let variants = [
            workload(
                "dataset_transform.v2",
                1,
                '1',
                ComputeTrustClass::Trusted,
                &["linux.arm64"],
                '2',
            ),
            workload(
                "dataset_transform.v1",
                2,
                '1',
                ComputeTrustClass::Trusted,
                &["linux.arm64"],
                '2',
            ),
            workload(
                "dataset_transform.v1",
                1,
                '3',
                ComputeTrustClass::Trusted,
                &["linux.arm64"],
                '2',
            ),
            workload(
                "dataset_transform.v1",
                1,
                '1',
                ComputeTrustClass::UltraTrusted,
                &["linux.arm64"],
                '2',
            ),
            workload(
                "dataset_transform.v1",
                1,
                '1',
                ComputeTrustClass::Trusted,
                &["linux.arm64", "storage.local"],
                '2',
            ),
            workload(
                "dataset_transform.v1",
                1,
                '1',
                ComputeTrustClass::Trusted,
                &["linux.arm64"],
                '4',
            ),
        ];

        for variant in variants {
            assert_ne!(base, variant);
        }
    }

    #[test]
    fn public_json_is_stable_and_carries_no_executable_or_private_surface() {
        let identity = workload(
            "dataset_transform.v1",
            9,
            'a',
            ComputeTrustClass::Trusted,
            &["storage.local", "linux.arm64"],
            'b',
        );
        let json = serde_json::to_string(&identity).unwrap();
        assert_eq!(
            json,
            format!(
                "{{\"schema_version\":1,\"family\":\"dataset_transform.v1\",\"semantic_generation\":9,\"input_identity\":\"{}\",\"trust_class\":\"trusted\",\"required_capabilities\":[\"linux.arm64\",\"storage.local\"],\"output_contract\":\"{}\"}}",
                digest('a').as_str(),
                digest('b').as_str()
            )
        );

        for forbidden in [
            "/private/",
            "cargo test",
            "command",
            "environment",
            "token",
            "credential",
            "process",
            "spawn",
            "kill",
            "release",
            "persist",
            "backend",
        ] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn semantic_generation_is_bounded_and_attempt_identity_is_absent() {
        assert_eq!(
            ComputeSemanticGeneration::new(0).unwrap_err().code(),
            "invalid_compute_semantic_generation"
        );
        assert_eq!(
            ComputeSemanticGeneration::new(MAX_COMPUTE_SEMANTIC_GENERATION + 1)
                .unwrap_err()
                .code(),
            "invalid_compute_semantic_generation"
        );

        let value = serde_json::to_value(workload(
            "repository_verification.v1",
            1,
            'a',
            ComputeTrustClass::HostileUnknown,
            &[],
            'b',
        ))
        .unwrap();
        let object = value
            .as_object()
            .expect("workload identity must be an object");
        for absent in ["attempt", "reservation", "host", "runner", "pid"] {
            assert!(!object.contains_key(absent));
        }
    }
}
