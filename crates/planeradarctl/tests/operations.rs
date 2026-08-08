use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use planeradarctl::DriverLock;
use planeradarctl::operations::{
    CaptureClock, CaptureMetadata, CaptureTransfer, DiagnosticCode, DiagnosticFacts, DoctorReport,
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
const DRIVER_VERSION: &str = "0.2.0";
const DRIVER_REVISION: &str = "b856694572316d0f485401af4d555a4ec7a8fe86";
const DRIVER_MANIFEST: &str = "0aa224ac2e175ee187182d8f32d32fa66741d5530a1d43032756716b90cca8e8";
const INSTALLED_DRIVER_MANIFEST: &str =
    "37b40967b952de49bf7663ffdae48e4208d1a6618ffc966d84c7f7a2176d969e";
const MODULE_SHA256: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OVERLAY_SHA256: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const BOOT_CONFIG_SHA256: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const KERNEL: &str = "6.18.34+rpt-rpi-v8";
const VERMAGIC: &str = "6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64";
const TEST_PUBLIC_KEY: &str =
    "AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

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
        running_application_revision: APP_REVISION.into(),
        installed_driver: artifact(DRIVER_VERSION, DRIVER_REVISION, DRIVER_MANIFEST),
        accepted_driver_manifest_sha256: DRIVER_MANIFEST.into(),
        persisted_driver_manifest_sha256: DRIVER_MANIFEST.into(),
        expected_driver: artifact(DRIVER_VERSION, DRIVER_REVISION, DRIVER_MANIFEST),
        running_kernel: KERNEL.into(),
        expected_kernel: KERNEL.into(),
        module_name: "hyperpixel2r_kms".into(),
        module_loaded: true,
        module_vermagic: VERMAGIC.into(),
        expected_module_vermagic: VERMAGIC.into(),
        module_sha256: MODULE_SHA256.into(),
        expected_module_sha256: MODULE_SHA256.into(),
        overlay_file: "hyperpixel2r-kms-f6213007a8e7.dtbo".into(),
        expected_overlay_file: "hyperpixel2r-kms-f6213007a8e7.dtbo".into(),
        overlay_sha256: OVERLAY_SHA256.into(),
        expected_overlay_sha256: OVERLAY_SHA256.into(),
        boot_config_sha256: BOOT_CONFIG_SHA256.into(),
        normal_kernel_file: None,
        normal_kernel_sha256: None,
        normal_initramfs_file: None,
        normal_initramfs_sha256: None,
        base_dtb_sha256: None,
        vc4_overlay_sha256: None,
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

#[derive(Clone, Default)]
struct FakeClock {
    now: Rc<Cell<Duration>>,
}

impl CaptureClock for FakeClock {
    fn now(&self) -> Duration {
        self.now.get()
    }
}

impl FakeClock {
    fn advance(&self, duration: Duration) {
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

    fn debug_frame_metadata(
        &self,
        _timeout: Duration,
    ) -> Result<Option<CaptureMetadata>, OperationError> {
        let mut metadata = self.source_metadata.borrow_mut();
        if metadata.len() > 1 {
            metadata.pop_front().expect("queued metadata")
        } else {
            metadata.front().cloned().unwrap_or(Ok(None))
        }
    }

    fn signal_debug_frame(&self, _timeout: Duration) -> Result<(), OperationError> {
        self.signaled.set(true);
        Ok(())
    }

    fn capture_debug_frame(
        &self,
        before: Option<&CaptureMetadata>,
        _timeout: Duration,
    ) -> Result<CaptureTransfer, OperationError> {
        if let Some(error) = self.fetch_error.borrow().clone() {
            return Err(error);
        }
        let source = self
            .debug_frame_metadata(Duration::from_secs(1))?
            .ok_or(OperationError::CaptureTimedOut)?;
        if before.is_some_and(|before| {
            source.inode == before.inode || source.modified_ns < before.modified_ns
        }) {
            return Err(OperationError::CaptureTimedOut);
        }
        let published = self.published_metadata.borrow().clone()?;
        Ok(CaptureTransfer {
            source,
            published: published.clone(),
            rechecked: published,
            bytes: self.capture.borrow().clone(),
        })
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

fn capture_protocol(
    source: &CaptureMetadata,
    published: &CaptureMetadata,
    contents: &[u8],
) -> Vec<u8> {
    let header = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "source": source,
        "published": published,
    }))
    .expect("capture header");
    let footer = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "rechecked": published,
    }))
    .expect("capture footer");
    let mut output = Vec::new();
    output.extend_from_slice(&(header.len() as u32).to_be_bytes());
    output.extend_from_slice(&header);
    output.extend_from_slice(contents);
    output.extend_from_slice(&(footer.len() as u32).to_be_bytes());
    output.extend_from_slice(&footer);
    output
}

