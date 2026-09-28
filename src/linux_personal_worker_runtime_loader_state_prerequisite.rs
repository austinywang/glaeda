//! Descriptor-bound prerequisite for the fixed Ubuntu Noble dynamic-loader state.
//!
//! This module observes only the root loader configuration, every included `.conf` fragment, the
//! current loader cache, and the required absence of `/etc/ld.so.preload`. It does not resolve or
//! open an ELF interpreter or library, execute `ldconfig`/`ldd`, inspect package-manager state,
//! construct a runtime evidence class, or seal readiness.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Take};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path};

use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::{Errno, fcntl_dupfd_cloexec};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::linux_dynamic_loader_cache::{
    LINUX_DYNAMIC_LOADER_CACHE_MAX_BYTES, LinuxDynamicLoaderCache,
    LinuxDynamicLoaderCacheErrorKind, parse_linux_dynamic_loader_cache,
};
use crate::linux_dynamic_loader_config::{
    LINUX_DYNAMIC_LOADER_CONFIG_MAX_BYTES, LinuxDynamicLoaderConfig,
    LinuxDynamicLoaderConfigErrorKind, LinuxDynamicLoaderConfigRole,
    LinuxDynamicLoaderSearchDirectory, parse_linux_dynamic_loader_config,
};
use crate::manifest::RunnerScope;
use crate::ownership::ProjectIdentity;
use crate::personal_worker_runtime_contract::PersonalWorkerRuntimeArchitecture;

pub const PERSONAL_WORKER_RUNTIME_LOADER_STATE_PREREQUISITE_SCHEMA_VERSION: u8 = 1;

const MAX_FRAGMENT_COUNT: usize = 32;
const MAX_DIRECTORY_ENTRY_COUNT: usize = 128;
const MAX_FRAGMENT_NAME_BYTES: usize = 255;
const ROOT_CONFIG: &str = "ld.so.conf";
const CACHE_FILE: &str = "ld.so.cache";
const PRELOAD_FILE: &str = "ld.so.preload";
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const DOMAIN: &[u8] = b"smolrunner-personal-worker-runtime-loader-state-prerequisite-v1";
const REDACTED: &str = "<private-runtime-loader-state-prerequisite>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeLoaderStatePrerequisiteDisposition {
    ObservedPrerequisite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeLoaderStatePrerequisiteSummary {
    schema_version: u8,
    disposition: PersonalWorkerRuntimeLoaderStatePrerequisiteDisposition,
    fragment_count: u8,
}

impl PersonalWorkerRuntimeLoaderStatePrerequisiteSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> PersonalWorkerRuntimeLoaderStatePrerequisiteDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn fragment_count(self) -> u8 {
        self.fragment_count
    }
}

/// Opaque current prerequisite for later interpreter and transitive-library resolution.
///
/// The retained descriptors, bytes, parsed configuration, cache entries, and identity have no
/// public accessor, serialization, cloning, digest, path, or readiness conversion surface.
pub struct PersonalWorkerRuntimeLoaderStatePrerequisite {
    summary: PersonalWorkerRuntimeLoaderStatePrerequisiteSummary,
    _identity: Sha256Digest,
    _sources: LoaderStateSources,
}

impl PersonalWorkerRuntimeLoaderStatePrerequisite {
    #[must_use]
    pub const fn summary(&self) -> PersonalWorkerRuntimeLoaderStatePrerequisiteSummary {
        self.summary
    }

    pub(crate) fn reconfirm(
        &mut self,
    ) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
        let sources = &mut self._sources;
        sources.etc_chain.revalidate()?;
        sources.config_chain.revalidate()?;
        sources.root_config.revalidate()?;
        for fragment in &mut sources.fragments {
            fragment.file.revalidate()?;
        }
        sources.cache.revalidate()?;
        let fragment_names = sources
            .fragments
            .iter()
            .map(|fragment| fragment.name.clone())
            .collect::<Vec<_>>();
        revalidate_fragment_names(sources.config_chain.leaf(), &fragment_names)?;
        revalidate_preload_absent(sources.etc_chain.leaf())?;
        sources.config_chain.revalidate()?;
        sources.etc_chain.revalidate()
    }
}

