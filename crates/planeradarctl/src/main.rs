use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;
use std::time::UNIX_EPOCH;
use std::{env, fs};

use clap::Parser;
use nix::{
    errno::Errno,
    sys::signal::{
        SaFlags, SigAction, SigHandler, SigSet, SigmaskHow, Signal, killpg, pthread_sigmask, raise,
        sigaction,
    },
    unistd::{Pid, geteuid, getpgid, getpgrp, getpid, getppid, setpgid, tcgetpgrp, tcsetpgrp},
};
use planeradarctl::{
    DriverLock,
    cli::{Cli, Command, DriverCommand},
    config::{Environment, InstallConfig, resolve_missing_mutating_target},
    driver::{
        DriverAction, DriverContext, DriverManager, DriverTool, GhDriverReleaseSource,
        GhDriverReleaseVerifier, TargetProbe as DriverTargetProbe,
    },
    install::{
        ApplicationPayload, InstallOutcome, InstallRequest, Installer,
        extract_application_payload_at_mtime,
    },
    operations::{
        AcceptedPair, LifecycleBackend, LifecycleError, LifecycleManager, LifecycleState,
        MANAGEMENT_HELPER_PROTOCOL, ManagementHelper, OperationsClient, ReleasePair,
        SshOperationsBackend, SystemCaptureClock, UninstallPhase,
    },
    preflight::{SystemUnixClock, TargetPreflight},
    release::{GhReleaseSource, MANIFEST_NAME, ReleaseClient, ReleaseInput, Verifier},
    state::{
        ArtifactIdentity, InstallPhase, InstallState, LocalStateStore, OwnedFile, StateStore,
        TargetHardwareIdentity, TargetInstallState,
    },
    system_install::{
        SystemDriverActions, SystemHostPreflight,
        SystemInstallBackend as LibrarySystemInstallBackend, SystemInstallClock,
    },
    target::{SshTarget, TargetIdentity},
    transport::{
        Clock, CommandRunner as TransportCommandRunner, OpenSshTransport, ReconnectPolicy,
        RemoteCommand, SystemClock, SystemCommandRunner, Transport, TransportConfig,
        TransportError,
    },
};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const INTERNAL_BOOTSTRAP_ARG: &str = "--__planeradar-bootstrap-v1";
const INTERNAL_FOREGROUND_TTY_ARG: &str = "--__planeradar-foreground-tty-v1";
const INTERNAL_RESTORE_TTY_ARG: &str = "--__planeradar-restore-tty-v1";
const INTERNAL_BOOTSTRAP_MARKER: &str = "control-bootstrap.ready";
const INTERNAL_CONTINUE_MARKER: &str = "control-bootstrap.continue";
const INTERNAL_MARKER_MAX_BYTES: u64 = 96;
const INTERNAL_CONTINUE_TIMEOUT: Duration = Duration::from_secs(3);
const INTERNAL_CONTINUE_POLL_INTERVAL: Duration = Duration::from_millis(5);
static TERMINAL_SIGNAL_PARENT: AtomicI32 = AtomicI32::new(0);

extern "C" fn relay_terminal_signal(signal: libc::c_int) {
    let parent = TERMINAL_SIGNAL_PARENT.load(Ordering::Relaxed);
    if parent > 1 {
        // SAFETY: kill is async-signal-safe, and both arguments are plain
        // integers captured before the handler is installed.
        unsafe {
            libc::kill(parent, signal);
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(status) => status,
        Err(error) => {
            eprintln!("planeradarctl: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let arguments = match bootstrap_action()? {
        BootstrapAction::Execute { arguments } => arguments,
        BootstrapAction::Supervise {
            arguments,
            bootstrap,
        } => {
            return Ok(ExitCode::from(supervise_internal_control(
                arguments, bootstrap,
            )?));
        }
        BootstrapAction::TerminalAdjusted => return Ok(ExitCode::SUCCESS),
    };
    let cli = Cli::parse_from(arguments);
    if let Command::Driver { command } = cli.command.clone() {
        run_driver(command)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Command::SmokeVerify(options) = cli.command.clone() {
        planeradarctl::smoke::verify_smoke_artifacts(
            &options.release_dir,
            &options.doctor_json,
            &options.screenshot,
            UNIX_EPOCH
                .checked_add(Duration::from_secs(options.captured_after))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid capture timestamp")
                })?,
        )?;
        println!("Plane Radar smoke verified");
        return Ok(ExitCode::SUCCESS);
    }
    let environment = Environment::from_dotenv_path(Path::new(".env"))?;
    match cli.command.clone() {
        Command::Status(options) => {
            run_remote_operation(options.target, environment, RemoteOperation::Status)?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Doctor(options) => {
            run_remote_operation(
                options.target,
                environment,
                RemoteOperation::Doctor { json: options.json },
            )?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Screenshot(options) => {
            run_remote_operation(
                options.target,
                environment,
                RemoteOperation::Screenshot {
                    output: options.output,
                },
            )?;
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }
    if cli.command.is_mutating() {
        let command = cli.command.clone();
        let mut config = InstallConfig::resolve(cli, environment)?;
        let stdin = io::stdin();
        let stdin_is_terminal = stdin.is_terminal();
        resolve_missing_mutating_target(
            &mut config,
            stdin_is_terminal,
            &mut stdin.lock(),
            &mut io::stderr(),
        )?;
        match command {
            Command::Install(_) => run_install(config),
            Command::Upgrade(_) => run_lifecycle_target(config, "upgrade"),
            Command::Rollback(_) => run_lifecycle_target(config, "rollback"),
            Command::Uninstall(_) => run_lifecycle_target(config, "uninstall"),
            _ => unreachable!("mutating command classification is exhaustive"),
        }?;
    }
    Ok(ExitCode::SUCCESS)
}

enum BootstrapAction {
    Execute {
        arguments: Vec<OsString>,
    },
    Supervise {
        arguments: Vec<OsString>,
        bootstrap: InternalBootstrap,
    },
    TerminalAdjusted,
}

struct ForegroundTerminalGuard {
    original_pgid: Pid,
    control_pgid: Pid,
    restored: bool,
}

impl ForegroundTerminalGuard {
    fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        restore_terminal_foreground(self.original_pgid, self.control_pgid)?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for ForegroundTerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

struct InternalBootstrap {
    marker_file: fs::File,
    terminal_guard: Option<ForegroundTerminalGuard>,
}

impl InternalBootstrap {
    fn restore_terminal(&mut self) -> io::Result<()> {
        self.terminal_guard
            .as_mut()
            .map(ForegroundTerminalGuard::restore)
            .unwrap_or(Ok(()))
    }
}

enum SavedTerminal {
    None,
    Foreground {
        original_pgid: Pid,
        control_pgid: Pid,
    },
}

fn bootstrap_action() -> Result<BootstrapAction, Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().collect::<Vec<_>>();
    match arguments.get(1).and_then(|value| value.to_str()) {
        Some(INTERNAL_FOREGROUND_TTY_ARG) => {
            block_sigttou_permanently()?;
            if arguments.len() != 3 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "internal terminal foreground marker is required",
                )
                .into());
            }
            foreground_internal_terminal(Path::new(&arguments[2]))?;
            Ok(BootstrapAction::TerminalAdjusted)
        }
        Some(INTERNAL_RESTORE_TTY_ARG) => {
            block_sigttou_permanently()?;
            if arguments.len() != 3 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "internal terminal restore marker is required",
                )
                .into());
            }
            restore_internal_terminal(Path::new(&arguments[2]))?;
            Ok(BootstrapAction::TerminalAdjusted)
        }
        Some(INTERNAL_BOOTSTRAP_ARG) => {
            if arguments.len() < 4 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "internal bootstrap markers are required",
                )
                .into());
            }
            let marker = PathBuf::from(arguments.remove(2));
            let continue_marker = PathBuf::from(arguments.remove(2));
            arguments.remove(1);
            let bootstrap = enter_internal_bootstrap(&marker, &continue_marker)?;
            Ok(BootstrapAction::Supervise {
                arguments,
                bootstrap,
            })
        }
        _ => Ok(BootstrapAction::Execute { arguments }),
    }
}

fn supervise_internal_control(
    arguments: Vec<OsString>,
    mut bootstrap: InternalBootstrap,
) -> io::Result<u8> {
    let executable = env::current_exe()?;
    let worker_status = match ProcessCommand::new(executable)
        .args(arguments.iter().skip(1))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .and_then(|mut worker| worker.wait())
    {
        Ok(status) => worker_exit_code(status),
        Err(error) => {
            eprintln!("planeradarctl: internal control worker failed: {error}");
            1
        }
    };

    bootstrap.restore_terminal()?;
    writeln!(bootstrap.marker_file, "complete {worker_status}")?;
    bootstrap.marker_file.sync_all()?;
    killpg(getpgrp(), Signal::SIGSTOP).map_err(|error| {
        io::Error::other(format!(
            "internal supervisor process-group stop failed: {error}"
        ))
    })?;
    Ok(worker_status)
}

fn worker_exit_code(status: std::process::ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        u8::try_from(code).unwrap_or(1)
    } else if let Some(signal) = status.signal() {
        u8::try_from(128_i32.saturating_add(signal)).unwrap_or(1)
    } else {
        1
    }
}

fn validate_internal_private_file(
    marker: &Path,
    expected_name: &str,
    require_empty: bool,
) -> io::Result<fs::File> {
    if !marker.is_absolute()
        || marker.file_name().and_then(|name| name.to_str()) != Some(expected_name)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "internal bootstrap marker path is invalid",
        ));
    }

    let executable = env::current_exe()?;
    let executable_parent = executable.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "internal bootstrap executable has no parent",
        )
    })?;
    let marker_parent = marker.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "internal bootstrap marker has no parent",
        )
    })?;
    if fs::canonicalize(executable_parent)? != fs::canonicalize(marker_parent)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "internal bootstrap marker is outside the executable directory",
        ));
    }

    let expected_uid = geteuid().as_raw();
    let parent_metadata = fs::symlink_metadata(executable_parent)?;
    if !parent_metadata.is_dir()
        || parent_metadata.uid() != expected_uid
        || parent_metadata.mode() & 0o7777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "internal bootstrap executable directory is not private",
        ));
    }

    let marker_metadata = fs::symlink_metadata(marker)?;
    if !marker_metadata.is_file()
        || marker_metadata.uid() != expected_uid
        || marker_metadata.mode() & 0o7777 != 0o600
        || (require_empty && marker_metadata.len() != 0)
        || (!require_empty
            && (marker_metadata.len() == 0 || marker_metadata.len() > INTERNAL_MARKER_MAX_BYTES))
        || marker_metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "internal bootstrap marker is not a new private regular file",
        ));
    }
    let marker_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(marker)?;
    let opened_metadata = marker_file.metadata()?;
    if opened_metadata.dev() != marker_metadata.dev()
        || opened_metadata.ino() != marker_metadata.ino()
        || !opened_metadata.is_file()
        || opened_metadata.uid() != expected_uid
        || opened_metadata.mode() & 0o7777 != 0o600
        || opened_metadata.len() != marker_metadata.len()
        || opened_metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "internal bootstrap marker identity changed",
        ));
    }

    Ok(marker_file)
}

fn validate_internal_marker(marker: &Path, require_empty: bool) -> io::Result<fs::File> {
    validate_internal_private_file(marker, INTERNAL_BOOTSTRAP_MARKER, require_empty)
}

fn inherited_foreground_terminal() -> io::Result<Option<Pid>> {
    let foreground = match tcgetpgrp(io::stdin()) {
        Ok(pgid) => pgid,
        Err(Errno::ENOTTY) => return Ok(None),
        Err(error) => {
            return Err(io::Error::other(format!(
                "internal bootstrap terminal inspection failed: {error}"
            )));
        }
    };
    let parent_pgid = getpgid(Some(getppid())).map_err(|error| {
        io::Error::other(format!(
            "internal bootstrap parent process-group inspection failed: {error}"
        ))
    })?;
    if foreground != parent_pgid || getpgrp() != parent_pgid {
        block_sigttou_permanently()?;
        return Err(io::Error::other(
            "internal bootstrap installer is not the terminal foreground process group",
        ));
    }
    Ok(Some(foreground))
}

fn with_sigttou_blocked<T>(operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let mut blocked = SigSet::empty();
    blocked.add(Signal::SIGTTOU);
    let mut previous = SigSet::empty();
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), Some(&mut previous))
        .map_err(|error| io::Error::other(format!("could not block SIGTTOU: {error}")))?;
    let result = operation();
    let restored = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None)
        .map_err(|error| io::Error::other(format!("could not restore signal mask: {error}")));
    match (result, restored) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn block_sigttou_permanently() -> io::Result<()> {
    let mut blocked = SigSet::empty();
    blocked.add(Signal::SIGTTOU);
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), None)
        .map_err(|error| io::Error::other(format!("could not block SIGTTOU: {error}")))
}

