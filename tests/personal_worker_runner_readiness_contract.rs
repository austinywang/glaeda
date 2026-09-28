#![cfg(unix)]
#![allow(dead_code)]

use std::collections::VecDeque;
use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub use glaeda::{
    actions_runner_readiness, artifact, execution_admission, lima_host_identity, lima_lifecycle,
    lima_observation, mac_availability, macos_resource_observation, operator_config,
    personal_worker_mac_observation, personal_worker_operator_read, personal_worker_queue,
    personal_worker_read_model, personal_worker_store, process, unix_personal_worker_store,
    verification_profile,
};

mod lima_host_identity_support;

#[path = "../src/personal_worker_runner_readiness.rs"]
mod personal_worker_runner_readiness;

use glaeda::actions_runner_readiness::{
    ActionsRunnerConfiguredIdentity, ActionsRunnerName, ActionsRunnerReadinessAdapter,
    ActionsRunnerReadinessRequest,
};
use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    DrainAcknowledgement, EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput,
    ExecutionAdmissionRecord, ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId,
};
use glaeda::lima_lifecycle::LimaResourceProfile;
use glaeda::lima_observation::{
    LimaArchitecture, LimaFilesystemObjectIdentity, LimaInstanceName, LimaObservationAdapter,
    LimaObservationClock, LimaObservationRequest, LimaRuntimeState, LimaVmType,
};
use glaeda::mac_availability::AvailabilityRequest;
use glaeda::macos_resource_observation::{
    lima_process_command, memory_pressure_command, power_command, swap_command,
};
use glaeda::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use glaeda::personal_worker_operator_read::{
    PersonalWorkerOperatorJobRead, PersonalWorkerOperatorReadService,
    PersonalWorkerOperatorStatusRead,
};
use glaeda::personal_worker_queue::{
    PersonalWorkerActiveReservation, PersonalWorkerActivityEvidence, PersonalWorkerCacheAccessMode,
    PersonalWorkerCacheNamespace, PersonalWorkerCancellationState, PersonalWorkerJobRequest,
    PersonalWorkerPriority, PersonalWorkerProfile, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_read_model::PersonalWorkerJobReadRequest;
use glaeda::personal_worker_store::{PersonalWorkerDurableCacheLease, PersonalWorkerStoreDocument};
use glaeda::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::verification_profile::{CacheId, VerificationProfileId};
use lima_host_identity_support::LimaHostIdentityFixture;
use personal_worker_mac_observation::{
    PersonalWorkerMacObservation, PersonalWorkerMacObservationAdapter,
    PersonalWorkerMacObservationClock, logical_cpu_command, vm_stat_command,
};
use personal_worker_runner_readiness::{
    PersonalWorkerRunnerReadinessAdapter, PersonalWorkerRunnerReadinessDisposition,
    PersonalWorkerRunnerReadinessReason,
};

const GIB: u64 = 1_024 * 1_024 * 1_024;
const BASE_MILLIS: u64 = 5_000_000;
const RUNNER_ROOT: &str = "/home/runner/actions-runner";
const DRAIN_MARKER: &str = "/home/runner/actions-runner/.smolrunner-draining";
const CACHE_PATH: &str = "/home/runner/.cache/cargo";
const CONFIG_HEX: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LISTENER_PID: u32 = 42;
const WORKER_PID: u32 = 43;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static LIMA_HOST_FIXTURE: LimaHostIdentityFixture =
        LimaHostIdentityFixture::new("runner-readiness", "smolrunner");
}

fn lima_home() -> String {
    LIMA_HOST_FIXTURE.with(LimaHostIdentityFixture::lima_home_string)
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-runner-readiness-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create state root");
        fs::set_permissions(&path, Permissions::from_mode(0o750)).expect("state root mode");
        Self(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
enum Step {
    Output(Output),
    Error(io::ErrorKind),
}

#[derive(Debug)]
struct Output {
    stdout: String,
    status: Option<i32>,
    success: bool,
}

impl Output {
    fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            status: Some(0),
            success: true,
        }
    }

    fn absent() -> Self {
        Self {
            stdout: String::new(),
            status: Some(1),
            success: false,
        }
    }
}

