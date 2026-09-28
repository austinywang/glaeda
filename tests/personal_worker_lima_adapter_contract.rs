#![cfg(unix)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::os::unix::fs::{FileExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

mod lima_host_identity_support;

use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput, ExecutionAdmissionRecord,
    ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId,
};
use glaeda::lima_lifecycle::{
    LimaCacheDiskId, LimaCacheDiskIdentity, LimaInstanceId, LimaInstanceIdentity,
    LimaLifecycleObservation, LimaLifecycleObservationDefinition, LimaLifecyclePolicy,
    LimaLifecycleState, LimaObservedResources, LimaProfileGeneration, LimaResourceProfile,
};
use glaeda::lima_lifecycle_executor::{
    LimaLifecycleExecutionAction, LimaLifecycleExecutionPhase, LimaLifecycleExecutionRefusalCode,
    LimaLifecycleExecutor,
};
use glaeda::lima_observation::{
    LimaArchitecture, LimaInstanceName, LimaObservationAdapter, LimaObservationRequest, LimaVmType,
};
use glaeda::mac_availability::{
    AvailabilityRequest, EffectiveAvailabilityMode, HostPowerSource, JobActivity,
    MacAvailabilityObservation, MemoryPressure, ObservationFreshness, VmPowerState,
};
use glaeda::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use glaeda::personal_worker_lima_adapter::{
    PERSONAL_WORKER_LIMA_ADAPTER_SCHEMA_VERSION, PersonalWorkerLimaAdapter,
    PersonalWorkerLimaInput, PersonalWorkerLimaRefusalCode,
};
use glaeda::personal_worker_lima_authority::{
    PersonalWorkerLimaAttemptPhase, PersonalWorkerLimaAuthorityDocument,
    PersonalWorkerLimaRecoveryDisposition, personal_worker_lima_enrollment_confirmation,
};
use glaeda::personal_worker_mac_observation::{
    PersonalWorkerMacObservation, PersonalWorkerMacObservationAdapter,
    PersonalWorkerMacObservationClock,
};
use glaeda::personal_worker_queue::{
    PERSONAL_WORKER_QUEUE_SCHEMA_VERSION, PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
    PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES, PersonalWorkerActiveReservation,
    PersonalWorkerActivityEvidence, PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace,
    PersonalWorkerCancellationState, PersonalWorkerJobRequest, PersonalWorkerPriority,
    PersonalWorkerProfile, PersonalWorkerProfileObservation, PersonalWorkerQueueDecision,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_store::{
    PersonalWorkerDurableCacheLease, PersonalWorkerStoreDocument, PersonalWorkerStoreRevision,
};
use glaeda::personal_worker_tick::{
    PersonalWorkerTickAction, PersonalWorkerTickInput, PersonalWorkerTickPlan,
    PersonalWorkerTickPolicy,
};
use glaeda::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::unix_personal_worker_store::lima_authority::{
    UnixPersonalWorkerLimaAuthorityErrorKind, UnixPersonalWorkerLimaAuthorityGuard,
};
use glaeda::verification_profile::{CacheId, VerificationProfileId};
use lima_host_identity_support::{LimaHostIdentityFixture, rewrite_disk_identity};

const CACHE_PATH: &str = "/home/runner/.cache/cargo";
const LIMACTL: &str = "/opt/homebrew/bin/limactl";
const DISK_BYTES: u64 = 80 * 1024 * 1024 * 1024;
static NEXT_STATE_ROOT: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static LIMA_HOST_FIXTURE: LimaHostIdentityFixture =
        LimaHostIdentityFixture::new("b03-durable-adapter", "smolrunner");
}

type ScriptedHook = Box<dyn FnOnce()>;

fn lima_home() -> String {
    LIMA_HOST_FIXTURE.with(LimaHostIdentityFixture::lima_home_string)
}

struct TempStateRoot(PathBuf);

impl TempStateRoot {
    fn new() -> Self {
        let sequence = NEXT_STATE_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-b03-durable-adapter-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("state root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempStateRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ScriptedExecutor {
    outputs: RefCell<VecDeque<String>>,
    seen: RefCell<Vec<(CommandSpec, Duration)>>,
    fail_at: Option<usize>,
    hook: RefCell<Option<(usize, ScriptedHook)>>,
}

impl ScriptedExecutor {
    fn new(outputs: Vec<String>) -> Self {
        Self {
            outputs: RefCell::new(outputs.into()),
            seen: RefCell::new(Vec::new()),
            fail_at: None,
            hook: RefCell::new(None),
        }
    }

    fn failing_at(outputs: Vec<String>, fail_at: usize) -> Self {
        Self {
            outputs: RefCell::new(outputs.into()),
            seen: RefCell::new(Vec::new()),
            fail_at: Some(fail_at),
            hook: RefCell::new(None),
        }
    }

    fn with_hook(self, hook: impl FnOnce() + 'static) -> Self {
        self.with_hook_at(0, hook)
    }

    fn with_hook_at(self, index: usize, hook: impl FnOnce() + 'static) -> Self {
        *self.hook.borrow_mut() = Some((index, Box::new(hook)));
        self
    }
}

impl CommandExecutor for ScriptedExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        Err(io::Error::other("untimed execution is forbidden"))
    }
}

impl TimedCommandExecutor for ScriptedExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        let index = self.seen.borrow().len();
        self.seen.borrow_mut().push((spec.clone(), timeout));
        if self
            .hook
            .borrow()
            .as_ref()
            .is_some_and(|(hook_index, _)| *hook_index == index)
        {
            let (_, hook) = self.hook.borrow_mut().take().expect("matching hook");
            hook();
        }
        let failed = self.fail_at == Some(index);
        let stdout = self.outputs.borrow_mut().pop_front().unwrap_or_default();
        Ok(ExecutionRecord {
            argv: spec.displayed_argv(),
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(if failed { 1 } else { 0 }),
            success: !failed,
            stdout,
            stderr: String::new(),
        })
    }
}

struct FixedClock(RefCell<VecDeque<u64>>);

impl PersonalWorkerMacObservationClock for FixedClock {
    fn unix_millis(&self) -> io::Result<u64> {
        self.0
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| io::Error::other("missing scripted time"))
    }
}

struct IncrementingClock(Cell<u64>);

