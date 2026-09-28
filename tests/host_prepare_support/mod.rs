use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str;
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::debian_package_plan::{DebianPackagePlan, build_package_plan, parse_os_release};
use glaeda::durable_lane_execution::LaneCommandRunner;
use glaeda::host::Presence;
use glaeda::host_preparation_command::{
    HostPreparationCommandDecision, decide_host_preparation, host_preparation_confirmation,
};
use glaeda::host_preparation_plan::{HostPreparationResult, plan_host_preparation};
use glaeda::host_readiness::{
    ExactExecutableObservation, HostObservationState, HostReadinessReport, RunnerAccountReadiness,
};
use glaeda::host_rootless_podman::HostRootlessPodmanReadiness;
use glaeda::journal::{ActionFailure, ActionReceipt, ExecutionLane};
use glaeda::journal_document::{JournalStateDocument, decode_journal_document};
use glaeda::lane_command::{LaneCommand, LaneCommandKind, LinuxAccountName};
use glaeda::lane_executor::RootLaneExecutor;
use glaeda::linux_state::LinuxStateRoot;
use glaeda::linux_state_prepare::prepare_installation;
use glaeda::manifest::RunnerScope;
use glaeda::ownership::ProjectIdentity;
use glaeda::process::{CommandExecutor, CommandSpec, ProcessExecutor};
use glaeda::rootless_podman_config_observation::{
    ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION, RootlessPodmanConfigObservationReport,
};
use glaeda::rootless_podman_config_resolution::{
    ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION, RootlessPodmanConfigAssessment,
    RootlessPodmanConfigAssessmentState,
};
use glaeda::rootless_podman_preflight::{
    ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION, RootlessPodmanExecutableObservation,
    RootlessPodmanPreflightDisposition, RootlessPodmanPreflightObservation,
    RootlessPodmanPreflightState, RootlessPodmanStaticPreflightReport,
};
use glaeda::runner_account_observation::{
    RunnerAccountObservationPaths, RunnerAccountObservationReport, observe_runner_account,
};
use glaeda::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservation,
    PreparationObservationState, RunnerAccountObservations, RunnerAccountResourceKind,
    build_runner_account_plan,
};
use glaeda::runner_user::VerifiedRunnerUser;
use glaeda::state::{InstallationId, JournalId, StateLayout};
use glaeda::state_document::ProjectStateDocument;
use glaeda::state_store::{StateRead, StateRecord};
use glaeda::subordinate_id::{PodmanMigrationPlan, build_exact_subordinate_id_plan};

pub(super) const HOME: &str = "/var/lib/smolrunner-acceptance";
pub(super) const PRIVATE_SENTINEL: &str = "private-observation-token-never-public";
pub(super) const SUBID_COUNT: u32 = 65_536;
const USERNAME: &str = "smolaccept";
const SUBUID_START: u32 = 2_000_000;
const SUBGID_START: u32 = 2_100_000;
const REPOSITORY: &str = "acceptance/host-prepare";

static NEXT_STATE_ROOT: AtomicU64 = AtomicU64::new(1);

pub(super) fn acceptance_enabled() -> bool {
    env::var("SMOLRUNNER_LINUX_ACCEPTANCE").as_deref() == Ok("1")
}

pub(super) fn prepare_matching_host() -> (DesiredRunnerAccount, RunnerAccountObservationReport) {
    require_disposable_root();
    reset_subordinate_authorities();
    fs::create_dir_all("/var/lib/systemd/linger").expect("create linger directory");

    let desired = desired_account();
    let paths = RunnerAccountObservationPaths::system_default();
    let initial = observe_runner_account(&desired, &ProcessExecutor, &paths)
        .expect("observe disposable account before preparation");
    let plan = build_runner_account_plan(desired.clone(), initial.observations)
        .expect("build disposable account plan");
    let executor = RootLaneExecutor::system();
    for item in &plan.items {
        if item.kind == RunnerAccountResourceKind::Linger {
            continue;
        }
        let Some(command) = item.command.as_ref() else {
            continue;
        };
        let record = executor
            .execute(command)
            .unwrap_or_else(|error| panic!("execute {:?}: {error}", item.kind));
        assert!(
            record.success(),
            "account command {:?} exited {:?}: {}",
            item.kind,
            record.status(),
            record.process().stderr
        );
    }

    let linger = Path::new("/var/lib/systemd/linger").join(USERNAME);
    fs::write(&linger, "").expect("write linger marker");
    fs::set_permissions(&linger, fs::Permissions::from_mode(0o644)).expect("protect linger marker");

    let intermediate = observe_runner_account(&desired, &ProcessExecutor, &paths)
        .expect("observe prepared account identity");
    let identity = intermediate.identity().expect("prepared account identity");
    let runtime = CommandSpec::new("/usr/bin/install")
        .argument("--directory")
        .argument("--mode")
        .argument("0700")
        .argument("--owner")
        .argument(USERNAME)
        .argument("--group")
        .argument(USERNAME)
        .argument("--")
        .argument(format!("/run/user/{}", identity.uid()));
    let record = ProcessExecutor
        .execute(&runtime)
        .expect("create runner runtime directory");
    assert!(record.success, "runtime fixture failed: {}", record.stderr);

    let observed = observe_runner_account(&desired, &ProcessExecutor, &paths)
        .expect("observe fully prepared account");
    for item in [
        &observed.observations.group,
        &observed.observations.user,
        &observed.observations.home,
        &observed.observations.subordinate_uids,
        &observed.observations.subordinate_gids,
        &observed.observations.linger,
    ] {
        assert_eq!(item.state(), PreparationObservationState::Matching);
    }
    (desired, observed)
}

