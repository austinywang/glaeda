use std::path::Path;

use serde::Serialize;

use crate::manifest::{Manifest, RunnerScope};

pub const PLAN_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanReport {
    pub schema_version: u8,
    pub source: String,
    pub repository: String,
    pub actions: Vec<PlanAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanAction {
    pub id: String,
    pub kind: PlanActionKind,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanActionKind {
    EnsureRunnerUser,
    EnsureRunnerRegistration,
    EnsureContainerImage,
    ConfigureVerification,
}

#[must_use]
pub fn build(manifest: &Manifest, source: &Path) -> PlanReport {
    let scope_target = match manifest.runner.scope {
        RunnerScope::Repository => manifest.repository.clone(),
        RunnerScope::Organization => manifest.repository.split_once('/').map_or_else(
            || manifest.repository.clone(),
            |(owner, _)| owner.to_owned(),
        ),
    };
    let labels = manifest.runner.labels.join(", ");
    let suites = manifest
        .verify
        .suites
        .iter()
        .map(|(name, argument)| format!("{name}={argument}"))
        .collect::<Vec<_>>()
        .join(", ");

    PlanReport {
        schema_version: PLAN_SCHEMA_VERSION,
        source: source.display().to_string(),
        repository: manifest.repository.clone(),
        actions: vec![
            PlanAction {
                id: "runner-user".to_owned(),
                kind: PlanActionKind::EnsureRunnerUser,
                summary: format!("ensure dedicated runner user {}", manifest.runner.user),
                detail: "The user will be unprivileged and isolated from unrelated project groups."
                    .to_owned(),
            },
            PlanAction {
                id: "runner-registration".to_owned(),
                kind: PlanActionKind::EnsureRunnerRegistration,
                summary: format!(
                    "ensure official GitHub Actions runner registration for {scope_target}"
                ),
                detail: format!(
                    "scope={}, labels=[{labels}], repository={}",
                    manifest.runner.scope, manifest.repository
                ),
            },
            PlanAction {
                id: "container-image".to_owned(),
                kind: PlanActionKind::EnsureContainerImage,
                summary: format!("ensure project image {}", manifest.container.image),
                detail: format!(
                    "build from {} and record the resulting immutable image digest",
                    manifest.container.file.display()
                ),
            },
            PlanAction {
                id: "verification".to_owned(),
                kind: PlanActionKind::ConfigureVerification,
                summary: "configure disposable project verification".to_owned(),
                detail: format!(
                    "command={}, suites=[{suites}], memory={}, cpus={}, pids={}, forks=deny, trigger=operator",
                    manifest.verify.command.display(),
                    manifest.limits.memory,
                    manifest.limits.cpus,
                    manifest.limits.pids
                ),
            },
        ],
    }
}

#[must_use]
pub fn render_human(report: &PlanReport) -> String {
    let mut output = format!(
        "Glaeda plan\n\nManifest: {}\nRepository: {}\n\n",
        report.source, report.repository
    );

    for (index, action) in report.actions.iter().enumerate() {
        output.push_str(&format!("{}. {}\n", index + 1, action.summary));
        output.push_str(&format!("   {}\n", action.detail));
    }

    output.push_str("\nNo changes were made.\n");
    output
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::manifest::parse;

    use super::{PlanActionKind, build, render_human};

    const MANIFEST: &str = r#"
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
    fn plan_is_stable_and_read_only_by_construction() {
        let manifest = parse(MANIFEST).expect("valid manifest");
        let plan = build(&manifest, Path::new("glaeda.yml"));

        assert_eq!(plan.actions.len(), 4);
        assert_eq!(plan.actions[0].kind, PlanActionKind::EnsureRunnerUser);
        assert_eq!(plan.actions[3].kind, PlanActionKind::ConfigureVerification);
        assert!(render_human(&plan).ends_with("No changes were made.\n"));
    }

    #[test]
    fn organization_scope_targets_owner() {
        let manifest = parse(&MANIFEST.replace("scope: repository", "scope: organization"))
            .expect("valid organization manifest");
        let plan = build(&manifest, Path::new("glaeda.yml"));
        assert!(plan.actions[1].summary.ends_with("for example"));
    }
}
