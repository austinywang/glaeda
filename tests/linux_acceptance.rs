#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use glaeda::debian_package_probe::DpkgQueryProbe;
use glaeda::host::Presence;
use glaeda::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
use glaeda::lane_command::{LaneCommand, LinuxAccountName, PackageName, RunnerUserContext};
use glaeda::lane_executable::{resolve_reviewed_environment_executable, verify_executable};
use glaeda::lane_executor::{RootLaneExecutor, RunnerUserLaneExecutor};
use glaeda::process::{CommandExecutor, CommandSpec, ProcessExecutor};
use glaeda::runner_account_observation::{
    RunnerAccountObservationPaths, getent_command, observe_runner_account,
};
use glaeda::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservationState,
    RunnerAccountPlanDisposition, RunnerAccountResourceKind, build_runner_account_plan,
};
use glaeda::runner_user::{
    inspect_runtime_directory, parse_passwd_record, parse_subordinate_ranges, verify_runner_user,
};

const USERNAME: &str = "smolaccept";
const HOME: &str = "/var/lib/smolrunner-acceptance";
const SUBUID_START: u32 = 2_000_000;
const SUBGID_START: u32 = 2_100_000;
const SUBID_COUNT: u32 = 65_536;

fn acceptance_enabled() -> bool {
    env::var("SMOLRUNNER_LINUX_ACCEPTANCE").as_deref() == Ok("1")
}

fn account_name() -> LinuxAccountName {
    LinuxAccountName::parse(USERNAME).expect("acceptance account name is valid")
}

fn desired_account() -> DesiredRunnerAccount {
    DesiredRunnerAccount::new(
        account_name(),
        account_name(),
        HOME,
        PlannedSubordinateRange::new(SUBUID_START, SUBID_COUNT)
            .expect("acceptance subordinate UID range is valid"),
        PlannedSubordinateRange::new(SUBGID_START, SUBID_COUNT)
            .expect("acceptance subordinate GID range is valid"),
    )
    .expect("acceptance account is valid")
}

fn action(id: &str, lane: ExecutionLane) -> PlannedMutation {
    PlannedMutation::new(
        id,
        lane,
        format!("acceptance action {id}"),
        RollbackClass::Compensating,
        Preconditions::new(["disposable acceptance container"]),
    )
}

fn require_effective_root() {
    let status = fs::read_to_string("/proc/self/status").expect("read process status");
    let effective_uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u32>().ok())
        .expect("parse effective UID");
    assert_eq!(effective_uid, 0, "acceptance container must run as root");
}

fn prepare_empty_system_authorities() {
    assert!(
        Path::new("/.dockerenv").exists(),
        "refusing to reset /etc/subuid and /etc/subgid outside a disposable Docker container"
    );
    for path in ["/etc/subuid", "/etc/subgid"] {
        fs::write(path, "").unwrap_or_else(|error| panic!("reset {path}: {error}"));
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))
            .unwrap_or_else(|error| panic!("set permissions on {path}: {error}"));
    }
    fs::create_dir_all("/var/lib/systemd/linger").expect("create linger observation directory");
}

fn observed_os_id() -> String {
    let release = fs::read_to_string("/etc/os-release").expect("read operating-system identity");
    release
        .lines()
        .find_map(|line| line.strip_prefix("ID="))
        .map(|value| value.trim_matches('"').to_owned())
        .expect("operating-system identity contains ID")
}

#[test]
fn disposable_guest_matches_the_requested_distribution() {
    if !acceptance_enabled() {
        return;
    }

    let expected = env::var("SMOLRUNNER_EXPECTED_OS").expect("expected OS is supplied by harness");
    assert_eq!(observed_os_id(), expected);
}

#[test]
fn process_execution_starts_empty_and_adds_only_explicit_environment_values() {
    if !acceptance_enabled() {
        return;
    }

    let environment_program =
        resolve_reviewed_environment_executable().expect("resolve reviewed environment executable");
    let empty = ProcessExecutor
        .execute(&CommandSpec::new(environment_program.path()))
        .expect("execute env with an empty child environment");
    assert!(empty.success);
    assert!(empty.stdout.is_empty());
    assert!(empty.environment_keys.is_empty());

    let allowlisted_spec = CommandSpec::new(environment_program.path())
        .environment("HOME", "/var/empty")
        .environment("SMOLRUNNER_ACCEPTANCE", "allowed");
    let allowlisted = ProcessExecutor
        .execute(&allowlisted_spec)
        .expect("execute env with an allowlist");
    let lines = allowlisted.stdout.lines().collect::<BTreeSet<_>>();
    assert_eq!(
        lines,
        BTreeSet::from(["HOME=/var/empty", "SMOLRUNNER_ACCEPTANCE=allowed"])
    );
    assert_eq!(
        allowlisted.environment_keys,
        ["HOME".to_owned(), "SMOLRUNNER_ACCEPTANCE".to_owned()]
    );
}