#[derive(Debug, Default)]
struct TimedExecutor {
    steps: Mutex<VecDeque<Step>>,
    seen: Mutex<Vec<(CommandSpec, Duration)>>,
}

impl TimedExecutor {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<(CommandSpec, Duration)> {
        self.seen.lock().expect("seen lock").clone()
    }
}

impl CommandExecutor for TimedExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        Err(io::Error::other("untimed runner observation is forbidden"))
    }
}

impl TimedCommandExecutor for TimedExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        self.seen
            .lock()
            .expect("seen lock")
            .push((spec.clone(), timeout));
        match self
            .steps
            .lock()
            .expect("steps lock")
            .pop_front()
            .expect("scripted runner command")
        {
            Step::Error(kind) => Err(io::Error::new(kind, "private fixture failure")),
            Step::Output(output) => Ok(ExecutionRecord {
                argv: spec.displayed_argv(),
                environment_keys: spec.environment.keys().cloned().collect(),
                status: output.status,
                success: output.success,
                stdout: output.stdout,
                stderr: String::new(),
            }),
        }
    }
}

struct Clock(Mutex<VecDeque<u64>>);

impl Clock {
    fn new(values: impl IntoIterator<Item = u64>) -> Self {
        Self(Mutex::new(values.into_iter().collect()))
    }
}

impl LimaObservationClock for Clock {
    fn unix_seconds(&self) -> io::Result<u64> {
        self.0
            .lock()
            .expect("clock lock")
            .pop_front()
            .ok_or_else(|| io::Error::other("private clock fixture exhausted"))
    }
}

struct MacClock(Mutex<VecDeque<u64>>);

impl MacClock {
    fn new(values: impl IntoIterator<Item = u64>) -> Self {
        Self(Mutex::new(values.into_iter().collect()))
    }
}

impl PersonalWorkerMacObservationClock for MacClock {
    fn unix_millis(&self) -> io::Result<u64> {
        self.0
            .lock()
            .expect("Mac clock lock")
            .pop_front()
            .ok_or_else(|| io::Error::other("private Mac clock fixture exhausted"))
    }
}

struct MacExecutor {
    state: LimaRuntimeState,
    profile: LimaResourceProfile,
    lima_home: String,
}

impl CommandExecutor for MacExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        Err(io::Error::other("untimed Mac observation is forbidden"))
    }
}

impl TimedCommandExecutor for MacExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        _timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        let argv = spec.displayed_argv();
        let output = self.output_for(&argv)?;
        Ok(ExecutionRecord {
            argv,
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout: output,
            stderr: String::new(),
        })
    }
}

