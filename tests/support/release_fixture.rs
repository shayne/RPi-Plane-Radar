use std::fs;
use std::path::{Path, PathBuf};

use planeradarctl::DriverLock;
use planeradarctl::state::ArtifactIdentity;
use serde_json::json;
use sha2::{Digest, Sha256};

pub const SOURCE_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const SOURCE_TREE: &str = "cccccccccccccccccccccccccccccccccccccccc";
pub const SOURCE_DATE_EPOCH: u64 = 1_785_153_600;

pub struct ReleaseFixture {
    pub directory: PathBuf,
    pub application: ArtifactIdentity,
    pub driver: ArtifactIdentity,
}

pub fn build_release(directory: &Path) -> ReleaseFixture {
    build_release_with_payload(directory, fixture_payload(4096))
}

pub fn build_release_with_payload(directory: &Path, payload: Vec<u8>) -> ReleaseFixture {
    fs::create_dir_all(directory).expect("release fixture directory");
    let lock = DriverLock::checked_in().expect("checked-in driver lock");
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
            source_commit: lock.commit,
            sha256: lock.manifest_sha256,
        },
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
