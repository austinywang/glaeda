//! Pure generation/domain vocabulary for the personal-worker Linux runtime closure.
//!
//! The accepted SmolRunner-v1 runtime evidence spans seven digest domains. Fresh Glaeda runtime
//! evidence must move those domains together so one sealed readiness identity cannot silently mix
//! old and current class evidence.
//!
//! This module hashes no runtime evidence and performs no host observation, package inspection,
//! install, execution, persistence, or cleanup. A domain policy grants zero runtime authority.

use serde::Serialize;

pub const PERSONAL_WORKER_RUNTIME_IDENTITY_GENERATION_SCHEMA_VERSION: u8 = 1;

const SMOLRUNNER_READINESS_V1: &[u8] = b"smolrunner-personal-worker-runtime-readiness-v1";
const GLAEDA_READINESS_V2: &[u8] = b"glaeda-personal-worker-runtime-readiness-v2";
const SMOLRUNNER_REQUIRED_POLICY_V1: &[u8] =
    b"smolrunner-personal-worker-runtime-required-policy-v1";
const GLAEDA_REQUIRED_POLICY_V2: &[u8] = b"glaeda-personal-worker-runtime-required-policy-v2";
const SMOLRUNNER_ACCOUNT_EVIDENCE_V1: &[u8] =
    b"smolrunner-personal-worker-runtime-account-evidence-v1";
const GLAEDA_ACCOUNT_EVIDENCE_V2: &[u8] = b"glaeda-personal-worker-runtime-account-evidence-v2";
const SMOLRUNNER_KERNEL_PREREQUISITE_V1: &[u8] =
    b"smolrunner-personal-worker-runtime-kernel-prerequisite-v1";
const GLAEDA_KERNEL_PREREQUISITE_V2: &[u8] =
    b"glaeda-personal-worker-runtime-kernel-prerequisite-v2";
const SMOLRUNNER_EXECUTABLE_PREREQUISITE_V1: &[u8] =
    b"smolrunner-personal-worker-runtime-executable-prerequisite-v1";
const GLAEDA_EXECUTABLE_PREREQUISITE_V2: &[u8] =
    b"glaeda-personal-worker-runtime-executable-prerequisite-v2";
const SMOLRUNNER_LOADER_OBJECT_PREREQUISITE_V1: &[u8] =
    b"smolrunner-personal-worker-runtime-loader-object-prerequisite-v1";
const GLAEDA_LOADER_OBJECT_PREREQUISITE_V2: &[u8] =
    b"glaeda-personal-worker-runtime-loader-object-prerequisite-v2";
const SMOLRUNNER_LOADER_STATE_PREREQUISITE_V1: &[u8] =
    b"smolrunner-personal-worker-runtime-loader-state-prerequisite-v1";
const GLAEDA_LOADER_STATE_PREREQUISITE_V2: &[u8] =
    b"glaeda-personal-worker-runtime-loader-state-prerequisite-v2";

/// Closed semantic generation for every identity entering one runtime closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeIdentityGeneration {
    SmolrunnerV1,
    GlaedaV2,
}

impl PersonalWorkerRuntimeIdentityGeneration {
    pub const CURRENT: Self = Self::GlaedaV2;

    #[must_use]
    pub const fn is_legacy(self) -> bool {
        matches!(self, Self::SmolrunnerV1)
    }
}

/// Every currently audited digest class in the sealed personal-worker runtime closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeIdentityDomainKind {
    Readiness,
    RequiredPolicy,
    AccountEvidence,
    KernelPrerequisite,
    ExecutablePrerequisite,
    LoaderObjectPrerequisite,
    LoaderStatePrerequisite,
}

impl PersonalWorkerRuntimeIdentityDomainKind {
    pub const ALL: [Self; 7] = [
        Self::Readiness,
        Self::RequiredPolicy,
        Self::AccountEvidence,
        Self::KernelPrerequisite,
        Self::ExecutablePrerequisite,
        Self::LoaderObjectPrerequisite,
        Self::LoaderStatePrerequisite,
    ];
}

/// Exact generation + domain-kind pairing for one runtime identity computation.
///
/// Raw domain bytes are deliberately omitted from serialization. The later runtime evidence types
/// should carry the semantic generation they belong to, while hashing code obtains bytes through
/// this closed policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeIdentityDomainPolicy {
    schema_version: u8,
    generation: PersonalWorkerRuntimeIdentityGeneration,
    kind: PersonalWorkerRuntimeIdentityDomainKind,
}

