use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{self, Pid, Signal};
use serde::Serialize;
use zeroize::Zeroizing;

const REDACTED: &str = "[REDACTED]";
const CAPTURE_BUFFER_BYTES: usize = 8_192;
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(25);
pub const MAX_CAPTURED_STREAM_BYTES: usize = 1_048_576;
/// Maximum exact plain stdin accepted by the one-shot process boundary.
///
/// The 16 KiB ceiling is shared with the reviewed guest-control protocol-v2 request transaction;
/// no current product caller uses a larger or arbitrary input surface.
pub const MAX_CAPTURED_STDIN_BYTES: usize = 16 * 1024;
pub const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub(crate) fn from_zeroizing(value: Zeroizing<String>) -> Self {
        Self(value)
    }

    fn expose(&self) -> &str {
        &self.0
    }

    fn zeroizing_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.0.len());
        bytes.extend_from_slice(self.0.as_bytes());
        Zeroizing::new(bytes)
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED)
    }
}

impl Serialize for SecretString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(REDACTED)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "sensitivity", content = "value", rename_all = "snake_case")]
pub enum CommandValue {
    Plain(String),
    Secret(SecretString),
}

impl CommandValue {
    fn exposed(&self) -> &str {
        match self {
            Self::Plain(value) => value,
            Self::Secret(value) => value.expose(),
        }
    }

    fn displayed(&self) -> String {
        match self {
            Self::Plain(value) => value.clone(),
            Self::Secret(_) => REDACTED.to_owned(),
        }
    }

    fn secret(&self) -> Option<&str> {
        match self {
            Self::Plain(_) => None,
            Self::Secret(value) if value.expose().is_empty() => None,
            Self::Secret(value) => Some(value.expose()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub arguments: Vec<CommandValue>,
    pub environment: BTreeMap<String, CommandValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret_stdin: Option<Box<SecretString>>,
}

impl CommandSpec {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            secret_stdin: None,
        }
    }

    #[must_use]
    pub fn argument(mut self, value: impl Into<String>) -> Self {
        self.arguments.push(CommandValue::Plain(value.into()));
        self
    }

    #[must_use]
    pub fn secret_argument(mut self, value: impl Into<String>) -> Self {
        self.arguments
            .push(CommandValue::Secret(SecretString::new(value)));
        self
    }

    #[must_use]
    pub fn environment(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment
            .insert(key.into(), CommandValue::Plain(value.into()));
        self
    }

    #[must_use]
    pub fn secret_environment(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment
            .insert(key.into(), CommandValue::Secret(SecretString::new(value)));
        self
    }

    /// Supply one secret line on standard input without placing it in argv or the environment.
    ///
    /// The production executor writes the value followed by one newline from a separately
    /// zeroizing buffer, closes the pipe, discards both output streams at the operating-system
    /// boundary, and treats a write failure as command failure. Discarding output prevents a
    /// hostile child from reflecting the secret into ordinary host capture allocations.
    #[must_use]
    pub(crate) fn zeroizing_secret_stdin_line(mut self, value: Zeroizing<String>) -> Self {
        self.secret_stdin = Some(Box::new(SecretString::from_zeroizing(value)));
        self
    }

    #[must_use]
    pub fn displayed_argv(&self) -> Vec<String> {
        std::iter::once(self.program.display().to_string())
            .chain(self.arguments.iter().map(CommandValue::displayed))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionRecord {
    pub argv: Vec<String>,
    pub environment_keys: Vec<String>,
    pub status: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandExecutor {
    /// Execute one explicit program without an implicit shell.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the program path is unsafe, the process cannot be started, output
    /// capture fails, or either output stream exceeds the fixed capture limit.
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord>;
}

pub trait TimedCommandExecutor: CommandExecutor {
    /// Execute one explicit program with a reviewed nonzero wall-clock deadline.
    ///
    /// The production executor starts the child in a fresh process group and sends `SIGKILL` to
    /// that group when the deadline expires. This covers the direct child and ordinary descendants;
    /// it does not claim the stronger cgroup ownership and cleanup evidence deferred to issue #205.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a zero or implementation-exceeding timeout and `TimedOut` after
    /// terminating and reaping a command that exceeds the accepted deadline. Other failures retain
    /// the `CommandExecutor` contract.
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord>;
}

pub trait TimedInputCommandExecutor: TimedCommandExecutor {
    /// Execute one explicit program with a reviewed timeout and one bounded non-secret stdin body.
    ///
    /// Input bytes are written exactly once with no implicit terminator. The executor never inserts
    /// them into argv, environment, or an `ExecutionRecord` field; child-controlled stdout/stderr
    /// remains ordinary captured output and may independently echo those bytes.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for an invalid timeout or input larger than the fixed stdin limit.
    /// Writer setup/write failure, output capture failure, timeout, and output exhaustion retain the
    /// existing process-group termination and reaping contract.
    fn execute_with_input(
        &self,
        spec: &CommandSpec,
        input: &[u8],
        timeout: Duration,
    ) -> io::Result<ExecutionRecord>;
}

pub trait TimedWorkingDirectoryCommandExecutor: TimedCommandExecutor {
    /// Execute one explicit program from one exact normalized working directory.
    ///
    /// This retains the ordinary timeout, process-group termination, bounded output, closed stdin,
    /// and cleared-environment contract. The working directory remains private and never enters the
    /// returned execution record.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` unless the directory is a normalized absolute non-root UTF-8 path,
    /// or when the timeout is invalid. Spawn, capture, output-limit, and timeout failures retain the
    /// ordinary timed-executor contract.
    fn execute_in_directory_with_timeout(
        &self,
        spec: &CommandSpec,
        working_directory: &Path,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessExecutor;

impl CommandExecutor for ProcessExecutor {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        execute_process(spec, None)
    }
}

impl TimedCommandExecutor for ProcessExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        validate_timeout(timeout)?;
        execute_process(spec, Some(timeout))
    }
}

impl TimedInputCommandExecutor for ProcessExecutor {
    fn execute_with_input(
        &self,
        spec: &CommandSpec,
        input: &[u8],
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        validate_timeout(timeout)?;
        validate_plain_stdin(input)?;
        execute_process_with_input_spawner(spec, Some(timeout), Some(input), &ThreadCaptureSpawner)
    }
}

impl TimedWorkingDirectoryCommandExecutor for ProcessExecutor {
    fn execute_in_directory_with_timeout(
        &self,
        spec: &CommandSpec,
        working_directory: &Path,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        validate_timeout(timeout)?;
        validate_working_directory(working_directory)?;
        execute_process_with_working_directory_input_spawner(
            spec,
            Some(timeout),
            None,
            Some(working_directory),
            &ThreadCaptureSpawner,
        )
    }
}

fn validate_timeout(timeout: Duration) -> io::Result<()> {
    if timeout.is_zero() || timeout > MAX_COMMAND_TIMEOUT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command timeout must be within the bounded positive range",
        ));
    }
    Ok(())
}

