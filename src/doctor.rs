use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{Check, CheckStatus, DoctorReport};

#[must_use]
pub fn inspect_host() -> DoctorReport {
    DoctorReport::from_checks(vec![
        check_operating_system(),
        check_architecture(),
        check_systemd(),
        check_cgroup_v2(),
        check_command("podman", false),
        check_command("git", true),
    ])
}

#[must_use]
pub fn render_human(report: &DoctorReport) -> String {
    let mut output = String::new();
    output.push_str("Glaeda doctor\n\n");

    for check in &report.checks {
        let marker = match check.status {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        };

        output.push_str(&format!("[{marker}] {}: {}\n", check.id, check.summary));
        if let Some(detail) = &check.detail {
            output.push_str(&format!("       {detail}\n"));
        }
    }

    output.push_str(&format!("\nOverall: {:?}\n", report.overall));
    output
}

fn check_operating_system() -> Check {
    if cfg!(target_os = "linux") {
        Check::new(
            "operating-system",
            CheckStatus::Pass,
            linux_pretty_name().unwrap_or_else(|| "Linux".to_owned()),
            None,
        )
    } else {
        Check::new(
            "operating-system",
            CheckStatus::Fail,
            format!("unsupported host operating system: {}", env::consts::OS),
            Some("The first Glaeda host target is Linux.".to_owned()),
        )
    }
}

fn check_architecture() -> Check {
    let supported = matches!(env::consts::ARCH, "x86_64" | "aarch64");
    Check::new(
        "architecture",
        if supported {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        env::consts::ARCH,
        (!supported).then(|| "Initial releases are tested on x86_64 and aarch64.".to_owned()),
    )
}

fn check_systemd() -> Check {
    let systemctl = find_command("systemctl");
    let booted_with_systemd = Path::new("/run/systemd/system").is_dir();

    match (systemctl, booted_with_systemd) {
        (Some(path), true) => Check::new(
            "systemd",
            CheckStatus::Pass,
            "systemd is available",
            Some(format!("systemctl={}", path.display())),
        ),
        (Some(path), false) => Check::new(
            "systemd",
            CheckStatus::Warn,
            "systemctl exists, but this environment is not booted with systemd",
            Some(format!("systemctl={}", path.display())),
        ),
        (None, _) => Check::new(
            "systemd",
            CheckStatus::Warn,
            "systemctl was not found",
            Some("Runner service installation will not be available.".to_owned()),
        ),
    }
}

fn check_cgroup_v2() -> Check {
    let controllers = Path::new("/sys/fs/cgroup/cgroup.controllers");
    if controllers.is_file() {
        Check::new(
            "cgroup-v2",
            CheckStatus::Pass,
            "cgroup v2 is available",
            fs::read_to_string(controllers)
                .ok()
                .map(|value| format!("controllers={}", value.trim())),
        )
    } else {
        Check::new(
            "cgroup-v2",
            CheckStatus::Warn,
            "cgroup v2 was not detected",
            Some("CPU, memory, and PID delegation cannot be safely configured yet.".to_owned()),
        )
    }
}

fn check_command(name: &str, required: bool) -> Check {
    match find_command(name) {
        Some(path) => Check::new(
            format!("command-{name}"),
            CheckStatus::Pass,
            format!("{name} is available"),
            Some(path.display().to_string()),
        ),
        None => Check::new(
            format!("command-{name}"),
            if required {
                CheckStatus::Fail
            } else {
                CheckStatus::Warn
            },
            format!("{name} was not found in PATH"),
            None,
        ),
    }
}

fn find_command(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn linux_pretty_name() -> Option<String> {
    let contents = fs::read_to_string("/etc/os-release").ok()?;
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key == "PRETTY_NAME").then(|| trim_quotes(value).to_owned())
    })
}

fn trim_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use crate::CheckStatus;

    use super::{inspect_host, trim_quotes};

    #[test]
    fn trims_os_release_quotes() {
        assert_eq!(trim_quotes("\"Debian GNU/Linux\""), "Debian GNU/Linux");
        assert_eq!(trim_quotes("Debian"), "Debian");
    }

    #[test]
    fn doctor_always_returns_checks() {
        let report = inspect_host();
        assert!(!report.checks.is_empty());
        assert!(matches!(
            report.overall,
            CheckStatus::Pass | CheckStatus::Warn | CheckStatus::Fail
        ));
    }
}
