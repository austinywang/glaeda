#![cfg(target_os = "macos")]

use std::cell::Cell;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use glaeda::disposable_template_runtime::DisposableTemplateRuntime;
use glaeda::lima_observation::{LimaInstanceName, SystemLimaObservationClock};
use glaeda::process::{
    CommandExecutor, CommandSpec, ExecutionRecord, ProcessExecutor, TimedCommandExecutor,
};
use rustix::process::{Pid, test_kill_process_group};
use serde_json::Value;

const OPT_IN_ENV: &str = "SMOLRUNNER_PHYSICAL_CONTROLLER_DEATH_ACCEPTANCE";
const OPT_IN_TOKEN: &str = "template-create-sigkill";
const CHILD_ENV: &str = "SMOLRUNNER_PHYSICAL_CONTROLLER_DEATH_CHILD";
const ROOT_ENV: &str = "SMOLRUNNER_PHYSICAL_CONTROLLER_DEATH_ROOT";
const INSTANCE_ENV: &str = "SMOLRUNNER_PHYSICAL_CONTROLLER_DEATH_INSTANCE";
const LIMACTL: &str = "/opt/homebrew/bin/limactl";
const LIMACTL_SAFE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const GENERATION_DOCUMENT: &str = "personal-worker/disposable-template-generation.json";
const STARTED_WAIT: Duration = Duration::from_secs(90);
const PROCESS_WAIT: Duration = Duration::from_secs(30);
const QUIESCENCE_WAIT: Duration = Duration::from_secs(20 * 60);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5 * 60);

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct ControllerChildGuard {
    child: Option<Child>,
}

impl ControllerChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn pid(&self) -> u32 {
        self.child
            .as_ref()
            .expect("controller guard retains its child")
            .id()
    }

    fn is_running(&mut self) -> io::Result<bool> {
        self.child
            .as_mut()
            .expect("controller guard retains its child")
            .try_wait()
            .map(|status| status.is_none())
    }

    fn kill_and_reap(&mut self) -> io::Result<ExitStatus> {
        let child = self
            .child
            .as_mut()
            .expect("controller guard retains its child");
        child.kill()?;
        let status = child.wait()?;
        self.child = None;
        Ok(status)
    }
}

impl Drop for ControllerChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = child.kill();
        if child.wait().is_err() {
            eprintln!("controller-death acceptance could not reap its controller child");
        }
    }
}

#[derive(Clone, Copy)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    mode: u32,
}

impl DirectoryIdentity {
    fn capture(path: &Path) -> Self {
        let metadata = fs::symlink_metadata(path).expect("capture exact directory identity");
        assert!(
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
            "exact controller-death namespace entry must be one directory"
        );
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            mode: metadata.mode() & 0o777,
        }
    }

    fn verify(self, path: &Path) -> bool {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return false;
        };
        metadata.file_type().is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
            && metadata.uid() == self.uid
            && metadata.mode() & 0o777 == self.mode
    }
}

struct ProcessObservation {
    ppid: u32,
    pgid: Pid,
    argv: Vec<String>,
}

struct PhysicalControllerFixture {
    root: PathBuf,
    state_root: PathBuf,
    lima_home: PathBuf,
    instance: LimaInstanceName,
    mutation_pgid: Cell<Option<Pid>>,
    cleanup_authorized: Cell<bool>,
    root_identity: DirectoryIdentity,
    state_root_identity: DirectoryIdentity,
    lima_home_identity: DirectoryIdentity,
}