fn validate_plain_stdin(input: &[u8]) -> io::Result<()> {
    if input.len() > MAX_CAPTURED_STDIN_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command stdin exceeds the fixed bounded input limit",
        ));
    }
    Ok(())
}

fn validate_working_directory(working_directory: &Path) -> io::Result<()> {
    if !working_directory.is_absolute()
        || working_directory == Path::new("/")
        || working_directory.to_str().is_none()
        || !working_directory
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "working directory must be a normalized absolute non-root UTF-8 path",
        ));
    }
    Ok(())
}

fn execute_process(spec: &CommandSpec, timeout: Option<Duration>) -> io::Result<ExecutionRecord> {
    execute_process_with_spawner(spec, timeout, &ThreadCaptureSpawner)
}

fn execute_process_with_spawner<S: CaptureThreadSpawner>(
    spec: &CommandSpec,
    timeout: Option<Duration>,
    spawner: &S,
) -> io::Result<ExecutionRecord> {
    execute_process_with_input_spawner(spec, timeout, None, spawner)
}

fn execute_process_with_input_spawner<S: CaptureThreadSpawner>(
    spec: &CommandSpec,
    timeout: Option<Duration>,
    input: Option<&[u8]>,
    spawner: &S,
) -> io::Result<ExecutionRecord> {
    execute_process_with_working_directory_input_spawner(spec, timeout, input, None, spawner)
}

