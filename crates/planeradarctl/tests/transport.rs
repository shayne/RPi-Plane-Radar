use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    ffi::OsString,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use planeradarctl::{
    target::SshTarget,
    transport::{
        Clock, CommandOutput, CommandRunner, Invocation, OpenSshTransport, ReconnectPolicy,
        RemoteCommand, Transport, TransportConfig,
    },
};

#[derive(Default)]
struct RecordingRunner {
    invocations: RefCell<Vec<Invocation>>,
}

impl RecordingRunner {
    fn invocations(&self) -> Vec<Invocation> {
        self.invocations.borrow().clone()
    }
}

impl CommandRunner for &RecordingRunner {
    fn run(
        &self,
        invocation: Invocation,
    ) -> Result<CommandOutput, planeradarctl::transport::RunnerError> {
        self.invocations.borrow_mut().push(invocation);
        Ok(CommandOutput::success(Vec::new(), Vec::new()))
    }
}

struct ScriptedRunner {
    invocations: RefCell<Vec<Invocation>>,
    responses: RefCell<VecDeque<Result<CommandOutput, planeradarctl::transport::RunnerError>>>,
}

impl ScriptedRunner {
    fn new(
        responses: impl IntoIterator<
            Item = Result<CommandOutput, planeradarctl::transport::RunnerError>,
        >,
    ) -> Self {
        Self {
            invocations: RefCell::new(Vec::new()),
            responses: RefCell::new(responses.into_iter().collect()),
        }
    }

    fn invocations(&self) -> Vec<Invocation> {
        self.invocations.borrow().clone()
    }
}

impl CommandRunner for &ScriptedRunner {
    fn run(
        &self,
        invocation: Invocation,
    ) -> Result<CommandOutput, planeradarctl::transport::RunnerError> {
        self.invocations.borrow_mut().push(invocation);
        self.responses
            .borrow_mut()
            .pop_front()
            .expect("test supplied a response for every invocation")
    }
}

#[derive(Default)]
struct FakeClock {
    elapsed: Cell<Duration>,
    sleeps: RefCell<Vec<Duration>>,
}

impl FakeClock {
    fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.borrow().clone()
    }
}

impl Clock for &FakeClock {
    fn now(&self) -> Duration {
        self.elapsed.get()
    }

    fn sleep(&self, duration: Duration) {
        self.sleeps.borrow_mut().push(duration);
        self.elapsed.set(self.elapsed.get() + duration);
    }
}

struct AdvancingRunner<'clock> {
    invocations: RefCell<Vec<Invocation>>,
    responses: RefCell<VecDeque<Result<CommandOutput, planeradarctl::transport::RunnerError>>>,
    clock: &'clock FakeClock,
    advance_by: Duration,
}

impl<'clock> AdvancingRunner<'clock> {
    fn new(
        clock: &'clock FakeClock,
        advance_by: Duration,
        responses: impl IntoIterator<
            Item = Result<CommandOutput, planeradarctl::transport::RunnerError>,
        >,
    ) -> Self {
        Self {
            invocations: RefCell::new(Vec::new()),
            responses: RefCell::new(responses.into_iter().collect()),
            clock,
            advance_by,
        }
    }

    fn invocations(&self) -> Vec<Invocation> {
        self.invocations.borrow().clone()
    }
}

impl CommandRunner for &AdvancingRunner<'_> {
    fn run(
        &self,
        invocation: Invocation,
    ) -> Result<CommandOutput, planeradarctl::transport::RunnerError> {
        self.invocations.borrow_mut().push(invocation);
        self.clock
            .elapsed
            .set(self.clock.elapsed.get() + self.advance_by);
        self.responses
            .borrow_mut()
            .pop_front()
            .expect("test supplied a response for every invocation")
    }
}

const TEST_PUBLIC_KEY: &str =
    "AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const TEST_FINGERPRINT: &str = "SHA256:kmYcvdi2GkPeWxB6XLjrZB8JHsy2Hm8luHMFp9GMvqk";

fn ssh_wire_string(value: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(4 + value.len());
    encoded.extend_from_slice(&(value.len() as u32).to_be_bytes());
    encoded.extend_from_slice(value);
    encoded
}

fn encoded_ecdsa_p256_key(curve: &[u8], point: &[u8], trailing: &[u8]) -> String {
    let mut blob = ssh_wire_string(b"ecdsa-sha2-nistp256");
    blob.extend(ssh_wire_string(curve));
    blob.extend(ssh_wire_string(point));
    blob.extend_from_slice(trailing);
    STANDARD.encode(blob)
}

fn encoded_rsa_key(exponent: &[u8], modulus: &[u8], trailing: &[u8]) -> String {
    let mut blob = ssh_wire_string(b"ssh-rsa");
    blob.extend(ssh_wire_string(exponent));
    blob.extend(ssh_wire_string(modulus));
    blob.extend_from_slice(trailing);
    STANDARD.encode(blob)
}

fn encoded_ed25519_key(key: &[u8], trailing: &[u8]) -> String {
    let mut blob = ssh_wire_string(b"ssh-ed25519");
    blob.extend(ssh_wire_string(key));
    blob.extend_from_slice(trailing);
    STANDARD.encode(blob)
}

fn test_ecdsa_p256_key() -> String {
    let mut point = vec![4];
    point.extend([7; 64]);
    encoded_ecdsa_p256_key(b"nistp256", &point, &[])
}

fn test_rsa_key() -> String {
    encoded_rsa_key(&[1, 0, 1], &[1], &[])
}

fn trusted_key_output(host: &str) -> Vec<u8> {
    format!("{host} ssh-ed25519 {TEST_PUBLIC_KEY}\n").into_bytes()
}