impl PhysicalControllerFixture {
    fn new() -> Self {
        assert_eq!(
            std::env::consts::ARCH,
            "aarch64",
            "controller-death acceptance requires Apple silicon"
        );
        assert!(
            Path::new(LIMACTL).is_file(),
            "controller-death acceptance requires the reviewed Homebrew limactl path"
        );

        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from("/private/tmp").join(format!(
            "smolrunner-controller-death-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create exact controller-death root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("protect exact controller-death root");
        let state_root = root.join("state");
        let lima_home = root.join("lima");
        fs::create_dir(&state_root).expect("create exact controller-death state root");
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o750))
            .expect("protect exact controller-death state root");
        fs::create_dir(&lima_home).expect("create exact controller-death Lima home");
        fs::set_permissions(&lima_home, fs::Permissions::from_mode(0o700))
            .expect("protect exact controller-death Lima home");

        let root_identity = DirectoryIdentity::capture(&root);
        let state_root_identity = DirectoryIdentity::capture(&state_root);
        let lima_home_identity = DirectoryIdentity::capture(&lima_home);
        assert_eq!(root_identity.mode, 0o700, "exact root mode drifted");
        assert_eq!(
            state_root_identity.mode, 0o750,
            "exact state-root mode drifted"
        );
        assert_eq!(
            lima_home_identity.mode, 0o700,
            "exact Lima-home mode drifted"
        );

        let instance =
            LimaInstanceName::parse(&format!("smolrunner-cd-{}-{sequence}", std::process::id()))
                .expect("build exact controller-death instance name");

        Self {
            root,
            state_root,
            lima_home,
            instance,
            mutation_pgid: Cell::new(None),
            cleanup_authorized: Cell::new(false),
            root_identity,
            state_root_identity,
            lima_home_identity,
        }
    }

    fn runtime(&self) -> DisposableTemplateRuntime {
        DisposableTemplateRuntime::new(
            &self.state_root,
            LIMACTL,
            &self.lima_home,
            self.instance.clone(),
        )
        .expect("construct exact controller-death template runtime")
    }

