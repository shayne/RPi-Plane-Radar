use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use planeradarctl::DriverLock;
use planeradarctl::operations::{
    CaptureClock, CaptureMetadata, DiagnosticCode, DiagnosticFacts, DoctorReport,
    MAX_CAPTURE_BYTES, OperationError, OperationsBackend, OperationsClient, SshOperationsBackend,
};
use planeradarctl::state::ArtifactIdentity;
use planeradarctl::target::{SshTarget, TargetIdentity};
use planeradarctl::transport::{
    Output, ReconnectPolicy, RemoteCommand, TargetProbe, Transport, TransportError,
};
use sha2::{Digest, Sha256};

const APP_REVISION: &str = "1111111111111111111111111111111111111111";
const APP_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DRIVER_REVISION: &str = "f6213007a8e780309e34b220351fc229e3c7d554";
const DRIVER_MANIFEST: &str = "5f0cd1deba54c740e58b8aee588b3a4b43143e58bc2ad342c9f81cba2cb402e1";
const KERNEL: &str = "6.18.34+rpt-rpi-v8";
const VERMAGIC: &str = "6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64";

fn artifact(version: &str, revision: &str, sha256: &str) -> ArtifactIdentity {
    ArtifactIdentity {
        version: version.into(),
        source_commit: revision.into(),
        sha256: sha256.into(),
    }
}

fn healthy_facts() -> DiagnosticFacts {
    DiagnosticFacts {
        target_model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        target_serial: "1000000012345678".into(),
        expected_target_model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        expected_target_serial: "1000000012345678".into(),
        target_os_id: "raspbian".into(),
        target_os_version: "13".into(),
        target_architecture: "arm64".into(),
        installed_application: artifact("0.1.0", APP_REVISION, APP_SHA256),
        expected_application: artifact("0.1.0", APP_REVISION, APP_SHA256),
        installed_driver: artifact("0.1.0-rc.14", DRIVER_REVISION, DRIVER_MANIFEST),
        expected_driver: artifact("0.1.0-rc.14", DRIVER_REVISION, DRIVER_MANIFEST),
        running_kernel: KERNEL.into(),
        expected_kernel: KERNEL.into(),
        module_name: "hyperpixel2r_kms".into(),
        module_loaded: true,
        module_vermagic: VERMAGIC.into(),
        expected_module_vermagic: VERMAGIC.into(),
        overlay_file: "hyperpixel2r-kms-f6213007a8e7.dtbo".into(),
        expected_overlay_file: "hyperpixel2r-kms-f6213007a8e7.dtbo".into(),
        overlay_configured: true,
        drm_device: "/dev/dri/card0".into(),
        drm_mode: "480x480".into(),
        renderer: "opengles2".into(),
        touch_device: Some("HyperPixel 2.1 Round Touch".into()),
        service_active: true,
        service_restart_count: 0,
        http_healthy: true,
        mdns_hostname: "planeradar.local".into(),
        mdns_reachable: true,
        settings_configured: true,
    }
}

#[derive(Default)]
struct FakeClock {
    now: Cell<Duration>,
}

impl CaptureClock for FakeClock {
    fn now(&self) -> Duration {
        self.now.get()
    }

    fn sleep(&self, duration: Duration) {
        self.now.set(self.now.get() + duration);
    }
}

struct FakeBackend {
    facts: RefCell<Result<DiagnosticFacts, OperationError>>,
    source_metadata: RefCell<VecDeque<Result<Option<CaptureMetadata>, OperationError>>>,
    published_metadata: RefCell<Result<CaptureMetadata, OperationError>>,
    capture: RefCell<Vec<u8>>,
    fetch_error: RefCell<Option<OperationError>>,
    signaled: Cell<bool>,
}

impl FakeBackend {
    fn healthy(capture: Vec<u8>) -> Self {
        let source = source_metadata(10, 20, &capture);
        let fresh = source_metadata(11, 21, &capture);
        let published = published_metadata(30, 40, &capture);
        Self {
            facts: RefCell::new(Ok(healthy_facts())),
            source_metadata: RefCell::new(VecDeque::from([Ok(Some(source)), Ok(Some(fresh))])),
            published_metadata: RefCell::new(Ok(published)),
            capture: RefCell::new(capture),
            fetch_error: RefCell::new(None),
            signaled: Cell::new(false),
        }
    }
}