fn capture_protocol_with_unknown_header_field(mut protocol: Vec<u8>) -> Vec<u8> {
    let header_length =
        u32::from_be_bytes(protocol[..4].try_into().expect("header length")) as usize;
    let mut header: serde_json::Value =
        serde_json::from_slice(&protocol[4..4 + header_length]).expect("capture header");
    header
        .as_object_mut()
        .expect("capture header object")
        .insert("unexpected".into(), serde_json::Value::Bool(true));
    let header = serde_json::to_vec(&header).expect("mutated header");
    let rest = protocol.split_off(4 + header_length);
    let mut output = Vec::new();
    output.extend_from_slice(&(header.len() as u32).to_be_bytes());
    output.extend_from_slice(&header);
    output.extend_from_slice(&rest);
    output
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
            "running application revision",
            Box::new(|facts| facts.running_application_revision = "3".repeat(40)),
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
            "accepted driver manifest",
            Box::new(|facts| facts.accepted_driver_manifest_sha256 = "f".repeat(64)),
            DiagnosticCode::DriverManifestMismatch,
        ),
        (
            "persisted driver manifest",
            Box::new(|facts| facts.persisted_driver_manifest_sha256 = "e".repeat(64)),
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
            "module digest",
            Box::new(|facts| facts.module_sha256 = "e".repeat(64)),
            DiagnosticCode::ModuleMismatch,
        ),
        (
            "overlay",
            Box::new(|facts| facts.overlay_configured = false),
            DiagnosticCode::OverlayMismatch,
        ),
        (
            "overlay digest",
            Box::new(|facts| facts.overlay_sha256 = "e".repeat(64)),
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
            "DRM device",
            Box::new(|facts| facts.drm_device = "unavailable".into()),
            DiagnosticCode::DrmDeviceWrong,
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
        format!(
            "Plane Radar healthy: app 0.1.0@111111111111, driver {DRIVER_VERSION}@{}, 480x480 opengles2",
            &DRIVER_REVISION[..12]
        )
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
fn boot_provenance_is_optional_and_does_not_replace_public_driver_identity() {
    let backend = FakeBackend::healthy(rgba_png(480, 480));
    let mut facts = healthy_facts();
    facts.normal_kernel_file = Some("kernel8.img".into());
    facts.normal_kernel_sha256 = Some("a".repeat(64));
    facts.normal_initramfs_file = Some("initramfs8".into());
    facts.normal_initramfs_sha256 = Some("b".repeat(64));
    facts.base_dtb_sha256 = Some("c".repeat(64));
    facts.vc4_overlay_sha256 = Some("d".repeat(64));
    *backend.facts.borrow_mut() = Ok(facts.clone());

    let report = OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("schema-4 boot provenance");

    assert_eq!(
        report.facts.normal_kernel_file.as_deref(),
        Some("kernel8.img")
    );
    assert_eq!(
        report.facts.normal_initramfs_file.as_deref(),
        Some("initramfs8")
    );
    assert_eq!(report.facts.installed_driver, facts.installed_driver);
    assert_eq!(
        report.facts.accepted_driver_manifest_sha256,
        facts.accepted_driver_manifest_sha256
    );
}

#[test]
fn screenshot_accepts_only_a_fresh_valid_480_by_480_rgba_png() {
    let bytes = rgba_png(480, 480);
    let backend = FakeBackend::healthy(bytes.clone());
    let directory = tempfile::tempdir().expect("temporary directory");
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");
    let destination = directory_path.join("radar.png");

    let result = OperationsClient::new(&backend, FakeClock::default())
        .screenshot(&destination, Duration::from_secs(2))
        .expect("capture");

    assert!(backend.signaled.get());
    assert_eq!(fs::read(&destination).expect("destination"), bytes);
    assert_eq!(result.destination, destination);
    assert_eq!(result.sha256, sha256(&bytes));
}

#[test]
fn screenshot_accepts_the_default_bare_relative_output_name() {
    let bytes = rgba_png(480, 480);
    let backend = FakeBackend::healthy(bytes.clone());
    let destination_file = tempfile::Builder::new()
        .prefix(".planeradar-operation-test-")
        .suffix(".png")
        .tempfile_in(".")
        .expect("temporary destination");
    let destination = PathBuf::from(
        destination_file
            .path()
            .file_name()
            .expect("bare destination name"),
    );

    OperationsClient::new(&backend, FakeClock::default())
        .screenshot(&destination, Duration::from_secs(2))
        .expect("bare relative capture");

    assert_eq!(fs::read(&destination).expect("destination"), bytes);
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
        let directory_path =
            fs::canonicalize(directory.path()).expect("canonical temporary directory");
        let destination = directory_path.join("radar.png");
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
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");
    assert_eq!(
        OperationsClient::new(&unsafe_backend, FakeClock::default())
            .screenshot(&directory_path.join("unsafe.png"), Duration::from_secs(1))
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
                &directory_path.join("stale.png"),
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
            .screenshot(&directory_path.join("large.png"), Duration::from_secs(1))
            .expect_err("oversized"),
        OperationError::CaptureTooLarge
    );

    let absent_backend = FakeBackend::healthy(bytes);
    *absent_backend.source_metadata.borrow_mut() = VecDeque::from([Ok(None)]);
    assert_eq!(
        OperationsClient::new(&absent_backend, FakeClock::default())
            .screenshot(
                &directory_path.join("absent.png"),
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
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");
    assert_eq!(
        OperationsClient::new(&backend, FakeClock::default())
            .screenshot(&directory_path.join("owner.png"), Duration::from_secs(1))
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
        fn debug_frame_metadata(
            &self,
            timeout: Duration,
        ) -> Result<Option<CaptureMetadata>, OperationError> {
            self.inner.debug_frame_metadata(timeout)
        }
        fn signal_debug_frame(&self, timeout: Duration) -> Result<(), OperationError> {
            self.inner.signal_debug_frame(timeout)
        }
        fn capture_debug_frame(
            &self,
            before: Option<&CaptureMetadata>,
            timeout: Duration,
        ) -> Result<CaptureTransfer, OperationError> {
            let mut capture = self.inner.capture_debug_frame(before, timeout)?;
            self.reads.set(self.reads.get() + 1);
            capture.rechecked.inode += 1;
            Ok(capture)
        }
    }
    let changing = ChangingBackend {
        inner: &changed,
        reads: Cell::new(0),
    };
    assert_eq!(
        OperationsClient::new(&changing, FakeClock::default())
            .screenshot(&directory_path.join("changed.png"), Duration::from_secs(1))
            .expect_err("changed published capture"),
        OperationError::RemoteCaptureChanged
    );
}

#[test]
fn screenshot_rejects_a_snapshot_not_bound_to_the_selected_fresh_source() {
    let selected = rgba_png(480, 480);
    let mut pixels = vec![0; 480 * 480 * 4];
    pixels[0] = 255;
    let replacement = png_bytes(480, 480, png::ColorType::Rgba, pixels);
    let backend = FakeBackend::healthy(selected);
    *backend.capture.borrow_mut() = replacement.clone();
    *backend.published_metadata.borrow_mut() = Ok(published_metadata(30, 40, &replacement));
    let directory = tempfile::tempdir().expect("temporary directory");
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");

    assert_eq!(
        OperationsClient::new(&backend, FakeClock::default())
            .screenshot(&directory_path.join("unbound.png"), Duration::from_secs(1))
            .expect_err("published bytes must belong to selected source"),
        OperationError::RemoteCaptureChanged
    );
}

#[test]
fn screenshot_deadline_includes_snapshot_transfer_recheck_decode_and_persist() {
    struct SlowPublish<'a> {
        inner: &'a FakeBackend,
        clock: FakeClock,
    }
    impl OperationsBackend for SlowPublish<'_> {
        fn diagnostic_facts(&self) -> Result<DiagnosticFacts, OperationError> {
            self.inner.diagnostic_facts()
        }
        fn debug_frame_metadata(
            &self,
            timeout: Duration,
        ) -> Result<Option<CaptureMetadata>, OperationError> {
            self.inner.debug_frame_metadata(timeout)
        }
        fn signal_debug_frame(&self, timeout: Duration) -> Result<(), OperationError> {
            self.inner.signal_debug_frame(timeout)
        }
        fn capture_debug_frame(
            &self,
            before: Option<&CaptureMetadata>,
            timeout: Duration,
        ) -> Result<CaptureTransfer, OperationError> {
            self.clock.advance(Duration::from_secs(2));
            self.inner.capture_debug_frame(before, timeout)
        }
    }

    let bytes = rgba_png(480, 480);
    let backend = FakeBackend::healthy(bytes);
    let clock = FakeClock::default();
    let slow = SlowPublish {
        inner: &backend,
        clock: clock.clone(),
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");
    assert_eq!(
        OperationsClient::new(&slow, clock)
            .screenshot(&directory_path.join("late.png"), Duration::from_secs(1))
            .expect_err("whole operation deadline"),
        OperationError::CaptureTimedOut
    );
    assert!(!directory_path.join("late.png").exists());
}

#[test]
fn screenshot_never_reports_a_deadline_failure_after_committing_the_destination() {
    #[derive(Clone)]
    struct CommitAwareClock {
        destination: PathBuf,
        committed: Vec<u8>,
    }
    impl CaptureClock for CommitAwareClock {
        fn now(&self) -> Duration {
            if fs::read(&self.destination).ok().as_deref() == Some(self.committed.as_slice()) {
                Duration::from_secs(2)
            } else {
                Duration::ZERO
            }
        }
    }

    let bytes = rgba_png(480, 480);
    let backend = FakeBackend::healthy(bytes.clone());
    let directory = tempfile::tempdir().expect("temporary directory");
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");
    let destination = directory_path.join("radar.png");
    fs::write(&destination, b"keep me").expect("existing destination");
    let clock = CommitAwareClock {
        destination: destination.clone(),
        committed: bytes.clone(),
    };

    OperationsClient::new(&backend, clock)
        .screenshot(&destination, Duration::from_secs(1))
        .expect("successful rename is the commit point");

    assert_eq!(
        fs::read(&destination).expect("committed destination"),
        bytes
    );
}

#[test]
fn screenshot_rejects_an_existing_parent_writable_by_other_users() {
    let bytes = rgba_png(480, 480);
    let backend = FakeBackend::healthy(bytes);
    let directory = tempfile::tempdir().expect("temporary directory");
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");
    let unsafe_parent = directory_path.join("unsafe-parent");
    fs::create_dir(&unsafe_parent).expect("unsafe parent");
    fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777))
        .expect("unsafe parent mode");
    let destination = unsafe_parent.join("radar.png");

    assert_eq!(
        OperationsClient::new(&backend, FakeClock::default())
            .screenshot(&destination, Duration::from_secs(1))
            .expect_err("writable parent"),
        OperationError::UnsafeLocalDestination
    );
    assert!(!backend.signaled.get());
    assert!(!destination.exists());
}