fn matching_probe_responses(
    host: &str,
) -> Vec<Result<CommandOutput, planeradarctl::transport::RunnerError>> {
    vec![
        Ok(CommandOutput::success(trusted_key_output(host), Vec::new())),
        Ok(CommandOutput::success(
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"10000000abcdef01\n".to_vec(),
            Vec::new(),
        )),
    ]
}

fn refusal_responses(
    host: &str,
) -> Vec<Result<CommandOutput, planeradarctl::transport::RunnerError>> {
    vec![
        Ok(CommandOutput::success(trusted_key_output(host), Vec::new())),
        Ok(CommandOutput::new(
            255,
            Vec::new(),
            b"connection refused".to_vec(),
        )),
    ]
}

fn reconnect_policy() -> ReconnectPolicy {
    ReconnectPolicy::new(
        Duration::from_secs(3),
        Duration::from_secs(5),
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(3),
    )
    .expect("bounded policy")
}

#[test]
fn ordinary_commands_use_strict_argument_vectors_without_a_local_shell() {
    let runner = RecordingRunner::default();
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");

    transport
        .run(
            &target,
            RemoteCommand::ordinary(["uname", "-r"]).expect("remote command"),
        )
        .expect("command succeeds");

    let invocations = runner.invocations();
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].program(), "ssh");
    assert_eq!(
        invocations[0].arguments(),
        [
            "-F",
            "/dev/null",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "UpdateHostKeys=no",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "HostKeyAlgorithms=ssh-ed25519,ecdsa-sha2-nistp256,rsa-sha2-512,rsa-sha2-256",
            "-o",
            "UserKnownHostsFile=/private/trusted_known_hosts",
            "--",
            "alice@radar.local",
            "uname",
            "-r",
        ]
    );
}

#[test]
fn bounded_remote_output_sets_one_wall_clock_and_stdout_budget() {
    let runner = RecordingRunner::default();
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");

    transport
        .run_bounded(
            &target,
            RemoteCommand::ordinary(["sudo", "-n", "planeradar", "capture-snapshot"])
                .expect("remote command"),
            Duration::from_millis(750),
            8 * 1024 * 1024 + 4096,
        )
        .expect("bounded command succeeds");

    let invocation = &runner.invocations()[0];
    assert_eq!(invocation.timeout(), Some(Duration::from_millis(750)));
    assert_eq!(invocation.stdout_limit(), Some(8 * 1024 * 1024 + 4096));
}

#[test]
fn remote_shell_arguments_are_quoted_and_controls_are_rejected() {
    let runner = RecordingRunner::default();
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");

    transport
        .run(
            &target,
            RemoteCommand::ordinary(["printf", "two words", "", "single'quote", "$(id); & | < >"])
                .expect("remote command"),
        )
        .expect("command succeeds");

    assert_eq!(
        runner.invocations()[0].arguments()[16..],
        [
            "--",
            "alice@radar.local",
            "printf",
            "'two words'",
            "''",
            "'single'\"'\"'quote'",
            "'$(id); & | < >'",
        ]
    );

    for forbidden in ["contains\0nul", "contains\rreturn", "contains\nnewline"] {
        assert!(
            RemoteCommand::ordinary(["printf", forbidden]).is_err(),
            "{forbidden:?} must be rejected before SSH"
        );
    }

    transport
        .run(
            &target,
            RemoteCommand::ordinary(["PATH=/untrusted", "argument"]).expect("remote command"),
        )
        .expect("assignment-like command name is literal");
    transport
        .run(
            &target,
            RemoteCommand::ordinary(["if", "argument"]).expect("remote command"),
        )
        .expect("reserved command name is literal");
    let invocations = runner.invocations();
    assert_eq!(
        invocations[1].arguments()[16..],
        ["--", "alice@radar.local", "'PATH=/untrusted'", "argument"]
    );
    assert_eq!(
        invocations[2].arguments()[16..],
        ["--", "alice@radar.local", "'if'", "argument"]
    );
}

#[test]
fn bash_only_reserved_command_names_are_literal_remote_argv_zero() {
    let runner = RecordingRunner::default();
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");

    for command in ["function", "select", "coproc"] {
        transport
            .run(
                &target,
                RemoteCommand::ordinary([command, "argument"]).expect("remote command"),
            )
            .expect("recorded command succeeds");
    }

    for (invocation, command) in runner
        .invocations()
        .iter()
        .zip(["function", "select", "coproc"])
    {
        assert_eq!(invocation.arguments()[16], "--");
        assert_eq!(invocation.arguments()[17], "alice@radar.local");
        assert_eq!(invocation.arguments()[18], format!("'{command}'"));
        assert_eq!(invocation.arguments()[19], "argument");
    }
}

#[test]
fn only_typed_interactive_sudo_requests_allocate_a_tty() {
    let runner = RecordingRunner::default();
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");

    transport
        .run(
            &target,
            RemoteCommand::interactive_sudo(["sudo", "apt-get", "install", "-y", "jq"])
                .expect("typed sudo"),
        )
        .expect("sudo succeeds");

    let invocations = runner.invocations();
    let arguments = invocations[0].arguments();
    assert_eq!(arguments[0], "-tt");
    assert_eq!(
        arguments[17..],
        [
            "--",
            "alice@radar.local",
            "sudo",
            "apt-get",
            "install",
            "-y",
            "jq"
        ]
    );
    assert!(
        RemoteCommand::interactive_sudo(["apt-get", "install", "jq"]).is_err(),
        "interactive PTY allocation must be tied to the sudo type"
    );
}

