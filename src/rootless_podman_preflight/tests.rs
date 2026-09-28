use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::debian_package_plan::{build_package_plan, parse_os_release};
use crate::host::Presence;
use crate::rootless_podman_config_resolution::{
    ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION, RootlessPodmanConfigAssessment,
    RootlessPodmanConfigAssessmentState, RootlessPodmanConfigField,
    RootlessPodmanConfigFieldAssessment,
};
use crate::runner_account_plan::{
    PreparationObservation, PreparationObservationState, RunnerAccountObservations,
};

use super::support::{
    ExecutableProbe, RuntimeFilesystem, RuntimeIdentity, RuntimePathKind, RuntimePathMetadata,
    RuntimePathObservation,
};
use super::{
    RootlessPodmanPreflightDisposition, RootlessPodmanPreflightPaths, RootlessPodmanPreflightState,
    observe_with, observe_with_environment,
};

struct FakeFilesystem {
    observation: RuntimePathObservation,
}

impl RuntimeFilesystem for FakeFilesystem {
    fn inspect(&self, _path: &Path) -> RuntimePathObservation {
        self.observation
    }
}

fn package_plan(state: Presence) -> crate::debian_package_plan::DebianPackagePlan {
    let distribution = parse_os_release("ID=ubuntu\nVERSION_ID=24.04\n").expect("distribution");
    let observed = [
        "git",
        "podman",
        "uidmap",
        "slirp4netns",
        "fuse-overlayfs",
        "dbus-user-session",
    ]
    .into_iter()
    .map(|package| (package.to_owned(), state))
    .collect::<BTreeMap<_, _>>();
    build_package_plan(distribution, &observed).expect("package plan")
}

fn account_observations(state: PreparationObservationState) -> RunnerAccountObservations {
    let make = || PreparationObservation::new(state, ["bounded evidence"]).expect("observation");
    RunnerAccountObservations {
        group: make(),
        user: make(),
        home: make(),
        subordinate_uids: make(),
        subordinate_gids: make(),
        linger: make(),
    }
}

fn config_assessment(state: RootlessPodmanConfigAssessmentState) -> RootlessPodmanConfigAssessment {
    RootlessPodmanConfigAssessment {
        schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
        state,
        fields: Vec::new(),
    }
}

fn matching_executable(_path: &Path) -> ExecutableProbe {
    ExecutableProbe {
        state: RootlessPodmanPreflightState::Matching,
        evidence: vec!["matching executable".to_owned()],
    }
}

#[test]
fn matching_static_state_is_ready_only_for_later_smoke_verification() {
    let report = observe_with(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &config_assessment(RootlessPodmanConfigAssessmentState::Matching),
        &RootlessPodmanPreflightPaths::new("/run/user").expect("paths"),
        &FakeFilesystem {
            observation: RuntimePathObservation::Present(RuntimePathMetadata {
                kind: RuntimePathKind::Directory,
                uid: 1001,
                mode: 0o700,
            }),
        },
        &matching_executable,
    );

    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification
    );
    assert_eq!(
        report.configuration.state,
        RootlessPodmanPreflightState::Matching
    );
    assert_eq!(report.executables.len(), 8);
    assert!(report.executables.iter().all(|item| {
        item.state == RootlessPodmanPreflightState::Matching
            && item.path.is_absolute()
            && item.path.as_path() != Path::new("podman")
    }));
}

#[test]
fn selected_rust_coreutils_env_is_reverified_and_reported_matching() {
    let probed = RefCell::new(Vec::new());
    let report = observe_with_environment(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &config_assessment(RootlessPodmanConfigAssessmentState::Matching),
        &RootlessPodmanPreflightPaths::system_default(),
        (
            &FakeFilesystem {
                observation: RuntimePathObservation::Present(RuntimePathMetadata {
                    kind: RuntimePathKind::Directory,
                    uid: 1001,
                    mode: 0o700,
                }),
            },
            &|path| {
                probed.borrow_mut().push(path.to_path_buf());
                matching_executable(path)
            },
            &|| Ok(PathBuf::from("/usr/lib/cargo/bin/coreutils/env")),
        ),
    );

    let environment = report
        .executables
        .iter()
        .find(|executable| executable.name == "env")
        .expect("environment observation");
    assert_eq!(
        environment.path,
        Path::new("/usr/lib/cargo/bin/coreutils/env")
    );
    assert_eq!(environment.state, RootlessPodmanPreflightState::Matching);
    assert!(
        probed
            .borrow()
            .contains(&PathBuf::from("/usr/lib/cargo/bin/coreutils/env"))
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification
    );
}

