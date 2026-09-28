#[path = "../src/operator_config_location.rs"]
mod operator_config_location;

use std::ffi::OsStr;
use std::path::Path;

use operator_config_location::{
    GLAEDA_CONFIG_ENVIRONMENT_KEY, OperatorConfigLocationInputs,
    OperatorConfigLocationSelectionErrorKind, OperatorConfigLocationSource,
    SMOLRUNNER_CONFIG_LEGACY_ENVIRONMENT_KEY, select_operator_config_location,
};

fn inputs<'a>(
    explicit: Option<&'a str>,
    current: Option<&'a str>,
    legacy: Option<&'a str>,
    home: Option<&'a str>,
    macos: bool,
) -> OperatorConfigLocationInputs<'a> {
    OperatorConfigLocationInputs::new(
        explicit.map(OsStr::new),
        current.map(OsStr::new),
        legacy.map(OsStr::new),
        home.map(OsStr::new),
        macos,
    )
}

#[test]
fn current_environment_contract_uses_the_glaeda_key() {
    assert_eq!(GLAEDA_CONFIG_ENVIRONMENT_KEY, "GLAEDA_CONFIG");
    assert_eq!(
        SMOLRUNNER_CONFIG_LEGACY_ENVIRONMENT_KEY,
        "SMOLRUNNER_CONFIG"
    );
}

#[test]
fn explicit_path_has_highest_precedence_even_when_environment_values_conflict() {
    let selected = select_operator_config_location(inputs(
        Some("/private/explicit/config.json"),
        Some("/private/current/config.json"),
        Some("/private/legacy/config.json"),
        None,
        false,
    ))
    .expect("explicit path must win before environment conflict evaluation");

    assert_eq!(selected.source(), OperatorConfigLocationSource::Explicit);
    assert_eq!(selected.path(), Path::new("/private/explicit/config.json"));
}

#[test]
fn conflicting_current_and_legacy_environment_paths_fail_closed() {
    let error = select_operator_config_location(inputs(
        None,
        Some("/private/current/config.json"),
        Some("/private/legacy/config.json"),
        Some("/Users/operator"),
        true,
    ))
    .expect_err("disagreeing environment selectors must fail");

    assert_eq!(
        error.kind(),
        OperatorConfigLocationSelectionErrorKind::ConflictingEnvironment
    );
}

#[test]
fn agreeing_environment_values_are_classified_as_current_glaeda_input() {
    let selected = select_operator_config_location(inputs(
        None,
        Some("/private/shared/config.json"),
        Some("/private/shared/config.json"),
        None,
        false,
    ))
    .expect("equal selectors may identify one exact path");

    assert_eq!(
        selected.source(),
        OperatorConfigLocationSource::GlaedaEnvironment
    );
    assert!(!selected.source().is_legacy());
    assert_eq!(selected.path(), Path::new("/private/shared/config.json"));
}

#[test]
fn legacy_environment_is_deliberate_and_never_becomes_the_current_default() {
    let selected = select_operator_config_location(inputs(
        None,
        None,
        Some("/private/legacy/config.json"),
        Some("/Users/operator"),
        true,
    ))
    .expect("explicit legacy environment input remains available");

    assert_eq!(
        selected.source(),
        OperatorConfigLocationSource::SmolrunnerLegacyEnvironment
    );
    assert!(selected.source().is_legacy());
    assert_eq!(selected.path(), Path::new("/private/legacy/config.json"));
}

#[test]
fn no_environment_on_macos_selects_only_the_glaeda_managed_directory() {
    let selected =
        select_operator_config_location(inputs(None, None, None, Some("/Users/operator"), true))
            .expect("macOS has one reviewed current default");

    assert_eq!(
        selected.source(),
        OperatorConfigLocationSource::MacosGlaedaDefault
    );
    assert_eq!(
        selected.path(),
        Path::new("/Users/operator/Library/Application Support/Glaeda/config.json")
    );
    assert!(!selected.path().to_string_lossy().contains("SmolRunner"));
}

#[test]
fn default_selection_requires_a_home_and_reviewed_platform() {
    let missing_home = select_operator_config_location(inputs(None, None, None, None, true))
        .expect_err("macOS default requires an operator home");
    assert_eq!(
        missing_home.kind(),
        OperatorConfigLocationSelectionErrorKind::MissingOperatorHome
    );

    let unsupported =
        select_operator_config_location(inputs(None, None, None, Some("/home/operator"), false))
            .expect_err("another platform has no implicit config default in this contract");
    assert_eq!(
        unsupported.kind(),
        OperatorConfigLocationSelectionErrorKind::UnsupportedPlatform
    );
}

#[test]
fn public_serialization_and_debug_hide_private_paths() {
    let selected = select_operator_config_location(inputs(
        None,
        Some("/private/operator/secrets-nearby/config.json"),
        None,
        Some("/Users/operator"),
        true,
    ))
    .expect("current environment path selects");

    let json = serde_json::to_string(&selected).expect("selection serializes");
    let debug = format!("{selected:?}");
    assert!(json.contains("\"source\":\"glaeda_environment\""));
    assert!(!json.contains("/private/operator"));
    assert!(!debug.contains("/private/operator"));
}
