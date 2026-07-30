use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const INTERNAL_BOOTSTRAP_ARG: &str = "--__planeradar-bootstrap-v1";
const INTERNAL_FOREGROUND_ARG: &str = "--__planeradar-foreground-tty-v1";
const INTERNAL_RESTORE_ARG: &str = "--__planeradar-restore-tty-v1";
const MARKER_NAME: &str = "control-bootstrap.ready";

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("guarded child")
    }

    fn wait(mut self) -> std::process::ExitStatus {
        let mut child = self.0.take().expect("guarded child");
        child.wait().expect("wait for guarded child")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.0.as_mut() else {
            return;
        };
        let _ = signal_process_group(child.id(), libc::SIGKILL);
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn signal_process_group(pgid: u32, signal: i32) -> bool {
    let Ok(pgid) = i32::try_from(pgid) else {
        return false;
    };
    // SAFETY: a negative, nonzero PID targets the process group with this ID.
    unsafe { libc::kill(-pgid, signal) == 0 }
}

struct PrivateControl {
    _temporary: tempfile::TempDir,
    executable: PathBuf,
    marker: PathBuf,
    continue_marker: PathBuf,
}

fn private_control() -> PrivateControl {
    let temporary = tempfile::tempdir().expect("private control fixture");
    let directory = temporary.path().join("control-private");
    DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("create private control directory");
    fs::copy(
        env!("CARGO_BIN_EXE_planeradarctl"),
        directory.join("planeradarctl"),
    )
    .expect("copy control executable");
    fs::set_permissions(
        directory.join("planeradarctl"),
        fs::Permissions::from_mode(0o700),
    )
    .expect("make copied control executable");
    let marker = directory.join(MARKER_NAME);
    let marker_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&marker)
        .expect("precreate bootstrap marker");
    drop(marker_file);
    let continue_marker = directory.join("control-bootstrap.continue");
    let continue_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&continue_marker)
        .expect("precreate continue marker");
    drop(continue_file);
    PrivateControl {
        _temporary: temporary,
        executable: directory.join("planeradarctl"),
        marker,
        continue_marker,
    }
}

fn spawn_control(executable: &Path, marker: &Path, continue_marker: &Path) -> ChildGuard {
    ChildGuard::new(
        Command::new(executable)
            .arg(INTERNAL_BOOTSTRAP_ARG)
            .arg(marker)
            .arg(continue_marker)
            .arg("--help")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn private bootstrap control"),
    )
}