#[test]
fn screenshot_rejects_a_temp_name_substituted_after_its_fd_is_validated() {
    #[derive(Clone)]
    struct TempSubstitutionClock {
        directory: PathBuf,
        substituted: Rc<Cell<bool>>,
    }
    impl CaptureClock for TempSubstitutionClock {
        fn now(&self) -> Duration {
            if !self.substituted.get()
                && let Some(path) = fs::read_dir(&self.directory)
                    .expect("destination directory")
                    .filter_map(Result::ok)
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".planeradar-capture-")
                    })
                    .map(|entry| entry.path())
            {
                fs::remove_file(&path).expect("unlink opened temporary entry");
                fs::write(&path, b"attacker substitution").expect("substitute temporary entry");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .expect("substitute mode");
                self.substituted.set(true);
            }
            Duration::ZERO
        }
    }

    let bytes = rgba_png(480, 480);
    let backend = FakeBackend::healthy(bytes);
    let directory = tempfile::tempdir().expect("temporary directory");
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");
    let destination = directory_path.join("radar.png");
    fs::write(&destination, b"keep me").expect("existing destination");
    let substituted = Rc::new(Cell::new(false));
    let clock = TempSubstitutionClock {
        directory: directory_path,
        substituted: substituted.clone(),
    };

    assert_eq!(
        OperationsClient::new(&backend, clock)
            .screenshot(&destination, Duration::from_secs(1))
            .expect_err("substituted temporary entry"),
        OperationError::UnsafeLocalDestination
    );
    assert!(substituted.get());
    assert_eq!(
        fs::read(&destination).expect("preserved destination"),
        b"keep me"
    );
}

