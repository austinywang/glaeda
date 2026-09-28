//! Low-fixed-cost measurement front door for ultra-trusted same-worktree commands.
//!
//! This deliberately owns only direct native execution. Cross-worktree cache views and source
//! composition remain in `scripts/hot-run` until their existing semantics are migrated intact.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read as _, Write as _};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt as _;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, ExitStatus};
use std::process::{Command, ExitCode, Stdio};
#[cfg(target_os = "linux")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
#[cfg(target_os = "linux")]
use glaeda::cargo_target_observation::{
    CargoTargetObservation, CargoTargetObservationError, observe_cargo_target,
};
#[cfg(target_os = "linux")]
use glaeda::process::ProcessExecutor;
#[cfg(target_os = "linux")]
use glaeda::project_checkout_observation::{
    ProjectCheckoutObservation, ProjectCheckoutObservationError, ProjectCheckoutObserver,
};
use rustix::process::geteuid;
#[cfg(target_os = "linux")]
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fd::OwnedFd,
    fs::{
        AtFlags, FileType, FlockOperation, Mode, OFlags, fcntl_getfl, fcntl_setfl, flock, fstat,
        mkdirat, open as rustix_open, openat as rustix_openat, statat,
    },
    io::{Errno, FdFlags, fcntl_getfd, fcntl_setfd, write as rustix_write},
    process::{
        Pid, PidfdFlags, Signal, WaitOptions, getgid, getpgid, getpid, getuid, kill_process,
        kill_process_group, pidfd_open, pidfd_send_signal, test_kill_process_group, waitpid,
    },
    thread::{CpuSet, sched_getaffinity, sched_setaffinity},
};
#[cfg(target_os = "linux")]
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};
#[cfg(target_os = "linux")]
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

const MAX_OBSERVATION_BYTES: u64 = 64 * 1024;
#[cfg(target_os = "linux")]
const TERMINATION_GRACE: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const RESOURCE_SCOPE_OBSERVATION_GRACE: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const CPU_GRANT_RELEASE_GRACE: Duration = Duration::from_millis(250);
#[cfg(target_os = "linux")]
const INTERNAL_SCOPE_ENTRY: &str = "--glaeda-internal-scope-entry-v1";
#[cfg(target_os = "linux")]
const INTERNAL_CPU_GRANT_KEEPER: &str = "--glaeda-internal-cpu-grant-keeper-v1";
#[cfg(target_os = "linux")]
const HEAVY_SCOPE_PROPERTIES: &[&str] = &[
    "CPUQuota=1200%",
    "MemoryHigh=8G",
    "MemoryMax=12G",
    "TasksMax=1024",
];
#[cfg(target_os = "linux")]
const BACKGROUND_SCOPE_PROPERTIES: &[&str] = &["CPUWeight=25"];
const SHA256_PREFIX: &str = "sha256:";
const GIT_OVERRIDE_NAMES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];
const PRESSURE_KINDS: &[&str] = &["cpu", "memory", "io"];

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-hot-run",
    about = "Measure an ultra-trusted command directly in one native Git worktree"
)]
struct Cli {
    /// Resident Git worktree root. It must be the task's physical worktree root.
    #[arg(long)]
    resident: PathBuf,
    /// Task working directory within the resident Git worktree.
    #[arg(long)]
    task: PathBuf,
    /// Record one resident-relative cache path as direct native state.
    #[arg(long, action = clap::ArgAction::Append)]
    cache: Vec<String>,
    /// Atomically write one bounded schema-v6 developer observation.
    #[arg(long)]
    measurement: Option<PathBuf>,
    /// Caller-owned exact-work comparison digest recorded only in a measurement.
    #[arg(long)]
    comparison_key: Option<String>,
    /// Bounded public identity for the resolved runtime executable.
    #[arg(long)]
    runtime_id: Option<String>,
    /// Optional expected digest for the resolved runtime executable.
    #[arg(long)]
    runtime_sha256: Option<String>,
    /// Canonical toolchain bin directory placed first in descendant PATH.
    #[arg(long)]
    runtime_bin: Option<PathBuf>,
    /// Positive finite wall-clock deadline for the complete owned execution boundary.
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<f64>,
    /// Place the command in one reviewed transient resource scope.
    #[arg(long, value_enum)]
    resource_profile: Option<ResourceProfile>,
    /// Reserve and restrict one benchmark sample to this canonical CPU list.
    #[arg(long, value_name = "LIST")]
    cpu_set: Option<String>,
    /// Absolute executable or PATH-resolved command followed by its arguments.
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ResourceProfile {
    #[value(name = "big-red-heavy")]
    BigRedHeavy,
    #[value(name = "big-red-background")]
    BigRedBackground,
}

impl ResourceProfile {
    #[cfg(target_os = "linux")]
    fn as_str(self) -> &'static str {
        match self {
            Self::BigRedHeavy => "big-red-heavy",
            Self::BigRedBackground => "big-red-background",
        }
    }

    #[cfg(target_os = "linux")]
    fn scope_properties(self) -> &'static [&'static str] {
        match self {
            Self::BigRedHeavy => HEAVY_SCOPE_PROPERTIES,
            Self::BigRedBackground => BACKGROUND_SCOPE_PROPERTIES,
        }
    }
}

#[cfg(target_os = "linux")]
fn parse_cpu_set(value: &str, inherited: &CpuSet) -> Result<CpuSetRequest, String> {
    if value.is_empty() || value.len() > 4096 {
        return Err("CPU set must be a nonempty bounded canonical list".into());
    }
    let mut cpus = BTreeSet::new();
    for component in value.split(',') {
        if component.is_empty() {
            return Err("CPU set is not a canonical list".into());
        }
        let fields = component.split('-').collect::<Vec<_>>();
        let (first, last) = match fields.as_slice() {
            [single] => {
                let cpu = parse_cpu_id(single)?;
                (cpu, cpu)
            }
            [first, last] => (parse_cpu_id(first)?, parse_cpu_id(last)?),
            _ => return Err("CPU set is not a canonical list".into()),
        };
        if first > last {
            return Err("CPU set range is descending".into());
        }
        for cpu in first..=last {
            if !cpus.insert(cpu) {
                return Err("CPU set contains duplicate or overlapping CPUs".into());
            }
        }
    }
    let cpus = cpus.into_iter().collect::<Vec<_>>();
    let canonical = canonical_cpu_list(&cpus);
    if value != canonical {
        return Err(format!("CPU set is not canonical; use {canonical}"));
    }
    let mut mask = CpuSet::new();
    for cpu in &cpus {
        if !inherited.is_set(*cpu) {
            return Err("CPU set includes a CPU outside the effective inherited affinity".into());
        }
        mask.set(*cpu);
    }
    Ok(CpuSetRequest {
        cpus,
        canonical,
        mask,
    })
}

#[cfg(target_os = "linux")]
fn parse_cpu_id(value: &str) -> Result<usize, String> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("CPU set contains a noncanonical CPU identifier".into());
    }
    let cpu = value
        .parse::<usize>()
        .map_err(|_| "CPU set contains an invalid CPU identifier".to_owned())?;
    if cpu >= CpuSet::MAX_CPU {
        return Err("CPU set exceeds the supported kernel CPU mask".into());
    }
    Ok(cpu)
}

#[cfg(target_os = "linux")]
fn canonical_cpu_list(cpus: &[usize]) -> String {
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < cpus.len() {
        let first = cpus[index];
        let mut last = first;
        while index + 1 < cpus.len() && cpus[index + 1] == last + 1 {
            index += 1;
            last = cpus[index];
        }
        ranges.push(if first == last {
            first.to_string()
        } else {
            format!("{first}-{last}")
        });
        index += 1;
    }
    ranges.join(",")
}

#[cfg(target_os = "linux")]
fn acquire_cpu_grant(value: &str) -> Result<CpuGrant, String> {
    let inherited =
        sched_getaffinity(None).map_err(|_| "effective CPU affinity is unavailable".to_owned())?;
    let request = parse_cpu_set(value, &inherited)?;
    let runtime_path = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "XDG_RUNTIME_DIR is unavailable or not absolute".to_owned())?;
    let runtime = rustix_open(
        &runtime_path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "XDG_RUNTIME_DIR cannot be opened safely".to_owned())?;
    inspect_private_cpu_grant_directory(&runtime, "XDG_RUNTIME_DIR")?;
    match mkdirat(&runtime, "glaeda-cpu-grants", Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => return Err("CPU grant directory cannot be created".into()),
    }
    let directory = rustix_openat(
        &runtime,
        "glaeda-cpu-grants",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "CPU grant directory cannot be opened safely".to_owned())?;
    inspect_private_cpu_grant_directory(&directory, "CPU grant directory")?;
    inspect_private_cpu_grant_directory(&runtime, "XDG_RUNTIME_DIR")?;

    let mut locks = Vec::with_capacity(request.cpus.len());
    for cpu in &request.cpus {
        let name = format!("cpu-{cpu}.lock");
        let lock = rustix_openat(
            &directory,
            name.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| "CPU grant lock cannot be opened safely".to_owned())?;
        inspect_private_cpu_grant_lock(&directory, &name, &lock)?;
        let deadline = Instant::now()
            .checked_add(CPU_GRANT_RELEASE_GRACE)
            .ok_or_else(|| "CPU grant release observation exceeds the clock range".to_owned())?;
        loop {
            match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(Errno::WOULDBLOCK) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(Errno::WOULDBLOCK) => return Err(format!("CPU {cpu} is already reserved")),
                Err(_) => return Err("CPU grant lock cannot be acquired".into()),
            }
        }
        inspect_private_cpu_grant_lock(&directory, &name, &lock)?;
        locks.push(lock);
    }
    Ok(CpuGrant {
        request,
        _directory: directory,
        _locks: locks,
    })
}

