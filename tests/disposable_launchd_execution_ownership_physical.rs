#![cfg(target_os = "macos")]

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use glaeda::process::{CommandSpec, ExecutionRecord, ProcessExecutor, TimedCommandExecutor};
use rustix::fs::{self as rustix_fs, Mode, OFlags};
use rustix::process::{Gid, Pid, getegid, geteuid, test_kill_process};
use sha2::{Digest as _, Sha256};

const OPT_IN_ENV: &str = "SMOLRUNNER_PHYSICAL_LAUNCHD_EXECUTION_ACCEPTANCE";
const OPT_IN_TOKEN: &str = "transient-register-before-start-v1";
const CHILD_ENV: &str = "SMOLRUNNER_PHYSICAL_LAUNCHD_EXECUTION_CHILD";
const ROOT_ENV: &str = "SMOLRUNNER_PHYSICAL_LAUNCHD_EXECUTION_ROOT";
const LABEL_ENV: &str = "SMOLRUNNER_PHYSICAL_LAUNCHD_EXECUTION_LABEL";
const CHILD_MODE: &str = "controller";
const LAUNCHCTL: &str = "/bin/launchctl";
const ENV: &str = "/usr/bin/env";
const SLEEP: &str = "/bin/sleep";
const SYSTEM_RANDOM_SOURCE: &str = "/dev/urandom";
const SLEEP_SECONDS: &str = "120";
const PLIST_NAME: &str = "transient-execution.plist";
const OWNERSHIP_MARKER: &str = "ownership-committed";
const START_AUTHORIZED_MARKER: &str = "start-authorized";
const STARTED_MARKER: &str = "started";
const START_AUTHORIZED_BYTES: &[u8] = b"start-authorized-v1\n";
const STARTED_BYTES: &[u8] = b"started-v1\n";
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(15);
const CHECKPOINT_WAIT: Duration = Duration::from_secs(30);
const START_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

impl DirectoryIdentity {
    fn capture(path: &Path) -> Self {
        let metadata =
            fs::symlink_metadata(path).expect("capture exact launchd proof root identity");
        assert!(
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
            "launchd proof root must be one real directory"
        );
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode() & 0o7777,
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
            && metadata.gid() == self.gid
            && metadata.mode() & 0o7777 == self.mode
    }
}

#[derive(Clone, Copy)]
struct FileIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    links: u64,
    len: u64,
}

impl FileIdentity {
    fn capture(path: &Path, expected: &[u8], uid: u32, gid: u32) -> Self {
        let before =
            fs::symlink_metadata(path).expect("capture exact launchd proof plist identity");
        assert!(
            before.file_type().is_file()
                && !before.file_type().is_symlink()
                && before.uid() == uid
                && before.gid() == gid
                && before.mode() & 0o7777 == 0o600
                && before.nlink() == 1
                && before.len() == u64::try_from(expected.len()).expect("plist length fits u64"),
            "launchd proof plist metadata is not exact"
        );
        assert!(
            fs::read(path).is_ok_and(|bytes| bytes == expected),
            "launchd proof plist bytes drifted"
        );
        let after = fs::symlink_metadata(path).expect("recheck exact launchd proof plist identity");
        assert!(
            same_metadata(&before, &after),
            "launchd proof plist identity changed while observed"
        );
        Self {
            device: before.dev(),
            inode: before.ino(),
            uid: before.uid(),
            gid: before.gid(),
            mode: before.mode() & 0o7777,
            links: before.nlink(),
            len: before.len(),
        }
    }

    fn verify(self, path: &Path, expected: &[u8]) -> bool {
        let Ok(before) = fs::symlink_metadata(path) else {
            return false;
        };
        if !before.file_type().is_file()
            || before.file_type().is_symlink()
            || before.dev() != self.device
            || before.ino() != self.inode
            || before.uid() != self.uid
            || before.gid() != self.gid
            || before.mode() & 0o7777 != self.mode
            || before.nlink() != self.links
            || before.len() != self.len
            || fs::read(path).ok().as_deref() != Some(expected)
        {
            return false;
        }
        fs::symlink_metadata(path).is_ok_and(|after| same_metadata(&before, &after))
    }
}