impl PersonalWorkerMacObservationClock for IncrementingClock {
    fn unix_millis(&self) -> io::Result<u64> {
        let value = self.0.get();
        self.0.set(value + 100);
        Ok(value)
    }
}

struct HookClock {
    next: Cell<u64>,
    calls: Cell<usize>,
    hook_at: usize,
    hook: RefCell<Option<ScriptedHook>>,
}

impl HookClock {
    fn new(start: u64, hook_at: usize, hook: impl FnOnce() + 'static) -> Self {
        Self {
            next: Cell::new(start),
            calls: Cell::new(0),
            hook_at,
            hook: RefCell::new(Some(Box::new(hook))),
        }
    }
}

impl PersonalWorkerMacObservationClock for HookClock {
    fn unix_millis(&self) -> io::Result<u64> {
        let call = self.calls.get();
        self.calls.set(call + 1);
        if call == self.hook_at
            && let Some(hook) = self.hook.borrow_mut().take()
        {
            hook();
        }
        let value = self.next.get();
        self.next.set(value + 100);
        Ok(value)
    }
}

fn epoch(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("epoch")
}

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}

fn config(root: &Path) -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse(root).expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse("/home/runner/workspace").expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        AvailabilityRequest::Active,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::CodesOnly,
    )
    .expect("config")
}

fn request(home: &str) -> LimaObservationRequest {
    LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        home,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        30,
    )
    .expect("request")
}

fn broker_identity() -> LimaInstanceIdentity {
    LimaInstanceIdentity::new(
        LimaInstanceId::parse("smolrunner").expect("instance ID"),
        LimaCacheDiskIdentity::new(
            LimaCacheDiskId::parse("smolrunner-cache").expect("cache ID"),
            digest('b'),
        ),
    )
}

fn lifecycle(
    state: LimaLifecycleState,
    profile: LimaResourceProfile,
    generation: u64,
    observed_at: u64,
) -> LimaLifecycleObservation {
    let last_activity_at = epoch(1_000);
    LimaLifecycleObservation::new(LimaLifecycleObservationDefinition {
        identity: broker_identity(),
        state,
        profile,
        profile_generation: LimaProfileGeneration::new(generation).expect("generation"),
        observed_resources: LimaObservedResources::for_profile(profile),
        observed_at: epoch(observed_at),
        active_reservation_id: None,
        last_activity_at,
        idle_deadline: epoch(last_activity_at.get() + profile.idle_deadline_offset_millis()),
        graceful_stop_acknowledgement: None,
    })
    .expect("lifecycle")
}

fn host_outputs() -> Vec<String> {
    vec![
        "1\n".to_owned(),
        "total = 4096.00M used = 1024.00M free = 3072.00M (encrypted)\n".to_owned(),
        "Now drawing from 'AC Power'\n -InternalBattery-0\t100%; charged;\n".to_owned(),
        " 100 1 1.0 2048 00:05 /opt/homebrew/bin/limactl\n".to_owned(),
        concat!(
            "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n",
            "Pages free: 100000.\n",
            "Pages active: 200000.\n",
            "Pages inactive: 300000.\n",
            "Pages speculative: 100000.\n",
            "Pages purgeable: 50000.\n",
        )
        .to_owned(),
        "10\n".to_owned(),
    ]
}

fn instance_json(state: &str, profile: LimaResourceProfile) -> String {
    let envelope = profile.envelope();
    format!(
        "{{\"name\":\"smolrunner\",\"status\":\"{state}\",\"dir\":\"{}/smolrunner\",\"vmType\":\"vz\",\"arch\":\"aarch64\",\"cpus\":{},\"memory\":{},\"disk\":{DISK_BYTES},\"errors\":[]}}\n",
        lima_home(),
        envelope.vcpus,
        envelope.memory_bytes
    )
}

fn lima_running_outputs(profile: LimaResourceProfile) -> Vec<String> {
    let envelope = profile.envelope();
    vec![
        instance_json("Running", profile),
        "aarch64\n".to_owned(),
        format!("{}\n", envelope.vcpus),
        "4096\n".to_owned(),
        format!("{}\n", (envelope.memory_bytes - 16 * 1024 * 1024) / 4_096),
        format!("{}  /etc/machine-id\n", "a".repeat(64)),
        "2049:2\n".to_owned(),
        "2049:12345\n".to_owned(),
        instance_json("Running", profile),
    ]
}

fn running_outputs(profile: LimaResourceProfile) -> Vec<String> {
    let mut outputs = host_outputs();
    outputs.extend(lima_running_outputs(profile));
    outputs
}

fn stopped_outputs(profile: LimaResourceProfile) -> Vec<String> {
    let mut outputs = host_outputs();
    outputs.extend([
        instance_json("Stopped", profile),
        instance_json("Stopped", profile),
    ]);
    outputs
}

fn mac_observation(
    config: &OperatorConfig,
    request: &LimaObservationRequest,
    running: bool,
    profile: LimaResourceProfile,
    start: u64,
) -> PersonalWorkerMacObservation {
    mac_observation_with_window(config, request, running, profile, start, 30_000)
}

fn mac_observation_with_window(
    config: &OperatorConfig,
    request: &LimaObservationRequest,
    running: bool,
    profile: LimaResourceProfile,
    start: u64,
    freshness_window_millis: u64,
) -> PersonalWorkerMacObservation {
    let executor = ScriptedExecutor::new(if running {
        running_outputs(profile)
    } else {
        stopped_outputs(profile)
    });
    let clock = FixedClock(RefCell::new(
        [start, start + 1_000, start + 2_000, start + 3_000].into(),
    ));
    let result =
        PersonalWorkerMacObservationAdapter::new(freshness_window_millis, Duration::from_secs(5))
            .expect("Mac adapter")
            .observe(
                config,
                request,
                &LimaObservationAdapter::new(LIMACTL).expect("Lima adapter"),
                &executor,
                &clock,
            );
    result.unwrap_or_else(|error| {
        panic!(
            "sealed Mac observation: {error:?}; seen={:?}; remaining={:?}",
            executor
                .seen
                .borrow()
                .iter()
                .map(|(command, _)| command.displayed_argv())
                .collect::<Vec<_>>(),
            executor.outputs.borrow()
        )
    })
}

