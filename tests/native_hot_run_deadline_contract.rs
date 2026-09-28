#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process};
use rustix::thread::{CpuSet, sched_getaffinity};
use serde_json::Value;

static HEAVY_SCOPE_TEST_LOCK: Mutex<()> = Mutex::new(());

struct RunningFixture {
    directory: std::path::PathBuf,
    leader_file: std::path::PathBuf,
    descendant_file: std::path::PathBuf,
    wrapper: Option<Child>,
}

impl Drop for RunningFixture {
    fn drop(&mut self) {
        if let Ok(raw) = fs::read_to_string(&self.leader_file)
            && let Ok(raw) = raw.trim().parse::<i32>()
            && let Some(pid) = Pid::from_raw(raw)
        {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        if let Ok(raw) = fs::read_to_string(&self.descendant_file)
            && let Ok(raw) = raw.trim().parse::<i32>()
            && let Some(pid) = Pid::from_raw(raw)
        {
            let _ = kill_process(pid, Signal::KILL);
        }
        if let Some(wrapper) = self.wrapper.as_mut() {
            let _ = wrapper.kill();
            let _ = wrapper.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn wait_for_file(path: &std::path::Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if fs::read_to_string(path).is_ok_and(|contents| !contents.is_empty()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "native hot-run did not exit");
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_process_absence(pid: Pid, timeout: Duration, label: &str) {
    let deadline = Instant::now() + timeout;
    loop {
        match test_kill_process(pid) {
            Err(Errno::SRCH) => return,
            Ok(()) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            result => panic!("{label} remained live: {result:?}"),
        }
    }
}

fn heavy_user_scope_is_available() -> bool {
    Command::new("/usr/bin/systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--expand-environment=no",
            "--property",
            "CPUQuota=1200%",
            "--property",
            "MemoryHigh=8G",
            "--property",
            "MemoryMax=12G",
            "--property",
            "TasksMax=1024",
            "/bin/true",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn profile_without_timeout_is_refused_before_scope_creation() {
    let repository = env!("CARGO_MANIFEST_DIR");
    let output = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-heavy",
            "--",
            "/bin/true",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "glaeda-hot-run error: --resource-profile requires --timeout\n"
    );
}

#[test]
fn heavy_profile_applies_exact_cgroup_limits_and_receipt() {
    let _scope_guard = HEAVY_SCOPE_TEST_LOCK.lock().unwrap();
    if !heavy_user_scope_is_available() {
        return;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "glaeda-native-hot-run-profile-limits-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let observation = directory.join("cgroup.txt");
    let measurement = directory.join("measurement.json");
    let shell = format!(
        "group=$(/usr/bin/awk -F: '$1 == \"0\" {{ print $3 }}' /proc/self/cgroup); \
         /usr/bin/printf 'cpu=%s\\nhigh=%s\\nmax=%s\\npids=%s\\nliteral=%s\\n' \
         \"$(/usr/bin/cat /sys/fs/cgroup$group/cpu.max)\" \
         \"$(/usr/bin/cat /sys/fs/cgroup$group/memory.high)\" \
         \"$(/usr/bin/cat /sys/fs/cgroup$group/memory.max)\" \
         \"$(/usr/bin/cat /sys/fs/cgroup$group/pids.max)\" \"$1\" > {}; exit 17",
        observation.display()
    );
    let repository = env!("CARGO_MANIFEST_DIR");
    let status = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-heavy",
            "--measurement",
            measurement.to_str().unwrap(),
            "--timeout",
            "3",
            "--",
            "/bin/sh",
            "-c",
            &shell,
            "glaeda-profile-test",
            "${HOME}",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(17));

    let cgroup = fs::read_to_string(&observation).unwrap();
    let mut lines = cgroup.lines();
    let cpu = lines.next().unwrap().strip_prefix("cpu=").unwrap();
    let (quota, period) = cpu.split_once(' ').unwrap();
    let quota = quota.parse::<u64>().unwrap();
    let period = period.parse::<u64>().unwrap();
    assert_eq!(quota, period * 12);
    assert_eq!(lines.next(), Some("high=8589934592"));
    assert_eq!(lines.next(), Some("max=12884901888"));
    assert_eq!(lines.next(), Some("pids=1024"));
    assert_eq!(lines.next(), Some("literal=${HOME}"));
    assert_eq!(lines.next(), None);

    let report: Value = serde_json::from_reader(fs::File::open(&measurement).unwrap()).unwrap();
    assert_eq!(report["timeout_seconds"], 3.0);
    assert_eq!(report["resource_profile"], "big-red-heavy");
    assert_eq!(report["resource_accounting"], "gnu_time_inside_scope");
    assert_eq!(report["exit_code"], 17);
    assert_eq!(report["signal"], Value::Null);
    assert_eq!(report["completion_reason"], "exited");

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn background_profile_applies_exact_cpu_weight_and_receipt() {
    let _scope_guard = HEAVY_SCOPE_TEST_LOCK.lock().unwrap();
    if !heavy_user_scope_is_available() {
        return;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "glaeda-native-hot-run-background-weight-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let observation = directory.join("cpu-weight.txt");
    let measurement = directory.join("measurement.json");
    let shell = format!(
        "group=$(/usr/bin/awk -F: '$1 == \"0\" {{ print $3 }}' /proc/self/cgroup); \
         /usr/bin/cat /sys/fs/cgroup$group/cpu.weight > {}",
        observation.display()
    );
    let repository = env!("CARGO_MANIFEST_DIR");
    let output = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-background",
            "--measurement",
            measurement.to_str().unwrap(),
            "--timeout",
            "3",
            "--",
            "/bin/sh",
            "-c",
            &shell,
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read_to_string(&observation).unwrap().trim(), "25");

    let report: Value = serde_json::from_reader(fs::File::open(&measurement).unwrap()).unwrap();
    assert_eq!(report["timeout_seconds"], 3.0);
    assert_eq!(report["resource_profile"], "big-red-background");
    assert_eq!(report["resource_accounting"], "gnu_time_inside_scope");
    assert_eq!(report["exit_code"], 0);
    assert_eq!(report["completion_reason"], "exited");

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cpu_grant_survives_launcher_crash_until_the_scoped_worker_exits() {
    let _scope_guard = HEAVY_SCOPE_TEST_LOCK.lock().unwrap();
    if !heavy_user_scope_is_available() {
        return;
    }
    let inherited = sched_getaffinity(None).unwrap();
    let cpu = (0..CpuSet::MAX_CPU)
        .rev()
        .find(|cpu| inherited.is_set(*cpu))
        .expect("at least one effective CPU");
    let cpu = cpu.to_string();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "glaeda-native-hot-run-cpu-grant-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let worker_file = directory.join("worker.txt");
    let python = format!(
        "import os,time; os.closerange(3,1048576); open({:?},'w').write(str(os.getpid())+'\\n'+','.join(map(str,sorted(os.sched_getaffinity(0))))); time.sleep(1)",
        worker_file.to_str().unwrap()
    );
    let repository = env!("CARGO_MANIFEST_DIR");
    let mut wrapper = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-heavy",
            "--cpu-set",
            &cpu,
            "--timeout",
            "5",
            "--",
            "/usr/bin/python3",
            "-c",
            &python,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_file(&worker_file, Duration::from_secs(3));
    let worker = fs::read_to_string(&worker_file).unwrap();
    let mut worker = worker.lines();
    let worker_pid = worker.next().unwrap().parse::<i32>().unwrap();
    assert_eq!(worker.next(), Some(cpu.as_str()));
    assert_eq!(worker.next(), None);

    wrapper.kill().unwrap();
    let status = wrapper.wait().unwrap();
    assert_eq!(status.signal(), Some(signal_hook::consts::signal::SIGKILL));

    let overlap = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-heavy",
            "--cpu-set",
            &cpu,
            "--timeout",
            "3",
            "--",
            "/bin/true",
        ])
        .output()
        .unwrap();
    assert_eq!(overlap.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(overlap.stderr).unwrap(),
        format!("glaeda-hot-run error: CPU {cpu} is already reserved\n")
    );

    wait_for_process_absence(
        Pid::from_raw(worker_pid).unwrap(),
        Duration::from_secs(3),
        "CPU-granted worker after launcher crash",
    );
    let reacquired = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-heavy",
            "--cpu-set",
            &cpu,
            "--timeout",
            "3",
            "--",
            "/bin/true",
        ])
        .status()
        .unwrap();
    assert!(reacquired.success());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cpu_granted_worker_cannot_widen_its_admitted_affinity() {
    let _scope_guard = HEAVY_SCOPE_TEST_LOCK.lock().unwrap();
    if !heavy_user_scope_is_available() {
        return;
    }
    let inherited = sched_getaffinity(None).unwrap();
    let cpus = (0..CpuSet::MAX_CPU)
        .filter(|cpu| inherited.is_set(*cpu))
        .collect::<Vec<_>>();
    if cpus.len() < 2 {
        return;
    }
    let reserved = cpus[cpus.len() - 1];
    let other = cpus[cpus.len() - 2];
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "glaeda-native-hot-run-cpu-seal-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let result_file = directory.join("result.txt");
    #[cfg(target_arch = "x86_64")]
    let python = format!(
        "import ctypes,os\n\
         native_errno='none'\n\
         try: os.sched_setaffinity(0,{{{reserved},{other}}})\n\
         except OSError as error: native_errno=str(error.errno)\n\
         word_bits=ctypes.sizeof(ctypes.c_ulong)*8\n\
         mask_type=ctypes.c_ulong*((max({reserved},{other})//word_bits)+1)\n\
         mask=mask_type()\n\
         for cpu in ({reserved},{other}): mask[cpu//word_bits] |= 1 << (cpu%word_bits)\n\
         libc=ctypes.CDLL(None,use_errno=True)\n\
         ctypes.set_errno(0)\n\
         result=libc.syscall(0x400000cb,0,ctypes.sizeof(mask),ctypes.byref(mask))\n\
         alternate_errno='none' if result == 0 else str(ctypes.get_errno())\n\
         open({:?},'w').write(native_errno+'\\n'+alternate_errno+'\\n'+','.join(map(str,sorted(os.sched_getaffinity(0)))))",
        result_file.to_str().unwrap()
    );
    #[cfg(not(target_arch = "x86_64"))]
    let python = format!(
        "import os; errno='none';\ntry: os.sched_setaffinity(0,{{{reserved},{other}}})\nexcept OSError as error: errno=str(error.errno)\nopen({:?},'w').write(errno+'\\n'+','.join(map(str,sorted(os.sched_getaffinity(0)))))",
        result_file.to_str().unwrap()
    );
    let repository = env!("CARGO_MANIFEST_DIR");
    let status = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-heavy",
            "--cpu-set",
            &reserved.to_string(),
            "--timeout",
            "3",
            "--",
            "/usr/bin/python3",
            "-c",
            &python,
        ])
        .status()
        .unwrap();
    assert!(status.success());
    #[cfg(target_arch = "x86_64")]
    assert_eq!(
        fs::read_to_string(&result_file).unwrap(),
        format!(
            "{}\n{}\n{reserved}",
            rustix::io::Errno::PERM.raw_os_error(),
            rustix::io::Errno::PERM.raw_os_error()
        )
    );
    #[cfg(not(target_arch = "x86_64"))]
    assert_eq!(
        fs::read_to_string(&result_file).unwrap(),
        format!("{}\n{reserved}", rustix::io::Errno::PERM.raw_os_error())
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn sigint_is_forwarded_to_the_owned_process_group_and_receipted() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "glaeda-native-hot-run-sigint-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let leader_file = directory.join("leader.pid");
    let descendant_file = directory.join("descendant.pid");
    let measurement = directory.join("measurement.json");
    let shell = format!(
        "echo $$ > {}; sleep 60 & echo $! > {}; wait",
        leader_file.display(),
        descendant_file.display()
    );
    let repository = env!("CARGO_MANIFEST_DIR");
    let wrapper = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--measurement",
            measurement.to_str().unwrap(),
            "--timeout",
            "30",
            "--",
            "/bin/sh",
            "-c",
            &shell,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut fixture = RunningFixture {
        directory,
        leader_file,
        descendant_file: descendant_file.clone(),
        wrapper: Some(wrapper),
    };
    wait_for_file(&descendant_file, Duration::from_secs(3));

    let wrapper_pid = Pid::from_child(fixture.wrapper.as_ref().unwrap());
    kill_process(wrapper_pid, Signal::INT).unwrap();
    let status = wait_for_exit(fixture.wrapper.as_mut().unwrap(), Duration::from_secs(5));
    assert_eq!(status.code(), Some(130));
    fixture.wrapper = None;

    let descendant_pid = fs::read_to_string(&descendant_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let descendant_pid = Pid::from_raw(descendant_pid).unwrap();
    wait_for_process_absence(
        descendant_pid,
        Duration::from_secs(2),
        "interrupted descendant",
    );

    let report: Value = serde_json::from_reader(fs::File::open(&measurement).unwrap()).unwrap();
    assert_eq!(report["timeout_seconds"], 30.0);
    assert_eq!(report["exit_code"], 130);
    assert_eq!(report["signal"], signal_hook::consts::signal::SIGKILL);
    assert_eq!(report["completion_reason"], "operator_interrupt");
    assert_eq!(
        report["resource_accounting"],
        "unavailable_after_forced_termination"
    );
}

#[test]
fn profiled_deadline_terminates_the_scoped_process_group_and_receipts_it() {
    let _scope_guard = HEAVY_SCOPE_TEST_LOCK.lock().unwrap();
    if !heavy_user_scope_is_available() {
        return;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "glaeda-native-hot-run-profile-deadline-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let group_file = directory.join("group.pid");
    let descendant_file = directory.join("descendant.pid");
    let descendant_group_file = directory.join("descendant-group.pid");
    let measurement = directory.join("measurement.json");
    let shell = format!(
        "/usr/bin/awk '{{ print $5 }}' /proc/self/stat > {}; \
         /usr/bin/setsid /bin/sh -c \
         '/usr/bin/awk \"{{ print \\$5 }}\" /proc/self/stat > {}; \
          echo $$ > {}; exec /bin/sleep 60' & wait",
        group_file.display(),
        descendant_group_file.display(),
        descendant_file.display()
    );
    let repository = env!("CARGO_MANIFEST_DIR");
    let wrapper = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-heavy",
            "--measurement",
            measurement.to_str().unwrap(),
            "--timeout",
            "1",
            "--",
            "/bin/sh",
            "-c",
            &shell,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut fixture = RunningFixture {
        directory,
        leader_file: group_file,
        descendant_file: descendant_file.clone(),
        wrapper: Some(wrapper),
    };
    wait_for_file(&descendant_file, Duration::from_secs(3));
    let leader_group = fs::read_to_string(&fixture.leader_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let descendant_group = fs::read_to_string(&descendant_group_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    assert_ne!(leader_group, descendant_group);

    let status = wait_for_exit(fixture.wrapper.as_mut().unwrap(), Duration::from_secs(5));
    assert_eq!(status.code(), Some(124));
    fixture.wrapper = None;

    let descendant_pid = fs::read_to_string(&descendant_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    wait_for_process_absence(
        Pid::from_raw(descendant_pid).unwrap(),
        Duration::from_secs(2),
        "profiled deadline descendant",
    );

    let report: Value = serde_json::from_reader(fs::File::open(&measurement).unwrap()).unwrap();
    assert_eq!(report["resource_profile"], "big-red-heavy");
    assert_eq!(report["timeout_seconds"], 1.0);
    assert_eq!(report["exit_code"], 124);
    assert_eq!(report["signal"], signal_hook::consts::signal::SIGKILL);
    assert_eq!(report["completion_reason"], "deadline_exceeded");
    assert_eq!(
        report["resource_accounting"],
        "unavailable_after_forced_termination"
    );
}

#[test]
fn profiled_deadline_waits_for_scope_after_fast_leader_exit() {
    let _scope_guard = HEAVY_SCOPE_TEST_LOCK.lock().unwrap();
    if !heavy_user_scope_is_available() {
        return;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "glaeda-native-hot-run-fast-leader-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let leader_file = directory.join("leader.pid");
    let descendant_file = directory.join("descendant.pid");
    let measurement = directory.join("measurement.json");
    let shell = format!(
        "/usr/bin/awk '{{ print $5 }}' /proc/self/stat > {}; \
         /usr/bin/setsid /bin/sh -c \
         'echo $$ > {}; exec /bin/sleep 60' & exit 0",
        leader_file.display(),
        descendant_file.display()
    );
    let repository = env!("CARGO_MANIFEST_DIR");
    let wrapper = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-heavy",
            "--measurement",
            measurement.to_str().unwrap(),
            "--timeout",
            "1",
            "--",
            "/bin/sh",
            "-c",
            &shell,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut fixture = RunningFixture {
        directory,
        leader_file,
        descendant_file: descendant_file.clone(),
        wrapper: Some(wrapper),
    };
    wait_for_file(&descendant_file, Duration::from_secs(3));

    let status = wait_for_exit(fixture.wrapper.as_mut().unwrap(), Duration::from_secs(5));
    assert_eq!(status.code(), Some(124));
    fixture.wrapper = None;

    let descendant_pid = fs::read_to_string(&descendant_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    wait_for_process_absence(
        Pid::from_raw(descendant_pid).unwrap(),
        Duration::from_secs(2),
        "fast-leader descendant",
    );

    let report: Value = serde_json::from_reader(fs::File::open(&measurement).unwrap()).unwrap();
    assert_eq!(report["resource_profile"], "big-red-heavy");
    assert_eq!(report["timeout_seconds"], 1.0);
    assert_eq!(report["exit_code"], 124);
    assert_eq!(report["signal"], signal_hook::consts::signal::SIGKILL);
    assert_eq!(report["completion_reason"], "deadline_exceeded");
}
