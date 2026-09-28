use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::debian_package_plan::{build_package_plan, parse_os_release};
use crate::host::Presence;
use crate::host_readiness::{
    ExactExecutableObservation, HostObservationState, HostReadinessReport, RunnerAccountReadiness,
};
use crate::host_rootless_podman::HostRootlessPodmanReadiness;
use crate::journal::{ExecutionLane, RollbackClass};
use crate::lane_command::{LaneCommand, LaneCommandKind, LinuxAccountName};
use crate::rootless_podman_config_observation::{
    ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION, RootlessPodmanConfigObservationReport,
};
use crate::rootless_podman_config_resolution::{
    ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION, RootlessPodmanConfigAssessment,
    RootlessPodmanConfigAssessmentState,
};
use crate::rootless_podman_preflight::{
    ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION, RootlessPodmanExecutableObservation,
    RootlessPodmanPreflightDisposition, RootlessPodmanPreflightObservation,
    RootlessPodmanPreflightState, RootlessPodmanStaticPreflightReport,
};
use crate::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservation,
    PreparationObservationState, RunnerAccountObservations, build_runner_account_plan,
};
use crate::subordinate_id::{
    PodmanMigrationPlan, SubordinateIdKind, build_exact_subordinate_id_plan,
};

use super::{
    DeferredActionReason, FreshObservationRequirement, HostPreparationBlockerCode,
    HostPreparationResource, HostPreparationResult, plan_host_preparation, render_human,
};

fn account(value: &str) -> LinuxAccountName {
    LinuxAccountName::parse(value).expect("valid account")
}

fn desired() -> DesiredRunnerAccount {
    DesiredRunnerAccount::new(
        account("project-runner"),
        account("project-runner"),
        "/var/lib/project-runner",
        PlannedSubordinateRange::new(100_000, 65_536).expect("UID range"),
        PlannedSubordinateRange::new(200_000, 65_536).expect("GID range"),
    )
    .expect("desired account")
}

fn observation(state: PreparationObservationState, label: &str) -> PreparationObservation {
    PreparationObservation::new(state, [format!("observed {label}")]).expect("bounded observation")
}

fn observations(
    group: PreparationObservationState,
    user: PreparationObservationState,
    home: PreparationObservationState,
    uid: PreparationObservationState,
    gid: PreparationObservationState,
    linger: PreparationObservationState,
) -> RunnerAccountObservations {
    RunnerAccountObservations {
        group: observation(group, "group"),
        user: observation(user, "user"),
        home: observation(home, "home"),
        subordinate_uids: observation(uid, "subordinate UIDs"),
        subordinate_gids: observation(gid, "subordinate GIDs"),
        linger: observation(linger, "linger"),
    }
}

fn all(state: PreparationObservationState) -> RunnerAccountObservations {
    observations(state, state, state, state, state, state)
}

fn packages(overrides: &[(&str, Presence)]) -> crate::debian_package_plan::DebianPackagePlan {
    let mut inventory = [
        "git",
        "podman",
        "uidmap",
        "slirp4netns",
        "fuse-overlayfs",
        "dbus-user-session",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), Presence::Present))
    .collect::<BTreeMap<_, _>>();
    for (name, state) in overrides {
        inventory.insert((*name).to_owned(), *state);
    }
    build_package_plan(
        parse_os_release("ID=debian\nVERSION_ID=\"12\"\n").expect("distribution"),
        &inventory,
    )
    .expect("package plan")
}

fn exact_executables(
    package_plan: &crate::debian_package_plan::DebianPackagePlan,
) -> Vec<ExactExecutableObservation> {
    [
        ("git", "/usr/bin/git"),
        ("podman", "/usr/bin/podman"),
        ("systemctl", "/usr/bin/systemctl"),
    ]
    .into_iter()
    .map(|(name, path)| {
        let state = if package_plan
            .missing_packages
            .iter()
            .any(|package| package.as_str() == name)
        {
            HostObservationState::Absent
        } else {
            HostObservationState::Matching
        };
        ExactExecutableObservation {
            name: name.to_owned(),
            path: PathBuf::from(path),
            state,
            evidence: vec![format!("{name} sentinel evidence")],
        }
    })
    .collect()
}

