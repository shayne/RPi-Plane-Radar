use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::{
    config::{DRIVER_REPOSITORY, DriverLock},
    release::{ReleaseSourceError, StreamingCommandRunner, SystemStreamingCommandRunner},
    transport::{CommandRunner, Invocation, SystemCommandRunner},
};

const DRIVER_MANIFEST_NAME: &str = "driver-manifest.json";
const DRIVER_SOURCE_NAME: &str = "hyperpixel2r-kms-source.tar.zst";
const DRIVER_SBOM_NAME: &str = "SBOM.spdx.json";
const MAX_DRIVER_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_DRIVER_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const DRIVER_GITHUB_REPOSITORY: &str = "shayne/hyperpixel2r-kms";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetProbe {
    kernel_release: String,
}

impl TargetProbe {
    pub fn new(kernel_release: impl Into<String>) -> Result<Self, DriverError> {
        let kernel_release = kernel_release.into();
        validate_kernel_release(&kernel_release)?;
        Ok(Self { kernel_release })
    }

    pub fn kernel_release(&self) -> &str {
        &self.kernel_release
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct PrebuiltBundle {
    path: PathBuf,
    kernel_release: String,
}

impl fmt::Debug for PrebuiltBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrebuiltBundle")
            .field("path", &"<redacted>")
            .field("kernel_release", &self.kernel_release)
            .finish()
    }
}

