#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

mod lima_host_identity_support;

use glaeda::lima_lifecycle::LimaResourceProfile;
use glaeda::lima_observation::{
    LimaArchitecture, LimaInstanceName, LimaObservationAdapter, LimaObservationRequest,
    LimaRuntimeState, LimaVmType,
};
use glaeda::mac_availability::{
    AvailabilityRequest, HostPowerSource, MemoryPressure, ObservationFreshness,
};
use glaeda::macos_resource_observation::{
    lima_process_command, memory_pressure_command, power_command, swap_command,
};
use glaeda::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use glaeda::process::{
    CommandExecutor, CommandSpec, ExecutionRecord, ProcessExecutor, TimedCommandExecutor,
};
use glaeda::verification_profile::VerificationProfileId;

use glaeda::personal_worker_mac_observation::{
    PersonalWorkerMacObservationAdapter, PersonalWorkerMacObservationClock,
    PersonalWorkerMacObservationErrorKind, SystemPersonalWorkerMacObservationClock,
    logical_cpu_command, vm_stat_command,
};
use lima_host_identity_support::{LimaHostIdentityFixture, rewrite_disk_identity};

const CACHE_PATH: &str = "/home/runner/.cache/cargo";
const INTERACTIVE_MEMORY: u64 = 3 * 1024 * 1024 * 1024;
const WORK_MEMORY: u64 = 10 * 1024 * 1024 * 1024;
const DISK_BYTES: u64 = 80 * 1024 * 1024 * 1024;

#[derive(Default)]
struct FakeExecutor {
    receipts: RefCell<BTreeMap<Vec<String>, VecDeque<ExecutionRecord>>>,
    seen: RefCell<Vec<(CommandSpec, Duration)>>,
    after_next: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl FakeExecutor {
    fn with(self, command: CommandSpec, stdout: impl Into<String>) -> Self {
        self.with_record(
            command.clone(),
            ExecutionRecord {
                argv: command.displayed_argv(),
                environment_keys: command.environment.keys().cloned().collect(),
                status: Some(0),
                success: true,
                stdout: stdout.into(),
                stderr: String::new(),
            },
        )
    }

    fn with_record(mut self, command: CommandSpec, record: ExecutionRecord) -> Self {
        self.receipts
            .get_mut()
            .entry(command.displayed_argv())
            .or_default()
            .push_back(record);
        self
    }

    fn replacing(self, command: CommandSpec, stdout: impl Into<String>) -> Self {
        let record = ExecutionRecord {
            argv: command.displayed_argv(),
            environment_keys: command.environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
        };
        self.replacing_record(command, record)
    }

    fn replacing_record(mut self, command: CommandSpec, record: ExecutionRecord) -> Self {
        self.receipts
            .get_mut()
            .insert(command.displayed_argv(), VecDeque::from([record]));
        self
    }

    fn seen(&self) -> Vec<(CommandSpec, Duration)> {
        self.seen.borrow().clone()
    }

    fn with_after_next(self, callback: impl FnOnce() + 'static) -> Self {
        *self.after_next.borrow_mut() = Some(Box::new(callback));
        self
    }
}

impl CommandExecutor for FakeExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        Err(io::Error::other("untimed execution is forbidden in B02"))
    }
}

impl TimedCommandExecutor for FakeExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        self.seen.borrow_mut().push((spec.clone(), timeout));
        let record = self
            .receipts
            .borrow_mut()
            .get_mut(&spec.displayed_argv())
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "private fixture missing"))?;
        if let Some(callback) = self.after_next.borrow_mut().take() {
            callback();
        }
        Ok(record)
    }
}

struct FakeClock {
    values: RefCell<VecDeque<io::Result<u64>>>,
}

impl FakeClock {
    fn new(values: impl IntoIterator<Item = u64>) -> Self {
        Self {
            values: RefCell::new(values.into_iter().map(Ok).collect()),
        }
    }
}

impl PersonalWorkerMacObservationClock for FakeClock {
    fn unix_millis(&self) -> io::Result<u64> {
        self.values
            .borrow_mut()
            .pop_front()
            .expect("scripted B02 clock value")
    }
}

fn config(availability: AvailabilityRequest) -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse("/Users/operator/Library/Application Support/SmolRunner")
            .expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse("/home/runner/workspace").expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        availability,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::CodesOnly,
    )
    .expect("operator config")
}

