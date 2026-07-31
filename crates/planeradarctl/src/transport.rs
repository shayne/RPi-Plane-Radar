use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ffi::OsString,
    fmt,
    io::Write,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::fs;

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::target::{SshHost, SshTarget, TargetIdentity};

const HOST_KEY_ALGORITHMS: &str = "ssh-ed25519,ecdsa-sha2-nistp256,rsa-sha2-512,rsa-sha2-256";
const MAX_HOST_KEY_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_PROBE_OUTPUT_BYTES: usize = 256;
const MAX_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_RECONNECT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_CAPTURED_STREAM_BYTES: usize = 128 * 1024;
const MAX_RECONNECT_CANDIDATES: usize = 8;
const TRUSTED_HOST_KEY_ALGORITHM_ORDER: [&str; 3] =
    ["ssh-ed25519", "ecdsa-sha2-nistp256", "ssh-rsa"];
const TRANSIENT_SSH_FAILURE_MARKERS: [&[u8]; 9] = [
    b"connection refused",
    b"connection timed out",
    b"operation timed out",
    b"connection reset by peer",
    b"connection closed by remote host",
    b"connection closed by ",
    b"no route to host",
    b"network is unreachable",
    b"could not resolve hostname",
];

#[derive(Clone, Copy)]
struct ProbeDeadline {
    deadline: Duration,
    configured_timeout: Duration,
    timeout_error: TransportError,
}

/// Injectable elapsed-time boundary for reboot polling. Test implementations
/// advance deterministic time without waiting.
pub trait Clock {
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

/// Production monotonic clock. It is only selected by `with_runner`; tests use
/// `with_runner_and_clock` with a fake clock instead.
pub struct SystemClock {
    started_at: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.started_at.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Bounded timing and discovery policy for an expected reboot.
#[derive(Clone, Eq, PartialEq)]
pub struct ReconnectPolicy {
    disconnect_timeout: Duration,
    reconnect_timeout: Duration,
    initial_backoff: Duration,
    max_backoff: Duration,
    connect_timeout: Duration,
    desired_local_hostname: Option<String>,
    identity_preverified: bool,
}

impl fmt::Debug for ReconnectPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReconnectPolicy")
            .field("disconnect_timeout", &self.disconnect_timeout)
            .field("reconnect_timeout", &self.reconnect_timeout)
            .field("initial_backoff", &self.initial_backoff)
            .field("max_backoff", &self.max_backoff)
            .field("connect_timeout", &self.connect_timeout)
            .field("desired_local_hostname", &"<redacted>")
            .field("identity_preverified", &self.identity_preverified)
            .finish()
    }
}

impl ReconnectPolicy {
    pub fn new(
        disconnect_timeout: Duration,
        reconnect_timeout: Duration,
        initial_backoff: Duration,
        max_backoff: Duration,
        connect_timeout: Duration,
    ) -> Result<Self, TransportError> {
        if disconnect_timeout.is_zero()
            || reconnect_timeout.is_zero()
            || initial_backoff.is_zero()
            || max_backoff.is_zero()
            || connect_timeout.is_zero()
            || disconnect_timeout > MAX_DISCONNECT_TIMEOUT
            || reconnect_timeout > MAX_RECONNECT_TIMEOUT
            || initial_backoff > max_backoff
            || max_backoff > MAX_BACKOFF
            || connect_timeout > MAX_CONNECT_TIMEOUT
            || connect_timeout < Duration::from_secs(1)
        {
            return Err(TransportError::InvalidReconnectPolicy);
        }
        Ok(Self {
            disconnect_timeout,
            reconnect_timeout,
            initial_backoff,
            max_backoff,
            connect_timeout,
            desired_local_hostname: None,
            identity_preverified: false,
        })
    }

    pub fn with_desired_local_hostname(
        mut self,
        hostname: impl Into<String>,
    ) -> Result<Self, TransportError> {
        let hostname = hostname.into();
        if !hostname.ends_with(".local")
            || SshTarget::from_str(&format!("probe@{hostname}")).is_err()
        {
            return Err(TransportError::InvalidReconnectPolicy);
        }
        self.desired_local_hostname = Some(hostname);
        Ok(self)
    }

    pub fn after_identity_verified(mut self) -> Self {
        self.identity_preverified = true;
        self
    }
}

/// One typed program invocation. The arguments are passed directly to the
/// program; no local shell is involved.
#[derive(Clone, Eq, PartialEq)]
pub struct Invocation {
    program: String,
    arguments: Vec<String>,
    os_arguments: Vec<OsString>,
    timeout: Option<Duration>,
    stdout_limit: Option<usize>,
}

impl Invocation {
    pub fn new(program: impl Into<String>, arguments: Vec<String>) -> Self {
        let os_arguments = arguments.iter().map(OsString::from).collect();
        Self {
            program: program.into(),
            arguments,
            os_arguments,
            timeout: None,
            stdout_limit: None,
        }
    }

    pub fn new_os(program: impl Into<String>, os_arguments: Vec<OsString>) -> Self {
        let arguments = os_arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        Self {
            program: program.into(),
            arguments,
            os_arguments,
            timeout: None,
            stdout_limit: None,
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    /// OS-native arguments used by the production runner. Local copy paths are
    /// retained here without Unicode conversion.
    pub fn os_arguments(&self) -> &[OsString] {
        &self.os_arguments
    }

    /// Adds a wall-clock execution limit for the production runner. The
    /// timeout is separate from OpenSSH's connection timeout so a remote
    /// command cannot consume more than a reboot phase's remaining budget.
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    pub(crate) fn with_stdout_limit(mut self, limit: usize) -> Self {
        self.stdout_limit = Some(limit);
        self
    }

    pub fn stdout_limit(&self) -> Option<usize> {
        self.stdout_limit
    }
}

impl fmt::Debug for Invocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Invocation")
            .field("program", &self.program)
            .field("argument_count", &self.arguments.len())
            .field("timeout", &self.timeout)
            .field("stdout_limit", &self.stdout_limit)
            .finish()
    }
}

/// Captured program output. Accessors deliberately require an explicit caller
/// choice so formatting a transport result cannot leak remote output.
#[derive(Clone, Eq, PartialEq)]
pub struct CommandOutput {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CommandOutput {
    pub fn success(stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            status: 0,
            stdout,
            stderr,
        }
    }

    pub fn new(status: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            status,
            stdout,
            stderr,
        }
    }

    pub fn status(&self) -> i32 {
        self.status
    }

    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    fn succeeded(&self) -> bool {
        self.status == 0
    }
}

impl fmt::Debug for CommandOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandOutput")
            .field("status", &self.status)
            .field(
                "stdout",
                &format_args!("<redacted {} bytes>", self.stdout.len()),
            )
            .field(
                "stderr",
                &format_args!("<redacted {} bytes>", self.stderr.len()),
            )
            .finish()
    }
}

