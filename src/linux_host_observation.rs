use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustix::process::geteuid;
use serde::Serialize;

use crate::process::{CommandSpec, TimedCommandExecutor};

pub const LINUX_HOST_OBSERVATION_SCHEMA_VERSION: u8 = 3;
pub const DEFAULT_WATCHED_PORTS: &[u16] = &[
    3000, 3001, 4000, 4173, 4200, 5000, 5173, 5174, 8000, 8080, 8888,
];
pub const MAX_WATCHED_PORTS: usize = 32;

const SYSTEMCTL_PROGRAM: &str = "/usr/bin/systemctl";
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SMALL_PROC_BYTES: usize = 65_536;
const MAX_CPU_STAT_BYTES: usize = 1_048_576;
const MAX_SOCKET_TABLE_BYTES: usize = 16 * 1_048_576;
const MAX_FAILED_UNIT_OUTPUT_BYTES: usize = 65_536;
const MAX_SCHEDULER_VALUE_BYTES: usize = 256;
// Linux SCX_OPS_NAME_LEN includes one trailing NUL in the kernel struct.
const MAX_SCHED_EXT_OPS_NAME_BYTES: usize = 127;
const MAX_CPU_LIST_BYTES: usize = 65_536;
const MAX_LOGICAL_CPUS: u16 = 4_096;
const MAX_FAILED_UNITS: u16 = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinuxHostObservation {
    document_type: &'static str,
    schema_version: u8,
    authority: &'static str,
    scope: &'static str,
    observed_at_unix_millis: u64,
    cpu: CpuObservation,
    memory: MemoryObservation,
    pressure: PressureObservation,
    scheduler: SchedulerObservation,
    services: ServiceFailureObservation,
    watched_ports: Vec<WatchedPortObservation>,
}

impl LinuxHostObservation {
    #[must_use]
    pub const fn observed_at_unix_millis(&self) -> u64 {
        self.observed_at_unix_millis
    }

    #[must_use]
    pub const fn cpu(&self) -> CpuObservation {
        self.cpu
    }

    #[must_use]
    pub const fn memory(&self) -> MemoryObservation {
        self.memory
    }

    #[must_use]
    pub const fn pressure(&self) -> PressureObservation {
        self.pressure
    }

    #[must_use]
    pub const fn scheduler(&self) -> &SchedulerObservation {
        &self.scheduler
    }

    #[must_use]
    pub const fn services(&self) -> ServiceFailureObservation {
        self.services
    }