impl OperationsBackend for FakeBackend {
    fn diagnostic_facts(&self) -> Result<DiagnosticFacts, OperationError> {
        self.facts.borrow().clone()
    }

    fn debug_frame_metadata(&self) -> Result<Option<CaptureMetadata>, OperationError> {
        let mut metadata = self.source_metadata.borrow_mut();
        if metadata.len() > 1 {
            metadata.pop_front().expect("queued metadata")
        } else {
            metadata.front().cloned().unwrap_or(Ok(None))
        }
    }

    fn signal_debug_frame(&self) -> Result<(), OperationError> {
        self.signaled.set(true);
        Ok(())
    }

    fn publish_debug_frame(&self) -> Result<CaptureMetadata, OperationError> {
        self.published_metadata.borrow().clone()
    }

    fn published_frame_metadata(&self) -> Result<CaptureMetadata, OperationError> {
        self.published_metadata.borrow().clone()
    }

    fn fetch_published_frame(&self, destination: &Path) -> Result<(), OperationError> {
        if let Some(error) = self.fetch_error.borrow().clone() {
            return Err(error);
        }
        fs::write(destination, self.capture.borrow().as_slice())
            .map_err(|_| OperationError::LocalIo)
    }
}

fn source_metadata(inode: u64, modified_ns: u64, contents: &[u8]) -> CaptureMetadata {
    CaptureMetadata {
        inode,
        modified_ns,
        size: contents.len() as u64,
        sha256: sha256(contents),
        uid: 1000,
        gid: 1000,
        mode: 0o600,
        links: 1,
        regular: true,
        symlink: false,
    }
}

fn published_metadata(inode: u64, modified_ns: u64, contents: &[u8]) -> CaptureMetadata {
    CaptureMetadata {
        uid: 0,
        gid: 0,
        ..source_metadata(inode, modified_ns, contents)
    }
}

fn sha256(contents: &[u8]) -> String {
    Sha256::digest(contents)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn rgba_png(width: u32, height: u32) -> Vec<u8> {
    png_bytes(
        width,
        height,
        png::ColorType::Rgba,
        vec![0; width as usize * height as usize * 4],
    )
}

fn rgb_png(width: u32, height: u32) -> Vec<u8> {
    png_bytes(
        width,
        height,
        png::ColorType::Rgb,
        vec![0; width as usize * height as usize * 3],
    )
}

fn png_bytes(width: u32, height: u32, color: png::ColorType, pixels: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(&pixels).expect("PNG data");
    }
    bytes
}

