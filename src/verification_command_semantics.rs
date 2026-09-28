use serde::Serialize;

pub const VERIFICATION_COMMAND_SEMANTICS_SCHEMA_VERSION: u8 = 1;

const REPOSITORY_BOOTSTRAP_PATH: &str = "scripts/bootstrap";
const CARGO_CAPABILITY: &str = "cargo";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCommandMeaning {
    Required,
    Doctor,
    Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCommandWorkingDirectory {
    RepositoryRoot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationProgramKind {
    RepositoryProgram,
    ToolCapability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationProgramSemantics {
    kind: VerificationProgramKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    repository_relative_path: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability: Option<&'static str>,
}

impl VerificationProgramSemantics {
    #[must_use]
    pub const fn kind(&self) -> VerificationProgramKind {
        self.kind
    }

    #[must_use]
    pub const fn repository_relative_path(&self) -> Option<&'static str> {
        self.repository_relative_path
    }

    #[must_use]
    pub const fn capability(&self) -> Option<&'static str> {
        self.capability
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationCommandStepSemantics {
    program: VerificationProgramSemantics,
    arguments: Vec<&'static str>,
}

impl VerificationCommandStepSemantics {
    #[must_use]
    pub const fn program(&self) -> &VerificationProgramSemantics {
        &self.program
    }

    #[must_use]
    pub fn arguments(&self) -> &[&'static str] {
        &self.arguments
    }
}

/// One exact checked-in profile meaning expressed only as ordered fixed invocations.
///
/// A later executor must resolve repository programs and tool capabilities to reviewed absolute
/// executable paths before spawning. This type grants no execution, shell, mutation, Git,
/// credential, publication, or workflow authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationCommandSemantics {
    schema_version: u8,
    meaning: VerificationCommandMeaning,
    working_directory: VerificationCommandWorkingDirectory,
    steps: Vec<VerificationCommandStepSemantics>,
}

impl VerificationCommandSemantics {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn meaning(&self) -> VerificationCommandMeaning {
        self.meaning
    }

    #[must_use]
    pub const fn working_directory(&self) -> VerificationCommandWorkingDirectory {
        self.working_directory
    }

    #[must_use]
    pub fn steps(&self) -> &[VerificationCommandStepSemantics] {
        &self.steps
    }

    /// Serialize the versioned semantics in deterministic field and step order.
    ///
    /// The checked-in document contains no maps, private paths, environment values, or command
    /// output, so this compact JSON is suitable input for a separately owned identity digest.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the fixed document cannot be encoded.
    pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Return the three current profile meanings in stable required/doctor/plan order.
#[must_use]
pub fn glaeda_verification_command_semantics() -> [VerificationCommandSemantics; 3] {
    [
        required_verification_command_semantics(),
        doctor_verification_command_semantics(),
        plan_verification_command_semantics(),
    ]
}

/// Define `required` as the eight repository-required checks emitted by the named profile.
#[must_use]
pub fn required_verification_command_semantics() -> VerificationCommandSemantics {
    semantics(
        VerificationCommandMeaning::Required,
        vec![
            repository_bootstrap(&["--output", "json"]),
            cargo(&["fmt", "--all", "--", "--check"]),
            cargo(&[
                "clippy",
                "--locked",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ]),
            cargo(&["test", "--locked", "--all-targets", "--all-features"]),
            doctor_step(),
            plan_quarry_step(),
            plan_glossless_step(),
            host_plan_quarry_step(),
        ],
    )
}

/// Define `doctor` as the machine-readable doctor check from current `AGENTS.md`.
#[must_use]
pub fn doctor_verification_command_semantics() -> VerificationCommandSemantics {
    semantics(VerificationCommandMeaning::Doctor, vec![doctor_step()])
}

/// Define `plan` as the three reference plan/host-plan smoke checks from current `AGENTS.md`.
#[must_use]
pub fn plan_verification_command_semantics() -> VerificationCommandSemantics {
    semantics(
        VerificationCommandMeaning::Plan,
        vec![
            plan_quarry_step(),
            plan_glossless_step(),
            host_plan_quarry_step(),
        ],
    )
}

fn semantics(
    meaning: VerificationCommandMeaning,
    steps: Vec<VerificationCommandStepSemantics>,
) -> VerificationCommandSemantics {
    VerificationCommandSemantics {
        schema_version: VERIFICATION_COMMAND_SEMANTICS_SCHEMA_VERSION,
        meaning,
        working_directory: VerificationCommandWorkingDirectory::RepositoryRoot,
        steps,
    }
}

fn repository_bootstrap(arguments: &[&'static str]) -> VerificationCommandStepSemantics {
    VerificationCommandStepSemantics {
        program: VerificationProgramSemantics {
            kind: VerificationProgramKind::RepositoryProgram,
            repository_relative_path: Some(REPOSITORY_BOOTSTRAP_PATH),
            capability: None,
        },
        arguments: arguments.to_vec(),
    }
}

fn cargo(arguments: &[&'static str]) -> VerificationCommandStepSemantics {
    VerificationCommandStepSemantics {
        program: VerificationProgramSemantics {
            kind: VerificationProgramKind::ToolCapability,
            repository_relative_path: None,
            capability: Some(CARGO_CAPABILITY),
        },
        arguments: arguments.to_vec(),
    }
}

fn doctor_step() -> VerificationCommandStepSemantics {
    cargo(&[
        "run", "--locked", "--quiet", "--", "--output", "json", "doctor",
    ])
}

fn plan_quarry_step() -> VerificationCommandStepSemantics {
    cargo(&[
        "run",
        "--locked",
        "--quiet",
        "--",
        "plan",
        "--file",
        "examples/quarry.yml",
    ])
}

fn plan_glossless_step() -> VerificationCommandStepSemantics {
    cargo(&[
        "run",
        "--locked",
        "--quiet",
        "--",
        "--output",
        "json",
        "plan",
        "--file",
        "examples/glossless.yml",
    ])
}

fn host_plan_quarry_step() -> VerificationCommandStepSemantics {
    cargo(&[
        "run",
        "--locked",
        "--quiet",
        "--",
        "--output",
        "json",
        "host",
        "plan",
        "--file",
        "examples/quarry.yml",
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_meanings_are_stable_and_exact() {
        let [required, doctor, plan] = glaeda_verification_command_semantics();

        assert_eq!(required.schema_version(), 1);
        assert_eq!(required.meaning(), VerificationCommandMeaning::Required);
        assert_eq!(doctor.meaning(), VerificationCommandMeaning::Doctor);
        assert_eq!(plan.meaning(), VerificationCommandMeaning::Plan);
        assert_eq!(required.steps().len(), 8);
        assert_eq!(doctor.steps().len(), 1);
        assert_eq!(plan.steps().len(), 3);
        assert_eq!(required.steps()[4], doctor.steps()[0]);
        assert_eq!(&required.steps()[5..], plan.steps());
        for semantics in [&required, &doctor, &plan] {
            assert_eq!(
                semantics.working_directory(),
                VerificationCommandWorkingDirectory::RepositoryRoot
            );
        }
    }

    #[test]
    fn required_steps_match_named_profile_contract_in_order() {
        let required = required_verification_command_semantics();
        let steps = required.steps();

        assert_eq!(
            steps[0].program().kind(),
            VerificationProgramKind::RepositoryProgram
        );
        assert_eq!(
            steps[0].program().repository_relative_path(),
            Some("scripts/bootstrap")
        );
        assert_eq!(steps[0].program().capability(), None);
        assert_eq!(steps[0].arguments(), &["--output", "json"]);

        let cargo_arguments = [
            &["fmt", "--all", "--", "--check"][..],
            &[
                "clippy",
                "--locked",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ][..],
            &["test", "--locked", "--all-targets", "--all-features"][..],
            &[
                "run", "--locked", "--quiet", "--", "--output", "json", "doctor",
            ][..],
            &[
                "run",
                "--locked",
                "--quiet",
                "--",
                "plan",
                "--file",
                "examples/quarry.yml",
            ][..],
            &[
                "run",
                "--locked",
                "--quiet",
                "--",
                "--output",
                "json",
                "plan",
                "--file",
                "examples/glossless.yml",
            ][..],
            &[
                "run",
                "--locked",
                "--quiet",
                "--",
                "--output",
                "json",
                "host",
                "plan",
                "--file",
                "examples/quarry.yml",
            ][..],
        ];
        for (step, expected_arguments) in steps[1..].iter().zip(cargo_arguments) {
            assert_eq!(
                step.program().kind(),
                VerificationProgramKind::ToolCapability
            );
            assert_eq!(step.program().repository_relative_path(), None);
            assert_eq!(step.program().capability(), Some("cargo"));
            assert_eq!(step.arguments(), expected_arguments);
        }
    }

    #[test]
    fn canonical_json_is_versioned_ordered_and_path_free() {
        let required = required_verification_command_semantics();
        let first = required.canonical_json().expect("canonical JSON");
        let second = required.canonical_json().expect("canonical JSON");

        assert_eq!(first, second);
        assert!(first.starts_with("{\"schema_version\":1,\"meaning\":\"required\""));
        assert!(first.contains("\"repository_relative_path\":\"scripts/bootstrap\""));
        assert!(first.contains("\"capability\":\"cargo\""));
        assert!(first.contains("\"arguments\":[\"fmt\",\"--all\",\"--\",\"--check\"]"));
        for forbidden in [
            "/home/",
            "/Users/",
            "CARGO_HOME=",
            "RUSTUP_HOME=",
            "credential",
            "token",
            "secret",
        ] {
            assert!(
                !first.contains(forbidden),
                "canonical JSON leaked {forbidden}"
            );
        }
    }
}