impl MacExecutor {
    fn output_for(&self, argv: &[String]) -> io::Result<String> {
        let envelope = self.profile.envelope();
        if argv == memory_pressure_command().displayed_argv() {
            return Ok("1\n".to_owned());
        }
        if argv == swap_command().displayed_argv() {
            return Ok("total = 4096.00M used = 1024.00M free = 3072.00M (encrypted)\n".to_owned());
        }
        if argv == power_command().displayed_argv() {
            return Ok(
                "Now drawing from 'AC Power'\n -InternalBattery-0\t100%; charged;\n".to_owned(),
            );
        }
        if argv == lima_process_command().displayed_argv() {
            return Ok(" 100 1 1.0 2048 00:05 /opt/homebrew/bin/limactl\n".to_owned());
        }
        if argv == vm_stat_command().displayed_argv() {
            return Ok(concat!(
                "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n",
                "Pages free: 100000.\n",
                "Pages active: 200000.\n",
                "Pages inactive: 300000.\n",
                "Pages speculative: 100000.\n",
                "Pages purgeable: 50000.\n",
            )
            .to_owned());
        }
        if argv == logical_cpu_command().displayed_argv() {
            return Ok("10\n".to_owned());
        }
        if argv.get(2).map(String::as_str) == Some("list") {
            let state = match self.state {
                LimaRuntimeState::Uninitialized => "Uninitialized",
                LimaRuntimeState::Installing => "Installing",
                LimaRuntimeState::Broken => "Broken",
                LimaRuntimeState::Stopped => "Stopped",
                LimaRuntimeState::Running => "Running",
            };
            return Ok(format!(
                "{{\"name\":\"smolrunner\",\"status\":\"{state}\",\"dir\":\"{}/smolrunner\",\"vmType\":\"vz\",\"arch\":\"aarch64\",\"cpus\":{},\"memory\":{},\"disk\":{},\"errors\":[]}}\n",
                self.lima_home,
                envelope.vcpus,
                envelope.memory_bytes,
                80 * GIB,
            ));
        }
        match argv.get(5).map(String::as_str) {
            Some("/usr/bin/uname") => Ok("aarch64\n".to_owned()),
            Some("/usr/bin/getconf")
                if argv.get(6).map(String::as_str) == Some("_NPROCESSORS_ONLN") =>
            {
                Ok(format!("{}\n", envelope.vcpus))
            }
            Some("/usr/bin/getconf") if argv.get(6).map(String::as_str) == Some("PAGE_SIZE") => {
                Ok("4096\n".to_owned())
            }
            Some("/usr/bin/getconf") if argv.get(6).map(String::as_str) == Some("_PHYS_PAGES") => {
                Ok(format!("{}\n", envelope.memory_bytes / 4096))
            }
            Some("/usr/bin/sha256sum") => Ok(format!("{}  /etc/machine-id\n", "a".repeat(64))),
            Some("/usr/bin/stat") if argv.last().map(String::as_str) == Some("/") => {
                Ok("2049:2\n".to_owned())
            }
            Some("/usr/bin/sudo") if argv.get(7).map(String::as_str) == Some("/usr/bin/stat") => {
                Ok("2049:3\n".to_owned())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "private Mac command fixture missing",
            )),
        }
    }
}

fn digest(hex: &str) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", hex.repeat(64))).expect("digest")
}

fn config(root: &Path) -> OperatorConfig {
    config_with_availability(root, AvailabilityRequest::Auto)
}

fn config_with_availability(root: &Path, availability: AvailabilityRequest) -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse(root).expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse("/home/runner/workspace").expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        availability,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::IncludeSuggestions,
    )
    .expect("config")
}

fn runner_request() -> ActionsRunnerReadinessRequest {
    runner_request_for(&lima_home())
}

fn runner_request_for(lima_home: &str) -> ActionsRunnerReadinessRequest {
    ActionsRunnerReadinessRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        ActionsRunnerName::parse("smolrunner-macbook").expect("runner name"),
        lima_home,
        RUNNER_ROOT,
        DRAIN_MARKER,
        digest("b"),
        digest("b"),
        digest("b"),
        digest("b"),
        digest("b"),
    )
    .expect("runner request")
}

fn lima_request_for(lima_home: &str, max_age_seconds: u64) -> LimaObservationRequest {
    LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        lima_home,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        max_age_seconds,
    )
    .expect("Lima request")
}

fn expected_runner_identity() -> ActionsRunnerConfiguredIdentity {
    ActionsRunnerConfiguredIdentity {
        runner_name: ActionsRunnerName::parse("smolrunner-macbook").expect("runner name"),
        configuration_digest: digest("b"),
        runner_root: LimaFilesystemObjectIdentity {
            device_id: 2049,
            inode: 500,
        },
    }
}

fn sealed_mac_observation(
    config: &OperatorConfig,
    state: LimaRuntimeState,
) -> PersonalWorkerMacObservation {
    sealed_mac_observation_with(
        config,
        state,
        LimaResourceProfile::Interactive,
        &lima_home(),
        35_000,
        35,
    )
}

fn sealed_mac_observation_for_profile(
    config: &OperatorConfig,
    state: LimaRuntimeState,
    profile: LimaResourceProfile,
) -> PersonalWorkerMacObservation {
    sealed_mac_observation_with(config, state, profile, &lima_home(), 35_000, 35)
}