impl fmt::Debug for PersonalWorkerRuntimeLoaderStatePrerequisite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeLoaderStatePrerequisite")
            .field("summary", &self.summary)
            .field("private_prerequisite", &REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind {
    IdentityMismatch,
    Missing,
    UnsupportedArchitecture,
    UnsafeFilesystem,
    UnsafeConfiguration,
    VersionIncompatible,
    InvalidConfiguration,
    ChangedDuringRead,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    pub kind: PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeLoaderStatePrerequisiteError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerRuntimeLoaderStatePrerequisiteError {}

/// Observe fixed Noble loader state without executing a child process or resolving a library.
///
/// # Errors
///
/// Fails closed unless the running architecture matches; the root configuration, included
/// fragments, cache, directory chains, and pathname bindings are safe and stable; every parser
/// accepts the exact bounded bytes; and `/etc/ld.so.preload` remains absent through observation.
pub fn observe_personal_worker_runtime_loader_state_prerequisite(
    project: &ProjectIdentity,
    architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<
    PersonalWorkerRuntimeLoaderStatePrerequisite,
    PersonalWorkerRuntimeLoaderStatePrerequisiteError,
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
    PersonalWorkerRuntimeLoaderStatePrerequisite,
    PersonalWorkerRuntimeLoaderStatePrerequisiteError,
>
where
    F: FnOnce(),
{
    if host_architecture().ok_or_else(unsupported_error)? != architecture {
        return Err(identity_error());
    }

    let etc_chain = DirectoryChain::open(root_path, "etc", authority_owner)?;
    require_preload_absent(etc_chain.leaf())?;
    let mut root_config = BoundFile::open(
        etc_chain.leaf(),
        ROOT_CONFIG,
        authority_owner,
        LINUX_DYNAMIC_LOADER_CONFIG_MAX_BYTES,
    )?;
    let mut cache = BoundFile::open(
        etc_chain.leaf(),
        CACHE_FILE,
        authority_owner,
        LINUX_DYNAMIC_LOADER_CACHE_MAX_BYTES,
    )?;
    let config_chain = DirectoryChain::open(root_path, "etc/ld.so.conf.d", authority_owner)?;
    let fragment_names = enumerate_fragment_names(config_chain.leaf())?;
    if fragment_names.is_empty() || fragment_names.len() > MAX_FRAGMENT_COUNT {
        return Err(invalid_error());
    }
    let mut fragments = Vec::with_capacity(fragment_names.len());
    for name in &fragment_names {
        fragments.push(ConfigFragment {
            name: name.clone(),
            file: BoundFile::open(
                config_chain.leaf(),
                name,
                authority_owner,
                LINUX_DYNAMIC_LOADER_CONFIG_MAX_BYTES,
            )?,
            parsed: None,
        });
    }

    root_config.load()?;
    let parsed_root = parse_linux_dynamic_loader_config(
        root_config.bytes()?,
        architecture,
        LinuxDynamicLoaderConfigRole::Root,
    )
    .map_err(map_config_parse_error)?;
    let mut seen_directories = BTreeSet::new();
    for fragment in &mut fragments {
        fragment.file.load()?;
        let parsed = parse_linux_dynamic_loader_config(
            fragment.file.bytes()?,
            architecture,
            LinuxDynamicLoaderConfigRole::Fragment,
        )
        .map_err(map_config_parse_error)?;
        for directory in parsed.search_directories() {
            if !seen_directories.insert(*directory) {
                return Err(invalid_error());
            }
        }
        fragment.parsed = Some(parsed);
    }
    if seen_directories.is_empty() {
        return Err(invalid_error());
    }
    cache.load()?;
    let parsed_cache = parse_linux_dynamic_loader_cache(cache.bytes()?, architecture)
        .map_err(map_cache_parse_error)?;
    if parsed_cache.entries().is_empty() {
        return Err(invalid_error());
    }

    before_revalidation();
    etc_chain.revalidate()?;
    config_chain.revalidate()?;
    root_config.revalidate()?;
    for fragment in &mut fragments {
        fragment.file.revalidate()?;
    }
    cache.revalidate()?;
    revalidate_fragment_names(config_chain.leaf(), &fragment_names)?;
    revalidate_preload_absent(etc_chain.leaf())?;
    config_chain.revalidate()?;
    etc_chain.revalidate()?;

    let sources = LoaderStateSources {
        etc_chain,
        config_chain,
        root_config,
        parsed_root,
        fragments,
        cache,
        parsed_cache,
    };
    let identity = derive_identity(project, architecture, &sources)?;
    Ok(PersonalWorkerRuntimeLoaderStatePrerequisite {
        summary: PersonalWorkerRuntimeLoaderStatePrerequisiteSummary {
            schema_version: PERSONAL_WORKER_RUNTIME_LOADER_STATE_PREREQUISITE_SCHEMA_VERSION,
            disposition:
                PersonalWorkerRuntimeLoaderStatePrerequisiteDisposition::ObservedPrerequisite,
            fragment_count: u8::try_from(fragment_names.len()).map_err(|_| invalid_error())?,
        },
        _identity: identity,
        _sources: sources,
    })
}

struct LoaderStateSources {
    etc_chain: DirectoryChain,
    config_chain: DirectoryChain,
    root_config: BoundFile,
    parsed_root: LinuxDynamicLoaderConfig,
    fragments: Vec<ConfigFragment>,
    cache: BoundFile,
    parsed_cache: LinuxDynamicLoaderCache,
}

struct ConfigFragment {
    name: String,
    file: BoundFile,
    parsed: Option<LinuxDynamicLoaderConfig>,
}

struct BoundFile {
    file: File,
    parent: OwnedFd,
    name: String,
    snapshot: rustix::fs::Stat,
    max_bytes: usize,
    bytes: Option<Vec<u8>>,
    digest: Option<[u8; 32]>,
}

impl BoundFile {
    fn open(
        parent: BorrowedFd<'_>,
        name: &str,
        owner: (u32, u32),
        max_bytes: usize,
    ) -> Result<Self, PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
        let parent = fcntl_dupfd_cloexec(parent, 0).map_err(|_| io_error())?;
        let fd = fs::openat(&parent, name, FILE_FLAGS, Mode::empty()).map_err(map_required_open)?;
        let file = File::from(fd);
        let snapshot = fs::fstat(&file).map_err(|_| io_error())?;
        inspect_file(&snapshot, owner, max_bytes)?;
        Ok(Self {
            file,
            parent,
            name: name.to_owned(),
            snapshot,
            max_bytes,
            bytes: None,
            digest: None,
        })
    }

    fn load(&mut self) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
        let first = read_bounded(&mut self.file, self.max_bytes)?;
        self.file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
        let second = read_bounded(&mut self.file, self.max_bytes)?;
        let after = fs::fstat(&self.file).map_err(|_| io_error())?;
        if first != second
            || first.len() as i64 != self.snapshot.st_size
            || !same_snapshot(&self.snapshot, &after)
        {
            return Err(changed_error());
        }
        self.digest = Some(Sha256::digest(&first).into());
        self.bytes = Some(first);
        Ok(())
    }

    fn bytes(&self) -> Result<&[u8], PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
        self.bytes.as_deref().ok_or_else(invalid_error)
    }

    fn revalidate(&mut self) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
        let expected_digest = self.digest.ok_or_else(invalid_error)?;
        let before = fs::fstat(&self.file).map_err(|_| changed_error())?;
        if !same_snapshot(&self.snapshot, &before) {
            return Err(changed_error());
        }
        for _ in 0..2 {
            self.file
                .seek(SeekFrom::Start(0))
                .map_err(|_| changed_error())?;
            let bytes =
                read_bounded(&mut self.file, self.max_bytes).map_err(|_| changed_error())?;
            if <[u8; 32]>::from(Sha256::digest(&bytes)) != expected_digest {
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
    ) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
        hash_field(hasher, self.name.as_bytes());
        for value in [
            self.snapshot.st_uid,
            self.snapshot.st_gid,
            self.snapshot.st_mode,
        ] {
            hash_field(hasher, &value.to_be_bytes());
        }
        hash_field(hasher, &(self.snapshot.st_size as u64).to_be_bytes());
        hash_field(hasher, &self.digest.ok_or_else(invalid_error)?);
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
    ) -> Result<Self, PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
        let root =
            fs::open(root_path, DIRECTORY_FLAGS, Mode::empty()).map_err(map_required_open)?;
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
            let fd = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_required_open)?;
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

    fn revalidate(&self) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
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