#[cfg(target_os = "linux")]
fn inspect_private_cpu_grant_directory(directory: &OwnedFd, subject: &str) -> Result<(), String> {
    let stat = fstat(directory).map_err(|_| format!("{subject} cannot be inspected"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_uid != getuid().as_raw()
        || stat.st_gid != getgid().as_raw()
        || stat.st_mode & 0o7777 != 0o700
    {
        return Err(format!("{subject} is not a private current-user directory"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn inspect_private_cpu_grant_lock(
    directory: &OwnedFd,
    name: &str,
    lock: &OwnedFd,
) -> Result<(), String> {
    let held = fstat(lock).map_err(|_| "CPU grant lock cannot be inspected".to_owned())?;
    let linked = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| "CPU grant lock cannot be rebound".to_owned())?;
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || held.st_nlink != 1
        || held.st_uid != getuid().as_raw()
        || held.st_gid != getgid().as_raw()
        || held.st_mode & 0o7777 != 0o600
        || held.st_size != 0
        || (held.st_dev, held.st_ino) != (linked.st_dev, linked.st_ino)
    {
        return Err("CPU grant lock is not one private stable empty file".into());
    }
    Ok(())
}

#[derive(Debug)]
struct NativeCache {
    path: String,
}

#[derive(Debug)]
struct RuntimeDeclaration {
    id: String,
    expected_program_sha256: Option<String>,
}

#[derive(Debug)]
struct RuntimeBinBinding {
    path: PathBuf,
    identity_sha256: String,
}

#[derive(Debug)]
struct RuntimeContract {
    id: String,
    program_sha256: String,
    runtime_bin_binding_sha256: Option<String>,
}

#[derive(Debug)]
struct CommandResult {
    elapsed: Duration,
    timeout_seconds: Option<f64>,
    resource_profile: Option<&'static str>,
    cpu_set: Option<String>,
    user_cpu_seconds: Option<f64>,
    system_cpu_seconds: Option<f64>,
    max_rss_kib: Option<u64>,
    resource_accounting: &'static str,
    exit_code: i32,
    signal: Option<i32>,
    completion_reason: &'static str,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone)]
struct CpuSetRequest {
    cpus: Vec<usize>,
    canonical: String,
    #[cfg(target_os = "linux")]
    mask: CpuSet,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug)]
struct CommandExecution<'a> {
    command: &'a [OsString],
    cwd: &'a Path,
    environment_path: Option<&'a OsStr>,
    timeout: Option<Duration>,
    timeout_seconds: Option<f64>,
    resource_profile: Option<ResourceProfile>,
    cpu_set: Option<&'a CpuSetRequest>,
    #[cfg(target_os = "linux")]
    cpu_locks: &'a [OwnedFd],
    #[cfg(not(target_os = "linux"))]
    cpu_locks: &'a [()],
    measured: bool,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct CpuGrant {
    request: CpuSetRequest,
    _directory: OwnedFd,
    _locks: Vec<OwnedFd>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct NativeTargetSnapshot {
    checkout: ProjectCheckoutObservation,
    cargo_target: CargoTargetObservation,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum TerminalObservation<T, E> {
    Observed { observation: T },
    Unavailable { error: E },
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct NativeTargetTerminalSnapshot {
    checkout: TerminalObservation<ProjectCheckoutObservation, ProjectCheckoutObservationError>,
    cargo_target: TerminalObservation<CargoTargetObservation, CargoTargetObservationError>,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct NativeTargetMeasurementObservation {
    before: NativeTargetSnapshot,
    after: NativeTargetTerminalSnapshot,
    before_elapsed: Duration,
    after_elapsed: Duration,
}

#[derive(Debug)]
struct MeasurementObservations {
    machine_before: Value,
    machine_after: Value,
    native_target: Value,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct OwnedResourceScope {
    cgroup: OwnedFd,
    kill: OwnedFd,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct CpuGrantKeeper {
    child: Child,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ResourceScopeEntryExecutable {
    executable: File,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineWaitOutcome {
    Exited,
    Interrupted(i32),
    Deadline,
}

fn main() -> ExitCode {
    #[cfg(target_os = "linux")]
    if env::args_os().nth(1).as_deref() == Some(OsStr::new(INTERNAL_SCOPE_ENTRY)) {
        let arguments = env::args_os().skip(2).collect::<Vec<_>>();
        return match run_resource_scope_entry(arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("glaeda-hot-run scope-entry error: {error}");
                ExitCode::from(126)
            }
        };
    }
    #[cfg(target_os = "linux")]
    if env::args_os().nth(1).as_deref() == Some(OsStr::new(INTERNAL_CPU_GRANT_KEEPER)) {
        let arguments = env::args_os().skip(2).collect::<Vec<_>>();
        return match run_cpu_grant_keeper(arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("glaeda-hot-run CPU grant keeper error: {error}");
                ExitCode::from(126)
            }
        };
    }
    match run(Cli::parse()) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(255)),
        Err(error) => {
            eprintln!("glaeda-hot-run error: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(target_os = "linux")]
fn run_resource_scope_entry(mut arguments: Vec<OsString>) -> Result<(), String> {
    let cpu_set = if arguments.first().map(OsString::as_os_str) == Some(OsStr::new("--cpu-set")) {
        if arguments.len() < 3 {
            return Err("internal scope entry CPU set is missing".into());
        }
        arguments.remove(0);
        let value = arguments.remove(0);
        let value = value
            .to_str()
            .ok_or_else(|| "internal scope entry CPU set is invalid".to_owned())?;
        let inherited = sched_getaffinity(None)
            .map_err(|_| "internal scope entry affinity is unavailable".to_owned())?;
        Some(parse_cpu_set(value, &inherited)?)
    } else {
        None
    };
    if arguments.first().map(OsString::as_os_str) != Some(OsStr::new("--")) {
        return Err("internal scope entry arguments are invalid".into());
    }
    arguments.remove(0);
    if arguments.is_empty() {
        return Err("internal scope entry command is missing".into());
    }
    if let Some(cpu_set) = cpu_set.as_ref() {
        sched_setaffinity(None, &cpu_set.mask)
            .map_err(|_| "internal scope entry could not apply CPU affinity".to_owned())?;
        seal_cpu_affinity().map_err(|error| {
            format!("internal scope entry could not seal CPU affinity: {error}")
        })?;
    }
    kill_process(getpid(), Signal::STOP)
        .map_err(|_| "internal scope entry could not stop before admission".to_owned())?;
    let error = Command::new(&arguments[0]).args(&arguments[1..]).exec();
    Err(format!("cannot enter admitted resource scope: {error}"))
}

#[cfg(target_os = "linux")]
fn inspect_inherited_cpu_grant_lock(cpu: usize, fd: i32) -> Result<(), String> {
    let descriptor = PathBuf::from(format!("/proc/self/fd/{fd}"));
    let stat = fs::metadata(&descriptor)
        .map_err(|_| "internal scope entry CPU lock descriptor is unavailable".to_owned())?;
    let target = fs::read_link(&descriptor)
        .map_err(|_| "internal scope entry CPU lock descriptor cannot be resolved".to_owned())?;
    if !stat.file_type().is_file()
        || stat.nlink() != 1
        || stat.uid() != getuid().as_raw()
        || stat.gid() != getgid().as_raw()
        || stat.mode() & 0o7777 != 0o600
        || stat.size() != 0
        || target.file_name() != Some(OsStr::new(&format!("cpu-{cpu}.lock")))
    {
        return Err("internal scope entry CPU lock descriptor is unsafe".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn seal_cpu_affinity() -> Result<(), String> {
    let architecture = std::env::consts::ARCH
        .try_into()
        .map_err(|_| "the host architecture is unsupported".to_owned())?;
    let mut rules = BTreeMap::new();
    #[cfg(target_arch = "x86_64")]
    {
        // x32 shares AUDIT_ARCH_X86_64 with the native ABI but sets bit 30 in
        // seccomp_data.nr. Seal both encodings even when libc already exposes
        // the bit-set number from an x32 build.
        const X32_SYSCALL_BIT: i64 = 0x4000_0000;
        let native_number = libc::SYS_sched_setaffinity & !X32_SYSCALL_BIT;
        rules.insert(native_number, Vec::new());
        rules.insert(native_number | X32_SYSCALL_BIT, Vec::new());
    }
    #[cfg(not(target_arch = "x86_64"))]
    rules.insert(libc::SYS_sched_setaffinity, Vec::new());
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        architecture,
    )
    .map_err(|error| format!("filter construction failed: {error}"))?;
    let program: BpfProgram = filter
        .try_into()
        .map_err(|error| format!("filter compilation failed: {error}"))?;
    seccompiler::apply_filter(&program)
        .map_err(|error| format!("filter installation failed: {error}"))
}

#[cfg(target_os = "linux")]
fn run_cpu_grant_keeper(mut arguments: Vec<OsString>) -> Result<(), String> {
    if arguments.first().map(OsString::as_os_str) != Some(OsStr::new("--cgroup-fd"))
        || arguments.len() < 4
    {
        return Err("internal CPU grant keeper cgroup descriptor is missing".into());
    }
    arguments.remove(0);
    let cgroup_fd = parse_internal_fd(arguments.remove(0), "cgroup")?;
    let mut locks = Vec::new();
    while arguments.first().map(OsString::as_os_str) == Some(OsStr::new("--cpu-lock-fd")) {
        if arguments.len() < 3 {
            return Err("internal CPU grant keeper lock descriptor is missing".into());
        }
        arguments.remove(0);
        let cpu = arguments
            .remove(0)
            .to_str()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|cpu| *cpu < CpuSet::MAX_CPU)
            .ok_or_else(|| "internal CPU grant keeper CPU identifier is invalid".to_owned())?;
        let fd = parse_internal_fd(arguments.remove(0), "lock")?;
        inspect_inherited_cpu_grant_lock(cpu, fd)?;
        locks.push(fd);
    }
    if !arguments.is_empty() || locks.is_empty() {
        return Err("internal CPU grant keeper arguments are invalid".into());
    }
    if !cpu_grant_scope_is_populated(cgroup_fd)? {
        return Err("internal CPU grant keeper scope is not populated".into());
    }
    std::io::stdout()
        .write_all(b"1")
        .and_then(|()| std::io::stdout().flush())
        .map_err(|_| "internal CPU grant keeper acknowledgement failed".to_owned())?;
    while cpu_grant_scope_is_populated(cgroup_fd)? {
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn parse_internal_fd(value: OsString, subject: &str) -> Result<i32, String> {
    value
        .to_str()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|fd| *fd > 2)
        .ok_or_else(|| format!("internal CPU grant keeper {subject} descriptor is invalid"))
}

#[cfg(target_os = "linux")]
fn cpu_grant_scope_is_populated(cgroup_fd: i32) -> Result<bool, String> {
    let descriptor = PathBuf::from(format!("/proc/self/fd/{cgroup_fd}"));
    let metadata = match fs::metadata(&descriptor) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("internal CPU grant keeper scope is unavailable".into()),
    };
    let target = fs::read_link(&descriptor)
        .map_err(|_| "internal CPU grant keeper scope cannot be resolved".to_owned())?;
    if target.as_os_str().as_bytes().ends_with(b" (deleted)") {
        return Ok(false);
    }
    if !metadata.file_type().is_dir()
        || !target.starts_with("/sys/fs/cgroup")
        || !target
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("glaeda-hot-run-") && name.ends_with(".scope"))
    {
        return Err("internal CPU grant keeper scope descriptor is unsafe".into());
    }
    let mut raw = String::new();
    match File::open(descriptor.join("cgroup.events")).and_then(|events| {
        events
            .take(MAX_OBSERVATION_BYTES + 1)
            .read_to_string(&mut raw)
    }) {
        Ok(_) => {}
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || cgroup_events_read_means_removed(&error) =>
        {
            return Ok(false);
        }
        Err(_) => return Err("internal CPU grant keeper scope cannot be observed".into()),
    }
    if raw.len() as u64 > MAX_OBSERVATION_BYTES {
        return Err("internal CPU grant keeper scope observation is too large".into());
    }
    let mut populated = raw
        .lines()
        .filter_map(|line| line.strip_prefix("populated "));
    match (populated.next(), populated.next()) {
        (Some("0"), None) => Ok(false),
        (Some("1"), None) => Ok(true),
        _ => Err("internal CPU grant keeper scope population is invalid".into()),
    }
}

fn run(cli: Cli) -> Result<i32, String> {
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        return Err("native hot-run execution requires Linux or macOS".into());
    }
    if cfg!(target_os = "macos")
        && (cli.timeout.is_some() || cli.resource_profile.is_some() || cli.cpu_set.is_some())
    {
        return Err(
            "macOS native hot-run does not support --timeout, --resource-profile or --cpu-set"
                .into(),
        );
    }
    if cli.comparison_key.is_some() && cli.measurement.is_none() {
        return Err("--comparison-key requires --measurement".into());
    }
    if let Some(key) = cli.comparison_key.as_deref() {
        validate_comparison_key(key)?;
    }
    let timeout_seconds = cli.timeout;
    let timeout = timeout_seconds.map(validate_timeout).transpose()?;
    if cli.resource_profile.is_some() && timeout.is_none() {
        return Err("--resource-profile requires --timeout".into());
    }
    if cli.cpu_set.is_some() && cli.resource_profile.is_none() {
        return Err("--cpu-set requires --resource-profile".into());
    }
    let runtime_declaration =
        parse_runtime_contract(cli.runtime_id.as_deref(), cli.runtime_sha256.as_deref())?;
    let runtime_bin = observe_runtime_bin(
        cli.runtime_bin.as_deref(),
        runtime_declaration
            .as_ref()
            .map(|declaration| declaration.id.as_str()),
    )?;
    for name in GIT_OVERRIDE_NAMES {
        if env::var_os(name).is_some() {
            return Err(format!("Git environment override is unsupported: {name}"));
        }
    }

    let caches = cli
        .cache
        .iter()
        .map(|value| parse_native_cache(value))
        .collect::<Result<Vec<_>, _>>()?;
    validate_native_caches(&caches)?;
    let resident = cli
        .resident
        .canonicalize()
        .map_err(|error| format!("resident worktree is unavailable: {error}"))?;
    let task_cwd = cli
        .task
        .canonicalize()
        .map_err(|error| format!("task working directory is unavailable: {error}"))?;
    if !resident.is_dir() || !task_cwd.is_dir() {
        return Err("resident and task must be directories".into());
    }
    let git = resolve_program(OsStr::new("git"), &task_cwd, None)?;
    let task_root = observe_git_root(&git, &task_cwd)?;
    if task_root != resident {
        return Err("resident must be the task's physical Git worktree root".into());
    }
    let bound_path = runtime_bin.as_ref().map(runtime_environment_path);
    let command = resolve_command(&cli.command, &task_cwd, bound_path.as_deref())?;
    let runtime_contract = verify_runtime_contract(
        runtime_declaration.as_ref(),
        Path::new(&command[0]),
        runtime_bin.as_ref(),
    )?;

    #[cfg(target_os = "linux")]
    let cpu_grant = cli.cpu_set.as_deref().map(acquire_cpu_grant).transpose()?;
    #[cfg(target_os = "linux")]
    let cpu_set = cpu_grant.as_ref().map(|grant| &grant.request);
    #[cfg(target_os = "linux")]
    let cpu_locks = cpu_grant
        .as_ref()
        .map(|grant| grant._locks.as_slice())
        .unwrap_or_default();
    #[cfg(not(target_os = "linux"))]
    let cpu_set: Option<&CpuSetRequest> = None;
    #[cfg(not(target_os = "linux"))]
    let cpu_locks: &[()] = &[];
    #[cfg(target_os = "linux")]
    let native_target_before =
        if cli.measurement.is_some() && caches.iter().any(|cache| cache.path == "target") {
            let started = Instant::now();
            let observer = ProjectCheckoutObserver::new(git.clone())
                .map_err(|error| format!("cannot prepare checkout observation: {error}"))?;
            let before = observe_native_target_before(&observer, &resident)?;
            Some((observer, before, started.elapsed()))
        } else {
            None
        };

    let machine_before = cli.measurement.as_ref().map(|_| observe_machine());
    let result = execute_command(CommandExecution {
        command: &command,
        cwd: &task_cwd,
        environment_path: bound_path.as_deref(),
        timeout,
        timeout_seconds,
        resource_profile: cli.resource_profile,
        cpu_set,
        cpu_locks,
        measured: cli.measurement.is_some(),
    })?;
    if let Some(destination) = cli.measurement.as_ref() {
        let machine_after = observe_machine();
        #[cfg(target_os = "linux")]
        let native_target_observation =
            native_target_before.map(|(observer, before, before_elapsed)| {
                let started = Instant::now();
                let after = observe_native_target_after(&observer, &resident);
                NativeTargetMeasurementObservation {
                    before,
                    after,
                    before_elapsed,
                    after_elapsed: started.elapsed(),
                }
            });
        #[cfg(target_os = "linux")]
        let native_target_observation = native_target_observation
            .as_ref()
            .map(|observation| native_target_report(observation, result.elapsed))
            .unwrap_or(Value::Null);
        #[cfg(not(target_os = "linux"))]
        let native_target_observation = Value::Null;
        write_measurement(
            destination,
            &result,
            &caches,
            runtime_contract.as_ref(),
            cli.comparison_key.as_deref(),
            MeasurementObservations {
                machine_before: machine_before.expect("measurement observation exists"),
                machine_after,
                native_target: native_target_observation,
            },
        )?;
    }
    Ok(result.exit_code)
}

#[cfg(target_os = "linux")]
fn observe_native_target_before(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
) -> Result<NativeTargetSnapshot, String> {
    let checkout_observation = observer
        .observe(checkout, &ProcessExecutor)
        .map_err(|error| format!("cannot observe checkout before command: {error}"))?;
    let cargo_target = observe_cargo_target(checkout)
        .map_err(|error| format!("cannot observe Cargo target before command: {error}"))?;
    Ok(NativeTargetSnapshot {
        checkout: checkout_observation,
        cargo_target,
    })
}

#[cfg(target_os = "linux")]
fn observe_native_target_after(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
) -> NativeTargetTerminalSnapshot {
    let checkout_observation = match observer.observe(checkout, &ProcessExecutor) {
        Ok(observation) => TerminalObservation::Observed { observation },
        Err(error) => TerminalObservation::Unavailable { error },
    };
    let cargo_target = match observe_cargo_target(checkout) {
        Ok(observation) => TerminalObservation::Observed { observation },
        Err(error) => TerminalObservation::Unavailable { error },
    };
    NativeTargetTerminalSnapshot {
        checkout: checkout_observation,
        cargo_target,
    }
}

#[cfg(target_os = "linux")]
fn native_target_report(
    observation: &NativeTargetMeasurementObservation,
    command_elapsed: Duration,
) -> Value {
    let observation_elapsed = observation
        .before_elapsed
        .saturating_add(observation.after_elapsed);
    json!({
        "authority": "performance_observation_only",
        "atomic": false,
        "before": observation.before,
        "after": observation.after,
        "before_elapsed_seconds": round_seconds(observation.before_elapsed),
        "after_elapsed_seconds": round_seconds(observation.after_elapsed),
        "observation_elapsed_seconds": round_seconds(observation_elapsed),
        "command_plus_observation_elapsed_seconds": round_seconds(
            command_elapsed.saturating_add(observation_elapsed)
        ),
    })
}

fn validate_timeout(seconds: f64) -> Result<Duration, String> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("timeout must be a positive finite number".into());
    }
    Duration::try_from_secs_f64(seconds)
        .map_err(|_| "timeout exceeds the supported clock range".to_owned())
}

fn validate_comparison_key(value: &str) -> Result<(), String> {
    let Some(digest) = value.strip_prefix(SHA256_PREFIX) else {
        return Err("comparison key must be canonical SHA-256".into());
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("comparison key must be canonical SHA-256".into());
    }
    Ok(())
}

fn parse_runtime_contract(
    runtime_id: Option<&str>,
    program_sha256: Option<&str>,
) -> Result<Option<RuntimeDeclaration>, String> {
    let Some(runtime_id) = runtime_id else {
        if program_sha256.is_some() {
            return Err("runtime executable digest requires a runtime ID".into());
        }
        return Ok(None);
    };
    let valid_id = !runtime_id.is_empty()
        && runtime_id.len() <= 96
        && runtime_id.is_ascii()
        && runtime_id
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && runtime_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !runtime_id.contains("..");
    if !valid_id {
        return Err("runtime ID must be bounded safe ASCII".into());
    }
    if let Some(digest) = program_sha256 {
        validate_sha256(digest)
            .map_err(|_| "runtime executable digest must be canonical SHA-256".to_owned())?;
    }
    Ok(Some(RuntimeDeclaration {
        id: runtime_id.to_owned(),
        expected_program_sha256: program_sha256.map(str::to_owned),
    }))
}

fn validate_sha256(value: &str) -> Result<(), ()> {
    let digest = value.strip_prefix(SHA256_PREFIX).ok_or(())?;
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(())
    }
}

fn observe_runtime_bin(
    runtime_bin: Option<&Path>,
    runtime_id: Option<&str>,
) -> Result<Option<RuntimeBinBinding>, String> {
    let Some(path) = runtime_bin else {
        return Ok(None);
    };
    if runtime_id.is_none() {
        return Err("runtime bin binding requires a runtime ID".into());
    }
    if !path.is_absolute() {
        return Err("runtime bin binding must be an absolute canonical path".into());
    }
    let details =
        fs::symlink_metadata(path).map_err(|_| "runtime bin binding is unavailable".to_owned())?;
    let resolved = path
        .canonicalize()
        .map_err(|_| "runtime bin binding is unavailable".to_owned())?;
    if details.file_type().is_symlink() || !details.is_dir() {
        return Err("runtime bin binding is not a plain directory".into());
    }
    if resolved != path {
        return Err("runtime bin binding contains a symbolic-link component".into());
    }
    if details.uid() != 0 && details.uid() != geteuid().as_raw() {
        return Err("runtime bin binding is not owned by root or the current user".into());
    }
    if details.mode() & 0o022 != 0 {
        return Err("runtime bin binding is writable by an untrusted identity".into());
    }
    Ok(Some(RuntimeBinBinding {
        path: path.to_owned(),
        identity_sha256: runtime_bin_identity(path, &details),
    }))
}

fn runtime_bin_identity(path: &Path, details: &fs::Metadata) -> String {
    let mut digest = Sha256::new();
    digest.update(b"glaeda-hot-run-runtime-bin-v1");
    digest.update(b"\0");
    digest.update(path.as_os_str().as_bytes());
    let mtime_ns = i128::from(details.mtime()) * 1_000_000_000 + i128::from(details.mtime_nsec());
    let ctime_ns = i128::from(details.ctime()) * 1_000_000_000 + i128::from(details.ctime_nsec());
    for value in [
        u128::from(details.dev()),
        u128::from(details.ino()),
        u128::from(details.uid()),
        u128::from(details.gid()),
        u128::from(details.mode() & 0o7777),
        u128::from(details.nlink()),
        u128::from(details.size()),
    ] {
        digest.update(b"\0");
        digest.update(value.to_string().as_bytes());
    }
    for value in [mtime_ns, ctime_ns] {
        digest.update(b"\0");
        digest.update(value.to_string().as_bytes());
    }
    format!("sha256:{:x}", digest.finalize())
}

fn revalidate_runtime_bin(binding: &RuntimeBinBinding) -> Result<(), String> {
    let current = observe_runtime_bin(Some(&binding.path), Some("revalidate"))?
        .expect("runtime bin observation exists");
    if current.identity_sha256 != binding.identity_sha256 {
        return Err("runtime bin binding changed during preflight".into());
    }
    Ok(())
}

fn runtime_environment_path(binding: &RuntimeBinBinding) -> OsString {
    let mut path = binding.path.as_os_str().to_os_string();
    if let Some(inherited) = env::var_os("PATH").filter(|value| !value.is_empty()) {
        path.push(OsStr::new(":"));
        path.push(inherited);
    }
    path
}

fn verify_runtime_contract(
    declaration: Option<&RuntimeDeclaration>,
    program: &Path,
    runtime_bin: Option<&RuntimeBinBinding>,
) -> Result<Option<RuntimeContract>, String> {
    let Some(declaration) = declaration else {
        return Ok(None);
    };
    if let Some(binding) = runtime_bin {
        if program.parent() != Some(binding.path.as_path()) {
            return Err("runtime executable is outside the bound runtime bin".into());
        }
        revalidate_runtime_bin(binding)?;
    }
    let observed = sha256_file(program)?;
    if declaration
        .expected_program_sha256
        .as_ref()
        .is_some_and(|expected| expected != &observed)
    {
        return Err("runtime executable content does not match declared digest".into());
    }
    if let Some(binding) = runtime_bin {
        revalidate_runtime_bin(binding)?;
    }
    Ok(Some(RuntimeContract {
        id: declaration.id.clone(),
        program_sha256: observed,
        runtime_bin_binding_sha256: runtime_bin.map(|binding| binding.identity_sha256.clone()),
    }))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut executable =
        File::open(path).map_err(|error| format!("cannot read runtime executable: {error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = executable
            .read(&mut buffer)
            .map_err(|error| format!("cannot read runtime executable: {error}"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn parse_native_cache(value: &str) -> Result<NativeCache, String> {
    let Some(path) = value.strip_suffix(":native") else {
        return Err("native front door accepts only PATH:native cache declarations".into());
    };
    if path.is_empty() || path.contains('\0') || path.contains("//") {
        return Err("cache path must be a nonempty normalized relative path".into());
    }
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("cache path must be a nonempty normalized relative path".into());
    }
    Ok(NativeCache {
        path: path.to_owned(),
    })
}

fn validate_native_caches(caches: &[NativeCache]) -> Result<(), String> {
    let paths = caches
        .iter()
        .map(|cache| Path::new(&cache.path))
        .collect::<Vec<_>>();
    if paths.iter().copied().collect::<BTreeSet<_>>().len() != paths.len() {
        return Err("cache paths must be unique".into());
    }
    for (index, left) in paths.iter().enumerate() {
        for right in &paths[index + 1..] {
            if left.starts_with(right) || right.starts_with(left) {
                return Err("cache paths must not overlap".into());
            }
        }
    }
    Ok(())
}

fn resolve_command(
    command: &[OsString],
    cwd: &Path,
    search_path: Option<&OsStr>,
) -> Result<Vec<OsString>, String> {
    let requested = command
        .first()
        .ok_or_else(|| "a command is required after --".to_owned())?;
    let program = resolve_program(requested, cwd, search_path)?;
    Ok(std::iter::once(program.into_os_string())
        .chain(command.iter().skip(1).cloned())
        .collect())
}

fn resolve_program(
    requested: &OsStr,
    cwd: &Path,
    search_path: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let requested_path = Path::new(requested);
    let candidate = if requested_path.components().count() > 1 || requested_path.is_absolute() {
        if requested_path.is_absolute() {
            requested_path.to_owned()
        } else {
            cwd.join(requested_path)
        }
    } else {
        let inherited_path;
        let path = match search_path {
            Some(path) => path,
            None => {
                inherited_path = env::var_os("PATH").unwrap_or_default();
                &inherited_path
            }
        };
        env::split_paths(path)
            .map(|directory| directory.join(requested_path))
            .find(|path| is_executable_file(path))
            .ok_or_else(|| format!("command is unavailable: {}", requested.to_string_lossy()))?
    };
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    };
    if !is_executable_file(&absolute) {
        return Err(format!(
            "command is not executable: {}",
            requested.to_string_lossy()
        ));
    }
    Ok(absolute)
}

fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn observe_git_root(git: &Path, task: &Path) -> Result<PathBuf, String> {
    let output = Command::new(git)
        .args([
            OsStr::new("-C"),
            task.as_os_str(),
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--show-toplevel"),
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("cannot inspect task Git worktree: {error}"))?;
    if !output.status.success() || output.stdout.len() > 4096 {
        return Err("task is not a bounded Git worktree".into());
    }
    let raw = std::str::from_utf8(&output.stdout)
        .map_err(|_| "Git worktree root is not UTF-8".to_owned())?
        .trim_end_matches(['\r', '\n']);
    if raw.is_empty() || raw.contains('\n') || raw.contains('\r') {
        return Err("Git returned an invalid worktree root".into());
    }
    Path::new(raw)
        .canonicalize()
        .map_err(|error| format!("Git worktree root is unavailable: {error}"))
}

#[cfg(target_os = "linux")]
struct DeadlineSignalControl {
    interrupted: Arc<AtomicBool>,
    terminated: Arc<AtomicBool>,
    wake_read: UnixStream,
    signal_actions: Vec<signal_hook::SigId>,
}

#[cfg(target_os = "linux")]
impl DeadlineSignalControl {
    fn new() -> Result<Self, String> {
        use signal_hook::consts::signal::{SIGINT, SIGTERM};

        let (wake_read, wake_write) =
            UnixStream::pair().map_err(|_| "deadline signal control is unavailable".to_owned())?;
        wake_read
            .set_nonblocking(true)
            .map_err(|_| "deadline signal control is unavailable".to_owned())?;
        let interrupted = Arc::new(AtomicBool::new(false));
        let terminated = Arc::new(AtomicBool::new(false));
        let sigint_write = wake_write
            .try_clone()
            .map_err(|_| "deadline signal control is unavailable".to_owned())?;
        let mut signal_actions = Vec::with_capacity(4);
        let setup = (|| -> std::io::Result<()> {
            signal_actions.push(signal_hook::flag::register(
                SIGTERM,
                Arc::clone(&terminated),
            )?);
            signal_actions.push(signal_hook::low_level::pipe::register(SIGTERM, wake_write)?);
            signal_actions.push(signal_hook::flag::register(
                SIGINT,
                Arc::clone(&interrupted),
            )?);
            signal_actions.push(signal_hook::low_level::pipe::register(
                SIGINT,
                sigint_write,
            )?);
            Ok(())
        })();
        if setup.is_err() {
            for action in signal_actions.drain(..) {
                let _ = signal_hook::low_level::unregister(action);
            }
            return Err("deadline signal control is unavailable".into());
        }
        Ok(Self {
            interrupted,
            terminated,
            wake_read,
            signal_actions,
        })
    }

    fn pending_signal(&self) -> Option<i32> {
        if self.interrupted.swap(false, Ordering::SeqCst) {
            Some(signal_hook::consts::signal::SIGINT)
        } else if self.terminated.swap(false, Ordering::SeqCst) {
            Some(signal_hook::consts::signal::SIGTERM)
        } else {
            None
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for DeadlineSignalControl {
    fn drop(&mut self) {
        for action in self.signal_actions.drain(..) {
            let _ = signal_hook::low_level::unregister(action);
        }
    }
}

#[cfg(target_os = "linux")]
fn wait_for_deadline_event(
    pidfd: &OwnedFd,
    control: &mut DeadlineSignalControl,
    deadline: Instant,
) -> Result<DeadlineWaitOutcome, String> {
    loop {
        if let Some(signal) = control.pending_signal() {
            return Ok(DeadlineWaitOutcome::Interrupted(signal));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(DeadlineWaitOutcome::Deadline);
        }
        let timeout = Timespec::try_from(deadline.saturating_duration_since(now))
            .map_err(|_| "deadline exceeds the supported clock range".to_owned())?;
        let (process_events, signal_events) = {
            let watched = PollFlags::IN | PollFlags::ERR | PollFlags::HUP;
            let mut descriptors = [
                PollFd::new(pidfd, watched),
                PollFd::new(&control.wake_read, watched),
            ];
            match poll(&mut descriptors, Some(&timeout)) {
                Ok(0) => continue,
                Ok(_) => (descriptors[0].revents(), descriptors[1].revents()),
                Err(Errno::INTR) => continue,
                Err(_) => return Err("deadline process observation failed".into()),
            }
        };
        if let Some(signal) = control.pending_signal() {
            return Ok(DeadlineWaitOutcome::Interrupted(signal));
        }
        if process_events.intersects(PollFlags::IN | PollFlags::HUP) {
            return Ok(DeadlineWaitOutcome::Exited);
        }
        if process_events.intersects(PollFlags::ERR) {
            return Err("deadline process observation failed".into());
        }
        if signal_events.intersects(PollFlags::IN | PollFlags::HUP) {
            let mut wake = [0_u8; 64];
            match control.wake_read.read(&mut wake) {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => return Err("deadline signal observation failed".into()),
            }
        }
        if signal_events.intersects(PollFlags::ERR) {
            return Err("deadline signal observation failed".into());
        }
    }
}

#[cfg(target_os = "linux")]
fn wait_for_resource_scope_deadline_event(
    child: &mut Child,
    scope: &OwnedResourceScope,
    control: &mut DeadlineSignalControl,
    deadline: Instant,
) -> Result<(DeadlineWaitOutcome, Option<ExitStatus>), String> {
    let mut leader_status = None;
    loop {
        if let Some(signal) = control.pending_signal() {
            return Ok((DeadlineWaitOutcome::Interrupted(signal), leader_status));
        }
        if leader_status.is_none() {
            leader_status = child
                .try_wait()
                .map_err(|_| "owned command could not be observed".to_owned())?;
        }
        if leader_status.is_some() && !resource_scope_is_populated(scope)? {
            return Ok((DeadlineWaitOutcome::Exited, leader_status));
        }
        if Instant::now() >= deadline {
            return Ok((DeadlineWaitOutcome::Deadline, leader_status));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn send_process_group_signal(pid: Pid, signal: Signal) -> Result<(), String> {
    match kill_process_group(pid, signal) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(_) => Err("owned command process group could not be terminated".into()),
    }
}

#[cfg(target_os = "linux")]
fn send_owned_signal(pid: Pid, pidfd: &OwnedFd, signal: Signal) -> Result<(), String> {
    let group_missing = match kill_process_group(pid, signal) {
        Ok(()) => false,
        Err(Errno::SRCH) => true,
        Err(_) => return Err("owned command process group could not be terminated".into()),
    };
    let leader_outside_group = match getpgid(Some(pid)) {
        Ok(group) => group != pid,
        Err(Errno::SRCH) => false,
        Err(_) => return Err("owned command leader could not be observed".into()),
    };
    if group_missing || leader_outside_group {
        match pidfd_send_signal(pidfd, signal) {
            Ok(()) | Err(Errno::SRCH) => {}
            Err(_) => return Err("owned command leader could not be terminated".into()),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_group_is_live(pid: Pid) -> Result<bool, String> {
    match test_kill_process_group(pid) {
        Ok(()) => Ok(true),
        Err(Errno::SRCH) => Ok(false),
        Err(_) => Err("owned command process group could not be observed".into()),
    }
}

#[cfg(target_os = "linux")]
fn terminate_process_group_with_grace(
    child: &mut Child,
    pid: Pid,
    pidfd: &OwnedFd,
    initial_signal: Signal,
) -> Result<(ExitStatus, i32), String> {
    let result = (|| {
        send_owned_signal(pid, pidfd, initial_signal)?;
        let grace_deadline = Instant::now()
            .checked_add(TERMINATION_GRACE)
            .ok_or_else(|| "termination grace exceeds the supported clock range".to_owned())?;
        let mut leader_status = None;
        loop {
            if leader_status.is_none() {
                leader_status = child
                    .try_wait()
                    .map_err(|_| "owned command could not be observed".to_owned())?;
            }
            if !process_group_is_live(pid)?
                && let Some(status) = leader_status
            {
                return Ok((status, initial_signal.as_raw()));
            }
            if Instant::now() >= grace_deadline {
                send_owned_signal(pid, pidfd, Signal::KILL)?;
                let status = match leader_status {
                    Some(status) => status,
                    None => child
                        .wait()
                        .map_err(|_| "owned command could not be reaped".to_owned())?,
                };
                return Ok((status, Signal::KILL.as_raw()));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        abort_timed_child(child, pid, Some(pidfd));
    }
    result
}

#[cfg(target_os = "linux")]
fn abort_timed_child(child: &mut Child, pid: Pid, pidfd: Option<&OwnedFd>) {
    let _ = send_process_group_signal(pid, Signal::KILL);
    if let Some(pidfd) = pidfd {
        let _ = pidfd_send_signal(pidfd, Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_os = "linux")]
fn unique_resource_scope_unit() -> Result<String, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_nanos();
    Ok(format!(
        "glaeda-hot-run-{}-{nonce}.scope",
        std::process::id()
    ))
}

#[cfg(target_os = "linux")]
impl ResourceScopeEntryExecutable {
    fn prepare() -> Result<Self, String> {
        let executable = File::open("/proc/self/exe")
            .map_err(|_| "exact resource scope entry executable is unavailable".to_owned())?;
        Ok(Self { executable })
    }

    fn proc_path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.executable.as_raw_fd()
        ))
    }
}

#[cfg(target_os = "linux")]
fn wait_for_resource_scope_entry_stop(
    pid: Pid,
    command_deadline: Option<Instant>,
) -> Result<(), String> {
    let observation_deadline = Instant::now()
        .checked_add(RESOURCE_SCOPE_OBSERVATION_GRACE)
        .ok_or_else(|| {
            "resource scope entry observation exceeds the supported clock range".to_owned()
        })?;
    let deadline = command_deadline
        .map(|deadline| deadline.min(observation_deadline))
        .unwrap_or(observation_deadline);
    loop {
        match waitpid(Some(pid), WaitOptions::NOHANG | WaitOptions::UNTRACED) {
            Ok(Some((_, status))) if status.stopping_signal() == Some(Signal::STOP.as_raw()) => {
                return Ok(());
            }
            Ok(Some(_)) => return Err("resource scope entry exited before admission".into()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => return Err("resource scope entry could not be observed".into()),
            Err(_) => return Err("resource scope entry could not be observed".into()),
        }
    }
}

#[cfg(target_os = "linux")]
fn process_cgroup(pid: Pid) -> Result<Option<PathBuf>, String> {
    let path = PathBuf::from(format!("/proc/{}/cgroup", pid.as_raw_nonzero().get()));
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("resource scope membership could not be observed".into()),
    };
    let mut unified = raw.lines().filter_map(|line| line.strip_prefix("0::"));
    let Some(raw_path) = unified.next() else {
        return Err("unified resource scope membership is unavailable".into());
    };
    if unified.next().is_some() {
        return Err("unified resource scope membership is ambiguous".into());
    }
    let path = PathBuf::from(raw_path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err("resource scope membership is invalid".into());
    }
    Ok(Some(path))
}

#[cfg(target_os = "linux")]
fn open_cgroup_directory(cgroup: &Path) -> Result<OwnedFd, String> {
    let mut result = PathBuf::from("/sys/fs/cgroup");
    for component in cgroup.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => result.push(part),
            _ => return Err("resource scope membership is invalid".into()),
        }
    }
    rustix_open(
        &result,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| "owned resource scope could not be opened".to_owned())
}

#[cfg(target_os = "linux")]
fn observe_owned_resource_scope(
    pid: Pid,
    unit: &str,
    command_deadline: Option<Instant>,
) -> Result<OwnedResourceScope, String> {
    let observation_deadline = Instant::now()
        .checked_add(RESOURCE_SCOPE_OBSERVATION_GRACE)
        .ok_or_else(|| "resource scope observation exceeds the supported clock range".to_owned())?;
    let deadline = command_deadline
        .map(|deadline| deadline.min(observation_deadline))
        .unwrap_or(observation_deadline);
    loop {
        let cgroup = process_cgroup(pid)?
            .ok_or_else(|| "resource scope ownership is unavailable".to_owned())?;
        if cgroup.file_name() == Some(OsStr::new(unit)) {
            let cgroup = open_cgroup_directory(&cgroup)?;
            let kill = rustix_openat(
                &cgroup,
                "cgroup.kill",
                OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|_| "owned resource scope kill control is unavailable".to_owned())?;
            return Ok(OwnedResourceScope { cgroup, kill });
        }
        if Instant::now() >= deadline {
            return Err("resource scope ownership could not be observed".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn cgroup_events_read_means_removed(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(Errno::NODEV.raw_os_error())
}

#[cfg(target_os = "linux")]
fn resource_scope_is_populated(scope: &OwnedResourceScope) -> Result<bool, String> {
    let events = match rustix_openat(
        &scope.cgroup,
        "cgroup.events",
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(events) => events,
        Err(Errno::NOENT) => return Ok(false),
        Err(_) => return Err("owned resource scope could not be observed".into()),
    };
    let events = File::from(events);
    let mut raw = String::new();
    match events
        .take(MAX_OBSERVATION_BYTES + 1)
        .read_to_string(&mut raw)
    {
        Ok(_) => {}
        Err(error) if cgroup_events_read_means_removed(&error) => return Ok(false),
        Err(_) => return Err("owned resource scope could not be observed".into()),
    }
    if raw.len() as u64 > MAX_OBSERVATION_BYTES {
        return Err("owned resource scope observation is too large".into());
    }
    let mut populated = raw
        .lines()
        .filter_map(|line| line.strip_prefix("populated "));
    let Some(value) = populated.next() else {
        return Err("owned resource scope population is unavailable".into());
    };
    if populated.next().is_some() {
        return Err("owned resource scope population is ambiguous".into());
    }
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err("owned resource scope population is invalid".into()),
    }
}

#[cfg(target_os = "linux")]
fn wait_for_resource_scope_empty(
    scope: &OwnedResourceScope,
    timeout: Duration,
) -> Result<bool, String> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "resource scope observation exceeds the supported clock range".to_owned())?;
    loop {
        if !resource_scope_is_populated(scope)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn kill_resource_scope(scope: &OwnedResourceScope) -> Result<(), String> {
    if !resource_scope_is_populated(scope)? {
        return Ok(());
    }
    match rustix_write(&scope.kill, b"1") {
        Ok(1) => Ok(()),
        _ => Err("owned resource scope could not be terminated".into()),
    }
}

#[cfg(target_os = "linux")]
fn terminate_resource_scope_with_grace(
    child: &mut Child,
    pid: Pid,
    pidfd: &OwnedFd,
    scope: &OwnedResourceScope,
    initial_signal: Signal,
) -> Result<(ExitStatus, i32), String> {
    let result = (|| {
        send_owned_signal(pid, pidfd, initial_signal)?;
        let grace_deadline = Instant::now()
            .checked_add(TERMINATION_GRACE)
            .ok_or_else(|| "termination grace exceeds the supported clock range".to_owned())?;
        let mut leader_status = None;
        loop {
            if leader_status.is_none() {
                leader_status = child
                    .try_wait()
                    .map_err(|_| "owned command could not be observed".to_owned())?;
            }
            if !resource_scope_is_populated(scope)?
                && let Some(status) = leader_status
            {
                return Ok((status, initial_signal.as_raw()));
            }
            if Instant::now() >= grace_deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        kill_resource_scope(scope)?;
        let kill_deadline = Instant::now()
            .checked_add(TERMINATION_GRACE)
            .ok_or_else(|| "termination grace exceeds the supported clock range".to_owned())?;
        loop {
            if leader_status.is_none() {
                leader_status = child
                    .try_wait()
                    .map_err(|_| "owned command could not be observed".to_owned())?;
            }
            if !resource_scope_is_populated(scope)?
                && let Some(status) = leader_status
            {
                return Ok((status, Signal::KILL.as_raw()));
            }
            if Instant::now() >= kill_deadline {
                return Err("owned resource scope cleanup is incomplete".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = kill_resource_scope(scope);
        abort_timed_child(child, pid, Some(pidfd));
        let _ = wait_for_resource_scope_empty(scope, TERMINATION_GRACE);
    }
    result
}

#[cfg(target_os = "linux")]
fn settle_finished_resource_scope(scope: &OwnedResourceScope) -> Result<(), String> {
    if wait_for_resource_scope_empty(scope, TERMINATION_GRACE)? {
        return Ok(());
    }
    kill_resource_scope(scope)?;
    if wait_for_resource_scope_empty(scope, TERMINATION_GRACE)? {
        Err("owned resource scope outlived its launcher".into())
    } else {
        Err("owned resource scope cleanup is incomplete".into())
    }
}

#[cfg(target_os = "linux")]
fn abort_resource_scope_execution(
    child: &mut Child,
    pid: Pid,
    pidfd: Option<&OwnedFd>,
    scope: &OwnedResourceScope,
) {
    let _ = kill_resource_scope(scope);
    abort_timed_child(child, pid, pidfd);
    let _ = wait_for_resource_scope_empty(scope, TERMINATION_GRACE);
}

#[cfg(target_os = "linux")]
fn spawn_cpu_grant_keeper(
    executable: &ResourceScopeEntryExecutable,
    scope: &OwnedResourceScope,
    cpu_set: &CpuSetRequest,
    locks: &[OwnedFd],
) -> Result<CpuGrantKeeper, String> {
    if locks.len() != cpu_set.cpus.len() {
        return Err("CPU grant keeper lock set is incomplete".into());
    }
    let descriptors = std::iter::once(&scope.cgroup)
        .chain(locks.iter())
        .collect::<Vec<_>>();
    let original_flags = descriptors
        .iter()
        .map(|fd| fcntl_getfd(fd).map_err(|_| "CPU grant descriptor flags are unavailable"))
        .collect::<Result<Vec<_>, _>>()?;
    for (fd, flags) in descriptors.iter().zip(&original_flags) {
        if fcntl_setfd(fd, *flags & !FdFlags::CLOEXEC).is_err() {
            for (restore_fd, restore_flags) in descriptors.iter().zip(&original_flags) {
                let _ = fcntl_setfd(restore_fd, *restore_flags);
            }
            return Err("CPU grant descriptors cannot be inherited safely".into());
        }
    }
    let mut command = Command::new(executable.proc_path());
    command
        .arg(INTERNAL_CPU_GRANT_KEEPER)
        .arg("--cgroup-fd")
        .arg(scope.cgroup.as_raw_fd().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .process_group(0);
    for (cpu, lock) in cpu_set.cpus.iter().zip(locks) {
        command
            .arg("--cpu-lock-fd")
            .arg(cpu.to_string())
            .arg(lock.as_raw_fd().to_string());
    }
    let child = command.spawn();
    let mut restored = true;
    for (fd, flags) in descriptors.iter().zip(&original_flags) {
        if fcntl_setfd(fd, *flags).is_err() {
            restored = false;
        }
    }
    let mut child =
        child.map_err(|error| format!("CPU grant keeper cannot be launched: {error}"))?;
    if !restored {
        let _ = child.kill();
        let _ = child.wait();
        return Err("CPU grant descriptor inheritance cannot be closed".into());
    }
    let mut acknowledgement = child
        .stdout
        .take()
        .ok_or_else(|| "CPU grant keeper acknowledgement is unavailable".to_owned())?;
    let status_flags = fcntl_getfl(&acknowledgement)
        .map_err(|_| "CPU grant keeper acknowledgement cannot be observed".to_owned())?;
    fcntl_setfl(&acknowledgement, status_flags | OFlags::NONBLOCK)
        .map_err(|_| "CPU grant keeper acknowledgement cannot be observed".to_owned())?;
    let deadline = Instant::now()
        .checked_add(RESOURCE_SCOPE_OBSERVATION_GRACE)
        .ok_or_else(|| "CPU grant keeper acknowledgement exceeds the clock range".to_owned())?;
    let mut byte = [0_u8; 1];
    loop {
        match acknowledgement.read(&mut byte) {
            Ok(1) if byte == *b"1" => return Ok(CpuGrantKeeper { child }),
            Ok(0) => break,
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
        if child.try_wait().ok().flatten().is_some() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("CPU grant keeper did not acknowledge ownership".into())
}

#[cfg(target_os = "linux")]
fn verify_cpu_affinity_sealed(pid: Pid) -> Result<(), String> {
    let status = fs::read_to_string(format!("/proc/{}/status", pid.as_raw_nonzero().get()))
        .map_err(|_| "admitted CPU affinity seal cannot be observed".to_owned())?;
    let seccomp = status
        .lines()
        .filter_map(|line| line.strip_prefix("Seccomp:\t"))
        .collect::<Vec<_>>();
    let no_new_privs = status
        .lines()
        .filter_map(|line| line.strip_prefix("NoNewPrivs:\t"))
        .collect::<Vec<_>>();
    if seccomp.as_slice() != ["2"] || no_new_privs.as_slice() != ["1"] {
        return Err("admitted CPU affinity is not sealed".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn settle_cpu_grant_keeper(keeper: &mut CpuGrantKeeper) -> Result<(), String> {
    let deadline = Instant::now()
        .checked_add(TERMINATION_GRACE)
        .ok_or_else(|| "CPU grant keeper cleanup exceeds the clock range".to_owned())?;
    loop {
        match keeper.child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err("CPU grant keeper exited unsuccessfully".into()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let _ = keeper.child.kill();
                let _ = keeper.child.wait();
                return Err("CPU grant keeper cleanup is incomplete".into());
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn execute_command(execution: CommandExecution<'_>) -> Result<CommandResult, String> {
    let CommandExecution {
        command,
        cwd,
        environment_path,
        timeout,
        timeout_seconds,
        resource_profile,
        cpu_set,
        cpu_locks,
        measured,
    } = execution;
    let mut signal_control = timeout.map(|_| DeadlineSignalControl::new()).transpose()?;
    let systemd_run = resource_profile
        .map(|_| resolve_program(OsStr::new("/usr/bin/systemd-run"), cwd, None))
        .transpose()?;
    let resource_scope_unit = resource_profile
        .map(|_| unique_resource_scope_unit())
        .transpose()?;
    let resource_scope_entry = resource_profile
        .map(|_| ResourceScopeEntryExecutable::prepare())
        .transpose()?;
    let mut time_report = None;
    let mut arguments = if measured {
        let gnu_time = resolve_program(OsStr::new("time"), cwd, None)?;
        let report = unique_temporary_path("glaeda-hot-run-time")?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&report)
            .map_err(|error| format!("cannot create resource observation: {error}"))?;
        let wrapped = [
            vec![
                gnu_time.into_os_string(),
                OsString::from("--quiet"),
                OsString::from("--format"),
                OsString::from("%U\n%S\n%M\n%x"),
                OsString::from("--output"),
                report.clone().into_os_string(),
            ],
            command.to_vec(),
        ]
        .concat();
        time_report = Some(report);
        wrapped
    } else {
        command.to_vec()
    };
    if let Some(scope_entry) = resource_scope_entry.as_ref() {
        let mut entry = vec![
            scope_entry.proc_path().into_os_string(),
            OsString::from(INTERNAL_SCOPE_ENTRY),
        ];
        if let Some(cpu_set) = cpu_set {
            entry.push(OsString::from("--cpu-set"));
            entry.push(OsString::from(&cpu_set.canonical));
        }
        entry.push(OsString::from("--"));
        arguments = [entry, arguments].concat();
    }
    if let Some(systemd_run) = systemd_run {
        let properties = resource_profile
            .expect("profiled command has a resource profile")
            .scope_properties()
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        arguments = [
            vec![
                systemd_run.into_os_string(),
                OsString::from("--user"),
                OsString::from("--scope"),
                OsString::from("--quiet"),
                OsString::from("--collect"),
                OsString::from("--expand-environment=no"),
                OsString::from("--unit"),
                OsString::from(
                    resource_scope_unit
                        .as_deref()
                        .expect("profiled command has a resource scope unit"),
                ),
            ],
            properties
                .iter()
                .flat_map(|value| [OsString::from("--property"), OsString::from(value)])
                .collect(),
            arguments,
        ]
        .concat();
    }

    let mut process = Command::new(&arguments[0]);
    process.args(&arguments[1..]).current_dir(cwd);
    if let Some(path) = environment_path {
        process.env("PATH", path);
    }
    if timeout.is_some() {
        process.process_group(0);
    }
    let started = Instant::now();
    let deadline = match timeout {
        Some(duration) => match started.checked_add(duration) {
            Some(deadline) => Some(deadline),
            None => {
                if let Some(path) = time_report.as_ref() {
                    let _ = fs::remove_file(path);
                }
                return Err("timeout exceeds the supported clock range".into());
            }
        },
        None => None,
    };
    let mut child = process.spawn().map_err(|error| {
        if let Some(path) = time_report.as_ref() {
            let _ = fs::remove_file(path);
        }
        format!("cannot launch command: {error}")
    })?;
    let child_pid = Pid::from_child(&child);
    let resource_scope_pidfd = if resource_profile.is_some() {
        match pidfd_open(child_pid, PidfdFlags::empty()) {
            Ok(pidfd) => Some(pidfd),
            Err(_) => {
                abort_timed_child(&mut child, child_pid, None);
                if let Some(path) = time_report.as_ref() {
                    let _ = fs::remove_file(path);
                }
                return Err("resource scope entry identity is unavailable".into());
            }
        }
    } else {
        None
    };
    if resource_profile.is_some()
        && let Err(error) = wait_for_resource_scope_entry_stop(child_pid, deadline)
    {
        abort_timed_child(&mut child, child_pid, resource_scope_pidfd.as_ref());
        if let Some(path) = time_report.as_ref() {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }
    let resource_scope = match resource_scope_unit.as_deref() {
        Some(unit) => match observe_owned_resource_scope(child_pid, unit, deadline) {
            Ok(scope) => Some(scope),
            Err(error) => {
                abort_timed_child(&mut child, child_pid, resource_scope_pidfd.as_ref());
                if let Some(path) = time_report.as_ref() {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
        },
        None => None,
    };
    let mut cpu_grant_keeper = if let Some(cpu_set) = cpu_set {
        let keeper = spawn_cpu_grant_keeper(
            resource_scope_entry
                .as_ref()
                .expect("CPU-granted command has an exact scope entry executable"),
            resource_scope
                .as_ref()
                .expect("CPU-granted command has an owned resource scope"),
            cpu_set,
            cpu_locks,
        );
        match keeper {
            Ok(keeper) => Some(keeper),
            Err(error) => {
                abort_resource_scope_execution(
                    &mut child,
                    child_pid,
                    resource_scope_pidfd.as_ref(),
                    resource_scope
                        .as_ref()
                        .expect("CPU-granted command has an owned resource scope"),
                );
                if let Some(path) = time_report.as_ref() {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
        }
    } else {
        None
    };
    if let Some(cpu_set) = cpu_set {
        let observed = match sched_getaffinity(Some(child_pid)) {
            Ok(observed) => observed,
            Err(_) => {
                if let Some(scope) = resource_scope.as_ref() {
                    abort_resource_scope_execution(
                        &mut child,
                        child_pid,
                        resource_scope_pidfd.as_ref(),
                        scope,
                    );
                } else {
                    abort_timed_child(&mut child, child_pid, resource_scope_pidfd.as_ref());
                }
                if let Some(path) = time_report.as_ref() {
                    let _ = fs::remove_file(path);
                }
                return Err("admitted CPU affinity cannot be observed".into());
            }
        };
        if observed != cpu_set.mask {
            if let Some(scope) = resource_scope.as_ref() {
                abort_resource_scope_execution(
                    &mut child,
                    child_pid,
                    resource_scope_pidfd.as_ref(),
                    scope,
                );
            } else {
                abort_timed_child(&mut child, child_pid, resource_scope_pidfd.as_ref());
            }
            if let Some(path) = time_report.as_ref() {
                let _ = fs::remove_file(path);
            }
            return Err("admitted CPU affinity does not match the reserved CPU set".into());
        }
        if let Err(error) = verify_cpu_affinity_sealed(child_pid) {
            abort_resource_scope_execution(
                &mut child,
                child_pid,
                resource_scope_pidfd.as_ref(),
                resource_scope
                    .as_ref()
                    .expect("CPU-granted command has an owned resource scope"),
            );
            if let Some(path) = time_report.as_ref() {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
    }
    if let Some(pidfd) = resource_scope_pidfd.as_ref()
        && let Err(error) = pidfd_send_signal(pidfd, Signal::CONT)
    {
        if let Some(scope) = resource_scope.as_ref() {
            abort_resource_scope_execution(&mut child, child_pid, Some(pidfd), scope);
        } else {
            abort_timed_child(&mut child, child_pid, Some(pidfd));
        }
        if let Some(path) = time_report.as_ref() {
            let _ = fs::remove_file(path);
        }
        return Err(format!(
            "resource scope entry could not be admitted: {error}"
        ));
    }
    drop(resource_scope_entry);
    let mut forced_completion = None;
    let status = if let Some(deadline) = deadline {
        let pid = child_pid;
        let pidfd = match resource_scope_pidfd {
            Some(pidfd) => pidfd,
            None => match pidfd_open(pid, PidfdFlags::empty()) {
                Ok(pidfd) => pidfd,
                Err(_) => {
                    match resource_scope.as_ref() {
                        Some(scope) => abort_resource_scope_execution(&mut child, pid, None, scope),
                        None => abort_timed_child(&mut child, pid, None),
                    }
                    if let Some(path) = time_report.as_ref() {
                        let _ = fs::remove_file(path);
                    }
                    return Err("deadline process observation is unavailable".into());
                }
            },
        };
        let outcome = match resource_scope.as_ref() {
            Some(scope) => wait_for_resource_scope_deadline_event(
                &mut child,
                scope,
                signal_control
                    .as_mut()
                    .expect("deadline signal control exists"),
                deadline,
            ),
            None => wait_for_deadline_event(
                &pidfd,
                signal_control
                    .as_mut()
                    .expect("deadline signal control exists"),
                deadline,
            )
            .map(|outcome| (outcome, None)),
        };
        let (outcome, observed_status) = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                match resource_scope.as_ref() {
                    Some(scope) => {
                        abort_resource_scope_execution(&mut child, pid, Some(&pidfd), scope)
                    }
                    None => abort_timed_child(&mut child, pid, Some(&pidfd)),
                }
                if let Some(path) = time_report.as_ref() {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
        };
        match outcome {
            DeadlineWaitOutcome::Exited => match observed_status {
                Some(status) => status,
                None => match child.wait() {
                    Ok(status) => status,
                    Err(_) => {
                        match resource_scope.as_ref() {
                            Some(scope) => {
                                abort_resource_scope_execution(&mut child, pid, Some(&pidfd), scope)
                            }
                            None => abort_timed_child(&mut child, pid, Some(&pidfd)),
                        }
                        if let Some(path) = time_report.as_ref() {
                            let _ = fs::remove_file(path);
                        }
                        return Err("owned command could not be reaped".into());
                    }
                },
            },
            DeadlineWaitOutcome::Interrupted(signal) => {
                let initial = if signal == signal_hook::consts::signal::SIGINT {
                    Signal::INT
                } else {
                    Signal::TERM
                };
                let termination = match resource_scope.as_ref() {
                    Some(scope) => {
                        terminate_resource_scope_with_grace(&mut child, pid, &pidfd, scope, initial)
                    }
                    None => terminate_process_group_with_grace(&mut child, pid, &pidfd, initial),
                };
                let (status, signal_used) = match termination {
                    Ok(result) => result,
                    Err(error) => {
                        if let Some(path) = time_report.as_ref() {
                            let _ = fs::remove_file(path);
                        }
                        return Err(error);
                    }
                };
                forced_completion = Some((128 + signal, signal_used, "operator_interrupt"));
                status
            }
            DeadlineWaitOutcome::Deadline => {
                let termination = match resource_scope.as_ref() {
                    Some(scope) => terminate_resource_scope_with_grace(
                        &mut child,
                        pid,
                        &pidfd,
                        scope,
                        Signal::TERM,
                    ),
                    None => {
                        terminate_process_group_with_grace(&mut child, pid, &pidfd, Signal::TERM)
                    }
                };
                let (status, signal_used) = match termination {
                    Ok(result) => result,
                    Err(error) => {
                        if let Some(path) = time_report.as_ref() {
                            let _ = fs::remove_file(path);
                        }
                        return Err(error);
                    }
                };
                forced_completion = Some((124, signal_used, "deadline_exceeded"));
                status
            }
        }
    } else {
        match child.wait() {
            Ok(status) => status,
            Err(error) => {
                match resource_scope.as_ref() {
                    Some(scope) => {
                        abort_resource_scope_execution(&mut child, child_pid, None, scope)
                    }
                    None => {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
                if let Some(path) = time_report.as_ref() {
                    let _ = fs::remove_file(path);
                }
                return Err(format!("cannot wait for command: {error}"));
            }
        }
    };
    if let Some(scope) = resource_scope.as_ref()
        && let Err(error) = settle_finished_resource_scope(scope)
    {
        if let Some(path) = time_report.as_ref() {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }
    if let Some(keeper) = cpu_grant_keeper.as_mut()
        && let Err(error) = settle_cpu_grant_keeper(keeper)
    {
        if let Some(path) = time_report.as_ref() {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }
    let elapsed = started.elapsed();

    let timed_usage = time_report
        .as_ref()
        .and_then(|path| parse_time_report(path).ok());
    if let Some(path) = time_report.as_ref() {
        let _ = fs::remove_file(path);
    }
    let forced_termination = forced_completion.is_some();
    let (exit_code, signal, completion_reason) =
        if let Some((code, signal, reason)) = forced_completion {
            (code, Some(signal), reason)
        } else {
            let raw_code = status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
            let time_exit = timed_usage.as_ref().map(|usage| usage.3);
            if let Some(signal) = status.signal() {
                (128 + signal, Some(signal), "signaled")
            } else if time_exit == Some(0)
                && (128..=255).contains(&raw_code)
                && valid_signal(raw_code - 128)
            {
                (raw_code, Some(raw_code - 128), "signaled")
            } else {
                (raw_code, None, "exited")
            }
        };
    let (user_cpu_seconds, system_cpu_seconds, max_rss_kib, resource_accounting) =
        if forced_termination {
            (None, None, None, "unavailable_after_forced_termination")
        } else {
            match timed_usage {
                Some((user, system, rss, _)) => {
                    let accounting = if resource_profile.is_some() {
                        "gnu_time_inside_scope"
                    } else {
                        "gnu_time_command_tree"
                    };
                    (Some(user), Some(system), Some(rss), accounting)
                }
                None if measured => (None, None, None, "unavailable_for_measured_command"),
                None => (None, None, None, "not_measured"),
            }
        };
    Ok(CommandResult {
        elapsed,
        timeout_seconds,
        resource_profile: resource_profile.map(ResourceProfile::as_str),
        cpu_set: cpu_set.map(|request| request.canonical.clone()),
        user_cpu_seconds,
        system_cpu_seconds,
        max_rss_kib,
        resource_accounting,
        exit_code,
        signal,
        completion_reason,
    })
}

#[cfg(target_os = "macos")]
fn execute_command(execution: CommandExecution<'_>) -> Result<CommandResult, String> {
    // The same-worktree path deliberately leaves native cache validity to the build tool.
    // It neither materializes a task view nor claims Linux process/resource controls.
    if execution.timeout.is_some()
        || execution.resource_profile.is_some()
        || execution.cpu_set.is_some()
    {
        return Err("macOS native hot-run does not support process/resource controls".into());
    }
    let mut command = Command::new(&execution.command[0]);
    command
        .args(&execution.command[1..])
        .current_dir(execution.cwd);
    if let Some(path) = execution.environment_path {
        command.env("PATH", path);
    }
    let started = Instant::now();
    let status = command
        .status()
        .map_err(|error| format!("cannot execute command: {error}"))?;
    let signal = status.signal();
    Ok(CommandResult {
        elapsed: started.elapsed(),
        timeout_seconds: None,
        resource_profile: None,
        cpu_set: None,
        user_cpu_seconds: None,
        system_cpu_seconds: None,
        max_rss_kib: None,
        resource_accounting: if execution.measured {
            "unavailable_for_measured_command"
        } else {
            "not_measured"
        },
        exit_code: status.code().unwrap_or_else(|| 128 + signal.unwrap_or(0)),
        signal,
        completion_reason: if signal.is_some() {
            "signaled"
        } else {
            "exited"
        },
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn execute_command(_execution: CommandExecution<'_>) -> Result<CommandResult, String> {
    Err("native hot-run execution requires Linux or macOS".into())
}

#[cfg(target_os = "linux")]
fn valid_signal(signal: i32) -> bool {
    (1..=31).contains(&signal) || (34..=64).contains(&signal)
}

#[cfg(target_os = "linux")]
fn parse_time_report(path: &Path) -> Result<(f64, f64, u64, i32), String> {
    let raw = read_bounded(path)?;
    let fields = raw.lines().collect::<Vec<_>>();
    if fields.len() != 4 {
        return Err("resource observation is incomplete".into());
    }
    let user = fields[0]
        .parse::<f64>()
        .map_err(|_| "resource user CPU is invalid".to_owned())?;
    let system = fields[1]
        .parse::<f64>()
        .map_err(|_| "resource system CPU is invalid".to_owned())?;
    let rss = fields[2]
        .parse::<u64>()
        .map_err(|_| "resource peak RSS is invalid".to_owned())?;
    let exit = fields[3]
        .parse::<i32>()
        .map_err(|_| "resource exit status is invalid".to_owned())?;
    if !user.is_finite() || user < 0.0 || !system.is_finite() || system < 0.0 {
        return Err("resource CPU observation is out of range".into());
    }
    Ok((user, system, rss, exit))
}

fn observe_machine() -> Value {
    let started = Instant::now();
    let online = optional_online_cpu_count();
    let allowed = allowed_cpu_count();
    let load = optional_observation(Path::new("/proc/loadavg"), parse_load_average);
    let memory = optional_observation(Path::new("/proc/meminfo"), parse_meminfo);
    let mut pressure = Map::new();
    for kind in PRESSURE_KINDS {
        pressure.insert(
            (*kind).to_owned(),
            optional_observation(&Path::new("/proc/pressure").join(kind), parse_pressure)
                .unwrap_or(Value::Null),
        );
    }
    let present = usize::from(online.is_some())
        + usize::from(allowed.is_some())
        + usize::from(load.is_some())
        + usize::from(memory.is_some())
        + pressure.values().filter(|value| !value.is_null()).count();
    let status = if present == 7 {
        "observed"
    } else if present > 0 {
        "partial"
    } else {
        "unavailable"
    };
    json!({
        "status": status,
        "observation_elapsed_seconds": round_seconds(started.elapsed()),
        "online_logical_cpus": online,
        "allowed_logical_cpus": allowed,
        "load_average": load,
        "memory": memory,
        "pressure": pressure,
    })
}

fn optional_online_cpu_count() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        read_bounded(Path::new("/sys/devices/system/cpu/online"))
            .ok()
            .and_then(|raw| parse_cpu_list(&raw).ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn allowed_cpu_count() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        sched_getaffinity(None)
            .ok()
            .map(|set| u64::from(set.count()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_cpu_list(raw: &str) -> Result<u64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("online CPU observation is empty".into());
    }
    let mut count = 0_u64;
    let mut prior_end = None;
    for span in raw.split(',') {
        let (start, end) = match span.split_once('-') {
            Some((start, end)) => (
                start
                    .parse::<u64>()
                    .map_err(|_| "online CPU observation is invalid".to_owned())?,
                end.parse::<u64>()
                    .map_err(|_| "online CPU observation is invalid".to_owned())?,
            ),
            None => {
                let value = span
                    .parse::<u64>()
                    .map_err(|_| "online CPU observation is invalid".to_owned())?;
                (value, value)
            }
        };
        if end < start || prior_end.is_some_and(|prior| start <= prior) {
            return Err("online CPU observation is overlapping or unordered".into());
        }
        count = count
            .checked_add(end - start + 1)
            .ok_or_else(|| "online CPU observation is out of range".to_owned())?;
        prior_end = Some(end);
    }
    Ok(count)
}

fn optional_observation(path: &Path, parser: fn(&str) -> Result<Value, String>) -> Option<Value> {
    read_bounded(path).ok().and_then(|raw| parser(&raw).ok())
}

fn read_bounded(path: &Path) -> Result<String, String> {
    let mut source = File::open(path).map_err(|error| error.to_string())?;
    let mut raw = String::new();
    std::io::Read::by_ref(&mut source)
        .take(MAX_OBSERVATION_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|error| error.to_string())?;
    if raw.len() as u64 > MAX_OBSERVATION_BYTES {
        return Err("observation exceeds the bounded input size".into());
    }
    if !raw.is_ascii() {
        return Err("observation is not ASCII".into());
    }
    Ok(raw)
}

fn parse_nonnegative_finite(raw: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|_| "value is not numeric".to_owned())?;
    if !value.is_finite() || value < 0.0 {
        return Err("value is not finite and nonnegative".into());
    }
    Ok(value)
}

fn parse_load_average(raw: &str) -> Result<Value, String> {
    let fields = raw.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 3 {
        return Err("load-average observation is incomplete".into());
    }
    Ok(json!({
        "one_minute": parse_nonnegative_finite(fields[0])?,
        "five_minutes": parse_nonnegative_finite(fields[1])?,
        "fifteen_minutes": parse_nonnegative_finite(fields[2])?,
    }))
}

fn parse_meminfo(raw: &str) -> Result<Value, String> {
    let mut values = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for line in raw.lines() {
        let Some((key, remainder)) = line.split_once(':') else {
            return Err("memory observation contains malformed fields".into());
        };
        if !seen.insert(key) {
            return Err("memory observation contains duplicate fields".into());
        }
        let fields = remainder.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 || fields[1] != "kB" {
            continue;
        }
        let kib = fields[0]
            .parse::<u64>()
            .map_err(|_| "memory observation value is out of range".to_owned())?;
        values.insert(
            key,
            kib.checked_mul(1024)
                .ok_or_else(|| "memory observation value is out of range".to_owned())?,
        );
    }
    let available = *values
        .get("MemAvailable")
        .ok_or_else(|| "memory observation is incomplete".to_owned())?;
    let swap_total = *values
        .get("SwapTotal")
        .ok_or_else(|| "memory observation is incomplete".to_owned())?;
    let swap_free = *values
        .get("SwapFree")
        .ok_or_else(|| "memory observation is incomplete".to_owned())?;
    let swap_used = swap_total
        .checked_sub(swap_free)
        .ok_or_else(|| "memory observation has inconsistent swap totals".to_owned())?;
    Ok(json!({
        "available_bytes": available,
        "swap_total_bytes": swap_total,
        "swap_used_bytes": swap_used,
    }))
}

fn parse_pressure(raw: &str) -> Result<Value, String> {
    let mut result = Map::new();
    for line in raw.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 || !matches!(fields[0], "some" | "full") {
            return Err("pressure observation has an unsupported shape".into());
        }
        if result.contains_key(fields[0]) {
            return Err("pressure observation contains duplicate classes".into());
        }
        let mut pairs = BTreeMap::new();
        for field in &fields[1..] {
            let Some((key, value)) = field.split_once('=') else {
                return Err("pressure observation contains malformed fields".into());
            };
            if pairs.insert(key, value).is_some() {
                return Err("pressure observation contains duplicate fields".into());
            }
        }
        if pairs.keys().copied().collect::<BTreeSet<_>>()
            != BTreeSet::from(["avg10", "avg60", "avg300", "total"])
        {
            return Err("pressure observation is incomplete".into());
        }
        let total = pairs["total"]
            .parse::<u64>()
            .map_err(|_| "pressure total is not a nonnegative integer".to_owned())?;
        result.insert(
            fields[0].to_owned(),
            json!({
                "avg10": parse_nonnegative_finite(pairs["avg10"] )?,
                "avg60": parse_nonnegative_finite(pairs["avg60"] )?,
                "avg300": parse_nonnegative_finite(pairs["avg300"] )?,
                "total_microseconds": total,
            }),
        );
    }
    if !result.contains_key("some") {
        return Err("pressure observation does not contain the some class".into());
    }
    Ok(Value::Object(result))
}

fn nested_i128(value: &Value, keys: &[&str]) -> Option<i128> {
    let mut current = value;
    for key in keys {
        current = current.get(*key)?;
    }
    current.as_u64().map(i128::from)
}

fn pressure_interval(before: &Value, after: &Value, elapsed: Duration) -> Value {
    let elapsed_seconds = elapsed.as_secs_f64();
    let mut pressure = Map::new();
    for kind in PRESSURE_KINDS {
        let mut classes = Map::new();
        for class in ["some", "full"] {
            let keys = ["pressure", *kind, class, "total_microseconds"];
            let before_total = nested_i128(before, &keys);
            let after_total = nested_i128(after, &keys);
            let delta = before_total
                .zip(after_total)
                .and_then(|(before, after)| (after >= before).then_some(after - before));
            let fraction = delta.and_then(|value| {
                (elapsed_seconds > 0.0)
                    .then_some(round_to(value as f64 / (elapsed_seconds * 1_000_000.0), 9))
            });
            classes.insert(
                class.to_owned(),
                json!({
                    "total_microseconds_delta": delta,
                    "stall_fraction_of_command_elapsed": fraction,
                }),
            );
        }
        pressure.insert((*kind).to_owned(), Value::Object(classes));
    }
    let available_delta = signed_delta(before, after, &["memory", "available_bytes"]);
    let swap_delta = signed_delta(before, after, &["memory", "swap_used_bytes"]);
    json!({
        "duration_basis": "command_elapsed",
        "elapsed_seconds": round_seconds(elapsed),
        "memory": {
            "available_bytes_delta": available_delta,
            "swap_used_bytes_delta": swap_delta,
        },
        "pressure": pressure,
    })
}

fn signed_delta(before: &Value, after: &Value, keys: &[&str]) -> Option<i128> {
    nested_i128(after, keys)
        .zip(nested_i128(before, keys))
        .map(|(after, before)| after - before)
}

fn write_measurement(
    destination: &Path,
    result: &CommandResult,
    caches: &[NativeCache],
    runtime: Option<&RuntimeContract>,
    comparison_key: Option<&str>,
    observations: MeasurementObservations,
) -> Result<(), String> {
    let destination = absolute_path(destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| "measurement destination has no parent".to_owned())?;
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(parent)
        .map_err(|error| format!("cannot create measurement parent: {error}"))?;
    let temporary = unique_sibling_path(&destination)?;
    let machine_interval = pressure_interval(
        &observations.machine_before,
        &observations.machine_after,
        result.elapsed,
    );
    let report = json!({
        "schema_version": 6,
        "document_type": "glaeda-hot-run-measurement",
        "authority": "developer_observation_only",
        "comparison_key": comparison_key,
        "cross_worktree": false,
        "resource_profile": result.resource_profile,
        "cpu_set": result.cpu_set,
        "machine_observation": {
            "scope": "host_aggregate",
            "before": observations.machine_before,
            "after": observations.machine_after,
            "interval": machine_interval,
        },
        "timeout_seconds": result.timeout_seconds,
        "cache_views": caches.iter().map(|cache| json!({"path": cache.path, "mode": "native"})).collect::<Vec<_>>(),
        "state_preparation": [],
        "source_preparation": Value::Null,
        "native_target_observation": observations.native_target,
        "runtime": runtime.map(runtime_report).unwrap_or(Value::Null),
        "elapsed_seconds": round_seconds(result.elapsed),
        "preparation_elapsed_seconds": 0.0,
        "command_plus_preparation_elapsed_seconds": round_seconds(result.elapsed),
        "user_cpu_seconds": result.user_cpu_seconds.map(|value| round_to(value, 6)),
        "system_cpu_seconds": result.system_cpu_seconds.map(|value| round_to(value, 6)),
        "max_rss_kib": result.max_rss_kib,
        "resource_accounting": result.resource_accounting,
        "exit_code": result.exit_code,
        "signal": result.signal,
        "completion_reason": result.completion_reason,
    });
    let write_result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| format!("cannot create measurement temporary file: {error}"))?;
        serde_json::to_writer(&mut file, &report)
            .map_err(|error| format!("cannot encode measurement: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("cannot finish measurement: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot persist measurement: {error}"))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("cannot publish measurement: {error}"))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn runtime_report(runtime: &RuntimeContract) -> Value {
    let mut report = Map::from_iter([
        ("id".to_owned(), Value::String(runtime.id.clone())),
        (
            "program_sha256".to_owned(),
            Value::String(runtime.program_sha256.clone()),
        ),
    ]);
    if let Some(binding) = runtime.runtime_bin_binding_sha256.as_ref() {
        report.insert(
            "descendant_path".to_owned(),
            Value::String("runtime_bin_first".into()),
        );
        report.insert(
            "runtime_bin_binding_sha256".to_owned(),
            Value::String(binding.clone()),
        );
    }
    Value::Object(report)
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| format!("cannot resolve measurement destination: {error}"))
    }
}

#[cfg(any(target_os = "linux", test))]
fn unique_temporary_path(prefix: &str) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_nanos();
    Ok(env::temp_dir().join(format!(".{prefix}-{}-{nonce}", std::process::id())))
}

fn unique_sibling_path(destination: &Path) -> Result<PathBuf, String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "measurement destination has no parent".to_owned())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_nanos();
    Ok(parent.join(format!(
        ".glaeda-hot-run-measurement-{}-{nonce}",
        std::process::id()
    )))
}

fn round_seconds(duration: Duration) -> f64 {
    round_to(duration.as_secs_f64(), 6)
}

fn round_to(value: f64, places: i32) -> f64 {
    let factor = 10_f64.powi(places);
    (value * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::symlink;
    #[cfg(target_os = "linux")]
    use std::thread;

    #[cfg(target_os = "linux")]
    use rustix::process::test_kill_process;

    fn test_directory(label: &str) -> PathBuf {
        let path = unique_temporary_path(label).unwrap();
        fs::create_dir(&path).unwrap();
        path
    }

    #[cfg(target_os = "linux")]
    fn initialize_test_repository(path: &Path) {
        assert!(
            Command::new("/usr/bin/git")
                .args(["init", "--quiet"])
                .current_dir(path)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("/usr/bin/git")
                .args([
                    "-c",
                    "user.name=Glaeda Test",
                    "-c",
                    "user.email=glaeda-test@example.invalid",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    "fixture",
                ])
                .current_dir(path)
                .status()
                .unwrap()
                .success()
        );
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn native_execution_refuses_unsupported_platform_before_observation() {
        let error = run(Cli {
            resident: PathBuf::from("/nonexistent/resident"),
            task: PathBuf::from("/nonexistent/task"),
            cache: Vec::new(),
            measurement: None,
            comparison_key: None,
            runtime_id: None,
            runtime_sha256: None,
            runtime_bin: None,
            timeout: None,
            resource_profile: None,
            cpu_set: None,
            command: vec![OsString::from("/bin/true")],
        })
        .unwrap_err();
        assert_eq!(error, "native hot-run execution requires Linux or macOS");
    }

    #[test]
    fn comparison_keys_are_canonical() {
        assert!(validate_comparison_key(&format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(validate_comparison_key(&format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(validate_comparison_key("sha256:1234").is_err());
    }

    #[test]
    fn timeout_values_are_positive_and_finite() {
        assert_eq!(validate_timeout(0.125).unwrap(), Duration::from_millis(125));
        for invalid in [0.0, -1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert!(validate_timeout(invalid).is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resource_profiles_require_a_timeout_before_host_observation() {
        for resource_profile in [
            ResourceProfile::BigRedHeavy,
            ResourceProfile::BigRedBackground,
        ] {
            let error = run(Cli {
                resident: PathBuf::from("/path/that/must/not/be/observed"),
                task: PathBuf::from("/path/that/must/not/be/observed"),
                cache: Vec::new(),
                measurement: None,
                comparison_key: None,
                runtime_id: None,
                runtime_sha256: None,
                runtime_bin: None,
                timeout: None,
                resource_profile: Some(resource_profile),
                cpu_set: None,
                command: vec![OsString::from("/bin/true")],
            })
            .unwrap_err();
            assert_eq!(error, "--resource-profile requires --timeout");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_sets_require_a_resource_profile_before_host_observation() {
        let error = run(Cli {
            resident: PathBuf::from("/path/that/must/not/be/observed"),
            task: PathBuf::from("/path/that/must/not/be/observed"),
            cache: Vec::new(),
            measurement: None,
            comparison_key: None,
            runtime_id: None,
            runtime_sha256: None,
            runtime_bin: None,
            timeout: Some(1.0),
            resource_profile: None,
            cpu_set: Some("0".into()),
            command: vec![OsString::from("/bin/true")],
        })
        .unwrap_err();
        assert_eq!(error, "--cpu-set requires --resource-profile");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_set_parser_binds_canonical_effective_cpus() {
        let mut inherited = CpuSet::new();
        for cpu in [0, 1, 2, 4, 7, 8] {
            inherited.set(cpu);
        }
        let parsed = parse_cpu_set("0-2,4,7-8", &inherited).unwrap();
        assert_eq!(parsed.cpus, [0, 1, 2, 4, 7, 8]);
        assert_eq!(parsed.canonical, "0-2,4,7-8");
        assert_eq!(parsed.mask.count(), 6);
        for invalid in ["", "00", "0,0", "0-0", "2-1", "0-1,2", "0,,2"] {
            assert!(
                parse_cpu_set(invalid, &inherited).is_err(),
                "accepted {invalid}"
            );
        }
        assert!(parse_cpu_set("3", &inherited).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn removed_cgroup_events_read_is_terminal_empty() {
        let removed = std::io::Error::from_raw_os_error(Errno::NODEV.raw_os_error());
        let unavailable = std::io::Error::from_raw_os_error(Errno::IO.raw_os_error());
        assert!(cgroup_events_read_means_removed(&removed));
        assert!(!cgroup_events_read_means_removed(&unavailable));
    }

    #[test]
    fn runtime_declarations_are_bounded_and_digest_bound() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let declaration = parse_runtime_contract(Some("node-v26.8.1"), Some(&digest))
            .unwrap()
            .unwrap();
        assert_eq!(declaration.id, "node-v26.8.1");
        assert_eq!(
            declaration.expected_program_sha256.as_deref(),
            Some(digest.as_str())
        );
        assert!(parse_runtime_contract(None, Some(&digest)).is_err());
        for invalid in ["", ".node", "node..current", "node/current"] {
            assert!(parse_runtime_contract(Some(invalid), None).is_err());
        }
        assert!(parse_runtime_contract(Some(&"a".repeat(97)), None).is_err());
    }

    #[test]
    fn native_cache_paths_are_normalized_and_relative() {
        assert_eq!(parse_native_cache("target:native").unwrap().path, "target");
        for value in [
            "target:overlay",
            "/target:native",
            "../target:native",
            "a//b:native",
        ] {
            assert!(parse_native_cache(value).is_err(), "accepted {value}");
        }
        let duplicate = [
            parse_native_cache("target:native").unwrap(),
            parse_native_cache("target:native").unwrap(),
        ];
        assert!(validate_native_caches(&duplicate).is_err());
        let overlap = [
            parse_native_cache("build:native").unwrap(),
            parse_native_cache("build/output:native").unwrap(),
        ];
        assert!(validate_native_caches(&overlap).is_err());
    }

    #[test]
    fn machine_parsers_preserve_missing_and_strict_values() {
        assert_eq!(parse_cpu_list("0-7,16,20-22\n").unwrap(), 12);
        assert!(parse_cpu_list("0-7,7-8").is_err());
        assert_eq!(
            parse_load_average("1.25 2.5 3.75 1/2 3").unwrap(),
            json!({"one_minute": 1.25, "five_minutes": 2.5, "fifteen_minutes": 3.75})
        );
        assert_eq!(
            parse_meminfo("MemAvailable: 1024 kB\nSwapTotal: 512 kB\nSwapFree: 128 kB\n").unwrap(),
            json!({"available_bytes": 1048576, "swap_total_bytes": 524288, "swap_used_bytes": 393216})
        );
        assert!(parse_meminfo("MemAvailable: 1 kB\nSwapTotal: 1 kB\n").is_err());
        assert!(parse_load_average("nan 1 1").is_err());
        let pressure = parse_pressure(
            "some avg10=0.25 avg60=1.50 avg300=2.75 total=12345\nfull avg10=0 avg60=0 avg300=0 total=0\n",
        )
        .unwrap();
        assert_eq!(pressure["some"]["total_microseconds"], 12345);
    }

    #[test]
    fn pressure_interval_rejects_counter_reset() {
        let before = json!({"memory": {"available_bytes": 1000, "swap_used_bytes": 200}, "pressure": {"cpu": {"some": {"total_microseconds": 1000}}}});
        let after = json!({"memory": {"available_bytes": 900, "swap_used_bytes": 230}, "pressure": {"cpu": {"some": {"total_microseconds": 1500}}}});
        let interval = pressure_interval(&before, &after, Duration::from_secs(2));
        assert_eq!(interval["memory"]["available_bytes_delta"], -100);
        assert_eq!(
            interval["pressure"]["cpu"]["some"]["total_microseconds_delta"],
            500
        );
        assert_eq!(
            interval["pressure"]["io"]["some"]["total_microseconds_delta"],
            Value::Null
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_measurement_preserves_exit_and_schema() {
        let fixture = test_directory("glaeda-hot-run-test");
        initialize_test_repository(&fixture);
        fs::create_dir(fixture.join("target")).unwrap();
        fs::write(fixture.join("target/fixture"), b"stable target fixture\n").unwrap();
        let measurement = fixture.join("measurement.json");
        let code = run(Cli {
            resident: fixture.clone(),
            task: fixture.clone(),
            cache: vec!["target:native".into()],
            measurement: Some(measurement.clone()),
            comparison_key: Some(format!("sha256:{}", "a".repeat(64))),
            runtime_id: None,
            runtime_sha256: None,
            runtime_bin: None,
            timeout: Some(3.0),
            resource_profile: None,
            cpu_set: None,
            command: vec![
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("exit 17"),
            ],
        })
        .unwrap();
        assert_eq!(code, 17);
        let report: Value = serde_json::from_reader(File::open(&measurement).unwrap()).unwrap();
        assert_eq!(report["schema_version"], 6);
        assert_eq!(report["document_type"], "glaeda-hot-run-measurement");
        assert_eq!(report["authority"], "developer_observation_only");
        assert_eq!(report["cross_worktree"], false);
        assert_eq!(report["cache_views"][0]["path"], "target");
        let target = &report["native_target_observation"];
        assert_eq!(target["authority"], "performance_observation_only");
        assert_eq!(target["atomic"], false);
        assert_eq!(
            target["before"]["cargo_target"]["state"]["state"],
            "present"
        );
        assert_eq!(target["after"]["checkout"]["state"], "observed");
        assert_eq!(target["after"]["cargo_target"]["state"], "observed");
        assert!(target["before_elapsed_seconds"].as_f64().unwrap() >= 0.0);
        assert!(target["after_elapsed_seconds"].as_f64().unwrap() >= 0.0);
        assert!(
            target["command_plus_observation_elapsed_seconds"]
                .as_f64()
                .unwrap()
                >= report["elapsed_seconds"].as_f64().unwrap()
        );
        assert_eq!(report["resource_accounting"], "gnu_time_command_tree");
        assert_eq!(report["cpu_set"], Value::Null);
        assert_eq!(report["timeout_seconds"], 3.0);
        assert_eq!(report["exit_code"], 17);
        assert_eq!(report["signal"], Value::Null);
        assert_eq!(report["completion_reason"], "exited");
        assert_eq!(
            fs::metadata(&measurement).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(fixture).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn post_command_target_failure_is_receipted_without_erasing_command_result() {
        let fixture = test_directory("glaeda-hot-run-post-observation-test");
        initialize_test_repository(&fixture);
        fs::create_dir(fixture.join("target")).unwrap();
        let measurement = fixture.join("measurement.json");
        let code = run(Cli {
            resident: fixture.clone(),
            task: fixture.clone(),
            cache: vec!["target:native".into()],
            measurement: Some(measurement.clone()),
            comparison_key: Some(format!("sha256:{}", "b".repeat(64))),
            runtime_id: None,
            runtime_sha256: None,
            runtime_bin: None,
            timeout: Some(3.0),
            resource_profile: None,
            cpu_set: None,
            command: vec![
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("/bin/rm -rf -- target && /bin/ln -s -- /tmp target; exit 17"),
            ],
        })
        .unwrap();
        assert_eq!(code, 17);
        let report: Value = serde_json::from_reader(File::open(&measurement).unwrap()).unwrap();
        assert_eq!(report["exit_code"], 17);
        assert_eq!(report["completion_reason"], "exited");
        assert_eq!(
            report["native_target_observation"]["after"]["checkout"]["state"],
            "observed"
        );
        let target = &report["native_target_observation"]["after"]["cargo_target"];
        assert_eq!(target["state"], "unavailable");
        assert_eq!(target["error"]["code"], "cargo_target_unsafe_shape");
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains(fixture.to_str().unwrap()));
        fs::remove_dir_all(fixture).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn measured_signal_is_distinct_from_same_numeric_exit() {
        let fixture = test_directory("glaeda-hot-run-signal-test");
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for (name, shell, reason, expected_signal) in [
            ("exit", "exit 143", "exited", Value::Null),
            ("signal", "kill -TERM $$", "signaled", json!(15)),
        ] {
            let measurement = fixture.join(format!("{name}.json"));
            let code = run(Cli {
                resident: repository.clone(),
                task: repository.clone(),
                cache: Vec::new(),
                measurement: Some(measurement.clone()),
                comparison_key: None,
                runtime_id: None,
                runtime_sha256: None,
                runtime_bin: None,
                timeout: None,
                resource_profile: None,
                cpu_set: None,
                command: vec![
                    OsString::from("/bin/sh"),
                    OsString::from("-c"),
                    OsString::from(shell),
                ],
            })
            .unwrap();
            assert_eq!(code, 143);
            let report: Value = serde_json::from_reader(File::open(&measurement).unwrap()).unwrap();
            assert_eq!(report["completion_reason"], reason);
            assert_eq!(report["signal"], expected_signal);
            assert_eq!(report["native_target_observation"], Value::Null);
            fs::remove_file(measurement).unwrap();
        }
        fs::remove_dir(fixture).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deadline_terminates_the_complete_process_group() {
        let fixture = test_directory("glaeda-hot-run-deadline-test");
        let child_pid_file = fixture.join("child.pid");
        let measurement = fixture.join("measurement.json");
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let shell = format!("sleep 60 & echo $! > {}; wait", child_pid_file.display());
        let started = Instant::now();
        let code = run(Cli {
            resident: repository.clone(),
            task: repository,
            cache: Vec::new(),
            measurement: Some(measurement.clone()),
            comparison_key: None,
            runtime_id: None,
            runtime_sha256: None,
            runtime_bin: None,
            timeout: Some(0.5),
            resource_profile: None,
            cpu_set: None,
            command: vec![
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(shell),
            ],
        })
        .unwrap();
        assert_eq!(code, 124);
        assert!(started.elapsed() < Duration::from_secs(3));

        let report: Value = serde_json::from_reader(File::open(&measurement).unwrap()).unwrap();
        assert_eq!(report["timeout_seconds"], 0.5);
        assert_eq!(report["exit_code"], 124);
        assert_eq!(report["signal"], signal_hook::consts::signal::SIGTERM);
        assert_eq!(report["completion_reason"], "deadline_exceeded");
        assert_eq!(
            report["resource_accounting"],
            "unavailable_after_forced_termination"
        );
        assert_eq!(report["user_cpu_seconds"], Value::Null);
        assert_eq!(report["system_cpu_seconds"], Value::Null);
        assert_eq!(report["max_rss_kib"], Value::Null);

        let child_pid = fs::read_to_string(&child_pid_file)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let child_pid = Pid::from_raw(child_pid).unwrap();
        let absent_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match test_kill_process(child_pid) {
                Err(Errno::SRCH) => break,
                Ok(()) if Instant::now() < absent_deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                result => panic!("deadline descendant remained live: {result:?}"),
            }
        }

        fs::remove_file(measurement).unwrap();
        fs::remove_file(child_pid_file).unwrap();
        fs::remove_dir(fixture).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_bin_binds_program_digest_and_descendant_path() {
        let fixture = test_directory("glaeda-hot-run-runtime-test");
        let runtime_bin = fixture.join("bin");
        fs::create_dir(&runtime_bin).unwrap();
        let program = runtime_bin.join("runtime-tool");
        let descendant = runtime_bin.join("runtime-descendant");
        fs::write(&program, b"#!/bin/sh\nexec runtime-descendant\n").unwrap();
        fs::write(&descendant, b"#!/bin/sh\nexit 17\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&descendant, fs::Permissions::from_mode(0o700)).unwrap();
        let program_digest = sha256_file(&program).unwrap();
        let measurement = fixture.join("measurement.json");
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let wrong_measurement = fixture.join("wrong-measurement.json");
        let error = run(Cli {
            resident: repository.clone(),
            task: repository.clone(),
            cache: Vec::new(),
            measurement: Some(wrong_measurement.clone()),
            comparison_key: None,
            runtime_id: Some("fixture-runtime-v1".into()),
            runtime_sha256: Some(format!("sha256:{}", "0".repeat(64))),
            runtime_bin: Some(runtime_bin.clone()),
            timeout: None,
            resource_profile: None,
            cpu_set: None,
            command: vec![OsString::from("runtime-tool")],
        })
        .unwrap_err();
        assert_eq!(
            error,
            "runtime executable content does not match declared digest"
        );
        assert!(!wrong_measurement.exists());

        let code = run(Cli {
            resident: repository.clone(),
            task: repository.clone(),
            cache: Vec::new(),
            measurement: Some(measurement.clone()),
            comparison_key: None,
            runtime_id: Some("fixture-runtime-v1".into()),
            runtime_sha256: Some(program_digest.clone()),
            runtime_bin: Some(runtime_bin.clone()),
            timeout: None,
            resource_profile: None,
            cpu_set: None,
            command: vec![OsString::from("runtime-tool")],
        })
        .unwrap();
        assert_eq!(code, 17);
        let report_text = fs::read_to_string(&measurement).unwrap();
        let report: Value = serde_json::from_str(&report_text).unwrap();
        assert_eq!(report["runtime"]["id"], "fixture-runtime-v1");
        assert_eq!(report["runtime"]["program_sha256"], program_digest);
        assert_eq!(report["runtime"]["descendant_path"], "runtime_bin_first");
        assert!(
            report["runtime"]["runtime_bin_binding_sha256"]
                .as_str()
                .is_some_and(|value| validate_sha256(value).is_ok())
        );
        assert!(!report_text.contains(fixture.to_str().unwrap()));

        let outside_error = run(Cli {
            resident: repository.clone(),
            task: repository,
            cache: Vec::new(),
            measurement: None,
            comparison_key: None,
            runtime_id: Some("fixture-runtime-v1".into()),
            runtime_sha256: None,
            runtime_bin: Some(runtime_bin.clone()),
            timeout: None,
            resource_profile: None,
            cpu_set: None,
            command: vec![OsString::from("/bin/true")],
        })
        .unwrap_err();
        assert_eq!(
            outside_error,
            "runtime executable is outside the bound runtime bin"
        );

        let alias = fixture.join("bin-alias");
        symlink(&runtime_bin, &alias).unwrap();
        assert!(observe_runtime_bin(Some(&alias), Some("fixture")).is_err());

        fs::remove_file(alias).unwrap();
        fs::remove_file(measurement).unwrap();
        let binding = observe_runtime_bin(Some(&runtime_bin), Some("fixture"))
            .unwrap()
            .unwrap();
        let moved = fixture.join("old-bin");
        fs::rename(&runtime_bin, &moved).unwrap();
        fs::create_dir(&runtime_bin).unwrap();
        assert_eq!(
            revalidate_runtime_bin(&binding).unwrap_err(),
            "runtime bin binding changed during preflight"
        );
        fs::remove_dir(runtime_bin).unwrap();
        fs::remove_file(moved.join("runtime-descendant")).unwrap();
        fs::remove_file(moved.join("runtime-tool")).unwrap();
        fs::remove_dir(moved).unwrap();
        fs::remove_dir(fixture).unwrap();
    }

    #[test]
    fn runtime_bin_refuses_a_group_or_world_writable_directory() {
        let fixture = test_directory("glaeda-hot-run-runtime-mode-test")
            .canonicalize()
            .unwrap();
        let runtime_bin = fixture.join("bin");
        fs::create_dir(&runtime_bin).unwrap();
        for mode in [0o775, 0o757, 0o777] {
            fs::set_permissions(&runtime_bin, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(
                observe_runtime_bin(Some(&runtime_bin), Some("fixture")).unwrap_err(),
                "runtime bin binding is writable by an untrusted identity"
            );
        }

        fs::set_permissions(&runtime_bin, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            observe_runtime_bin(Some(&runtime_bin), Some("fixture"))
                .unwrap()
                .is_some()
        );

        fs::remove_dir(runtime_bin).unwrap();
        fs::remove_dir(fixture).unwrap();
    }
}
