use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use planeradarctl::{
    DriverLock,
    driver::{
        DriverAction, DriverContext, DriverManager, DriverPlan, DriverReleaseSource,
        DriverReleaseVerifier, DriverResolver, DriverTool, GhDriverReleaseSource,
        GhDriverReleaseVerifier, PrebuiltBundle, TargetProbe,
    },
    release::{ReleaseSourceError, StreamingCommandRunner},
    transport::{CommandOutput, CommandRunner, Invocation, RunnerError, SystemCommandRunner},
};
use semver::Version;
use sha2::{Digest, Sha256};
use tar::{Builder, EntryType, Header};

const MANIFEST_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DRIVER_COMMIT: &str = "ca95ffeb30b3c361f16cfc228c7bf2b78abf2b4c";

fn probe(kernel_release: &str) -> TargetProbe {
    TargetProbe::new(kernel_release).expect("valid target probe")
}

fn resolver(bundle: PrebuiltBundle) -> DriverResolver {
    DriverResolver::new(PathBuf::from("/cache/source"), vec![bundle])
        .expect("valid driver resolver")
}

fn prebuilt(
    kernel_release: &str,
    vermagic: &str,
    internal_manifest_digest: &str,
) -> Result<PrebuiltBundle, planeradarctl::driver::DriverError> {
    PrebuiltBundle::verified(
        PathBuf::from("/cache/prebuilt.tar.zst"),
        kernel_release,
        vermagic,
        internal_manifest_digest,
        MANIFEST_DIGEST,
    )
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn archive(entries: &[(&str, EntryType, &[u8], Option<&str>)]) -> Vec<u8> {
    let encoder = zstd::Encoder::new(Vec::new(), 1).expect("zstd encoder");
    let mut builder = Builder::new(encoder);
    for (path, entry_type, contents, link_name) in entries {
        let mut header = Header::new_gnu();
        header.set_entry_type(*entry_type);
        header.set_mode(if entry_type.is_file() { 0o644 } else { 0o777 });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(1);
        header.set_size(if entry_type.is_file() {
            contents.len() as u64
        } else {
            0
        });
        if let Some(link_name) = link_name {
            header.set_link_name(link_name).expect("link name");
        }
        header.set_cksum();
        builder
            .append_data(&mut header, path, *contents)
            .expect("tar entry");
    }
    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish zstd")
}

fn source_archive() -> Vec<u8> {
    archive(&[
        (
            "hyperpixel2r-kms-0.1.0/scripts/verify-boot.sh",
            EntryType::Regular,
            b"#!/usr/bin/env bash\n",
            None,
        ),
        (
            "hyperpixel2r-kms-0.1.0/kernel/Kbuild",
            EntryType::Regular,
            b"obj-m += hyperpixel2r_kms.o\n",
            None,
        ),
    ])
}

fn prebuilt_archive(
    kernel_release: &str,
    vermagic: &str,
    declared_module_digest: Option<&str>,
) -> Vec<u8> {
    let module = b"module bytes";
    let module_digest = declared_module_digest
        .map(str::to_owned)
        .unwrap_or_else(|| digest(module));
    let manifest = format!(
        "schema_version\t1\nsource_revision\t{DRIVER_COMMIT}\nkernel_release\t{kernel_release}\nmodule_file\thyperpixel2r_kms.ko\nmodule_sha256\t{module_digest}\nmodule_vermagic\t{vermagic}\n"
    );
    archive(&[
        (
            "hyperpixel2r-kms-0.1.0-6.18.34+rpt-rpi-v8-aarch64/manifest.txt",
            EntryType::Regular,
            manifest.as_bytes(),
            None,
        ),
        (
            "hyperpixel2r-kms-0.1.0-6.18.34+rpt-rpi-v8-aarch64/hyperpixel2r_kms.ko",
            EntryType::Regular,
            module,
            None,
        ),
    ])
}

fn release_manifest(source: &[u8], prebuilt: Option<(&[u8], &str)>) -> Vec<u8> {
    let mut artifacts = vec![
        serde_json::json!({
            "name": "hyperpixel2r-kms-source.tar.zst",
            "kind": "source-archive",
            "sha256": digest(source),
            "size": source.len(),
        }),
        serde_json::json!({
            "name": "SBOM.spdx.json",
            "kind": "sbom",
            "sha256": digest(b"sbom"),
            "size": 4,
        }),
    ];
    if let Some((bytes, kernel_release)) = prebuilt {
        artifacts.push(serde_json::json!({
            "name": format!("hyperpixel2r-kms-{kernel_release}-aarch64.tar.zst"),
            "kind": "exact-kernel-bundle",
            "sha256": digest(bytes),
            "size": bytes.len(),
            "architecture": "aarch64",
            "kernel_release": kernel_release,
        }));
    }
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "driver_version": "0.1.0",
        "source": {
            "repository": "https://github.com/shayne/hyperpixel2r-kms",
            "commit": DRIVER_COMMIT,
            "tree": "1111111111111111111111111111111111111111",
            "date_epoch": 1,
        },
        "supported": {
            "board": "Raspberry Pi Zero 2 W",
            "display": "HyperPixel 2.1 Round",
            "operating_system": "Raspberry Pi OS Lite (Trixie, 64-bit)",
            "architecture": "aarch64",
            "kernel_policy": "exact-release-only",
        },
        "reproducibility": {
            "archive_format": "tar+zstd",
            "source_date_epoch": 1,
            "owner": 0,
            "group": 0,
            "mode_policy": "git-executable-or-regular",
        },
        "artifacts": artifacts,
    }))
    .expect("manifest JSON")
}