fn initial_worker_with_profile(profile: PersonalWorkerProfile) -> PersonalWorkerStoreDocument {
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: epoch(190_000),
            profile_observation: PersonalWorkerProfileObservation::observed(profile),
            activity_evidence: PersonalWorkerActivityEvidence::observed(epoch(1_000)),
            queued: Vec::new(),
            active: Vec::new(),
            pending_profile_change: None,
        },
        Vec::new(),
    )
    .expect("worker")
}

fn active_worker() -> PersonalWorkerStoreDocument {
    let limits =
        ExecutionResourceLimits::new(2_000, 2 * 1024 * 1024 * 1024, 2_048).expect("active limits");
    let identity = ExecutionAdmissionIdentity::new(
        ExecutionRequestId::parse("job-one").expect("request ID"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
    );
    let namespace = PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("cargo-target").expect("cache ID"),
        repository: RepositoryRef::parse("example/project").expect("repository"),
        namespace_digest: digest('c'),
    };
    let reservation_id = ReservationId::parse("reservation-one").expect("reservation ID");
    let reservation_generation = ReservationGeneration::new(1).expect("reservation generation");
    let reserved_at = epoch(170_000);
    let request = PersonalWorkerJobRequest {
        identity: identity.clone(),
        source: PersonalWorkerSourceIdentity::new(
            RepositoryRef::parse("example/project").expect("repository"),
            CommitId::parse(&"1".repeat(40)).expect("commit"),
            GitTreeId::parse(&"2".repeat(40)).expect("tree"),
        ),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: limits,
        cache_namespace: namespace.clone(),
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: epoch(160_000),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    };
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity,
        state: ExecutionAdmissionState::Running,
        observed_at: epoch(190_000),
        requested_limits: limits,
        host_capacity: Some(HostCapacityObservation::new(
            epoch(170_000),
            ExecutionResourceLimits::new(8_000, 12 * 1024 * 1024 * 1024, 4_096).expect("capacity"),
        )),
        applied_limits: Some(limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            reservation_id.clone(),
            reservation_generation,
            reserved_at,
            epoch(300_000),
        )),
        acknowledgement: None,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
        unavailable_reason: None,
    })
    .expect("active admission");
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: epoch(190_000),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Work,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(epoch(180_000)),
            queued: Vec::new(),
            active: vec![PersonalWorkerActiveReservation {
                request,
                admission,
                started_at: Some(epoch(180_000)),
            }],
            pending_profile_change: None,
        },
        vec![PersonalWorkerDurableCacheLease::new(
            ExecutionRequestId::parse("job-one").expect("request ID"),
            namespace,
            PersonalWorkerCacheAccessMode::Write,
            reservation_id,
            reservation_generation,
            reserved_at,
        )],
    )
    .expect("active worker")
}

fn start_tick(lifecycle: &LimaLifecycleObservation, store_revision: u64) -> PersonalWorkerTickPlan {
    let queue = PersonalWorkerQueueDecision {
        schema_version: PERSONAL_WORKER_QUEUE_SCHEMA_VERSION,
        generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
        observed_at: epoch(204_000),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Stopped,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(epoch(1_000)),
        desired_profile: PersonalWorkerProfile::Work,
        cancel_pending_downscale: false,
        profile_change_permitted: true,
        schedulable_cpu_millis: PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
        schedulable_memory_bytes: PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
        schedulable_pids: 3_072,
        selected: Vec::new(),
        visibility: Vec::new(),
    };
    let availability = MacAvailabilityObservation {
        effective_mode: EffectiveAvailabilityMode::Off,
        vm_power: VmPowerState::Stopped,
        job_activity: JobActivity::Idle,
        freshness: ObservationFreshness::Fresh,
        host_power: HostPowerSource::Ac,
        memory_pressure: MemoryPressure::Normal,
        operator_hold: false,
    };
    let capacity = HostCapacityObservation::new(
        epoch(204_000),
        ExecutionResourceLimits::new(8_000, 12 * 1024 * 1024 * 1024, 4_096).expect("capacity"),
    );
    let plan = PersonalWorkerTickPolicy::new(30_000, 30_000, 30_000)
        .expect("policy")
        .plan(PersonalWorkerTickInput {
            store_revision: PersonalWorkerStoreRevision::new(store_revision).expect("revision"),
            decision_at: epoch(205_123),
            queue: &queue,
            lifecycle_policy: &LimaLifecyclePolicy::new(30_000).expect("lifecycle policy"),
            lifecycle: Some(lifecycle),
            runner: None,
            capacity: Some(capacity),
            availability_request: AvailabilityRequest::Active,
            availability: Some(availability),
        })
        .expect("tick");
    assert!(
        matches!(plan.action(), PersonalWorkerTickAction::StartVm { .. }),
        "unexpected tick: {:?}",
        plan.action()
    );
    plan
}

#[derive(Debug, Clone, Copy)]
enum LifecycleCase {
    EditOnly,
    StopOnly,
    StopEditStart,
}

impl LifecycleCase {
    const fn current(
        self,
    ) -> (
        LimaLifecycleState,
        LimaResourceProfile,
        PersonalWorkerProfile,
    ) {
        match self {
            Self::EditOnly => (
                LimaLifecycleState::Stopped,
                LimaResourceProfile::Interactive,
                PersonalWorkerProfile::Stopped,
            ),
            Self::StopOnly => (
                LimaLifecycleState::Running,
                LimaResourceProfile::Work,
                PersonalWorkerProfile::Work,
            ),
            Self::StopEditStart => (
                LimaLifecycleState::Running,
                LimaResourceProfile::Interactive,
                PersonalWorkerProfile::Interactive,
            ),
        }
    }

    const fn desired(self) -> PersonalWorkerProfile {
        match self {
            Self::EditOnly | Self::StopEditStart => PersonalWorkerProfile::Work,
            Self::StopOnly => PersonalWorkerProfile::Stopped,
        }
    }

    const fn decision_at(self) -> u64 {
        match self {
            Self::EditOnly | Self::StopEditStart => 205_123,
            Self::StopOnly => 1_900_123,
        }
    }

    const fn expected_action(self) -> LimaLifecycleExecutionAction {
        match self {
            Self::EditOnly => LimaLifecycleExecutionAction::ChangeProfile,
            Self::StopOnly => LimaLifecycleExecutionAction::Stop {
                target_after_stop: PersonalWorkerProfile::Stopped,
            },
            Self::StopEditStart => LimaLifecycleExecutionAction::Stop {
                target_after_stop: PersonalWorkerProfile::Work,
            },
        }
    }