fn install_terminal_signal_relay(parent: Pid) -> io::Result<()> {
    TERMINAL_SIGNAL_PARENT.store(parent.as_raw(), Ordering::Relaxed);
    let action = SigAction::new(
        SigHandler::Handler(relay_terminal_signal),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    for signal in [Signal::SIGHUP, Signal::SIGINT, Signal::SIGTERM] {
        // SAFETY: the handler has C ABI, performs only an async-signal-safe
        // kill, and remains valid for the lifetime of this process.
        unsafe { sigaction(signal, &action) }.map_err(|error| {
            io::Error::other(format!(
                "internal bootstrap terminal signal relay failed: {error}"
            ))
        })?;
    }
    Ok(())
}

fn hand_off_terminal_foreground(
    original_pgid: Pid,
    control_pgid: Pid,
) -> io::Result<ForegroundTerminalGuard> {
    with_sigttou_blocked(|| {
        let foreground = tcgetpgrp(io::stdin()).map_err(|error| {
            io::Error::other(format!(
                "internal bootstrap terminal handoff inspection failed: {error}"
            ))
        })?;
        if foreground != control_pgid {
            if foreground != original_pgid {
                return Err(io::Error::other(
                    "internal bootstrap terminal foreground changed before handoff",
                ));
            }
            tcsetpgrp(io::stdin(), control_pgid).map_err(|error| {
                io::Error::other(format!(
                    "internal bootstrap terminal foreground handoff failed: {error}"
                ))
            })?;
        }
        if tcgetpgrp(io::stdin()).map_err(|error| {
            io::Error::other(format!(
                "internal bootstrap terminal handoff verification failed: {error}"
            ))
        })? != control_pgid
        {
            return Err(io::Error::other(
                "internal bootstrap terminal foreground handoff was not retained",
            ));
        }
        Ok(ForegroundTerminalGuard {
            original_pgid,
            control_pgid,
            restored: false,
        })
    })
}

fn restore_terminal_foreground(original_pgid: Pid, control_pgid: Pid) -> io::Result<()> {
    with_sigttou_blocked(|| {
        let foreground = tcgetpgrp(io::stdin()).map_err(|error| {
            io::Error::other(format!(
                "internal terminal restore inspection failed: {error}"
            ))
        })?;
        if foreground == original_pgid {
            return Ok(());
        }
        if foreground != control_pgid {
            return Err(io::Error::other(
                "internal terminal restore refused an unrelated foreground process group",
            ));
        }
        tcsetpgrp(io::stdin(), original_pgid).map_err(|error| {
            io::Error::other(format!(
                "internal terminal foreground restore failed: {error}"
            ))
        })?;
        if tcgetpgrp(io::stdin()).map_err(|error| {
            io::Error::other(format!(
                "internal terminal restore verification failed: {error}"
            ))
        })? != original_pgid
        {
            return Err(io::Error::other(
                "internal terminal foreground restore was not retained",
            ));
        }
        Ok(())
    })
}

fn parse_saved_terminal(contents: &str) -> io::Result<SavedTerminal> {
    let mut lines = contents.split_inclusive('\n');
    let readiness = lines.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "internal terminal marker is empty",
        )
    })?;
    if let Some(completion) = lines.next() {
        let completion = completion
            .strip_prefix("complete ")
            .and_then(|value| value.strip_suffix('\n'))
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|status| *status <= u8::MAX.into());
        if completion.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "internal terminal completion marker is malformed",
            ));
        }
    }
    if lines.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "internal terminal marker has unexpected records",
        ));
    }
    if readiness == "ready none\n" {
        return Ok(SavedTerminal::None);
    }
    let line = readiness.strip_suffix('\n').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "internal terminal marker is not newline terminated",
        )
    })?;
    let fields = line.split(' ').collect::<Vec<_>>();
    if fields.len() != 4 || fields[0] != "ready" || fields[1] != "tty" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "internal terminal marker is malformed",
        ));
    }
    let parse_pgid = |value: &str| {
        value
            .parse::<i32>()
            .ok()
            .filter(|pgid| *pgid > 0)
            .map(Pid::from_raw)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "internal terminal marker process group is malformed",
                )
            })
    };
    Ok(SavedTerminal::Foreground {
        original_pgid: parse_pgid(fields[2])?,
        control_pgid: parse_pgid(fields[3])?,
    })
}

fn restore_internal_terminal(marker: &Path) -> io::Result<()> {
    let mut marker_file = validate_internal_marker(marker, false)?;
    let mut contents = String::new();
    Read::by_ref(&mut marker_file)
        .take(INTERNAL_MARKER_MAX_BYTES + 1)
        .read_to_string(&mut contents)?;
    if contents.len() as u64 != marker_file.metadata()?.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "internal terminal marker length changed",
        ));
    }
    match parse_saved_terminal(&contents)? {
        SavedTerminal::None => Ok(()),
        SavedTerminal::Foreground {
            original_pgid,
            control_pgid,
        } => {
            let parent_pgid = getpgid(Some(getppid())).map_err(|error| {
                io::Error::other(format!(
                    "internal terminal restore parent inspection failed: {error}"
                ))
            })?;
            if parent_pgid != original_pgid || getpgrp() != original_pgid {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "internal terminal restore parent process group is invalid",
                ));
            }
            restore_terminal_foreground(original_pgid, control_pgid)
        }
    }
}

fn foreground_internal_terminal(marker: &Path) -> io::Result<()> {
    let mut marker_file = validate_internal_marker(marker, false)?;
    let mut contents = String::new();
    Read::by_ref(&mut marker_file)
        .take(INTERNAL_MARKER_MAX_BYTES + 1)
        .read_to_string(&mut contents)?;
    if contents.len() as u64 != marker_file.metadata()?.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "internal terminal marker length changed",
        ));
    }
    match parse_saved_terminal(&contents)? {
        SavedTerminal::None => Ok(()),
        SavedTerminal::Foreground {
            original_pgid,
            control_pgid,
        } => {
            let parent_pgid = getpgid(Some(getppid())).map_err(|error| {
                io::Error::other(format!(
                    "internal terminal foreground parent inspection failed: {error}"
                ))
            })?;
            if parent_pgid != original_pgid || getpgrp() != original_pgid {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "internal terminal foreground parent process group is invalid",
                ));
            }
            with_sigttou_blocked(|| {
                let foreground = tcgetpgrp(io::stdin()).map_err(|error| {
                    io::Error::other(format!(
                        "internal terminal foreground inspection failed: {error}"
                    ))
                })?;
                if foreground == control_pgid {
                    return Ok(());
                }
                if foreground != original_pgid {
                    return Err(io::Error::other(
                        "internal terminal foreground refused an unrelated process group",
                    ));
                }
                tcsetpgrp(io::stdin(), control_pgid).map_err(|error| {
                    io::Error::other(format!(
                        "internal terminal foreground handoff failed: {error}"
                    ))
                })?;
                if tcgetpgrp(io::stdin()).map_err(|error| {
                    io::Error::other(format!(
                        "internal terminal foreground verification failed: {error}"
                    ))
                })? != control_pgid
                {
                    return Err(io::Error::other(
                        "internal terminal foreground handoff was not retained",
                    ));
                }
                Ok(())
            })
        }
    }
}

fn await_internal_continue(continue_file: &mut fs::File) -> io::Result<()> {
    const EXPECTED: &[u8] = b"continue\n";
    let deadline = std::time::Instant::now() + INTERNAL_CONTINUE_TIMEOUT;
    loop {
        continue_file.seek(SeekFrom::Start(0))?;
        let mut contents = Vec::new();
        Read::by_ref(continue_file)
            .take(EXPECTED.len() as u64 + 1)
            .read_to_end(&mut contents)?;
        if contents == EXPECTED {
            return Ok(());
        }
        if !EXPECTED.starts_with(&contents) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "internal continue marker is malformed",
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "internal continue acknowledgement timed out",
            ));
        }
        std::thread::sleep(INTERNAL_CONTINUE_POLL_INTERVAL);
    }
}

fn enter_internal_bootstrap(
    marker: &Path,
    continue_marker: &Path,
) -> io::Result<InternalBootstrap> {
    let mut marker_file = validate_internal_marker(marker, true)?;
    let mut continue_file =
        validate_internal_private_file(continue_marker, INTERNAL_CONTINUE_MARKER, true)?;
    let inherited_terminal = inherited_foreground_terminal()?;
    let pid = getpid();
    setpgid(Pid::from_raw(0), Pid::from_raw(0))
        .map_err(|error| io::Error::other(format!("internal bootstrap setpgid failed: {error}")))?;
    if getpgrp() != pid {
        return Err(io::Error::other(
            "internal bootstrap process group identity is invalid",
        ));
    }
    if inherited_terminal.is_some() {
        install_terminal_signal_relay(getppid())?;
    }
    let marker_contents = inherited_terminal.map_or_else(
        || "ready none\n".to_owned(),
        |original_pgid| format!("ready tty {} {}\n", original_pgid.as_raw(), pid.as_raw()),
    );
    marker_file.write_all(marker_contents.as_bytes())?;
    marker_file.sync_all()?;
    raise(Signal::SIGSTOP)
        .map_err(|error| io::Error::other(format!("internal bootstrap stop failed: {error}")))?;
    await_internal_continue(&mut continue_file)?;
    let terminal_guard = inherited_terminal
        .map(|original_pgid| hand_off_terminal_foreground(original_pgid, pid))
        .transpose()?;
    Ok(InternalBootstrap {
        marker_file,
        terminal_guard,
    })
}

fn run_lifecycle_target(
    config: InstallConfig,
    operation: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    let target = config
        .target
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target is required"))?
        .parse::<SshTarget>()?;
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "an absolute home directory is required",
            )
        })?;
    let transport =
        OpenSshTransport::system(TransportConfig::new(home.join(".ssh").join("known_hosts"))?);
    let observed = transport.probe(&target)?.identity;
    let cache_root = home.join(".cache").join("planeradar");
    ensure_private_cache_root(&cache_root)?;
    let backend = SystemLifecycleBackend {
        transport,
        target: RefCell::new(target),
        expected_identity: observed,
        release_dir: config.release_dir,
        cache_root,
        lock: DriverLock::checked_in()?,
        verified_payloads: RefCell::new(BTreeMap::new()),
        candidate: RefCell::new(None),
        management_helper: RefCell::new(None),
        uninstall_helper: RefCell::new(None),
        staged_artifact: RefCell::new(None),
        last_owned_files: RefCell::new(None),
        driver_tool: RefCell::new(None),
        protocol_tool: RefCell::new(None),
        retained_driver_transition: Cell::new(false),
        #[cfg(test)]
        driver_protocol_actions: RefCell::new(None),
    };
    let manager = LifecycleManager::new(&backend);
    let outcome = match operation {
        "upgrade" => manager.upgrade(config.version.as_ref())?,
        "rollback" => manager.rollback(config.version.as_ref())?,
        "uninstall" => manager.uninstall(config.purge_settings)?,
        _ => unreachable!("known lifecycle operation"),
    };
    println!("{outcome}");
    Ok(())
}

#[cfg(test)]
type DriverProtocolActions = std::rc::Rc<RefCell<Vec<(DriverAction, Option<String>)>>>;

struct SystemLifecycleBackend<R = SystemCommandRunner, C = SystemClock> {
    transport: OpenSshTransport<R, C>,
    target: RefCell<SshTarget>,
    expected_identity: TargetIdentity,
    release_dir: Option<PathBuf>,
    cache_root: PathBuf,
    lock: DriverLock,
    verified_payloads: RefCell<BTreeMap<String, (ReleasePair, ApplicationPayload)>>,
    candidate: RefCell<Option<ReleasePair>>,
    management_helper: RefCell<Option<ManagementHelper>>,
    uninstall_helper: RefCell<Option<String>>,
    staged_artifact: RefCell<Option<OwnedFile>>,
    last_owned_files: RefCell<Option<Vec<OwnedFile>>>,
    driver_tool: RefCell<Option<(ArtifactIdentity, DriverTool<SystemCommandRunner>)>>,
    protocol_tool: RefCell<Option<DriverTool<SystemCommandRunner>>>,
    retained_driver_transition: Cell<bool>,
    #[cfg(test)]
    driver_protocol_actions: RefCell<Option<DriverProtocolActions>>,
}