pub(super) fn confirmed_migration(
    desired: &DesiredRunnerAccount,
    observed: &RunnerAccountObservationReport,
) -> HostPreparationCommandDecision {
    let identity = observed.identity().expect("matching account identity");
    let identity = (identity.uid(), identity.primary_gid());
    let migration = retained_migration(desired, &observed.observations, identity);
    let proposal = plan_host_preparation(readiness_report(
        desired,
        observed.observations.clone(),
        identity,
        Some(migration),
    ));
    match &proposal.result {
        HostPreparationResult::Executable { phase, .. } => {
            assert_eq!(phase.id, "host-preparation-runner-migration-phase");
            assert_eq!(phase.actions.len(), 1);
            assert_eq!(phase.actions[0].lane, ExecutionLane::RunnerUser);
            assert_eq!(
                phase.actions[0].command_kind,
                LaneCommandKind::RunnerPodmanMigrate
            );
        }
        other => panic!("expected migration-only proposal, got {other:?}"),
    }
    let public = serde_json::to_string(&proposal).expect("serialize proposal");
    assert!(!public.contains(PRIVATE_SENTINEL));
    assert!(!public.contains(HOME));
    let confirmation = host_preparation_confirmation(&proposal)
        .expect("derive confirmation")
        .value()
        .to_owned();
    decide_host_preparation(proposal, Some(&confirmation)).expect("confirm migration proposal")
}

pub(super) fn mapping_decision(
    desired: &DesiredRunnerAccount,
    observed: &RunnerAccountObservationReport,
) -> HostPreparationCommandDecision {
    let identity = observed.identity().expect("matching account identity");
    let mut changed = observed.observations.clone();
    changed.subordinate_uids = observation(PreparationObservationState::Absent, "mapping barrier");
    let proposal = plan_host_preparation(readiness_report(
        desired,
        changed,
        (identity.uid(), identity.primary_gid()),
        None,
    ));
    let confirmation = host_preparation_confirmation(&proposal)
        .expect("derive mapping confirmation")
        .value()
        .to_owned();
    decide_host_preparation(proposal, Some(&confirmation)).expect("confirm mapping proposal")
}

pub(super) fn fresh_ready_proposal(
    desired: &DesiredRunnerAccount,
    observed: RunnerAccountObservationReport,
) -> HostPreparationResult {
    let identity = observed.identity().expect("fresh account identity");
    plan_host_preparation(readiness_report(
        desired,
        observed.observations,
        (identity.uid(), identity.primary_gid()),
        None,
    ))
    .result
}

pub(super) struct TempStateRoot {
    pub(super) path: PathBuf,
    pub(super) installation_id: InstallationId,
}