fn rootless_executables(
    package_plan: &crate::debian_package_plan::DebianPackagePlan,
) -> Vec<RootlessPodmanExecutableObservation> {
    [
        ("podman", "/usr/bin/podman", "podman"),
        ("runuser", "/usr/sbin/runuser", ""),
        ("env", "/usr/bin/env", ""),
        ("systemctl", "/usr/bin/systemctl", ""),
        ("newuidmap", "/usr/bin/newuidmap", "uidmap"),
        ("newgidmap", "/usr/bin/newgidmap", "uidmap"),
        ("slirp4netns", "/usr/bin/slirp4netns", "slirp4netns"),
        (
            "fuse-overlayfs",
            "/usr/bin/fuse-overlayfs",
            "fuse-overlayfs",
        ),
    ]
    .into_iter()
    .map(
        |(name, path, package)| RootlessPodmanExecutableObservation {
            name: name.to_owned(),
            path: PathBuf::from(path),
            state: if !package.is_empty()
                && package_plan
                    .missing_packages
                    .iter()
                    .any(|candidate| candidate.as_str() == package)
            {
                RootlessPodmanPreflightState::Absent
            } else {
                RootlessPodmanPreflightState::Matching
            },
            evidence: vec![format!("{name} reviewed")],
        },
    )
    .collect()
}

fn rootless_ready(
    package_plan: &crate::debian_package_plan::DebianPackagePlan,
) -> HostRootlessPodmanReadiness {
    let package_state =
        if package_plan.disposition == crate::debian_package_plan::PackagePlanDisposition::Ready {
            RootlessPodmanPreflightState::Matching
        } else {
            RootlessPodmanPreflightState::Absent
        };
    let disposition = if package_state == RootlessPodmanPreflightState::Matching {
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification
    } else {
        RootlessPodmanPreflightDisposition::ChangesRequired
    };
    HostRootlessPodmanReadiness::Observed {
        configuration: Box::new(RootlessPodmanConfigObservationReport {
            schema_version: ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION,
            sources: Vec::new(),
            assessment: RootlessPodmanConfigAssessment {
                schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
                state: RootlessPodmanConfigAssessmentState::Matching,
                fields: Vec::new(),
            },
        }),
        preflight: Box::new(RootlessPodmanStaticPreflightReport {
            schema_version: ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION,
            disposition,
            packages: preflight_observation(package_state),
            runner_account: preflight_observation(RootlessPodmanPreflightState::Matching),
            runtime_directory: preflight_observation(RootlessPodmanPreflightState::Matching),
            configuration: preflight_observation(RootlessPodmanPreflightState::Matching),
            executables: rootless_executables(package_plan),
        }),
    }
}

fn preflight_observation(
    state: RootlessPodmanPreflightState,
) -> RootlessPodmanPreflightObservation {
    RootlessPodmanPreflightObservation {
        state,
        evidence: vec!["normalized preflight evidence".to_owned()],
    }
}

fn report(
    package_plan: crate::debian_package_plan::DebianPackagePlan,
    observations: RunnerAccountObservations,
    identity: Option<(u32, u32)>,
) -> HostReadinessReport {
    let desired = desired();
    let account_plan =
        build_runner_account_plan(desired.clone(), observations.clone()).expect("account plan");
    let subordinate_ids = build_exact_subordinate_id_plan(
        &desired,
        &observations,
        identity,
        std::path::Path::new("/etc/subuid"),
        std::path::Path::new("/etc/subgid"),
    )
    .expect("subordinate plan");
    let states = [
        observations.group.state(),
        observations.user.state(),
        observations.home.state(),
        observations.subordinate_uids.state(),
        observations.subordinate_gids.state(),
        observations.linger.state(),
    ];
    let all_matching = states
        .iter()
        .all(|state| *state == PreparationObservationState::Matching);
    let rootless_podman = if all_matching {
        rootless_ready(&package_plan)
    } else {
        let state = if states.contains(&PreparationObservationState::Conflicting) {
            RootlessPodmanPreflightState::Conflicting
        } else if states.contains(&PreparationObservationState::Unknown) {
            RootlessPodmanPreflightState::Unknown
        } else {
            RootlessPodmanPreflightState::Absent
        };
        HostRootlessPodmanReadiness::Deferred {
            state,
            evidence: vec!["deferred sentinel evidence".to_owned()],
        }
    };
    HostReadinessReport {
        schema_version: crate::host_readiness::HOST_READINESS_SCHEMA_VERSION,
        repository: "owner/repository".to_owned(),
        executables: exact_executables(&package_plan),
        package_plan,
        rootless_podman,
        runner_account: RunnerAccountReadiness::Planned {
            observations: Box::new(observations),
            plan: account_plan,
            subordinate_ids: Box::new(subordinate_ids),
        },
    }
}