    #[must_use]
    pub fn watched_ports(&self) -> &[WatchedPortObservation] {
        &self.watched_ports
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CpuObservation {
    pub logical_cpus: u16,
    pub load_1m_micros: u64,
    pub load_5m_micros: u64,
    pub load_15m_micros: u64,
    pub runnable_entities: u32,
    pub total_entities: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MemoryObservation {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PressureObservation {
    pub cpu: PressureSample,
    pub memory: PressureSample,
    pub io: PressureSample,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PressureSample {
    pub avg10_micros: u32,
    pub avg60_micros: u32,
    pub avg300_micros: u32,
    pub total_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchedulerObservation {
    pub autogroup: SchedulerFeatureState,
    pub sched_ext: SchedExtObservation,
    pub cpu_policy: CpuSchedulingPolicyObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CpuSchedulingPolicyObservation {
    pub online_logical_cpus: u16,
    pub smt: SchedulerFeatureState,
    pub nohz_full_online_logical_cpus: u16,
    pub isolated_online_logical_cpus: u16,
    pub frequency: CpuFrequencyObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "support", rename_all = "snake_case")]
pub enum CpuFrequencyObservation {
    Unsupported,
    Supported { classes: Vec<CpuFrequencyClass> },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CpuFrequencyClass {
    pub driver: String,
    pub governor: String,
    pub energy_performance_preference: Option<String>,
    pub hardware_max_frequency_khz: u64,
    pub policy_min_frequency_khz: u64,
    pub policy_max_frequency_khz: u64,
    pub logical_cpus: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerFeatureState {
    Unsupported,
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "support", rename_all = "snake_case")]
pub enum SchedExtObservation {
    Unsupported,
    Supported {
        state: SchedExtState,
        enable_sequence: u64,
        active_ops: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedExtState {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ServiceFailureObservation {
    pub system: ObservedCount,
    pub user: ObservedCount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "knowledge", rename_all = "snake_case")]
pub enum ObservedCount {
    Known { count: u16 },
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WatchedPortObservation {
    pub port: u16,
    pub listening: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxHostObservationErrorKind {
    InvalidRequest,
    ObservationUnavailable,
    InvalidKernelData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinuxHostObservationError {
    pub kind: LinuxHostObservationErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl std::fmt::Display for LinuxHostObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for LinuxHostObservationError {}

/// Observe one Linux host through fixed kernel and systemd interfaces.
///
/// Failed-unit observation is best-effort and represented explicitly as unavailable. Kernel
/// resource and listener observations are required because a partial `/proc` report would be
/// misleading.
///
/// # Errors
///
/// Returns a path-free bounded error for an invalid port request, unavailable required kernel
/// interface, or malformed kernel output.
pub fn observe_linux_host(
    watched_ports: &[u16],
    executor: &impl TimedCommandExecutor,
) -> Result<LinuxHostObservation, LinuxHostObservationError> {
    let observed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| observation_unavailable())?
        .as_millis();
    let observed_at = u64::try_from(observed_at).map_err(|_| observation_unavailable())?;
    observe_linux_host_at(
        Path::new("/proc"),
        Path::new("/sys"),
        watched_ports,
        geteuid().as_raw(),
        observed_at,
        executor,
    )
}

fn observe_linux_host_at(
    proc_root: &Path,
    sys_root: &Path,
    watched_ports: &[u16],
    uid: u32,
    observed_at_unix_millis: u64,
    executor: &impl TimedCommandExecutor,
) -> Result<LinuxHostObservation, LinuxHostObservationError> {
    let watched_ports = normalize_ports(watched_ports)?;
    if observed_at_unix_millis == 0 {
        return Err(invalid_request());
    }

    let stat = read_bounded(&proc_root.join("stat"), MAX_CPU_STAT_BYTES)?;
    let loadavg = read_bounded(&proc_root.join("loadavg"), MAX_SMALL_PROC_BYTES)?;
    let meminfo = read_bounded(&proc_root.join("meminfo"), MAX_SMALL_PROC_BYTES)?;
    let cpu_pressure = read_bounded(&proc_root.join("pressure/cpu"), MAX_SMALL_PROC_BYTES)?;
    let memory_pressure = read_bounded(&proc_root.join("pressure/memory"), MAX_SMALL_PROC_BYTES)?;
    let io_pressure = read_bounded(&proc_root.join("pressure/io"), MAX_SMALL_PROC_BYTES)?;
    let tcp = read_bounded(&proc_root.join("net/tcp"), MAX_SOCKET_TABLE_BYTES)?;
    let tcp6 = read_bounded(&proc_root.join("net/tcp6"), MAX_SOCKET_TABLE_BYTES)?;

    let cpu = parse_cpu(&stat, &loadavg)?;
    let memory = parse_memory(&meminfo)?;
    let pressure = PressureObservation {
        cpu: parse_pressure(&cpu_pressure)?,
        memory: parse_pressure(&memory_pressure)?,
        io: parse_pressure(&io_pressure)?,
    };
    let scheduler = observe_scheduler(proc_root, sys_root)?;
    let listening = parse_listening_ports(&tcp, &tcp6)?;
    let watched_ports = watched_ports
        .into_iter()
        .map(|port| WatchedPortObservation {
            port,
            listening: listening.contains(&port),
        })
        .collect();
    let services = ServiceFailureObservation {
        system: observe_failed_units(systemctl_command(false, uid), executor),
        user: observe_failed_units(systemctl_command(true, uid), executor),
    };

    Ok(LinuxHostObservation {
        document_type: "glaeda-linux-host-observation",
        schema_version: LINUX_HOST_OBSERVATION_SCHEMA_VERSION,
        authority: "observation_only",
        scope: "current_execution_context",
        observed_at_unix_millis,
        cpu,
        memory,
        pressure,
        scheduler,
        services,
        watched_ports,
    })
}

fn observe_scheduler(
    proc_root: &Path,
    sys_root: &Path,
) -> Result<SchedulerObservation, LinuxHostObservationError> {
    let autogroup = match read_optional_bounded(
        &proc_root.join("sys/kernel/sched_autogroup_enabled"),
        MAX_SCHEDULER_VALUE_BYTES,
    )? {
        None => SchedulerFeatureState::Unsupported,
        Some(value) => match value.trim() {
            "0" => SchedulerFeatureState::Disabled,
            "1" => SchedulerFeatureState::Enabled,
            _ => return Err(invalid_kernel_data()),
        },
    };
    let sched_ext_root = sys_root.join("kernel/sched_ext");
    let sched_ext = match fs::symlink_metadata(&sched_ext_root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SchedExtObservation::Unsupported
        }
        Err(_) => return Err(observation_unavailable()),
        Ok(metadata) if metadata.file_type().is_dir() => observe_sched_ext(&sched_ext_root)?,
        Ok(_) => return Err(invalid_kernel_data()),
    };
    let cpu_policy = observe_cpu_policy(sys_root)?;
    Ok(SchedulerObservation {
        autogroup,
        sched_ext,
        cpu_policy,
    })
}

fn observe_cpu_policy(
    sys_root: &Path,
) -> Result<CpuSchedulingPolicyObservation, LinuxHostObservationError> {
    let root = sys_root.join("devices/system/cpu");
    let online_before = parse_cpu_list(
        &read_bounded(&root.join("online"), MAX_CPU_LIST_BYTES)?,
        false,
    )?;
    let online_logical_cpus =
        u16::try_from(online_before.len()).map_err(|_| invalid_kernel_data())?;
    let nohz_full_before = parse_cpu_list(
        &read_bounded(&root.join("nohz_full"), MAX_CPU_LIST_BYTES)?,
        true,
    )?;
    let isolated_before = parse_cpu_list(
        &read_bounded(&root.join("isolated"), MAX_CPU_LIST_BYTES)?,
        true,
    )?;
    let smt_before = observe_smt(&root)?;
    let frequency_before = observe_cpu_frequency(&root, &online_before)?;
    let nohz_full_after = parse_cpu_list(
        &read_bounded(&root.join("nohz_full"), MAX_CPU_LIST_BYTES)?,
        true,
    )?;
    let isolated_after = parse_cpu_list(
        &read_bounded(&root.join("isolated"), MAX_CPU_LIST_BYTES)?,
        true,
    )?;
    let smt_after = observe_smt(&root)?;
    let frequency_after = observe_cpu_frequency(&root, &online_before)?;
    let online_after = parse_cpu_list(
        &read_bounded(&root.join("online"), MAX_CPU_LIST_BYTES)?,
        false,
    )?;
    if online_before != online_after
        || nohz_full_before != nohz_full_after
        || isolated_before != isolated_after
        || smt_before != smt_after
        || frequency_before != frequency_after
    {
        return Err(observation_unavailable());
    }
    Ok(CpuSchedulingPolicyObservation {
        online_logical_cpus,
        smt: smt_after,
        nohz_full_online_logical_cpus: u16::try_from(
            nohz_full_after.intersection(&online_after).count(),
        )
        .map_err(|_| invalid_kernel_data())?,
        isolated_online_logical_cpus: u16::try_from(
            isolated_after.intersection(&online_after).count(),
        )
        .map_err(|_| invalid_kernel_data())?,
        frequency: frequency_after,
    })
}

fn observe_smt(cpu_root: &Path) -> Result<SchedulerFeatureState, LinuxHostObservationError> {
    match read_optional_bounded(&cpu_root.join("smt/active"), MAX_SCHEDULER_VALUE_BYTES)? {
        None => Ok(SchedulerFeatureState::Unsupported),
        Some(value) => match value.trim() {
            "0" => Ok(SchedulerFeatureState::Disabled),
            "1" => Ok(SchedulerFeatureState::Enabled),
            _ => Err(invalid_kernel_data()),
        },
    }
}

fn observe_cpu_frequency(
    cpu_root: &Path,
    online: &BTreeSet<u16>,
) -> Result<CpuFrequencyObservation, LinuxHostObservationError> {
    let mut saw_present = false;
    let mut saw_absent = false;
    let mut classes = BTreeMap::<(String, String, Option<String>, u64, u64, u64), u16>::new();
    for cpu in online {
        let root = cpu_root.join(format!("cpu{cpu}/cpufreq"));
        let driver =
            read_optional_bounded(&root.join("scaling_driver"), MAX_SCHEDULER_VALUE_BYTES)?;
        let governor =
            read_optional_bounded(&root.join("scaling_governor"), MAX_SCHEDULER_VALUE_BYTES)?;
        let max_frequency =
            read_optional_bounded(&root.join("cpuinfo_max_freq"), MAX_SCHEDULER_VALUE_BYTES)?;
        let policy_min =
            read_optional_bounded(&root.join("scaling_min_freq"), MAX_SCHEDULER_VALUE_BYTES)?;
        let policy_max =
            read_optional_bounded(&root.join("scaling_max_freq"), MAX_SCHEDULER_VALUE_BYTES)?;
        let preference = read_optional_bounded(
            &root.join("energy_performance_preference"),
            MAX_SCHEDULER_VALUE_BYTES,
        )?;
        if driver.is_none()
            && governor.is_none()
            && max_frequency.is_none()
            && policy_min.is_none()
            && policy_max.is_none()
            && preference.is_none()
        {
            saw_absent = true;
            continue;
        }
        saw_present = true;
        let hardware_max_frequency_khz =
            parse_positive_u64(&max_frequency.ok_or_else(invalid_kernel_data)?)?;
        let policy_min_frequency_khz =
            parse_positive_u64(&policy_min.ok_or_else(invalid_kernel_data)?)?;
        let policy_max_frequency_khz =
            parse_positive_u64(&policy_max.ok_or_else(invalid_kernel_data)?)?;
        if policy_min_frequency_khz > policy_max_frequency_khz
            || policy_max_frequency_khz > hardware_max_frequency_khz
        {
            return Err(invalid_kernel_data());
        }
        let key = (
            parse_kernel_identifier(&driver.ok_or_else(invalid_kernel_data)?)?,
            parse_kernel_identifier(&governor.ok_or_else(invalid_kernel_data)?)?,
            preference
                .as_deref()
                .map(parse_kernel_identifier)
                .transpose()?,
            hardware_max_frequency_khz,
            policy_min_frequency_khz,
            policy_max_frequency_khz,
        );
        let count = classes.entry(key).or_default();
        *count = count.checked_add(1).ok_or_else(invalid_kernel_data)?;
    }
    if saw_present && saw_absent {
        return Err(invalid_kernel_data());
    }
    if !saw_present {
        return Ok(CpuFrequencyObservation::Unsupported);
    }
    Ok(CpuFrequencyObservation::Supported {
        classes: classes
            .into_iter()
            .map(
                |(
                    (
                        driver,
                        governor,
                        energy_performance_preference,
                        hardware_max_frequency_khz,
                        policy_min_frequency_khz,
                        policy_max_frequency_khz,
                    ),
                    logical_cpus,
                )| CpuFrequencyClass {
                    driver,
                    governor,
                    energy_performance_preference,
                    hardware_max_frequency_khz,
                    policy_min_frequency_khz,
                    policy_max_frequency_khz,
                    logical_cpus,
                },
            )
            .collect(),
    })
}

fn observe_sched_ext(root: &Path) -> Result<SchedExtObservation, LinuxHostObservationError> {
    let state_before = parse_sched_ext_state(&read_bounded(
        &root.join("state"),
        MAX_SCHEDULER_VALUE_BYTES,
    )?)?;
    let sequence_before = parse_u64(&read_bounded(
        &root.join("enable_seq"),
        MAX_SCHEDULER_VALUE_BYTES,
    )?)?;
    let active_ops = match state_before {
        SchedExtState::Disabled => None,
        SchedExtState::Enabled => Some(parse_sched_ext_ops(&read_bounded(
            &root.join("root/ops"),
            MAX_SCHEDULER_VALUE_BYTES,
        )?)?),
    };
    let state_after = parse_sched_ext_state(&read_bounded(
        &root.join("state"),
        MAX_SCHEDULER_VALUE_BYTES,
    )?)?;
    let sequence_after = parse_u64(&read_bounded(
        &root.join("enable_seq"),
        MAX_SCHEDULER_VALUE_BYTES,
    )?)?;
    if state_before != state_after || sequence_before != sequence_after {
        return Err(observation_unavailable());
    }
    Ok(SchedExtObservation::Supported {
        state: state_after,
        enable_sequence: sequence_after,
        active_ops,
    })
}

fn parse_sched_ext_state(value: &str) -> Result<SchedExtState, LinuxHostObservationError> {
    match value.trim() {
        "disabled" => Ok(SchedExtState::Disabled),
        "enabled" => Ok(SchedExtState::Enabled),
        _ => Err(invalid_kernel_data()),
    }
}

fn parse_sched_ext_ops(value: &str) -> Result<String, LinuxHostObservationError> {
    let value = value.strip_suffix('\n').unwrap_or(value);
    if value.is_empty()
        || value.len() > MAX_SCHED_EXT_OPS_NAME_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
    {
        return Err(invalid_kernel_data());
    }
    Ok(value.to_owned())
}

fn parse_u64(value: &str) -> Result<u64, LinuxHostObservationError> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_kernel_data());
    }
    value.parse().map_err(|_| invalid_kernel_data())
}

fn parse_positive_u64(value: &str) -> Result<u64, LinuxHostObservationError> {
    let value = parse_u64(value)?;
    if value == 0 {
        Err(invalid_kernel_data())
    } else {
        Ok(value)
    }
}

fn parse_kernel_identifier(value: &str) -> Result<String, LinuxHostObservationError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 63
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(invalid_kernel_data());
    }
    Ok(value.to_owned())
}

fn parse_cpu_list(
    value: &str,
    allow_empty: bool,
) -> Result<BTreeSet<u16>, LinuxHostObservationError> {
    let value = value.trim();
    if value.is_empty() || (allow_empty && value == "(null)") {
        return if allow_empty {
            Ok(BTreeSet::new())
        } else {
            Err(invalid_kernel_data())
        };
    }
    let mut cpus = BTreeSet::new();
    for part in value.split(',') {
        if part.is_empty() {
            return Err(invalid_kernel_data());
        }
        let (start, end) = match part.split_once('-') {
            None => {
                let cpu = parse_cpu_id(part)?;
                (cpu, cpu)
            }
            Some((start, end)) if !start.is_empty() && !end.is_empty() && !end.contains('-') => {
                (parse_cpu_id(start)?, parse_cpu_id(end)?)
            }
            Some(_) => return Err(invalid_kernel_data()),
        };
        if start > end {
            return Err(invalid_kernel_data());
        }
        for cpu in start..=end {
            if !cpus.insert(cpu) {
                return Err(invalid_kernel_data());
            }
        }
    }
    if cpus.len() > usize::from(MAX_LOGICAL_CPUS) {
        return Err(invalid_kernel_data());
    }
    Ok(cpus)
}

fn parse_cpu_id(value: &str) -> Result<u16, LinuxHostObservationError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_kernel_data());
    }
    let cpu = value.parse::<u16>().map_err(|_| invalid_kernel_data())?;
    if cpu >= MAX_LOGICAL_CPUS {
        return Err(invalid_kernel_data());
    }
    Ok(cpu)
}

fn normalize_ports(ports: &[u16]) -> Result<Vec<u16>, LinuxHostObservationError> {
    if ports.is_empty() || ports.len() > MAX_WATCHED_PORTS || ports.contains(&0) {
        return Err(invalid_request());
    }
    Ok(ports
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn read_bounded(path: &Path, limit: usize) -> Result<String, LinuxHostObservationError> {
    let mut bytes = Vec::with_capacity(limit.min(MAX_SMALL_PROC_BYTES));
    File::open(path)
        .map_err(|_| observation_unavailable())?
        .take(u64::try_from(limit).map_err(|_| invalid_kernel_data())? + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| observation_unavailable())?;
    if bytes.len() > limit {
        return Err(invalid_kernel_data());
    }
    String::from_utf8(bytes).map_err(|_| invalid_kernel_data())
}

fn read_optional_bounded(
    path: &Path,
    limit: usize,
) -> Result<Option<String>, LinuxHostObservationError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(observation_unavailable()),
    };
    let mut bytes = Vec::with_capacity(limit.min(MAX_SMALL_PROC_BYTES));
    file.take(u64::try_from(limit).map_err(|_| invalid_kernel_data())? + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| observation_unavailable())?;
    if bytes.len() > limit {
        return Err(invalid_kernel_data());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| invalid_kernel_data())
}

fn parse_cpu(stat: &str, loadavg: &str) -> Result<CpuObservation, LinuxHostObservationError> {
    let logical_cpus = stat
        .lines()
        .filter_map(|line| line.split_ascii_whitespace().next())
        .filter(|name| {
            name.strip_prefix("cpu").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
        .count();
    let logical_cpus = u16::try_from(logical_cpus).map_err(|_| invalid_kernel_data())?;
    if logical_cpus == 0 || logical_cpus > MAX_LOGICAL_CPUS {
        return Err(invalid_kernel_data());
    }

    let mut fields = loadavg.split_ascii_whitespace();
    let load_1m_micros =
        parse_decimal_micros(fields.next().ok_or_else(invalid_kernel_data)?, 1_000_000)?;
    let load_5m_micros =
        parse_decimal_micros(fields.next().ok_or_else(invalid_kernel_data)?, 1_000_000)?;
    let load_15m_micros =
        parse_decimal_micros(fields.next().ok_or_else(invalid_kernel_data)?, 1_000_000)?;
    let entities = fields.next().ok_or_else(invalid_kernel_data)?;
    let Some((runnable, total)) = entities.split_once('/') else {
        return Err(invalid_kernel_data());
    };
    let runnable_entities = parse_positive_u32(runnable)?;
    let total_entities = parse_positive_u32(total)?;
    if runnable_entities > total_entities {
        return Err(invalid_kernel_data());
    }
    let last_pid = fields.next().ok_or_else(invalid_kernel_data)?;
    let _ = last_pid.parse::<u32>().map_err(|_| invalid_kernel_data())?;
    if fields.next().is_some() {
        return Err(invalid_kernel_data());
    }

    Ok(CpuObservation {
        logical_cpus,
        load_1m_micros,
        load_5m_micros,
        load_15m_micros,
        runnable_entities,
        total_entities,
    })
}

fn parse_memory(value: &str) -> Result<MemoryObservation, LinuxHostObservationError> {
    let mut total = None;
    let mut available = None;
    let mut swap_total = None;
    let mut swap_free = None;
    for line in value.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            return Err(invalid_kernel_data());
        };
        let slot = match key {
            "MemTotal" => &mut total,
            "MemAvailable" => &mut available,
            "SwapTotal" => &mut swap_total,
            "SwapFree" => &mut swap_free,
            _ => continue,
        };
        if slot.is_some() {
            return Err(invalid_kernel_data());
        }
        let mut fields = rest.split_ascii_whitespace();
        let kib = fields
            .next()
            .ok_or_else(invalid_kernel_data)?
            .parse::<u64>()
            .map_err(|_| invalid_kernel_data())?;
        if fields.next() != Some("kB") || fields.next().is_some() {
            return Err(invalid_kernel_data());
        }
        *slot = Some(kib.checked_mul(1_024).ok_or_else(invalid_kernel_data)?);
    }
    let total = total.ok_or_else(invalid_kernel_data)?;
    let available = available.ok_or_else(invalid_kernel_data)?;
    let swap_total = swap_total.ok_or_else(invalid_kernel_data)?;
    let swap_free = swap_free.ok_or_else(invalid_kernel_data)?;
    if available > total || swap_free > swap_total {
        return Err(invalid_kernel_data());
    }
    Ok(MemoryObservation {
        total_bytes: total,
        available_bytes: available,
        swap_total_bytes: swap_total,
        swap_used_bytes: swap_total - swap_free,
    })
}

fn parse_pressure(value: &str) -> Result<PressureSample, LinuxHostObservationError> {
    let line = value
        .lines()
        .find(|line| line.starts_with("some "))
        .ok_or_else(invalid_kernel_data)?;
    let mut avg10 = None;
    let mut avg60 = None;
    let mut avg300 = None;
    let mut total = None;
    for field in line.split_ascii_whitespace().skip(1) {
        let Some((key, raw)) = field.split_once('=') else {
            return Err(invalid_kernel_data());
        };
        match key {
            "avg10" if avg10.is_none() => avg10 = Some(parse_decimal_micros(raw, 100)?),
            "avg60" if avg60.is_none() => avg60 = Some(parse_decimal_micros(raw, 100)?),
            "avg300" if avg300.is_none() => avg300 = Some(parse_decimal_micros(raw, 100)?),
            "total" if total.is_none() => {
                total = Some(raw.parse::<u64>().map_err(|_| invalid_kernel_data())?);
            }
            _ => return Err(invalid_kernel_data()),
        }
    }
    Ok(PressureSample {
        avg10_micros: u32::try_from(avg10.ok_or_else(invalid_kernel_data)?)
            .map_err(|_| invalid_kernel_data())?,
        avg60_micros: u32::try_from(avg60.ok_or_else(invalid_kernel_data)?)
            .map_err(|_| invalid_kernel_data())?,
        avg300_micros: u32::try_from(avg300.ok_or_else(invalid_kernel_data)?)
            .map_err(|_| invalid_kernel_data())?,
        total_micros: total.ok_or_else(invalid_kernel_data)?,
    })
}

fn parse_decimal_micros(value: &str, max_whole: u64) -> Result<u64, LinuxHostObservationError> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || fraction.len() > 6
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_kernel_data());
    }
    let whole = whole.parse::<u64>().map_err(|_| invalid_kernel_data())?;
    if whole > max_whole || (whole == max_whole && !fraction.bytes().all(|byte| byte == b'0')) {
        return Err(invalid_kernel_data());
    }
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u64>().map_err(|_| invalid_kernel_data())?
            * 10_u64.pow(u32::try_from(6 - fraction.len()).map_err(|_| invalid_kernel_data())?)
    };
    whole
        .checked_mul(1_000_000)
        .and_then(|scaled| scaled.checked_add(fraction))
        .ok_or_else(invalid_kernel_data)
}

