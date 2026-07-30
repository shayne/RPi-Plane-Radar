use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const INTERNAL_BOOTSTRAP_ARG: &str = "--__planeradar-bootstrap-v1";
const INTERNAL_FOREGROUND_ARG: &str = "--__planeradar-foreground-tty-v1";
const INTERNAL_RESTORE_ARG: &str = "--__planeradar-restore-tty-v1";
const MARKER_NAME: &str = "control-bootstrap.ready";

struct PtyFixture {
    _temporary: tempfile::TempDir,
    executable: PathBuf,
    marker: PathBuf,
    continue_marker: PathBuf,
    bin: PathBuf,
    input_record: PathBuf,
    continued_record: PathBuf,
    control_pid_record: PathBuf,
    descendant_pid_record: PathBuf,
    grandchild_pid_record: PathBuf,
    ssh_process_record: PathBuf,
    mutation_record: PathBuf,
    result_record: PathBuf,
    handoff_trace_record: PathBuf,
}

fn pty_fixture() -> PtyFixture {
    let temporary = tempfile::tempdir().expect("PTY fixture");
    let private = temporary.path().join("private");
    let bin = temporary.path().join("bin");
    let home = temporary.path().join("home");
    DirBuilder::new()
        .mode(0o700)
        .create(&private)
        .expect("private executable directory");
    fs::create_dir(&bin).expect("fixture bin");
    fs::create_dir(&home).expect("fixture home");
    fs::create_dir(home.join(".ssh")).expect("fixture ssh directory");
    fs::write(home.join(".ssh").join("known_hosts"), b"").expect("fixture known hosts");
    let executable = private.join("planeradarctl");
    fs::copy(env!("CARGO_BIN_EXE_planeradarctl"), &executable).expect("copy native control");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
        .expect("native control mode");
    let marker = private.join(MARKER_NAME);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&marker)
        .expect("private bootstrap marker");
    let continue_marker = private.join("control-bootstrap.continue");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&continue_marker)
        .expect("private continue marker");
    let input_record = temporary.path().join("input");
    let fake_ssh = bin.join("ssh");
    fs::write(
        &fake_ssh,
        r#"#!/usr/bin/env python3
import os
import signal
import subprocess
import sys
import time

with open(os.environ["PLANERADAR_PTY_SSH_PROCESS_RECORD"], "w", encoding="utf-8") as record:
    record.write(
        f"{os.getpid()} {os.getpgrp()} {os.getppid()} {os.getpgid(os.getppid())}\n"
    )
value = sys.stdin.readline().rstrip("\n")
with open(os.environ["PLANERADAR_PTY_INPUT_RECORD"], "w", encoding="utf-8") as record:
    record.write(f"{value}\n")
print("fixture ssh stdout", flush=True)
print("fixture ssh stderr", file=sys.stderr, flush=True)
descendant_program = r'''
import os
import signal
import subprocess
import sys
for received in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
    signal.signal(received, signal.SIG_IGN)
grandchild_program = r"""
import os
import signal
import time
for received in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
    signal.signal(received, signal.SIG_IGN)
time.sleep(20)
with open(os.environ["PLANERADAR_PTY_MUTATION_RECORD"], "w", encoding="utf-8") as record:
    record.write("delayed mutation\n")
"""
grandchild = subprocess.Popen([sys.executable, "-c", grandchild_program])
with open(os.environ["PLANERADAR_PTY_GRANDCHILD_PID_RECORD"], "w", encoding="utf-8") as record:
    record.write(f"{grandchild.pid}\n")
grandchild.wait()
'''
descendant = subprocess.Popen(
    [sys.executable, "-c", descendant_program],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
    close_fds=True,
)
with open(os.environ["PLANERADAR_PTY_DESCENDANT_PID_RECORD"], "w", encoding="utf-8") as record:
    record.write(f"{descendant.pid}\n")
while not os.path.exists(os.environ["PLANERADAR_PTY_GRANDCHILD_PID_RECORD"]):
    time.sleep(0.005)
sys.exit(37)
"#,
    )
    .expect("fake ssh");
    fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755)).expect("fake ssh mode");
    let fake_ssh_keygen = bin.join("ssh-keygen");
    fs::write(
        &fake_ssh_keygen,
        r#"#!/bin/sh
host=$2
printf '# Host %s found\n%s ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n' "$host" "$host"
"#,
    )
    .expect("fake ssh-keygen");
    fs::set_permissions(&fake_ssh_keygen, fs::Permissions::from_mode(0o755))
        .expect("fake ssh-keygen mode");

    PtyFixture {
        continued_record: temporary.path().join("continued"),
        control_pid_record: temporary.path().join("control-pid"),
        descendant_pid_record: temporary.path().join("descendant-pid"),
        grandchild_pid_record: temporary.path().join("grandchild-pid"),
        ssh_process_record: temporary.path().join("ssh-process"),
        mutation_record: temporary.path().join("mutation"),
        result_record: temporary.path().join("result"),
        handoff_trace_record: temporary.path().join("handoff-trace"),
        _temporary: temporary,
        executable,
        marker,
        continue_marker,
        bin,
        input_record,
    }
}

