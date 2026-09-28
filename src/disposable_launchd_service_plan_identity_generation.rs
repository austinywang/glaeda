//! Pure generation/domain policy for disposable LaunchAgent plan approval identities.
//!
//! The existing v1 approval digest is durable mutation evidence. Its domain bytes remain exact for
//! the retained SmolRunner service generation. Fresh Glaeda service selectors receive a distinct
//! successor domain through one closed mapping from the service generation.
//!
//! This module hashes no plan fields and performs no filesystem or launchd work. A selected domain
//! grants zero install, removal, bootstrap, bootout, cleanup, coexistence, or adoption authority.

use serde::Serialize;

use crate::disposable_launchd_service_generation::DisposableLaunchdServiceGeneration;

pub const DISPOSABLE_LAUNCHD_SERVICE_PLAN_IDENTITY_GENERATION_SCHEMA_VERSION: u8 = 1;

const SMOLRUNNER_V1_PLAN_DOMAIN: &[u8] = b"smolrunner.disposable-launchd-service-plan.v1\0";
const GLAEDA_V2_PLAN_DOMAIN: &[u8] = b"glaeda.disposable-launchd-service-plan.v2\0";

/// Closed semantic generation of the exact LaunchAgent plan approval digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServicePlanIdentityGeneration {
    SmolrunnerV1,
    GlaedaV2,
}

impl DisposableLaunchdServicePlanIdentityGeneration {
    pub const CURRENT: Self = Self::GlaedaV2;

    #[must_use]
    pub const fn is_legacy(self) -> bool {
        matches!(self, Self::SmolrunnerV1)
    }
}

/// Exact inseparable service-selector and plan-identity generation pairing.
///
/// There is no public constructor. The only production direction is from one reviewed service
/// generation to its matching plan identity generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServicePlanIdentityPolicy {
    schema_version: u8,
    service_generation: DisposableLaunchdServiceGeneration,
    plan_identity_generation: DisposableLaunchdServicePlanIdentityGeneration,
}

impl DisposableLaunchdServicePlanIdentityPolicy {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn service_generation(self) -> DisposableLaunchdServiceGeneration {
        self.service_generation
    }

    #[must_use]
    pub const fn plan_identity_generation(self) -> DisposableLaunchdServicePlanIdentityGeneration {
        self.plan_identity_generation
    }

    /// Return the exact reviewed domain bytes for the selected approval generation.
    ///
    /// The later planner must keep its current field encoding/order intact for SmolRunner v1 and
    /// select these bytes solely through this closed policy.
    #[must_use]
    pub const fn domain(self) -> &'static [u8] {
        match self.plan_identity_generation {
            DisposableLaunchdServicePlanIdentityGeneration::SmolrunnerV1 => {
                SMOLRUNNER_V1_PLAN_DOMAIN
            }
            DisposableLaunchdServicePlanIdentityGeneration::GlaedaV2 => GLAEDA_V2_PLAN_DOMAIN,
        }
    }
}

/// Derive the exact plan-identity generation from one closed service-selector generation.
#[must_use]
pub const fn disposable_launchd_service_plan_identity_policy(
    service_generation: DisposableLaunchdServiceGeneration,
) -> DisposableLaunchdServicePlanIdentityPolicy {
    let plan_identity_generation = match service_generation {
        DisposableLaunchdServiceGeneration::SmolrunnerLegacyV1 => {
            DisposableLaunchdServicePlanIdentityGeneration::SmolrunnerV1
        }
        DisposableLaunchdServiceGeneration::GlaedaCurrentV2 => {
            DisposableLaunchdServicePlanIdentityGeneration::GlaedaV2
        }
    };
    DisposableLaunchdServicePlanIdentityPolicy {
        schema_version: DISPOSABLE_LAUNCHD_SERVICE_PLAN_IDENTITY_GENERATION_SCHEMA_VERSION,
        service_generation,
        plan_identity_generation,
    }
}