fn sealed_mac_observation_with(
    config: &OperatorConfig,
    state: LimaRuntimeState,
    profile: LimaResourceProfile,
    lima_home: &str,
    freshness_window_millis: u64,
    lima_max_age_seconds: u64,
) -> PersonalWorkerMacObservation {
    PersonalWorkerMacObservationAdapter::new(freshness_window_millis, Duration::from_secs(5))
        .expect("Mac adapter")
        .observe(
            config,
            &lima_request_for(lima_home, lima_max_age_seconds),
            &LimaObservationAdapter::new("/opt/homebrew/bin/limactl").expect("Lima adapter"),
            &MacExecutor {
                state,
                profile,
                lima_home: lima_home.to_owned(),
            },
            &MacClock::new([90_000, 91_000, 95_000, 95_000]),
        )
        .expect("sealed Mac observation")
}

fn millis(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn limits() -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(2_000, 2 * GIB, 2_048).expect("limits")
}

fn active_document(state: Option<ExecutionAdmissionState>) -> PersonalWorkerStoreDocument {
    document(
        state,
        if state.is_some() {
            PersonalWorkerProfile::Work
        } else {
            PersonalWorkerProfile::Interactive
        },
    )
}

fn stopped_document() -> PersonalWorkerStoreDocument {
    document(None, PersonalWorkerProfile::Stopped)
}

fn document(
    state: Option<ExecutionAdmissionState>,
    profile: PersonalWorkerProfile,
) -> PersonalWorkerStoreDocument {
    let mut active = Vec::new();
    let mut leases = Vec::new();
    if let Some(state) = state {
        let identity = ExecutionAdmissionIdentity::new(
            ExecutionRequestId::parse("job-one").expect("request ID"),
            VerificationProfileId::parse("smolrunner.required").expect("verification profile"),
            RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
        );
        let namespace = PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: CacheId::parse("build-cache").expect("cache ID"),
            repository: RepositoryRef::parse("example/project").expect("repository"),
            namespace_digest: digest("c"),
        };
        let request = PersonalWorkerJobRequest {
            identity: identity.clone(),
            source: PersonalWorkerSourceIdentity::new(
                RepositoryRef::parse("example/project").expect("repository"),
                CommitId::parse(&"1".repeat(40)).expect("commit"),
                GitTreeId::parse(&"2".repeat(40)).expect("tree"),
            ),
            priority: PersonalWorkerPriority::Normal,
            requested_limits: limits(),
            cache_namespace: namespace.clone(),
            cache_access: PersonalWorkerCacheAccessMode::Write,
            submitted_at: millis(BASE_MILLIS - 120_000),
            operator_deadline: None,
            cancellation: PersonalWorkerCancellationState::Active,
            fallback_eligibility: FallbackProfileEligibility::ineligible(),
        };
        let reservation_id = ReservationId::parse("reservation-one").expect("reservation ID");
        let generation = ReservationGeneration::new(1).expect("reservation generation");
        let reserved_at = millis(BASE_MILLIS - 30_000);
        let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
            identity,
            state,
            observed_at: millis(BASE_MILLIS - 10_000),
            requested_limits: limits(),
            host_capacity: Some(HostCapacityObservation::new(
                millis(BASE_MILLIS - 30_000),
                ExecutionResourceLimits::new(8_000, 10 * GIB, 4_096).expect("capacity"),
            )),
            applied_limits: Some(limits()),
            queue_position: None,
            reservation: Some(ReservationEvidence::new(
                reservation_id.clone(),
                generation,
                reserved_at,
                millis(BASE_MILLIS + 3_600_000),
            )),
            acknowledgement: (state == ExecutionAdmissionState::Draining)
                .then_some(DrainAcknowledgement::Drain),
            fallback_eligibility: FallbackProfileEligibility::ineligible(),
            unavailable_reason: None,
        })
        .expect("admission");
        active.push(PersonalWorkerActiveReservation {
            request,
            admission,
            started_at: (state != ExecutionAdmissionState::Reserved)
                .then_some(millis(BASE_MILLIS - 20_000)),
        });
        leases.push(PersonalWorkerDurableCacheLease::new(
            ExecutionRequestId::parse("job-one").expect("request ID"),
            namespace,
            PersonalWorkerCacheAccessMode::Write,
            reservation_id,
            generation,
            reserved_at,
        ));
    }
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: millis(BASE_MILLIS),
            profile_observation: PersonalWorkerProfileObservation::observed(profile),
            activity_evidence: if state.is_some() {
                PersonalWorkerActivityEvidence::observed(millis(BASE_MILLIS))
            } else {
                PersonalWorkerActivityEvidence::Never
            },
            queued: vec![],
            active,
            pending_profile_change: None,
        },
        leases,
    )
    .expect("document")
}