#[test]
fn matching_report_is_ready_and_retains_exact_source() {
    let report = report(
        packages(&[]),
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    );
    let proposal = plan_host_preparation(report.clone());
    assert!(matches!(proposal.result, HostPreparationResult::Ready));
    assert_eq!(proposal.source.report(), &report);
    assert_eq!(proposal.source.identity.schema_version, 1);
}

#[test]
fn reviewed_rust_coreutils_env_path_is_ready() {
    let mut report = report(
        packages(&[]),
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    );
    let HostRootlessPodmanReadiness::Observed { preflight, .. } = &mut report.rootless_podman
    else {
        panic!("observed rootless preflight expected");
    };
    let environment = preflight
        .executables
        .iter_mut()
        .find(|executable| executable.name == "env")
        .expect("environment executable");
    environment.path = PathBuf::from("/usr/lib/cargo/bin/coreutils/env");

    let proposal = plan_host_preparation(report);
    assert!(matches!(proposal.result, HostPreparationResult::Ready));
}

#[test]
fn unreviewed_environment_path_blocks_without_leaking_the_path() {
    let mut report = report(
        packages(&[]),
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    );
    let HostRootlessPodmanReadiness::Observed { preflight, .. } = &mut report.rootless_podman
    else {
        panic!("observed rootless preflight expected");
    };
    let environment = preflight
        .executables
        .iter_mut()
        .find(|executable| executable.name == "env")
        .expect("environment executable");
    environment.path = PathBuf::from("/tmp/private-env-marker");

    let proposal = plan_host_preparation(report);
    let HostPreparationResult::Blocked { blockers } = &proposal.result else {
        panic!("unreviewed environment path must block");
    };
    assert!(blockers.iter().any(|blocker| {
        blocker.resource == HostPreparationResource::RootlessPodman
            && blocker.code == HostPreparationBlockerCode::InconsistentObservation
    }));
    let json = serde_json::to_string(&proposal).expect("serialize proposal");
    let human = render_human(&proposal);
    assert!(!json.contains("private-env-marker"));
    assert!(!human.contains("private-env-marker"));
}

#[test]
fn package_only_phase_is_root_irreversible_and_durable() {
    let proposal = plan_host_preparation(report(
        packages(&[("git", Presence::Absent)]),
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    ));
    let HostPreparationResult::Executable {
        phase,
        continuation_barriers,
        deferred_actions,
    } = &proposal.result
    else {
        panic!("package phase expected");
    };
    assert_eq!(phase.id, "host-preparation-root-phase");
    assert_eq!(phase.actions.len(), 1);
    assert_eq!(phase.actions[0].id, "install-debian-host-prerequisites");
    assert_eq!(phase.actions[0].lane, ExecutionLane::Root);
    assert_eq!(phase.actions[0].command_kind, LaneCommandKind::AptInstall);
    assert_eq!(phase.actions[0].rollback, RollbackClass::Irreversible);
    assert!(phase.actions[0].depends_on.is_empty());
    assert!(continuation_barriers.is_empty());
    assert!(deferred_actions.is_empty());
    let _accepted = phase.durable_plan();
}

