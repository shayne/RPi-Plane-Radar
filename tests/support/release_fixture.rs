use std::fs;
use std::path::{Path, PathBuf};

use planeradarctl::DriverLock;
use planeradarctl::state::ArtifactIdentity;
use serde_json::json;
use sha2::{Digest, Sha256};

pub const SOURCE_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const SOURCE_TREE: &str = "cccccccccccccccccccccccccccccccccccccccc";
pub const SOURCE_DATE_EPOCH: u64 = 1_785_153_600;
pub const TASK_TWO_DRIVER_VERSION: &str = "0.1.1";
pub const TASK_TWO_DRIVER_COMMIT: &str = "bb76bf8a3e9e02ce1b1acd4df97200083ca57277";
pub const TASK_TWO_DRIVER_TREE: &str = "0693468744845cc03e91b6dfdd10b8cd676dbce6";
pub const TASK_TWO_DRIVER_DATE_EPOCH: u64 = 1_785_742_634;
pub const TASK_TWO_DRIVER_CAPABILITY: &str = "pwm-backlight-v1";

pub struct ReleaseFixture {
    pub directory: PathBuf,
    pub application: ArtifactIdentity,
    pub driver: ArtifactIdentity,
}

pub fn build_release(directory: &Path) -> ReleaseFixture {
    build_release_with_payload(directory, fixture_payload(4096))
}

pub fn build_release_with_payload(directory: &Path, payload: Vec<u8>) -> ReleaseFixture {
    let lock = DriverLock::checked_in().expect("checked-in driver lock");
    build_release_with_payload_and_driver(directory, payload, &lock)
}

pub fn build_release_with_driver(directory: &Path, lock: &DriverLock) -> ReleaseFixture {
    build_release_with_payload_and_driver(directory, fixture_payload(4096), lock)
}

fn build_release_with_payload_and_driver(
    directory: &Path,
    payload: Vec<u8>,
    lock: &DriverLock,
) -> ReleaseFixture {
    fs::create_dir_all(directory).expect("release fixture directory");
    let application_archive = application_archive(&payload);
    let control_aarch64 = b"deterministic aarch64 control archive\n".to_vec();
    let control_x86_64 = b"deterministic x86_64 control archive\n".to_vec();
    let application_archive_sha256 = sha256(&application_archive);
    let control_aarch64_sha256 = sha256(&control_aarch64);
    let control_x86_64_sha256 = sha256(&control_x86_64);
    let application_name = "planeradar-aarch64-linux-gnu.tar.zst";
    let control_aarch64_name = "planeradarctl-aarch64-apple-darwin.tar.zst";
    let control_x86_64_name = "planeradarctl-x86_64-apple-darwin.tar.zst";
    let manifest = json!({
        "schema_version": 1,
        "version": "0.1.0-rc.1",
        "source_commit": SOURCE_COMMIT,
        "source_tree": SOURCE_TREE,
        "source_timestamp": "2026-07-27T12:00:00Z",
        "source_date_epoch": SOURCE_DATE_EPOCH,
        "repository": "https://github.com/shayne/RPi-Plane-Radar",
        "workflow": {
            "repository": "shayne/RPi-Plane-Radar",
            "path": ".github/workflows/release.yml",
            "ref": "refs/heads/main",
            "commit": SOURCE_COMMIT
        },
        "supported": {
            "model": "Raspberry Pi Zero 2 W",
            "display": "HyperPixel 2.1 Round",
            "operating_system": "Raspberry Pi OS Lite Trixie (64-bit)",
            "architecture": "aarch64",
            "kernel_policy": "driver-manifest-supported"
        },
        "required_target_packages": [
            "avahi-daemon",
            "ca-certificates",
            "curl",
            "device-tree-compiler",
            "dkms",
            "evtest",
            "kmod",
            "libegl1",
            "libgl1-mesa-dri",
            "libgles2",
            "libsdl2-2.0-0",
            "pngcheck"
        ],
        "minimum_control_version": "0.1.0",
        "driver": {
            "repository": lock.repository.clone(),
            "version": lock.version.to_string(),
            "commit": lock.commit.clone(),
            "manifest_sha256": lock.manifest_sha256.clone(),
            "required_capability": lock.required_capability.clone(),
            "lifecycle_protocol": "accepted-driver-v2"
        },
        "artifacts": {
            application_name: {
                "kind": "application",
                "platform": "linux-gnu",
                "architecture": "aarch64",
                "size": application_archive.len(),
                "sha256": application_archive_sha256,
                "runnable": true
            },
            control_aarch64_name: {
                "kind": "control",
                "platform": "apple-darwin",
                "architecture": "aarch64",
                "size": control_aarch64.len(),
                "sha256": control_aarch64_sha256,
                "runnable": true
            },
            control_x86_64_name: {
                "kind": "control",
                "platform": "apple-darwin",
                "architecture": "x86_64",
                "size": control_x86_64.len(),
                "sha256": control_x86_64_sha256,
                "runnable": true
            }
        }
    });
    let manifest = serde_json::to_vec_pretty(&manifest).expect("release fixture manifest");
    let assets = [
        (
            "SBOM.spdx.json",
            b"{\"spdxVersion\":\"SPDX-2.3\"}\n".to_vec(),
        ),
        ("install.sh", b"#!/bin/sh\nexit 0\n".to_vec()),
        (application_name, application_archive),
        (control_aarch64_name, control_aarch64),
        (control_x86_64_name, control_x86_64),
        ("release-manifest.json", manifest),
    ];
    let mut checksums = String::new();
    for (name, contents) in assets {
        fs::write(directory.join(name), &contents).expect("release fixture asset");
        checksums.push_str(&format!("{}  {name}\n", sha256(&contents)));
    }
    fs::write(directory.join("SHA256SUMS"), checksums).expect("release fixture checksums");

    ReleaseFixture {
        directory: directory.to_owned(),
        application: ArtifactIdentity {
            version: "0.1.0-rc.1".into(),
            source_commit: SOURCE_COMMIT.into(),
            sha256: sha256(&payload),
        },
        driver: ArtifactIdentity {
            version: lock.version.to_string(),
            source_commit: lock.commit.clone(),
            sha256: lock.manifest_sha256.clone(),
        },
    }
}