fn wait_for_marker(marker: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let mut ready = String::new();
        File::open(marker)
            .expect("open bootstrap marker")
            .read_to_string(&mut ready)
            .expect("read bootstrap marker");
        if ready == "ready none\n" {
            return;
        }
        assert!(
            child.try_wait().expect("inspect bootstrap child").is_none(),
            "bootstrap child exited before publishing readiness"
        );
        assert!(
            Instant::now() < deadline,
            "bootstrap child did not publish readiness"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_completion(marker: &Path, child: &mut Child, expected_status: u8) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let expected = format!("ready none\ncomplete {expected_status}\n");
    loop {
        let contents = fs::read_to_string(marker).expect("read completion marker");
        if contents == expected {
            let (_, _, state) = process_snapshot(child.id());
            if state.starts_with('T') {
                return;
            }
        }
        assert!(
            child
                .try_wait()
                .expect("inspect supervisor child")
                .is_none(),
            "supervisor exited before the completion handoff"
        );
        assert!(
            Instant::now() < deadline,
            "supervisor did not publish and stop on completion: {contents:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn process_snapshot(pid: u32) -> (u32, u32, String) {
    let output = Command::new("/bin/ps")
        .args([
            "-o",
            "ppid=",
            "-o",
            "pgid=",
            "-o",
            "state=",
            "-p",
            &pid.to_string(),
        ])
        .output()
        .expect("inspect bootstrap process");
    assert!(output.status.success(), "bootstrap ps failed");
    let text = String::from_utf8(output.stdout).expect("UTF-8 bootstrap ps");
    let mut fields = text.split_whitespace();
    let ppid = fields
        .next()
        .expect("bootstrap PPID")
        .parse()
        .expect("numeric bootstrap PPID");
    let pgid = fields
        .next()
        .expect("bootstrap PGID")
        .parse()
        .expect("numeric bootstrap PGID");
    let state = fields.next().expect("bootstrap state").to_owned();
    assert!(fields.next().is_none(), "unexpected bootstrap ps fields");
    (ppid, pgid, state)
}

fn wait_for_stopped_snapshot(pid: u32) -> (u32, u32, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = process_snapshot(pid);
        assert_eq!(snapshot.0, std::process::id());
        assert_eq!(snapshot.1, pid);
        if snapshot.2.starts_with('T') {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "bootstrap process did not enter stopped state: {}",
            snapshot.2
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_promptly(mut child: ChildGuard) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.child_mut().try_wait().expect("poll bootstrap child") {
            child.0.take();
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "invalid bootstrap invocation did not exit promptly"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn native_bootstrap_stops_before_cli_parsing_in_its_owned_process_group() {
    let fixture = private_control();
    let mut child = spawn_control(
        &fixture.executable,
        &fixture.marker,
        &fixture.continue_marker,
    );
    let pid = child.child_mut().id();
    wait_for_marker(&fixture.marker, child.child_mut());

    let (ppid, pgid, state) = wait_for_stopped_snapshot(pid);
    assert_eq!(ppid, std::process::id());
    assert_eq!(pgid, pid);
    assert!(state.starts_with('T'), "bootstrap state was {state}");

    assert!(signal_process_group(pid, libc::SIGCONT));
    fs::write(&fixture.continue_marker, b"continue\n").expect("acknowledge bootstrap continue");
    wait_for_completion(&fixture.marker, child.child_mut(), 0);
    let (completion_ppid, completion_pgid, completion_state) = process_snapshot(pid);
    assert_eq!(completion_ppid, std::process::id());
    assert_eq!(completion_pgid, pid);
    assert!(completion_state.starts_with('T'));
    assert!(signal_process_group(pid, libc::SIGSTOP));
    assert!(signal_process_group(pid, libc::SIGKILL));
    assert_eq!(
        child.wait().signal(),
        Some(libc::SIGKILL),
        "retained supervisor was not killed"
    );
}

#[test]
fn native_bootstrap_owned_group_can_be_killed_while_still_stopped() {
    let fixture = private_control();
    let mut child = spawn_control(
        &fixture.executable,
        &fixture.marker,
        &fixture.continue_marker,
    );
    let pid = child.child_mut().id();
    wait_for_marker(&fixture.marker, child.child_mut());
    let (_, pgid, state) = wait_for_stopped_snapshot(pid);
    assert_eq!(pgid, pid);
    assert!(state.starts_with('T'));

    assert!(signal_process_group(pid, libc::SIGKILL));
    assert!(!child.wait().success(), "killed bootstrap child succeeded");
}

#[test]
fn native_bootstrap_rejects_marker_paths_outside_its_private_executable_directory() {
    let fixture = private_control();
    let outside = fixture._temporary.path().join("outside");
    fs::write(&outside, b"").expect("outside marker");
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).expect("outside marker mode");
    assert!(
        !wait_promptly(spawn_control(
            &fixture.executable,
            &outside,
            &fixture.continue_marker
        ))
        .success()
    );
}

#[test]
fn native_bootstrap_rejects_symlink_marker_without_touching_its_target() {
    let fixture = private_control();
    fs::remove_file(&fixture.marker).expect("remove regular marker");
    let target = fixture._temporary.path().join("symlink-target");
    fs::write(&target, b"untouched").expect("symlink target");
    symlink(&target, &fixture.marker).expect("bootstrap marker symlink");

    assert!(
        !wait_promptly(spawn_control(
            &fixture.executable,
            &fixture.marker,
            &fixture.continue_marker
        ))
        .success()
    );
    assert_eq!(
        fs::read(&target).expect("read symlink target"),
        b"untouched"
    );
}

#[test]
fn native_bootstrap_rejects_hardlinked_marker_without_touching_its_peer() {
    let fixture = private_control();
    fs::remove_file(&fixture.marker).expect("remove single-link marker");
    let peer = fixture._temporary.path().join("hardlink-peer");
    fs::write(&peer, b"").expect("hardlink peer");
    fs::set_permissions(&peer, fs::Permissions::from_mode(0o600)).expect("hardlink peer mode");
    fs::hard_link(&peer, &fixture.marker).expect("hardlinked bootstrap marker");

    assert!(
        !wait_promptly(spawn_control(
            &fixture.executable,
            &fixture.marker,
            &fixture.continue_marker
        ))
        .success()
    );
    assert_eq!(fs::read(&peer).expect("read hardlink peer"), b"");
}

#[test]
fn native_bootstrap_rejects_non_private_marker_and_directory_modes() {
    for weaken_directory in [false, true] {
        let fixture = private_control();
        if weaken_directory {
            fs::set_permissions(
                fixture.executable.parent().expect("executable parent"),
                fs::Permissions::from_mode(0o755),
            )
            .expect("weaken private directory");
        } else {
            fs::set_permissions(&fixture.marker, fs::Permissions::from_mode(0o644))
                .expect("weaken marker mode");
        }
        assert!(
            !wait_promptly(spawn_control(
                &fixture.executable,
                &fixture.marker,
                &fixture.continue_marker
            ))
            .success()
        );
    }
}

#[test]
fn native_bootstrap_rejects_nonempty_or_wrongly_named_markers() {
    for wrong_name in [false, true] {
        let fixture = private_control();
        let marker = if wrong_name {
            let path = fixture
                .executable
                .parent()
                .expect("executable parent")
                .join("other.ready");
            fs::write(&path, b"").expect("wrongly named marker");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .expect("wrongly named marker mode");
            path
        } else {
            fs::write(&fixture.marker, b"occupied").expect("nonempty marker");
            fixture.marker.clone()
        };
        assert!(
            !wait_promptly(spawn_control(
                &fixture.executable,
                &marker,
                &fixture.continue_marker
            ))
            .success()
        );
    }
}

#[test]
fn native_terminal_helpers_reject_untrusted_saved_process_groups() {
    for action in [INTERNAL_FOREGROUND_ARG, INTERNAL_RESTORE_ARG] {
        let fixture = private_control();
        fs::write(&fixture.marker, b"ready tty 1 2\n").expect("hostile saved process groups");
        let output = Command::new(&fixture.executable)
            .arg(action)
            .arg(&fixture.marker)
            .stdin(Stdio::null())
            .output()
            .expect("run native terminal helper");
        assert!(
            !output.status.success(),
            "hostile terminal helper {action} succeeded"
        );
        assert_eq!(
            fs::read(&fixture.marker).expect("read hostile marker"),
            b"ready tty 1 2\n"
        );
    }
}