#[cfg(unix)]
#[test]
fn screenshot_never_redirects_when_the_destination_parent_is_swapped() {
    use std::os::unix::fs::symlink;

    struct ParentSwap<'a> {
        inner: &'a FakeBackend,
        parent: PathBuf,
        displaced: PathBuf,
        attacker: PathBuf,
    }
    impl OperationsBackend for ParentSwap<'_> {
        fn diagnostic_facts(&self) -> Result<DiagnosticFacts, OperationError> {
            self.inner.diagnostic_facts()
        }
        fn debug_frame_metadata(
            &self,
            timeout: Duration,
        ) -> Result<Option<CaptureMetadata>, OperationError> {
            self.inner.debug_frame_metadata(timeout)
        }
        fn signal_debug_frame(&self, timeout: Duration) -> Result<(), OperationError> {
            fs::rename(&self.parent, &self.displaced).expect("displace validated parent");
            symlink(&self.attacker, &self.parent).expect("swap parent for symlink");
            self.inner.signal_debug_frame(timeout)
        }
        fn capture_debug_frame(
            &self,
            before: Option<&CaptureMetadata>,
            timeout: Duration,
        ) -> Result<CaptureTransfer, OperationError> {
            self.inner.capture_debug_frame(before, timeout)
        }
    }

    let bytes = rgba_png(480, 480);
    let backend = FakeBackend::healthy(bytes);
    let directory = tempfile::tempdir().expect("temporary directory");
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");
    let parent = directory_path.join("output");
    let displaced = directory_path.join("validated-output");
    let attacker = directory_path.join("attacker");
    fs::create_dir(&parent).expect("output parent");
    fs::create_dir(&attacker).expect("attacker parent");
    let swapped = ParentSwap {
        inner: &backend,
        parent: parent.clone(),
        displaced,
        attacker: attacker.clone(),
    };

    assert_eq!(
        OperationsClient::new(&swapped, FakeClock::default())
            .screenshot(&parent.join("radar.png"), Duration::from_secs(1))
            .expect_err("swapped parent"),
        OperationError::UnsafeLocalDestination
    );
    assert!(!attacker.join("radar.png").exists());
}