#[test]
fn copies_use_strict_scp_vectors_and_reject_unsafe_remote_paths() {
    let runner = RecordingRunner::default();
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");

    transport
        .copy_to(
            &target,
            Path::new("/tmp/--staged-release.tar.zst"),
            Path::new("/var/lib/planeradar/release.tar.zst"),
        )
        .expect("copy to target");
    transport
        .copy_from(
            &target,
            Path::new("/var/lib/planeradar/screenshot.png"),
            Path::new("/tmp/--screenshot.png"),
        )
        .expect("copy from target");

    let invocations = runner.invocations();
    assert_eq!(invocations[0].program(), "scp");
    assert_eq!(
        invocations[0].arguments(),
        [
            "-F",
            "/dev/null",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "UpdateHostKeys=no",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "HostKeyAlgorithms=ssh-ed25519,ecdsa-sha2-nistp256,rsa-sha2-512,rsa-sha2-256",
            "-o",
            "UserKnownHostsFile=/private/trusted_known_hosts",
            "--",
            "/tmp/--staged-release.tar.zst",
            "alice@radar.local:/var/lib/planeradar/release.tar.zst",
        ]
    );
    assert_eq!(
        invocations[1].arguments()[16..],
        [
            "--",
            "alice@radar.local:/var/lib/planeradar/screenshot.png",
            "/tmp/--screenshot.png",
        ]
    );
    assert!(
        invocations
            .iter()
            .flat_map(|invocation| invocation.arguments())
            .all(|argument| argument != "-tt"),
        "copies must never allocate a PTY"
    );

    for remote in [
        "relative/path",
        "/var/lib/../etc/shadow",
        "/var/lib/planeradar:ambiguous",
        "/var/lib/planeradar/$(id)",
        "/var/lib/planeradar/two words",
        "/var/lib/planeradar//empty-component",
        "/var/lib/planeradar/line\nbreak",
    ] {
        assert!(
            transport
                .copy_to(&target, Path::new("/tmp/source"), Path::new(remote))
                .is_err(),
            "{remote:?} must not be usable as an scp remote path"
        );
    }
}

#[cfg(unix)]
#[test]
fn copies_preserve_non_unicode_local_paths_as_os_arguments() {
    let runner = RecordingRunner::default();
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");
    let local = PathBuf::from(OsString::from_vec(b"/tmp/staged-\xff-release".to_vec()));

    transport
        .copy_to(&target, &local, Path::new("/var/lib/planeradar/release"))
        .expect("copy accepts an opaque local path");

    assert_eq!(
        runner.invocations()[0].os_arguments()[17],
        local.as_os_str()
    );
}

#[test]
fn relative_local_copy_paths_cannot_be_reinterpreted_as_scp_remote_endpoints() {
    let runner = RecordingRunner::default();
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");

    transport
        .copy_to(
            &target,
            Path::new("operator@other-host:/payload"),
            Path::new("/var/lib/planeradar/release"),
        )
        .expect("copy to target");
    transport
        .copy_from(
            &target,
            Path::new("/var/lib/planeradar/screenshot"),
            Path::new("--looks-like-an-option"),
        )
        .expect("copy from target");

    let invocations = runner.invocations();
    assert_eq!(
        invocations[0].arguments()[17],
        "./operator@other-host:/payload"
    );
    assert_eq!(invocations[1].arguments()[18], "./--looks-like-an-option");
}

#[test]
fn captured_output_and_errors_redact_remote_contents() {
    let output = CommandOutput::new(
        23,
        b"private-location-and-token".to_vec(),
        b"private-stderr".to_vec(),
    );
    let debug = format!("{output:?}");
    assert!(debug.contains("redacted"));
    assert!(!debug.contains("private-location-and-token"));
    assert!(!debug.contains("private-stderr"));
    let invocation = Invocation::new("ssh", vec!["private-token-and-setting".into()]);
    assert!(!format!("{invocation:?}").contains("private-token-and-setting"));
    let command =
        RemoteCommand::ordinary(["printf", "private-token-and-setting"]).expect("remote command");
    assert!(!format!("{command:?}").contains("private-token-and-setting"));
    let config = TransportConfig::new(PathBuf::from("/private/location/settings")).expect("config");
    assert!(!format!("{config:?}").contains("/private/location/settings"));
    let policy = reconnect_policy()
        .with_desired_local_hostname("private-location.local")
        .expect("hostname");
    assert!(!format!("{policy:?}").contains("private-location.local"));
    let target = SshTarget::from_str("private-user@private-location.local").expect("target");
    let target_debug = format!("{target:?}");
    assert!(!target_debug.contains("private-user"));
    assert!(!target_debug.contains("private-location.local"));
    assert!(
        !format!(
            "{}",
            planeradarctl::transport::TransportError::CommandFailed
        )
        .contains("private")
    );
}

