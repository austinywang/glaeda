use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

pub const LOCAL_INSTALL_CARGO_CONFIG_PREFLIGHT_SCHEMA_VERSION: u8 = 1;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const CONFIG_DIRECTORY: &str = ".cargo";
const MODERN_CONFIG: &str = "config.toml";
const LEGACY_CONFIG: &str = "config";

#[derive(Clone, PartialEq, Eq)]
pub struct LocalInstallCargoConfigPreflightContext {
    build_root: PathBuf,
    runner_uid: u32,
    runner_gid: u32,
}

impl LocalInstallCargoConfigPreflightContext {
    /// Bind one private isolated self-build root and the expected non-root owner.
    ///
    /// # Errors
    ///
    /// Returns an error unless the build root is one canonical absolute non-root UTF-8 path and
    /// the expected owner is a non-root user/group identity.
    pub fn new(
        build_root: impl Into<PathBuf>,
        runner_uid: u32,
        runner_gid: u32,
    ) -> Result<Self, LocalInstallCargoConfigPreflightError> {
        let build_root = canonical_private_path(build_root.into())?;
        if runner_uid == 0 || runner_gid == 0 {
            return Err(LocalInstallCargoConfigPreflightError::InvalidRunnerIdentity);
        }
        Ok(Self {
            build_root,
            runner_uid,
            runner_gid,
        })
    }

    fn working_directory(&self) -> PathBuf {
        self.build_root.join("work")
    }

    fn isolated_home(&self) -> PathBuf {
        self.build_root.join("home")
    }

    fn cargo_home(&self) -> PathBuf {
        self.build_root.join("cargo-home")
    }

    fn target_directory(&self) -> PathBuf {
        self.build_root.join("target")
    }
}

impl fmt::Debug for LocalInstallCargoConfigPreflightContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalInstallCargoConfigPreflightContext")
            .field("build_root", &"<private-isolated-build-root>")
            .field("runner_identity", &"<private-current-user-identity>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallBuildRootDisposition {
    Missing,
    Ready,
    Unsafe,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallCargoConfigDisposition {
    Absent,
    Present,
    Unsafe,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallCargoHomeConfigDisposition {
    Missing,
    Absent,
    Present,
    Unsafe,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallCargoConfigBlockingCode {
    BuildRootMissing,
    UnsafeBuildRoot,
    BuildRootUnknown,
    LineageConfigPresent,
    LineageUnsafe,
    LineageUnknown,
    CargoHomeMissing,
    CargoHomeConfigPresent,
    CargoHomeUnsafe,
    CargoHomeUnknown,
    ObservationChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallCargoConfigRepairCode {
    CreateIsolatedBuildRoot,
    CreateIsolatedCargoHome,
}

/// Path-private proof for one exact isolated Cargo lookup observation.
///
/// External crates may inspect and serialize the evidence through read-only accessors but cannot
/// construct or mutate the proof fields directly.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallCargoConfigPreflightReceipt {
    schema_version: u8,
    build_root: LocalInstallBuildRootDisposition,
    lineage_config: LocalInstallCargoConfigDisposition,
    cargo_home_config: LocalInstallCargoHomeConfigDisposition,
    ready: bool,
    blocking_codes: Vec<LocalInstallCargoConfigBlockingCode>,
    repair_codes: Vec<LocalInstallCargoConfigRepairCode>,
}

impl LocalInstallCargoConfigPreflightReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn build_root(&self) -> LocalInstallBuildRootDisposition {
        self.build_root
    }

    #[must_use]
    pub const fn lineage_config(&self) -> LocalInstallCargoConfigDisposition {
        self.lineage_config
    }

    #[must_use]
    pub const fn cargo_home_config(&self) -> LocalInstallCargoHomeConfigDisposition {
        self.cargo_home_config
    }

    #[must_use]
    pub const fn ready(&self) -> bool {
        self.ready
    }

    #[must_use]
    pub fn blocking_codes(&self) -> &[LocalInstallCargoConfigBlockingCode] {
        &self.blocking_codes
    }

    #[must_use]
    pub fn repair_codes(&self) -> &[LocalInstallCargoConfigRepairCode] {
        &self.repair_codes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallCargoConfigPreflightError {
    InvalidBuildRoot,
    InvalidRunnerIdentity,
}

impl fmt::Display for LocalInstallCargoConfigPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBuildRoot => "isolated Cargo preflight build root is invalid",
            Self::InvalidRunnerIdentity => "isolated Cargo preflight runner identity is invalid",
        })
    }
}

impl std::error::Error for LocalInstallCargoConfigPreflightError {}