#[test]
fn reviewed_linux_executables_resolve_to_protected_absolute_files() {
    if !acceptance_enabled() {
        return;
    }

    for path in [
        "/usr/bin/dpkg-query",
        "/usr/bin/getent",
        "/usr/bin/git",
        "/usr/bin/install",
        "/usr/sbin/groupadd",
        "/usr/sbin/nologin",
        "/usr/sbin/runuser",
        "/usr/sbin/useradd",
        "/usr/sbin/usermod",
    ] {
        let verified = verify_executable(Path::new(path))
            .unwrap_or_else(|error| panic!("verify {path}: {error}"));
        assert_eq!(verified.path(), Path::new(path));
    }
    let environment_program =
        resolve_reviewed_environment_executable().expect("resolve reviewed environment executable");
    let verified = verify_executable(environment_program.path())
        .expect("selected environment executable remains reviewed");
    assert_eq!(verified.path(), environment_program.path());
}

#[test]
fn debian_package_observation_uses_the_real_dpkg_inventory() {
    if !acceptance_enabled() {
        return;
    }

    let packages = [
        PackageName::parse("base-files").expect("package name"),
        PackageName::parse("git").expect("package name"),
        PackageName::parse("smolrunner-never-installed").expect("package name"),
    ];
    let observation = DpkgQueryProbe::new(ProcessExecutor)
        .observe(&packages)
        .expect("observe package inventory");

    assert_eq!(observation.packages()["base-files"], Presence::Present);
    assert_eq!(observation.packages()["git"], Presence::Present);
    assert_eq!(
        observation.packages()["smolrunner-never-installed"],
        Presence::Absent
    );
    assert!(observation.receipt().environment_keys.is_empty());
}