impl PersonalWorkerRuntimeIdentityDomainPolicy {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn generation(self) -> PersonalWorkerRuntimeIdentityGeneration {
        self.generation
    }

    #[must_use]
    pub const fn kind(self) -> PersonalWorkerRuntimeIdentityDomainKind {
        self.kind
    }

    #[must_use]
    pub const fn domain(self) -> &'static [u8] {
        match (self.generation, self.kind) {
            (
                PersonalWorkerRuntimeIdentityGeneration::SmolrunnerV1,
                PersonalWorkerRuntimeIdentityDomainKind::Readiness,
            ) => SMOLRUNNER_READINESS_V1,
            (
                PersonalWorkerRuntimeIdentityGeneration::GlaedaV2,
                PersonalWorkerRuntimeIdentityDomainKind::Readiness,
            ) => GLAEDA_READINESS_V2,
            (
                PersonalWorkerRuntimeIdentityGeneration::SmolrunnerV1,
                PersonalWorkerRuntimeIdentityDomainKind::RequiredPolicy,
            ) => SMOLRUNNER_REQUIRED_POLICY_V1,
            (
                PersonalWorkerRuntimeIdentityGeneration::GlaedaV2,
                PersonalWorkerRuntimeIdentityDomainKind::RequiredPolicy,
            ) => GLAEDA_REQUIRED_POLICY_V2,
            (
                PersonalWorkerRuntimeIdentityGeneration::SmolrunnerV1,
                PersonalWorkerRuntimeIdentityDomainKind::AccountEvidence,
            ) => SMOLRUNNER_ACCOUNT_EVIDENCE_V1,
            (
                PersonalWorkerRuntimeIdentityGeneration::GlaedaV2,
                PersonalWorkerRuntimeIdentityDomainKind::AccountEvidence,
            ) => GLAEDA_ACCOUNT_EVIDENCE_V2,
            (
                PersonalWorkerRuntimeIdentityGeneration::SmolrunnerV1,
                PersonalWorkerRuntimeIdentityDomainKind::KernelPrerequisite,
            ) => SMOLRUNNER_KERNEL_PREREQUISITE_V1,
            (
                PersonalWorkerRuntimeIdentityGeneration::GlaedaV2,
                PersonalWorkerRuntimeIdentityDomainKind::KernelPrerequisite,
            ) => GLAEDA_KERNEL_PREREQUISITE_V2,
            (
                PersonalWorkerRuntimeIdentityGeneration::SmolrunnerV1,
                PersonalWorkerRuntimeIdentityDomainKind::ExecutablePrerequisite,
            ) => SMOLRUNNER_EXECUTABLE_PREREQUISITE_V1,
            (
                PersonalWorkerRuntimeIdentityGeneration::GlaedaV2,
                PersonalWorkerRuntimeIdentityDomainKind::ExecutablePrerequisite,
            ) => GLAEDA_EXECUTABLE_PREREQUISITE_V2,
            (
                PersonalWorkerRuntimeIdentityGeneration::SmolrunnerV1,
                PersonalWorkerRuntimeIdentityDomainKind::LoaderObjectPrerequisite,
            ) => SMOLRUNNER_LOADER_OBJECT_PREREQUISITE_V1,
            (
                PersonalWorkerRuntimeIdentityGeneration::GlaedaV2,
                PersonalWorkerRuntimeIdentityDomainKind::LoaderObjectPrerequisite,
            ) => GLAEDA_LOADER_OBJECT_PREREQUISITE_V2,
            (
                PersonalWorkerRuntimeIdentityGeneration::SmolrunnerV1,
                PersonalWorkerRuntimeIdentityDomainKind::LoaderStatePrerequisite,
            ) => SMOLRUNNER_LOADER_STATE_PREREQUISITE_V1,
            (
                PersonalWorkerRuntimeIdentityGeneration::GlaedaV2,
                PersonalWorkerRuntimeIdentityDomainKind::LoaderStatePrerequisite,
            ) => GLAEDA_LOADER_STATE_PREREQUISITE_V2,
        }
    }
}

/// Select one exact domain policy from closed generation + class vocabulary.
#[must_use]
pub const fn personal_worker_runtime_identity_domain_policy(
    generation: PersonalWorkerRuntimeIdentityGeneration,
    kind: PersonalWorkerRuntimeIdentityDomainKind,
) -> PersonalWorkerRuntimeIdentityDomainPolicy {
    PersonalWorkerRuntimeIdentityDomainPolicy {
        schema_version: PERSONAL_WORKER_RUNTIME_IDENTITY_GENERATION_SCHEMA_VERSION,
        generation,
        kind,
    }
}
