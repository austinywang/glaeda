//! Pure versioned digest vocabulary for project/workspace observation identities.
//!
//! The existing project checkout, discovery, and trusted-workspace observers historically used
//! SmolRunner-branded domain separators. Those bytes are evidence: changing a separator in place
//! would make the same host facts claim a different identity under the same generation.
//!
//! This module centralizes the two hashing forms used by those observers and makes the generation
//! explicit. It performs no filesystem observation, persistence, process execution, cleanup, or
//! adoption. A digest returned here is equality data only and grants zero authority over the
//! filesystem facts that a caller supplied.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;

const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

const SMOLRUNNER_PROJECT_MATERIALIZATION_V1: &[u8] = b"smolrunner-project-materialization-v1\0";
const GLAEDA_PROJECT_MATERIALIZATION_V2: &[u8] = b"glaeda-project-materialization-v2\0";
const SMOLRUNNER_PROJECT_DISCOVERY_ROOT_V1: &[u8] = b"smolrunner-project-discovery-root-v1\0";
const GLAEDA_PROJECT_DISCOVERY_ROOT_V2: &[u8] = b"glaeda-project-discovery-root-v2\0";
const SMOLRUNNER_PROJECT_DISCOVERY_ENTRY_V1: &[u8] = b"smolrunner-project-discovery-entry-v1\0";
const GLAEDA_PROJECT_DISCOVERY_ENTRY_V2: &[u8] = b"glaeda-project-discovery-entry-v2\0";

const SMOLRUNNER_TRUSTED_WORKSPACE_ID_V1: &[u8] = b"smolrunner-trusted-workspace-id-v1";
const GLAEDA_TRUSTED_WORKSPACE_ID_V2: &[u8] = b"glaeda-trusted-workspace-id-v2";
const SMOLRUNNER_TRUSTED_CACHE_NAMESPACE_V1: &[u8] = b"smolrunner-trusted-cache-namespace-v1";
const GLAEDA_TRUSTED_CACHE_NAMESPACE_V2: &[u8] = b"glaeda-trusted-cache-namespace-v2";
const SMOLRUNNER_TRUSTED_WORKSPACE_EVIDENCE_V1: &[u8] = b"smolrunner-trusted-workspace-evidence-v1";
const GLAEDA_TRUSTED_WORKSPACE_EVIDENCE_V2: &[u8] = b"glaeda-trusted-workspace-evidence-v2";

/// Closed generation selector for the project/workspace identity family.
///
/// Fresh Glaeda observation code should use [`Self::CURRENT`]. The legacy generation exists only so
/// old exact evidence can still be reproduced or compared deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectWorkspaceIdentityGeneration {
    SmolrunnerV1,
    GlaedaV2,
}

impl ProjectWorkspaceIdentityGeneration {
    pub const CURRENT: Self = Self::GlaedaV2;
}

/// Filesystem-object identity purpose using the historical fixed-width host-fact encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectWorkspaceFilesystemIdentityKind {
    Materialization,
    DiscoveryRoot,
    DiscoveryEntry,
}

/// Trusted-workspace digest purpose using the historical length-prefixed field encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedWorkspaceIdentityKind {
    WorkspaceId,
    CacheNamespace,
    Evidence,
}

/// Compute one project/workspace filesystem identity from already-observed opaque host facts.
///
/// The byte encoding intentionally matches the current v1 checkout/discovery implementations:
///
/// ```text
/// domain || device:u64-be || inode:u64-be || owner:u32-be
/// ```
///
/// The function observes nothing and the result carries no filesystem capability.
///
/// # Errors
///
/// Returns a bounded error only if the canonical SHA-256 text cannot be represented by the shared
/// digest type.
pub fn project_workspace_filesystem_identity(
    generation: ProjectWorkspaceIdentityGeneration,
    kind: ProjectWorkspaceFilesystemIdentityKind,
    device: u64,
    inode: u64,
    owner: u32,
) -> Result<Sha256Digest, ProjectWorkspaceIdentityError> {
    let mut hasher = Sha256::new();
    hasher.update(filesystem_domain(generation, kind));
    hasher.update(device.to_be_bytes());
    hasher.update(inode.to_be_bytes());
    hasher.update(owner.to_be_bytes());
    parse_digest(hasher.finalize().as_slice())
}