fn enumerate_fragment_names(
    directory: BorrowedFd<'_>,
) -> Result<Vec<String>, PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
    let directory =
        fs::openat(directory, ".", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| io_error())?;
    let mut entries = Dir::read_from(&directory).map_err(|_| io_error())?;
    let mut entry_count = 0_usize;
    let mut names = Vec::new();
    for entry in &mut entries {
        let entry = entry.map_err(|_| io_error())?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        entry_count = entry_count.checked_add(1).ok_or_else(invalid_error)?;
        if entry_count > MAX_DIRECTORY_ENTRY_COUNT {
            return Err(invalid_error());
        }
        // glibc's ordinary `glob` expansion does not select leading-dot entries for `*.conf`.
        if bytes.first() == Some(&b'.') || !bytes.ends_with(b".conf") {
            continue;
        }
        if bytes.is_empty()
            || bytes.len() > MAX_FRAGMENT_NAME_BYTES
            || bytes.iter().any(|byte| {
                !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'+' | b'-')
            })
        {
            return Err(unsafe_configuration_error());
        }
        names.push(
            std::str::from_utf8(bytes)
                .map_err(|_| unsafe_configuration_error())?
                .to_owned(),
        );
    }
    names.sort_unstable();
    if names.windows(2).any(|window| window[0] == window[1]) {
        return Err(invalid_error());
    }
    Ok(names)
}