#[test]
fn initial_probe_binds_a_locally_calculated_trusted_host_key_to_validated_hardware() {
    let runner = ScriptedRunner::new([
        Ok(CommandOutput::success(
            format!("# Host radar.local found\nradar.local ssh-ed25519 {TEST_PUBLIC_KEY}\n")
                .into_bytes(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"10000000abcdef01\n".to_vec(),
            Vec::new(),
        )),
    ]);
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");

    let probe = transport.probe(&target).expect("trusted probe");

    assert_eq!(probe.identity.host_key_sha256, TEST_FINGERPRINT);
    assert_eq!(probe.identity.model, "Raspberry Pi Zero 2 W Rev 1.0");
    assert_eq!(probe.identity.serial, "10000000abcdef01");
    let invocations = runner.invocations();
    assert_eq!(
        invocations[0].arguments(),
        ["-F", "radar.local", "-f", "/private/trusted_known_hosts"]
    );
    assert_eq!(invocations[1].program(), "ssh");
    assert_eq!(
        invocations[1].arguments()[16..],
        [
            "--",
            "alice@radar.local",
            "tr -d '\\0' < /proc/device-tree/model",
        ]
    );
    assert_eq!(
        invocations[2].arguments()[16..],
        [
            "--",
            "alice@radar.local",
            "awk -F ': ' '/^Serial/{print $2; exit}' /proc/cpuinfo",
        ]
    );
}

#[test]
fn trusted_multi_algorithm_keys_prefer_ed25519_and_pin_strict_ssh_to_it() {
    let ecdsa = test_ecdsa_p256_key();
    let rsa = test_rsa_key();
    let trusted = format!(
        "radar.local ssh-rsa {rsa}\nradar.local ecdsa-sha2-nistp256 {ecdsa}\nradar.local ssh-ed25519 {TEST_PUBLIC_KEY}\n"
    );
    let runner = ScriptedRunner::new([
        Ok(CommandOutput::success(trusted.into_bytes(), Vec::new())),
        Ok(CommandOutput::success(
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"10000000abcdef01\n".to_vec(),
            Vec::new(),
        )),
    ]);
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");

    let probe = transport
        .probe(&target)
        .expect("nonconflicting trusted keys");

    assert_eq!(probe.identity.host_key_sha256, TEST_FINGERPRINT);
    assert!(
        runner.invocations()[1]
            .arguments()
            .iter()
            .any(|argument| argument == "HostKeyAlgorithms=ssh-ed25519"),
        "the strict probe must pin the selected first-preference key algorithm"
    );
}

#[test]
fn identity_bound_probe_scans_for_the_persisted_key_instead_of_using_stale_known_hosts() {
    let scan = format!("planeradar.local ssh-ed25519 {TEST_PUBLIC_KEY}\n");
    let runner = ScriptedRunner::new([
        Ok(CommandOutput::success(scan.into_bytes(), Vec::new())),
        Ok(CommandOutput::success(
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"10000000abcdef01\n".to_vec(),
            Vec::new(),
        )),
    ]);
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@planeradar.local").expect("target");
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };

    let reconnected = transport
        .probe_identity_bound(&target, &expected)
        .expect("probe through the exact scanned key");

    assert_eq!(reconnected, target);
    let invocations = runner.invocations();
    assert_eq!(invocations[0].program(), "ssh-keyscan");
    assert_eq!(
        invocations[0].arguments(),
        [
            "-T",
            "4",
            "-t",
            "ed25519,ecdsa,rsa",
            "--",
            "planeradar.local"
        ]
    );
    assert!(
        invocations.iter().all(|invocation| {
            invocation.program() != "ssh-keygen"
                && invocation
                    .arguments()
                    .iter()
                    .all(|argument| argument != "/private/trusted_known_hosts")
        }),
        "identity-bound adoption must not select a stale key from known_hosts"
    );
}

