use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ownership::{OwnershipMarker, ProjectIdentity, ResourceIdentity};
use crate::project_workspace_identity::ProjectWorkspaceIdentityGeneration;
use crate::state::InstallationId;
use crate::state_document::{
    ProjectStateDocument, ResourceStateDocument, StateDocument, encode_state_document,
};
use crate::state_root_generation::{
    GLAEDA_CURRENT_STATE_ROOT, SMOLRUNNER_LEGACY_STATE_ROOT, StateRootSelection,
};

use super::{
    CACHE_RESOURCE_FILE, PROJECT_FILE, RESOURCES_DIRECTORY, TrustedWorkspaceReceiptErrorKind,
    WORKSPACE_RESOURCE_FILE, produce_trusted_workspace_cache_receipt,
    produce_trusted_workspace_cache_receipt_for_generation, produce_with_hook,
    select_trusted_workspace_root,
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-trusted-workspace-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("set root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn supported_owner(&self) -> bool {
        self.0.metadata().expect("root metadata").uid() != 0
            && self.0.metadata().expect("root metadata").gid() != 0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    root: TempRoot,
    installation_id: InstallationId,
    project: ProjectIdentity,
    installation: PathBuf,
    resources: PathBuf,
    workspace: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = TempRoot::new(label);
        let installation_id = InstallationId::parse("1111111111111111").expect("installation ID");
        let project = ProjectIdentity {
            repository: "example/project".to_owned(),
            runner_scope: crate::manifest::RunnerScope::Repository,
            runner_user: "project-runner".to_owned(),
        };
        let installation = root
            .path()
            .join("installations")
            .join(installation_id.as_str());
        let resources = installation.join(RESOURCES_DIRECTORY);
        let workspace = root.path().join("runner-workspace");
        let cache = workspace.join("target");
        create_directory(root.path().join("installations"), 0o750);
        create_directory(&installation, 0o750);
        create_directory(&resources, 0o750);
        create_directory(&workspace, 0o700);
        create_directory(&cache, 0o700);

        let fixture = Self {
            root,
            installation_id,
            project,
            installation,
            resources,
            workspace,
            cache,
        };
        fixture.write_project();
        fixture.write_directory_resource(
            WORKSPACE_RESOURCE_FILE,
            fixture.workspace.clone(),
            fixture.workspace.metadata().expect("workspace metadata"),
        );
        fixture.write_directory_resource(
            CACHE_RESOURCE_FILE,
            fixture.cache.clone(),
            fixture.cache.metadata().expect("cache metadata"),
        );
        fixture
    }

    fn write_project(&self) {
        let document =
            ProjectStateDocument::new(self.installation_id.clone(), self.project.clone())
                .expect("project document");
        write_state(
            self.installation.join(PROJECT_FILE),
            StateDocument::Project(document),
        );
    }

    fn write_directory_resource(&self, name: &str, path: PathBuf, metadata: fs::Metadata) {
        let path = path.to_str().expect("UTF-8 fixture path");
        let identity = ResourceIdentity::directory(
            path,
            self.installation_id.as_str(),
            metadata.uid(),
            metadata.gid(),
            metadata.mode() & 0o7777,
        )
        .expect("directory identity");
        let marker = OwnershipMarker::new(
            self.installation_id.as_str(),
            self.project.clone(),
            identity,
        );
        let document = ResourceStateDocument::new(marker).expect("resource document");
        write_state(self.resources.join(name), StateDocument::Resource(document));
    }

    fn receipt(&self) -> super::TrustedWorkspaceCacheReceipt {
        produce_trusted_workspace_cache_receipt(self.root.path(), &self.project).expect("receipt")
    }

    fn receipt_for_generation(
        &self,
        generation: ProjectWorkspaceIdentityGeneration,
    ) -> super::TrustedWorkspaceCacheReceipt {
        produce_trusted_workspace_cache_receipt_for_generation(
            self.root.path(),
            &self.project,
            generation,
        )
        .expect("receipt")
    }
}