/// The external execution boundary, which tests replace with a deterministic
/// runner. Implementations receive a complete program/argument vector.
pub trait CommandRunner {
    fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError>;
}

/// Production runner. It calls the named OpenSSH program directly through
/// `std::process::Command`; it never invokes a local command shell.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError> {
        let timeout = invocation.timeout;
        let stdout_limit = invocation.stdout_limit.unwrap_or(MAX_CAPTURED_STREAM_BYTES);
        let started_at = Instant::now();
        let mut child = Command::new(&invocation.program)
            .args(invocation.os_arguments())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| RunnerError::Failed)?;
        let stdout = child.stdout.take().ok_or(RunnerError::Failed)?;
        let stderr = child.stderr.take().ok_or(RunnerError::Failed)?;
        let (overflow_sender, overflow_receiver) = mpsc::channel();
        let stdout_reader = thread::spawn({
            let overflow_sender = overflow_sender.clone();
            move || read_limited_stream(stdout, stdout_limit, overflow_sender)
        });
        let stderr_reader = thread::spawn(move || {
            read_limited_stream(stderr, MAX_CAPTURED_STREAM_BYTES, overflow_sender)
        });

        let mut failure = None;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    let wait = match timeout {
                        Some(limit) => match limit.checked_sub(started_at.elapsed()) {
                            Some(remaining) if !remaining.is_zero() => {
                                remaining.min(Duration::from_millis(5))
                            }
                            _ => {
                                failure = Some(RunnerError::TimedOut);
                                let _ = child.kill();
                                break child.wait().map_err(|_| RunnerError::Failed)?;
                            }
                        },
                        None => Duration::from_millis(5),
                    };
                    match overflow_receiver.recv_timeout(wait) {
                        Ok(()) => {
                            failure = Some(RunnerError::Failed);
                            let _ = child.kill();
                            break child.wait().map_err(|_| RunnerError::Failed)?;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            // Both streams can close before the child exits.
                            // Keep polling at the same bounded interval rather
                            // than turning that normal condition into a CPU spin.
                            thread::sleep(wait);
                            continue;
                        }
                    }
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunnerError::Failed);
                }
            }
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| RunnerError::Failed)?
            .map_err(|_| RunnerError::Failed)?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| RunnerError::Failed)?
            .map_err(|_| RunnerError::Failed)?;
        if stdout.exceeded || stderr.exceeded {
            return Err(RunnerError::Failed);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(CommandOutput::new(
            status.code().unwrap_or(-1),
            stdout.bytes,
            stderr.bytes,
        ))
    }
}

struct BoundedCapture {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn read_limited_stream<R: std::io::Read>(
    mut reader: R,
    limit: usize,
    overflow_sender: mpsc::Sender<()>,
) -> std::io::Result<BoundedCapture> {
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut exceeded = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        if retained < count && !exceeded {
            exceeded = true;
            let _ = overflow_sender.send(());
        }
    }
    Ok(BoundedCapture { bytes, exceeded })
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum RunnerError {
    #[error("the OpenSSH program execution failed")]
    Failed,
    #[error("the OpenSSH program execution timed out")]
    TimedOut,
}

/// Explicit connection policy for noninteractive OpenSSH operations.
#[derive(Clone, Eq, PartialEq)]
pub struct TransportConfig {
    trusted_known_hosts: PathBuf,
    connect_timeout_seconds: u16,
}

impl fmt::Debug for TransportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportConfig")
            .field("trusted_known_hosts", &"<redacted>")
            .field("connect_timeout_seconds", &self.connect_timeout_seconds)
            .finish()
    }
}

impl TransportConfig {
    pub fn new(trusted_known_hosts: PathBuf) -> Result<Self, TransportError> {
        if !trusted_known_hosts.is_absolute()
            || trusted_known_hosts.as_os_str().is_empty()
            || trusted_known_hosts
                .to_string_lossy()
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(TransportError::InvalidConfiguration);
        }
        Ok(Self {
            trusted_known_hosts,
            connect_timeout_seconds: 5,
        })
    }
}

/// A typed remote command. Arguments are later quoted for the remote shell as
/// required by OpenSSH; they are never joined into a local shell command.
#[derive(Clone, Eq, PartialEq)]
pub struct RemoteCommand {
    arguments: Vec<String>,
    interactive_sudo: bool,
}

impl fmt::Debug for RemoteCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteCommand")
            .field("argument_count", &self.arguments.len())
            .field("interactive_sudo", &self.interactive_sudo)
            .finish()
    }
}

impl RemoteCommand {
    pub fn ordinary<I, S>(arguments: I) -> Result<Self, TransportError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::new(arguments, false)
    }

    /// Creates the only command form permitted to allocate an SSH TTY.
    pub fn interactive_sudo<I, S>(arguments: I) -> Result<Self, TransportError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let command = Self::new(arguments, true)?;
        if command
            .arguments
            .first()
            .is_none_or(|argument| argument != "sudo")
        {
            return Err(TransportError::InteractiveSudoMustStartWithSudo);
        }
        Ok(command)
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub fn is_interactive_sudo(&self) -> bool {
        self.interactive_sudo
    }

    fn new<I, S>(arguments: I, interactive_sudo: bool) -> Result<Self, TransportError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        if arguments.is_empty()
            || arguments.iter().any(|argument| {
                argument
                    .bytes()
                    .any(|byte| matches!(byte, b'\0' | b'\r' | b'\n'))
            })
        {
            return Err(TransportError::InvalidRemoteCommand);
        }
        Ok(Self {
            arguments,
            interactive_sudo,
        })
    }
}

pub type Output = CommandOutput;

/// A validated target identity collected through a strict OpenSSH connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetProbe {
    pub identity: TargetIdentity,
}

/// The subset of the transport interface completed in the first TDD slice.
pub trait Transport {
    fn probe(&self, target: &SshTarget) -> Result<TargetProbe, TransportError>;
    fn probe_identity_bound(
        &self,
        target: &SshTarget,
        expected: &TargetIdentity,
    ) -> Result<SshTarget, TransportError> {
        let probe = self.probe(target)?;
        if expected.matches(&probe.identity) {
            Ok(target.clone())
        } else {
            Err(TransportError::TargetIdentityMismatch)
        }
    }
    fn run(&self, target: &SshTarget, request: RemoteCommand) -> Result<Output, TransportError>;
    fn run_bounded(
        &self,
        target: &SshTarget,
        request: RemoteCommand,
        _timeout: Duration,
        _stdout_limit: usize,
    ) -> Result<Output, TransportError> {
        self.run(target, request)
    }
    fn copy_to(
        &self,
        target: &SshTarget,
        local: &Path,
        remote: &Path,
    ) -> Result<(), TransportError>;
    fn copy_from(
        &self,
        target: &SshTarget,
        remote: &Path,
        local: &Path,
    ) -> Result<(), TransportError>;
    fn wait_for_reboot(
        &self,
        identity: &TargetIdentity,
        addresses: &[SshTarget],
        policy: ReconnectPolicy,
    ) -> Result<SshTarget, TransportError>;
}

