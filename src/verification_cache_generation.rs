//! Pure generation classification for personal-worker verification cache identities.
//!
//! Fresh Glaeda and retained SmolRunner-v1 verification profiles are distinct checked-in command
//! contracts. Cache namespace domains must follow that exact generation instead of inferring a
//! generation from a directory name, repository basename, or caller-supplied string.
//!
//! This module hashes nothing and performs no cache/filesystem access. The returned generation is
//! equality vocabulary only and grants zero cache ownership, adoption, cleanup, or mutation
//! authority.

use std::fmt;

use serde::Serialize;

use crate::verification_profile_registry::{
    GLAEDA_DOCTOR_COMMAND_ID, GLAEDA_DOCTOR_PROFILE_ID, GLAEDA_PLAN_COMMAND_ID,
    GLAEDA_PLAN_PROFILE_ID, GLAEDA_REQUIRED_COMMAND_ID, GLAEDA_REQUIRED_PROFILE_ID,
    RegisteredVerificationProfile, SMOLRUNNER_DOCTOR_PROFILE_ID, SMOLRUNNER_PLAN_PROFILE_ID,
    SMOLRUNNER_REQUIRED_PROFILE_ID,
};

const GLAEDA_REPOSITORY: &str = "teamleaderleo/glaeda";
const SMOLRUNNER_V1_REPOSITORY: &str = "teamleaderleo/smolrunner";
const SMOLRUNNER_V1_REQUIRED_COMMAND_ID: &str = "smolrunner.required.v1";
const SMOLRUNNER_V1_DOCTOR_COMMAND_ID: &str = "smolrunner.doctor.v1";
const SMOLRUNNER_V1_PLAN_COMMAND_ID: &str = "smolrunner.plan.v1";

/// Closed generation selector for verification-derived cache namespace identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCacheIdentityGeneration {
    SmolrunnerV1,
    GlaedaV2,
}

impl VerificationCacheIdentityGeneration {
    pub const CURRENT: Self = Self::GlaedaV2;
}

/// Classify one exact checked-in verification profile/command contract.
///
/// `RegisteredVerificationProfile` has no public arbitrary constructor: the registry owns the
/// canonical profile, repository, command ID, and command digest. This classifier checks the
/// generation-bearing profile/repository/command-ID tuple and refuses any future or inconsistent
/// tuple until its cache generation is reviewed explicitly.
///
/// # Errors
///
/// Returns a bounded error when the registered tuple is outside the reviewed Glaeda-v2 and
/// SmolRunner-v1 generations.
pub fn verification_cache_identity_generation(
    profile: &RegisteredVerificationProfile,
) -> Result<VerificationCacheIdentityGeneration, VerificationCacheGenerationError> {
    classify_tuple(
        profile.profile_id().as_str(),
        profile.canonical_command().identity().repository().as_str(),
        profile.canonical_command().identity().command_id().as_str(),
    )
}

fn classify_tuple(
    profile_id: &str,
    repository: &str,
    command_id: &str,
) -> Result<VerificationCacheIdentityGeneration, VerificationCacheGenerationError> {
    match (profile_id, repository, command_id) {
        (GLAEDA_REQUIRED_PROFILE_ID, GLAEDA_REPOSITORY, GLAEDA_REQUIRED_COMMAND_ID)
        | (GLAEDA_DOCTOR_PROFILE_ID, GLAEDA_REPOSITORY, GLAEDA_DOCTOR_COMMAND_ID)
        | (GLAEDA_PLAN_PROFILE_ID, GLAEDA_REPOSITORY, GLAEDA_PLAN_COMMAND_ID) => {
            Ok(VerificationCacheIdentityGeneration::GlaedaV2)
        }
        (
            SMOLRUNNER_REQUIRED_PROFILE_ID,
            SMOLRUNNER_V1_REPOSITORY,
            SMOLRUNNER_V1_REQUIRED_COMMAND_ID,
        )
        | (
            SMOLRUNNER_DOCTOR_PROFILE_ID,
            SMOLRUNNER_V1_REPOSITORY,
            SMOLRUNNER_V1_DOCTOR_COMMAND_ID,
        )
        | (SMOLRUNNER_PLAN_PROFILE_ID, SMOLRUNNER_V1_REPOSITORY, SMOLRUNNER_V1_PLAN_COMMAND_ID) => {
            Ok(VerificationCacheIdentityGeneration::SmolrunnerV1)
        }
        _ => Err(VerificationCacheGenerationError),
    }
}

/// Bounded fail-closed generation classification error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct VerificationCacheGenerationError;

impl fmt::Display for VerificationCacheGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("verification cache identity generation is unrecognized")
    }
}

impl std::error::Error for VerificationCacheGenerationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossed_profile_command_generations_fail_closed() {
        for tuple in [
            (
                GLAEDA_REQUIRED_PROFILE_ID,
                GLAEDA_REPOSITORY,
                SMOLRUNNER_V1_REQUIRED_COMMAND_ID,
            ),
            (
                SMOLRUNNER_REQUIRED_PROFILE_ID,
                SMOLRUNNER_V1_REPOSITORY,
                GLAEDA_REQUIRED_COMMAND_ID,
            ),
            (
                GLAEDA_DOCTOR_PROFILE_ID,
                SMOLRUNNER_V1_REPOSITORY,
                GLAEDA_DOCTOR_COMMAND_ID,
            ),
            (
                SMOLRUNNER_PLAN_PROFILE_ID,
                GLAEDA_REPOSITORY,
                SMOLRUNNER_V1_PLAN_COMMAND_ID,
            ),
        ] {
            classify_tuple(tuple.0, tuple.1, tuple.2)
                .expect_err("cross-generation tuple must fail");
        }
    }

    #[test]
    fn unknown_future_tuple_requires_explicit_review() {
        classify_tuple("glaeda.future", GLAEDA_REPOSITORY, "glaeda.future.v3")
            .expect_err("future generation must fail closed");
    }
}