fn create_directory(path: impl AsRef<Path>, mode: u32) {
    fs::create_dir(path.as_ref()).expect("create directory");
    fs::set_permissions(path.as_ref(), fs::Permissions::from_mode(mode)).expect("set mode");
}

fn write_state(path: PathBuf, document: StateDocument) {
    let encoded = encode_state_document(&document).expect("encode state document");
    fs::write(&path, encoded).expect("write state document");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("set state mode");
}

#[test]
fn descriptor_relative_success_is_private_and_deterministic() {
    let fixture = Fixture::new("success");
    if !fixture.root.supported_owner() {
        return;
    }

    let first = fixture.receipt();
    let first_workspace_id = first.workspace_id().as_str().to_owned();
    let first_namespace = first.cache_namespace_digest().as_str().to_owned();
    let first_evidence = first.trusted_evidence_digest().as_str().to_owned();
    let json = serde_json::to_string(&first).expect("serialize receipt");
    let debug = format!("{first:?}");
    let location_debug = format!("{:?}", first.workspace_location_identity());

    assert_eq!(
        first.installation_id().as_str(),
        fixture.installation_id.as_str()
    );
    assert_eq!(first.repository().as_str(), "example/project");
    assert_eq!(
        first.identity_generation(),
        ProjectWorkspaceIdentityGeneration::GlaedaV2
    );
    assert_eq!(first.cache_id().as_str(), "cargo-target");
    assert!(first_workspace_id.starts_with("workspace-"));
    assert!(first_namespace.starts_with("sha256:"));
    assert!(first_evidence.starts_with("sha256:"));
    assert_eq!(location_debug, "<private-workspace-location>");
    for private in [
        fixture.root.path(),
        fixture.workspace.as_path(),
        fixture.cache.as_path(),
        fixture.installation.as_path(),
    ] {
        let private = private.to_str().expect("UTF-8 path");
        assert!(!json.contains(private));
        assert!(!debug.contains(private));
    }
    assert!(!json.contains("ready"));
    assert!(json.contains("\"schema_version\":2"));
    assert!(json.contains("\"identity_generation\":\"glaeda_v2\""));

    let second = fixture.receipt();
    assert_eq!(second.workspace_id().as_str(), first_workspace_id);
    assert_eq!(second.cache_namespace_digest().as_str(), first_namespace);
    assert_eq!(second.trusted_evidence_digest().as_str(), first_evidence);
    assert_eq!(
        second.workspace_location_identity(),
        first.workspace_location_identity()
    );
}

#[test]
fn fixed_root_selection_seals_the_matching_identity_generation() {
    let current = select_trusted_workspace_root(StateRootSelection::Current);
    let legacy = select_trusted_workspace_root(StateRootSelection::LegacySmolrunnerV1);

    assert_eq!(
        current.root.fixed_path(),
        Path::new(GLAEDA_CURRENT_STATE_ROOT)
    );
    assert_eq!(
        current.identity_generation,
        ProjectWorkspaceIdentityGeneration::GlaedaV2
    );
    assert_eq!(
        legacy.root.fixed_path(),
        Path::new(SMOLRUNNER_LEGACY_STATE_ROOT)
    );
    assert_eq!(
        legacy.identity_generation,
        ProjectWorkspaceIdentityGeneration::SmolrunnerV1
    );
    assert_ne!(current.root.fixed_path(), legacy.root.fixed_path());
    assert_ne!(current.identity_generation, legacy.identity_generation);
}