fn same_metadata(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.len() == right.len()
}

struct ControllerChildGuard {
    child: Option<Child>,
}

impl ControllerChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn is_running(&mut self) -> bool {
        self.child
            .as_mut()
            .expect("controller guard retains child")
            .try_wait()
            .expect("observe exact launchd proof controller")
            .is_none()
    }

    fn kill_and_reap(&mut self) -> ExitStatus {
        let child = self.child.as_mut().expect("controller guard retains child");
        child
            .kill()
            .expect("SIGKILL exact launchd proof controller");
        let status = child.wait().expect("reap exact launchd proof controller");
        self.child = None;
        status
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
            eprintln!("launchd execution proof could not reap its controller child");
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LaunchdObservation {
    Absent,
    Exact { pid: Option<u32> },
}

struct PhysicalLaunchdFixture {
    root: PathBuf,
    plist: PathBuf,
    label: String,
    uid: u32,
    gid: u32,
    plist_bytes: Vec<u8>,
    root_identity: DirectoryIdentity,
    plist_identity: FileIdentity,
    cleanup_authorized: Cell<bool>,
}

impl PhysicalLaunchdFixture {
    fn new() -> Self {
        assert!(
            std::env::consts::ARCH == "aarch64",
            "launchd execution proof requires Apple silicon"
        );
        let uid = geteuid().as_raw();
        let gid = getegid().as_raw();
        assert!(
            uid != 0,
            "launchd execution proof must run as the non-root operator"
        );
        assert!(
            Path::new(LAUNCHCTL).is_file(),
            "launchctl must exist at the reviewed path"
        );
        assert!(
            Path::new(ENV).is_file(),
            "env must exist at the reviewed path"
        );
        assert!(
            Path::new(SLEEP).is_file(),
            "sleep must exist at the reviewed path"
        );

        let identity = fresh_execution_identity();
        let root =
            PathBuf::from("/private/tmp").join(format!("smolrunner-launchd-exec-proof-{identity}"));
        DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("create exact launchd proof root");
        rustix_fs::chown(&root, None, Some(Gid::from_raw(gid)))
            .expect("set exact launchd proof root group");
        let root_identity = DirectoryIdentity::capture(&root);
        assert!(
            root_identity.uid == uid && root_identity.gid == gid && root_identity.mode == 0o700,
            "launchd proof root ownership or mode drifted"
        );

        let label = format!("io.smolrunner.execution-proof.{identity}");
        let plist_bytes = plist_bytes(&label);
        let plist = root.join(PLIST_NAME);
        publish_exact_file(&root, &plist, &plist_bytes);
        let plist_identity = FileIdentity::capture(&plist, &plist_bytes, uid, gid);

        Self {
            root,
            plist,
            label,
            uid,
            gid,
            plist_bytes,
            root_identity,
            plist_identity,
            cleanup_authorized: Cell::new(false),
        }
    }

    fn reopen(root: PathBuf, label: String) -> Self {
        let uid = geteuid().as_raw();
        let gid = getegid().as_raw();
        let root_identity = DirectoryIdentity::capture(&root);
        assert!(
            root_identity.uid == uid && root_identity.gid == gid && root_identity.mode == 0o700,
            "launchd proof root ownership or mode drifted"
        );
        let plist_bytes = plist_bytes(&label);
        let plist = root.join(PLIST_NAME);
        let plist_identity = FileIdentity::capture(&plist, &plist_bytes, uid, gid);
        Self {
            root,
            plist,
            label,
            uid,
            gid,
            plist_bytes,
            root_identity,
            plist_identity,
            cleanup_authorized: Cell::new(false),
        }
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn target(&self) -> String {
        format!("{}/{}", self.domain(), self.label)
    }

    fn ownership_record(&self) -> Vec<u8> {
        format!(
            "schema=1\nexecution_identity={}\nplist_sha256=sha256:{:x}\nlifecycle=prepared\n",
            self.label,
            Sha256::digest(&self.plist_bytes)
        )
        .into_bytes()
    }

    fn verify_namespace(&self) -> Result<(), &'static str> {
        if !self.root_identity.verify(&self.root)
            || !self.plist_identity.verify(&self.plist, &self.plist_bytes)
        {
            return Err("launchd execution proof namespace identity drifted");
        }
        Ok(())
    }

    fn observe(&self) -> Result<LaunchdObservation, &'static str> {
        let target = self.target();
        let record = launchctl(
            CommandSpec::new(LAUNCHCTL)
                .argument("print")
                .argument(&target),
        )?;
        if !record.success {
            return if record.status == Some(113) {
                Ok(LaunchdObservation::Absent)
            } else {
                Err("launchd execution proof observation command failed")
            };
        }

        let path = exact_path(&self.plist)?;
        let expected_header = format!("{target} = {{\n");
        let expected_path = format!("\n\tpath = {path}\n");
        let expected_program = format!("\n\tprogram = {ENV}\n");
        let expected_arguments = format!(
            "\n\targuments = {{\n\t\t{ENV}\n\t\t-i\n\t\t{SLEEP}\n\t\t{SLEEP_SECONDS}\n\t}}\n"
        );
        if !record.stdout.starts_with(&expected_header)
            || !record.stdout.contains(&expected_path)
            || !record.stdout.contains("\n\ttype = LaunchAgent\n")
            || !record.stdout.contains(&expected_program)
            || !record.stdout.contains(&expected_arguments)
        {
            return Err("launchd execution proof observed foreign or malformed service identity");
        }

        let mut pid = None;
        for line in record.stdout.lines() {
            let Some(raw) = line.strip_prefix("\tpid = ") else {
                continue;
            };
            let parsed = raw
                .trim()
                .parse::<u32>()
                .map_err(|_| "launchd execution proof observed malformed service PID")?;
            if pid.replace(parsed).is_some() {
                return Err("launchd execution proof observed duplicate service PID evidence");
            }
        }
        Ok(LaunchdObservation::Exact { pid })
    }

    fn bootstrap(&self) -> Result<(), &'static str> {
        self.verify_namespace()?;
        let path = exact_path(&self.plist)?;
        let record = launchctl(
            CommandSpec::new(LAUNCHCTL)
                .argument("bootstrap")
                .argument(self.domain())
                .secret_argument(path),
        )?;
        if !record.success {
            return Err("launchd execution proof bootstrap command failed");
        }
        self.verify_namespace()
    }

    fn kickstart(&self) -> Result<(), &'static str> {
        self.verify_namespace()?;
        let record = launchctl(
            CommandSpec::new(LAUNCHCTL)
                .argument("kickstart")
                .argument(self.target()),
        )?;
        if !record.success {
            return Err("launchd execution proof kickstart command failed");
        }
        Ok(())
    }

    fn publish_ownership(&self) {
        publish_exact_file(
            &self.root,
            &self.root.join(OWNERSHIP_MARKER),
            &self.ownership_record(),
        );
    }

    fn publish_start_authorized(&self) {
        publish_exact_file(
            &self.root,
            &self.root.join(START_AUTHORIZED_MARKER),
            START_AUTHORIZED_BYTES,
        );
    }

    fn publish_started(&self) {
        publish_exact_file(&self.root, &self.root.join(STARTED_MARKER), STARTED_BYTES);
    }

    fn marker_matches(&self, name: &str, expected: &[u8]) -> bool {
        let path = self.root.join(name);
        let Ok(before) = fs::symlink_metadata(&path) else {
            return false;
        };
        before.file_type().is_file()
            && !before.file_type().is_symlink()
            && before.uid() == self.uid
            && before.gid() == self.gid
            && before.mode() & 0o7777 == 0o600
            && before.nlink() == 1
            && before.len() == u64::try_from(expected.len()).unwrap_or(u64::MAX)
            && fs::read(&path).ok().as_deref() == Some(expected)
            && fs::symlink_metadata(path).is_ok_and(|after| same_metadata(&before, &after))
    }

    fn wait_for_marker(&self, name: &str, expected: &[u8], timeout: Duration) -> bool {
        wait_until(timeout, || self.marker_matches(name, expected))
    }

    fn wait_for_running(&self, timeout: Duration) -> Result<u32, &'static str> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.observe()? {
                LaunchdObservation::Exact { pid: Some(pid) } => return Ok(pid),
                LaunchdObservation::Exact { pid: None } => {}
                LaunchdObservation::Absent => {
                    return Err("launchd execution proof service disappeared before running");
                }
            }
            if Instant::now() >= deadline {
                return Err("launchd execution proof service did not start in time");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_for_absence(&self, timeout: Duration) -> Result<(), &'static str> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.observe()? == LaunchdObservation::Absent {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("launchd execution proof service did not become absent in time");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn authorize_cleanup(&self) -> Result<(), &'static str> {
        self.verify_namespace()?;
        if !self.marker_matches(OWNERSHIP_MARKER, &self.ownership_record())
            || !self.marker_matches(START_AUTHORIZED_MARKER, START_AUTHORIZED_BYTES)
            || !self.marker_matches(STARTED_MARKER, STARTED_BYTES)
            || !matches!(self.observe()?, LaunchdObservation::Exact { .. })
        {
            return Err("launchd execution proof lacks exact cleanup authority");
        }
        self.cleanup_authorized.set(true);
        Ok(())
    }

    fn cleanup(&self, observed_pid: u32) -> Result<(), &'static str> {
        if !self.cleanup_authorized.replace(false) {
            return Err("launchd execution proof cleanup authority was not freshly granted");
        }
        self.verify_namespace()?;
        if !matches!(self.observe()?, LaunchdObservation::Exact { .. }) {
            return Err("launchd execution proof service identity changed before cleanup");
        }
        let record = launchctl(
            CommandSpec::new(LAUNCHCTL)
                .argument("bootout")
                .argument(self.target()),
        )?;
        if !record.success {
            return Err("launchd execution proof bootout command failed");
        }
        self.wait_for_absence(START_WAIT)?;
        wait_for_process_absence(observed_pid, START_WAIT)?;
        self.verify_namespace()?;

        if !self.marker_matches(OWNERSHIP_MARKER, &self.ownership_record())
            || !self.marker_matches(START_AUTHORIZED_MARKER, START_AUTHORIZED_BYTES)
            || !self.marker_matches(STARTED_MARKER, STARTED_BYTES)
        {
            return Err("launchd execution proof markers changed before cleanup");
        }
        let entries = fs::read_dir(&self.root)
            .map_err(|_| "launchd execution proof root could not be enumerated")?
            .map(|entry| {
                entry
                    .map_err(|_| "launchd execution proof root entry was unreadable")?
                    .file_name()
                    .into_string()
                    .map_err(|_| "launchd execution proof root contained a non-UTF-8 entry")
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let expected = [
            PLIST_NAME,
            OWNERSHIP_MARKER,
            START_AUTHORIZED_MARKER,
            STARTED_MARKER,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        if entries != expected {
            return Err("launchd execution proof root contained unexpected state");
        }

        fs::remove_file(self.root.join(STARTED_MARKER))
            .map_err(|_| "launchd execution proof started marker cleanup failed")?;
        fs::remove_file(self.root.join(START_AUTHORIZED_MARKER))
            .map_err(|_| "launchd execution proof authorization marker cleanup failed")?;
        fs::remove_file(self.root.join(OWNERSHIP_MARKER))
            .map_err(|_| "launchd execution proof ownership marker cleanup failed")?;
        fs::remove_file(&self.plist).map_err(|_| "launchd execution proof plist cleanup failed")?;
        fsync_directory(&self.root);
        fs::remove_dir(&self.root).map_err(|_| "launchd execution proof root cleanup failed")?;
        Ok(())
    }
}

impl Drop for PhysicalLaunchdFixture {
    fn drop(&mut self) {
        if self.root.exists() {
            eprintln!(
                "launchd execution proof retained recovery state for service_label={}",
                self.label
            );
        }
    }
}

fn fresh_execution_identity() -> String {
    let mut source = File::open(SYSTEM_RANDOM_SOURCE).expect("open system random source");
    let mut random = [0_u8; 16];
    source
        .read_exact(&mut random)
        .expect("read exact launchd proof identity entropy");
    random.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn process_is_live(pid: u32) -> Result<bool, &'static str> {
    let raw = i32::try_from(pid).map_err(|_| "launchd execution proof PID was out of range")?;
    let pid = Pid::from_raw(raw).ok_or("launchd execution proof PID was invalid")?;
    match test_kill_process(pid) {
        Ok(()) => Ok(true),
        Err(error) if error == rustix::io::Errno::SRCH => Ok(false),
        Err(_) => Err("launchd execution proof process liveness was unavailable"),
    }
}

fn wait_for_process_absence(pid: u32, timeout: Duration) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_is_live(pid)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("launchd execution proof process did not become absent in time");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn plist_bytes(label: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{label}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{ENV}</string>\n    <string>-i</string>\n    <string>{SLEEP}</string>\n    <string>{SLEEP_SECONDS}</string>\n  </array>\n</dict>\n</plist>\n"
    )
    .into_bytes()
}

fn publish_exact_file(root: &Path, path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("publish exact launchd proof file");
    file.write_all(bytes)
        .expect("write exact launchd proof file");
    file.sync_all().expect("fsync exact launchd proof file");
    let metadata = fs::symlink_metadata(path).expect("inspect exact launchd proof file");
    assert!(
        metadata.file_type().is_file()
            && !metadata.file_type().is_symlink()
            && metadata.mode() & 0o7777 == 0o600
            && metadata.nlink() == 1,
        "published launchd proof file identity is unsafe"
    );
    fsync_directory(root);
}

fn fsync_directory(path: &Path) {
    let flags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::CLOEXEC)
        .union(OFlags::NOFOLLOW);
    let directory = rustix_fs::open(path, flags, Mode::empty())
        .expect("open exact launchd proof directory for fsync");
    rustix_fs::fsync(directory).expect("fsync exact launchd proof directory");
}

