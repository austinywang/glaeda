//! Descriptor-bound prerequisite for the fixed personal-worker GNU dynamic-loader object.
//!
//! This module observes only the one architecture-specific loader selected by the admitted
//! top-level ELF objects. It follows the reviewed usr-merge symlink route with root-confined
//! `openat2`, binds that route to a separately no-follow-opened canonical target, and retains the
//! target descriptor through final revalidation. It does not resolve `DT_NEEDED` libraries,
//! inspect package-manager state, execute a command, construct a runtime evidence class, or seal
//! readiness.

use std::fmt;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Take};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path};

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::{Errno, fcntl_dupfd_cloexec};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::linux_elf_runtime_dependency::{
    LINUX_RUNTIME_ELF_MAX_BYTES, LinuxRuntimeDynamicLoader, LinuxRuntimeLoaderObject,
    parse_linux_runtime_loader_object,
};
use crate::manifest::RunnerScope;
use crate::ownership::ProjectIdentity;
use crate::personal_worker_runtime_contract::PersonalWorkerRuntimeArchitecture;

pub const PERSONAL_WORKER_RUNTIME_LOADER_OBJECT_PREREQUISITE_SCHEMA_VERSION: u8 = 1;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const RESOLVED_DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const RESOLVED_FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const RESOLVE_FLAGS: ResolveFlags = ResolveFlags::IN_ROOT.union(ResolveFlags::NO_MAGICLINKS);
const DOMAIN: &[u8] = b"smolrunner-personal-worker-runtime-loader-object-prerequisite-v1";
const REDACTED: &str = "<private-runtime-loader-object-prerequisite>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeLoaderObjectPrerequisiteDisposition {
    ObservedPrerequisite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeLoaderObjectPrerequisiteSummary {
    schema_version: u8,
    disposition: PersonalWorkerRuntimeLoaderObjectPrerequisiteDisposition,
    loader_count: u8,
}

impl PersonalWorkerRuntimeLoaderObjectPrerequisiteSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> PersonalWorkerRuntimeLoaderObjectPrerequisiteDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn loader_count(self) -> u8 {
        self.loader_count
    }
}

/// Opaque current prerequisite for later interpreter and transitive-library evidence classes.
///
/// The retained descriptors, bytes digest, loader model, and private identity have no public
/// accessor, serialization, cloning, digest, path, or readiness conversion surface.
pub struct PersonalWorkerRuntimeLoaderObjectPrerequisite {
    summary: PersonalWorkerRuntimeLoaderObjectPrerequisiteSummary,
    _identity: Sha256Digest,
    _source: BoundLoaderObject,
}

impl PersonalWorkerRuntimeLoaderObjectPrerequisite {
    #[must_use]
    pub const fn summary(&self) -> PersonalWorkerRuntimeLoaderObjectPrerequisiteSummary {
        self.summary
    }

    pub(crate) fn reconfirm(
        &mut self,
    ) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        self._source.revalidate()
    }
}

