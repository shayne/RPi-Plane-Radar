use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{Read, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use planeradarctl::{
    DriverLock,
    driver::{
        DriverAction, DriverContext, DriverError, DriverManager, DriverPlan, DriverReleaseSource,
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
const DRIVER_TREE: &str = "1111111111111111111111111111111111111111";
const TAG_OBJECT: &str = "e205b33925c9f0cfe7be5b47d30c5a013a3577ac";
const EXPECTED_VERMAGIC: &str = "6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64";

fn probe(kernel_release: &str) -> TargetProbe {
    let vermagic = if kernel_release == "6.18.34+rpt-rpi-v8" {
        EXPECTED_VERMAGIC.to_owned()
    } else {
        format!("{kernel_release} SMP preempt mod_unload modversions aarch64")
    };
    TargetProbe::new(kernel_release, vermagic).expect("valid target probe")
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
        EXPECTED_VERMAGIC,
        internal_manifest_digest,
        MANIFEST_DIGEST,
    )
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn write_executable(path: &std::path::Path, contents: &[u8]) {
    fs::write(path, contents).expect("write executable fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make fixture executable");
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

fn source_archive_with_extra_files(
    entries: impl IntoIterator<Item = (String, Vec<u8>)>,
) -> Vec<u8> {
    let prefix = "hyperpixel2r-kms-0.1.0";
    let identity = format!(
        "schema_version\t1\nrepository\thttps://github.com/shayne/hyperpixel2r-kms\nsource_revision\t{DRIVER_COMMIT}\nsource_tree\t{DRIVER_TREE}\n"
    );
    let mut files = vec![
        (
            format!("{prefix}/scripts/verify-boot.sh"),
            b"#!/usr/bin/env bash\n".to_vec(),
        ),
        (
            format!("{prefix}/release/source-identity.txt"),
            identity.into_bytes(),
        ),
    ];
    files.extend(entries);
    let encoder = zstd::Encoder::new(Vec::new(), 1).expect("zstd encoder");
    let mut builder = Builder::new(encoder);
    for (path, contents) in files {
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(1);
        header.set_size(contents.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, path, contents.as_slice())
            .expect("tar entry");
    }
    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish zstd")
}

fn source_archive() -> Vec<u8> {
    source_archive_with_identity(
        "https://github.com/shayne/hyperpixel2r-kms",
        DRIVER_COMMIT,
        DRIVER_TREE,
    )
}

fn source_archive_with_identity(repository: &str, commit: &str, tree: &str) -> Vec<u8> {
    let identity = format!(
        "schema_version\t1\nrepository\t{repository}\nsource_revision\t{commit}\nsource_tree\t{tree}\n"
    );
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
        (
            "hyperpixel2r-kms-0.1.0/release/source-identity.txt",
            EntryType::Regular,
            identity.as_bytes(),
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
        let (bundle_manifest_sha256, vermagic) = prebuilt_contract(bytes);
        artifacts.push(serde_json::json!({
            "name": format!("hyperpixel2r-kms-{kernel_release}-aarch64.tar.zst"),
            "kind": "exact-kernel-bundle",
            "sha256": digest(bytes),
            "size": bytes.len(),
            "architecture": "aarch64",
            "kernel_release": kernel_release,
            "vermagic": vermagic,
            "bundle_manifest_sha256": bundle_manifest_sha256,
        }));
    }
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "driver_version": "0.1.0",
        "source": {
            "repository": "https://github.com/shayne/hyperpixel2r-kms",
            "commit": DRIVER_COMMIT,
            "tree": DRIVER_TREE,
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

fn prebuilt_contract(bytes: &[u8]) -> (String, String) {
    let decoder = zstd::Decoder::new(bytes).expect("prebuilt decoder");
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().expect("prebuilt entries") {
        let mut entry = entry.expect("prebuilt entry");
        if entry
            .path()
            .expect("prebuilt path")
            .ends_with("manifest.txt")
        {
            let mut manifest = Vec::new();
            entry.read_to_end(&mut manifest).expect("prebuilt manifest");
            let vermagic = std::str::from_utf8(&manifest)
                .expect("manifest UTF-8")
                .lines()
                .find_map(|line| line.strip_prefix("module_vermagic\t"))
                .expect("manifest vermagic")
                .to_owned();
            return (digest(&manifest), vermagic);
        }
    }
    panic!("prebuilt manifest missing");
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
    fn verify(&self, lock: &DriverLock, assets: &[PathBuf]) -> Result<(), DriverError> {
        self.calls
            .lock()
            .expect("verifier lock")
            .push((lock.version.clone(), assets.len()));
        if self.reject {
            Err(DriverError::VerificationFailed)
        } else {
            Ok(())
        }
    }
}

#[test]
fn sync_rejects_source_identity_repository_commit_and_tree_mismatches() {
    for (repository, commit, tree) in [
        (
            "https://github.com/attacker/hyperpixel2r-kms",
            DRIVER_COMMIT,
            DRIVER_TREE,
        ),
        (
            "https://github.com/shayne/hyperpixel2r-kms",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            DRIVER_TREE,
        ),
        (
            "https://github.com/shayne/hyperpixel2r-kms",
            DRIVER_COMMIT,
            "cccccccccccccccccccccccccccccccccccccccc",
        ),
    ] {
        let source = source_archive_with_identity(repository, commit, tree);
        let manifest = release_manifest(&source, None);
        let manager = DriverManager::new(
            FakeReleaseSource::with_assets([
                ("driver-manifest.json".into(), manifest.clone()),
                ("hyperpixel2r-kms-source.tar.zst".into(), source),
                ("SBOM.spdx.json".into(), b"sbom".to_vec()),
            ]),
            FakeReleaseVerifier::default(),
            tempfile::tempdir()
                .expect("temporary cache parent")
                .keep()
                .join("cache"),
        );

        assert!(
            manager.sync(&lock_for(&manifest)).is_err(),
            "mismatched extracted source identity was accepted: {repository} {commit} {tree}"
        );
    }
}

#[test]
fn sync_rejects_materialized_script_and_mode_tampering() {
    for tamper in [
        "script-bytes",
        "script-mode",
        "directory-mode",
        "materialized-root-mode",
    ] {
        let source = source_archive();
        let manifest = release_manifest(&source, None);
        let manager = DriverManager::new(
            FakeReleaseSource::with_assets([
                ("driver-manifest.json".into(), manifest.clone()),
                ("hyperpixel2r-kms-source.tar.zst".into(), source),
                ("SBOM.spdx.json".into(), b"sbom".to_vec()),
            ]),
            FakeReleaseVerifier::default(),
            tempfile::tempdir()
                .expect("temporary cache parent")
                .keep()
                .join("cache"),
        );
        let synced = manager.sync(&lock_for(&manifest)).expect("initial sync");
        let script = synced.source_root().join("scripts/verify-boot.sh");
        match tamper {
            "script-bytes" => fs::write(&script, b"#!/bin/sh\nmalicious\n").expect("tamper script"),
            "script-mode" => fs::set_permissions(&script, fs::Permissions::from_mode(0o777))
                .expect("tamper script mode"),
            "directory-mode" => fs::set_permissions(
                synced.source_root().join("scripts"),
                fs::Permissions::from_mode(0o777),
            )
            .expect("tamper directory mode"),
            "materialized-root-mode" => {
                fs::set_permissions(synced.source_root(), fs::Permissions::from_mode(0o777))
                    .expect("tamper materialized root mode")
            }
            _ => unreachable!(),
        }

        assert!(
            manager.sync(&lock_for(&manifest)).is_err(),
            "materialized cache accepted {tamper} tampering"
        );
    }
}

#[test]
fn sync_rejects_a_symlinked_cache_root_ancestor() {
    let temporary = tempfile::tempdir().expect("temporary cache parent");
    let outside = temporary.path().join("outside");
    fs::create_dir(&outside).expect("outside cache directory");
    let cache_link = temporary.path().join(".cache");
    symlink(&outside, &cache_link).expect("symlinked cache ancestor");
    let source = source_archive();
    let manifest = release_manifest(&source, None);
    let manager = DriverManager::new(
        FakeReleaseSource::with_assets([
            ("driver-manifest.json".into(), manifest.clone()),
            ("hyperpixel2r-kms-source.tar.zst".into(), source),
            ("SBOM.spdx.json".into(), b"sbom".to_vec()),
        ]),
        FakeReleaseVerifier::default(),
        cache_link.join("driver"),
    );

    assert!(manager.sync(&lock_for(&manifest)).is_err());
    assert!(
        fs::read_dir(&outside)
            .expect("outside cache remains readable")
            .next()
            .is_none(),
        "sync wrote through a symlinked cache ancestor"
    );
}

#[test]
fn sync_rejects_a_populated_cache_behind_a_symlinked_ancestor() {
    let temporary = tempfile::tempdir().expect("temporary cache parent");
    let outside = temporary.path().join("outside");
    fs::create_dir(&outside).expect("outside cache directory");
    let source = source_archive();
    let manifest = release_manifest(&source, None);
    let assets = || {
        FakeReleaseSource::with_assets([
            ("driver-manifest.json".into(), manifest.clone()),
            ("hyperpixel2r-kms-source.tar.zst".into(), source.clone()),
            ("SBOM.spdx.json".into(), b"sbom".to_vec()),
        ])
    };
    let outside_manager = DriverManager::new(
        assets(),
        FakeReleaseVerifier::default(),
        outside.join("driver"),
    );
    outside_manager
        .sync(&lock_for(&manifest))
        .expect("populate outside cache");

    let cache_link = temporary.path().join(".cache");
    symlink(&outside, &cache_link).expect("symlink populated cache ancestor");
    let linked_manager = DriverManager::new(
        assets(),
        FakeReleaseVerifier::default(),
        cache_link.join("driver"),
    );

    assert!(
        linked_manager.sync(&lock_for(&manifest)).is_err(),
        "populated cache behind a symlinked ancestor was accepted"
    );
}

fn manager_for_source_archive(
    source: Vec<u8>,
) -> (
    DriverManager<FakeReleaseSource, FakeReleaseVerifier>,
    DriverLock,
) {
    let manifest = release_manifest(&source, None);
    let lock = lock_for(&manifest);
    let manager = DriverManager::new(
        FakeReleaseSource::with_assets([
            ("driver-manifest.json".into(), manifest),
            ("hyperpixel2r-kms-source.tar.zst".into(), source),
            ("SBOM.spdx.json".into(), b"sbom".to_vec()),
        ]),
        FakeReleaseVerifier::default(),
        tempfile::tempdir()
            .expect("temporary cache parent")
            .keep()
            .join("cache"),
    );
    (manager, lock)
}

#[test]
fn sync_rejects_an_archive_entry_over_the_uncompressed_limit() {
    let source = source_archive_with_extra_files([(
        "hyperpixel2r-kms-0.1.0/oversized.bin".into(),
        vec![b'a'; 8 * 1024 * 1024 + 1],
    )]);
    let (manager, lock) = manager_for_source_archive(source);

    assert!(
        manager.sync(&lock).is_err(),
        "oversized uncompressed archive entry was accepted"
    );
}

#[test]
fn sync_rejects_an_archive_over_the_total_uncompressed_limit() {
    let source = source_archive_with_extra_files((0..3).map(|index| {
        (
            format!("hyperpixel2r-kms-0.1.0/large-{index}.bin"),
            vec![b'a'; 6 * 1024 * 1024],
        )
    }));
    let (manager, lock) = manager_for_source_archive(source);

    assert!(
        manager.sync(&lock).is_err(),
        "archive exceeding the total uncompressed limit was accepted"
    );
}

#[test]
fn sync_rejects_an_archive_over_the_entry_count_limit() {
    let source = source_archive_with_extra_files((0..1025).map(|index| {
        (
            format!("hyperpixel2r-kms-0.1.0/many/{index:04}.txt"),
            b"x".to_vec(),
        )
    }));
    let (manager, lock) = manager_for_source_archive(source);

    assert!(
        manager.sync(&lock).is_err(),
        "archive exceeding the entry-count limit was accepted"
    );
}

#[test]
fn sync_rejects_an_archive_path_over_the_depth_limit() {
    let deep = std::iter::repeat_n("deep", 33)
        .collect::<Vec<_>>()
        .join("/");
    let source = source_archive_with_extra_files([(
        format!("hyperpixel2r-kms-0.1.0/{deep}/leaf"),
        b"x".to_vec(),
    )]);
    let (manager, lock) = manager_for_source_archive(source);

    assert!(
        manager.sync(&lock).is_err(),
        "archive path exceeding the depth limit was accepted"
    );
}

#[test]
fn exact_kernel_selects_prebuilt_and_new_kernel_falls_back_to_cross_build() {
    let resolver = resolver(
        prebuilt("6.18.34+rpt-rpi-v8", EXPECTED_VERMAGIC, MANIFEST_DIGEST)
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
    let suffix_drift_probe = TargetProbe::new(
        "6.18.34+rpt-rpi-v8",
        "6.18.34+rpt-rpi-v8 SMP preempt mod_unload aarch64",
    )
    .expect("valid drifted target probe");
    assert!(matches!(
        resolver
            .resolve(&suffix_drift_probe)
            .expect("resolve vermagic drift"),
        DriverPlan::CrossBuild { .. }
    ));
}

#[test]
fn prebuilt_identity_rejects_kernel_vermagic_and_internal_manifest_digest_drift() {
    for (kernel_release, vermagic, internal_digest) in [
        ("6.18.35+rpt-rpi-v8", EXPECTED_VERMAGIC, MANIFEST_DIGEST),
        (
            "6.18.34+rpt-rpi-v8",
            "6.18.35+rpt-rpi-v8 SMP preempt mod_unload aarch64",
            MANIFEST_DIGEST,
        ),
        (
            "6.18.34+rpt-rpi-v8",
            EXPECTED_VERMAGIC,
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
    let prebuilt = prebuilt_archive("6.18.34+rpt-rpi-v8", EXPECTED_VERMAGIC, None);
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
fn resolved_driver_plan_drives_the_exact_prebuilt_and_crossbuild_command_sequences() {
    let source = source_archive();
    let prebuilt = prebuilt_archive("6.18.34+rpt-rpi-v8", EXPECTED_VERMAGIC, None);
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
            .expect("temporary cache parent")
            .keep()
            .join("cache"),
    );
    let synced = manager.sync(&lock_for(&manifest)).expect("sync driver");

    let prebuilt_runner = RecordingRunner::default();
    let prebuilt_tool = synced
        .tool(
            prebuilt_runner.clone(),
            &probe("6.18.34+rpt-rpi-v8"),
            DriverContext {
                target: "shayne@planeradar.local".into(),
                kernel_release: "6.18.34+rpt-rpi-v8".into(),
                kernel_export: PathBuf::from("/cache/kernel"),
                artifacts: PathBuf::from("/cache/artifacts"),
                replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
            },
        )
        .expect("prebuilt tool");
    prebuilt_tool
        .prepare_and_stage()
        .expect("prepare prebuilt plan");
    let prebuilt_calls = prebuilt_runner
        .invocations
        .lock()
        .expect("prebuilt invocation lock");
    assert_eq!(
        prebuilt_calls
            .iter()
            .map(|invocation| invocation.arguments()[0]
                .rsplit('/')
                .next()
                .expect("script"))
            .collect::<Vec<_>>(),
        ["export-target-kbuild.sh", "stage-tryboot.sh"]
    );
    assert!(
        prebuilt_calls[1]
            .arguments()
            .windows(2)
            .any(|pair| pair[0] == "--artifact-dir" && pair[1].contains("/prebuilt/")),
        "prebuilt plan did not stage the verified extracted bundle"
    );
    drop(prebuilt_calls);

    let crossbuild_runner = RecordingRunner::default();
    let new_kernel = "6.18.35+rpt-rpi-v8";
    let crossbuild_tool = synced
        .tool(
            crossbuild_runner.clone(),
            &probe(new_kernel),
            DriverContext {
                target: "shayne@planeradar.local".into(),
                kernel_release: new_kernel.into(),
                kernel_export: PathBuf::from("/cache/kernel"),
                artifacts: PathBuf::from("/cache/artifacts"),
                replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
            },
        )
        .expect("crossbuild tool");
    crossbuild_tool
        .prepare_and_stage()
        .expect("prepare crossbuild plan");
    let crossbuild_calls = crossbuild_runner
        .invocations
        .lock()
        .expect("crossbuild invocation lock");
    assert_eq!(
        crossbuild_calls
            .iter()
            .map(|invocation| invocation.arguments()[0]
                .rsplit('/')
                .next()
                .expect("script"))
            .collect::<Vec<_>>(),
        [
            "export-target-kbuild.sh",
            "build-driver.sh",
            "stage-tryboot.sh"
        ]
    );
    assert!(
        crossbuild_calls[1]
            .arguments()
            .windows(2)
            .any(|pair| pair == ["--source-revision", DRIVER_COMMIT]),
        "crossbuild did not bind the locked source revision"
    );
}

#[test]
fn prebuilt_preparation_executes_target_export_before_the_real_stage_boundary() {
    let temporary = tempfile::tempdir().expect("temporary production contract");
    let source = temporary.path().join("source");
    let scripts = source.join("scripts");
    let prebuilt = temporary.path().join("prebuilt");
    let kernel_target = temporary.path().join("kernel-target");
    let artifacts = temporary.path().join("artifacts");
    fs::create_dir_all(&scripts).expect("source scripts");
    fs::create_dir(&prebuilt).expect("verified prebuilt directory");
    write_executable(
        &scripts.join("export-target-kbuild.sh"),
        br#"#!/usr/bin/env bash
set -euo pipefail
output=''
while test "$#" -gt 0; do
  case "$1" in
    --target) shift 2 ;;
    --output) output="$2"; shift 2 ;;
    *) exit 64 ;;
  esac
done
test -n "$output"
mkdir -p "$output/6.18.34+rpt-rpi-v8"
printf 'schema_version\t1\nkernel_release\t6.18.34+rpt-rpi-v8\n' \
  > "$output/6.18.34+rpt-rpi-v8/target.txt"
"#,
    );
    write_executable(
        &scripts.join("stage-tryboot.sh"),
        br#"#!/usr/bin/env bash
set -euo pipefail
kernel_target=''
artifact_dir=''
while test "$#" -gt 0; do
  case "$1" in
    --target|--replace-overlay) shift 2 ;;
    --kernel-target) kernel_target="$2"; shift 2 ;;
    --artifact-dir) artifact_dir="$2"; shift 2 ;;
    *) exit 64 ;;
  esac
done
test -d "$artifact_dir"
test -f "$kernel_target/6.18.34+rpt-rpi-v8/target.txt"
printf 'staged\n' > "$kernel_target/stage-complete"
"#,
    );
    write_executable(
        &scripts.join("build-driver.sh"),
        br#"#!/usr/bin/env bash
set -euo pipefail
output=''
while test "$#" -gt 0; do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    *) shift 2 ;;
  esac
done
mkdir -p "$output"
printf 'unexpected build\n' > "$output/build-was-run"
"#,
    );
    let tool = DriverTool::new(
        SystemCommandRunner,
        source,
        DriverPlan::Prebuilt { archive: prebuilt },
        DriverContext {
            target: "pi@fixture".into(),
            kernel_release: "6.18.34+rpt-rpi-v8".into(),
            kernel_export: kernel_target.clone(),
            artifacts: artifacts.clone(),
            replace_overlay: "hyperpixel2r-kms-aaaaaaaaaaaa.dtbo".into(),
        },
        "0.1.0".into(),
        DRIVER_COMMIT.into(),
    )
    .expect("production prebuilt tool");

    tool.prepare_and_stage()
        .expect("fresh-target prebuilt preparation");

    assert_eq!(
        fs::read_to_string(kernel_target.join("stage-complete")).expect("stage marker"),
        "staged\n"
    );
    assert!(
        !artifacts.join("build-was-run").exists(),
        "prebuilt preparation invoked Build"
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
fn sync_rejects_prebuilt_vermagic_suffix_and_bundle_manifest_digest_drift() {
    for mutation in [
        "vermagic-suffix",
        "bundle-manifest-digest",
        "missing-contract",
    ] {
        let source = source_archive();
        let prebuilt = prebuilt_archive(
            "6.18.34+rpt-rpi-v8",
            "6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64",
            None,
        );
        let mut manifest: serde_json::Value = serde_json::from_slice(&release_manifest(
            &source,
            Some((&prebuilt, "6.18.34+rpt-rpi-v8")),
        ))
        .expect("release manifest");
        let exact = manifest["artifacts"]
            .as_array_mut()
            .expect("artifact array")
            .iter_mut()
            .find(|artifact| artifact["kind"] == "exact-kernel-bundle")
            .expect("exact artifact");
        match mutation {
            "vermagic-suffix" => {
                exact["vermagic"] =
                    serde_json::json!("6.18.34+rpt-rpi-v8 SMP preempt mod_unload aarch64");
            }
            "bundle-manifest-digest" => {
                exact["bundle_manifest_sha256"] = serde_json::json!(
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                );
            }
            "missing-contract" => {
                exact
                    .as_object_mut()
                    .expect("exact artifact object")
                    .remove("bundle_manifest_sha256");
            }
            _ => unreachable!(),
        }
        let manifest = serde_json::to_vec(&manifest).expect("mutated manifest");
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
            "prebuilt accepted {mutation} drift"
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
fn github_verifier_binds_the_dereferenced_tag_commit_and_release_workflow_to_every_asset() {
    let runner = SequencedRunner::new([
        Ok(CommandOutput::success(
            format!("tag\t{TAG_OBJECT}\n").into_bytes(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            format!("commit\t{DRIVER_COMMIT}\n").into_bytes(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(Vec::new(), Vec::new())),
        Ok(CommandOutput::success(Vec::new(), Vec::new())),
    ]);
    let verifier = GhDriverReleaseVerifier::new(runner.clone());
    let manifest = release_manifest(&source_archive(), None);
    verifier
        .verify(
            &lock_for(&manifest),
            &[
                PathBuf::from("/cache/driver-manifest.json"),
                PathBuf::from("/cache/source.tar.zst"),
            ],
        )
        .expect("verify driver release");

    let invocations = runner.invocations.lock().expect("verification invocations");
    assert_eq!(invocations.len(), 4);
    assert_eq!(
        invocations[0].arguments(),
        [
            "api",
            "repos/shayne/hyperpixel2r-kms/git/ref/tags/v0.1.0-rc.11",
            "--jq",
            r#".object.type + "\t" + .object.sha"#,
        ]
    );
    assert_eq!(
        invocations[1].arguments(),
        [
            "api",
            &format!("repos/shayne/hyperpixel2r-kms/git/tags/{TAG_OBJECT}"),
            "--jq",
            r#".object.type + "\t" + .object.sha"#,
        ]
    );
    for (invocation, asset) in invocations[2..]
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
                "--signer-workflow",
                "github.com/shayne/hyperpixel2r-kms/.github/workflows/release.yml",
                "--source-digest",
                DRIVER_COMMIT,
            ]
        );
    }
}

#[test]
fn github_verifier_rejects_a_tag_dereferencing_to_another_commit() {
    let runner = SequencedRunner::new([
        Ok(CommandOutput::success(
            format!("tag\t{TAG_OBJECT}\n").into_bytes(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            b"commit\tbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n".to_vec(),
            Vec::new(),
        )),
    ]);
    let verifier = GhDriverReleaseVerifier::new(runner);
    let manifest = release_manifest(&source_archive(), None);

    assert!(
        verifier
            .verify(
                &lock_for(&manifest),
                &[
                    PathBuf::from("/cache/driver-manifest.json"),
                    PathBuf::from("/cache/source.tar.zst"),
                ],
            )
            .is_err(),
        "release tag pointing at another commit was accepted"
    );
}

#[test]
fn github_verifier_rejects_a_disallowed_attestation_signer() {
    let runner = SequencedRunner::new([
        Ok(CommandOutput::success(
            format!("tag\t{TAG_OBJECT}\n").into_bytes(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            format!("commit\t{DRIVER_COMMIT}\n").into_bytes(),
            Vec::new(),
        )),
        Ok(CommandOutput::new(
            1,
            Vec::new(),
            b"attestation signer workflow does not match\n".to_vec(),
        )),
    ]);
    let verifier = GhDriverReleaseVerifier::new(runner);
    let manifest = release_manifest(&source_archive(), None);

    assert!(
        verifier
            .verify(
                &lock_for(&manifest),
                &[PathBuf::from("/cache/driver-manifest.json")],
            )
            .is_err(),
        "attestation from a disallowed signer was accepted"
    );
}

#[test]
fn release_verification_preserves_bounded_nonzero_and_spawn_diagnostics() {
    let manifest = release_manifest(&source_archive(), None);
    let lock = lock_for(&manifest);
    let mut stderr = vec![b'x'; 8 * 1024];
    stderr.extend_from_slice(b"\x00unsafe-control");
    let nonzero = GhDriverReleaseVerifier::new(SequencedRunner::new([
        Ok(CommandOutput::success(
            format!("tag\t{TAG_OBJECT}\n").into_bytes(),
            Vec::new(),
        )),
        Ok(CommandOutput::success(
            format!("commit\t{DRIVER_COMMIT}\n").into_bytes(),
            Vec::new(),
        )),
        Ok(CommandOutput::new(23, Vec::new(), stderr)),
    ]));
    let error = nonzero
        .verify(&lock, &[PathBuf::from("/cache/driver-manifest.json")])
        .expect_err("nonzero attestation verification");
    match error {
        DriverError::ReleaseCommandFailed {
            program,
            status,
            stderr,
        } => {
            assert_eq!(program, "gh");
            assert_eq!(status, 23);
            assert!(!stderr.is_empty());
            assert!(stderr.len() <= 4096);
            assert!(!stderr.contains('\0'));
        }
        other => panic!("wrong nonzero diagnostic: {other:?}"),
    }

    let spawn = GhDriverReleaseVerifier::new(SequencedRunner::new([Err(RunnerError::TimedOut)]));
    let error = spawn
        .verify(&lock, &[PathBuf::from("/cache/driver-manifest.json")])
        .expect_err("spawn failure");
    assert!(matches!(
        error,
        DriverError::ReleaseCommandSpawn {
            ref program,
            source: RunnerError::TimedOut,
        } if program == "gh"
    ));
}

#[test]
fn lifecycle_tools_preserve_bounded_nonzero_and_spawn_diagnostics() {
    let context = DriverContext {
        target: "shayne@planeradar.local".into(),
        kernel_release: "6.18.34+rpt-rpi-v8".into(),
        kernel_export: PathBuf::from("/cache/kernel"),
        artifacts: PathBuf::from("/cache/artifacts"),
        replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
    };
    let nonzero = DriverTool::new(
        SequencedRunner::new([Ok(CommandOutput::new(19, Vec::new(), vec![b'e'; 8 * 1024]))]),
        PathBuf::from("/cache/source"),
        DriverPlan::CrossBuild {
            source: PathBuf::from("/cache/source"),
        },
        context.clone(),
        "0.1.0".into(),
        DRIVER_COMMIT.into(),
    )
    .expect("nonzero tool");
    let error = nonzero
        .run(DriverAction::StageTryboot)
        .expect_err("nonzero lifecycle command");
    match error {
        DriverError::ToolCommandFailed {
            action,
            program,
            status,
            stderr,
        } => {
            assert_eq!(action, DriverAction::StageTryboot);
            assert_eq!(program, "bash");
            assert_eq!(status, 19);
            assert!(!stderr.is_empty());
            assert!(stderr.len() <= 4096);
        }
        other => panic!("wrong lifecycle nonzero diagnostic: {other:?}"),
    }

    let spawn = DriverTool::new(
        SequencedRunner::new([Err(RunnerError::Failed)]),
        PathBuf::from("/cache/source"),
        DriverPlan::CrossBuild {
            source: PathBuf::from("/cache/source"),
        },
        context,
        "0.1.0".into(),
        DRIVER_COMMIT.into(),
    )
    .expect("spawn tool");
    let error = spawn
        .run(DriverAction::RollbackBoot)
        .expect_err("lifecycle spawn failure");
    assert!(matches!(
        error,
        DriverError::ToolCommandSpawn {
            action: DriverAction::RollbackBoot,
            ref program,
            source: RunnerError::Failed,
        } if program == "bash"
    ));
}

#[derive(Clone, Default)]
struct RecordingRunner {
    invocations: Arc<Mutex<Vec<Invocation>>>,
}

#[derive(Clone)]
struct SequencedRunner {
    invocations: Arc<Mutex<Vec<Invocation>>>,
    results: Arc<Mutex<VecDeque<Result<CommandOutput, RunnerError>>>>,
}

impl SequencedRunner {
    fn new(results: impl IntoIterator<Item = Result<CommandOutput, RunnerError>>) -> Self {
        Self {
            invocations: Arc::new(Mutex::new(Vec::new())),
            results: Arc::new(Mutex::new(results.into_iter().collect())),
        }
    }
}

impl CommandRunner for SequencedRunner {
    fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError> {
        self.invocations
            .lock()
            .expect("sequenced invocation lock")
            .push(invocation);
        self.results
            .lock()
            .expect("sequenced results lock")
            .pop_front()
            .expect("sequenced runner exhausted")
    }
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
        DriverPlan::CrossBuild {
            source: PathBuf::from("/cache/source"),
        },
        DriverContext {
            target: "shayne@planeradar.local".into(),
            kernel_release: "6.18.34+rpt-rpi-v8".into(),
            kernel_export: PathBuf::from("/cache/kernel"),
            artifacts: PathBuf::from("/cache/artifacts"),
            replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
        },
        "0.1.0".into(),
        DRIVER_COMMIT.into(),
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
    assert_eq!(
        invocations[3].arguments(),
        [
            "/cache/source/scripts/verify-boot.sh",
            "--target",
            "shayne@planeradar.local",
            "--expect-tryboot",
            "--expect-driver-version",
            "0.1.0",
            "--expect-overlay-file",
            "hyperpixel2r-kms-ca95ffeb30b3.dtbo",
            "--json",
        ]
    );
}

#[test]
fn verify_boot_rejects_json_for_a_different_locked_driver_version() {
    let runner = SequencedRunner::new([Ok(CommandOutput::success(
        br#"{"schema_version":1,"driver_version":"0.1.1","kernel_release":"6.18.34+rpt-rpi-v8","module":"hyperpixel2r_kms","drm_mode":"480x480","touch":true,"sdl_driver":"KMSDRM","renderer":"opengles2","accepted":true}"#.to_vec(),
        Vec::new(),
    ))]);
    let tool = DriverTool::new(
        runner.clone(),
        PathBuf::from("/cache/source"),
        DriverPlan::CrossBuild {
            source: PathBuf::from("/cache/source"),
        },
        DriverContext {
            target: "shayne@planeradar.local".into(),
            kernel_release: "6.18.34+rpt-rpi-v8".into(),
            kernel_export: PathBuf::from("/cache/kernel"),
            artifacts: PathBuf::from("/cache/artifacts"),
            replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
        },
        "0.1.0".into(),
        DRIVER_COMMIT.into(),
    )
    .expect("valid locked tool");

    assert!(matches!(
        tool.run(DriverAction::VerifyBoot),
        Err(DriverError::InvalidVerification)
    ));
    assert_eq!(
        runner.invocations.lock().expect("invocation lock")[0].arguments(),
        [
            "/cache/source/scripts/verify-boot.sh",
            "--target",
            "shayne@planeradar.local",
            "--expect-tryboot",
            "--expect-driver-version",
            "0.1.0",
            "--expect-overlay-file",
            "hyperpixel2r-kms-ca95ffeb30b3.dtbo",
            "--json",
        ]
    );
}

#[test]
fn normal_boot_verification_keeps_the_same_locked_candidate_identity() {
    let runner = RecordingRunner::default();
    let tool = DriverTool::new(
        runner.clone(),
        PathBuf::from("/cache/source"),
        DriverPlan::CrossBuild {
            source: PathBuf::from("/cache/source"),
        },
        DriverContext {
            target: "shayne@planeradar.local".into(),
            kernel_release: "6.18.34+rpt-rpi-v8".into(),
            kernel_export: PathBuf::from("/cache/kernel"),
            artifacts: PathBuf::from("/cache/artifacts"),
            replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
        },
        "0.1.0".into(),
        DRIVER_COMMIT.into(),
    )
    .expect("valid locked tool");

    let verification = tool.verify_normal_boot().expect("strict normal boot");
    assert!(verification.accepted);
    assert_eq!(
        runner.invocations.lock().expect("invocation lock")[0].arguments(),
        [
            "/cache/source/scripts/verify-boot.sh",
            "--target",
            "shayne@planeradar.local",
            "--expect-normal",
            "--expect-driver-version",
            "0.1.0",
            "--expect-overlay-file",
            "hyperpixel2r-kms-ca95ffeb30b3.dtbo",
            "--json",
        ]
    );
}

#[test]
fn legacy_cleanup_uses_the_locked_overlay_through_the_existing_uninstall_action() {
    let runner = RecordingRunner::default();
    let tool = DriverTool::new(
        runner.clone(),
        PathBuf::from("/cache/source"),
        DriverPlan::CrossBuild {
            source: PathBuf::from("/cache/source"),
        },
        DriverContext {
            target: "shayne@planeradar.local".into(),
            kernel_release: "6.18.34+rpt-rpi-v8".into(),
            kernel_export: PathBuf::from("/cache/kernel"),
            artifacts: PathBuf::from("/cache/artifacts"),
            replace_overlay: "planeradar-hyperpixel2r-eefaf3ae40fd".into(),
        },
        "0.1.0".into(),
        DRIVER_COMMIT.into(),
    )
    .expect("valid locked tool");

    tool.cleanup_legacy_planeradar()
        .expect("exact legacy cleanup");

    assert_eq!(
        runner.invocations.lock().expect("invocation lock")[0].arguments(),
        [
            "/cache/source/scripts/uninstall.sh",
            "--target",
            "shayne@planeradar.local",
            "--cleanup-legacy-planeradar",
            "--expect-overlay-file",
            "hyperpixel2r-kms-ca95ffeb30b3.dtbo",
        ]
    );
}

#[test]
#[ignore = "runs the explicitly selected live driver lifecycle phase"]
fn locked_release_runs_the_selected_live_phase_through_the_typed_adapter() {
    let target =
        std::env::var("PLANERADAR_DRIVER_LIVE_TARGET").expect("live target environment variable");
    let workspace = PathBuf::from(
        std::env::var_os("PLANERADAR_DRIVER_LIVE_WORKSPACE")
            .expect("live workspace environment variable"),
    );
    let replace_overlay = std::env::var("PLANERADAR_DRIVER_REPLACE_OVERLAY")
        .expect("currently accepted overlay environment variable");
    let phase =
        std::env::var("PLANERADAR_DRIVER_LIVE_PHASE").expect("live lifecycle phase variable");
    assert!(workspace.is_absolute());
    let manager = DriverManager::new(
        GhDriverReleaseSource::system(),
        GhDriverReleaseVerifier::system(),
        workspace.join("cache"),
    );
    let synced = manager
        .sync(&DriverLock::checked_in().expect("checked-in driver lock"))
        .expect("sync locked driver release");
    let tool = synced
        .tool(
            SystemCommandRunner,
            &TargetProbe::new("6.18.34+rpt-rpi-v8", EXPECTED_VERMAGIC).expect("live target probe"),
            DriverContext {
                target,
                kernel_release: "6.18.34+rpt-rpi-v8".into(),
                kernel_export: workspace.join("kernel-target"),
                artifacts: workspace.join("artifacts"),
                replace_overlay,
            },
        )
        .expect("live locked driver tool");

    match phase.as_str() {
        "prepare-and-stage" => tool
            .prepare_and_stage()
            .expect("prepare and stage live driver"),
        "accept-tryboot" => {
            let verification = tool
                .run(DriverAction::VerifyBoot)
                .expect("strict tryboot verification")
                .expect("verification JSON");
            assert!(verification.accepted);
            tool.run(DriverAction::CommitBoot)
                .expect("commit accepted tryboot");
        }
        "accept-normal-cleanup" => {
            assert!(
                tool.verify_normal_boot()
                    .expect("strict normal boot")
                    .accepted
            );
            tool.cleanup_legacy_planeradar()
                .expect("exact legacy cleanup");
            tool.cleanup_legacy_planeradar()
                .expect("idempotent exact legacy cleanup");
        }
        _ => panic!("unsupported live driver phase: {phase}"),
    }
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
    let tool = synced
        .tool(
            SystemCommandRunner,
            &TargetProbe::new("6.18.34+rpt-rpi-v8", EXPECTED_VERMAGIC).expect("live target probe"),
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
    let tool = synced
        .tool(
            SystemCommandRunner,
            &TargetProbe::new("6.18.34+rpt-rpi-v8", EXPECTED_VERMAGIC).expect("live target probe"),
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