/// Compute one trusted-workspace identity from already-validated bounded field bytes.
///
/// The byte encoding intentionally matches `trusted_workspace_receipt` v1:
///
/// ```text
/// domain || 0x00 || repeated(field_length:u64-be || field_bytes)
/// ```
///
/// Callers retain responsibility for deciding which typed fields belong to each identity. This
/// helper only closes the generation/domain separator and common canonical encoding.
///
/// # Errors
///
/// Returns a bounded error only if the canonical SHA-256 text cannot be represented by the shared
/// digest type.
pub fn trusted_workspace_identity<'a>(
    generation: ProjectWorkspaceIdentityGeneration,
    kind: TrustedWorkspaceIdentityKind,
    fields: impl IntoIterator<Item = &'a [u8]>,
) -> Result<Sha256Digest, ProjectWorkspaceIdentityError> {
    let mut hasher = Sha256::new();
    hasher.update(trusted_workspace_domain(generation, kind));
    hasher.update([0]);
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    parse_digest(hasher.finalize().as_slice())
}

const fn filesystem_domain(
    generation: ProjectWorkspaceIdentityGeneration,
    kind: ProjectWorkspaceFilesystemIdentityKind,
) -> &'static [u8] {
    match (generation, kind) {
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            ProjectWorkspaceFilesystemIdentityKind::Materialization,
        ) => SMOLRUNNER_PROJECT_MATERIALIZATION_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            ProjectWorkspaceFilesystemIdentityKind::Materialization,
        ) => GLAEDA_PROJECT_MATERIALIZATION_V2,
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryRoot,
        ) => SMOLRUNNER_PROJECT_DISCOVERY_ROOT_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryRoot,
        ) => GLAEDA_PROJECT_DISCOVERY_ROOT_V2,
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryEntry,
        ) => SMOLRUNNER_PROJECT_DISCOVERY_ENTRY_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryEntry,
        ) => GLAEDA_PROJECT_DISCOVERY_ENTRY_V2,
    }
}

const fn trusted_workspace_domain(
    generation: ProjectWorkspaceIdentityGeneration,
    kind: TrustedWorkspaceIdentityKind,
) -> &'static [u8] {
    match (generation, kind) {
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            TrustedWorkspaceIdentityKind::WorkspaceId,
        ) => SMOLRUNNER_TRUSTED_WORKSPACE_ID_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            TrustedWorkspaceIdentityKind::WorkspaceId,
        ) => GLAEDA_TRUSTED_WORKSPACE_ID_V2,
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            TrustedWorkspaceIdentityKind::CacheNamespace,
        ) => SMOLRUNNER_TRUSTED_CACHE_NAMESPACE_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            TrustedWorkspaceIdentityKind::CacheNamespace,
        ) => GLAEDA_TRUSTED_CACHE_NAMESPACE_V2,
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            TrustedWorkspaceIdentityKind::Evidence,
        ) => SMOLRUNNER_TRUSTED_WORKSPACE_EVIDENCE_V1,
        (ProjectWorkspaceIdentityGeneration::GlaedaV2, TrustedWorkspaceIdentityKind::Evidence) => {
            GLAEDA_TRUSTED_WORKSPACE_EVIDENCE_V2
        }
    }
}

fn parse_digest(bytes: &[u8]) -> Result<Sha256Digest, ProjectWorkspaceIdentityError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| ProjectWorkspaceIdentityError)
}

/// Bounded construction error for the pure identity helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectWorkspaceIdentityError;

impl fmt::Display for ProjectWorkspaceIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("project/workspace identity could not be represented")
    }
}

