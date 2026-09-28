use std::process::ExitCode;

#[cfg(target_os = "linux")]
use clap::{Parser, ValueEnum};
#[cfg(target_os = "linux")]
use glaeda::linux_host_observation::{
    CpuFrequencyObservation, DEFAULT_WATCHED_PORTS, LinuxHostObservation,
    LinuxHostObservationError, MAX_WATCHED_PORTS, ObservedCount, PressureSample,
    SchedExtObservation, SchedExtState, SchedulerFeatureState, observe_linux_host,
};
#[cfg(target_os = "linux")]
use glaeda::process::ProcessExecutor;
#[cfg(target_os = "linux")]
use serde::Serialize;

#[cfg(target_os = "linux")]
#[derive(Debug, Parser)]
#[command(
    name = "glaeda-host-observe",
    about = "Observe bounded Linux machine handoff facts without mutation"
)]
struct Cli {
    /// Select human or JSON output.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,

    /// Watch one TCP port instead of the bounded default development-port set.
    #[arg(long = "port", value_parser = clap::value_parser!(u16).range(1..))]
    ports: Vec<u16>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct LinuxHostObservationErrorReport<'a> {
    document_type: &'static str,
    schema_version: u8,
    authority: &'static str,
    error: &'a LinuxHostObservationError,
}

#[cfg(target_os = "linux")]
impl<'a> LinuxHostObservationErrorReport<'a> {
    fn new(error: &'a LinuxHostObservationError) -> Self {
        Self {
            document_type: "glaeda-linux-host-observation-error",
            schema_version: 1,
            authority: "observation_only",
            error,
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let cli = Cli::parse();
    let ports = if cli.ports.is_empty() {
        DEFAULT_WATCHED_PORTS
    } else {
        &cli.ports
    };
    if ports.len() > MAX_WATCHED_PORTS {
        return emit_error(
            cli.output,
            &LinuxHostObservationError {
                kind: glaeda::linux_host_observation::LinuxHostObservationErrorKind::InvalidRequest,
                code: "invalid_host_observation_request",
                problem: "host observation request is outside the bounded supported shape",
            },
        );
    }
    match observe_linux_host(ports, &ProcessExecutor) {
        Ok(report) => emit_report(cli.output, &report),
        Err(error) => emit_error(cli.output, &error),
    }
}

#[cfg(target_os = "linux")]
fn emit_report(output: OutputFormat, report: &LinuxHostObservation) -> ExitCode {
    match output {
        OutputFormat::Human => print!("{}", render_human(report)),
        OutputFormat::Json => match serde_json::to_string(report) {
            Ok(json) => println!("{json}"),
            Err(_) => {
                eprintln!("Linux host observation could not be encoded");
                return ExitCode::from(2);
            }
        },
    }
    ExitCode::SUCCESS
}

#[cfg(target_os = "linux")]
fn emit_error(output: OutputFormat, error: &LinuxHostObservationError) -> ExitCode {
    let report = LinuxHostObservationErrorReport::new(error);
    match output {
        OutputFormat::Human => eprintln!(
            "Linux host observation unavailable: code={} problem={}",
            error.code, error.problem
        ),
        OutputFormat::Json => match serde_json::to_string(&report) {
            Ok(json) => eprintln!("{json}"),
            Err(_) => eprintln!("Linux host observation error could not be encoded"),
        },
    }
    ExitCode::from(2)
}

#[cfg(target_os = "linux")]
fn render_human(report: &LinuxHostObservation) -> String {
    let cpu = report.cpu();
    let memory = report.memory();
    let pressure = report.pressure();
    let services = report.services();
    let scheduler = report.scheduler();
    let ports = report
        .watched_ports()
        .iter()
        .map(|entry| {
            format!(
                "{}={}",
                entry.port,
                if entry.listening {
                    "listening"
                } else {
                    "absent"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            "Linux host observation: authority=observation_only observed_at={}\n",
            "cpu: logical={} load={} runnable={}/{} pressure={}\n",
            "memory: total={} available={} swap_used={} swap_total={} pressure={}\n",
            "io: pressure={}\n",
            "scheduler: autogroup={} sched_ext={}\n",
            "cpu policy: online={} smt={} nohz_full={} isolated={} frequency={}\n",
            "failed units: system={} user={}\n",
            "watched ports: {}\n"
        ),
        report.observed_at_unix_millis(),
        cpu.logical_cpus,
        format_micros(cpu.load_1m_micros),
        cpu.runnable_entities,
        cpu.total_entities,
        render_pressure(pressure.cpu),
        memory.total_bytes,
        memory.available_bytes,
        memory.swap_used_bytes,
        memory.swap_total_bytes,
        render_pressure(pressure.memory),
        render_pressure(pressure.io),
        render_feature_state(scheduler.autogroup),
        render_sched_ext(&scheduler.sched_ext),
        scheduler.cpu_policy.online_logical_cpus,
        render_feature_state(scheduler.cpu_policy.smt),
        scheduler.cpu_policy.nohz_full_online_logical_cpus,
        scheduler.cpu_policy.isolated_online_logical_cpus,
        render_cpu_frequency(&scheduler.cpu_policy.frequency),
        render_count(services.system),
        render_count(services.user),
        ports,
    )
}

#[cfg(target_os = "linux")]
const fn render_feature_state(state: SchedulerFeatureState) -> &'static str {
    match state {
        SchedulerFeatureState::Unsupported => "unsupported",
        SchedulerFeatureState::Disabled => "disabled",
        SchedulerFeatureState::Enabled => "enabled",
    }
}

#[cfg(target_os = "linux")]
fn render_cpu_frequency(observation: &CpuFrequencyObservation) -> String {
    match observation {
        CpuFrequencyObservation::Unsupported => "unsupported".to_owned(),
        CpuFrequencyObservation::Supported { classes } => classes
            .iter()
            .map(|class| {
                format!(
                    "{}/{}/epp={}/hardware_max={}khz/policy={}-{}khz:{}",
                    class.driver,
                    class.governor,
                    class
                        .energy_performance_preference
                        .as_deref()
                        .unwrap_or("none"),
                    class.hardware_max_frequency_khz,
                    class.policy_min_frequency_khz,
                    class.policy_max_frequency_khz,
                    class.logical_cpus
                )
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}

#[cfg(target_os = "linux")]
fn render_sched_ext(observation: &SchedExtObservation) -> String {
    match observation {
        SchedExtObservation::Unsupported => "unsupported".to_owned(),
        SchedExtObservation::Supported {
            state,
            enable_sequence,
            active_ops,
        } => format!(
            "state={} enable_sequence={} active_ops={}",
            match state {
                SchedExtState::Disabled => "disabled",
                SchedExtState::Enabled => "enabled",
            },
            enable_sequence,
            active_ops.as_deref().unwrap_or("none")
        ),
    }
}

#[cfg(target_os = "linux")]
fn render_pressure(sample: PressureSample) -> String {
    format!(
        "avg10:{} total:{}",
        format_micros(u64::from(sample.avg10_micros)),
        sample.total_micros
    )
}

#[cfg(target_os = "linux")]
fn render_count(count: ObservedCount) -> String {
    match count {
        ObservedCount::Known { count } => count.to_string(),
        ObservedCount::Unavailable => "unavailable".to_owned(),
    }
}

#[cfg(target_os = "linux")]
fn format_micros(value: u64) -> String {
    format!("{}.{:06}", value / 1_000_000, value % 1_000_000)
}

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("glaeda-host-observe is unavailable: Linux is required");
    ExitCode::from(2)
}