#[test]
fn healthy_doctor_report_is_stable_strict_and_redacted() {
    let backend = FakeBackend::healthy(rgba_png(480, 480));
    let client = OperationsClient::new(&backend, FakeClock::default());

    let report = client.doctor().expect("doctor");
    assert!(report.healthy);
    assert_eq!(report.diagnostics, [DiagnosticCode::Healthy]);

    let json = report.to_json().expect("stable JSON");
    assert_eq!(json, report.to_json().expect("repeat JSON"));
    assert!(json.len() < 16 * 1024);
    assert!(json.contains(r#""schema_version":1"#));
    assert!(json.contains(r#""settings_configured":true"#));
    assert!(!json.contains("latitude"));
    assert!(!json.contains("longitude"));
    assert!(!json.contains("password"));
    assert!(!json.contains("/Users/"));
    assert_eq!(
        DoctorReport::from_json(json.as_bytes()).expect("parse"),
        report
    );

    let unknown = json.replacen(
        r#"{"schema_version":1,"#,
        r#"{"schema_version":1,"extra":true,"#,
        1,
    );
    assert!(DoctorReport::from_json(unknown.as_bytes()).is_err());
    let duplicate = json.replacen(
        r#"{"schema_version":1,"#,
        r#"{"schema_version":1,"schema_version":1,"#,
        1,
    );
    assert!(DoctorReport::from_json(duplicate.as_bytes()).is_err());
    let mut oversized = json.into_bytes();
    oversized.resize(32 * 1024 + 1, b' ');
    assert!(DoctorReport::from_json(&oversized).is_err());
}

#[test]
fn every_required_health_mismatch_has_a_distinct_stable_diagnostic() {
    type FactMutation = Box<dyn FnOnce(&mut DiagnosticFacts)>;
    let cases: Vec<(&str, FactMutation, DiagnosticCode)> = vec![
        (
            "target platform",
            Box::new(|facts| facts.target_architecture = "armhf".into()),
            DiagnosticCode::TargetPlatformMismatch,
        ),
        (
            "application revision",
            Box::new(|facts| facts.installed_application.source_commit = "3".repeat(40)),
            DiagnosticCode::ApplicationRevisionMismatch,
        ),
        (
            "application checksum",
            Box::new(|facts| facts.installed_application.sha256 = "c".repeat(64)),
            DiagnosticCode::ApplicationChecksumMismatch,
        ),
        (
            "driver version",
            Box::new(|facts| facts.installed_driver.version = "0.1.0-rc.13".into()),
            DiagnosticCode::DriverVersionMismatch,
        ),
        (
            "driver revision",
            Box::new(|facts| facts.installed_driver.source_commit = "4".repeat(40)),
            DiagnosticCode::DriverRevisionMismatch,
        ),
        (
            "driver manifest",
            Box::new(|facts| facts.installed_driver.sha256 = "d".repeat(64)),
            DiagnosticCode::DriverManifestMismatch,
        ),
        (
            "kernel",
            Box::new(|facts| facts.running_kernel = "6.18.35+rpt-rpi-v8".into()),
            DiagnosticCode::KernelMismatch,
        ),
        (
            "module missing",
            Box::new(|facts| facts.module_loaded = false),
            DiagnosticCode::ModuleMismatch,
        ),
        (
            "module vermagic",
            Box::new(|facts| facts.module_vermagic = "wrong".into()),
            DiagnosticCode::ModuleMismatch,
        ),
        (
            "overlay",
            Box::new(|facts| facts.overlay_configured = false),
            DiagnosticCode::OverlayMismatch,
        ),
        (
            "service",
            Box::new(|facts| facts.service_active = false),
            DiagnosticCode::ServiceInactive,
        ),
        (
            "restart count",
            Box::new(|facts| facts.service_restart_count = 1),
            DiagnosticCode::UnexpectedRestartCount,
        ),
        (
            "HTTP",
            Box::new(|facts| facts.http_healthy = false),
            DiagnosticCode::HttpFailure,
        ),
        (
            "touch",
            Box::new(|facts| facts.touch_device = None),
            DiagnosticCode::TouchMissing,
        ),
        (
            "DRM mode",
            Box::new(|facts| facts.drm_mode = "640x480".into()),
            DiagnosticCode::DrmModeWrong,
        ),
        (
            "renderer",
            Box::new(|facts| facts.renderer = "software".into()),
            DiagnosticCode::RendererWrong,
        ),
        (
            "mDNS",
            Box::new(|facts| facts.mdns_reachable = false),
            DiagnosticCode::MdnsFailure,
        ),
    ];

    for (name, mutate, expected) in cases {
        let backend = FakeBackend::healthy(rgba_png(480, 480));
        let mut facts = healthy_facts();
        mutate(&mut facts);
        *backend.facts.borrow_mut() = Ok(facts);
        let report = OperationsClient::new(&backend, FakeClock::default())
            .doctor()
            .expect("doctor report");
        assert!(!report.healthy, "{name}");
        assert!(report.diagnostics.contains(&expected), "{name}: {report:?}");
    }
}

#[test]
fn status_is_concise_on_success_and_typed_on_failure() {
    let backend = FakeBackend::healthy(rgba_png(480, 480));
    let client = OperationsClient::new(&backend, FakeClock::default());
    assert_eq!(
        client.status().expect("healthy status").to_string(),
        "Plane Radar healthy: app 0.1.0@111111111111, driver 0.1.0-rc.14@f6213007a8e7, 480x480 opengles2"
    );

    let mut facts = healthy_facts();
    facts.http_healthy = false;
    *backend.facts.borrow_mut() = Ok(facts);
    assert_eq!(
        client.status().expect_err("unhealthy status"),
        OperationError::Unhealthy(DiagnosticCode::HttpFailure)
    );
}

#[test]
fn screenshot_accepts_only_a_fresh_valid_480_by_480_rgba_png() {
    let bytes = rgba_png(480, 480);
    let backend = FakeBackend::healthy(bytes.clone());
    let directory = tempfile::tempdir().expect("temporary directory");
    let destination = directory.path().join("radar.png");

    let result = OperationsClient::new(&backend, FakeClock::default())
        .screenshot(&destination, Duration::from_secs(2))
        .expect("capture");

    assert!(backend.signaled.get());
    assert_eq!(fs::read(&destination).expect("destination"), bytes);
    assert_eq!(result.destination, destination);
    assert_eq!(result.sha256, sha256(&bytes));
}

#[test]
fn screenshot_rejects_wrong_dimensions_non_rgba_and_corrupt_png() {
    let cases = [
        (
            "wrong dimensions",
            rgba_png(479, 480),
            OperationError::WrongPngDimensions,
        ),
        (
            "non RGBA",
            rgb_png(480, 480),
            OperationError::WrongPngFormat,
        ),
        (
            "truncated",
            rgba_png(480, 480)[..80].to_vec(),
            OperationError::InvalidPng,
        ),
        ("corrupt", b"not a png".to_vec(), OperationError::InvalidPng),
    ];
    for (name, bytes, expected) in cases {
        let backend = FakeBackend::healthy(bytes);
        let directory = tempfile::tempdir().expect("temporary directory");
        let destination = directory.path().join("radar.png");
        assert_eq!(
            OperationsClient::new(&backend, FakeClock::default())
                .screenshot(&destination, Duration::from_secs(2))
                .expect_err(name),
            expected,
            "{name}"
        );
        assert!(!destination.exists(), "{name}");
    }
}

#[test]
fn screenshot_rejects_unsafe_remote_metadata_stale_data_size_and_timeout() {
    let bytes = rgba_png(480, 480);
    let unsafe_backend = FakeBackend::healthy(bytes.clone());
    unsafe_backend
        .source_metadata
        .borrow_mut()
        .front_mut()
        .expect("metadata")
        .as_mut()
        .expect("metadata")
        .as_mut()
        .expect("metadata")
        .symlink = true;
    let directory = tempfile::tempdir().expect("temporary directory");
    assert_eq!(
        OperationsClient::new(&unsafe_backend, FakeClock::default())
            .screenshot(&directory.path().join("unsafe.png"), Duration::from_secs(1))
            .expect_err("symlink"),
        OperationError::UnsafeRemoteCapture
    );
    assert!(!unsafe_backend.signaled.get());

    let stale_backend = FakeBackend::healthy(bytes.clone());
    let stale = stale_backend
        .source_metadata
        .borrow()
        .front()
        .expect("metadata")
        .clone();
    *stale_backend.source_metadata.borrow_mut() = VecDeque::from([stale]);
    assert_eq!(
        OperationsClient::new(&stale_backend, FakeClock::default())
            .screenshot(
                &directory.path().join("stale.png"),
                Duration::from_millis(250)
            )
            .expect_err("stale"),
        OperationError::CaptureTimedOut
    );

    let oversized_backend = FakeBackend::healthy(bytes.clone());
    oversized_backend
        .published_metadata
        .borrow_mut()
        .as_mut()
        .expect("metadata")
        .size = MAX_CAPTURE_BYTES + 1;
    assert_eq!(
        OperationsClient::new(&oversized_backend, FakeClock::default())
            .screenshot(&directory.path().join("large.png"), Duration::from_secs(1))
            .expect_err("oversized"),
        OperationError::CaptureTooLarge
    );

    let absent_backend = FakeBackend::healthy(bytes);
    *absent_backend.source_metadata.borrow_mut() = VecDeque::from([Ok(None)]);
    assert_eq!(
        OperationsClient::new(&absent_backend, FakeClock::default())
            .screenshot(
                &directory.path().join("absent.png"),
                Duration::from_millis(250)
            )
            .expect_err("no fresh capture"),
        OperationError::CaptureTimedOut
    );
}

#[test]
fn screenshot_requires_root_owned_stable_published_file() {
    let bytes = rgba_png(480, 480);
    let backend = FakeBackend::healthy(bytes.clone());
    backend
        .published_metadata
        .borrow_mut()
        .as_mut()
        .expect("metadata")
        .uid = 1000;
    let directory = tempfile::tempdir().expect("temporary directory");
    assert_eq!(
        OperationsClient::new(&backend, FakeClock::default())
            .screenshot(&directory.path().join("owner.png"), Duration::from_secs(1))
            .expect_err("owner"),
        OperationError::UnsafeRemoteCapture
    );

    let changed = FakeBackend::healthy(bytes);
    let original = changed
        .published_metadata
        .borrow()
        .as_ref()
        .expect("metadata")
        .clone();
    // The fake changes the remote identity after transfer.
    changed.capture.replace(rgba_png(480, 480));
    let _ = changed.published_metadata.replace(Ok(CaptureMetadata {
        inode: original.inode + 1,
        ..original
    }));
    // A dedicated backend wrapper exercises the post-copy metadata mismatch.
    struct ChangingBackend<'a> {
        inner: &'a FakeBackend,
        reads: Cell<u8>,
    }
    impl OperationsBackend for ChangingBackend<'_> {
        fn diagnostic_facts(&self) -> Result<DiagnosticFacts, OperationError> {
            self.inner.diagnostic_facts()
        }
        fn debug_frame_metadata(&self) -> Result<Option<CaptureMetadata>, OperationError> {
            self.inner.debug_frame_metadata()
        }
        fn signal_debug_frame(&self) -> Result<(), OperationError> {
            self.inner.signal_debug_frame()
        }
        fn publish_debug_frame(&self) -> Result<CaptureMetadata, OperationError> {
            let current = self.inner.publish_debug_frame()?;
            Ok(CaptureMetadata {
                inode: current.inode - 1,
                ..current
            })
        }
        fn published_frame_metadata(&self) -> Result<CaptureMetadata, OperationError> {
            self.reads.set(self.reads.get() + 1);
            self.inner.published_frame_metadata()
        }
        fn fetch_published_frame(&self, destination: &Path) -> Result<(), OperationError> {
            self.inner.fetch_published_frame(destination)
        }
    }
    let changing = ChangingBackend {
        inner: &changed,
        reads: Cell::new(0),
    };
    assert_eq!(
        OperationsClient::new(&changing, FakeClock::default())
            .screenshot(
                &directory.path().join("changed.png"),
                Duration::from_secs(1)
            )
            .expect_err("changed published capture"),
        OperationError::RemoteCaptureChanged
    );
}

#[cfg(unix)]
#[test]
fn screenshot_rejects_destination_symlink_and_preserves_existing_file_on_failure() {
    use std::os::unix::fs::symlink;

    let bytes = rgba_png(480, 480);
    let directory = tempfile::tempdir().expect("temporary directory");
    let victim = directory.path().join("victim.png");
    fs::write(&victim, b"victim").expect("victim");
    let link = directory.path().join("radar.png");
    symlink(&victim, &link).expect("symlink");
    let backend = FakeBackend::healthy(bytes.clone());
    assert_eq!(
        OperationsClient::new(&backend, FakeClock::default())
            .screenshot(&link, Duration::from_secs(1))
            .expect_err("destination symlink"),
        OperationError::UnsafeLocalDestination
    );
    assert_eq!(fs::read(&victim).expect("victim"), b"victim");

    let real_parent = directory.path().join("real-parent");
    let nested = real_parent.join("nested");
    fs::create_dir_all(&nested).expect("real nested parent");
    let linked_parent = directory.path().join("linked-parent");
    symlink(&real_parent, &linked_parent).expect("parent symlink");
    let backend = FakeBackend::healthy(bytes.clone());
    assert_eq!(
        OperationsClient::new(&backend, FakeClock::default())
            .screenshot(
                &linked_parent.join("nested").join("radar.png"),
                Duration::from_secs(1)
            )
            .expect_err("ancestor symlink"),
        OperationError::UnsafeLocalDestination
    );

    let destination = directory.path().join("existing.png");
    fs::write(&destination, b"keep me").expect("existing");
    let failing = FakeBackend::healthy(bytes.clone());
    *failing.fetch_error.borrow_mut() = Some(OperationError::Transport);
    assert_eq!(
        OperationsClient::new(&failing, FakeClock::default())
            .screenshot(&destination, Duration::from_secs(1))
            .expect_err("transfer failure"),
        OperationError::Transport
    );
    assert_eq!(fs::read(&destination).expect("existing"), b"keep me");

    let invalid = FakeBackend::healthy(rgb_png(480, 480));
    assert_eq!(
        OperationsClient::new(&invalid, FakeClock::default())
            .screenshot(&destination, Duration::from_secs(1))
            .expect_err("invalid replacement"),
        OperationError::WrongPngFormat
    );
    assert_eq!(
        fs::read(&destination).expect("preserved existing"),
        b"keep me"
    );
}

#[derive(Default)]
struct RecordingTransport {
    outputs: RefCell<VecDeque<Result<Output, TransportError>>>,
    commands: RefCell<Vec<Vec<String>>>,
    copies: RefCell<Vec<(PathBuf, PathBuf)>>,
}

impl Transport for RecordingTransport {
    fn probe(&self, _target: &SshTarget) -> Result<TargetProbe, TransportError> {
        Ok(TargetProbe {
            identity: TargetIdentity {
                host_key_sha256: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
                serial: "1000000012345678".into(),
            },
        })
    }

    fn run(&self, _target: &SshTarget, request: RemoteCommand) -> Result<Output, TransportError> {
        self.commands
            .borrow_mut()
            .push(request.arguments().to_vec());
        self.outputs
            .borrow_mut()
            .pop_front()
            .expect("queued output")
    }

    fn copy_to(
        &self,
        _target: &SshTarget,
        _local: &Path,
        _remote: &Path,
    ) -> Result<(), TransportError> {
        panic!("operations never upload")
    }

    fn copy_from(
        &self,
        _target: &SshTarget,
        remote: &Path,
        local: &Path,
    ) -> Result<(), TransportError> {
        self.copies
            .borrow_mut()
            .push((remote.to_owned(), local.to_owned()));
        Ok(())
    }

    fn wait_for_reboot(
        &self,
        _identity: &TargetIdentity,
        _addresses: &[SshTarget],
        _policy: ReconnectPolicy,
    ) -> Result<SshTarget, TransportError> {
        panic!("operations never reboot")
    }
}

fn target_state_json() -> Vec<u8> {
    format!(
        r#"{{"schema_version":1,"hardware":{{"model":"Raspberry Pi Zero 2 W Rev 1.0","serial":"1000000012345678"}},"application":{{"version":"0.1.0","source_commit":"{APP_REVISION}","sha256":"{APP_SHA256}"}},"driver":{{"version":"0.1.0-rc.14","source_commit":"{DRIVER_REVISION}","sha256":"{DRIVER_MANIFEST}"}},"owned_files":[{{"target_path":"/opt/planeradar/bin/planeradar","sha256":"{APP_SHA256}"}}],"last_verified_phase":"complete"}}"#
    )
    .into_bytes()
}

fn diagnostic_probe_json() -> Vec<u8> {
    format!(
        r#"{{"schema_version":1,"os_id":"raspbian","os_version":"13","architecture":"arm64","application_version":"0.1.0","application_revision":"{APP_REVISION}","application_sha256":"{APP_SHA256}","expected_kernel":"{KERNEL}","running_kernel":"{KERNEL}","module_loaded":true,"module_vermagic":"{VERMAGIC}","expected_module_vermagic":"{VERMAGIC}","overlay_file":"hyperpixel2r-kms-f6213007a8e7.dtbo","expected_overlay_file":"hyperpixel2r-kms-f6213007a8e7.dtbo","overlay_configured":true,"drm_device":"/dev/dri/card0","drm_mode":"480x480","renderer":"opengles2","touch_device":"HyperPixel 2.1 Round Touch","service_active":true,"service_restart_count":0,"http_healthy":true,"hostname":"planeradar","settings_configured":true}}"#
    )
    .into_bytes()
}

#[test]
fn production_backend_collects_strict_fixed_diagnostics_without_mutating_target() {
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state_json(), Vec::new())),
        Ok(Output::success(diagnostic_probe_json(), Vec::new())),
    ]);
    let target: SshTarget = "shayne@planeradar.local".parse().expect("target");
    let backend = SshOperationsBackend::new(
        &transport,
        target,
        DriverLock::checked_in().expect("driver lock"),
    );

    let report = OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("doctor");

    assert!(report.healthy, "{report:?}");
    let commands = transport.commands.borrow();
    assert_eq!(commands.len(), 2);
    assert!(commands.iter().all(|command| {
        command.first().map(String::as_str) == Some("/usr/bin/timeout")
            && command
                .get(1)
                .is_some_and(|seconds| matches!(seconds.as_str(), "10" | "15"))
    }));
    assert_eq!(
        &commands[0][2..6],
        [
            "sudo",
            "-n",
            "/opt/planeradar/bin/planeradar",
            "installer-state"
        ]
    );
    assert_eq!(commands[0].last().map(String::as_str), Some("read"));
    assert_eq!(&commands[1][2..5], ["sudo", "-n", "sh"]);
    assert!(
        commands[1]
            .iter()
            .any(|argument| argument == "planeradar-diagnostics")
    );
    let flattened = commands.concat().join(" ");
    assert!(!flattened.contains("latitude"));
    assert!(!flattened.contains("longitude"));
}

