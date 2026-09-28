use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};

use rustix::fs::{self, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{getegid, geteuid};
use serde::Serialize;

use super::LocalInstallBuildCommandContext;

pub const LOCAL_INSTALL_DIRECTORY_PREFLIGHT_SCHEMA_VERSION: u8 = 1;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallDerivedDirectoryDisposition {
    Ready,
    Missing,
    Unsafe,
    Unknown,
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallDirectoryBlockingCode {
    BuildRootMissing,
    BuildRootUnsafe,
    BuildRootUnknown,
    BuildRootChanged,
    WorkUnsafe,
    WorkUnknown,
    WorkChanged,
    HomeUnsafe,
    HomeUnknown,
    HomeChanged,
    CargoHomeUnsafe,
    CargoHomeUnknown,
    CargoHomeChanged,
    TargetUnsafe,
    TargetUnknown,
    TargetChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallDirectoryRepairCode {
    CreateWork,
    CreateHome,
    CreateCargoHome,
    CreateTarget,
}

/// Path-private classification of the four exact directories derived beneath one self-build root.
///
/// Fields remain private so readiness and repairability cannot be edited after observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallDirectoryPreflightReceipt {
    schema_version: u8,
    work: LocalInstallDerivedDirectoryDisposition,
    home: LocalInstallDerivedDirectoryDisposition,
    cargo_home: LocalInstallDerivedDirectoryDisposition,
    target: LocalInstallDerivedDirectoryDisposition,
    ready: bool,
    repairable: bool,
    blocking_codes: Vec<LocalInstallDirectoryBlockingCode>,
    repair_codes: Vec<LocalInstallDirectoryRepairCode>,
}

impl LocalInstallDirectoryPreflightReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn work(&self) -> LocalInstallDerivedDirectoryDisposition {
        self.work
    }

    #[must_use]
    pub const fn home(&self) -> LocalInstallDerivedDirectoryDisposition {
        self.home
    }

    #[must_use]
    pub const fn cargo_home(&self) -> LocalInstallDerivedDirectoryDisposition {
        self.cargo_home
    }

    #[must_use]
    pub const fn target(&self) -> LocalInstallDerivedDirectoryDisposition {
        self.target
    }

    #[must_use]
    pub const fn ready(&self) -> bool {
        self.ready
    }

    #[must_use]
    pub const fn repairable(&self) -> bool {
        self.repairable
    }

    #[must_use]
    pub fn blocking_codes(&self) -> &[LocalInstallDirectoryBlockingCode] {
        &self.blocking_codes
    }

    #[must_use]
    pub fn repair_codes(&self) -> &[LocalInstallDirectoryRepairCode] {
        &self.repair_codes
    }
}

/// Observe only the four exact private directories derived by the accepted build-command context.
///
/// Cargo configuration is deliberately outside this observer. The build root itself is required to
/// be present and private before a missing child becomes a bounded repair requirement. A missing or
/// unprovable build root leaves child creation unavailable to this slice.
#[must_use]
pub fn observe_local_install_directory_preflight(
    context: &LocalInstallBuildCommandContext,
) -> LocalInstallDirectoryPreflightReceipt {
    observe_with(context, || {})
}

fn observe_with(
    context: &LocalInstallBuildCommandContext,
    between_snapshots: impl FnOnce(),
) -> LocalInstallDirectoryPreflightReceipt {
    let first = snapshot(context);
    between_snapshots();
    let second = snapshot(context);
    public_receipt(&first, &second)
}

#[derive(Debug, Clone)]
struct DirectorySnapshot {
    root: BuildRootObservation,
    work: ChildObservation,
    home: ChildObservation,
    cargo_home: ChildObservation,
    target: ChildObservation,
}

#[derive(Debug, Clone)]
enum BuildRootObservation {
    Ready(RootEvidence),
    Missing,
    Unsafe,
    Unknown,
    Changed,
}

impl BuildRootObservation {
    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ready(left), Self::Ready(right)) => left.same_as(right),
            (Self::Missing, Self::Missing)
            | (Self::Unsafe, Self::Unsafe)
            | (Self::Unknown, Self::Unknown)
            | (Self::Changed, Self::Changed) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
struct RootEvidence {
    lineage: Vec<PrivateMetadata>,
    root: PrivateMetadata,
}

impl RootEvidence {
    fn same_as(&self, other: &Self) -> bool {
        self.lineage.len() == other.lineage.len()
            && self
                .lineage
                .iter()
                .zip(&other.lineage)
                .all(|(left, right)| left.same_path_component_as(right))
            && self.root.same_as(&other.root)
    }
}

#[derive(Debug)]
struct OpenBuildRoot {
    directory: File,
    evidence: RootEvidence,
}

#[derive(Debug, Clone)]
enum ChildObservation {
    Ready(PrivateMetadata),
    Missing,
    Unsafe,
    Unknown,
}

impl ChildObservation {
    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ready(left), Self::Ready(right)) => left.same_as(right),
            (Self::Missing, Self::Missing)
            | (Self::Unsafe, Self::Unsafe)
            | (Self::Unknown, Self::Unknown) => true,
            _ => false,
        }
    }

    const fn disposition(&self) -> LocalInstallDerivedDirectoryDisposition {
        match self {
            Self::Ready(_) => LocalInstallDerivedDirectoryDisposition::Ready,
            Self::Missing => LocalInstallDerivedDirectoryDisposition::Missing,
            Self::Unsafe => LocalInstallDerivedDirectoryDisposition::Unsafe,
            Self::Unknown => LocalInstallDerivedDirectoryDisposition::Unknown,
        }
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
            && self.changed_seconds == other.changed_seconds
            && self.changed_nanoseconds == other.changed_nanoseconds
    }

    /// Directory contents and timestamps of lineage ancestors can change because unrelated jobs
    /// create sibling entries. The entry identity, ownership, and permission class must remain
    /// exact; the final build root is compared separately with the complete metadata record.
    fn same_path_component_as(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.uid == other.uid
            && self.gid == other.gid
            && self.mode == other.mode
    }
}