fn lock_for(manifest: &[u8]) -> DriverLock {
    DriverLock {
        repository: "https://github.com/shayne/hyperpixel2r-kms".into(),
        version: Version::parse("0.1.0-rc.11").expect("version"),
        commit: DRIVER_COMMIT.into(),
        manifest_sha256: digest(manifest),
    }
}

#[derive(Clone, Default)]
struct FakeReleaseSource {
    assets: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl FakeReleaseSource {
    fn with_assets(assets: impl IntoIterator<Item = (String, Vec<u8>)>) -> Self {
        Self {
            assets: Arc::new(Mutex::new(assets.into_iter().collect())),
        }
    }
}

impl DriverReleaseSource for FakeReleaseSource {
    fn stream(
        &self,
        _version: &Version,
        name: &str,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError> {
        let assets = self.assets.lock().expect("asset lock");
        let bytes = assets.get(name).ok_or(ReleaseSourceError::Failed)?;
        sink.write_all(bytes)
            .map_err(|_| ReleaseSourceError::Failed)
    }
}

#[derive(Clone, Default)]
struct FakeReleaseVerifier {
    reject: bool,
    calls: Arc<Mutex<Vec<(Version, usize)>>>,
}

impl DriverReleaseVerifier for FakeReleaseVerifier {
    fn verify(&self, version: &Version, assets: &[PathBuf]) -> Result<(), io::Error> {
        self.calls
            .lock()
            .expect("verifier lock")
            .push((version.clone(), assets.len()));
        if self.reject {
            Err(io::Error::other("fixture rejection"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn exact_kernel_selects_prebuilt_and_new_kernel_falls_back_to_cross_build() {
    let resolver = resolver(
        prebuilt(
            "6.18.34+rpt-rpi-v8",
            "6.18.34+rpt-rpi-v8 SMP preempt mod_unload aarch64",
            MANIFEST_DIGEST,
        )
        .expect("verified prebuilt"),
    );

    assert!(matches!(
        resolver
            .resolve(&probe("6.18.34+rpt-rpi-v8"))
            .expect("resolve exact kernel"),
        DriverPlan::Prebuilt { .. }
    ));
    assert!(matches!(
        resolver
            .resolve(&probe("6.18.35+rpt-rpi-v8"))
            .expect("resolve new kernel"),
        DriverPlan::CrossBuild { .. }
    ));
}

#[test]
fn prebuilt_identity_rejects_kernel_vermagic_and_internal_manifest_digest_drift() {
    for (kernel_release, vermagic, internal_digest) in [
        (
            "6.18.35+rpt-rpi-v8",
            "6.18.34+rpt-rpi-v8 SMP preempt mod_unload aarch64",
            MANIFEST_DIGEST,
        ),
        (
            "6.18.34+rpt-rpi-v8",
            "6.18.35+rpt-rpi-v8 SMP preempt mod_unload aarch64",
            MANIFEST_DIGEST,
        ),
        (
            "6.18.34+rpt-rpi-v8",
            "6.18.34+rpt-rpi-v8 SMP preempt mod_unload aarch64",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
    ] {
        assert!(
            prebuilt(kernel_release, vermagic, internal_digest).is_err(),
            "drifted prebuilt identity was accepted"
        );
    }
}

#[test]
fn sync_verifies_the_locked_release_and_materializes_safe_source_and_prebuilt_archives() {
    let source = source_archive();
    let prebuilt = prebuilt_archive(
        "6.18.34+rpt-rpi-v8",
        "6.18.34+rpt-rpi-v8 SMP preempt mod_unload aarch64",
        None,
    );
    let manifest = release_manifest(&source, Some((&prebuilt, "6.18.34+rpt-rpi-v8")));
    let release_source = FakeReleaseSource::with_assets([
        ("driver-manifest.json".into(), manifest.clone()),
        ("hyperpixel2r-kms-source.tar.zst".into(), source),
        ("SBOM.spdx.json".into(), b"sbom".to_vec()),
        (
            "hyperpixel2r-kms-6.18.34+rpt-rpi-v8-aarch64.tar.zst".into(),
            prebuilt,
        ),
    ]);
    let verifier = FakeReleaseVerifier::default();
    let temporary = tempfile::tempdir().expect("temporary cache");
    let manager = DriverManager::new(
        release_source,
        verifier.clone(),
        temporary.path().join("cache"),
    );

    let synced = manager.sync(&lock_for(&manifest)).expect("sync driver");
    let script = synced.source_root().join("scripts/verify-boot.sh");
    assert!(script.is_file());
    assert!(
        !script
            .symlink_metadata()
            .expect("script metadata")
            .is_symlink()
    );
    assert!(matches!(
        synced
            .resolver()
            .resolve(&probe("6.18.34+rpt-rpi-v8"))
            .expect("resolve prebuilt"),
        DriverPlan::Prebuilt { .. }
    ));
    assert_eq!(
        verifier.calls.lock().expect("verifier calls").as_slice(),
        &[(Version::parse("0.1.0-rc.11").expect("version"), 4)]
    );
}

#[test]
fn sync_rejects_unsafe_source_entries_without_materializing_them() {
    for (label, entry_type, link_name) in [
        ("symlink", EntryType::Symlink, Some("/outside")),
        ("hardlink", EntryType::Link, Some("/outside")),
    ] {
        let source = archive(&[("hyperpixel2r-kms-0.1.0/unsafe", entry_type, b"", link_name)]);
        let manifest = release_manifest(&source, None);
        let manager = DriverManager::new(
            FakeReleaseSource::with_assets([
                ("driver-manifest.json".into(), manifest.clone()),
                ("hyperpixel2r-kms-source.tar.zst".into(), source),
                ("SBOM.spdx.json".into(), b"sbom".to_vec()),
            ]),
            FakeReleaseVerifier::default(),
            tempfile::tempdir()
                .expect("temporary parent")
                .keep()
                .join("cache"),
        );
        assert!(
            manager.sync(&lock_for(&manifest)).is_err(),
            "{label} source archive was accepted"
        );
    }
}

#[test]
fn sync_rejects_prebuilt_internal_kernel_vermagic_and_module_digest_drift() {
    for (kernel_release, vermagic, module_digest) in [
        (
            "6.18.35+rpt-rpi-v8",
            "6.18.34+rpt-rpi-v8 SMP preempt mod_unload aarch64",
            None,
        ),
        (
            "6.18.34+rpt-rpi-v8",
            "6.18.35+rpt-rpi-v8 SMP preempt mod_unload aarch64",
            None,
        ),
        (
            "6.18.34+rpt-rpi-v8",
            "6.18.34+rpt-rpi-v8 SMP preempt mod_unload aarch64",
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        ),
    ] {
        let source = source_archive();
        let prebuilt = prebuilt_archive(kernel_release, vermagic, module_digest);
        let manifest = release_manifest(&source, Some((&prebuilt, "6.18.34+rpt-rpi-v8")));
        let manager = DriverManager::new(
            FakeReleaseSource::with_assets([
                ("driver-manifest.json".into(), manifest.clone()),
                ("hyperpixel2r-kms-source.tar.zst".into(), source),
                ("SBOM.spdx.json".into(), b"sbom".to_vec()),
                (
                    "hyperpixel2r-kms-6.18.34+rpt-rpi-v8-aarch64.tar.zst".into(),
                    prebuilt,
                ),
            ]),
            FakeReleaseVerifier::default(),
            tempfile::tempdir()
                .expect("temporary parent")
                .keep()
                .join("cache"),
        );
        assert!(
            manager.sync(&lock_for(&manifest)).is_err(),
            "drifted prebuilt archive was accepted"
        );
    }
}

#[test]
fn failed_update_leaves_the_existing_lock_byte_for_byte_unchanged() {
    let source = source_archive();
    let manifest = release_manifest(&source, None);
    let manager = DriverManager::new(
        FakeReleaseSource::with_assets([
            ("driver-manifest.json".into(), manifest),
            ("hyperpixel2r-kms-source.tar.zst".into(), source),
            ("SBOM.spdx.json".into(), b"sbom".to_vec()),
        ]),
        FakeReleaseVerifier {
            reject: true,
            ..FakeReleaseVerifier::default()
        },
        tempfile::tempdir()
            .expect("temporary cache parent")
            .keep()
            .join("cache"),
    );
    let temporary = tempfile::tempdir().expect("temporary lock");
    let lock_path = temporary.path().join("driver.lock.toml");
    let before = b"repository = \"https://github.com/shayne/hyperpixel2r-kms\"\nversion = \"0.1.0-rc.4\"\ncommit = \"6826419b4f3ab01c2e1ce9a3ef870186ae2cc3b8\"\nmanifest_sha256 = \"93f413aac135b44585703a03717d5aa2e9ae6b2b2d4b178d193d4758dfdedee7\"\n";
    fs::write(&lock_path, before).expect("seed lock");

    assert!(
        manager
            .update(&lock_path, &Version::parse("0.1.0-rc.11").expect("version"))
            .is_err()
    );
    assert_eq!(fs::read(&lock_path).expect("lock after failure"), before);
}

#[derive(Clone, Default)]
struct RecordingStreamingRunner {
    invocations: Arc<Mutex<Vec<Invocation>>>,
}

impl StreamingCommandRunner for RecordingStreamingRunner {
    fn run_streaming(
        &self,
        invocation: Invocation,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError> {
        self.invocations
            .lock()
            .expect("stream invocation lock")
            .push(invocation);
        sink.write_all(b"asset")
            .map_err(|_| ReleaseSourceError::Failed)
    }
}

#[test]
fn github_release_source_is_fixed_to_the_external_repository_and_exact_tag() {
    let runner = RecordingStreamingRunner::default();
    let source = GhDriverReleaseSource::new(runner.clone());
    let mut bytes = Vec::new();
    source
        .stream(
            &Version::parse("0.1.0-rc.11").expect("version"),
            "driver-manifest.json",
            &mut bytes,
        )
        .expect("stream release asset");

    assert_eq!(bytes, b"asset");
    let invocations = runner.invocations.lock().expect("stream invocations");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].program(), "gh");
    assert_eq!(
        invocations[0].arguments(),
        [
            "release",
            "download",
            "v0.1.0-rc.11",
            "--pattern",
            "driver-manifest.json",
            "--output",
            "-",
            "-R",
            "shayne/hyperpixel2r-kms",
        ]
    );
}

#[test]
fn github_verifier_checks_the_release_and_attestation_for_every_downloaded_asset() {
    let runner = RecordingRunner::default();
    let verifier = GhDriverReleaseVerifier::new(runner.clone());
    verifier
        .verify(
            &Version::parse("0.1.0-rc.11").expect("version"),
            &[
                PathBuf::from("/cache/driver-manifest.json"),
                PathBuf::from("/cache/source.tar.zst"),
            ],
        )
        .expect("verify driver release");

    let invocations = runner.invocations.lock().expect("verification invocations");
    assert_eq!(invocations.len(), 2);
    for (invocation, asset) in invocations
        .iter()
        .zip(["/cache/driver-manifest.json", "/cache/source.tar.zst"])
    {
        assert_eq!(invocation.program(), "gh");
        assert_eq!(
            invocation.arguments(),
            [
                "attestation",
                "verify",
                asset,
                "-R",
                "shayne/hyperpixel2r-kms",
            ]
        );
    }
}

#[derive(Clone, Default)]
struct RecordingRunner {
    invocations: Arc<Mutex<Vec<Invocation>>>,
}

impl CommandRunner for RecordingRunner {
    fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError> {
        let verification = invocation
            .arguments()
            .first()
            .is_some_and(|argument| argument.ends_with("verify-boot.sh"));
        self.invocations
            .lock()
            .expect("invocation lock")
            .push(invocation);
        Ok(CommandOutput::success(
            if verification {
                br#"{"schema_version":1,"driver_version":"0.1.0","kernel_release":"6.18.34+rpt-rpi-v8","module":"hyperpixel2r_kms","drm_mode":"480x480","touch":true,"sdl_driver":"KMSDRM","renderer":"opengles2","accepted":true}"#.to_vec()
            } else {
                Vec::new()
            },
            Vec::new(),
        ))
    }
}

#[test]
fn typed_actions_invoke_only_the_exact_external_driver_scripts_and_parse_verification_json() {
    let runner = RecordingRunner::default();
    let tool = DriverTool::new(
        runner.clone(),
        PathBuf::from("/cache/source"),
        DriverContext {
            target: "shayne@planeradar.local".into(),
            kernel_release: "6.18.34+rpt-rpi-v8".into(),
            kernel_export: PathBuf::from("/cache/kernel"),
            artifacts: PathBuf::from("/cache/artifacts"),
            replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
        },
    )
    .expect("valid driver tool");

    for action in [
        DriverAction::ExportKernel,
        DriverAction::Build,
        DriverAction::StageTryboot,
        DriverAction::VerifyBoot,
        DriverAction::CommitBoot,
        DriverAction::RollbackBoot,
        DriverAction::Uninstall,
    ] {
        let result = tool.run(action).expect("typed action");
        if action == DriverAction::VerifyBoot {
            let verification = result.expect("verification result");
            assert_eq!(verification.kernel_release, "6.18.34+rpt-rpi-v8");
            assert_eq!(verification.module, "hyperpixel2r_kms");
            assert_eq!(verification.drm_mode, "480x480");
            assert!(verification.touch);
            assert_eq!(verification.renderer, "opengles2");
            assert!(verification.accepted);
        } else {
            assert!(result.is_none());
        }
    }

    let invocations = runner.invocations.lock().expect("invocation lock");
    let scripts = invocations
        .iter()
        .map(|invocation| {
            assert_eq!(invocation.program(), "bash");
            invocation.arguments()[0].clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        scripts,
        [
            "/cache/source/scripts/export-target-kbuild.sh",
            "/cache/source/scripts/build-driver.sh",
            "/cache/source/scripts/stage-tryboot.sh",
            "/cache/source/scripts/verify-boot.sh",
            "/cache/source/scripts/commit-boot.sh",
            "/cache/source/scripts/rollback-boot.sh",
            "/cache/source/scripts/uninstall.sh",
        ]
    );
    for invocation in invocations.iter() {
        assert!(
            invocation
                .arguments()
                .windows(2)
                .any(|pair| pair == ["--target", "shayne@planeradar.local"]),
            "target was not passed as a separate argument"
        );
    }
    let build = &invocations[1];
    assert!(
        build
            .arguments()
            .windows(2)
            .any(|pair| pair == ["--kernel-target", "/cache/kernel"]),
        "build did not consume the exported kernel target path"
    );
    let stage = &invocations[2];
    assert!(
        stage
            .arguments()
            .windows(2)
            .any(|pair| pair == ["--kernel-target", "/cache/kernel"]),
        "stage did not consume the exported kernel target path"
    );
}

#[test]
#[ignore = "requires the authorized live Pi and GitHub release access"]
fn locked_release_builds_the_live_kernel_from_separate_cache_paths() {
    let target =
        std::env::var("PLANERADAR_DRIVER_LIVE_TARGET").expect("live target environment variable");
    let workspace = PathBuf::from(
        std::env::var_os("PLANERADAR_DRIVER_LIVE_WORKSPACE")
            .expect("live workspace environment variable"),
    );
    assert!(workspace.is_absolute());
    let manager = DriverManager::new(
        GhDriverReleaseSource::system(),
        GhDriverReleaseVerifier::system(),
        workspace.join("cache"),
    );
    let synced = manager
        .sync(&DriverLock::checked_in().expect("checked-in driver lock"))
        .expect("sync locked driver release");
    let kernel_export = workspace.join("kernel-target");
    let artifacts = workspace.join("artifacts");
    let tool = DriverTool::new(
        SystemCommandRunner,
        synced.source_root().to_owned(),
        DriverContext {
            target,
            kernel_release: "6.18.34+rpt-rpi-v8".into(),
            kernel_export: kernel_export.clone(),
            artifacts: artifacts.clone(),
            replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
        },
    )
    .expect("live driver tool");

    tool.run(DriverAction::ExportKernel)
        .expect("export live kernel");
    tool.run(DriverAction::Build)
        .expect("build live kernel from locked release");

    assert!(
        kernel_export
            .join("6.18.34+rpt-rpi-v8/target.txt")
            .is_file()
    );
    assert!(
        artifacts
            .join("6.18.34+rpt-rpi-v8/hyperpixel2r_kms.ko")
            .is_file()
    );
    assert!(!synced.source_root().join(".git").exists());
    assert!(!synced.source_root().join("dist").exists());
}

#[test]
#[ignore = "reboots the explicitly authorized live Pi into one-shot tryboot"]
fn locked_release_stages_the_live_tryboot_through_the_typed_adapter() {
    let target =
        std::env::var("PLANERADAR_DRIVER_LIVE_TARGET").expect("live target environment variable");
    let workspace = PathBuf::from(
        std::env::var_os("PLANERADAR_DRIVER_LIVE_WORKSPACE")
            .expect("live workspace environment variable"),
    );
    assert!(workspace.is_absolute());
    let manager = DriverManager::new(
        GhDriverReleaseSource::system(),
        GhDriverReleaseVerifier::system(),
        workspace.join("cache"),
    );
    let synced = manager
        .sync(&DriverLock::checked_in().expect("checked-in driver lock"))
        .expect("sync locked driver release");
    let tool = DriverTool::new(
        SystemCommandRunner,
        synced.source_root().to_owned(),
        DriverContext {
            target,
            kernel_release: "6.18.34+rpt-rpi-v8".into(),
            kernel_export: workspace.join("kernel-target"),
            artifacts: workspace.join("artifacts"),
            replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
        },
    )
    .expect("live driver tool");

    tool.run(DriverAction::StageTryboot)
        .expect("stage locked driver into one-shot tryboot");
}
