//! Pure fixed selectors for legacy and current disposable-worker LaunchAgent generations.
//!
//! These values identify one reviewed service namespace. They perform no filesystem or launchd
//! observation and grant zero install, removal, bootstrap, bootout, cleanup, or adoption authority.
//! The later service planner must bind the selected generation to its matching plan-identity
//! generation and freshly prove old/new coexistence state before any mutation.

use serde::Serialize;

pub const DISPOSABLE_LAUNCHD_SERVICE_GENERATION_SCHEMA_VERSION: u8 = 1;

const SMOLRUNNER_LEGACY_LABEL: &str = "io.smolrunner.disposable-worker";
const SMOLRUNNER_LEGACY_PLIST_FILE: &str = "io.smolrunner.disposable-worker.plist";
const SMOLRUNNER_LEGACY_APPLY_LOCK: &str = ".io.smolrunner.disposable-worker.apply.lock";
const SMOLRUNNER_LEGACY_STAGED_PLIST_PREFIX: &str = ".io.smolrunner.disposable-worker.plist.next.";

const GLAEDA_CURRENT_LABEL: &str = "io.glaeda.disposable-worker";
const GLAEDA_CURRENT_PLIST_FILE: &str = "io.glaeda.disposable-worker.plist";
const GLAEDA_CURRENT_APPLY_LOCK: &str = ".io.glaeda.disposable-worker.apply.lock";
const GLAEDA_CURRENT_STAGED_PLIST_PREFIX: &str = ".io.glaeda.disposable-worker.plist.next.";

/// Closed semantic generation of the disposable-worker LaunchAgent namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceGeneration {
    SmolrunnerLegacyV1,
    GlaedaCurrentV2,
}

impl DisposableLaunchdServiceGeneration {
    pub const CURRENT: Self = Self::GlaedaCurrentV2;

    #[must_use]
    pub const fn is_legacy(self) -> bool {
        matches!(self, Self::SmolrunnerLegacyV1)
    }
}

/// Fixed selector vocabulary for one reviewed service generation.
///
/// Every value is a repository-owned basename/label. Operator-home paths and launchd user-domain
/// identities remain outside this pure value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServiceSelectors {
    schema_version: u8,
    generation: DisposableLaunchdServiceGeneration,
    label: &'static str,
    plist_file_name: &'static str,
    apply_lock_file_name: &'static str,
    staged_plist_prefix: &'static str,
}

impl DisposableLaunchdServiceSelectors {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn generation(self) -> DisposableLaunchdServiceGeneration {
        self.generation
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        self.label
    }

    #[must_use]
    pub const fn plist_file_name(self) -> &'static str {
        self.plist_file_name
    }

    #[must_use]
    pub const fn apply_lock_file_name(self) -> &'static str {
        self.apply_lock_file_name
    }

    #[must_use]
    pub const fn staged_plist_prefix(self) -> &'static str {
        self.staged_plist_prefix
    }
}

/// Return the complete fixed selector set for one explicit service generation.
///
/// There is deliberately no free-form label/filename constructor and no selector based on the
/// presence of an existing plist or loaded launchd job.
#[must_use]
pub const fn disposable_launchd_service_selectors(
    generation: DisposableLaunchdServiceGeneration,
) -> DisposableLaunchdServiceSelectors {
    let (label, plist_file_name, apply_lock_file_name, staged_plist_prefix) = match generation {
        DisposableLaunchdServiceGeneration::SmolrunnerLegacyV1 => (
            SMOLRUNNER_LEGACY_LABEL,
            SMOLRUNNER_LEGACY_PLIST_FILE,
            SMOLRUNNER_LEGACY_APPLY_LOCK,
            SMOLRUNNER_LEGACY_STAGED_PLIST_PREFIX,
        ),
        DisposableLaunchdServiceGeneration::GlaedaCurrentV2 => (
            GLAEDA_CURRENT_LABEL,
            GLAEDA_CURRENT_PLIST_FILE,
            GLAEDA_CURRENT_APPLY_LOCK,
            GLAEDA_CURRENT_STAGED_PLIST_PREFIX,
        ),
    };
    DisposableLaunchdServiceSelectors {
        schema_version: DISPOSABLE_LAUNCHD_SERVICE_GENERATION_SCHEMA_VERSION,
        generation,
        label,
        plist_file_name,
        apply_lock_file_name,
        staged_plist_prefix,
    }
}
