use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::host_readiness::HostReadinessReport;
use crate::host_rootless_podman::HostRootlessPodmanReadiness;
use crate::lane_executable::is_supported_environment_executable_path;
use crate::rootless_podman_config_observation::ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION;
use crate::rootless_podman_config_resolution::{
    ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION, RootlessPodmanConfigAssessmentState,
};
use crate::rootless_podman_preflight::{
    ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION, RootlessPodmanPreflightDisposition,
    RootlessPodmanPreflightState, RootlessPodmanStaticPreflightReport,
};

use super::planner::{inconsistent, package_is_missing, push_blocker};
use super::{HostPreparationBlocker, HostPreparationBlockerCode, HostPreparationResource};

const ROOTLESS_EXECUTABLES: [(&str, &str); 8] = [
    ("podman", "/usr/bin/podman"),
    ("runuser", "/usr/sbin/runuser"),
    ("env", "/usr/bin/env"),
    ("systemctl", "/usr/bin/systemctl"),
    ("newuidmap", "/usr/bin/newuidmap"),
    ("newgidmap", "/usr/bin/newgidmap"),
    ("slirp4netns", "/usr/bin/slirp4netns"),
    ("fuse-overlayfs", "/usr/bin/fuse-overlayfs"),
];

pub(super) fn validate_rootless_podman(
    report: &HostReadinessReport,
    has_package_action: bool,
    has_account_action: bool,
    has_mapping_action: bool,
    blockers: &mut Vec<HostPreparationBlocker>,
) -> bool {
    match &report.rootless_podman {
        HostRootlessPodmanReadiness::Deferred { state, .. } => {
            match state {
                RootlessPodmanPreflightState::Absent
                    if has_account_action || has_mapping_action => {}
                RootlessPodmanPreflightState::Absent => inconsistent(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    "rootless Podman preflight is deferred without an account preparation action",
                ),
                RootlessPodmanPreflightState::Unknown => push_blocker(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    HostPreparationBlockerCode::NeedsInspection,
                    "rootless Podman preflight evidence remains incomplete",
                ),
                RootlessPodmanPreflightState::Conflicting
                | RootlessPodmanPreflightState::Blocked => push_blocker(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    HostPreparationBlockerCode::ConflictingEvidence,
                    "rootless Podman preflight evidence conflicts with reviewed policy",
                ),
                RootlessPodmanPreflightState::Matching => inconsistent(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    "a matching rootless Podman preflight cannot be represented as deferred",
                ),
            }
            false
        }
        HostRootlessPodmanReadiness::Observed {
            configuration,
            preflight,
        } => {
            if configuration.schema_version != ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION
                || configuration.assessment.schema_version
                    != ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION
                || preflight.schema_version != ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION
            {
                push_blocker(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    HostPreparationBlockerCode::UnsupportedSchema,
                    "a rootless Podman observation schema is unsupported",
                );
            }
            let derived = configuration
                .assessment
                .fields
                .iter()
                .map(|field| field.state)
                .max();
            if derived.is_some_and(|state| state != configuration.assessment.state) {
                inconsistent(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    "the rootless Podman configuration assessment conflicts with its field results",
                );
            }
            let expected_configuration_state = assessment_state(configuration.assessment.state);
            if preflight.configuration.state != expected_configuration_state {
                inconsistent(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    "the rootless Podman configuration summary conflicts with its assessment",
                );
            }

            validate_package_component(preflight.packages.state, has_package_action, blockers);
            if preflight.runner_account.state != RootlessPodmanPreflightState::Matching {
                inconsistent(
                    blockers,
                    HostPreparationResource::RunnerAccount,
                    "an observed rootless Podman report requires matching runner account evidence",
                );
            }
            validate_unmapped_component(
                preflight.runtime_directory.state,
                "runner runtime-directory",
                blockers,
            );
            validate_unmapped_component(
                preflight.configuration.state,
                "rootless Podman configuration",
                blockers,
            );
            validate_executables(report, preflight, has_package_action, blockers);
            match preflight.disposition {
                RootlessPodmanPreflightDisposition::ReadyForSmokeVerification => {}
                RootlessPodmanPreflightDisposition::ChangesRequired => {
                    if !has_package_action {
                        push_blocker(
                            blockers,
                            HostPreparationResource::RootlessPodman,
                            HostPreparationBlockerCode::UnsupportedMutation,
                            "rootless Podman preflight requires changes outside the mapped package actions",
                        );
                    }
                }
                RootlessPodmanPreflightDisposition::NeedsInspection => push_blocker(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    HostPreparationBlockerCode::NeedsInspection,
                    "rootless Podman preflight requires fresh inspection",
                ),
                RootlessPodmanPreflightDisposition::Blocked => push_blocker(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    HostPreparationBlockerCode::ConflictingEvidence,
                    "rootless Podman preflight is blocked by reviewed evidence",
                ),
            }
            report.package_plan.disposition
                == crate::debian_package_plan::PackagePlanDisposition::Ready
                && preflight.disposition
                    == RootlessPodmanPreflightDisposition::ReadyForSmokeVerification
                && preflight.packages.state == RootlessPodmanPreflightState::Matching
                && preflight.runner_account.state == RootlessPodmanPreflightState::Matching
                && preflight.runtime_directory.state == RootlessPodmanPreflightState::Matching
                && preflight.configuration.state == RootlessPodmanPreflightState::Matching
                && preflight
                    .executables
                    .iter()
                    .all(|executable| executable.state == RootlessPodmanPreflightState::Matching)
        }
    }
}

