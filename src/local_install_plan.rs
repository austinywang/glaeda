use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::{CommitId, GitTreeId, Sha256Digest};

pub const LOCAL_INSTALL_PLAN_SCHEMA_VERSION: u8 = 2;
pub const MAX_RETAINED_LOCAL_INSTALL_GENERATIONS: usize = 1;
pub const MAX_LOCAL_INSTALL_TOOLCHAIN_BYTES: usize = 128;
pub const MAX_LOCAL_INSTALL_VERSION_BYTES: usize = 128;
pub const MAX_LAUNCHER_OBSERVATIONS: usize = 4;

const SMOLRUNNER_SOURCE_DIGEST_DOMAIN_V1: &[u8] = b"smolrunner-local-install-source-v1\0";
const GLAEDA_SOURCE_DIGEST_DOMAIN_V2: &[u8] = b"glaeda-local-install-source-v2\0";
const SMOLRUNNER_GENERATION_DIGEST_DOMAIN_V1: &[u8] = b"smolrunner-local-install-generation-v1\0";
const GLAEDA_GENERATION_DIGEST_DOMAIN_V2: &[u8] = b"glaeda-local-install-generation-v2\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Closed identity generation for local self-install source, build, and installed evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallIdentityGeneration {
    SmolrunnerV1,
    GlaedaV2,
}