fn revalidate_fragment_names(
    directory: BorrowedFd<'_>,
    expected: &[String],
) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
    match enumerate_fragment_names(directory) {
        Ok(observed) if observed == expected => Ok(()),
        Ok(_) => Err(changed_error()),
        Err(error) if error.kind == PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::Io => {
            Err(error)
        }
        Err(_) => Err(changed_error()),
    }
}

fn require_preload_absent(
    etc: BorrowedFd<'_>,
) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
    match fs::statat(etc, PRELOAD_FILE, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(()),
        Ok(_) | Err(Errno::LOOP | Errno::NOTDIR) => Err(unsafe_configuration_error()),
        Err(_) => Err(io_error()),
    }
}

fn revalidate_preload_absent(
    etc: BorrowedFd<'_>,
) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
    match fs::statat(etc, PRELOAD_FILE, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(()),
        Ok(_) | Err(Errno::LOOP | Errno::NOTDIR) => Err(changed_error()),
        Err(_) => Err(io_error()),
    }
}

fn derive_identity(
    project: &ProjectIdentity,
    architecture: PersonalWorkerRuntimeArchitecture,
    sources: &LoaderStateSources,
) -> Result<Sha256Digest, PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
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
    sources.etc_chain.hash_semantics(&mut hasher);
    sources.config_chain.hash_semantics(&mut hasher);
    sources.root_config.hash_semantics(&mut hasher)?;
    hash_config_semantics(&mut hasher, &sources.parsed_root);
    for fragment in &sources.fragments {
        hash_field(&mut hasher, fragment.name.as_bytes());
        fragment.file.hash_semantics(&mut hasher)?;
        hash_config_semantics(
            &mut hasher,
            fragment.parsed.as_ref().ok_or_else(invalid_error)?,
        );
    }
    sources.cache.hash_semantics(&mut hasher)?;
    hash_cache_semantics(&mut hasher, &sources.parsed_cache);
    hash_field(&mut hasher, b"preload_absent");
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| invalid_error())
}