/// Prove the isolated self-build Cargo lookup path from direct filesystem evidence.
///
/// The observation runs twice. Exact private directories and config files retain change-time
/// evidence; shared lineage directories retain stable entry identity without treating unrelated
/// sibling churn as configuration drift. Any relevant difference becomes one bounded
/// `observation_changed` refusal. No source-checkout or personal-home Cargo path is inspected
/// unless it is literally an ancestor of the selected isolated build root.
#[must_use]
pub fn observe_local_install_cargo_config_preflight(
    context: &LocalInstallCargoConfigPreflightContext,
) -> LocalInstallCargoConfigPreflightReceipt {
    observe_with(context, &UnixCargoConfigFilesystem)
}

fn observe_with(
    context: &LocalInstallCargoConfigPreflightContext,
    filesystem: &impl CargoConfigFilesystem,
) -> LocalInstallCargoConfigPreflightReceipt {
    let first = snapshot(context, filesystem);
    let second = snapshot(context, filesystem);
    if !first.same_as(&second) {
        return changed_receipt();
    }
    first.public_receipt()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PrivateSnapshot {
    build_root: DirectoryObservation,
    work: DirectoryObservation,
    home: DirectoryObservation,
    target: DirectoryObservation,
    lineage: Vec<LineageObservation>,
    cargo_home: CargoHomeObservation,
}

impl PrivateSnapshot {
    fn same_as(&self, other: &Self) -> bool {
        self.build_root.same_exact_as(&other.build_root)
            && self.work.same_exact_as(&other.work)
            && self.home.same_exact_as(&other.home)
            && self.target.same_exact_as(&other.target)
            && self.lineage.len() == other.lineage.len()
            && self
                .lineage
                .iter()
                .zip(&other.lineage)
                .all(|(left, right)| left.same_as(right))
            && self.cargo_home.same_as(&other.cargo_home)
    }

    fn public_receipt(&self) -> LocalInstallCargoConfigPreflightReceipt {
        let build_root = build_root_disposition(self);
        let lineage_config = lineage_disposition(&self.lineage);
        let cargo_home_config = cargo_home_disposition(&self.cargo_home);
        let ready = build_root == LocalInstallBuildRootDisposition::Ready
            && lineage_config == LocalInstallCargoConfigDisposition::Absent
            && cargo_home_config == LocalInstallCargoHomeConfigDisposition::Absent;

        let mut blocking_codes = BTreeSet::new();
        let mut repair_codes = BTreeSet::new();
        match build_root {
            LocalInstallBuildRootDisposition::Missing => {
                blocking_codes.insert(LocalInstallCargoConfigBlockingCode::BuildRootMissing);
                repair_codes.insert(LocalInstallCargoConfigRepairCode::CreateIsolatedBuildRoot);
            }
            LocalInstallBuildRootDisposition::Unsafe => {
                blocking_codes.insert(LocalInstallCargoConfigBlockingCode::UnsafeBuildRoot);
            }
            LocalInstallBuildRootDisposition::Unknown => {
                blocking_codes.insert(LocalInstallCargoConfigBlockingCode::BuildRootUnknown);
            }
            LocalInstallBuildRootDisposition::Ready => {
                match lineage_config {
                    LocalInstallCargoConfigDisposition::Present => {
                        blocking_codes
                            .insert(LocalInstallCargoConfigBlockingCode::LineageConfigPresent);
                    }
                    LocalInstallCargoConfigDisposition::Unsafe => {
                        blocking_codes.insert(LocalInstallCargoConfigBlockingCode::LineageUnsafe);
                    }
                    LocalInstallCargoConfigDisposition::Unknown => {
                        blocking_codes.insert(LocalInstallCargoConfigBlockingCode::LineageUnknown);
                    }
                    LocalInstallCargoConfigDisposition::Absent => {}
                }
                match cargo_home_config {
                    LocalInstallCargoHomeConfigDisposition::Missing => {
                        blocking_codes
                            .insert(LocalInstallCargoConfigBlockingCode::CargoHomeMissing);
                        repair_codes
                            .insert(LocalInstallCargoConfigRepairCode::CreateIsolatedCargoHome);
                    }
                    LocalInstallCargoHomeConfigDisposition::Present => {
                        blocking_codes
                            .insert(LocalInstallCargoConfigBlockingCode::CargoHomeConfigPresent);
                    }
                    LocalInstallCargoHomeConfigDisposition::Unsafe => {
                        blocking_codes.insert(LocalInstallCargoConfigBlockingCode::CargoHomeUnsafe);
                    }
                    LocalInstallCargoHomeConfigDisposition::Unknown => {
                        blocking_codes
                            .insert(LocalInstallCargoConfigBlockingCode::CargoHomeUnknown);
                    }
                    LocalInstallCargoHomeConfigDisposition::Absent => {}
                }
            }
        }

        LocalInstallCargoConfigPreflightReceipt {
            schema_version: LOCAL_INSTALL_CARGO_CONFIG_PREFLIGHT_SCHEMA_VERSION,
            build_root,
            lineage_config,
            cargo_home_config,
            ready,
            blocking_codes: blocking_codes.into_iter().collect(),
            repair_codes: repair_codes.into_iter().collect(),
        }
    }
}

fn changed_receipt() -> LocalInstallCargoConfigPreflightReceipt {
    LocalInstallCargoConfigPreflightReceipt {
        schema_version: LOCAL_INSTALL_CARGO_CONFIG_PREFLIGHT_SCHEMA_VERSION,
        build_root: LocalInstallBuildRootDisposition::Unknown,
        lineage_config: LocalInstallCargoConfigDisposition::Unknown,
        cargo_home_config: LocalInstallCargoHomeConfigDisposition::Unknown,
        ready: false,
        blocking_codes: vec![LocalInstallCargoConfigBlockingCode::ObservationChanged],
        repair_codes: vec![],
    }
}

fn snapshot(
    context: &LocalInstallCargoConfigPreflightContext,
    filesystem: &impl CargoConfigFilesystem,
) -> PrivateSnapshot {
    let build_root = filesystem.directory(
        &context.build_root,
        DirectoryExpectation::ExactPrivateRunner,
        context,
    );
    let work = filesystem.directory(
        &context.working_directory(),
        DirectoryExpectation::ExactPrivateRunner,
        context,
    );
    let home = filesystem.directory(
        &context.isolated_home(),
        DirectoryExpectation::ExactPrivateRunner,
        context,
    );
    let target = filesystem.directory(
        &context.target_directory(),
        DirectoryExpectation::ExactPrivateRunner,
        context,
    );

    let layout_is_safe = build_root.is_ready()
        && !work.is_unsafe_or_unknown()
        && !home.is_unsafe_or_unknown()
        && !target.is_unsafe_or_unknown();
    let lineage = if layout_is_safe {
        observe_lineage(context, filesystem)
    } else {
        vec![]
    };
    let cargo_home = if build_root.is_ready() {
        observe_cargo_home(context, filesystem)
    } else {
        CargoHomeObservation::missing()
    };

    PrivateSnapshot {
        build_root,
        work,
        home,
        target,
        lineage,
        cargo_home,
    }
}

fn observe_lineage(
    context: &LocalInstallCargoConfigPreflightContext,
    filesystem: &impl CargoConfigFilesystem,
) -> Vec<LineageObservation> {
    let working_directory = context.working_directory();
    working_directory
        .ancestors()
        .map(|ancestor| {
            let exact_private =
                ancestor == working_directory.as_path() || ancestor == context.build_root.as_path();
            let ancestor_state = filesystem.directory(
                ancestor,
                if exact_private {
                    DirectoryExpectation::ExactPrivateRunner
                } else {
                    DirectoryExpectation::TrustedAncestor
                },
                context,
            );
            let cargo_path = ancestor.join(CONFIG_DIRECTORY);
            let cargo_directory = if ancestor_state.is_ready() {
                filesystem.directory(&cargo_path, DirectoryExpectation::TrustedAncestor, context)
            } else {
                DirectoryObservation::Missing
            };
            let (modern, legacy) = if cargo_directory.is_ready() {
                (
                    filesystem.config_file(&cargo_path.join(MODERN_CONFIG), context),
                    filesystem.config_file(&cargo_path.join(LEGACY_CONFIG), context),
                )
            } else {
                (ConfigObservation::Missing, ConfigObservation::Missing)
            };
            LineageObservation {
                ancestor: ancestor_state,
                cargo_directory,
                modern,
                legacy,
            }
        })
        .collect()
}

fn observe_cargo_home(
    context: &LocalInstallCargoConfigPreflightContext,
    filesystem: &impl CargoConfigFilesystem,
) -> CargoHomeObservation {
    let cargo_home = context.cargo_home();
    let directory = filesystem.directory(
        &cargo_home,
        DirectoryExpectation::ExactPrivateRunner,
        context,
    );
    let (modern, legacy) = if directory.is_ready() {
        (
            filesystem.config_file(&cargo_home.join(MODERN_CONFIG), context),
            filesystem.config_file(&cargo_home.join(LEGACY_CONFIG), context),
        )
    } else {
        (ConfigObservation::Missing, ConfigObservation::Missing)
    };
    CargoHomeObservation {
        directory,
        modern,
        legacy,
    }
}

fn build_root_disposition(snapshot: &PrivateSnapshot) -> LocalInstallBuildRootDisposition {
    match snapshot.build_root {
        DirectoryObservation::Missing => LocalInstallBuildRootDisposition::Missing,
        DirectoryObservation::Unsafe => LocalInstallBuildRootDisposition::Unsafe,
        DirectoryObservation::Unknown => LocalInstallBuildRootDisposition::Unknown,
        DirectoryObservation::Ready(_) => {
            if [snapshot.work, snapshot.home, snapshot.target]
                .iter()
                .any(|state| matches!(state, DirectoryObservation::Unsafe))
            {
                LocalInstallBuildRootDisposition::Unsafe
            } else if [snapshot.work, snapshot.home, snapshot.target]
                .iter()
                .any(|state| matches!(state, DirectoryObservation::Unknown))
            {
                LocalInstallBuildRootDisposition::Unknown
            } else {
                LocalInstallBuildRootDisposition::Ready
            }
        }
    }
}

fn lineage_disposition(lineage: &[LineageObservation]) -> LocalInstallCargoConfigDisposition {
    if lineage.is_empty() {
        return LocalInstallCargoConfigDisposition::Unknown;
    }
    if lineage.iter().any(LineageObservation::unsafe_evidence) {
        return LocalInstallCargoConfigDisposition::Unsafe;
    }
    if lineage.iter().any(LineageObservation::unknown_evidence) {
        return LocalInstallCargoConfigDisposition::Unknown;
    }
    if lineage.iter().any(LineageObservation::present_evidence) {
        return LocalInstallCargoConfigDisposition::Present;
    }
    LocalInstallCargoConfigDisposition::Absent
}

fn cargo_home_disposition(
    observation: &CargoHomeObservation,
) -> LocalInstallCargoHomeConfigDisposition {
    match observation.directory {
        DirectoryObservation::Missing => LocalInstallCargoHomeConfigDisposition::Missing,
        DirectoryObservation::Unsafe => LocalInstallCargoHomeConfigDisposition::Unsafe,
        DirectoryObservation::Unknown => LocalInstallCargoHomeConfigDisposition::Unknown,
        DirectoryObservation::Ready(_) => {
            if observation.modern.is_unsafe() || observation.legacy.is_unsafe() {
                LocalInstallCargoHomeConfigDisposition::Unsafe
            } else if observation.modern.is_unknown() || observation.legacy.is_unknown() {
                LocalInstallCargoHomeConfigDisposition::Unknown
            } else if observation.modern.is_present() || observation.legacy.is_present() {
                LocalInstallCargoHomeConfigDisposition::Present
            } else {
                LocalInstallCargoHomeConfigDisposition::Absent
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectoryExpectation {
    ExactPrivateRunner,
    TrustedAncestor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectIdentity {
    device: i128,
    inode: u128,
    uid: u32,
    gid: u32,
    mode: u64,
    change_seconds: i128,
    change_nanoseconds: i128,
}

impl ObjectIdentity {
    fn same_path_entry_as(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.uid == other.uid
            && self.gid == other.gid
            && self.mode == other.mode
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectoryObservation {
    Missing,
    Ready(ObjectIdentity),
    Unsafe,
    Unknown,
}

impl DirectoryObservation {
    fn is_ready(self) -> bool {
        matches!(self, Self::Ready(_))
    }

    fn is_unsafe_or_unknown(self) -> bool {
        matches!(self, Self::Unsafe | Self::Unknown)
    }

    fn same_exact_as(&self, other: &Self) -> bool {
        self == other
    }

    fn same_path_entry_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ready(left), Self::Ready(right)) => left.same_path_entry_as(right),
            _ => self == other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigObservation {
    Missing,
    Present(ObjectIdentity),
    Unsafe,
    Unknown,
}

impl ConfigObservation {
    fn is_present(self) -> bool {
        matches!(self, Self::Present(_))
    }

    fn is_unsafe(self) -> bool {
        matches!(self, Self::Unsafe)
    }

    fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LineageObservation {
    ancestor: DirectoryObservation,
    cargo_directory: DirectoryObservation,
    modern: ConfigObservation,
    legacy: ConfigObservation,
}

impl LineageObservation {
    fn same_as(&self, other: &Self) -> bool {
        self.ancestor.same_path_entry_as(&other.ancestor)
            && self
                .cargo_directory
                .same_path_entry_as(&other.cargo_directory)
            && self.modern == other.modern
            && self.legacy == other.legacy
    }

    fn unsafe_evidence(&self) -> bool {
        matches!(
            (
                self.ancestor,
                self.cargo_directory,
                self.modern,
                self.legacy
            ),
            (DirectoryObservation::Unsafe, _, _, _)
                | (_, DirectoryObservation::Unsafe, _, _)
                | (_, _, ConfigObservation::Unsafe, _)
                | (_, _, _, ConfigObservation::Unsafe)
        )
    }

    fn unknown_evidence(&self) -> bool {
        matches!(
            (
                self.ancestor,
                self.cargo_directory,
                self.modern,
                self.legacy
            ),
            (DirectoryObservation::Unknown, _, _, _)
                | (_, DirectoryObservation::Unknown, _, _)
                | (_, _, ConfigObservation::Unknown, _)
                | (_, _, _, ConfigObservation::Unknown)
        )
    }

    fn present_evidence(&self) -> bool {
        self.modern.is_present() || self.legacy.is_present()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CargoHomeObservation {
    directory: DirectoryObservation,
    modern: ConfigObservation,
    legacy: ConfigObservation,
}

impl CargoHomeObservation {
    fn missing() -> Self {
        Self {
            directory: DirectoryObservation::Missing,
            modern: ConfigObservation::Missing,
            legacy: ConfigObservation::Missing,
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        self.directory.same_exact_as(&other.directory)
            && self.modern == other.modern
            && self.legacy == other.legacy
    }
}

trait CargoConfigFilesystem {
    fn directory(
        &self,
        path: &Path,
        expectation: DirectoryExpectation,
        context: &LocalInstallCargoConfigPreflightContext,
    ) -> DirectoryObservation;

    fn config_file(
        &self,
        path: &Path,
        context: &LocalInstallCargoConfigPreflightContext,
    ) -> ConfigObservation;
}

struct UnixCargoConfigFilesystem;

impl CargoConfigFilesystem for UnixCargoConfigFilesystem {
    fn directory(
        &self,
        path: &Path,
        expectation: DirectoryExpectation,
        context: &LocalInstallCargoConfigPreflightContext,
    ) -> DirectoryObservation {
        inspect_directory(path, expectation, context)
    }

    fn config_file(
        &self,
        path: &Path,
        context: &LocalInstallCargoConfigPreflightContext,
    ) -> ConfigObservation {
        inspect_config_file(path, context)
    }
}

fn inspect_directory(
    path: &Path,
    expectation: DirectoryExpectation,
    context: &LocalInstallCargoConfigPreflightContext,
) -> DirectoryObservation {
    let components = normal_components(path);
    if components.is_empty() && path != Path::new("/") {
        return DirectoryObservation::Unsafe;
    }
    let mut directory = match fs::open("/", DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => directory,
        Err(_) => return DirectoryObservation::Unknown,
    };
    if path == Path::new("/") {
        return inspect_open_directory(&directory, DirectoryExpectation::TrustedAncestor, context);
    }
    for (index, component) in components.iter().copied().enumerate() {
        let opened = match fs::openat(directory.as_fd(), component, DIRECTORY_FLAGS, Mode::empty())
        {
            Ok(opened) => opened,
            Err(Errno::NOENT) => return DirectoryObservation::Missing,
            Err(Errno::LOOP | Errno::NOTDIR) => return DirectoryObservation::Unsafe,
            Err(_) => return DirectoryObservation::Unknown,
        };
        let leaf = index + 1 == components.len();
        let inspected = inspect_open_directory(
            &opened,
            if leaf {
                expectation
            } else {
                DirectoryExpectation::TrustedAncestor
            },
            context,
        );
        if !inspected.is_ready() {
            return inspected;
        }
        directory = opened;
    }
    inspect_open_directory(&directory, expectation, context)
}

fn inspect_open_directory(
    directory: &OwnedFd,
    expectation: DirectoryExpectation,
    context: &LocalInstallCargoConfigPreflightContext,
) -> DirectoryObservation {
    let stat = match fs::fstat(directory) {
        Ok(stat) => stat,
        Err(_) => return DirectoryObservation::Unknown,
    };
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return DirectoryObservation::Unsafe;
    }
    let mode = stat.st_mode & 0o7777;
    let metadata_ok = match expectation {
        DirectoryExpectation::ExactPrivateRunner => {
            stat.st_uid == context.runner_uid
                && stat.st_gid == context.runner_gid
                && u64::from(mode) == u64::from(PRIVATE_DIRECTORY_MODE)
        }
        DirectoryExpectation::TrustedAncestor => {
            trusted_ancestor_is_ready(stat.st_uid, stat.st_gid, u64::from(mode), context)
        }
    };
    if !metadata_ok {
        return DirectoryObservation::Unsafe;
    }
    DirectoryObservation::Ready(ObjectIdentity {
        device: i128::from(stat.st_dev),
        inode: u128::from(stat.st_ino),
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: u64::from(mode),
        change_seconds: i128::from(stat.st_ctime),
        change_nanoseconds: i128::from(stat.st_ctime_nsec),
    })
}

fn inspect_config_file(
    path: &Path,
    context: &LocalInstallCargoConfigPreflightContext,
) -> ConfigObservation {
    let components = normal_components(path);
    let Some((&file_name, parents)) = components.split_last() else {
        return ConfigObservation::Unsafe;
    };
    let mut directory = match fs::open("/", DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => directory,
        Err(_) => return ConfigObservation::Unknown,
    };
    for &component in parents {
        let opened = match fs::openat(directory.as_fd(), component, DIRECTORY_FLAGS, Mode::empty())
        {
            Ok(opened) => opened,
            Err(Errno::NOENT) => return ConfigObservation::Missing,
            Err(Errno::LOOP | Errno::NOTDIR) => return ConfigObservation::Unsafe,
            Err(_) => return ConfigObservation::Unknown,
        };
        match inspect_open_directory(&opened, DirectoryExpectation::TrustedAncestor, context) {
            DirectoryObservation::Ready(_) => directory = opened,
            DirectoryObservation::Unknown => return ConfigObservation::Unknown,
            DirectoryObservation::Missing | DirectoryObservation::Unsafe => {
                return ConfigObservation::Unsafe;
            }
        }
    }
    let file = match fs::openat(directory.as_fd(), file_name, FILE_FLAGS, Mode::empty()) {
        Ok(file) => file,
        Err(Errno::NOENT) => return ConfigObservation::Missing,
        Err(Errno::LOOP | Errno::NOTDIR | Errno::ISDIR) => return ConfigObservation::Unsafe,
        Err(_) => return ConfigObservation::Unknown,
    };
    let stat = match fs::fstat(&file) {
        Ok(stat) => stat,
        Err(_) => return ConfigObservation::Unknown,
    };
    let mode = stat.st_mode & 0o7777;
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || !trusted_owner(stat.st_uid, stat.st_gid, context)
        || mode & 0o022 != 0
    {
        return ConfigObservation::Unsafe;
    }
    ConfigObservation::Present(ObjectIdentity {
        device: i128::from(stat.st_dev),
        inode: u128::from(stat.st_ino),
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: u64::from(mode),
        change_seconds: i128::from(stat.st_ctime),
        change_nanoseconds: i128::from(stat.st_ctime_nsec),
    })
}

fn trusted_owner(uid: u32, gid: u32, context: &LocalInstallCargoConfigPreflightContext) -> bool {
    uid == 0 || (uid == context.runner_uid && gid == context.runner_gid)
}

fn trusted_ancestor_is_ready(
    uid: u32,
    gid: u32,
    mode: u64,
    context: &LocalInstallCargoConfigPreflightContext,
) -> bool {
    trusted_owner(uid, gid, context)
        && mode & 0o002 == 0
        && (mode & 0o020 == 0 || (uid == context.runner_uid && gid == context.runner_gid))
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

fn canonical_private_path(path: PathBuf) -> Result<PathBuf, LocalInstallCargoConfigPreflightError> {
    let Some(value) = path.to_str() else {
        return Err(LocalInstallCargoConfigPreflightError::InvalidBuildRoot);
    };
    if value.is_empty()
        || value == "/"
        || value.len() > 4_096
        || value.ends_with('/')
        || value.contains("//")
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(LocalInstallCargoConfigPreflightError::InvalidBuildRoot);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    struct FakeFilesystem {
        directories: RefCell<BTreeMap<PathBuf, DirectoryObservation>>,
        files: RefCell<BTreeMap<PathBuf, ConfigObservation>>,
        queries: RefCell<Vec<PathBuf>>,
        mutation_after_queries: Cell<Option<usize>>,
        query_count: Cell<usize>,
    }

    impl FakeFilesystem {
        fn new() -> Self {
            Self {
                directories: RefCell::new(BTreeMap::new()),
                files: RefCell::new(BTreeMap::new()),
                queries: RefCell::new(vec![]),
                mutation_after_queries: Cell::new(None),
                query_count: Cell::new(0),
            }
        }

        fn directory(self, path: &str, state: DirectoryObservation) -> Self {
            self.directories.borrow_mut().insert(path.into(), state);
            self
        }

        fn file(self, path: &str, state: ConfigObservation) -> Self {
            self.files.borrow_mut().insert(path.into(), state);
            self
        }

        fn mutate_after_queries(self, count: usize) -> Self {
            self.mutation_after_queries.set(Some(count));
            self
        }

        fn record(&self, path: &Path) {
            self.queries.borrow_mut().push(path.to_path_buf());
            let count = self.query_count.get() + 1;
            self.query_count.set(count);
            if self.mutation_after_queries.get() == Some(count) {
                self.files.borrow_mut().insert(
                    "/var/lib/smolrunner-build/.cargo/config.toml".into(),
                    present(99),
                );
            }
        }
    }

    impl CargoConfigFilesystem for FakeFilesystem {
        fn directory(
            &self,
            path: &Path,
            _expectation: DirectoryExpectation,
            _context: &LocalInstallCargoConfigPreflightContext,
        ) -> DirectoryObservation {
            self.record(path);
            self.directories
                .borrow()
                .get(path)
                .copied()
                .unwrap_or_else(|| ready_directory(path))
        }

        fn config_file(
            &self,
            path: &Path,
            _context: &LocalInstallCargoConfigPreflightContext,
        ) -> ConfigObservation {
            self.record(path);
            self.files
                .borrow()
                .get(path)
                .copied()
                .unwrap_or(ConfigObservation::Missing)
        }
    }

    fn context() -> LocalInstallCargoConfigPreflightContext {
        LocalInstallCargoConfigPreflightContext::new("/var/lib/smolrunner-build", 501, 20).unwrap()
    }

    #[test]
    fn runner_owned_primary_group_write_does_not_add_ambient_authority() {
        let context = context();
        assert!(trusted_ancestor_is_ready(501, 20, 0o775, &context));
        assert!(!trusted_ancestor_is_ready(0, 0, 0o775, &context));
        assert!(!trusted_ancestor_is_ready(501, 20, 0o777, &context));
        assert!(!trusted_ancestor_is_ready(777, 20, 0o755, &context));
    }

    #[test]
    fn root_owned_system_group_is_trusted_only_without_group_write() {
        let context = context();
        assert!(trusted_owner(0, 80, &context));
        assert!(trusted_ancestor_is_ready(0, 80, 0o755, &context));
        assert!(!trusted_ancestor_is_ready(0, 80, 0o775, &context));
    }

    fn identity(seed: u64) -> ObjectIdentity {
        ObjectIdentity {
            device: 1,
            inode: u128::from(seed),
            uid: 501,
            gid: 20,
            mode: u64::from(PRIVATE_DIRECTORY_MODE),
            change_seconds: i128::from(seed),
            change_nanoseconds: 0,
        }
    }

    fn ready_directory(path: &Path) -> DirectoryObservation {
        let seed = path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(17_u64, |value, byte| {
                value.wrapping_mul(31) + u64::from(*byte)
            });
        DirectoryObservation::Ready(identity(seed))
    }

    fn present(seed: u64) -> ConfigObservation {
        ConfigObservation::Present(identity(seed))
    }

    #[test]
    fn empty_private_layout_is_ready_and_path_private() {
        let filesystem = FakeFilesystem::new();
        let receipt = observe_with(&context(), &filesystem);
        assert!(receipt.ready());
        assert_eq!(
            receipt.build_root(),
            LocalInstallBuildRootDisposition::Ready
        );
        assert_eq!(
            receipt.lineage_config(),
            LocalInstallCargoConfigDisposition::Absent
        );
        assert_eq!(
            receipt.cargo_home_config(),
            LocalInstallCargoHomeConfigDisposition::Absent
        );
        assert!(receipt.blocking_codes().is_empty());
        assert!(receipt.repair_codes().is_empty());
        assert_eq!(
            receipt.schema_version(),
            LOCAL_INSTALL_CARGO_CONFIG_PREFLIGHT_SCHEMA_VERSION
        );

        let public = serde_json::to_string(&receipt).unwrap();
        assert!(!public.contains("/var/lib"));
        assert!(!public.contains("501"));
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            json!({
                "schema_version": 1,
                "build_root": "ready",
                "lineage_config": "absent",
                "cargo_home_config": "absent",
                "ready": true,
                "blocking_codes": [],
                "repair_codes": []
            })
        );
    }

    #[test]
    fn missing_build_root_is_one_root_cause_repair() {
        let filesystem = FakeFilesystem::new()
            .directory("/var/lib/smolrunner-build", DirectoryObservation::Missing);
        let receipt = observe_with(&context(), &filesystem);
        assert!(!receipt.ready);
        assert_eq!(
            receipt.build_root,
            LocalInstallBuildRootDisposition::Missing
        );
        assert_eq!(
            receipt.blocking_codes,
            vec![LocalInstallCargoConfigBlockingCode::BuildRootMissing]
        );
        assert_eq!(
            receipt.repair_codes,
            vec![LocalInstallCargoConfigRepairCode::CreateIsolatedBuildRoot]
        );
    }

    #[test]
    fn modern_or_legacy_config_anywhere_in_lineage_blocks() {
        for name in [MODERN_CONFIG, LEGACY_CONFIG] {
            let filesystem = FakeFilesystem::new().file(
                &format!("/var/lib/smolrunner-build/work/.cargo/{name}"),
                present(44),
            );
            let receipt = observe_with(&context(), &filesystem);
            assert_eq!(
                receipt.lineage_config,
                LocalInstallCargoConfigDisposition::Present
            );
            assert!(
                receipt
                    .blocking_codes
                    .contains(&LocalInstallCargoConfigBlockingCode::LineageConfigPresent)
            );
        }

        let filesystem = FakeFilesystem::new().file("/.cargo/config.toml", present(45));
        assert_eq!(
            observe_with(&context(), &filesystem).lineage_config,
            LocalInstallCargoConfigDisposition::Present
        );
    }

    #[test]
    fn unsafe_lookup_objects_win_over_present_evidence() {
        let filesystem = FakeFilesystem::new()
            .directory(
                "/var/lib/smolrunner-build/.cargo",
                DirectoryObservation::Unsafe,
            )
            .file("/var/.cargo/config", present(50));
        let receipt = observe_with(&context(), &filesystem);
        assert_eq!(
            receipt.lineage_config,
            LocalInstallCargoConfigDisposition::Unsafe
        );

        let unsafe_file = FakeFilesystem::new().file(
            "/var/lib/smolrunner-build/work/.cargo/config.toml",
            ConfigObservation::Unsafe,
        );
        assert_eq!(
            observe_with(&context(), &unsafe_file).lineage_config,
            LocalInstallCargoConfigDisposition::Unsafe
        );
    }

    #[test]
    fn cargo_home_missing_present_unsafe_and_unknown_are_distinct() {
        let missing = FakeFilesystem::new().directory(
            "/var/lib/smolrunner-build/cargo-home",
            DirectoryObservation::Missing,
        );
        let missing_receipt = observe_with(&context(), &missing);
        assert_eq!(
            missing_receipt.cargo_home_config,
            LocalInstallCargoHomeConfigDisposition::Missing
        );
        assert_eq!(
            missing_receipt.repair_codes,
            vec![LocalInstallCargoConfigRepairCode::CreateIsolatedCargoHome]
        );

        let present_fs = FakeFilesystem::new().file(
            "/var/lib/smolrunner-build/cargo-home/config.toml",
            present(60),
        );
        assert_eq!(
            observe_with(&context(), &present_fs).cargo_home_config,
            LocalInstallCargoHomeConfigDisposition::Present
        );

        let unsafe_fs = FakeFilesystem::new().directory(
            "/var/lib/smolrunner-build/cargo-home",
            DirectoryObservation::Unsafe,
        );
        assert_eq!(
            observe_with(&context(), &unsafe_fs).cargo_home_config,
            LocalInstallCargoHomeConfigDisposition::Unsafe
        );

        let unknown_fs = FakeFilesystem::new().directory(
            "/var/lib/smolrunner-build/cargo-home",
            DirectoryObservation::Unknown,
        );
        assert_eq!(
            observe_with(&context(), &unknown_fs).cargo_home_config,
            LocalInstallCargoHomeConfigDisposition::Unknown
        );
    }

    #[test]
    fn unsafe_existing_layout_child_blocks_only_at_build_root_boundary() {
        for child in ["work", "home", "target"] {
            let filesystem = FakeFilesystem::new().directory(
                &format!("/var/lib/smolrunner-build/{child}"),
                DirectoryObservation::Unsafe,
            );
            let receipt = observe_with(&context(), &filesystem);
            assert_eq!(receipt.build_root, LocalInstallBuildRootDisposition::Unsafe);
            assert_eq!(
                receipt.blocking_codes,
                vec![LocalInstallCargoConfigBlockingCode::UnsafeBuildRoot]
            );
            assert!(!receipt.ready);
        }
    }

    #[test]
    fn unrelated_source_and_personal_cargo_paths_are_never_queried() {
        let filesystem = FakeFilesystem::new()
            .file("/secret-source/.cargo/config.toml", present(70))
            .file("/home/operator/.cargo/config", present(71));
        let receipt = observe_with(&context(), &filesystem);
        assert!(receipt.ready);
        let queries = filesystem.queries.borrow();
        assert!(
            queries
                .iter()
                .all(|path| !path.starts_with("/secret-source"))
        );
        assert!(
            queries
                .iter()
                .all(|path| !path.starts_with("/home/operator"))
        );
    }

    #[test]
    fn changed_private_snapshot_fails_closed_without_private_evidence() {
        let filesystem = FakeFilesystem::new().mutate_after_queries(30);
        let receipt = observe_with(&context(), &filesystem);
        assert!(!receipt.ready);
        assert_eq!(
            receipt.blocking_codes,
            vec![LocalInstallCargoConfigBlockingCode::ObservationChanged]
        );
        assert_eq!(
            receipt.build_root,
            LocalInstallBuildRootDisposition::Unknown
        );
        let public = serde_json::to_string(&receipt).unwrap();
        assert!(!public.contains("smolrunner-build"));
        assert!(!public.contains("99"));
    }

    #[test]
    fn stable_observation_is_deterministic() {
        let filesystem = FakeFilesystem::new();
        let first = observe_with(&context(), &filesystem);
        let second = observe_with(&context(), &filesystem);
        assert_eq!(first, second);
    }

    #[test]
    fn context_rejects_root_relative_and_root_identity() {
        assert_eq!(
            LocalInstallCargoConfigPreflightContext::new("/", 501, 20).unwrap_err(),
            LocalInstallCargoConfigPreflightError::InvalidBuildRoot
        );
        assert_eq!(
            LocalInstallCargoConfigPreflightContext::new("relative", 501, 20).unwrap_err(),
            LocalInstallCargoConfigPreflightError::InvalidBuildRoot
        );
        assert_eq!(
            LocalInstallCargoConfigPreflightContext::new("/var/lib/build", 0, 20).unwrap_err(),
            LocalInstallCargoConfigPreflightError::InvalidRunnerIdentity
        );
    }
}