impl fmt::Debug for PersonalWorkerRuntimeLoaderObjectPrerequisite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeLoaderObjectPrerequisite")
            .field("summary", &self.summary)
            .field("private_prerequisite", &REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind {
    IdentityMismatch,
    Missing,
    UnsupportedArchitecture,
    UnsafeFilesystem,
    InvalidLoader,
    ChangedDuringRead,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    pub kind: PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeLoaderObjectPrerequisiteError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerRuntimeLoaderObjectPrerequisiteError {}

/// Observe and retain the fixed architecture-specific GNU loader without executing a command.
///
/// # Errors
///
/// Fails closed unless the running architecture matches; the logical loader path is confined
/// beneath `/`, resolves through the exact reviewed usr-merge parent to the exact separately
/// opened canonical target; all directories and the target have protected root-owned metadata;
/// the bounded loader bytes parse; and all held/path/resolved objects remain unchanged through
/// final revalidation.
pub fn observe_personal_worker_runtime_loader_object_prerequisite(
    project: &ProjectIdentity,
    architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<
    PersonalWorkerRuntimeLoaderObjectPrerequisite,
    PersonalWorkerRuntimeLoaderObjectPrerequisiteError,
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
    PersonalWorkerRuntimeLoaderObjectPrerequisite,
    PersonalWorkerRuntimeLoaderObjectPrerequisiteError,
>
where
    F: FnOnce(),
{
    if host_architecture().ok_or_else(unsupported_error)? != architecture {
        return Err(identity_error());
    }
    let route = LoaderRoute::for_architecture(architecture);
    let mut source = BoundLoaderObject::open(root_path, authority_owner, route)?;
    source.load(architecture)?;
    before_revalidation();
    source.revalidate()?;
    let identity = derive_identity(project, architecture, &source)?;
    Ok(PersonalWorkerRuntimeLoaderObjectPrerequisite {
        summary: PersonalWorkerRuntimeLoaderObjectPrerequisiteSummary {
            schema_version: PERSONAL_WORKER_RUNTIME_LOADER_OBJECT_PREREQUISITE_SCHEMA_VERSION,
            disposition:
                PersonalWorkerRuntimeLoaderObjectPrerequisiteDisposition::ObservedPrerequisite,
            loader_count: 1,
        },
        _identity: identity,
        _source: source,
    })
}

#[derive(Clone, Copy)]
struct LoaderRoute {
    logical_parent: &'static str,
    logical_path: &'static str,
    logical_parent_target: &'static str,
    logical_loader_target: &'static str,
    canonical_source_parent: &'static str,
    canonical_target_parent: &'static str,
    canonical_target_name: &'static str,
    loader: LinuxRuntimeDynamicLoader,
}

impl LoaderRoute {
    const fn for_architecture(architecture: PersonalWorkerRuntimeArchitecture) -> Self {
        match architecture {
            PersonalWorkerRuntimeArchitecture::Aarch64 => Self {
                logical_parent: "lib",
                logical_path: "lib/ld-linux-aarch64.so.1",
                logical_parent_target: "usr/lib",
                logical_loader_target: "aarch64-linux-gnu/ld-linux-aarch64.so.1",
                canonical_source_parent: "usr/lib",
                canonical_target_parent: "usr/lib/aarch64-linux-gnu",
                canonical_target_name: "ld-linux-aarch64.so.1",
                loader: LinuxRuntimeDynamicLoader::Aarch64Gnu,
            },
            PersonalWorkerRuntimeArchitecture::X86_64 => Self {
                logical_parent: "lib64",
                logical_path: "lib64/ld-linux-x86-64.so.2",
                logical_parent_target: "usr/lib64",
                logical_loader_target: "../lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
                canonical_source_parent: "usr/lib64",
                canonical_target_parent: "usr/lib/x86_64-linux-gnu",
                canonical_target_name: "ld-linux-x86-64.so.2",
                loader: LinuxRuntimeDynamicLoader::X86_64Gnu,
            },
        }
    }
}

struct BoundLoaderObject {
    route: LoaderRoute,
    root: OwnedFd,
    root_path: std::path::PathBuf,
    root_snapshot: rustix::fs::Stat,
    source_chain: DirectoryChain,
    target_chain: DirectoryChain,
    logical_parent_link: BoundSymlink,
    logical_loader_link: BoundSymlink,
    file: BoundLoaderFile,
}

impl BoundLoaderObject {
    fn open(
        root_path: &Path,
        owner: (u32, u32),
        route: LoaderRoute,
    ) -> Result<Self, PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        let root = fs::open(root_path, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
        let root_snapshot = fs::fstat(&root).map_err(|_| io_error())?;
        inspect_directory(&root_snapshot, owner)?;
        let source_chain =
            DirectoryChain::open_from(root.as_fd(), route.canonical_source_parent, owner)?;
        let target_chain =
            DirectoryChain::open_from(root.as_fd(), route.canonical_target_parent, owner)?;
        let logical_parent_link = BoundSymlink::open(
            root.as_fd(),
            route.logical_parent,
            route.logical_parent_target,
            owner,
        )?;
        let logical_loader_name = Path::new(route.logical_path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(identity_error)?;
        let logical_loader_link = BoundSymlink::open(
            source_chain.leaf(),
            logical_loader_name,
            route.logical_loader_target,
            owner,
        )?;

        let resolved_parent = fs::openat2(
            &root,
            route.logical_parent,
            RESOLVED_DIRECTORY_FLAGS,
            Mode::empty(),
            RESOLVE_FLAGS,
        )
        .map_err(map_resolved_open)?;
        let resolved_parent_stat = fs::fstat(&resolved_parent).map_err(|_| io_error())?;
        if !same_object(&resolved_parent_stat, &source_chain.leaf_snapshot()) {
            return Err(unsafe_error());
        }

        let file = BoundLoaderFile::open(target_chain.leaf(), route.canonical_target_name, owner)?;
        let resolved_file = fs::openat2(
            &root,
            route.logical_path,
            RESOLVED_FILE_FLAGS,
            Mode::empty(),
            RESOLVE_FLAGS,
        )
        .map_err(map_resolved_open)?;
        let resolved_file_stat = fs::fstat(&resolved_file).map_err(|_| io_error())?;
        if !same_snapshot(&resolved_file_stat, &file.snapshot) {
            return Err(unsafe_error());
        }
        Ok(Self {
            route,
            root,
            root_path: root_path.to_owned(),
            root_snapshot,
            source_chain,
            target_chain,
            logical_parent_link,
            logical_loader_link,
            file,
        })
    }

    fn load(
        &mut self,
        architecture: PersonalWorkerRuntimeArchitecture,
    ) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        self.file.load(architecture, self.route.loader)
    }

    fn revalidate(&mut self) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        self.source_chain.revalidate()?;
        self.target_chain.revalidate()?;
        self.logical_parent_link.revalidate()?;
        self.logical_loader_link.revalidate()?;
        self.file.revalidate()?;
        self.revalidate_root()?;
        let resolved_parent = fs::openat2(
            &self.root,
            self.route.logical_parent,
            RESOLVED_DIRECTORY_FLAGS,
            Mode::empty(),
            RESOLVE_FLAGS,
        )
        .map_err(|_| changed_error())?;
        let resolved_parent = fs::fstat(&resolved_parent).map_err(|_| changed_error())?;
        if !same_object(&resolved_parent, &self.source_chain.leaf_snapshot()) {
            return Err(changed_error());
        }
        let resolved_file = fs::openat2(
            &self.root,
            self.route.logical_path,
            RESOLVED_FILE_FLAGS,
            Mode::empty(),
            RESOLVE_FLAGS,
        )
        .map_err(|_| changed_error())?;
        let resolved_file = fs::fstat(&resolved_file).map_err(|_| changed_error())?;
        if !same_snapshot(&resolved_file, &self.file.snapshot) {
            return Err(changed_error());
        }
        self.source_chain.revalidate()?;
        self.target_chain.revalidate()?;
        self.logical_parent_link.revalidate()?;
        self.logical_loader_link.revalidate()?;
        self.revalidate_root()
    }

    fn revalidate_root(&self) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        let held_root = fs::fstat(&self.root).map_err(|_| changed_error())?;
        let rebound_root = fs::open(&self.root_path, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| changed_error())?;
        let rebound_root = fs::fstat(&rebound_root).map_err(|_| changed_error())?;
        if !same_snapshot(&self.root_snapshot, &held_root)
            || !same_snapshot(&self.root_snapshot, &rebound_root)
        {
            return Err(changed_error());
        }
        Ok(())
    }

    fn semantic_digest(
        &self,
    ) -> Result<[u8; 32], PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, self.route.logical_path.as_bytes());
        hash_field(&mut hasher, self.route.logical_parent_target.as_bytes());
        hash_field(&mut hasher, self.route.logical_loader_target.as_bytes());
        hash_field(&mut hasher, self.route.canonical_source_parent.as_bytes());
        hash_field(&mut hasher, self.route.canonical_target_parent.as_bytes());
        hash_field(&mut hasher, self.route.canonical_target_name.as_bytes());
        self.source_chain.hash_semantics(&mut hasher);
        self.target_chain.hash_semantics(&mut hasher);
        self.file.hash_semantics(&mut hasher)?;
        Ok(hasher.finalize().into())
    }
}