fn execute_process_with_working_directory_input_spawner<S: CaptureThreadSpawner>(
    spec: &CommandSpec,
    timeout: Option<Duration>,
    input: Option<&[u8]>,
    working_directory: Option<&Path>,
    spawner: &S,
) -> io::Result<ExecutionRecord> {
    ensure_absolute_program(&spec.program)?;
    if let Some(input) = input {
        validate_plain_stdin(input)?;
        if spec.secret_stdin.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "plain and secret standard input cannot be combined",
            ));
        }
        if timeout.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "plain-input commands require a reviewed wall-clock timeout",
            ));
        }
    }
    let deadline = timeout
        .map(|duration| {
            Instant::now().checked_add(duration).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "command timeout exceeds the supported clock range",
                )
            })
        })
        .transpose()?;
    if spec.secret_stdin.is_some() {
        return execute_secret_process_with_discarded_output(
            spec,
            deadline,
            working_directory,
            spawner,
        );
    }

    let mut command = Command::new(&spec.program);
    command
        .env_clear()
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(working_directory) = working_directory {
        command.current_dir(working_directory);
    }
    command.args(spec.arguments.iter().map(CommandValue::exposed));
    for (key, value) in &spec.environment {
        command.env(key, value.exposed());
    }

    let mut child = command.spawn()?;
    let input_pipe = if let Some(input) = input {
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                abort_spawned_child(&mut child)?;
                return Err(io::Error::other(
                    "child stdin was not available after requesting a pipe",
                ));
            }
        };
        Some((stdin, input.to_vec()))
    } else {
        None
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            abort_spawned_child(&mut child)?;
            return Err(io::Error::other(
                "child stdout was not available after requesting a pipe",
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            abort_spawned_child(&mut child)?;
            return Err(io::Error::other(
                "child stderr was not available after requesting a pipe",
            ));
        }
    };

    let (sender, receiver) = mpsc::channel();
    let stdout_reader = match spawner.spawn(stdout, CapturedStream::Stdout, sender.clone()) {
        Ok(reader) => reader,
        Err(error) => {
            cleanup_capture_setup_failure(&mut child, Vec::new())?;
            return Err(error);
        }
    };
    let stderr_reader = match spawner.spawn(stderr, CapturedStream::Stderr, sender.clone()) {
        Ok(reader) => reader,
        Err(error) => {
            cleanup_capture_setup_failure(&mut child, vec![stdout_reader])?;
            return Err(error);
        }
    };
    let mut input_writer = if let Some((stdin, input)) = input_pipe {
        match spawner.spawn_input_writer(stdin, input, sender.clone()) {
            Ok(writer) => Some(writer),
            Err(error) => {
                cleanup_capture_setup_failure(&mut child, vec![stdout_reader, stderr_reader])?;
                return Err(error);
            }
        }
    } else {
        None
    };
    drop(sender);
    let mut stdout_bytes = None;
    let mut stderr_bytes = None;
    let mut input_complete = input_writer.is_none();
    let mut exceeded = BTreeSet::new();
    let mut capture_error = None;
    let mut input_error = None;
    let mut timed_out = false;

    while stdout_bytes.is_none() || stderr_bytes.is_none() || !input_complete {
        let abort_in_progress =
            timed_out || capture_error.is_some() || input_error.is_some() || !exceeded.is_empty();
        let event = if abort_in_progress {
            match receiver.recv() {
                Ok(event) => event,
                Err(_) => {
                    let wait_result = child.wait();
                    let stdout_join_result = join_capture_reader(stdout_reader);
                    let stderr_join_result = join_capture_reader(stderr_reader);
                    let input_join_result = join_optional_input_writer(input_writer.take());
                    wait_result?;
                    stdout_join_result?;
                    stderr_join_result?;
                    input_join_result?;
                    return Err(io::Error::other(
                        "process I/O workers stopped before reporting completion",
                    ));
                }
            }
        } else if let Some(deadline) = deadline {
            let now = Instant::now();
            if now >= deadline {
                timed_out = true;
                terminate_process_group(&mut child)?;
                continue;
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(CAPTURE_POLL_INTERVAL);
            match receiver.recv_timeout(wait) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    let termination_result = terminate_process_group(&mut child);
                    let wait_result = child.wait();
                    let stdout_join_result = join_capture_reader(stdout_reader);
                    let stderr_join_result = join_capture_reader(stderr_reader);
                    let input_join_result = join_optional_input_writer(input_writer.take());
                    termination_result?;
                    wait_result?;
                    stdout_join_result?;
                    stderr_join_result?;
                    input_join_result?;
                    return Err(io::Error::other(
                        "process I/O workers stopped before reporting completion",
                    ));
                }
            }
        } else {
            match receiver.recv() {
                Ok(event) => event,
                Err(_) => {
                    let termination_result = terminate_process_group(&mut child);
                    let wait_result = child.wait();
                    let stdout_join_result = join_capture_reader(stdout_reader);
                    let stderr_join_result = join_capture_reader(stderr_reader);
                    let input_join_result = join_optional_input_writer(input_writer.take());
                    termination_result?;
                    wait_result?;
                    stdout_join_result?;
                    stderr_join_result?;
                    input_join_result?;
                    return Err(io::Error::other(
                        "process I/O workers stopped before reporting completion",
                    ));
                }
            }
        };
        match event {
            CaptureEvent::LimitExceeded(stream) => {
                exceeded.insert(stream);
                terminate_process_group(&mut child)?;
            }
            CaptureEvent::Completed(stream, result) => {
                let bytes = match result {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        if capture_error.is_none() {
                            capture_error = Some(error);
                        }
                        terminate_process_group(&mut child)?;
                        Vec::new()
                    }
                };
                match stream {
                    CapturedStream::Stdout => stdout_bytes = Some(bytes),
                    CapturedStream::Stderr => stderr_bytes = Some(bytes),
                }
            }
            CaptureEvent::InputCompleted(result) => {
                input_complete = true;
                if let Err(error) = result {
                    if input_error.is_none() {
                        input_error = Some(error);
                    }
                    terminate_process_group(&mut child)?;
                }
            }
        }
    }

    let status = if capture_error.is_some() || input_error.is_some() || !exceeded.is_empty() {
        child.wait()?
    } else {
        wait_for_child(&mut child, deadline, &mut timed_out)?
    };
    join_capture_reader(stdout_reader)?;
    join_capture_reader(stderr_reader)?;
    join_optional_input_writer(input_writer.take())?;
    if timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "child exceeded the reviewed wall-clock timeout",
        ));
    }
    if let Some(error) = capture_error {
        return Err(error);
    }
    if !exceeded.is_empty() {
        let streams = exceeded
            .iter()
            .map(|stream| stream.as_str())
            .collect::<Vec<_>>()
            .join(" and ");
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("child {streams} exceeded the {MAX_CAPTURED_STREAM_BYTES}-byte capture limit"),
        ));
    }
    if let Some(error) = input_error {
        return Err(error);
    }
    let stdout_bytes = stdout_bytes.expect("stdout completion recorded");
    let stderr_bytes = stderr_bytes.expect("stderr completion recorded");
    let secrets = spec
        .arguments
        .iter()
        .chain(spec.environment.values())
        .filter_map(CommandValue::secret)
        .collect::<Vec<_>>();

    Ok(ExecutionRecord {
        argv: spec.displayed_argv(),
        environment_keys: spec.environment.keys().cloned().collect(),
        status: status.code(),
        success: status.success(),
        stdout: redact(&String::from_utf8_lossy(&stdout_bytes), &secrets),
        stderr: redact(&String::from_utf8_lossy(&stderr_bytes), &secrets),
    })
}

