#![cfg(unix)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::local_install_build_command::LocalInstallBuildCommandContext;
use glaeda::local_install_build_command::toolchain_preflight::{
    LOCAL_INSTALL_TOOLCHAIN_PROBE_TIMEOUT, LocalInstallToolchainBlockingCode,
    LocalInstallToolchainExecutableDisposition, LocalInstallToolchainPreflightReceipt,
    MAX_LOCAL_INSTALL_TOOLCHAIN_VERSION_OUTPUT_BYTES, observe_local_install_toolchain_preflight,
};
use glaeda::local_install_plan::LocalInstallToolchainIdentity;
use glaeda::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempToolchain {
    root: PathBuf,
    bin: PathBuf,
    cargo: PathBuf,
    rustc: PathBuf,
    rustdoc: PathBuf,
}

impl TempToolchain {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let temporary_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize test temporary directory");
        let root = temporary_root.join(format!(
            "glaeda-toolchain-preflight-acceptance-{label}-{}-{sequence}",
            std::process::id()
        ));
        let bin = root.join("toolchain/bin");
        fs::create_dir_all(&bin).expect("create toolchain bin");
        let cargo = bin.join("cargo");
        let rustc = bin.join("rustc");
        let rustdoc = bin.join("rustdoc");
        for path in [&cargo, &rustc, &rustdoc] {
            write_executable(path);
        }
        Self {
            root,
            bin,
            cargo,
            rustc,
            rustdoc,
        }
    }

    fn context(&self) -> LocalInstallBuildCommandContext {
        LocalInstallBuildCommandContext::new(
            self.root.join("source"),
            self.root.join("build"),
            self.cargo.clone(),
            self.rustc.clone(),
            self.rustdoc.clone(),
        )
        .expect("command context")
    }

    fn replace_bin(&self) {
        let old = self.root.join("toolchain/bin.old");
        fs::rename(&self.bin, &old).expect("move old toolchain bin");
        fs::create_dir(&self.bin).expect("create replacement bin");
        for path in [&self.cargo, &self.rustc, &self.rustdoc] {
            write_executable(path);
        }
    }
}

impl Drop for TempToolchain {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_executable(path: &Path) {
    let bytes = if cfg!(target_os = "linux") {
        b"\x7fELFacceptance-reviewed-toolchain".to_vec()
    } else {
        vec![0xfe, 0xed, 0xfa, 0xcf, b'a', b'c', b'c', b'e', b'p', b't']
    };
    fs::write(path, bytes).expect("write executable fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("set executable mode");
}

#[derive(Clone)]
struct Response {
    stdout: String,
    stderr: String,
    status: i32,
}

impl Response {
    fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            status: 0,
        }
    }

    fn success_with_stderr(stdout: impl Into<String>, stderr: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: stderr.into(),
            status: 0,
        }
    }
}

struct ScriptedExecutor<'a> {
    responses: RefCell<VecDeque<Response>>,
    calls: Cell<usize>,
    replace_bin_on_call: Option<(usize, &'a TempToolchain)>,
}

impl<'a> ScriptedExecutor<'a> {
    fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: RefCell::new(responses.into()),
            calls: Cell::new(0),
            replace_bin_on_call: None,
        }
    }

    fn replacing_bin_on_call(mut self, call: usize, fixture: &'a TempToolchain) -> Self {
        self.replace_bin_on_call = Some((call, fixture));
        self
    }
}

impl CommandExecutor for ScriptedExecutor<'_> {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        panic!("toolchain preflight acceptance requires timed execution")
    }
}

impl TimedCommandExecutor for ScriptedExecutor<'_> {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: std::time::Duration,
    ) -> io::Result<ExecutionRecord> {
        assert_eq!(timeout, LOCAL_INSTALL_TOOLCHAIN_PROBE_TIMEOUT);
        let call = self.calls.get();
        if self
            .replace_bin_on_call
            .is_some_and(|(target, _)| target == call)
        {
            self.replace_bin_on_call
                .expect("replacement fixture")
                .1
                .replace_bin();
        }
        self.calls.set(call + 1);
        let response = self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("scripted response");
        Ok(ExecutionRecord {
            argv: spec.displayed_argv(),
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(response.status),
            success: response.status == 0,
            stdout: response.stdout,
            stderr: response.stderr,
        })
    }
}