fn parse_positive_u32(value: &str) -> Result<u32, LinuxHostObservationError> {
    let value = value.parse::<u32>().map_err(|_| invalid_kernel_data())?;
    if value == 0 {
        Err(invalid_kernel_data())
    } else {
        Ok(value)
    }
}

fn parse_listening_ports(
    tcp: &str,
    tcp6: &str,
) -> Result<BTreeSet<u16>, LinuxHostObservationError> {
    let mut ports = BTreeSet::new();
    for table in [tcp, tcp6] {
        let mut lines = table.lines();
        let header = lines.next().ok_or_else(invalid_kernel_data)?;
        if !header.contains("local_address") || !header.contains("st") {
            return Err(invalid_kernel_data());
        }
        for line in lines {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.len() < 4 {
                return Err(invalid_kernel_data());
            }
            if fields[3] != "0A" {
                continue;
            }
            let Some((address, port)) = fields[1].rsplit_once(':') else {
                return Err(invalid_kernel_data());
            };
            if address.is_empty() || !address.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid_kernel_data());
            }
            ports.insert(u16::from_str_radix(port, 16).map_err(|_| invalid_kernel_data())?);
        }
    }
    Ok(ports)
}

fn systemctl_command(user: bool, uid: u32) -> CommandSpec {
    let mut command = CommandSpec::new(SYSTEMCTL_PROGRAM)
        .argument(if user { "--user" } else { "--system" })
        .argument("--failed")
        .argument("--no-legend")
        .argument("--plain")
        .argument("--no-pager")
        .environment("LANG", "C")
        .environment("LC_ALL", "C");
    if user {
        let runtime = format!("/run/user/{uid}");
        command = command
            .environment(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={runtime}/bus"),
            )
            .environment("XDG_RUNTIME_DIR", runtime);
    }
    command
}

