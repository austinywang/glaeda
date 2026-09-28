use std::fmt;

use serde::Serialize;

pub const OPERATOR_PUBLIC_ERROR_SCHEMA_VERSION: u8 = 1;
pub const MAX_OPERATOR_PUBLIC_ERROR_SUMMARY_BYTES: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorRetryClass {
    Immediate,
    AfterRefresh,
    AfterRepair,
    AfterDependency,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorRemediationClass {
    Retry,
    Refresh,
    Repair,
    Dependency,
    ApprovalRequired,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorDependencyClass {
    Configuration,
    DurableState,
    Lima,
    RunnerReadiness,
    Repository,
    Github,
    Service,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorApprovalClass {
    CredentialChange,
    OperatorService,
    PaidCapacity,
    ExternalPublication,
    ReleaseSigning,
    DestructiveDataChange,
    IrreversibleMigration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum OperatorSuggestedCommand {
    #[serde(rename = "glaeda status")]
    Status,
    #[serde(rename = "glaeda worker init")]
    WorkerInit,
    #[serde(rename = "glaeda worker run-once")]
    WorkerRunOnce,
    #[serde(rename = "glaeda queue list")]
    QueueList,
}

pub const ALL_OPERATOR_SUGGESTED_COMMANDS: &[OperatorSuggestedCommand] = &[
    OperatorSuggestedCommand::Status,
    OperatorSuggestedCommand::WorkerInit,
    OperatorSuggestedCommand::WorkerRunOnce,
    OperatorSuggestedCommand::QueueList,
];

impl OperatorSuggestedCommand {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Status => "glaeda status",
            Self::WorkerInit => "glaeda worker init",
            Self::WorkerRunOnce => "glaeda worker run-once",
            Self::QueueList => "glaeda queue list",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OperatorErrorSpec {
    summary: &'static str,
    retry: OperatorRetryClass,
    remediation: OperatorRemediationClass,
    suggested_command: Option<OperatorSuggestedCommand>,
    dependency: Option<OperatorDependencyClass>,
    approval: Option<OperatorApprovalClass>,
}

impl OperatorErrorSpec {
    const fn new(
        summary: &'static str,
        retry: OperatorRetryClass,
        remediation: OperatorRemediationClass,
        suggested_command: Option<OperatorSuggestedCommand>,
        dependency: Option<OperatorDependencyClass>,
        approval: Option<OperatorApprovalClass>,
    ) -> Self {
        Self {
            summary,
            retry,
            remediation,
            suggested_command,
            dependency,
            approval,
        }
    }
}

#[rustfmt::skip]
macro_rules! operator_error_spec {
    (
        summary: $summary:literal,
        retry: $retry:ident,
        remediation: $remediation:ident,
        command: $command:expr,
        dependency: $dependency:expr,
        approval: $approval:expr,
    ) => {
        OperatorErrorSpec::new(
            $summary,
            OperatorRetryClass::$retry,
            OperatorRemediationClass::$remediation,
            $command,
            $dependency,
            $approval,
        )
    };
}

#[rustfmt::skip]
macro_rules! define_operator_errors {
    (
        $(
            $code:ident { $($fields:tt)* },
        )+
    ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum OperatorErrorCode {
            $($code,)+
        }

        pub const ALL_OPERATOR_ERROR_CODES: &[OperatorErrorCode] = &[
            $(OperatorErrorCode::$code,)+
        ];

        const fn operator_error_spec(code: OperatorErrorCode) -> OperatorErrorSpec {
            match code {
                $(
                    OperatorErrorCode::$code => operator_error_spec!($($fields)*,),
                )+
            }
        }
    };
}

include!("operator_error/catalog.rs");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperatorPublicError {
    schema_version: u8,
    code: OperatorErrorCode,
    summary: &'static str,
    retry: OperatorRetryClass,
    remediation: OperatorRemediationClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_command: Option<OperatorSuggestedCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependency: Option<OperatorDependencyClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval: Option<OperatorApprovalClass>,
}

impl OperatorPublicError {
    #[must_use]
    pub const fn from_code(code: OperatorErrorCode) -> Self {
        let spec = operator_error_spec(code);
        Self {
            schema_version: OPERATOR_PUBLIC_ERROR_SCHEMA_VERSION,
            code,
            summary: spec.summary,
            retry: spec.retry,
            remediation: spec.remediation,
            suggested_command: spec.suggested_command,
            dependency: spec.dependency,
            approval: spec.approval,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn code(&self) -> OperatorErrorCode {
        self.code
    }

    #[must_use]
    pub const fn summary(&self) -> &'static str {
        self.summary
    }

    #[must_use]
    pub const fn retry(&self) -> OperatorRetryClass {
        self.retry
    }

    #[must_use]
    pub const fn remediation(&self) -> OperatorRemediationClass {
        self.remediation
    }

    #[must_use]
    pub const fn suggested_command(&self) -> Option<OperatorSuggestedCommand> {
        self.suggested_command
    }

    #[must_use]
    pub const fn dependency(&self) -> Option<OperatorDependencyClass> {
        self.dependency
    }

    #[must_use]
    pub const fn approval(&self) -> Option<OperatorApprovalClass> {
        self.approval
    }
}

impl fmt::Display for OperatorPublicError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.summary)
    }
}

impl std::error::Error for OperatorPublicError {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::{
        ALL_OPERATOR_ERROR_CODES, ALL_OPERATOR_SUGGESTED_COMMANDS,
        MAX_OPERATOR_PUBLIC_ERROR_SUMMARY_BYTES, OPERATOR_PUBLIC_ERROR_SCHEMA_VERSION,
        OperatorApprovalClass, OperatorDependencyClass, OperatorErrorCode, OperatorPublicError,
        OperatorRemediationClass, OperatorRetryClass, OperatorSuggestedCommand,
    };

    #[test]
    fn every_code_has_one_unique_complete_mapping() {
        assert_eq!(ALL_OPERATOR_ERROR_CODES.len(), 61);
        let mut encoded_codes = BTreeSet::new();

        for &code in ALL_OPERATOR_ERROR_CODES {
            let public = OperatorPublicError::from_code(code);
            let encoded_code = serde_json::to_string(&code).expect("serialize code");
            assert!(
                encoded_codes.insert(encoded_code),
                "duplicate code: {code:?}"
            );
            assert_eq!(
                public.schema_version(),
                OPERATOR_PUBLIC_ERROR_SCHEMA_VERSION
            );
            assert_eq!(public.code(), code);
            assert!(!public.summary().is_empty());
            assert!(public.summary().len() <= MAX_OPERATOR_PUBLIC_ERROR_SUMMARY_BYTES);
            assert!(!public.summary().contains('\n'));
            assert!(!public.summary().contains('\r'));

            match public.remediation() {
                OperatorRemediationClass::ApprovalRequired => {
                    assert!(public.approval().is_some(), "missing approval: {code:?}");
                }
                _ => assert!(public.approval().is_none(), "unexpected approval: {code:?}"),
            }

            if public.remediation() == OperatorRemediationClass::Dependency {
                assert!(
                    public.dependency().is_some(),
                    "missing dependency: {code:?}"
                );
                assert_eq!(public.retry(), OperatorRetryClass::AfterDependency);
            }

            if public.remediation() == OperatorRemediationClass::Terminal {
                assert_eq!(public.retry(), OperatorRetryClass::Never);
            }
        }
    }

    #[test]
    fn suggested_commands_are_exact_static_allowlist() {
        let expected = [
            "glaeda status",
            "glaeda worker init",
            "glaeda worker run-once",
            "glaeda queue list",
        ];

        for (command, expected) in ALL_OPERATOR_SUGGESTED_COMMANDS.iter().zip(expected) {
            assert_eq!(command.as_str(), expected);
            assert_eq!(
                serde_json::to_value(*command).expect("serialize command"),
                json!(expected)
            );
        }

        assert_eq!(ALL_OPERATOR_SUGGESTED_COMMANDS.len(), expected.len());
        assert_eq!(OperatorSuggestedCommand::Status.as_str(), "glaeda status");
    }

    #[test]
    fn exact_json_is_derived_from_one_code() {
        let public = OperatorPublicError::from_code(OperatorErrorCode::DurableStateMissing);
        assert_eq!(
            serde_json::to_value(public).expect("serialize public error"),
            json!({
                "schema_version": 1,
                "code": "durable_state_missing",
                "summary": "Durable personal-worker state is missing.",
                "retry": "after_repair",
                "remediation": "repair",
                "suggested_command": "glaeda worker init",
                "dependency": "durable_state"
            })
        );
    }

    #[test]
    fn accepted_product_failure_classes_have_distinct_codes_and_tuples() {
        let cases = [
            (
                OperatorErrorCode::DurableStateRecoveryRequired,
                OperatorRetryClass::AfterRepair,
                OperatorRemediationClass::Repair,
                Some(OperatorSuggestedCommand::WorkerRunOnce),
                Some(OperatorDependencyClass::DurableState),
            ),
            (
                OperatorErrorCode::UnsupportedPlatform,
                OperatorRetryClass::Never,
                OperatorRemediationClass::Terminal,
                Some(OperatorSuggestedCommand::Status),
                None,
            ),
            (
                OperatorErrorCode::LimaIdentityMismatch,
                OperatorRetryClass::AfterRepair,
                OperatorRemediationClass::Repair,
                Some(OperatorSuggestedCommand::Status),
                Some(OperatorDependencyClass::Lima),
            ),
            (
                OperatorErrorCode::CapacityRefused,
                OperatorRetryClass::AfterRefresh,
                OperatorRemediationClass::Refresh,
                Some(OperatorSuggestedCommand::WorkerRunOnce),
                Some(OperatorDependencyClass::RunnerReadiness),
            ),
            (
                OperatorErrorCode::TerminalClassificationInconclusive,
                OperatorRetryClass::Never,
                OperatorRemediationClass::Terminal,
                Some(OperatorSuggestedCommand::Status),
                None,
            ),
            (
                OperatorErrorCode::DurableStateVersionIncompatible,
                OperatorRetryClass::AfterRepair,
                OperatorRemediationClass::Repair,
                Some(OperatorSuggestedCommand::Status),
                Some(OperatorDependencyClass::DurableState),
            ),
        ];

        for (code, retry, remediation, command, dependency) in cases {
            let public = OperatorPublicError::from_code(code);
            assert_eq!(public.retry(), retry, "retry mismatch: {code:?}");
            assert_eq!(
                public.remediation(),
                remediation,
                "remediation mismatch: {code:?}"
            );
            assert_eq!(
                public.suggested_command(),
                command,
                "command mismatch: {code:?}"
            );
            assert_eq!(
                public.dependency(),
                dependency,
                "dependency mismatch: {code:?}"
            );
            assert!(public.approval().is_none());
        }
    }

    #[test]
    fn approval_codes_map_to_exact_human_effect_classes() {
        let cases = [
            (
                OperatorErrorCode::CredentialChangeApprovalRequired,
                OperatorApprovalClass::CredentialChange,
            ),
            (
                OperatorErrorCode::OperatorServiceApprovalRequired,
                OperatorApprovalClass::OperatorService,
            ),
            (
                OperatorErrorCode::PaidCapacityApprovalRequired,
                OperatorApprovalClass::PaidCapacity,
            ),
            (
                OperatorErrorCode::ExternalPublicationApprovalRequired,
                OperatorApprovalClass::ExternalPublication,
            ),
            (
                OperatorErrorCode::ReleaseSigningApprovalRequired,
                OperatorApprovalClass::ReleaseSigning,
            ),
            (
                OperatorErrorCode::DestructiveDataChangeApprovalRequired,
                OperatorApprovalClass::DestructiveDataChange,
            ),
            (
                OperatorErrorCode::IrreversibleMigrationApprovalRequired,
                OperatorApprovalClass::IrreversibleMigration,
            ),
        ];

        for (code, approval) in cases {
            let public = OperatorPublicError::from_code(code);
            assert_eq!(
                public.remediation(),
                OperatorRemediationClass::ApprovalRequired
            );
            assert_eq!(public.retry(), OperatorRetryClass::AfterDependency);
            assert_eq!(public.approval(), Some(approval));
            assert!(public.suggested_command().is_none());
        }
    }

    #[test]
    fn public_json_debug_and_display_cannot_include_private_sentinels() {
        let forbidden = [
            "/Users/private-operator",
            "/home/private-runner",
            "PRIVATE_TOKEN_SENTINEL",
            "Authorization: Bearer private",
            "private-command --secret",
            "private child stderr",
            "/sys/fs/cgroup/private",
            "pid=424242",
            "private kernel prose",
        ];

        for &code in ALL_OPERATOR_ERROR_CODES {
            let public = OperatorPublicError::from_code(code);
            let encoded = serde_json::to_string(&public).expect("serialize public error");
            let debug = format!("{public:?}");
            let display = public.to_string();
            for sentinel in forbidden {
                assert!(!encoded.contains(sentinel), "{code:?} leaked in JSON");
                assert!(!debug.contains(sentinel), "{code:?} leaked in Debug");
                assert!(!display.contains(sentinel), "{code:?} leaked in Display");
            }
        }
    }
}