struct BoundSymlink {
    parent: OwnedFd,
    name: String,
    target: &'static str,
    snapshot: rustix::fs::Stat,
}

impl BoundSymlink {
    fn open(
        parent: BorrowedFd<'_>,
        name: &str,
        target: &'static str,
        owner: (u32, u32),
    ) -> Result<Self, PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        let parent = fcntl_dupfd_cloexec(parent, 0).map_err(|_| io_error())?;
        let snapshot = fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(map_open)?;
        inspect_symlink(&snapshot, owner, target.len())?;
        let observed = fs::readlinkat(&parent, name, Vec::new()).map_err(|_| io_error())?;
        if observed.as_bytes() != target.as_bytes() {
            return Err(unsafe_error());
        }
        Ok(Self {
            parent,
            name: name.to_owned(),
            target,
            snapshot,
        })
    }

    fn revalidate(&self) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        let snapshot = fs::statat(&self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| changed_error())?;
        let target =
            fs::readlinkat(&self.parent, &self.name, Vec::new()).map_err(|_| changed_error())?;
        if !same_snapshot(&self.snapshot, &snapshot) || target.as_bytes() != self.target.as_bytes()
        {
            return Err(changed_error());
        }
        Ok(())
    }
}

struct BoundLoaderFile {
    file: File,
    parent: OwnedFd,
    name: String,
    snapshot: rustix::fs::Stat,
    content_digest: Option<[u8; 32]>,
    object_digest: Option<[u8; 32]>,
}