impl PrebuiltBundle {
    pub fn verified(
        path: PathBuf,
        kernel_release: impl Into<String>,
        vermagic: &str,
        internal_manifest_digest: &str,
        expected_manifest_digest: &str,
    ) -> Result<Self, DriverError> {
        let kernel_release = kernel_release.into();
        validate_kernel_release(&kernel_release)?;
        let vermagic_release = vermagic
            .split_whitespace()
            .next()
            .ok_or(DriverError::InvalidPrebuiltIdentity)?;
        if vermagic_release != kernel_release
            || internal_manifest_digest != expected_manifest_digest
            || !is_lower_hex(expected_manifest_digest, 64)
        {
            return Err(DriverError::InvalidPrebuiltIdentity);
        }
        Ok(Self {
            path,
            kernel_release,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DriverPlan {
    Prebuilt { archive: PathBuf },
    CrossBuild { source: PathBuf },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriverResolver {
    source: PathBuf,
    prebuilt: Vec<PrebuiltBundle>,
}

impl DriverResolver {
    pub fn new(source: PathBuf, prebuilt: Vec<PrebuiltBundle>) -> Result<Self, DriverError> {
        if source.as_os_str().is_empty() {
            return Err(DriverError::InvalidPath);
        }
        let mut releases = prebuilt
            .iter()
            .map(|bundle| bundle.kernel_release.as_str())
            .collect::<Vec<_>>();
        releases.sort_unstable();
        if releases.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(DriverError::DuplicatePrebuilt);
        }
        Ok(Self { source, prebuilt })
    }

    pub fn resolve(&self, probe: &TargetProbe) -> Result<DriverPlan, DriverError> {
        if let Some(bundle) = self
            .prebuilt
            .iter()
            .find(|bundle| bundle.kernel_release == probe.kernel_release)
        {
            return Ok(DriverPlan::Prebuilt {
                archive: bundle.path.clone(),
            });
        }
        Ok(DriverPlan::CrossBuild {
            source: self.source.clone(),
        })
    }
}

pub trait DriverReleaseSource {
    fn stream(
        &self,
        version: &Version,
        name: &str,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError>;
}

pub trait DriverReleaseVerifier {
    fn verify(&self, version: &Version, assets: &[PathBuf]) -> Result<(), io::Error>;
}

pub struct GhDriverReleaseSource<R = SystemStreamingCommandRunner> {
    runner: R,
}

impl GhDriverReleaseSource<SystemStreamingCommandRunner> {
    pub fn system() -> Self {
        Self::new(SystemStreamingCommandRunner)
    }
}

impl<R> GhDriverReleaseSource<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: StreamingCommandRunner> DriverReleaseSource for GhDriverReleaseSource<R> {
    fn stream(
        &self,
        version: &Version,
        name: &str,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError> {
        if !safe_name(name) {
            return Err(ReleaseSourceError::Failed);
        }
        self.runner.run_streaming(
            Invocation::new(
                "gh",
                vec![
                    "release".into(),
                    "download".into(),
                    format!("v{version}"),
                    "--pattern".into(),
                    name.into(),
                    "--output".into(),
                    "-".into(),
                    "-R".into(),
                    DRIVER_GITHUB_REPOSITORY.into(),
                ],
            ),
            sink,
        )
    }
}

pub struct GhDriverReleaseVerifier<R = SystemCommandRunner> {
    runner: R,
}

impl GhDriverReleaseVerifier<SystemCommandRunner> {
    pub fn system() -> Self {
        Self::new(SystemCommandRunner)
    }
}

impl<R> GhDriverReleaseVerifier<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: CommandRunner> DriverReleaseVerifier for GhDriverReleaseVerifier<R> {
    fn verify(&self, _version: &Version, assets: &[PathBuf]) -> Result<(), io::Error> {
        for asset in assets {
            if !asset.is_absolute() {
                return Err(io::Error::other("driver asset path is not absolute"));
            }
            require_success(
                &self.runner,
                Invocation::new_os(
                    "gh",
                    vec![
                        "attestation".into(),
                        "verify".into(),
                        asset.clone().into_os_string(),
                        "-R".into(),
                        DRIVER_GITHUB_REPOSITORY.into(),
                    ],
                ),
            )?;
        }
        Ok(())
    }
}

fn require_success<R: CommandRunner>(runner: &R, invocation: Invocation) -> Result<(), io::Error> {
    let output = runner
        .run(invocation)
        .map_err(|_| io::Error::other("driver release verification command failed"))?;
    if output.status() != 0 {
        return Err(io::Error::other(
            "driver release verification command failed",
        ));
    }
    Ok(())
}

pub struct DriverManager<S, V> {
    source: S,
    verifier: V,
    cache_root: PathBuf,
}

impl<S, V> DriverManager<S, V> {
    pub fn new(source: S, verifier: V, cache_root: PathBuf) -> Self {
        Self {
            source,
            verifier,
            cache_root,
        }
    }
}

impl<S: DriverReleaseSource, V: DriverReleaseVerifier> DriverManager<S, V> {
    pub fn sync(&self, lock: &DriverLock) -> Result<SyncedDriver, DriverError> {
        lock.validate().map_err(|_| DriverError::InvalidLock)?;
        let acquired = self.acquire(&lock.version)?;
        if acquired.lock != *lock {
            return Err(DriverError::InvalidLock);
        }
        self.materialize(acquired)
    }

    pub fn update(&self, lock_path: &Path, version: &Version) -> Result<DriverLock, DriverError> {
        if lock_path.as_os_str().is_empty() {
            return Err(DriverError::InvalidLock);
        }
        let acquired = self.acquire(version)?;
        let synced = self.materialize(acquired)?;
        let lock = synced.lock.clone();
        atomic_write_lock(lock_path, &lock)?;
        Ok(lock)
    }

    fn acquire(&self, version: &Version) -> Result<AcquiredDriver, DriverError> {
        if !self.cache_root.is_absolute() {
            return Err(DriverError::InvalidPath);
        }
        ensure_directory(&self.cache_root)?;
        let manifest_bytes = self.download_manifest(version)?;
        let manifest_digest = sha256_bytes(&manifest_bytes);
        let manifest = DriverManifest::parse(&manifest_bytes, version)?;
        let lock = DriverLock {
            repository: DRIVER_REPOSITORY.into(),
            version: version.clone(),
            commit: manifest.source.commit.clone(),
            manifest_sha256: manifest_digest.clone(),
        };
        lock.validate().map_err(|_| DriverError::InvalidLock)?;

        let manifest_path = cache_bytes(
            &self.cache_root,
            DRIVER_MANIFEST_NAME,
            &manifest_digest,
            &manifest_bytes,
        )?;
        let mut assets = vec![manifest_path];
        let mut downloaded = BTreeMap::new();
        for artifact in &manifest.artifacts {
            let path = self.download_artifact(version, artifact)?;
            assets.push(path.clone());
            downloaded.insert(artifact.name().to_owned(), path);
        }
        self.verifier
            .verify(version, &assets)
            .map_err(|_| DriverError::VerificationFailed)?;
        for artifact in &manifest.artifacts {
            let path = downloaded
                .get(artifact.name())
                .ok_or(DriverError::InvalidManifest)?;
            validate_file(path, artifact.size(), artifact.sha256())?;
        }
        Ok(AcquiredDriver {
            lock,
            manifest,
            downloaded,
        })
    }

    fn download_manifest(&self, version: &Version) -> Result<Vec<u8>, DriverError> {
        let mut sink = LimitedVec::new(MAX_DRIVER_MANIFEST_BYTES);
        self.source
            .stream(version, DRIVER_MANIFEST_NAME, &mut sink)
            .map_err(|_| DriverError::DownloadFailed)?;
        sink.finish()
    }

    fn download_artifact(
        &self,
        version: &Version,
        artifact: &DriverArtifact,
    ) -> Result<PathBuf, DriverError> {
        let directory = self.cache_root.join("artifacts").join(artifact.sha256());
        ensure_directory(&directory)?;
        let destination = directory.join(artifact.name());
        if validate_file(&destination, artifact.size(), artifact.sha256()).is_ok() {
            return Ok(destination);
        }
        if destination.exists() || destination.symlink_metadata().is_ok() {
            return Err(DriverError::UnsafeCache);
        }

        let mut temporary = NamedTempFile::new_in(&directory).map_err(DriverError::Io)?;
        set_private_file(temporary.as_file())?;
        {
            let mut sink =
                ArtifactSink::new(temporary.as_file_mut(), artifact.size(), artifact.sha256());
            self.source
                .stream(version, artifact.name(), &mut sink)
                .map_err(|_| DriverError::DownloadFailed)?;
            sink.finish()?;
        }
        temporary.as_file_mut().flush().map_err(DriverError::Io)?;
        temporary.as_file().sync_all().map_err(DriverError::Io)?;
        temporary
            .persist_noclobber(&destination)
            .map_err(|_| DriverError::UnsafeCache)?;
        validate_file(&destination, artifact.size(), artifact.sha256())?;
        sync_directory(&directory)?;
        Ok(destination)
    }

    fn materialize(&self, acquired: AcquiredDriver) -> Result<SyncedDriver, DriverError> {
        let source_artifact = acquired
            .manifest
            .artifacts
            .iter()
            .find(|artifact| matches!(artifact, DriverArtifact::Source { .. }))
            .ok_or(DriverError::InvalidManifest)?;
        let source_archive = acquired
            .downloaded
            .get(source_artifact.name())
            .ok_or(DriverError::InvalidManifest)?;
        let source_root = extract_archive(
            source_archive,
            &self
                .cache_root
                .join("sources")
                .join(source_artifact.sha256()),
        )?;

        let mut prebuilt = Vec::new();
        for artifact in acquired
            .manifest
            .artifacts
            .iter()
            .filter(|artifact| matches!(artifact, DriverArtifact::Prebuilt { .. }))
        {
            let DriverArtifact::Prebuilt { kernel_release, .. } = artifact else {
                unreachable!()
            };
            let archive = acquired
                .downloaded
                .get(artifact.name())
                .ok_or(DriverError::InvalidManifest)?;
            let extracted = extract_archive(
                archive,
                &self.cache_root.join("prebuilt").join(artifact.sha256()),
            )?;
            let identity = validate_prebuilt(&extracted, kernel_release, &acquired.lock.commit)?;
            prebuilt.push(PrebuiltBundle::verified(
                extracted,
                identity.kernel_release,
                &identity.vermagic,
                &acquired.lock.manifest_sha256,
                &acquired.lock.manifest_sha256,
            )?);
        }
        let resolver = DriverResolver::new(source_root.clone(), prebuilt)?;
        Ok(SyncedDriver {
            lock: acquired.lock,
            source_root,
            resolver,
        })
    }
}

pub struct SyncedDriver {
    lock: DriverLock,
    source_root: PathBuf,
    resolver: DriverResolver,
}

impl SyncedDriver {
    pub fn lock(&self) -> &DriverLock {
        &self.lock
    }

    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    pub fn resolver(&self) -> &DriverResolver {
        &self.resolver
    }
}

struct AcquiredDriver {
    lock: DriverLock,
    manifest: DriverManifest,
    downloaded: BTreeMap<String, PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverManifest {
    schema_version: u32,
    driver_version: String,
    source: DriverSourceIdentity,
    supported: DriverSupported,
    reproducibility: DriverReproducibility,
    artifacts: Vec<DriverArtifact>,
}

impl DriverManifest {
    fn parse(bytes: &[u8], release_version: &Version) -> Result<Self, DriverError> {
        if bytes.len() > MAX_DRIVER_MANIFEST_BYTES {
            return Err(DriverError::InvalidManifest);
        }
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|_| DriverError::InvalidManifest)?;
        let driver_version =
            Version::parse(&manifest.driver_version).map_err(|_| DriverError::InvalidManifest)?;
        if manifest.schema_version != 1
            || driver_version.to_string() != manifest.driver_version
            || driver_version.major != release_version.major
            || driver_version.minor != release_version.minor
            || driver_version.patch != release_version.patch
            || !driver_version.pre.is_empty()
            || manifest.source.repository != DRIVER_REPOSITORY
            || !is_lower_hex(&manifest.source.commit, 40)
            || !is_lower_hex(&manifest.source.tree, 40)
            || manifest.source.date_epoch != manifest.reproducibility.source_date_epoch
            || manifest.supported.board != "Raspberry Pi Zero 2 W"
            || manifest.supported.display != "HyperPixel 2.1 Round"
            || manifest.supported.operating_system != "Raspberry Pi OS Lite (Trixie, 64-bit)"
            || manifest.supported.architecture != "aarch64"
            || manifest.supported.kernel_policy != "exact-release-only"
            || manifest.reproducibility.archive_format != "tar+zstd"
            || manifest.reproducibility.owner != 0
            || manifest.reproducibility.group != 0
            || manifest.reproducibility.mode_policy != "git-executable-or-regular"
        {
            return Err(DriverError::InvalidManifest);
        }
        let mut names = HashSet::new();
        let mut source_count = 0;
        let mut sbom_count = 0;
        let mut kernels = HashSet::new();
        for artifact in &manifest.artifacts {
            if !safe_name(artifact.name())
                || artifact.size() == 0
                || artifact.size() > MAX_DRIVER_ARTIFACT_BYTES
                || !is_lower_hex(artifact.sha256(), 64)
                || !names.insert(artifact.name())
            {
                return Err(DriverError::InvalidManifest);
            }
            match artifact {
                DriverArtifact::Source { name, .. } => {
                    source_count += 1;
                    if name != DRIVER_SOURCE_NAME {
                        return Err(DriverError::InvalidManifest);
                    }
                }
                DriverArtifact::Sbom { name, .. } => {
                    sbom_count += 1;
                    if name != DRIVER_SBOM_NAME {
                        return Err(DriverError::InvalidManifest);
                    }
                }
                DriverArtifact::Prebuilt {
                    name,
                    architecture,
                    kernel_release,
                    ..
                } => {
                    validate_kernel_release(kernel_release)?;
                    if architecture != "aarch64"
                        || name != &format!("hyperpixel2r-kms-{kernel_release}-aarch64.tar.zst")
                        || !kernels.insert(kernel_release)
                    {
                        return Err(DriverError::InvalidManifest);
                    }
                }
            }
        }
        if source_count != 1 || sbom_count != 1 {
            return Err(DriverError::InvalidManifest);
        }
        Ok(manifest)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverSourceIdentity {
    repository: String,
    commit: String,
    tree: String,
    date_epoch: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverSupported {
    board: String,
    display: String,
    operating_system: String,
    architecture: String,
    kernel_policy: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverReproducibility {
    archive_format: String,
    source_date_epoch: u64,
    owner: u32,
    group: u32,
    mode_policy: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum DriverArtifact {
    #[serde(rename = "source-archive")]
    Source {
        name: String,
        sha256: String,
        size: u64,
    },
    Sbom {
        name: String,
        sha256: String,
        size: u64,
    },
    #[serde(rename = "exact-kernel-bundle")]
    Prebuilt {
        name: String,
        sha256: String,
        size: u64,
        architecture: String,
        kernel_release: String,
    },
}

impl DriverArtifact {
    fn name(&self) -> &str {
        match self {
            Self::Source { name, .. } | Self::Sbom { name, .. } | Self::Prebuilt { name, .. } => {
                name
            }
        }
    }

    fn sha256(&self) -> &str {
        match self {
            Self::Source { sha256, .. }
            | Self::Sbom { sha256, .. }
            | Self::Prebuilt { sha256, .. } => sha256,
        }
    }

    fn size(&self) -> u64 {
        match self {
            Self::Source { size, .. } | Self::Sbom { size, .. } | Self::Prebuilt { size, .. } => {
                *size
            }
        }
    }
}

struct LimitedVec {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl LimitedVec {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }

    fn finish(self) -> Result<Vec<u8>, DriverError> {
        if self.exceeded {
            Err(DriverError::InvalidManifest)
        } else {
            Ok(self.bytes)
        }
    }
}

impl Write for LimitedVec {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        let accepted = remaining.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..accepted]);
        if accepted != bytes.len() {
            self.exceeded = true;
            return Err(io::Error::other("manifest limit exceeded"));
        }
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ArtifactSink<'a> {
    file: &'a mut File,
    expected_size: u64,
    expected_digest: &'a str,
    written: u64,
    hasher: Sha256,
}

impl<'a> ArtifactSink<'a> {
    fn new(file: &'a mut File, expected_size: u64, expected_digest: &'a str) -> Self {
        Self {
            file,
            expected_size,
            expected_digest,
            written: 0,
            hasher: Sha256::new(),
        }
    }

    fn finish(self) -> Result<(), DriverError> {
        let digest = format!("{:x}", self.hasher.finalize());
        if self.written != self.expected_size || digest != self.expected_digest {
            return Err(DriverError::ArtifactMismatch);
        }
        Ok(())
    }
}

impl Write for ArtifactSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len()).map_err(|_| io::Error::other("size overflow"))?;
        let next = self
            .written
            .checked_add(length)
            .ok_or_else(|| io::Error::other("size overflow"))?;
        if next > self.expected_size {
            return Err(io::Error::other("artifact limit exceeded"));
        }
        self.file.write_all(bytes)?;
        self.hasher.update(bytes);
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn cache_bytes(
    cache_root: &Path,
    name: &str,
    digest: &str,
    bytes: &[u8],
) -> Result<PathBuf, DriverError> {
    let directory = cache_root.join("artifacts").join(digest);
    ensure_directory(&directory)?;
    let path = directory.join(name);
    if validate_file(
        &path,
        u64::try_from(bytes.len()).map_err(|_| DriverError::ArtifactMismatch)?,
        digest,
    )
    .is_ok()
    {
        return Ok(path);
    }
    if path.exists() || path.symlink_metadata().is_ok() {
        return Err(DriverError::UnsafeCache);
    }
    let mut temporary = NamedTempFile::new_in(&directory).map_err(DriverError::Io)?;
    set_private_file(temporary.as_file())?;
    temporary.write_all(bytes).map_err(DriverError::Io)?;
    temporary.flush().map_err(DriverError::Io)?;
    temporary.as_file().sync_all().map_err(DriverError::Io)?;
    temporary
        .persist_noclobber(&path)
        .map_err(|_| DriverError::UnsafeCache)?;
    sync_directory(&directory)?;
    Ok(path)
}

fn ensure_directory(path: &Path) -> Result<(), DriverError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(DriverError::InvalidPath);
    }
    if let Ok(metadata) = path.symlink_metadata() {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DriverError::UnsafeCache);
        }
        #[cfg(unix)]
        if metadata.mode() & 0o077 != 0 {
            return Err(DriverError::UnsafeCache);
        }
        return Ok(());
    }
    let parent = path.parent().ok_or(DriverError::InvalidPath)?;
    if parent != path && !parent.exists() {
        ensure_directory(parent)?;
    }
    #[cfg(unix)]
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(DriverError::Io)?;
    #[cfg(not(unix))]
    fs::create_dir(path).map_err(DriverError::Io)?;
    Ok(())
}

fn set_private_file(file: &File) -> Result<(), DriverError> {
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(DriverError::Io)?;
    Ok(())
}

fn validate_file(
    path: &Path,
    expected_size: u64,
    expected_digest: &str,
) -> Result<(), DriverError> {
    validate_regular_digest(path, expected_size, expected_digest, true)
}

fn validate_regular_digest(
    path: &Path,
    expected_size: u64,
    expected_digest: &str,
    require_private_mode: bool,
) -> Result<(), DriverError> {
    let metadata = path.symlink_metadata().map_err(DriverError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != expected_size {
        return Err(DriverError::UnsafeCache);
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 || (require_private_mode && metadata.mode() & 0o077 != 0) {
        return Err(DriverError::UnsafeCache);
    }
    let mut file = File::open(path).map_err(DriverError::Io)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(DriverError::Io)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if format!("{:x}", hasher.finalize()) != expected_digest {
        return Err(DriverError::ArtifactMismatch);
    }
    Ok(())
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<PathBuf, DriverError> {
    if destination.exists() || destination.symlink_metadata().is_ok() {
        return validate_materialized(destination);
    }
    let parent = destination.parent().ok_or(DriverError::InvalidPath)?;
    ensure_directory(parent)?;
    let temporary = tempfile::Builder::new()
        .prefix(".extract.")
        .tempdir_in(parent)
        .map_err(DriverError::Io)?;
    let decoder = zstd::Decoder::new(File::open(archive).map_err(DriverError::Io)?)
        .map_err(DriverError::Io)?;
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().map_err(DriverError::Io)? {
        let mut entry = entry.map_err(DriverError::Io)?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(DriverError::UnsafeArchive);
        }
        let relative = entry.path().map_err(|_| DriverError::UnsafeArchive)?;
        if !safe_relative(&relative) {
            return Err(DriverError::UnsafeArchive);
        }
        let output = temporary.path().join(&relative);
        if entry_type.is_dir() {
            create_extraction_directory(&output)?;
            continue;
        }
        let parent = output.parent().ok_or(DriverError::UnsafeArchive)?;
        create_extraction_directory(parent)?;
        let mode = entry
            .header()
            .mode()
            .map_err(|_| DriverError::UnsafeArchive)?;
        let final_mode = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(final_mode);
        let mut file = options.open(&output).map_err(DriverError::Io)?;
        io::copy(&mut entry, &mut file).map_err(DriverError::Io)?;
        file.flush().map_err(DriverError::Io)?;
        file.sync_all().map_err(DriverError::Io)?;
    }
    let temporary_path = temporary.keep();
    fs::rename(&temporary_path, destination).map_err(DriverError::Io)?;
    sync_directory(parent)?;
    validate_materialized(destination)
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn create_extraction_directory(path: &Path) -> Result<(), DriverError> {
    if path.exists() {
        let metadata = path.symlink_metadata().map_err(DriverError::Io)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DriverError::UnsafeArchive);
        }
        return Ok(());
    }
    fs::create_dir_all(path).map_err(DriverError::Io)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(DriverError::Io)?;
    Ok(())
}

fn validate_materialized(destination: &Path) -> Result<PathBuf, DriverError> {
    let metadata = destination.symlink_metadata().map_err(DriverError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(DriverError::UnsafeCache);
    }
    let entries = fs::read_dir(destination)
        .map_err(DriverError::Io)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DriverError::Io)?;
    if entries.len() != 1 {
        return Err(DriverError::UnsafeArchive);
    }
    let root = entries[0].path();
    let root_metadata = root.symlink_metadata().map_err(DriverError::Io)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(DriverError::UnsafeArchive);
    }
    validate_tree(&root)?;
    Ok(root)
}

fn validate_tree(path: &Path) -> Result<(), DriverError> {
    for entry in fs::read_dir(path).map_err(DriverError::Io)? {
        let entry = entry.map_err(DriverError::Io)?;
        let metadata = entry.path().symlink_metadata().map_err(DriverError::Io)?;
        if metadata.file_type().is_symlink() {
            return Err(DriverError::UnsafeCache);
        }
        if metadata.is_dir() {
            validate_tree(&entry.path())?;
        } else if !metadata.is_file() {
            return Err(DriverError::UnsafeCache);
        } else {
            #[cfg(unix)]
            if metadata.nlink() != 1 {
                return Err(DriverError::UnsafeCache);
            }
        }
    }
    Ok(())
}

struct PrebuiltIdentity {
    kernel_release: String,
    vermagic: String,
}

fn validate_prebuilt(
    root: &Path,
    expected_kernel: &str,
    expected_commit: &str,
) -> Result<PrebuiltIdentity, DriverError> {
    let manifest_path = root.join("manifest.txt");
    let manifest_metadata = manifest_path.symlink_metadata().map_err(DriverError::Io)?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        return Err(DriverError::InvalidPrebuiltIdentity);
    }
    let contents = fs::read_to_string(&manifest_path).map_err(DriverError::Io)?;
    let mut fields = BTreeMap::new();
    for line in contents.lines() {
        let (key, value) = line
            .split_once('\t')
            .ok_or(DriverError::InvalidPrebuiltIdentity)?;
        if key.is_empty() || value.is_empty() || fields.insert(key, value).is_some() {
            return Err(DriverError::InvalidPrebuiltIdentity);
        }
    }
    let kernel_release = fields
        .get("kernel_release")
        .ok_or(DriverError::InvalidPrebuiltIdentity)?;
    let source_revision = fields
        .get("source_revision")
        .ok_or(DriverError::InvalidPrebuiltIdentity)?;
    let module_file = fields
        .get("module_file")
        .ok_or(DriverError::InvalidPrebuiltIdentity)?;
    let module_sha256 = fields
        .get("module_sha256")
        .ok_or(DriverError::InvalidPrebuiltIdentity)?;
    let vermagic = fields
        .get("module_vermagic")
        .ok_or(DriverError::InvalidPrebuiltIdentity)?;
    if *kernel_release != expected_kernel
        || *source_revision != expected_commit
        || !safe_name(module_file)
        || !is_lower_hex(module_sha256, 64)
        || vermagic.split_whitespace().next() != Some(expected_kernel)
    {
        return Err(DriverError::InvalidPrebuiltIdentity);
    }
    let module = root.join(module_file);
    let size = module.symlink_metadata().map_err(DriverError::Io)?.len();
    validate_regular_digest(&module, size, module_sha256, false)?;
    Ok(PrebuiltIdentity {
        kernel_release: (*kernel_release).to_owned(),
        vermagic: (*vermagic).to_owned(),
    })
}

fn atomic_write_lock(path: &Path, lock: &DriverLock) -> Result<(), DriverError> {
    let parent = path.parent().ok_or(DriverError::InvalidPath)?;
    if !parent.is_absolute() {
        return Err(DriverError::InvalidPath);
    }
    let contents = format!(
        "repository = \"{}\"\nversion = \"{}\"\ncommit = \"{}\"\nmanifest_sha256 = \"{}\"\n",
        lock.repository, lock.version, lock.commit, lock.manifest_sha256
    );
    let mut temporary = NamedTempFile::new_in(parent).map_err(DriverError::Io)?;
    set_private_file(temporary.as_file())?;
    temporary
        .write_all(contents.as_bytes())
        .map_err(DriverError::Io)?;
    temporary.flush().map_err(DriverError::Io)?;
    temporary.as_file().sync_all().map_err(DriverError::Io)?;
    temporary
        .persist(path)
        .map_err(|error| DriverError::Io(error.error))?;
    sync_directory(parent)?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), DriverError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(DriverError::Io)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 180
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverAction {
    ExportKernel,
    Build,
    StageTryboot,
    VerifyBoot,
    CommitBoot,
    RollbackBoot,
    Uninstall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriverContext {
    pub target: String,
    pub kernel_release: String,
    pub kernel_export: PathBuf,
    pub artifacts: PathBuf,
    pub replace_overlay: String,
}

pub struct DriverTool<R> {
    runner: R,
    source: PathBuf,
    context: DriverContext,
}

impl<R> DriverTool<R> {
    pub fn new(runner: R, source: PathBuf, context: DriverContext) -> Result<Self, DriverError> {
        if !source.is_absolute()
            || !context.kernel_export.is_absolute()
            || !context.artifacts.is_absolute()
            || context.target.is_empty()
            || context.target.contains(['\0', '\r', '\n'])
            || context.replace_overlay.is_empty()
        {
            return Err(DriverError::InvalidContext);
        }
        validate_kernel_release(&context.kernel_release)?;
        Ok(Self {
            runner,
            source,
            context,
        })
    }
}

impl<R: CommandRunner> DriverTool<R> {
    pub fn run(&self, action: DriverAction) -> Result<Option<DriverVerification>, DriverError> {
        let script = match action {
            DriverAction::ExportKernel => "export-target-kbuild.sh",
            DriverAction::Build => "build-driver.sh",
            DriverAction::StageTryboot => "stage-tryboot.sh",
            DriverAction::VerifyBoot => "verify-boot.sh",
            DriverAction::CommitBoot => "commit-boot.sh",
            DriverAction::RollbackBoot => "rollback-boot.sh",
            DriverAction::Uninstall => "uninstall.sh",
        };
        let script = self.source.join("scripts").join(script);
        let mut arguments = vec![
            script.to_string_lossy().into_owned(),
            "--target".into(),
            self.context.target.clone(),
        ];
        match action {
            DriverAction::ExportKernel => {
                arguments.extend([
                    "--output".into(),
                    self.context.kernel_export.to_string_lossy().into_owned(),
                ]);
            }
            DriverAction::Build => {
                arguments.extend([
                    "--kernel-release".into(),
                    self.context.kernel_release.clone(),
                    "--kernel-target".into(),
                    self.context.kernel_export.to_string_lossy().into_owned(),
                    "--output".into(),
                    self.context.artifacts.to_string_lossy().into_owned(),
                ]);
            }
            DriverAction::StageTryboot => {
                arguments.extend([
                    "--artifact-dir".into(),
                    self.context
                        .artifacts
                        .join(&self.context.kernel_release)
                        .to_string_lossy()
                        .into_owned(),
                    "--kernel-target".into(),
                    self.context.kernel_export.to_string_lossy().into_owned(),
                    "--replace-overlay".into(),
                    self.context.replace_overlay.clone(),
                ]);
            }
            DriverAction::VerifyBoot => {
                arguments.extend(["--expect-tryboot".into(), "--json".into()]);
            }
            DriverAction::CommitBoot | DriverAction::RollbackBoot | DriverAction::Uninstall => {}
        }
        let output = self
            .runner
            .run(Invocation::new("bash", arguments))
            .map_err(|_| DriverError::ToolFailed)?;
        if output.status() != 0 {
            return Err(DriverError::ToolFailed);
        }
        if action != DriverAction::VerifyBoot {
            return Ok(None);
        }
        let verification: DriverVerification = serde_json::from_slice(output.stdout())
            .map_err(|_| DriverError::InvalidVerification)?;
        verification.validate(&self.context.kernel_release)?;
        Ok(Some(verification))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DriverVerification {
    pub schema_version: u32,
    pub driver_version: String,
    pub kernel_release: String,
    pub module: String,
    pub drm_mode: String,
    pub touch: bool,
    pub sdl_driver: String,
    pub renderer: String,
    pub accepted: bool,
}

impl DriverVerification {
    fn validate(&self, expected_kernel: &str) -> Result<(), DriverError> {
        if self.schema_version != 1
            || self.kernel_release != expected_kernel
            || self.module != "hyperpixel2r_kms"
            || self.drm_mode != "480x480"
            || !self.touch
            || self.sdl_driver != "KMSDRM"
            || self.renderer != "opengles2"
            || !self.accepted
        {
            return Err(DriverError::InvalidVerification);
        }
        Ok(())
    }
}

fn validate_kernel_release(value: &str) -> Result<(), DriverError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(DriverError::InvalidKernelRelease);
    }
    Ok(())
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("driver kernel release is invalid")]
    InvalidKernelRelease,
    #[error("driver path is invalid")]
    InvalidPath,
    #[error("driver prebuilt identity is invalid")]
    InvalidPrebuiltIdentity,
    #[error("driver release contains duplicate prebuilt kernels")]
    DuplicatePrebuilt,
    #[error("driver tool context is invalid")]
    InvalidContext,
    #[error("driver tool failed")]
    ToolFailed,
    #[error("driver verification output is invalid")]
    InvalidVerification,
    #[error("driver lock is invalid")]
    InvalidLock,
    #[error("driver release manifest is invalid")]
    InvalidManifest,
    #[error("driver release download failed")]
    DownloadFailed,
    #[error("driver release verification failed")]
    VerificationFailed,
    #[error("driver release artifact does not match its manifest")]
    ArtifactMismatch,
    #[error("driver release cache is unsafe")]
    UnsafeCache,
    #[error("driver release archive is unsafe")]
    UnsafeArchive,
    #[error("driver filesystem operation failed")]
    Io(#[source] io::Error),
}