fn expected() -> LocalInstallToolchainIdentity {
    LocalInstallToolchainIdentity::parse("rust-1.97.1-aarch64-apple-darwin")
        .expect("expected toolchain")
}

fn assert_path_private(receipt: &LocalInstallToolchainPreflightReceipt, fixture: &TempToolchain) {
    let public = serde_json::to_string(receipt).expect("receipt JSON");
    assert!(!public.contains(fixture.root.to_string_lossy().as_ref()));
}

#[test]
fn oversized_stdout_and_nonempty_stderr_are_unknown() {
    let fixture = TempToolchain::new("bounded-output");
    let context = fixture.context();
    let oversized = format!(
        "cargo 1.97.1 {}\n",
        "x".repeat(MAX_LOCAL_INSTALL_TOOLCHAIN_VERSION_OUTPUT_BYTES)
    );
    let executor = ScriptedExecutor::new(vec![
        Response::success(oversized),
        Response::success_with_stderr("rustc 1.97.1 (exact)\n", "unexpected stderr\n"),
        Response::success("rustdoc 1.97.1 (exact)\n"),
    ]);

    let receipt = observe_local_install_toolchain_preflight(&expected(), &context, &executor);

    assert_eq!(
        receipt.cargo(),
        LocalInstallToolchainExecutableDisposition::Unknown
    );
    assert_eq!(
        receipt.rustc(),
        LocalInstallToolchainExecutableDisposition::Unknown
    );
    assert_eq!(
        receipt.rustdoc(),
        LocalInstallToolchainExecutableDisposition::Exact
    );
    assert_eq!(
        receipt.blocking_codes(),
        [
            LocalInstallToolchainBlockingCode::CargoUnknown,
            LocalInstallToolchainBlockingCode::RustcUnknown,
        ]
    );
    assert_path_private(&receipt, &fixture);
}

#[test]
fn parent_directory_replacement_during_cargo_probe_is_changed() {
    let fixture = TempToolchain::new("parent-replacement");
    let context = fixture.context();
    let executor = ScriptedExecutor::new(vec![
        Response::success("cargo 1.97.1 (exact)\n"),
        Response::success("rustc 1.97.1 (exact)\n"),
        Response::success("rustdoc 1.97.1 (exact)\n"),
    ])
    .replacing_bin_on_call(0, &fixture);

    let receipt = observe_local_install_toolchain_preflight(&expected(), &context, &executor);

    assert_eq!(
        receipt.cargo(),
        LocalInstallToolchainExecutableDisposition::Changed
    );
    assert_eq!(
        receipt.rustc(),
        LocalInstallToolchainExecutableDisposition::Exact
    );
    assert_eq!(
        receipt.rustdoc(),
        LocalInstallToolchainExecutableDisposition::Exact
    );
    assert_eq!(
        receipt.blocking_codes(),
        [LocalInstallToolchainBlockingCode::CargoChanged]
    );
    assert_path_private(&receipt, &fixture);
}

#[test]
fn writable_toolchain_parent_is_unsafe_without_executing_a_probe() {
    let fixture = TempToolchain::new("writable-parent");
    fs::set_permissions(&fixture.bin, fs::Permissions::from_mode(0o777))
        .expect("make toolchain parent world writable");
    let context = fixture.context();
    let executor = ScriptedExecutor::new(Vec::new());

    let receipt = observe_local_install_toolchain_preflight(&expected(), &context, &executor);

    assert_eq!(
        receipt.cargo(),
        LocalInstallToolchainExecutableDisposition::Unsafe
    );
    assert_eq!(
        receipt.rustc(),
        LocalInstallToolchainExecutableDisposition::Unsafe
    );
    assert_eq!(
        receipt.rustdoc(),
        LocalInstallToolchainExecutableDisposition::Unsafe
    );
    assert_eq!(
        receipt.blocking_codes(),
        [
            LocalInstallToolchainBlockingCode::CargoUnsafe,
            LocalInstallToolchainBlockingCode::RustcUnsafe,
            LocalInstallToolchainBlockingCode::RustdocUnsafe,
        ]
    );
    assert_eq!(executor.calls.get(), 0);
    assert_path_private(&receipt, &fixture);
}
