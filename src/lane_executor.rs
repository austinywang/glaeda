use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path};

use serde::Serialize;

use crate::journal::ExecutionLane;
use crate::lane_command::{LaneCommand, LaneCommandKind, LinuxAccountName, PackageName};
use crate::lane_executable::verify_lane_command;
use crate::process::{
    CommandExecutor, CommandSpec, CommandValue, ExecutionRecord, ProcessExecutor,
};
use crate::runner_user::{MIN_SUBORDINATE_ID_COUNT, VerifiedRunnerUser};

const PROC_SELF_STATUS: &str = "/proc/self/status";
const MAX_PROC_STATUS_BYTES: usize = 64 * 1024;
const APT_GET: &str = "/usr/bin/apt-get";
const GROUPADD: &str = "/usr/sbin/groupadd";
const USERADD: &str = "/usr/sbin/useradd";
const USERMOD: &str = "/usr/sbin/usermod";
const INSTALL: &str = "/usr/bin/install";
const LOGINCTL: &str = "/usr/bin/loginctl";
const RUNUSER: &str = "/usr/sbin/runuser";
const PODMAN: &str = "/usr/bin/podman";
const GIT: &str = "/usr/bin/git";
const NOLOGIN: &str = "/usr/sbin/nologin";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneExecutionErrorKind {
    LaneMismatch,
    InvalidCommand,
    InvalidRunnerEvidence,
    UnsupportedPrivilege,
    ExecutableVerification,
    Process,
}

/// Bounded public failure from a typed privilege-lane executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaneExecutionError {
    kind: LaneExecutionErrorKind,
    public_message: String,
}

impl LaneExecutionError {
    #[must_use]
    pub const fn kind(&self) -> LaneExecutionErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(kind: LaneExecutionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for LaneExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for LaneExecutionError {}

/// Typed lane metadata paired with the complete bounded and redacted subprocess record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaneExecutionRecord {
    action_id: String,
    lane: ExecutionLane,
    kind: LaneCommandKind,
    process: ExecutionRecord,
}

impl LaneExecutionRecord {
    #[must_use]
    pub fn action_id(&self) -> &str {
        &self.action_id
    }

    #[must_use]
    pub const fn lane(&self) -> ExecutionLane {
        self.lane
    }

    #[must_use]
    pub const fn kind(&self) -> LaneCommandKind {
        self.kind
    }

    #[must_use]
    pub fn process(&self) -> &ExecutionRecord {
        &self.process
    }

    #[must_use]
    pub fn into_process(self) -> ExecutionRecord {
        self.process
    }

    #[must_use]
    pub const fn status(&self) -> Option<i32> {
        self.process.status
    }

    #[must_use]
    pub const fn success(&self) -> bool {
        self.process.success
    }

    fn new(command: &LaneCommand, process: ExecutionRecord) -> Self {
        Self {
            action_id: command.action_id().to_owned(),
            lane: command.lane(),
            kind: command.kind(),
            process,
        }
    }
}

trait PrivilegeProbe {
    /// Return the process's effective Linux UID.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when privilege evidence cannot be obtained or parsed safely.
    fn effective_uid(&self) -> io::Result<u32>;
}

#[derive(Debug, Default, Clone, Copy)]
struct ProcStatusPrivilegeProbe;

impl PrivilegeProbe for ProcStatusPrivilegeProbe {
    fn effective_uid(&self) -> io::Result<u32> {
        let mut bytes = Vec::with_capacity(MAX_PROC_STATUS_BYTES + 1);
        let mut reader = fs::File::open(PROC_SELF_STATUS)?.take((MAX_PROC_STATUS_BYTES + 1) as u64);
        reader.read_to_end(&mut bytes)?;
        if bytes.len() > MAX_PROC_STATUS_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status exceeds the configured size limit",
            ));
        }
        let input = std::str::from_utf8(&bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "process status is not UTF-8")
        })?;
        parse_effective_uid(input)
    }
}