fn open_pty() -> (File, File) {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    // SAFETY: openpty initializes both file descriptors when it succeeds. The
    // returned descriptors are immediately wrapped in owning File values.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        result,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful openpty returned two distinct owned descriptors.
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}

fn spawn_wrapper(fixture: &PtyFixture, slave: &File) -> Child {
    let script = r#"
set -u
original_pgid="$(/bin/ps -o pgid= -p "$$" | /usr/bin/tr -d '[:space:]')"
"$PLANERADAR_CONTROL" "$PLANERADAR_BOOTSTRAP_ARG" "$PLANERADAR_MARKER" \
  "$PLANERADAR_CONTINUE_MARKER" \
  status pi@radar.local <&0 >&1 2>&2 &
control_pid=$!
printf '%s\n' "$control_pid" >"$PLANERADAR_CONTROL_PID_RECORD"
for attempt in $(/usr/bin/seq 1 300); do
  readiness=""
  IFS= read -r readiness <"$PLANERADAR_MARKER" || true
  state="$(/bin/ps -o state= -p "$control_pid" 2>/dev/null | /usr/bin/tr -d '[:space:]')" || true
  if [[ "$readiness" == ready* && "$state" == T* ]]; then
    break
  fi
  /bin/sleep 0.01
done
/bin/kill -CONT -- "-$control_pid"
"$PLANERADAR_CONTROL" "$PLANERADAR_FOREGROUND_ARG" "$PLANERADAR_MARKER" <&0 >&1 2>&2
foreground_status=$?
parent_handoff_tpgid="$(/bin/ps -o tpgid= -p "$$" | /usr/bin/tr -d '[:space:]')"
"$PLANERADAR_CONTROL" "$PLANERADAR_RESTORE_ARG" "$PLANERADAR_MARKER" <&0 >&1 2>&2
reclaimed_tpgid="$(/bin/ps -o tpgid= -p "$$" | /usr/bin/tr -d '[:space:]')"
printf '%s %s\n' "$parent_handoff_tpgid" "$reclaimed_tpgid" \
  >"$PLANERADAR_HANDOFF_TRACE_RECORD"
printf 'continue\n' >"$PLANERADAR_CONTINUE_MARKER"
printf 'continued\n' >"$PLANERADAR_CONTINUED_RECORD"
for attempt in $(/usr/bin/seq 1 300); do
  completion="$(/usr/bin/awk 'NR == 2 { print }' "$PLANERADAR_MARKER")"
  state="$(/bin/ps -o state= -p "$control_pid" 2>/dev/null | /usr/bin/tr -d '[:space:]')" || true
  descendant_pid=""
  grandchild_pid=""
  descendant_state=""
  grandchild_state=""
  descendant_pgid=""
  grandchild_pgid=""
  [[ ! -s "$PLANERADAR_PTY_DESCENDANT_PID_RECORD" ]] ||
    descendant_pid="$(<"$PLANERADAR_PTY_DESCENDANT_PID_RECORD")"
  [[ ! -s "$PLANERADAR_PTY_GRANDCHILD_PID_RECORD" ]] ||
    grandchild_pid="$(<"$PLANERADAR_PTY_GRANDCHILD_PID_RECORD")"
  [[ -z "$descendant_pid" ]] ||
    descendant_state="$(/bin/ps -o state= -p "$descendant_pid" 2>/dev/null | /usr/bin/tr -d '[:space:]')" || true
  [[ -z "$grandchild_pid" ]] ||
    grandchild_state="$(/bin/ps -o state= -p "$grandchild_pid" 2>/dev/null | /usr/bin/tr -d '[:space:]')" || true
  [[ -z "$descendant_pid" ]] ||
    descendant_pgid="$(/bin/ps -o pgid= -p "$descendant_pid" 2>/dev/null | /usr/bin/tr -d '[:space:]')" || true
  [[ -z "$grandchild_pid" ]] ||
    grandchild_pgid="$(/bin/ps -o pgid= -p "$grandchild_pid" 2>/dev/null | /usr/bin/tr -d '[:space:]')" || true
  if [[ "$completion" == "complete 1" && "$state" == T* &&
        "$descendant_state" == T* && "$grandchild_state" == T* ]]; then
    break
  fi
  /bin/sleep 0.01
done
"$PLANERADAR_CONTROL" "$PLANERADAR_RESTORE_ARG" "$PLANERADAR_MARKER" <&0 >&1 2>&2
restore_status=$?
/bin/kill -STOP -- "-$control_pid"
/bin/kill -KILL -- "-$control_pid"
wait "$control_pid"
control_status=$?
descendants_alive=1
for attempt in $(/usr/bin/seq 1 300); do
  if ! /bin/kill -0 "$descendant_pid" 2>/dev/null &&
     ! /bin/kill -0 "$grandchild_pid" 2>/dev/null; then
    descendants_alive=0
    break
  fi
  /bin/sleep 0.01
done
mutation_present=0
[[ ! -e "$PLANERADAR_PTY_MUTATION_RECORD" ]] || mutation_present=1
read -r ssh_pid ssh_pgid worker_pid worker_pgid <"$PLANERADAR_PTY_SSH_PROCESS_RECORD"
restored_tpgid="$(/bin/ps -o tpgid= -p "$$" | /usr/bin/tr -d '[:space:]')"
printf '%s %s %s %s %s %s %s %s %s %s %s %s %s %s %s\n' \
  "$original_pgid" "$control_status" "$foreground_status" "$restore_status" "$restored_tpgid" \
  "$descendant_state" "$grandchild_state" "$descendants_alive" "$mutation_present" \
  "$descendant_pgid" "$grandchild_pgid" \
  "$ssh_pid" "$ssh_pgid" "$worker_pid" "$worker_pgid" \
  >"$PLANERADAR_RESULT_RECORD"
"#;
    let fixture_path = format!("{}:/usr/bin:/bin", fixture.bin.display());
    let mut command = Command::new("/bin/bash");
    command
        .args(["-c", script])
        .current_dir(fixture._temporary.path())
        .env("HOME", fixture._temporary.path().join("home"))
        .env("PATH", fixture_path)
        .env("PLANERADAR_CONTROL", &fixture.executable)
        .env("PLANERADAR_BOOTSTRAP_ARG", INTERNAL_BOOTSTRAP_ARG)
        .env("PLANERADAR_FOREGROUND_ARG", INTERNAL_FOREGROUND_ARG)
        .env("PLANERADAR_RESTORE_ARG", INTERNAL_RESTORE_ARG)
        .env("PLANERADAR_MARKER", &fixture.marker)
        .env("PLANERADAR_CONTINUE_MARKER", &fixture.continue_marker)
        .env(
            "PLANERADAR_HANDOFF_TRACE_RECORD",
            &fixture.handoff_trace_record,
        )
        .env("PLANERADAR_PTY_INPUT_RECORD", &fixture.input_record)
        .env("PLANERADAR_CONTINUED_RECORD", &fixture.continued_record)
        .env("PLANERADAR_CONTROL_PID_RECORD", &fixture.control_pid_record)
        .env(
            "PLANERADAR_PTY_DESCENDANT_PID_RECORD",
            &fixture.descendant_pid_record,
        )
        .env(
            "PLANERADAR_PTY_GRANDCHILD_PID_RECORD",
            &fixture.grandchild_pid_record,
        )
        .env(
            "PLANERADAR_PTY_SSH_PROCESS_RECORD",
            &fixture.ssh_process_record,
        )
        .env("PLANERADAR_PTY_MUTATION_RECORD", &fixture.mutation_record)
        .env("PLANERADAR_RESULT_RECORD", &fixture.result_record)
        .stdin(Stdio::from(
            slave.try_clone().expect("clone PTY slave stdin"),
        ))
        .stdout(Stdio::from(
            slave.try_clone().expect("clone PTY slave stdout"),
        ))
        .stderr(Stdio::from(
            slave.try_clone().expect("clone PTY slave stderr"),
        ));
    // SAFETY: the closure uses only async-signal-safe libc calls between fork
    // and exec. Standard input has already been connected to the PTY slave.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::tcsetpgrp(0, libc::getpgrp()) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().expect("spawn PTY wrapper")
}