pub fn task_two_driver_manifest() -> Vec<u8> {
    let source_archive = b"task-two-candidate-source-archive\n";
    let sbom = b"task-two-candidate-sbom\n";
    let mut manifest = serde_json::to_vec_pretty(&json!({
        "schema_version": 2,
        "driver_version": TASK_TWO_DRIVER_VERSION,
        "capabilities": [TASK_TWO_DRIVER_CAPABILITY],
        "source": {
            "repository": "https://github.com/shayne/hyperpixel2r-kms",
            "commit": TASK_TWO_DRIVER_COMMIT,
            "tree": TASK_TWO_DRIVER_TREE,
            "date_epoch": TASK_TWO_DRIVER_DATE_EPOCH,
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
            "source_date_epoch": TASK_TWO_DRIVER_DATE_EPOCH,
            "owner": 0,
            "group": 0,
            "mode_policy": "git-executable-or-regular",
        },
        "artifacts": [
            {
                "name": "hyperpixel2r-kms-source.tar.zst",
                "kind": "source-archive",
                "sha256": sha256(source_archive),
                "size": source_archive.len(),
            },
            {
                "name": "SBOM.spdx.json",
                "kind": "sbom",
                "sha256": sha256(sbom),
                "size": sbom.len(),
            },
        ],
    }))
    .expect("Task 2 driver manifest fixture");
    manifest.push(b'\n');
    manifest
}

pub fn task_two_driver_lock() -> DriverLock {
    let manifest = task_two_driver_manifest();
    DriverLock {
        repository: "https://github.com/shayne/hyperpixel2r-kms".into(),
        version: TASK_TWO_DRIVER_VERSION
            .parse()
            .expect("Task 2 driver version"),
        commit: TASK_TWO_DRIVER_COMMIT.into(),
        manifest_sha256: sha256(&manifest),
        required_capability: TASK_TWO_DRIVER_CAPABILITY.into(),
    }
}

pub fn fixture_payload(length: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_u32;
    let mut payload = (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect::<Vec<_>>();
    if payload.len() >= 20 {
        payload[..4].copy_from_slice(b"\x7fELF");
        payload[4] = 2;
        payload[5] = 1;
        payload[18] = 0xb7;
        payload[19] = 0;
    }
    payload
}

fn application_archive(payload: &[u8]) -> Vec<u8> {
    let mut compressed = Vec::new();
    {
        let encoder =
            zstd::stream::write::Encoder::new(&mut compressed, 3).expect("zstd fixture encoder");
        let mut archive = tar::Builder::new(encoder);
        archive.mode(tar::HeaderMode::Deterministic);
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(SOURCE_DATE_EPOCH);
        header.set_cksum();
        archive
            .append_data(&mut header, "planeradar", payload)
            .expect("application fixture member");
        let encoder = archive.into_inner().expect("finish fixture tar");
        encoder.finish().expect("finish fixture zstd");
    }
    compressed
}

fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}