/// OpenSSH transport parameterized over its process runner for deterministic
/// tests and a production `std::process::Command` adapter in the final slice.
pub struct OpenSshTransport<R, C = SystemClock> {
    runner: R,
    config: TransportConfig,
    clock: C,
    ephemeral_host_keys: RefCell<HashMap<SshTarget, HostKey>>,
}

impl<R> OpenSshTransport<R, SystemClock> {
    pub fn with_runner(runner: R, config: TransportConfig) -> Self {
        Self {
            runner,
            config,
            clock: SystemClock::default(),
            ephemeral_host_keys: RefCell::new(HashMap::new()),
        }
    }
}

impl OpenSshTransport<SystemCommandRunner, SystemClock> {
    pub fn system(config: TransportConfig) -> Self {
        Self::with_runner(SystemCommandRunner, config)
    }
}

impl<R, C> OpenSshTransport<R, C> {
    pub fn with_runner_and_clock(runner: R, config: TransportConfig, clock: C) -> Self {
        Self {
            runner,
            config,
            clock,
            ephemeral_host_keys: RefCell::new(HashMap::new()),
        }
    }
}

impl<R: CommandRunner, C: Clock> Transport for OpenSshTransport<R, C> {
    fn probe(&self, target: &SshTarget) -> Result<TargetProbe, TransportError> {
        self.probe_with_timeout(
            target,
            Duration::from_secs(self.config.connect_timeout_seconds.into()),
        )
    }

    fn probe_identity_bound(
        &self,
        target: &SshTarget,
        expected: &TargetIdentity,
    ) -> Result<SshTarget, TransportError> {
        self.probe_identity_bound_with_timeout(
            target,
            expected,
            Duration::from_secs(self.config.connect_timeout_seconds.into()),
        )
    }

    fn run(&self, target: &SshTarget, request: RemoteCommand) -> Result<Output, TransportError> {
        let (mut arguments, _ephemeral_trust) = self.noninteractive_arguments_for_target(target)?;
        if request.interactive_sudo {
            arguments.insert(0, "-tt".into());
        }
        arguments.extend(target.ssh_arguments());
        arguments.extend(
            request
                .arguments
                .iter()
                .enumerate()
                .map(|(index, argument)| quote_remote_argument(argument, index == 0)),
        );
        let output = self.runner.run(Invocation::new("ssh", arguments))?;
        if output.succeeded() {
            Ok(output)
        } else {
            Err(TransportError::CommandFailed)
        }
    }

    fn run_bounded(
        &self,
        target: &SshTarget,
        request: RemoteCommand,
        timeout: Duration,
        stdout_limit: usize,
    ) -> Result<Output, TransportError> {
        if timeout.is_zero() || stdout_limit == 0 {
            return Err(TransportError::CommandFailed);
        }
        let (mut arguments, _ephemeral_trust) = self.noninteractive_arguments_for_target(target)?;
        if request.interactive_sudo {
            arguments.insert(0, "-tt".into());
        }
        arguments.extend(target.ssh_arguments());
        arguments.extend(
            request
                .arguments
                .iter()
                .enumerate()
                .map(|(index, argument)| quote_remote_argument(argument, index == 0)),
        );
        let output = self.runner.run(
            Invocation::new("ssh", arguments)
                .with_timeout(timeout)
                .with_stdout_limit(stdout_limit),
        )?;
        if output.succeeded() {
            Ok(output)
        } else {
            Err(TransportError::CommandFailed)
        }
    }

    fn copy_to(
        &self,
        target: &SshTarget,
        local: &Path,
        remote: &Path,
    ) -> Result<(), TransportError> {
        let remote = self.remote_copy_destination(target, remote)?;
        self.copy(target, vec![local_copy_argument(local)?, remote.into()])
    }

    fn copy_from(
        &self,
        target: &SshTarget,
        remote: &Path,
        local: &Path,
    ) -> Result<(), TransportError> {
        let remote = self.remote_copy_destination(target, remote)?;
        self.copy(target, vec![remote.into(), local_copy_argument(local)?])
    }

    fn wait_for_reboot(
        &self,
        identity: &TargetIdentity,
        addresses: &[SshTarget],
        policy: ReconnectPolicy,
    ) -> Result<SshTarget, TransportError> {
        let candidates = reconnection_candidates(addresses, &policy)?;
        let original = candidates
            .first()
            .expect("candidate validation always retains the original")
            .clone();
        let disconnect_deadline = self.clock.now() + policy.disconnect_timeout;
        if !policy.identity_preverified {
            let (_, initial_probe) = self.probe_reconnect_original_until_deadline(
                &original,
                identity,
                disconnect_deadline,
                policy.connect_timeout,
                TransportError::NeverDisconnected,
            )?;
            self.expect_identity(identity, initial_probe)?;
        }
        let mut backoff = policy.initial_backoff;
        loop {
            let probe = self.probe_reconnect_original_until_deadline(
                &original,
                identity,
                disconnect_deadline,
                policy.connect_timeout,
                TransportError::NeverDisconnected,
            );
            self.require_before_deadline(disconnect_deadline, TransportError::NeverDisconnected)?;
            match probe {
                Ok((_, probe)) => self.expect_identity(identity, probe)?,
                Err(TransportError::ConnectionUnavailable) => break,
                Err(error) => return Err(error),
            }
            if !self.sleep_before(disconnect_deadline, &mut backoff, &policy) {
                return Err(TransportError::NeverDisconnected);
            }
        }

        let reconnect_deadline = self.clock.now() + policy.reconnect_timeout;
        backoff = policy.initial_backoff;
        loop {
            for candidate in &candidates {
                let probe = if candidate == &original {
                    self.probe_reconnect_original_until_deadline(
                        candidate,
                        identity,
                        reconnect_deadline,
                        policy.connect_timeout,
                        TransportError::ReconnectTimedOut,
                    )
                } else {
                    self.probe_alternate_until_deadline(
                        candidate,
                        identity,
                        reconnect_deadline,
                        policy.connect_timeout,
                        TransportError::ReconnectTimedOut,
                    )
                };
                self.require_before_deadline(
                    reconnect_deadline,
                    TransportError::ReconnectTimedOut,
                )?;
                match probe {
                    Ok((reconnected, probe)) => {
                        self.expect_identity(identity, probe)?;
                        return Ok(reconnected);
                    }
                    Err(TransportError::ConnectionUnavailable) => continue,
                    Err(error) => return Err(error),
                }
            }
            if !self.sleep_before(reconnect_deadline, &mut backoff, &policy) {
                return Err(TransportError::ReconnectTimedOut);
            }
        }
    }
}

