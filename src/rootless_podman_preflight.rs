use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::debian_package_plan::{DebianPackagePlan, PackagePlanDisposition};
use crate::lane_executable::{
    is_supported_environment_executable_path, resolve_reviewed_environment_executable,
};
use crate::rootless_podman_config_resolution::{
    RootlessPodmanConfigAssessment, RootlessPodmanConfigAssessmentState, RootlessPodmanConfigField,
};
use crate::runner_account_observation::ObservedRunnerIdentity;
use crate::runner_account_plan::{PreparationObservationState, RunnerAccountObservations};

mod support;
#[cfg(test)]
mod tests;

use support::{
    ExecutableProbe, LinuxRuntimeFilesystem, RuntimeFilesystem, RuntimeIdentity, RuntimePathKind,
    RuntimePathObservation, canonical_non_root_path, verify_reviewed_executable,
};

pub const ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION: u8 = 1;

const ENV_COMPATIBILITY_PATH: &str = "/usr/bin/env";

#[derive(Clone, Copy)]
enum ReviewedExecutablePath {
    Fixed(&'static str),
    Environment,
}

const REVIEWED_EXECUTABLES: [(&str, ReviewedExecutablePath); 8] = [
    ("podman", ReviewedExecutablePath::Fixed("/usr/bin/podman")),
    (
        "runuser",
        ReviewedExecutablePath::Fixed("/usr/sbin/runuser"),
    ),
    ("env", ReviewedExecutablePath::Environment),
    (
        "systemctl",
        ReviewedExecutablePath::Fixed("/usr/bin/systemctl"),
    ),
    (
        "newuidmap",
        ReviewedExecutablePath::Fixed("/usr/bin/newuidmap"),
    ),
    (
        "newgidmap",
        ReviewedExecutablePath::Fixed("/usr/bin/newgidmap"),
    ),
    (
        "slirp4netns",
        ReviewedExecutablePath::Fixed("/usr/bin/slirp4netns"),
    ),
    (
        "fuse-overlayfs",
        ReviewedExecutablePath::Fixed("/usr/bin/fuse-overlayfs"),
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanPreflightState {
    Matching,
    Absent,
    Unknown,
    Conflicting,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanPreflightDisposition {
    ReadyForSmokeVerification,
    ChangesRequired,
    NeedsInspection,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanPreflightObservation {
    pub state: RootlessPodmanPreflightState,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanExecutableObservation {
    pub name: String,
    pub path: PathBuf,
    pub state: RootlessPodmanPreflightState,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanStaticPreflightReport {
    pub schema_version: u8,
    pub disposition: RootlessPodmanPreflightDisposition,
    pub packages: RootlessPodmanPreflightObservation,
    pub runner_account: RootlessPodmanPreflightObservation,
    pub runtime_directory: RootlessPodmanPreflightObservation,
    pub configuration: RootlessPodmanPreflightObservation,
    pub executables: Vec<RootlessPodmanExecutableObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanPreflightPaths {
    runtime_root: PathBuf,
}

impl RootlessPodmanPreflightPaths {
    #[must_use]
    pub fn system_default() -> Self {
        Self {
            runtime_root: "/run/user".into(),
        }
    }

    /// Build relocated static-preflight paths for an explicitly trusted host root.
    ///
    /// # Errors
    ///
    /// Returns an error unless the runtime root is a canonical non-root absolute path.
    pub fn new(runtime_root: impl Into<PathBuf>) -> Result<Self, String> {
        let runtime_root = canonical_non_root_path(runtime_root.into())?;
        Ok(Self { runtime_root })
    }

    #[must_use]
    pub fn runtime_directory(&self, uid: u32) -> PathBuf {
        self.runtime_root.join(uid.to_string())
    }
}

/// Inspect non-mutating prerequisites for a later rootless Podman smoke verification.
///
/// This function never executes Podman or another child process. It classifies the existing package
/// plan, runner-account evidence, exact helper metadata, runner runtime directory, and a pure
/// reviewed configuration assessment. A matching report proves only that an explicit, journaled
/// first-run smoke action may be planned.
#[must_use]
pub fn observe_rootless_podman_static_preflight(
    package_plan: &DebianPackagePlan,
    account_observations: &RunnerAccountObservations,
    identity: Option<ObservedRunnerIdentity>,
    configuration: &RootlessPodmanConfigAssessment,
    paths: &RootlessPodmanPreflightPaths,
) -> RootlessPodmanStaticPreflightReport {
    let runtime_identity = identity.map(|identity| RuntimeIdentity {
        uid: identity.uid(),
    });
    observe_with_environment(
        package_plan,
        account_observations,
        runtime_identity,
        configuration,
        paths,
        (
            &LinuxRuntimeFilesystem,
            &verify_reviewed_executable,
            &|| {
                resolve_reviewed_environment_executable()
                    .map(|executable| executable.path().to_path_buf())
                    .map_err(|_| ())
            },
        ),
    )
}

fn observe_with_environment<F, P, E>(
    package_plan: &DebianPackagePlan,
    account_observations: &RunnerAccountObservations,
    identity: Option<RuntimeIdentity>,
    configuration_assessment: &RootlessPodmanConfigAssessment,
    paths: &RootlessPodmanPreflightPaths,
    dependencies: (&F, &P, &E),
) -> RootlessPodmanStaticPreflightReport
where
    F: RuntimeFilesystem,
    P: Fn(&Path) -> ExecutableProbe,
    E: Fn() -> Result<PathBuf, ()>,
{
    let (filesystem, executable_probe, environment_path) = dependencies;
    let packages = classify_packages(package_plan);
    let runner_account = classify_runner_account(account_observations, identity);
    let runtime_directory = classify_runtime_directory(identity, paths, filesystem);
    let configuration = classify_configuration(configuration_assessment);
    let executables = REVIEWED_EXECUTABLES
        .into_iter()
        .map(|(name, reviewed_path)| {
            let path = match reviewed_path {
                ReviewedExecutablePath::Fixed(path) => PathBuf::from(path),
                ReviewedExecutablePath::Environment => match environment_path() {
                    Ok(path) if is_supported_environment_executable_path(&path) => path,
                    Ok(_) | Err(()) => {
                        return RootlessPodmanExecutableObservation {
                            name: name.to_owned(),
                            path: PathBuf::from(ENV_COMPATIBILITY_PATH),
                            state: RootlessPodmanPreflightState::Conflicting,
                            evidence: vec![
                                "no supported reviewed environment executable is available"
                                    .to_owned(),
                            ],
                        };
                    }
                },
            };
            let probe = executable_probe(&path);
            RootlessPodmanExecutableObservation {
                name: name.to_owned(),
                path,
                state: probe.state,
                evidence: probe.evidence,
            }
        })
        .collect::<Vec<_>>();

    let disposition = disposition(
        [
            &packages,
            &runner_account,
            &runtime_directory,
            &configuration,
        ]
        .into_iter()
        .map(|observation| observation.state)
        .chain(executables.iter().map(|observation| observation.state)),
    );

    RootlessPodmanStaticPreflightReport {
        schema_version: ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION,
        disposition,
        packages,
        runner_account,
        runtime_directory,
        configuration,
        executables,
    }
}

#[cfg(test)]
fn observe_with(
    package_plan: &DebianPackagePlan,
    account_observations: &RunnerAccountObservations,
    identity: Option<RuntimeIdentity>,
    configuration_assessment: &RootlessPodmanConfigAssessment,
    paths: &RootlessPodmanPreflightPaths,
    filesystem: &impl RuntimeFilesystem,
    executable_probe: &impl Fn(&Path) -> ExecutableProbe,
) -> RootlessPodmanStaticPreflightReport {
    observe_with_environment(
        package_plan,
        account_observations,
        identity,
        configuration_assessment,
        paths,
        (filesystem, executable_probe, &|| {
            Ok(PathBuf::from(ENV_COMPATIBILITY_PATH))
        }),
    )
}

fn classify_packages(package_plan: &DebianPackagePlan) -> RootlessPodmanPreflightObservation {
    match package_plan.disposition {
        PackagePlanDisposition::Ready => observation(
            RootlessPodmanPreflightState::Matching,
            "all reviewed rootless Podman prerequisite packages are present",
        ),
        PackagePlanDisposition::Required => observation(
            RootlessPodmanPreflightState::Absent,
            format!(
                "reviewed prerequisite packages are absent: {}",
                package_plan
                    .missing_packages
                    .iter()
                    .map(|package| package.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        PackagePlanDisposition::NeedsInspection => observation(
            RootlessPodmanPreflightState::Unknown,
            format!(
                "reviewed prerequisite package state is unknown: {}",
                package_plan
                    .unknown_packages
                    .iter()
                    .map(|package| package.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
    }
}

fn classify_runner_account(
    observations: &RunnerAccountObservations,
    identity: Option<RuntimeIdentity>,
) -> RootlessPodmanPreflightObservation {
    let states = [
        observations.group.state(),
        observations.user.state(),
        observations.home.state(),
        observations.subordinate_uids.state(),
        observations.subordinate_gids.state(),
        observations.linger.state(),
    ];

    if states.contains(&PreparationObservationState::Conflicting) {
        observation(
            RootlessPodmanPreflightState::Conflicting,
            "runner account preparation evidence conflicts with the reviewed policy",
        )
    } else if states.contains(&PreparationObservationState::Unknown) {
        observation(
            RootlessPodmanPreflightState::Unknown,
            "runner account preparation evidence is incomplete",
        )
    } else if states.contains(&PreparationObservationState::Absent) {
        observation(
            RootlessPodmanPreflightState::Absent,
            "runner account preparation is not complete",
        )
    } else if identity.is_none() {
        observation(
            RootlessPodmanPreflightState::Unknown,
            "matching runner account observations did not include a resolved non-root identity",
        )
    } else {
        observation(
            RootlessPodmanPreflightState::Matching,
            "runner account, home, subordinate IDs, and linger match the reviewed policy",
        )
    }
}

fn classify_runtime_directory(
    identity: Option<RuntimeIdentity>,
    paths: &RootlessPodmanPreflightPaths,
    filesystem: &impl RuntimeFilesystem,
) -> RootlessPodmanPreflightObservation {
    let Some(identity) = identity else {
        return observation(
            RootlessPodmanPreflightState::Blocked,
            "runtime-directory inspection is blocked until the exact runner UID is known",
        );
    };
    if identity.uid == 0 {
        return observation(
            RootlessPodmanPreflightState::Conflicting,
            "runner runtime directory cannot be associated with UID zero",
        );
    }

    let path = paths.runtime_directory(identity.uid);
    match filesystem.inspect(&path) {
        RuntimePathObservation::Missing => observation(
            RootlessPodmanPreflightState::Absent,
            format!("runner runtime directory {} is absent", path.display()),
        ),
        RuntimePathObservation::Unknown => observation(
            RootlessPodmanPreflightState::Unknown,
            format!(
                "runner runtime directory {} could not be inspected safely",
                path.display()
            ),
        ),
        RuntimePathObservation::Present(metadata)
            if metadata.kind == RuntimePathKind::Directory
                && metadata.uid == identity.uid
                && metadata.mode & 0o022 == 0 =>
        {
            observation(
                RootlessPodmanPreflightState::Matching,
                format!(
                    "runner runtime directory {} is owned by UID {} and is not writable by group or others",
                    path.display(),
                    identity.uid
                ),
            )
        }
        RuntimePathObservation::Present(_) => observation(
            RootlessPodmanPreflightState::Conflicting,
            format!(
                "runner runtime directory {} has incompatible type, ownership, or mode",
                path.display()
            ),
        ),
    }
}

fn classify_configuration(
    assessment: &RootlessPodmanConfigAssessment,
) -> RootlessPodmanPreflightObservation {
    let derived_state = assessment.fields.iter().map(|field| field.state).max();
    if derived_state.is_some_and(|state| state != assessment.state) {
        return observation(
            RootlessPodmanPreflightState::Conflicting,
            "rootless Podman configuration assessment summary conflicts with its field results",
        );
    }
    let state = match assessment.state {
        RootlessPodmanConfigAssessmentState::Matching => RootlessPodmanPreflightState::Matching,
        RootlessPodmanConfigAssessmentState::Absent => RootlessPodmanPreflightState::Absent,
        RootlessPodmanConfigAssessmentState::Unknown => RootlessPodmanPreflightState::Unknown,
        RootlessPodmanConfigAssessmentState::Conflicting => {
            RootlessPodmanPreflightState::Conflicting
        }
    };
    let mut evidence = assessment
        .fields
        .iter()
        .filter(|field| field.state != RootlessPodmanConfigAssessmentState::Matching)
        .map(|field| {
            format!(
                "{} is {} for the reviewed rootless Podman policy",
                configuration_field_name(field.field),
                configuration_state_name(field.state)
            )
        })
        .collect::<Vec<_>>();
    if evidence.is_empty() {
        evidence.push(match state {
            RootlessPodmanPreflightState::Matching => {
                "all reviewed rootless Podman configuration fields match explicit policy".to_owned()
            }
            RootlessPodmanPreflightState::Absent => {
                "required rootless Podman configuration is absent".to_owned()
            }
            RootlessPodmanPreflightState::Unknown => {
                "rootless Podman configuration precedence could not be resolved safely".to_owned()
            }
            RootlessPodmanPreflightState::Conflicting => {
                "rootless Podman configuration conflicts with explicit policy".to_owned()
            }
            RootlessPodmanPreflightState::Blocked => unreachable!("configuration is never blocked"),
        });
    }
    RootlessPodmanPreflightObservation { state, evidence }
}

fn configuration_field_name(field: RootlessPodmanConfigField) -> &'static str {
    match field {
        RootlessPodmanConfigField::StorageDriver => "storage driver",
        RootlessPodmanConfigField::GraphRoot => "graph root",
        RootlessPodmanConfigField::RunRoot => "run root",
        RootlessPodmanConfigField::OverlayMountProgram => "overlay mount program",
        RootlessPodmanConfigField::CgroupManager => "cgroup manager",
        RootlessPodmanConfigField::NetworkBackend => "network backend",
    }
}

fn configuration_state_name(state: RootlessPodmanConfigAssessmentState) -> &'static str {
    match state {
        RootlessPodmanConfigAssessmentState::Matching => "matching",
        RootlessPodmanConfigAssessmentState::Absent => "absent",
        RootlessPodmanConfigAssessmentState::Unknown => "unknown",
        RootlessPodmanConfigAssessmentState::Conflicting => "conflicting",
    }
}

fn disposition(
    states: impl IntoIterator<Item = RootlessPodmanPreflightState>,
) -> RootlessPodmanPreflightDisposition {
    let states = states.into_iter().collect::<Vec<_>>();
    if states.contains(&RootlessPodmanPreflightState::Conflicting) {
        RootlessPodmanPreflightDisposition::Blocked
    } else if states.contains(&RootlessPodmanPreflightState::Unknown) {
        RootlessPodmanPreflightDisposition::NeedsInspection
    } else if states.contains(&RootlessPodmanPreflightState::Absent) {
        RootlessPodmanPreflightDisposition::ChangesRequired
    } else if states.contains(&RootlessPodmanPreflightState::Blocked) {
        RootlessPodmanPreflightDisposition::NeedsInspection
    } else {
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification
    }
}

fn observation(
    state: RootlessPodmanPreflightState,
    evidence: impl Into<String>,
) -> RootlessPodmanPreflightObservation {
    RootlessPodmanPreflightObservation {
        state,
        evidence: vec![evidence.into()],
    }
}