    fn generation_phase(&self) -> Option<String> {
        let bytes = fs::read(self.state_root.join(GENERATION_DOCUMENT)).ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        value
            .get("phase")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    fn direct_child_pids(controller_pid: u32) -> Vec<u32> {
        let controller_pid = controller_pid.to_string();
        let output = Command::new("/usr/bin/pgrep")
            .env_clear()
            .arg("-P")
            .arg(&controller_pid)
            .output()
            .expect("observe exact controller child processes");
        if output.status.code() == Some(1) {
            return Vec::new();
        }
        assert!(
            output.status.success(),
            "controller child process observation must succeed"
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| {
                line.trim()
                    .parse::<u32>()
                    .expect("controller child PID must be numeric")
            })
            .collect()
    }

    fn process_observation(pid: u32) -> Option<ProcessObservation> {
        let pid_text = pid.to_string();
        let output = Command::new("/bin/ps")
            .env_clear()
            .args(["-p", &pid_text, "-o", "pid=,ppid=,pgid=,command="])
            .output()
            .expect("inspect exact controller child process");
        if output.status.code() == Some(1) {
            return None;
        }
        assert!(
            output.status.success(),
            "exact controller child process inspection must succeed"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines().filter(|line| !line.trim().is_empty());
        let line = lines.next()?;
        assert!(
            lines.next().is_none(),
            "exact PID observation must return at most one process"
        );
        let mut fields = line.split_ascii_whitespace();
        let observed_pid = fields.next()?.parse::<u32>().ok()?;
        let ppid = fields.next()?.parse::<u32>().ok()?;
        let raw_pgid = fields.next()?.parse::<i32>().ok()?;
        if observed_pid != pid {
            return None;
        }
        let pgid = Pid::from_raw(raw_pgid)?;
        let argv = fields.map(str::to_owned).collect();
        Some(ProcessObservation { ppid, pgid, argv })
    }

    fn observe_owned_mutation_pgid(&self, controller_pid: u32) -> Option<Pid> {
        let mut observed = None;
        for child_pid in Self::direct_child_pids(controller_pid) {
            let Some(process) = Self::process_observation(child_pid) else {
                continue;
            };
            if process.ppid != controller_pid
                || process.argv.first().map(String::as_str) != Some(LIMACTL)
                || !process
                    .argv
                    .iter()
                    .any(|argument| argument == self.instance.as_str())
            {
                continue;
            }
            if let Some(existing) = observed {
                assert_eq!(
                    existing, process.pgid,
                    "one controller mutation may own only one fresh process group"
                );
            }
            observed = Some(process.pgid);
        }

        if let Some(pgid) = observed {
            if let Some(existing) = self.mutation_pgid.get() {
                assert_eq!(
                    existing, pgid,
                    "owned mutation process-group identity must remain stable"
                );
            } else {
                self.mutation_pgid.set(Some(pgid));
            }
        }
        observed
    }

    fn process_group_is_live(pgid: Pid) -> io::Result<bool> {
        match test_kill_process_group(pgid) {
            Ok(()) => Ok(true),
            Err(error) if error == rustix::io::Errno::SRCH => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn wait_for_phase(&self, phase: &str, timeout: Duration) -> bool {
        wait_until(timeout, || {
            self.generation_phase().as_deref() == Some(phase)
        })
    }

    fn wait_for_owned_mutation_pgid(&self, controller_pid: u32, timeout: Duration) -> Option<Pid> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pgid) = self.observe_owned_mutation_pgid(controller_pid) {
                return Some(pgid);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_for_quiescence(pgid: Pid, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if !Self::process_group_is_live(pgid)? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn verify_namespace_identity(&self) -> Result<(), &'static str> {
        if !self.root_identity.verify(&self.root)
            || !self.state_root_identity.verify(&self.state_root)
            || !self.lima_home_identity.verify(&self.lima_home)
        {
            return Err("controller-death namespace identity drifted");
        }
        Ok(())
    }

    fn authorize_cleanup(&self, mutation_pgid: Pid) -> Result<(), &'static str> {
        if self.mutation_pgid.get() != Some(mutation_pgid) {
            return Err("owned controller-death process-group identity is unavailable");
        }
        match Self::process_group_is_live(mutation_pgid) {
            Ok(false) => {}
            Ok(true) => return Err("owned controller-death mutation is still running"),
            Err(_) => return Err("owned controller-death process-group state is unavailable"),
        }
        self.verify_namespace_identity()?;
        self.cleanup_authorized.set(true);
        Ok(())
    }

    fn cleanup(&self, mutation_pgid: Pid) -> Result<(), &'static str> {
        if !self.cleanup_authorized.replace(false) {
            return Err("controller-death cleanup lacks fresh reconciliation authority");
        }
        if self.mutation_pgid.get() != Some(mutation_pgid) {
            return Err("owned controller-death process-group identity is unavailable");
        }
        match Self::process_group_is_live(mutation_pgid) {
            Ok(false) => {}
            Ok(true) => return Err("owned controller-death mutation is still running"),
            Err(_) => return Err("owned controller-death process-group state is unavailable"),
        }

        self.verify_namespace_identity()?;
        let instance_directory = self.lima_home.join(self.instance.as_str());
        let instance_present = match fs::symlink_metadata(&instance_directory) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                    return Err("exact controller-death Lima instance identity drifted");
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(_) => return Err("exact controller-death Lima instance state is unavailable"),
        };
        if instance_present {
            self.verify_namespace_identity()?;
            let command = CommandSpec::new(LIMACTL)
                .argument("--tty=false")
                .secret_environment("HOME", exact_path(&self.state_root))
                .secret_environment("LIMA_HOME", exact_path(&self.lima_home))
                .environment("LANG", "C")
                .environment("LC_ALL", "C")
                .environment("PATH", LIMACTL_SAFE_PATH)
                .argument("delete")
                .argument("--force")
                .argument(self.instance.as_str());
            let record = ProcessExecutor
                .execute_with_timeout(&command, CLEANUP_TIMEOUT)
                .map_err(|_| "exact controller-death Lima cleanup command failed")?;
            if !record.success {
                return Err("exact controller-death Lima cleanup command was unsuccessful");
            }
            match fs::symlink_metadata(&instance_directory) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => return Err("exact controller-death Lima cleanup was not proven"),
            }
        }

        self.verify_namespace_identity()?;
        fs::remove_dir_all(&self.root).map_err(|_| "controller-death root cleanup failed")?;
        Ok(())
    }
}