#[test]
fn account_preparation_observation_and_runner_user_transition_work_in_a_container() {
    if !acceptance_enabled() {
        return;
    }

    require_effective_root();
    prepare_empty_system_authorities();

    let desired = desired_account();
    let system_paths = RunnerAccountObservationPaths::system_default();
    let initial = observe_runner_account(&desired, &ProcessExecutor, &system_paths)
        .expect("observe clean disposable container");
    assert_eq!(
        initial.observations.group.state(),
        PreparationObservationState::Absent
    );
    assert_eq!(
        initial.observations.user.state(),
        PreparationObservationState::Absent
    );

    let plan = build_runner_account_plan(desired.clone(), initial.observations)
        .expect("plan account preparation from observed absence");
    let root = RootLaneExecutor::system();
    for item in &plan.items {
        if item.kind == RunnerAccountResourceKind::Linger {
            continue;
        }
        let command = item.command.as_ref().unwrap_or_else(|| {
            panic!(
                "container-valid resource {:?} did not produce a command: {:?}",
                item.kind, item.disposition
            )
        });
        let record = root
            .execute(command)
            .unwrap_or_else(|error| panic!("execute {:?}: {error}", item.kind));
        assert!(
            record.success(),
            "command {:?} exited {:?}: {}",
            item.kind,
            record.status(),
            record.process().stderr
        );
    }

    let observed = observe_runner_account(&desired, &ProcessExecutor, &system_paths)
        .expect("observe prepared account");
    for observation in [
        &observed.observations.group,
        &observed.observations.user,
        &observed.observations.home,
        &observed.observations.subordinate_uids,
        &observed.observations.subordinate_gids,
    ] {
        assert_eq!(observation.state(), PreparationObservationState::Matching);
    }
    assert_eq!(
        observed.observations.linger.state(),
        PreparationObservationState::Absent
    );

    let rerun = build_runner_account_plan(desired.clone(), observed.observations.clone())
        .expect("plan prepared account again");
    for item in rerun
        .items
        .iter()
        .filter(|item| item.kind != RunnerAccountResourceKind::Linger)
    {
        assert_eq!(item.disposition, RunnerAccountPlanDisposition::Satisfied);
        assert!(item.command.is_none());
        assert!(item.mutation.is_none());
    }

    let fixture_root = Path::new("/var/lib/smolrunner-acceptance-observation");
    if fixture_root.exists() {
        fs::remove_dir_all(fixture_root).expect("remove previous relocated observation fixture");
    }
    fs::create_dir_all(fixture_root.join("linger")).expect("create relocated observation fixture");
    fs::write(
        fixture_root.join("subuid"),
        format!("{USERNAME}:{SUBUID_START}:{SUBID_COUNT}\n"),
    )
    .expect("write relocated subordinate UID authority");
    fs::write(
        fixture_root.join("subgid"),
        format!("{USERNAME}:{SUBGID_START}:{SUBID_COUNT}\n"),
    )
    .expect("write relocated subordinate GID authority");
    fs::write(fixture_root.join("linger").join(USERNAME), "")
        .expect("write relocated linger marker");
    for path in [
        fixture_root.join("subuid"),
        fixture_root.join("subgid"),
        fixture_root.join("linger").join(USERNAME),
    ] {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .unwrap_or_else(|error| panic!("protect {}: {error}", path.display()));
    }
    let relocated_paths = RunnerAccountObservationPaths::new(
        fixture_root.join("subuid"),
        fixture_root.join("subgid"),
        fixture_root.join("linger"),
    )
    .expect("build relocated observation paths");
    let relocated = observe_runner_account(&desired, &ProcessExecutor, &relocated_paths)
        .expect("observe relocated protected authorities");
    assert_eq!(
        relocated.observations.linger.state(),
        PreparationObservationState::Matching
    );
    assert_eq!(
        relocated.observations.subordinate_uids.state(),
        PreparationObservationState::Matching
    );
    assert_eq!(
        relocated.observations.subordinate_gids.state(),
        PreparationObservationState::Matching
    );

    let identity = observed.identity().expect("matching account identity");
    let runtime_directory = format!("/run/user/{}", identity.uid());
    let runtime_fixture = CommandSpec::new("/usr/bin/install")
        .argument("--directory")
        .argument("--mode")
        .argument("0700")
        .argument("--owner")
        .argument(USERNAME)
        .argument("--group")
        .argument(USERNAME)
        .argument("--")
        .argument(&runtime_directory);
    let runtime_receipt = ProcessExecutor
        .execute(&runtime_fixture)
        .expect("create disposable runtime-directory fixture");
    assert!(runtime_receipt.success);

    let username = account_name();
    let passwd_command = getent_command("passwd", &username).expect("reviewed passwd lookup");
    let passwd_receipt = ProcessExecutor
        .execute(&passwd_command)
        .expect("execute passwd lookup");
    assert!(passwd_receipt.success);
    let passwd = parse_passwd_record(&passwd_receipt.stdout).expect("parse passwd evidence");
    let subordinate_uids = parse_subordinate_ranges(
        &fs::read_to_string("/etc/subuid").expect("read subordinate UID authority"),
        &username,
    )
    .expect("parse subordinate UID evidence");
    let subordinate_gids = parse_subordinate_ranges(
        &fs::read_to_string("/etc/subgid").expect("read subordinate GID authority"),
        &username,
    )
    .expect("parse subordinate GID evidence");
    let context = RunnerUserContext::new(username, identity.uid(), identity.primary_gid(), HOME)
        .expect("build runner-user context");
    let runtime = inspect_runtime_directory(&context).expect("inspect runtime directory");
    let verified = verify_runner_user(
        &context,
        &passwd,
        &subordinate_uids,
        &subordinate_gids,
        &runtime,
    )
    .expect("verify runner-user evidence");

    let command = LaneCommand::runner_git_version(
        &action("runner-git-version", ExecutionLane::RunnerUser),
        &context,
    )
    .expect("build reviewed runner-user command");
    let record = RunnerUserLaneExecutor::system()
        .execute(&command, &verified)
        .expect("execute through reviewed runner-user transition");
    assert!(record.success());
    assert!(record.process().stdout.starts_with("git version "));
    assert!(record.process().environment_keys.is_empty());
    assert!(
        command
            .spec()
            .displayed_argv()
            .contains(&format!("XDG_RUNTIME_DIR={runtime_directory}"))
    );
}