fn durable_reads(
    root: &TempRoot,
    config: &OperatorConfig,
    document: &PersonalWorkerStoreDocument,
    active: bool,
) -> (
    PersonalWorkerOperatorStatusRead,
    Option<PersonalWorkerOperatorJobRead>,
) {
    UnixPersonalWorkerStore::initialize_if_clean(&root.0, document).expect("initialize store");
    let status = PersonalWorkerOperatorReadService::read_status(config, None).expect("status read");
    let job = active.then(|| {
        PersonalWorkerOperatorReadService::read_job(
            config,
            PersonalWorkerJobReadRequest::new(
                document.revision(),
                document.queue().generation,
                ExecutionRequestId::parse("job-one").expect("request ID"),
            ),
        )
        .expect("job read")
    });
    (status, job)
}

fn runner_adapter() -> ActionsRunnerReadinessAdapter {
    ActionsRunnerReadinessAdapter::new("/opt/homebrew/bin/limactl").expect("runner adapter")
}

fn adapter() -> PersonalWorkerRunnerReadinessAdapter {
    PersonalWorkerRunnerReadinessAdapter::new(Duration::from_secs(5)).expect("adapter")
}

fn running_steps(worker: bool, draining: bool) -> Vec<Step> {
    let mut steps = Vec::new();
    append_identity(&mut steps);
    steps.push(Step::Output(if draining {
        Output::success("")
    } else {
        Output::absent()
    }));
    append_process_snapshot(&mut steps, worker);
    append_process_snapshot(&mut steps, worker);
    append_identity(&mut steps);
    steps.push(Step::Output(if draining {
        Output::success("")
    } else {
        Output::absent()
    }));
    steps
}

fn append_identity(steps: &mut Vec<Step>) {
    steps.push(Step::Output(Output::success("2049:500\n")));
    steps.push(Step::Output(Output::success(format!(
        "{CONFIG_HEX}  [REDACTED]\n"
    ))));
    for _ in 0..4 {
        steps.push(Step::Output(Output::success(format!(
            "{CONFIG_HEX}  [REDACTED]\n"
        ))));
    }
}

fn append_process_snapshot(steps: &mut Vec<Step>, worker: bool) {
    steps.push(Step::Output(Output::success(format!("{LISTENER_PID}\n"))));
    steps.push(Step::Output(if worker {
        Output::success(format!("{WORKER_PID}\n"))
    } else {
        Output::absent()
    }));
    append_process_identity(steps, "Runner.Listener", 4_200, LISTENER_PID);
    if worker {
        append_process_identity(steps, "Runner.Worker", 4_300, WORKER_PID);
    }
}

fn append_process_identity(steps: &mut Vec<Step>, name: &str, inode: u64, pid: u32) {
    steps.push(Step::Output(Output::success(format!(
        "{RUNNER_ROOT}/bin/{name}\n"
    ))));
    steps.push(Step::Output(Output::success(format!(
        "{CONFIG_HEX}  [REDACTED]\n"
    ))));
    steps.push(Step::Output(Output::success(format!("{RUNNER_ROOT}\n"))));
    steps.push(Step::Output(Output::success(format!("900:{inode}\n"))));
    let _ = pid;
}