impl<R: TransportCommandRunner, C: Clock> SystemLifecycleBackend<R, C> {
    fn verified_payload_key(pair: &ReleasePair) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            pair.application.version,
            pair.application.source_commit,
            pair.application.sha256,
            pair.driver.version,
            pair.driver.source_commit,
            pair.driver.sha256
        )
    }

    fn target(&self) -> SshTarget {
        self.target.borrow().clone()
    }

    fn run_remote(
        &self,
        command: RemoteCommand,
    ) -> Result<planeradarctl::transport::CommandOutput, LifecycleError> {
        self.transport
            .run_bounded(&self.target(), command, Duration::from_secs(30), 64 * 1024)
            .map_err(|_| LifecycleError::Backend)
    }

    fn management_helper_path(&self) -> Result<String, LifecycleError> {
        self.management_helper
            .borrow()
            .as_ref()
            .filter(|helper| helper.protocol == MANAGEMENT_HELPER_PROTOCOL)
            .map(|helper| helper.target_path.clone())
            .ok_or(LifecycleError::ManagementHelperRequired)
    }

    fn command_helper_path(&self) -> Result<String, LifecycleError> {
        if let Ok(helper) = self.management_helper_path() {
            return Ok(helper);
        }
        self.uninstall_helper
            .borrow()
            .clone()
            .ok_or(LifecycleError::ManagementHelperRequired)
    }

    fn read_private_state_file(&self, path: &str) -> Result<Option<Vec<u8>>, LifecycleError> {
        let command = private_state_read_command(path)?;
        let output = self.run_remote(command)?;
        if matches!(output.stdout(), b"null" | b"null\n") {
            return Ok(None);
        }
        Ok(Some(output.stdout().to_vec()))
    }

    fn read_private_lifecycle_state(&self) -> Result<Option<LifecycleState>, LifecycleError> {
        let Some(bytes) =
            self.read_private_state_file("/var/lib/planeradar-installer/lifecycle.json")?
        else {
            return Ok(None);
        };
        let state = LifecycleState::from_json(&bytes)?;
        if state.hardware().model != self.expected_identity.model
            || state.hardware().serial != self.expected_identity.serial
        {
            return Err(LifecycleError::InvalidState);
        }
        Ok(Some(state))
    }

    fn verify_recovery_helper(&self, helper: &OwnedFile) -> Result<(), LifecycleError> {
        let command = RemoteCommand::interactive_sudo([
            "sudo",
            "sh",
            "-c",
            "set -eu; p=$1; expected=$2; test ! -L \"$p\" && test -f \"$p\" && test \"$(stat -c '%u:%g:%a:%h' -- \"$p\")\" = '0:0:700:1' && test \"$(sha256sum -- \"$p\" | awk '{print $1}')\" = \"$expected\"",
            "planeradar-recovery-helper",
            helper.target_path.as_str(),
            helper.sha256.as_str(),
        ])
        .map_err(|_| LifecycleError::Backend)?;
        self.run_remote(command)?;
        Ok(())
    }

    fn verify_management_helper(&self, helper: &ManagementHelper) -> Result<(), LifecycleError> {
        if helper.protocol != MANAGEMENT_HELPER_PROTOCOL {
            return Err(LifecycleError::InvalidState);
        }
        self.verify_recovery_helper(&OwnedFile {
            target_path: helper.target_path.clone(),
            sha256: helper.application.sha256.clone(),
        })?;
        let command = RemoteCommand::interactive_sudo([
            "sudo",
            helper.target_path.as_str(),
            "lifecycle-protocol",
        ])
        .map_err(|_| LifecycleError::Backend)?;
        let output = self.run_remote(command)?;
        if output.stdout() != format!("{MANAGEMENT_HELPER_PROTOCOL}\n").as_bytes() {
            return Err(LifecycleError::Backend);
        }
        Ok(())
    }

    fn latest_release_version(&self) -> Result<Version, LifecycleError> {
        planeradarctl::release::GhLatestStableReleaseResolver::new(SystemCommandRunner)
            .resolve()
            .map_err(|_| LifecycleError::Backend)
    }

    fn release_version(&self, requested: Option<&Version>) -> Result<Version, LifecycleError> {
        if let Some(version) = requested {
            return Ok(version.clone());
        }
        if let Some(directory) = &self.release_dir {
            let bytes =
                fs::read(directory.join(MANIFEST_NAME)).map_err(|_| LifecycleError::Backend)?;
            if bytes.len() > planeradarctl::release::MAX_MANIFEST_BYTES {
                return Err(LifecycleError::Backend);
            }
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| LifecycleError::Backend)?;
            let version = value
                .get("version")
                .and_then(serde_json::Value::as_str)
                .ok_or(LifecycleError::Backend)?;
            return Version::parse(version).map_err(|_| LifecycleError::Backend);
        }
        self.latest_release_version()
    }

    fn verify_release_with_lock(
        &self,
        version: &Version,
        driver_lock: &DriverLock,
    ) -> Result<(ReleasePair, ApplicationPayload), LifecycleError> {
        let input = self
            .release_dir
            .as_deref()
            .map_or(ReleaseInput::Downloaded, ReleaseInput::Local);
        let release =
            ReleaseClient::new(GhReleaseSource::system(), self.cache_root.join("release"))
                .resolve(version, driver_lock, input)
                .map_err(|_| LifecycleError::ImmutableReleaseMismatch)?;
        Verifier::new(SystemCommandRunner)
            .verify(version, &release)
            .map_err(|_| LifecycleError::ImmutableReleaseMismatch)?;
        let artifact = release
            .artifacts
            .iter()
            .find(|artifact| artifact.artifact.name == "planeradar-aarch64-linux-gnu.tar.zst")
            .ok_or(LifecycleError::ImmutableReleaseMismatch)?;
        let payload = extract_application_payload_at_mtime(
            &artifact.path,
            &artifact.artifact.sha256,
            &self.cache_root.join("payloads"),
            release.manifest.source_date_epoch,
        )
        .map_err(|_| LifecycleError::ImmutableReleaseMismatch)?;
        let pair = ReleasePair {
            application: ArtifactIdentity {
                version: release.manifest.version.to_string(),
                source_commit: release.manifest.source_commit,
                sha256: payload.sha256().into(),
            },
            driver: ArtifactIdentity {
                version: release.manifest.driver.version.to_string(),
                source_commit: release.manifest.driver.commit,
                sha256: release.manifest.driver.manifest_sha256,
            },
        };
        Ok((pair, payload))
    }

    fn ensure_driver_tool(&self, identity: &ArtifactIdentity) -> Result<(), LifecycleError> {
        if self
            .driver_tool
            .borrow()
            .as_ref()
            .is_some_and(|(current, _)| current == identity)
        {
            return Ok(());
        }
        let lock = DriverLock {
            repository: planeradarctl::config::DRIVER_REPOSITORY.into(),
            version: Version::parse(&identity.version).map_err(|_| LifecycleError::Backend)?,
            commit: identity.source_commit.clone(),
            manifest_sha256: identity.sha256.clone(),
        };
        let facts = TargetPreflight::new(&self.transport, SystemUnixClock)
            .facts(&self.target())
            .map_err(|_| LifecycleError::Backend)?;
        let probe = DriverTargetProbe::new(facts.kernel_release.clone(), facts.kernel_vermagic)
            .map_err(|_| LifecycleError::Backend)?;
        let synced = DriverManager::new(
            GhDriverReleaseSource::system(),
            GhDriverReleaseVerifier::system(),
            self.cache_root.join("driver"),
        )
        .sync(&lock)
        .map_err(|_| LifecycleError::Backend)?;
        let tool = synced
            .tool(
                SystemCommandRunner,
                &probe,
                DriverContext {
                    target: self.target().ssh_destination(),
                    kernel_release: facts.kernel_release.clone(),
                    kernel_export: self
                        .cache_root
                        .join("kernel-export")
                        .join(&facts.kernel_release),
                    artifacts: self.cache_root.join("driver-artifacts"),
                    replace_overlay: facts.replace_overlay.clone(),
                },
            )
            .map_err(|_| LifecycleError::Backend)?;
        *self.driver_tool.borrow_mut() = Some((identity.clone(), tool));
        Ok(())
    }

    fn ensure_protocol_tool(&self) -> Result<(), LifecycleError> {
        if self.protocol_tool.borrow().is_some() {
            return Ok(());
        }
        let facts = TargetPreflight::new(&self.transport, SystemUnixClock)
            .facts(&self.target())
            .map_err(|_| LifecycleError::Backend)?;
        let probe = DriverTargetProbe::new(facts.kernel_release.clone(), facts.kernel_vermagic)
            .map_err(|_| LifecycleError::Backend)?;
        let synced = DriverManager::new(
            GhDriverReleaseSource::system(),
            GhDriverReleaseVerifier::system(),
            self.cache_root.join("driver-protocol"),
        )
        .sync(&self.lock)
        .map_err(|_| LifecycleError::Backend)?;
        let tool = synced
            .tool(
                SystemCommandRunner,
                &probe,
                DriverContext {
                    target: self.target().ssh_destination(),
                    kernel_release: facts.kernel_release.clone(),
                    kernel_export: self
                        .cache_root
                        .join("kernel-export")
                        .join(&facts.kernel_release),
                    artifacts: self.cache_root.join("driver-artifacts"),
                    replace_overlay: facts.replace_overlay.clone(),
                },
            )
            .map_err(|_| LifecycleError::Backend)?;
        *self.protocol_tool.borrow_mut() = Some(tool);
        Ok(())
    }

    fn run_driver_protocol(
        &self,
        action: DriverAction,
        identity: Option<&ArtifactIdentity>,
    ) -> Result<(), LifecycleError> {
        #[cfg(test)]
        if let Some(actions) = self.driver_protocol_actions.borrow().as_ref() {
            actions.borrow_mut().push((
                action,
                identity.map(|identity| identity.source_commit.clone()),
            ));
            return Ok(());
        }
        self.ensure_protocol_tool()?;
        self.protocol_tool
            .borrow()
            .as_ref()
            .ok_or(LifecycleError::Backend)?
            .run_accepted_protocol(
                action,
                identity.map(|identity| identity.source_commit.as_str()),
            )
            .map_err(|_| LifecycleError::Backend)
    }

    fn reboot_and_reconnect(&self, tryboot: bool) -> Result<(), LifecycleError> {
        let original = self.target();
        let command = if tryboot {
            tryboot_reboot_command()
        } else {
            final_reboot_command()
        }
        .map_err(|_| LifecycleError::Backend)?;
        match self
            .transport
            .run_bounded(&original, command, Duration::from_secs(30), 4 * 1024)
        {
            Ok(_) | Err(TransportError::CommandFailed) => {}
            Err(_) => return Err(LifecycleError::Backend),
        }
        let policy = ReconnectPolicy::new(
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(1),
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .map_err(|_| LifecycleError::Backend)?;
        let target = self
            .transport
            .wait_for_reboot(
                &self.expected_identity,
                std::slice::from_ref(&original),
                policy,
            )
            .map_err(|_| LifecycleError::Backend)?;
        *self.target.borrow_mut() = target;
        *self.driver_tool.borrow_mut() = None;
        *self.protocol_tool.borrow_mut() = None;
        Ok(())
    }

    fn ownership_json(files: &[OwnedFile]) -> Result<String, LifecycleError> {
        #[derive(serde::Serialize)]
        struct Ownership<'a> {
            schema_version: u32,
            owned_files: &'a [OwnedFile],
        }
        serde_json::to_string(&Ownership {
            schema_version: 1,
            owned_files: files,
        })
        .map_err(|_| LifecycleError::InvalidOwnership)
    }

    fn parse_ownership(bytes: &[u8]) -> Result<Vec<OwnedFile>, LifecycleError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Ownership {
            schema_version: u32,
            owned_files: Vec<OwnedFile>,
        }
        if bytes.len() > 64 * 1024 {
            return Err(LifecycleError::InvalidOwnership);
        }
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let ownership = Ownership::deserialize(&mut deserializer)
            .map_err(|_| LifecycleError::InvalidOwnership)?;
        deserializer
            .end()
            .map_err(|_| LifecycleError::InvalidOwnership)?;
        if ownership.schema_version != 1 || ownership.owned_files.is_empty() {
            return Err(LifecycleError::InvalidOwnership);
        }
        let state = LifecycleState::installed(
            TargetHardwareIdentity {
                model: "Raspberry Pi Zero 2 W".into(),
                serial: "0000000000000000".into(),
            },
            vec![AcceptedPair {
                pair: ReleasePair {
                    application: ArtifactIdentity {
                        version: "0.0.0".into(),
                        source_commit: "0".repeat(40),
                        sha256: "0".repeat(64),
                    },
                    driver: ArtifactIdentity {
                        version: "0.0.0".into(),
                        source_commit: "0".repeat(40),
                        sha256: "0".repeat(64),
                    },
                },
                sequence: 1,
                owned_files: ownership.owned_files.clone(),
            }],
        );
        if state.is_err() {
            return Err(LifecycleError::InvalidOwnership);
        }
        Ok(ownership.owned_files)
    }

    fn activate_pair(
        &self,
        pair: &ReleasePair,
        current_owned: &[OwnedFile],
    ) -> Result<Vec<OwnedFile>, LifecycleError> {
        let artifact = self
            .staged_artifact
            .borrow()
            .as_ref()
            .filter(|artifact| artifact.sha256 == pair.application.sha256)
            .map(|artifact| artifact.target_path.clone())
            .unwrap_or_else(|| {
                format!(
                    "/opt/planeradar/releases/{}/{}/planeradar",
                    pair.application.version, pair.application.sha256
                )
            });
        self.activate_artifact(pair, current_owned, &artifact)
    }

    fn deploy_management_helper(
        &self,
        pair: &ReleasePair,
    ) -> Result<ManagementHelper, LifecycleError> {
        let payloads = self.verified_payloads.borrow();
        let (_, payload) = payloads
            .get(&Self::verified_payload_key(pair))
            .filter(|(verified, _)| verified == pair)
            .ok_or(LifecycleError::ImmutableReleaseMismatch)?;
        let create = RemoteCommand::ordinary([
            "sh",
            "-c",
            "umask 077; mktemp -d /var/tmp/planeradar-upload.XXXXXXXXXX",
        ])
        .map_err(|_| LifecycleError::Backend)?;
        let output = self.run_remote(create)?;
        let directory = std::str::from_utf8(output.stdout())
            .ok()
            .map(str::trim)
            .filter(|path| path.starts_with("/var/tmp/planeradar-upload."))
            .ok_or(LifecycleError::Backend)?
            .to_owned();
        let upload = format!("{directory}/payload");
        self.transport
            .copy_to(&self.target(), payload.path(), Path::new(&upload))
            .map_err(|_| LifecycleError::Backend)?;
        let helper = format!(
            "/var/lib/planeradar-installer/helpers/{}/planeradar",
            pair.application.sha256
        );
        let deploy = deploy_helper_command(
            &upload,
            &helper,
            &pair.application.sha256,
            &pair.application.source_commit,
        )
        .map_err(|_| LifecycleError::Backend)?;
        let deployed = self.run_remote(deploy);
        let cleanup = RemoteCommand::ordinary(["rm", "-rf", "--", directory.as_str()])
            .map_err(|_| LifecycleError::Backend)?;
        let cleaned = self.run_remote(cleanup);
        deployed?;
        cleaned?;
        let owned = OwnedFile {
            target_path: helper.clone(),
            sha256: pair.application.sha256.clone(),
        };
        self.verify_recovery_helper(&owned)?;
        let helper = ManagementHelper {
            application: pair.application.clone(),
            target_path: helper,
            protocol: MANAGEMENT_HELPER_PROTOCOL.into(),
        };
        self.verify_management_helper(&helper)?;
        *self.management_helper.borrow_mut() = Some(helper.clone());
        Ok(helper)
    }

    fn recover_management_helper(
        &self,
        transaction: &planeradarctl::operations::LifecycleTransaction,
    ) -> Result<(), LifecycleError> {
        let expected = &transaction.management_helper;
        if self
            .management_helper
            .borrow()
            .as_ref()
            .is_some_and(|helper| helper == expected)
            && self.verify_management_helper(expected).is_ok()
        {
            return Ok(());
        }
        let pair = if transaction.candidate.application == expected.application {
            &transaction.candidate
        } else if transaction.prior.pair.application == expected.application {
            &transaction.prior.pair
        } else {
            return Err(LifecycleError::InvalidState);
        };
        if !self
            .verified_payloads
            .borrow()
            .get(&Self::verified_payload_key(pair))
            .is_some_and(|(verified, _)| verified == pair)
        {
            self.verify_historical_release(pair)?;
        }
        let recovered = self.deploy_management_helper(pair)?;
        if &recovered != expected {
            return Err(LifecycleError::InvalidState);
        }
        Ok(())
    }

    fn deploy_candidate_artifact(&self, pair: &ReleasePair) -> Result<OwnedFile, LifecycleError> {
        let payloads = self.verified_payloads.borrow();
        let (_, payload) = payloads
            .get(&Self::verified_payload_key(pair))
            .filter(|(verified, _)| verified == pair)
            .ok_or(LifecycleError::ImmutableReleaseMismatch)?;
        let create = RemoteCommand::ordinary([
            "sh",
            "-c",
            "umask 077; mktemp -d /var/tmp/planeradar-upload.XXXXXXXXXX",
        ])
        .map_err(|_| LifecycleError::Backend)?;
        let output = self.run_remote(create)?;
        let directory = std::str::from_utf8(output.stdout())
            .ok()
            .map(str::trim)
            .filter(|path| path.starts_with("/var/tmp/planeradar-upload."))
            .ok_or(LifecycleError::Backend)?
            .to_owned();
        let upload = format!("{directory}/payload");
        self.transport
            .copy_to(&self.target(), payload.path(), Path::new(&upload))
            .map_err(|_| LifecycleError::Backend)?;
        let artifact = OwnedFile {
            target_path: format!(
                "/opt/planeradar/releases/{}/{}/planeradar",
                pair.application.version, pair.application.sha256
            ),
            sha256: pair.application.sha256.clone(),
        };
        let deploy = deploy_candidate_artifact_command(
            &upload,
            &artifact.target_path,
            &pair.application.version,
            &artifact.sha256,
        )
        .map_err(|_| LifecycleError::Backend)?;
        let deployed = self.run_remote(deploy);
        let cleanup = RemoteCommand::ordinary(["rm", "-rf", "--", directory.as_str()])
            .map_err(|_| LifecycleError::Backend)?;
        let cleaned = self.run_remote(cleanup);
        deployed?;
        cleaned?;
        *self.staged_artifact.borrow_mut() = Some(artifact.clone());
        Ok(artifact)
    }

    fn activate_artifact(
        &self,
        pair: &ReleasePair,
        current_owned: &[OwnedFile],
        artifact: &str,
    ) -> Result<Vec<OwnedFile>, LifecycleError> {
        let helper = self.management_helper_path()?;
        let owned_json = Self::ownership_json(current_owned)?;
        let command = RemoteCommand::interactive_sudo([
            "sudo",
            helper.as_str(),
            "lifecycle-activate",
            "--artifact",
            artifact,
            "--version",
            pair.application.version.as_str(),
            "--revision",
            pair.application.source_commit.as_str(),
            "--sha256",
            pair.application.sha256.as_str(),
            "--owned-json",
            owned_json.as_str(),
        ])
        .map_err(|_| LifecycleError::Backend)?;
        let output = self.run_remote(command)?;
        Self::parse_ownership(output.stdout())
    }

    fn verify_application(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
        let command = RemoteCommand::ordinary([
            "sh",
            "-c",
            "test ! -L /opt/planeradar/bin/planeradar && test -x /opt/planeradar/bin/planeradar && test \"$(sha256sum -- /opt/planeradar/bin/planeradar | awk '{print $1}')\" = \"$1\" && test \"$(tr -d '\\r\\n' </opt/planeradar/REVISION)\" = \"$2\" && systemctl is-enabled --quiet planeradar.service && systemctl is-active --quiet planeradar.service && hostname=$(tr -d '\\r\\n' </etc/hostname) && curl --fail --silent --show-error --max-time 5 --max-filesize 4096 -H \"Host: $hostname.local\" http://127.0.0.1/healthz >/dev/null",
            "planeradar-lifecycle-health",
            pair.application.sha256.as_str(),
            pair.application.source_commit.as_str(),
        ])
        .map_err(|_| LifecycleError::Backend)?;
        self.run_remote(command)?;
        Ok(())
    }
}