trait ReviewedExecutableVerifier {
    /// Verify every reviewed executable required by one typed command.
    ///
    /// # Errors
    ///
    /// Returns a bounded execution error when any executable lacks trusted evidence.
    fn verify(&self, command: &LaneCommand) -> Result<(), LaneExecutionError>;
}

#[derive(Debug, Default, Clone, Copy)]
struct SystemExecutableVerifier;

impl ReviewedExecutableVerifier for SystemExecutableVerifier {
    fn verify(&self, command: &LaneCommand) -> Result<(), LaneExecutionError> {
        verify_lane_command(command).map(|_| ()).map_err(|error| {
            LaneExecutionError::new(
                LaneExecutionErrorKind::ExecutableVerification,
                error.message().to_owned(),
            )
        })
    }
}

trait RunnerEvidence {
    fn username(&self) -> &LinuxAccountName;
    fn uid(&self) -> u32;
    fn primary_gid(&self) -> u32;
    fn home(&self) -> &str;
    fn runtime_directory(&self) -> &Path;
    fn subordinate_uid_count(&self) -> u64;
    fn subordinate_gid_count(&self) -> u64;
}

impl RunnerEvidence for VerifiedRunnerUser {
    fn username(&self) -> &LinuxAccountName {
        self.username()
    }

    fn uid(&self) -> u32 {
        self.uid()
    }

    fn primary_gid(&self) -> u32 {
        self.primary_gid()
    }

    fn home(&self) -> &str {
        self.home()
    }

    fn runtime_directory(&self) -> &Path {
        self.runtime_directory()
    }

    fn subordinate_uid_count(&self) -> u64 {
        self.subordinate_uid_count()
    }