#[test]
fn account_creation_has_stable_order_and_dependencies() {
    let proposal = plan_host_preparation(report(
        packages(&[]),
        all(PreparationObservationState::Absent),
        None,
    ));
    let HostPreparationResult::Executable { phase, .. } = &proposal.result else {
        panic!("account phase expected");
    };
    assert_eq!(
        phase
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        [
            "ensure-runner-group",
            "ensure-runner-user",
            "ensure-runner-home",
            "enable-runner-linger",
            "ensure-runner-subordinate-uids",
            "ensure-runner-subordinate-gids",
        ]
    );
    assert_eq!(phase.actions[1].depends_on, ["ensure-runner-group"]);
    assert_eq!(phase.actions[2].depends_on, ["ensure-runner-user"]);
    assert_eq!(phase.actions[3].depends_on, ["ensure-runner-user"]);
    assert_eq!(phase.actions[4].depends_on, ["ensure-runner-user"]);
    assert_eq!(
        phase.actions[5].depends_on,
        ["ensure-runner-user", "ensure-runner-subordinate-uids"]
    );
    assert!(phase.actions.iter().all(|action| {
        action.lane == ExecutionLane::Root && action.rollback == RollbackClass::Irreversible
    }));
}

#[test]
fn mapping_change_stops_before_migration_and_requires_both_authorities() {
    let observations = observations(
        PreparationObservationState::Matching,
        PreparationObservationState::Matching,
        PreparationObservationState::Matching,
        PreparationObservationState::Absent,
        PreparationObservationState::Matching,
        PreparationObservationState::Matching,
    );
    let proposal = plan_host_preparation(report(packages(&[]), observations, Some((1001, 1001))));
    let HostPreparationResult::Executable {
        phase,
        continuation_barriers,
        deferred_actions,
    } = &proposal.result
    else {
        panic!("mapping phase expected");
    };
    assert_eq!(
        phase
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        ["ensure-runner-subordinate-uids"]
    );
    assert!(
        phase
            .actions
            .iter()
            .all(|action| action.lane == ExecutionLane::Root)
    );
    assert_eq!(continuation_barriers.len(), 1);
    assert_eq!(
        continuation_barriers[0].after_action_ids,
        ["ensure-runner-subordinate-uids"]
    );
    assert_eq!(continuation_barriers[0].requirements.len(), 4);
    assert!(
        continuation_barriers[0]
            .requirements
            .iter()
            .any(|requirement| {
                matches!(
                    requirement,
                    FreshObservationRequirement::SubordinateAuthority {
                        authority: SubordinateIdKind::Uid,
                        path,
                        ..
                    } if path == "/etc/subuid"
                )
            })
    );
    assert!(
        continuation_barriers[0]
            .requirements
            .iter()
            .any(|requirement| {
                matches!(
                    requirement,
                    FreshObservationRequirement::SubordinateAuthority {
                        authority: SubordinateIdKind::Gid,
                        path,
                        ..
                    } if path == "/etc/subgid"
                )
            })
    );
    assert_eq!(deferred_actions.len(), 1);
    assert_eq!(
        deferred_actions[0].id,
        "migrate-runner-podman-after-subordinate-id-change"
    );
    assert_eq!(deferred_actions[0].lane, ExecutionLane::RunnerUser);
    assert_eq!(
        deferred_actions[0].reason,
        DeferredActionReason::FreshObservationRequired
    );
    assert!(
        phase
            .actions
            .iter()
            .all(|action| action.command_kind != LaneCommandKind::RunnerPodmanMigrate)
    );
}

#[test]
fn freshly_matching_report_may_emit_migration_only() {
    let changed_observations = observations(
        PreparationObservationState::Matching,
        PreparationObservationState::Matching,
        PreparationObservationState::Matching,
        PreparationObservationState::Absent,
        PreparationObservationState::Matching,
        PreparationObservationState::Matching,
    );
    let mut changed = report(packages(&[]), changed_observations, Some((1001, 1001)));
    let migration = match &mut changed.runner_account {
        RunnerAccountReadiness::Planned {
            subordinate_ids, ..
        } => std::mem::replace(
            &mut subordinate_ids.podman_migration,
            PodmanMigrationPlan::NotRequired,
        ),
        RunnerAccountReadiness::NeedsConfiguration { .. } => unreachable!(),
    };
    let mut matching = report(
        packages(&[]),
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    );
    match &mut matching.runner_account {
        RunnerAccountReadiness::Planned {
            subordinate_ids, ..
        } => subordinate_ids.podman_migration = migration,
        RunnerAccountReadiness::NeedsConfiguration { .. } => unreachable!(),
    }
    let proposal = plan_host_preparation(matching);
    let HostPreparationResult::Executable {
        phase,
        continuation_barriers,
        deferred_actions,
    } = &proposal.result
    else {
        panic!("migration phase expected");
    };
    assert_eq!(phase.id, "host-preparation-runner-migration-phase");
    assert_eq!(phase.actions.len(), 1);
    assert_eq!(
        phase.actions[0].id,
        "migrate-runner-podman-after-subordinate-id-change"
    );
    assert_eq!(phase.actions[0].lane, ExecutionLane::RunnerUser);
    assert_eq!(
        phase.actions[0].command_kind,
        LaneCommandKind::RunnerPodmanMigrate
    );
    assert!(continuation_barriers.is_empty());
    assert!(deferred_actions.is_empty());
}