impl<R: TransportCommandRunner, C: Clock> LifecycleBackend for SystemLifecycleBackend<R, C> {
    fn load_lifecycle_state(&self) -> Result<LifecycleState, LifecycleError> {
        if let Some(state) = self.read_private_lifecycle_state()? {
            if let Some(transaction) = state.transaction() {
                self.recover_management_helper(transaction)?;
            } else if let Some(uninstall) = state.uninstall_transaction() {
                if uninstall.phase != UninstallPhase::DriverRemoved {
                    self.verify_recovery_helper(&uninstall.recovery_helper)?;
                }
                *self.uninstall_helper.borrow_mut() =
                    Some(uninstall.recovery_helper.target_path.clone());
            } else {
                for accepted in state.accepted() {
                    self.retire_recovery_helper(&accepted.pair.application)?;
                }
            }
            return Ok(state);
        }

        let legacy = self.read_private_state_file("/var/lib/planeradar-installer/state.json")?;
        if legacy.is_some() && self.management_helper.borrow().is_none() {
            return Err(LifecycleError::ManagementHelperRequired);
        }
        let mut deserializer =
            serde_json::Deserializer::from_slice(legacy.as_deref().unwrap_or(b"null"));
        let state = Option::<TargetInstallState>::deserialize(&mut deserializer)
            .map_err(|_| LifecycleError::InvalidState)?;
        deserializer
            .end()
            .map_err(|_| LifecycleError::InvalidState)?;
        if let Some(state) = state {
            let migrated = LifecycleState::migrate_task14(&state)?;
            if migrated.hardware().model != self.expected_identity.model
                || migrated.hardware().serial != self.expected_identity.serial
            {
                return Err(LifecycleError::InvalidState);
            }
            let expected = &migrated
                .accepted()
                .first()
                .ok_or(LifecycleError::InvalidState)?
                .pair;
            self.verify_historical_release(expected)?;
            self.run_driver_protocol(
                DriverAction::RecordAccepted,
                Some(&migrated.accepted()[0].pair.driver),
            )?;
            self.save_lifecycle_state(&migrated)?;
            return Ok(migrated);
        }
        LifecycleState::empty(TargetHardwareIdentity {
            model: self.expected_identity.model.clone(),
            serial: self.expected_identity.serial.clone(),
        })
    }

    fn save_lifecycle_state(&self, state: &LifecycleState) -> Result<(), LifecycleError> {
        if let Some(transaction) = state.transaction()
            && self.management_helper.borrow().as_ref() != Some(&transaction.management_helper)
        {
            return Err(LifecycleError::ManagementHelperRequired);
        }
        let helper = self.command_helper_path()?;
        if let Some(current) = state.accepted().first() {
            let target_state = TargetInstallState {
                schema_version: 1,
                hardware: state.hardware().clone(),
                application: Some(current.pair.application.clone()),
                driver: Some(current.pair.driver.clone()),
                owned_files: current.owned_files.clone(),
                last_verified_phase: InstallPhase::Complete,
            };
            let target_json = target_state
                .to_json()
                .map_err(|_| LifecycleError::InvalidState)?;
            let command = RemoteCommand::interactive_sudo([
                "sudo",
                helper.as_str(),
                "installer-state",
                "write",
                "--json",
                target_json.as_str(),
            ])
            .map_err(|_| LifecycleError::Backend)?;
            self.run_remote(command)?;
        }
        // lifecycle.json is the commit record.  Compatibility state is written
        // first so an error can never report failure after the new lifecycle
        // pair has already become durable.
        let json = state.to_json()?;
        let command = RemoteCommand::interactive_sudo([
            "sudo",
            helper.as_str(),
            "lifecycle-state",
            "write",
            "--json",
            json.as_str(),
        ])
        .map_err(|_| LifecycleError::Backend)?;
        let output = self.run_remote(command)?;
        let returned = LifecycleState::from_json(output.stdout())?;
        if &returned != state {
            return Err(LifecycleError::InvalidState);
        }
        Ok(())
    }

    fn resolve_release(&self, requested: Option<&Version>) -> Result<ReleasePair, LifecycleError> {
        let version = self.release_version(requested)?;
        let (pair, payload) = self.verify_release_with_lock(&version, &self.lock)?;
        self.verified_payloads
            .borrow_mut()
            .insert(Self::verified_payload_key(&pair), (pair.clone(), payload));
        *self.candidate.borrow_mut() = Some(pair.clone());
        Ok(pair)
    }

    fn verify_historical_release(&self, expected: &ReleasePair) -> Result<(), LifecycleError> {
        if self
            .verified_payloads
            .borrow()
            .get(&Self::verified_payload_key(expected))
            .is_some_and(|(verified, _)| verified == expected)
        {
            return Ok(());
        }
        let version = Version::parse(&expected.application.version)
            .map_err(|_| LifecycleError::ImmutableReleaseMismatch)?;
        let lock = DriverLock {
            repository: planeradarctl::config::DRIVER_REPOSITORY.into(),
            version: Version::parse(&expected.driver.version)
                .map_err(|_| LifecycleError::ImmutableReleaseMismatch)?,
            commit: expected.driver.source_commit.clone(),
            manifest_sha256: expected.driver.sha256.clone(),
        };
        let (verified, payload) = self.verify_release_with_lock(&version, &lock)?;
        if &verified != expected {
            return Err(LifecycleError::ImmutableReleaseMismatch);
        }
        self.verified_payloads
            .borrow_mut()
            .insert(Self::verified_payload_key(&verified), (verified, payload));
        Ok(())
    }

    fn prepare_management_helper(
        &self,
        pair: &ReleasePair,
    ) -> Result<ManagementHelper, LifecycleError> {
        if !self
            .verified_payloads
            .borrow()
            .get(&Self::verified_payload_key(pair))
            .is_some_and(|(verified, _)| verified == pair)
        {
            self.verify_historical_release(pair)?;
        }
        self.deploy_management_helper(pair)
    }

    fn retire_management_helper(&self, helper: &ManagementHelper) -> Result<(), LifecycleError> {
        let command = retire_lifecycle_helper_command(
            &helper.target_path,
            &helper.application.sha256,
            &helper.application.source_commit,
        )
        .map_err(|_| LifecycleError::Backend)?;
        self.run_remote(command)?;
        let helper_is_active = self.management_helper.borrow().as_ref() == Some(helper);
        if helper_is_active {
            *self.management_helper.borrow_mut() = None;
        }
        Ok(())
    }

    fn stage_application(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
        if !self
            .verified_payloads
            .borrow()
            .get(&Self::verified_payload_key(pair))
            .is_some_and(|(verified, _)| verified == pair)
        {
            return Err(LifecycleError::ImmutableReleaseMismatch);
        }
        let state = self.load_lifecycle_state()?;
        let current = state
            .accepted()
            .first()
            .ok_or(LifecycleError::NoAcceptedPair)?;
        let preserved = self.activate_artifact(
            &current.pair,
            &current.owned_files,
            "/opt/planeradar/bin/planeradar",
        )?;
        *self.last_owned_files.borrow_mut() = Some(preserved);
        self.deploy_candidate_artifact(pair)?;
        Ok(())
    }

    fn stage_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
        let state = self.load_lifecycle_state()?;
        let current = state
            .accepted()
            .first()
            .ok_or(LifecycleError::NoAcceptedPair)?;
        self.run_driver_protocol(DriverAction::RecordAccepted, Some(&current.pair.driver))?;
        let retained = state
            .accepted()
            .iter()
            .skip(1)
            .any(|accepted| accepted.pair.driver == pair.driver);
        self.retained_driver_transition.set(retained);
        if retained {
            return self.run_driver_protocol(DriverAction::StageRetained, Some(&pair.driver));
        }
        self.ensure_driver_tool(&pair.driver)?;
        let postconditions = {
            let driver = self.driver_tool.borrow();
            let tool = &driver.as_ref().ok_or(LifecycleError::Backend)?.1;
            tool.prepare_artifacts()
                .map_err(|_| LifecycleError::Backend)?;
            tool.postconditions().map_err(|_| LifecycleError::Backend)?
        };
        self.ensure_protocol_tool()?;
        self.protocol_tool
            .borrow()
            .as_ref()
            .ok_or(LifecycleError::Backend)?
            .prepare_accepted_protocol(&postconditions)
            .map_err(|_| LifecycleError::Backend)?;
        self.driver_tool
            .borrow()
            .as_ref()
            .ok_or(LifecycleError::Backend)?
            .1
            .stage_prepared()
            .map_err(|_| LifecycleError::Backend)
    }

    fn tryboot_driver(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.reboot_and_reconnect(true)
    }

    fn verify_tryboot_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.ensure_driver_tool(&pair.driver)?;
        self.driver_tool
            .borrow()
            .as_ref()
            .ok_or(LifecycleError::Backend)?
            .1
            .run(DriverAction::VerifyBoot)
            .map(|_| ())
            .map_err(|_| LifecycleError::Backend)
    }

    fn commit_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
        if self.retained_driver_transition.get() {
            return self.run_driver_protocol(DriverAction::CommitRetained, None);
        }
        self.ensure_driver_tool(&pair.driver)?;
        self.driver_tool
            .borrow()
            .as_ref()
            .ok_or(LifecycleError::Backend)?
            .1
            .run(DriverAction::CommitBoot)
            .map(|_| ())
            .map_err(|_| LifecycleError::Backend)?;
        self.run_driver_protocol(DriverAction::MarkCommittedAccepted, None)
    }

    fn reboot_normal(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.reboot_and_reconnect(false)
    }

    fn activate_application(&self, pair: &ReleasePair) -> Result<Vec<OwnedFile>, LifecycleError> {
        let state = self.load_lifecycle_state()?;
        let current = state
            .accepted()
            .first()
            .ok_or(LifecycleError::NoAcceptedPair)?;
        let current_owned = self
            .last_owned_files
            .borrow()
            .clone()
            .unwrap_or_else(|| current.owned_files.clone());
        let owned = self.activate_pair(pair, &current_owned)?;
        *self.last_owned_files.borrow_mut() = Some(owned.clone());
        Ok(owned)
    }

    fn restart_application(&self) -> Result<(), LifecycleError> {
        let command =
            RemoteCommand::interactive_sudo(["sudo", "systemctl", "restart", "planeradar.service"])
                .map_err(|_| LifecycleError::Backend)?;
        self.run_remote(command)?;
        Ok(())
    }

    fn verify_pair(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.ensure_driver_tool(&pair.driver)?;
        self.driver_tool
            .borrow()
            .as_ref()
            .ok_or(LifecycleError::Backend)?
            .1
            .verify_normal_boot()
            .map_err(|_| LifecycleError::Backend)?;
        self.verify_application(pair)?;
        let state = self.read_private_lifecycle_state()?;
        if state
            .as_ref()
            .and_then(LifecycleState::transaction)
            .is_some_and(|transaction| {
                transaction.candidate == *pair
                    && transaction.prior.pair.driver != transaction.candidate.driver
                    && transaction.phase
                        == planeradarctl::operations::LifecyclePhase::ApplicationRestarted
            })
        {
            self.run_driver_protocol(DriverAction::MarkVerifiedAccepted, None)?;
        }
        Ok(())
    }

    fn finalize_driver_acceptance(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.run_driver_protocol(DriverAction::FinalizeAccepted, None)
    }

    fn restore_application(&self, prior: &AcceptedPair) -> Result<Vec<OwnedFile>, LifecycleError> {
        if self.verify_application(&prior.pair).is_ok() {
            return Ok(prior.owned_files.clone());
        }
        let current = self
            .last_owned_files
            .borrow()
            .clone()
            .unwrap_or_else(|| prior.owned_files.clone());
        let restored = self.activate_pair(&prior.pair, &current)?;
        *self.last_owned_files.borrow_mut() = Some(restored.clone());
        Ok(restored)
    }

    fn restore_driver(&self, prior: &AcceptedPair) -> Result<(), LifecycleError> {
        let _ = self.run_driver_protocol(DriverAction::RecoverAccepted, None);
        self.ensure_driver_tool(&prior.pair.driver)?;
        if self
            .driver_tool
            .borrow()
            .as_ref()
            .ok_or(LifecycleError::Backend)?
            .1
            .verify_normal_boot()
            .is_ok()
        {
            return Ok(());
        }
        self.stage_driver(&prior.pair)?;
        self.tryboot_driver(&prior.pair)?;
        self.verify_tryboot_driver(&prior.pair)?;
        self.commit_driver(&prior.pair)?;
        self.reboot_normal(&prior.pair)
    }

    fn retire_candidate(&self, owned_files: &[OwnedFile]) -> Result<(), LifecycleError> {
        let helper = self.management_helper_path()?;
        let json = Self::ownership_json(owned_files)?;
        let command = RemoteCommand::interactive_sudo([
            "sudo",
            helper.as_str(),
            "lifecycle-retire",
            "--owned-json",
            json.as_str(),
        ])
        .map_err(|_| LifecycleError::Backend)?;
        self.run_remote(command)?;
        Ok(())
    }

    fn prepare_uninstall(&self, accepted: &AcceptedPair) -> Result<OwnedFile, LifecycleError> {
        let live_digests = accepted
            .owned_files
            .iter()
            .filter(|file| file.target_path == "/opt/planeradar/bin/planeradar")
            .map(|file| file.sha256.as_str())
            .collect::<Vec<_>>();
        let [digest] = live_digests.as_slice() else {
            return Err(LifecycleError::InvalidOwnership);
        };
        let helper = format!("/var/lib/planeradar-installer/helpers/{digest}/planeradar");
        let preserve = preserve_lifecycle_helper_command(&helper, digest)
            .map_err(|_| LifecycleError::Backend)?;
        self.run_remote(preserve)?;
        *self.uninstall_helper.borrow_mut() = Some(helper.clone());
        Ok(OwnedFile {
            target_path: helper,
            sha256: (*digest).to_owned(),
        })
    }

    fn uninstall_application(
        &self,
        owned_files: &[OwnedFile],
        purge_settings: bool,
    ) -> Result<(), LifecycleError> {
        let helper = self.command_helper_path()?;
        let json = Self::ownership_json(owned_files)?;
        let mut arguments = vec![
            "sudo",
            helper.as_str(),
            "lifecycle-uninstall",
            "--owned-json",
            json.as_str(),
        ];
        if purge_settings {
            arguments.push("--purge-settings");
        }
        let command =
            RemoteCommand::interactive_sudo(arguments).map_err(|_| LifecycleError::Backend)?;
        self.run_remote(command)?;
        Ok(())
    }

    fn uninstall_driver(&self, drivers: &[ArtifactIdentity]) -> Result<(), LifecycleError> {
        let Some((driver, historical)) = drivers.split_first() else {
            return Err(LifecycleError::InvalidState);
        };
        self.run_driver_protocol(DriverAction::UninstallAccepted, Some(driver))?;
        self.reboot_and_reconnect(false)?;
        let overlay = format!(
            "/boot/firmware/overlays/hyperpixel2r-kms-{}.dtbo",
            &driver.source_commit[..12]
        );
        let command = RemoteCommand::interactive_sudo([
            "sudo",
            "sh",
            "-c",
            "set -eu; test ! -e /lib/modules/$(uname -r)/extra/hyperpixel2r_kms.ko; test ! -L \"$1\" && test ! -e \"$1\"; ! awk '{ line=$0; sub(/^[[:space:]]+/, \"\", line); if (line ~ /^dtoverlay=/ && line ~ /hyperpixel2r/) found=1 } END { exit found ? 0 : 1 }' /boot/firmware/config.txt",
            "planeradar-stock-driver",
            overlay.as_str(),
        ])
        .map_err(|_| LifecycleError::Backend)?;
        self.run_remote(command)?;
        for inactive in historical {
            self.run_driver_protocol(DriverAction::RetireInactive, Some(inactive))?;
        }
        Ok(())
    }

    fn finalize_driver_uninstall(&self) -> Result<(), LifecycleError> {
        self.run_driver_protocol(DriverAction::FinalizeUninstall, None)
    }

    fn finalize_uninstall(&self, state: &LifecycleState) -> Result<(), LifecycleError> {
        let uninstall = state
            .uninstall_transaction()
            .ok_or(LifecycleError::InvalidState)?;
        let current = &uninstall.accepted;
        let digest = current.pair.application.sha256.as_str();
        let expected_helper = uninstall.recovery_helper.target_path.clone();
        if self.uninstall_helper.borrow().as_deref() != Some(expected_helper.as_str()) {
            return Err(LifecycleError::Backend);
        }
        let lifecycle_json = state.to_json()?;
        let lifecycle_sha256 = format!("{:x}", Sha256::digest(lifecycle_json.as_bytes()));
        let installer_state = TargetInstallState {
            schema_version: 1,
            hardware: state.hardware().clone(),
            application: Some(current.pair.application.clone()),
            driver: Some(current.pair.driver.clone()),
            owned_files: current.owned_files.clone(),
            last_verified_phase: InstallPhase::Complete,
        };
        let installer_json = installer_state
            .to_json()
            .map_err(|_| LifecycleError::InvalidState)?;
        let installer_sha256 = format!("{:x}", Sha256::digest(installer_json.as_bytes()));
        let command = finalize_lifecycle_uninstall_command(
            &expected_helper,
            digest,
            &current.pair.application.source_commit,
            &lifecycle_sha256,
            &installer_sha256,
        )
        .map_err(|_| LifecycleError::Backend)?;
        self.run_remote(command)?;
        Ok(())
    }

    fn retire_recovery_helper(&self, application: &ArtifactIdentity) -> Result<(), LifecycleError> {
        let helper = format!(
            "/var/lib/planeradar-installer/helpers/{}/planeradar",
            application.sha256
        );
        let command = retire_lifecycle_helper_command(
            &helper,
            &application.sha256,
            &application.source_commit,
        )
        .map_err(|_| LifecycleError::Backend)?;
        self.run_remote(command)?;
        let helper_is_active = self.uninstall_helper.borrow().as_deref() == Some(helper.as_str());
        if helper_is_active {
            *self.uninstall_helper.borrow_mut() = None;
        }
        Ok(())
    }
}