fn spawn_background_wrapper(fixture: &PtyFixture, slave: &File) -> Child {
    let script = r#"
set -m
original_pgid="$(/bin/ps -o pgid= -p "$$" | /usr/bin/tr -d '[:space:]')"
(
  "$PLANERADAR_CONTROL" "$PLANERADAR_BOOTSTRAP_ARG" "$PLANERADAR_MARKER" \
    "$PLANERADAR_CONTINUE_MARKER" \
    --help <&0 >&1 2>&2
) &
background_job=$!
wait "$background_job"
control_status=$?
restored_tpgid="$(/bin/ps -o tpgid= -p "$$" | /usr/bin/tr -d '[:space:]')"
marker_size="$(/usr/bin/stat -f '%z' "$PLANERADAR_MARKER")"
printf '%s %s %s %s\n' \
  "$original_pgid" "$control_status" "$restored_tpgid" "$marker_size" \
  >"$PLANERADAR_RESULT_RECORD"
"#;
    let mut command = Command::new("/bin/bash");
    command
        .args(["-c", script])
        .current_dir(fixture._temporary.path())
        .env("PLANERADAR_CONTROL", &fixture.executable)
        .env("PLANERADAR_BOOTSTRAP_ARG", INTERNAL_BOOTSTRAP_ARG)
        .env("PLANERADAR_MARKER", &fixture.marker)
        .env("PLANERADAR_CONTINUE_MARKER", &fixture.continue_marker)
        .env("PLANERADAR_RESULT_RECORD", &fixture.result_record)
        .stdin(Stdio::from(
            slave.try_clone().expect("clone PTY slave stdin"),
        ))
        .stdout(Stdio::from(
            slave.try_clone().expect("clone PTY slave stdout"),
        ))
        .stderr(Stdio::from(
            slave.try_clone().expect("clone PTY slave stderr"),
        ));
    // SAFETY: the closure uses only async-signal-safe libc calls between fork
    // and exec. Standard input has already been connected to the PTY slave.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::tcsetpgrp(0, libc::getpgrp()) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().expect("spawn background PTY wrapper")
}