fn observe_failed_units(
    command: CommandSpec,
    executor: &impl TimedCommandExecutor,
) -> ObservedCount {
    let expected_argv = command.displayed_argv();
    let expected_environment_keys = command.environment.keys().cloned().collect::<Vec<_>>();
    let Ok(record) = executor.execute_with_timeout(&command, SYSTEMCTL_TIMEOUT) else {
        return ObservedCount::Unavailable;
    };
    if record.argv != expected_argv
        || record.environment_keys != expected_environment_keys
        || !record.success
        || record.status != Some(0)
        || !record.stderr.is_empty()
        || record.stdout.len() > MAX_FAILED_UNIT_OUTPUT_BYTES
        || record.stdout.contains(['\0', '\r', '\u{fffd}'])
    {
        return ObservedCount::Unavailable;
    }
    let count = record
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    match u16::try_from(count) {
        Ok(count) if count <= MAX_FAILED_UNITS => ObservedCount::Known { count },
        _ => ObservedCount::Unavailable,
    }
}

fn invalid_request() -> LinuxHostObservationError {
    LinuxHostObservationError {
        kind: LinuxHostObservationErrorKind::InvalidRequest,
        code: "invalid_host_observation_request",
        problem: "host observation request is outside the bounded supported shape",
    }
}