fn hash_config_semantics(hasher: &mut Sha256, config: &LinuxDynamicLoaderConfig) {
    hash_field(hasher, &[config.architecture() as u8]);
    hash_field(
        hasher,
        &[match config.role() {
            LinuxDynamicLoaderConfigRole::Root => 1,
            LinuxDynamicLoaderConfigRole::Fragment => 2,
        }],
    );
    hash_field(hasher, &[u8::from(config.includes_system_fragments())]);
    for directory in config.search_directories() {
        hash_field(
            hasher,
            &[match directory {
                LinuxDynamicLoaderSearchDirectory::Local => 1,
                LinuxDynamicLoaderSearchDirectory::LocalMultiarch => 2,
                LinuxDynamicLoaderSearchDirectory::LibMultiarch => 3,
                LinuxDynamicLoaderSearchDirectory::UsrLibMultiarch => 4,
            }],
        );
    }
}

fn hash_cache_semantics(hasher: &mut Sha256, cache: &LinuxDynamicLoaderCache) {
    hash_field(hasher, &[cache.architecture() as u8]);
    hash_field(
        hasher,
        &(cache.ignored_incompatible_entry_count() as u64).to_be_bytes(),
    );
    for hwcap in cache.glibc_hwcap_names() {
        hash_field(hasher, hwcap.as_bytes());
    }
    for entry in cache.entries() {
        hash_field(hasher, entry.library_name().as_bytes());
        hash_field(hasher, entry.library_path().as_bytes());
        hash_field(hasher, entry.hwcap_name().unwrap_or("").as_bytes());
        hash_field(hasher, &entry.isa_level().to_be_bytes());
    }
}

fn inspect_file(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
    max_bytes: usize,
) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o7777 != 0o0644
    {
        return Err(unsafe_error());
    }
    if stat.st_size <= 0 || stat.st_size as u64 > max_bytes as u64 {
        return Err(invalid_error());
    }
    Ok(())
}

fn inspect_directory(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
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
    max_bytes: usize,
) -> Result<Vec<u8>, PersonalWorkerRuntimeLoaderStatePrerequisiteError> {
    let mut reader: Take<&mut File> = file.take((max_bytes + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|_| io_error())?;
    if bytes.len() > max_bytes {
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

fn map_required_open(error: Errno) -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    match error {
        Errno::NOENT => missing_error(),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => unsafe_error(),
        _ => io_error(),
    }
}

fn map_cache_parse_error(
    error: crate::linux_dynamic_loader_cache::LinuxDynamicLoaderCacheError,
) -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    match error.kind {
        LinuxDynamicLoaderCacheErrorKind::VersionIncompatible => version_error(),
        LinuxDynamicLoaderCacheErrorKind::UnsafePath
        | LinuxDynamicLoaderCacheErrorKind::UnsupportedCapability => unsafe_configuration_error(),
        LinuxDynamicLoaderCacheErrorKind::Size
        | LinuxDynamicLoaderCacheErrorKind::Format
        | LinuxDynamicLoaderCacheErrorKind::Architecture => invalid_error(),
    }
}

fn map_config_parse_error(
    error: crate::linux_dynamic_loader_config::LinuxDynamicLoaderConfigError,
) -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    match error.kind {
        LinuxDynamicLoaderConfigErrorKind::UnsafeSearch => unsafe_configuration_error(),
        LinuxDynamicLoaderConfigErrorKind::Size | LinuxDynamicLoaderConfigErrorKind::Format => {
            invalid_error()
        }
    }
}