#[test]
fn production_backend_rejects_unknown_or_oversized_probe_output() {
    for probe in [
        br#"{"schema_version":1,"unknown":true}"#.to_vec(),
        vec![b'x'; 32 * 1024 + 1],
    ] {
        let transport = RecordingTransport::default();
        transport.outputs.borrow_mut().extend([
            Ok(Output::success(target_state_json(), Vec::new())),
            Ok(Output::success(probe, Vec::new())),
        ]);
        let backend = SshOperationsBackend::new(
            &transport,
            "shayne@planeradar.local".parse().expect("target"),
            DriverLock::checked_in().expect("driver lock"),
        );
        assert_eq!(
            backend.diagnostic_facts().expect_err("malformed probe"),
            OperationError::MalformedFacts
        );
    }
}

#[test]
fn production_capture_adapter_uses_only_systemd_and_fixed_remote_paths() {
    let bytes = rgba_png(480, 480);
    let source = serde_json::to_vec(&serde_json::json!({
        "inode": 1,
        "modified_ns": 2,
        "size": bytes.len(),
        "sha256": sha256(&bytes),
        "uid": 1000,
        "gid": 1000,
        "mode": 384,
        "links": 1,
        "regular": true,
        "symlink": false
    }))
    .expect("metadata");
    let published = serde_json::to_vec(&serde_json::json!({
        "inode": 3,
        "modified_ns": 4,
        "size": bytes.len(),
        "sha256": sha256(&bytes),
        "uid": 0,
        "gid": 0,
        "mode": 384,
        "links": 1,
        "regular": true,
        "symlink": false
    }))
    .expect("metadata");
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(source, Vec::new())),
        Ok(Output::success(Vec::new(), Vec::new())),
        Ok(Output::success(published.clone(), Vec::new())),
        Ok(Output::success(published, Vec::new())),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "shayne@planeradar.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );
    backend.debug_frame_metadata().expect("metadata");
    backend.signal_debug_frame().expect("signal");
    backend.publish_debug_frame().expect("publish");
    backend.published_frame_metadata().expect("published");
    let destination = Path::new("/tmp/planeradar-test-capture.png");
    backend
        .fetch_published_frame(destination)
        .expect("copy fixed capture");

    let commands = transport.commands.borrow();
    assert_eq!(
        commands[1][2..],
        [
            "sudo",
            "-n",
            "systemctl",
            "kill",
            "--signal=SIGUSR1",
            "planeradar.service"
        ]
    );
    assert!(
        commands[0]
            .iter()
            .any(|value| value == "/var/lib/planeradar/debug.png")
    );
    assert!(
        commands[2]
            .iter()
            .any(|value| value == "/var/lib/planeradar-installer/captures/current.png")
    );
    assert_eq!(
        transport.copies.borrow()[0],
        (
            PathBuf::from("/var/lib/planeradar-installer/captures/current.png"),
            destination.to_owned()
        )
    );
}