struct B02Fixture {
    host_identity: LimaHostIdentityFixture,
    lima_home: String,
}

impl B02Fixture {
    fn new() -> Self {
        let host_identity = LimaHostIdentityFixture::new("mac-observation", "smolrunner");
        let lima_home = host_identity.lima_home_string();
        Self {
            host_identity,
            lima_home,
        }
    }

    fn disk_path(&self, instance: &str) -> PathBuf {
        self.host_identity.lima_home().join(instance).join("disk")
    }

    fn request(&self, instance: &str) -> LimaObservationRequest {
        LimaObservationRequest::new(
            LimaInstanceName::parse(instance).expect("instance"),
            &self.lima_home,
            LimaVmType::Vz,
            LimaArchitecture::Aarch64,
            CACHE_PATH,
            30,
        )
        .expect("Lima request")
    }

    fn list_command(&self, instance: &str) -> CommandSpec {
        CommandSpec::new("/opt/homebrew/bin/limactl")
            .environment("HOME", "/var/empty")
            .environment("LIMA_HOME", &self.lima_home)
            .environment("LANG", "C")
            .environment("LC_ALL", "C")
            .environment("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .argument("--tty=false")
            .argument("list")
            .argument("--format=json")
            .argument("--all-fields")
            .argument(instance)
    }

    fn instance_json(
        &self,
        instance: &str,
        cpus: u16,
        memory_bytes: u64,
        disk_bytes: u64,
        state: &str,
    ) -> String {
        format!(
            "{{\"name\":\"{instance}\",\"status\":\"{state}\",\"dir\":\"{}/{instance}\",\"vmType\":\"vz\",\"arch\":\"aarch64\",\"cpus\":{cpus},\"memory\":{memory_bytes},\"disk\":{disk_bytes},\"errors\":[]}}\n",
            self.lima_home
        )
    }

    fn complete_executor(&self, cpus: u16, memory_bytes: u64) -> FakeExecutor {
        self.complete_executor_with_disk(cpus, memory_bytes, DISK_BYTES)
    }

    fn complete_executor_with_disk(
        &self,
        cpus: u16,
        memory_bytes: u64,
        disk_bytes: u64,
    ) -> FakeExecutor {
        let list = self.list_command("smolrunner");
        let list_output =
            self.instance_json("smolrunner", cpus, memory_bytes, disk_bytes, "Stopped");
        FakeExecutor::default()
            .with(memory_pressure_command(), "1\n")
            .with(
                swap_command(),
                "total = 4096.00M used = 1024.00M free = 3072.00M (encrypted)\n",
            )
            .with(
                power_command(),
                "Now drawing from 'AC Power'\n -InternalBattery-0\t100%; charged;\n",
            )
            .with(
                lima_process_command(),
                " 100 1 1.0 2048 00:05 /opt/homebrew/bin/limactl\n",
            )
            .with(
                vm_stat_command(),
                concat!(
                    "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n",
                    "Pages free: 100000.\n",
                    "Pages active: 200000.\n",
                    "Pages inactive: 300000.\n",
                    "Pages speculative: 100000.\n",
                    "Pages purgeable: 50000.\n",
                ),
            )
            .with(logical_cpu_command(), "10\n")
            .with(list.clone(), list_output.clone())
            .with(list, list_output)
    }
}

fn adapter() -> PersonalWorkerMacObservationAdapter {
    PersonalWorkerMacObservationAdapter::new(30_000, Duration::from_secs(5)).expect("B02 adapter")
}

fn lima_adapter() -> LimaObservationAdapter {
    LimaObservationAdapter::new("/opt/homebrew/bin/limactl").expect("Lima adapter")
}

fn clock() -> FakeClock {
    FakeClock::new([100_000, 101_000, 102_000, 103_000])
}

#[test]
fn exact_stopped_observation_binds_config_host_lima_profile_and_one_timeout() {
    let fixture = B02Fixture::new();
    let executor = fixture.complete_executor(4, INTERACTIVE_MEMORY);
    let config = config(AvailabilityRequest::Away);
    let request = fixture.request("smolrunner");
    let observation = adapter()
        .observe(&config, &request, &lima_adapter(), &executor, &clock())
        .expect("complete B02 observation");
    let report = observation.report();

    assert_eq!(report.schema_version, 1);
    assert_eq!(report.config_identity, *config.identity());
    assert_eq!(report.requested_availability, AvailabilityRequest::Away);
    assert_eq!(report.timing.started_at_millis, 100_000);
    assert_eq!(report.timing.observed_at_millis, 103_000);
    assert_eq!(report.timing.duration_millis, 3_000);
    assert_eq!(report.timing.expires_at_millis, 133_000);
    assert_eq!(report.host_headroom.logical_cpu_count, 10);
    assert_eq!(
        report.host_headroom.available_memory_bytes,
        500_000 * 16_384
    );
    assert_eq!(
        report.host_resources.memory_pressure,
        MemoryPressure::Normal
    );
    assert_eq!(report.host_resources.power.source, HostPowerSource::Ac);
    assert_eq!(report.host_resources.freshness, ObservationFreshness::Fresh);
    assert_eq!(
        report.lima.configured.runtime_state,
        LimaRuntimeState::Stopped
    );
    assert_eq!(report.lima_profile, LimaResourceProfile::Interactive);
    assert_eq!(
        observation.lima_source_identity(),
        &request.source_identity()
    );

    let seen = executor.seen();
    assert_eq!(seen.len(), 8);
    assert!(
        seen.iter()
            .all(|(_, timeout)| *timeout == Duration::from_secs(5))
    );
    assert!(
        seen[..6]
            .iter()
            .all(|(command, _)| command.environment.is_empty())
    );
    assert!(seen[6..].iter().all(|(command, _)| {
        command
            .environment
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            == vec!["HOME", "LANG", "LC_ALL", "LIMA_HOME", "PATH"]
    }));
    assert!(vm_stat_command().environment.is_empty());
    assert!(logical_cpu_command().environment.is_empty());

    let debug = format!("{observation:?}");
    let json = serde_json::to_string(report).expect("public report JSON");
    for private in [
        "/Users/operator",
        "/home/runner",
        "/opt/homebrew/bin/limactl",
        "InternalBattery",
    ] {
        assert!(!debug.contains(private), "debug leaked {private}");
        assert!(!json.contains(private), "JSON leaked {private}");
    }
}

#[test]
fn sealed_host_identity_is_bound_to_the_exact_private_lima_source() {
    let first = B02Fixture::new();
    let first_request = first.request("smolrunner");
    let first_observation = adapter()
        .observe(
            &config(AvailabilityRequest::Away),
            &first_request,
            &lima_adapter(),
            &first.complete_executor(4, INTERACTIVE_MEMORY),
            &clock(),
        )
        .expect("first sealed source");

    let second = B02Fixture::new();
    let second_request = second.request("smolrunner");
    let second_observation = adapter()
        .observe(
            &config(AvailabilityRequest::Away),
            &second_request,
            &lima_adapter(),
            &second.complete_executor(4, INTERACTIVE_MEMORY),
            &clock(),
        )
        .expect("second sealed source");

    assert_ne!(
        first_observation.lima_host_identity(),
        second_observation.lima_host_identity()
    );
    assert_ne!(
        first_observation.lima_request_identity(),
        second_observation.lima_request_identity()
    );
    for private_home in [&first.lima_home, &second.lima_home] {
        assert!(!format!("{first_observation:?}").contains(private_home));
        assert!(
            !serde_json::to_string(first_observation.report())
                .expect("public report")
                .contains(private_home)
        );
    }
}

#[test]
fn host_identity_drift_during_b02_window_fails_closed() {
    let fixture = B02Fixture::new();
    let disk_path = fixture.disk_path("smolrunner");
    let executor = fixture
        .complete_executor(4, INTERACTIVE_MEMORY)
        .with_after_next(move || rewrite_disk_identity(&disk_path, 0xfe));

    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Away),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &executor,
            &clock(),
        )
        .expect_err("host identity drift must refuse B02");

    assert_eq!(
        error.kind,
        PersonalWorkerMacObservationErrorKind::LimaHostIdentityEvidence
    );
    assert_eq!(error.code, "lima_host_identity_observation_failed");
    assert_eq!(
        error.lima_host_identity_kind,
        Some(glaeda::lima_host_identity::LimaHostIdentityErrorKind::IdentityDrift)
    );
    assert!(error.private_lima_host_identity_failure().is_some());
    assert!(!format!("{error:?}").contains(&fixture.lima_home));
}