    fn expected_mutations(self) -> Vec<&'static str> {
        match self {
            Self::EditOnly => vec!["edit"],
            Self::StopOnly => vec!["stop"],
            Self::StopEditStart => vec!["stop", "edit", "start"],
        }
    }
}

fn lifecycle_tick(
    case: LifecycleCase,
    lifecycle: &LimaLifecycleObservation,
) -> PersonalWorkerTickPlan {
    let (_, _, current_profile) = case.current();
    let decision_at = case.decision_at();
    let queue = PersonalWorkerQueueDecision {
        schema_version: PERSONAL_WORKER_QUEUE_SCHEMA_VERSION,
        generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
        observed_at: epoch(decision_at - 100),
        profile_observation: PersonalWorkerProfileObservation::observed(current_profile),
        activity_evidence: PersonalWorkerActivityEvidence::observed(epoch(1_000)),
        desired_profile: case.desired(),
        cancel_pending_downscale: false,
        profile_change_permitted: true,
        schedulable_cpu_millis: PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
        schedulable_memory_bytes: PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
        schedulable_pids: 3_072,
        selected: Vec::new(),
        visibility: Vec::new(),
    };
    let running = lifecycle.state() == LimaLifecycleState::Running;
    let availability_request = if matches!(case, LifecycleCase::StopOnly) {
        AvailabilityRequest::Off
    } else {
        AvailabilityRequest::Active
    };
    let availability = MacAvailabilityObservation {
        effective_mode: if running {
            EffectiveAvailabilityMode::Active
        } else {
            EffectiveAvailabilityMode::Off
        },
        vm_power: if running {
            VmPowerState::Running
        } else {
            VmPowerState::Stopped
        },
        job_activity: JobActivity::Idle,
        freshness: ObservationFreshness::Fresh,
        host_power: HostPowerSource::Ac,
        memory_pressure: MemoryPressure::Normal,
        operator_hold: false,
    };
    let runner = running.then(|| {
        glaeda::personal_worker_host_broker::HostBrokerRunnerObservation::new(
            broker_identity().instance_id().clone(),
            lifecycle.profile_generation(),
            epoch(decision_at - 100),
            if matches!(case, LifecycleCase::StopOnly) {
                glaeda::personal_worker_host_broker::HostBrokerRunnerState::Offline
            } else {
                glaeda::personal_worker_host_broker::HostBrokerRunnerState::IdleReady
            },
        )
    });
    PersonalWorkerTickPolicy::new(30_000, 30_000, 30_000)
        .expect("policy")
        .plan(PersonalWorkerTickInput {
            store_revision: PersonalWorkerStoreRevision::new(1).expect("revision"),
            decision_at: epoch(decision_at),
            queue: &queue,
            lifecycle_policy: &LimaLifecyclePolicy::new(30_000).expect("lifecycle policy"),
            lifecycle: Some(lifecycle),
            runner: runner.as_ref(),
            capacity: None,
            availability_request,
            availability: Some(availability),
        })
        .expect("lifecycle tick")
}

struct Setup {
    _root: TempStateRoot,
    config: OperatorConfig,
    request: LimaObservationRequest,
    pre_mac: PersonalWorkerMacObservation,
    lifecycle: LimaLifecycleObservation,
    tick: PersonalWorkerTickPlan,
}

fn initialize_enrolled_worker(
    root: &Path,
    config: &OperatorConfig,
    request: &LimaObservationRequest,
    current_profile: PersonalWorkerProfile,
) {
    UnixPersonalWorkerStore::initialize_if_clean(
        root,
        &initial_worker_with_profile(current_profile),
    )
    .expect("initialize worker");
    enroll_worker(root, config, request);
}

fn enroll_worker(root: &Path, config: &OperatorConfig, request: &LimaObservationRequest) {
    let running = mac_observation(
        config,
        request,
        true,
        LimaResourceProfile::Interactive,
        100_000,
    );
    let confirmation =
        personal_worker_lima_enrollment_confirmation(config, &running, request, broker_identity())
            .expect("confirmation");
    let enrollment = PersonalWorkerLimaAuthorityDocument::enroll(
        config,
        &running,
        request,
        broker_identity(),
        Some(confirmation.value()),
    )
    .expect("enrollment");
    let mut guard = UnixPersonalWorkerLimaAuthorityGuard::open(root).expect("guard");
    guard
        .publish_enrollment(enrollment, epoch(104_000))
        .expect("publish enrollment");
}

fn setup_active_work() -> Setup {
    let root = TempStateRoot::new();
    let config = config(root.path());
    let request = request(&lima_home());
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &active_worker())
        .expect("initialize active worker");
    enroll_worker(root.path(), &config, &request);
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        1,
        204_000,
    );
    let pre_mac = mac_observation(&config, &request, false, LimaResourceProfile::Work, 200_000);
    let tick = start_tick(&lifecycle, 1);
    Setup {
        _root: root,
        config,
        request,
        pre_mac,
        lifecycle,
        tick,
    }
}

fn setup_start(store_revision: u64) -> Setup {
    let root = TempStateRoot::new();
    let config = config(root.path());
    let request = request(&lima_home());
    initialize_enrolled_worker(
        root.path(),
        &config,
        &request,
        PersonalWorkerProfile::Stopped,
    );
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        1,
        204_000,
    );
    let pre_mac = mac_observation(&config, &request, false, LimaResourceProfile::Work, 200_000);
    let tick = start_tick(&lifecycle, store_revision);
    Setup {
        _root: root,
        config,
        request,
        pre_mac,
        lifecycle,
        tick,
    }
}

fn setup_case(case: LifecycleCase) -> Setup {
    let root = TempStateRoot::new();
    let config = config(root.path());
    let request = request(&lima_home());
    let (state, profile, current_profile) = case.current();
    initialize_enrolled_worker(root.path(), &config, &request, current_profile);
    let observed_at = case.decision_at() - 1_000;
    let lifecycle = lifecycle(state, profile, 1, observed_at);
    let pre_mac = mac_observation(
        &config,
        &request,
        state == LimaLifecycleState::Running,
        profile,
        observed_at - 4_000,
    );
    let tick = lifecycle_tick(case, &lifecycle);
    Setup {
        _root: root,
        config,
        request,
        pre_mac,
        lifecycle,
        tick,
    }
}

