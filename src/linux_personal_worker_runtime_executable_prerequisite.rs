//! Descriptor-bound prerequisites for the fixed personal-worker runtime executables.
//!
//! This module observes only the eleven fixed top-level executable paths from the accepted R01
//! contract. It does not resolve the ELF interpreter or `DT_NEEDED` libraries, consult loader
//! configuration/cache state, inspect package-manager state, execute a command, construct a
//! runtime evidence class, or seal readiness.

use std::fmt;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Take};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path};

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use rustix::io::{Errno, fcntl_dupfd_cloexec};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::linux_elf_runtime_dependency::{
    LINUX_RUNTIME_ELF_MAX_BYTES, LinuxRuntimeDynamicSearchPolicy, LinuxRuntimeElfDependency,
    LinuxRuntimeElfLinkage, parse_linux_runtime_elf_dependency,
};
use crate::manifest::RunnerScope;
use crate::ownership::ProjectIdentity;
use crate::personal_worker_runtime_contract::PersonalWorkerRuntimeArchitecture;

pub const PERSONAL_WORKER_RUNTIME_EXECUTABLE_PREREQUISITE_SCHEMA_VERSION: u8 = 1;

const EXECUTABLE_COUNT: usize = 11;
const MAX_TOTAL_EXECUTABLE_BYTES: u64 = 268_435_456;
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const DOMAIN: &[u8] = b"smolrunner-personal-worker-runtime-executable-prerequisite-v1";
const REDACTED: &str = "<private-runtime-executable-prerequisite>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeExecutablePrerequisiteDisposition {
    ObservedPrerequisite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeExecutablePrerequisiteSummary {
    schema_version: u8,
    disposition: PersonalWorkerRuntimeExecutablePrerequisiteDisposition,
    executable_count: u8,
}

impl PersonalWorkerRuntimeExecutablePrerequisiteSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> PersonalWorkerRuntimeExecutablePrerequisiteDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn executable_count(self) -> u8 {
        self.executable_count
    }
}

/// Opaque current prerequisite for the future executable-closure evidence classes.
///
/// The retained descriptors and parsed dependencies have no public accessor, serialization,
/// cloning, digest, path, or readiness conversion surface.
pub struct PersonalWorkerRuntimeExecutablePrerequisite {
    summary: PersonalWorkerRuntimeExecutablePrerequisiteSummary,
    _identity: Sha256Digest,
    _sources: Vec<BoundExecutable>,
}

impl PersonalWorkerRuntimeExecutablePrerequisite {
    #[must_use]
    pub const fn summary(&self) -> PersonalWorkerRuntimeExecutablePrerequisiteSummary {
        self.summary
    }

    pub(crate) fn reconfirm(
        &mut self,
    ) -> Result<(), PersonalWorkerRuntimeExecutablePrerequisiteError> {
        for source in &self._sources {
            source.chain.revalidate()?;
        }
        for source in &mut self._sources {
            source.file.revalidate()?;
        }
        for source in &self._sources {
            source.chain.revalidate()?;
        }
        Ok(())
    }
}