#[test]
fn identity_bound_probe_resolves_macos_mdns_before_retrying_keyscan() {
    let scan = format!("192.0.2.10 ssh-ed25519 {TEST_PUBLIC_KEY}\n");
    let runner = ScriptedRunner::new([
        Ok(CommandOutput::new(
            1,
            Vec::new(),
            b"getaddrinfo planeradar.local: nodename nor servname provided".to_vec(),
        )),
        Ok(CommandOutput::success(
            b"name: planeradar.local\nip_address: 192.0.2.10\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(scan.into_bytes(), Vec::new())),
        Ok(CommandOutput::success(
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"10000000abcdef01\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(Vec::new(), Vec::new())),
    ]);
    let transport = OpenSshTransport::with_runner(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
    );
    let target = SshTarget::from_str("alice@planeradar.local").expect("target");
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };

    let reconnected = transport
        .probe_identity_bound(&target, &expected)
        .expect("mDNS name resolved before the exact-key retry");

    assert_eq!(
        reconnected,
        SshTarget::from_str("alice@planeradar.local").expect("requested target")
    );
    transport
        .run(
            &reconnected,
            RemoteCommand::ordinary(["true"]).expect("remote command"),
        )
        .expect("the adopted exact-key trust remains available");
    let invocations = runner.invocations();
    assert_eq!(invocations[0].program(), "ssh-keyscan");
    assert_eq!(invocations[1].program(), "dscacheutil");
    assert_eq!(
        invocations[1].arguments(),
        ["-q", "host", "-a", "name", "planeradar.local"]
    );
    assert_eq!(invocations[2].program(), "ssh-keyscan");
    assert_eq!(
        invocations[2].arguments(),
        ["-T", "4", "-t", "ed25519,ecdsa,rsa", "--", "192.0.2.10"]
    );
    assert!(
        invocations[3]
            .arguments()
            .iter()
            .any(|argument| argument == "alice@192.0.2.10"),
        "the strict probe must keep using the verified numeric address"
    );
    assert!(
        invocations[5]
            .arguments()
            .iter()
            .any(|argument| argument == "alice@planeradar.local"),
        "later commands must return to the requested hostname"
    );
    assert!(
        invocations[5]
            .arguments()
            .iter()
            .all(|argument| argument != "UserKnownHostsFile=/private/trusted_known_hosts")
            && invocations[5]
                .arguments()
                .iter()
                .any(|argument| argument == "HostKeyAlgorithms=ssh-ed25519"),
        "later commands must retain only the adopted exact host key"
    );
}

#[test]
fn trusted_host_key_parser_rejects_missing_malformed_and_conflicting_entries() {
    for (name, output) in [
        ("missing", "# Host radar.local not found\n".to_owned()),
        (
            "malformed",
            "radar.local ssh-ed25519 not-base64\n".to_owned(),
        ),
        (
            "unsupported key type",
            format!("radar.local ssh-dss {TEST_PUBLIC_KEY}\n"),
        ),
        (
            "conflicting keys",
            format!(
                "radar.local ssh-ed25519 {TEST_PUBLIC_KEY}\nradar.local ssh-ed25519 {}\n",
                encoded_ed25519_key(&[1; 32], &[]),
            ),
        ),
    ] {
        assert!(
            planeradarctl::transport::trusted_host_key_fingerprint(output.as_bytes()).is_err(),
            "{name} key discovery output must fail closed"
        );
    }
}

#[test]
fn trusted_host_key_parser_requires_exact_algorithm_specific_wire_encodings() {
    let mut valid_point = vec![4];
    valid_point.extend([7; 64]);
    let mut truncated_rsa = ssh_wire_string(b"ssh-rsa");
    truncated_rsa.extend(ssh_wire_string(&[1, 0, 1]));
    let cases = [
        ("one-byte ed25519 blob", "ssh-ed25519", STANDARD.encode([0])),
        (
            "short ed25519 key",
            "ssh-ed25519",
            encoded_ed25519_key(&[0; 31], &[]),
        ),
        (
            "ed25519 trailing byte",
            "ssh-ed25519",
            encoded_ed25519_key(&[0; 32], &[0]),
        ),
        (
            "ecdsa wrong curve",
            "ecdsa-sha2-nistp256",
            encoded_ecdsa_p256_key(b"nistp384", &valid_point, &[]),
        ),
        (
            "ecdsa compressed point",
            "ecdsa-sha2-nistp256",
            encoded_ecdsa_p256_key(b"nistp256", &[2; 65], &[]),
        ),
        (
            "ecdsa short point",
            "ecdsa-sha2-nistp256",
            encoded_ecdsa_p256_key(b"nistp256", &valid_point[..64], &[]),
        ),
        (
            "ecdsa trailing byte",
            "ecdsa-sha2-nistp256",
            encoded_ecdsa_p256_key(b"nistp256", &valid_point, &[0]),
        ),
        ("truncated rsa", "ssh-rsa", STANDARD.encode(truncated_rsa)),
        (
            "empty rsa exponent",
            "ssh-rsa",
            encoded_rsa_key(&[], &[1], &[]),
        ),
        (
            "zero rsa exponent",
            "ssh-rsa",
            encoded_rsa_key(&[0], &[1], &[]),
        ),
        (
            "zero rsa modulus",
            "ssh-rsa",
            encoded_rsa_key(&[1, 0, 1], &[0], &[]),
        ),
        (
            "negative rsa modulus",
            "ssh-rsa",
            encoded_rsa_key(&[1, 0, 1], &[0x80], &[]),
        ),
        (
            "noncanonical rsa exponent",
            "ssh-rsa",
            encoded_rsa_key(&[0, 1], &[1], &[]),
        ),
        (
            "rsa trailing byte",
            "ssh-rsa",
            encoded_rsa_key(&[1, 0, 1], &[1], &[0]),
        ),
    ];

    for (name, algorithm, encoded) in cases {
        let output = format!("radar.local {algorithm} {encoded}\n");
        assert!(
            planeradarctl::transport::trusted_host_key_fingerprint(output.as_bytes()).is_err(),
            "{name} must fail closed"
        );
    }
}

#[test]
fn probes_reject_oversized_trailing_or_invalid_identity_output() {
    let cases = [
        (
            "trailing model field",
            b"Raspberry Pi Zero 2 W Rev 1.0\nextra".to_vec(),
            b"10000000abcdef01\n".to_vec(),
        ),
        (
            "oversized model",
            vec![b'a'; 257],
            b"10000000abcdef01\n".to_vec(),
        ),
        (
            "wrong model",
            b"Raspberry Pi 5\n".to_vec(),
            b"10000000abcdef01\n".to_vec(),
        ),
        (
            "uppercase serial",
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            b"10000000ABCDEF01\n".to_vec(),
        ),
    ];

    for (name, model, serial) in cases {
        let runner = ScriptedRunner::new([
            Ok(CommandOutput::success(
                format!("radar.local ssh-ed25519 {TEST_PUBLIC_KEY}\n").into_bytes(),
                Vec::new(),
            )),
            Ok(CommandOutput::success(model, Vec::new())),
            Ok(CommandOutput::success(serial, Vec::new())),
        ]);
        let transport = OpenSshTransport::with_runner(
            &runner,
            TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        );
        let target = SshTarget::from_str("alice@radar.local").expect("target");

        assert!(transport.probe(&target).is_err(), "{name} must fail closed");
    }
}

#[test]
fn reboot_wait_requires_disconnect_then_accepts_an_exact_new_local_hostname() {
    let known = format!("radar.local ssh-ed25519 {TEST_PUBLIC_KEY}\n");
    let scan = format!(
        "planeradar.local ssh-ed25519 {TEST_PUBLIC_KEY}\nplaneradar.local ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB\n"
    );
    let runner = ScriptedRunner::new([
        Ok(CommandOutput::success(
            known.as_bytes().to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"10000000abcdef01\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            known.as_bytes().to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::new(
            255,
            Vec::new(),
            b"connection refused".to_vec(),
        )),
        Ok(CommandOutput::success(
            known.as_bytes().to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::new(
            255,
            Vec::new(),
            b"connection refused".to_vec(),
        )),
        Ok(CommandOutput::success(scan.into_bytes(), Vec::new())),
        Ok(CommandOutput::success(
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"10000000abcdef01\n".to_vec(),
            Vec::new(),
        )),
    ]);
    let clock = FakeClock::default();
    let transport = OpenSshTransport::with_runner_and_clock(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &clock,
    );
    let original = SshTarget::from_str("alice@radar.local").expect("target");
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };
    let policy = ReconnectPolicy::new(
        Duration::from_secs(10),
        Duration::from_secs(30),
        Duration::from_secs(1),
        Duration::from_secs(4),
        Duration::from_secs(3),
    )
    .expect("bounded policy")
    .with_desired_local_hostname("planeradar.local")
    .expect("desired hostname");

    let reconnected = transport
        .wait_for_reboot(&expected, std::slice::from_ref(&original), policy)
        .expect("reconnect through exact scanned key");

    assert_eq!(
        reconnected,
        SshTarget::from_str("alice@planeradar.local").expect("new target")
    );
    let invocations = runner.invocations();
    assert_eq!(invocations[7].program(), "ssh-keyscan");
    assert_eq!(
        invocations[7].arguments(),
        [
            "-T",
            "2",
            "-t",
            "ed25519,ecdsa,rsa",
            "--",
            "planeradar.local"
        ]
    );
    assert!(
        invocations[8]
            .arguments()
            .iter()
            .any(|argument| argument == "StrictHostKeyChecking=yes"),
        "alternate address must still use strict host-key checking"
    );
    assert!(
        invocations[8]
            .arguments()
            .iter()
            .any(|argument| argument == "ConnectTimeout=3"),
        "per-attempt reconnect timeout must reach the strict SSH probe"
    );
    assert!(
        invocations[8]
            .arguments()
            .iter()
            .any(|argument| argument == "GlobalKnownHostsFile=/dev/null"),
        "alternate exact-key probe must exclude ambient global known-hosts"
    );
    assert!(
        invocations[8]
            .arguments()
            .windows(2)
            .any(|arguments| arguments == ["-F", "/dev/null"]),
        "alternate exact-key probe must exclude ambient ssh config"
    );
    assert!(
        invocations[8]
            .arguments()
            .iter()
            .all(|argument| argument != "StrictHostKeyChecking=no" && argument != "accept-new"),
        "alternate address must not weaken strict host-key policy"
    );
    assert_eq!(clock.sleeps(), Vec::<Duration>::new());
}

#[test]
fn reboot_wait_uses_the_persisted_key_when_a_numeric_target_is_absent_from_known_hosts() {
    let scan = format!("192.0.2.10 ssh-ed25519 {TEST_PUBLIC_KEY}\n");
    let identity = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };
    let unavailable_trusted_key = || Ok(CommandOutput::new(1, Vec::new(), Vec::new()));
    let matching_scan = || Ok(CommandOutput::success(scan.as_bytes().to_vec(), Vec::new()));
    let model = || {
        Ok(CommandOutput::success(
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            Vec::new(),
        ))
    };
    let serial = || {
        Ok(CommandOutput::success(
            b"10000000abcdef01\n".to_vec(),
            Vec::new(),
        ))
    };
    let runner = ScriptedRunner::new([
        unavailable_trusted_key(),
        matching_scan(),
        model(),
        serial(),
        Ok(CommandOutput::new(
            255,
            Vec::new(),
            b"connection refused".to_vec(),
        )),
        model(),
        serial(),
    ]);
    let clock = FakeClock::default();
    let transport = OpenSshTransport::with_runner_and_clock(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &clock,
    );
    let target = SshTarget::from_str("alice@192.0.2.10").expect("numeric target");

    let reconnected = transport
        .wait_for_reboot(&identity, std::slice::from_ref(&target), reconnect_policy())
        .expect("identity-bound numeric reboot");

    assert_eq!(reconnected, target);
    assert_eq!(
        runner
            .invocations()
            .iter()
            .filter(|invocation| invocation.program() == "ssh-keyscan")
            .count(),
        1
    );
    assert!(
        runner.invocations()[4]
            .arguments()
            .iter()
            .all(|argument| argument != "UserKnownHostsFile=/private/trusted_known_hosts"),
        "reboot polling must retain the adopted exact host key"
    );
}

#[test]
fn reboot_wait_times_out_when_the_original_target_never_disconnects_without_real_sleeping() {
    let mut responses = matching_probe_responses("radar.local");
    responses.extend(matching_probe_responses("radar.local"));
    responses.extend(matching_probe_responses("radar.local"));
    responses.extend(matching_probe_responses("radar.local"));
    let runner = ScriptedRunner::new(responses);
    let clock = FakeClock::default();
    let transport = OpenSshTransport::with_runner_and_clock(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &clock,
    );
    let original = SshTarget::from_str("alice@radar.local").expect("target");
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };

    assert!(matches!(
        transport.wait_for_reboot(&expected, &[original], reconnect_policy()),
        Err(planeradarctl::transport::TransportError::NeverDisconnected)
    ));
    assert_eq!(
        clock.sleeps(),
        vec![Duration::from_secs(1), Duration::from_secs(2)]
    );
}

#[test]
fn reboot_wait_bounds_backoff_and_distinguishes_a_disconnected_target_that_never_returns() {
    let mut responses = matching_probe_responses("radar.local");
    responses.extend(refusal_responses("radar.local"));
    responses.extend(refusal_responses("radar.local"));
    responses.extend(refusal_responses("radar.local"));
    responses.extend(refusal_responses("radar.local"));
    responses.extend(refusal_responses("radar.local"));
    let runner = ScriptedRunner::new(responses);
    let clock = FakeClock::default();
    let transport = OpenSshTransport::with_runner_and_clock(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &clock,
    );
    let original = SshTarget::from_str("alice@radar.local").expect("target");
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };

    assert!(matches!(
        transport.wait_for_reboot(&expected, &[original], reconnect_policy()),
        Err(planeradarctl::transport::TransportError::ReconnectTimedOut)
    ));
    assert_eq!(
        clock.sleeps(),
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(2)
        ]
    );
}