#[test]
fn matching_mappings_have_no_barrier_or_deferred_work() {
    let proposal = plan_host_preparation(report(
        packages(&[]),
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    ));
    assert!(matches!(proposal.result, HostPreparationResult::Ready));
}

#[test]
fn unknown_and_conflicting_evidence_fail_closed() {
    let unknown = plan_host_preparation(report(
        packages(&[("git", Presence::Unknown)]),
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    ));
    let HostPreparationResult::Blocked { blockers } = unknown.result else {
        panic!("unknown package state must block");
    };
    assert!(blockers.iter().any(|blocker| {
        blocker.resource == HostPreparationResource::DebianPackages
            && blocker.code == HostPreparationBlockerCode::NeedsInspection
    }));

    let conflicting = plan_host_preparation(report(
        packages(&[]),
        observations(
            PreparationObservationState::Conflicting,
            PreparationObservationState::Absent,
            PreparationObservationState::Absent,
            PreparationObservationState::Absent,
            PreparationObservationState::Absent,
            PreparationObservationState::Absent,
        ),
        None,
    ));
    let HostPreparationResult::Blocked { blockers } = conflicting.result else {
        panic!("conflicting account state must block");
    };
    assert!(
        blockers
            .iter()
            .any(|blocker| blocker.code == HostPreparationBlockerCode::ConflictingEvidence)
    );
}

#[test]
fn invalid_command_binding_is_rejected() {
    let mut report = report(
        packages(&[("git", Presence::Absent)]),
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    );
    let mutation = report
        .package_plan
        .mutation
        .as_ref()
        .expect("package mutation");
    report.package_plan.command = Some(
        LaneCommand::ensure_system_group(mutation, &account("project-runner"))
            .expect("safe but wrong command kind"),
    );
    let proposal = plan_host_preparation(report);
    let HostPreparationResult::Blocked { blockers } = proposal.result else {
        panic!("invalid binding must block");
    };
    assert!(blockers.iter().any(|blocker| {
        blocker.resource == HostPreparationResource::DebianPackages
            && blocker.code == HostPreparationBlockerCode::InvalidCommandBinding
    }));
}

#[test]
fn json_and_human_output_exclude_commands_environment_and_raw_evidence() {
    let observations = observations(
        PreparationObservationState::Matching,
        PreparationObservationState::Matching,
        PreparationObservationState::Matching,
        PreparationObservationState::Absent,
        PreparationObservationState::Matching,
        PreparationObservationState::Matching,
    );
    let mut report = report(packages(&[]), observations, Some((1001, 1001)));
    report.executables[0].evidence = vec!["PRIVATE_RAW_SENTINEL".to_owned()];
    let proposal = plan_host_preparation(report);
    let json = serde_json::to_string(&proposal).expect("serialize proposal");
    let human = render_human(&proposal);
    for forbidden in [
        "PRIVATE_RAW_SENTINEL",
        "/usr/sbin/runuser",
        "HOME=",
        "XDG_RUNTIME_DIR=",
        "--add-subuids",
        "environment",
    ] {
        assert!(!json.contains(forbidden), "JSON leaked {forbidden}");
        assert!(
            !human.contains(forbidden),
            "human output leaked {forbidden}"
        );
    }
}
