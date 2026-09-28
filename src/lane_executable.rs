use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::lane_command::LaneCommand;

const CLASSIC_ENV_PATH: &str = "/usr/bin/env";
const UBUNTU_RUST_COREUTILS_ENV_PATH: &str = "/usr/lib/cargo/bin/coreutils/env";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableVerificationErrorKind {
    Missing,
    Symlink,
    NonRegularFile,
    WrongOwner,
    WritableByNonOwner,
    NotExecutable,
    Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutableVerificationError {
    kind: ExecutableVerificationErrorKind,
    path: PathBuf,
    public_message: String,
}

impl ExecutableVerificationError {
    #[must_use]
    pub fn kind(&self) -> ExecutableVerificationErrorKind {
        self.kind
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(kind: ExecutableVerificationErrorKind, path: &Path, message: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.to_path_buf(),
            public_message: message.into(),
        }
    }
}

impl fmt::Display for ExecutableVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for ExecutableVerificationError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedExecutable {
    path: PathBuf,
    mode: u32,
}

impl VerifiedExecutable {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn mode(&self) -> u32 {
        self.mode
    }
}

/// One purpose-bound, verified `env` executable selected from the closed supported layouts.
///
/// The selected path always names a real regular executable. The `/usr/bin/env` compatibility
/// alias is never followed after it is observed as a symlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedEnvironmentExecutable(VerifiedExecutable);

impl VerifiedEnvironmentExecutable {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

pub(crate) fn is_supported_environment_executable_path(path: &Path) -> bool {
    path == Path::new(CLASSIC_ENV_PATH) || path == Path::new(UBUNTU_RUST_COREUTILS_ENV_PATH)
}

/// Bounded, path-free failure to select a supported reviewed `env` executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentExecutableResolutionError;

impl fmt::Display for EnvironmentExecutableResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("no supported reviewed environment executable is available")
    }
}

impl std::error::Error for EnvironmentExecutableResolutionError {}

/// Select a verified regular `env` executable from the closed supported system layouts.
///
/// Classic distributions provide a regular `/usr/bin/env`. Ubuntu 26 may instead expose that path
/// as a symlink while providing the real Rust-coreutils applet at the second fixed path. Selection
/// verifies and returns the real leaf; it never follows or later executes the compatibility alias.
///
/// # Errors
///
/// Returns a path-free error unless one supported candidate is a root-owned, protected, executable
/// regular file. Unsafe metadata on the classic path fails closed rather than selecting around it.
pub fn resolve_reviewed_environment_executable()
-> Result<VerifiedEnvironmentExecutable, EnvironmentExecutableResolutionError> {
    resolve_reviewed_environment_executable_with(&verify_executable)
}

fn resolve_reviewed_environment_executable_with(
    verify: &impl Fn(&Path) -> Result<VerifiedExecutable, ExecutableVerificationError>,
) -> Result<VerifiedEnvironmentExecutable, EnvironmentExecutableResolutionError> {
    match verify(Path::new(CLASSIC_ENV_PATH)) {
        Ok(executable) => return Ok(VerifiedEnvironmentExecutable(executable)),
        Err(error)
            if matches!(
                error.kind(),
                ExecutableVerificationErrorKind::Missing | ExecutableVerificationErrorKind::Symlink
            ) => {}
        Err(_) => return Err(EnvironmentExecutableResolutionError),
    }

    verify(Path::new(UBUNTU_RUST_COREUTILS_ENV_PATH))
        .map(VerifiedEnvironmentExecutable)
        .map_err(|_| EnvironmentExecutableResolutionError)
}

/// Verify every executable required by one typed lane command.
///
/// # Errors
///
/// Returns the first bounded verification error when a required executable is missing, symlinked,
/// non-regular, not root-owned, writable by group or others, or lacks executable permission bits.
pub fn verify_lane_command(
    command: &LaneCommand,
) -> Result<Vec<VerifiedExecutable>, ExecutableVerificationError> {
    command
        .required_programs()
        .into_iter()
        .map(verify_lane_command_executable)
        .collect()
}

fn verify_lane_command_executable(
    path: &Path,
) -> Result<VerifiedExecutable, ExecutableVerificationError> {
    #[cfg(test)]
    if is_supported_environment_executable_path(path) {
        return Ok(VerifiedExecutable {
            path: path.to_path_buf(),
            mode: 0o755,
        });
    }
    verify_executable(path)
}

#[cfg(test)]
pub(crate) fn test_environment_executable() -> VerifiedEnvironmentExecutable {
    let path = match fs::symlink_metadata(CLASSIC_ENV_PATH) {
        Ok(metadata) if metadata.file_type().is_symlink() => UBUNTU_RUST_COREUTILS_ENV_PATH,
        _ => CLASSIC_ENV_PATH,
    };
    VerifiedEnvironmentExecutable(VerifiedExecutable {
        path: PathBuf::from(path),
        mode: 0o755,
    })
}