fn quote_remote_argument(argument: &str, command_position: bool) -> String {
    if !command_position_requires_quotes(argument, command_position)
        && !argument.is_empty()
        && argument.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'_' | b'@' | b'%' | b'+' | b'=' | b',' | b':' | b'.' | b'/' | b'-'
                )
        })
    {
        argument.into()
    } else {
        format!("'{}'", argument.replace('\'', "'\"'\"'"))
    }
}

fn command_position_requires_quotes(argument: &str, command_position: bool) -> bool {
    command_position
        && (argument.contains('=')
            || matches!(
                argument,
                "!" | "case"
                    | "do"
                    | "done"
                    | "elif"
                    | "else"
                    | "esac"
                    | "fi"
                    | "for"
                    | "function"
                    | "if"
                    | "in"
                    | "coproc"
                    | "select"
                    | "then"
                    | "time"
                    | "until"
                    | "while"
            ))
}

fn validate_remote_copy_path(path: &Path) -> Result<&str, TransportError> {
    let value = path.to_str().ok_or(TransportError::InvalidRemoteCopyPath)?;
    if !value.starts_with('/')
        || value.len() == 1
        || value.ends_with('/')
        || value.contains("//")
        || value.split('/').skip(1).any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || !component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        })
    {
        return Err(TransportError::InvalidRemoteCopyPath);
    }
    Ok(value)
}

fn local_copy_argument(path: &Path) -> Result<OsString, TransportError> {
    if path.as_os_str().is_empty() {
        return Err(TransportError::InvalidLocalCopyPath);
    }
    if path.is_absolute() {
        Ok(path.as_os_str().to_os_string())
    } else {
        Ok(PathBuf::from("./").join(path).into_os_string())
    }
}

impl<R, C> OpenSshTransport<R, C> {
    fn noninteractive_arguments(&self) -> Vec<String> {
        self.noninteractive_arguments_for(
            &self.config.trusted_known_hosts,
            self.config.connect_timeout_seconds.into(),
            HOST_KEY_ALGORITHMS,
        )
    }

    fn noninteractive_arguments_for(
        &self,
        known_hosts: &Path,
        connect_timeout_seconds: u64,
        host_key_algorithms: &str,
    ) -> Vec<String> {
        vec![
            "-F".into(),
            "/dev/null".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            "-o".into(),
            "GlobalKnownHostsFile=/dev/null".into(),
            "-o".into(),
            "UpdateHostKeys=no".into(),
            "-o".into(),
            format!("ConnectTimeout={connect_timeout_seconds}"),
            "-o".into(),
            format!("HostKeyAlgorithms={host_key_algorithms}"),
            "-o".into(),
            format!("UserKnownHostsFile={}", known_hosts.display()),
        ]
    }

    fn noninteractive_arguments_for_target(
        &self,
        target: &SshTarget,
    ) -> Result<(Vec<String>, Option<NamedTempFile>), TransportError> {
        let Some(host_key) = self.ephemeral_host_keys.borrow().get(target).cloned() else {
            return Ok((self.noninteractive_arguments(), None));
        };
        let known_hosts = exact_known_hosts_file(target, &host_key)?;
        let arguments = self.noninteractive_arguments_for(
            known_hosts.path(),
            self.config.connect_timeout_seconds.into(),
            host_key.ssh_host_key_algorithms(),
        );
        Ok((arguments, Some(known_hosts)))
    }

    fn copy(&self, target: &SshTarget, paths: Vec<OsString>) -> Result<(), TransportError>
    where
        R: CommandRunner,
    {
        let (base_arguments, _ephemeral_trust) =
            self.noninteractive_arguments_for_target(target)?;
        let mut arguments = base_arguments
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        arguments.push("--".into());
        arguments.extend(paths);
        let output = self.runner.run(Invocation::new_os("scp", arguments))?;
        if output.succeeded() {
            Ok(())
        } else {
            Err(TransportError::CommandFailed)
        }
    }

    fn remote_copy_destination(
        &self,
        target: &SshTarget,
        remote: &Path,
    ) -> Result<String, TransportError> {
        let remote = validate_remote_copy_path(remote)?;
        Ok(format!("{}:{remote}", target.ssh_destination()))
    }

    fn trusted_host_key(&self, target: &SshTarget) -> Result<HostKey, TransportError>
    where
        R: CommandRunner,
    {
        let output = self.runner.run(Invocation::new(
            "ssh-keygen",
            vec![
                "-F".into(),
                target.host().as_str().into_owned(),
                "-f".into(),
                self.config
                    .trusted_known_hosts
                    .to_string_lossy()
                    .into_owned(),
            ],
        ))?;
        if !output.succeeded() {
            return Err(TransportError::TrustedHostKeyUnavailable);
        }
        exactly_one_trusted_host_key(output.stdout())
    }

    fn trusted_host_key_until_deadline(
        &self,
        target: &SshTarget,
        deadline: Duration,
        configured_timeout: Duration,
        timeout_error: TransportError,
    ) -> Result<HostKey, TransportError>
    where
        R: CommandRunner,
        C: Clock,
    {
        let timeout = self.attempt_timeout(deadline, configured_timeout, timeout_error)?;
        let output = self.run_until_deadline(
            Invocation::new(
                "ssh-keygen",
                vec![
                    "-F".into(),
                    target.host().as_str().into_owned(),
                    "-f".into(),
                    self.config
                        .trusted_known_hosts
                        .to_string_lossy()
                        .into_owned(),
                ],
            ),
            timeout,
            deadline,
            timeout_error,
        )?;
        if !output.succeeded() {
            return Err(TransportError::TrustedHostKeyUnavailable);
        }
        exactly_one_trusted_host_key(output.stdout())
    }

    fn probe_with_timeout(
        &self,
        target: &SshTarget,
        connect_timeout: Duration,
    ) -> Result<TargetProbe, TransportError>
    where
        R: CommandRunner,
    {
        if let Some(host_key) = self.ephemeral_host_keys.borrow().get(target).cloned() {
            let known_hosts = exact_known_hosts_file(target, &host_key)?;
            return self.probe_with_known_hosts(
                target,
                host_key,
                known_hosts.path(),
                connect_timeout,
            );
        }
        let host_key = self.trusted_host_key(target)?;
        self.probe_with_known_hosts(
            target,
            host_key,
            &self.config.trusted_known_hosts,
            connect_timeout,
        )
    }

