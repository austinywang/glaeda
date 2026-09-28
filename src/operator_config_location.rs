//! Pure current/legacy location selection for operator configuration.
//!
//! This module chooses which already-supplied private path should be validated and opened by the
//! filesystem store. It performs no environment reads and no filesystem access. In particular,
//! it never probes the old SmolRunner default location and cannot adopt that path because a file
//! happens to exist there.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub const GLAEDA_CONFIG_ENVIRONMENT_KEY: &str = "GLAEDA_CONFIG";
pub const SMOLRUNNER_CONFIG_LEGACY_ENVIRONMENT_KEY: &str = "SMOLRUNNER_CONFIG";

const LIBRARY_DIRECTORY: &str = "Library";
const APPLICATION_SUPPORT_DIRECTORY: &str = "Application Support";
const GLAEDA_MANAGED_DIRECTORY: &str = "Glaeda";
const CONFIG_FILE: &str = "config.json";

/// Closed source class for one selected operator-config path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorConfigLocationSource {
    Explicit,
    GlaedaEnvironment,
    SmolrunnerLegacyEnvironment,
    MacosGlaedaDefault,
}

impl OperatorConfigLocationSource {
    #[must_use]
    pub const fn is_legacy(self) -> bool {
        matches!(self, Self::SmolrunnerLegacyEnvironment)
    }
}

/// Private location inputs gathered by the later platform/environment adapter.
#[derive(Clone, Copy)]
pub struct OperatorConfigLocationInputs<'a> {
    explicit_path: Option<&'a OsStr>,
    glaeda_environment: Option<&'a OsStr>,
    smolrunner_legacy_environment: Option<&'a OsStr>,
    operator_home: Option<&'a OsStr>,
    supports_macos_default: bool,
}

impl fmt::Debug for OperatorConfigLocationInputs<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorConfigLocationInputs")
            .field(
                "explicit_path",
                &self.explicit_path.map(|_| "<private-path>"),
            )
            .field(
                "glaeda_environment",
                &self.glaeda_environment.map(|_| "<private-path>"),
            )
            .field(
                "smolrunner_legacy_environment",
                &self.smolrunner_legacy_environment.map(|_| "<private-path>"),
            )
            .field(
                "operator_home",
                &self.operator_home.map(|_| "<private-path>"),
            )
            .field("supports_macos_default", &self.supports_macos_default)
            .finish()
    }
}

impl<'a> OperatorConfigLocationInputs<'a> {
    #[must_use]
    pub const fn new(
        explicit_path: Option<&'a OsStr>,
        glaeda_environment: Option<&'a OsStr>,
        smolrunner_legacy_environment: Option<&'a OsStr>,
        operator_home: Option<&'a OsStr>,
        supports_macos_default: bool,
    ) -> Self {
        Self {
            explicit_path,
            glaeda_environment,
            smolrunner_legacy_environment,
            operator_home,
            supports_macos_default,
        }
    }
}

/// Selected private path plus the bounded source class that chose it.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SelectedOperatorConfigLocation {
    source: OperatorConfigLocationSource,
    #[serde(skip)]
    path: PathBuf,
}

impl SelectedOperatorConfigLocation {
    #[must_use]
    pub const fn source(&self) -> OperatorConfigLocationSource {
        self.source
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Debug for SelectedOperatorConfigLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectedOperatorConfigLocation")
            .field("source", &self.source)
            .field("path", &"<private-operator-config-path>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorConfigLocationSelectionErrorKind {
    ConflictingEnvironment,
    MissingOperatorHome,
    UnsupportedPlatform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperatorConfigLocationSelectionError {
    kind: OperatorConfigLocationSelectionErrorKind,
}

impl OperatorConfigLocationSelectionError {
    #[must_use]
    pub const fn kind(self) -> OperatorConfigLocationSelectionErrorKind {
        self.kind
    }
}

impl fmt::Display for OperatorConfigLocationSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("operator configuration location selection was refused")
    }
}

impl std::error::Error for OperatorConfigLocationSelectionError {}

/// Select one private operator-config location without reading the environment or filesystem.
///
/// Precedence is exact:
///
/// 1. caller-supplied explicit path;
/// 2. current `GLAEDA_CONFIG` path;
/// 3. explicit legacy `SMOLRUNNER_CONFIG` path;
/// 4. the fixed Glaeda macOS default.
///
/// When both environment families are supplied, unequal values refuse before either can be used.
/// Equal values select the current Glaeda environment class. There is deliberately no old-default
/// fallback or path-presence input.
///
/// # Errors
///
/// Returns a bounded error for conflicting environment selectors, a missing home directory needed
/// for the macOS default, or a platform without a reviewed default.
pub fn select_operator_config_location(
    inputs: OperatorConfigLocationInputs<'_>,
) -> Result<SelectedOperatorConfigLocation, OperatorConfigLocationSelectionError> {
    if let Some(path) = inputs.explicit_path {
        return Ok(selected(OperatorConfigLocationSource::Explicit, path));
    }

    match (
        inputs.glaeda_environment,
        inputs.smolrunner_legacy_environment,
    ) {
        (Some(current), Some(legacy)) if current != legacy => {
            return Err(selection_error(
                OperatorConfigLocationSelectionErrorKind::ConflictingEnvironment,
            ));
        }
        (Some(current), _) => {
            return Ok(selected(
                OperatorConfigLocationSource::GlaedaEnvironment,
                current,
            ));
        }
        (None, Some(legacy)) => {
            return Ok(selected(
                OperatorConfigLocationSource::SmolrunnerLegacyEnvironment,
                legacy,
            ));
        }
        (None, None) => {}
    }

    if !inputs.supports_macos_default {
        return Err(selection_error(
            OperatorConfigLocationSelectionErrorKind::UnsupportedPlatform,
        ));
    }
    let home = inputs.operator_home.ok_or_else(|| {
        selection_error(OperatorConfigLocationSelectionErrorKind::MissingOperatorHome)
    })?;
    let path = Path::new(home)
        .join(LIBRARY_DIRECTORY)
        .join(APPLICATION_SUPPORT_DIRECTORY)
        .join(GLAEDA_MANAGED_DIRECTORY)
        .join(CONFIG_FILE);
    Ok(SelectedOperatorConfigLocation {
        source: OperatorConfigLocationSource::MacosGlaedaDefault,
        path,
    })
}

fn selected(source: OperatorConfigLocationSource, path: &OsStr) -> SelectedOperatorConfigLocation {
    SelectedOperatorConfigLocation {
        source,
        path: OsString::from(path).into(),
    }
}

const fn selection_error(
    kind: OperatorConfigLocationSelectionErrorKind,
) -> OperatorConfigLocationSelectionError {
    OperatorConfigLocationSelectionError { kind }
}