#[test]
fn host_disk_length_must_match_the_exact_lima_report() {
    let fixture = B02Fixture::new();
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Away),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &fixture.complete_executor_with_disk(4, INTERACTIVE_MEMORY, DISK_BYTES - 512),
            &clock(),
        )
        .expect_err("disk length mismatch must refuse B02");

    assert_eq!(
        error.kind,
        PersonalWorkerMacObservationErrorKind::LimaHostIdentityEvidence
    );
    assert_eq!(error.code, "lima_host_disk_size_mismatch");
}

#[test]
fn work_profile_is_exact_and_unreviewed_resource_envelopes_fail_closed() {
    let fixture = B02Fixture::new();
    let work = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &fixture.complete_executor(8, WORK_MEMORY),
            &clock(),
        )
        .expect("work observation");
    assert_eq!(work.report().lima_profile, LimaResourceProfile::Work);

    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &fixture.complete_executor(6, 6 * 1024 * 1024 * 1024),
            &clock(),
        )
        .expect_err("unreviewed Lima profile");
    assert_eq!(
        error.kind,
        PersonalWorkerMacObservationErrorKind::LimaEvidence
    );
    assert_eq!(error.code, "unsupported_lima_profile");
}

#[test]
fn config_request_mismatch_fails_before_clock_or_commands() {
    let fixture = B02Fixture::new();
    let executor = fixture.complete_executor(4, INTERACTIVE_MEMORY);
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &fixture.request("other"),
            &lima_adapter(),
            &executor,
            &FakeClock::new([]),
        )
        .expect_err("config/request mismatch");

    assert_eq!(error.code, "config_lima_instance_mismatch");
    assert!(executor.seen().is_empty());
}