    fn probe_identity_bound_with_timeout(
        &self,
        target: &SshTarget,
        expected: &TargetIdentity,
        connect_timeout: Duration,
    ) -> Result<SshTarget, TransportError>
    where
        R: CommandRunner,
        C: Clock,
    {
        let deadline = self.clock.now() + connect_timeout.saturating_mul(6);
        let (reconnected, probe) = self.probe_alternate_until_deadline(
            target,
            expected,
            deadline,
            connect_timeout,
            TransportError::ConnectionUnavailable,
        )?;
        if expected.matches(&probe.identity) {
            Ok(reconnected)
        } else {
            Err(TransportError::TargetIdentityMismatch)
        }
    }

    fn probe_until_deadline(
        &self,
        target: &SshTarget,
        deadline: Duration,
        configured_timeout: Duration,
        timeout_error: TransportError,
    ) -> Result<TargetProbe, TransportError>
    where
        R: CommandRunner,
        C: Clock,
    {
        if let Some(host_key) = self.ephemeral_host_keys.borrow().get(target).cloned() {
            let known_hosts = exact_known_hosts_file(target, &host_key)?;
            return self.probe_with_known_hosts_until_deadline(
                target,
                host_key,
                known_hosts.path(),
                deadline,
                configured_timeout,
                timeout_error,
            );
        }
        let host_key = self.trusted_host_key_until_deadline(
            target,
            deadline,
            configured_timeout,
            timeout_error,
        )?;
        self.probe_with_known_hosts_until_deadline(
            target,
            host_key,
            &self.config.trusted_known_hosts,
            deadline,
            configured_timeout,
            timeout_error,
        )
    }

    fn fixed_probe_with_known_hosts(
        &self,
        target: &SshTarget,
        script: &str,
        known_hosts: &Path,
        connect_timeout: Duration,
        host_key: &HostKey,
    ) -> Result<String, TransportError>
    where
        R: CommandRunner,
    {
        let mut arguments = self.noninteractive_arguments_for(
            known_hosts,
            connect_timeout.as_secs(),
            host_key.ssh_host_key_algorithms(),
        );
        arguments.extend(target.ssh_arguments());
        arguments.push(script.into());
        let output = self.runner.run(Invocation::new("ssh", arguments))?;
        if !output.succeeded() {
            return Err(classify_probe_failure(&output));
        }
        parse_probe_text(output.stdout())
    }

    fn fixed_probe_with_known_hosts_until_deadline(
        &self,
        target: &SshTarget,
        script: &str,
        known_hosts: &Path,
        host_key: &HostKey,
        timing: ProbeDeadline,
    ) -> Result<String, TransportError>
    where
        R: CommandRunner,
        C: Clock,
    {
        let timeout = self.attempt_timeout(
            timing.deadline,
            timing.configured_timeout,
            timing.timeout_error,
        )?;
        let mut arguments = self.noninteractive_arguments_for(
            known_hosts,
            timeout.as_secs(),
            host_key.ssh_host_key_algorithms(),
        );
        arguments.extend(target.ssh_arguments());
        arguments.push(script.into());
        let output = self.run_until_deadline(
            Invocation::new("ssh", arguments),
            timeout,
            timing.deadline,
            timing.timeout_error,
        )?;
        if !output.succeeded() {
            return Err(classify_probe_failure(&output));
        }
        parse_probe_text(output.stdout())
    }