impl BoundLoaderFile {
    fn open(
        parent: BorrowedFd<'_>,
        name: &str,
        owner: (u32, u32),
    ) -> Result<Self, PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        let parent = fcntl_dupfd_cloexec(parent, 0).map_err(|_| io_error())?;
        let fd = fs::openat(&parent, name, FILE_FLAGS, Mode::empty()).map_err(map_open)?;
        let file = File::from(fd);
        let snapshot = fs::fstat(&file).map_err(|_| io_error())?;
        inspect_loader(&snapshot, owner)?;
        Ok(Self {
            file,
            parent,
            name: name.to_owned(),
            snapshot,
            content_digest: None,
            object_digest: None,
        })
    }

    fn load(
        &mut self,
        architecture: PersonalWorkerRuntimeArchitecture,
        expected_loader: LinuxRuntimeDynamicLoader,
    ) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        let first = read_bounded(&mut self.file)?;
        let object =
            parse_linux_runtime_loader_object(&first, architecture).map_err(|_| invalid_error())?;
        if object.loader() != expected_loader || object.architecture() != architecture {
            return Err(invalid_error());
        }
        let content_digest: [u8; 32] = Sha256::digest(&first).into();
        let object_digest = loader_object_digest(object);
        self.file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
        let second = read_bounded(&mut self.file)?;
        let after = fs::fstat(&self.file).map_err(|_| io_error())?;
        if first != second || !same_snapshot(&self.snapshot, &after) {
            return Err(changed_error());
        }
        self.content_digest = Some(content_digest);
        self.object_digest = Some(object_digest);
        Ok(())
    }

    fn revalidate(&mut self) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
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
    ) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        for value in [
            self.snapshot.st_uid,
            self.snapshot.st_gid,
            self.snapshot.st_mode,
        ] {
            hash_field(hasher, &value.to_be_bytes());
        }
        hash_field(hasher, &(self.snapshot.st_size as u64).to_be_bytes());
        hash_field(hasher, &self.content_digest.ok_or_else(invalid_error)?);
        hash_field(hasher, &self.object_digest.ok_or_else(invalid_error)?);
        Ok(())
    }
}

struct DirectoryNode {
    fd: OwnedFd,
    name: String,
    snapshot: rustix::fs::Stat,
}

struct DirectoryChain {
    root: OwnedFd,
    root_snapshot: rustix::fs::Stat,
    nodes: Vec<DirectoryNode>,
}