impl TempStateRoot {
    pub(super) fn new(label: &str) -> Self {
        let sequence = NEXT_STATE_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "smolrunner-host-prepare-{label}-{}-{sequence}",
            std::process::id()
        ));
        if path.exists() {
            fs::remove_dir_all(&path).expect("remove stale state root");
        }
        fs::create_dir(&path).expect("create state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("protect state root");
        let installation_id = InstallationId::parse(&format!(
            "acceptance-{:08x}-{sequence:08x}",
            std::process::id()
        ))
        .expect("installation ID");
        prepare_installation(&path, &installation_id).expect("prepare installation");
        let project = ProjectIdentity {
            repository: REPOSITORY.to_owned(),
            runner_scope: RunnerScope::Repository,
            runner_user: USERNAME.to_owned(),
        };
        let document =
            ProjectStateDocument::new(installation_id.clone(), project).expect("project document");
        let record = StateRecord::project(document).expect("project record");
        let mut store = LinuxStateRoot::open(&path).expect("open state root");
        store.write_atomic(&record).expect("write project state");
        Self {
            path,
            installation_id,
        }
    }

    pub(super) fn journal_id(&self, label: &str) -> JournalId {
        JournalId::parse(&format!("{label}-{:08x}", std::process::id())).expect("journal ID")
    }
}

impl Drop for TempStateRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub(super) fn read_journal(
    store: &LinuxStateRoot,
    installation_id: &InstallationId,
    journal_id: &JournalId,
) -> JournalStateDocument {
    let path = StateLayout::journal_document(installation_id, journal_id);
    let bytes = match store.read(&path).expect("read journal") {
        StateRead::Present(bytes) => bytes,
        StateRead::Missing => panic!("durable journal is missing"),
    };
    decode_journal_document(str::from_utf8(&bytes).expect("journal UTF-8")).expect("decode journal")
}

pub(super) struct IsolatedMigrationRunner {
    expected_argv: Vec<String>,
    sentinel: PathBuf,
    fail: bool,
    pub(super) calls: usize,
}

impl IsolatedMigrationRunner {
    pub(super) fn new(verified: &VerifiedRunnerUser, sentinel: PathBuf, fail: bool) -> Self {
        Self {
            expected_argv: vec![
                "/usr/sbin/runuser".to_owned(),
                "--user".to_owned(),
                verified.username().as_str().to_owned(),
                "--".to_owned(),
                "/usr/bin/env".to_owned(),
                "--ignore-environment".to_owned(),
                format!("HOME={}", verified.home()),
                format!("USER={}", verified.username().as_str()),
                format!("LOGNAME={}", verified.username().as_str()),
                format!("XDG_RUNTIME_DIR={}", verified.runtime_directory().display()),
                "/usr/bin/podman".to_owned(),
                "system".to_owned(),
                "migrate".to_owned(),
            ],
            sentinel,
            fail,
            calls: 0,
        }
    }
}

impl LaneCommandRunner for IsolatedMigrationRunner {
    fn run(&mut self, command: &LaneCommand) -> Result<ActionReceipt, ActionFailure> {
        self.calls += 1;
        assert_eq!(command.lane(), ExecutionLane::RunnerUser);
        assert_eq!(command.kind(), LaneCommandKind::RunnerPodmanMigrate);
        assert_eq!(command.spec().displayed_argv(), self.expected_argv);
        assert!(command.spec().environment.is_empty());
        fs::write(&self.sentinel, "isolated migration boundary invoked")
            .expect("write process sentinel");
        if self.fail {
            Err(ActionFailure::public(
                "migration_failed",
                "isolated reviewed migration boundary failed",
            ))
        } else {
            Ok(ActionReceipt::public(
                "isolated reviewed migration boundary completed",
            ))
        }
    }
}

#[derive(Default)]
pub(super) struct RecordingRunner {
    pub(super) kinds: Vec<LaneCommandKind>,
}

impl LaneCommandRunner for RecordingRunner {
    fn run(&mut self, command: &LaneCommand) -> Result<ActionReceipt, ActionFailure> {
        self.kinds.push(command.kind());
        Ok(ActionReceipt::public("isolated root boundary completed"))
    }
}

fn desired_account() -> DesiredRunnerAccount {
    let account = LinuxAccountName::parse(USERNAME).expect("account name");
    DesiredRunnerAccount::new(
        account.clone(),
        account,
        HOME,
        PlannedSubordinateRange::new(SUBUID_START, SUBID_COUNT).expect("subuid range"),
        PlannedSubordinateRange::new(SUBGID_START, SUBID_COUNT).expect("subgid range"),
    )
    .expect("desired account")
}

fn retained_migration(
    desired: &DesiredRunnerAccount,
    observations: &RunnerAccountObservations,
    identity: (u32, u32),
) -> PodmanMigrationPlan {
    let mut changed = observations.clone();
    changed.subordinate_uids = observation(PreparationObservationState::Absent, "changed subuid");
    let plan = build_exact_subordinate_id_plan(
        desired,
        &changed,
        Some(identity),
        Path::new("/etc/subuid"),
        Path::new("/etc/subgid"),
    )
    .expect("changed subordinate plan");
    match plan.podman_migration {
        required @ PodmanMigrationPlan::Required { .. } => required,
        other => panic!("expected retained migration debt, got {other:?}"),
    }
}