#[test]
fn idle_ready_binds_exact_sources_timeout_and_private_evidence() {
    let root = TempRoot::new("idle");
    let config = config(&root.0);
    let document = active_document(None);
    let (status, job) = durable_reads(&root, &config, &document, false);
    let executor = TimedExecutor::new(running_steps(false, false));
    let observation = adapter().observe(
        &config,
        &sealed_mac_observation(&config, LimaRuntimeState::Running),
        &runner_request(),
        &expected_runner_identity(),
        &status,
        job.as_ref(),
        &runner_adapter(),
        &executor,
        &Clock::new([100, 105]),
    );
    let report = observation.report();

    assert_eq!(
        report.disposition,
        PersonalWorkerRunnerReadinessDisposition::Ready
    );
    assert_eq!(
        report.reason,
        PersonalWorkerRunnerReadinessReason::IdleReady
    );
    assert!(report.active.is_none());
    assert_eq!(report.config_identity, *config.identity());
    assert_eq!(report.store_revision, document.revision());
    assert_eq!(report.queue_generation, document.queue().generation);
    assert!(observation.private_runner_observation().is_some());
    assert!(observation.private_runner_failure().is_none());
    let seen = executor.seen();
    assert_eq!(seen.len(), 26);
    assert!(
        seen.iter()
            .all(|(_, timeout)| *timeout == Duration::from_secs(5))
    );
    assert!(seen.iter().all(|(command, _)| {
        command
            .environment
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            == vec!["HOME", "LANG", "LC_ALL", "LIMA_HOME", "PATH"]
    }));
    let debug = format!("{observation:?}");
    let json = serde_json::to_string(report).expect("JSON");
    let lima_home = lima_home();
    for private in [RUNNER_ROOT, lima_home.as_str(), "/proc/42", "/proc/43"] {
        assert!(!debug.contains(private));
        assert!(!json.contains(private));
    }
}

#[test]
fn coherent_busy_and_draining_states_bind_the_exact_active_job() {
    for (state, draining, expected_reason) in [
        (
            ExecutionAdmissionState::Running,
            false,
            PersonalWorkerRunnerReadinessReason::ActiveJobRunning,
        ),
        (
            ExecutionAdmissionState::Draining,
            true,
            PersonalWorkerRunnerReadinessReason::ActiveJobDraining,
        ),
    ] {
        let root = TempRoot::new("active");
        let config = config(&root.0);
        let document = active_document(Some(state));
        let (status, job) = durable_reads(&root, &config, &document, true);
        let observation = adapter().observe(
            &config,
            &sealed_mac_observation_for_profile(
                &config,
                LimaRuntimeState::Running,
                LimaResourceProfile::Work,
            ),
            &runner_request(),
            &expected_runner_identity(),
            &status,
            job.as_ref(),
            &runner_adapter(),
            &TimedExecutor::new(running_steps(true, draining)),
            &Clock::new([100, 105]),
        );
        assert_eq!(
            observation.report().disposition,
            PersonalWorkerRunnerReadinessDisposition::Blocked
        );
        assert_eq!(observation.report().reason, expected_reason);
        let active = observation
            .report()
            .active
            .as_ref()
            .expect("active evidence");
        assert_eq!(active.request_id.as_str(), "job-one");
        assert_eq!(active.admission_state, state);
    }
}