enum RemoteOperation {
    Status,
    Doctor { json: bool },
    Screenshot { output: PathBuf },
}

fn run_remote_operation(
    cli_target: Option<String>,
    environment: Environment,
    operation: RemoteOperation,
) -> Result<(), Box<dyn std::error::Error>> {
    let target_text = cli_target
        .or(environment.target)
        .filter(|target| !target.is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "target is required as user@host or PLANERADAR_PI_TARGET",
            )
        })?;
    let target = target_text.parse::<SshTarget>()?;
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "an absolute home directory is required",
            )
        })?;
    let transport =
        OpenSshTransport::system(TransportConfig::new(home.join(".ssh").join("known_hosts"))?);
    let backend = SshOperationsBackend::new(&transport, target, DriverLock::checked_in()?);
    let client = OperationsClient::new(&backend, SystemCaptureClock::default());

    match operation {
        RemoteOperation::Status => {
            println!("{}", client.status()?);
        }
        RemoteOperation::Doctor { json } => {
            let report = client.doctor()?;
            if json {
                println!("{}", report.to_json()?);
            } else if report.healthy {
                println!("Plane Radar doctor: healthy");
            } else {
                println!(
                    "Plane Radar doctor: unhealthy ({})",
                    report
                        .diagnostics
                        .iter()
                        .map(|diagnostic| format!("{diagnostic:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !report.healthy {
                let diagnostic = report
                    .diagnostics
                    .first()
                    .copied()
                    .ok_or_else(|| io::Error::other("doctor returned no diagnostic"))?;
                return Err(
                    planeradarctl::operations::OperationError::Unhealthy(diagnostic).into(),
                );
            }
        }
        RemoteOperation::Screenshot { output } => {
            let capture = client.screenshot(&output, Duration::from_secs(15))?;
            println!(
                "Screenshot saved to {} (sha256 {})",
                capture.destination.display(),
                capture.sha256
            );
        }
    }
    Ok(())
}

fn run_install(config: InstallConfig) -> Result<(), Box<dyn std::error::Error>> {
    let target_text = config
        .target
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target is required"))?;
    let target = target_text.parse::<SshTarget>()?;
    let lock = DriverLock::checked_in()?;
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "an absolute home directory is required",
            )
        })?;
    let cache_root = home.join(".cache").join("planeradar");
    ensure_private_cache_root(&cache_root)?;
    let version = requested_version(&config, SystemCommandRunner)?;
    let release_input = config
        .release_dir
        .as_deref()
        .map_or(ReleaseInput::Downloaded, ReleaseInput::Local);
    let release = ReleaseClient::new(GhReleaseSource::system(), cache_root.join("release"))
        .resolve(&version, &lock, release_input)?;
    Verifier::new(SystemCommandRunner).verify(&version, &release)?;
    let application_artifact = release
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact.name == "planeradar-aarch64-linux-gnu.tar.zst")
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "release has no supported application artifact",
            )
        })?;
    let application_payload = extract_application_payload_at_mtime(
        &application_artifact.path,
        &application_artifact.artifact.sha256,
        &cache_root.join("payloads"),
        release.manifest.source_date_epoch,
    )?;
    let payload_sha256 = application_payload.sha256().to_owned();
    let source_commit = release.manifest.source_commit.clone();
    let release_version = release.manifest.version.clone();

    let application_identity = ArtifactIdentity {
        version: release_version.to_string(),
        source_commit,
        sha256: payload_sha256.clone(),
    };
    let driver_identity = ArtifactIdentity {
        version: lock.version.to_string(),
        source_commit: lock.commit.clone(),
        sha256: lock.manifest_sha256.clone(),
    };
    let transport =
        OpenSshTransport::system(TransportConfig::new(home.join(".ssh").join("known_hosts"))?);
    let (target, observed, state_store) = select_install_target(
        &transport,
        target,
        &config.hostname,
        &home,
        &application_identity,
        &driver_identity,
    )?;
    let driver_actions = SystemDriverActions::new(lock.clone(), cache_root);
    let backend = LibrarySystemInstallBackend::new(
        transport,
        target,
        observed.clone(),
        env::current_dir()?,
        config.docker_context,
        application_payload,
        SystemHostPreflight,
        driver_actions,
        SystemInstallClock::default(),
    );
    let request = InstallRequest {
        target: observed,
        application: application_identity,
        driver: driver_identity,
        desired_hostname: config.hostname,
    };

    match Installer::new(&backend, &state_store).run(request)? {
        InstallOutcome::Complete => {
            println!("Installation complete.");
            Ok(())
        }
        InstallOutcome::AlreadyComplete => {
            println!("Installation is already complete.");
            Ok(())
        }
        InstallOutcome::Interrupted {
            phase,
            reason,
            guidance,
        } => {
            if let Some(guidance) = guidance {
                eprintln!("{guidance}");
            }
            Err(io::Error::other(format!(
                "installation interrupted after {phase:?}: {reason:?}"
            ))
            .into())
        }
    }
}

fn select_install_target(
    transport: &OpenSshTransport<SystemCommandRunner>,
    original: SshTarget,
    desired_hostname: &str,
    home: &Path,
    application: &ArtifactIdentity,
    driver: &ArtifactIdentity,
) -> Result<(SshTarget, TargetIdentity, LocalStateStore), Box<dyn std::error::Error>> {
    let desired = format!("{}@{desired_hostname}.local", original.username().as_str())
        .parse::<SshTarget>()?;
    let mut candidates = Vec::new();
    for (is_original, target) in [(true, original), (false, desired)] {
        if candidates
            .iter()
            .any(|candidate: &InstallCandidate| candidate.target == target)
        {
            continue;
        }
        if let Ok(probe) = transport.probe(&target) {
            let store = LocalStateStore::from_environment(home, probe.identity.clone())?;
            let persisted = store.load()?;
            candidates.push(InstallCandidate {
                target,
                observed: probe.identity,
                store,
                persisted,
                is_original,
            });
        }
    }
    if candidates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "neither the original target nor the desired hostname is reachable",
        )
        .into());
    }
    let candidate_states = candidates
        .iter()
        .map(|candidate| InstallCandidateState {
            is_original: candidate.is_original,
            observed: candidate.observed.clone(),
            persisted: candidate.persisted.clone(),
        })
        .collect::<Vec<_>>();
    let selected = select_candidate_index(&candidate_states, application, driver)?;
    let candidate = candidates.swap_remove(selected);
    Ok((candidate.target, candidate.observed, candidate.store))
}

struct InstallCandidate {
    target: SshTarget,
    observed: TargetIdentity,
    store: LocalStateStore,
    persisted: Option<InstallState>,
    is_original: bool,
}

struct InstallCandidateState {
    is_original: bool,
    observed: TargetIdentity,
    persisted: Option<InstallState>,
}

fn select_candidate_index(
    candidates: &[InstallCandidateState],
    application: &ArtifactIdentity,
    driver: &ArtifactIdentity,
) -> Result<usize, io::Error> {
    let matching = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            candidate
                .persisted
                .as_ref()
                .is_some_and(|persisted| {
                    resume_state_matches(persisted, &candidate.observed, application, driver)
                })
                .then_some(index)
        })
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [index] => return Ok(*index),
        [_, _, ..] => {
            let first = &candidates[matching[0]];
            if matching.iter().skip(1).all(|index| {
                let candidate = &candidates[*index];
                candidate.observed == first.observed && candidate.persisted == first.persisted
            }) {
                return Ok(matching
                    .iter()
                    .copied()
                    .find(|index| candidates[*index].is_original)
                    .unwrap_or(matching[0]));
            }
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "multiple reachable targets match the persisted installation identity",
            ));
        }
        [] => {}
    }
    if candidates
        .iter()
        .any(|candidate| candidate.persisted.is_some())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "reachable target does not match its persisted installation identity and artifacts",
        ));
    }
    candidates
        .iter()
        .position(|candidate| candidate.is_original)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "desired hostname has no persisted installation identity",
            )
        })
}

fn private_state_read_command(path: &str) -> Result<RemoteCommand, LifecycleError> {
    if !matches!(
        path,
        "/var/lib/planeradar-installer/lifecycle.json" | "/var/lib/planeradar-installer/state.json"
    ) {
        return Err(LifecycleError::InvalidState);
    }
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        "set -eu; p=$1; d=${p%/*}; if test ! -e \"$d\"; then printf 'null'; exit 0; fi; test ! -L \"$d\" && test -d \"$d\" && test \"$(stat -c '%u:%g:%a' -- \"$d\")\" = '0:0:700'; if test ! -e \"$p\"; then printf 'null'; exit 0; fi; test ! -L \"$p\" && test -f \"$p\" && test \"$(stat -c '%u:%g:%a:%h' -- \"$p\")\" = '0:0:600:1'; cat -- \"$p\"",
        "planeradar-private-state",
        path,
    ])
    .map_err(|_| LifecycleError::Backend)
}

fn resume_state_matches(
    persisted: &InstallState,
    observed: &TargetIdentity,
    application: &ArtifactIdentity,
    driver: &ArtifactIdentity,
) -> bool {
    &persisted.target == observed
        && persisted.application
            == (persisted.phase >= InstallPhase::ApplicationAcquired).then(|| application.clone())
        && persisted.driver
            == (persisted.phase >= InstallPhase::DriverReady).then(|| driver.clone())
}

fn requested_version<R: TransportCommandRunner>(
    config: &InstallConfig,
    runner: R,
) -> Result<Version, Box<dyn std::error::Error>> {
    if let Some(version) = &config.version {
        return Ok(version.clone());
    }
    let Some(release_directory) = config.release_dir.as_deref() else {
        return Ok(planeradarctl::release::GhLatestStableReleaseResolver::new(runner).resolve()?);
    };
    let manifest_bytes = fs::read(release_directory.join(MANIFEST_NAME))?;
    let manifest_value: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    let version_text = manifest_value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "release version is missing"))?;
    Ok(Version::parse(version_text)?)
}

fn ensure_private_cache_root(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("cache root has no parent"))?;
    if !parent.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(parent)?;
    }
    let parent_metadata = fs::symlink_metadata(parent)?;
    if !parent_metadata.file_type().is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(io::Error::other("cache parent is not a safe directory"));
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(path)?;
        }
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(io::Error::other("cache root is not a private directory"));
    }
    Ok(())
}

fn tryboot_reboot_command() -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "reboot", "0 tryboot"])
}

fn final_reboot_command() -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "systemctl", "reboot"])
}