#[cfg(unix)]
#[test]
fn screenshot_rejects_destination_symlink_and_preserves_existing_file_on_failure() {
    use std::os::unix::fs::symlink;

    let bytes = rgba_png(480, 480);
    let directory = tempfile::tempdir().expect("temporary directory");
    let directory_path = fs::canonicalize(directory.path()).expect("canonical temporary directory");
    let victim = directory_path.join("victim.png");
    fs::write(&victim, b"victim").expect("victim");
    let link = directory_path.join("radar.png");
    symlink(&victim, &link).expect("symlink");
    let backend = FakeBackend::healthy(bytes.clone());
    assert_eq!(
        OperationsClient::new(&backend, FakeClock::default())
            .screenshot(&link, Duration::from_secs(1))
            .expect_err("destination symlink"),
        OperationError::UnsafeLocalDestination
    );
    assert_eq!(fs::read(&victim).expect("victim"), b"victim");

    let real_parent = directory_path.join("real-parent");
    let nested = real_parent.join("nested");
    fs::create_dir_all(&nested).expect("real nested parent");
    let linked_parent = directory_path.join("linked-parent");
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

    let destination = directory_path.join("existing.png");
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
    target_state_json_with_driver_manifest(DRIVER_MANIFEST)
}

fn target_state_json_with_driver_manifest(driver_manifest: &str) -> Vec<u8> {
    format!(
        r#"{{"schema_version":1,"hardware":{{"model":"Raspberry Pi Zero 2 W Rev 1.0","serial":"1000000012345678"}},"application":{{"version":"0.1.0","source_commit":"{APP_REVISION}","sha256":"{APP_SHA256}"}},"driver":{{"version":"{DRIVER_VERSION}","source_commit":"{DRIVER_REVISION}","sha256":"{driver_manifest}"}},"owned_files":[{{"target_path":"/opt/planeradar/bin/planeradar","sha256":"{APP_SHA256}"}}],"last_verified_phase":"complete"}}"#
    )
    .into_bytes()
}

fn diagnostic_probe_json() -> Vec<u8> {
    let health = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!(
            r#"{{"configured":true,"state":"RADAR","data_stale":false,"revision":"{APP_REVISION}"}}"#
        ),
    );
    format!(
        r#"{{"schema_version":1,"os_id":"raspbian","os_version":"13","architecture":"arm64","application_version":"0.1.0","application_revision":"{APP_REVISION}","application_sha256":"{APP_SHA256}","driver_version":"{DRIVER_VERSION}","driver_revision":"{DRIVER_REVISION}","driver_manifest_sha256":"{DRIVER_MANIFEST}","accepted_driver_manifest_sha256":"{DRIVER_MANIFEST}","expected_kernel":"{KERNEL}","running_kernel":"{KERNEL}","module_loaded":true,"module_vermagic":"{VERMAGIC}","expected_module_vermagic":"{VERMAGIC}","module_sha256":"{MODULE_SHA256}","expected_module_sha256":"{MODULE_SHA256}","overlay_file":"hyperpixel2r-kms-261a29f45963.dtbo","expected_overlay_file":"hyperpixel2r-kms-261a29f45963.dtbo","overlay_sha256":"{OVERLAY_SHA256}","expected_overlay_sha256":"{OVERLAY_SHA256}","boot_config_sha256":"{BOOT_CONFIG_SHA256}","normal_kernel_file":"","normal_kernel_sha256":"","normal_initramfs_file":"","normal_initramfs_sha256":"","base_dtb_sha256":"","vc4_overlay_sha256":"","overlay_configured":true,"drm_device":"/dev/dri/card0","drm_mode":"480x480","renderer":"opengles2","touch_device":"HyperPixel 2.1 Round Touch","service_active":true,"service_restart_count":0,"health_base64":"{health}","hostname":"planeradar"}}"#
    )
    .into_bytes()
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable");
    let mut permissions = fs::metadata(path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).expect("executable permissions");
}