impl fmt::Debug for PersonalWorkerRuntimeExecutablePrerequisite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeExecutablePrerequisite")
            .field("summary", &self.summary)
            .field("private_prerequisite", &REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeExecutablePrerequisiteErrorKind {
    IdentityMismatch,
    Missing,
    UnsupportedArchitecture,
    UnsafeFilesystem,
    InvalidExecutable,
    ChangedDuringRead,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeExecutablePrerequisiteError {
    pub kind: PersonalWorkerRuntimeExecutablePrerequisiteErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for PersonalWorkerRuntimeExecutablePrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeExecutablePrerequisiteError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerRuntimeExecutablePrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerRuntimeExecutablePrerequisiteError {}

/// Observe the fixed top-level runtime executables without invoking a child process.
///
/// # Errors
///
/// Fails closed unless the running architecture matches, all fixed paths are root-owned exact-mode
/// single-link regular files beneath protected root-owned directories, every bounded ELF parses
/// with its path-specific search policy, the aggregate file bytes are bounded, and every held and
/// pathname object remains unchanged through final revalidation.
pub fn observe_personal_worker_runtime_executable_prerequisite(
    project: &ProjectIdentity,
    architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<
    PersonalWorkerRuntimeExecutablePrerequisite,
    PersonalWorkerRuntimeExecutablePrerequisiteError,
> {
    observe_at(Path::new("/"), (0, 0), project, architecture, || {})
}

fn observe_at<F>(
    root_path: &Path,
    authority_owner: (u32, u32),
    project: &ProjectIdentity,
    architecture: PersonalWorkerRuntimeArchitecture,
    before_revalidation: F,
) -> Result<
    PersonalWorkerRuntimeExecutablePrerequisite,
    PersonalWorkerRuntimeExecutablePrerequisiteError,
>
where
    F: FnOnce(),
{
    if host_architecture().ok_or_else(unsupported_error)? != architecture {
        return Err(identity_error());
    }

    let mut sources = Vec::with_capacity(EXECUTABLE_COUNT);
    let mut total_bytes = 0_u64;
    for kind in ExecutableKind::ALL {
        let (parent, name) = kind.path_components();
        let chain = DirectoryChain::open(root_path, parent, authority_owner)?;
        let file =
            BoundExecutableFile::open(chain.leaf(), name, authority_owner, kind.expected_mode())?;
        total_bytes = total_bytes
            .checked_add(file.size())
            .filter(|total| *total <= MAX_TOTAL_EXECUTABLE_BYTES)
            .ok_or_else(invalid_error)?;
        sources.push(BoundExecutable { kind, chain, file });
    }

    for source in &mut sources {
        source.load(architecture)?;
    }
    before_revalidation();
    for source in &sources {
        source.chain.revalidate()?;
    }
    for source in &mut sources {
        source.file.revalidate()?;
    }
    for source in &sources {
        source.chain.revalidate()?;
    }

    let identity = derive_identity(project, architecture, &sources)?;
    Ok(PersonalWorkerRuntimeExecutablePrerequisite {
        summary: PersonalWorkerRuntimeExecutablePrerequisiteSummary {
            schema_version: PERSONAL_WORKER_RUNTIME_EXECUTABLE_PREREQUISITE_SCHEMA_VERSION,
            disposition:
                PersonalWorkerRuntimeExecutablePrerequisiteDisposition::ObservedPrerequisite,
            executable_count: EXECUTABLE_COUNT as u8,
        },
        _identity: identity,
        _sources: sources,
    })
}

#[derive(Clone, Copy)]
enum ExecutableKind {
    Podman,
    Git,
    Runuser,
    Env,
    Systemctl,
    SystemdRun,
    Crun,
    Conmon,
    Catatonit,
    Newuidmap,
    Newgidmap,
}

impl ExecutableKind {
    const ALL: [Self; EXECUTABLE_COUNT] = [
        Self::Podman,
        Self::Git,
        Self::Runuser,
        Self::Env,
        Self::Systemctl,
        Self::SystemdRun,
        Self::Crun,
        Self::Conmon,
        Self::Catatonit,
        Self::Newuidmap,
        Self::Newgidmap,
    ];

    const fn tag(self) -> u8 {
        match self {
            Self::Podman => 1,
            Self::Git => 2,
            Self::Runuser => 3,
            Self::Env => 4,
            Self::Systemctl => 5,
            Self::SystemdRun => 6,
            Self::Crun => 7,
            Self::Conmon => 8,
            Self::Catatonit => 9,
            Self::Newuidmap => 10,
            Self::Newgidmap => 11,
        }
    }

    const fn path(self) -> &'static str {
        match self {
            Self::Podman => "/usr/bin/podman",
            Self::Git => "/usr/bin/git",
            Self::Runuser => "/usr/sbin/runuser",
            Self::Env => "/usr/bin/env",
            Self::Systemctl => "/usr/bin/systemctl",
            Self::SystemdRun => "/usr/bin/systemd-run",
            Self::Crun => "/usr/bin/crun",
            Self::Conmon => "/usr/bin/conmon",
            Self::Catatonit => "/usr/bin/catatonit",
            Self::Newuidmap => "/usr/bin/newuidmap",
            Self::Newgidmap => "/usr/bin/newgidmap",
        }
    }

    fn path_components(self) -> (&'static str, &'static str) {
        let (parent, name) = self.path().rsplit_once('/').expect("fixed absolute path");
        (
            parent
                .strip_prefix('/')
                .expect("fixed root-relative parent"),
            name,
        )
    }

    const fn expected_mode(self) -> u32 {
        match self {
            Self::Newuidmap | Self::Newgidmap => 0o4755,
            _ => 0o0755,
        }
    }

    const fn expected_linkage(self) -> LinuxRuntimeElfLinkage {
        match self {
            Self::Catatonit => LinuxRuntimeElfLinkage::Static,
            _ => LinuxRuntimeElfLinkage::Dynamic,
        }
    }

    const fn expected_search(self) -> Option<LinuxRuntimeDynamicSearchPolicy> {
        match self {
            Self::Catatonit => None,
            Self::Systemctl | Self::SystemdRun => {
                Some(LinuxRuntimeDynamicSearchPolicy::SystemdPrivate)
            }
            _ => Some(LinuxRuntimeDynamicSearchPolicy::Default),
        }
    }
}

struct BoundExecutable {
    kind: ExecutableKind,
    chain: DirectoryChain,
    file: BoundExecutableFile,
}

impl BoundExecutable {
    fn load(
        &mut self,
        architecture: PersonalWorkerRuntimeArchitecture,
    ) -> Result<(), PersonalWorkerRuntimeExecutablePrerequisiteError> {
        self.file.load(architecture, self.kind)
    }

    fn semantic_digest(
        &self,
    ) -> Result<[u8; 32], PersonalWorkerRuntimeExecutablePrerequisiteError> {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, &[self.kind.tag()]);
        hash_field(&mut hasher, self.kind.path().as_bytes());
        self.chain.hash_semantics(&mut hasher);
        self.file.hash_semantics(&mut hasher)?;
        Ok(hasher.finalize().into())
    }
}

struct BoundExecutableFile {
    file: File,
    parent: OwnedFd,
    name: String,
    snapshot: rustix::fs::Stat,
    content_digest: Option<[u8; 32]>,
    dependency_digest: Option<[u8; 32]>,
}

impl BoundExecutableFile {
    fn open(
        parent: BorrowedFd<'_>,
        name: &str,
        owner: (u32, u32),
        expected_mode: u32,
    ) -> Result<Self, PersonalWorkerRuntimeExecutablePrerequisiteError> {
        let parent = fcntl_dupfd_cloexec(parent, 0).map_err(|_| io_error())?;
        let fd = fs::openat(&parent, name, FILE_FLAGS, Mode::empty()).map_err(map_open)?;
        let file = File::from(fd);
        let snapshot = fs::fstat(&file).map_err(|_| io_error())?;
        inspect_executable(&snapshot, owner, expected_mode)?;
        Ok(Self {
            file,
            parent,
            name: name.to_owned(),
            snapshot,
            content_digest: None,
            dependency_digest: None,
        })
    }

    fn size(&self) -> u64 {
        self.snapshot.st_size as u64
    }

    fn load(
        &mut self,
        architecture: PersonalWorkerRuntimeArchitecture,
        kind: ExecutableKind,
    ) -> Result<(), PersonalWorkerRuntimeExecutablePrerequisiteError> {
        let first = read_bounded(&mut self.file)?;
        let dependency = parse_linux_runtime_elf_dependency(&first, architecture)
            .map_err(|_| invalid_error())?;
        validate_dependency(&dependency, kind)?;
        let content_digest: [u8; 32] = Sha256::digest(&first).into();
        let dependency_digest = dependency_digest(&dependency);
        self.file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
        let second = read_bounded(&mut self.file)?;
        let after = fs::fstat(&self.file).map_err(|_| io_error())?;
        if first != second || !same_snapshot(&self.snapshot, &after) {
            return Err(changed_error());
        }
        self.content_digest = Some(content_digest);
        self.dependency_digest = Some(dependency_digest);
        Ok(())
    }

    fn revalidate(&mut self) -> Result<(), PersonalWorkerRuntimeExecutablePrerequisiteError> {
        let content_digest = self.content_digest.ok_or_else(invalid_error)?;
        let before = fs::fstat(&self.file).map_err(|_| changed_error())?;
        if !same_snapshot(&self.snapshot, &before) {
            return Err(changed_error());
        }
        for _ in 0..2 {
            self.file
                .seek(SeekFrom::Start(0))
                .map_err(|_| changed_error())?;
            let bytes = read_bounded(&mut self.file).map_err(|_| changed_error())?;
            if <[u8; 32]>::from(Sha256::digest(&bytes)) != content_digest {
                return Err(changed_error());
            }
        }
        let after = fs::fstat(&self.file).map_err(|_| changed_error())?;
        let path = fs::statat(&self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| changed_error())?;
        if !same_snapshot(&self.snapshot, &after) || !same_snapshot(&self.snapshot, &path) {
            return Err(changed_error());
        }
        Ok(())
    }

    fn hash_semantics(
        &self,
        hasher: &mut Sha256,
    ) -> Result<(), PersonalWorkerRuntimeExecutablePrerequisiteError> {
        for value in [
            self.snapshot.st_uid,
            self.snapshot.st_gid,
            self.snapshot.st_mode,
        ] {
            hash_field(hasher, &value.to_be_bytes());
        }
        hash_field(hasher, &(self.snapshot.st_size as u64).to_be_bytes());
        hash_field(hasher, &self.content_digest.ok_or_else(invalid_error)?);
        hash_field(hasher, &self.dependency_digest.ok_or_else(invalid_error)?);
        Ok(())
    }
}

struct DirectoryNode {
    fd: OwnedFd,
    name: Option<String>,
    snapshot: rustix::fs::Stat,
}

struct DirectoryChain {
    root_path: std::path::PathBuf,
    nodes: Vec<DirectoryNode>,
}

impl DirectoryChain {
    fn open(
        root_path: &Path,
        relative: &str,
        owner: (u32, u32),
    ) -> Result<Self, PersonalWorkerRuntimeExecutablePrerequisiteError> {
        let root = fs::open(root_path, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
        let root_stat = fs::fstat(&root).map_err(|_| io_error())?;
        inspect_directory(&root_stat, owner)?;
        let mut nodes = vec![DirectoryNode {
            fd: root,
            name: None,
            snapshot: root_stat,
        }];
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(identity_error());
        }
        for component in path.components() {
            let name = component.as_os_str().to_str().ok_or_else(identity_error)?;
            let parent = nodes.last().expect("root node").fd.as_fd();
            let fd = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
            let snapshot = fs::fstat(&fd).map_err(|_| io_error())?;
            inspect_directory(&snapshot, owner)?;
            nodes.push(DirectoryNode {
                fd,
                name: Some(name.to_owned()),
                snapshot,
            });
        }
        Ok(Self {
            root_path: root_path.to_owned(),
            nodes,
        })
    }

    fn leaf(&self) -> BorrowedFd<'_> {
        self.nodes.last().expect("directory chain").fd.as_fd()
    }

    fn revalidate(&self) -> Result<(), PersonalWorkerRuntimeExecutablePrerequisiteError> {
        for (index, node) in self.nodes.iter().enumerate() {
            let held = fs::fstat(&node.fd).map_err(|_| changed_error())?;
            if !same_snapshot(&node.snapshot, &held) {
                return Err(changed_error());
            }
            if index > 0 {
                let path = fs::statat(
                    &self.nodes[index - 1].fd,
                    node.name.as_deref().expect("child name"),
                    AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(|_| changed_error())?;
                if !same_snapshot(&node.snapshot, &path) {
                    return Err(changed_error());
                }
            }
        }
        let rebound = fs::open(&self.root_path, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| changed_error())?;
        let rebound = fs::fstat(&rebound).map_err(|_| changed_error())?;
        if !same_snapshot(&self.nodes[0].snapshot, &rebound) {
            return Err(changed_error());
        }
        Ok(())
    }

    fn hash_semantics(&self, hasher: &mut Sha256) {
        for node in &self.nodes {
            hash_field(hasher, node.name.as_deref().unwrap_or("/").as_bytes());
            for value in [
                node.snapshot.st_uid,
                node.snapshot.st_gid,
                node.snapshot.st_mode,
            ] {
                hash_field(hasher, &value.to_be_bytes());
            }
        }
    }
}

fn validate_dependency(
    dependency: &LinuxRuntimeElfDependency,
    kind: ExecutableKind,
) -> Result<(), PersonalWorkerRuntimeExecutablePrerequisiteError> {
    if dependency.linkage() != kind.expected_linkage()
        || dependency.dynamic_search() != kind.expected_search()
        || match kind.expected_linkage() {
            LinuxRuntimeElfLinkage::Static => {
                dependency.loader().is_some() || !dependency.needed_libraries().is_empty()
            }
            LinuxRuntimeElfLinkage::Dynamic => {
                dependency.loader().is_none() || dependency.needed_libraries().is_empty()
            }
        }
    {
        return Err(invalid_error());
    }
    Ok(())
}

fn dependency_digest(dependency: &LinuxRuntimeElfDependency) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"elf_dependency");
    hash_field(&mut hasher, &[dependency.architecture() as u8]);
    hash_field(
        &mut hasher,
        &[match dependency.linkage() {
            LinuxRuntimeElfLinkage::Static => 1,
            LinuxRuntimeElfLinkage::Dynamic => 2,
        }],
    );
    hash_field(
        &mut hasher,
        &[match dependency.dynamic_search() {
            None => 0,
            Some(LinuxRuntimeDynamicSearchPolicy::Default) => 1,
            Some(LinuxRuntimeDynamicSearchPolicy::SystemdPrivate) => 2,
        }],
    );
    for library in dependency.needed_libraries() {
        hash_field(&mut hasher, library.as_bytes());
    }
    hasher.finalize().into()
}

