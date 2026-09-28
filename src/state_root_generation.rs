//! Pure current/legacy selection for Glaeda's fixed Linux durable state roots.
//!
//! This module performs no filesystem access. In particular, it never probes one root to decide
//! whether another root should be selected. Existing SmolRunner state remains a separately selected
//! legacy generation; directory presence alone carries no migration or adoption authority.

use std::path::Path;

use serde::Serialize;

pub const STATE_ROOT_GENERATION_SCHEMA_VERSION: u8 = 1;
pub const GLAEDA_CURRENT_STATE_ROOT: &str = "/var/lib/glaeda";
pub const SMOLRUNNER_LEGACY_STATE_ROOT: &str = "/var/lib/smolrunner";

/// Exact semantic generation of one fixed product-owned Linux durable-state root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateRootGeneration {
    SmolrunnerLegacyV1,
    GlaedaCurrentV1,
}

impl StateRootGeneration {
    pub const CURRENT: Self = Self::GlaedaCurrentV1;

    #[must_use]
    pub const fn is_current(self) -> bool {
        matches!(self, Self::GlaedaCurrentV1)
    }

    #[must_use]
    pub const fn is_legacy(self) -> bool {
        matches!(self, Self::SmolrunnerLegacyV1)
    }

    const fn fixed_path(self) -> &'static str {
        match self {
            Self::SmolrunnerLegacyV1 => SMOLRUNNER_LEGACY_STATE_ROOT,
            Self::GlaedaCurrentV1 => GLAEDA_CURRENT_STATE_ROOT,
        }
    }
}

/// Closed caller intent for choosing a fixed root generation.
///
/// There is deliberately no `Auto` or "existing root" variant. Current operation selects Glaeda;
/// old SmolRunner state requires an explicit legacy/recovery choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateRootSelection {
    Current,
    LegacySmolrunnerV1,
}

/// Bounded selected-root fact. The fixed absolute path stays out of ordinary serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SelectedStateRoot {
    schema_version: u8,
    generation: StateRootGeneration,
}

impl SelectedStateRoot {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn generation(self) -> StateRootGeneration {
        self.generation
    }

    /// Return the reviewed fixed path for code that owns descriptor-based root opening.
    ///
    /// Calling this method grants no filesystem ownership, migration, cleanup, or recovery
    /// authority. The later I/O adapter must still apply its existing no-follow/owner/mode checks.
    #[must_use]
    pub fn fixed_path(self) -> &'static Path {
        Path::new(self.generation.fixed_path())
    }
}

/// Select one fixed state-root generation from explicit closed caller intent.
///
/// This function is pure and cannot inspect whether either root exists.
#[must_use]
pub const fn select_state_root(selection: StateRootSelection) -> SelectedStateRoot {
    let generation = match selection {
        StateRootSelection::Current => StateRootGeneration::CURRENT,
        StateRootSelection::LegacySmolrunnerV1 => StateRootGeneration::SmolrunnerLegacyV1,
    };
    SelectedStateRoot {
        schema_version: STATE_ROOT_GENERATION_SCHEMA_VERSION,
        generation,
    }
}