#[test]
fn legacy_and_current_generations_are_explicit_and_fully_separated() {
    let fixture = Fixture::new("generations");
    if !fixture.root.supported_owner() {
        return;
    }

    let legacy = fixture.receipt_for_generation(ProjectWorkspaceIdentityGeneration::SmolrunnerV1);
    let current = fixture.receipt_for_generation(ProjectWorkspaceIdentityGeneration::GlaedaV2);

    assert_eq!(
        legacy.identity_generation(),
        ProjectWorkspaceIdentityGeneration::SmolrunnerV1
    );
    assert_eq!(
        current.identity_generation(),
        ProjectWorkspaceIdentityGeneration::GlaedaV2
    );
    assert_ne!(legacy.workspace_id(), current.workspace_id());
    assert_ne!(
        legacy.cache_namespace_digest(),
        current.cache_namespace_digest()
    );
    assert_ne!(
        legacy.trusted_evidence_digest(),
        current.trusted_evidence_digest()
    );
    assert_eq!(
        legacy.workspace_location_identity(),
        current.workspace_location_identity()
    );
}

#[test]
fn same_path_workspace_replacement_gets_a_distinct_private_identity() {
    let fixture = Fixture::new("workspace-rebind");
    if !fixture.root.supported_owner() {
        return;
    }
    let first = fixture.receipt();
    let displaced = fixture.root.path().join("runner-workspace-displaced");
    fs::rename(&fixture.workspace, displaced).expect("displace first workspace");
    create_directory(&fixture.workspace, 0o700);
    create_directory(&fixture.cache, 0o700);

    let second = fixture.receipt();

    assert_ne!(
        second.workspace_location_identity(),
        first.workspace_location_identity()
    );
    assert_eq!(
        format!("{:?}", second.workspace_location_identity()),
        "<private-workspace-location>"
    );
}

#[test]
fn duplicate_installations_are_ambiguous() {
    let fixture = Fixture::new("duplicate");
    if !fixture.root.supported_owner() {
        return;
    }
    let second_id = InstallationId::parse("2222222222222222").expect("installation ID");
    let second = fixture
        .root
        .path()
        .join("installations")
        .join(second_id.as_str());
    create_directory(&second, 0o750);
    create_directory(second.join(RESOURCES_DIRECTORY), 0o750);
    let project = ProjectStateDocument::new(second_id, fixture.project.clone()).expect("project");
    write_state(second.join(PROJECT_FILE), StateDocument::Project(project));

    let error = produce_trusted_workspace_cache_receipt(fixture.root.path(), &fixture.project)
        .expect_err("duplicate project must fail");
    assert_eq!(
        error.kind(),
        TrustedWorkspaceReceiptErrorKind::AmbiguousState
    );
}

#[test]
fn hard_linked_state_record_fails_closed() {
    let fixture = Fixture::new("hard-link");
    if !fixture.root.supported_owner() {
        return;
    }
    let record = fixture.resources.join(WORKSPACE_RESOURCE_FILE);
    fs::hard_link(&record, fixture.resources.join("workspace-copy.json")).expect("hard link");

    let error = produce_trusted_workspace_cache_receipt(fixture.root.path(), &fixture.project)
        .expect_err("hard-linked state must fail");
    assert_eq!(
        error.kind(),
        TrustedWorkspaceReceiptErrorKind::UnsafeFilesystem
    );
    assert_eq!(error.stage(), "workspace_record");
}

#[test]
fn symlinked_workspace_or_cache_fails_closed() {
    let fixture = Fixture::new("symlink");
    if !fixture.root.supported_owner() {
        return;
    }
    let real = fixture.root.path().join("real-workspace");
    create_directory(&real, 0o700);
    fs::remove_dir_all(&fixture.workspace).expect("remove workspace");
    symlink(&real, &fixture.workspace).expect("workspace symlink");

    let error = produce_trusted_workspace_cache_receipt(fixture.root.path(), &fixture.project)
        .expect_err("symlinked workspace must fail");
    assert_eq!(
        error.kind(),
        TrustedWorkspaceReceiptErrorKind::UnsafeFilesystem
    );
}