/// Verify one reviewed absolute executable path without following a final symlink.
///
/// # Errors
///
/// Returns a bounded verification error when metadata cannot be read or the executable fails the
/// root-owner, regular-file, write-bit, or execute-bit policy.
pub fn verify_executable(path: &Path) -> Result<VerifiedExecutable, ExecutableVerificationError> {
    if !path.is_absolute() {
        return Err(ExecutableVerificationError::new(
            ExecutableVerificationErrorKind::Metadata,
            path,
            "reviewed executable path is not absolute",
        ));
    }

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ExecutableVerificationError::new(
                ExecutableVerificationErrorKind::Missing,
                path,
                format!("reviewed executable does not exist: {}", path.display()),
            ));
        }
        Err(_) => {
            return Err(ExecutableVerificationError::new(
                ExecutableVerificationErrorKind::Metadata,
                path,
                format!("could not inspect reviewed executable: {}", path.display()),
            ));
        }
    };

    let object_kind = if metadata.file_type().is_symlink() {
        ObservedObjectKind::Symlink
    } else if metadata.is_file() {
        ObservedObjectKind::RegularFile
    } else {
        ObservedObjectKind::Other
    };
    verify_observation(path, object_kind, metadata.uid(), metadata.mode() & 0o7777)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedObjectKind {
    RegularFile,
    Symlink,
    Other,
}