fn snapshot(context: &LocalInstallBuildCommandContext) -> DirectorySnapshot {
    let first_root = match open_build_root(&context.build_root) {
        Ok(root) => root,
        Err(root) => return unavailable_snapshot(root),
    };
    let work = observe_child(&first_root.directory, "work");
    let home = observe_child(&first_root.directory, "home");
    let cargo_home = observe_child(&first_root.directory, "cargo-home");
    let target = observe_child(&first_root.directory, "target");

    let root = match open_build_root(&context.build_root) {
        Ok(second_root) if first_root.evidence.same_as(&second_root.evidence) => {
            BuildRootObservation::Ready(first_root.evidence)
        }
        Ok(_) => BuildRootObservation::Changed,
        Err(_) => BuildRootObservation::Changed,
    };

    DirectorySnapshot {
        root,
        work,
        home,
        cargo_home,
        target,
    }
}

fn unavailable_snapshot(root: BuildRootObservation) -> DirectorySnapshot {
    DirectorySnapshot {
        root,
        work: ChildObservation::Unknown,
        home: ChildObservation::Unknown,
        cargo_home: ChildObservation::Unknown,
        target: ChildObservation::Unknown,
    }
}

fn open_build_root(path: &Path) -> Result<OpenBuildRoot, BuildRootObservation> {
    if !valid_absolute_path(path) {
        return Err(BuildRootObservation::Unsafe);
    }
    let components = normal_components(path);
    if components.is_empty() {
        return Err(BuildRootObservation::Unsafe);
    }

    let root =
        fs::open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| BuildRootObservation::Unknown)?;
    let mut current = File::from(root);
    let mut lineage = vec![PrivateMetadata::from_metadata(
        &current
            .metadata()
            .map_err(|_| BuildRootObservation::Unknown)?,
    )];
    for (index, component) in components.iter().enumerate() {
        let opened = match fs::openat(current.as_fd(), *component, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(opened) => opened,
            Err(Errno::NOENT) if index + 1 == components.len() => {
                return Err(BuildRootObservation::Missing);
            }
            Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => {
                return Err(BuildRootObservation::Unsafe);
            }
            Err(_) => return Err(BuildRootObservation::Unknown),
        };
        let opened = File::from(opened);
        let metadata = opened
            .metadata()
            .map_err(|_| BuildRootObservation::Unknown)?;
        if !metadata.is_dir() {
            return Err(BuildRootObservation::Unsafe);
        }
        let private = PrivateMetadata::from_metadata(&metadata);
        if index + 1 == components.len() && !private_directory_is_ready(&private) {
            return Err(BuildRootObservation::Unsafe);
        }
        lineage.push(private);
        current = opened;
    }
    let root = lineage
        .last()
        .expect("validated build-root lineage is non-empty")
        .clone();
    Ok(OpenBuildRoot {
        directory: current,
        evidence: RootEvidence { lineage, root },
    })
}