#[test]
fn unsupported_environment_selection_blocks_without_probing_it() {
    let probed = RefCell::new(Vec::new());
    let report = observe_with_environment(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &config_assessment(RootlessPodmanConfigAssessmentState::Matching),
        &RootlessPodmanPreflightPaths::system_default(),
        (
            &FakeFilesystem {
                observation: RuntimePathObservation::Present(RuntimePathMetadata {
                    kind: RuntimePathKind::Directory,
                    uid: 1001,
                    mode: 0o700,
                }),
            },
            &|path| {
                probed.borrow_mut().push(path.to_path_buf());
                matching_executable(path)
            },
            &|| Ok(PathBuf::from("/tmp/private-env-marker")),
        ),
    );

    let environment = report
        .executables
        .iter()
        .find(|executable| executable.name == "env")
        .expect("environment observation");
    assert_eq!(environment.path, Path::new("/usr/bin/env"));
    assert_eq!(environment.state, RootlessPodmanPreflightState::Conflicting);
    assert_eq!(
        environment.evidence,
        ["no supported reviewed environment executable is available"]
    );
    assert!(
        !probed
            .borrow()
            .contains(&PathBuf::from("/tmp/private-env-marker"))
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::Blocked
    );
}

#[test]
fn unavailable_environment_selection_blocks_with_bounded_evidence() {
    let report = observe_with_environment(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &config_assessment(RootlessPodmanConfigAssessmentState::Matching),
        &RootlessPodmanPreflightPaths::system_default(),
        (
            &FakeFilesystem {
                observation: RuntimePathObservation::Present(RuntimePathMetadata {
                    kind: RuntimePathKind::Directory,
                    uid: 1001,
                    mode: 0o700,
                }),
            },
            &matching_executable,
            &|| Err(()),
        ),
    );

    let environment = report
        .executables
        .iter()
        .find(|executable| executable.name == "env")
        .expect("environment observation");
    assert_eq!(
        environment.evidence,
        ["no supported reviewed environment executable is available"]
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::Blocked
    );
}

#[test]
fn unknown_account_blocks_runtime_inspection_and_fails_closed() {
    let report = observe_with(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Unknown),
        None,
        &config_assessment(RootlessPodmanConfigAssessmentState::Matching),
        &RootlessPodmanPreflightPaths::system_default(),
        &FakeFilesystem {
            observation: RuntimePathObservation::Present(RuntimePathMetadata {
                kind: RuntimePathKind::Directory,
                uid: 1001,
                mode: 0o700,
            }),
        },
        &matching_executable,
    );

    assert_eq!(
        report.runner_account.state,
        RootlessPodmanPreflightState::Unknown
    );
    assert_eq!(
        report.runtime_directory.state,
        RootlessPodmanPreflightState::Blocked
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::NeedsInspection
    );
}

#[test]
fn conflicting_runtime_directory_blocks_preflight() {
    let report = observe_with(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &config_assessment(RootlessPodmanConfigAssessmentState::Matching),
        &RootlessPodmanPreflightPaths::system_default(),
        &FakeFilesystem {
            observation: RuntimePathObservation::Present(RuntimePathMetadata {
                kind: RuntimePathKind::Directory,
                uid: 2000,
                mode: 0o777,
            }),
        },
        &matching_executable,
    );

    assert_eq!(
        report.runtime_directory.state,
        RootlessPodmanPreflightState::Conflicting
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::Blocked
    );
}

