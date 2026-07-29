use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;
use std::time::SystemTime;

use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::DriverLock;
use crate::install::extract_application_payload_at_mtime;
use crate::operations::DoctorReport;
use crate::release::{ArtifactKind, MAX_ARTIFACT_SIZE, MAX_CONTROL_ARTIFACT_SIZE, ReleaseManifest};
use crate::state::ArtifactIdentity;

const MAX_CHECKSUM_BYTES: u64 = 16 * 1024;
const MAX_SUPPORT_ASSET_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SCREENSHOT_BYTES: u64 = 8 * 1024 * 1024;
const EXPECTED_RELEASE_FILES: [&str; 6] = [
    "SBOM.spdx.json",
    "install.sh",
    "planeradar-aarch64-linux-gnu.tar.zst",
    "planeradarctl-aarch64-apple-darwin.tar.zst",
    "planeradarctl-x86_64-apple-darwin.tar.zst",
    "release-manifest.json",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmokeVerification {
    pub application: ArtifactIdentity,
    pub driver: ArtifactIdentity,
    pub width: u32,
    pub height: u32,
    pub screenshot_sha256: String,
}

#[derive(Debug, Error)]
pub enum SmokeError {
    #[error("smoke input I/O failed")]
    Io(#[from] io::Error),
    #[error("local release metadata is invalid")]
    InvalidRelease,
    #[error("doctor output is invalid or unhealthy")]
    InvalidDoctor,
    #[error("the screenshot is stale or invalid")]
    InvalidScreenshot,
    #[error("installed identities do not match the local release")]
    IdentityMismatch,
}

pub fn verify_smoke_artifacts(
    release_dir: &Path,
    doctor_json: &Path,
    screenshot: &Path,
    captured_after: SystemTime,
) -> Result<SmokeVerification, SmokeError> {
    require_directory(release_dir)?;
    let checksums = parse_checksums(&release_dir.join("SHA256SUMS"))?;
    if checksums.len() != EXPECTED_RELEASE_FILES.len()
        || checksums
            .keys()
            .map(String::as_str)
            .ne(EXPECTED_RELEASE_FILES)
    {
        return Err(SmokeError::InvalidRelease);
    }
    let manifest_bytes =
        read_regular_bounded(&release_dir.join("release-manifest.json"), 64 * 1024)?;
    if checksums.get("release-manifest.json")
        != Some(&format!("{:x}", Sha256::digest(&manifest_bytes)))
    {
        return Err(SmokeError::InvalidRelease);
    }
    let value: Value =
        serde_json::from_slice(&manifest_bytes).map_err(|_| SmokeError::InvalidRelease)?;
    let version = value
        .get("version")
        .and_then(Value::as_str)
        .and_then(|value| Version::parse(value).ok())
        .ok_or(SmokeError::InvalidRelease)?;
    let lock = DriverLock::checked_in().map_err(|_| SmokeError::InvalidRelease)?;
    let manifest = ReleaseManifest::parse(&manifest_bytes, &version, &lock)
        .map_err(|_| SmokeError::InvalidRelease)?;
    for name in ["SBOM.spdx.json", "install.sh"] {
        let bytes = read_regular_bounded(&release_dir.join(name), MAX_SUPPORT_ASSET_BYTES)?;
        if checksums.get(name) != Some(&format!("{:x}", Sha256::digest(&bytes))) {
            return Err(SmokeError::InvalidRelease);
        }
    }
    for artifact in &manifest.artifacts {
        let maximum = match artifact.kind {
            ArtifactKind::Application => MAX_ARTIFACT_SIZE,
            ArtifactKind::Control => MAX_CONTROL_ARTIFACT_SIZE,
        };
        if artifact.size > maximum {
            return Err(SmokeError::InvalidRelease);
        }
        let bytes = read_regular_bounded(&release_dir.join(&artifact.name), maximum)?;
        if bytes.len() as u64 != artifact.size
            || format!("{:x}", Sha256::digest(&bytes)) != artifact.sha256
            || checksums.get(&artifact.name) != Some(&artifact.sha256)
        {
            return Err(SmokeError::InvalidRelease);
        }
    }
    let application_archive = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == ArtifactKind::Application)
        .ok_or(SmokeError::InvalidRelease)?;
    let private = tempfile::tempdir().map_err(SmokeError::Io)?;
    let payload = extract_application_payload_at_mtime(
        &release_dir.join(&application_archive.name),
        &application_archive.sha256,
        &private.path().join("payload"),
        manifest.source_date_epoch,
    )
    .map_err(|_| SmokeError::InvalidRelease)?;
    let application = ArtifactIdentity {
        version: manifest.version.to_string(),
        source_commit: manifest.source_commit.clone(),
        sha256: payload.sha256().into(),
    };
    let driver = ArtifactIdentity {
        version: manifest.driver.version.to_string(),
        source_commit: manifest.driver.commit.clone(),
        sha256: manifest.driver.manifest_sha256.clone(),
    };

    let doctor = DoctorReport::from_json(&read_regular_bounded(doctor_json, 32 * 1024)?)
        .map_err(|_| SmokeError::InvalidDoctor)?;
    if !doctor.healthy {
        return Err(SmokeError::InvalidDoctor);
    }
    let facts = &doctor.facts;
    if facts.installed_application != application
        || facts.expected_application != application
        || facts.running_application_revision != application.source_commit
        || facts.installed_driver != driver
        || facts.expected_driver != driver
        || facts.persisted_driver_manifest_sha256 != driver.sha256
    {
        return Err(SmokeError::IdentityMismatch);
    }

    let screenshot_metadata = fs::symlink_metadata(screenshot)?;
    if !screenshot_metadata.file_type().is_file()
        || screenshot_metadata.len() == 0
        || screenshot_metadata.len() > MAX_SCREENSHOT_BYTES
        || screenshot_metadata.modified()? < captured_after
    {
        return Err(SmokeError::InvalidScreenshot);
    }
    let screenshot_bytes = read_regular_bounded(screenshot, MAX_SCREENSHOT_BYTES)?;
    const PNG_IEND: &[u8; 12] = b"\0\0\0\0IEND\xae\x42\x60\x82";
    if !screenshot_bytes.ends_with(PNG_IEND) {
        return Err(SmokeError::InvalidScreenshot);
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(&screenshot_bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|_| SmokeError::InvalidScreenshot)?;
    let info = reader.info();
    if info.width != 480
        || info.height != 480
        || info.color_type != png::ColorType::Rgba
        || info.bit_depth != png::BitDepth::Eight
    {
        return Err(SmokeError::InvalidScreenshot);
    }
    let mut output = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or(SmokeError::InvalidScreenshot)?
    ];
    let frame = reader
        .next_frame(&mut output)
        .map_err(|_| SmokeError::InvalidScreenshot)?;
    if frame.width != 480
        || frame.height != 480
        || frame.color_type != png::ColorType::Rgba
        || frame.bit_depth != png::BitDepth::Eight
        || frame.buffer_size() != 480 * 480 * 4
    {
        return Err(SmokeError::InvalidScreenshot);
    }

    Ok(SmokeVerification {
        application,
        driver,
        width: frame.width,
        height: frame.height,
        screenshot_sha256: format!("{:x}", Sha256::digest(&screenshot_bytes)),
    })
}

fn require_directory(path: &Path) -> Result<(), SmokeError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(SmokeError::InvalidRelease)
    }
}

fn read_regular_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, SmokeError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > maximum {
        return Err(SmokeError::InvalidRelease);
    }
    let mut file = File::open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.len() != metadata.len() {
        return Err(SmokeError::InvalidRelease);
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != opened.len() || bytes.len() as u64 > maximum {
        return Err(SmokeError::InvalidRelease);
    }
    Ok(bytes)
}

fn parse_checksums(path: &Path) -> Result<BTreeMap<String, String>, SmokeError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > MAX_CHECKSUM_BYTES
    {
        return Err(SmokeError::InvalidRelease);
    }
    let reader = BufReader::new(File::open(path)?);
    let mut checksums = BTreeMap::new();
    for line in reader.lines() {
        let line = line?;
        let Some((digest, name)) = line.split_once("  ") else {
            return Err(SmokeError::InvalidRelease);
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !EXPECTED_RELEASE_FILES.contains(&name)
            || checksums.insert(name.into(), digest.into()).is_some()
        {
            return Err(SmokeError::InvalidRelease);
        }
    }
    Ok(checksums)
}