    fn probe_alternate_until_deadline(
        &self,
        target: &SshTarget,
        expected: &TargetIdentity,
        deadline: Duration,
        configured_timeout: Duration,
        timeout_error: TransportError,
    ) -> Result<(SshTarget, TargetProbe), TransportError>
    where
        R: CommandRunner,
        C: Clock,
    {
        let (host_key, reconnected) = self.scan_expected_host_key_until_deadline(
            target,
            expected,
            deadline,
            configured_timeout,
            timeout_error,
        )?;
        let mut known_hosts =
            NamedTempFile::new().map_err(|_| TransportError::EphemeralTrustFailed)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            known_hosts
                .as_file_mut()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| TransportError::EphemeralTrustFailed)?;
        }
        known_hosts
            .write_all(host_key.known_hosts_line(&reconnected).as_bytes())
            .and_then(|()| known_hosts.flush())
            .map_err(|_| TransportError::EphemeralTrustFailed)?;
        self.require_before_deadline(deadline, timeout_error)?;
        let probe = self.probe_with_known_hosts_until_deadline(
            &reconnected,
            host_key.clone(),
            known_hosts.path(),
            deadline,
            configured_timeout,
            timeout_error,
        )?;
        if !expected.matches(&probe.identity) {
            return Err(TransportError::TargetIdentityMismatch);
        }
        let mut ephemeral_host_keys = self.ephemeral_host_keys.borrow_mut();
        ephemeral_host_keys.insert(target.clone(), host_key.clone());
        ephemeral_host_keys.insert(reconnected.clone(), host_key);
        Ok((target.clone(), probe))
    }

    fn probe_reconnect_original_until_deadline(
        &self,
        target: &SshTarget,
        expected: &TargetIdentity,
        deadline: Duration,
        configured_timeout: Duration,
        timeout_error: TransportError,
    ) -> Result<(SshTarget, TargetProbe), TransportError>
    where
        R: CommandRunner,
        C: Clock,
    {
        match self.probe_until_deadline(target, deadline, configured_timeout, timeout_error) {
            Ok(probe) => Ok((target.clone(), probe)),
            Err(TransportError::TrustedHostKeyUnavailable) => self.probe_alternate_until_deadline(
                target,
                expected,
                deadline,
                configured_timeout,
                timeout_error,
            ),
            Err(error) => Err(error),
        }
    }

    fn scan_expected_host_key_until_deadline(
        &self,
        target: &SshTarget,
        expected: &TargetIdentity,
        deadline: Duration,
        configured_timeout: Duration,
        timeout_error: TransportError,
    ) -> Result<(HostKey, SshTarget), TransportError>
    where
        R: CommandRunner,
        C: Clock,
    {
        let timeout = self.attempt_timeout(deadline, configured_timeout, timeout_error)?;
        let scan = self.run_until_deadline(
            keyscan_invocation(target.host().as_str().into_owned(), timeout),
            timeout,
            deadline,
            timeout_error,
        )?;
        if scan.succeeded() {
            return select_scanned_host_key(scan.stdout(), &expected.host_key_sha256)
                .map(|host_key| (host_key, target.clone()));
        }
        if matches!(target.host(), SshHost::Ipv4(_)) {
            return Err(TransportError::ConnectionUnavailable);
        }

        let timeout = self.attempt_timeout(deadline, configured_timeout, timeout_error)?;
        let resolved = self.run_until_deadline(
            Invocation::new(
                "dscacheutil",
                vec![
                    "-q".into(),
                    "host".into(),
                    "-a".into(),
                    "name".into(),
                    target.host().as_str().into_owned(),
                ],
            ),
            timeout,
            deadline,
            timeout_error,
        )?;
        if !resolved.succeeded() {
            return Err(TransportError::ConnectionUnavailable);
        }

        let mut reached_host = false;
        for address in resolved_host_addresses(resolved.stdout())? {
            let timeout = self.attempt_timeout(deadline, configured_timeout, timeout_error)?;
            let scan = self.run_until_deadline(
                keyscan_invocation(address.to_string(), timeout),
                timeout,
                deadline,
                timeout_error,
            )?;
            if !scan.succeeded() {
                continue;
            }
            reached_host = true;
            match select_scanned_host_key(scan.stdout(), &expected.host_key_sha256) {
                Ok(host_key) => {
                    let reconnected = format!("{}@{address}", target.username().as_str())
                        .parse()
                        .map_err(|_| TransportError::EphemeralTrustFailed)?;
                    return Ok((host_key, reconnected));
                }
                Err(TransportError::HostKeyMismatch) => {}
                Err(error) => return Err(error),
            }
        }
        if reached_host {
            Err(TransportError::HostKeyMismatch)
        } else {
            Err(TransportError::ConnectionUnavailable)
        }
    }

    fn probe_with_known_hosts(
        &self,
        target: &SshTarget,
        host_key: HostKey,
        known_hosts: &Path,
        connect_timeout: Duration,
    ) -> Result<TargetProbe, TransportError>
    where
        R: CommandRunner,
    {
        let model = self.fixed_probe_with_known_hosts(
            target,
            "tr -d '\\0' < /proc/device-tree/model",
            known_hosts,
            connect_timeout,
            &host_key,
        )?;
        let serial = self.fixed_probe_with_known_hosts(
            target,
            "awk -F ': ' '/^Serial/{print $2; exit}' /proc/cpuinfo",
            known_hosts,
            connect_timeout,
            &host_key,
        )?;
        validate_probe_identity(&host_key.fingerprint, &model, &serial)?;
        Ok(TargetProbe {
            identity: TargetIdentity {
                host_key_sha256: host_key.fingerprint,
                model,
                serial,
            },
        })
    }

    fn probe_with_known_hosts_until_deadline(
        &self,
        target: &SshTarget,
        host_key: HostKey,
        known_hosts: &Path,
        deadline: Duration,
        configured_timeout: Duration,
        timeout_error: TransportError,
    ) -> Result<TargetProbe, TransportError>
    where
        R: CommandRunner,
        C: Clock,
    {
        let model = self.fixed_probe_with_known_hosts_until_deadline(
            target,
            "tr -d '\\0' < /proc/device-tree/model",
            known_hosts,
            &host_key,
            ProbeDeadline {
                deadline,
                configured_timeout,
                timeout_error,
            },
        )?;
        let serial = self.fixed_probe_with_known_hosts_until_deadline(
            target,
            "awk -F ': ' '/^Serial/{print $2; exit}' /proc/cpuinfo",
            known_hosts,
            &host_key,
            ProbeDeadline {
                deadline,
                configured_timeout,
                timeout_error,
            },
        )?;
        validate_probe_identity(&host_key.fingerprint, &model, &serial)?;
        Ok(TargetProbe {
            identity: TargetIdentity {
                host_key_sha256: host_key.fingerprint,
                model,
                serial,
            },
        })
    }

    fn expect_identity(
        &self,
        expected: &TargetIdentity,
        probe: TargetProbe,
    ) -> Result<(), TransportError> {
        if expected.matches(&probe.identity) {
            Ok(())
        } else {
            Err(TransportError::TargetIdentityMismatch)
        }
    }

    /// Executes one externally spawned command under the current reboot phase
    /// budget. Every caller first recomputes its own timeout, so an aggregate
    /// probe cannot grant the full configured timeout to each of its child
    /// commands. The production runner enforces this wall-clock limit; fake
    /// runners make the same bound directly inspectable in tests.
    fn run_until_deadline(
        &self,
        invocation: Invocation,
        timeout: Duration,
        deadline: Duration,
        timeout_error: TransportError,
    ) -> Result<CommandOutput, TransportError>
    where
        R: CommandRunner,
        C: Clock,
    {
        match self.runner.run(invocation.with_timeout(timeout)) {
            Ok(output) => {
                self.require_before_deadline(deadline, timeout_error)?;
                Ok(output)
            }
            Err(RunnerError::TimedOut) => {
                self.require_before_deadline(deadline, timeout_error)?;
                Err(TransportError::ConnectionUnavailable)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn sleep_before(
        &self,
        deadline: Duration,
        backoff: &mut Duration,
        policy: &ReconnectPolicy,
    ) -> bool
    where
        C: Clock,
    {
        let now = self.clock.now();
        if now >= deadline {
            return false;
        }
        self.clock
            .sleep((*backoff).min(deadline.saturating_sub(now)));
        *backoff = backoff.saturating_mul(2).min(policy.max_backoff);
        true
    }

    fn attempt_timeout(
        &self,
        deadline: Duration,
        configured_timeout: Duration,
        timeout_error: TransportError,
    ) -> Result<Duration, TransportError>
    where
        C: Clock,
    {
        bounded_attempt_timeout(
            deadline.saturating_sub(self.clock.now()),
            configured_timeout,
        )
        .ok_or(timeout_error)
    }

    fn require_before_deadline(
        &self,
        deadline: Duration,
        timeout_error: TransportError,
    ) -> Result<(), TransportError>
    where
        C: Clock,
    {
        if self.clock.now() >= deadline {
            Err(timeout_error)
        } else {
            Ok(())
        }
    }
}

fn reconnection_candidates(
    addresses: &[SshTarget],
    policy: &ReconnectPolicy,
) -> Result<Vec<SshTarget>, TransportError> {
    let Some(original) = addresses.first() else {
        return Err(TransportError::NoReconnectCandidates);
    };
    if addresses.len() > MAX_RECONNECT_CANDIDATES {
        return Err(TransportError::TooManyReconnectCandidates);
    }

    let mut candidates = Vec::with_capacity(addresses.len());
    let mut seen = HashSet::with_capacity(addresses.len());
    for candidate in addresses {
        if seen.insert(candidate) {
            candidates.push(candidate.clone());
        }
    }
    if let Some(hostname) = &policy.desired_local_hostname {
        let desired = SshTarget::from_str(&format!("{}@{hostname}", original.username().as_str()))
            .map_err(|_| TransportError::InvalidReconnectPolicy)?;
        if seen.insert(&desired) {
            if candidates.len() == MAX_RECONNECT_CANDIDATES {
                return Err(TransportError::TooManyReconnectCandidates);
            }
            candidates.push(desired);
        }
    }
    Ok(candidates)
}

fn keyscan_invocation(host: String, timeout: Duration) -> Invocation {
    let keyscan_timeout_seconds = timeout.as_secs().saturating_sub(1).max(1);
    Invocation::new(
        "ssh-keyscan",
        vec![
            "-T".into(),
            keyscan_timeout_seconds.to_string(),
            "-t".into(),
            "ed25519,ecdsa,rsa".into(),
            "--".into(),
            host,
        ],
    )
}

fn resolved_host_addresses(output: &[u8]) -> Result<Vec<Ipv4Addr>, TransportError> {
    if output.len() > MAX_HOST_KEY_OUTPUT_BYTES {
        return Err(TransportError::ConnectionUnavailable);
    }
    let output = std::str::from_utf8(output).map_err(|_| TransportError::ConnectionUnavailable)?;
    let mut seen = HashSet::new();
    let mut addresses = Vec::new();
    for line in output.lines() {
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        if field.trim() != "ip_address" {
            continue;
        }
        let address = value
            .trim()
            .parse::<Ipv4Addr>()
            .map_err(|_| TransportError::ConnectionUnavailable)?;
        if seen.insert(address) {
            addresses.push(address);
            if addresses.len() > MAX_RECONNECT_CANDIDATES {
                return Err(TransportError::ConnectionUnavailable);
            }
        }
    }
    if addresses.is_empty() {
        return Err(TransportError::ConnectionUnavailable);
    }
    addresses.sort();
    Ok(addresses)
}

fn bounded_attempt_timeout(remaining: Duration, configured_timeout: Duration) -> Option<Duration> {
    let timeout = remaining.min(configured_timeout);
    (timeout >= Duration::from_secs(1)).then_some(timeout)
}

#[derive(Clone, Eq, PartialEq)]
struct HostKey {
    algorithm: String,
    encoded: String,
    fingerprint: String,
}

impl fmt::Debug for HostKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostKey")
            .field("algorithm", &self.algorithm)
            .field("encoded", &"<redacted>")
            .field("fingerprint", &"<redacted>")
            .finish()
    }
}