fn deploy_helper_command(
    upload_path: &str,
    helper_path: &str,
    sha256: &str,
    revision: &str,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        r#"set -eu; upload=$1; helper=$2; digest=$3; revision=$4; case "$helper" in /var/lib/planeradar-installer/helpers/"$digest"/planeradar) ;; *) exit 64 ;; esac; test ! -L "$upload" && test -f "$upload"; test "$(sha256sum -- "$upload" | awk '{print $1}')" = "$digest"; root=${helper%/planeradar}; install -d -o root -g root -m 0700 /var/lib/planeradar-installer; install -d -o root -g root -m 0700 /var/lib/planeradar-installer/helpers; install -d -o root -g root -m 0700 "$root"; binary_tmp="$root/.planeradar.$$"; checksum_tmp="$root/.planeradar.sha256.$$"; revision_tmp="$root/.planeradar.revision.$$"; trap 'rm -f -- "$binary_tmp" "$checksum_tmp" "$revision_tmp"' EXIT HUP INT TERM; install -o root -g root -m 0700 -- "$upload" "$binary_tmp"; printf '%s  planeradar\n' "$digest" >"$checksum_tmp"; printf '%s\n' "$revision" >"$revision_tmp"; chown root:root "$checksum_tmp" "$revision_tmp"; chmod 0600 "$checksum_tmp" "$revision_tmp"; test "$(sha256sum -- "$binary_tmp" | awk '{print $1}')" = "$digest"; mv -f -- "$binary_tmp" "$helper"; mv -f -- "$checksum_tmp" "$helper.sha256"; mv -f -- "$revision_tmp" "$helper.revision"; trap - EXIT HUP INT TERM; test ! -L "$helper" && test -f "$helper" && test -x "$helper"; test "$(stat -c '%u:%g:%a' -- "$helper")" = "0:0:700"; test "$(sha256sum -- "$helper" | awk '{print $1}')" = "$digest""#,
        "planeradar-helper-deploy",
        upload_path,
        helper_path,
        sha256,
        revision,
    ])
}

fn deploy_candidate_artifact_command(
    upload_path: &str,
    artifact_path: &str,
    version: &str,
    sha256: &str,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        r#"set -eu; upload=$1; artifact=$2; version=$3; digest=$4; case "$artifact" in /opt/planeradar/releases/"$version"/"$digest"/planeradar) ;; *) exit 64 ;; esac; test ! -L "$upload" && test -f "$upload"; test "$(sha256sum -- "$upload" | awk '{print $1}')" = "$digest"; regular_artifact() { test ! -L "$artifact" && test -f "$artifact" && test "$(stat -c '%u:%g:%a:%h' -- "$artifact")" = '0:0:600:1' && test "$(sha256sum -- "$artifact" | awk '{print $1}')" = "$digest"; }; if test -e "$artifact"; then test ! -L "$artifact" && test -f "$artifact" && test "$(stat -c '%u:%g:%h' -- "$artifact")" = '0:0:1' && test "$(sha256sum -- "$artifact" | awk '{print $1}')" = "$digest"; else safe_dir() { if test -e "$1"; then test ! -L "$1" && test -d "$1" && test "$(stat -c '%u:%g:%a' -- "$1")" = '0:0:755'; else install -d -o root -g root -m 0755 -- "$1"; fi; }; safe_dir /opt/planeradar; safe_dir /opt/planeradar/releases; safe_dir "/opt/planeradar/releases/$version"; safe_dir "/opt/planeradar/releases/$version/$digest"; temporary="${artifact%/planeradar}/.planeradar.$$"; trap 'rm -f -- "$temporary"' EXIT HUP INT TERM; install -o root -g root -m 0600 -- "$upload" "$temporary"; test "$(sha256sum -- "$temporary" | awk '{print $1}')" = "$digest"; mv -n -- "$temporary" "$artifact"; rm -f -- "$temporary"; trap - EXIT HUP INT TERM; regular_artifact; fi"#,
        "planeradar-candidate-deploy",
        upload_path,
        artifact_path,
        version,
        sha256,
    ])
}

fn preserve_lifecycle_helper_command(
    helper_path: &str,
    sha256: &str,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        r#"set -eu; source=/opt/planeradar/bin/planeradar; helper=$1; digest=$2; case "$helper" in /var/lib/planeradar-installer/helpers/"$digest"/planeradar) ;; *) exit 64 ;; esac; root=${helper%/planeradar}; regular_helper() { test ! -L "$helper" && test -f "$helper" && test -x "$helper" && test "$(stat -c '%u:%g:%a:%h' -- "$helper")" = "0:0:700:1" && test "$(sha256sum -- "$helper" | awk '{print $1}')" = "$digest"; }; if test -e "$helper"; then regular_helper; else test ! -L "$source" && test -f "$source" && test -x "$source"; test "$(stat -c '%u:%g:%a:%h' -- "$source")" = "0:0:755:1"; test "$(sha256sum -- "$source" | awk '{print $1}')" = "$digest"; safe_dir() { if test -e "$1"; then test ! -L "$1" && test -d "$1" && test "$(stat -c '%u:%g:%a' -- "$1")" = "0:0:700"; else install -d -o root -g root -m 0700 -- "$1"; fi; }; safe_dir /var/lib/planeradar-installer; safe_dir /var/lib/planeradar-installer/helpers; safe_dir "$root"; temporary="$root/.planeradar.$$"; trap 'rm -f -- "$temporary"' EXIT HUP INT TERM; install -o root -g root -m 0700 -- "$source" "$temporary"; test "$(stat -c '%u:%g:%a:%h' -- "$temporary")" = "0:0:700:1"; test "$(sha256sum -- "$temporary" | awk '{print $1}')" = "$digest"; mv -n -- "$temporary" "$helper"; regular_helper; rm -f -- "$temporary"; trap - EXIT HUP INT TERM; fi; regular_helper"#,
        "planeradar-helper-preserve",
        helper_path,
        sha256,
    ])
}

fn retire_lifecycle_helper_command(
    helper_path: &str,
    sha256: &str,
    revision: &str,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        r#"set -eu; helper=$1; digest=$2; revision=$3; case "$helper" in /var/lib/planeradar-installer/helpers/"$digest"/planeradar) ;; *) exit 64 ;; esac; regular() { test ! -L "$1" && test -f "$1" && test "$(stat -c '%u:%g:%a:%h' -- "$1")" = "0:0:$2:1"; }; if test -e "$helper"; then regular "$helper" 700; test "$(sha256sum -- "$helper" | awk '{print $1}')" = "$digest"; else test ! -L "$helper"; fi; checksum="$helper.sha256"; if test -e "$checksum"; then regular "$checksum" 600; test "$(cat -- "$checksum")" = "$digest  planeradar"; else test ! -L "$checksum"; fi; revision_file="$helper.revision"; if test -e "$revision_file"; then regular "$revision_file" 600; test "$(cat -- "$revision_file")" = "$revision"; else test ! -L "$revision_file"; fi; rm -f -- "$checksum" "$revision_file" "$helper"; rmdir -- "${helper%/planeradar}" 2>/dev/null || true; rmdir -- /var/lib/planeradar-installer/helpers 2>/dev/null || true"#,
        "planeradar-helper-retire",
        helper_path,
        sha256,
        revision,
    ])
}

fn finalize_lifecycle_uninstall_command(
    helper_path: &str,
    sha256: &str,
    revision: &str,
    lifecycle_state_sha256: &str,
    installer_state_sha256: &str,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        r#"set -eu; helper=$1; digest=$2; revision=$3; lifecycle_digest=$4; installer_digest=$5; case "$helper" in /var/lib/planeradar-installer/helpers/"$digest"/planeradar) ;; *) exit 64 ;; esac; regular() { test ! -L "$1" && test -f "$1" && test "$(stat -c '%u:%g:%a:%h' -- "$1")" = "0:0:$2:1"; }; matches() { test "$(sha256sum -- "$1" | awk '{print $1}')" = "$2"; }; exact_or_absent() { if test -e "$1"; then regular "$1" "$2" && matches "$1" "$3"; else test ! -L "$1"; fi; }; lifecycle=/var/lib/planeradar-installer/lifecycle.json; installer=/var/lib/planeradar-installer/state.json; checksum="$helper.sha256"; revision_file="$helper.revision"; regular "$lifecycle" 600 && matches "$lifecycle" "$lifecycle_digest"; exact_or_absent "$helper" 700 "$digest"; if test -e "$installer"; then regular "$installer" 600 && matches "$installer" "$installer_digest"; else test ! -L "$installer"; fi; if test -e "$checksum"; then regular "$checksum" 600 && test "$(cat -- "$checksum")" = "$digest  planeradar"; else test ! -L "$checksum"; fi; if test -e "$revision_file"; then regular "$revision_file" 600 && test "$(cat -- "$revision_file")" = "$revision"; else test ! -L "$revision_file"; fi; rm -f -- "$checksum" "$revision_file" "$helper"; rmdir -- "${helper%/planeradar}" 2>/dev/null || true; rmdir -- /var/lib/planeradar-installer/helpers 2>/dev/null || true; rm -f -- "$installer"; sync -f /var/lib/planeradar-installer; rm -- "$lifecycle"; sync -f /var/lib/planeradar-installer; rmdir -- /var/lib/planeradar-installer 2>/dev/null || true"#,
        "planeradar-uninstall-finalize",
        helper_path,
        sha256,
        revision,
        lifecycle_state_sha256,
        installer_state_sha256,
    ])
}