fn wait_for_file(path: &Path, deadline: Instant) {
    while !path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(path.exists(), "timed out waiting for {}", path.display());
}

fn kill_fixture(child: &mut Child, fixture: &PtyFixture) {
    if let Ok(pid) = fs::read_to_string(&fixture.control_pid_record) {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", &format!("-{}", pid.trim())])
            .status();
    }
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(2);
    while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_available(master: &mut File) -> String {
    // SAFETY: fcntl operates on this live PTY master descriptor.
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0, "read PTY flags");
    // SAFETY: the descriptor and flags were validated above.
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0,
        "make PTY nonblocking"
    );
    let mut output = Vec::new();
    let _ = master.read_to_end(&mut output);
    String::from_utf8_lossy(&output).into_owned()
}

#[test]
fn native_bootstrap_hands_off_a_controlling_tty_before_inherited_stdin_and_restores_it() {
    let fixture = pty_fixture();
    let (mut master, slave) = open_pty();
    let mut child = spawn_wrapper(&fixture, &slave);
    drop(slave);
    let deadline = Instant::now() + Duration::from_secs(15);
    wait_for_file(&fixture.continued_record, deadline);
    master
        .write_all(b"interactive terminal input\n")
        .expect("write PTY input");
    while !fixture.result_record.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !fixture.result_record.exists() {
        let diagnostic = Command::new("/bin/ps")
            .args([
                "-o",
                "pid=",
                "-o",
                "ppid=",
                "-o",
                "pgid=",
                "-o",
                "tpgid=",
                "-o",
                "state=",
                "-o",
                "command=",
                "-p",
                &child.id().to_string(),
            ])
            .output()
            .expect("PTY timeout diagnostic");
        let output = read_available(&mut master);
        kill_fixture(&mut child, &fixture);
        panic!(
            "native bootstrap hung after inherited PTY input\nwrapper={}\nPTY output={output}",
            String::from_utf8_lossy(&diagnostic.stdout)
        );
    }
    if child
        .try_wait()
        .expect("inspect completed PTY wrapper")
        .is_none()
    {
        let _ = child.kill();
    }
    let output = read_available(&mut master);
    assert_eq!(
        fs::read_to_string(&fixture.input_record)
            .unwrap_or_else(|error| { panic!("interactive input record: {error}; PTY={output}") }),
        "interactive terminal input\n"
    );
    let result = fs::read_to_string(&fixture.result_record).expect("PTY result record");
    let fields = result.split_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.len(), 15, "unexpected PTY result: {result:?}");
    assert_eq!(fields[1], "137", "supervisor was not killed: {result:?}");
    assert_eq!(
        fields[2], "0",
        "native foreground handoff failed: {result:?}"
    );
    assert_eq!(fields[3], "0", "native restore failed: {result:?}");
    assert_eq!(fields[0], fields[4], "foreground PGID was not restored");
    assert!(
        fields[5].starts_with('T') && fields[6].starts_with('T'),
        "native completion did not stop the whole owned group: {result:?}"
    );
    assert_eq!(
        fields[7], "0",
        "completion descendants survived: {result:?}"
    );
    assert_eq!(fields[8], "0", "completion descendant mutated: {result:?}");
    let control_pid = fs::read_to_string(&fixture.control_pid_record).expect("control PID record");
    let handoff_trace =
        fs::read_to_string(&fixture.handoff_trace_record).expect("handoff trace record");
    let handoff_fields = handoff_trace.split_whitespace().collect::<Vec<_>>();
    assert_eq!(
        handoff_fields,
        [control_pid.trim(), fields[0]],
        "fixture did not force control-to-installer foreground reclaim"
    );
    assert_eq!(fields[9], control_pid.trim(), "descendant escaped group");
    assert_eq!(fields[10], control_pid.trim(), "grandchild escaped group");
    assert_eq!(fields[12], control_pid.trim(), "SSH escaped group");
    assert_eq!(fields[14], control_pid.trim(), "worker escaped group");
    assert!(
        output.contains("planeradarctl: target operation transport failed"),
        "missing native PTY stderr: {output}"
    );
}

#[test]
fn native_bootstrap_refuses_to_seize_a_terminal_from_a_background_installer_group() {
    let fixture = pty_fixture();
    let (mut master, slave) = open_pty();
    let mut child = spawn_background_wrapper(&fixture, &slave);
    drop(slave);
    let deadline = Instant::now() + Duration::from_secs(15);
    wait_for_file(&fixture.result_record, deadline);
    let result = fs::read_to_string(&fixture.result_record).expect("background PTY result");
    let fields = result.split_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.len(), 4, "unexpected background result: {result:?}");
    assert_eq!(fields[1], "1", "background bootstrap unexpectedly ran");
    assert_eq!(
        fields[0], fields[2],
        "background bootstrap seized the terminal"
    );
    assert_eq!(fields[3], "0", "background bootstrap claimed readiness");
    let output = read_available(&mut master);
    assert!(
        output.contains("installer is not the terminal foreground process group"),
        "missing background rejection: {output}"
    );
    if child
        .try_wait()
        .expect("inspect background PTY wrapper")
        .is_none()
    {
        let _ = child.kill();
    }
}