impl HostKey {
    fn ssh_host_key_algorithms(&self) -> &'static str {
        match self.algorithm.as_str() {
            "ssh-ed25519" => "ssh-ed25519",
            "ecdsa-sha2-nistp256" => "ecdsa-sha2-nistp256",
            "ssh-rsa" => "rsa-sha2-512,rsa-sha2-256",
            _ => unreachable!("host key parser permits only supported algorithms"),
        }
    }

    fn known_hosts_line(&self, target: &SshTarget) -> String {
        format!(
            "{} {} {}\n",
            target.host().as_str(),
            self.algorithm,
            self.encoded
        )
    }
}

fn exact_known_hosts_file(
    target: &SshTarget,
    host_key: &HostKey,
) -> Result<NamedTempFile, TransportError> {
    let mut known_hosts = NamedTempFile::new().map_err(|_| TransportError::EphemeralTrustFailed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        known_hosts
            .as_file_mut()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| TransportError::EphemeralTrustFailed)?;
    }
    known_hosts
        .write_all(host_key.known_hosts_line(target).as_bytes())
        .and_then(|()| known_hosts.flush())
        .map_err(|_| TransportError::EphemeralTrustFailed)?;
    Ok(known_hosts)
}

/// Calculates the OpenSSH SHA-256 fingerprint from locally trusted
/// `ssh-keygen -F` results. One identical key per supported algorithm is
/// permitted; the deterministic algorithm preference is Ed25519, ECDSA P-256,
/// then RSA. The public key never originates from remote probe output, and
/// malformed or conflicting records fail closed.
pub fn trusted_host_key_fingerprint(output: &[u8]) -> Result<String, TransportError> {
    Ok(exactly_one_trusted_host_key(output)?.fingerprint)
}

fn exactly_one_trusted_host_key(output: &[u8]) -> Result<HostKey, TransportError> {
    let keys = parse_host_keys(output)?;
    let mut selected = None;
    for algorithm in TRUSTED_HOST_KEY_ALGORITHM_ORDER {
        let mut algorithm_keys = keys.iter().filter(|key| key.algorithm == algorithm);
        let Some(first) = algorithm_keys.next() else {
            continue;
        };
        if algorithm_keys.any(|key| key.encoded != first.encoded) {
            return Err(TransportError::TrustedHostKeyUnavailable);
        }
        if selected.is_none() {
            selected = Some(first.clone());
        }
    }
    selected.ok_or(TransportError::TrustedHostKeyUnavailable)
}

fn select_scanned_host_key(
    output: &[u8],
    expected_fingerprint: &str,
) -> Result<HostKey, TransportError> {
    let mut matching = parse_host_keys(output)?
        .into_iter()
        .filter(|key| key.fingerprint == expected_fingerprint)
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| {
        left.algorithm
            .cmp(&right.algorithm)
            .then(left.encoded.cmp(&right.encoded))
    });
    matching
        .dedup_by(|left, right| left.algorithm == right.algorithm && left.encoded == right.encoded);
    match matching.as_slice() {
        [key] => Ok(key.clone()),
        _ => Err(TransportError::HostKeyMismatch),
    }
}

fn parse_host_keys(output: &[u8]) -> Result<Vec<HostKey>, TransportError> {
    if output.len() > MAX_HOST_KEY_OUTPUT_BYTES {
        return Err(TransportError::TrustedHostKeyUnavailable);
    }
    let output =
        std::str::from_utf8(output).map_err(|_| TransportError::TrustedHostKeyUnavailable)?;
    let mut keys = Vec::new();
    for line in output.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(TransportError::TrustedHostKeyUnavailable);
        }
        let mut fields = line.split_ascii_whitespace();
        let _host = fields
            .next()
            .ok_or(TransportError::TrustedHostKeyUnavailable)?;
        let algorithm = fields
            .next()
            .ok_or(TransportError::TrustedHostKeyUnavailable)?;
        let encoded = fields
            .next()
            .ok_or(TransportError::TrustedHostKeyUnavailable)?;
        let fingerprint = fingerprint_public_key(algorithm, encoded)?;
        keys.push(HostKey {
            algorithm: algorithm.into(),
            encoded: encoded.into(),
            fingerprint,
        });
    }
    if keys.is_empty() {
        Err(TransportError::TrustedHostKeyUnavailable)
    } else {
        Ok(keys)
    }
}

fn fingerprint_public_key(algorithm: &str, encoded: &str) -> Result<String, TransportError> {
    if !matches!(algorithm, "ssh-ed25519" | "ecdsa-sha2-nistp256" | "ssh-rsa") {
        return Err(TransportError::TrustedHostKeyUnavailable);
    }
    let key = STANDARD
        .decode(encoded)
        .map_err(|_| TransportError::TrustedHostKeyUnavailable)?;
    if STANDARD.encode(&key) != encoded || !ssh_public_key_blob_matches(algorithm, &key) {
        return Err(TransportError::TrustedHostKeyUnavailable);
    }
    let digest = Sha256::digest(key);
    Ok(format!("SHA256:{}", STANDARD_NO_PAD.encode(digest)))
}