#[test]
fn reserved_job_with_idle_listener_is_ready_for_the_bounded_execution_step() {
    let root = TempRoot::new("reserved");
    let config = config(&root.0);
    let document = active_document(Some(ExecutionAdmissionState::Reserved));
    let (status, job) = durable_reads(&root, &config, &document, true);
    let observation = adapter().observe(
        &config,
        &sealed_mac_observation_for_profile(
            &config,
            LimaRuntimeState::Running,
            LimaResourceProfile::Work,
        ),
        &runner_request(),
        &expected_runner_identity(),
        &status,
        job.as_ref(),
        &runner_adapter(),
        &TimedExecutor::new(running_steps(false, false)),
        &Clock::new([100, 105]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::Ready
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::ReservedJobReady
    );
    assert_eq!(
        observation
            .report()
            .active
            .as_ref()
            .expect("active reservation")
            .admission_state,
        ExecutionAdmissionState::Reserved
    );
}

#[test]
fn stopped_and_installing_lima_create_observation_debt_without_commands() {
    for (state, reason) in [
        (
            LimaRuntimeState::Stopped,
            PersonalWorkerRunnerReadinessReason::LimaOffline,
        ),
        (
            LimaRuntimeState::Installing,
            PersonalWorkerRunnerReadinessReason::LimaStarting,
        ),
    ] {
        let root = TempRoot::new("nonrunning");
        let config = config(&root.0);
        let document = stopped_document();
        let (status, _) = durable_reads(&root, &config, &document, false);
        let executor = TimedExecutor::default();
        let observation = adapter().observe(
            &config,
            &sealed_mac_observation(&config, state),
            &runner_request(),
            &expected_runner_identity(),
            &status,
            None,
            &runner_adapter(),
            &executor,
            &Clock::new([100]),
        );
        assert_eq!(
            observation.report().disposition,
            PersonalWorkerRunnerReadinessDisposition::Observe
        );
        assert_eq!(observation.report().reason, reason);
        assert!(executor.seen().is_empty());
    }
}

#[test]
fn mismatched_private_lima_source_fails_before_runner_commands() {
    let root = TempRoot::new("mismatch");
    let config = config(&root.0);
    let document = active_document(None);
    let (status, _) = durable_reads(&root, &config, &document, false);
    let mac = sealed_mac_observation(&config, LimaRuntimeState::Running);
    let executor = TimedExecutor::default();
    let observation = adapter().observe(
        &config,
        &mac,
        &runner_request_for("/Users/operator/.lima-other"),
        &expected_runner_identity(),
        &status,
        None,
        &runner_adapter(),
        &executor,
        &Clock::new([]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::RepairRequired
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::ConfigurationMismatch
    );
    assert!(executor.seen().is_empty());
    let source_debug = format!("{:?}", mac.lima_source_identity());
    assert!(!source_debug.contains(&lima_home()));
    assert!(source_debug.contains("<private-lima-home>"));
}

#[test]
fn runner_command_failure_is_observation_debt_with_private_failure_only() {
    let root = TempRoot::new("command-failure");
    let config = config(&root.0);
    let document = active_document(None);
    let (status, _) = durable_reads(&root, &config, &document, false);
    let observation = adapter().observe(
        &config,
        &sealed_mac_observation(&config, LimaRuntimeState::Running),
        &runner_request(),
        &expected_runner_identity(),
        &status,
        None,
        &runner_adapter(),
        &TimedExecutor::new([Step::Error(io::ErrorKind::TimedOut)]),
        &Clock::new([100]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::Observe
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::RunnerEvidenceUnavailable
    );
    assert!(observation.report().runner.is_none());
    assert!(observation.private_runner_failure().is_some());
    let json = serde_json::to_string(observation.report()).expect("JSON");
    let debug = format!("{observation:?}");
    assert!(!json.contains("private fixture"));
    assert!(!debug.contains("private fixture"));
}

#[test]
fn stale_source_is_observation_debt_without_runner_commands() {
    let root = TempRoot::new("stale");
    let config = config(&root.0);
    let document = active_document(None);
    let (status, _) = durable_reads(&root, &config, &document, false);
    let executor = TimedExecutor::default();
    let mac = sealed_mac_observation_with(
        &config,
        LimaRuntimeState::Running,
        LimaResourceProfile::Interactive,
        &lima_home(),
        45_000,
        35,
    );
    let observation = adapter().observe(
        &config,
        &mac,
        &runner_request(),
        &expected_runner_identity(),
        &status,
        None,
        &runner_adapter(),
        &executor,
        &Clock::new([131]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::Observe
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::RunnerStale
    );
    assert!(executor.seen().is_empty());
}

#[test]
fn enclosing_mac_evidence_expiring_during_runner_observation_is_observation_debt() {
    let root = TempRoot::new("mac-expiry");
    let config = config(&root.0);
    let document = active_document(None);
    let (status, _) = durable_reads(&root, &config, &document, false);
    let mac = sealed_mac_observation_with(
        &config,
        LimaRuntimeState::Running,
        LimaResourceProfile::Interactive,
        &lima_home(),
        9_000,
        35,
    );
    let observation = adapter().observe(
        &config,
        &mac,
        &runner_request(),
        &expected_runner_identity(),
        &status,
        None,
        &runner_adapter(),
        &TimedExecutor::new(running_steps(false, false)),
        &Clock::new([100, 105]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::Observe
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::MacEvidenceStale
    );
}

#[test]
fn sealed_status_from_another_config_is_repair_debt_before_commands() {
    let root = TempRoot::new("config-drift");
    let accepted = config(&root.0);
    let drifted = config_with_availability(&root.0, AvailabilityRequest::Active);
    let document = active_document(None);
    let (status, _) = durable_reads(&root, &drifted, &document, false);
    let executor = TimedExecutor::default();
    let observation = adapter().observe(
        &accepted,
        &sealed_mac_observation(&accepted, LimaRuntimeState::Running),
        &runner_request(),
        &expected_runner_identity(),
        &status,
        None,
        &runner_adapter(),
        &executor,
        &Clock::new([]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::RepairRequired
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::ConfigurationMismatch
    );
    assert!(executor.seen().is_empty());
}

#[test]
fn durable_profile_drift_is_repair_debt_before_commands() {
    let root = TempRoot::new("profile-drift");
    let config = config(&root.0);
    let document = active_document(None);
    let (status, _) = durable_reads(&root, &config, &document, false);
    let executor = TimedExecutor::default();
    let observation = adapter().observe(
        &config,
        &sealed_mac_observation_for_profile(
            &config,
            LimaRuntimeState::Running,
            LimaResourceProfile::Work,
        ),
        &runner_request(),
        &expected_runner_identity(),
        &status,
        None,
        &runner_adapter(),
        &executor,
        &Clock::new([]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::RepairRequired
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::WorkerSnapshotMismatch
    );
    assert!(executor.seen().is_empty());
}

#[test]
fn active_job_on_nonrunning_or_broken_lima_does_not_become_ready() {
    let root = TempRoot::new("active-nonrunning");
    let active_config = config(&root.0);
    let document = active_document(Some(ExecutionAdmissionState::Running));
    let (status, job) = durable_reads(&root, &active_config, &document, true);
    let executor = TimedExecutor::default();
    let observation = adapter().observe(
        &active_config,
        &sealed_mac_observation_for_profile(
            &active_config,
            LimaRuntimeState::Installing,
            LimaResourceProfile::Work,
        ),
        &runner_request(),
        &expected_runner_identity(),
        &status,
        job.as_ref(),
        &runner_adapter(),
        &executor,
        &Clock::new([100]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::RepairRequired
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::WorkerSnapshotMismatch
    );
    assert!(executor.seen().is_empty());

    let root = TempRoot::new("broken");
    let broken_config = config(&root.0);
    let document = active_document(None);
    let (status, _) = durable_reads(&root, &broken_config, &document, false);
    let observation = adapter().observe(
        &broken_config,
        &sealed_mac_observation(&broken_config, LimaRuntimeState::Broken),
        &runner_request(),
        &expected_runner_identity(),
        &status,
        None,
        &runner_adapter(),
        &TimedExecutor::default(),
        &Clock::new([100]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::Blocked
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::LimaUnavailable
    );
}

#[test]
fn impossible_idle_with_running_job_requires_repair() {
    let root = TempRoot::new("lost-job");
    let config = config(&root.0);
    let document = active_document(Some(ExecutionAdmissionState::Running));
    let (status, job) = durable_reads(&root, &config, &document, true);
    let observation = adapter().observe(
        &config,
        &sealed_mac_observation_for_profile(
            &config,
            LimaRuntimeState::Running,
            LimaResourceProfile::Work,
        ),
        &runner_request(),
        &expected_runner_identity(),
        &status,
        job.as_ref(),
        &runner_adapter(),
        &TimedExecutor::new(running_steps(false, false)),
        &Clock::new([100, 105]),
    );
    assert_eq!(
        observation.report().disposition,
        PersonalWorkerRunnerReadinessDisposition::RepairRequired
    );
    assert_eq!(
        observation.report().reason,
        PersonalWorkerRunnerReadinessReason::ActiveJobMismatch
    );
}

#[test]
fn policy_and_authority_surface_remain_bounded() {
    assert!(PersonalWorkerRunnerReadinessAdapter::new(Duration::ZERO).is_err());
    assert!(PersonalWorkerRunnerReadinessAdapter::new(Duration::from_secs(31)).is_err());
    let source = include_str!("../src/personal_worker_runner_readiness.rs");
    for forbidden in [
        "Command::new",
        "std::fs",
        "github",
        "registration token",
        "config.sh",
        "start_vm",
        "stop_vm",
        "queue.push",
        "cache mutation",
        "sh -c",
    ] {
        assert!(
            !source.contains(forbidden),
            "forbidden authority: {forbidden}"
        );
    }
}