fn observe_child(parent: &File, name: &str) -> ChildObservation {
    let opened = match fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(opened) => opened,
        Err(Errno::NOENT) => return ChildObservation::Missing,
        Err(Errno::LOOP | Errno::NOTDIR) => return ChildObservation::Unsafe,
        Err(_) => return ChildObservation::Unknown,
    };
    let opened = File::from(opened);
    let metadata = match opened.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return ChildObservation::Unknown,
    };
    if !metadata.is_dir() {
        return ChildObservation::Unsafe;
    }
    let private = PrivateMetadata::from_metadata(&metadata);
    if private_directory_is_ready(&private) {
        ChildObservation::Ready(private)
    } else {
        ChildObservation::Unsafe
    }
}

fn private_directory_is_ready(metadata: &PrivateMetadata) -> bool {
    metadata.uid == geteuid().as_raw()
        && metadata.gid == getegid().as_raw()
        && metadata.mode & 0o7777 == PRIVATE_DIRECTORY_MODE
}

fn public_receipt(
    first: &DirectorySnapshot,
    second: &DirectorySnapshot,
) -> LocalInstallDirectoryPreflightReceipt {
    if !first.root.same_as(&second.root) {
        return root_failure_receipt(
            LocalInstallDerivedDirectoryDisposition::Changed,
            LocalInstallDirectoryBlockingCode::BuildRootChanged,
        );
    }
    match &first.root {
        BuildRootObservation::Missing => {
            return root_failure_receipt(
                LocalInstallDerivedDirectoryDisposition::Unknown,
                LocalInstallDirectoryBlockingCode::BuildRootMissing,
            );
        }
        BuildRootObservation::Unsafe => {
            return root_failure_receipt(
                LocalInstallDerivedDirectoryDisposition::Unsafe,
                LocalInstallDirectoryBlockingCode::BuildRootUnsafe,
            );
        }
        BuildRootObservation::Unknown => {
            return root_failure_receipt(
                LocalInstallDerivedDirectoryDisposition::Unknown,
                LocalInstallDirectoryBlockingCode::BuildRootUnknown,
            );
        }
        BuildRootObservation::Changed => {
            return root_failure_receipt(
                LocalInstallDerivedDirectoryDisposition::Changed,
                LocalInstallDirectoryBlockingCode::BuildRootChanged,
            );
        }
        BuildRootObservation::Ready(_) => {}
    }

    let work = child_disposition(&first.work, &second.work);
    let home = child_disposition(&first.home, &second.home);
    let cargo_home = child_disposition(&first.cargo_home, &second.cargo_home);
    let target = child_disposition(&first.target, &second.target);

    let mut blocking_codes = BTreeSet::new();
    let mut repair_codes = BTreeSet::new();
    classify_child(
        work,
        LocalInstallDirectoryBlockingCode::WorkUnsafe,
        LocalInstallDirectoryBlockingCode::WorkUnknown,
        LocalInstallDirectoryBlockingCode::WorkChanged,
        LocalInstallDirectoryRepairCode::CreateWork,
        &mut blocking_codes,
        &mut repair_codes,
    );
    classify_child(
        home,
        LocalInstallDirectoryBlockingCode::HomeUnsafe,
        LocalInstallDirectoryBlockingCode::HomeUnknown,
        LocalInstallDirectoryBlockingCode::HomeChanged,
        LocalInstallDirectoryRepairCode::CreateHome,
        &mut blocking_codes,
        &mut repair_codes,
    );
    classify_child(
        cargo_home,
        LocalInstallDirectoryBlockingCode::CargoHomeUnsafe,
        LocalInstallDirectoryBlockingCode::CargoHomeUnknown,
        LocalInstallDirectoryBlockingCode::CargoHomeChanged,
        LocalInstallDirectoryRepairCode::CreateCargoHome,
        &mut blocking_codes,
        &mut repair_codes,
    );
    classify_child(
        target,
        LocalInstallDirectoryBlockingCode::TargetUnsafe,
        LocalInstallDirectoryBlockingCode::TargetUnknown,
        LocalInstallDirectoryBlockingCode::TargetChanged,
        LocalInstallDirectoryRepairCode::CreateTarget,
        &mut blocking_codes,
        &mut repair_codes,
    );

    let blocking_codes = blocking_codes.into_iter().collect::<Vec<_>>();
    let repair_codes = repair_codes.into_iter().collect::<Vec<_>>();
    let ready = blocking_codes.is_empty() && repair_codes.is_empty();
    let repairable = blocking_codes.is_empty() && !repair_codes.is_empty();

    LocalInstallDirectoryPreflightReceipt {
        schema_version: LOCAL_INSTALL_DIRECTORY_PREFLIGHT_SCHEMA_VERSION,
        work,
        home,
        cargo_home,
        target,
        ready,
        repairable,
        blocking_codes,
        repair_codes,
    }
}