#[test]
fn malformed_duplicate_overflowed_memory_and_cpu_evidence_are_refused() {
    let fixture = B02Fixture::new();
    for output in [
        "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: 1.\nPages inactive: 1.\n",
        "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: 1.\nPages free: 2.\nPages inactive: 1.\nPages speculative: 1.\n",
        "Mach Virtual Memory Statistics: (page size of 65536 bytes)\nPages free: 18446744073709551615.\nPages inactive: 1.\nPages speculative: 1.\n",
    ] {
        let command = vm_stat_command();
        let executor = fixture
            .complete_executor(4, INTERACTIVE_MEMORY)
            .replacing(command, output);
        let error = adapter()
            .observe(
                &config(AvailabilityRequest::Active),
                &fixture.request("smolrunner"),
                &lima_adapter(),
                &executor,
                &clock(),
            )
            .expect_err("invalid memory evidence");
        assert_eq!(error.code, "malformed_available_memory");
    }

    for output in ["0\n", "1025\n", "10\n11\n", "010\n"] {
        let command = logical_cpu_command();
        let executor = fixture
            .complete_executor(4, INTERACTIVE_MEMORY)
            .replacing(command, output);
        let error = adapter()
            .observe(
                &config(AvailabilityRequest::Active),
                &fixture.request("smolrunner"),
                &lima_adapter(),
                &executor,
                &clock(),
            )
            .expect_err("invalid CPU evidence");
        assert_eq!(error.code, "malformed_logical_cpu_count");
    }
}

#[test]
fn command_identity_output_and_timeout_failures_are_bounded_and_private() {
    let fixture = B02Fixture::new();
    let command = vm_stat_command();
    let executor = fixture
        .complete_executor(4, INTERACTIVE_MEMORY)
        .replacing_record(
            command,
            ExecutionRecord {
                argv: vec!["/usr/bin/vm_stat".to_owned(), "--unsafe".to_owned()],
                environment_keys: Vec::new(),
                status: Some(0),
                success: true,
                stdout: "/Users/operator/private".to_owned(),
                stderr: String::new(),
            },
        );
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &executor,
            &clock(),
        )
        .expect_err("command drift");
    assert_eq!(error.code, "command_identity_mismatch");
    assert!(!format!("{error:?}").contains("/Users/operator"));
    assert!(
        !serde_json::to_string(&error)
            .expect("error JSON")
            .contains("/Users/operator")
    );

    let command = vm_stat_command();
    let executor = fixture
        .complete_executor(4, INTERACTIVE_MEMORY)
        .replacing_record(
            command.clone(),
            ExecutionRecord {
                argv: command.displayed_argv(),
                environment_keys: Vec::new(),
                status: Some(0),
                success: true,
                stdout: "x".repeat(65_537),
                stderr: String::new(),
            },
        );
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &executor,
            &clock(),
        )
        .expect_err("oversized direct output");
    assert_eq!(error.code, "unbounded_command_output");

    assert!(PersonalWorkerMacObservationAdapter::new(30_000, Duration::ZERO).is_err());
    assert!(PersonalWorkerMacObservationAdapter::new(30_000, Duration::from_secs(31)).is_err());
}