fn verify_observation(
    path: &Path,
    object_kind: ObservedObjectKind,
    owner_uid: u32,
    mode: u32,
) -> Result<VerifiedExecutable, ExecutableVerificationError> {
    match object_kind {
        ObservedObjectKind::Symlink => {
            return Err(ExecutableVerificationError::new(
                ExecutableVerificationErrorKind::Symlink,
                path,
                format!("reviewed executable is a symlink: {}", path.display()),
            ));
        }
        ObservedObjectKind::Other => {
            return Err(ExecutableVerificationError::new(
                ExecutableVerificationErrorKind::NonRegularFile,
                path,
                format!(
                    "reviewed executable is not a regular file: {}",
                    path.display()
                ),
            ));
        }
        ObservedObjectKind::RegularFile => {}
    }
    if owner_uid != 0 {
        return Err(ExecutableVerificationError::new(
            ExecutableVerificationErrorKind::WrongOwner,
            path,
            format!(
                "reviewed executable is not owned by root: {}",
                path.display()
            ),
        ));
    }
    if mode & 0o022 != 0 {
        return Err(ExecutableVerificationError::new(
            ExecutableVerificationErrorKind::WritableByNonOwner,
            path,
            format!(
                "reviewed executable is writable by group or others: {}",
                path.display()
            ),
        ));
    }
    if mode & 0o111 == 0 {
        return Err(ExecutableVerificationError::new(
            ExecutableVerificationErrorKind::NotExecutable,
            path,
            format!("reviewed executable lacks execute bits: {}", path.display()),
        ));
    }
    Ok(VerifiedExecutable {
        path: path.to_path_buf(),
        mode,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Path;

    use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
    use crate::lane_command::{LaneCommand, LinuxAccountName, RunnerUserContext};

    use super::{
        CLASSIC_ENV_PATH, EnvironmentExecutableResolutionError, ExecutableVerificationErrorKind,
        ObservedObjectKind, UBUNTU_RUST_COREUTILS_ENV_PATH,
        resolve_reviewed_environment_executable, resolve_reviewed_environment_executable_with,
        verify_lane_command, verify_observation,
    };

    fn action() -> PlannedMutation {
        PlannedMutation::new(
            "inspect-runner-user",
            ExecutionLane::RunnerUser,
            "inspect runner-user tools",
            RollbackClass::Reversible,
            Preconditions::new(["runner user inspected"]),
        )
    }

    fn runner() -> RunnerUserContext {
        RunnerUserContext::new(
            LinuxAccountName::parse("project-runner").expect("runner name"),
            1001,
            1001,
            "/srv/runner",
        )
        .expect("runner context")
    }

    #[test]
    fn pure_evidence_accepts_only_root_owned_nonwritable_executables() {
        let verified = verify_observation(
            Path::new("/usr/bin/example"),
            ObservedObjectKind::RegularFile,
            0,
            0o755,
        )
        .expect("valid executable evidence");
        assert_eq!(verified.path(), Path::new("/usr/bin/example"));
        assert_eq!(verified.mode(), 0o755);

        for (kind, uid, mode, expected) in [
            (
                ObservedObjectKind::Symlink,
                0,
                0o755,
                ExecutableVerificationErrorKind::Symlink,
            ),
            (
                ObservedObjectKind::Other,
                0,
                0o755,
                ExecutableVerificationErrorKind::NonRegularFile,
            ),
            (
                ObservedObjectKind::RegularFile,
                1000,
                0o755,
                ExecutableVerificationErrorKind::WrongOwner,
            ),
            (
                ObservedObjectKind::RegularFile,
                0,
                0o775,
                ExecutableVerificationErrorKind::WritableByNonOwner,
            ),
            (
                ObservedObjectKind::RegularFile,
                0,
                0o644,
                ExecutableVerificationErrorKind::NotExecutable,
            ),
        ] {
            let error = verify_observation(Path::new("/usr/bin/example"), kind, uid, mode)
                .expect_err("invalid executable evidence");
            assert_eq!(error.kind(), expected);
        }
    }

    #[test]
    fn environment_selection_prefers_a_regular_classic_leaf() {
        let observed = RefCell::new(Vec::new());
        let selected = resolve_reviewed_environment_executable_with(&|path| {
            observed.borrow_mut().push(path.to_path_buf());
            verify_observation(path, ObservedObjectKind::RegularFile, 0, 0o755)
        })
        .expect("classic env selection");

        assert_eq!(selected.path(), Path::new(CLASSIC_ENV_PATH));
        let command = LaneCommand::runner_git_version_with_environment_program(
            &action(),
            &runner(),
            selected,
        );
        assert_eq!(command.spec().displayed_argv()[4], CLASSIC_ENV_PATH);
        assert_eq!(observed.into_inner(), [Path::new(CLASSIC_ENV_PATH)]);
    }

    #[test]
    fn environment_selection_uses_the_real_ubuntu_rust_coreutils_leaf() {
        let observed = RefCell::new(Vec::new());
        let selected = resolve_reviewed_environment_executable_with(&|path| {
            observed.borrow_mut().push(path.to_path_buf());
            let kind = if path == Path::new(CLASSIC_ENV_PATH) {
                ObservedObjectKind::Symlink
            } else {
                ObservedObjectKind::RegularFile
            };
            verify_observation(path, kind, 0, 0o755)
        })
        .expect("Rust-coreutils env selection");

        assert_eq!(selected.path(), Path::new(UBUNTU_RUST_COREUTILS_ENV_PATH));
        let command = LaneCommand::runner_git_version_with_environment_program(
            &action(),
            &runner(),
            selected,
        );
        assert_eq!(
            command.spec().displayed_argv()[4],
            UBUNTU_RUST_COREUTILS_ENV_PATH
        );
        assert_eq!(
            observed.into_inner(),
            [
                Path::new(CLASSIC_ENV_PATH),
                Path::new(UBUNTU_RUST_COREUTILS_ENV_PATH),
            ]
        );
    }

    #[test]
    fn environment_selection_does_not_bypass_unsafe_classic_metadata() {
        let observed = RefCell::new(Vec::new());
        let error = resolve_reviewed_environment_executable_with(&|path| {
            observed.borrow_mut().push(path.to_path_buf());
            verify_observation(path, ObservedObjectKind::RegularFile, 1000, 0o755)
        })
        .expect_err("unsafe classic env must fail closed");

        assert_eq!(error, EnvironmentExecutableResolutionError);
        assert_eq!(observed.into_inner(), [Path::new(CLASSIC_ENV_PATH)]);
        assert!(!error.to_string().contains('/'));
    }

    #[test]
    fn environment_selection_rejects_an_unsafe_rust_coreutils_leaf() {
        let error = resolve_reviewed_environment_executable_with(&|path| {
            if path == Path::new(CLASSIC_ENV_PATH) {
                verify_observation(path, ObservedObjectKind::Symlink, 0, 0o777)
            } else {
                verify_observation(path, ObservedObjectKind::RegularFile, 1000, 0o755)
            }
        })
        .expect_err("unsafe Rust-coreutils env must fail closed");

        assert_eq!(error, EnvironmentExecutableResolutionError);
        assert!(!error.to_string().contains('/'));
    }

    #[test]
    fn runner_git_command_verifies_outer_and_inner_reviewed_programs_when_present() {
        if !Path::new("/usr/sbin/runuser").exists() || !Path::new("/usr/bin/git").exists() {
            return;
        }
        if fs::symlink_metadata("/usr/sbin/runuser").map_or(true, |metadata| metadata.uid() != 0) {
            // An unprivileged single-ID user namespace reports host-root files
            // with the overflow UID. The injected metadata tests above retain
            // exact owner-policy coverage without treating that view as a host.
            return;
        }
        let runner = runner();
        let command = LaneCommand::runner_git_version(&action(), &runner).expect("git command");
        let verified = verify_lane_command(&command).expect("verify reviewed programs");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0].path(), Path::new("/usr/sbin/runuser"));
        assert_eq!(
            verified[1].path(),
            resolve_reviewed_environment_executable()
                .expect("reviewed env")
                .path()
        );
        assert_eq!(verified[2].path(), Path::new("/usr/bin/git"));
    }
}
