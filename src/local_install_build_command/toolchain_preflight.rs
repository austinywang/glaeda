use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read as _;
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};
use std::time::Duration;

use rustix::fs::{self, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::geteuid;
use serde::Serialize;

use crate::local_install_plan::LocalInstallToolchainIdentity;
use crate::process::{CommandSpec, TimedCommandExecutor};

use super::LocalInstallBuildCommandContext;

pub const LOCAL_INSTALL_TOOLCHAIN_PREFLIGHT_SCHEMA_VERSION: u8 = 1;
pub const LOCAL_INSTALL_TOOLCHAIN_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_LOCAL_INSTALL_TOOLCHAIN_VERSION_OUTPUT_BYTES: usize = 256;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallToolchainExecutableDisposition {
    Exact,
    Mismatch,
    Unsafe,
    Unknown,
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallToolchainBlockingCode {
    ExpectedIdentityUnsupported,
    CargoMismatch,
    CargoUnsafe,
    CargoUnknown,
    CargoChanged,
    RustcMismatch,
    RustcUnsafe,
    RustcUnknown,
    RustcChanged,
    RustdocMismatch,
    RustdocUnsafe,
    RustdocUnknown,
    RustdocChanged,
}

/// Path-private proof that the three exact self-build executables belong to one expected Rust
/// toolchain family.
///
/// Fields are private so successful observation remains immutable after construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallToolchainPreflightReceipt {
    schema_version: u8,
    expected_toolchain: LocalInstallToolchainIdentity,
    cargo: LocalInstallToolchainExecutableDisposition,
    rustc: LocalInstallToolchainExecutableDisposition,
    rustdoc: LocalInstallToolchainExecutableDisposition,
    ready: bool,
    blocking_codes: Vec<LocalInstallToolchainBlockingCode>,
}

impl LocalInstallToolchainPreflightReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn expected_toolchain(&self) -> &LocalInstallToolchainIdentity {
        &self.expected_toolchain
    }

    #[must_use]
    pub const fn cargo(&self) -> LocalInstallToolchainExecutableDisposition {
        self.cargo
    }

    #[must_use]
    pub const fn rustc(&self) -> LocalInstallToolchainExecutableDisposition {
        self.rustc
    }

    #[must_use]
    pub const fn rustdoc(&self) -> LocalInstallToolchainExecutableDisposition {
        self.rustdoc
    }

    #[must_use]
    pub const fn ready(&self) -> bool {
        self.ready
    }

    #[must_use]
    pub fn blocking_codes(&self) -> &[LocalInstallToolchainBlockingCode] {
        &self.blocking_codes
    }
}

/// Observe the exact Cargo, rustc, and rustdoc objects retained privately by the build command
/// context and prove their fixed `--version` outputs against one expected toolchain identity.
///
/// Every executable path is snapshotted before and after probing. Every directory in the no-follow
/// chain must be root- or runner-owned and not group/world writable, except for sticky directories
/// whose root/runner-owned children cannot be replaced by another user. The private snapshot also
/// retains change-time evidence, so entry replacement and restore (ABA) during the probe changes
/// the proof. Raw paths, filesystem identities, and command output never enter the public receipt.
#[must_use]
pub fn observe_local_install_toolchain_preflight(
    expected: &LocalInstallToolchainIdentity,
    context: &LocalInstallBuildCommandContext,
    executor: &impl TimedCommandExecutor,
) -> LocalInstallToolchainPreflightReceipt {
    let Some(expected_version) = expected_version(expected.as_str()) else {
        return unsupported_expected_identity(expected);
    };

    let cargo = observe_tool(
        ToolKind::Cargo,
        &context.cargo_program,
        expected_version,
        executor,
    );
    let rustc = observe_tool(
        ToolKind::Rustc,
        &context.rustc_program,
        expected_version,
        executor,
    );
    let rustdoc = observe_tool(
        ToolKind::Rustdoc,
        &context.rustdoc_program,
        expected_version,
        executor,
    );

    let mut blocking_codes = BTreeSet::new();
    add_blocker(&mut blocking_codes, ToolKind::Cargo, cargo);
    add_blocker(&mut blocking_codes, ToolKind::Rustc, rustc);
    add_blocker(&mut blocking_codes, ToolKind::Rustdoc, rustdoc);
    let blocking_codes = blocking_codes.into_iter().collect::<Vec<_>>();

    LocalInstallToolchainPreflightReceipt {
        schema_version: LOCAL_INSTALL_TOOLCHAIN_PREFLIGHT_SCHEMA_VERSION,
        expected_toolchain: expected.clone(),
        cargo,
        rustc,
        rustdoc,
        ready: blocking_codes.is_empty(),
        blocking_codes,
    }
}