#[test]
fn missing_packages_and_helpers_require_changes_without_running_podman() {
    let report = observe_with(
        &package_plan(Presence::Absent),
        &account_observations(PreparationObservationState::Absent),
        None,
        &config_assessment(RootlessPodmanConfigAssessmentState::Matching),
        &RootlessPodmanPreflightPaths::system_default(),
        &FakeFilesystem {
            observation: RuntimePathObservation::Missing,
        },
        &|path| ExecutableProbe {
            state: if path == Path::new("/usr/bin/podman") {
                RootlessPodmanPreflightState::Absent
            } else {
                RootlessPodmanPreflightState::Matching
            },
            evidence: vec!["fixed fake metadata result".to_owned()],
        },
    );

    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::ChangesRequired
    );
    assert_eq!(
        report.executables[0].state,
        RootlessPodmanPreflightState::Absent
    );
}

#[test]
fn conflicting_configuration_blocks_preflight() {
    let report = observe_with(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &config_assessment(RootlessPodmanConfigAssessmentState::Conflicting),
        &RootlessPodmanPreflightPaths::system_default(),
        &FakeFilesystem {
            observation: RuntimePathObservation::Present(RuntimePathMetadata {
                kind: RuntimePathKind::Directory,
                uid: 1001,
                mode: 0o700,
            }),
        },
        &matching_executable,
    );

    assert_eq!(
        report.configuration.state,
        RootlessPodmanPreflightState::Conflicting
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::Blocked
    );
}

#[test]
fn unknown_configuration_requires_inspection() {
    let report = observe_with(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &config_assessment(RootlessPodmanConfigAssessmentState::Unknown),
        &RootlessPodmanPreflightPaths::system_default(),
        &FakeFilesystem {
            observation: RuntimePathObservation::Present(RuntimePathMetadata {
                kind: RuntimePathKind::Directory,
                uid: 1001,
                mode: 0o700,
            }),
        },
        &matching_executable,
    );

    assert_eq!(
        report.configuration.state,
        RootlessPodmanPreflightState::Unknown
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::NeedsInspection
    );
}

#[test]
fn absent_configuration_requires_changes() {
    let report = observe_with(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &config_assessment(RootlessPodmanConfigAssessmentState::Absent),
        &RootlessPodmanPreflightPaths::system_default(),
        &FakeFilesystem {
            observation: RuntimePathObservation::Present(RuntimePathMetadata {
                kind: RuntimePathKind::Directory,
                uid: 1001,
                mode: 0o700,
            }),
        },
        &matching_executable,
    );

    assert_eq!(
        report.configuration.state,
        RootlessPodmanPreflightState::Absent
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::ChangesRequired
    );
}

#[test]
fn inconsistent_configuration_assessment_blocks_preflight() {
    let assessment = RootlessPodmanConfigAssessment {
        schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
        state: RootlessPodmanConfigAssessmentState::Matching,
        fields: vec![RootlessPodmanConfigFieldAssessment {
            field: RootlessPodmanConfigField::NetworkBackend,
            state: RootlessPodmanConfigAssessmentState::Conflicting,
            expected: "netavark".to_owned(),
            observed: Some("cni".to_owned()),
            evidence: vec!["bounded test evidence".to_owned()],
        }],
    };
    let report = observe_with(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &assessment,
        &RootlessPodmanPreflightPaths::system_default(),
        &FakeFilesystem {
            observation: RuntimePathObservation::Present(RuntimePathMetadata {
                kind: RuntimePathKind::Directory,
                uid: 1001,
                mode: 0o700,
            }),
        },
        &matching_executable,
    );

    assert_eq!(
        report.configuration.state,
        RootlessPodmanPreflightState::Conflicting
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::Blocked
    );
}

#[test]
fn unsafe_runtime_root_is_rejected() {
    for path in ["run/user", "/", "/run/../user", "/run/user/"] {
        assert!(RootlessPodmanPreflightPaths::new(path).is_err());
    }
}