fn child_disposition(
    first: &ChildObservation,
    second: &ChildObservation,
) -> LocalInstallDerivedDirectoryDisposition {
    if first.same_as(second) {
        first.disposition()
    } else {
        LocalInstallDerivedDirectoryDisposition::Changed
    }
}

#[allow(clippy::too_many_arguments)]
fn classify_child(
    disposition: LocalInstallDerivedDirectoryDisposition,
    unsafe_code: LocalInstallDirectoryBlockingCode,
    unknown_code: LocalInstallDirectoryBlockingCode,
    changed_code: LocalInstallDirectoryBlockingCode,
    repair_code: LocalInstallDirectoryRepairCode,
    blockers: &mut BTreeSet<LocalInstallDirectoryBlockingCode>,
    repairs: &mut BTreeSet<LocalInstallDirectoryRepairCode>,
) {
    match disposition {
        LocalInstallDerivedDirectoryDisposition::Ready => {}
        LocalInstallDerivedDirectoryDisposition::Missing => {
            repairs.insert(repair_code);
        }
        LocalInstallDerivedDirectoryDisposition::Unsafe => {
            blockers.insert(unsafe_code);
        }
        LocalInstallDerivedDirectoryDisposition::Unknown => {
            blockers.insert(unknown_code);
        }
        LocalInstallDerivedDirectoryDisposition::Changed => {
            blockers.insert(changed_code);
        }
    }
}