fn start_outputs(include_post_mac: bool) -> Vec<String> {
    let mut outputs = vec![String::new()];
    outputs.extend(lima_running_outputs(LimaResourceProfile::Work));
    if include_post_mac {
        outputs.extend(running_outputs(LimaResourceProfile::Work));
    }
    outputs
}

fn case_outputs(case: LifecycleCase) -> Vec<String> {
    let mut outputs = Vec::new();
    match case {
        LifecycleCase::EditOnly => {
            outputs.push(String::new());
            outputs.extend([
                instance_json("Stopped", LimaResourceProfile::Work),
                instance_json("Stopped", LimaResourceProfile::Work),
            ]);
            outputs.extend(stopped_outputs(LimaResourceProfile::Work));
        }
        LifecycleCase::StopOnly => {
            outputs.push(String::new());
            outputs.extend([
                instance_json("Stopped", LimaResourceProfile::Work),
                instance_json("Stopped", LimaResourceProfile::Work),
            ]);
            outputs.extend(stopped_outputs(LimaResourceProfile::Work));
        }
        LifecycleCase::StopEditStart => {
            outputs.push(String::new());
            outputs.extend([
                instance_json("Stopped", LimaResourceProfile::Interactive),
                instance_json("Stopped", LimaResourceProfile::Interactive),
            ]);
            outputs.push(String::new());
            outputs.extend([
                instance_json("Stopped", LimaResourceProfile::Work),
                instance_json("Stopped", LimaResourceProfile::Work),
            ]);
            outputs.push(String::new());
            outputs.extend(lima_running_outputs(LimaResourceProfile::Work));
            outputs.extend(running_outputs(LimaResourceProfile::Work));
        }
    }
    outputs
}

fn execute_start(
    setup: &Setup,
    commands: &ScriptedExecutor,
    clock: &impl PersonalWorkerMacObservationClock,
) -> Result<
    glaeda::personal_worker_lima_adapter::PersonalWorkerLimaExecution,
    glaeda::personal_worker_lima_adapter::PersonalWorkerLimaFailure,
> {
    PersonalWorkerLimaAdapter.execute(
        PersonalWorkerLimaInput {
            config: &setup.config,
            tick: &setup.tick,
            lifecycle: &setup.lifecycle,
            mac: &setup.pre_mac,
            observation_request: &setup.request,
        },
        &LimaLifecycleExecutor::new(LIMACTL, lima_home(), setup.request.instance().clone())
            .expect("lifecycle executor"),
        &PersonalWorkerMacObservationAdapter::new(30_000, Duration::from_secs(5))
            .expect("Mac adapter"),
        &LimaObservationAdapter::new(LIMACTL).expect("Lima adapter"),
        commands,
        clock,
    )
}

fn assert_public_failure_is_private(
    failure: &glaeda::personal_worker_lima_adapter::PersonalWorkerLimaFailure,
    setup: &Setup,
) {
    let public = serde_json::to_string(failure).expect("public failure JSON");
    let debug = format!("{failure:?}");
    let private_state_root = setup.config.state_root().as_path().to_string_lossy();
    let private_lima_home = lima_home();
    for private in [
        private_state_root.as_ref(),
        private_lima_home.as_str(),
        CACHE_PATH,
        LIMACTL,
        "persistent_identity",
        "argv",
    ] {
        assert!(!public.contains(private), "public failure leaked {private}");
        assert!(!debug.contains(private), "failure Debug leaked {private}");
    }
}

#[test]
fn exact_millisecond_start_checkpoints_completes_and_settles_under_one_lock() {
    let setup = setup_start(1);
    let commands = ScriptedExecutor::new(start_outputs(true));
    let execution = execute_start(&setup, &commands, &IncrementingClock(Cell::new(205_124)))
        .expect("durable start");
    assert_eq!(
        execution.schema_version(),
        PERSONAL_WORKER_LIMA_ADAPTER_SCHEMA_VERSION
    );
    assert_eq!(execution.action(), LimaLifecycleExecutionAction::Start);
    assert_eq!(execution.before_store_revision().get(), 1);
    assert_eq!(execution.before_queue_generation().get(), 1);
    assert_eq!(execution.after_store_revision().get(), 2);
    assert_eq!(execution.after_queue_generation().get(), 2);

    let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
        .expect("reopen settled authority");
    assert_eq!(
        guard
            .authority()
            .expect("authority")
            .recovery_report()
            .disposition(),
        PersonalWorkerLimaRecoveryDisposition::Clean
    );
    assert!(guard.authority().expect("authority").attempt().is_none());
    assert_eq!(guard.store_revision().get(), 2);
    assert_eq!(guard.queue_generation().get(), 2);

    let seen = commands.seen.borrow();
    assert_eq!(seen.len(), 25);
    assert_eq!(
        seen[0].0.displayed_argv(),
        vec![LIMACTL, "start", "smolrunner"]
    );
    assert_eq!(seen[0].1, Duration::from_secs(5 * 60));
    assert!(
        seen[1..10]
            .iter()
            .all(|(_, timeout)| *timeout == Duration::from_secs(30))
    );
    assert!(
        seen[10..]
            .iter()
            .all(|(_, timeout)| *timeout == Duration::from_secs(5))
    );
    assert!(seen.iter().all(|(command, _)| {
        command.environment.keys().all(|key| {
            matches!(
                key.as_str(),
                "HOME" | "LANG" | "LC_ALL" | "LIMA_HOME" | "PATH"
            )
        })
    }));
    let public = serde_json::to_string(&execution).expect("public JSON");
    let debug = format!("{execution:?}");
    let private_state_root = setup.config.state_root().as_path().to_string_lossy();
    let private_lima_home = lima_home();
    for private in [
        private_state_root.as_ref(),
        private_lima_home.as_str(),
        CACHE_PATH,
        LIMACTL,
        "persistent_identity",
        "argv",
    ] {
        assert!(!public.contains(private), "public JSON leaked {private}");
        assert!(!debug.contains(private), "Debug leaked {private}");
    }
}