impl LocalInstallIdentityGeneration {
    pub const CURRENT: Self = Self::GlaedaV2;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct LocalInstallToolchainIdentity(String);

impl LocalInstallToolchainIdentity {
    /// Parse one bounded toolchain identity used to bind a local binary build.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is bounded ASCII using the reviewed token vocabulary.
    pub fn parse(value: &str) -> Result<Self, LocalInstallPlanError> {
        if value.is_empty()
            || value.len() > MAX_LOCAL_INSTALL_TOOLCHAIN_BYTES
            || !value.is_ascii()
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
            })
        {
            return Err(error(
                LocalInstallPlanErrorKind::InvalidToolchain,
                "invalid_toolchain",
                "local install toolchain identity is invalid",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallSourceIdentity {
    identity_generation: LocalInstallIdentityGeneration,
    commit: CommitId,
    tree: GitTreeId,
    cargo_lock_digest: Sha256Digest,
    toolchain: LocalInstallToolchainIdentity,
    digest: Sha256Digest,
}

impl LocalInstallSourceIdentity {
    /// Bind exact checkout, lockfile, and toolchain evidence to one canonical source identity.
    ///
    /// # Errors
    ///
    /// Returns an error only when canonical identity encoding fails.
    pub fn new(
        commit: CommitId,
        tree: GitTreeId,
        cargo_lock_digest: Sha256Digest,
        toolchain: LocalInstallToolchainIdentity,
    ) -> Result<Self, LocalInstallPlanError> {
        Self::with_identity_generation(
            LocalInstallIdentityGeneration::CURRENT,
            commit,
            tree,
            cargo_lock_digest,
            toolchain,
        )
    }

    /// Reproduce one exact retained local-install source identity generation.
    ///
    /// Fresh planning uses [`Self::new`]. The legacy generation exists only for exact historical
    /// recovery evidence and cannot be inferred from a repository name or existing digest.
    ///
    /// # Errors
    ///
    /// Returns an error only when canonical identity encoding fails.
    pub fn with_identity_generation(
        identity_generation: LocalInstallIdentityGeneration,
        commit: CommitId,
        tree: GitTreeId,
        cargo_lock_digest: Sha256Digest,
        toolchain: LocalInstallToolchainIdentity,
    ) -> Result<Self, LocalInstallPlanError> {
        #[derive(Serialize)]
        struct LegacyDocument<'a> {
            commit: &'a CommitId,
            tree: &'a GitTreeId,
            cargo_lock_digest: &'a Sha256Digest,
            toolchain: &'a LocalInstallToolchainIdentity,
        }
        #[derive(Serialize)]
        struct CurrentDocument<'a> {
            schema_version: u8,
            identity_generation: LocalInstallIdentityGeneration,
            commit: &'a CommitId,
            tree: &'a GitTreeId,
            cargo_lock_digest: &'a Sha256Digest,
            toolchain: &'a LocalInstallToolchainIdentity,
        }
        let digest = match identity_generation {
            LocalInstallIdentityGeneration::SmolrunnerV1 => canonical_digest(
                SMOLRUNNER_SOURCE_DIGEST_DOMAIN_V1,
                &LegacyDocument {
                    commit: &commit,
                    tree: &tree,
                    cargo_lock_digest: &cargo_lock_digest,
                    toolchain: &toolchain,
                },
            )?,
            LocalInstallIdentityGeneration::GlaedaV2 => canonical_digest(
                GLAEDA_SOURCE_DIGEST_DOMAIN_V2,
                &CurrentDocument {
                    schema_version: LOCAL_INSTALL_PLAN_SCHEMA_VERSION,
                    identity_generation,
                    commit: &commit,
                    tree: &tree,
                    cargo_lock_digest: &cargo_lock_digest,
                    toolchain: &toolchain,
                },
            )?,
        };
        Ok(Self {
            identity_generation,
            commit,
            tree,
            cargo_lock_digest,
            toolchain,
            digest,
        })
    }

    #[must_use]
    pub const fn identity_generation(&self) -> LocalInstallIdentityGeneration {
        self.identity_generation
    }

    #[must_use]
    pub const fn commit(&self) -> &CommitId {
        &self.commit
    }

    #[must_use]
    pub const fn tree(&self) -> &GitTreeId {
        &self.tree
    }

    #[must_use]
    pub const fn cargo_lock_digest(&self) -> &Sha256Digest {
        &self.cargo_lock_digest
    }

    #[must_use]
    pub const fn toolchain(&self) -> &LocalInstallToolchainIdentity {
        &self.toolchain
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct LocalInstallGenerationIdentity {
    pub number: u64,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledLocalBinaryGeneration {
    pub identity: LocalInstallGenerationIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessor: Option<LocalInstallGenerationIdentity>,
    pub source: LocalInstallSourceIdentity,
    pub binary_digest: Sha256Digest,
    pub binary_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<InstalledLocalBinaryGeneration>,
    pub retained: Vec<InstalledLocalBinaryGeneration>,
}

impl LocalInstallState {
    /// Construct bounded accepted and retained local-install state.
    ///
    /// # Errors
    ///
    /// Returns an error for excess retained generations or duplicate generation identities.
    pub fn new(
        accepted: Option<InstalledLocalBinaryGeneration>,
        retained: Vec<InstalledLocalBinaryGeneration>,
    ) -> Result<Self, LocalInstallPlanError> {
        if retained.len() > MAX_RETAINED_LOCAL_INSTALL_GENERATIONS {
            return Err(error(
                LocalInstallPlanErrorKind::RetainedGenerationLimit,
                "retained_generation_limit",
                "local install retained-generation limit was exceeded",
            ));
        }
        let mut identities = BTreeSet::new();
        if let Some(current) = accepted.as_ref() {
            identities.insert(current.identity.clone());
        }
        for generation in &retained {
            if !identities.insert(generation.identity.clone()) {
                return Err(error(
                    LocalInstallPlanErrorKind::DuplicateGenerationIdentity,
                    "duplicate_generation_identity",
                    "local install state contains a duplicate generation identity",
                ));
            }
        }
        Ok(Self { accepted, retained })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallPlatform {
    Macos,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherLocationClass {
    HomeLocalBin,
    HomeBin,
    HomebrewBin,
    UsrLocalBin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherDirectoryDisposition {
    ReadyUserOwned,
    NeedsElevation,
    Unsafe,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LauncherEntryDisposition {
    Absent,
    Owned { generation_digest: Sha256Digest },
    Foreign,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LauncherLocationObservation {
    pub location: LauncherLocationClass,
    pub in_path: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_rank: Option<u16>,
    pub directory: LauncherDirectoryDisposition,
    pub entry: LauncherEntryDisposition,
}

impl LauncherLocationObservation {
    /// Construct one symbolic observation for an approved launcher location.
    ///
    /// # Errors
    ///
    /// Returns an error unless PATH presence and rank agree.
    pub fn new(
        location: LauncherLocationClass,
        in_path: bool,
        path_rank: Option<u16>,
        directory: LauncherDirectoryDisposition,
        entry: LauncherEntryDisposition,
    ) -> Result<Self, LocalInstallPlanError> {
        if in_path != path_rank.is_some()
            || (!in_path && !matches!(entry, LauncherEntryDisposition::Absent))
            || (directory == LauncherDirectoryDisposition::Unavailable
                && !matches!(entry, LauncherEntryDisposition::Absent))
        {
            return Err(error(
                LocalInstallPlanErrorKind::InvalidLauncherObservation,
                "invalid_launcher_observation",
                "approved launcher observation is internally inconsistent",
            ));
        }
        Ok(Self {
            location,
            in_path,
            path_rank,
            directory,
            entry,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallBuildPlan {
    pub target_generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_predecessor: Option<LocalInstallGenerationIdentity>,
    pub source: LocalInstallSourceIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuiltLocalBinaryEvidence {
    pub source_digest: Sha256Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessor: Option<LocalInstallGenerationIdentity>,
    pub binary_digest: Sha256Digest,
    pub binary_version: String,
}

impl BuiltLocalBinaryEvidence {
    /// Construct bounded reviewed build evidence.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid binary version string.
    pub fn new(
        source_digest: Sha256Digest,
        predecessor: Option<LocalInstallGenerationIdentity>,
        binary_digest: Sha256Digest,
        binary_version: impl Into<String>,
    ) -> Result<Self, LocalInstallPlanError> {
        let binary_version = binary_version.into();
        if binary_version.is_empty()
            || binary_version.len() > MAX_LOCAL_INSTALL_VERSION_BYTES
            || !binary_version.is_ascii()
            || binary_version.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(error(
                LocalInstallPlanErrorKind::InvalidBinaryVersion,
                "invalid_binary_version",
                "local install binary version is invalid",
            ));
        }
        Ok(Self {
            source_digest,
            predecessor,
            binary_digest,
            binary_version,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LauncherSwitchPlan {
    pub target_generation: LocalInstallGenerationIdentity,
    pub location: LauncherLocationClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LocalInstallDecision {
    BuildRequired {
        plan: LocalInstallBuildPlan,
    },
    SwitchLauncher {
        plan: LauncherSwitchPlan,
    },
    Satisfied {
        generation: LocalInstallGenerationIdentity,
        location: LauncherLocationClass,
    },
    ElevationRequired {
        generation: LocalInstallGenerationIdentity,
        location: LauncherLocationClass,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallRollbackPlan {
    pub from: LocalInstallGenerationIdentity,
    pub to: LocalInstallGenerationIdentity,
    pub launcher: RollbackLauncherRequirement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "requirement", rename_all = "snake_case")]
pub enum RollbackLauncherRequirement {
    AlreadyPointsToTarget { location: LauncherLocationClass },
    Switch { location: LauncherLocationClass },
    Elevation { location: LauncherLocationClass },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallPlanErrorKind {
    InvalidToolchain,
    InvalidBinaryVersion,
    IdentityEncodingFailed,
    RetainedGenerationLimit,
    DuplicateGenerationIdentity,
    GenerationExhausted,
    BuildEvidenceConflict,
    TooManyLauncherObservations,
    InvalidLauncherObservation,
    DuplicateLauncherLocation,
    DuplicatePathRank,
    UnsupportedLauncherLocation,
    ForeignLauncher,
    UnknownLauncher,
    UnsafeLauncherLocation,
    NoApprovedLauncherLocation,
    RollbackTargetUnavailable,
    MissingAcceptedGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallPlanError {
    pub kind: LocalInstallPlanErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for LocalInstallPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for LocalInstallPlanError {}

/// Decide whether exact source requires a build or can proceed directly to launcher convergence.
///
/// # Errors
///
/// Returns a bounded error for exhausted generation space or invalid launcher evidence.
pub fn plan_local_install(
    state: &LocalInstallState,
    source: &LocalInstallSourceIdentity,
    platform: LocalInstallPlatform,
    launchers: &[LauncherLocationObservation],
) -> Result<LocalInstallDecision, LocalInstallPlanError> {
    let Some(accepted) = state.accepted.as_ref() else {
        return Ok(LocalInstallDecision::BuildRequired {
            plan: LocalInstallBuildPlan {
                target_generation: 1,
                expected_predecessor: None,
                source: source.clone(),
            },
        });
    };
    if accepted.source != *source {
        let target_generation = accepted.identity.number.checked_add(1).ok_or_else(|| {
            error(
                LocalInstallPlanErrorKind::GenerationExhausted,
                "generation_exhausted",
                "local install generation number is exhausted",
            )
        })?;
        return Ok(LocalInstallDecision::BuildRequired {
            plan: LocalInstallBuildPlan {
                target_generation,
                expected_predecessor: Some(accepted.identity.clone()),
                source: source.clone(),
            },
        });
    }
    plan_launcher_for_generation(&accepted.identity, platform, launchers)
}

/// Turn one exact build plan plus reviewed artifact evidence into an immutable candidate generation.
///
/// # Errors
///
/// Returns a conflict unless source and predecessor evidence exactly match the build plan.
pub fn complete_local_install_build(
    plan: &LocalInstallBuildPlan,
    evidence: BuiltLocalBinaryEvidence,
) -> Result<InstalledLocalBinaryGeneration, LocalInstallPlanError> {
    if evidence.source_digest != plan.source.digest
        || evidence.predecessor != plan.expected_predecessor
    {
        return Err(error(
            LocalInstallPlanErrorKind::BuildEvidenceConflict,
            "build_evidence_conflict",
            "local install build evidence does not match the exact build plan",
        ));
    }
    #[derive(Serialize)]
    struct LegacySourceDocument<'a> {
        commit: &'a CommitId,
        tree: &'a GitTreeId,
        cargo_lock_digest: &'a Sha256Digest,
        toolchain: &'a LocalInstallToolchainIdentity,
        digest: &'a Sha256Digest,
    }
    #[derive(Serialize)]
    struct LegacyDocument<'a> {
        schema_version: u8,
        number: u64,
        predecessor: &'a Option<LocalInstallGenerationIdentity>,
        source: LegacySourceDocument<'a>,
        binary_digest: &'a Sha256Digest,
        binary_version: &'a str,
    }
    #[derive(Serialize)]
    struct CurrentDocument<'a> {
        schema_version: u8,
        identity_generation: LocalInstallIdentityGeneration,
        number: u64,
        predecessor: &'a Option<LocalInstallGenerationIdentity>,
        source: &'a LocalInstallSourceIdentity,
        binary_digest: &'a Sha256Digest,
        binary_version: &'a str,
    }
    let digest = match plan.source.identity_generation {
        LocalInstallIdentityGeneration::SmolrunnerV1 => canonical_digest(
            SMOLRUNNER_GENERATION_DIGEST_DOMAIN_V1,
            &LegacyDocument {
                schema_version: 1,
                number: plan.target_generation,
                predecessor: &plan.expected_predecessor,
                source: LegacySourceDocument {
                    commit: &plan.source.commit,
                    tree: &plan.source.tree,
                    cargo_lock_digest: &plan.source.cargo_lock_digest,
                    toolchain: &plan.source.toolchain,
                    digest: &plan.source.digest,
                },
                binary_digest: &evidence.binary_digest,
                binary_version: &evidence.binary_version,
            },
        )?,
        LocalInstallIdentityGeneration::GlaedaV2 => canonical_digest(
            GLAEDA_GENERATION_DIGEST_DOMAIN_V2,
            &CurrentDocument {
                schema_version: LOCAL_INSTALL_PLAN_SCHEMA_VERSION,
                identity_generation: plan.source.identity_generation,
                number: plan.target_generation,
                predecessor: &plan.expected_predecessor,
                source: &plan.source,
                binary_digest: &evidence.binary_digest,
                binary_version: &evidence.binary_version,
            },
        )?,
    };
    Ok(InstalledLocalBinaryGeneration {
        identity: LocalInstallGenerationIdentity {
            number: plan.target_generation,
            digest,
        },
        predecessor: plan.expected_predecessor.clone(),
        source: plan.source.clone(),
        binary_digest: evidence.binary_digest,
        binary_version: evidence.binary_version,
    })
}

/// Plan only the stable canonical `glaeda` launcher for one accepted or candidate generation.
///
/// The first existing launcher entry in PATH controls command resolution. A stale owned launcher at
/// an earlier elevation-requiring location therefore produces an elevation barrier immediately;
/// installing another launcher later in PATH would leave the stale command shadowing it. An absent
/// elevation-requiring location does not shadow later PATH entries and may be skipped in favor of a
/// later user-owned location.
///
/// # Errors
///
/// Returns a bounded conflict for foreign/unknown shadowing, unsafe approved locations, invalid
/// observations, unsupported classes, or the absence of any approved PATH location.
pub fn plan_launcher_for_generation(
    generation: &LocalInstallGenerationIdentity,
    platform: LocalInstallPlatform,
    launchers: &[LauncherLocationObservation],
) -> Result<LocalInstallDecision, LocalInstallPlanError> {
    let sorted = validated_launcher_observations(platform, launchers)?;
    let mut absent_elevation_candidate = None;
    for observation in sorted {
        match &observation.entry {
            LauncherEntryDisposition::Foreign => {
                return Err(error(
                    LocalInstallPlanErrorKind::ForeignLauncher,
                    "foreign_launcher",
                    "an earlier approved PATH location contains a foreign glaeda launcher",
                ));
            }
            LauncherEntryDisposition::Unknown => {
                return Err(error(
                    LocalInstallPlanErrorKind::UnknownLauncher,
                    "unknown_launcher",
                    "an earlier approved PATH location contains an unclassified glaeda launcher",
                ));
            }
            LauncherEntryDisposition::Owned { generation_digest } => {
                if observation.directory == LauncherDirectoryDisposition::Unsafe {
                    return Err(error(
                        LocalInstallPlanErrorKind::UnsafeLauncherLocation,
                        "unsafe_launcher_location",
                        "approved launcher location is unsafe",
                    ));
                }
                if generation_digest == &generation.digest {
                    return Ok(LocalInstallDecision::Satisfied {
                        generation: generation.clone(),
                        location: observation.location,
                    });
                }
                return match observation.directory {
                    LauncherDirectoryDisposition::ReadyUserOwned => {
                        Ok(LocalInstallDecision::SwitchLauncher {
                            plan: LauncherSwitchPlan {
                                target_generation: generation.clone(),
                                location: observation.location,
                            },
                        })
                    }
                    LauncherDirectoryDisposition::NeedsElevation => {
                        Ok(LocalInstallDecision::ElevationRequired {
                            generation: generation.clone(),
                            location: observation.location,
                        })
                    }
                    LauncherDirectoryDisposition::Unavailable => Err(error(
                        LocalInstallPlanErrorKind::InvalidLauncherObservation,
                        "invalid_launcher_observation",
                        "owned launcher cannot exist in an unavailable location",
                    )),
                    LauncherDirectoryDisposition::Unsafe => unreachable!(),
                };
            }
            LauncherEntryDisposition::Absent => match observation.directory {
                LauncherDirectoryDisposition::ReadyUserOwned => {
                    return Ok(LocalInstallDecision::SwitchLauncher {
                        plan: LauncherSwitchPlan {
                            target_generation: generation.clone(),
                            location: observation.location,
                        },
                    });
                }
                LauncherDirectoryDisposition::NeedsElevation => {
                    absent_elevation_candidate.get_or_insert(observation.location);
                }
                LauncherDirectoryDisposition::Unavailable => {}
                LauncherDirectoryDisposition::Unsafe => {
                    return Err(error(
                        LocalInstallPlanErrorKind::UnsafeLauncherLocation,
                        "unsafe_launcher_location",
                        "approved launcher location is unsafe",
                    ));
                }
            },
        }
    }
    if let Some(location) = absent_elevation_candidate {
        return Ok(LocalInstallDecision::ElevationRequired {
            generation: generation.clone(),
            location,
        });
    }
    Err(error(
        LocalInstallPlanErrorKind::NoApprovedLauncherLocation,
        "no_approved_launcher_location",
        "no approved launcher location is available in PATH",
    ))
}

/// Plan rollback to one retained verified generation through the same launcher safety rules.
///
/// # Errors
///
/// Returns an error unless current state exists, the target is retained, and launcher evidence is
/// safe enough to switch or already resolves to the target.
pub fn plan_local_install_rollback(
    state: &LocalInstallState,
    target: &LocalInstallGenerationIdentity,
    platform: LocalInstallPlatform,
    launchers: &[LauncherLocationObservation],
) -> Result<LocalInstallRollbackPlan, LocalInstallPlanError> {
    let current = state.accepted.as_ref().ok_or_else(|| {
        error(
            LocalInstallPlanErrorKind::MissingAcceptedGeneration,
            "missing_accepted_generation",
            "local install rollback requires an accepted generation",
        )
    })?;
    let retained = state
        .retained
        .iter()
        .find(|generation| generation.identity == *target)
        .ok_or_else(|| {
            error(
                LocalInstallPlanErrorKind::RollbackTargetUnavailable,
                "rollback_target_unavailable",
                "requested local install rollback generation is not retained",
            )
        })?;
    let launcher = match plan_launcher_for_generation(&retained.identity, platform, launchers)? {
        LocalInstallDecision::Satisfied { location, .. } => {
            RollbackLauncherRequirement::AlreadyPointsToTarget { location }
        }
        LocalInstallDecision::SwitchLauncher { plan } => RollbackLauncherRequirement::Switch {
            location: plan.location,
        },
        LocalInstallDecision::ElevationRequired { location, .. } => {
            RollbackLauncherRequirement::Elevation { location }
        }
        LocalInstallDecision::BuildRequired { .. } => unreachable!(),
    };
    Ok(LocalInstallRollbackPlan {
        from: current.identity.clone(),
        to: retained.identity.clone(),
        launcher,
    })
}

fn validated_launcher_observations(
    platform: LocalInstallPlatform,
    launchers: &[LauncherLocationObservation],
) -> Result<Vec<&LauncherLocationObservation>, LocalInstallPlanError> {
    if launchers.len() > MAX_LAUNCHER_OBSERVATIONS {
        return Err(error(
            LocalInstallPlanErrorKind::TooManyLauncherObservations,
            "too_many_launcher_observations",
            "local install launcher observation bound was exceeded",
        ));
    }
    let mut locations = BTreeSet::new();
    let mut ranks = BTreeSet::new();
    let mut result = Vec::new();
    for observation in launchers {
        if !locations.insert(observation.location) {
            return Err(error(
                LocalInstallPlanErrorKind::DuplicateLauncherLocation,
                "duplicate_launcher_location",
                "approved launcher location was observed more than once",
            ));
        }
        if platform == LocalInstallPlatform::Linux
            && observation.location == LauncherLocationClass::HomebrewBin
        {
            return Err(error(
                LocalInstallPlanErrorKind::UnsupportedLauncherLocation,
                "unsupported_launcher_location",
                "Homebrew launcher location is unsupported on Linux",
            ));
        }
        if observation.in_path != observation.path_rank.is_some() {
            return Err(error(
                LocalInstallPlanErrorKind::InvalidLauncherObservation,
                "invalid_launcher_observation",
                "approved launcher observation PATH rank is inconsistent",
            ));
        }
        if let Some(rank) = observation.path_rank {
            if !ranks.insert(rank) {
                return Err(error(
                    LocalInstallPlanErrorKind::DuplicatePathRank,
                    "duplicate_path_rank",
                    "approved launcher observations contain duplicate PATH ranks",
                ));
            }
            result.push(observation);
        }
    }
    result.sort_by_key(|observation| observation.path_rank.expect("filtered PATH rank"));
    Ok(result)
}

fn canonical_digest(
    domain: &[u8],
    document: &impl Serialize,
) -> Result<Sha256Digest, LocalInstallPlanError> {
    let bytes = serde_json::to_vec(document).map_err(|_| {
        error(
            LocalInstallPlanErrorKind::IdentityEncodingFailed,
            "identity_encoding_failed",
            "local install canonical identity could not be encoded",
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| {
        error(
            LocalInstallPlanErrorKind::IdentityEncodingFailed,
            "identity_encoding_failed",
            "local install canonical identity could not be encoded",
        )
    })
}

const fn error(
    kind: LocalInstallPlanErrorKind,
    code: &'static str,
    problem: &'static str,
) -> LocalInstallPlanError {
    LocalInstallPlanError {
        kind,
        code,
        problem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(ch: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", ch.to_string().repeat(64))).expect("digest")
    }

    fn commit(ch: char) -> CommitId {
        CommitId::parse(&ch.to_string().repeat(40)).expect("commit")
    }

    fn tree(ch: char) -> GitTreeId {
        GitTreeId::parse(&ch.to_string().repeat(40)).expect("tree")
    }

    fn source(ch: char) -> LocalInstallSourceIdentity {
        LocalInstallSourceIdentity::new(
            commit(ch),
            tree(ch),
            digest(ch),
            LocalInstallToolchainIdentity::parse("rust-1.97.1-aarch64-apple-darwin")
                .expect("toolchain"),
        )
        .expect("source")
    }

    fn legacy_source(ch: char) -> LocalInstallSourceIdentity {
        LocalInstallSourceIdentity::with_identity_generation(
            LocalInstallIdentityGeneration::SmolrunnerV1,
            commit(ch),
            tree(ch),
            digest(ch),
            LocalInstallToolchainIdentity::parse("rust-1.97.1-aarch64-apple-darwin")
                .expect("toolchain"),
        )
        .expect("legacy source")
    }

    fn build_generation(
        plan: &LocalInstallBuildPlan,
        binary: char,
    ) -> InstalledLocalBinaryGeneration {
        complete_local_install_build(
            plan,
            BuiltLocalBinaryEvidence::new(
                plan.source.digest.clone(),
                plan.expected_predecessor.clone(),
                digest(binary),
                "glaeda 0.1.0",
            )
            .expect("build evidence"),
        )
        .expect("generation")
    }

    fn launcher(
        location: LauncherLocationClass,
        rank: u16,
        directory: LauncherDirectoryDisposition,
        entry: LauncherEntryDisposition,
    ) -> LauncherLocationObservation {
        LauncherLocationObservation::new(location, true, Some(rank), directory, entry)
            .expect("launcher")
    }

    #[test]
    fn source_and_generation_identity_are_deterministic() {
        let first_source = source('a');
        let second_source = source('a');
        assert_eq!(first_source, second_source);
        assert_eq!(
            first_source.digest().as_str(),
            "sha256:d3cdbc0c7f38a5899698897e7d703e2dcceb69f9e2a5cef50cb76309e1504093"
        );

        let state = LocalInstallState::new(None, Vec::new()).expect("state");
        let LocalInstallDecision::BuildRequired { plan } =
            plan_local_install(&state, &first_source, LocalInstallPlatform::Macos, &[])
                .expect("plan")
        else {
            panic!("expected build")
        };
        let first = build_generation(&plan, 'b');
        let second = build_generation(&plan, 'b');
        assert_eq!(first, second);
        assert_eq!(first.identity.number, 1);
        assert_eq!(
            first.identity.digest.as_str(),
            "sha256:7a26d60f3a04ea9d12c3ac9b3a8faefdeebc692ed3929030b3cdf31d83d228f3"
        );
        assert_eq!(
            first.source.identity_generation(),
            LocalInstallIdentityGeneration::GlaedaV2
        );
    }

    #[test]
    fn legacy_source_and_installed_generation_vectors_remain_exact() {
        let source = legacy_source('a');
        assert_eq!(
            source.digest().as_str(),
            "sha256:7ae71e7bd75c0b5f8da6a951774c25887b7c740f34435cd3acc1297da547a9b7"
        );
        let plan = LocalInstallBuildPlan {
            target_generation: 1,
            expected_predecessor: None,
            source: source.clone(),
        };
        let installed = complete_local_install_build(
            &plan,
            BuiltLocalBinaryEvidence::new(
                source.digest().clone(),
                None,
                digest('b'),
                "smolrunner 0.1.0",
            )
            .expect("legacy evidence"),
        )
        .expect("legacy generation");

        assert_eq!(
            installed.identity.digest.as_str(),
            "sha256:e508ca7a23235e7ef522ef4c213d54d16c8d6db9b931a902ea7ab237b1d7c331"
        );
        assert_ne!(
            source,
            LocalInstallSourceIdentity::new(
                commit('a'),
                tree('a'),
                digest('a'),
                LocalInstallToolchainIdentity::parse("rust-1.97.1-aarch64-apple-darwin")
                    .expect("toolchain"),
            )
            .expect("current source")
        );
    }

    #[test]
    fn source_changes_produce_monotonic_build_plan_and_conflicting_evidence_fails() {
        let initial_source = source('a');
        let state = LocalInstallState::new(None, Vec::new()).expect("state");
        let LocalInstallDecision::BuildRequired { plan } =
            plan_local_install(&state, &initial_source, LocalInstallPlatform::Macos, &[])
                .expect("initial plan")
        else {
            panic!("expected build")
        };
        let installed = build_generation(&plan, 'b');
        let state = LocalInstallState::new(Some(installed.clone()), Vec::new()).expect("state");
        let LocalInstallDecision::BuildRequired { plan } =
            plan_local_install(&state, &source('c'), LocalInstallPlatform::Macos, &[])
                .expect("upgrade plan")
        else {
            panic!("expected upgrade build")
        };
        assert_eq!(plan.target_generation, 2);
        assert_eq!(plan.expected_predecessor, Some(installed.identity.clone()));

        let bad = BuiltLocalBinaryEvidence::new(
            digest('f'),
            plan.expected_predecessor.clone(),
            digest('d'),
            "glaeda 0.1.0",
        )
        .expect("bad evidence");
        assert_eq!(
            complete_local_install_build(&plan, bad)
                .expect_err("source mismatch")
                .kind,
            LocalInstallPlanErrorKind::BuildEvidenceConflict
        );
    }

    #[test]
    fn accepted_source_skips_build_and_safe_launcher_converges() {
        let wanted = source('a');
        let initial = LocalInstallState::new(None, Vec::new()).expect("state");
        let LocalInstallDecision::BuildRequired { plan } =
            plan_local_install(&initial, &wanted, LocalInstallPlatform::Macos, &[]).expect("build")
        else {
            panic!("expected build")
        };
        let installed = build_generation(&plan, 'b');
        let state = LocalInstallState::new(Some(installed.clone()), Vec::new()).expect("state");

        let absent = launcher(
            LauncherLocationClass::HomeLocalBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        );
        assert!(matches!(
            plan_local_install(&state, &wanted, LocalInstallPlatform::Macos, &[absent])
                .expect("launcher plan"),
            LocalInstallDecision::SwitchLauncher { .. }
        ));

        let current = launcher(
            LauncherLocationClass::HomeLocalBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Owned {
                generation_digest: installed.identity.digest.clone(),
            },
        );
        assert!(matches!(
            plan_local_install(&state, &wanted, LocalInstallPlatform::Macos, &[current])
                .expect("satisfied"),
            LocalInstallDecision::Satisfied { .. }
        ));
    }

    #[test]
    fn foreign_or_unknown_earliest_launcher_blocks_later_safe_location() {
        let generation = LocalInstallGenerationIdentity {
            number: 1,
            digest: digest('a'),
        };
        let safe_later = launcher(
            LauncherLocationClass::HomeLocalBin,
            1,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        );
        for entry in [
            LauncherEntryDisposition::Foreign,
            LauncherEntryDisposition::Unknown,
        ] {
            let first = launcher(
                LauncherLocationClass::HomebrewBin,
                0,
                LauncherDirectoryDisposition::ReadyUserOwned,
                entry,
            );
            let error = plan_launcher_for_generation(
                &generation,
                LocalInstallPlatform::Macos,
                &[first, safe_later.clone()],
            )
            .expect_err("earlier launcher must block");
            assert!(matches!(
                error.kind,
                LocalInstallPlanErrorKind::ForeignLauncher
                    | LocalInstallPlanErrorKind::UnknownLauncher
            ));
        }
    }

    #[test]
    fn absent_elevated_slot_can_yield_to_later_user_owned_slot() {
        let generation = LocalInstallGenerationIdentity {
            number: 1,
            digest: digest('a'),
        };
        let elevated_absent = launcher(
            LauncherLocationClass::UsrLocalBin,
            0,
            LauncherDirectoryDisposition::NeedsElevation,
            LauncherEntryDisposition::Absent,
        );
        let user_owned = launcher(
            LauncherLocationClass::HomeLocalBin,
            1,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        );
        let decision = plan_launcher_for_generation(
            &generation,
            LocalInstallPlatform::Macos,
            &[elevated_absent, user_owned],
        )
        .expect("later user-owned location");
        assert!(matches!(
            decision,
            LocalInstallDecision::SwitchLauncher {
                plan: LauncherSwitchPlan {
                    location: LauncherLocationClass::HomeLocalBin,
                    ..
                }
            }
        ));
    }

    #[test]
    fn stale_owned_earlier_launcher_requires_repair_at_that_location() {
        let generation = LocalInstallGenerationIdentity {
            number: 2,
            digest: digest('b'),
        };
        let stale_elevated = launcher(
            LauncherLocationClass::UsrLocalBin,
            0,
            LauncherDirectoryDisposition::NeedsElevation,
            LauncherEntryDisposition::Owned {
                generation_digest: digest('a'),
            },
        );
        let safe_later = launcher(
            LauncherLocationClass::HomeLocalBin,
            1,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        );
        let decision = plan_launcher_for_generation(
            &generation,
            LocalInstallPlatform::Macos,
            &[stale_elevated, safe_later],
        )
        .expect("elevation barrier");
        assert!(matches!(
            decision,
            LocalInstallDecision::ElevationRequired {
                location: LauncherLocationClass::UsrLocalBin,
                ..
            }
        ));
    }

    #[test]
    fn no_safe_user_location_falls_back_to_earliest_absent_elevation_candidate() {
        let generation = LocalInstallGenerationIdentity {
            number: 1,
            digest: digest('a'),
        };
        let first = launcher(
            LauncherLocationClass::UsrLocalBin,
            2,
            LauncherDirectoryDisposition::NeedsElevation,
            LauncherEntryDisposition::Absent,
        );
        let second = launcher(
            LauncherLocationClass::HomebrewBin,
            4,
            LauncherDirectoryDisposition::NeedsElevation,
            LauncherEntryDisposition::Absent,
        );
        let decision = plan_launcher_for_generation(
            &generation,
            LocalInstallPlatform::Macos,
            &[second, first],
        )
        .expect("elevation");
        assert!(matches!(
            decision,
            LocalInstallDecision::ElevationRequired {
                location: LauncherLocationClass::UsrLocalBin,
                ..
            }
        ));
    }

    #[test]
    fn unsupported_homebrew_linux_and_duplicate_ranks_fail_closed() {
        let generation = LocalInstallGenerationIdentity {
            number: 1,
            digest: digest('a'),
        };
        let homebrew = launcher(
            LauncherLocationClass::HomebrewBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        );
        assert_eq!(
            plan_launcher_for_generation(&generation, LocalInstallPlatform::Linux, &[homebrew])
                .expect_err("Homebrew on Linux")
                .kind,
            LocalInstallPlanErrorKind::UnsupportedLauncherLocation
        );

        let one = launcher(
            LauncherLocationClass::HomeLocalBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        );
        let two = launcher(
            LauncherLocationClass::HomeBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        );
        assert_eq!(
            plan_launcher_for_generation(&generation, LocalInstallPlatform::Macos, &[one, two])
                .expect_err("duplicate rank")
                .kind,
            LocalInstallPlanErrorKind::DuplicatePathRank
        );
    }

    #[test]
    fn rollback_targets_only_retained_verified_generation_and_uses_same_launcher_rules() {
        let first_source = source('a');
        let empty = LocalInstallState::new(None, Vec::new()).expect("empty");
        let LocalInstallDecision::BuildRequired { plan } =
            plan_local_install(&empty, &first_source, LocalInstallPlatform::Macos, &[])
                .expect("first plan")
        else {
            panic!("expected build")
        };
        let first = build_generation(&plan, 'b');
        let current_state = LocalInstallState::new(Some(first.clone()), Vec::new()).expect("state");
        let LocalInstallDecision::BuildRequired { plan } = plan_local_install(
            &current_state,
            &source('c'),
            LocalInstallPlatform::Macos,
            &[],
        )
        .expect("upgrade") else {
            panic!("expected upgrade")
        };
        let second = build_generation(&plan, 'd');
        let state =
            LocalInstallState::new(Some(second.clone()), vec![first.clone()]).expect("state");
        let stale = launcher(
            LauncherLocationClass::HomeLocalBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Owned {
                generation_digest: second.identity.digest.clone(),
            },
        );
        let rollback = plan_local_install_rollback(
            &state,
            &first.identity,
            LocalInstallPlatform::Macos,
            &[stale],
        )
        .expect("rollback");
        assert_eq!(rollback.from, second.identity);
        assert_eq!(rollback.to, first.identity);
        assert!(matches!(
            rollback.launcher,
            RollbackLauncherRequirement::Switch { .. }
        ));
    }

    #[test]
    fn bounds_overflow_and_privacy_fail_closed() {
        let wanted = source('a');
        let identity = LocalInstallGenerationIdentity {
            number: u64::MAX,
            digest: digest('b'),
        };
        let accepted = InstalledLocalBinaryGeneration {
            identity: identity.clone(),
            predecessor: None,
            source: source('c'),
            binary_digest: digest('d'),
            binary_version: "glaeda 0.1.0".to_owned(),
        };
        let state = LocalInstallState::new(Some(accepted), Vec::new()).expect("state");
        assert_eq!(
            plan_local_install(&state, &wanted, LocalInstallPlatform::Macos, &[])
                .expect_err("generation overflow")
                .kind,
            LocalInstallPlanErrorKind::GenerationExhausted
        );

        let retained = InstalledLocalBinaryGeneration {
            identity: LocalInstallGenerationIdentity {
                number: 1,
                digest: digest('e'),
            },
            predecessor: None,
            source: wanted.clone(),
            binary_digest: digest('f'),
            binary_version: "glaeda 0.1.0".to_owned(),
        };
        assert_eq!(
            LocalInstallState::new(None, vec![retained.clone(), retained])
                .expect_err("retention bound")
                .kind,
            LocalInstallPlanErrorKind::RetainedGenerationLimit
        );

        let public = serde_json::to_string(&wanted).expect("public source");
        for private in [
            "/Users/",
            "/home/",
            "HOME=",
            "CARGO_HOME",
            "RUSTFLAGS",
            "proxy",
            "credential",
        ] {
            assert!(
                !public.contains(private),
                "leaked private marker: {private}"
            );
        }
    }
}