fn launchctl(spec: CommandSpec) -> Result<ExecutionRecord, &'static str> {
    ProcessExecutor
        .execute_with_timeout(&spec, LAUNCHCTL_TIMEOUT)
        .map_err(|_| "launchd execution proof launchctl command could not execute")
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

fn exact_path(path: &Path) -> Result<String, &'static str> {
    path.to_str()
        .map(str::to_owned)
        .ok_or("launchd execution proof path was not exact UTF-8")
}

#[test]
#[ignore = "child controller for the exact transient launchd execution-ownership proof"]
fn physical_transient_launchd_execution_controller_child() {
    assert_eq!(
        std::env::var(OPT_IN_ENV).as_deref(),
        Ok(OPT_IN_TOKEN),
        "child controller requires the exact launchd physical acceptance token"
    );
    assert_eq!(
        std::env::var(CHILD_ENV).as_deref(),
        Ok(CHILD_MODE),
        "child controller may run only in the exact launchd proof mode"
    );
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("child launchd proof root"));
    let label = std::env::var(LABEL_ENV).expect("child launchd proof label");
    let fixture = PhysicalLaunchdFixture::reopen(root, label);

    let initial = fixture
        .observe()
        .expect("observe initial exact launchd state");
    assert!(
        initial == LaunchdObservation::Absent,
        "transient launchd proof service must begin absent"
    );
    fixture
        .bootstrap()
        .expect("bootstrap exact transient launchd job");
    let bootstrapped = fixture
        .observe()
        .expect("observe exact bootstrapped launchd job");
    assert!(
        matches!(bootstrapped, LaunchdObservation::Exact { pid: None }),
        "bootstrap must register the exact job without starting it"
    );

    fixture.publish_ownership();
    assert!(
        fixture.wait_for_marker(
            START_AUTHORIZED_MARKER,
            START_AUTHORIZED_BYTES,
            CHECKPOINT_WAIT,
        ),
        "parent never durably authorized the exact transient launchd start"
    );
    let gated = fixture
        .observe()
        .expect("re-observe exact launchd job before kickstart");
    assert!(
        matches!(gated, LaunchdObservation::Exact { pid: None }),
        "transient launchd job started before the exact external start gate"
    );

    fixture
        .kickstart()
        .expect("kickstart exact transient launchd job");
    fixture
        .wait_for_running(START_WAIT)
        .expect("observe exact transient launchd job running");
    fixture.publish_started();

    thread::sleep(Duration::from_secs(5 * 60));
    panic!("launchd execution proof controller was not SIGKILLed at the expected checkpoint");
}