#[test]
fn doctor_json_process_exit_and_stream_contract_covers_healthy_and_unhealthy_targets() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = fs::canonicalize(temporary.path()).expect("canonical temporary directory");
    let binaries = root.join("bin");
    let fixtures = root.join("fixtures");
    let home = root.join("home");
    fs::create_dir(&binaries).expect("binary directory");
    fs::create_dir(&fixtures).expect("fixture directory");
    fs::create_dir_all(home.join(".ssh")).expect("home directory");
    fs::write(home.join(".ssh").join("known_hosts"), b"").expect("known hosts");
    fs::write(fixtures.join("state.json"), target_state_json()).expect("target state fixture");
    fs::write(fixtures.join("diagnostic.json"), diagnostic_probe_json())
        .expect("diagnostic fixture");

    write_executable(
        &binaries.join("ssh-keygen"),
        &format!(
            "#!/bin/sh\nhost=$2\nprintf '# Host %s found\\n%s ssh-ed25519 {TEST_PUBLIC_KEY}\\n' \"$host\" \"$host\"\n"
        ),
    );
    write_executable(
        &binaries.join("ssh"),
        "#!/bin/sh\ncase \"$*\" in\n  *'/proc/device-tree/model'*) printf 'Raspberry Pi Zero 2 W Rev 1.0\\n' ;;\n  *'/proc/cpuinfo'*) printf '1000000012345678\\n' ;;\n  *'installer-state'*) cat \"$PLANERADAR_TEST_FIXTURES/state.json\" ;;\n  *'planeradar-diagnostics'*) cat \"$PLANERADAR_TEST_FIXTURES/diagnostic.json\" ;;\n  *) exit 91 ;;\nesac\n",
    );

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_planeradarctl"))
            .args(["doctor", "pi@raspberrypi.local", "--json"])
            .current_dir(&root)
            .env("HOME", &home)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", binaries.to_string_lossy()),
            )
            .env("PLANERADAR_TEST_FIXTURES", &fixtures)
            .output()
            .expect("run planeradarctl")
    };

    let healthy = run();
    assert!(healthy.status.success(), "{healthy:?}");
    assert!(healthy.stderr.is_empty(), "{healthy:?}");
    let healthy_report =
        DoctorReport::from_json(&healthy.stdout).expect("healthy stdout is one strict report");
    assert!(healthy_report.healthy);

    let health = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!(
            r#"{{"configured":true,"state":"RADAR","data_stale":false,"revision":"{APP_REVISION}"}}"#
        ),
    );
    let unhealthy_probe = String::from_utf8(diagnostic_probe_json())
        .expect("diagnostic fixture")
        .replace(
            &format!(r#""health_base64":"{health}""#),
            r#""health_base64":"""#,
        );
    fs::write(fixtures.join("diagnostic.json"), unhealthy_probe)
        .expect("unhealthy diagnostic fixture");

    let unhealthy = run();
    assert!(!unhealthy.status.success(), "{unhealthy:?}");
    let unhealthy_report =
        DoctorReport::from_json(&unhealthy.stdout).expect("unhealthy stdout is one strict report");
    assert!(!unhealthy_report.healthy);
    assert_eq!(
        unhealthy_report.diagnostics,
        [
            DiagnosticCode::ApplicationRevisionMismatch,
            DiagnosticCode::HttpFailure
        ]
    );
    assert_eq!(
        String::from_utf8(unhealthy.stderr).expect("typed stderr"),
        "planeradarctl: target is unhealthy: ApplicationRevisionMismatch\n"
    );
}

#[test]
fn production_doctor_observes_driver_artifact_hashes_and_strict_running_health() {
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state_json(), Vec::new())),
        Ok(Output::success(diagnostic_probe_json(), Vec::new())),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );

    let report = OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("strict doctor");

    assert!(report.healthy, "{report:?}");
    assert_eq!(report.facts.installed_driver.sha256, DRIVER_MANIFEST);
    assert_eq!(report.facts.module_sha256, MODULE_SHA256);
    assert_eq!(report.facts.overlay_sha256, OVERLAY_SHA256);
    assert_eq!(report.facts.boot_config_sha256, BOOT_CONFIG_SHA256);
    assert_eq!(report.facts.running_application_revision, APP_REVISION);
}

#[test]
fn production_doctor_distinguishes_release_manifest_from_accepted_bundle_manifest() {
    let probe = String::from_utf8(diagnostic_probe_json())
        .expect("probe JSON")
        .replace(
            &format!(
                r#""driver_manifest_sha256":"{DRIVER_MANIFEST}","accepted_driver_manifest_sha256":"{DRIVER_MANIFEST}","#,
            ),
            &format!(
                r#""driver_manifest_sha256":"{INSTALLED_DRIVER_MANIFEST}","accepted_driver_manifest_sha256":"{INSTALLED_DRIVER_MANIFEST}","#,
            ),
        )
        .into_bytes();
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state_json(), Vec::new())),
        Ok(Output::success(probe, Vec::new())),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );

    let report = OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("distinct manifest identity layers");

    assert!(report.healthy, "{report:?}");
    assert_eq!(
        report.facts.installed_driver.sha256,
        INSTALLED_DRIVER_MANIFEST
    );
    assert_eq!(
        report.facts.persisted_driver_manifest_sha256,
        DRIVER_MANIFEST
    );
}