impl std::error::Error for ProjectWorkspaceIdentityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_generations_have_pinned_distinct_vectors() {
        let cases = [
            (
                ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
                ProjectWorkspaceFilesystemIdentityKind::Materialization,
                "sha256:f10970c34ab915d6efe4c0d873315a5f5ab342779c86446908624abfd3512af0",
            ),
            (
                ProjectWorkspaceIdentityGeneration::GlaedaV2,
                ProjectWorkspaceFilesystemIdentityKind::Materialization,
                "sha256:b346ce27b7ae2b9c25f1ef32fa97a6e4066128e5a1a0a053670c7cc9c2bc99a5",
            ),
            (
                ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
                ProjectWorkspaceFilesystemIdentityKind::DiscoveryRoot,
                "sha256:af0a45d9c94d6aaf81f9664157d2b54a1e1c3a91a8f050684473f881b81c4478",
            ),
            (
                ProjectWorkspaceIdentityGeneration::GlaedaV2,
                ProjectWorkspaceFilesystemIdentityKind::DiscoveryRoot,
                "sha256:c3c5c8de19fffd26d694c800706d2642fc260d6ad906ecdbde80e0d01ea29c21",
            ),
            (
                ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
                ProjectWorkspaceFilesystemIdentityKind::DiscoveryEntry,
                "sha256:a4e6d36a91600b117838c39182dfe07d2e07b1722f3b46c23a6f5355372ffa14",
            ),
            (
                ProjectWorkspaceIdentityGeneration::GlaedaV2,
                ProjectWorkspaceFilesystemIdentityKind::DiscoveryEntry,
                "sha256:7546cbb28f97ed65e92df6c1374ffab1d10e2734d79dc3452d618e5628e66ebf",
            ),
        ];

        for (generation, kind, expected) in cases {
            let actual = project_workspace_filesystem_identity(generation, kind, 7, 11, 1_000)
                .expect("identity");
            assert_eq!(actual.as_str(), expected);
        }
    }

    #[test]
    fn trusted_workspace_generations_have_pinned_distinct_vectors() {
        let fields = [b"installation".as_slice(), b"workspace".as_slice()];
        let cases = [
            (
                ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
                TrustedWorkspaceIdentityKind::WorkspaceId,
                "sha256:93da9898e5ed3199d31cd4df522cbaae878f9be276dbe6ede84fbfffc6d04bf3",
            ),
            (
                ProjectWorkspaceIdentityGeneration::GlaedaV2,
                TrustedWorkspaceIdentityKind::WorkspaceId,
                "sha256:6c1c8ff7f4a84d67d198f49df5c6a4114aae151c31e5f8c776be922af700f5aa",
            ),
            (
                ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
                TrustedWorkspaceIdentityKind::CacheNamespace,
                "sha256:7d123fadd24c99e594d6e1c3235877ec1a49f29b3f51b516e8630e587bea182a",
            ),
            (
                ProjectWorkspaceIdentityGeneration::GlaedaV2,
                TrustedWorkspaceIdentityKind::CacheNamespace,
                "sha256:a8f900c3ab6fc3d939950631d1454fa00640a3d2912a506fcd79bf14a4fac6f1",
            ),
            (
                ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
                TrustedWorkspaceIdentityKind::Evidence,
                "sha256:5a07faa8a85423b12eb97c4ae28fc4a7f2018fe8b83e2108f46352383dd61282",
            ),
            (
                ProjectWorkspaceIdentityGeneration::GlaedaV2,
                TrustedWorkspaceIdentityKind::Evidence,
                "sha256:d78ce94e6786b5a0c9da4885d93f4d15293ff7d1ca3fc48f80e8dd343c717932",
            ),
        ];

        for (generation, kind, expected) in cases {
            let actual = trusted_workspace_identity(generation, kind, fields).expect("identity");
            assert_eq!(actual.as_str(), expected);
        }
    }
}