const fn error(
    kind: PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind,
    code: &'static str,
    message: &'static str,
) -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    PersonalWorkerRuntimeLoaderStatePrerequisiteError {
        kind,
        code,
        message,
    }
}

const fn identity_error() -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::IdentityMismatch,
        "runtime_loader_state_identity_mismatch",
        "runtime loader-state prerequisite does not match the requested identity",
    )
}

const fn missing_error() -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::Missing,
        "runtime_loader_state_missing",
        "a required runtime loader-state prerequisite is missing",
    )
}

const fn unsupported_error() -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::UnsupportedArchitecture,
        "runtime_loader_state_architecture_unsupported",
        "runtime loader-state observation does not support this architecture",
    )
}

const fn unsafe_error() -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::UnsafeFilesystem,
        "runtime_loader_state_filesystem_unsafe",
        "runtime loader-state prerequisite filesystem evidence is unsafe",
    )
}

const fn unsafe_configuration_error() -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::UnsafeConfiguration,
        "runtime_loader_state_configuration_unsafe",
        "runtime loader-state prerequisite selects unreviewed loader authority",
    )
}

const fn version_error() -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::VersionIncompatible,
        "runtime_loader_state_version_incompatible",
        "runtime loader-state prerequisite requires an explicit format migration",
    )
}

const fn invalid_error() -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::InvalidConfiguration,
        "runtime_loader_state_invalid",
        "runtime loader-state prerequisite is invalid or outside canonical bounds",
    )
}

const fn changed_error() -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::ChangedDuringRead,
        "runtime_loader_state_changed",
        "runtime loader-state prerequisite changed during observation",
    )
}

const fn io_error() -> PersonalWorkerRuntimeLoaderStatePrerequisiteError {
    error(
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::Io,
        "runtime_loader_state_io",
        "runtime loader-state prerequisite could not be read safely",
    )
}

#[cfg(test)]
mod tests {
    use std::fs as stdfs;
    use std::io::ErrorKind;
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn project() -> ProjectIdentity {
        ProjectIdentity {
            repository: "example/runtime".to_owned(),
            runner_scope: RunnerScope::Repository,
            runner_user: "runtime-runner".to_owned(),
        }
    }