#[test]
fn production_doctor_uses_protected_release_state_for_prerelease_application_version() {
    let target_state = String::from_utf8(target_state_json())
        .expect("target state JSON")
        .replace(
            r#""application":{"version":"0.1.0""#,
            r#""application":{"version":"0.1.0-rc.7""#,
        )
        .into_bytes();
    let transport = RecordingTransport::default();
    let probe = String::from_utf8(diagnostic_probe_json())
        .expect("diagnostic probe JSON")
        .replace(
            r#""application_version":"0.1.0""#,
            r#""application_version":"0.1.0-rc.7""#,
        )
        .into_bytes();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state, Vec::new())),
        Ok(Output::success(probe, Vec::new())),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );

    let report = OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("prerelease application identity");

    assert!(report.healthy, "{report:?}");
    assert_eq!(report.facts.installed_application.version, "0.1.0-rc.7");
    assert_eq!(
        transport.commands.borrow()[1].last().map(String::as_str),
        Some("0.1.0-rc.7"),
        "diagnostic script did not receive the protected release version"
    );
}

#[test]
fn production_doctor_recognizes_the_kernel_ft5x06_touch_device() {
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state_json(), Vec::new())),
        Ok(Output::success(diagnostic_probe_json(), Vec::new())),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );

    OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("doctor");

    let commands = transport.commands.borrow();
    let script = commands[1].join(" ");
    assert!(
        script.contains("generic ft5x06"),
        "diagnostic probe ignored the actual kernel touch-device name"
    );
}

#[test]
fn production_doctor_accepts_root_owned_fat_boot_file_modes() {
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state_json(), Vec::new())),
        Ok(Output::success(diagnostic_probe_json(), Vec::new())),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );

    OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("doctor");

    let commands = transport.commands.borrow();
    let script = commands[1].join(" ");
    assert!(
        script.contains("644|755")
            && script.contains(r#"boot_regular "$overlay""#)
            && script.contains(r#"boot_regular "$config""#),
        "diagnostic probe rejected the FAT-compatible boot-file mode policy"
    );
}

#[test]
fn production_doctor_rejects_persisted_manifest_digest_that_disagrees_with_observed_driver() {
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(
            target_state_json_with_driver_manifest(
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            ),
            Vec::new(),
        )),
        Ok(Output::success(diagnostic_probe_json(), Vec::new())),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );

    let report = OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("persisted mismatch is a diagnostic");

    assert!(!report.healthy);
    assert_eq!(report.diagnostics, [DiagnosticCode::DriverManifestMismatch]);
}

#[test]
fn production_doctor_rejects_malformed_health_and_flags_a_stale_running_revision() {
    let stale_health = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        r#"{"configured":true,"state":"RADAR","data_stale":false,"revision":"3333333333333333333333333333333333333333"}"#,
    );
    let stale_probe = String::from_utf8(diagnostic_probe_json())
        .expect("probe JSON")
        .replace(
            &base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                format!(
                    r#"{{"configured":true,"state":"RADAR","data_stale":false,"revision":"{APP_REVISION}"}}"#
                ),
            ),
            &stale_health,
        )
        .into_bytes();
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state_json(), Vec::new())),
        Ok(Output::success(stale_probe, Vec::new())),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );
    let report = OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("stale revision is a diagnostic");
    assert_eq!(
        report.diagnostics,
        [DiagnosticCode::ApplicationRevisionMismatch]
    );

    let malformed_probe = String::from_utf8(diagnostic_probe_json())
        .expect("probe JSON")
        .replace(
            &base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                format!(
                    r#"{{"configured":true,"state":"RADAR","data_stale":false,"revision":"{APP_REVISION}"}}"#
                ),
            ),
            "bm90LWpzb24=",
        )
        .into_bytes();
    let malformed_transport = RecordingTransport::default();
    malformed_transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state_json(), Vec::new())),
        Ok(Output::success(malformed_probe, Vec::new())),
    ]);
    let malformed_backend = SshOperationsBackend::new(
        &malformed_transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );
    assert_eq!(
        malformed_backend
            .diagnostic_facts()
            .expect_err("malformed health"),
        OperationError::MalformedFacts
    );
}