fn execute_secret_process_with_discarded_output<S: CaptureThreadSpawner>(
    spec: &CommandSpec,
    deadline: Option<Instant>,
    working_directory: Option<&Path>,
    spawner: &S,
) -> io::Result<ExecutionRecord> {
    let deadline = deadline.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "secret-input commands require a reviewed wall-clock timeout",
        )
    })?;
    let secret = spec
        .secret_stdin
        .as_ref()
        .expect("secret-output path requires secret standard input");
    let mut command = Command::new(&spec.program);
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(working_directory) = working_directory {
        command.current_dir(working_directory);
    }
    command.args(spec.arguments.iter().map(CommandValue::exposed));
    for (key, value) in &spec.environment {
        command.env(key, value.exposed());
    }

    let mut child = command.spawn()?;
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            abort_spawned_child(&mut child)?;
            return Err(io::Error::other(
                "child stdin was not available after requesting a pipe",
            ));
        }
    };
    let writer = match spawner.spawn_secret_writer(stdin, secret.zeroizing_bytes()) {
        Ok(writer) => writer,
        Err(error) => {
            abort_spawned_child(&mut child)?;
            return Err(error);
        }
    };
    while !writer.is_finished() {
        let now = Instant::now();
        if now >= deadline {
            let termination_result = terminate_process_group(&mut child);
            let wait_result = child.wait();
            let _writer_result = join_secret_writer(writer);
            termination_result?;
            wait_result?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "child exceeded the reviewed wall-clock timeout",
            ));
        }
        thread::sleep(
            deadline
                .saturating_duration_since(now)
                .min(CAPTURE_POLL_INTERVAL),
        );
    }
    if let Err(error) = join_secret_writer(writer) {
        let termination_result = terminate_process_group(&mut child);
        let wait_result = child.wait();
        termination_result?;
        wait_result?;
        return Err(error);
    }
    let mut timed_out = false;
    let status = match wait_for_child(&mut child, Some(deadline), &mut timed_out) {
        Ok(status) => status,
        Err(error) => {
            let termination_result = terminate_process_group(&mut child);
            let wait_result = child.wait();
            termination_result?;
            wait_result?;
            return Err(error);
        }
    };
    if timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "child exceeded the reviewed wall-clock timeout",
        ));
    }

    Ok(ExecutionRecord {
        argv: spec.displayed_argv(),
        environment_keys: spec.environment.keys().cloned().collect(),
        status: status.code(),
        success: status.success(),
        stdout: String::new(),
        stderr: String::new(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CapturedStream {
    Stdout,
    Stderr,
}

impl CapturedStream {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

enum CaptureEvent {
    LimitExceeded(CapturedStream),
    Completed(CapturedStream, io::Result<Vec<u8>>),
    InputCompleted(io::Result<()>),
}

trait CaptureThreadSpawner {
    fn spawn<R: Read + Send + 'static>(
        &self,
        reader: R,
        stream: CapturedStream,
        sender: Sender<CaptureEvent>,
    ) -> io::Result<JoinHandle<()>>;

    fn spawn_input_writer<W: Write + Send + 'static>(
        &self,
        writer: W,
        input: Vec<u8>,
        sender: Sender<CaptureEvent>,
    ) -> io::Result<JoinHandle<()>>;

    fn spawn_secret_writer<W: Write + Send + 'static>(
        &self,
        writer: W,
        secret: Zeroizing<Vec<u8>>,
    ) -> io::Result<JoinHandle<io::Result<()>>>;
}

struct ThreadCaptureSpawner;

impl CaptureThreadSpawner for ThreadCaptureSpawner {
    fn spawn<R: Read + Send + 'static>(
        &self,
        reader: R,
        stream: CapturedStream,
        sender: Sender<CaptureEvent>,
    ) -> io::Result<JoinHandle<()>> {
        thread::Builder::new().spawn(move || {
            let result = capture_stream(reader, stream, &sender);
            let _ = sender.send(CaptureEvent::Completed(stream, result));
        })
    }

    fn spawn_input_writer<W: Write + Send + 'static>(
        &self,
        mut writer: W,
        input: Vec<u8>,
        sender: Sender<CaptureEvent>,
    ) -> io::Result<JoinHandle<()>> {
        thread::Builder::new().spawn(move || {
            let result = writer.write_all(&input).and_then(|()| writer.flush());
            let _ = sender.send(CaptureEvent::InputCompleted(result));
        })
    }

    fn spawn_secret_writer<W: Write + Send + 'static>(
        &self,
        mut writer: W,
        secret: Zeroizing<Vec<u8>>,
    ) -> io::Result<JoinHandle<io::Result<()>>> {
        thread::Builder::new().spawn(move || {
            writer.write_all(&secret)?;
            writer.write_all(b"\n")?;
            writer.flush()
        })
    }
}

fn capture_stream(
    mut reader: impl Read,
    stream: CapturedStream,
    sender: &Sender<CaptureEvent>,
) -> io::Result<Vec<u8>> {
    let mut captured = Vec::with_capacity(CAPTURE_BUFFER_BYTES);
    let mut buffer = [0_u8; CAPTURE_BUFFER_BYTES];
    let mut limit_reported = false;

    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };

        if limit_reported {
            continue;
        }
        let remaining = MAX_CAPTURED_STREAM_BYTES - captured.len();
        let retained = remaining.min(count);
        captured.extend_from_slice(&buffer[..retained]);
        if retained < count {
            limit_reported = true;
            let _ = sender.send(CaptureEvent::LimitExceeded(stream));
        }
    }

    Ok(captured)
}

fn terminate_process_group(child: &mut Child) -> io::Result<()> {
    let pid = Pid::from_child(child);
    match process::kill_process_group(pid, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(_) => match child.kill() {
            Ok(()) => Ok(()),
            Err(_) if child.try_wait()?.is_some() => Ok(()),
            Err(error) => Err(error),
        },
    }
}

fn abort_spawned_child(child: &mut Child) -> io::Result<()> {
    terminate_process_group(child)?;
    child.wait()?;
    Ok(())
}