fn run_driver(command: DriverCommand) -> Result<(), Box<dyn std::error::Error>> {
    let repository = std::env::current_dir()?;
    let cache = repository.join(".cache/driver");
    let manager = DriverManager::new(
        GhDriverReleaseSource::system(),
        GhDriverReleaseVerifier::system(),
        cache,
    );
    match command {
        DriverCommand::Sync => {
            let lock =
                DriverLock::parse(&fs::read_to_string(repository.join("driver.lock.toml"))?)?;
            let synced = manager.sync(&lock)?;
            println!("Synced locked HyperPixel driver {}", synced.lock().version);
        }
        DriverCommand::Update { version } => {
            let version = Version::parse(&version)?;
            let lock = manager.update(&repository.join("driver.lock.toml"), &version)?;
            println!("Updated HyperPixel driver lock to {}", lock.version);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command as SystemCommand;
    use std::rc::Rc;

    use planeradarctl::config::{DriverLock, InstallConfig};
    use planeradarctl::install::{ApplicationPayload, BackendFailure};
    use planeradarctl::operations::{ManagementHelper, ReleasePair};
    use planeradarctl::state::{
        ArtifactIdentity, InstallPhase as TargetInstallPhase, OwnedFile, TargetHardwareIdentity,
        TargetInstallState,
    };
    use planeradarctl::system_install::{
        committed_driver_command, hostname_command, staged_driver_transaction_command,
        target_install_command, target_install_ownership_command, tryboot_wait_failure,
    };
    use planeradarctl::target::SshTarget;
    use planeradarctl::transport::{
        CommandOutput, CommandRunner, Invocation, OpenSshTransport, RunnerError, TransportConfig,
    };
    use semver::Version;
    use sha2::{Digest, Sha256};

    use super::{
        DriverProtocolActions, InstallCandidateState, InstallPhase, InstallState, MANIFEST_NAME,
        SystemLifecycleBackend, TargetIdentity, TransportError, deploy_helper_command,
        final_reboot_command, finalize_lifecycle_uninstall_command,
        preserve_lifecycle_helper_command, private_state_read_command, requested_version,
        resume_state_matches, retire_lifecycle_helper_command, select_candidate_index,
        tryboot_reboot_command, worker_exit_code,
    };

    #[test]
    fn native_supervisor_maps_worker_exit_and_signal_statuses() {
        let exited = SystemCommand::new("/bin/sh")
            .args(["-c", "exit 37"])
            .status()
            .expect("run exiting worker");
        assert_eq!(worker_exit_code(exited), 37);

        let mut signaled = SystemCommand::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("run signaled worker");
        // SAFETY: the child PID is live and retained until the following wait.
        assert_eq!(
            unsafe { libc::kill(signaled.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        assert_eq!(
            worker_exit_code(signaled.wait().expect("wait for signaled worker")),
            143
        );
    }

    #[derive(Default)]
    struct RecordingLifecycleRunner {
        invocations: RefCell<Vec<Invocation>>,
    }

    impl CommandRunner for &RecordingLifecycleRunner {
        fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError> {
            self.invocations.borrow_mut().push(invocation);
            Ok(CommandOutput::success(Vec::new(), Vec::new()))
        }
    }

    struct ScriptedLifecycleRunner {
        invocations: RefCell<Vec<Invocation>>,
        responses: RefCell<VecDeque<CommandOutput>>,
    }

    impl ScriptedLifecycleRunner {
        fn new(responses: impl IntoIterator<Item = CommandOutput>) -> Self {
            Self {
                invocations: RefCell::new(Vec::new()),
                responses: RefCell::new(responses.into_iter().collect()),
            }
        }
    }

    impl CommandRunner for &ScriptedLifecycleRunner {
        fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError> {
            self.invocations.borrow_mut().push(invocation);
            self.responses
                .borrow_mut()
                .pop_front()
                .ok_or(RunnerError::Failed)
        }
    }

    fn requested_version_config(
        version: Option<Version>,
        release_dir: Option<PathBuf>,
    ) -> InstallConfig {
        InstallConfig {
            target: Some("pi@raspberrypi.local".into()),
            hostname: "planeradar".into(),
            version,
            release_dir,
            docker_context: None,
            non_interactive: true,
            purge_settings: false,
        }
    }

    #[test]
    fn exact_version_and_local_release_selectors_bypass_latest_stable_resolution() {
        let runner = RecordingLifecycleRunner::default();
        let explicit = requested_version_config(Some(Version::new(2, 3, 4)), None);
        assert_eq!(
            requested_version(&explicit, &runner).expect("explicit version"),
            Version::new(2, 3, 4)
        );

        let temporary = tempfile::tempdir().expect("local release");
        fs::write(
            temporary.path().join(MANIFEST_NAME),
            br#"{"version":"3.4.5"}"#,
        )
        .expect("local release manifest");
        let local = requested_version_config(None, Some(temporary.path().into()));
        assert_eq!(
            requested_version(&local, &runner).expect("local version"),
            Version::new(3, 4, 5)
        );
        assert!(
            runner.invocations.borrow().is_empty(),
            "explicit selectors invoked latest-stable resolution"
        );
    }

    fn application_payload(root: &Path, bytes: &[u8]) -> ApplicationPayload {
        let mut archive = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut archive);
            let mut header = tar::Header::new_gnu();
            header.set_path("planeradar").expect("application path");
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append(&header, bytes)
                .expect("append application payload");
            builder.finish().expect("finish application archive");
        }
        let compressed =
            zstd::stream::encode_all(archive.as_slice(), 1).expect("compress application archive");
        let archive_path = root.join("application.tar.zst");
        fs::write(&archive_path, &compressed).expect("write application archive");
        let archive_sha = format!("{:x}", Sha256::digest(&compressed));
        planeradarctl::install::extract_application_payload(
            &archive_path,
            &archive_sha,
            &root.join("payload-cache"),
        )
        .expect("extract application payload")
    }

    fn scripted_backend<'a>(
        runner: &'a ScriptedLifecycleRunner,
        root: &Path,
        verified_payloads: BTreeMap<String, (ReleasePair, ApplicationPayload)>,
        management_helper: Option<ManagementHelper>,
        driver_protocol_actions: Option<DriverProtocolActions>,
    ) -> SystemLifecycleBackend<&'a ScriptedLifecycleRunner> {
        SystemLifecycleBackend {
            transport: OpenSshTransport::with_runner(
                runner,
                TransportConfig::new(root.join("known_hosts")).expect("transport config"),
            ),
            target: RefCell::new(
                "pi@planeradar.local"
                    .parse::<SshTarget>()
                    .expect("SSH target"),
            ),
            expected_identity: TargetIdentity {
                host_key_sha256: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                model: "Raspberry Pi Zero 2 W".into(),
                serial: "10000000abcdef01".into(),
            },
            release_dir: None,
            cache_root: root.into(),
            lock: DriverLock::checked_in().expect("checked-in lock"),
            verified_payloads: RefCell::new(verified_payloads),
            candidate: RefCell::new(None),
            management_helper: RefCell::new(management_helper),
            uninstall_helper: RefCell::new(None),
            staged_artifact: RefCell::new(None),
            last_owned_files: RefCell::new(None),
            driver_tool: RefCell::new(None),
            protocol_tool: RefCell::new(None),
            retained_driver_transition: std::cell::Cell::new(false),
            driver_protocol_actions: RefCell::new(driver_protocol_actions),
        }
    }

    fn release_pair(version: &str, application_seed: char, driver_seed: char) -> ReleasePair {
        ReleasePair {
            application: ArtifactIdentity {
                version: version.into(),
                source_commit: application_seed.to_string().repeat(40),
                sha256: application_seed.to_string().repeat(64),
            },
            driver: ArtifactIdentity {
                version: "0.1.0".into(),
                source_commit: driver_seed.to_string().repeat(40),
                sha256: driver_seed.to_string().repeat(64),
            },
        }
    }

    #[test]
    fn production_backend_selects_only_the_exact_management_helper_for_lifecycle_commands() {
        let runner = RecordingLifecycleRunner::default();
        let temporary = tempfile::tempdir().expect("temporary backend");
        let candidate = release_pair("2.0.0", '2', 'b');
        let helper = ManagementHelper {
            application: candidate.application.clone(),
            target_path: format!(
                "/var/lib/planeradar-installer/helpers/{}/planeradar",
                candidate.application.sha256
            ),
            protocol: "lifecycle-v3".into(),
        };
        let backend = SystemLifecycleBackend {
            transport: OpenSshTransport::with_runner(
                &runner,
                TransportConfig::new(temporary.path().join("known_hosts"))
                    .expect("transport config"),
            ),
            target: RefCell::new(
                "pi@planeradar.local"
                    .parse::<SshTarget>()
                    .expect("SSH target"),
            ),
            expected_identity: TargetIdentity {
                host_key_sha256: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                model: "Raspberry Pi Zero 2 W".into(),
                serial: "10000000abcdef01".into(),
            },
            release_dir: None,
            cache_root: PathBuf::from(temporary.path()),
            lock: DriverLock::checked_in().expect("checked-in lock"),
            verified_payloads: RefCell::new(BTreeMap::new()),
            candidate: RefCell::new(None),
            management_helper: RefCell::new(Some(helper.clone())),
            uninstall_helper: RefCell::new(None),
            staged_artifact: RefCell::new(None),
            last_owned_files: RefCell::new(None),
            driver_tool: RefCell::new(None),
            protocol_tool: RefCell::new(None),
            retained_driver_transition: std::cell::Cell::new(false),
            driver_protocol_actions: RefCell::new(None),
        };

        assert_eq!(
            backend
                .management_helper_path()
                .expect("management helper path"),
            helper.target_path
        );
    }

    #[test]
    fn fresh_production_backend_unconditionally_finalizes_durable_driver_acceptance() {
        let runner = RecordingLifecycleRunner::default();
        let temporary = tempfile::tempdir().expect("temporary backend");
        let candidate = release_pair("2.0.0", '2', 'b');
        let actions = Rc::new(RefCell::new(Vec::new()));
        let backend = SystemLifecycleBackend {
            transport: OpenSshTransport::with_runner(
                &runner,
                TransportConfig::new(temporary.path().join("known_hosts"))
                    .expect("transport config"),
            ),
            target: RefCell::new(
                "pi@planeradar.local"
                    .parse::<SshTarget>()
                    .expect("SSH target"),
            ),
            expected_identity: TargetIdentity {
                host_key_sha256: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                model: "Raspberry Pi Zero 2 W".into(),
                serial: "10000000abcdef01".into(),
            },
            release_dir: None,
            cache_root: PathBuf::from(temporary.path()),
            lock: DriverLock::checked_in().expect("checked-in lock"),
            verified_payloads: RefCell::new(BTreeMap::new()),
            candidate: RefCell::new(None),
            management_helper: RefCell::new(None),
            uninstall_helper: RefCell::new(None),
            staged_artifact: RefCell::new(None),
            last_owned_files: RefCell::new(None),
            driver_tool: RefCell::new(None),
            protocol_tool: RefCell::new(None),
            retained_driver_transition: std::cell::Cell::new(false),
            driver_protocol_actions: RefCell::new(Some(actions.clone())),
        };

        planeradarctl::operations::LifecycleBackend::finalize_driver_acceptance(
            &backend, &candidate,
        )
        .expect("idempotent driver-v2 finalizer");

        assert_eq!(
            actions.borrow().as_slice(),
            [(planeradarctl::driver::DriverAction::FinalizeAccepted, None)]
        );
    }

    #[test]
    fn production_task14_migration_writes_state_only_through_the_current_management_helper() {
        let temporary = tempfile::tempdir().expect("temporary backend");
        let payload = application_payload(temporary.path(), b"task14-application-payload");
        let task14_pair = ReleasePair {
            application: ArtifactIdentity {
                version: "1.0.0".into(),
                source_commit: "1".repeat(40),
                sha256: payload.sha256().into(),
            },
            driver: ArtifactIdentity {
                version: "0.1.0".into(),
                source_commit: "a".repeat(40),
                sha256: "a".repeat(64),
            },
        };
        let task14_state = TargetInstallState {
            schema_version: 1,
            hardware: TargetHardwareIdentity {
                model: "Raspberry Pi Zero 2 W".into(),
                serial: "10000000abcdef01".into(),
            },
            application: Some(task14_pair.application.clone()),
            driver: Some(task14_pair.driver.clone()),
            owned_files: vec![OwnedFile {
                target_path: "/opt/planeradar/bin/planeradar".into(),
                sha256: task14_pair.application.sha256.clone(),
            }],
            last_verified_phase: TargetInstallPhase::Complete,
        };
        let migrated = planeradarctl::operations::LifecycleState::migrate_task14(&task14_state)
            .expect("migrated state");
        let current = release_pair("2.0.0", '2', 'b');
        let current_helper = ManagementHelper {
            application: current.application.clone(),
            target_path: format!(
                "/var/lib/planeradar-installer/helpers/{}/planeradar",
                current.application.sha256
            ),
            protocol: "lifecycle-v3".into(),
        };
        let runner = ScriptedLifecycleRunner::new([
            CommandOutput::success(b"null\n".to_vec(), Vec::new()),
            CommandOutput::success(
                task14_state
                    .to_json()
                    .expect("Task14 state JSON")
                    .into_bytes(),
                Vec::new(),
            ),
            CommandOutput::success(Vec::new(), Vec::new()),
            CommandOutput::success(
                migrated.to_json().expect("lifecycle JSON").into_bytes(),
                Vec::new(),
            ),
        ]);
        let mut verified = BTreeMap::new();
        verified.insert(
            SystemLifecycleBackend::<&ScriptedLifecycleRunner>::verified_payload_key(&task14_pair),
            (task14_pair.clone(), payload),
        );
        let actions = Rc::new(RefCell::new(Vec::new()));
        let backend = scripted_backend(
            &runner,
            temporary.path(),
            verified,
            Some(current_helper.clone()),
            Some(actions.clone()),
        );

        let loaded = planeradarctl::operations::LifecycleBackend::load_lifecycle_state(&backend)
            .expect("production Task14 migration");

        assert_eq!(loaded, migrated);
        assert_eq!(
            actions.borrow().as_slice(),
            [(
                planeradarctl::driver::DriverAction::RecordAccepted,
                Some(task14_pair.driver.source_commit.clone())
            )]
        );
        let invocations = runner.invocations.borrow();
        let writes = invocations
            .iter()
            .filter(|invocation| {
                let arguments = invocation.arguments().join(" ");
                arguments.contains("installer-state") || arguments.contains("lifecycle-state")
            })
            .collect::<Vec<_>>();
        assert_eq!(writes.len(), 2);
        let historical_helper = format!(
            "/var/lib/planeradar-installer/helpers/{}/planeradar",
            task14_pair.application.sha256
        );
        for invocation in writes {
            let arguments = invocation.arguments().join(" ");
            assert!(arguments.contains(&current_helper.target_path));
            assert!(!arguments.contains(&historical_helper));
            assert!(!arguments.contains("/opt/planeradar/bin/planeradar installer-state"));
            assert!(!arguments.contains("/opt/planeradar/bin/planeradar lifecycle-state"));
        }
    }

    #[test]
    fn production_historical_activation_executes_current_helper_and_treats_task14_as_data() {
        let temporary = tempfile::tempdir().expect("temporary backend");
        let current = release_pair("2.0.0", '2', 'b');
        let historical = release_pair("1.0.0", '1', 'a');
        let helper = ManagementHelper {
            application: current.application.clone(),
            target_path: format!(
                "/var/lib/planeradar-installer/helpers/{}/planeradar",
                current.application.sha256
            ),
            protocol: "lifecycle-v3".into(),
        };
        let returned_ownership = serde_json::json!({
            "schema_version": 1,
            "owned_files": [{
                "target_path": "/opt/planeradar/bin/planeradar",
                "sha256": historical.application.sha256
            }]
        });
        let runner = ScriptedLifecycleRunner::new([CommandOutput::success(
            serde_json::to_vec(&returned_ownership).expect("ownership JSON"),
            Vec::new(),
        )]);
        let backend = scripted_backend(
            &runner,
            temporary.path(),
            BTreeMap::new(),
            Some(helper.clone()),
            None,
        );
        let historical_artifact = format!(
            "/opt/planeradar/releases/{}/{}/planeradar",
            historical.application.version, historical.application.sha256
        );

        backend
            .activate_artifact(
                &historical,
                &[OwnedFile {
                    target_path: "/opt/planeradar/bin/planeradar".into(),
                    sha256: current.application.sha256,
                }],
                &historical_artifact,
            )
            .expect("historical activation through current helper");

        let invocation = &runner.invocations.borrow()[0];
        let arguments = invocation.arguments().join(" ");
        let historical_helper = format!(
            "/var/lib/planeradar-installer/helpers/{}/planeradar",
            historical.application.sha256
        );
        assert!(arguments.contains(&helper.target_path));
        assert!(arguments.contains("lifecycle-activate"));
        assert!(arguments.contains(&historical_artifact));
        assert!(!arguments.contains(&historical_helper));
        assert!(!arguments.contains("/opt/planeradar/bin/planeradar lifecycle-activate"));
    }

    #[test]
    fn fresh_production_backend_recovers_persisted_helper_before_candidate_retirement() {
        let temporary = tempfile::tempdir().expect("temporary backend");
        let payload = application_payload(temporary.path(), b"current-management-payload");
        let candidate = ReleasePair {
            application: ArtifactIdentity {
                version: "2.0.0".into(),
                source_commit: "2".repeat(40),
                sha256: payload.sha256().into(),
            },
            driver: ArtifactIdentity {
                version: "0.1.0".into(),
                source_commit: "b".repeat(40),
                sha256: "b".repeat(64),
            },
        };
        let prior = release_pair("1.0.0", '1', 'a');
        let helper = ManagementHelper {
            application: candidate.application.clone(),
            target_path: format!(
                "/var/lib/planeradar-installer/helpers/{}/planeradar",
                candidate.application.sha256
            ),
            protocol: "lifecycle-v3".into(),
        };
        let candidate_release = OwnedFile {
            target_path: format!(
                "/opt/planeradar/releases/{}/{}/planeradar",
                candidate.application.version, candidate.application.sha256
            ),
            sha256: candidate.application.sha256.clone(),
        };
        let state = planeradarctl::operations::LifecycleState::from_json(
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 3,
                "hardware": {
                    "model": "Raspberry Pi Zero 2 W",
                    "serial": "10000000abcdef01"
                },
                "accepted": [{
                    "pair": prior.clone(),
                    "sequence": 1,
                    "owned_files": [{
                        "target_path": "/opt/planeradar/bin/planeradar",
                        "sha256": prior.application.sha256
                    }]
                }],
                "transaction": {
                    "prior": {
                        "pair": prior,
                        "sequence": 1,
                        "owned_files": [{
                            "target_path": "/opt/planeradar/bin/planeradar",
                            "sha256": "1".repeat(64)
                        }]
                    },
                    "candidate": candidate.clone(),
                    "management_helper": helper.clone(),
                    "candidate_owned_files": [candidate_release.clone()],
                    "restored_owned_files": null,
                    "phase": "prepared"
                },
                "uninstall": null
            }))
            .expect("transaction JSON")
            .as_slice(),
        )
        .expect("durable transaction");
        let runner = ScriptedLifecycleRunner::new([
            CommandOutput::success(
                state.to_json().expect("state JSON").into_bytes(),
                Vec::new(),
            ),
            CommandOutput::success(
                b"/var/tmp/planeradar-upload.ABCDEF1234\n".to_vec(),
                Vec::new(),
            ),
            CommandOutput::success(Vec::new(), Vec::new()),
            CommandOutput::success(Vec::new(), Vec::new()),
            CommandOutput::success(Vec::new(), Vec::new()),
            CommandOutput::success(Vec::new(), Vec::new()),
            CommandOutput::success(Vec::new(), Vec::new()),
            CommandOutput::success(b"lifecycle-v3\n".to_vec(), Vec::new()),
            CommandOutput::success(Vec::new(), Vec::new()),
        ]);
        let mut verified = BTreeMap::new();
        verified.insert(
            SystemLifecycleBackend::<&ScriptedLifecycleRunner>::verified_payload_key(&candidate),
            (candidate.clone(), payload),
        );
        let backend = scripted_backend(&runner, temporary.path(), verified, None, None);

        planeradarctl::operations::LifecycleBackend::load_lifecycle_state(&backend)
            .expect("fresh helper recovery");
        planeradarctl::operations::LifecycleBackend::retire_candidate(
            &backend,
            &[candidate_release],
        )
        .expect("candidate retirement");

        let invocations = runner.invocations.borrow();
        let protocol_index = invocations
            .iter()
            .position(|invocation| {
                invocation
                    .arguments()
                    .join(" ")
                    .contains("lifecycle-protocol")
            })
            .expect("protocol verification command");
        let retire_index = invocations
            .iter()
            .position(|invocation| {
                invocation
                    .arguments()
                    .join(" ")
                    .contains("lifecycle-retire")
            })
            .expect("candidate retirement command");
        assert!(protocol_index < retire_index);
        let retirement = invocations[retire_index].arguments().join(" ");
        assert!(retirement.contains(&helper.target_path));
        assert!(!retirement.contains("/opt/planeradar/bin/planeradar"));
    }

    #[test]
    fn production_adapter_uses_exact_typed_reboot_and_hostname_commands() {
        let tryboot = tryboot_reboot_command().expect("tryboot command");
        assert!(tryboot.is_interactive_sudo());
        assert_eq!(tryboot.arguments(), ["sudo", "reboot", "0 tryboot"]);

        let hostname = hostname_command("planeradar").expect("hostname command");
        assert!(hostname.is_interactive_sudo());
        assert_eq!(
            hostname.arguments(),
            ["sudo", "hostnamectl", "set-hostname", "planeradar"]
        );

        let final_reboot = final_reboot_command().expect("final reboot command");
        assert!(final_reboot.is_interactive_sudo());
        assert_eq!(final_reboot.arguments(), ["sudo", "systemctl", "reboot"]);
    }

    #[test]
    fn task14_migration_reads_private_state_without_invoking_the_installed_binary() {
        for path in [
            "/var/lib/planeradar-installer/lifecycle.json",
            "/var/lib/planeradar-installer/state.json",
        ] {
            let command = private_state_read_command(path).expect("private state read command");
            assert!(command.is_interactive_sudo());
            assert_eq!(command.arguments()[0..3], ["sudo", "sh", "-c"]);
            assert_eq!(
                &command.arguments()[4..],
                ["planeradar-private-state", path]
            );
            assert!(
                command
                    .arguments()
                    .iter()
                    .all(|argument| !argument.contains("/opt/planeradar/bin/planeradar"))
            );
        }
        assert!(private_state_read_command("/tmp/state.json").is_err());
    }

    #[test]
    fn tryboot_timeout_requires_an_observed_disconnect() {
        assert_eq!(
            tryboot_wait_failure(TransportError::ReconnectTimedOut),
            BackendFailure::TrybootTimedOut
        );
        assert_eq!(
            tryboot_wait_failure(TransportError::NeverDisconnected),
            BackendFailure::OperationFailed
        );
        assert_eq!(
            tryboot_wait_failure(TransportError::CommandFailed),
            BackendFailure::OperationFailed
        );
    }

    #[test]
    fn fresh_process_hostname_resume_requires_the_persisted_exact_identity_and_artifacts() {
        let target = TargetIdentity {
            host_key_sha256: "SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A".into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef01".into(),
        };
        let application = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "1".repeat(40),
            sha256: "2".repeat(64),
        };
        let driver = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "3".repeat(40),
            sha256: "4".repeat(64),
        };
        let resumed = InstallState {
            schema_version: 1,
            target: target.clone(),
            phase: InstallPhase::HostnameChanged,
            application: Some(application.clone()),
            driver: Some(driver.clone()),
        };

        assert!(resume_state_matches(
            &resumed,
            &target,
            &application,
            &driver
        ));
        let mut wrong_identity = target.clone();
        wrong_identity.serial = "10000000abcdef02".into();
        assert!(!resume_state_matches(
            &resumed,
            &wrong_identity,
            &application,
            &driver
        ));
        let mut wrong_artifact = resumed;
        wrong_artifact
            .application
            .as_mut()
            .expect("application")
            .sha256 = "5".repeat(64);
        assert!(!resume_state_matches(
            &wrong_artifact,
            &target,
            &application,
            &driver
        ));
    }

    #[test]
    fn durable_matching_identity_wins_over_a_reachable_reused_original_hostname() {
        let application = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "1".repeat(40),
            sha256: "2".repeat(64),
        };
        let driver = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "3".repeat(40),
            sha256: "4".repeat(64),
        };
        let installed = TargetIdentity {
            host_key_sha256: "SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A".into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef01".into(),
        };
        let reused_original = TargetIdentity {
            host_key_sha256: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef02".into(),
        };
        let persisted = InstallState {
            schema_version: 1,
            target: installed.clone(),
            phase: InstallPhase::HostnameChanged,
            application: Some(application.clone()),
            driver: Some(driver.clone()),
        };
        let candidates = [
            InstallCandidateState {
                is_original: true,
                observed: reused_original,
                persisted: None,
            },
            InstallCandidateState {
                is_original: false,
                observed: installed,
                persisted: Some(persisted),
            },
        ];

        assert_eq!(
            select_candidate_index(&candidates, &application, &driver).expect("safe candidate"),
            1
        );
        assert_eq!(
            select_candidate_index(&candidates[..1], &application, &driver)
                .expect("fresh original"),
            0
        );
    }

    #[test]
    fn two_aliases_for_one_persisted_pi_select_the_original_deterministically() {
        let application = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "1".repeat(40),
            sha256: "2".repeat(64),
        };
        let driver = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "3".repeat(40),
            sha256: "4".repeat(64),
        };
        let identity = TargetIdentity {
            host_key_sha256: "SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A".into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef01".into(),
        };
        let persisted = InstallState {
            schema_version: 1,
            target: identity.clone(),
            phase: InstallPhase::HostnameChanged,
            application: Some(application.clone()),
            driver: Some(driver.clone()),
        };
        let candidates = [
            InstallCandidateState {
                is_original: true,
                observed: identity.clone(),
                persisted: Some(persisted.clone()),
            },
            InstallCandidateState {
                is_original: false,
                observed: identity,
                persisted: Some(persisted),
            },
        ];

        assert_eq!(
            select_candidate_index(&candidates, &application, &driver)
                .expect("two aliases for one Pi"),
            0
        );
    }

    #[test]
    fn two_distinct_persisted_pis_with_matching_artifacts_remain_ambiguous() {
        let application = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "1".repeat(40),
            sha256: "2".repeat(64),
        };
        let driver = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "3".repeat(40),
            sha256: "4".repeat(64),
        };
        let first = TargetIdentity {
            host_key_sha256: "SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A".into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef01".into(),
        };
        let second = TargetIdentity {
            host_key_sha256: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef02".into(),
        };
        let state = |target: TargetIdentity| InstallState {
            schema_version: 1,
            target,
            phase: InstallPhase::HostnameChanged,
            application: Some(application.clone()),
            driver: Some(driver.clone()),
        };
        let candidates = [
            InstallCandidateState {
                is_original: true,
                observed: first.clone(),
                persisted: Some(state(first)),
            },
            InstallCandidateState {
                is_original: false,
                observed: second.clone(),
                persisted: Some(state(second)),
            },
        ];

        assert!(select_candidate_index(&candidates, &application, &driver).is_err());
    }

    #[test]
    fn driver_postconditions_are_bound_to_exact_transaction_and_committed_identity() {
        let expected = planeradarctl::driver::DriverPostconditions {
            driver_version: "0.1.0".into(),
            source_revision: "58c42896c8829a034f42a4bc92886dd6f21775a8".into(),
            source_tree: "1111111111111111111111111111111111111111".into(),
            kernel_release: "6.12.47+rpt-rpi-v8".into(),
            module_vermagic: "6.12.47+rpt-rpi-v8 SMP preempt mod_unload aarch64".into(),
            manifest_sha256: "2222222222222222222222222222222222222222222222222222222222222222"
                .into(),
            module_file: "hyperpixel2r_kms.ko".into(),
            module_sha256: "3333333333333333333333333333333333333333333333333333333333333333"
                .into(),
            overlay_file: "hyperpixel2r-kms-e5953b27463c.dtbo".into(),
            overlay_sha256: "4444444444444444444444444444444444444444444444444444444444444444"
                .into(),
            applied_dtb_file: "hyperpixel2r-kms-applied.dtb".into(),
            applied_dtb_sha256: "5555555555555555555555555555555555555555555555555555555555555555"
                .into(),
            replaced_overlay: "vc4-kms-dpi-hyperpixel2r".into(),
        };
        let command =
            staged_driver_transaction_command(&expected).expect("staged transaction command");
        assert!(command.is_interactive_sudo());
        assert_eq!(
            &command.arguments()[4..],
            [
                "planeradar-driver-transaction",
                "0.1.0",
                "58c42896c8829a034f42a4bc92886dd6f21775a8",
                "1111111111111111111111111111111111111111",
                "6.12.47+rpt-rpi-v8",
                "6.12.47+rpt-rpi-v8 SMP preempt mod_unload aarch64",
                "2222222222222222222222222222222222222222222222222222222222222222",
                "hyperpixel2r_kms.ko",
                "3333333333333333333333333333333333333333333333333333333333333333",
                "hyperpixel2r-kms-e5953b27463c.dtbo",
                "4444444444444444444444444444444444444444444444444444444444444444",
                "hyperpixel2r-kms-applied.dtb",
                "5555555555555555555555555555555555555555555555555555555555555555",
                "vc4-kms-dpi-hyperpixel2r",
            ]
        );

        let committed = committed_driver_command(&expected).expect("committed");
        assert!(committed.is_interactive_sudo());
        assert_eq!(
            &committed.arguments()[4..],
            [
                "planeradar-driver-committed",
                "0.1.0",
                "58c42896c8829a034f42a4bc92886dd6f21775a8",
                "1111111111111111111111111111111111111111",
                "6.12.47+rpt-rpi-v8",
                "6.12.47+rpt-rpi-v8 SMP preempt mod_unload aarch64",
                "2222222222222222222222222222222222222222222222222222222222222222",
                "hyperpixel2r_kms.ko",
                "3333333333333333333333333333333333333333333333333333333333333333",
                "hyperpixel2r-kms-e5953b27463c.dtbo",
                "4444444444444444444444444444444444444444444444444444444444444444",
                "hyperpixel2r-kms-applied.dtb",
                "5555555555555555555555555555555555555555555555555555555555555555",
                "vc4-kms-dpi-hyperpixel2r",
            ]
        );
    }

    #[test]
    fn production_adapter_invokes_the_versioned_helper_with_exact_machine_arguments() {
        let digest = "2".repeat(64);
        let helper = format!("/var/lib/planeradar-installer/helpers/{digest}/planeradar");
        let checksum = format!("{helper}.sha256");
        let revision = format!("{helper}.revision");
        let command =
            target_install_command(&helper, &checksum, &revision).expect("target install command");

        assert!(command.is_interactive_sudo());
        assert_eq!(
            command.arguments(),
            [
                "sudo",
                helper.as_str(),
                "install",
                "--artifact",
                helper.as_str(),
                "--checksum-file",
                checksum.as_str(),
                "--revision-file",
                revision.as_str(),
                "--json",
            ]
        );
        let ownership =
            target_install_ownership_command(&helper).expect("target ownership command");
        assert!(ownership.is_interactive_sudo());
        assert_eq!(
            ownership.arguments(),
            ["sudo", helper.as_str(), "installer-ownership"]
        );

        let revision_identity = "1".repeat(40);
        let deploy = deploy_helper_command(
            "/var/tmp/planeradar-upload.ABCDEF1234/payload",
            &helper,
            &digest,
            &revision_identity,
        )
        .expect("deployment command");
        assert!(deploy.is_interactive_sudo());
        assert_eq!(deploy.arguments()[0..3], ["sudo", "sh", "-c"]);
        assert_eq!(
            &deploy.arguments()[4..],
            [
                "planeradar-helper-deploy",
                "/var/tmp/planeradar-upload.ABCDEF1234/payload",
                helper.as_str(),
                digest.as_str(),
                revision_identity.as_str(),
            ]
        );

        let preserved =
            preserve_lifecycle_helper_command(&helper, &digest).expect("preserve helper command");
        assert!(preserved.is_interactive_sudo());
        assert_eq!(preserved.arguments()[0..3], ["sudo", "sh", "-c"]);
        assert_eq!(
            &preserved.arguments()[4..],
            [
                "planeradar-helper-preserve",
                helper.as_str(),
                digest.as_str(),
            ]
        );

        let lifecycle_digest = "3".repeat(64);
        let installer_digest = "4".repeat(64);
        let finalized = finalize_lifecycle_uninstall_command(
            &helper,
            &digest,
            &revision_identity,
            &lifecycle_digest,
            &installer_digest,
        )
        .expect("finalize uninstall command");
        assert!(finalized.is_interactive_sudo());
        assert_eq!(finalized.arguments()[0..3], ["sudo", "sh", "-c"]);
        assert_eq!(
            &finalized.arguments()[4..],
            [
                "planeradar-uninstall-finalize",
                helper.as_str(),
                digest.as_str(),
                revision_identity.as_str(),
                lifecycle_digest.as_str(),
                installer_digest.as_str(),
            ]
        );

        let retired = retire_lifecycle_helper_command(&helper, &digest, &revision_identity)
            .expect("retire helper command");
        assert!(retired.is_interactive_sudo());
        assert_eq!(retired.arguments()[0..3], ["sudo", "sh", "-c"]);
        assert_eq!(
            &retired.arguments()[4..],
            [
                "planeradar-helper-retire",
                helper.as_str(),
                digest.as_str(),
                revision_identity.as_str(),
            ]
        );

        let retire_script = &retired.arguments()[3];
        assert!(!retire_script.contains("find "));
        assert!(retire_script.contains(
            "case \"$helper\" in /var/lib/planeradar-installer/helpers/\"$digest\"/planeradar)"
        ));
        assert!(retire_script.contains("rm -f -- \"$checksum\" \"$revision_file\" \"$helper\""));

        let finalize_script = &finalized.arguments()[3];
        assert!(finalize_script.contains("exact_or_absent \"$helper\""));
        assert!(!finalize_script.contains("find "));
        let helper_delete = finalize_script
            .find("rm -f -- \"$checksum\" \"$revision_file\" \"$helper\"")
            .expect("helper deletion");
        let installer_delete = finalize_script
            .find("rm -f -- \"$installer\"")
            .expect("installer state deletion");
        let lifecycle_delete = finalize_script
            .find("rm -- \"$lifecycle\"")
            .expect("lifecycle state deletion");
        assert!(helper_delete < installer_delete);
        assert!(installer_delete < lifecycle_delete);
    }
}