#[test]
fn generic_exit_255_failures_are_terminal_and_never_trigger_reconnect() {
    for stderr in [
        b"Permission denied (publickey).".as_slice(),
        b"Bad configuration option: made-up-setting".as_slice(),
    ] {
        let mut responses = matching_probe_responses("radar.local");
        responses.extend([
            Ok(CommandOutput::success(
                trusted_key_output("radar.local"),
                Vec::new(),
            )),
            Ok(CommandOutput::new(255, Vec::new(), stderr.to_vec())),
        ]);
        let runner = ScriptedRunner::new(responses);
        let clock = FakeClock::default();
        let transport = OpenSshTransport::with_runner_and_clock(
            &runner,
            TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
            &clock,
        );
        let original = SshTarget::from_str("alice@radar.local").expect("target");
        let expected = planeradarctl::target::TargetIdentity {
            host_key_sha256: TEST_FINGERPRINT.into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef01".into(),
        };

        assert!(matches!(
            transport.wait_for_reboot(&expected, &[original], reconnect_policy()),
            Err(planeradarctl::transport::TransportError::ProbeFailed)
        ));
        assert_eq!(
            runner.invocations().len(),
            5,
            "terminal failure must not retry"
        );
        assert!(
            runner
                .invocations()
                .iter()
                .all(|invocation| invocation.program() != "ssh-keyscan"),
            "terminal failure must not scan alternate candidates"
        );
        assert!(
            clock.sleeps().is_empty(),
            "terminal failure must not back off"
        );
    }
}