impl DirectoryChain {
    fn open_from(
        root: BorrowedFd<'_>,
        relative: &str,
        owner: (u32, u32),
    ) -> Result<Self, PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        let root = fcntl_dupfd_cloexec(root, 0).map_err(|_| io_error())?;
        let root_snapshot = fs::fstat(&root).map_err(|_| io_error())?;
        inspect_directory(&root_snapshot, owner)?;
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(identity_error());
        }
        let mut nodes = Vec::new();
        for component in path.components() {
            let name = component.as_os_str().to_str().ok_or_else(identity_error)?;
            let parent = nodes
                .last()
                .map_or_else(|| root.as_fd(), |node: &DirectoryNode| node.fd.as_fd());
            let fd = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
            let snapshot = fs::fstat(&fd).map_err(|_| io_error())?;
            inspect_directory(&snapshot, owner)?;
            nodes.push(DirectoryNode {
                fd,
                name: name.to_owned(),
                snapshot,
            });
        }
        if nodes.is_empty() {
            return Err(identity_error());
        }
        Ok(Self {
            root,
            root_snapshot,
            nodes,
        })
    }

    fn leaf(&self) -> BorrowedFd<'_> {
        self.nodes
            .last()
            .expect("nonempty directory chain")
            .fd
            .as_fd()
    }

    fn leaf_snapshot(&self) -> rustix::fs::Stat {
        self.nodes
            .last()
            .expect("nonempty directory chain")
            .snapshot
    }

    fn revalidate(&self) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
        let held_root = fs::fstat(&self.root).map_err(|_| changed_error())?;
        if !same_snapshot(&self.root_snapshot, &held_root) {
            return Err(changed_error());
        }
        for (index, node) in self.nodes.iter().enumerate() {
            let held = fs::fstat(&node.fd).map_err(|_| changed_error())?;
            let parent = if index == 0 {
                self.root.as_fd()
            } else {
                self.nodes[index - 1].fd.as_fd()
            };
            let path = fs::statat(parent, &node.name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| changed_error())?;
            if !same_snapshot(&node.snapshot, &held) || !same_snapshot(&node.snapshot, &path) {
                return Err(changed_error());
            }
        }
        Ok(())
    }

    fn hash_semantics(&self, hasher: &mut Sha256) {
        for node in &self.nodes {
            hash_field(hasher, node.name.as_bytes());
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

fn loader_object_digest(object: LinuxRuntimeLoaderObject) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"loader_object");
    hash_field(&mut hasher, &[object.architecture() as u8]);
    hash_field(
        &mut hasher,
        &[match object.loader() {
            LinuxRuntimeDynamicLoader::Aarch64Gnu => 1,
            LinuxRuntimeDynamicLoader::X86_64Gnu => 2,
        }],
    );
    hasher.finalize().into()
}

fn derive_identity(
    project: &ProjectIdentity,
    architecture: PersonalWorkerRuntimeArchitecture,
    source: &BoundLoaderObject,
) -> Result<Sha256Digest, PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
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
    hash_field(&mut hasher, &source.semantic_digest()?);
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| invalid_error())
}

fn inspect_loader(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o7777 != 0o0755
    {
        return Err(unsafe_error());
    }
    if stat.st_size <= 0 || stat.st_size as u64 > LINUX_RUNTIME_ELF_MAX_BYTES as u64 {
        return Err(invalid_error());
    }
    Ok(())
}

fn inspect_symlink(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
    target_length: usize,
) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
    if !FileType::from_raw_mode(stat.st_mode).is_symlink()
        || stat.st_nlink != 1
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o7777 != 0o0777
        || stat.st_size != target_length as i64
    {
        return Err(unsafe_error());
    }
    Ok(())
}

fn inspect_directory(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
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
) -> Result<Vec<u8>, PersonalWorkerRuntimeLoaderObjectPrerequisiteError> {
    let mut reader: Take<&mut File> = file.take((LINUX_RUNTIME_ELF_MAX_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|_| io_error())?;
    if bytes.len() > LINUX_RUNTIME_ELF_MAX_BYTES {
        return Err(invalid_error());
    }
    Ok(bytes)
}

fn same_object(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn same_snapshot(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    same_object(left, right)
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
    kind: PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind,
    code: &'static str,
    message: &'static str,
) -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
        kind,
        code,
        message,
    }
}

const fn identity_error() -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::IdentityMismatch,
        "runtime_loader_object_identity_mismatch",
        "runtime loader-object prerequisite identity does not match",
    )
}

const fn missing_error() -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::Missing,
        "runtime_loader_object_missing",
        "required runtime loader-object prerequisite is missing",
    )
}

const fn unsupported_error() -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::UnsupportedArchitecture,
        "runtime_loader_object_unsupported_architecture",
        "runtime loader-object observation is unsupported on this architecture",
    )
}

const fn unsafe_error() -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::UnsafeFilesystem,
        "runtime_loader_object_unsafe_filesystem",
        "runtime loader-object filesystem evidence is unsafe",
    )
}

const fn invalid_error() -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::InvalidLoader,
        "runtime_loader_object_invalid",
        "runtime loader-object evidence is invalid",
    )
}

const fn changed_error() -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::ChangedDuringRead,
        "runtime_loader_object_changed",
        "runtime loader-object evidence changed during observation",
    )
}

const fn io_error() -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::Io,
        "runtime_loader_object_io",
        "runtime loader-object evidence could not be read",
    )
}

fn map_open(error: Errno) -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    match error {
        Errno::NOENT => missing_error(),
        Errno::LOOP | Errno::NOTDIR => unsafe_error(),
        _ => io_error(),
    }
}

