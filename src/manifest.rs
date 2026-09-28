use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MANIFEST_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u8,
    pub repository: String,
    pub runner: RunnerConfig,
    pub container: ContainerConfig,
    pub verify: VerifyConfig,
    pub limits: ResourceLimits,
    pub trust: TrustConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerConfig {
    pub scope: RunnerScope,
    pub user: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerScope {
    Repository,
    Organization,
}

impl fmt::Display for RunnerScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository => formatter.write_str("repository"),
            Self::Organization => formatter.write_str("organization"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerConfig {
    pub image: String,
    pub file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyConfig {
    pub command: PathBuf,
    pub suites: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    pub memory: String,
    pub cpus: f64,
    pub pids: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    pub forks: ForkPolicy,
    pub trigger: TriggerPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPolicy {
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerPolicy {
    Operator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManifestProblem {
    pub field: String,
    pub message: String,
}

impl ManifestProblem {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestErrorKind {
    Io,
    Parse,
    Validation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManifestError {
    pub kind: ManifestErrorKind,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<ManifestProblem>,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "{}", self.message)?;
        for problem in &self.problems {
            writeln!(formatter, "- {}: {}", problem.field, problem.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ManifestError {}

/// Load and validate a Glaeda manifest from disk.
///
/// # Errors
///
/// Returns a structured error when the file cannot be read, YAML cannot be parsed, or any
/// version-one invariant is invalid.
pub fn load(path: &Path) -> Result<Manifest, ManifestError> {
    let contents = fs::read_to_string(path).map_err(|error| ManifestError {
        kind: ManifestErrorKind::Io,
        message: format!("failed to read manifest {}: {error}", path.display()),
        problems: Vec::new(),
    })?;
    parse(&contents)
}

/// Parse and validate a Glaeda manifest.
///
/// # Errors
///
/// Returns a structured parse or validation error. Unknown fields and unknown versions fail
/// closed.
pub fn parse(contents: &str) -> Result<Manifest, ManifestError> {
    let manifest: Manifest = serde_yaml::from_str(contents).map_err(|error| ManifestError {
        kind: ManifestErrorKind::Parse,
        message: format!("failed to parse Glaeda manifest: {error}"),
        problems: Vec::new(),
    })?;

    let problems = validate(&manifest);
    if problems.is_empty() {
        Ok(manifest)
    } else {
        Err(ManifestError {
            kind: ManifestErrorKind::Validation,
            message: "manifest validation failed".to_owned(),
            problems,
        })
    }
}

#[must_use]
pub fn validate(manifest: &Manifest) -> Vec<ManifestProblem> {
    let mut problems = Vec::new();

    if manifest.version != MANIFEST_VERSION {
        problems.push(ManifestProblem::new(
            "version",
            format!(
                "unsupported schema version {}; only version {MANIFEST_VERSION} is accepted",
                manifest.version
            ),
        ));
    }

    validate_repository(&manifest.repository, &mut problems);
    validate_linux_user(&manifest.runner.user, &mut problems);
    validate_labels(&manifest.runner.labels, &mut problems);
    validate_image(&manifest.container.image, &mut problems);
    validate_relative_path("container.file", &manifest.container.file, &mut problems);
    validate_relative_path("verify.command", &manifest.verify.command, &mut problems);
    validate_suites(&manifest.verify.suites, &mut problems);
    validate_limits(&manifest.limits, &mut problems);

    problems
}

fn validate_repository(repository: &str, problems: &mut Vec<ManifestProblem>) {
    let parts = repository.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts
            .iter()
            .any(|part| part.is_empty() || !is_github_name(part))
    {
        problems.push(ManifestProblem::new(
            "repository",
            "must be exactly OWNER/REPOSITORY using letters, digits, '.', '_', or '-'",
        ));
    }
}

fn is_github_name(value: &str) -> bool {
    value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_linux_user(user: &str, problems: &mut Vec<ManifestProblem>) {
    let mut bytes = user.bytes();
    let first_valid = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'_');
    let rest_valid = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
    });

    if user.len() > 32 || !first_valid || !rest_valid {
        problems.push(ManifestProblem::new(
            "runner.user",
            "must be a Linux username of at most 32 characters using lowercase letters, digits, '_' or '-'",
        ));
    }
}

fn validate_labels(labels: &[String], problems: &mut Vec<ManifestProblem>) {
    if labels.is_empty() {
        problems.push(ManifestProblem::new(
            "runner.labels",
            "must contain at least one project-specific label",
        ));
        return;
    }

    let mut seen = BTreeSet::new();
    for label in labels {
        let valid = !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !valid {
            problems.push(ManifestProblem::new(
                "runner.labels",
                format!("invalid label {label:?}"),
            ));
        }
        if !seen.insert(label) {
            problems.push(ManifestProblem::new(
                "runner.labels",
                format!("duplicate label {label:?}"),
            ));
        }
    }
}

fn validate_image(image: &str, problems: &mut Vec<ManifestProblem>) {
    if image.is_empty() || image.chars().any(char::is_whitespace) {
        problems.push(ManifestProblem::new(
            "container.image",
            "must be a non-empty image reference without whitespace",
        ));
    }
}

fn validate_relative_path(field: &str, path: &Path, problems: &mut Vec<ManifestProblem>) {
    let valid = !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));

    if !valid {
        problems.push(ManifestProblem::new(
            field,
            "must be a non-empty relative project path without '..'",
        ));
    }
}

fn validate_suites(suites: &BTreeMap<String, String>, problems: &mut Vec<ManifestProblem>) {
    if suites.is_empty() {
        problems.push(ManifestProblem::new(
            "verify.suites",
            "must define at least one named suite",
        ));
    }

    for (name, argument) in suites {
        let valid_name = !name.is_empty()
            && name.len() <= 32
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            });
        if !valid_name {
            problems.push(ManifestProblem::new(
                "verify.suites",
                format!("invalid suite name {name:?}"),
            ));
        }
        if argument.is_empty() || argument.chars().any(char::is_whitespace) {
            problems.push(ManifestProblem::new(
                "verify.suites",
                format!("suite {name:?} must map to one non-empty argument without whitespace"),
            ));
        }
    }
}

fn validate_limits(limits: &ResourceLimits, problems: &mut Vec<ManifestProblem>) {
    if !valid_memory(&limits.memory) {
        problems.push(ManifestProblem::new(
            "limits.memory",
            "must be a positive integer followed by KiB, MiB, or GiB",
        ));
    }
    if !limits.cpus.is_finite() || !(0.0..=128.0).contains(&limits.cpus) || limits.cpus == 0.0 {
        problems.push(ManifestProblem::new(
            "limits.cpus",
            "must be greater than zero and at most 128",
        ));
    }
    if limits.pids == 0 {
        problems.push(ManifestProblem::new(
            "limits.pids",
            "must be greater than zero",
        ));
    }
}

fn valid_memory(value: &str) -> bool {
    ["KiB", "MiB", "GiB"].iter().any(|suffix| {
        value
            .strip_suffix(suffix)
            .and_then(|number| number.parse::<u64>().ok())
            .is_some_and(|number| number > 0)
    })
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST_VERSION, ManifestErrorKind, parse};

    const VALID: &str = r#"
version: 1
repository: example/project
runner:
  scope: repository
  user: project-runner
  labels: [project-ci]
container:
  image: localhost/project-ci:1
  file: build/ci/Containerfile
verify:
  command: scripts/run-vps-verification.sh
  suites:
    focused: focused
    full: full
limits:
  memory: 2GiB
  cpus: 1.5
  pids: 768
trust:
  forks: deny
  trigger: operator
"#;

    #[test]
    fn parses_valid_manifest() {
        let manifest = parse(VALID).expect("valid manifest");
        assert_eq!(manifest.version, MANIFEST_VERSION);
        assert_eq!(manifest.repository, "example/project");
    }

    #[test]
    fn rejects_forward_version() {
        let error = parse(&VALID.replace("version: 1", "version: 2"))
            .expect_err("future versions must fail closed");
        assert_eq!(error.kind, ManifestErrorKind::Validation);
        assert!(
            error
                .problems
                .iter()
                .any(|problem| problem.field == "version")
        );
    }

    #[test]
    fn rejects_parent_paths_and_invalid_users() {
        let input = VALID
            .replace("project-runner", "Project Runner")
            .replace("build/ci/Containerfile", "../Containerfile");
        let error = parse(&input).expect_err("invalid manifest");
        assert!(
            error
                .problems
                .iter()
                .any(|problem| problem.field == "runner.user")
        );
        assert!(
            error
                .problems
                .iter()
                .any(|problem| problem.field == "container.file")
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = parse(&VALID.replace(
            "repository: example/project",
            "repository: example/project\nsurprise: true",
        ))
        .expect_err("unknown fields must fail closed");
        assert_eq!(error.kind, ManifestErrorKind::Parse);
    }
}
