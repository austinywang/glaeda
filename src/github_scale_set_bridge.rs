// The adapter is intentionally private until the durable M3 consumer owns its lifecycle. Landing
// the complete process/credential boundary first keeps the future service from inventing a second
// protocol path; remove this allowance when that consumer is connected.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;
#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::io::Seek;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
#[cfg(target_os = "linux")]
use rustix::fs::Mode;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
#[cfg(unix)]
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::artifact::Sha256Digest;
use crate::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
    ScaleSetRunnerReference, ScaleSetRunnerRequestId,
};
use crate::{disposable_worker_reconciler::ScaleSetDemand, execution_admission::EpochMillis};

const PROTOCOL_VERSION: u8 = 2;
const MAX_PROTOCOL_LINE_BYTES: usize = 128 * 1024;
const MAX_PRIVATE_KEY_BYTES: usize = 64 * 1024;
const MAX_JIT_CONFIG_BYTES: usize = 64 * 1024;
const MAX_EVENTS: usize = 50;
const MAX_ACQUIRE_REQUESTS: usize = 50;
const MAX_LABELS: usize = 32;
const MAX_BRIDGE_PROGRAM_BYTES: u64 = 64 * 1024 * 1024;
const BRIDGE_PROGRAM: &str = "/opt/smolrunner/bin/scaleset-bridge";
const DEFAULT_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(75);

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GitHubAppKeychainConfig {
    github_config_url: String,
    client_id: String,
    installation_id: u64,
    service: String,
    account: String,
}

impl GitHubAppKeychainConfig {
    pub(crate) fn new(
        github_config_url: &str,
        client_id: &str,
        installation_id: u64,
        service: &str,
        account: &str,
    ) -> Result<Self, ScaleSetBridgeError> {
        if !valid_github_config_url(github_config_url)
            || !bounded_token(client_id, 100)
            || installation_id == 0
            || !bounded_token(service, 128)
            || !bounded_token(account, 128)
        {
            return Err(ScaleSetBridgeError::new("invalid_github_app_identity"));
        }
        Ok(Self {
            github_config_url: github_config_url.to_owned(),
            client_id: client_id.to_owned(),
            installation_id,
            service: service.to_owned(),
            account: account.to_owned(),
        })
    }

