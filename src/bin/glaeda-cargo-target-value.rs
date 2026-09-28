#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("Cargo target value observation requires Linux");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::cmp::Ordering;
    use std::collections::BTreeSet;
    use std::fs::File;
    use std::io::{Read as _, Take};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use clap::{ArgAction, Parser, ValueEnum};
    use glaeda::cargo_target_observation::{CargoTargetState, observe_cargo_target};
    use glaeda::process::ProcessExecutor;
    use glaeda::project_checkout_observation::ProjectCheckoutObserver;
    use rustix::fs::{FileType, Mode, OFlags, fstat, open};
    use rustix::process::getuid;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    const REPORT_SCHEMA_VERSION: u8 = 1;
    const MEASUREMENT_SCHEMA_VERSION: u8 = 6;
    const MAX_SAMPLES_PER_ARM: usize = 32;
    const MAX_MEASUREMENT_BYTES: u64 = 1_048_576;
    const GIT_PROGRAM: &str = "/usr/bin/git";
    const SHA256_PREFIX: &str = "sha256:";

    #[derive(Debug, Parser)]
    #[command(
        name = "glaeda-cargo-target-value",
        about = "Compare bounded cold and warm measurements for one current Cargo target"
    )]
    struct Cli {
        /// Explicit canonical absolute Git checkout root containing the current target.
        #[arg(long)]
        checkout: PathBuf,

        /// Successful schema-v6 measurement whose target was absent before the command.
        #[arg(long, action = ArgAction::Append, required = true)]
        cold: Vec<PathBuf>,

        /// Successful schema-v6 measurement that reused the current target directory.
        #[arg(long, action = ArgAction::Append, required = true)]
        warm: Vec<PathBuf>,

        /// Select human or JSON output.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        output: OutputFormat,
    }

    #[derive(Debug, Clone, Copy, ValueEnum)]
    enum OutputFormat {
        Human,
        Json,
    }

    #[derive(Debug, Deserialize)]
    struct CacheView {
        path: String,
        mode: String,
    }

    #[derive(Debug, Deserialize)]
    struct MeasurementReceipt {
        schema_version: u8,
        document_type: String,
        authority: String,
        comparison_key: Option<String>,
        cross_worktree: bool,
        resource_profile: Option<String>,
        cpu_set: Option<String>,
        timeout_seconds: Option<f64>,
        cache_views: Vec<CacheView>,
        native_target_observation: Value,
        runtime: Option<RuntimeEvidence>,
        elapsed_seconds: f64,
        preparation_elapsed_seconds: f64,
        user_cpu_seconds: Option<f64>,
        system_cpu_seconds: Option<f64>,
        max_rss_kib: Option<u64>,
        resource_accounting: String,
        exit_code: Option<i32>,
        signal: Option<i32>,
        completion_reason: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RuntimeEvidence {
        id: String,
        program_sha256: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        descendant_path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        runtime_bin_binding_sha256: Option<String>,
    }

    #[derive(Debug)]
    struct AcceptedSample {
        receipt_identity: ReceiptIdentity,
        comparison_key: String,
        resource_profile: Option<String>,
        cpu_set: Option<String>,
        timeout_seconds: Option<f64>,
        runtime: Option<RuntimeEvidence>,
        resource_accounting: String,
        elapsed_seconds: f64,
        user_cpu_seconds: f64,
        system_cpu_seconds: f64,
        max_rss_kib: u64,
    }

    #[derive(Debug, Serialize)]
    struct ArmSummary {
        sample_count: usize,
        median_elapsed_seconds: f64,
        median_user_cpu_seconds: f64,
        median_system_cpu_seconds: f64,
        median_total_cpu_seconds: f64,
        median_max_rss_kib: f64,
    }

    #[derive(Debug, Serialize)]
    struct SavingsSummary {
        median_elapsed_seconds_saved: f64,
        median_elapsed_percent_saved: Option<f64>,
        median_elapsed_speedup: Option<f64>,
        median_total_cpu_seconds_saved: f64,
        median_max_rss_kib_delta: f64,
    }

    #[derive(Debug, Serialize)]
    struct CargoTargetValueReport {
        document_type: &'static str,
        schema_version: u8,
        authority: &'static str,
        atomic: bool,
        receipt_authenticity: &'static str,
        currentness: &'static str,
        successful_use_time: &'static str,
        mutation_performed: bool,
        comparison_key: String,
        resource_profile: Option<String>,
        cpu_set: Option<String>,
        timeout_seconds: Option<f64>,
        runtime: Option<RuntimeEvidence>,
        resource_accounting: String,
        checkout: Value,
        current_target: Value,
        current_target_allocated_bytes: u64,
        cold: ArmSummary,
        warm: ArmSummary,
        savings: SavingsSummary,
        value_disposition: &'static str,
        retention_authority: &'static str,
    }

    impl CargoTargetValueReport {
        fn render_human(&self) -> String {
            format!(
                concat!(
                    "Cargo target value: disposition={} authority={} mutation_performed=false\n",
                    "samples cold={} warm={} current_allocated_bytes={}\n",
                    "median_elapsed cold={:.6}s warm={:.6}s saved={:.6}s percent={} speedup={}\n",
                    "median_cpu_saved={:.6}s median_rss_delta={:.1}KiB\n",
                    "successful_use_time={} retention_authority={}\n"
                ),
                self.value_disposition,
                self.authority,
                self.cold.sample_count,
                self.warm.sample_count,
                self.current_target_allocated_bytes,
                self.cold.median_elapsed_seconds,
                self.warm.median_elapsed_seconds,
                self.savings.median_elapsed_seconds_saved,
                format_optional(self.savings.median_elapsed_percent_saved, "%"),
                format_optional(self.savings.median_elapsed_speedup, "x"),
                self.savings.median_total_cpu_seconds_saved,
                self.savings.median_max_rss_kib_delta,
                self.successful_use_time,
                self.retention_authority,
            )
        }
    }

    #[derive(Debug, Serialize)]
    struct ErrorReport<'a> {
        document_type: &'static str,
        schema_version: u8,
        authority: &'static str,
        code: &'a str,
        problem: &'a str,
        mutation_performed: bool,
    }

    pub fn main() -> ExitCode {
        let cli = Cli::parse();
        match run(&cli) {
            Ok(report) => emit_report(cli.output, report),
            Err(error) => emit_error(cli.output, error.code, error.problem),
        }
    }

    fn run(cli: &Cli) -> Result<CargoTargetValueReport, ObservationError> {
        validate_sample_count("cold", cli.cold.len())?;
        validate_sample_count("warm", cli.warm.len())?;

        let observer = ProjectCheckoutObserver::new(GIT_PROGRAM).map_err(|_| {
            unavailable(
                "cargo_target_value_checkout_unavailable",
                "checkout observation is unavailable",
            )
        })?;
        let checkout_before = observer
            .observe(&cli.checkout, &ProcessExecutor)
            .map_err(|_| {
                unavailable(
                    "cargo_target_value_checkout_unavailable",
                    "checkout observation is unavailable",
                )
            })?;
        let target = observe_cargo_target(&cli.checkout)
            .map_err(|error| unavailable(error.code(), error.problem()))?;
        let checkout_after = observer
            .observe(&cli.checkout, &ProcessExecutor)
            .map_err(|_| {
                unavailable(
                    "cargo_target_value_checkout_unavailable",
                    "checkout observation is unavailable",
                )
            })?;
        if checkout_before != checkout_after {
            return Err(unavailable(
                "cargo_target_value_checkout_changed",
                "checkout changed during value observation",
            ));
        }
        let (current_target_id, current_target_allocated_bytes) = match target.state() {
            CargoTargetState::Present {
                target_id,
                allocated_bytes,
                ..
            } => (target_id.as_str().to_owned(), *allocated_bytes),
            CargoTargetState::Absent => {
                return Err(unavailable(
                    "cargo_target_value_target_absent",
                    "current Cargo target is absent",
                ));
            }
        };
        let checkout_value = serde_json::to_value(&checkout_after).map_err(|_| {
            unavailable(
                "cargo_target_value_encoding_failed",
                "checkout observation could not be encoded",
            )
        })?;
        let target_value = serde_json::to_value(&target).map_err(|_| {
            unavailable(
                "cargo_target_value_encoding_failed",
                "Cargo target observation could not be encoded",
            )
        })?;

        let cold = cli
            .cold
            .iter()
            .map(|path| accept_sample(path, Arm::Cold, &checkout_value, &current_target_id))
            .collect::<Result<Vec<_>, _>>()?;
        let warm = cli
            .warm
            .iter()
            .map(|path| accept_sample(path, Arm::Warm, &checkout_value, &current_target_id))
            .collect::<Result<Vec<_>, _>>()?;
        validate_unique_receipts(&cold, &warm)?;
        validate_comparable(&cold, &warm)?;
        let checkout_final = observer
            .observe(&cli.checkout, &ProcessExecutor)
            .map_err(|_| {
                unavailable(
                    "cargo_target_value_checkout_unavailable",
                    "checkout observation is unavailable",
                )
            })?;
        let target_final = observe_cargo_target(&cli.checkout)
            .map_err(|error| unavailable(error.code(), error.problem()))?;
        if checkout_final != checkout_after || target_final != target {
            return Err(unavailable(
                "cargo_target_value_current_state_changed",
                "checkout or Cargo target changed during value observation",
            ));
        }

        let cold_summary = summarize(&cold);
        let warm_summary = summarize(&warm);
        let elapsed_saved =
            cold_summary.median_elapsed_seconds - warm_summary.median_elapsed_seconds;
        let elapsed_percent_saved = (cold_summary.median_elapsed_seconds > 0.0)
            .then_some(elapsed_saved / cold_summary.median_elapsed_seconds * 100.0);
        let elapsed_speedup = (warm_summary.median_elapsed_seconds > 0.0)
            .then_some(cold_summary.median_elapsed_seconds / warm_summary.median_elapsed_seconds);
        let value_disposition = if elapsed_saved > 0.0 {
            "positive_median_rebuild_savings_observed"
        } else {
            "no_positive_median_rebuild_savings_observed"
        };
        let savings = SavingsSummary {
            median_elapsed_seconds_saved: elapsed_saved,
            median_elapsed_percent_saved: elapsed_percent_saved,
            median_elapsed_speedup: elapsed_speedup,
            median_total_cpu_seconds_saved: cold_summary.median_total_cpu_seconds
                - warm_summary.median_total_cpu_seconds,
            median_max_rss_kib_delta: warm_summary.median_max_rss_kib
                - cold_summary.median_max_rss_kib,
        };
        let exemplar = &cold[0];
        Ok(CargoTargetValueReport {
            document_type: "glaeda-cargo-target-value-observation",
            schema_version: REPORT_SCHEMA_VERSION,
            authority: "performance_observation_only",
            atomic: false,
            receipt_authenticity: "unproven_caller_supplied",
            currentness: "bracketed_equal_checkout_and_target_snapshots_non_atomic",
            successful_use_time: "unknown_schema_v6_has_no_epoch_timestamp",
            mutation_performed: false,
            comparison_key: exemplar.comparison_key.clone(),
            resource_profile: exemplar.resource_profile.clone(),
            cpu_set: exemplar.cpu_set.clone(),
            timeout_seconds: exemplar.timeout_seconds,
            runtime: exemplar.runtime.clone(),
            resource_accounting: exemplar.resource_accounting.clone(),
            checkout: checkout_value,
            current_target: target_value,
            current_target_allocated_bytes,
            cold: cold_summary,
            warm: warm_summary,
            savings,
            value_disposition,
            retention_authority: "none",
        })
    }

    #[derive(Debug, Clone, Copy)]
    enum Arm {
        Cold,
        Warm,
    }

    fn accept_sample(
        path: &Path,
        arm: Arm,
        current_checkout: &Value,
        current_target_id: &str,
    ) -> Result<AcceptedSample, ObservationError> {
        let read = read_measurement(path)?;
        let receipt = read.receipt;
        if receipt.schema_version != MEASUREMENT_SCHEMA_VERSION
            || receipt.document_type != "glaeda-hot-run-measurement"
            || receipt.authority != "developer_observation_only"
            || receipt.cross_worktree
        {
            return Err(invalid("measurement identity is unsupported"));
        }
        if receipt.cache_views.len() != 1
            || receipt.cache_views[0].path != "target"
            || receipt.cache_views[0].mode != "native"
        {
            return Err(invalid("measurement does not isolate target:native"));
        }
        validate_resource_contract(
            receipt.resource_profile.as_deref(),
            receipt.cpu_set.as_deref(),
            receipt.timeout_seconds,
            &receipt.resource_accounting,
        )?;
        if let Some(runtime) = receipt.runtime.as_ref() {
            validate_runtime(runtime)?;
        }
        let comparison_key = receipt
            .comparison_key
            .filter(|value| canonical_sha256(value))
            .ok_or_else(|| invalid("measurement comparison key is unavailable"))?;
        if receipt.exit_code != Some(0)
            || receipt.signal.is_some()
            || receipt.completion_reason != "exited"
        {
            return Err(invalid("measurement command was not successful"));
        }
        if !finite_nonnegative(receipt.elapsed_seconds)
            || receipt.preparation_elapsed_seconds != 0.0
        {
            return Err(invalid("measurement elapsed time is invalid"));
        }
        let user_cpu_seconds = receipt
            .user_cpu_seconds
            .filter(|value| finite_nonnegative(*value))
            .ok_or_else(|| invalid("measurement user CPU is unavailable"))?;
        let system_cpu_seconds = receipt
            .system_cpu_seconds
            .filter(|value| finite_nonnegative(*value))
            .ok_or_else(|| invalid("measurement system CPU is unavailable"))?;
        let max_rss_kib = receipt
            .max_rss_kib
            .ok_or_else(|| invalid("measurement maximum RSS is unavailable"))?;
        validate_native_observation(
            &receipt.native_target_observation,
            arm,
            current_checkout,
            current_target_id,
        )?;
        Ok(AcceptedSample {
            receipt_identity: read.identity,
            comparison_key,
            resource_profile: receipt.resource_profile,
            cpu_set: receipt.cpu_set,
            timeout_seconds: receipt.timeout_seconds,
            runtime: receipt.runtime,
            resource_accounting: receipt.resource_accounting,
            elapsed_seconds: receipt.elapsed_seconds,
            user_cpu_seconds,
            system_cpu_seconds,
            max_rss_kib,
        })
    }

    fn validate_native_observation(
        observation: &Value,
        arm: Arm,
        current_checkout: &Value,
        current_target_id: &str,
    ) -> Result<(), ObservationError> {
        if observation.get("authority").and_then(Value::as_str)
            != Some("performance_observation_only")
            || observation.get("atomic").and_then(Value::as_bool) != Some(false)
        {
            return Err(invalid("native target observation identity is unsupported"));
        }
        let before = observation
            .get("before")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("native target before observation is unavailable"))?;
        if before.get("checkout") != Some(current_checkout) {
            return Err(invalid(
                "measurement checkout does not match the current checkout",
            ));
        }
        let after = observation
            .get("after")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("native target after observation is unavailable"))?;
        let after_checkout = observed_terminal(after.get("checkout"), "checkout")?;
        if after_checkout != current_checkout {
            return Err(invalid(
                "terminal checkout does not match the current checkout",
            ));
        }
        let before_target = before
            .get("cargo_target")
            .ok_or_else(|| invalid("native target before observation is unavailable"))?;
        let after_target = observed_terminal(after.get("cargo_target"), "Cargo target")?;
        match arm {
            Arm::Cold => {
                if target_state(before_target)? != TargetState::Absent {
                    return Err(invalid("cold measurement did not start without a target"));
                }
                target_id(after_target)?;
            }
            Arm::Warm => {
                let before_id = target_id(before_target)?;
                let after_id = target_id(after_target)?;
                if before_id != after_id || before_id != current_target_id {
                    return Err(invalid(
                        "warm measurement does not reuse the current target directory",
                    ));
                }
            }
        }
        Ok(())
    }

    fn observed_terminal<'a>(
        value: Option<&'a Value>,
        kind: &str,
    ) -> Result<&'a Value, ObservationError> {
        let value = value.ok_or_else(|| invalid("terminal observation is unavailable"))?;
        if value.get("state").and_then(Value::as_str) != Some("observed") {
            return Err(invalid(match kind {
                "checkout" => "terminal checkout observation is unavailable",
                _ => "terminal Cargo target observation is unavailable",
            }));
        }
        value
            .get("observation")
            .ok_or_else(|| invalid("terminal observation payload is unavailable"))
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TargetState {
        Absent,
        Present,
    }

    fn target_state(value: &Value) -> Result<TargetState, ObservationError> {
        if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
            return Err(invalid("Cargo target observation schema is unsupported"));
        }
        match value
            .get("state")
            .and_then(|state| state.get("state"))
            .and_then(Value::as_str)
        {
            Some("absent") => Ok(TargetState::Absent),
            Some("present") => Ok(TargetState::Present),
            _ => Err(invalid("Cargo target observation state is unsupported")),
        }
    }

    fn target_id(value: &Value) -> Result<&str, ObservationError> {
        if target_state(value)? != TargetState::Present {
            return Err(invalid("Cargo target is not present"));
        }
        value
            .get("state")
            .and_then(|state| state.get("target_id"))
            .and_then(Value::as_str)
            .filter(|value| canonical_sha256(value))
            .ok_or_else(|| invalid("Cargo target identity is invalid"))
    }

    fn validate_comparable(
        cold: &[AcceptedSample],
        warm: &[AcceptedSample],
    ) -> Result<(), ObservationError> {
        let exemplar = &cold[0];
        for sample in cold.iter().chain(warm) {
            if sample.comparison_key != exemplar.comparison_key
                || sample.resource_profile != exemplar.resource_profile
                || sample.cpu_set != exemplar.cpu_set
                || sample.timeout_seconds != exemplar.timeout_seconds
                || sample.runtime != exemplar.runtime
                || sample.resource_accounting != exemplar.resource_accounting
            {
                return Err(invalid("measurement arms are not comparable"));
            }
        }
        Ok(())
    }

    fn validate_resource_contract(
        resource_profile: Option<&str>,
        cpu_set: Option<&str>,
        timeout_seconds: Option<f64>,
        resource_accounting: &str,
    ) -> Result<(), ObservationError> {
        if timeout_seconds.is_some_and(|value| !value.is_finite() || value <= 0.0) {
            return Err(invalid("measurement timeout is invalid"));
        }
        match resource_profile {
            None if cpu_set.is_none() && resource_accounting == "gnu_time_command_tree" => Ok(()),
            Some("big-red-heavy")
                if timeout_seconds.is_some()
                    && cpu_set.is_none_or(canonical_cpu_set_identity)
                    && resource_accounting == "gnu_time_inside_scope" =>
            {
                Ok(())
            }
            _ => Err(invalid("measurement resource contract is unsupported")),
        }
    }

    fn canonical_cpu_set_identity(value: &str) -> bool {
        if value.is_empty() || value.len() > 4096 {
            return false;
        }
        let mut previous: Option<usize> = None;
        for component in value.split(',') {
            let fields = component.split('-').collect::<Vec<_>>();
            let (first, last) = match fields.as_slice() {
                [single] => match canonical_cpu_id(single) {
                    Some(cpu) => (cpu, cpu),
                    None => return false,
                },
                [first, last] => match (canonical_cpu_id(first), canonical_cpu_id(last)) {
                    (Some(first), Some(last)) if first < last => (first, last),
                    _ => return false,
                },
                _ => return false,
            };
            if previous.is_some_and(|previous| first <= previous.saturating_add(1)) {
                return false;
            }
            previous = Some(last);
        }
        true
    }

    fn canonical_cpu_id(value: &str) -> Option<usize> {
        (!value.is_empty()
            && !(value.len() > 1 && value.starts_with('0'))
            && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<usize>().ok())
        .flatten()
    }

    fn validate_runtime(runtime: &RuntimeEvidence) -> Result<(), ObservationError> {
        let valid_id = !runtime.id.is_empty()
            && runtime.id.len() <= 96
            && runtime.id.is_ascii()
            && runtime
                .id
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && runtime
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            && !runtime.id.contains("..");
        let valid_binding = match (
            runtime.descendant_path.as_deref(),
            runtime.runtime_bin_binding_sha256.as_deref(),
        ) {
            (None, None) => true,
            (Some("runtime_bin_first"), Some(digest)) => canonical_sha256(digest),
            _ => false,
        };
        if !valid_id || !canonical_sha256(&runtime.program_sha256) || !valid_binding {
            return Err(invalid("measurement runtime contract is invalid"));
        }
        Ok(())
    }

    fn validate_unique_receipts(
        cold: &[AcceptedSample],
        warm: &[AcceptedSample],
    ) -> Result<(), ObservationError> {
        let mut identities = BTreeSet::new();
        for sample in cold.iter().chain(warm) {
            if !identities.insert(sample.receipt_identity) {
                return Err(invalid(
                    "measurement file is duplicated across the sample set",
                ));
            }
        }
        Ok(())
    }

    fn summarize(samples: &[AcceptedSample]) -> ArmSummary {
        ArmSummary {
            sample_count: samples.len(),
            median_elapsed_seconds: median(samples.iter().map(|sample| sample.elapsed_seconds)),
            median_user_cpu_seconds: median(samples.iter().map(|sample| sample.user_cpu_seconds)),
            median_system_cpu_seconds: median(
                samples.iter().map(|sample| sample.system_cpu_seconds),
            ),
            median_total_cpu_seconds: median(
                samples
                    .iter()
                    .map(|sample| sample.user_cpu_seconds + sample.system_cpu_seconds),
            ),
            median_max_rss_kib: median(samples.iter().map(|sample| sample.max_rss_kib as f64)),
        }
    }

    fn median(values: impl Iterator<Item = f64>) -> f64 {
        let mut values = values.collect::<Vec<_>>();
        values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
        let middle = values.len() / 2;
        if values.len().is_multiple_of(2) {
            (values[middle - 1] + values[middle]) / 2.0
        } else {
            values[middle]
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    struct ReceiptIdentity {
        device: u64,
        inode: u64,
    }

    struct ReadMeasurement {
        identity: ReceiptIdentity,
        receipt: MeasurementReceipt,
    }

    fn read_measurement(path: &Path) -> Result<ReadMeasurement, ObservationError> {
        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| invalid("measurement file is unavailable"))?;
        let before = fstat(&descriptor).map_err(|_| invalid("measurement file is unavailable"))?;
        if !FileType::from_raw_mode(before.st_mode).is_file()
            || before.st_uid != getuid().as_raw()
            || before.st_mode & 0o077 != 0
            || before.st_size < 0
            || u64::try_from(before.st_size)
                .ok()
                .is_none_or(|size| size > MAX_MEASUREMENT_BYTES)
        {
            return Err(invalid("measurement file has an unsafe shape"));
        }
        let file = File::from(descriptor);
        let mut bytes = Vec::new();
        let mut bounded: Take<File> = file.take(MAX_MEASUREMENT_BYTES + 1);
        bounded
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("measurement file is unreadable"))?;
        if bytes.len() as u64 > MAX_MEASUREMENT_BYTES {
            return Err(invalid("measurement file exceeds the size bound"));
        }
        let after =
            fstat(bounded.into_inner()).map_err(|_| invalid("measurement file is unavailable"))?;
        if !same_file_snapshot(&before, &after)
            || u64::try_from(after.st_size).ok() != Some(bytes.len() as u64)
        {
            return Err(invalid("measurement file changed during observation"));
        }
        let receipt =
            serde_json::from_slice(&bytes).map_err(|_| invalid("measurement JSON is invalid"))?;
        Ok(ReadMeasurement {
            identity: ReceiptIdentity {
                device: before.st_dev,
                inode: before.st_ino,
            },
            receipt,
        })
    }

    fn same_file_snapshot(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
        left.st_dev == right.st_dev
            && left.st_ino == right.st_ino
            && left.st_mode == right.st_mode
            && left.st_nlink == right.st_nlink
            && left.st_uid == right.st_uid
            && left.st_gid == right.st_gid
            && left.st_size == right.st_size
            && left.st_mtime == right.st_mtime
            && left.st_mtime_nsec == right.st_mtime_nsec
            && left.st_ctime == right.st_ctime
            && left.st_ctime_nsec == right.st_ctime_nsec
    }

    fn validate_sample_count(arm: &str, count: usize) -> Result<(), ObservationError> {
        if count == 0 || count > MAX_SAMPLES_PER_ARM {
            return Err(invalid(match arm {
                "cold" => "cold sample count must be between 1 and 32",
                _ => "warm sample count must be between 1 and 32",
            }));
        }
        Ok(())
    }

    fn canonical_sha256(value: &str) -> bool {
        value.strip_prefix(SHA256_PREFIX).is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }

    fn finite_nonnegative(value: f64) -> bool {
        value.is_finite() && value >= 0.0
    }

    fn format_optional(value: Option<f64>, suffix: &str) -> String {
        value
            .map(|value| format!("{value:.3}{suffix}"))
            .unwrap_or_else(|| "unknown".to_owned())
    }

    #[derive(Debug)]
    struct ObservationError {
        code: &'static str,
        problem: &'static str,
    }

    fn invalid(problem: &'static str) -> ObservationError {
        unavailable("cargo_target_value_invalid_measurement", problem)
    }

    const fn unavailable(code: &'static str, problem: &'static str) -> ObservationError {
        ObservationError { code, problem }
    }

    fn emit_report(output: OutputFormat, report: CargoTargetValueReport) -> ExitCode {
        match output {
            OutputFormat::Human => print!("{}", report.render_human()),
            OutputFormat::Json => match serde_json::to_string(&report) {
                Ok(encoded) => println!("{encoded}"),
                Err(_) => {
                    eprintln!("Cargo target value observation could not be encoded");
                    return ExitCode::from(2);
                }
            },
        }
        ExitCode::SUCCESS
    }

    fn emit_error(output: OutputFormat, code: &str, problem: &str) -> ExitCode {
        let report = ErrorReport {
            document_type: "glaeda-cargo-target-value-observation-error",
            schema_version: REPORT_SCHEMA_VERSION,
            authority: "performance_observation_only",
            code,
            problem,
            mutation_performed: false,
        };
        match output {
            OutputFormat::Human => {
                eprintln!("Cargo target value observation unavailable: {problem}")
            }
            OutputFormat::Json => match serde_json::to_string(&report) {
                Ok(encoded) => eprintln!("{encoded}"),
                Err(_) => eprintln!("Cargo target value observation error could not be encoded"),
            },
        }
        ExitCode::from(2)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn median_handles_odd_and_even_inputs() {
            assert_eq!(median([3.0, 1.0, 2.0].into_iter()), 2.0);
            assert_eq!(median([4.0, 1.0, 2.0, 3.0].into_iter()), 2.5);
        }

        #[test]
        fn digest_validation_is_canonical() {
            assert!(canonical_sha256(&format!("sha256:{}", "a".repeat(64))));
            assert!(!canonical_sha256(&format!("sha256:{}", "A".repeat(64))));
            assert!(!canonical_sha256(&format!("sha256:{}", "a".repeat(63))));
        }

        #[test]
        fn cpu_set_identity_rejects_extreme_noncanonical_input_without_panicking() {
            assert!(canonical_cpu_set_identity(&usize::MAX.to_string()));
            assert!(!canonical_cpu_set_identity(&format!("{},1", usize::MAX)));
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}