fn cleanup_capture_setup_failure(
    child: &mut Child,
    started_readers: Vec<JoinHandle<()>>,
) -> io::Result<()> {
    let termination_result = terminate_process_group(child);
    let wait_result = child.wait();
    let join_result = started_readers
        .into_iter()
        .map(join_capture_reader)
        .collect::<io::Result<Vec<_>>>();
    termination_result?;
    wait_result?;
    join_result?;
    Ok(())
}

fn wait_for_child(
    child: &mut Child,
    deadline: Option<Instant>,
    timed_out: &mut bool,
) -> io::Result<ExitStatus> {
    let Some(deadline) = deadline else {
        return child.wait();
    };
    if *timed_out {
        return child.wait();
    }

    loop {
        let now = Instant::now();
        if now >= deadline {
            *timed_out = true;
            terminate_process_group(child)?;
            return child.wait();
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        thread::sleep(
            deadline
                .saturating_duration_since(now)
                .min(CAPTURE_POLL_INTERVAL),
        );
    }
}

fn join_capture_reader(handle: JoinHandle<()>) -> io::Result<()> {
    handle
        .join()
        .map_err(|_| io::Error::other("output capture worker panicked"))
}

fn join_input_writer(handle: JoinHandle<()>) -> io::Result<()> {
    handle
        .join()
        .map_err(|_| io::Error::other("plain-input worker panicked"))
}

fn join_optional_input_writer(handle: Option<JoinHandle<()>>) -> io::Result<()> {
    match handle {
        Some(handle) => join_input_writer(handle),
        None => Ok(()),
    }
}

fn join_secret_writer(handle: JoinHandle<io::Result<()>>) -> io::Result<()> {
    handle
        .join()
        .map_err(|_| io::Error::other("secret-input worker panicked"))?
}

fn ensure_absolute_program(program: &Path) -> io::Result<()> {
    if program.is_absolute() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "command program must be an absolute path: {}",
                program.display()
            ),
        ))
    }
}