impl Drop for PhysicalControllerFixture {
    fn drop(&mut self) {
        if !self.root.exists() {
            return;
        }
        if !self.cleanup_authorized.get() {
            eprintln!(
                "controller-death acceptance retained recovery state without cleanup authority"
            );
            return;
        }
        let Some(mutation_pgid) = self.mutation_pgid.get() else {
            eprintln!(
                "controller-death acceptance retained recovery state without process-group identity"
            );
            return;
        };
        match Self::process_group_is_live(mutation_pgid) {
            Ok(false) => {
                if self.cleanup(mutation_pgid).is_err() {
                    eprintln!("controller-death acceptance retained exact recovery state");
                }
            }
            Ok(true) => {
                eprintln!("controller-death acceptance retained a live exact recovery namespace");
            }
            Err(_) => {
                eprintln!(
                    "controller-death acceptance retained recovery state with unknown process-group status"
                );
            }
        }
    }
}

struct RestartProbeExecutor {
    mutation_pgid: Pid,
    mutation_attempts: Cell<u32>,
    conflicting_mutation_attempts: Cell<u32>,
}

impl RestartProbeExecutor {
    fn new(mutation_pgid: Pid) -> Self {
        Self {
            mutation_pgid,
            mutation_attempts: Cell::new(0),
            conflicting_mutation_attempts: Cell::new(0),
        }
    }

    fn mutation_attempts(&self) -> u32 {
        self.mutation_attempts.get()
    }

    fn conflicting_mutation_attempts(&self) -> u32 {
        self.conflicting_mutation_attempts.get()
    }

    fn refuse_mutation(&self, spec: &CommandSpec) -> io::Result<()> {
        let argv = spec.displayed_argv();
        if argv
            .iter()
            .any(|value| matches!(value.as_str(), "start" | "stop" | "delete"))
        {
            self.mutation_attempts
                .set(self.mutation_attempts.get().saturating_add(1));
            match PhysicalControllerFixture::process_group_is_live(self.mutation_pgid) {
                Ok(false) => {}
                Ok(true) | Err(_) => self
                    .conflicting_mutation_attempts
                    .set(self.conflicting_mutation_attempts.get().saturating_add(1)),
            }
            return Err(io::Error::other(
                "controller-death proof refused a second external mutation",
            ));
        }
        Ok(())
    }
}

impl CommandExecutor for RestartProbeExecutor {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.refuse_mutation(spec)?;
        ProcessExecutor.execute(spec)
    }
}

impl TimedCommandExecutor for RestartProbeExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        self.refuse_mutation(spec)?;
        ProcessExecutor.execute_with_timeout(spec, timeout)
    }
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn exact_path(path: &Path) -> String {
    path.to_str()
        .expect("controller-death test paths remain exact UTF-8")
        .to_owned()
}

#[test]
fn controller_child_guard_kills_and_reaps_on_drop() {
    let child = Command::new("/bin/sleep")
        .env_clear()
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start disposable guard fixture");
    let pid = child.id();
    {
        let _guard = ControllerChildGuard::new(child);
    }
    let still_exists = Command::new("/bin/kill")
        .env_clear()
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("inspect disposable guard fixture");
    assert!(
        !still_exists.success(),
        "dropping the armed guard must kill and reap its controller"
    );
}

#[test]
#[ignore = "child controller for the exact physical template-create SIGKILL proof"]
fn physical_controller_child_template_create() {
    assert_eq!(
        std::env::var(OPT_IN_ENV).as_deref(),
        Ok(OPT_IN_TOKEN),
        "child controller requires the exact physical acceptance token"
    );
    assert_eq!(
        std::env::var(CHILD_ENV).as_deref(),
        Ok("template-create"),
        "child controller may run only in template-create mode"
    );
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("child controller root"));
    let state_root = root.join("state");
    let lima_home = root.join("lima");
    let instance = LimaInstanceName::parse(
        &std::env::var(INSTANCE_ENV).expect("child controller instance identity"),
    )
    .expect("child controller instance name");
    let runtime = DisposableTemplateRuntime::new(&state_root, LIMACTL, &lima_home, instance)
        .expect("construct child controller template runtime");
    let clock = SystemLimaObservationClock;

    runtime
        .reconcile_once(&ProcessExecutor, &clock)
        .expect("authorize physical source creation from exact absence");
    runtime
        .reconcile_once(&ProcessExecutor, &clock)
        .expect("physical create completes only when the parent does not kill this controller");
}