#[test]
fn production_backend_collects_strict_fixed_diagnostics_without_mutating_target() {
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state_json(), Vec::new())),
        Ok(Output::success(diagnostic_probe_json(), Vec::new())),
    ]);
    let target: SshTarget = "pi@raspberrypi.local".parse().expect("target");
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
    assert!(
        std::process::Command::new("sh")
            .args(["-n", "-c", &commands[1][6]])
            .status()
            .expect("validate diagnostic shell syntax")
            .success(),
        "diagnostic command is not syntactically valid"
    );
    let flattened = commands.concat().join(" ");
    assert!(!flattened.contains("latitude"));
    assert!(!flattened.contains("longitude"));
}

#[test]
fn production_doctor_accepts_exact_legacy_and_metadata_receipt_shapes() {
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(target_state_json(), Vec::new())),
        Ok(Output::success(diagnostic_probe_json(), Vec::new())),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );

    OperationsClient::new(&backend, FakeClock::default())
        .doctor()
        .expect("doctor");

    let commands = transport.commands.borrow();
    let program = &commands[1][6];
    assert!(program.contains("3:16:0"));
    assert!(program.contains("3:18:0"));
    assert!(program.contains("4:22:0"));
    assert!(program.contains("4:24:0"));
    assert!(program.contains("17:0"));
    assert!(program.contains("18:0"));
    assert!(program.contains("exact-backlight-metadata-v1"));
    assert!(program.contains("prior-backlight-metadata"));
    for binding in [
        "test \"$normal_kernel_file\" = kernel8.img",
        "test \"$normal_initramfs_file\" = initramfs8",
        "/boot/firmware/kernel8.img",
        "/boot/firmware/initramfs8",
        "/boot/firmware/bcm2710-rpi-zero-2-w.dtb",
        "/boot/firmware/overlays/vc4-kms-v3d.dtbo",
    ] {
        assert!(
            program.contains(binding),
            "schema-4 probe omitted {binding}"
        );
    }
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
            "pi@raspberrypi.local".parse().expect("target"),
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
    let before = CaptureMetadata {
        inode: 1,
        modified_ns: 2,
        size: bytes.len() as u64,
        sha256: sha256(&bytes),
        uid: 1000,
        gid: 1000,
        mode: 384,
        links: 1,
        regular: true,
        symlink: false,
    };
    let source = CaptureMetadata {
        inode: 2,
        modified_ns: 3,
        ..before.clone()
    };
    let published = CaptureMetadata {
        inode: 3,
        modified_ns: 4,
        uid: 0,
        gid: 0,
        ..source.clone()
    };
    let transport = RecordingTransport::default();
    transport.outputs.borrow_mut().extend([
        Ok(Output::success(
            serde_json::to_vec(&before).expect("metadata"),
            Vec::new(),
        )),
        Ok(Output::success(Vec::new(), Vec::new())),
        Ok(Output::success(
            capture_protocol(&source, &published, &bytes),
            Vec::new(),
        )),
    ]);
    let backend = SshOperationsBackend::new(
        &transport,
        "pi@raspberrypi.local".parse().expect("target"),
        DriverLock::checked_in().expect("driver lock"),
    );
    let observed = backend
        .debug_frame_metadata(Duration::from_secs(1))
        .expect("metadata");
    backend
        .signal_debug_frame(Duration::from_secs(1))
        .expect("signal");
    let transfer = backend
        .capture_debug_frame(observed.as_ref(), Duration::from_secs(1))
        .expect("bounded privileged capture");
    assert_eq!(transfer.bytes, bytes);

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
    assert!(commands[0].iter().any(|value| value == "capture-metadata"));
    assert!(commands[2].iter().any(|value| value == "capture-snapshot"));
    assert!(
        transport.copies.borrow().is_empty(),
        "a root-only capture must travel over bounded privileged SSH stdout, not unprivileged SCP"
    );
}

#[test]
fn production_capture_parser_rejects_truncated_trailing_and_unknown_protocol_data() {
    let bytes = rgba_png(480, 480);
    let source = source_metadata(2, 3, &bytes);
    let published = published_metadata(3, 4, &bytes);
    let valid = capture_protocol(&source, &published, &bytes);
    let mut truncated = valid.clone();
    truncated.pop();
    let mut trailing = valid.clone();
    trailing.push(0);
    let unknown = capture_protocol_with_unknown_header_field(valid);

    for (name, protocol) in [
        ("truncated", truncated),
        ("trailing", trailing),
        ("unknown field", unknown),
    ] {
        let transport = RecordingTransport::default();
        transport
            .outputs
            .borrow_mut()
            .push_back(Ok(Output::success(protocol, Vec::new())));
        let backend = SshOperationsBackend::new(
            &transport,
            "pi@raspberrypi.local".parse().expect("target"),
            DriverLock::checked_in().expect("driver lock"),
        );
        assert!(
            backend
                .capture_debug_frame(None, Duration::from_secs(1))
                .is_err(),
            "{name} protocol must be rejected"
        );
    }
}