#[test]
fn reboot_wait_treats_open_ssh_connection_closed_by_address_as_a_disconnect() {
    let mut responses = matching_probe_responses("radar.local");
    responses.extend([
        Ok(CommandOutput::success(
            trusted_key_output("radar.local"),
            Vec::new(),
        )),
        Ok(CommandOutput::new(
            255,
            Vec::new(),
            b"CONNECTION CLOSED BY 192.0.2.1 PORT 22".to_vec(),
        )),
    ]);
    responses.extend(matching_probe_responses("radar.local"));
    let runner = ScriptedRunner::new(responses);
    let clock = FakeClock::default();
    let transport = OpenSshTransport::with_runner_and_clock(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &clock,
    );
    let original = SshTarget::from_str("alice@radar.local").expect("target");
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };

    assert_eq!(
        transport
            .wait_for_reboot(
                &expected,
                std::slice::from_ref(&original),
                reconnect_policy()
            )
            .expect("OpenSSH address-close stderr must enter reconnect"),
        original
    );
    assert_eq!(
        runner.invocations().len(),
        8,
        "the reconnect probe must run after the observed disconnect"
    );
    assert!(clock.sleeps().is_empty());
}

#[test]
fn preverified_reboot_accepts_disconnect_before_the_first_wait_probe() {
    let mut responses = refusal_responses("radar.local");
    responses.extend(matching_probe_responses("radar.local"));
    let runner = ScriptedRunner::new(responses);
    let clock = FakeClock::default();
    let transport = OpenSshTransport::with_runner_and_clock(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &clock,
    );
    let target = SshTarget::from_str("alice@radar.local").expect("target");
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };
    let policy = reconnect_policy().after_identity_verified();

    assert_eq!(
        transport
            .wait_for_reboot(&expected, std::slice::from_ref(&target), policy)
            .expect("reconnect after an already observed identity"),
        target
    );
}

#[test]
fn reboot_deadline_caps_each_keyscan_and_probe_suboperation() {
    let known = format!("radar.local ssh-ed25519 {TEST_PUBLIC_KEY}\n");
    let scan = format!("planeradar.local ssh-ed25519 {TEST_PUBLIC_KEY}\n");
    let clock = FakeClock::default();
    let mut responses = matching_probe_responses("radar.local");
    responses.extend([
        Ok(CommandOutput::success(
            known.as_bytes().to_vec(),
            Vec::new(),
        )),
        Ok(CommandOutput::new(
            255,
            Vec::new(),
            b"connection refused".to_vec(),
        )),
        Ok(CommandOutput::success(known.into_bytes(), Vec::new())),
        Ok(CommandOutput::new(
            255,
            Vec::new(),
            b"connection refused".to_vec(),
        )),
        Ok(CommandOutput::success(scan.into_bytes(), Vec::new())),
        Ok(CommandOutput::success(
            b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
            Vec::new(),
        )),
    ]);
    let runner = AdvancingRunner::new(&clock, Duration::from_secs(1), responses);
    let transport = OpenSshTransport::with_runner_and_clock(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &clock,
    );
    let original = SshTarget::from_str("alice@radar.local").expect("target");
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };
    let policy = ReconnectPolicy::new(
        Duration::from_secs(10),
        Duration::from_secs(4),
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(3),
    )
    .expect("policy")
    .with_desired_local_hostname("planeradar.local")
    .expect("desired address");

    assert!(matches!(
        transport.wait_for_reboot(&expected, &[original], policy),
        Err(planeradarctl::transport::TransportError::ReconnectTimedOut)
    ));

    let invocations = runner.invocations();
    assert_eq!(invocations.len(), 9, "deadline stops before serial probe");
    assert_eq!(
        invocations
            .iter()
            .map(Invocation::timeout)
            .collect::<Vec<_>>(),
        vec![
            Some(Duration::from_secs(3)),
            Some(Duration::from_secs(3)),
            Some(Duration::from_secs(3)),
            Some(Duration::from_secs(3)),
            Some(Duration::from_secs(3)),
            Some(Duration::from_secs(3)),
            Some(Duration::from_secs(3)),
            Some(Duration::from_secs(2)),
            Some(Duration::from_secs(1)),
        ],
        "every external operation receives the phase's then-current remainder"
    );
    assert_eq!(invocations[7].program(), "ssh-keyscan");
    assert_eq!(invocations[8].program(), "ssh");
    assert!(clock.sleeps().is_empty(), "fake runner never sleeps");
}