fn derive_identity(
    project: &ProjectIdentity,
    architecture: PersonalWorkerRuntimeArchitecture,
    sources: &[BoundExecutable],
) -> Result<Sha256Digest, PersonalWorkerRuntimeExecutablePrerequisiteError> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, DOMAIN);
    hash_field(&mut hasher, project.repository.as_bytes());
    hash_field(
        &mut hasher,
        &[match project.runner_scope {
            RunnerScope::Repository => 1,
            RunnerScope::Organization => 2,
        }],
    );
    hash_field(&mut hasher, project.runner_user.as_bytes());
    hash_field(&mut hasher, &[architecture as u8]);
    for source in sources {
        hash_field(&mut hasher, &source.semantic_digest()?);
    }
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| invalid_error())
}

fn inspect_executable(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
    expected_mode: u32,
) -> Result<(), PersonalWorkerRuntimeExecutablePrerequisiteError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o7777 != expected_mode
    {
        return Err(unsafe_error());
    }
    if stat.st_size <= 0 || stat.st_size as u64 > LINUX_RUNTIME_ELF_MAX_BYTES as u64 {
        return Err(invalid_error());
    }
    Ok(())
}

fn inspect_directory(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeExecutablePrerequisiteError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o022 != 0
    {
        return Err(unsafe_error());
    }
    Ok(())
}