fn redact(value: &str, secrets: &[&str]) -> String {
    secrets.iter().fold(value.to_owned(), |output, secret| {
        output.replace(secret, REDACTED)
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;
    use std::thread;
    use std::thread::JoinHandle;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        CaptureEvent, CaptureThreadSpawner, CapturedStream, CommandExecutor, CommandSpec,
        MAX_CAPTURED_STDIN_BYTES, MAX_CAPTURED_STREAM_BYTES, MAX_COMMAND_TIMEOUT, ProcessExecutor,
        REDACTED, ThreadCaptureSpawner, TimedCommandExecutor, TimedInputCommandExecutor,
        TimedWorkingDirectoryCommandExecutor, Zeroizing, execute_process_with_input_spawner,
        execute_process_with_spawner,
    };

    struct FailingCaptureSpawner {
        fail_on_call: usize,
        calls: AtomicUsize,
    }

    impl FailingCaptureSpawner {
        const fn new(fail_on_call: usize) -> Self {
            Self {
                fail_on_call,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl CaptureThreadSpawner for FailingCaptureSpawner {
        fn spawn<R: Read + Send + 'static>(
            &self,
            reader: R,
            stream: CapturedStream,
            sender: Sender<CaptureEvent>,
        ) -> io::Result<JoinHandle<()>> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if call == self.fail_on_call {
                return Err(io::Error::other("injected capture thread failure"));
            }
            ThreadCaptureSpawner.spawn(reader, stream, sender)
        }

        fn spawn_input_writer<W: Write + Send + 'static>(
            &self,
            writer: W,
            input: Vec<u8>,
            sender: Sender<CaptureEvent>,
        ) -> io::Result<JoinHandle<()>> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if call == self.fail_on_call {
                return Err(io::Error::other("injected plain-input thread failure"));
            }
            ThreadCaptureSpawner.spawn_input_writer(writer, input, sender)
        }

        fn spawn_secret_writer<W: Write + Send + 'static>(
            &self,
            writer: W,
            secret: Zeroizing<Vec<u8>>,
        ) -> io::Result<JoinHandle<io::Result<()>>> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if call == self.fail_on_call {
                return Err(io::Error::other("injected secret-input thread failure"));
            }
            ThreadCaptureSpawner.spawn_secret_writer(writer, secret)
        }
    }

    struct FailingInputWriteSpawner;

    impl CaptureThreadSpawner for FailingInputWriteSpawner {
        fn spawn<R: Read + Send + 'static>(
            &self,
            reader: R,
            stream: CapturedStream,
            sender: Sender<CaptureEvent>,
        ) -> io::Result<JoinHandle<()>> {
            ThreadCaptureSpawner.spawn(reader, stream, sender)
        }

        fn spawn_input_writer<W: Write + Send + 'static>(
            &self,
            _writer: W,
            _input: Vec<u8>,
            sender: Sender<CaptureEvent>,
        ) -> io::Result<JoinHandle<()>> {
            thread::Builder::new().spawn(move || {
                let _ = sender.send(CaptureEvent::InputCompleted(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected plain-input write failure",
                ))));
            })
        }

        fn spawn_secret_writer<W: Write + Send + 'static>(
            &self,
            writer: W,
            secret: Zeroizing<Vec<u8>>,
        ) -> io::Result<JoinHandle<io::Result<()>>> {
            ThreadCaptureSpawner.spawn_secret_writer(writer, secret)
        }
    }

    #[test]
    fn serialization_and_debug_output_redact_secrets() {
        let spec = CommandSpec::new("/usr/bin/example")
            .argument("visible")
            .secret_argument("very-secret")
            .secret_environment("TOKEN", "environment-secret")
            .zeroizing_secret_stdin_line(Zeroizing::new("stdin-secret".to_owned()));

        let debug = format!("{spec:?}");
        let json = serde_json::to_string(&spec).expect("serialize command spec");
        assert!(!debug.contains("very-secret"));
        assert!(!debug.contains("environment-secret"));
        assert!(!debug.contains("stdin-secret"));
        assert!(!json.contains("very-secret"));
        assert!(!json.contains("environment-secret"));
        assert!(!json.contains("stdin-secret"));
        assert!(json.contains(REDACTED));
    }

    #[test]
    fn secret_standard_input_is_written_once_with_output_discarded() -> io::Result<()> {
        let cat = Path::new("/bin/cat");
        if !cat.is_file() {
            return Ok(());
        }
        let secret = "guest-jit-secret";
        let spec =
            CommandSpec::new(cat).zeroizing_secret_stdin_line(Zeroizing::new(secret.to_owned()));
        let record = ProcessExecutor.execute_with_timeout(&spec, Duration::from_secs(1))?;

        assert!(record.success);
        assert!(record.stdout.is_empty());
        assert!(record.stderr.is_empty());
        assert!(!record.argv.join(" ").contains(secret));
        assert!(record.environment_keys.is_empty());
        Ok(())
    }

    #[test]
    fn secret_reflection_to_both_streams_never_enters_the_execution_record() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let secret = "reflected-jit-secret";
        let spec = CommandSpec::new(python)
            .argument("-c")
            .argument(
                "import os,sys; value=sys.stdin.buffer.readline(); os.write(1,value); os.write(2,value)",
            )
            .zeroizing_secret_stdin_line(Zeroizing::new(secret.to_owned()));
        let record = ProcessExecutor.execute_with_timeout(&spec, Duration::from_secs(1))?;

        assert!(record.success);
        assert!(record.stdout.is_empty());
        assert!(record.stderr.is_empty());
        assert!(!format!("{record:?}").contains(secret));
        Ok(())
    }

    #[test]
    fn process_output_is_redacted() -> io::Result<()> {
        let printf = Path::new("/usr/bin/printf");
        if !printf.is_file() {
            return Ok(());
        }

        let spec = CommandSpec::new(printf)
            .argument("%s")
            .secret_argument("top-secret");
        let record = ProcessExecutor.execute(&spec)?;

        assert!(record.success);
        assert_eq!(record.stdout, REDACTED);
        assert!(!record.argv.join(" ").contains("top-secret"));
        Ok(())
    }

    #[test]
    fn stdout_above_the_capture_limit_terminates_the_child() -> io::Result<()> {
        assert_stream_limit("sys.stdout.buffer.write(b'x' * size)", "stdout")
    }

    #[test]
    fn stderr_above_the_capture_limit_terminates_the_child() -> io::Result<()> {
        assert_stream_limit("sys.stderr.buffer.write(b'x' * size)", "stderr")
    }

    #[test]
    fn stdout_and_stderr_are_drained_concurrently() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let chunk = 256 * 1_024;
        let script = format!("import os; os.write(1, b'o' * {chunk}); os.write(2, b'e' * {chunk})");
        let record =
            ProcessExecutor.execute(&CommandSpec::new(python).argument("-c").argument(script))?;

        assert!(record.success);
        assert_eq!(record.stdout.len(), chunk);
        assert_eq!(record.stderr.len(), chunk);
        Ok(())
    }

    #[test]
    fn relative_programs_are_rejected() {
        let error = ProcessExecutor
            .execute(&CommandSpec::new("printf"))
            .expect_err("relative program must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn timed_working_directory_is_exact_private_and_validated_before_spawn() -> io::Result<()> {
        let pwd = Path::new("/usr/bin/pwd");
        if !pwd.is_file() {
            return Ok(());
        }
        let fixture = timeout_fixture_directory()?;
        let record = ProcessExecutor.execute_in_directory_with_timeout(
            &CommandSpec::new(pwd),
            &fixture,
            Duration::from_secs(1),
        )?;
        assert!(record.success);
        assert_eq!(record.stdout.trim_end(), fixture.to_string_lossy());
        assert!(!format!("{record:?}").contains("working_directory"));

        for invalid in [
            Path::new("relative"),
            Path::new("/"),
            Path::new("/tmp/../tmp"),
        ] {
            let error = ProcessExecutor
                .execute_in_directory_with_timeout(
                    &CommandSpec::new("/absolute/program/that/must/not/exist"),
                    invalid,
                    Duration::from_secs(1),
                )
                .expect_err("invalid cwd must fail before spawn");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
        fs::remove_dir(fixture)?;
        Ok(())
    }

    #[test]
    fn timed_execution_accepts_bounded_deadlines_and_completed_commands() -> io::Result<()> {
        let printf = Path::new("/usr/bin/printf");
        if !printf.is_file() {
            return Ok(());
        }

        let record = ProcessExecutor.execute_with_timeout(
            &CommandSpec::new(printf).argument("complete"),
            Duration::from_secs(1),
        )?;

        assert!(record.success);
        assert_eq!(record.stdout, "complete");
        Ok(())
    }

    #[test]
    fn timed_execution_rejects_invalid_deadlines_before_spawn() {
        let missing = CommandSpec::new("/absolute/program/that/must/not/exist");

        for timeout in [Duration::ZERO, MAX_COMMAND_TIMEOUT + Duration::from_secs(1)] {
            let error = ProcessExecutor
                .execute_with_timeout(&missing, timeout)
                .expect_err("invalid timeout must fail before spawn");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn timeout_terminates_the_child_process_group() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }

        let fixture = timeout_fixture_directory()?;
        let marker = fixture.join("descendant-survived");
        let descendant = "import pathlib,sys,time; time.sleep(0.6); pathlib.Path(sys.argv[1]).write_text('survived')";
        let parent = "import subprocess,sys,time; subprocess.Popen([sys.executable, '-c', sys.argv[1], sys.argv[2]]); time.sleep(10)";
        let error = ProcessExecutor
            .execute_with_timeout(
                &CommandSpec::new(python)
                    .argument("-c")
                    .argument(parent)
                    .argument(descendant)
                    .argument(marker.to_string_lossy()),
                Duration::from_millis(100),
            )
            .expect_err("long-running process group must time out");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        thread::sleep(Duration::from_millis(800));
        assert!(
            !marker.exists(),
            "ordinary descendant survived group timeout"
        );
        fs::remove_dir(&fixture)?;
        Ok(())
    }

    #[test]
    fn timeout_still_applies_after_the_child_closes_both_output_pipes() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }

        let error = ProcessExecutor
            .execute_with_timeout(
                &CommandSpec::new(python)
                    .argument("-c")
                    .argument("import os,time; os.close(1); os.close(2); time.sleep(10)"),
                Duration::from_millis(100),
            )
            .expect_err("closed output pipes must not bypass the command deadline");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        Ok(())
    }

    #[test]
    fn secret_writer_deadline_survives_direct_child_exit_with_inherited_stdin() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }

        let fixture = timeout_fixture_directory()?;
        let marker = fixture.join("secret-stdin-descendant-survived");
        let script = "import os,pathlib,sys,time; pid=os.fork(); pid and os._exit(0); time.sleep(0.6); pathlib.Path(sys.argv[1]).write_text('survived'); time.sleep(10)";
        let error = ProcessExecutor
            .execute_with_timeout(
                &CommandSpec::new(python)
                    .argument("-c")
                    .argument(script)
                    .argument(marker.to_string_lossy())
                    .zeroizing_secret_stdin_line(Zeroizing::new("s".repeat(65_536))),
                Duration::from_millis(100),
            )
            .expect_err("an inherited unread secret pipe must not outlive the deadline");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        thread::sleep(Duration::from_millis(800));
        assert!(
            !marker.exists(),
            "secret-input descendant survived group timeout"
        );
        fs::remove_dir(&fixture)?;
        Ok(())
    }

    #[test]
    fn timed_execution_preserves_output_limit_precedence() -> io::Result<()> {
        let yes = Path::new("/usr/bin/yes");
        if !yes.is_file() {
            return Ok(());
        }

        let error = ProcessExecutor
            .execute_with_timeout(
                &CommandSpec::new(yes).argument("x"),
                Duration::from_secs(10),
            )
            .expect_err("output exhaustion must abort before the later deadline");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("stdout"));
        Ok(())
    }

    #[test]
    fn capture_thread_setup_failure_terminates_and_reaps_the_spawned_group() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }

        for fail_on_call in [1, 2] {
            let fixture = timeout_fixture_directory()?;
            let marker = fixture.join("process-survived-capture-setup-failure");
            let script = "import pathlib,sys,time; time.sleep(0.6); pathlib.Path(sys.argv[1]).write_text('survived'); time.sleep(10)";
            let spec = CommandSpec::new(python)
                .argument("-c")
                .argument(script)
                .argument(marker.to_string_lossy());
            let error = execute_process_with_spawner(
                &spec,
                Some(Duration::from_secs(2)),
                &FailingCaptureSpawner::new(fail_on_call),
            )
            .expect_err("capture thread setup failure must abort the spawned process group");

            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(error.to_string(), "injected capture thread failure");
            thread::sleep(Duration::from_millis(800));
            assert!(
                !marker.exists(),
                "spawned process survived capture thread setup failure"
            );
            fs::remove_dir(&fixture)?;
        }

        let fixture = timeout_fixture_directory()?;
        let marker = fixture.join("process-survived-secret-writer-setup-failure");
        let script = "import pathlib,sys,time; time.sleep(0.6); pathlib.Path(sys.argv[1]).write_text('survived'); time.sleep(10)";
        let spec = CommandSpec::new(python)
            .argument("-c")
            .argument(script)
            .argument(marker.to_string_lossy())
            .zeroizing_secret_stdin_line(Zeroizing::new("secret".to_owned()));
        let error = execute_process_with_spawner(
            &spec,
            Some(Duration::from_secs(2)),
            &FailingCaptureSpawner::new(1),
        )
        .expect_err("secret-input setup failure must abort the spawned process group");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "injected secret-input thread failure");
        thread::sleep(Duration::from_millis(800));
        assert!(
            !marker.exists(),
            "spawned process survived writer setup failure"
        );
        fs::remove_dir(&fixture)?;
        Ok(())
    }

    #[test]
    fn plain_standard_input_is_exact_and_output_is_captured() -> io::Result<()> {
        let cat = Path::new("/bin/cat");
        if !cat.is_file() {
            return Ok(());
        }
        let sentinel = b"guest-request-without-implicit-newline";
        let record = ProcessExecutor.execute_with_input(
            &CommandSpec::new(cat),
            sentinel,
            Duration::from_secs(1),
        )?;
        assert!(record.success);
        assert_eq!(record.stdout.as_bytes(), sentinel);
        assert!(record.stderr.is_empty());
        assert!(
            !record
                .argv
                .join(" ")
                .contains("guest-request-without-implicit-newline")
        );
        assert!(record.environment_keys.is_empty());
        Ok(())
    }

    #[test]
    fn plain_standard_input_accepts_empty_and_maximum_documents() -> io::Result<()> {
        let cat = Path::new("/bin/cat");
        if !cat.is_file() {
            return Ok(());
        }
        for input in [Vec::new(), vec![b'x'; MAX_CAPTURED_STDIN_BYTES]] {
            let record = ProcessExecutor.execute_with_input(
                &CommandSpec::new(cat),
                &input,
                Duration::from_secs(1),
            )?;
            assert!(record.success);
            assert_eq!(record.stdout.as_bytes(), input.as_slice());
        }
        Ok(())
    }

    #[test]
    fn plain_standard_input_is_not_added_to_record_surfaces() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let sentinel = b"request-body-sentinel";
        let record = ProcessExecutor.execute_with_input(
            &CommandSpec::new(python)
                .argument("-c")
                .argument("import sys; sys.stdin.buffer.read(); sys.stdout.write('ok')"),
            sentinel,
            Duration::from_secs(1),
        )?;
        assert!(record.success);
        assert_eq!(record.stdout, "ok");
        let debug = format!("{record:?}");
        let json = serde_json::to_string(&record).unwrap();
        assert!(!debug.contains("request-body-sentinel"));
        assert!(!json.contains("request-body-sentinel"));
        Ok(())
    }

    #[test]
    fn oversized_plain_standard_input_fails_before_spawn() {
        let missing = CommandSpec::new("/absolute/program/that/must/not/exist");
        let input = vec![b'x'; MAX_CAPTURED_STDIN_BYTES + 1];
        let error = ProcessExecutor
            .execute_with_input(&missing, &input, Duration::from_secs(1))
            .expect_err("oversized stdin must fail before spawn");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn unread_plain_standard_input_cannot_bypass_timeout() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let error = ProcessExecutor
            .execute_with_input(
                &CommandSpec::new(python)
                    .argument("-c")
                    .argument("import time; time.sleep(10)"),
                &vec![b'x'; MAX_CAPTURED_STDIN_BYTES],
                Duration::from_millis(100),
            )
            .expect_err("unread stdin must remain inside the reviewed deadline");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        Ok(())
    }

    #[test]
    fn plain_input_writer_setup_failure_aborts_spawned_group() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let fixture = timeout_fixture_directory()?;
        let marker = fixture.join("process-survived-input-writer-setup-failure");
        let script = "import pathlib,sys,time; time.sleep(0.6); pathlib.Path(sys.argv[1]).write_text('survived'); time.sleep(10)";
        let spec = CommandSpec::new(python)
            .argument("-c")
            .argument(script)
            .argument(marker.to_string_lossy());
        let error = execute_process_with_input_spawner(
            &spec,
            Some(Duration::from_secs(2)),
            Some(b"input"),
            &FailingCaptureSpawner::new(3),
        )
        .expect_err("input writer setup failure must abort the spawned process group");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "injected plain-input thread failure");
        thread::sleep(Duration::from_millis(800));
        assert!(
            !marker.exists(),
            "spawned process survived writer setup failure"
        );
        fs::remove_dir(&fixture)?;
        Ok(())
    }

    #[test]
    fn plain_input_write_failure_aborts_spawned_group() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let fixture = timeout_fixture_directory()?;
        let marker = fixture.join("process-survived-input-write-failure");
        let script = "import pathlib,sys,time; time.sleep(0.6); pathlib.Path(sys.argv[1]).write_text('survived'); time.sleep(10)";
        let spec = CommandSpec::new(python)
            .argument("-c")
            .argument(script)
            .argument(marker.to_string_lossy());
        let error = execute_process_with_input_spawner(
            &spec,
            Some(Duration::from_secs(2)),
            Some(b"input"),
            &FailingInputWriteSpawner,
        )
        .expect_err("input write failure must abort the spawned process group");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), "injected plain-input write failure");
        thread::sleep(Duration::from_millis(800));
        assert!(
            !marker.exists(),
            "spawned process survived input write failure"
        );
        fs::remove_dir(&fixture)?;
        Ok(())
    }

    #[test]
    fn output_exhaustion_precedes_plain_input_side_effects() -> io::Result<()> {
        let yes = Path::new("/usr/bin/yes");
        if !yes.is_file() {
            return Ok(());
        }
        let error = ProcessExecutor
            .execute_with_input(
                &CommandSpec::new(yes).argument("x"),
                b"input",
                Duration::from_secs(10),
            )
            .expect_err("output exhaustion must remain the terminal classification");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("stdout"));
        Ok(())
    }

    fn timeout_fixture_directory() -> io::Result<PathBuf> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "smolrunner-process-timeout-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(path)
    }

    fn assert_stream_limit(script_body: &str, stream: &str) -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let script = format!(
            "import sys; size = {}; {script_body}; sys.stdout.flush(); sys.stderr.flush()",
            MAX_CAPTURED_STREAM_BYTES + 1
        );
        let error = ProcessExecutor
            .execute(&CommandSpec::new(python).argument("-c").argument(script))
            .expect_err("capture limit must fail closed");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(stream));
        assert!(
            error
                .to_string()
                .contains(&MAX_CAPTURED_STREAM_BYTES.to_string())
        );
        Ok(())
    }
}