fn ssh_public_key_blob_matches(algorithm: &str, key: &[u8]) -> bool {
    let mut reader = SshWireReader::new(key);
    let Some(encoded_algorithm) = reader.read_string() else {
        return false;
    };
    if encoded_algorithm != algorithm.as_bytes() {
        return false;
    }

    match algorithm {
        "ssh-ed25519" => reader
            .read_string()
            .is_some_and(|public_key| public_key.len() == 32 && reader.is_empty()),
        "ecdsa-sha2-nistp256" => {
            let Some(curve) = reader.read_string() else {
                return false;
            };
            let Some(point) = reader.read_string() else {
                return false;
            };
            curve == b"nistp256"
                && point.len() == 65
                && point.first() == Some(&4)
                && reader.is_empty()
        }
        "ssh-rsa" => {
            let Some(exponent) = reader.read_string() else {
                return false;
            };
            let Some(modulus) = reader.read_string() else {
                return false;
            };
            canonical_positive_mpint(exponent)
                && canonical_positive_mpint(modulus)
                && reader.is_empty()
        }
        _ => false,
    }
}

struct SshWireReader<'input> {
    remaining: &'input [u8],
}

impl<'input> SshWireReader<'input> {
    fn new(input: &'input [u8]) -> Self {
        Self { remaining: input }
    }

    fn read_string(&mut self) -> Option<&'input [u8]> {
        let length_bytes = self.remaining.get(..4)?;
        let length = u32::from_be_bytes(length_bytes.try_into().expect("four bytes")) as usize;
        self.remaining = &self.remaining[4..];
        let value = self.remaining.get(..length)?;
        self.remaining = &self.remaining[length..];
        Some(value)
    }

    fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }
}

fn canonical_positive_mpint(value: &[u8]) -> bool {
    match value {
        [] => false,
        [0] => false,
        [0, second, ..] => *second >= 0x80,
        [first, ..] if *first < 0x80 => true,
        _ => false,
    }
}

fn parse_probe_text(output: &[u8]) -> Result<String, TransportError> {
    if output.is_empty() || output.len() > MAX_PROBE_OUTPUT_BYTES {
        return Err(TransportError::ProbeFailed);
    }
    let output = std::str::from_utf8(output).map_err(|_| TransportError::ProbeFailed)?;
    let value = output.strip_suffix('\n').unwrap_or(output);
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(TransportError::ProbeFailed);
    }
    Ok(value.into())
}

fn classify_probe_failure(output: &CommandOutput) -> TransportError {
    if output.status() != 255 {
        return TransportError::ProbeFailed;
    }
    if output
        .stderr()
        .windows(b"HOST IDENTIFICATION HAS CHANGED".len())
        .any(|window| window == b"HOST IDENTIFICATION HAS CHANGED")
        || output
            .stderr()
            .windows(b"Host key verification failed".len())
            .any(|window| window == b"Host key verification failed")
    {
        TransportError::HostKeyMismatch
    } else if TRANSIENT_SSH_FAILURE_MARKERS
        .iter()
        .any(|marker| contains_ascii_case_insensitive(output.stderr(), marker))
    {
        TransportError::ConnectionUnavailable
    } else {
        TransportError::ProbeFailed
    }
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn validate_probe_identity(
    host_key_sha256: &str,
    model: &str,
    serial: &str,
) -> Result<(), TransportError> {
    let fingerprint = host_key_sha256
        .strip_prefix("SHA256:")
        .ok_or(TransportError::ProbeFailed)?;
    let valid_fingerprint = fingerprint.len() == 43
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'));
    let valid_model = model == "Raspberry Pi Zero 2 W"
        || model
            .strip_prefix("Raspberry Pi Zero 2 W Rev ")
            .is_some_and(|revision| {
                !revision.is_empty()
                    && revision
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'.')
            });
    let valid_serial = serial.len() == 16
        && serial
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if valid_fingerprint && valid_model && valid_serial {
        Ok(())
    } else {
        Err(TransportError::ProbeFailed)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum TransportError {
    #[error("OpenSSH transport configuration is invalid")]
    InvalidConfiguration,
    #[error("remote command is invalid")]
    InvalidRemoteCommand,
    #[error("interactive commands must use an explicit sudo request")]
    InteractiveSudoMustStartWithSudo,
    #[error("remote copy path is invalid")]
    InvalidRemoteCopyPath,
    #[error("local copy path is invalid")]
    InvalidLocalCopyPath,
    #[error("a single strict trusted host key is required")]
    TrustedHostKeyUnavailable,
    #[error("target identity probe failed")]
    ProbeFailed,
    #[error("target was not reachable through the strict OpenSSH transport")]
    ConnectionUnavailable,
    #[error("reconnected target host key does not match the recorded target")]
    HostKeyMismatch,
    #[error("reconnected target identity does not match the recorded target")]
    TargetIdentityMismatch,
    #[error("temporary exact-key trust setup failed")]
    EphemeralTrustFailed,
    #[error("reboot was not observed because the original target never disconnected")]
    NeverDisconnected,
    #[error("target disconnected but did not return before the reconnect deadline")]
    ReconnectTimedOut,
    #[error("at least one original SSH target is required for reconnect")]
    NoReconnectCandidates,
    #[error("too many SSH reconnect candidates were supplied")]
    TooManyReconnectCandidates,
    #[error("reconnect policy is invalid or exceeds safe bounds")]
    InvalidReconnectPolicy,
    #[error("OpenSSH command failed")]
    CommandFailed,
    #[error("OpenSSH runner failed")]
    Runner(#[from] RunnerError),
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, sync::mpsc, time::Duration};

    use super::{bounded_attempt_timeout, read_limited_stream};

    #[test]
    fn bounded_capture_retains_only_the_configured_prefix_and_reports_overflow() {
        let (sender, receiver) = mpsc::channel();

        let captured =
            read_limited_stream(Cursor::new(b"abcdef"), 3, sender).expect("in-memory stream reads");

        assert_eq!(captured.bytes, b"abc");
        assert!(captured.exceeded);
        receiver.recv().expect("overflow notification");
    }

    #[test]
    fn reconnect_attempt_timeout_is_capped_by_the_remaining_phase_budget() {
        assert_eq!(
            bounded_attempt_timeout(Duration::from_secs(1), Duration::from_secs(3)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            bounded_attempt_timeout(Duration::from_millis(999), Duration::from_secs(3)),
            None
        );
    }
}