fn observation_unavailable() -> LinuxHostObservationError {
    LinuxHostObservationError {
        kind: LinuxHostObservationErrorKind::ObservationUnavailable,
        code: "host_observation_unavailable",
        problem: "required Linux host observation is unavailable",
    }
}

fn invalid_kernel_data() -> LinuxHostObservationError {
    LinuxHostObservationError {
        kind: LinuxHostObservationErrorKind::InvalidKernelData,
        code: "invalid_linux_host_observation",
        problem: "Linux returned invalid or untrusted host observation data",
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};

    use super::{
        CpuFrequencyClass, CpuFrequencyObservation, LinuxHostObservation,
        LinuxHostObservationError, LinuxHostObservationErrorKind, MAX_SCHED_EXT_OPS_NAME_BYTES,
        ObservedCount, SYSTEMCTL_TIMEOUT, SchedExtObservation, SchedExtState,
        SchedulerFeatureState, observe_linux_host_at, parse_cpu_list, parse_sched_ext_ops,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "glaeda-linux-host-observation-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("pressure")).expect("create pressure fixture");
            fs::create_dir_all(root.join("net")).expect("create network fixture");
            fs::create_dir_all(root.join("sys/kernel/sched_ext/root"))
                .expect("create scheduler fixture");
            fs::create_dir_all(root.join("sys/devices/system/cpu/smt"))
                .expect("create SMT fixture");
            for cpu in 0..2 {
                let cpufreq = root.join(format!("sys/devices/system/cpu/cpu{cpu}/cpufreq"));
                fs::create_dir_all(&cpufreq).expect("create CPU frequency fixture");
                fs::write(cpufreq.join("scaling_driver"), "test_pstate\n")
                    .expect("write scaling driver");
                fs::write(cpufreq.join("scaling_governor"), "powersave\n")
                    .expect("write scaling governor");
                fs::write(
                    cpufreq.join("cpuinfo_max_freq"),
                    if cpu == 0 { "5000000\n" } else { "4000000\n" },
                )
                .expect("write maximum CPU frequency");
                fs::write(cpufreq.join("scaling_min_freq"), "400000\n")
                    .expect("write policy minimum CPU frequency");
                fs::write(
                    cpufreq.join("scaling_max_freq"),
                    if cpu == 0 { "5000000\n" } else { "3500000\n" },
                )
                .expect("write policy maximum CPU frequency");
                fs::write(
                    cpufreq.join("energy_performance_preference"),
                    "balance_performance\n",
                )
                .expect("write energy performance preference");
            }
            fs::write(root.join("sys/devices/system/cpu/online"), "0-1\n")
                .expect("write online CPUs");
            fs::write(root.join("sys/devices/system/cpu/nohz_full"), "\n")
                .expect("write nohz_full CPUs");
            fs::write(root.join("sys/devices/system/cpu/isolated"), "\n")
                .expect("write isolated CPUs");
            fs::write(root.join("sys/devices/system/cpu/smt/active"), "0\n")
                .expect("write SMT state");
            fs::write(
                root.join("stat"),
                "cpu  1 2 3 4 5 6 7 8 9 10\ncpu0 1 2 3 4 5 6 7 8 9 10\ncpu1 1 2 3 4 5 6 7 8 9 10\nintr 1\n",
            )
            .expect("write stat");
            fs::write(root.join("loadavg"), "1.25 2.50 3.75 2/100 123\n").expect("write loadavg");
            fs::write(root.join("sys/kernel/sched_autogroup_enabled"), "1\n")
                .expect("write autogroup");
            fs::write(root.join("sys/kernel/sched_ext/state"), "disabled\n")
                .expect("write sched_ext state");
            fs::write(root.join("sys/kernel/sched_ext/enable_seq"), "0\n")
                .expect("write sched_ext sequence");
            fs::write(
                root.join("meminfo"),
                "MemTotal:       1000000 kB\nMemFree:         250000 kB\nMemAvailable:    750000 kB\nSwapTotal:       200000 kB\nSwapFree:        125000 kB\n",
            )
            .expect("write meminfo");
            for (name, value) in [
                ("cpu", "some avg10=1.25 avg60=2.50 avg300=3.75 total=10\n"),
                (
                    "memory",
                    "some avg10=0.00 avg60=0.01 avg300=0.02 total=20\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=1\n",
                ),
                ("io", "some avg10=4.00 avg60=5.00 avg300=6.00 total=30\n"),
            ] {
                fs::write(root.join("pressure").join(name), value).expect("write pressure");
            }
            let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
            fs::write(
                root.join("net/tcp"),
                format!(
                    "{header}   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 1\n"
                ),
            )
            .expect("write tcp");
            fs::write(
                root.join("net/tcp6"),
                format!(
                    "{header}   0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 2\n"
                ),
            )
            .expect("write tcp6");
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn observe_fixture(
        fixture: &Fixture,
        ports: &[u16],
        uid: u32,
        executor: &impl TimedCommandExecutor,
    ) -> Result<LinuxHostObservation, LinuxHostObservationError> {
        observe_linux_host_at(
            fixture.path(),
            &fixture.path().join("sys"),
            ports,
            uid,
            1_000,
            executor,
        )
    }

    struct ScriptedExecutor {
        records: RefCell<VecDeque<ExecutionRecord>>,
        commands: RefCell<Vec<CommandSpec>>,
    }

    impl ScriptedExecutor {
        fn successful(system: &str, user: &str) -> Self {
            Self {
                records: RefCell::new(
                    [record(system, 0, ""), record(user, 0, "")]
                        .into_iter()
                        .collect(),
                ),
                commands: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandExecutor for ScriptedExecutor {
        fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            panic!("host observation must use the timed executor")
        }
    }

    impl TimedCommandExecutor for ScriptedExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            assert_eq!(timeout, SYSTEMCTL_TIMEOUT);
            self.commands.borrow_mut().push(spec.clone());
            let mut record = self.records.borrow_mut().pop_front().expect("record");
            record.argv = spec.displayed_argv();
            record.environment_keys = spec.environment.keys().cloned().collect();
            Ok(record)
        }
    }

    fn record(stdout: &str, status: i32, stderr: &str) -> ExecutionRecord {
        ExecutionRecord {
            argv: Vec::new(),
            environment_keys: Vec::new(),
            status: Some(status),
            success: status == 0,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn exact_fixture_produces_typed_path_private_report() {
        let fixture = Fixture::new();
        let executor = ScriptedExecutor::successful(
            "failed-a.service loaded failed failed\nfailed-b.service loaded failed failed\n",
            "",
        );
        let report = observe_fixture(&fixture, &[8080, 3000, 3000, 5173], 1000, &executor)
            .expect("observation");
        assert_eq!(report.cpu().logical_cpus, 2);
        assert_eq!(report.cpu().load_1m_micros, 1_250_000);
        assert_eq!(report.memory().total_bytes, 1_024_000_000);
        assert_eq!(report.memory().swap_used_bytes, 76_800_000);
        assert_eq!(report.pressure().cpu.avg10_micros, 1_250_000);
        assert_eq!(report.scheduler().autogroup, SchedulerFeatureState::Enabled);
        assert_eq!(
            report.scheduler().sched_ext,
            SchedExtObservation::Supported {
                state: SchedExtState::Disabled,
                enable_sequence: 0,
                active_ops: None,
            }
        );
        assert_eq!(report.scheduler().cpu_policy.online_logical_cpus, 2);
        assert_eq!(
            report.scheduler().cpu_policy.smt,
            SchedulerFeatureState::Disabled
        );
        assert_eq!(
            report.scheduler().cpu_policy.nohz_full_online_logical_cpus,
            0
        );
        assert_eq!(
            report.scheduler().cpu_policy.isolated_online_logical_cpus,
            0
        );
        assert_eq!(
            report.scheduler().cpu_policy.frequency,
            CpuFrequencyObservation::Supported {
                classes: vec![
                    CpuFrequencyClass {
                        driver: "test_pstate".to_owned(),
                        governor: "powersave".to_owned(),
                        energy_performance_preference: Some("balance_performance".to_owned()),
                        hardware_max_frequency_khz: 4_000_000,
                        policy_min_frequency_khz: 400_000,
                        policy_max_frequency_khz: 3_500_000,
                        logical_cpus: 1,
                    },
                    CpuFrequencyClass {
                        driver: "test_pstate".to_owned(),
                        governor: "powersave".to_owned(),
                        energy_performance_preference: Some("balance_performance".to_owned()),
                        hardware_max_frequency_khz: 5_000_000,
                        policy_min_frequency_khz: 400_000,
                        policy_max_frequency_khz: 5_000_000,
                        logical_cpus: 1,
                    },
                ],
            }
        );
        assert_eq!(report.services().system, ObservedCount::Known { count: 2 });
        assert_eq!(report.services().user, ObservedCount::Known { count: 0 });
        assert_eq!(
            report
                .watched_ports()
                .iter()
                .map(|entry| (entry.port, entry.listening))
                .collect::<Vec<_>>(),
            vec![(3000, true), (5173, false), (8080, true)]
        );
        let encoded = serde_json::to_string(&report).expect("serialize report");
        assert!(!encoded.contains(fixture.path().to_str().expect("UTF-8 fixture")));
        assert!(!encoded.contains("failed-a"));
    }

    #[test]
    fn systemd_failures_are_unknown_without_losing_kernel_evidence() {
        let fixture = Fixture::new();
        let executor = ScriptedExecutor::successful("", "");
        executor.records.borrow_mut()[0] = record("", 1, "offline\n");
        executor.records.borrow_mut()[1] = record("", 1, "offline\n");
        let report =
            observe_fixture(&fixture, &[3000], 1000, &executor).expect("partial observation");
        assert_eq!(report.services().system, ObservedCount::Unavailable);
        assert_eq!(report.services().user, ObservedCount::Unavailable);
    }

    #[test]
    fn requests_and_kernel_data_fail_closed() {
        let fixture = Fixture::new();
        let executor = ScriptedExecutor::successful("", "");
        let error =
            observe_fixture(&fixture, &[], 1000, &executor).expect_err("empty watched ports");
        assert_eq!(error.kind, LinuxHostObservationErrorKind::InvalidRequest);

        fs::write(fixture.path().join("loadavg"), "not-loadavg\n").expect("corrupt loadavg");
        let error =
            observe_fixture(&fixture, &[3000], 1000, &executor).expect_err("invalid kernel data");
        assert_eq!(error.kind, LinuxHostObservationErrorKind::InvalidKernelData);
        assert!(
            !error
                .to_string()
                .contains(fixture.path().to_str().expect("UTF-8 fixture"))
        );
    }

    #[test]
    fn systemctl_commands_are_fixed_and_environment_minimal() {
        let fixture = Fixture::new();
        let executor = ScriptedExecutor::successful("", "");
        observe_fixture(&fixture, &[3000], 42, &executor).expect("observation");
        let commands = executor.commands.borrow();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0].displayed_argv(),
            [
                "/usr/bin/systemctl",
                "--system",
                "--failed",
                "--no-legend",
                "--plain",
                "--no-pager"
            ]
        );
        assert_eq!(
            commands[1].displayed_argv(),
            [
                "/usr/bin/systemctl",
                "--user",
                "--failed",
                "--no-legend",
                "--plain",
                "--no-pager"
            ]
        );
        assert_eq!(commands[0].environment.len(), 2);
        assert_eq!(commands[1].environment.len(), 4);
    }

    #[test]
    fn sched_ext_support_and_active_ops_are_typed() {
        let fixture = Fixture::new();
        fs::write(
            fixture.path().join("sys/kernel/sched_ext/state"),
            "enabled\n",
        )
        .expect("enable sched_ext fixture");
        fs::write(
            fixture.path().join("sys/kernel/sched_ext/enable_seq"),
            "17\n",
        )
        .expect("write sched_ext fixture sequence");
        fs::write(
            fixture.path().join("sys/kernel/sched_ext/root/ops"),
            "lavd_1.1.3_x86_64_unknown_linux_gnu\n",
        )
        .expect("write sched_ext fixture ops");
        let executor = ScriptedExecutor::successful("", "");
        let report = observe_fixture(&fixture, &[3000], 1000, &executor)
            .expect("enabled scheduler observation");
        assert_eq!(
            report.scheduler().sched_ext,
            SchedExtObservation::Supported {
                state: SchedExtState::Enabled,
                enable_sequence: 17,
                active_ops: Some("lavd_1.1.3_x86_64_unknown_linux_gnu".to_owned()),
            }
        );

        fs::write(
            fixture.path().join("sys/kernel/sched_ext/root/ops"),
            "bad-name\n",
        )
        .expect("write malformed sched_ext ops");
        let executor = ScriptedExecutor::successful("", "");
        let error = observe_fixture(&fixture, &[3000], 1000, &executor)
            .expect_err("malformed scheduler ops");
        assert_eq!(error.kind, LinuxHostObservationErrorKind::InvalidKernelData);
    }

    #[test]
    fn sched_ext_ops_name_matches_the_kernel_object_name_contract() {
        assert_eq!(
            parse_sched_ext_ops("lavd_1.1.3_x86_64_unknown_linux_gnu\n")
                .expect("versioned kernel ops name"),
            "lavd_1.1.3_x86_64_unknown_linux_gnu"
        );
        assert_eq!(
            parse_sched_ext_ops(&"a".repeat(MAX_SCHED_EXT_OPS_NAME_BYTES))
                .expect("maximum kernel ops name"),
            "a".repeat(MAX_SCHED_EXT_OPS_NAME_BYTES)
        );
        let oversized = "a".repeat(MAX_SCHED_EXT_OPS_NAME_BYTES + 1);
        for invalid in [
            "",
            "bad-name",
            "bad/name",
            "bad\\name",
            " lavd",
            "lavd ",
            "\tlavd",
            "lavd\t",
            "lavd\r\n",
            "lavd\n\n",
            "lavd\nother",
            oversized.as_str(),
        ] {
            assert_eq!(
                parse_sched_ext_ops(invalid)
                    .expect_err("invalid ops name")
                    .kind,
                LinuxHostObservationErrorKind::InvalidKernelData
            );
        }
    }

    #[test]
    fn absent_sched_ext_is_unsupported_but_malformed_data_fails() {
        let fixture = Fixture::new();
        fs::remove_dir_all(fixture.path().join("sys/kernel/sched_ext"))
            .expect("remove sched_ext fixture");
        let executor = ScriptedExecutor::successful("", "");
        let report = observe_fixture(&fixture, &[3000], 1000, &executor)
            .expect("unsupported scheduler observation");
        assert_eq!(
            report.scheduler().sched_ext,
            SchedExtObservation::Unsupported
        );

        fs::remove_file(fixture.path().join("sys/kernel/sched_autogroup_enabled"))
            .expect("remove autogroup fixture");
        let executor = ScriptedExecutor::successful("", "");
        let report = observe_fixture(&fixture, &[3000], 1000, &executor)
            .expect("unsupported autogroup observation");
        assert_eq!(
            report.scheduler().autogroup,
            SchedulerFeatureState::Unsupported
        );

        fs::create_dir_all(fixture.path().join("sys/kernel/sched_ext"))
            .expect("recreate sched_ext fixture");
        fs::write(
            fixture.path().join("sys/kernel/sched_ext/state"),
            "unknown\n",
        )
        .expect("write invalid sched_ext state");
        fs::write(
            fixture.path().join("sys/kernel/sched_ext/enable_seq"),
            "0\n",
        )
        .expect("write sched_ext sequence");
        let executor = ScriptedExecutor::successful("", "");
        let error = observe_fixture(&fixture, &[3000], 1000, &executor)
            .expect_err("malformed scheduler data");
        assert_eq!(error.kind, LinuxHostObservationErrorKind::InvalidKernelData);
    }

    #[test]
    fn cpu_lists_are_canonical_bounded_sets() {
        assert_eq!(
            parse_cpu_list("0-2,7,9-10\n", false)
                .expect("valid CPU list")
                .into_iter()
                .collect::<Vec<_>>(),
            [0, 1, 2, 7, 9, 10]
        );
        assert!(
            parse_cpu_list("\n", true)
                .expect("empty optional list")
                .is_empty()
        );
        assert!(
            parse_cpu_list("(null)\n", true)
                .expect("kernel null optional list")
                .is_empty()
        );
        for malformed in ["", "0-1,1", "2-1", "0,,1", "0-1-2", "4096", "x"] {
            assert!(
                parse_cpu_list(malformed, false).is_err(),
                "accepted {malformed:?}"
            );
        }
    }

    #[test]
    fn absent_optional_cpu_policy_is_typed_but_partial_data_fails() {
        let fixture = Fixture::new();
        fs::remove_dir_all(fixture.path().join("sys/devices/system/cpu/smt"))
            .expect("remove SMT fixture");
        for cpu in 0..2 {
            fs::remove_dir_all(
                fixture
                    .path()
                    .join(format!("sys/devices/system/cpu/cpu{cpu}/cpufreq")),
            )
            .expect("remove CPU frequency fixture");
        }
        let executor = ScriptedExecutor::successful("", "");
        let report = observe_fixture(&fixture, &[3000], 1000, &executor)
            .expect("unsupported optional CPU policy");
        assert_eq!(
            report.scheduler().cpu_policy.smt,
            SchedulerFeatureState::Unsupported
        );
        assert_eq!(
            report.scheduler().cpu_policy.frequency,
            CpuFrequencyObservation::Unsupported
        );

        let fixture = Fixture::new();
        fs::remove_file(
            fixture
                .path()
                .join("sys/devices/system/cpu/cpu1/cpufreq/scaling_governor"),
        )
        .expect("remove one governor");
        let executor = ScriptedExecutor::successful("", "");
        let error = observe_fixture(&fixture, &[3000], 1000, &executor)
            .expect_err("partial CPU frequency policy");
        assert_eq!(error.kind, LinuxHostObservationErrorKind::InvalidKernelData);
    }

    #[test]
    fn cpu_policy_counts_only_online_members_and_rejects_invalid_frequency_bounds() {
        let fixture = Fixture::new();
        fs::write(
            fixture.path().join("sys/devices/system/cpu/nohz_full"),
            "2\n",
        )
        .expect("write offline nohz_full CPU");
        let executor = ScriptedExecutor::successful("", "");
        let report = observe_fixture(&fixture, &[3000], 1000, &executor)
            .expect("offline configured nohz_full CPU");
        assert_eq!(
            report.scheduler().cpu_policy.nohz_full_online_logical_cpus,
            0
        );

        let fixture = Fixture::new();
        fs::write(
            fixture
                .path()
                .join("sys/devices/system/cpu/cpu0/cpufreq/scaling_min_freq"),
            "6000000\n",
        )
        .expect("write impossible policy bounds");
        let executor = ScriptedExecutor::successful("", "");
        let error = observe_fixture(&fixture, &[3000], 1000, &executor)
            .expect_err("minimum frequency above maximum");
        assert_eq!(error.kind, LinuxHostObservationErrorKind::InvalidKernelData);

        let fixture = Fixture::new();
        fs::write(fixture.path().join("sys/devices/system/cpu/online"), "0\n")
            .expect("write one online CPU");
        let executor = ScriptedExecutor::successful("", "");
        let report =
            observe_fixture(&fixture, &[3000], 1000, &executor).expect("one online CPU policy");
        assert_eq!(report.scheduler().cpu_policy.online_logical_cpus, 1);
    }
}