fn validate_package_component(
    state: RootlessPodmanPreflightState,
    has_package_action: bool,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    match state {
        RootlessPodmanPreflightState::Matching => {}
        RootlessPodmanPreflightState::Absent if has_package_action => {}
        RootlessPodmanPreflightState::Absent => push_blocker(
            blockers,
            HostPreparationResource::DebianPackages,
            HostPreparationBlockerCode::UnsupportedMutation,
            "rootless Podman package evidence is absent without a mapped action",
        ),
        RootlessPodmanPreflightState::Unknown => push_blocker(
            blockers,
            HostPreparationResource::DebianPackages,
            HostPreparationBlockerCode::NeedsInspection,
            "rootless Podman package evidence remains incomplete",
        ),
        RootlessPodmanPreflightState::Conflicting | RootlessPodmanPreflightState::Blocked => {
            push_blocker(
                blockers,
                HostPreparationResource::DebianPackages,
                HostPreparationBlockerCode::ConflictingEvidence,
                "rootless Podman package evidence conflicts with reviewed policy",
            );
        }
    }
}

fn validate_unmapped_component(
    state: RootlessPodmanPreflightState,
    label: &str,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    match state {
        RootlessPodmanPreflightState::Matching => {}
        RootlessPodmanPreflightState::Absent => push_blocker(
            blockers,
            HostPreparationResource::RootlessPodman,
            HostPreparationBlockerCode::UnsupportedMutation,
            &format!("{label} preparation has no action in this planner slice"),
        ),
        RootlessPodmanPreflightState::Unknown => push_blocker(
            blockers,
            HostPreparationResource::RootlessPodman,
            HostPreparationBlockerCode::NeedsInspection,
            &format!("{label} evidence remains incomplete"),
        ),
        RootlessPodmanPreflightState::Conflicting | RootlessPodmanPreflightState::Blocked => {
            push_blocker(
                blockers,
                HostPreparationResource::RootlessPodman,
                HostPreparationBlockerCode::ConflictingEvidence,
                &format!("{label} evidence conflicts with reviewed policy"),
            );
        }
    }
}

fn validate_executables(
    report: &HostReadinessReport,
    preflight: &RootlessPodmanStaticPreflightReport,
    has_package_action: bool,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    let expected = ROOTLESS_EXECUTABLES.into_iter().collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for observation in &preflight.executables {
        let Some(expected_path) = expected.get(observation.name.as_str()) else {
            inconsistent(
                blockers,
                HostPreparationResource::RootlessPodman,
                "the rootless Podman preflight contains an unsupported executable",
            );
            continue;
        };
        let path_matches = if observation.name == "env" {
            is_supported_environment_executable_path(&observation.path)
        } else {
            observation.path == Path::new(*expected_path)
        };
        if !seen.insert(observation.name.as_str()) || !path_matches {
            inconsistent(
                blockers,
                HostPreparationResource::RootlessPodman,
                "the rootless Podman executable set contains a duplicate or unexpected path",
            );
            continue;
        }
        match observation.state {
            RootlessPodmanPreflightState::Matching => {}
            RootlessPodmanPreflightState::Absent
                if has_package_action
                    && executable_absence_is_explained(report, &observation.name) => {}
            RootlessPodmanPreflightState::Absent => push_blocker(
                blockers,
                HostPreparationResource::RootlessPodman,
                HostPreparationBlockerCode::UnsupportedMutation,
                "a rootless Podman executable is absent without a mapped package action",
            ),
            RootlessPodmanPreflightState::Unknown => push_blocker(
                blockers,
                HostPreparationResource::RootlessPodman,
                HostPreparationBlockerCode::NeedsInspection,
                "rootless Podman executable metadata remains unknown",
            ),
            RootlessPodmanPreflightState::Conflicting | RootlessPodmanPreflightState::Blocked => {
                push_blocker(
                    blockers,
                    HostPreparationResource::RootlessPodman,
                    HostPreparationBlockerCode::ConflictingEvidence,
                    "rootless Podman executable metadata conflicts with reviewed policy",
                )
            }
        }
    }
    if seen.len() != expected.len() {
        inconsistent(
            blockers,
            HostPreparationResource::RootlessPodman,
            "the rootless Podman executable observation set is incomplete",
        );
    }
}

fn executable_absence_is_explained(report: &HostReadinessReport, name: &str) -> bool {
    match name {
        "podman" => package_is_missing(report, "podman"),
        "newuidmap" | "newgidmap" => package_is_missing(report, "uidmap"),
        "slirp4netns" => package_is_missing(report, "slirp4netns"),
        "fuse-overlayfs" => package_is_missing(report, "fuse-overlayfs"),
        "runuser" | "env" | "systemctl" => false,
        _ => false,
    }
}

const fn assessment_state(
    state: RootlessPodmanConfigAssessmentState,
) -> RootlessPodmanPreflightState {
    match state {
        RootlessPodmanConfigAssessmentState::Matching => RootlessPodmanPreflightState::Matching,
        RootlessPodmanConfigAssessmentState::Absent => RootlessPodmanPreflightState::Absent,
        RootlessPodmanConfigAssessmentState::Unknown => RootlessPodmanPreflightState::Unknown,
        RootlessPodmanConfigAssessmentState::Conflicting => {
            RootlessPodmanPreflightState::Conflicting
        }
    }
}