fn unsupported_expected_identity(
    expected: &LocalInstallToolchainIdentity,
) -> LocalInstallToolchainPreflightReceipt {
    LocalInstallToolchainPreflightReceipt {
        schema_version: LOCAL_INSTALL_TOOLCHAIN_PREFLIGHT_SCHEMA_VERSION,
        expected_toolchain: expected.clone(),
        cargo: LocalInstallToolchainExecutableDisposition::Unknown,
        rustc: LocalInstallToolchainExecutableDisposition::Unknown,
        rustdoc: LocalInstallToolchainExecutableDisposition::Unknown,
        ready: false,
        blocking_codes: vec![LocalInstallToolchainBlockingCode::ExpectedIdentityUnsupported],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Cargo,
    Rustc,
    Rustdoc,
}

impl ToolKind {
    const fn command_name(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Rustc => "rustc",
            Self::Rustdoc => "rustdoc",
        }
    }
}

fn observe_tool(
    kind: ToolKind,
    path: &Path,
    expected_version: &str,
    executor: &impl TimedCommandExecutor,
) -> LocalInstallToolchainExecutableDisposition {
    let before = match executable_snapshot(path) {
        Ok(snapshot) => snapshot,
        Err(ExecutableObservationError::Unsafe) => {
            return LocalInstallToolchainExecutableDisposition::Unsafe;
        }
        Err(ExecutableObservationError::Unknown) => {
            return LocalInstallToolchainExecutableDisposition::Unknown;
        }
    };

    let probe = probe_version(kind, path, executor);
    let after = match executable_snapshot(path) {
        Ok(snapshot) => snapshot,
        Err(_) => return LocalInstallToolchainExecutableDisposition::Changed,
    };
    if !before.same_as(&after) {
        return LocalInstallToolchainExecutableDisposition::Changed;
    }

    match probe {
        VersionProbe::Version(version) if version == expected_version => {
            LocalInstallToolchainExecutableDisposition::Exact
        }
        VersionProbe::Version(_) => LocalInstallToolchainExecutableDisposition::Mismatch,
        VersionProbe::Unknown => LocalInstallToolchainExecutableDisposition::Unknown,
    }
}

fn add_blocker(
    blockers: &mut BTreeSet<LocalInstallToolchainBlockingCode>,
    kind: ToolKind,
    disposition: LocalInstallToolchainExecutableDisposition,
) {
    use LocalInstallToolchainBlockingCode as Blocker;
    use LocalInstallToolchainExecutableDisposition as Disposition;

    let blocker = match (kind, disposition) {
        (_, Disposition::Exact) => return,
        (ToolKind::Cargo, Disposition::Mismatch) => Blocker::CargoMismatch,
        (ToolKind::Cargo, Disposition::Unsafe) => Blocker::CargoUnsafe,
        (ToolKind::Cargo, Disposition::Unknown) => Blocker::CargoUnknown,
        (ToolKind::Cargo, Disposition::Changed) => Blocker::CargoChanged,
        (ToolKind::Rustc, Disposition::Mismatch) => Blocker::RustcMismatch,
        (ToolKind::Rustc, Disposition::Unsafe) => Blocker::RustcUnsafe,
        (ToolKind::Rustc, Disposition::Unknown) => Blocker::RustcUnknown,
        (ToolKind::Rustc, Disposition::Changed) => Blocker::RustcChanged,
        (ToolKind::Rustdoc, Disposition::Mismatch) => Blocker::RustdocMismatch,
        (ToolKind::Rustdoc, Disposition::Unsafe) => Blocker::RustdocUnsafe,
        (ToolKind::Rustdoc, Disposition::Unknown) => Blocker::RustdocUnknown,
        (ToolKind::Rustdoc, Disposition::Changed) => Blocker::RustdocChanged,
    };
    blockers.insert(blocker);
}