#[test]
#[ignore = "bootstraps and removes one transient LaunchAgent and SIGKILLs its controller"]
fn physical_transient_launchd_execution_is_registered_before_start_and_survives_controller_death() {
    assert_eq!(
        std::env::var(OPT_IN_ENV).as_deref(),
        Ok(OPT_IN_TOKEN),
        "set the exact transient launchd execution physical acceptance token"
    );
    let fixture = PhysicalLaunchdFixture::new();
    let initial = fixture
        .observe()
        .expect("observe initial launchd proof state");
    assert!(
        initial == LaunchdObservation::Absent,
        "transient launchd proof service must begin absent"
    );

    let executable = std::env::current_exe().expect("locate exact launchd proof test binary");
    let controller = Command::new(executable)
        .env_clear()
        .args([
            "--ignored",
            "--exact",
            "physical_transient_launchd_execution_controller_child",
            "--nocapture",
        ])
        .env(OPT_IN_ENV, OPT_IN_TOKEN)
        .env(CHILD_ENV, CHILD_MODE)
        .env(ROOT_ENV, &fixture.root)
        .env(LABEL_ENV, &fixture.label)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start exact launchd proof controller child");
    let mut controller = ControllerChildGuard::new(controller);

    let ownership = fixture.ownership_record();
    assert!(
        fixture.wait_for_marker(OWNERSHIP_MARKER, &ownership, CHECKPOINT_WAIT),
        "controller never durably published exact launchd execution ownership"
    );
    assert!(
        controller.is_running(),
        "launchd proof controller exited before start authorization"
    );
    let registered = fixture
        .observe()
        .expect("independently observe registered launchd job before start");
    assert!(
        matches!(registered, LaunchdObservation::Exact { pid: None }),
        "launchd job was not loaded and process-free at the durable ownership boundary"
    );
    eprintln!("launchd execution proof: register_before_start=true");

    fixture.publish_start_authorized();
    assert!(
        fixture.wait_for_marker(STARTED_MARKER, STARTED_BYTES, CHECKPOINT_WAIT),
        "controller never published the exact launchd started checkpoint"
    );
    let running_pid = fixture
        .wait_for_running(START_WAIT)
        .expect("independently observe exact launchd job running");
    assert!(
        controller.is_running(),
        "launchd proof controller exited before SIGKILL"
    );

    let killed = controller.kill_and_reap();
    assert!(killed.signal() == Some(9), "controller must die by SIGKILL");
    let after_kill = fixture
        .observe()
        .expect("re-observe exact launchd job after controller SIGKILL");
    assert!(
        matches!(after_kill, LaunchdObservation::Exact { pid: Some(pid) } if pid == running_pid),
        "launchd-owned job identity did not survive controller SIGKILL exactly"
    );
    fixture
        .verify_namespace()
        .expect("protected launchd execution identity drifted after controller death");
    eprintln!("launchd execution proof: controller_death_observation=true");

    fixture
        .authorize_cleanup()
        .expect("authorize exact launchd proof cleanup");
    fixture
        .cleanup(running_pid)
        .expect("clean exact launchd proof service and namespace");
    assert!(
        !fixture.root.exists(),
        "launchd proof root must be absent after cleanup"
    );
    let final_state = fixture
        .observe()
        .expect("prove final launchd proof absence");
    assert!(
        final_state == LaunchdObservation::Absent,
        "transient launchd proof service must be absent after cleanup"
    );
    eprintln!("launchd execution proof: cleanup=true");
}
