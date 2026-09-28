#![cfg(target_os = "macos")]

use std::fs::{self, File, Metadata};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use glaeda::artifact::Sha256Digest;
use glaeda::disposable_launchd_service::{
    DISPOSABLE_LAUNCHD_SERVICE_LABEL, DisposableLaunchdServiceDesiredState,
    plan_disposable_launchd_service,
};
use glaeda::disposable_worker_enrollment::{
    MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES, decode_disposable_worker_enrollment,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const OPT_IN_ENV: &str = "SMOLRUNNER_INSTALLED_SERVICE_ACCEPTANCE";
const OPT_IN_TOKEN: &str = "preflight-identities";
const PROGRAM_ENV: &str = "SMOLRUNNER_INSTALLED_SERVICE_PROGRAM";
const ENROLLMENT_ENV: &str = "SMOLRUNNER_INSTALLED_SERVICE_ENROLLMENT";
const OPERATOR_HOME_ENV: &str = "SMOLRUNNER_INSTALLED_SERVICE_OPERATOR_HOME";
const BRIDGE_PROGRAM: &str = "/opt/smolrunner/bin/scaleset-bridge";
const MAX_PROGRAM_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BRIDGE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Snapshot {
    dev: u64,
    ino: u64,
    mode: u32,
    nlink: u64,
    uid: u32,
    gid: u32,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl Snapshot {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            mode: metadata.mode(),
            nlink: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            size: metadata.size(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ProtectedKind {
    Program,
    Bridge,
    Enrollment,
}

#[derive(Debug)]
struct ProtectedObservation {
    digest: Sha256Digest,
    bytes: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
struct InstalledServicePreflightReceipt {
    schema_version: u8,
    receipt_type: &'static str,
    state: &'static str,
    service_label: &'static str,
    launchd_domain: String,
    program_digest: String,
    bridge_digest: String,
    enrollment_digest: String,
    plan_identity: String,
    target_plan_matches_library: bool,
    protected_leaf_evidence_stable: bool,
    canonical_enrollment_valid: bool,
    bridge_digest_matches_enrollment: bool,
    private_paths_exposed: bool,
    next_action: &'static str,
}

fn exact_absolute(path: &Path) -> bool {
    let normalized = path.components().collect::<PathBuf>();
    path.is_absolute()
        && normalized.as_os_str() == path.as_os_str()
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

fn canonical_exact(path: &Path) -> bool {
    fs::canonicalize(path).is_ok_and(|canonical| canonical == path)
}

fn inspect_directory(path: &Path, uid: u32, gid: u32) {
    assert!(
        exact_absolute(path),
        "directory path must be exact and absolute"
    );
    assert!(canonical_exact(path), "directory path must be canonical");
    let metadata = fs::symlink_metadata(path).expect("inspect exact directory");
    assert!(metadata.file_type().is_dir(), "expected a directory");
    assert!(
        !metadata.file_type().is_symlink(),
        "directory may not be a symlink"
    );
    assert_eq!(metadata.uid(), uid, "directory owner must match operator");
    assert_eq!(metadata.gid(), gid, "directory group must match operator");
    assert_eq!(
        metadata.mode() & 0o022,
        0,
        "directory may not be group/world writable"
    );
}

fn observe_protected(path: &Path, kind: ProtectedKind) -> ProtectedObservation {
    assert!(
        exact_absolute(path),
        "protected path must be exact and absolute"
    );
    assert!(canonical_exact(path), "protected path must be canonical");

    let path_before = fs::symlink_metadata(path).expect("inspect protected path before open");
    assert!(
        !path_before.file_type().is_symlink(),
        "protected path may not be a symlink"
    );
    let before = Snapshot::from_metadata(&path_before);
    assert_eq!(before.nlink, 1, "protected file must have one hard link");
    assert_ne!(before.size, 0, "protected file must be nonempty");

    let current_uid = rustix::process::geteuid().as_raw();
    let current_gid = rustix::process::getegid().as_raw();
    match kind {
        ProtectedKind::Program | ProtectedKind::Bridge => {
            assert_eq!(before.uid, 0, "executable must be root-owned");
            assert_eq!(
                before.mode & 0o022,
                0,
                "executable may not be group/world writable"
            );
            assert_ne!(
                before.mode & 0o111,
                0,
                "executable must have an execute bit"
            );
        }
        ProtectedKind::Enrollment => {
            assert_eq!(
                before.uid, current_uid,
                "enrollment owner must match operator"
            );
            assert_eq!(
                before.gid, current_gid,
                "enrollment group must match operator"
            );
            assert_eq!(
                before.mode & 0o7777,
                0o600,
                "enrollment mode must be exactly 0600"
            );
        }
    }

    let limit = match kind {
        ProtectedKind::Program => MAX_PROGRAM_BYTES,
        ProtectedKind::Bridge => MAX_BRIDGE_BYTES,
        ProtectedKind::Enrollment => MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES as u64,
    };
    assert!(
        before.size <= limit,
        "protected file exceeds reviewed byte bound"
    );

    let mut file = File::open(path).expect("open exact protected file");
    let held_before = Snapshot::from_metadata(&file.metadata().expect("inspect held file"));
    assert_eq!(before, held_before, "protected path changed during open");

    let capture = matches!(kind, ProtectedKind::Enrollment);
    let mut hasher = Sha256::new();
    let mut bytes = capture.then(Vec::new);
    let mut total = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer).expect("read exact protected file");
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).expect("read length fits u64"))
            .expect("protected file byte count does not overflow");
        assert!(
            total <= limit,
            "protected file exceeded reviewed byte bound while reading"
        );
        hasher.update(&buffer[..read]);
        if let Some(bytes) = &mut bytes {
            bytes.extend_from_slice(&buffer[..read]);
        }
    }
    let digest = Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .expect("SHA-256 output is canonical");

    file.seek(SeekFrom::Start(0))
        .expect("rewind held protected file");
    let mut confirmation_hasher = Sha256::new();
    let mut confirmation_total = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .expect("confirm exact protected file bytes");
        if read == 0 {
            break;
        }
        confirmation_total = confirmation_total
            .checked_add(u64::try_from(read).expect("read length fits u64"))
            .expect("confirmation byte count does not overflow");
        assert!(
            confirmation_total <= limit,
            "protected file exceeded reviewed byte bound during confirmation"
        );
        confirmation_hasher.update(&buffer[..read]);
    }
    let confirmation = Sha256Digest::parse(&format!("sha256:{:x}", confirmation_hasher.finalize()))
        .expect("confirmation SHA-256 output is canonical");
    assert_eq!(
        digest, confirmation,
        "protected file bytes changed while held"
    );

    let held_after = Snapshot::from_metadata(&file.metadata().expect("reinspect held file"));
    let path_after = Snapshot::from_metadata(
        &fs::symlink_metadata(path).expect("reinspect protected path after read"),
    );
    assert_eq!(
        before, held_after,
        "held protected file changed while reading"
    );
    assert_eq!(before, path_after, "protected path changed while reading");
    assert!(
        canonical_exact(path),
        "protected path stopped being canonical"
    );

    ProtectedObservation { digest, bytes }
}

fn parse_bridge_digest(enrollment: &[u8]) -> Sha256Digest {
    decode_disposable_worker_enrollment(enrollment).expect("decode canonical enrollment");
    let value: Value = serde_json::from_slice(enrollment).expect("parse validated enrollment JSON");
    let bridge_digest = value
        .get("bridge")
        .and_then(|bridge| bridge.get("program_digest"))
        .and_then(Value::as_str)
        .expect("validated enrollment contains bridge.program_digest");
    Sha256Digest::parse(bridge_digest).expect("validated bridge digest is canonical")
}

fn target_plan(
    program: &Path,
    operator_home: &Path,
    program_digest: &Sha256Digest,
    enrollment: &Path,
    enrollment_digest: &Sha256Digest,
) -> Value {
    let output = Command::new(program)
        .env_clear()
        .current_dir("/")
        .args([
            "--output",
            "json",
            "service",
            "plan",
            "--desired",
            "installed",
        ])
        .arg("--operator-home")
        .arg(operator_home)
        .arg("--program")
        .arg(program)
        .arg("--program-digest")
        .arg(program_digest.as_str())
        .arg("--enrollment")
        .arg(enrollment)
        .arg("--enrollment-digest")
        .arg(enrollment_digest.as_str())
        .stdin(Stdio::null())
        .output()
        .expect("run exact target SmolRunner service plan");
    assert!(
        output.status.success(),
        "target SmolRunner service plan must succeed"
    );
    assert!(
        output.stderr.is_empty(),
        "successful JSON service plan must not use stderr"
    );

    let stdout =
        String::from_utf8(output.stdout).expect("target service plan output must be UTF-8");
    for private in [operator_home, program, enrollment] {
        let private = private.to_str().expect("acceptance paths must be UTF-8");
        assert!(
            !stdout.contains(private),
            "target service plan JSON exposed a private path"
        );
    }
    serde_json::from_str(&stdout).expect("target service plan must emit one JSON document")
}

#[test]
#[ignore = "read-only preflight for the explicitly selected installed-service physical acceptance"]
fn installed_service_identities_produce_one_exact_approval_plan() {
    assert_eq!(
        std::env::var(OPT_IN_ENV).as_deref(),
        Ok(OPT_IN_TOKEN),
        "set the exact installed-service preflight opt-in token"
    );

    let current_uid = rustix::process::geteuid().as_raw();
    let current_gid = rustix::process::getegid().as_raw();
    assert_ne!(
        current_uid, 0,
        "installed service preflight must run as the operator, not root"
    );

    let program = PathBuf::from(
        std::env::var_os(PROGRAM_ENV).expect("explicit target SmolRunner program path is required"),
    );
    let enrollment = PathBuf::from(
        std::env::var_os(ENROLLMENT_ENV).expect("explicit canonical enrollment path is required"),
    );
    let operator_home = PathBuf::from(
        std::env::var_os(OPERATOR_HOME_ENV).expect("explicit operator home path is required"),
    );
    let bridge = Path::new(BRIDGE_PROGRAM);

    inspect_directory(&operator_home, current_uid, current_gid);
    inspect_directory(&operator_home.join("Library"), current_uid, current_gid);
    inspect_directory(
        &operator_home.join("Library/LaunchAgents"),
        current_uid,
        current_gid,
    );

    let program_observation = observe_protected(&program, ProtectedKind::Program);
    let bridge_observation = observe_protected(bridge, ProtectedKind::Bridge);
    let enrollment_observation = observe_protected(&enrollment, ProtectedKind::Enrollment);
    let enrollment_bytes = enrollment_observation
        .bytes
        .as_deref()
        .expect("enrollment observation retains bounded bytes");
    let enrolled_bridge_digest = parse_bridge_digest(enrollment_bytes);
    assert_eq!(
        bridge_observation.digest, enrolled_bridge_digest,
        "installed bridge bytes must match the canonical enrollment"
    );

    let plan = plan_disposable_launchd_service(
        DisposableLaunchdServiceDesiredState::Installed,
        current_uid,
        &operator_home,
        &program,
        &program_observation.digest,
        &enrollment,
        &enrollment_observation.digest,
    )
    .expect("build exact installed-service approval plan");
    let target = target_plan(
        &program,
        &operator_home,
        &program_observation.digest,
        &enrollment,
        &enrollment_observation.digest,
    );
    let target_identity = target
        .get("plan_identity")
        .and_then(Value::as_str)
        .expect("target plan exposes one public plan identity");
    assert_eq!(
        target_identity,
        plan.report().plan_identity().as_str(),
        "target binary and linked library must derive the same exact plan identity"
    );
    assert_eq!(
        target.get("desired_state").and_then(Value::as_str),
        Some("installed")
    );
    assert_eq!(
        target.get("service_label").and_then(Value::as_str),
        Some(DISPOSABLE_LAUNCHD_SERVICE_LABEL)
    );
    assert_eq!(
        target.get("launchd_domain").and_then(Value::as_str),
        Some(plan.report().launchd_domain())
    );
    assert_eq!(
        target
            .get("requires_operator_approval")
            .and_then(Value::as_bool),
        Some(true)
    );

    let receipt = InstalledServicePreflightReceipt {
        schema_version: 1,
        receipt_type: "smolrunner-installed-service-preflight-receipt",
        state: "ready_for_operator_approval",
        service_label: DISPOSABLE_LAUNCHD_SERVICE_LABEL,
        launchd_domain: plan.report().launchd_domain().to_owned(),
        program_digest: program_observation.digest.as_str().to_owned(),
        bridge_digest: bridge_observation.digest.as_str().to_owned(),
        enrollment_digest: enrollment_observation.digest.as_str().to_owned(),
        plan_identity: plan.report().plan_identity().as_str().to_owned(),
        target_plan_matches_library: true,
        protected_leaf_evidence_stable: true,
        canonical_enrollment_valid: true,
        bridge_digest_matches_enrollment: true,
        private_paths_exposed: false,
        next_action: "obtain_explicit_operator_approval_for_exact_plan_identity",
    };
    let json = serde_json::to_string_pretty(&receipt).expect("serialize bounded preflight receipt");
    for private in [&operator_home, &program, &enrollment] {
        let private = private.to_str().expect("acceptance paths must be UTF-8");
        assert!(
            !json.contains(private),
            "preflight receipt exposed a private path"
        );
    }
    assert!(
        !json.contains(BRIDGE_PROGRAM),
        "preflight receipt exposed the bridge path"
    );
    println!("{json}");
}