#[derive(Debug, Clone)]
struct ExecutableSnapshot {
    directories: Vec<PrivateMetadata>,
    file: PrivateMetadata,
}

impl ExecutableSnapshot {
    fn same_as(&self, other: &Self) -> bool {
        self.directories.len() == other.directories.len()
            && self
                .directories
                .iter()
                .enumerate()
                .zip(&other.directories)
                .all(|((index, left), right)| {
                    if index + 1 == self.directories.len() {
                        left.same_as(right)
                    } else {
                        left.same_path_component_as(right)
                    }
                })
            && self.file.same_as(&other.file)
    }
}

#[derive(Debug, Clone)]
struct PrivateMetadata {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl PrivateMetadata {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.uid == other.uid
            && self.gid == other.gid
            && self.mode == other.mode
            && self.links == other.links
            && self.size == other.size
            && self.modified_seconds == other.modified_seconds
            && self.modified_nanoseconds == other.modified_nanoseconds
            && self.changed_seconds == other.changed_seconds
            && self.changed_nanoseconds == other.changed_nanoseconds
    }

    /// Higher path components retain exact entry identity without treating unrelated sibling churn
    /// as tool replacement. The direct executable parent is compared separately with the complete
    /// metadata record.
    fn same_path_component_as(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.uid == other.uid
            && self.gid == other.gid
            && self.mode == other.mode
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutableObservationError {
    Unsafe,
    Unknown,
}

fn executable_snapshot(path: &Path) -> Result<ExecutableSnapshot, ExecutableObservationError> {
    if !valid_absolute_path(path) {
        return Err(ExecutableObservationError::Unsafe);
    }
    let components = normal_components(path);
    let Some((file_name, parent_components)) = components.split_last() else {
        return Err(ExecutableObservationError::Unsafe);
    };

    let root = fs::open("/", DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| ExecutableObservationError::Unknown)?;
    let mut current = File::from(root);
    let root_metadata = metadata(&current)?;
    if !valid_directory_metadata(&root_metadata) {
        return Err(ExecutableObservationError::Unsafe);
    }
    let mut directories = vec![PrivateMetadata::from_metadata(&root_metadata)];
    for component in parent_components {
        let opened = fs::openat(current.as_fd(), *component, DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_directory_open)?;
        let opened = File::from(opened);
        let observed = metadata(&opened)?;
        if !valid_directory_metadata(&observed) {
            return Err(ExecutableObservationError::Unsafe);
        }
        directories.push(PrivateMetadata::from_metadata(&observed));
        current = opened;
    }

    let executable = fs::openat(current.as_fd(), *file_name, FILE_FLAGS, Mode::empty())
        .map_err(map_file_open)?;
    let mut executable = File::from(executable);
    let executable_metadata = metadata(&executable)?;
    if !valid_executable_metadata(&executable_metadata) {
        return Err(ExecutableObservationError::Unsafe);
    }
    let mut magic = [0_u8; 4];
    executable
        .read_exact(&mut magic)
        .map_err(|_| ExecutableObservationError::Unknown)?;
    if !reviewed_executable_magic(magic) {
        return Err(ExecutableObservationError::Unsafe);
    }

    Ok(ExecutableSnapshot {
        directories,
        file: PrivateMetadata::from_metadata(&executable_metadata),
    })
}

fn metadata(file: &File) -> Result<std::fs::Metadata, ExecutableObservationError> {
    file.metadata()
        .map_err(|_| ExecutableObservationError::Unknown)
}

fn valid_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    let private = PrivateMetadata::from_metadata(metadata);
    metadata.is_file()
        && metadata.nlink() == 1
        && owner_and_mode_are_reviewed(&private)
        && metadata.len() >= 4
}

fn valid_directory_metadata(metadata: &std::fs::Metadata) -> bool {
    let private = PrivateMetadata::from_metadata(metadata);
    metadata.is_dir() && directory_owner_and_mode_are_reviewed(&private)
}

fn directory_owner_and_mode_are_reviewed(metadata: &PrivateMetadata) -> bool {
    (metadata.uid == 0 || metadata.uid == geteuid().as_raw())
        && (metadata.mode & 0o022 == 0 || metadata.mode & 0o1000 != 0)
}

fn owner_and_mode_are_reviewed(metadata: &PrivateMetadata) -> bool {
    (metadata.uid == 0 || metadata.uid == geteuid().as_raw())
        && metadata.mode & 0o022 == 0
        && metadata.mode & 0o111 != 0
}

fn reviewed_executable_magic(magic: [u8; 4]) -> bool {
    if cfg!(target_os = "linux") {
        magic == *b"\x7fELF"
    } else if cfg!(target_os = "macos") {
        matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
    } else {
        false
    }
}

fn valid_absolute_path(path: &Path) -> bool {
    path != Path::new("/")
        && path.is_absolute()
        && path.to_str().is_some()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn normal_components(path: &Path) -> Vec<&OsStr> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            Component::RootDir => None,
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => None,
        })
        .collect()
}