#[test]
fn edit_stop_and_stop_edit_start_persist_their_exact_complete_graphs() {
    for case in [
        LifecycleCase::EditOnly,
        LifecycleCase::StopOnly,
        LifecycleCase::StopEditStart,
    ] {
        let setup = setup_case(case);
        let commands = ScriptedExecutor::new(case_outputs(case));
        let execution = execute_start(
            &setup,
            &commands,
            &IncrementingClock(Cell::new(case.decision_at() + 1)),
        )
        .unwrap_or_else(|error| panic!("{case:?} failed: {error:?}"));
        assert_eq!(execution.action(), case.expected_action());
        assert_eq!(execution.after_store_revision().get(), 2);
        assert_eq!(execution.after_queue_generation().get(), 2);
        let expected_state = if matches!(case, LifecycleCase::StopEditStart) {
            LimaLifecycleState::Running
        } else {
            LimaLifecycleState::Stopped
        };
        assert_eq!(execution.after_state(), expected_state);
        assert_eq!(execution.after_profile(), LimaResourceProfile::Work);
        assert_eq!(
            execution.after_profile_generation().get(),
            if matches!(case, LifecycleCase::StopOnly) {
                1
            } else {
                2
            }
        );

        let mutations = commands
            .seen
            .borrow()
            .iter()
            .filter_map(|(command, _)| {
                let argv = command.displayed_argv();
                argv.get(1)
                    .filter(|argument| matches!(argument.as_str(), "stop" | "edit" | "start"))
                    .cloned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            mutations,
            case.expected_mutations()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
        let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
            .expect("reopen settled authority");
        assert_eq!(
            guard
                .authority()
                .expect("authority")
                .recovery_report()
                .disposition(),
            PersonalWorkerLimaRecoveryDisposition::Clean
        );
        assert!(guard.authority().expect("authority").attempt().is_none());
    }
}

#[test]
fn stale_snapshot_and_private_source_drift_fail_before_prepared_or_command() {
    for source_drift in [false, true] {
        let setup = setup_start(if source_drift { 1 } else { 2 });
        let commands = ScriptedExecutor::new(Vec::new());
        let matching_home = lima_home();
        let executor_home = if source_drift {
            "/Users/operator/.different-lima"
        } else {
            matching_home.as_str()
        };
        let error = PersonalWorkerLimaAdapter
            .execute(
                PersonalWorkerLimaInput {
                    config: &setup.config,
                    tick: &setup.tick,
                    lifecycle: &setup.lifecycle,
                    mac: &setup.pre_mac,
                    observation_request: &setup.request,
                },
                &LimaLifecycleExecutor::new(
                    LIMACTL,
                    executor_home,
                    setup.request.instance().clone(),
                )
                .expect("lifecycle executor"),
                &PersonalWorkerMacObservationAdapter::new(30_000, Duration::from_secs(5))
                    .expect("Mac adapter"),
                &LimaObservationAdapter::new(LIMACTL).expect("Lima adapter"),
                &commands,
                &IncrementingClock(Cell::new(205_124)),
            )
            .expect_err("preflight drift");
        assert_eq!(error.code, PersonalWorkerLimaRefusalCode::LifecycleRefusal);
        assert_eq!(
            error.lifecycle_phase,
            Some(LimaLifecycleExecutionPhase::InputValidation)
        );
        assert_eq!(
            error.lifecycle_code,
            Some(if source_drift {
                LimaLifecycleExecutionRefusalCode::IdentityMismatch
            } else {
                LimaLifecycleExecutionRefusalCode::BrokerStateRevisionMismatch
            })
        );
        assert!(commands.seen.borrow().is_empty());
        let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
            .expect("reopen clean authority");
        assert!(guard.authority().expect("authority").attempt().is_none());
    }
}

#[test]
fn durable_active_work_refuses_before_prepared_or_command() {
    let setup = setup_active_work();
    let commands = ScriptedExecutor::new(Vec::new());
    let error = execute_start(&setup, &commands, &IncrementingClock(Cell::new(205_124)))
        .expect_err("active durable work must veto lifecycle mutation");
    assert_eq!(error.code, PersonalWorkerLimaRefusalCode::InvalidInput);
    assert!(commands.seen.borrow().is_empty());
    let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
        .expect("reopen active worker");
    assert!(guard.has_active_work());
    assert!(guard.authority().expect("authority").attempt().is_none());
}

#[test]
fn held_host_identity_drift_refuses_before_prepared_or_command() {
    let setup = setup_start(1);
    let disk = Path::new(&lima_home())
        .join(setup.request.instance().as_str())
        .join("disk");
    let file = fs::File::open(&disk).expect("open fixture disk identity");
    let mut original_identity_byte = [0_u8; 1];
    file.read_exact_at(&mut original_identity_byte, 512 + 56)
        .expect("read fixture disk identity");
    rewrite_disk_identity(&disk, 0xfe);

    let commands = ScriptedExecutor::new(Vec::new());
    let result = execute_start(&setup, &commands, &IncrementingClock(Cell::new(205_124)));
    rewrite_disk_identity(&disk, original_identity_byte[0]);

    let error = result.expect_err("held host identity drift must veto lifecycle mutation");
    assert_eq!(
        error.code,
        PersonalWorkerLimaRefusalCode::SourceIdentityDrift
    );
    assert_eq!(
        error.observation_kind,
        Some(
            glaeda::personal_worker_mac_observation::PersonalWorkerMacObservationErrorKind::LimaHostIdentityEvidence
        )
    );
    assert!(commands.seen.borrow().is_empty());
    assert_public_failure_is_private(&error, &setup);
    let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
        .expect("reopen unchanged authority");
    assert!(guard.authority().expect("authority").attempt().is_none());
}

#[test]
fn composite_reconfirms_host_identity_before_the_next_command() {
    let setup = setup_case(LifecycleCase::StopEditStart);
    let disk = Path::new(&lima_home())
        .join(setup.request.instance().as_str())
        .join("disk");
    let file = fs::File::open(&disk).expect("open fixture disk identity");
    let mut original_identity_byte = [0_u8; 1];
    file.read_exact_at(&mut original_identity_byte, 512 + 56)
        .expect("read fixture disk identity");
    let drifted_disk = disk.clone();
    let commands = ScriptedExecutor::new(case_outputs(LifecycleCase::StopEditStart))
        .with_hook_at(2, move || rewrite_disk_identity(&drifted_disk, 0xfd));

    let result = execute_start(
        &setup,
        &commands,
        &IncrementingClock(Cell::new(LifecycleCase::StopEditStart.decision_at() + 1)),
    );
    rewrite_disk_identity(&disk, original_identity_byte[0]);

    let error = result.expect_err("composite host drift must block the next mutation");
    assert_eq!(error.code, PersonalWorkerLimaRefusalCode::LifecycleRefusal);
    assert_eq!(
        error.lifecycle_code,
        Some(LimaLifecycleExecutionRefusalCode::CheckpointFailed)
    );
    assert_eq!(
        error.observation_kind,
        Some(
            glaeda::personal_worker_mac_observation::PersonalWorkerMacObservationErrorKind::LimaHostIdentityEvidence
        )
    );
    let mutations = commands
        .seen
        .borrow()
        .iter()
        .filter(|(command, _)| {
            command
                .displayed_argv()
                .get(1)
                .is_some_and(|argument| matches!(argument.as_str(), "stop" | "edit" | "start"))
        })
        .count();
    assert_eq!(mutations, 1);
    let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
        .expect("reopen checkpointed authority");
    let recovery = guard.authority().expect("authority").recovery_report();
    assert_eq!(
        recovery.disposition(),
        PersonalWorkerLimaRecoveryDisposition::ReobserveCheckpointedMutation
    );
    assert_eq!(
        recovery.attempt().expect("attempt").phase(),
        PersonalWorkerLimaAttemptPhase::StopCompleted
    );
}

#[test]
fn command_failure_retains_started_recovery_and_blocks_replay() {
    let setup = setup_start(1);
    let commands = ScriptedExecutor::failing_at(vec![String::new()], 0);
    let error = execute_start(&setup, &commands, &IncrementingClock(Cell::new(205_124)))
        .expect_err("start command failure");
    assert_eq!(error.code, PersonalWorkerLimaRefusalCode::LifecycleRefusal);
    assert_eq!(
        error.lifecycle_code,
        Some(LimaLifecycleExecutionRefusalCode::CommandFailed)
    );
    assert_eq!(commands.seen.borrow().len(), 1);
    assert_public_failure_is_private(&error, &setup);

    let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
        .expect("reopen uncertain attempt");
    let recovery = guard.authority().expect("authority").recovery_report();
    assert_eq!(
        recovery.disposition(),
        PersonalWorkerLimaRecoveryDisposition::ReobserveUncertainMutation
    );
    assert_eq!(
        recovery.attempt().expect("attempt").phase(),
        PersonalWorkerLimaAttemptPhase::StartStarted
    );
    drop(guard);

    let replay_commands = ScriptedExecutor::new(start_outputs(true));
    let replay = execute_start(
        &setup,
        &replay_commands,
        &IncrementingClock(Cell::new(206_000)),
    )
    .expect_err("existing attempt blocks replay");
    assert_eq!(replay.code, PersonalWorkerLimaRefusalCode::RecoveryRequired);
    assert!(replay_commands.seen.borrow().is_empty());
}

#[test]
fn checkpoint_clock_failures_preserve_the_last_durable_boundary() {
    for (times, expected_phase, expected_commands, expected_disposition) in [
        (
            vec![205_124],
            PersonalWorkerLimaAttemptPhase::Prepared,
            0,
            PersonalWorkerLimaRecoveryDisposition::ReobserveBeforeFirstMutation,
        ),
        (
            vec![205_124, 236_000],
            PersonalWorkerLimaAttemptPhase::Prepared,
            0,
            PersonalWorkerLimaRecoveryDisposition::ReobserveBeforeFirstMutation,
        ),
        (
            vec![205_124, 205_224],
            PersonalWorkerLimaAttemptPhase::StartStarted,
            1,
            PersonalWorkerLimaRecoveryDisposition::ReobserveUncertainMutation,
        ),
        (
            vec![205_124, 205_224, 205_324],
            PersonalWorkerLimaAttemptPhase::StartCompleted,
            1,
            PersonalWorkerLimaRecoveryDisposition::ReobserveCheckpointedMutation,
        ),
    ] {
        let setup = setup_start(1);
        let commands = ScriptedExecutor::new(vec![String::new()]);
        let error = execute_start(&setup, &commands, &FixedClock(RefCell::new(times.into())))
            .expect_err("checkpoint clock failure");
        assert_eq!(error.code, PersonalWorkerLimaRefusalCode::LifecycleRefusal);
        assert_eq!(
            error.lifecycle_code,
            Some(LimaLifecycleExecutionRefusalCode::CheckpointFailed)
        );
        assert_eq!(commands.seen.borrow().len(), expected_commands);
        let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
            .expect("reopen checkpointed attempt");
        let recovery = guard.authority().expect("authority").recovery_report();
        assert_eq!(recovery.disposition(), expected_disposition);
        assert_eq!(recovery.attempt().expect("attempt").phase(), expected_phase);
    }
}

#[test]
fn exact_outer_b02_expiry_refuses_before_prepared_and_first_command() {
    for expires_before_prepared in [true, false] {
        let mut setup = setup_start(1);
        setup.pre_mac = mac_observation_with_window(
            &setup.config,
            &setup.request,
            false,
            LimaResourceProfile::Work,
            200_000,
            5_000,
        );
        assert_eq!(setup.pre_mac.report().timing.observed_at_millis, 203_000);
        assert_eq!(setup.pre_mac.report().timing.expires_at_millis, 208_000);
        assert!(setup.pre_mac.report().lima.timing.expires_at_unix_seconds > 208);
        assert!(208_001 - setup.tick.decision_at().get() < 30_000);
        assert!(208_001 - setup.lifecycle.observed_at().get() < 30_000);

        let commands = ScriptedExecutor::new(Vec::new());
        let error = if expires_before_prepared {
            execute_start(
                &setup,
                &commands,
                &FixedClock(RefCell::new(vec![208_001].into())),
            )
            .expect_err("expired outer B02 must refuse before Prepared")
        } else {
            execute_start(
                &setup,
                &commands,
                &FixedClock(RefCell::new(vec![205_124, 208_001].into())),
            )
            .expect_err("expired outer B02 must refuse before first command")
        };
        assert!(commands.seen.borrow().is_empty());
        let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
            .expect("reopen exact B02 expiry state");
        let authority = guard.authority().expect("authority");
        let attempt = authority.attempt();
        if expires_before_prepared {
            assert_eq!(error.code, PersonalWorkerLimaRefusalCode::InvalidInput);
            assert!(attempt.is_none());
        } else {
            assert_eq!(error.code, PersonalWorkerLimaRefusalCode::LifecycleRefusal);
            assert_eq!(
                error.lifecycle_code,
                Some(LimaLifecycleExecutionRefusalCode::CheckpointFailed)
            );
            assert_eq!(
                attempt.expect("Prepared attempt").phase(),
                PersonalWorkerLimaAttemptPhase::Prepared
            );
            assert_eq!(
                authority.recovery_report().disposition(),
                PersonalWorkerLimaRecoveryDisposition::ReobserveBeforeFirstMutation
            );
        }
    }
}

#[test]
fn ambiguous_checkpoint_publication_requires_recovery_without_a_command() {
    let setup = setup_start(1);
    let stage = setup
        .config
        .state_root()
        .as_path()
        .join("personal-worker/.lima-authority.next.json");
    let injected_stage = stage.clone();
    let clock = HookClock::new(205_124, 1, move || {
        fs::write(&injected_stage, []).expect("inject conflicting authority stage");
        fs::set_permissions(&injected_stage, fs::Permissions::from_mode(0o600))
            .expect("private injected stage");
    });
    let commands = ScriptedExecutor::new(Vec::new());
    let error = execute_start(&setup, &commands, &clock)
        .expect_err("ambiguous checkpoint publication must require recovery");

    assert_eq!(error.code, PersonalWorkerLimaRefusalCode::RecoveryRequired);
    assert_eq!(
        error.lifecycle_code,
        Some(LimaLifecycleExecutionRefusalCode::CheckpointFailed)
    );
    assert_eq!(error.durable_code, Some("recovery_required"));
    assert!(error.recovery.is_none());
    assert!(commands.seen.borrow().is_empty());
    assert_public_failure_is_private(&error, &setup);
    fs::remove_file(stage).expect("remove injected stage");
}

#[test]
fn ambiguous_prepared_publication_requires_recovery_without_a_command() {
    let setup = setup_start(1);
    let stage = setup
        .config
        .state_root()
        .as_path()
        .join("personal-worker/.lima-authority.next.json");
    let injected_stage = stage.clone();
    let clock = HookClock::new(205_124, 0, move || {
        fs::write(&injected_stage, []).expect("inject conflicting authority stage");
        fs::set_permissions(&injected_stage, fs::Permissions::from_mode(0o600))
            .expect("private injected stage");
    });
    let commands = ScriptedExecutor::new(Vec::new());
    let error = execute_start(&setup, &commands, &clock)
        .expect_err("ambiguous Prepared publication must require recovery");

    assert_eq!(error.code, PersonalWorkerLimaRefusalCode::RecoveryRequired);
    assert_eq!(error.lifecycle_code, None);
    assert_eq!(error.durable_code, Some("recovery_required"));
    assert!(error.recovery.is_none());
    assert!(commands.seen.borrow().is_empty());
    assert_public_failure_is_private(&error, &setup);
    fs::remove_file(stage).expect("remove injected stage");
}

#[test]
fn ambiguous_completed_publication_requires_recovery_before_settlement() {
    let setup = setup_start(1);
    let stage = setup
        .config
        .state_root()
        .as_path()
        .join("personal-worker/.lima-authority.next.json");
    let injected_stage = stage.clone();
    let commands = ScriptedExecutor::new(start_outputs(true)).with_hook_at(24, move || {
        fs::write(&injected_stage, []).expect("inject conflicting authority stage");
        fs::set_permissions(&injected_stage, fs::Permissions::from_mode(0o600))
            .expect("private injected stage");
    });
    let error = execute_start(&setup, &commands, &IncrementingClock(Cell::new(205_124)))
        .expect_err("ambiguous Completed publication must require recovery");

    assert_eq!(error.code, PersonalWorkerLimaRefusalCode::RecoveryRequired);
    assert_eq!(error.durable_code, Some("recovery_required"));
    assert!(error.recovery.is_none());
    assert_eq!(commands.seen.borrow().len(), 25);
    assert_public_failure_is_private(&error, &setup);
    fs::remove_file(stage).expect("remove injected stage");
}

#[test]
fn canonical_writer_lock_excludes_concurrent_mutation_for_the_whole_sequence() {
    let setup = setup_start(1);
    let root = setup.config.state_root().as_path().to_path_buf();
    let observed_busy = Arc::new(AtomicBool::new(false));
    let hook_busy = Arc::clone(&observed_busy);
    let commands = ScriptedExecutor::new(start_outputs(true)).with_hook(move || {
        let error = UnixPersonalWorkerLimaAuthorityGuard::open(&root)
            .expect_err("concurrent lifecycle guard must not acquire the writer lock");
        assert_eq!(error.kind(), UnixPersonalWorkerLimaAuthorityErrorKind::Busy);
        hook_busy.store(true, Ordering::Relaxed);
    });
    execute_start(&setup, &commands, &IncrementingClock(Cell::new(205_124)))
        .expect("durable start while probing lock contention");
    assert!(observed_busy.load(Ordering::Relaxed));
}

#[test]
fn failed_sealed_post_observation_leaves_verify_started_without_settlement() {
    let setup = setup_start(1);
    let commands = ScriptedExecutor::new(start_outputs(false));
    let error = execute_start(&setup, &commands, &IncrementingClock(Cell::new(205_124)))
        .expect_err("post B02 observation failure");
    assert_eq!(
        error.code,
        PersonalWorkerLimaRefusalCode::PostObservationFailed
    );
    assert_public_failure_is_private(&error, &setup);

    let guard = UnixPersonalWorkerLimaAuthorityGuard::open(setup.config.state_root().as_path())
        .expect("reopen verifying attempt");
    let recovery = guard.authority().expect("authority").recovery_report();
    assert_eq!(
        recovery.disposition(),
        PersonalWorkerLimaRecoveryDisposition::Reverify
    );
    assert_eq!(
        recovery.attempt().expect("attempt").phase(),
        PersonalWorkerLimaAttemptPhase::VerifyStarted
    );
    assert_eq!(guard.store_revision().get(), 1);
    assert_eq!(guard.queue_generation().get(), 1);
}