fn read_bounded(
    file: &mut File,
) -> Result<Vec<u8>, PersonalWorkerRuntimeExecutablePrerequisiteError> {
    let mut reader: Take<&mut File> = file.take((LINUX_RUNTIME_ELF_MAX_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|_| io_error())?;
    if bytes.len() > LINUX_RUNTIME_ELF_MAX_BYTES {
        return Err(invalid_error());
    }
    Ok(bytes)
}

fn same_snapshot(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn host_architecture() -> Option<PersonalWorkerRuntimeArchitecture> {
    match std::env::consts::ARCH {
        "aarch64" => Some(PersonalWorkerRuntimeArchitecture::Aarch64),
        "x86_64" => Some(PersonalWorkerRuntimeArchitecture::X86_64),
        _ => None,
    }
}

const fn error(
    kind: PersonalWorkerRuntimeExecutablePrerequisiteErrorKind,
    code: &'static str,
    message: &'static str,
) -> PersonalWorkerRuntimeExecutablePrerequisiteError {
    PersonalWorkerRuntimeExecutablePrerequisiteError {
        kind,
        code,
        message,
    }
}

const fn identity_error() -> PersonalWorkerRuntimeExecutablePrerequisiteError {
    error(
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::IdentityMismatch,
        "runtime_executable_identity_mismatch",
        "runtime executable prerequisite does not match the requested identity",
    )
}

const fn missing_error() -> PersonalWorkerRuntimeExecutablePrerequisiteError {
    error(
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::Missing,
        "runtime_executable_missing",
        "a required runtime executable prerequisite is missing",
    )
}

const fn unsupported_error() -> PersonalWorkerRuntimeExecutablePrerequisiteError {
    error(
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::UnsupportedArchitecture,
        "runtime_executable_architecture_unsupported",
        "runtime executable observation does not support this architecture",
    )
}

const fn unsafe_error() -> PersonalWorkerRuntimeExecutablePrerequisiteError {
    error(
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::UnsafeFilesystem,
        "runtime_executable_filesystem_unsafe",
        "runtime executable prerequisite filesystem evidence is unsafe",
    )
}

const fn invalid_error() -> PersonalWorkerRuntimeExecutablePrerequisiteError {
    error(
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::InvalidExecutable,
        "runtime_executable_invalid",
        "runtime executable prerequisite is invalid or outside canonical bounds",
    )
}

const fn changed_error() -> PersonalWorkerRuntimeExecutablePrerequisiteError {
    error(
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::ChangedDuringRead,
        "runtime_executable_changed",
        "runtime executable prerequisite changed during observation",
    )
}

const fn io_error() -> PersonalWorkerRuntimeExecutablePrerequisiteError {
    error(
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::Io,
        "runtime_executable_io",
        "runtime executable prerequisite could not be read safely",
    )
}

fn map_open(error: Errno) -> PersonalWorkerRuntimeExecutablePrerequisiteError {
    match error {
        Errno::NOENT => missing_error(),
        Errno::LOOP | Errno::NOTDIR => unsafe_error(),
        _ => io_error(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs as stdfs;
    use std::io::ErrorKind;
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn disposable_noble_executables_are_descriptor_bound_prerequisites() {
        if std::env::var("GLAEDA_DISPOSABLE_PROBE").as_deref() != Ok("1") {
            return;
        }
        let architecture = host_architecture().expect("supported hosted architecture");
        let project = project();
        for kind in ExecutableKind::ALL {
            let (parent, name) = kind.path_components();
            let chain = DirectoryChain::open(Path::new("/"), parent, (0, 0))
                .unwrap_or_else(|error| panic!("open executable kind {}: {error:?}", kind.tag()));
            let mut file =
                BoundExecutableFile::open(chain.leaf(), name, (0, 0), kind.expected_mode())
                    .unwrap_or_else(|error| {
                        panic!("open executable file kind {}: {error:?}", kind.tag())
                    });
            file.load(architecture, kind)
                .unwrap_or_else(|error| panic!("load executable kind {}: {error:?}", kind.tag()));
        }
        let live = observe_personal_worker_runtime_executable_prerequisite(&project, architecture)
            .expect("observe live package executables");
        assert_eq!(live.summary().executable_count(), EXECUTABLE_COUNT as u8);

        let fixture = Fixture::new();
        let owner = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        fixture.populate();
        let mut observed = observe_at(&fixture.root, owner, &project, architecture, || {})
            .expect("observe copied executable fixture");
        let debug = format!("{observed:?}");
        assert!(debug.contains(REDACTED));
        assert!(!debug.contains("/usr/"));
        assert!(!debug.contains("sha256:"));
        assert_eq!(
            observed.summary().disposition(),
            PersonalWorkerRuntimeExecutablePrerequisiteDisposition::ObservedPrerequisite
        );
        observed.reconfirm().expect("reconfirm stable executables");
        fixture.set_mode(ExecutableKind::Podman, 0o0775);
        let reconfirmed = observed
            .reconfirm()
            .expect_err("post-observation executable drift");
        assert_eq!(
            reconfirmed.kind,
            PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::ChangedDuringRead
        );
        fixture.restore_mode(ExecutableKind::Podman);

        let changed = observe_at(&fixture.root, owner, &project, architecture, || {
            let path = fixture.root.join("usr/bin/podman");
            stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o0775))
                .expect("change observed executable mode");
        })
        .expect_err("mid-observation executable drift");
        assert_eq!(
            changed.kind,
            PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::ChangedDuringRead
        );
        fixture.restore_mode(ExecutableKind::Podman);

        fixture.set_mode(ExecutableKind::Podman, 0o0775);
        let unsafe_file = observe_at(&fixture.root, owner, &project, architecture, || {})
            .expect_err("group-writable executable");
        assert_eq!(
            unsafe_file.kind,
            PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::UnsafeFilesystem
        );
        fixture.restore_mode(ExecutableKind::Podman);

        let podman = fixture.path(ExecutableKind::Podman);
        let extra_link = fixture.root.join("usr/bin/podman-extra-link");
        stdfs::hard_link(&podman, &extra_link).expect("create executable hard link");
        let hardlinked = observe_at(&fixture.root, owner, &project, architecture, || {})
            .expect_err("hardlinked executable");
        assert_eq!(
            hardlinked.kind,
            PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::UnsafeFilesystem
        );
        stdfs::remove_file(extra_link).expect("remove executable hard link");

        stdfs::remove_file(&podman).expect("remove fixture executable");
        symlink("git", &podman).expect("replace executable with symlink");
        let symlinked = observe_at(&fixture.root, owner, &project, architecture, || {})
            .expect_err("symlinked executable");
        assert_eq!(
            symlinked.kind,
            PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::UnsafeFilesystem
        );

        let opposite = match architecture {
            PersonalWorkerRuntimeArchitecture::Aarch64 => PersonalWorkerRuntimeArchitecture::X86_64,
            PersonalWorkerRuntimeArchitecture::X86_64 => PersonalWorkerRuntimeArchitecture::Aarch64,
        };
        let mismatch = observe_at(
            Path::new("/definitely/missing/runtime-root"),
            owner,
            &project,
            opposite,
            || {},
        )
        .expect_err("architecture mismatch precedes filesystem access");
        assert_eq!(
            mismatch.kind,
            PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::IdentityMismatch
        );
    }

    fn project() -> ProjectIdentity {
        ProjectIdentity {
            repository: "example/runtime".to_owned(),
            runner_scope: RunnerScope::Repository,
            runner_user: "runtime-runner".to_owned(),
        }
    }

    struct Fixture {
        parent: std::path::PathBuf,
        parent_identity: (u64, u64),
        root: std::path::PathBuf,
        root_identity: (u64, u64),
    }

    impl Fixture {
        fn new() -> Self {
            let parent = std::env::current_dir()
                .expect("current directory")
                .join("target/r01-executable-prerequisite-fixtures");
            stdfs::create_dir_all(&parent).expect("create executable fixture parent");
            let parent_metadata = stdfs::symlink_metadata(&parent).expect("inspect fixture parent");
            assert!(
                parent_metadata.file_type().is_dir() && !parent_metadata.file_type().is_symlink(),
                "fixture parent must be a real directory"
            );
            let parent_identity = (parent_metadata.dev(), parent_metadata.ino());

            for _ in 0..128 {
                let nonce = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
                let root = parent.join(format!("{}-{nonce}", std::process::id()));
                let mut builder = stdfs::DirBuilder::new();
                builder.mode(0o0700);
                match builder.create(&root) {
                    Ok(()) => {
                        let root_metadata = stdfs::symlink_metadata(&root)
                            .expect("inspect created executable fixture root");
                        let fixture = Self {
                            parent: parent.clone(),
                            parent_identity,
                            root,
                            root_identity: (root_metadata.dev(), root_metadata.ino()),
                        };
                        stdfs::create_dir_all(fixture.root.join("usr/bin"))
                            .expect("create bin fixture");
                        stdfs::create_dir_all(fixture.root.join("usr/sbin"))
                            .expect("create sbin fixture");
                        return fixture;
                    }
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create private executable fixture root: {error}"),
                }
            }
            panic!("allocate unique executable fixture root")
        }

        fn populate(&self) {
            for kind in ExecutableKind::ALL {
                let destination = self.root.join(kind.path().trim_start_matches('/'));
                stdfs::copy(kind.path(), &destination).expect("copy package executable");
                self.restore_mode(kind);
            }
        }

        fn restore_mode(&self, kind: ExecutableKind) {
            self.set_mode(kind, kind.expected_mode());
        }

        fn set_mode(&self, kind: ExecutableKind, mode: u32) {
            let path = self.path(kind);
            stdfs::set_permissions(path, stdfs::Permissions::from_mode(mode))
                .expect("set fixture executable mode");
        }

        fn path(&self, kind: ExecutableKind) -> std::path::PathBuf {
            self.root.join(kind.path().trim_start_matches('/'))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            assert_eq!(self.root.parent(), Some(self.parent.as_path()));
            let parent_metadata = stdfs::symlink_metadata(&self.parent)
                .expect("revalidate executable fixture parent");
            assert_eq!(
                (parent_metadata.dev(), parent_metadata.ino()),
                self.parent_identity,
                "fixture parent was rebound"
            );
            let root_metadata =
                stdfs::symlink_metadata(&self.root).expect("revalidate executable fixture root");
            assert!(
                root_metadata.file_type().is_dir() && !root_metadata.file_type().is_symlink(),
                "fixture root must remain a real directory"
            );
            assert_eq!(
                (root_metadata.dev(), root_metadata.ino()),
                self.root_identity,
                "fixture root was rebound"
            );
            stdfs::remove_dir_all(&self.root).expect("remove owned executable fixture");
            if let Err(error) = stdfs::remove_dir(&self.parent)
                && !matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                )
            {
                panic!("remove empty executable fixture parent: {error}");
            }
        }
    }

    #[test]
    fn fixed_paths_and_modes_match_the_accepted_contract() {
        let paths = ExecutableKind::ALL.map(ExecutableKind::path);
        assert_eq!(paths.len(), EXECUTABLE_COUNT);
        assert_eq!(paths[0], "/usr/bin/podman");
        assert_eq!(paths[2], "/usr/sbin/runuser");
        assert_eq!(paths[10], "/usr/bin/newgidmap");
        assert_eq!(ExecutableKind::Newuidmap.expected_mode(), 0o4755);
        assert_eq!(ExecutableKind::Newgidmap.expected_mode(), 0o4755);
        assert_eq!(
            ExecutableKind::Catatonit.expected_linkage(),
            LinuxRuntimeElfLinkage::Static
        );
        assert_eq!(ExecutableKind::Catatonit.expected_search(), None);
        assert!(
            ExecutableKind::ALL[..9]
                .iter()
                .all(|kind| kind.expected_mode() == 0o0755)
        );
    }

    #[test]
    fn public_errors_and_debug_are_path_free() {
        for value in [
            identity_error(),
            missing_error(),
            unsupported_error(),
            unsafe_error(),
            invalid_error(),
            changed_error(),
            io_error(),
        ] {
            let debug = format!("{value:?}");
            let json = serde_json::to_string(&value).expect("serialize public error");
            assert!(!debug.contains("/usr/"));
            assert!(!json.contains("/usr/"));
            assert!(!debug.contains("runtime-runner"));
        }
    }

    #[test]
    fn source_package_modes_are_the_expected_noble_modes() {
        if std::env::var("GLAEDA_DISPOSABLE_PROBE").as_deref() != Ok("1") {
            return;
        }
        for kind in ExecutableKind::ALL {
            let metadata =
                stdfs::symlink_metadata(kind.path()).expect("package executable metadata");
            assert_eq!(metadata.uid(), 0);
            assert_eq!(metadata.gid(), 0);
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(metadata.mode() & 0o7777, kind.expected_mode());
        }
    }
}