    fn subordinate_gid_count(&self) -> u64 {
        self.subordinate_gid_count()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RootLaneExecutor;

impl RootLaneExecutor {
    #[must_use]
    pub const fn system() -> Self {
        Self
    }

    /// Execute one reviewed root-lane command after every boundary check succeeds.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for lane mismatch, malformed command shape, unsupported privilege,
    /// untrusted executable evidence, or an incomplete process record.
    pub fn execute(
        &self,
        command: &LaneCommand,
    ) -> Result<LaneExecutionRecord, LaneExecutionError> {
        execute_root_lane(
            &ProcessExecutor,
            &SystemExecutableVerifier,
            &ProcStatusPrivilegeProbe,
            command,
        )
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RunnerUserLaneExecutor;

impl RunnerUserLaneExecutor {
    #[must_use]
    pub const fn system() -> Self {
        Self
    }

    /// Execute one reviewed command through the sealed runner-user boundary.
    ///
    /// The caller must supply previously verified account, runtime-directory, and subordinate-ID
    /// evidence. The executor rechecks that the typed command matches that exact evidence.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for lane mismatch, unsafe or mismatched runner evidence, malformed
    /// command shape, unsupported privilege, untrusted executable evidence, or an incomplete process
    /// record.
    pub fn execute(
        &self,
        command: &LaneCommand,
        runner: &VerifiedRunnerUser,
    ) -> Result<LaneExecutionRecord, LaneExecutionError> {
        execute_runner_user_lane(
            &ProcessExecutor,
            &SystemExecutableVerifier,
            &ProcStatusPrivilegeProbe,
            command,
            runner,
        )
    }
}

fn execute_root_lane(
    process: &impl CommandExecutor,
    executables: &impl ReviewedExecutableVerifier,
    privilege: &impl PrivilegeProbe,
    command: &LaneCommand,
) -> Result<LaneExecutionRecord, LaneExecutionError> {
    require_lane(command, ExecutionLane::Root)?;
    validate_root_command(command)?;
    require_effective_root(privilege)?;
    executables.verify(command)?;
    execute_bounded(process, command)
}

fn execute_runner_user_lane(
    process: &impl CommandExecutor,
    executables: &impl ReviewedExecutableVerifier,
    privilege: &impl PrivilegeProbe,
    command: &LaneCommand,
    runner: &impl RunnerEvidence,
) -> Result<LaneExecutionRecord, LaneExecutionError> {
    require_lane(command, ExecutionLane::RunnerUser)?;
    validate_runner_evidence(runner)?;
    validate_runner_command(command, runner)?;
    require_effective_root(privilege)?;
    executables.verify(command)?;
    execute_bounded(process, command)
}

fn execute_bounded(
    process: &impl CommandExecutor,
    command: &LaneCommand,
) -> Result<LaneExecutionRecord, LaneExecutionError> {
    process
        .execute(command.spec())
        .map(|record| LaneExecutionRecord::new(command, record))
        .map_err(|_| {
            LaneExecutionError::new(
                LaneExecutionErrorKind::Process,
                "reviewed lane process did not produce a complete bounded record; host state may have changed and must be re-observed before retry",
            )
        })
}

fn require_lane(command: &LaneCommand, expected: ExecutionLane) -> Result<(), LaneExecutionError> {
    if command.lane() == expected {
        Ok(())
    } else {
        Err(LaneExecutionError::new(
            LaneExecutionErrorKind::LaneMismatch,
            format!(
                "action {:?} is assigned to {:?}, but this executor accepts only {:?}",
                command.action_id(),
                command.lane(),
                expected
            ),
        ))
    }
}

fn require_effective_root(probe: &impl PrivilegeProbe) -> Result<(), LaneExecutionError> {
    let effective_uid = probe.effective_uid().map_err(|_| {
        LaneExecutionError::new(
            LaneExecutionErrorKind::UnsupportedPrivilege,
            "could not verify effective Linux privilege; no mutation was attempted",
        )
    })?;
    if effective_uid == 0 {
        Ok(())
    } else {
        Err(LaneExecutionError::new(
            LaneExecutionErrorKind::UnsupportedPrivilege,
            "mutating lane execution requires effective UID 0; rerun through explicit elevation such as sudo smolrunner apply",
        ))
    }
}

fn validate_root_command(command: &LaneCommand) -> Result<(), LaneExecutionError> {
    let spec = command.spec();
    if !spec.environment.is_empty() {
        return Err(invalid_command(
            "root-lane commands must have an empty child environment",
        ));
    }
    let arguments = plain_arguments(spec)?;
    match command.kind() {
        LaneCommandKind::AptInstall => validate_apt(spec, &arguments),
        LaneCommandKind::EnsureSystemGroup => validate_groupadd(spec, &arguments),
        LaneCommandKind::EnsureSystemUser => validate_useradd(spec, &arguments),
        LaneCommandKind::EnsureHomeDirectory => validate_install(spec, &arguments),
        LaneCommandKind::EnsureSubordinateUids => {
            validate_usermod(spec, &arguments, "--add-subuids")
        }
        LaneCommandKind::EnsureSubordinateGids => {
            validate_usermod(spec, &arguments, "--add-subgids")
        }
        LaneCommandKind::EnableLinger => validate_loginctl(spec, &arguments),
        LaneCommandKind::RunnerPodmanInfo
        | LaneCommandKind::RunnerPodmanMigrate
        | LaneCommandKind::RunnerGitVersion => Err(invalid_command(
            "runner-user command kind cannot execute in the root lane",
        )),
    }
}

fn validate_apt(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    if spec.program != Path::new(APT_GET)
        || arguments.len() < 4
        || arguments[..3] != ["install", "--yes", "--no-install-recommends"]
        || arguments[3..]
            .iter()
            .any(|package| PackageName::parse(package).is_err())
    {
        return Err(invalid_command("apt command shape is not reviewed"));
    }
    Ok(())
}

fn validate_groupadd(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    if spec.program != Path::new(GROUPADD)
        || arguments.len() != 2
        || arguments[0] != "--system"
        || LinuxAccountName::parse(arguments[1]).is_err()
    {
        return Err(invalid_command(
            "group creation command shape is not reviewed",
        ));
    }
    Ok(())
}

fn validate_useradd(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    let valid = spec.program == Path::new(USERADD)
        && arguments.len() == 9
        && arguments[0] == "--system"
        && arguments[1] == "--gid"
        && LinuxAccountName::parse(arguments[2]).is_ok()
        && arguments[3] == "--home-dir"
        && canonical_absolute_path(arguments[4])
        && arguments[5] == "--shell"
        && arguments[6] == NOLOGIN
        && arguments[7] == "--no-create-home"
        && LinuxAccountName::parse(arguments[8]).is_ok();
    if valid {
        Ok(())
    } else {
        Err(invalid_command(
            "user creation command shape is not reviewed",
        ))
    }
}

fn validate_install(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    let valid = spec.program == Path::new(INSTALL)
        && arguments.len() == 9
        && arguments[0] == "--directory"
        && arguments[1] == "--mode"
        && arguments[2] == "0750"
        && arguments[3] == "--owner"
        && LinuxAccountName::parse(arguments[4]).is_ok()
        && arguments[5] == "--group"
        && LinuxAccountName::parse(arguments[6]).is_ok()
        && arguments[7] == "--"
        && canonical_absolute_path(arguments[8]);
    if valid {
        Ok(())
    } else {
        Err(invalid_command(
            "runner home-directory command shape is not reviewed",
        ))
    }
}

fn validate_usermod(
    spec: &CommandSpec,
    arguments: &[&str],
    expected_option: &str,
) -> Result<(), LaneExecutionError> {
    let valid = spec.program == Path::new(USERMOD)
        && arguments.len() == 4
        && arguments[0] == expected_option
        && valid_subordinate_range(arguments[1])
        && arguments[2] == "--"
        && LinuxAccountName::parse(arguments[3]).is_ok();
    if valid {
        Ok(())
    } else {
        Err(invalid_command(
            "subordinate-ID command shape is not reviewed",
        ))
    }
}

fn valid_subordinate_range(value: &str) -> bool {
    let Some((start, end)) = value.split_once('-') else {
        return false;
    };
    let Some(start) = canonical_u32(start) else {
        return false;
    };
    let Some(end) = canonical_u32(end) else {
        return false;
    };
    start > 0 && start <= end && u64::from(end) - u64::from(start) + 1 >= MIN_SUBORDINATE_ID_COUNT
}

fn canonical_u32(value: &str) -> Option<u32> {
    let parsed = value.parse::<u32>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn validate_loginctl(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    if spec.program != Path::new(LOGINCTL)
        || arguments.len() != 2
        || arguments[0] != "enable-linger"
        || LinuxAccountName::parse(arguments[1]).is_err()
    {
        return Err(invalid_command("linger command shape is not reviewed"));
    }
    Ok(())
}

fn validate_runner_evidence(runner: &impl RunnerEvidence) -> Result<(), LaneExecutionError> {
    if runner.uid() == 0
        || runner.primary_gid() == 0
        || runner.subordinate_uid_count() < MIN_SUBORDINATE_ID_COUNT
        || runner.subordinate_gid_count() < MIN_SUBORDINATE_ID_COUNT
        || !canonical_absolute_path(runner.home())
        || runner.runtime_directory() != Path::new(&format!("/run/user/{}", runner.uid()))
    {
        return Err(LaneExecutionError::new(
            LaneExecutionErrorKind::InvalidRunnerEvidence,
            "runner-user evidence is incomplete or unsafe",
        ));
    }
    Ok(())
}

fn validate_runner_command(
    command: &LaneCommand,
    runner: &impl RunnerEvidence,
) -> Result<(), LaneExecutionError> {
    let spec = command.spec();
    if !spec.environment.is_empty() || spec.program != Path::new(RUNUSER) {
        return Err(invalid_command(
            "runner-user command outer boundary is not reviewed",
        ));
    }
    let (inner_program, inner_arguments): (&str, &[&str]) = match command.kind() {
        LaneCommandKind::RunnerPodmanInfo => (PODMAN, &["info", "--format", "json"]),
        LaneCommandKind::RunnerPodmanMigrate => (PODMAN, &["system", "migrate"]),
        LaneCommandKind::RunnerGitVersion => (GIT, &["--version"]),
        LaneCommandKind::AptInstall
        | LaneCommandKind::EnsureSystemGroup
        | LaneCommandKind::EnsureSystemUser
        | LaneCommandKind::EnsureHomeDirectory
        | LaneCommandKind::EnsureSubordinateUids
        | LaneCommandKind::EnsureSubordinateGids
        | LaneCommandKind::EnableLinger => {
            return Err(invalid_command(
                "root command kind cannot execute in the runner-user lane",
            ));
        }
    };

    let mut expected = vec![
        "--user".to_owned(),
        runner.username().as_str().to_owned(),
        "--".to_owned(),
        command
            .runner_environment_program()
            .ok_or_else(|| {
                invalid_command("runner-user command lacks a reviewed environment program")
            })?
            .to_str()
            .ok_or_else(|| {
                invalid_command("runner-user environment program is not a canonical path")
            })?
            .to_owned(),
        "--ignore-environment".to_owned(),
        format!("HOME={}", runner.home()),
        format!("USER={}", runner.username().as_str()),
        format!("LOGNAME={}", runner.username().as_str()),
        format!("XDG_RUNTIME_DIR={}", runner.runtime_directory().display()),
        inner_program.to_owned(),
    ];
    expected.extend(
        inner_arguments
            .iter()
            .map(|argument| (*argument).to_owned()),
    );
    let arguments = plain_arguments(spec)?;
    if arguments
        .iter()
        .copied()
        .ne(expected.iter().map(String::as_str))
    {
        return Err(invalid_command(
            "runner-user command argv or environment boundary differs from verified evidence",
        ));
    }
    Ok(())
}

fn plain_arguments(spec: &CommandSpec) -> Result<Vec<&str>, LaneExecutionError> {
    spec.arguments
        .iter()
        .map(|value| match value {
            CommandValue::Plain(value) => Ok(value.as_str()),
            CommandValue::Secret(_) => Err(invalid_command(
                "lane commands cannot contain secret or opaque argv values",
            )),
        })
        .collect()
}

fn canonical_absolute_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= 4_096
        && !value.ends_with('/')
        && !value.chars().any(char::is_control)
        && path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn invalid_command(message: impl Into<String>) -> LaneExecutionError {
    LaneExecutionError::new(LaneExecutionErrorKind::InvalidCommand, message)
}

fn parse_effective_uid(input: &str) -> io::Result<u32> {
    let mut effective_uid = None;
    for line in input.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("Uid:") {
            continue;
        }
        if effective_uid.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status contains multiple UID records",
            ));
        }
        let values = fields.collect::<Vec<_>>();
        if values.len() != 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status UID record is malformed",
            ));
        }
        let value = values[1];
        let parsed = value.parse::<u32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "process status effective UID is malformed",
            )
        })?;
        if parsed.to_string() != value {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status effective UID is noncanonical",
            ));
        }
        effective_uid = Some(parsed);
    }
    effective_uid.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "process status is missing the UID record",
        )
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::io;

    use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
    use crate::lane_command::{LaneCommand, LinuxAccountName, PackageName, RunnerUserContext};
    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};
    use crate::runner_user::MIN_SUBORDINATE_ID_COUNT;

    use super::{
        GIT, LaneExecutionError, LaneExecutionErrorKind, PODMAN, PrivilegeProbe,
        ReviewedExecutableVerifier, RunnerEvidence, execute_root_lane, execute_runner_user_lane,
        parse_effective_uid, validate_root_command,
    };

    struct FakeProcess {
        calls: RefCell<Vec<CommandSpec>>,
        record: ExecutionRecord,
        failure: Option<String>,
    }

    impl FakeProcess {
        fn successful() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                record: ExecutionRecord {
                    argv: vec!["/usr/bin/fake".to_owned(), "--checked".to_owned()],
                    environment_keys: vec![],
                    status: Some(0),
                    success: true,
                    stdout: "bounded stdout".to_owned(),
                    stderr: "bounded stderr".to_owned(),
                },
                failure: None,
            }
        }
    }

    impl CommandExecutor for FakeProcess {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.calls.borrow_mut().push(spec.clone());
            match &self.failure {
                Some(message) => Err(io::Error::other(message.clone())),
                None => Ok(self.record.clone()),
            }
        }
    }

    #[derive(Default)]
    struct FakeVerifier {
        calls: Cell<usize>,
        fail: bool,
    }

    impl ReviewedExecutableVerifier for FakeVerifier {
        fn verify(&self, _command: &LaneCommand) -> Result<(), LaneExecutionError> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                Err(LaneExecutionError::new(
                    LaneExecutionErrorKind::ExecutableVerification,
                    "reviewed executable evidence failed",
                ))
            } else {
                Ok(())
            }
        }
    }

    struct FakePrivilege {
        calls: Cell<usize>,
        uid: u32,
        fail: bool,
    }

    impl PrivilegeProbe for FakePrivilege {
        fn effective_uid(&self) -> io::Result<u32> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                Err(io::Error::other("private privilege failure"))
            } else {
                Ok(self.uid)
            }
        }
    }

    fn root_privilege() -> FakePrivilege {
        FakePrivilege {
            calls: Cell::new(0),
            uid: 0,
            fail: false,
        }
    }

    fn action(id: &str, lane: ExecutionLane) -> PlannedMutation {
        PlannedMutation::new(
            id,
            lane,
            "test lane execution",
            RollbackClass::Reversible,
            Preconditions::new(["evidence verified"]),
        )
    }

    fn account(value: &str) -> LinuxAccountName {
        LinuxAccountName::parse(value).expect("account name")
    }

    fn runner_context(username: &str, uid: u32, gid: u32) -> RunnerUserContext {
        RunnerUserContext::new(account(username), uid, gid, "/srv/project-runner")
            .expect("runner context")
    }

    struct FakeRunnerEvidence {
        username: LinuxAccountName,
        uid: u32,
        primary_gid: u32,
        home: String,
        runtime_directory: String,
        subordinate_uid_count: u64,
        subordinate_gid_count: u64,
    }

    impl RunnerEvidence for FakeRunnerEvidence {
        fn username(&self) -> &LinuxAccountName {
            &self.username
        }

        fn uid(&self) -> u32 {
            self.uid
        }

        fn primary_gid(&self) -> u32 {
            self.primary_gid
        }

        fn home(&self) -> &str {
            &self.home
        }

        fn runtime_directory(&self) -> &std::path::Path {
            std::path::Path::new(&self.runtime_directory)
        }

        fn subordinate_uid_count(&self) -> u64 {
            self.subordinate_uid_count
        }

        fn subordinate_gid_count(&self) -> u64 {
            self.subordinate_gid_count
        }
    }

    fn runner_evidence(username: &str, uid: u32, gid: u32, home: &str) -> FakeRunnerEvidence {
        FakeRunnerEvidence {
            username: account(username),
            uid,
            primary_gid: gid,
            home: home.to_owned(),
            runtime_directory: format!("/run/user/{uid}"),
            subordinate_uid_count: MIN_SUBORDINATE_ID_COUNT,
            subordinate_gid_count: MIN_SUBORDINATE_ID_COUNT,
        }
    }

    #[test]
    fn root_validator_accepts_reviewed_account_preparation_commands() {
        let root = action("prepare-account", ExecutionLane::Root);
        let account = account("project-runner");
        for command in [
            LaneCommand::ensure_home_directory(
                &root,
                &account,
                &account,
                "/var/lib/project-runner",
            )
            .expect("home command"),
            LaneCommand::ensure_subordinate_uids(&root, &account, 100_000, 65_536)
                .expect("subuid command"),
            LaneCommand::ensure_subordinate_gids(&root, &account, 200_000, 65_536)
                .expect("subgid command"),
        ] {
            validate_root_command(&command).expect("reviewed root command");
        }
    }

    #[test]
    fn root_executor_delivers_exact_empty_environment_and_retains_record() {
        let process = FakeProcess::successful();
        let expected_record = process.record.clone();
        let executables = FakeVerifier::default();
        let privilege = root_privilege();
        let command = LaneCommand::apt_install(
            &action("install-tools", ExecutionLane::Root),
            &[PackageName::parse("podman").expect("package")],
        )
        .expect("apt command");

        let record = execute_root_lane(&process, &executables, &privilege, &command)
            .expect("execute root lane");
        assert_eq!(record.action_id(), "install-tools");
        assert_eq!(record.lane(), ExecutionLane::Root);
        assert!(record.success());
        assert_eq!(record.status(), Some(0));
        assert_eq!(record.process(), &expected_record);
        let calls = process.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].environment.is_empty());
        assert_eq!(
            calls[0].displayed_argv(),
            [
                "/usr/bin/apt-get",
                "install",
                "--yes",
                "--no-install-recommends",
                "podman",
            ]
        );
    }

    #[test]
    fn runner_executor_delivers_exact_sealed_environment_boundary() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let privilege = root_privilege();
        let context = runner_context("project-runner", 1001, 1001);
        let runner = runner_evidence("project-runner", 1001, 1001, "/srv/project-runner");
        let command = LaneCommand::runner_podman_migrate(
            &action("migrate-podman", ExecutionLane::RunnerUser),
            &context,
        )
        .expect("runner command");
        let environment_program = command
            .runner_environment_program()
            .expect("reviewed environment program")
            .to_str()
            .expect("fixed reviewed path");

        let record =
            execute_runner_user_lane(&process, &executables, &privilege, &command, &runner)
                .expect("execute runner-user lane");
        assert!(record.success());
        let calls = process.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].environment.is_empty());
        assert_eq!(
            calls[0].displayed_argv(),
            [
                "/usr/sbin/runuser",
                "--user",
                "project-runner",
                "--",
                environment_program,
                "--ignore-environment",
                "HOME=/srv/project-runner",
                "USER=project-runner",
                "LOGNAME=project-runner",
                "XDG_RUNTIME_DIR=/run/user/1001",
                PODMAN,
                "system",
                "migrate",
            ]
        );
    }

    #[test]
    fn lane_mismatch_fails_before_any_evidence_or_process_work() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let privilege = root_privilege();
        let context = runner_context("project-runner", 1001, 1001);
        let command = LaneCommand::runner_podman_info(
            &action("inspect-podman", ExecutionLane::RunnerUser),
            &context,
        )
        .expect("runner command");

        let error = execute_root_lane(&process, &executables, &privilege, &command)
            .expect_err("root executor must reject runner command");
        assert_eq!(error.kind(), LaneExecutionErrorKind::LaneMismatch);
        assert_eq!(privilege.calls.get(), 0);
        assert_eq!(executables.calls.get(), 0);
        assert!(process.calls.borrow().is_empty());
    }

    #[test]
    fn unsupported_privilege_fails_with_recovery_instruction_before_mutation() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let privilege = FakePrivilege {
            calls: Cell::new(0),
            uid: 1000,
            fail: false,
        };
        let command = LaneCommand::apt_install(
            &action("install-tools", ExecutionLane::Root),
            &[PackageName::parse("git").expect("package")],
        )
        .expect("apt command");

        let error = execute_root_lane(&process, &executables, &privilege, &command)
            .expect_err("non-root execution must fail");
        assert_eq!(error.kind(), LaneExecutionErrorKind::UnsupportedPrivilege);
        assert!(error.message().contains("explicit elevation"));
        assert_eq!(privilege.calls.get(), 1);
        assert_eq!(executables.calls.get(), 0);
        assert!(process.calls.borrow().is_empty());
    }

    #[test]
    fn runner_evidence_mismatch_fails_before_privilege_or_process_execution() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let privilege = root_privilege();
        let context = runner_context("project-runner", 1001, 1001);
        let runner = runner_evidence("other-runner", 1002, 1002, "/srv/other-runner");
        let command = LaneCommand::runner_podman_info(
            &action("inspect-podman", ExecutionLane::RunnerUser),
            &context,
        )
        .expect("runner command");

        let error = execute_runner_user_lane(&process, &executables, &privilege, &command, &runner)
            .expect_err("mismatched runner evidence must fail");
        assert_eq!(error.kind(), LaneExecutionErrorKind::InvalidCommand);
        assert_eq!(privilege.calls.get(), 0);
        assert_eq!(executables.calls.get(), 0);
        assert!(process.calls.borrow().is_empty());
    }

    #[test]
    fn invalid_runner_evidence_fails_before_privilege_or_process_execution() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let privilege = root_privilege();
        let context = runner_context("project-runner", 1001, 1001);
        let runner = runner_evidence("project-runner", 0, 1001, "/srv/project-runner");
        let command = LaneCommand::runner_git_version(
            &action("inspect-git", ExecutionLane::RunnerUser),
            &context,
        )
        .expect("runner command");

        let error = execute_runner_user_lane(&process, &executables, &privilege, &command, &runner)
            .expect_err("root runner identity must fail");
        assert_eq!(error.kind(), LaneExecutionErrorKind::InvalidRunnerEvidence);
        assert_eq!(privilege.calls.get(), 0);
        assert_eq!(executables.calls.get(), 0);
        assert!(process.calls.borrow().is_empty());
    }

    #[test]
    fn executable_verification_failure_stops_process_execution() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier {
            calls: Cell::new(0),
            fail: true,
        };
        let privilege = root_privilege();
        let command = LaneCommand::apt_install(
            &action("install-tools", ExecutionLane::Root),
            &[PackageName::parse("git").expect("package")],
        )
        .expect("apt command");

        let error = execute_root_lane(&process, &executables, &privilege, &command)
            .expect_err("untrusted executable must fail");
        assert_eq!(error.kind(), LaneExecutionErrorKind::ExecutableVerification);
        assert_eq!(executables.calls.get(), 1);
        assert!(process.calls.borrow().is_empty());
    }

    #[test]
    fn process_failure_is_bounded_and_conservative() {
        let mut process = FakeProcess::successful();
        process.failure = Some("private token-bearing process error".to_owned());
        let executables = FakeVerifier::default();
        let privilege = root_privilege();
        let command = LaneCommand::apt_install(
            &action("install-tools", ExecutionLane::Root),
            &[PackageName::parse("git").expect("package")],
        )
        .expect("apt command");

        let error = execute_root_lane(&process, &executables, &privilege, &command)
            .expect_err("process failure must be bounded");
        assert_eq!(error.kind(), LaneExecutionErrorKind::Process);
        assert!(!error.message().contains("private"));
        assert!(error.message().contains("may have changed"));
        assert_eq!(process.calls.borrow().len(), 1);
    }

    #[test]
    fn proc_status_parser_requires_one_canonical_effective_uid() {
        assert_eq!(
            parse_effective_uid("Name:\tsmolrunner\nUid:\t1000\t0\t1000\t1000\n")
                .expect("effective UID"),
            0
        );
        for input in [
            "Name:\tsmolrunner\n",
            "Uid:\t1000\t00\t1000\t1000\n",
            "Uid:\t1000\t0\t1000\n",
            "Uid:\t1000\t0\t1000\t1000\nUid:\t1000\t0\t1000\t1000\n",
        ] {
            parse_effective_uid(input).expect_err("malformed UID evidence must fail");
        }
    }

    #[test]
    fn runner_command_constants_remain_absolute_reviewed_paths() {
        assert_eq!(PODMAN, "/usr/bin/podman");
        assert_eq!(GIT, "/usr/bin/git");
    }
}