    #[test]
    fn architecture_mismatch_precedes_filesystem_access() {
        let opposite = match host_architecture().expect("supported test architecture") {
            PersonalWorkerRuntimeArchitecture::Aarch64 => PersonalWorkerRuntimeArchitecture::X86_64,
            PersonalWorkerRuntimeArchitecture::X86_64 => PersonalWorkerRuntimeArchitecture::Aarch64,
        };
        let error = observe_at(
            Path::new("/definitely/missing/runtime-root"),
            (0, 0),
            &project(),
            opposite,
            || {},
        )
        .expect_err("architecture mismatch");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::IdentityMismatch
        );
    }

    #[test]
    fn public_errors_and_debug_are_path_free() {
        for value in [
            identity_error(),
            missing_error(),
            unsupported_error(),
            unsafe_error(),
            unsafe_configuration_error(),
            version_error(),
            invalid_error(),
            changed_error(),
            io_error(),
        ] {
            let debug = format!("{value:?}");
            let json = serde_json::to_string(&value).expect("serialize public error");
            for forbidden in ["/etc", "ld.so", "sha256:", "example/runtime"] {
                assert!(!debug.contains(forbidden));
                assert!(!json.contains(forbidden));
            }
        }
    }

    #[test]
    fn disposable_noble_loader_state_is_descriptor_bound() {
        if std::env::var("GLAEDA_DISPOSABLE_PROBE").as_deref() != Ok("1") {
            return;
        }
        let architecture = host_architecture().expect("supported package-probe architecture");
        let observed =
            observe_personal_worker_runtime_loader_state_prerequisite(&project(), architecture)
                .unwrap_or_else(|error| {
                    let mut fragments = std::fs::read_dir("/etc/ld.so.conf.d")
                        .expect("enumerate refused loader fragments")
                        .filter_map(Result::ok)
                        .filter_map(|entry| entry.file_name().into_string().ok())
                        .filter(|name| name.ends_with(".conf"))
                        .map(|name| {
                            let bytes =
                                std::fs::read(Path::new("/etc/ld.so.conf.d").join(&name))
                                    .expect("read refused loader fragment");
                            let outcome = parse_linux_dynamic_loader_config(
                                &bytes,
                                architecture,
                                LinuxDynamicLoaderConfigRole::Fragment,
                            )
                            .map(|_| "accepted")
                            .unwrap_or_else(|error| match error.kind {
                                LinuxDynamicLoaderConfigErrorKind::Size => "size",
                                LinuxDynamicLoaderConfigErrorKind::Format => "format",
                                LinuxDynamicLoaderConfigErrorKind::UnsafeSearch => "unsafe_search",
                            });
                            (name, outcome)
                        })
                        .collect::<Vec<_>>();
                    fragments.sort_unstable();
                    panic!(
                        "observe live Noble loader state: kind={:?}, fragments={fragments:?}, preload_present={}",
                        error.kind,
                        Path::new("/etc/ld.so.preload").exists()
                    )
                });
        assert_eq!(
            observed.summary().disposition(),
            PersonalWorkerRuntimeLoaderStatePrerequisiteDisposition::ObservedPrerequisite
        );
        assert!(observed.summary().fragment_count() > 0);
        let debug = format!("{observed:?}");
        assert!(debug.contains(REDACTED));
        for forbidden in ["/etc", "ld.so", "sha256:", "example/runtime"] {
            assert!(!debug.contains(forbidden));
        }

        let fixture = Fixture::new();
        fixture.populate();
        let owner = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        let mut fixture_observation =
            observe_at(&fixture.root, owner, &project(), architecture, || {})
                .expect("observe copied loader state");
        fixture_observation
            .reconfirm()
            .expect("reconfirm stable loader state");
        let cache = fixture.etc().join(CACHE_FILE);
        stdfs::set_permissions(&cache, stdfs::Permissions::from_mode(0o0664))
            .expect("change cache mode after observation");
        let reconfirmed = fixture_observation
            .reconfirm()
            .expect_err("post-observation loader-state drift");
        assert_eq!(
            reconfirmed.kind,
            PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::ChangedDuringRead
        );
        stdfs::set_permissions(&cache, stdfs::Permissions::from_mode(0o0644))
            .expect("restore cache mode");

        let hidden_fragment = fixture.config_dir().join(".not-globbed.conf");
        stdfs::write(&hidden_fragment, b"/tmp/not-loader-authority\n")
            .expect("create hidden non-glob fragment");
        observe_at(&fixture.root, owner, &project(), architecture, || {})
            .expect("ignore hidden fragment outside the fixed glob");
        stdfs::remove_file(hidden_fragment).expect("remove hidden non-glob fragment");

        fixture.set_mode(ROOT_CONFIG, 0o0664);
        let writable = observe_at(&fixture.root, owner, &project(), architecture, || {})
            .expect_err("group-writable root loader configuration");
        assert_eq!(
            writable.kind,
            PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::UnsafeFilesystem
        );
        fixture.set_mode(ROOT_CONFIG, 0o0644);

        let preload = fixture.etc().join(PRELOAD_FILE);
        stdfs::write(&preload, b"forbidden\n").expect("create preload fixture");
        stdfs::set_permissions(&preload, stdfs::Permissions::from_mode(0o0644))
            .expect("set preload mode");
        let preloaded = observe_at(&fixture.root, owner, &project(), architecture, || {})
            .expect_err("preload presence");
        assert_eq!(
            preloaded.kind,
            PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::UnsafeConfiguration
        );
        stdfs::remove_file(preload).expect("remove preload fixture");

        let unsafe_fragment = fixture.config_dir().join("zz-unsafe.conf");
        stdfs::write(&unsafe_fragment, b"/tmp/unreviewed\n")
            .expect("create unsafe config fragment");
        stdfs::set_permissions(&unsafe_fragment, stdfs::Permissions::from_mode(0o0644))
            .expect("set unsafe config mode");
        let unsafe_search = observe_at(&fixture.root, owner, &project(), architecture, || {})
            .expect_err("unsafe loader search fragment");
        assert_eq!(
            unsafe_search.kind,
            PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::UnsafeConfiguration
        );
        stdfs::remove_file(unsafe_fragment).expect("remove unsafe config fragment");

        let changed = observe_at(&fixture.root, owner, &project(), architecture, || {
            stdfs::set_permissions(&cache, stdfs::Permissions::from_mode(0o0664))
                .expect("change cache mode during observation");
        })
        .expect_err("cache metadata drift");
        assert_eq!(
            changed.kind,
            PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::ChangedDuringRead
        );
        fixture.set_mode(CACHE_FILE, 0o0644);
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
                .join("target/r01-loader-state-fixtures");
            stdfs::create_dir_all(&parent).expect("create loader fixture parent");
            let parent_metadata =
                stdfs::symlink_metadata(&parent).expect("inspect loader fixture parent");
            assert!(
                parent_metadata.file_type().is_dir() && !parent_metadata.file_type().is_symlink(),
                "loader fixture parent must be a real directory"
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
                            .expect("inspect created loader fixture root");
                        let fixture = Self {
                            parent: parent.clone(),
                            parent_identity,
                            root,
                            root_identity: (root_metadata.dev(), root_metadata.ino()),
                        };
                        stdfs::create_dir_all(fixture.config_dir())
                            .expect("create loader config fixture directory");
                        return fixture;
                    }
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create private loader fixture root: {error}"),
                }
            }
            panic!("allocate unique loader fixture root")
        }

        fn populate(&self) {
            for name in [ROOT_CONFIG, CACHE_FILE] {
                stdfs::copy(Path::new("/etc").join(name), self.etc().join(name))
                    .unwrap_or_else(|error| panic!("copy loader fixture {name}: {error}"));
                self.set_mode(name, 0o0644);
            }
            for entry in
                stdfs::read_dir("/etc/ld.so.conf.d").expect("enumerate live loader fragments")
            {
                let entry = entry.expect("read live loader fragment entry");
                let name = entry.file_name();
                if !name.as_encoded_bytes().ends_with(b".conf") {
                    continue;
                }
                let destination = self.config_dir().join(&name);
                stdfs::copy(entry.path(), &destination).expect("copy loader fragment");
                stdfs::set_permissions(&destination, stdfs::Permissions::from_mode(0o0644))
                    .expect("set loader fragment mode");
            }
        }

        fn etc(&self) -> std::path::PathBuf {
            self.root.join("etc")
        }

        fn config_dir(&self) -> std::path::PathBuf {
            self.etc().join("ld.so.conf.d")
        }

        fn set_mode(&self, name: &str, mode: u32) {
            stdfs::set_permissions(self.etc().join(name), stdfs::Permissions::from_mode(mode))
                .unwrap_or_else(|error| panic!("set loader fixture mode for {name}: {error}"));
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            assert_eq!(self.root.parent(), Some(self.parent.as_path()));
            let parent_metadata =
                stdfs::symlink_metadata(&self.parent).expect("revalidate loader fixture parent");
            assert_eq!(
                (parent_metadata.dev(), parent_metadata.ino()),
                self.parent_identity,
                "loader fixture parent was rebound"
            );
            let root_metadata =
                stdfs::symlink_metadata(&self.root).expect("revalidate loader fixture root");
            assert!(
                root_metadata.file_type().is_dir() && !root_metadata.file_type().is_symlink(),
                "loader fixture root must remain a real directory"
            );
            assert_eq!(
                (root_metadata.dev(), root_metadata.ino()),
                self.root_identity,
                "loader fixture root was rebound"
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