fn map_directory_open(error: Errno) -> ExecutableObservationError {
    match error {
        Errno::NOENT | Errno::LOOP | Errno::NOTDIR => ExecutableObservationError::Unsafe,
        _ => ExecutableObservationError::Unknown,
    }
}

fn map_file_open(error: Errno) -> ExecutableObservationError {
    match error {
        Errno::NOENT | Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => {
            ExecutableObservationError::Unsafe
        }
        _ => ExecutableObservationError::Unknown,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionProbe {
    Version(String),
    Unknown,
}

fn probe_version(
    kind: ToolKind,
    path: &Path,
    executor: &impl TimedCommandExecutor,
) -> VersionProbe {
    let spec = CommandSpec::new(path.to_path_buf())
        .argument("--version")
        .environment("LANG", "C")
        .environment("LC_ALL", "C");
    let expected_argv = spec.displayed_argv();
    let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
    let Ok(record) = executor.execute_with_timeout(&spec, LOCAL_INSTALL_TOOLCHAIN_PROBE_TIMEOUT)
    else {
        return VersionProbe::Unknown;
    };
    if record.argv != expected_argv
        || record.environment_keys != expected_environment_keys
        || !record.success
        || record.status != Some(0)
        || !record.stderr.is_empty()
        || record.stdout.len() > MAX_LOCAL_INSTALL_TOOLCHAIN_VERSION_OUTPUT_BYTES
    {
        return VersionProbe::Unknown;
    }
    parse_version_output(kind, &record.stdout).map_or(VersionProbe::Unknown, VersionProbe::Version)
}

fn parse_version_output(kind: ToolKind, output: &str) -> Option<String> {
    if output.contains('\r') || output.contains('\0') {
        return None;
    }
    let line = output.strip_suffix('\n').unwrap_or(output);
    if line.is_empty() || line.contains('\n') || !line.is_ascii() {
        return None;
    }
    let mut fields = line.split_ascii_whitespace();
    if fields.next()? != kind.command_name() {
        return None;
    }
    let version = fields.next()?;
    if !valid_version(version) {
        return None;
    }
    Some(version.to_owned())
}

fn expected_version(identity: &str) -> Option<&str> {
    let rest = identity.strip_prefix("rust-")?;
    let (version, target) = rest.split_once('-')?;
    if !valid_version(version)
        || target.is_empty()
        || !target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return None;
    }
    Some(version)
}

fn valid_version(version: &str) -> bool {
    let mut components = version.split('.');
    let Some(major) = components.next() else {
        return false;
    };
    let Some(minor) = components.next() else {
        return false;
    };
    let Some(patch) = components.next() else {
        return false;
    };
    components.next().is_none()
        && [major, minor, patch].into_iter().all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::process::{CommandExecutor, ExecutionRecord};

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempToolchain {
        root: PathBuf,
        cargo: PathBuf,
        rustc: PathBuf,
        rustdoc: PathBuf,
    }

    impl TempToolchain {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let temporary_root = fs::canonicalize(std::env::temp_dir())
                .expect("canonicalize test temporary directory");
            let root = temporary_root.join(format!(
                "smolrunner-toolchain-preflight-{label}-{}-{sequence}",
                std::process::id()
            ));
            let bin = root.join("bin");
            fs::create_dir_all(&bin).expect("create toolchain bin");
            let cargo = bin.join("cargo");
            let rustc = bin.join("rustc");
            let rustdoc = bin.join("rustdoc");
            for path in [&cargo, &rustc, &rustdoc] {
                write_executable(path);
            }
            Self {
                root,
                cargo,
                rustc,
                rustdoc,
            }
        }

        fn context(&self) -> LocalInstallBuildCommandContext {
            LocalInstallBuildCommandContext::new(
                self.root.join("source"),
                self.root.join("build"),
                self.cargo.clone(),
                self.rustc.clone(),
                self.rustdoc.clone(),
            )
            .expect("command context")
        }
    }

    impl Drop for TempToolchain {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_executable(path: &Path) {
        let mut bytes = if cfg!(target_os = "linux") {
            b"\x7fELFfake-reviewed-toolchain-object".to_vec()
        } else {
            vec![0xfe, 0xed, 0xfa, 0xcf, b'f', b'a', b'k', b'e']
        };
        bytes.extend_from_slice(b"-body");
        fs::write(path, bytes).expect("write executable fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("set executable mode");
    }

    #[derive(Clone)]
    struct Response {
        stdout: String,
        stderr: String,
        status: i32,
    }

    impl Response {
        fn success(stdout: impl Into<String>) -> Self {
            Self {
                stdout: stdout.into(),
                stderr: String::new(),
                status: 0,
            }
        }
    }

    struct ScriptedExecutor {
        responses: RefCell<VecDeque<Response>>,
        commands: RefCell<Vec<CommandSpec>>,
        calls: Cell<usize>,
        replacement: RefCell<Option<(usize, PathBuf)>>,
    }

    impl ScriptedExecutor {
        fn new(responses: Vec<Response>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                commands: RefCell::new(Vec::new()),
                calls: Cell::new(0),
                replacement: RefCell::new(None),
            }
        }

        fn replace_on_call(self, call: usize, path: PathBuf) -> Self {
            self.replacement.replace(Some((call, path)));
            self
        }
    }

    impl CommandExecutor for ScriptedExecutor {
        fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            panic!("toolchain preflight requires timed execution")
        }
    }

    impl TimedCommandExecutor for ScriptedExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            assert_eq!(timeout, LOCAL_INSTALL_TOOLCHAIN_PROBE_TIMEOUT);
            let call = self.calls.get();
            if self
                .replacement
                .borrow()
                .as_ref()
                .is_some_and(|(target, _)| *target == call)
            {
                let (_, path) = self.replacement.borrow_mut().take().expect("replacement");
                let staged = path.with_extension("replacement");
                write_executable(&staged);
                fs::rename(&staged, &path).expect("replace executable");
            }
            self.calls.set(call + 1);
            self.commands.borrow_mut().push(spec.clone());
            let response = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("scripted response");
            Ok(ExecutionRecord {
                argv: spec.displayed_argv(),
                environment_keys: spec.environment.keys().cloned().collect(),
                status: Some(response.status),
                success: response.status == 0,
                stdout: response.stdout,
                stderr: response.stderr,
            })
        }
    }

    fn expected() -> LocalInstallToolchainIdentity {
        LocalInstallToolchainIdentity::parse("rust-1.97.1-x86_64-unknown-linux-gnu")
            .expect("toolchain identity")
    }

    fn exact_responses() -> Vec<Response> {
        vec![
            Response::success("cargo 1.97.1 (111111111 2026-07-14)\n"),
            Response::success("rustc 1.97.1 (222222222 2026-07-14)\n"),
            Response::success("rustdoc 1.97.1 (333333333 2026-07-14)\n"),
        ]
    }

    #[test]
    fn exact_toolchain_is_ready_with_fixed_probe_contract() {
        let fixture = TempToolchain::new("exact");
        let context = fixture.context();
        let executor = ScriptedExecutor::new(exact_responses());
        let receipt = observe_local_install_toolchain_preflight(&expected(), &context, &executor);

        assert!(receipt.ready());
        assert_eq!(
            receipt.cargo(),
            LocalInstallToolchainExecutableDisposition::Exact
        );
        assert_eq!(
            receipt.rustc(),
            LocalInstallToolchainExecutableDisposition::Exact
        );
        assert_eq!(
            receipt.rustdoc(),
            LocalInstallToolchainExecutableDisposition::Exact
        );
        assert!(receipt.blocking_codes().is_empty());
        let commands = executor.commands.borrow();
        assert_eq!(commands.len(), 3);
        for (command, expected_program) in
            commands
                .iter()
                .zip([&fixture.cargo, &fixture.rustc, &fixture.rustdoc])
        {
            assert_eq!(&command.program, expected_program);
            assert_eq!(command.displayed_argv().len(), 2);
            assert_eq!(command.displayed_argv()[1], "--version");
            assert_eq!(
                command
                    .environment
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["LANG", "LC_ALL"]
            );
            assert!(!command.environment.contains_key("PATH"));
        }
        let public = serde_json::to_string(&receipt).expect("receipt JSON");
        assert!(!public.contains(fixture.root.to_string_lossy().as_ref()));
        assert!(!public.contains("111111111"));
    }

    #[test]
    fn valid_wrong_version_is_mismatch_and_malformed_output_is_unknown() {
        let fixture = TempToolchain::new("version-errors");
        let context = fixture.context();
        let executor = ScriptedExecutor::new(vec![
            Response::success("cargo 1.96.0 (old)\n"),
            Response::success("rustc 1.97.1 (exact)\n"),
            Response::success("surprising rustdoc output\n"),
        ]);
        let receipt = observe_local_install_toolchain_preflight(&expected(), &context, &executor);

        assert_eq!(
            receipt.cargo(),
            LocalInstallToolchainExecutableDisposition::Mismatch
        );
        assert_eq!(
            receipt.rustc(),
            LocalInstallToolchainExecutableDisposition::Exact
        );
        assert_eq!(
            receipt.rustdoc(),
            LocalInstallToolchainExecutableDisposition::Unknown
        );
        assert_eq!(
            receipt.blocking_codes(),
            [
                LocalInstallToolchainBlockingCode::CargoMismatch,
                LocalInstallToolchainBlockingCode::RustdocUnknown,
            ]
        );
    }

    #[test]
    fn executable_replacement_during_probe_is_changed() {
        let fixture = TempToolchain::new("replacement");
        let context = fixture.context();
        let executor =
            ScriptedExecutor::new(exact_responses()).replace_on_call(0, fixture.cargo.clone());
        let receipt = observe_local_install_toolchain_preflight(&expected(), &context, &executor);

        assert_eq!(
            receipt.cargo(),
            LocalInstallToolchainExecutableDisposition::Changed
        );
        assert_eq!(
            receipt.blocking_codes(),
            [LocalInstallToolchainBlockingCode::CargoChanged]
        );
    }

    #[test]
    fn symlink_hardlink_and_untrusted_write_modes_are_unsafe() {
        use std::os::unix::fs::symlink;

        for case in ["symlink", "hardlink", "writable"] {
            let fixture = TempToolchain::new(case);
            if case == "symlink" {
                fs::rename(&fixture.cargo, fixture.cargo.with_extension("real"))
                    .expect("move real cargo");
                symlink("cargo.real", &fixture.cargo).expect("cargo symlink");
            } else if case == "hardlink" {
                fs::hard_link(&fixture.cargo, fixture.cargo.with_extension("alias"))
                    .expect("cargo hardlink");
            } else {
                fs::set_permissions(&fixture.cargo, fs::Permissions::from_mode(0o775))
                    .expect("make cargo group writable");
            }
            let context = fixture.context();
            let executor = ScriptedExecutor::new(vec![
                Response::success("rustc 1.97.1 (exact)\n"),
                Response::success("rustdoc 1.97.1 (exact)\n"),
            ]);
            let receipt =
                observe_local_install_toolchain_preflight(&expected(), &context, &executor);
            assert_eq!(
                receipt.cargo(),
                LocalInstallToolchainExecutableDisposition::Unsafe
            );
            assert_eq!(
                receipt.blocking_codes(),
                [LocalInstallToolchainBlockingCode::CargoUnsafe]
            );
            assert_eq!(executor.commands.borrow().len(), 2);
        }
    }

    #[test]
    fn wrong_owner_class_is_rejected_by_metadata_policy() {
        let fixture = TempToolchain::new("owner");
        let metadata = fs::metadata(&fixture.cargo).expect("cargo metadata");
        let mut private = PrivateMetadata::from_metadata(&metadata);
        private.uid = geteuid().as_raw().saturating_add(1).max(1);
        assert_ne!(private.uid, 0);
        assert_ne!(private.uid, geteuid().as_raw());
        assert!(!owner_and_mode_are_reviewed(&private));
    }

    #[test]
    fn directory_policy_rejects_foreign_or_nonsticky_writable_ancestors() {
        let fixture = TempToolchain::new("directory-policy");
        let metadata = fs::metadata(&fixture.root).expect("fixture root metadata");
        let mut private = PrivateMetadata::from_metadata(&metadata);

        private.uid = geteuid().as_raw().saturating_add(1).max(1);
        assert_ne!(private.uid, 0);
        assert_ne!(private.uid, geteuid().as_raw());
        assert!(!directory_owner_and_mode_are_reviewed(&private));

        private.uid = geteuid().as_raw();
        private.mode = (private.mode & !0o7777) | 0o0777;
        assert!(!directory_owner_and_mode_are_reviewed(&private));

        private.mode |= 0o1000;
        assert!(directory_owner_and_mode_are_reviewed(&private));
    }

    #[test]
    fn shared_ancestor_churn_is_not_private_toolchain_change() {
        let fixture = TempToolchain::new("shared-ancestor-metadata");
        let metadata = fs::metadata(&fixture.root).expect("fixture root metadata");
        let before = PrivateMetadata::from_metadata(&metadata);
        let mut after = before.clone();
        after.links = after.links.saturating_add(1);
        after.size = after.size.saturating_add(4096);
        after.modified_nanoseconds = after.modified_nanoseconds.saturating_add(1);
        after.changed_nanoseconds = after.changed_nanoseconds.saturating_add(1);

        assert!(before.same_path_component_as(&after));
        assert!(!before.same_as(&after));
    }

    #[test]
    fn unsupported_expected_identity_blocks_without_process_execution() {
        let fixture = TempToolchain::new("unsupported-identity");
        let context = fixture.context();
        let expected = LocalInstallToolchainIdentity::parse("opaque-toolchain-token")
            .expect("lexically valid old identity");
        let executor = ScriptedExecutor::new(Vec::new());
        let receipt = observe_local_install_toolchain_preflight(&expected, &context, &executor);
        assert_eq!(
            receipt.blocking_codes(),
            [LocalInstallToolchainBlockingCode::ExpectedIdentityUnsupported]
        );
        assert!(executor.commands.borrow().is_empty());
    }

    #[test]
    fn stable_fixture_observation_is_deterministic() {
        let fixture = TempToolchain::new("deterministic");
        let context = fixture.context();
        let first = observe_local_install_toolchain_preflight(
            &expected(),
            &context,
            &ScriptedExecutor::new(exact_responses()),
        );
        let second = observe_local_install_toolchain_preflight(
            &expected(),
            &context,
            &ScriptedExecutor::new(exact_responses()),
        );
        assert_eq!(first, second);
    }
}
