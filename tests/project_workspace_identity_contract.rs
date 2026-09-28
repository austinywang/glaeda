mod artifact {
    pub use glaeda::artifact::Sha256Digest;
}

#[path = "../src/project_workspace_identity.rs"]
mod project_workspace_identity;

use project_workspace_identity::{
    ProjectWorkspaceFilesystemIdentityKind, ProjectWorkspaceIdentityGeneration,
    TrustedWorkspaceIdentityKind, project_workspace_filesystem_identity,
    trusted_workspace_identity,
};

const DEVICE: u64 = 1;
const INODE: u64 = 2;
const OWNER: u32 = 3;

#[test]
fn current_generation_is_explicitly_glaeda_v2() {
    assert_eq!(
        ProjectWorkspaceIdentityGeneration::CURRENT,
        ProjectWorkspaceIdentityGeneration::GlaedaV2
    );
    assert_eq!(
        serde_json::to_string(&ProjectWorkspaceIdentityGeneration::CURRENT).unwrap(),
        "\"glaeda_v2\""
    );
    assert_eq!(
        serde_json::to_string(&ProjectWorkspaceIdentityGeneration::SmolrunnerV1).unwrap(),
        "\"smolrunner_v1\""
    );
}

#[test]
fn smolrunner_v1_filesystem_identity_vectors_remain_exact() {
    let cases = [
        (
            ProjectWorkspaceFilesystemIdentityKind::Materialization,
            "sha256:ccd4f9f2e0865af7056fcc05a565be1cfce9401c08ec6e8904bdacaca2497369",
        ),
        (
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryRoot,
            "sha256:7132cc664772a59e2ad063d63563c241ef9401cf5b3b0687627b82d43df891ee",
        ),
        (
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryEntry,
            "sha256:547def7d65f4894b90303a9d2fd9bea70049b07ee608dd8199cc30f81022dfd8",
        ),
    ];

    for (kind, expected) in cases {
        assert_eq!(
            project_workspace_filesystem_identity(
                ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
                kind,
                DEVICE,
                INODE,
                OWNER,
            )
            .unwrap()
            .as_str(),
            expected
        );
    }
}

#[test]
fn glaeda_v2_filesystem_identities_are_distinct_and_deterministic() {
    let cases = [
        (
            ProjectWorkspaceFilesystemIdentityKind::Materialization,
            "sha256:ac619f499f2b2d9fedf9d3a1cff428aab52722bdcc6649dc4473c78e5db57e7f",
        ),
        (
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryRoot,
            "sha256:ed9dc8c13da86bbc893d42ff07603c03acf825e05705a5b952a44e5e24ea8799",
        ),
        (
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryEntry,
            "sha256:bfd20fd2d5c7620d0bddd3febe57f3eb5615cde2b79508b5029842964a4f97ed",
        ),
    ];

    for (kind, expected) in cases {
        let current = project_workspace_filesystem_identity(
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            kind,
            DEVICE,
            INODE,
            OWNER,
        )
        .unwrap();
        let legacy = project_workspace_filesystem_identity(
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            kind,
            DEVICE,
            INODE,
            OWNER,
        )
        .unwrap();
        assert_eq!(current.as_str(), expected);
        assert_ne!(current, legacy);
    }
}

#[test]
fn smolrunner_v1_trusted_workspace_vectors_remain_exact() {
    let fields: [&[u8]; 2] = [b"install-0000000001", br#"{"kind":"workspace"}"#];
    let cases = [
        (
            TrustedWorkspaceIdentityKind::WorkspaceId,
            "sha256:b373ca06d724dff010a7884c953b59e5ca08c6c06bdd1cd01a518d5e211ad65b",
        ),
        (
            TrustedWorkspaceIdentityKind::CacheNamespace,
            "sha256:08eb111ae18958175c2d3b59a9646af08075a8daa7236d9cb46cb30704489a16",
        ),
        (
            TrustedWorkspaceIdentityKind::Evidence,
            "sha256:51092ea2c99c50b35f1932c0683fb31bcaa498fcd948cf4e5000657c684a89fa",
        ),
    ];

    for (kind, expected) in cases {
        assert_eq!(
            trusted_workspace_identity(
                ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
                kind,
                fields,
            )
            .unwrap()
            .as_str(),
            expected
        );
    }
}

#[test]
fn glaeda_v2_trusted_workspace_vectors_are_distinct_and_deterministic() {
    let fields: [&[u8]; 2] = [b"install-0000000001", br#"{"kind":"workspace"}"#];
    let cases = [
        (
            TrustedWorkspaceIdentityKind::WorkspaceId,
            "sha256:4b668d8fc1b2541895af75762878fbad467f795f2071b1dbe4c7211717f70270",
        ),
        (
            TrustedWorkspaceIdentityKind::CacheNamespace,
            "sha256:f8d559575883fad28cf8b18c0961e2ffb8276c1699958a3cd49c57d3231935ff",
        ),
        (
            TrustedWorkspaceIdentityKind::Evidence,
            "sha256:ce3f42295d1b154fa346f6c4fecaad78cfbb0415a80d63d8844712412749dc83",
        ),
    ];

    for (kind, expected) in cases {
        let current =
            trusted_workspace_identity(ProjectWorkspaceIdentityGeneration::GlaedaV2, kind, fields)
                .unwrap();
        let legacy = trusted_workspace_identity(
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            kind,
            fields,
        )
        .unwrap();
        assert_eq!(current.as_str(), expected);
        assert_ne!(current, legacy);
    }
}

#[test]
fn length_prefixing_distinguishes_field_boundaries() {
    let left: [&[u8]; 2] = [b"ab", b"c"];
    let right: [&[u8]; 2] = [b"a", b"bc"];
    assert_ne!(
        trusted_workspace_identity(
            ProjectWorkspaceIdentityGeneration::CURRENT,
            TrustedWorkspaceIdentityKind::Evidence,
            left,
        )
        .unwrap(),
        trusted_workspace_identity(
            ProjectWorkspaceIdentityGeneration::CURRENT,
            TrustedWorkspaceIdentityKind::Evidence,
            right,
        )
        .unwrap()
    );
}