fn root_failure_receipt(
    disposition: LocalInstallDerivedDirectoryDisposition,
    blocker: LocalInstallDirectoryBlockingCode,
) -> LocalInstallDirectoryPreflightReceipt {
    LocalInstallDirectoryPreflightReceipt {
        schema_version: LOCAL_INSTALL_DIRECTORY_PREFLIGHT_SCHEMA_VERSION,
        work: disposition,
        home: disposition,
        cargo_home: disposition,
        target: disposition,
        ready: false,
        repairable: false,
        blocking_codes: vec![blocker],
        repair_codes: Vec::new(),
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempBuildRoot {
        container: PathBuf,
        build_root: PathBuf,
    }

    impl TempBuildRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let temporary_root = fs::canonicalize(std::env::temp_dir())
                .expect("canonicalize test temporary directory");
            let container = temporary_root.join(format!(
                "glaeda-directory-preflight-{label}-{}-{sequence}",
                std::process::id()
            ));
            let build_root = container.join("build-root");
            fs::create_dir_all(&build_root).expect("create build root");
            private_mode(&build_root);
            Self {
                container,
                build_root,
            }
        }

        fn context(&self) -> LocalInstallBuildCommandContext {
            LocalInstallBuildCommandContext::new(
                self.container.join("source"),
                self.build_root.clone(),
                "/reviewed-toolchain/cargo",
                "/reviewed-toolchain/rustc",
                "/reviewed-toolchain/rustdoc",
            )
            .expect("command context")
        }

        fn create_child(&self, name: &str) {
            let path = self.build_root.join(name);
            fs::create_dir(&path).expect("create child");
            private_mode(&path);
        }

        fn create_all(&self) {
            for name in ["work", "home", "cargo-home", "target"] {
                self.create_child(name);
            }
        }
    }

    impl Drop for TempBuildRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.container);
        }
    }

    fn private_mode(path: &Path) {
        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
            .expect("set private mode");
    }

    #[test]
    fn all_four_private_directories_are_ready_and_path_private() {
        let fixture = TempBuildRoot::new("ready");
        fixture.create_all();
        let receipt = observe_local_install_directory_preflight(&fixture.context());

        assert!(receipt.ready());
        assert!(!receipt.repairable());
        for disposition in [
            receipt.work(),
            receipt.home(),
            receipt.cargo_home(),
            receipt.target(),
        ] {
            assert_eq!(disposition, LocalInstallDerivedDirectoryDisposition::Ready);
        }
        assert!(receipt.blocking_codes().is_empty());
        assert!(receipt.repair_codes().is_empty());
        let public = serde_json::to_string(&receipt).expect("receipt JSON");
        assert!(!public.contains(fixture.container.to_string_lossy().as_ref()));
        assert!(!public.contains(".cargo"));
    }

    #[test]
    fn safely_missing_children_produce_only_bounded_repairs() {
        let fixture = TempBuildRoot::new("missing");
        fixture.create_child("home");
        fixture.create_child("target");
        let receipt = observe_local_install_directory_preflight(&fixture.context());

        assert!(!receipt.ready());
        assert!(receipt.repairable());
        assert_eq!(
            receipt.work(),
            LocalInstallDerivedDirectoryDisposition::Missing
        );
        assert_eq!(
            receipt.home(),
            LocalInstallDerivedDirectoryDisposition::Ready
        );
        assert_eq!(
            receipt.cargo_home(),
            LocalInstallDerivedDirectoryDisposition::Missing
        );
        assert_eq!(
            receipt.target(),
            LocalInstallDerivedDirectoryDisposition::Ready
        );
        assert!(receipt.blocking_codes().is_empty());
        assert_eq!(
            receipt.repair_codes(),
            [
                LocalInstallDirectoryRepairCode::CreateWork,
                LocalInstallDirectoryRepairCode::CreateCargoHome,
            ]
        );
    }

    #[test]
    fn unsafe_child_objects_block_without_repairs() {
        for case in ["symlink", "file", "mode"] {
            let fixture = TempBuildRoot::new(case);
            fixture.create_all();
            let work = fixture.build_root.join("work");
            fs::remove_dir(&work).expect("remove work");
            if case == "symlink" {
                fs::create_dir(fixture.build_root.join("work-real")).expect("real work");
                symlink("work-real", &work).expect("work symlink");
            } else if case == "file" {
                fs::write(&work, b"file").expect("work file");
            } else {
                fs::create_dir(&work).expect("work directory");
                fs::set_permissions(&work, fs::Permissions::from_mode(0o750))
                    .expect("widen work mode");
            }
            let receipt = observe_local_install_directory_preflight(&fixture.context());
            assert_eq!(
                receipt.work(),
                LocalInstallDerivedDirectoryDisposition::Unsafe
            );
            assert_eq!(
                receipt.blocking_codes(),
                [LocalInstallDirectoryBlockingCode::WorkUnsafe]
            );
            assert!(receipt.repair_codes().is_empty());
        }
    }

    #[test]
    fn missing_build_root_never_emits_child_repairs() {
        let fixture = TempBuildRoot::new("missing-root");
        fs::remove_dir(&fixture.build_root).expect("remove build root");
        let receipt = observe_local_install_directory_preflight(&fixture.context());
        assert!(!receipt.ready());
        assert!(!receipt.repairable());
        assert_eq!(
            receipt.blocking_codes(),
            [LocalInstallDirectoryBlockingCode::BuildRootMissing]
        );
        assert!(receipt.repair_codes().is_empty());
    }

    #[test]
    fn build_root_replacement_between_snapshots_is_changed() {
        let fixture = TempBuildRoot::new("root-replacement");
        fixture.create_all();
        let context = fixture.context();
        let replacement = fixture.container.join("replacement");
        let receipt = observe_with(&context, || {
            fs::rename(&fixture.build_root, &replacement).expect("move old build root");
            fs::create_dir(&fixture.build_root).expect("create replacement build root");
            private_mode(&fixture.build_root);
            for name in ["work", "home", "cargo-home", "target"] {
                let path = fixture.build_root.join(name);
                fs::create_dir(&path).expect("create replacement child");
                private_mode(&path);
            }
        });
        assert_eq!(
            receipt.blocking_codes(),
            [LocalInstallDirectoryBlockingCode::BuildRootChanged]
        );
        for disposition in [
            receipt.work(),
            receipt.home(),
            receipt.cargo_home(),
            receipt.target(),
        ] {
            assert_eq!(
                disposition,
                LocalInstallDerivedDirectoryDisposition::Changed
            );
        }
    }

    #[test]
    fn wrong_owner_metadata_fails_the_exact_private_policy() {
        let fixture = TempBuildRoot::new("owner-policy");
        let metadata = fs::metadata(&fixture.build_root).expect("build root metadata");
        let mut private = PrivateMetadata::from_metadata(&metadata);
        private.uid = geteuid().as_raw().saturating_add(1).max(1);
        assert_ne!(private.uid, geteuid().as_raw());
        assert!(!private_directory_is_ready(&private));
    }

    #[test]
    fn unrelated_lineage_churn_does_not_change_the_exact_build_root() {
        let fixture = TempBuildRoot::new("lineage-metadata");
        let metadata = fs::metadata(&fixture.build_root).expect("build root metadata");
        let before = PrivateMetadata::from_metadata(&metadata);
        let mut after = before.clone();
        after.links = after.links.saturating_add(1);
        after.changed_nanoseconds = after.changed_nanoseconds.saturating_add(1);

        assert!(before.same_path_component_as(&after));
        assert!(!before.same_as(&after));
    }

    #[test]
    fn stable_observation_is_deterministic() {
        let fixture = TempBuildRoot::new("deterministic");
        fixture.create_all();
        let first = observe_local_install_directory_preflight(&fixture.context());
        let second = observe_local_install_directory_preflight(&fixture.context());
        assert_eq!(first, second);
    }
}