fn readiness_report(
    desired: &DesiredRunnerAccount,
    observations: RunnerAccountObservations,
    identity: (u32, u32),
    migration: Option<PodmanMigrationPlan>,
) -> HostReadinessReport {
    let account_plan =
        build_runner_account_plan(desired.clone(), observations.clone()).expect("account plan");
    let mut subordinate_ids = build_exact_subordinate_id_plan(
        desired,
        &observations,
        Some(identity),
        Path::new("/etc/subuid"),
        Path::new("/etc/subgid"),
    )
    .expect("subordinate plan");
    if let Some(migration) = migration {
        subordinate_ids.podman_migration = migration;
    }
    HostReadinessReport {
        schema_version: glaeda::host_readiness::HOST_READINESS_SCHEMA_VERSION,
        repository: REPOSITORY.to_owned(),
        executables: exact_executables(),
        package_plan: package_plan(),
        rootless_podman: rootless_ready(),
        runner_account: RunnerAccountReadiness::Planned {
            observations: Box::new(observations),
            plan: account_plan,
            subordinate_ids: Box::new(subordinate_ids),
        },
    }
}

fn observation(state: PreparationObservationState, label: &str) -> PreparationObservation {
    PreparationObservation::new(
        state,
        [format!("{PRIVATE_SENTINEL} raw observation {label}")],
    )
    .expect("observation")
}

fn package_plan() -> DebianPackagePlan {
    let inventory = [
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
    build_package_plan(
        parse_os_release("ID=debian\nVERSION_ID=\"12\"\n").expect("distribution"),
        &inventory,
    )
    .expect("package plan")
}

fn exact_executables() -> Vec<ExactExecutableObservation> {
    [
        ("git", "/usr/bin/git"),
        ("podman", "/usr/bin/podman"),
        ("systemctl", "/usr/bin/systemctl"),
    ]
    .into_iter()
    .map(|(name, path)| ExactExecutableObservation {
        name: name.to_owned(),
        path: PathBuf::from(path),
        state: HostObservationState::Matching,
        evidence: vec![format!("{PRIVATE_SENTINEL} executable {name}")],
    })
    .collect()
}

fn matching(label: &str) -> RootlessPodmanPreflightObservation {
    RootlessPodmanPreflightObservation {
        state: RootlessPodmanPreflightState::Matching,
        evidence: vec![format!("{PRIVATE_SENTINEL} preflight {label}")],
    }
}

fn rootless_ready() -> HostRootlessPodmanReadiness {
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
            disposition: RootlessPodmanPreflightDisposition::ReadyForSmokeVerification,
            packages: matching("packages"),
            runner_account: matching("runner account"),
            runtime_directory: matching("runtime directory"),
            configuration: matching("configuration"),
            executables: [
                ("podman", "/usr/bin/podman"),
                ("runuser", "/usr/sbin/runuser"),
                ("env", "/usr/bin/env"),
                ("systemctl", "/usr/bin/systemctl"),
                ("newuidmap", "/usr/bin/newuidmap"),
                ("newgidmap", "/usr/bin/newgidmap"),
                ("slirp4netns", "/usr/bin/slirp4netns"),
                ("fuse-overlayfs", "/usr/bin/fuse-overlayfs"),
            ]
            .into_iter()
            .map(|(name, path)| RootlessPodmanExecutableObservation {
                name: name.to_owned(),
                path: PathBuf::from(path),
                state: RootlessPodmanPreflightState::Matching,
                evidence: vec![format!("{PRIVATE_SENTINEL} helper {name}")],
            })
            .collect(),
        }),
    }
}

fn require_disposable_root() {
    assert!(
        Path::new("/.dockerenv").exists(),
        "refusing host mutation outside a disposable Docker container"
    );
    let status = fs::read_to_string("/proc/self/status").expect("read process status");
    let uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u32>().ok())
        .expect("parse effective UID");
    assert_eq!(uid, 0, "acceptance container must run as root");
}

fn reset_subordinate_authorities() {
    for path in ["/etc/subuid", "/etc/subgid"] {
        fs::write(path, "").unwrap_or_else(|error| panic!("reset {path}: {error}"));
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))
            .unwrap_or_else(|error| panic!("protect {path}: {error}"));
    }
}
