use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

use semver::Version;
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::{
    config::{DRIVER_LIFECYCLE_PROTOCOL, DRIVER_REPOSITORY, DriverLock},
    transport::{CommandRunner, Invocation},
};

pub const APP_REPOSITORY: &str = "shayne/RPi-Plane-Radar";
pub const APP_REPOSITORY_URL: &str = "https://github.com/shayne/RPi-Plane-Radar";
pub const MANIFEST_NAME: &str = "release-manifest.json";
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_ARTIFACT_SIZE: u64 = 128 * 1024 * 1024;
pub const MAX_CONTROL_ARTIFACT_SIZE: u64 = 16 * 1024 * 1024;
pub const MAX_ARTIFACTS: usize = 64;
pub const MAX_ARTIFACT_NAME_BYTES: usize = 128;

const LATEST_STABLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const MAX_LATEST_STABLE_BYTES: usize = 1024;

pub struct GhLatestStableReleaseResolver<R> {
    runner: R,
}

impl<R> GhLatestStableReleaseResolver<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: CommandRunner> GhLatestStableReleaseResolver<R> {
    pub fn resolve(&self) -> Result<Version, ReleaseError> {
        let output = self
            .runner
            .run(
                Invocation::new(
                    "gh",
                    vec![
                        "release".into(),
                        "view".into(),
                        "-R".into(),
                        APP_REPOSITORY.into(),
                        "--json".into(),
                        "tagName,isDraft,isPrerelease".into(),
                    ],
                )
                .with_timeout(LATEST_STABLE_TIMEOUT)
                .with_stdout_limit(MAX_LATEST_STABLE_BYTES),
            )
            .map_err(|_| ReleaseError::LatestStableResolutionFailed)?;
        if output.status() != 0 || output.stdout().len() > MAX_LATEST_STABLE_BYTES {
            return Err(ReleaseError::LatestStableResolutionFailed);
        }
        let release: LatestStableRelease = serde_json::from_slice(output.stdout())
            .map_err(|_| ReleaseError::LatestStableResolutionFailed)?;
        if release.is_draft || release.is_prerelease {
            return Err(ReleaseError::LatestStableResolutionFailed);
        }
        let version_text = release
            .tag_name
            .strip_prefix('v')
            .ok_or(ReleaseError::LatestStableResolutionFailed)?;
        let version =
            Version::parse(version_text).map_err(|_| ReleaseError::LatestStableResolutionFailed)?;
        if !version.pre.is_empty() || format!("v{version}") != release.tag_name {
            return Err(ReleaseError::LatestStableResolutionFailed);
        }
        Ok(version)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LatestStableRelease {
    #[serde(rename = "tagName")]
    tag_name: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(rename = "isPrerelease")]
    is_prerelease: bool,
}

const SUPPORTED_MODEL: &str = "Raspberry Pi Zero 2 W";
const SUPPORTED_DISPLAY: &str = "HyperPixel 2.1 Round";
const SUPPORTED_OS: &str = "Raspberry Pi OS Lite Trixie (64-bit)";
const SUPPORTED_KERNEL_POLICY: &str = "driver-manifest-supported";
const CANDIDATE_RELEASE_WORKFLOW_PATH: &str = ".github/workflows/release.yml";
const STABLE_RELEASE_WORKFLOW_PATH: &str = ".github/workflows/stable-draft.yml";
const REQUIRED_TARGET_PACKAGES: &[&str] = &[
    "avahi-daemon",
    "ca-certificates",
    "device-tree-compiler",
    "dkms",
    "evtest",
    "kmod",
    "libegl1",
    "libgl1-mesa-dri",
    "libgles2",
    "libsdl2-2.0-0",
    "pngcheck",
];
const MAX_STREAM_STDERR_BYTES: usize = 64 * 1024;

/// The driver identity in a release manifest is the same checked-in lock type,
/// not a second independently parsed lock format.
pub type LockedDriver = DriverLock;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    Aarch64,
    X86_64,
    Any,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    Application,
    Control,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    LinuxGnu,
    AppleDarwin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupportedTarget {
    pub model: String,
    pub display: String,
    pub operating_system: String,
    pub architecture: Architecture,
    pub kernel_policy: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Artifact {
    pub name: String,
    pub kind: ArtifactKind,
    pub platform: Platform,
    pub architecture: Architecture,
    pub size: u64,
    pub sha256: String,
    pub runnable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowIdentity {
    pub repository: String,
    pub path: String,
    pub source_ref: String,
    pub commit: String,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub version: Version,
    pub source_commit: String,
    pub source_tree: String,
    pub source_timestamp: String,
    pub source_date_epoch: u64,
    pub repository: String,
    pub workflow: WorkflowIdentity,
    pub supported: SupportedTarget,
    pub required_target_packages: Vec<String>,
    pub minimum_control_version: Version,
    pub driver: LockedDriver,
    pub artifacts: Vec<Artifact>,
}

impl fmt::Debug for ReleaseManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReleaseManifest")
            .field("schema_version", &self.schema_version)
            .field("version", &self.version)
            .field("source_commit", &"<redacted>")
            .field("supported", &self.supported)
            .field("driver", &"<redacted locked driver>")
            .field("artifact_count", &self.artifacts.len())
            .finish()
    }
}

impl ReleaseManifest {
    pub fn parse(
        contents: &[u8],
        requested_version: &Version,
        driver_lock: &DriverLock,
    ) -> Result<Self, ReleaseError> {
        if contents.len() > MAX_MANIFEST_BYTES {
            return Err(ReleaseError::ManifestTooLarge);
        }
        let value = parse_unique_json(contents)?;
        let raw: RawReleaseManifest =
            serde_json::from_value(value).map_err(|_| ReleaseError::InvalidManifest)?;

        if raw.schema_version != 1 {
            return Err(ReleaseError::UnsupportedSchema);
        }
        let version = parse_canonical_version(&raw.version)?;
        if raw.version != requested_version.to_string() || &version != requested_version {
            return Err(ReleaseError::VersionMismatch);
        }
        let expected_workflow_path = if version.pre.is_empty() {
            STABLE_RELEASE_WORKFLOW_PATH
        } else {
            CANDIDATE_RELEASE_WORKFLOW_PATH
        };
        if !is_lower_hex(&raw.source_commit, 40) {
            return Err(ReleaseError::InvalidManifest);
        }
        if !is_lower_hex(&raw.source_tree, 40)
            || !is_utc_timestamp(&raw.source_timestamp)
            || raw.source_date_epoch == 0
            || raw.repository != APP_REPOSITORY_URL
            || raw.workflow.repository != APP_REPOSITORY
            || raw.workflow.path != expected_workflow_path
            || !is_safe_github_ref(&raw.workflow.source_ref)
            || raw.workflow.commit != raw.source_commit
        {
            return Err(ReleaseError::InvalidManifest);
        }
        let minimum_control_version = parse_canonical_version(&raw.minimum_control_version)?;
        if minimum_control_version
            > Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version")
            || raw.required_target_packages
                != REQUIRED_TARGET_PACKAGES
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>()
        {
            return Err(ReleaseError::InvalidManifest);
        }
        if raw.supported.model != SUPPORTED_MODEL
            || raw.supported.display != SUPPORTED_DISPLAY
            || raw.supported.operating_system != SUPPORTED_OS
            || raw.supported.architecture != Architecture::Aarch64
            || raw.supported.kernel_policy != SUPPORTED_KERNEL_POLICY
        {
            return Err(ReleaseError::UnsupportedTarget);
        }

        driver_lock
            .validate()
            .map_err(|_| ReleaseError::DriverLockMismatch)?;
        let driver_version = parse_canonical_version(&raw.driver.version)?;
        if raw.driver.repository != DRIVER_REPOSITORY
            || !is_lower_hex(&raw.driver.commit, 40)
            || !is_lower_hex(&raw.driver.manifest_sha256, 64)
            || raw.driver.repository != driver_lock.repository
            || driver_version != driver_lock.version
            || raw.driver.version != driver_lock.version.to_string()
            || raw.driver.commit != driver_lock.commit
            || raw.driver.manifest_sha256 != driver_lock.manifest_sha256
            || raw.driver.lifecycle_protocol != DRIVER_LIFECYCLE_PROTOCOL
        {
            return Err(ReleaseError::DriverLockMismatch);
        }

        if raw.artifacts.is_empty() || raw.artifacts.len() > MAX_ARTIFACTS {
            return Err(ReleaseError::InvalidArtifactSet);
        }
        let mut artifacts = Vec::with_capacity(raw.artifacts.len());
        for (name, artifact) in raw.artifacts {
            if !is_safe_artifact_name(&name)
                || artifact.size == 0
                || artifact.size > MAX_ARTIFACT_SIZE
                || (artifact.kind == ArtifactKind::Control
                    && artifact.size > MAX_CONTROL_ARTIFACT_SIZE)
                || !is_lower_hex(&artifact.sha256, 64)
            {
                return Err(ReleaseError::InvalidArtifactSet);
            }
            artifacts.push(Artifact {
                name,
                kind: artifact.kind,
                platform: artifact.platform,
                architecture: artifact.architecture,
                size: artifact.size,
                sha256: artifact.sha256,
                runnable: artifact.runnable,
            });
        }
        let expected = [
            (
                "planeradar-aarch64-linux-gnu.tar.zst",
                ArtifactKind::Application,
                Platform::LinuxGnu,
                Architecture::Aarch64,
            ),
            (
                "planeradarctl-aarch64-apple-darwin.tar.zst",
                ArtifactKind::Control,
                Platform::AppleDarwin,
                Architecture::Aarch64,
            ),
            (
                "planeradarctl-x86_64-apple-darwin.tar.zst",
                ArtifactKind::Control,
                Platform::AppleDarwin,
                Architecture::X86_64,
            ),
        ];
        if artifacts.len() != expected.len()
            || expected.iter().any(|(name, kind, platform, architecture)| {
                !artifacts.iter().any(|artifact| {
                    artifact.name == *name
                        && artifact.kind == *kind
                        && artifact.platform == *platform
                        && artifact.architecture == *architecture
                        && artifact.runnable
                })
            })
        {
            return Err(ReleaseError::InvalidArtifactSet);
        }

        Ok(Self {
            schema_version: 1,
            version,
            source_commit: raw.source_commit,
            source_tree: raw.source_tree,
            source_timestamp: raw.source_timestamp,
            source_date_epoch: raw.source_date_epoch,
            repository: raw.repository,
            workflow: WorkflowIdentity {
                repository: raw.workflow.repository,
                path: raw.workflow.path,
                source_ref: raw.workflow.source_ref,
                commit: raw.workflow.commit,
            },
            supported: SupportedTarget {
                model: raw.supported.model,
                display: raw.supported.display,
                operating_system: raw.supported.operating_system,
                architecture: raw.supported.architecture,
                kernel_policy: raw.supported.kernel_policy,
            },
            required_target_packages: raw.required_target_packages,
            minimum_control_version,
            driver: driver_lock.clone(),
            artifacts,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReleaseManifest {
    schema_version: u32,
    version: String,
    source_commit: String,
    source_tree: String,
    source_timestamp: String,
    source_date_epoch: u64,
    repository: String,
    workflow: RawWorkflowIdentity,
    supported: RawSupportedTarget,
    required_target_packages: Vec<String>,
    minimum_control_version: String,
    driver: RawLockedDriver,
    artifacts: BTreeMap<String, RawArtifact>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkflowIdentity {
    repository: String,
    path: String,
    #[serde(rename = "ref")]
    source_ref: String,
    commit: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSupportedTarget {
    model: String,
    display: String,
    operating_system: String,
    architecture: Architecture,
    kernel_policy: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLockedDriver {
    repository: String,
    version: String,
    commit: String,
    manifest_sha256: String,
    lifecycle_protocol: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArtifact {
    kind: ArtifactKind,
    platform: Platform,
    architecture: Architecture,
    size: u64,
    sha256: String,
    runnable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadRequest {
    repository: &'static str,
    tag: String,
    name: String,
}

impl DownloadRequest {
    pub fn new(version: &Version, name: impl Into<String>) -> Result<Self, ReleaseError> {
        let name = name.into();
        if !is_safe_artifact_name(&name) {
            return Err(ReleaseError::InvalidArtifactName);
        }
        Ok(Self {
            repository: APP_REPOSITORY,
            tag: format!("v{version}"),
            name,
        })
    }

    pub fn repository(&self) -> &str {
        self.repository
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn invocation(&self) -> Invocation {
        Invocation::new(
            "gh",
            vec![
                "release".into(),
                "download".into(),
                self.tag.clone(),
                "--pattern".into(),
                self.name.clone(),
                "--output".into(),
                "-".into(),
                "-R".into(),
                self.repository.into(),
            ],
        )
    }
}

pub trait ReleaseSource {
    fn stream(
        &self,
        request: &DownloadRequest,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError>;
}

pub trait StreamingCommandRunner {
    fn run_streaming(
        &self,
        invocation: Invocation,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemStreamingCommandRunner;

impl StreamingCommandRunner for SystemStreamingCommandRunner {
    fn run_streaming(
        &self,
        invocation: Invocation,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError> {
        let mut child = Command::new(invocation.program())
            .args(invocation.os_arguments())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| ReleaseSourceError::Failed)?;
        let mut stdout = child.stdout.take().ok_or(ReleaseSourceError::Failed)?;
        let stderr = child.stderr.take().ok_or(ReleaseSourceError::Failed)?;
        let stderr_reader = thread::spawn(move || drain_bounded(stderr, MAX_STREAM_STDERR_BYTES));

        let stream_result = io::copy(&mut stdout, sink);
        if stream_result.is_err() {
            let _ = child.kill();
        }
        let status = child.wait().map_err(|_| ReleaseSourceError::Failed)?;
        let stderr_ok = stderr_reader
            .join()
            .map_err(|_| ReleaseSourceError::Failed)??;
        if stream_result.is_err() || !status.success() || !stderr_ok {
            return Err(ReleaseSourceError::Failed);
        }
        Ok(())
    }
}

pub struct GhReleaseSource<R = SystemStreamingCommandRunner> {
    runner: R,
}

impl GhReleaseSource<SystemStreamingCommandRunner> {
    pub fn system() -> Self {
        Self::new(SystemStreamingCommandRunner)
    }
}

impl<R> GhReleaseSource<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }
}

impl<R> fmt::Debug for GhReleaseSource<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GhReleaseSource")
            .field("runner", &"<redacted>")
            .finish()
    }
}

impl<R: StreamingCommandRunner> ReleaseSource for GhReleaseSource<R> {
    fn stream(
        &self,
        request: &DownloadRequest,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError> {
        self.runner.run_streaming(request.invocation(), sink)
    }
}

#[derive(Clone, Copy)]
pub enum ReleaseInput<'a> {
    Local(&'a Path),
    Downloaded,
}

impl fmt::Debug for ReleaseInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(_) => formatter.debug_tuple("Local").field(&"<redacted>").finish(),
            Self::Downloaded => formatter.write_str("Downloaded"),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ResolvedArtifact {
    pub artifact: Artifact,
    pub path: PathBuf,
}

impl fmt::Debug for ResolvedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedArtifact")
            .field("name", &self.artifact.name)
            .field("runnable", &self.artifact.runnable)
            .field("path", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ResolvedRelease {
    pub tag: String,
    pub manifest: ReleaseManifest,
    pub artifacts: Vec<ResolvedArtifact>,
}

impl fmt::Debug for ResolvedRelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedRelease")
            .field("tag", &self.tag)
            .field("manifest", &self.manifest)
            .field("artifacts", &self.artifacts)
            .finish()
    }
}

pub struct ReleaseClient<S> {
    source: S,
    cache_root: PathBuf,
}

impl<S> ReleaseClient<S> {
    pub fn new(source: S, cache_root: PathBuf) -> Self {
        Self { source, cache_root }
    }
}

impl<S> fmt::Debug for ReleaseClient<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReleaseClient")
            .field("source", &"<redacted>")
            .field("cache_root", &"<redacted>")
            .finish()
    }
}

impl<S: ReleaseSource> ReleaseClient<S> {
    pub fn resolve(
        &self,
        requested_version: &Version,
        driver_lock: &DriverLock,
        input: ReleaseInput<'_>,
    ) -> Result<ResolvedRelease, ReleaseError> {
        if !self.cache_root.is_absolute() {
            return Err(ReleaseError::UnsafeCache);
        }
        let manifest_bytes = match input {
            ReleaseInput::Local(directory) => {
                validate_local_directory(directory)?;
                read_local_manifest(directory)?
            }
            ReleaseInput::Downloaded => {
                let request = DownloadRequest::new(requested_version, MANIFEST_NAME)?;
                let mut sink = BoundedMemory::new(MAX_MANIFEST_BYTES);
                let result = self.source.stream(&request, &mut sink);
                if sink.exceeded {
                    return Err(ReleaseError::ManifestTooLarge);
                }
                result.map_err(ReleaseError::Source)?;
                sink.bytes
            }
        };
        let manifest = ReleaseManifest::parse(&manifest_bytes, requested_version, driver_lock)?;
        ensure_private_directory(&self.cache_root)?;
        let artifacts_root = self.cache_root.join("artifacts");
        ensure_private_directory(&artifacts_root)?;

        let mut resolved_artifacts = Vec::with_capacity(manifest.artifacts.len());
        for artifact in &manifest.artifacts {
            let digest_directory = artifacts_root.join(&artifact.sha256);
            ensure_private_directory(&digest_directory)?;
            let destination = digest_directory.join(&artifact.name);
            if validate_cache_hit(&destination, artifact)? {
                resolved_artifacts.push(ResolvedArtifact {
                    artifact: artifact.clone(),
                    path: destination,
                });
                continue;
            }

            let mut temporary =
                NamedTempFile::new_in(&digest_directory).map_err(ReleaseError::Io)?;
            set_private_file_mode(temporary.as_file())?;
            let mut sink = ArtifactWriter::new(temporary.as_file_mut(), artifact.size);
            let stream_result = match input {
                ReleaseInput::Local(directory) => {
                    let mut source = open_local_regular_file(
                        &directory.join(&artifact.name),
                        Some(artifact.size),
                    )?;
                    io::copy(&mut source, &mut sink)
                        .map(|_| ())
                        .map_err(|_| ReleaseSourceError::Failed)
                }
                ReleaseInput::Downloaded => {
                    let request = DownloadRequest::new(requested_version, artifact.name.clone())?;
                    self.source.stream(&request, &mut sink)
                }
            };
            if sink.exceeded {
                return Err(ReleaseError::ArtifactTooLarge);
            }
            stream_result.map_err(ReleaseError::Source)?;
            let (written, digest) = sink.finish();
            if written != artifact.size {
                return Err(ReleaseError::ArtifactSizeMismatch);
            }
            if digest != artifact.sha256 {
                return Err(ReleaseError::ArtifactDigestMismatch);
            }
            temporary.as_file_mut().flush().map_err(ReleaseError::Io)?;
            temporary.as_file().sync_all().map_err(ReleaseError::Io)?;

            match temporary.persist_noclobber(&destination) {
                Ok(file) => {
                    file.sync_all().map_err(ReleaseError::Io)?;
                    sync_directory(&digest_directory)?;
                }
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                    drop(error.file);
                    if !validate_cache_hit(&destination, artifact)? {
                        return Err(ReleaseError::UnsafeCache);
                    }
                }
                Err(_) => return Err(ReleaseError::Io(io::Error::other("cache persist failed"))),
            }
            if !validate_cache_hit(&destination, artifact)? {
                return Err(ReleaseError::UnsafeCache);
            }
            resolved_artifacts.push(ResolvedArtifact {
                artifact: artifact.clone(),
                path: destination,
            });
        }

        Ok(ResolvedRelease {
            tag: format!("v{requested_version}"),
            manifest,
            artifacts: resolved_artifacts,
        })
    }
}

pub struct Verifier<R> {
    runner: R,
}

impl<R> Verifier<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }
}

impl<R> fmt::Debug for Verifier<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Verifier")
            .field("runner", &"<redacted>")
            .finish()
    }
}

impl<R: CommandRunner> Verifier<R> {
    pub fn verify(
        &self,
        requested_version: &Version,
        release: &ResolvedRelease,
    ) -> Result<(), ReleaseError> {
        let expected_tag = format!("v{requested_version}");
        if &release.manifest.version != requested_version
            || release.tag != expected_tag
            || release.artifacts.len() != release.manifest.artifacts.len()
            || release
                .artifacts
                .iter()
                .zip(&release.manifest.artifacts)
                .any(|(resolved, declared)| &resolved.artifact != declared)
        {
            return Err(ReleaseError::VersionMismatch);
        }
        revalidate_all_artifacts(release)?;
        if !requested_version.pre.is_empty() {
            return Ok(());
        }

        require_command_success(
            &self.runner,
            Invocation::new(
                "gh",
                vec![
                    "release".into(),
                    "verify".into(),
                    expected_tag,
                    "-R".into(),
                    APP_REPOSITORY.into(),
                ],
            ),
        )?;
        for artifact in release
            .artifacts
            .iter()
            .filter(|artifact| artifact.artifact.runnable)
        {
            revalidate_resolved_artifact(artifact)?;
            require_command_success(
                &self.runner,
                Invocation::new_os(
                    "gh",
                    vec![
                        "attestation".into(),
                        "verify".into(),
                        artifact.path.clone().into_os_string(),
                        "-R".into(),
                        APP_REPOSITORY.into(),
                        "--signer-workflow".into(),
                        format!("{APP_REPOSITORY}/{}", release.manifest.workflow.path).into(),
                        "--source-ref".into(),
                        release.manifest.workflow.source_ref.clone().into(),
                        "--source-digest".into(),
                        release.manifest.workflow.commit.clone().into(),
                        "--deny-self-hosted-runners".into(),
                    ],
                ),
            )?;
            revalidate_resolved_artifact(artifact)?;
        }
        revalidate_all_artifacts(release)?;
        Ok(())
    }
}

fn revalidate_all_artifacts(release: &ResolvedRelease) -> Result<(), ReleaseError> {
    for artifact in &release.artifacts {
        revalidate_resolved_artifact(artifact)?;
    }
    Ok(())
}

fn revalidate_resolved_artifact(artifact: &ResolvedArtifact) -> Result<(), ReleaseError> {
    if validate_cache_hit(&artifact.path, &artifact.artifact)? {
        Ok(())
    } else {
        Err(ReleaseError::UnsafeCache)
    }
}

fn require_command_success<R: CommandRunner>(
    runner: &R,
    invocation: Invocation,
) -> Result<(), ReleaseError> {
    let output = runner
        .run(invocation)
        .map_err(|_| ReleaseError::VerificationFailed)?;
    if output.status() != 0 {
        return Err(ReleaseError::VerificationFailed);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ReleaseSourceError {
    #[error("release stream failed")]
    Failed,
}

#[derive(Debug, Error)]
pub enum ReleaseError {
    #[error("release manifest exceeds its size limit")]
    ManifestTooLarge,
    #[error("release manifest is invalid")]
    InvalidManifest,
    #[error("release manifest schema is unsupported")]
    UnsupportedSchema,
    #[error("release version identity does not match")]
    VersionMismatch,
    #[error("release target is unsupported")]
    UnsupportedTarget,
    #[error("release driver identity does not match the checked-in lock")]
    DriverLockMismatch,
    #[error("release artifact set is invalid")]
    InvalidArtifactSet,
    #[error("release artifact name is unsafe")]
    InvalidArtifactName,
    #[error("release path is unsafe")]
    UnsafePath,
    #[error("release cache is unsafe")]
    UnsafeCache,
    #[error("release artifact exceeded its declared size")]
    ArtifactTooLarge,
    #[error("release artifact size does not match")]
    ArtifactSizeMismatch,
    #[error("release artifact digest does not match")]
    ArtifactDigestMismatch,
    #[error("release verification failed")]
    VerificationFailed,
    #[error("latest stable release resolution failed")]
    LatestStableResolutionFailed,
    #[error("release source failed")]
    Source(#[source] ReleaseSourceError),
    #[error("release filesystem operation failed")]
    Io(#[source] io::Error),
}

fn parse_canonical_version(value: &str) -> Result<Version, ReleaseError> {
    let version = Version::parse(value).map_err(|_| ReleaseError::InvalidManifest)?;
    if version.to_string() != value {
        return Err(ReleaseError::InvalidManifest);
    }
    Ok(version)
}

fn is_lower_hex(value: &str, expected_length: usize) -> bool {
    value.len() == expected_length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_utc_timestamp(value: &str) -> bool {
    value.len() == 20
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value.as_bytes()[10] == b'T'
        && value.as_bytes()[13] == b':'
        && value.as_bytes()[16] == b':'
        && value.as_bytes()[19] == b'Z'
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        })
}

fn is_safe_github_ref(value: &str) -> bool {
    let Some(suffix) = value
        .strip_prefix("refs/heads/")
        .or_else(|| value.strip_prefix("refs/tags/"))
    else {
        return false;
    };
    !suffix.is_empty()
        && suffix.len() <= 192
        && !suffix.contains("..")
        && !suffix.contains("//")
        && !suffix.contains("@{")
        && !suffix.ends_with('/')
        && !suffix.ends_with('.')
        && !suffix.ends_with(".lock")
        && suffix.bytes().enumerate().all(|(index, byte)| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => true,
            b'.' | b'_' | b'-' | b'/' => index != 0,
            _ => false,
        })
}

fn is_safe_artifact_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ARTIFACT_NAME_BYTES
        && name != "."
        && name != ".."
        && !name.contains("..")
        && name.bytes().enumerate().all(|(index, byte)| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => true,
            b'.' | b'_' | b'-' => index != 0,
            _ => false,
        })
}

fn parse_unique_json(contents: &[u8]) -> Result<Value, ReleaseError> {
    let mut deserializer = serde_json::Deserializer::from_slice(contents);
    let value =
        UniqueJson::deserialize(&mut deserializer).map_err(|_| ReleaseError::InvalidManifest)?;
    deserializer
        .end()
        .map_err(|_| ReleaseError::InvalidManifest)?;
    Ok(value.0)
}

struct UniqueJson(Value);

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

struct UniqueJsonVisitor;

impl<'de> Visitor<'de> for UniqueJsonVisitor {
    type Value = UniqueJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueJson(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueJson(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueJson(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(UniqueJson)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueJson(Value::String(value.into())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueJson(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<UniqueJson>()? {
            values.push(value.0);
        }
        Ok(UniqueJson(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            values.insert(key, object.next_value::<UniqueJson>()?.0);
        }
        Ok(UniqueJson(Value::Object(values)))
    }
}

struct BoundedMemory {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedMemory {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedMemory {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("bounded buffer exceeded"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ArtifactWriter<'a> {
    file: &'a mut File,
    expected: u64,
    written: u64,
    hasher: Sha256,
    exceeded: bool,
}

impl<'a> ArtifactWriter<'a> {
    fn new(file: &'a mut File, expected: u64) -> Self {
        Self {
            file,
            expected,
            written: 0,
            hasher: Sha256::new(),
            exceeded: false,
        }
    }

    fn finish(self) -> (u64, String) {
        (self.written, format!("{:x}", self.hasher.finalize()))
    }
}

impl Write for ArtifactWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() as u64 > self.expected.saturating_sub(self.written) {
            self.exceeded = true;
            return Err(io::Error::other("declared artifact size exceeded"));
        }
        let written = self.file.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn validate_local_directory(path: &Path) -> Result<(), ReleaseError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ReleaseError::UnsafePath)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(ReleaseError::UnsafePath);
    }
    Ok(())
}

fn read_local_manifest(directory: &Path) -> Result<Vec<u8>, ReleaseError> {
    let mut file = open_local_regular_file(&directory.join(MANIFEST_NAME), None)?;
    let mut contents = Vec::new();
    Read::by_ref(&mut file)
        .take((MAX_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut contents)
        .map_err(ReleaseError::Io)?;
    if contents.len() > MAX_MANIFEST_BYTES {
        return Err(ReleaseError::ManifestTooLarge);
    }
    Ok(contents)
}

fn open_local_regular_file(path: &Path, expected_size: Option<u64>) -> Result<File, ReleaseError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ReleaseError::UnsafePath)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(ReleaseError::UnsafePath);
    }
    if expected_size.is_some_and(|size| metadata.len() != size) {
        return Err(ReleaseError::ArtifactSizeMismatch);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path).map_err(|_| ReleaseError::UnsafePath)?;
    let opened = file.metadata().map_err(ReleaseError::Io)?;
    if !opened.is_file() || expected_size.is_some_and(|size| opened.len() != size) {
        return Err(ReleaseError::ArtifactSizeMismatch);
    }
    Ok(file)
}

fn ensure_private_directory(path: &Path) -> Result<(), ReleaseError> {
    match create_private_directory(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(ReleaseError::Io(error)),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| ReleaseError::UnsafeCache)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(ReleaseError::UnsafeCache);
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(ReleaseError::UnsafeCache);
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

fn set_private_file_mode(file: &File) -> Result<(), ReleaseError> {
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(ReleaseError::Io)?;
    Ok(())
}

fn validate_cache_hit(path: &Path, artifact: &Artifact) -> Result<bool, ReleaseError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(ReleaseError::UnsafeCache),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(ReleaseError::UnsafeCache);
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o777 != 0o600 || metadata.nlink() != 1 {
        return Err(ReleaseError::UnsafeCache);
    }
    if metadata.len() != artifact.size {
        return Err(ReleaseError::ArtifactSizeMismatch);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path).map_err(|_| ReleaseError::UnsafeCache)?;
    let opened = file.metadata().map_err(ReleaseError::Io)?;
    if !opened.is_file() || opened.len() != artifact.size {
        return Err(ReleaseError::UnsafeCache);
    }
    #[cfg(unix)]
    if opened.permissions().mode() & 0o777 != 0o600 || opened.nlink() != 1 {
        return Err(ReleaseError::UnsafeCache);
    }
    let digest = hash_reader(&mut file)?;
    if digest != artifact.sha256 {
        return Err(ReleaseError::ArtifactDigestMismatch);
    }
    Ok(true)
}

fn hash_reader(reader: &mut impl Read) -> Result<String, ReleaseError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(ReleaseError::Io)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sync_directory(path: &Path) -> Result<(), ReleaseError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(ReleaseError::Io)
}

fn drain_bounded(mut reader: impl Read, limit: usize) -> Result<bool, ReleaseSourceError> {
    let mut buffer = [0_u8; 8 * 1024];
    let mut total = 0_usize;
    let mut exceeded = false;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| ReleaseSourceError::Failed)?;
        if count == 0 {
            return Ok(!exceeded);
        }
        total = total.saturating_add(count);
        exceeded |= total > limit;
    }
}