#[test]
fn reconnect_fails_closed_for_a_mismatched_key_model_or_serial_and_deduplicates_candidates() {
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };
    let original = SshTarget::from_str("alice@radar.local").expect("target");

    let cases = [
        (
            "host key",
            vec![Ok(CommandOutput::success(
                b"planeradar.local ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB\n".to_vec(),
                Vec::new(),
            ))],
        ),
        (
            "model",
            vec![
                Ok(CommandOutput::success(
                    trusted_key_output("planeradar.local"),
                    Vec::new(),
                )),
                Ok(CommandOutput::success(
                    b"Raspberry Pi Zero 2 W Rev 1.1\n".to_vec(),
                    Vec::new(),
                )),
                Ok(CommandOutput::success(b"10000000abcdef01\n".to_vec(), Vec::new())),
            ],
        ),
        (
            "serial",
            vec![
                Ok(CommandOutput::success(
                    trusted_key_output("planeradar.local"),
                    Vec::new(),
                )),
                Ok(CommandOutput::success(
                    b"Raspberry Pi Zero 2 W Rev 1.0\n".to_vec(),
                    Vec::new(),
                )),
                Ok(CommandOutput::success(b"10000000abcdef02\n".to_vec(), Vec::new())),
            ],
        ),
    ];

    for (name, alternate) in cases {
        let mut responses = matching_probe_responses("radar.local");
        responses.extend(refusal_responses("radar.local"));
        responses.extend(refusal_responses("radar.local"));
        responses.extend(alternate);
        let runner = ScriptedRunner::new(responses);
        let clock = FakeClock::default();
        let transport = OpenSshTransport::with_runner_and_clock(
            &runner,
            TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
            &clock,
        );
        let policy = reconnect_policy()
            .with_desired_local_hostname("planeradar.local")
            .expect("hostname");

        assert!(
            transport
                .wait_for_reboot(&expected, &[original.clone(), original.clone()], policy)
                .is_err(),
            "reachable {name} mismatch must fail closed"
        );
        assert_eq!(
            runner
                .invocations()
                .iter()
                .filter(|invocation| invocation.program() == "ssh-keyscan")
                .count(),
            1,
            "duplicate original candidates must not add an alternate scan"
        );
    }
}

#[test]
fn reconnect_requires_candidates_and_rejects_out_of_bounds_policies() {
    let runner = ScriptedRunner::new([]);
    let clock = FakeClock::default();
    let transport = OpenSshTransport::with_runner_and_clock(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &clock,
    );
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };

    assert!(matches!(
        transport.wait_for_reboot(&expected, &[], reconnect_policy()),
        Err(planeradarctl::transport::TransportError::NoReconnectCandidates)
    ));
    assert!(
        ReconnectPolicy::new(
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .is_err()
    );
    assert!(
        ReconnectPolicy::new(
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .is_err()
    );
}

#[test]
fn reconnect_candidate_limit_rejects_overflow_before_probing_and_allows_the_boundary() {
    let boundary = (1..=8)
        .map(|octet| SshTarget::from_str(&format!("alice@192.0.2.{octet}")).expect("target"))
        .collect::<Vec<_>>();
    let overflow = (1..=9)
        .map(|octet| SshTarget::from_str(&format!("alice@192.0.2.{octet}")).expect("target"))
        .collect::<Vec<_>>();
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };

    let boundary_runner = RecordingRunner::default();
    let boundary_clock = FakeClock::default();
    let boundary_transport = OpenSshTransport::with_runner_and_clock(
        &boundary_runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &boundary_clock,
    );
    assert!(matches!(
        boundary_transport.wait_for_reboot(&expected, &boundary, reconnect_policy()),
        Err(planeradarctl::transport::TransportError::TrustedHostKeyUnavailable)
    ));
    assert_eq!(
        boundary_runner.invocations().len(),
        2,
        "the boundary list reaches the initial probe and identity-bound fallback"
    );

    let overflow_runner = RecordingRunner::default();
    let overflow_clock = FakeClock::default();
    let overflow_transport = OpenSshTransport::with_runner_and_clock(
        &overflow_runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &overflow_clock,
    );
    assert!(matches!(
        overflow_transport.wait_for_reboot(&expected, &overflow, reconnect_policy()),
        Err(planeradarctl::transport::TransportError::TooManyReconnectCandidates)
    ));
    assert!(
        overflow_runner.invocations().is_empty(),
        "over-limit input must fail before any host-key or network probe"
    );
}

#[test]
fn original_host_key_verification_failure_is_not_retried_as_a_disconnect() {
    let mut responses = matching_probe_responses("radar.local");
    responses.extend([
        Ok(CommandOutput::success(
            trusted_key_output("radar.local"),
            Vec::new(),
        )),
        Ok(CommandOutput::new(
            255,
            Vec::new(),
            b"@@@@@@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@@@@@@".to_vec(),
        )),
    ]);
    let runner = ScriptedRunner::new(responses);
    let clock = FakeClock::default();
    let transport = OpenSshTransport::with_runner_and_clock(
        &runner,
        TransportConfig::new(PathBuf::from("/private/trusted_known_hosts")).expect("config"),
        &clock,
    );
    let original = SshTarget::from_str("alice@radar.local").expect("target");
    let expected = planeradarctl::target::TargetIdentity {
        host_key_sha256: TEST_FINGERPRINT.into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    };

    assert!(matches!(
        transport.wait_for_reboot(&expected, &[original], reconnect_policy()),
        Err(planeradarctl::transport::TransportError::HostKeyMismatch)
    ));
    assert!(clock.sleeps().is_empty(), "mismatch must not be retried");
}