    pub(crate) fn new_without_keychain(
        github_config_url: &str,
        client_id: &str,
        installation_id: u64,
    ) -> Result<Self, ScaleSetBridgeError> {
        if !valid_github_config_url(github_config_url)
            || !bounded_token(client_id, 100)
            || installation_id == 0
        {
            return Err(ScaleSetBridgeError::new("invalid_github_app_identity"));
        }
        Ok(Self {
            github_config_url: github_config_url.to_owned(),
            client_id: client_id.to_owned(),
            installation_id,
            service: String::new(),
            account: String::new(),
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetBridgeTarget {
    id: u32,
    name: String,
    runner_group_id: u32,
    labels: Vec<String>,
    owner: String,
    max_capacity: u16,
}

impl ScaleSetBridgeTarget {
    pub(crate) fn new(
        id: u32,
        name: &str,
        runner_group_id: u32,
        labels: &[String],
        owner: &str,
        max_capacity: u16,
    ) -> Result<Self, ScaleSetBridgeError> {
        if id == 0
            || runner_group_id == 0
            || max_capacity != 1
            || !bounded_token(name, 100)
            || !bounded_token(owner, 100)
            || labels.is_empty()
            || labels.len() > 8
        {
            return Err(ScaleSetBridgeError::new("invalid_scale_set_target"));
        }
        let mut seen = BTreeSet::new();
        for label in labels {
            if !bounded_token(label, 100) || !seen.insert(label.as_str()) {
                return Err(ScaleSetBridgeError::new("invalid_scale_set_target"));
            }
        }
        Ok(Self {
            id,
            name: name.to_owned(),
            runner_group_id,
            labels: labels.to_vec(),
            owner: owner.to_owned(),
            max_capacity,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetBridgeConfig {
    program: PathBuf,
    program_digest: Sha256Digest,
    github_app: GitHubAppKeychainConfig,
    target: ScaleSetBridgeTarget,
}

impl ScaleSetBridgeConfig {
    pub(crate) fn new(
        program: &Path,
        program_digest: Sha256Digest,
        github_app: GitHubAppKeychainConfig,
        target: ScaleSetBridgeTarget,
    ) -> Result<Self, ScaleSetBridgeError> {
        if program != Path::new(BRIDGE_PROGRAM) || !canonical_absolute_path(program) {
            return Err(ScaleSetBridgeError::new("invalid_bridge_program"));
        }
        Ok(Self {
            program: program.to_path_buf(),
            program_digest,
            github_app,
            target,
        })
    }
}

struct GitHubAppPrivateKey(Vec<u8>);

impl GitHubAppPrivateKey {
    fn parse(mut bytes: Vec<u8>) -> Result<Self, ScaleSetBridgeError> {
        if bytes.is_empty()
            || bytes.len() > MAX_PRIVATE_KEY_BYTES
            || std::str::from_utf8(&bytes).is_err()
        {
            bytes.zeroize();
            return Err(ScaleSetBridgeError::new("invalid_github_app_private_key"));
        }
        Ok(Self(bytes))
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("private key was validated as UTF-8")
    }
}

impl ScaleSetStatistics {
    pub(crate) fn demand(
        self,
        observed_at: EpochMillis,
        expires_at: EpochMillis,
    ) -> Result<ScaleSetDemand, ScaleSetBridgeError> {
        ScaleSetDemand::new(
            u16::try_from(self.assigned_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_demand"))?,
            u16::try_from(self.running_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_demand"))?,
            observed_at,
            expires_at,
        )
        .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_demand"))
    }
}

impl Drop for GitHubAppPrivateKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for GitHubAppPrivateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[cfg(target_os = "macos")]
fn load_keychain_private_key(
    config: &GitHubAppKeychainConfig,
) -> Result<GitHubAppPrivateKey, ScaleSetBridgeError> {
    let bytes =
        security_framework::passwords::get_generic_password(&config.service, &config.account)
            .map_err(|_| ScaleSetBridgeError::new("keychain_credential_unavailable"))?;
    GitHubAppPrivateKey::parse(bytes)
}

#[cfg(target_os = "linux")]
fn load_private_key_file(
    path: &Path,
    expected_digest: &Sha256Digest,
) -> Result<GitHubAppPrivateKey, ScaleSetBridgeError> {
    if !canonical_absolute_path(path) {
        return Err(ScaleSetBridgeError::new(
            "invalid_github_app_private_key_path",
        ));
    }
    let path_before = std::fs::symlink_metadata(path)
        .map_err(|_| ScaleSetBridgeError::new("github_app_private_key_unavailable"))?;
    validate_private_key_metadata(&path_before)?;
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| ScaleSetBridgeError::new("github_app_private_key_unavailable"))?;
    let mut file = File::from(descriptor);
    let before = file
        .metadata()
        .map_err(|_| ScaleSetBridgeError::new("github_app_private_key_unavailable"))?;
    validate_private_key_metadata(&before)?;
    if !same_private_key_metadata(&path_before, &before) {
        return Err(ScaleSetBridgeError::new("github_app_private_key_changed"));
    }

    let mut bytes = Vec::with_capacity(before.len() as usize);
    if file.read_to_end(&mut bytes).is_err() || bytes.len() as u64 != before.len() {
        bytes.zeroize();
        return Err(ScaleSetBridgeError::new(
            "github_app_private_key_unavailable",
        ));
    }
    let observed = format!("sha256:{:x}", Sha256::digest(&bytes));
    if observed != expected_digest.as_str() {
        bytes.zeroize();
        return Err(ScaleSetBridgeError::new(
            "github_app_private_key_digest_mismatch",
        ));
    }
    let after = file
        .metadata()
        .map_err(|_| ScaleSetBridgeError::new("github_app_private_key_unavailable"))?;
    let path_after = std::fs::symlink_metadata(path)
        .map_err(|_| ScaleSetBridgeError::new("github_app_private_key_unavailable"))?;
    if !same_private_key_metadata(&before, &after)
        || !same_private_key_metadata(&before, &path_after)
    {
        bytes.zeroize();
        return Err(ScaleSetBridgeError::new("github_app_private_key_changed"));
    }
    GitHubAppPrivateKey::parse(bytes)
}

#[cfg(target_os = "linux")]
fn validate_private_key_metadata(metadata: &std::fs::Metadata) -> Result<(), ScaleSetBridgeError> {
    let mode = metadata.mode();
    if !metadata.file_type().is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > MAX_PRIVATE_KEY_BYTES as u64
        || !matches!(mode & 0o7777, 0o400 | 0o600)
    {
        return Err(ScaleSetBridgeError::new("unsafe_github_app_private_key"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn same_private_key_metadata(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.len() == right.len()
}

#[cfg(unix)]
struct VerifiedBridgeProgram {
    file: File,
    snapshot: BridgeProgramSnapshot,
    digest: Sha256Digest,
}

#[cfg(unix)]
impl VerifiedBridgeProgram {
    fn open(config: &ScaleSetBridgeConfig) -> Result<Self, ScaleSetBridgeError> {
        verify_protected_bridge_path(&config.program)?;
        let path_metadata = std::fs::symlink_metadata(&config.program)
            .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?;
        let snapshot = BridgeProgramSnapshot::from_metadata(&path_metadata)?;
        let mut file = OpenOptions::new()
            .read(true)
            .open(&config.program)
            .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?;
        snapshot.matches_metadata(
            &file
                .metadata()
                .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?,
        )?;
        require_bridge_digest(&mut file, &config.program_digest)?;
        snapshot.matches_metadata(
            &file
                .metadata()
                .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?,
        )?;
        let verified = Self {
            file,
            snapshot,
            digest: config.program_digest.clone(),
        };
        verified.confirm(&config.program)?;
        Ok(verified)
    }

    fn confirm(&self, path: &Path) -> Result<(), ScaleSetBridgeError> {
        verify_protected_bridge_path(path)?;
        self.snapshot.matches_metadata(
            &self
                .file
                .metadata()
                .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?,
        )?;
        self.snapshot.matches_metadata(
            &std::fs::symlink_metadata(path)
                .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?,
        )?;
        let mut file = self
            .file
            .try_clone()
            .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?;
        require_bridge_digest(&mut file, &self.digest)
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct BridgeProgramSnapshot {
    dev: u64,
    ino: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    nlink: u64,
    size: u64,
}

#[cfg(unix)]
impl BridgeProgramSnapshot {
    fn from_metadata(metadata: &std::fs::Metadata) -> Result<Self, ScaleSetBridgeError> {
        let mode = metadata.mode();
        if !metadata.file_type().is_file()
            || metadata.uid() != 0
            || metadata.gid() != 0
            || mode & 0o7777 != 0o555
            || metadata.nlink() != 1
            || metadata.len() == 0
            || metadata.len() > MAX_BRIDGE_PROGRAM_BYTES
        {
            return Err(ScaleSetBridgeError::new("unsafe_bridge_program"));
        }
        Ok(Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode,
            nlink: metadata.nlink(),
            size: metadata.len(),
        })
    }

    fn matches_metadata(self, metadata: &std::fs::Metadata) -> Result<(), ScaleSetBridgeError> {
        let current = Self::from_metadata(metadata)?;
        if current != self {
            return Err(ScaleSetBridgeError::new("bridge_program_changed"));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn verify_protected_bridge_path(path: &Path) -> Result<(), ScaleSetBridgeError> {
    if path != Path::new(BRIDGE_PROGRAM) {
        return Err(ScaleSetBridgeError::new("invalid_bridge_program"));
    }
    for component in ["/", "/opt", "/opt/smolrunner", "/opt/smolrunner/bin"] {
        let metadata = std::fs::symlink_metadata(component)
            .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.mode() & 0o022 != 0
        {
            return Err(ScaleSetBridgeError::new("unsafe_bridge_program"));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn require_bridge_digest(
    file: &mut File,
    expected: &Sha256Digest,
) -> Result<(), ScaleSetBridgeError> {
    file.rewind()
        .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ScaleSetBridgeError::new("bridge_program_unavailable"))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .filter(|total| *total <= MAX_BRIDGE_PROGRAM_BYTES)
            .ok_or_else(|| ScaleSetBridgeError::new("unsafe_bridge_program"))?;
        hasher.update(&buffer[..read]);
    }
    buffer.zeroize();
    let actual = format!("sha256:{:x}", hasher.finalize());
    if actual != expected.as_str() {
        return Err(ScaleSetBridgeError::new("bridge_program_digest_mismatch"));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ScaleSetBridgeError {
    code: &'static str,
}

impl ScaleSetBridgeError {
    pub(crate) const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetBridgeError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetBridgeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetStatistics {
    pub(crate) available_jobs: u32,
    pub(crate) acquired_jobs: u32,
    pub(crate) assigned_jobs: u32,
    pub(crate) running_jobs: u32,
    pub(crate) registered_runners: u32,
    pub(crate) busy_runners: u32,
    pub(crate) idle_runners: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetBridgeJobEvidence {
    pub(crate) runner_request_id: u64,
    pub(crate) repository: String,
    pub(crate) owner: String,
    pub(crate) job_id: ScaleSetJobId,
    pub(crate) workflow_run_id: u64,
    pub(crate) request_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetBridgeEvent {
    Available(ScaleSetBridgeJobEvidence),
    Assigned(ScaleSetBridgeJobEvidence),
    Started {
        job: ScaleSetBridgeJobEvidence,
        runner: ScaleSetRunnerReference,
    },
    Completed {
        job: ScaleSetBridgeJobEvidence,
        runner: Option<ScaleSetRunnerReference>,
        result: ScaleSetJobResult,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetBridgePoll {
    Idle {
        statistics: ScaleSetStatistics,
    },
    Message {
        message_id: u32,
        statistics: ScaleSetStatistics,
        events: Vec<ScaleSetBridgeEvent>,
    },
}

pub(crate) struct EncodedJitConfig(Zeroizing<Vec<u8>>);

impl EncodedJitConfig {
    pub(crate) fn expose_to_guest_handoff(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn into_zeroizing_string(
        mut self,
    ) -> Result<Zeroizing<String>, ScaleSetBridgeError> {
        let bytes = std::mem::take(&mut *self.0);
        match String::from_utf8(bytes) {
            Ok(value) => Ok(Zeroizing::new(value)),
            Err(error) => {
                let _bytes = Zeroizing::new(error.into_bytes());
                Err(ScaleSetBridgeError::new("invalid_jit_encoding"))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(value: &str) -> Self {
        Self(Zeroizing::new(value.as_bytes().to_vec()))
    }
}

impl fmt::Debug for EncodedJitConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

pub(crate) struct ScaleSetJitReceipt {
    pub(crate) runner: ScaleSetRunnerReference,
    pub(crate) config: EncodedJitConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetRunnerLookup {
    Absent,
    Present(ScaleSetRunnerReference),
}

trait BridgeTransport {
    fn exchange(
        &mut self,
        request: &BridgeRequest<'_>,
    ) -> Result<Zeroizing<Vec<u8>>, ScaleSetBridgeError>;
    fn poison(&mut self);
}

struct BoundedSecretBuffer {
    bytes: Zeroizing<Box<[u8]>>,
    len: usize,
}

impl BoundedSecretBuffer {
    fn new(capacity: usize) -> Self {
        // Allocate the entire protocol bound before any credential or JIT byte enters the buffer.
        // Growing a Vec after that point could leave an unwiped allocator copy behind.
        Self {
            bytes: Zeroizing::new(vec![0_u8; capacity].into_boxed_slice()),
            len: 0,
        }
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn remaining_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[self.len..]
    }

    fn advance(&mut self, count: usize) -> Result<(), ScaleSetBridgeError> {
        self.len = self
            .len
            .checked_add(count)
            .filter(|len| *len <= self.bytes.len())
            .ok_or_else(|| ScaleSetBridgeError::new("bridge_response_invalid"))?;
        Ok(())
    }

    fn push(&mut self, byte: u8) -> Result<(), ScaleSetBridgeError> {
        if self.len == self.bytes.len() {
            return Err(ScaleSetBridgeError::new("bridge_request_failed"));
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        Ok(())
    }

    fn strip_final_newline(&mut self) -> Result<(), ScaleSetBridgeError> {
        if self.len == 0 || self.bytes[self.len - 1] != b'\n' {
            return Err(ScaleSetBridgeError::new("bridge_response_invalid"));
        }
        self.len -= 1;
        self.bytes[self.len] = 0;
        if self.len > 0 && self.bytes[self.len - 1] == b'\r' {
            return Err(ScaleSetBridgeError::new("bridge_response_invalid"));
        }
        Ok(())
    }

    fn into_exact_vec(self) -> Zeroizing<Vec<u8>> {
        let mut result = Zeroizing::new(Vec::with_capacity(self.len));
        result.extend_from_slice(self.as_slice());
        result
    }
}

impl Write for BoundedSecretBuffer {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        if input.len() > self.bytes.len().saturating_sub(self.len) {
            return Err(std::io::Error::new(
                ErrorKind::WriteZero,
                "bounded bridge request exceeded its fixed buffer",
            ));
        }
        let end = self.len + input.len();
        self.bytes[self.len..end].copy_from_slice(input);
        self.len = end;
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct ChildBridgeTransport {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    poisoned: bool,
}

impl ChildBridgeTransport {
    fn spawn(program: &Path) -> Result<Self, ScaleSetBridgeError> {
        let mut command = Command::new(program);
        command
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Keep the bridge in the controller's process group. launchd kills every remaining
        // member of a job's process group when the controller dies, including SIGKILL paths that
        // cannot run Rust Drop. Explicit per-child termination below remains the normal path.
        let mut child = command
            .spawn()
            .map_err(|_| ScaleSetBridgeError::new("bridge_spawn_failed"))?;
        let input = match child.stdin.take() {
            Some(input) => input,
            None => {
                terminate_child(&mut child);
                return Err(ScaleSetBridgeError::new("bridge_pipe_unavailable"));
            }
        };
        let output = match child.stdout.take() {
            Some(output) => output,
            None => {
                terminate_child(&mut child);
                return Err(ScaleSetBridgeError::new("bridge_pipe_unavailable"));
            }
        };
        if make_nonblocking(&input).is_err() || make_nonblocking(&output).is_err() {
            terminate_child(&mut child);
            return Err(ScaleSetBridgeError::new("bridge_pipe_unavailable"));
        }
        Ok(Self {
            child,
            input,
            output,
            poisoned: false,
        })
    }

    fn exchange_inner(
        &mut self,
        request: &BridgeRequest<'_>,
        timeout: Duration,
    ) -> Result<Zeroizing<Vec<u8>>, ScaleSetBridgeError> {
        if self.poisoned {
            return Err(ScaleSetBridgeError::new("bridge_session_poisoned"));
        }
        let mut encoded = BoundedSecretBuffer::new(MAX_PROTOCOL_LINE_BYTES);
        serde_json::to_writer(&mut encoded, request)
            .map_err(|_| ScaleSetBridgeError::new("bridge_request_failed"))?;
        encoded.push(b'\n')?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| ScaleSetBridgeError::new("bridge_request_failed"))?;
        write_all_until(&mut self.input, encoded.as_slice(), deadline)?;

        let mut response = BoundedSecretBuffer::new(MAX_PROTOCOL_LINE_BYTES + 1);
        read_line_until(&mut self.output, &mut response, deadline)?;
        if response.len > MAX_PROTOCOL_LINE_BYTES {
            return Err(ScaleSetBridgeError::new("bridge_response_invalid"));
        }
        response.strip_final_newline()?;
        Ok(response.into_exact_vec())
    }

    fn exchange_with_timeout(
        &mut self,
        request: &BridgeRequest<'_>,
        timeout: Duration,
    ) -> Result<Zeroizing<Vec<u8>>, ScaleSetBridgeError> {
        let result = self.exchange_inner(request, timeout);
        if result.is_err() {
            self.poison();
        }
        result
    }

    fn terminate(&mut self) {
        terminate_child(&mut self.child);
    }
}

impl BridgeTransport for ChildBridgeTransport {
    fn exchange(
        &mut self,
        request: &BridgeRequest<'_>,
    ) -> Result<Zeroizing<Vec<u8>>, ScaleSetBridgeError> {
        self.exchange_with_timeout(request, request.exchange_timeout())
    }

    fn poison(&mut self) {
        self.poisoned = true;
        self.terminate();
    }
}

impl Drop for ChildBridgeTransport {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn terminate_child(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn make_nonblocking<Fd: AsFd>(fd: &Fd) -> Result<(), ScaleSetBridgeError> {
    let flags = fcntl_getfl(fd).map_err(|_| ScaleSetBridgeError::new("bridge_pipe_unavailable"))?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK)
        .map_err(|_| ScaleSetBridgeError::new("bridge_pipe_unavailable"))
}

fn write_all_until(
    output: &mut ChildStdin,
    bytes: &[u8],
    deadline: Instant,
) -> Result<(), ScaleSetBridgeError> {
    let mut written = 0;
    while written < bytes.len() {
        wait_for_fd(output, PollFlags::OUT, deadline, "bridge_request_timeout")?;
        match output.write(&bytes[written..]) {
            Ok(0) => return Err(ScaleSetBridgeError::new("bridge_request_failed")),
            Ok(count) => written += count,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(ScaleSetBridgeError::new("bridge_request_failed")),
        }
    }
    Ok(())
}

fn read_line_until(
    input: &mut ChildStdout,
    response: &mut BoundedSecretBuffer,
    deadline: Instant,
) -> Result<(), ScaleSetBridgeError> {
    loop {
        wait_for_fd(input, PollFlags::IN, deadline, "bridge_response_timeout")?;
        if response.remaining_mut().is_empty() {
            return Err(ScaleSetBridgeError::new("bridge_response_invalid"));
        }
        match input.read(response.remaining_mut()) {
            Ok(0) => return Err(ScaleSetBridgeError::new("bridge_response_invalid")),
            Ok(count) => {
                response.advance(count)?;
                if let Some(newline) = response.as_slice().iter().position(|byte| *byte == b'\n') {
                    if newline + 1 != response.len {
                        return Err(ScaleSetBridgeError::new("bridge_response_invalid"));
                    }
                    return Ok(());
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(ScaleSetBridgeError::new("bridge_response_failed")),
        }
    }
}

fn wait_for_fd<Fd: AsFd>(
    fd: &Fd,
    readiness: PollFlags,
    deadline: Instant,
    timeout_code: &'static str,
) -> Result<(), ScaleSetBridgeError> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| ScaleSetBridgeError::new(timeout_code))?;
        let timeout =
            Timespec::try_from(remaining).map_err(|_| ScaleSetBridgeError::new(timeout_code))?;
        let mut descriptor = [PollFd::new(fd, readiness | PollFlags::ERR | PollFlags::HUP)];
        match poll(&mut descriptor, Some(&timeout)) {
            Ok(0) => return Err(ScaleSetBridgeError::new(timeout_code)),
            Ok(_) => {
                let observed = descriptor[0].revents();
                if observed.intersects(PollFlags::ERR | PollFlags::HUP) {
                    return Err(ScaleSetBridgeError::new("bridge_pipe_unavailable"));
                }
                if observed.contains(readiness) {
                    return Ok(());
                }
            }
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(ScaleSetBridgeError::new("bridge_pipe_unavailable")),
        }
    }
}

pub(crate) struct ScaleSetBridgeClient {
    transport: Box<dyn BridgeTransport>,
    target: ScaleSetBridgeTarget,
}

impl ScaleSetBridgeClient {
    #[cfg(target_os = "linux")]
    pub(crate) fn connect_from_private_key_file(
        config: ScaleSetBridgeConfig,
        private_key_path: &Path,
        private_key_digest: &Sha256Digest,
    ) -> Result<Self, ScaleSetBridgeError> {
        let verified_program = VerifiedBridgeProgram::open(&config)?;
        let private_key = load_private_key_file(private_key_path, private_key_digest)?;
        verified_program.confirm(&config.program)?;
        let mut transport = ChildBridgeTransport::spawn(&config.program)?;
        if let Err(error) = verified_program.confirm(&config.program) {
            transport.poison();
            return Err(error);
        }
        Self::connect_with_transport(config, private_key, Box::new(transport))
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn connect_from_keychain(
        config: ScaleSetBridgeConfig,
    ) -> Result<Self, ScaleSetBridgeError> {
        let verified_program = VerifiedBridgeProgram::open(&config)?;
        let private_key = load_keychain_private_key(&config.github_app)?;
        verified_program.confirm(&config.program)?;
        let mut transport = ChildBridgeTransport::spawn(&config.program)?;
        if let Err(error) = verified_program.confirm(&config.program) {
            transport.poison();
            return Err(error);
        }
        Self::connect_with_transport(config, private_key, Box::new(transport))
    }

    fn connect_with_transport(
        config: ScaleSetBridgeConfig,
        private_key: GitHubAppPrivateKey,
        mut transport: Box<dyn BridgeTransport>,
    ) -> Result<Self, ScaleSetBridgeError> {
        let request = BridgeRequest::start(&config, &private_key);
        let response = exchange_and_decode(transport.as_mut(), &request)?;
        expect_ready(&response, &config.target)?;
        Ok(Self {
            transport,
            target: config.target,
        })
    }

    pub(crate) fn poll(
        &mut self,
        available_capacity: u16,
    ) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
        if available_capacity > self.target.max_capacity {
            return Err(ScaleSetBridgeError::new("invalid_bridge_capacity"));
        }
        let mut response = exchange_and_decode(
            self.transport.as_mut(),
            &BridgeRequest::poll(available_capacity),
        )?;
        let result = (|| match response.response_type.as_str() {
            "idle" => {
                response.require_idle_shape()?;
                Ok(ScaleSetBridgePoll::Idle {
                    statistics: response.require_statistics()?,
                })
            }
            "message" => {
                response.require_message_shape()?;
                let message_id = positive_u32(response.message_id, "invalid_bridge_message")?;
                if response.events.len() > MAX_EVENTS {
                    return Err(ScaleSetBridgeError::new("invalid_bridge_message"));
                }
                let statistics = response.require_statistics()?;
                let events = std::mem::take(&mut response.events)
                    .into_iter()
                    .map(|event| normalize_event(event, self.target.id))
                    .collect::<Result<Vec<_>, _>>()?;
                let available = events
                    .iter()
                    .filter(|event| matches!(event, ScaleSetBridgeEvent::Available(_)))
                    .count();
                if available > usize::from(available_capacity) {
                    return Err(ScaleSetBridgeError::new("invalid_bridge_message"));
                }
                Ok(ScaleSetBridgePoll::Message {
                    message_id,
                    statistics,
                    events,
                })
            }
            "error" => Err(response.bridge_error()?),
            _ => Err(ScaleSetBridgeError::new("invalid_bridge_response")),
        })();
        self.finish_response(result)
    }

    /// Restore a fresh bridge process to one exact durable acknowledged-message cursor.
    ///
    /// The bridge accepts this only before its first poll. A caller may first perform the standalone
    /// acquisition recovery, then restore the cursor and poll with capacity zero to observe later
    /// lifecycle evidence without admitting another job.
    pub(crate) fn resume_after(
        &mut self,
        last_acked_message_id: u32,
    ) -> Result<(), ScaleSetBridgeError> {
        if last_acked_message_id == 0 {
            return Err(ScaleSetBridgeError::new("invalid_recovery"));
        }
        let response = exchange_and_decode(
            self.transport.as_mut(),
            &BridgeRequest::resume(last_acked_message_id),
        )?;
        let result = if response.response_type == "error" {
            Err(response.bridge_error()?)
        } else if response.response_type == "restored"
            && response.message_id == Some(u64::from(last_acked_message_id))
            && response.code.is_none()
            && response.scale_set_id.is_none()
            && response.statistics.is_none()
            && response.events.is_empty()
            && response.acquired_requests.is_empty()
            && response.runner.is_none()
            && response.encoded_jit_config.is_none()
        {
            Ok(())
        } else {
            Err(ScaleSetBridgeError::new("invalid_bridge_response"))
        };
        self.finish_response(result)
    }

    pub(crate) fn poison(&mut self) {
        self.transport.poison();
    }

    pub(crate) fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
        let mut response =
            exchange_and_decode(self.transport.as_mut(), &BridgeRequest::ack(message_id))?;
        let result = (|| {
            if response.response_type == "error" {
                return Err(response.bridge_error()?);
            }
            if response.response_type != "acked"
                || response.message_id != Some(u64::from(message_id))
                || response.code.is_some()
                || response.scale_set_id.is_some()
                || response.statistics.is_some()
                || !response.events.is_empty()
                || response.runner.is_some()
                || response.encoded_jit_config.is_some()
            {
                return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
            }
            let mut seen = BTreeSet::new();
            for id in &response.acquired_requests {
                if *id == 0 || !seen.insert(*id) {
                    return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
                }
            }
            Ok(std::mem::take(&mut response.acquired_requests))
        })();
        self.finish_response(result)
    }

    pub(crate) fn acquire(
        &mut self,
        request_ids: &[ScaleSetRunnerRequestId],
    ) -> Result<Vec<ScaleSetRunnerRequestId>, ScaleSetBridgeError> {
        if request_ids.is_empty() || request_ids.len() > MAX_ACQUIRE_REQUESTS {
            return Err(ScaleSetBridgeError::new("invalid_acquisition_request"));
        }
        let mut requested = BTreeSet::new();
        for request_id in request_ids {
            if !requested.insert(request_id.get()) {
                return Err(ScaleSetBridgeError::new("invalid_acquisition_request"));
            }
        }

        let mut response = exchange_and_decode(
            self.transport.as_mut(),
            &BridgeRequest::acquire(request_ids),
        )?;
        let result = (|| {
            if response.response_type == "error" {
                return Err(response.bridge_error()?);
            }
            if response.response_type != "acquired"
                || response.code.is_some()
                || response.scale_set_id.is_some()
                || response.message_id.is_some()
                || response.statistics.is_some()
                || !response.events.is_empty()
                || response.runner.is_some()
                || response.encoded_jit_config.is_some()
                || response.acquired_requests.len() > request_ids.len()
            {
                return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
            }

            let mut seen = BTreeSet::new();
            let mut previous = None;
            let mut acquired = Vec::with_capacity(response.acquired_requests.len());
            for id in std::mem::take(&mut response.acquired_requests) {
                if id == 0
                    || !requested.contains(&id)
                    || !seen.insert(id)
                    || previous.is_some_and(|previous| previous >= id)
                {
                    return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
                }
                previous = Some(id);
                acquired.push(
                    ScaleSetRunnerRequestId::new(id)
                        .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
                );
            }
            Ok(acquired)
        })();
        self.finish_response(result)
    }

    pub(crate) fn generate_jit(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetJitReceipt, ScaleSetBridgeError> {
        let mut response = exchange_and_decode(
            self.transport.as_mut(),
            &BridgeRequest::runner("generate_jit", runner_name.as_str(), None, Some("_work")),
        )?;
        let result = (|| {
            if response.response_type == "error" {
                return Err(response.bridge_error()?);
            }
            if response.response_type != "jit"
                || response.code.is_some()
                || response.scale_set_id.is_some()
                || response.message_id.is_some()
                || response.statistics.is_some()
                || !response.events.is_empty()
                || !response.acquired_requests.is_empty()
            {
                return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
            }
            let runner = normalize_runner(
                response
                    .runner
                    .take()
                    .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?,
                self.target.id,
                Some(runner_name.as_str()),
            )?;
            let mut config = response
                .encoded_jit_config
                .take()
                .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?
                .into_bytes();
            if config.is_empty() || config.len() > MAX_JIT_CONFIG_BYTES {
                config.zeroize();
                return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
            }
            Ok(ScaleSetJitReceipt {
                runner,
                config: EncodedJitConfig(config),
            })
        })();
        self.finish_response(result)
    }

    pub(crate) fn observe_runner(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetRunnerLookup, ScaleSetBridgeError> {
        let mut response = exchange_and_decode(
            self.transport.as_mut(),
            &BridgeRequest::runner("observe_runner", runner_name.as_str(), None, None),
        )?;
        let result = (|| {
            if response.response_type == "error" {
                return Err(response.bridge_error()?);
            }
            if response.response_type == "runner_absent" {
                if response.code.is_some()
                    || response.scale_set_id.is_some()
                    || response.message_id.is_some()
                    || response.statistics.is_some()
                    || !response.events.is_empty()
                    || !response.acquired_requests.is_empty()
                    || response.runner.is_some()
                    || response.encoded_jit_config.is_some()
                {
                    return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
                }
                return Ok(ScaleSetRunnerLookup::Absent);
            }
            if response.response_type != "runner"
                || response.code.is_some()
                || response.scale_set_id.is_some()
                || response.message_id.is_some()
                || response.statistics.is_some()
                || !response.events.is_empty()
                || !response.acquired_requests.is_empty()
                || response.encoded_jit_config.is_some()
            {
                return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
            }
            normalize_runner(
                response
                    .runner
                    .take()
                    .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?,
                self.target.id,
                Some(runner_name.as_str()),
            )
            .map(ScaleSetRunnerLookup::Present)
        })();
        self.finish_response(result)
    }

    pub(crate) fn remove_runner(
        &mut self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<(), ScaleSetBridgeError> {
        let mut response = exchange_and_decode(
            self.transport.as_mut(),
            &BridgeRequest::runner(
                "remove_runner",
                runner.name.as_str(),
                Some(runner.id.get()),
                None,
            ),
        )?;
        let result = (|| {
            if response.response_type == "error" {
                return Err(response.bridge_error()?);
            }
            if response.response_type != "removed"
                || response.code.is_some()
                || response.scale_set_id.is_some()
                || response.message_id.is_some()
                || response.statistics.is_some()
                || !response.events.is_empty()
                || !response.acquired_requests.is_empty()
                || response.encoded_jit_config.is_some()
                || normalize_runner(
                    response
                        .runner
                        .take()
                        .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?,
                    self.target.id,
                    Some(runner.name.as_str()),
                )? != *runner
            {
                return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
            }
            Ok(())
        })();
        self.finish_response(result)
    }

    fn finish_response<T>(
        &mut self,
        result: Result<T, ScaleSetBridgeError>,
    ) -> Result<T, ScaleSetBridgeError> {
        if result.as_ref().is_err_and(|error| {
            matches!(
                error.code(),
                "invalid_bridge_response"
                    | "invalid_bridge_message"
                    | "invalid_bridge_event"
                    | "invalid_bridge_runner"
            )
        }) {
            self.transport.poison();
        }
        result
    }
}

fn exchange_and_decode(
    transport: &mut dyn BridgeTransport,
    request: &BridgeRequest<'_>,
) -> Result<BridgeResponse, ScaleSetBridgeError> {
    let bytes = transport.exchange(request)?;
    let result = decode_response(&bytes);
    if result.is_err() {
        transport.poison();
    }
    result
}

fn decode_response(bytes: &[u8]) -> Result<BridgeResponse, ScaleSetBridgeError> {
    if bytes.is_empty() || bytes.len() > MAX_PROTOCOL_LINE_BYTES {
        return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
    }
    let response: BridgeResponseWire<'_> = serde_json::from_slice(bytes)
        .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?;
    if response.version != PROTOCOL_VERSION {
        return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
    }
    let encoded_jit_config = response
        .encoded_jit_config
        .map(SecretString::from_raw_json)
        .transpose()?;
    Ok(BridgeResponse {
        response_type: response.response_type,
        code: response.code,
        scale_set_id: response.scale_set_id,
        message_id: response.message_id,
        statistics: response.statistics,
        events: response.events,
        acquired_requests: response.acquired_requests,
        runner: response.runner,
        encoded_jit_config,
    })
}

#[derive(Serialize)]
struct BridgeRequest<'a> {
    version: u8,
    operation: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<StartRequest<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_acked_message_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_capacity: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_request_ids: Option<&'a [ScaleSetRunnerRequestId]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work_folder: Option<&'a str>,
}

#[derive(Serialize)]
struct StartRequest<'a> {
    github_config_url: &'a str,
    client_id: &'a str,
    installation_id: u64,
    private_key: &'a str,
    scale_set_id: u32,
    scale_set_name: &'a str,
    runner_group_id: u32,
    labels: &'a [String],
    owner: &'a str,
    max_capacity: u16,
}

impl<'a> BridgeRequest<'a> {
    fn exchange_timeout(&self) -> Duration {
        if self.operation == "poll" {
            POLL_EXCHANGE_TIMEOUT
        } else {
            DEFAULT_EXCHANGE_TIMEOUT
        }
    }

    fn start(config: &'a ScaleSetBridgeConfig, key: &'a GitHubAppPrivateKey) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation: "start",
            start: Some(StartRequest {
                github_config_url: &config.github_app.github_config_url,
                client_id: &config.github_app.client_id,
                installation_id: config.github_app.installation_id,
                private_key: key.as_str(),
                scale_set_id: config.target.id,
                scale_set_name: &config.target.name,
                runner_group_id: config.target.runner_group_id,
                labels: &config.target.labels,
                owner: &config.target.owner,
                max_capacity: config.target.max_capacity,
            }),
            message_id: None,
            last_acked_message_id: None,
            max_capacity: None,
            runner_request_ids: None,
            runner_name: None,
            runner_id: None,
            work_folder: None,
        }
    }

    const fn poll(max_capacity: u16) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation: "poll",
            start: None,
            message_id: None,
            last_acked_message_id: None,
            max_capacity: Some(max_capacity),
            runner_request_ids: None,
            runner_name: None,
            runner_id: None,
            work_folder: None,
        }
    }

    const fn resume(last_acked_message_id: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation: "resume",
            start: None,
            message_id: None,
            last_acked_message_id: Some(last_acked_message_id),
            max_capacity: None,
            runner_request_ids: None,
            runner_name: None,
            runner_id: None,
            work_folder: None,
        }
    }

    const fn ack(message_id: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation: "ack",
            start: None,
            message_id: Some(message_id),
            last_acked_message_id: None,
            max_capacity: None,
            runner_request_ids: None,
            runner_name: None,
            runner_id: None,
            work_folder: None,
        }
    }

    const fn acquire(request_ids: &'a [ScaleSetRunnerRequestId]) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation: "acquire",
            start: None,
            message_id: None,
            last_acked_message_id: None,
            max_capacity: None,
            runner_request_ids: Some(request_ids),
            runner_name: None,
            runner_id: None,
            work_folder: None,
        }
    }

    const fn runner(
        operation: &'a str,
        runner_name: &'a str,
        runner_id: Option<u64>,
        work_folder: Option<&'a str>,
    ) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation,
            start: None,
            message_id: None,
            last_acked_message_id: None,
            max_capacity: None,
            runner_request_ids: None,
            runner_name: Some(runner_name),
            runner_id,
            work_folder,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeResponseWire<'a> {
    version: u8,
    #[serde(rename = "type")]
    response_type: String,
    code: Option<String>,
    scale_set_id: Option<u64>,
    message_id: Option<u64>,
    statistics: Option<StatisticsWire>,
    #[serde(default)]
    events: Vec<EventWire>,
    #[serde(default)]
    acquired_requests: Vec<u64>,
    runner: Option<RunnerWire>,
    #[serde(borrow)]
    encoded_jit_config: Option<&'a RawValue>,
}

struct BridgeResponse {
    response_type: String,
    code: Option<String>,
    scale_set_id: Option<u64>,
    message_id: Option<u64>,
    statistics: Option<StatisticsWire>,
    events: Vec<EventWire>,
    acquired_requests: Vec<u64>,
    runner: Option<RunnerWire>,
    encoded_jit_config: Option<SecretString>,
}

struct SecretString(Zeroizing<Vec<u8>>);

impl SecretString {
    fn from_raw_json(raw: &RawValue) -> Result<Self, ScaleSetBridgeError> {
        // Borrow the exact JSON token from the zeroizing transport buffer. Escapes are rejected so
        // serde_json never needs a separately allocated unescape buffer for the one-time secret.
        let raw = raw.get().as_bytes();
        if raw.len() < 2 || raw.first() != Some(&b'"') || raw.last() != Some(&b'"') {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        let value = &raw[1..raw.len() - 1];
        if value
            .iter()
            .any(|byte| *byte == b'\\' || *byte == b'"' || *byte < b' ')
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        let mut bytes = Vec::with_capacity(value.len());
        bytes.extend_from_slice(value);
        Ok(Self(Zeroizing::new(bytes)))
    }

    fn into_bytes(mut self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(std::mem::take(&mut *self.0))
    }
}

impl BridgeResponse {
    fn require_idle_shape(&self) -> Result<(), ScaleSetBridgeError> {
        if self.code.is_some()
            || self.scale_set_id.is_some()
            || self.message_id.is_some()
            || self.statistics.is_none()
            || !self.events.is_empty()
            || !self.acquired_requests.is_empty()
            || self.runner.is_some()
            || self.encoded_jit_config.is_some()
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        Ok(())
    }

    fn require_message_shape(&self) -> Result<(), ScaleSetBridgeError> {
        if self.code.is_some()
            || self.scale_set_id.is_some()
            || self.message_id.is_none()
            || self.statistics.is_none()
            || !self.acquired_requests.is_empty()
            || self.runner.is_some()
            || self.encoded_jit_config.is_some()
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        Ok(())
    }

    fn require_statistics(&self) -> Result<ScaleSetStatistics, ScaleSetBridgeError> {
        let wire = self
            .statistics
            .as_ref()
            .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?;
        let statistics = ScaleSetStatistics {
            available_jobs: u32::try_from(wire.available_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            acquired_jobs: u32::try_from(wire.acquired_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            assigned_jobs: u32::try_from(wire.assigned_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            running_jobs: u32::try_from(wire.running_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            registered_runners: u32::try_from(wire.registered_runners)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            busy_runners: u32::try_from(wire.busy_runners)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            idle_runners: u32::try_from(wire.idle_runners)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
        };
        let classified_runners = statistics
            .busy_runners
            .checked_add(statistics.idle_runners)
            .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?;
        if statistics.running_jobs > statistics.assigned_jobs
            || statistics.busy_runners > statistics.registered_runners
            || statistics.idle_runners > statistics.registered_runners
            || classified_runners > statistics.registered_runners
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        Ok(statistics)
    }

    fn bridge_error(&self) -> Result<ScaleSetBridgeError, ScaleSetBridgeError> {
        if self.response_type != "error"
            || self.scale_set_id.is_some()
            || self.message_id.is_some()
            || self.statistics.is_some()
            || !self.events.is_empty()
            || !self.acquired_requests.is_empty()
            || self.runner.is_some()
            || self.encoded_jit_config.is_some()
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        let code = self
            .code
            .as_deref()
            .and_then(known_bridge_error)
            .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?;
        Ok(ScaleSetBridgeError::new(code))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatisticsWire {
    available_jobs: u64,
    acquired_jobs: u64,
    assigned_jobs: u64,
    running_jobs: u64,
    registered_runners: u64,
    busy_runners: u64,
    idle_runners: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventWire {
    kind: String,
    runner_request_id: u64,
    repository: String,
    owner: String,
    job_id: String,
    workflow_run_id: u64,
    request_labels: Vec<String>,
    runner_id: Option<u64>,
    runner_name: Option<String>,
    result: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunnerWire {
    id: u64,
    name: String,
    scale_set_id: u64,
}

fn expect_ready(
    response: &BridgeResponse,
    target: &ScaleSetBridgeTarget,
) -> Result<(), ScaleSetBridgeError> {
    if response.response_type == "error" {
        return Err(response.bridge_error()?);
    }
    if response.response_type != "ready"
        || response.scale_set_id != Some(u64::from(target.id))
        || response.code.is_some()
        || response.message_id.is_some()
        || response.statistics.is_none()
        || !response.events.is_empty()
        || !response.acquired_requests.is_empty()
        || response.runner.is_some()
        || response.encoded_jit_config.is_some()
    {
        return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
    }
    response.require_statistics().map(|_| ())
}

fn normalize_event(
    wire: EventWire,
    scale_set_id: u32,
) -> Result<ScaleSetBridgeEvent, ScaleSetBridgeError> {
    if wire.runner_request_id == 0
        || wire.workflow_run_id == 0
        || !bounded_token(&wire.repository, 100)
        || !bounded_token(&wire.owner, 100)
        || wire.request_labels.len() > MAX_LABELS
        || wire
            .request_labels
            .iter()
            .any(|label| !bounded_token(label, 100))
    {
        return Err(ScaleSetBridgeError::new("invalid_bridge_event"));
    }
    let job = ScaleSetBridgeJobEvidence {
        runner_request_id: wire.runner_request_id,
        repository: wire.repository,
        owner: wire.owner,
        job_id: ScaleSetJobId::parse(&wire.job_id)
            .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_event"))?,
        workflow_run_id: wire.workflow_run_id,
        request_labels: wire.request_labels,
    };
    let runner = match (wire.runner_id, wire.runner_name) {
        (Some(id), Some(name)) => Some(normalize_runner(
            RunnerWire {
                id,
                name,
                scale_set_id: u64::from(scale_set_id),
            },
            scale_set_id,
            None,
        )?),
        (None, None) => None,
        _ => return Err(ScaleSetBridgeError::new("invalid_bridge_event")),
    };
    match wire.kind.as_str() {
        "available" if runner.is_none() && wire.result.is_none() => {
            Ok(ScaleSetBridgeEvent::Available(job))
        }
        "assigned" if runner.is_none() && wire.result.is_none() => {
            Ok(ScaleSetBridgeEvent::Assigned(job))
        }
        "started" if runner.is_some() && wire.result.is_none() => {
            Ok(ScaleSetBridgeEvent::Started {
                job,
                runner: runner.expect("runner presence checked"),
            })
        }
        "completed" => {
            let result = ScaleSetJobResult::parse(
                wire.result
                    .as_deref()
                    .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_event"))?,
            )
            .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_event"))?;
            if runner.is_none() && result.as_str() != "canceled" {
                return Err(ScaleSetBridgeError::new("invalid_bridge_event"));
            }
            Ok(ScaleSetBridgeEvent::Completed {
                job,
                runner,
                result,
            })
        }
        _ => Err(ScaleSetBridgeError::new("invalid_bridge_event")),
    }
}

fn normalize_runner(
    wire: RunnerWire,
    scale_set_id: u32,
    expected_name: Option<&str>,
) -> Result<ScaleSetRunnerReference, ScaleSetBridgeError> {
    if wire.scale_set_id != u64::from(scale_set_id)
        || expected_name.is_some_and(|expected| expected != wire.name)
    {
        return Err(ScaleSetBridgeError::new("invalid_bridge_runner"));
    }
    Ok(ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(wire.id)
            .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_runner"))?,
        ScaleSetRunnerName::parse(&wire.name)
            .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_runner"))?,
    ))
}

fn positive_u32(value: Option<u64>, code: &'static str) -> Result<u32, ScaleSetBridgeError> {
    value
        .filter(|value| *value > 0)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| ScaleSetBridgeError::new(code))
}

fn known_bridge_error(code: &str) -> Option<&'static str> {
    Some(match code {
        "unsupported_operation" => "unsupported_operation",
        "already_started" => "already_started",
        "invalid_start" => "invalid_start",
        "start_failed" => "start_failed",
        "not_started" => "not_started",
        "ack_required" => "ack_required",
        "poll_failed" => "poll_failed",
        "invalid_capacity" => "invalid_capacity",
        "invalid_recovery" => "invalid_recovery",
        "invalid_message" => "invalid_message",
        "message_mismatch" => "message_mismatch",
        "ack_failed" => "ack_failed",
        "invalid_acquisition_request" => "invalid_acquisition_request",
        "acquire_failed" => "acquire_failed",
        "invalid_acquisition" => "invalid_acquisition",
        "invalid_runner" => "invalid_runner",
        "scale_set_drift" => "scale_set_drift",
        "jit_failed" => "jit_failed",
        "runner_unavailable" => "runner_unavailable",
        "remove_failed" => "remove_failed",
        "response_too_large" => "response_too_large",
        _ => return None,
    })
}

fn valid_github_config_url(value: &str) -> bool {
    let Some(path) = value.strip_prefix("https://github.com/") else {
        return false;
    };
    let parts = path.split('/').collect::<Vec<_>>();
    (parts.len() == 1 || parts.len() == 2) && parts.iter().all(|part| bounded_token(part, 100))
}

fn bounded_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn canonical_absolute_path(path: &Path) -> bool {
    let Some(raw) = path.to_str() else {
        return false;
    };
    raw.starts_with('/')
        && raw.len() > 1
        && !raw.ends_with('/')
        && raw
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    struct ScriptedTransport {
        responses: VecDeque<Vec<u8>>,
        requests: Vec<Vec<u8>>,
        poisoned: Rc<Cell<bool>>,
    }

    impl ScriptedTransport {
        fn new(responses: &[&str]) -> Self {
            Self::with_poison_probe(responses).0
        }

        fn with_poison_probe(responses: &[&str]) -> (Self, Rc<Cell<bool>>) {
            let poisoned = Rc::new(Cell::new(false));
            let transport = Self {
                responses: responses
                    .iter()
                    .map(|response| response.as_bytes().to_vec())
                    .collect(),
                requests: Vec::new(),
                poisoned: Rc::clone(&poisoned),
            };
            (transport, poisoned)
        }
    }

    impl BridgeTransport for ScriptedTransport {
        fn exchange(
            &mut self,
            request: &BridgeRequest<'_>,
        ) -> Result<Zeroizing<Vec<u8>>, ScaleSetBridgeError> {
            if self.poisoned.get() {
                return Err(ScaleSetBridgeError::new("bridge_session_poisoned"));
            }
            self.requests.push(
                serde_json::to_vec(request)
                    .map_err(|_| ScaleSetBridgeError::new("script_encode_failed"))?,
            );
            self.responses
                .pop_front()
                .map(Zeroizing::new)
                .ok_or_else(|| ScaleSetBridgeError::new("script_exhausted"))
        }

        fn poison(&mut self) {
            self.poisoned.set(true);
        }
    }

    fn config() -> ScaleSetBridgeConfig {
        ScaleSetBridgeConfig::new(
            Path::new("/opt/smolrunner/bin/scaleset-bridge"),
            Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).unwrap(),
            GitHubAppKeychainConfig::new(
                "https://github.com/example/project",
                "Iv1.example",
                17,
                "dev.smolrunner.github-app",
                "example-project",
            )
            .unwrap(),
            ScaleSetBridgeTarget::new(
                23,
                "smolrunner",
                1,
                &["smolrunner".to_owned()],
                "smolrunner-host",
                1,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn request_id(value: u64) -> ScaleSetRunnerRequestId {
        ScaleSetRunnerRequestId::new(value).unwrap()
    }

    #[test]
    fn keychain_and_program_configuration_fail_closed() {
        assert!(
            GitHubAppKeychainConfig::new(
                "http://github.com/example",
                "Iv1.example",
                17,
                "service",
                "account"
            )
            .is_err()
        );
        assert!(
            ScaleSetBridgeConfig::new(
                Path::new("/opt/./bridge"),
                Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).unwrap(),
                config().github_app,
                config().target
            )
            .is_err()
        );
        assert!(
            ScaleSetBridgeTarget::new(
                23,
                "smolrunner",
                1,
                &["smolrunner".to_owned()],
                "smolrunner-host",
                0,
            )
            .is_err()
        );
        assert!(
            ScaleSetBridgeTarget::new(
                23,
                "smolrunner",
                1,
                &["smolrunner".to_owned()],
                "smolrunner-host",
                2,
            )
            .is_err()
        );
        let key = GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap();
        assert_eq!(format!("{key:?}"), "[REDACTED]");
    }

    #[test]
    fn session_maps_demand_events_and_exact_ack() {
        let transport = ScriptedTransport::new(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"message","message_id":7,"statistics":{"available_jobs":1,"acquired_jobs":0,"assigned_jobs":1,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0},"events":[{"kind":"available","runner_request_id":41,"repository":"project","owner":"example","job_id":"job-1","workflow_run_id":99,"request_labels":["smolrunner"]}]}"#,
            r#"{"version":2,"type":"acked","message_id":7,"acquired_requests":[41]}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        let ScaleSetBridgePoll::Message {
            message_id,
            statistics,
            events,
        } = client.poll(1).unwrap()
        else {
            panic!("expected message")
        };
        assert_eq!(message_id, 7);
        assert_eq!(statistics.assigned_jobs, 1);
        statistics
            .demand(
                EpochMillis::new(1_000).unwrap(),
                EpochMillis::new(1_030).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            events.as_slice(),
            [ScaleSetBridgeEvent::Available(_)]
        ));
        assert_eq!(client.ack(7).unwrap(), vec![41]);
    }

    #[test]
    fn fresh_session_restores_cursor_and_polls_lifecycle_at_zero_capacity() {
        let transport = ScriptedTransport::new(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"restored","message_id":7}"#,
            r#"{"version":2,"type":"message","message_id":8,"statistics":{"available_jobs":0,"acquired_jobs":1,"assigned_jobs":1,"running_jobs":1,"registered_runners":1,"busy_runners":1,"idle_runners":0},"events":[{"kind":"started","runner_request_id":41,"repository":"project","owner":"example","job_id":"job-1","workflow_run_id":99,"request_labels":["smolrunner"],"runner_id":81,"runner_name":"smolrunner-job-1"}]}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();

        assert_eq!(
            client.resume_after(0).unwrap_err().code(),
            "invalid_recovery"
        );
        client.resume_after(7).unwrap();
        let ScaleSetBridgePoll::Message {
            message_id, events, ..
        } = client.poll(0).unwrap()
        else {
            panic!("expected lifecycle message")
        };
        assert_eq!(message_id, 8);
        assert!(matches!(
            events.as_slice(),
            [ScaleSetBridgeEvent::Started { .. }]
        ));
    }

    #[test]
    fn runner_observation_distinguishes_strict_absence_and_exact_presence() {
        let transport = ScriptedTransport::new(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"runner_absent"}"#,
            r#"{"version":2,"type":"runner","runner":{"id":81,"name":"smolrunner-job-1","scale_set_id":23}}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        let name = ScaleSetRunnerName::parse("smolrunner-job-1").unwrap();

        assert_eq!(
            client.observe_runner(&name).unwrap(),
            ScaleSetRunnerLookup::Absent
        );
        let ScaleSetRunnerLookup::Present(runner) = client.observe_runner(&name).unwrap() else {
            panic!("expected exact runner")
        };
        assert_eq!(runner.id.get(), 81);
        assert_eq!(runner.name, name);
    }

    #[test]
    fn malformed_absence_response_poisons_the_bridge() {
        let (transport, poisoned) = ScriptedTransport::with_poison_probe(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"runner_absent","runner":{"id":81,"name":"smolrunner-job-1","scale_set_id":23}}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        let name = ScaleSetRunnerName::parse("smolrunner-job-1").unwrap();

        assert_eq!(
            client.observe_runner(&name).unwrap_err().code(),
            "invalid_bridge_response"
        );
        assert!(poisoned.get());
    }

    #[test]
    fn zero_capacity_poll_rejects_available_work_and_poisons_the_session() {
        let (transport, poisoned) = ScriptedTransport::with_poison_probe(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"message","message_id":8,"statistics":{"available_jobs":1,"acquired_jobs":0,"assigned_jobs":1,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0},"events":[{"kind":"available","runner_request_id":41,"repository":"project","owner":"example","job_id":"job-1","workflow_run_id":99,"request_labels":["smolrunner"]}]}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();

        assert_eq!(client.poll(0).unwrap_err().code(), "invalid_bridge_message");
        assert!(poisoned.get());
        assert_eq!(
            client.poll(0).unwrap_err().code(),
            "bridge_session_poisoned"
        );
    }

    #[test]
    fn replayable_acquire_accepts_exact_subset_and_empty_replay() {
        let transport = ScriptedTransport::new(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"acquired","acquired_requests":[41,43]}"#,
            r#"{"version":2,"type":"acquired","acquired_requests":[]}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        let requested = [request_id(41), request_id(42), request_id(43)];
        assert_eq!(
            client.acquire(&requested).unwrap(),
            vec![request_id(41), request_id(43)]
        );
        assert!(client.acquire(&requested).unwrap().is_empty());
    }

    #[test]
    fn replayable_acquire_rejects_invalid_requests_before_exchange() {
        let transport = ScriptedTransport::new(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"acquired","acquired_requests":[41]}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();

        assert_eq!(
            client.acquire(&[]).unwrap_err().code(),
            "invalid_acquisition_request"
        );
        assert_eq!(
            client
                .acquire(&[request_id(41), request_id(41)])
                .unwrap_err()
                .code(),
            "invalid_acquisition_request"
        );
        let oversized = (1..=51).map(request_id).collect::<Vec<_>>();
        assert_eq!(
            client.acquire(&oversized).unwrap_err().code(),
            "invalid_acquisition_request"
        );
        assert_eq!(
            client.acquire(&[request_id(41)]).unwrap(),
            vec![request_id(41)]
        );
    }

    #[test]
    fn replayable_acquire_poisons_foreign_response() {
        let (transport, poisoned) = ScriptedTransport::with_poison_probe(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"acquired","acquired_requests":[99]}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();

        assert_eq!(
            client.acquire(&[request_id(41)]).unwrap_err().code(),
            "invalid_bridge_response"
        );
        assert!(poisoned.get());
        assert_eq!(
            client.poll(1).unwrap_err().code(),
            "bridge_session_poisoned"
        );
    }

    #[test]
    fn replayable_acquire_preserves_known_service_refusal() {
        let (transport, poisoned) = ScriptedTransport::with_poison_probe(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"error","code":"invalid_acquisition_request"}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();

        assert_eq!(
            client.acquire(&[request_id(41)]).unwrap_err().code(),
            "invalid_acquisition_request"
        );
        assert!(!poisoned.get());
    }

    #[test]
    fn jit_secret_is_redacted_and_runner_is_exact() {
        let transport = ScriptedTransport::new(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"jit","runner":{"id":81,"name":"smolrunner-job-1","scale_set_id":23},"encoded_jit_config":"one-time-secret"}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        let receipt = client
            .generate_jit(&ScaleSetRunnerName::parse("smolrunner-job-1").unwrap())
            .unwrap();
        assert_eq!(receipt.runner.id.get(), 81);
        assert_eq!(format!("{:?}", receipt.config), "[REDACTED]");
        assert_eq!(receipt.config.expose_to_guest_handoff(), b"one-time-secret");

        let (transport, poisoned) = ScriptedTransport::with_poison_probe(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"jit","runner":{"id":81,"name":"smolrunner-job-1","scale_set_id":23},"encoded_jit_config":"escaped\u002dsecret"}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        let error =
            match client.generate_jit(&ScaleSetRunnerName::parse("smolrunner-job-1").unwrap()) {
                Ok(_) => panic!("escaped JIT secret was accepted"),
                Err(error) => error,
            };
        assert_eq!(error.code(), "invalid_bridge_response");
        assert!(poisoned.get());
    }

    #[test]
    fn unknown_response_fields_and_runnerless_non_cancel_fail_closed() {
        let old_version = match decode_response(br#"{"version":1,"type":"idle"}"#) {
            Ok(_) => panic!("old protocol response was accepted"),
            Err(error) => error,
        };
        assert_eq!(old_version.code(), "invalid_bridge_response");
        let unknown_error = match decode_response(br#"{"version":2,"type":"idle","unknown":true}"#)
        {
            Ok(_) => panic!("unknown response field was accepted"),
            Err(error) => error,
        };
        assert_eq!(unknown_error.code(), "invalid_bridge_response");

        let (transport, poisoned) = ScriptedTransport::with_poison_probe(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"idle","unknown":true}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        assert_eq!(
            client.poll(1).unwrap_err().code(),
            "invalid_bridge_response"
        );
        assert!(poisoned.get());

        let mut response = decode_response(
            br#"{"version":2,"type":"message","message_id":7,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0},"events":[{"kind":"completed","runner_request_id":41,"repository":"project","owner":"example","job_id":"job-1","workflow_run_id":99,"request_labels":["smolrunner"],"result":"failed"}]}"#,
        )
        .unwrap();
        assert_eq!(
            normalize_event(
                std::mem::take(&mut response.events)
                    .into_iter()
                    .next()
                    .unwrap(),
                23,
            )
            .unwrap_err()
            .code(),
            "invalid_bridge_event"
        );

        let (transport, poisoned) = ScriptedTransport::with_poison_probe(&[
            r#"{"version":2,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":2,"type":"message","message_id":8,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0},"events":[{"kind":"completed","runner_request_id":41,"repository":"project","owner":"example","job_id":"job-1","workflow_run_id":99,"request_labels":["smolrunner"],"result":"failed"}]}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        assert_eq!(client.poll(1).unwrap_err().code(), "invalid_bridge_event");
        assert!(poisoned.get());
        assert_eq!(
            client.poll(1).unwrap_err().code(),
            "bridge_session_poisoned"
        );
    }

    #[test]
    fn wedged_child_times_out_and_is_reaped() {
        let program = Path::new("/usr/bin/tail");
        if !program.exists() {
            return;
        }
        let mut transport = ChildBridgeTransport::spawn(program).unwrap();
        let error = transport
            .exchange_with_timeout(&BridgeRequest::poll(1), Duration::from_millis(20))
            .unwrap_err();
        assert_eq!(error.code(), "bridge_response_timeout");
        assert!(transport.poisoned);
        assert!(transport.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn bridge_inherits_the_controller_process_group() {
        use rustix::process::{Pid, getpgid, getpgrp};

        let program = Path::new("/usr/bin/tail");
        if !program.exists() {
            return;
        }
        let transport = ChildBridgeTransport::spawn(program).unwrap();
        let child = Pid::from_raw(transport.child.id() as _).unwrap();
        assert_eq!(getpgid(Some(child)).unwrap(), getpgrp());
    }
}