fn map_resolved_open(error: Errno) -> PersonalWorkerRuntimeLoaderObjectPrerequisiteError {
    match error {
        Errno::NOENT => missing_error(),
        Errno::LOOP | Errno::NOTDIR | Errno::XDEV => unsafe_error(),
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
    fn disposable_noble_loader_object_is_a_descriptor_bound_prerequisite() {
        if std::env::var("GLAEDA_DISPOSABLE_PROBE").as_deref() != Ok("1") {
            return;
        }
        let architecture = host_architecture().expect("supported hosted architecture");
        let project = project();
        let live =
            observe_personal_worker_runtime_loader_object_prerequisite(&project, architecture)
                .expect("observe live loader object");
        assert_eq!(live.summary().loader_count(), 1);

        let fixture = Fixture::new(architecture);
        let owner = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        let mut observed = observe_at(&fixture.root, owner, &project, architecture, || {})
            .expect("observe copied loader fixture");
        assert_eq!(
            observed.summary().disposition(),
            PersonalWorkerRuntimeLoaderObjectPrerequisiteDisposition::ObservedPrerequisite
        );
        let debug = format!("{observed:?}");
        assert!(debug.contains(REDACTED));
        assert!(!debug.contains("/lib"));
        assert!(!debug.contains("sha256:"));
        observed
            .reconfirm()
            .expect("reconfirm stable loader object");
        fixture.replace_logical_parent_with_target_parent();
        let reconfirmed = observed
            .reconfirm()
            .expect_err("post-observation loader route drift");
        assert_eq!(
            reconfirmed.kind,
            PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::ChangedDuringRead
        );
        fixture.restore_logical_route();

        let changed = observe_at(&fixture.root, owner, &project, architecture, || {
            fixture.replace_logical_parent_with_target_parent();
        })
        .expect_err("logical parent rebind");
        assert_eq!(
            changed.kind,
            PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::ChangedDuringRead
        );
        fixture.restore_logical_route();

        stdfs::set_permissions(
            fixture.canonical_target(),
            stdfs::Permissions::from_mode(0o0775),
        )
        .expect("make loader writable");
        let writable = observe_at(&fixture.root, owner, &project, architecture, || {})
            .expect_err("writable loader");
        assert_eq!(
            writable.kind,
            PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::UnsafeFilesystem
        );
        stdfs::set_permissions(
            fixture.canonical_target(),
            stdfs::Permissions::from_mode(0o0755),
        )
        .expect("restore loader mode");

        let extra = fixture.canonical_target().with_extension("extra-link");
        stdfs::hard_link(fixture.canonical_target(), &extra).expect("hardlink loader target");
        let hardlinked = observe_at(&fixture.root, owner, &project, architecture, || {})
            .expect_err("hardlinked loader");
        assert_eq!(
            hardlinked.kind,
            PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::UnsafeFilesystem
        );
        stdfs::remove_file(extra).expect("remove loader hardlink");

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
        .expect_err("architecture mismatch before filesystem access");
        assert_eq!(
            mismatch.kind,
            PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::IdentityMismatch
        );
    }

    #[test]
    fn route_contract_and_public_surfaces_are_fixed_and_private() {
        let aarch64 = LoaderRoute::for_architecture(PersonalWorkerRuntimeArchitecture::Aarch64);
        assert_eq!(aarch64.logical_path, "lib/ld-linux-aarch64.so.1");
        assert_eq!(aarch64.canonical_source_parent, "usr/lib");
        assert_eq!(aarch64.canonical_target_parent, "usr/lib/aarch64-linux-gnu");
        let x86 = LoaderRoute::for_architecture(PersonalWorkerRuntimeArchitecture::X86_64);
        assert_eq!(x86.logical_path, "lib64/ld-linux-x86-64.so.2");
        assert_eq!(x86.canonical_source_parent, "usr/lib64");
        assert_eq!(x86.canonical_target_parent, "usr/lib/x86_64-linux-gnu");

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
            assert!(!debug.contains("/lib"));
            assert!(!json.contains("/lib"));
            assert!(!debug.contains("runtime-runner"));
        }
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
        route: LoaderRoute,
    }

    impl Fixture {
        fn new(architecture: PersonalWorkerRuntimeArchitecture) -> Self {
            let parent = std::env::current_dir()
                .expect("current directory")
                .join("target/r01-loader-object-prerequisite-fixtures");
            stdfs::create_dir_all(&parent).expect("create loader fixture parent");
            let parent_metadata = stdfs::symlink_metadata(&parent).expect("inspect fixture parent");
            assert!(parent_metadata.file_type().is_dir());
            let parent_identity = (parent_metadata.dev(), parent_metadata.ino());
            for _ in 0..128 {
                let nonce = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
                let root = parent.join(format!("{}-{nonce}", std::process::id()));
                let mut builder = stdfs::DirBuilder::new();
                builder.mode(0o0700);
                match builder.create(&root) {
                    Ok(()) => {
                        let metadata =
                            stdfs::symlink_metadata(&root).expect("inspect fixture root");
                        let fixture = Self {
                            parent: parent.clone(),
                            parent_identity,
                            root,
                            root_identity: (metadata.dev(), metadata.ino()),
                            route: LoaderRoute::for_architecture(architecture),
                        };
                        fixture.populate(architecture);
                        return fixture;
                    }
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create private loader fixture root: {error}"),
                }
            }
            panic!("allocate unique loader fixture root")
        }

        fn populate(&self, architecture: PersonalWorkerRuntimeArchitecture) {
            stdfs::create_dir_all(self.root.join(self.route.canonical_source_parent))
                .expect("create source parent");
            stdfs::create_dir_all(self.root.join(self.route.canonical_target_parent))
                .expect("create target parent");
            let live = match architecture {
                PersonalWorkerRuntimeArchitecture::Aarch64 => "/lib/ld-linux-aarch64.so.1",
                PersonalWorkerRuntimeArchitecture::X86_64 => "/lib64/ld-linux-x86-64.so.2",
            };
            stdfs::copy(live, self.canonical_target()).expect("copy live loader object");
            stdfs::set_permissions(
                self.canonical_target(),
                stdfs::Permissions::from_mode(0o0755),
            )
            .expect("set loader target mode");
            self.restore_logical_route();
        }

        fn canonical_target(&self) -> std::path::PathBuf {
            self.root
                .join(self.route.canonical_target_parent)
                .join(self.route.canonical_target_name)
        }

        fn logical_parent(&self) -> std::path::PathBuf {
            self.root.join(self.route.logical_parent)
        }

        fn restore_logical_route(&self) {
            if stdfs::symlink_metadata(self.logical_parent()).is_ok() {
                stdfs::remove_file(self.logical_parent()).expect("remove prior logical parent");
            }
            symlink(self.route.canonical_source_parent, self.logical_parent())
                .expect("create usr-merge logical parent");
            let logical_entry = self.root.join(self.route.canonical_source_parent).join(
                Path::new(self.route.logical_path)
                    .file_name()
                    .expect("logical loader basename"),
            );
            if stdfs::symlink_metadata(&logical_entry).is_ok() {
                stdfs::remove_file(&logical_entry).expect("remove prior logical loader entry");
            }
            let relative_target = match self.route.loader {
                LinuxRuntimeDynamicLoader::Aarch64Gnu => "aarch64-linux-gnu/ld-linux-aarch64.so.1",
                LinuxRuntimeDynamicLoader::X86_64Gnu => {
                    "../lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
                }
            };
            symlink(relative_target, logical_entry).expect("create logical loader entry");
        }

        fn replace_logical_parent_with_target_parent(&self) {
            stdfs::remove_file(self.logical_parent()).expect("remove logical parent");
            symlink(self.route.canonical_target_parent, self.logical_parent())
                .expect("rebind logical parent");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            assert_eq!(self.root.parent(), Some(self.parent.as_path()));
            let parent_metadata =
                stdfs::symlink_metadata(&self.parent).expect("revalidate loader fixture parent");
            assert_eq!(
                (parent_metadata.dev(), parent_metadata.ino()),
                self.parent_identity
            );
            let root_metadata =
                stdfs::symlink_metadata(&self.root).expect("revalidate loader fixture root");
            assert!(root_metadata.file_type().is_dir());
            assert_eq!(
                (root_metadata.dev(), root_metadata.ino()),
                self.root_identity
            );
            stdfs::remove_dir_all(&self.root).expect("remove owned loader fixture");
            if let Err(error) = stdfs::remove_dir(&self.parent)
                && !matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                )
            {
                panic!("remove empty loader fixture parent: {error}");
            }
        }
    }
}