#[test]
#[ignore = "SIGKILLs one controller during an isolated physical Lima/VZ template create"]
fn physical_controller_death_during_template_create_is_observed_and_fenced() {
    assert_eq!(
        std::env::var(OPT_IN_ENV).as_deref(),
        Ok(OPT_IN_TOKEN),
        "set the exact controller-death physical acceptance token"
    );
    let fixture = PhysicalControllerFixture::new();
    let executable = std::env::current_exe().expect("locate exact controller-death test binary");
    let controller = Command::new(executable)
        .env_clear()
        .args([
            "--ignored",
            "--exact",
            "physical_controller_child_template_create",
            "--nocapture",
        ])
        .env(OPT_IN_ENV, OPT_IN_TOKEN)
        .env(CHILD_ENV, "template-create")
        .env(ROOT_ENV, &fixture.root)
        .env(INSTANCE_ENV, fixture.instance.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start exact child controller");
    let mut controller = ControllerChildGuard::new(controller);

    assert!(
        fixture.wait_for_phase("create_started", STARTED_WAIT),
        "child controller never published the exact create_started checkpoint"
    );
    let mutation_pgid = fixture
        .wait_for_owned_mutation_pgid(controller.pid(), PROCESS_WAIT)
        .expect(
            "create_started was published but no exact owned limactl process group became observable",
        );
    assert!(
        controller.is_running().expect("inspect child controller"),
        "child controller exited before the SIGKILL checkpoint"
    );

    let runtime = fixture.runtime();
    let clock = SystemLimaObservationClock;
    let probe = RestartProbeExecutor::new(mutation_pgid);

    let killed = controller
        .kill_and_reap()
        .expect("SIGKILL and reap exact child controller");
    assert_eq!(killed.signal(), Some(9), "controller must die by SIGKILL");
    assert_eq!(
        fixture.generation_phase().as_deref(),
        Some("create_started"),
        "controller death must retain the exact durable Started checkpoint"
    );

    let child_survived_controller_death =
        PhysicalControllerFixture::process_group_is_live(mutation_pgid)
            .expect("observe exact owned mutation process group after controller death");
    eprintln!(
        "controller-death proof: owned mutation process group survived SIGKILL={child_survived_controller_death}"
    );

    let _ = runtime.reconcile_once(&probe, &clock);
    for _ in 0..3 {
        if !PhysicalControllerFixture::process_group_is_live(mutation_pgid)
            .expect("observe exact owned mutation process group before restart probe")
        {
            break;
        }
        thread::sleep(Duration::from_millis(250));
        let _ = runtime.reconcile_once(&probe, &clock);
    }
    let conflicting_restart_mutation = probe.conflicting_mutation_attempts() != 0;

    assert!(
        PhysicalControllerFixture::wait_for_quiescence(mutation_pgid, QUIESCENCE_WAIT)
            .expect("observe exact owned mutation process group quiescence"),
        "the exact owned Lima mutation process group did not quiesce within the bounded physical proof window"
    );

    let post_probe = RestartProbeExecutor::new(mutation_pgid);
    let post_quiescence = runtime.reconcile_once(&post_probe, &clock);
    eprintln!("controller-death proof: post-quiescence restart outcome={post_quiescence:?}");
    assert_eq!(
        post_probe.mutation_attempts(),
        0,
        "post-quiescence reconciliation attempted an external mutation before cleanup"
    );
    let recovery_gap = post_quiescence
        .expect_err("current recovery must not claim a partial interrupted create is observable");
    assert_eq!(
        recovery_gap.code(),
        "template_host_identity_unavailable",
        "controller-death recovery must retain the exact current fail-closed classification"
    );
    fixture.authorize_cleanup(mutation_pgid).expect(
        "authorize fixture cleanup from process-group absence and exact namespace identity",
    );
    fixture
        .cleanup(mutation_pgid)
        .expect("remove the exact quiescent controller-death namespace");
    assert!(
        !fixture.root.exists(),
        "physical proof must leave no test root"
    );
    assert!(
        !conflicting_restart_mutation,
        "a restarted controller reached a second external mutation while the prior owned command was still live"
    );
}
