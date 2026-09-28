//! Pure descriptive latency summaries for exact hot-fleet A-B-B-A comparisons.
//!
//! This module preserves the accepted `HotFleetComparisonReportV1` wire contract and composes it
//! with final-result p50/p90 observations already derived by each window receipt. Missing validated
//! latency remains explicit through a 0/1/2-window observation count.

use serde::Serialize;

use crate::hot_fleet_window::{
    HotFleetComparisonRefusal, HotFleetComparisonReportV1, HotFleetWindowReceiptV1,
    compare_hot_fleet_abba,
};

pub const HOT_FLEET_LATENCY_COMPARISON_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum HotFleetLatencyComparisonDocumentType {
    HotFleetLatencyComparisonReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HotFleetObservedLatencyRange {
    pub observed_windows: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lower_millis: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upper_millis: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HotFleetArmLatencySummary {
    pub final_result_p50: HotFleetObservedLatencyRange,
    pub final_result_p90: HotFleetObservedLatencyRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotFleetLatencyComparisonReportV1 {
    document_type: HotFleetLatencyComparisonDocumentType,
    pub schema_version: u8,
    pub comparison: HotFleetComparisonReportV1,
    pub baseline_latency: HotFleetArmLatencySummary,
    pub candidate_latency: HotFleetArmLatencySummary,
}

impl HotFleetLatencyComparisonReportV1 {
    /// Render the accepted A-B-B-A comparison plus descriptive latency ranges.
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut output = self.comparison.render_human();
        output.push_str(&format!(
            "baseline final-result p50 range: {}\nbaseline final-result p90 range: {}\ncandidate final-result p50 range: {}\ncandidate final-result p90 range: {}\n",
            render_latency_range(self.baseline_latency.final_result_p50),
            render_latency_range(self.baseline_latency.final_result_p90),
            render_latency_range(self.candidate_latency.final_result_p50),
            render_latency_range(self.candidate_latency.final_result_p90),
        ));
        output
    }

    /// Render deterministic pretty JSON from the composed typed report.
    ///
    /// # Errors
    ///
    /// Returns only if serialization of the fixed model fails.
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Compare four exact contention windows and retain their observed final-result latency ranges.
///
/// The existing A-B-B-A reducer remains the sole comparability gate. This wrapper adds no policy,
/// significance, routing, admission, execution, or experiment authority.
///
/// # Errors
///
/// Returns the existing explicit comparison refusal when the four windows are not exact comparable
/// work.
pub fn compare_hot_fleet_abba_with_latency(
    windows: [&HotFleetWindowReceiptV1; 4],
) -> Result<HotFleetLatencyComparisonReportV1, HotFleetComparisonRefusal> {
    let comparison = compare_hot_fleet_abba(windows)?;
    let [a1, b1, b2, a2] = windows;

    Ok(HotFleetLatencyComparisonReportV1 {
        document_type: HotFleetLatencyComparisonDocumentType::HotFleetLatencyComparisonReport,
        schema_version: HOT_FLEET_LATENCY_COMPARISON_SCHEMA_VERSION,
        comparison,
        baseline_latency: arm_latency_summary([a1, a2]),
        candidate_latency: arm_latency_summary([b1, b2]),
    })
}

fn arm_latency_summary(windows: [&HotFleetWindowReceiptV1; 2]) -> HotFleetArmLatencySummary {
    let [first, second] = windows;
    HotFleetArmLatencySummary {
        final_result_p50: observed_latency_range([
            first.final_result_p50_millis(),
            second.final_result_p50_millis(),
        ]),
        final_result_p90: observed_latency_range([
            first.final_result_p90_millis(),
            second.final_result_p90_millis(),
        ]),
    }
}

fn observed_latency_range(values: [Option<u64>; 2]) -> HotFleetObservedLatencyRange {
    match values {
        [None, None] => HotFleetObservedLatencyRange {
            observed_windows: 0,
            lower_millis: None,
            upper_millis: None,
        },
        [Some(value), None] | [None, Some(value)] => HotFleetObservedLatencyRange {
            observed_windows: 1,
            lower_millis: Some(value),
            upper_millis: Some(value),
        },
        [Some(first), Some(second)] => HotFleetObservedLatencyRange {
            observed_windows: 2,
            lower_millis: Some(first.min(second)),
            upper_millis: Some(first.max(second)),
        },
    }
}

fn render_latency_range(range: HotFleetObservedLatencyRange) -> String {
    match (range.lower_millis, range.upper_millis) {
        (None, None) => format!("unknown ({}/2 windows)", range.observed_windows),
        (Some(lower), Some(upper)) if lower == upper => {
            format!("{lower} ms ({}/2 windows)", range.observed_windows)
        }
        (Some(lower), Some(upper)) => {
            format!("{lower}..{upper} ms ({}/2 windows)", range.observed_windows)
        }
        _ => "invalid latency range".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::compute_workload::ComputeTrustClass;
    use crate::execution_admission::{EpochMillis, ExecutionRequestId};
    use crate::hot_execution_performance::{
        HotBuildState, HotDependencyState, HotExecutionHeat, HotExecutionMilestones,
        HotExecutionMode, HotExecutionPerformanceIdentity, HotExecutionPerformanceReceipt,
        HotExecutionResultClass, HotIndexServiceState, HotRepositoryState, HotSandboxState,
    };
    use crate::hot_fleet_window::{
        HotFleetArm, HotFleetComparisonRefusalReason, HotFleetConcurrencyBasis,
        HotFleetConcurrencyObservation, HotFleetConcurrencyObservationSource, HotFleetDecisionKind,
        HotFleetSemanticValidation, HotFleetWindowCloseReason, HotFleetWindowIdentity,
        HotFleetWindowTiming, HotFleetWorkItem,
    };

    fn digest(seed: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
    }

    fn performance_receipt(
        work_id: &str,
        final_millis: u64,
        result: HotExecutionResultClass,
    ) -> HotExecutionPerformanceReceipt {
        HotExecutionPerformanceReceipt::new(
            HotExecutionPerformanceIdentity::new(
                work_id,
                "project",
                "source",
                "candidate",
                "lima-vz",
                "apple-silicon",
                "profile",
            )
            .unwrap(),
            HotExecutionMode::ResidentTaskLoop,
            final_millis,
            HotExecutionMilestones::new(
                Some(10),
                Some(10),
                Some(10),
                Some(20),
                Some(final_millis - 10),
                Some(final_millis),
                None,
            )
            .unwrap(),
            HotExecutionHeat::new(
                HotSandboxState::ResidentHit,
                HotRepositoryState::ResidentHit,
                HotDependencyState::ResidentHit,
                HotBuildState::IncrementalHit,
                HotIndexServiceState::ResidentHit,
            ),
            None,
            None,
            result,
        )
        .unwrap()
    }

    fn members(validated: usize, latency_base: u64) -> Vec<HotFleetWorkItem> {
        (0..4)
            .map(|index| {
                let work_id = format!("work-{index}");
                let result = if index < validated {
                    HotExecutionResultClass::Succeeded
                } else {
                    HotExecutionResultClass::Failed
                };
                let validation = if index < validated {
                    HotFleetSemanticValidation::Accepted
                } else {
                    HotFleetSemanticValidation::Unobserved
                };
                let final_millis = latency_base + index as u64 * 10;
                let attempt = crate::hot_fleet_window::HotFleetAttemptEvidence::new(
                    ExecutionRequestId::parse(&format!("req-{index}")).unwrap(),
                    Some(performance_receipt(&work_id, final_millis, result)),
                    validation,
                    false,
                )
                .unwrap();
                HotFleetWorkItem::new(
                    &work_id,
                    digest(char::from(b'a' + index as u8)),
                    Some(attempt),
                )
                .unwrap()
            })
            .collect()
    }

    fn identity(arm: HotFleetArm, block: &str, decision: &str) -> HotFleetWindowIdentity {
        HotFleetWindowIdentity::new(
            "exp-1",
            block,
            arm,
            HotFleetDecisionKind::HeavySlotPolicy,
            decision,
            "validator-v1",
            "same-source-class",
            "lima-vz",
            "apple-silicon",
            digest('f'),
            "blocked-arrivals",
            ComputeTrustClass::UltraTrusted,
            "non-treatment-v1",
        )
        .unwrap()
    }

    fn window(
        arm: HotFleetArm,
        block: &str,
        decision: &str,
        validated: usize,
        latency_base: u64,
    ) -> HotFleetWindowReceiptV1 {
        HotFleetWindowReceiptV1::new(
            identity(arm, block, decision),
            HotFleetWindowTiming::new(
                EpochMillis::new(1_000).unwrap(),
                EpochMillis::new(2_000).unwrap(),
                1_000,
                1_000,
                HotFleetWindowCloseReason::Deadline,
            )
            .unwrap(),
            HotFleetConcurrencyObservation::new(
                HotFleetConcurrencyBasis::HeavyExecution,
                4,
                HotFleetConcurrencyObservationSource::HarnessOwned,
            )
            .unwrap(),
            members(validated, latency_base),
            &[],
        )
        .unwrap()
    }

    #[test]
    fn comparable_abba_reports_observed_p50_and_p90_ranges() {
        let a1 = window(HotFleetArm::Baseline, "a1", "linked", 4, 700);
        let b1 = window(HotFleetArm::Candidate, "b1", "overlay", 4, 400);
        let b2 = window(HotFleetArm::Candidate, "b2", "overlay", 4, 500);
        let a2 = window(HotFleetArm::Baseline, "a2", "linked", 4, 800);

        let report = compare_hot_fleet_abba_with_latency([&a1, &b1, &b2, &a2]).unwrap();

        assert_eq!(report.baseline_latency.final_result_p50.observed_windows, 2);
        assert_eq!(
            report.baseline_latency.final_result_p50.lower_millis,
            Some(710)
        );
        assert_eq!(
            report.baseline_latency.final_result_p50.upper_millis,
            Some(810)
        );
        assert_eq!(
            report.baseline_latency.final_result_p90.lower_millis,
            Some(730)
        );
        assert_eq!(
            report.baseline_latency.final_result_p90.upper_millis,
            Some(830)
        );
        assert_eq!(
            report.candidate_latency.final_result_p50.lower_millis,
            Some(410)
        );
        assert_eq!(
            report.candidate_latency.final_result_p50.upper_millis,
            Some(510)
        );
        assert_eq!(
            report.candidate_latency.final_result_p90.lower_millis,
            Some(430)
        );
        assert_eq!(
            report.candidate_latency.final_result_p90.upper_millis,
            Some(530)
        );
    }

    #[test]
    fn missing_validated_latency_remains_a_partial_arm_observation() {
        let a1 = window(HotFleetArm::Baseline, "a1", "linked", 4, 700);
        let b1 = window(HotFleetArm::Candidate, "b1", "overlay", 4, 400);
        let b2 = window(HotFleetArm::Candidate, "b2", "overlay", 0, 500);
        let a2 = window(HotFleetArm::Baseline, "a2", "linked", 4, 800);

        let report = compare_hot_fleet_abba_with_latency([&a1, &b1, &b2, &a2]).unwrap();

        assert_eq!(
            report.candidate_latency.final_result_p90.observed_windows,
            1
        );
        assert_eq!(
            report.candidate_latency.final_result_p90.lower_millis,
            Some(430)
        );
        assert_eq!(
            report.candidate_latency.final_result_p90.upper_millis,
            Some(430)
        );
    }

    #[test]
    fn missing_latency_in_both_windows_remains_unknown() {
        let a1 = window(HotFleetArm::Baseline, "a1", "linked", 4, 700);
        let b1 = window(HotFleetArm::Candidate, "b1", "overlay", 0, 400);
        let b2 = window(HotFleetArm::Candidate, "b2", "overlay", 0, 500);
        let a2 = window(HotFleetArm::Baseline, "a2", "linked", 4, 800);

        let report = compare_hot_fleet_abba_with_latency([&a1, &b1, &b2, &a2]).unwrap();

        assert_eq!(
            report.candidate_latency.final_result_p90,
            HotFleetObservedLatencyRange {
                observed_windows: 0,
                lower_millis: None,
                upper_millis: None,
            }
        );
        assert!(
            report
                .render_human()
                .contains("candidate final-result p90 range: unknown (0/2 windows)")
        );
    }

    #[test]
    fn existing_comparison_refusal_is_preserved() {
        let a1 = window(HotFleetArm::Baseline, "a1", "linked", 4, 700);
        let b1 = window(HotFleetArm::Candidate, "b1", "overlay", 4, 400);
        let b2 = window(HotFleetArm::Candidate, "b2", "overlay", 4, 500);
        let a2 = window(HotFleetArm::Baseline, "a2", "linked", 4, 800);

        let refusal = compare_hot_fleet_abba_with_latency([&b1, &a1, &b2, &a2]).unwrap_err();
        assert_eq!(refusal.reason, HotFleetComparisonRefusalReason::ArmOrder);
    }

    #[test]
    fn human_and_json_render_the_same_latency_observations() {
        let a1 = window(HotFleetArm::Baseline, "a1", "linked", 4, 700);
        let b1 = window(HotFleetArm::Candidate, "b1", "overlay", 4, 400);
        let b2 = window(HotFleetArm::Candidate, "b2", "overlay", 0, 500);
        let a2 = window(HotFleetArm::Baseline, "a2", "linked", 4, 800);

        let report = compare_hot_fleet_abba_with_latency([&a1, &b1, &b2, &a2]).unwrap();
        let human = report.render_human();
        let json = report.render_json().unwrap();

        assert!(human.contains("baseline final-result p90 range: 730..830 ms (2/2 windows)"));
        assert!(human.contains("candidate final-result p90 range: 430 ms (1/2 windows)"));
        assert!(json.contains("\"observed_windows\": 1"));
        assert!(json.contains("\"lower_millis\": 430"));
        assert!(json.contains("\"document_type\": \"hot_fleet_comparison_report\""));
    }
}