#[test]
fn partial_low_level_host_evidence_stays_unknown_instead_of_becoming_absent() {
    let fixture = B02Fixture::new();
    let mut executor = fixture.complete_executor(4, INTERACTIVE_MEMORY);
    executor
        .receipts
        .get_mut()
        .remove(&power_command().displayed_argv());
    let observation = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &executor,
            &clock(),
        )
        .expect("partial host observation remains representable");
    assert_eq!(
        observation.report().host_resources.power.source,
        HostPowerSource::Unknown
    );
    assert!(!observation.report().host_resources.problems.is_empty());
}

#[test]
fn outer_and_lima_timing_must_be_fresh_monotonic_and_coherent() {
    let fixture = B02Fixture::new();
    let stale = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &fixture.complete_executor(4, INTERACTIVE_MEMORY),
            &FakeClock::new([100_000, 101_000, 102_000, 140_001]),
        )
        .expect_err("stale outer observation");
    assert_eq!(stale.code, "stale_observation");

    let incoherent = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &fixture.complete_executor(4, INTERACTIVE_MEMORY),
            &FakeClock::new([100_000, 99_000, 99_000, 103_000]),
        )
        .expect_err("Lima timing outside outer observation");
    assert_eq!(incoherent.code, "lima_timing_mismatch");
}

#[test]
fn lima_failures_retain_private_evidence_behind_bounded_redaction() {
    let fixture = B02Fixture::new();
    let list = fixture.list_command("smolrunner");
    let executor = fixture
        .complete_executor(4, INTERACTIVE_MEMORY)
        .replacing(list, "/Users/operator/private malformed Lima output\n");
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &fixture.request("smolrunner"),
            &lima_adapter(),
            &executor,
            &clock(),
        )
        .expect_err("malformed Lima observation");

    assert_eq!(error.code, "lima_observation_failed");
    assert!(error.lima_code.is_some());
    assert!(error.lima_phase.is_some());
    assert!(error.private_lima_failure().is_some());
    assert!(!format!("{error:?}").contains("/Users/operator"));
    assert!(
        !serde_json::to_string(&error)
            .expect("public B02 error JSON")
            .contains("/Users/operator")
    );
}

#[test]
fn source_has_read_only_fixed_command_authority_only() {
    assert_eq!(vm_stat_command().displayed_argv(), vec!["/usr/bin/vm_stat"]);
    assert_eq!(
        logical_cpu_command().displayed_argv(),
        vec!["/usr/sbin/sysctl", "-n", "hw.logicalcpu"]
    );

    let source =
        fs::read_to_string("src/personal_worker_mac_observation.rs").expect("B02 module source");
    for forbidden in [
        "Command::new",
        "std::fs",
        "std::env",
        "sh -c",
        "limactl start",
        "limactl stop",
        "limactl edit",
        "octocrab",
        "reqwest",
        "unsafe {",
    ] {
        assert!(
            !source.contains(forbidden),
            "forbidden B02 authority: {forbidden}"
        );
    }
}

#[test]
#[ignore = "requires an explicitly selected local stopped Lima instance on macOS"]
fn physical_stopped_b02_reaches_the_reviewed_profile_gate() {
    let lima_home = std::env::var("SMOLRUNNER_TEST_LIMA_HOME")
        .expect("set SMOLRUNNER_TEST_LIMA_HOME for the ignored physical test");
    let request = LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("physical instance"),
        lima_home,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        30,
    )
    .expect("physical request");
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Away),
            &request,
            &lima_adapter(),
            &ProcessExecutor,
            &SystemPersonalWorkerMacObservationClock,
        )
        .expect_err("the current legacy 4 CPU / 8 GiB profile is not reviewed");
    assert_eq!(
        error.kind,
        PersonalWorkerMacObservationErrorKind::LimaEvidence
    );
    assert_eq!(error.code, "unsupported_lima_profile");
}