#[test]
fn cache_escape_is_rejected_before_open() {
    let fixture = Fixture::new("escape");
    if !fixture.root.supported_owner() {
        return;
    }
    let escaped = fixture.root.path().join("escaped-cache");
    create_directory(&escaped, 0o700);
    fixture.write_directory_resource(
        CACHE_RESOURCE_FILE,
        escaped.clone(),
        escaped.metadata().expect("escaped metadata"),
    );

    let error = produce_trusted_workspace_cache_receipt(fixture.root.path(), &fixture.project)
        .expect_err("cache escape must fail");
    assert_eq!(
        error.kind(),
        TrustedWorkspaceReceiptErrorKind::IdentityMismatch
    );
    assert_eq!(error.stage(), "cache");
}

#[test]
fn durable_marker_must_match_observed_owner_and_mode() {
    let fixture = Fixture::new("marker-drift");
    if !fixture.root.supported_owner() {
        return;
    }
    let metadata = fixture.workspace.metadata().expect("workspace metadata");
    let path = fixture.workspace.to_str().expect("UTF-8 path");
    let wrong = ResourceIdentity::directory(
        path,
        fixture.installation_id.as_str(),
        metadata.uid() + 1,
        metadata.gid(),
        metadata.mode() & 0o7777,
    )
    .expect("wrong identity");
    let marker = OwnershipMarker::new(
        fixture.installation_id.as_str(),
        fixture.project.clone(),
        wrong,
    );
    let document = ResourceStateDocument::new(marker).expect("resource document");
    write_state(
        fixture.resources.join(WORKSPACE_RESOURCE_FILE),
        StateDocument::Resource(document),
    );

    let error = produce_trusted_workspace_cache_receipt(fixture.root.path(), &fixture.project)
        .expect_err("owner drift must fail");
    assert_eq!(
        error.kind(),
        TrustedWorkspaceReceiptErrorKind::IdentityMismatch
    );
    assert_eq!(error.stage(), "workspace");
}

#[test]
fn broad_runner_directory_mode_is_unsafe() {
    let fixture = Fixture::new("broad-mode");
    if !fixture.root.supported_owner() {
        return;
    }
    fs::set_permissions(&fixture.cache, fs::Permissions::from_mode(0o770)).expect("broaden cache");
    fixture.write_directory_resource(
        CACHE_RESOURCE_FILE,
        fixture.cache.clone(),
        fixture.cache.metadata().expect("cache metadata"),
    );

    let error = produce_trusted_workspace_cache_receipt(fixture.root.path(), &fixture.project)
        .expect_err("broad cache mode must fail");
    assert_eq!(
        error.kind(),
        TrustedWorkspaceReceiptErrorKind::UnsafeFilesystem
    );
    assert_eq!(error.stage(), "cache");
}

#[test]
fn missing_fixed_resource_record_is_typed() {
    let fixture = Fixture::new("missing-record");
    if !fixture.root.supported_owner() {
        return;
    }
    fs::remove_file(fixture.resources.join(CACHE_RESOURCE_FILE)).expect("remove cache record");

    let error = produce_trusted_workspace_cache_receipt(fixture.root.path(), &fixture.project)
        .expect_err("missing cache record must fail");
    assert_eq!(error.kind(), TrustedWorkspaceReceiptErrorKind::MissingState);
    assert_eq!(error.stage(), "cache_record");
}

#[test]
fn cache_path_replacement_after_open_is_rejected() {
    let fixture = Fixture::new("replacement-race");
    if !fixture.root.supported_owner() {
        return;
    }
    let replacement_path = fixture.cache.clone();
    let moved_path = fixture.workspace.join("target-held");

    let error = produce_with_hook(fixture.root.path(), &fixture.project, || {
        fs::rename(&replacement_path, &moved_path).expect("move opened cache");
        create_directory(&replacement_path, 0o700);
    })
    .expect_err("cache replacement must fail");

    assert_eq!(
        error.kind(),
        TrustedWorkspaceReceiptErrorKind::IdentityMismatch
    );
    // Replacing a child directory changes the held workspace directory link-count identity
    // before the cache entry itself is rechecked.
    assert_eq!(error.stage(), "workspace");
}
