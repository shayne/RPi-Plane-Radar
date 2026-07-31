use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
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
    transport::{CommandOutput, CommandRunner, Invocation, RunnerError, SystemCommandRunner},
};

const DRIVER_MANIFEST_NAME: &str = "driver-manifest.json";
const DRIVER_SOURCE_NAME: &str = "hyperpixel2r-kms-source.tar.zst";
const DRIVER_SBOM_NAME: &str = "SBOM.spdx.json";
const MAX_DRIVER_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_DRIVER_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARCHIVE_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 1024;
const MAX_ARCHIVE_PATH_DEPTH: usize = 32;
const DRIVER_GITHUB_REPOSITORY: &str = "shayne/hyperpixel2r-kms";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetProbe {
    kernel_release: String,
    vermagic: String,
}

impl TargetProbe {
    pub fn new(
        kernel_release: impl Into<String>,
        vermagic: impl Into<String>,
    ) -> Result<Self, DriverError> {
        let kernel_release = kernel_release.into();
        let vermagic = vermagic.into();
        validate_kernel_release(&kernel_release)?;
        validate_vermagic(&vermagic, &kernel_release)?;
        Ok(Self {
            kernel_release,
            vermagic,
        })
    }

    pub fn kernel_release(&self) -> &str {
        &self.kernel_release
    }

    pub fn vermagic(&self) -> &str {
        &self.vermagic
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct PrebuiltBundle {
    path: PathBuf,
    kernel_release: String,
    vermagic: String,
}

impl fmt::Debug for PrebuiltBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrebuiltBundle")
            .field("path", &"<redacted>")
            .field("kernel_release", &self.kernel_release)
            .field("vermagic", &self.vermagic)
            .finish()
    }
}

impl PrebuiltBundle {
    pub fn verified(
        path: PathBuf,
        kernel_release: impl Into<String>,
        actual_vermagic: &str,
        expected_vermagic: &str,
        actual_bundle_manifest_digest: &str,
        expected_bundle_manifest_digest: &str,
    ) -> Result<Self, DriverError> {
        let kernel_release = kernel_release.into();
        validate_kernel_release(&kernel_release)?;
        validate_vermagic(actual_vermagic, &kernel_release)?;
        validate_vermagic(expected_vermagic, &kernel_release)?;
        if actual_vermagic != expected_vermagic
            || actual_bundle_manifest_digest != expected_bundle_manifest_digest
            || !is_lower_hex(actual_bundle_manifest_digest, 64)
            || !is_lower_hex(expected_bundle_manifest_digest, 64)
        {
            return Err(DriverError::InvalidPrebuiltIdentity);
        }
        Ok(Self {
            path,
            kernel_release,
            vermagic: actual_vermagic.to_owned(),
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
        if let Some(bundle) = self.prebuilt.iter().find(|bundle| {
            bundle.kernel_release == probe.kernel_release && bundle.vermagic == probe.vermagic
        }) {
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
    fn verify(&self, lock: &DriverLock, assets: &[PathBuf]) -> Result<(), DriverError>;
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
    fn verify(&self, lock: &DriverLock, assets: &[PathBuf]) -> Result<(), DriverError> {
        lock.validate().map_err(|_| DriverError::InvalidLock)?;
        let tag = format!("v{}", lock.version);
        let reference = require_success(
            &self.runner,
            Invocation::new(
                "gh",
                vec![
                    "api".into(),
                    format!("repos/{DRIVER_GITHUB_REPOSITORY}/git/ref/tags/{tag}"),
                    "--jq".into(),
                    r#".object.type + "\t" + .object.sha"#.into(),
                ],
            ),
        )?;
        let tag_object = parse_git_object(reference.stdout(), "tag")?;
        let dereferenced = require_success(
            &self.runner,
            Invocation::new(
                "gh",
                vec![
                    "api".into(),
                    format!("repos/{DRIVER_GITHUB_REPOSITORY}/git/tags/{tag_object}"),
                    "--jq".into(),
                    r#".object.type + "\t" + .object.sha"#.into(),
                ],
            ),
        )?;
        if parse_git_object(dereferenced.stdout(), "commit")? != lock.commit {
            return Err(DriverError::InvalidReleaseTag);
        }
        for asset in assets {
            if !asset.is_absolute() {
                return Err(DriverError::InvalidPath);
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
                        "--signer-workflow".into(),
                        format!(
                            "github.com/{DRIVER_GITHUB_REPOSITORY}/.github/workflows/release.yml"
                        )
                        .into(),
                        "--source-digest".into(),
                        lock.commit.clone().into(),
                    ],
                ),
            )?;
        }
        Ok(())
    }
}

fn require_success<R: CommandRunner>(
    runner: &R,
    invocation: Invocation,
) -> Result<CommandOutput, DriverError> {
    let program = invocation.program().to_owned();
    let output = runner
        .run(invocation)
        .map_err(|source| DriverError::ReleaseCommandSpawn {
            program: program.clone(),
            source,
        })?;
    if output.status() != 0 {
        return Err(DriverError::ReleaseCommandFailed {
            program,
            status: output.status(),
            stderr: bounded_diagnostic(output.stderr()),
        });
    }
    Ok(output)
}

fn parse_git_object(bytes: &[u8], expected_type: &str) -> Result<String, DriverError> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| DriverError::InvalidReleaseTag)?
        .strip_suffix('\n')
        .ok_or(DriverError::InvalidReleaseTag)?;
    let (object_type, sha) = value
        .split_once('\t')
        .ok_or(DriverError::InvalidReleaseTag)?;
    if object_type != expected_type || !is_lower_hex(sha, 40) {
        return Err(DriverError::InvalidReleaseTag);
    }
    Ok(sha.to_owned())
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
        self.verifier.verify(&lock, &assets)?;
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
        let driver_version = acquired.manifest.driver_version.clone();
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
        validate_source_identity(&source_root, &acquired.manifest.source, &acquired.lock)?;

        let mut prebuilt = Vec::new();
        for artifact in acquired
            .manifest
            .artifacts
            .iter()
            .filter(|artifact| matches!(artifact, DriverArtifact::Prebuilt { .. }))
        {
            let DriverArtifact::Prebuilt {
                kernel_release,
                vermagic,
                bundle_manifest_sha256,
                ..
            } = artifact
            else {
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
            let identity = validate_prebuilt(
                &extracted,
                kernel_release,
                &acquired.lock.commit,
                vermagic,
                bundle_manifest_sha256,
            )?;
            prebuilt.push(PrebuiltBundle::verified(
                extracted,
                identity.kernel_release,
                &identity.vermagic,
                vermagic,
                &identity.bundle_manifest_sha256,
                bundle_manifest_sha256,
            )?);
        }
        let resolver = DriverResolver::new(source_root.clone(), prebuilt)?;
        Ok(SyncedDriver {
            lock: acquired.lock,
            driver_version,
            source_root,
            resolver,
        })
    }
}

pub struct SyncedDriver {
    lock: DriverLock,
    driver_version: String,
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

    pub fn tool<R>(
        &self,
        runner: R,
        probe: &TargetProbe,
        context: DriverContext,
    ) -> Result<DriverTool<R>, DriverError> {
        if context.kernel_release != probe.kernel_release {
            return Err(DriverError::InvalidContext);
        }
        DriverTool::new(
            runner,
            self.source_root.clone(),
            self.resolver.resolve(probe)?,
            context,
            self.driver_version.clone(),
            self.lock.commit.clone(),
        )
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
                    vermagic,
                    bundle_manifest_sha256,
                    ..
                } => {
                    validate_kernel_release(kernel_release)?;
                    if architecture != "aarch64"
                        || name != &format!("hyperpixel2r-kms-{kernel_release}-aarch64.tar.zst")
                        || validate_vermagic(vermagic, kernel_release).is_err()
                        || !is_lower_hex(bundle_manifest_sha256, 64)
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

fn validate_source_identity(
    source_root: &Path,
    expected: &DriverSourceIdentity,
    lock: &DriverLock,
) -> Result<(), DriverError> {
    let identity_path = source_root.join("release/source-identity.txt");
    let metadata = identity_path.symlink_metadata().map_err(DriverError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DriverError::InvalidManifest);
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 || metadata.mode() & 0o777 != 0o644 {
        return Err(DriverError::InvalidManifest);
    }
    let contents = fs::read_to_string(identity_path).map_err(DriverError::Io)?;
    let mut fields = BTreeMap::new();
    for line in contents.lines() {
        let (key, value) = line.split_once('\t').ok_or(DriverError::InvalidManifest)?;
        if key.is_empty() || value.is_empty() || fields.insert(key, value).is_some() {
            return Err(DriverError::InvalidManifest);
        }
    }
    if fields.len() != 4
        || fields.get("schema_version") != Some(&"1")
        || fields.get("repository") != Some(&expected.repository.as_str())
        || fields.get("source_revision") != Some(&expected.commit.as_str())
        || fields.get("source_tree") != Some(&expected.tree.as_str())
        || expected.repository != lock.repository
        || expected.commit != lock.commit
    {
        return Err(DriverError::InvalidManifest);
    }
    Ok(())
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
        vermagic: String,
        bundle_manifest_sha256: String,
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
    validate_existing_directory_ancestors(path)?;
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
    if parent != path {
        match parent.symlink_metadata() {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(DriverError::UnsafeCache);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => ensure_directory(parent)?,
            Err(error) => return Err(DriverError::Io(error)),
        }
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

fn validate_existing_directory_ancestors(path: &Path) -> Result<(), DriverError> {
    for ancestor in path
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
    {
        match ancestor.symlink_metadata() {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    if !trusted_system_directory_alias(ancestor, &metadata)? {
                        return Err(DriverError::UnsafeCache);
                    }
                } else if !metadata.is_dir() {
                    return Err(DriverError::UnsafeCache);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(DriverError::Io(error)),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn trusted_system_directory_alias(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<bool, DriverError> {
    let parent = path.parent().ok_or(DriverError::UnsafeCache)?;
    let parent_metadata = parent.symlink_metadata().map_err(DriverError::Io)?;
    Ok(metadata.uid() == 0
        && parent_metadata.is_dir()
        && parent_metadata.uid() == 0
        && parent_metadata.mode() & 0o022 == 0)
}

#[cfg(not(unix))]
fn trusted_system_directory_alias(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<bool, DriverError> {
    Ok(false)
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
    let parent = destination.parent().ok_or(DriverError::InvalidPath)?;
    ensure_directory(parent)?;
    let temporary = tempfile::Builder::new()
        .prefix(".extract.")
        .tempdir_in(parent)
        .map_err(DriverError::Io)?;
    let decoder = zstd::Decoder::new(File::open(archive).map_err(DriverError::Io)?)
        .map_err(DriverError::Io)?;
    let mut tar = tar::Archive::new(decoder);
    let mut entry_count = 0_usize;
    let mut total_bytes = 0_u64;
    for entry in tar.entries().map_err(DriverError::Io)? {
        let mut entry = entry.map_err(DriverError::Io)?;
        entry_count = entry_count
            .checked_add(1)
            .ok_or(DriverError::UnsafeArchive)?;
        if entry_count > MAX_ARCHIVE_ENTRIES {
            return Err(DriverError::UnsafeArchive);
        }
        let entry_bytes = entry
            .header()
            .size()
            .map_err(|_| DriverError::UnsafeArchive)?;
        if entry_bytes > MAX_ARCHIVE_ENTRY_BYTES {
            return Err(DriverError::UnsafeArchive);
        }
        total_bytes = total_bytes
            .checked_add(entry_bytes)
            .ok_or(DriverError::UnsafeArchive)?;
        if total_bytes > MAX_ARCHIVE_TOTAL_BYTES {
            return Err(DriverError::UnsafeArchive);
        }
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(DriverError::UnsafeArchive);
        }
        let relative = entry.path().map_err(|_| DriverError::UnsafeArchive)?;
        if !safe_relative(&relative) || relative.components().count() > MAX_ARCHIVE_PATH_DEPTH {
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
        let copied = io::copy(&mut entry, &mut file).map_err(DriverError::Io)?;
        if copied != entry_bytes {
            return Err(DriverError::UnsafeArchive);
        }
        file.flush().map_err(DriverError::Io)?;
        file.sync_all().map_err(DriverError::Io)?;
    }
    let fresh_root = validate_materialized(temporary.path())?;
    if destination.exists() || destination.symlink_metadata().is_ok() {
        let existing_root = validate_materialized(destination)?;
        if materialized_manifest(&existing_root)? != materialized_manifest(&fresh_root)? {
            return Err(DriverError::UnsafeCache);
        }
        return Ok(existing_root);
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

#[derive(Debug, Eq, PartialEq)]
enum MaterializedEntry {
    Directory {
        mode: u32,
        uid: u32,
        gid: u32,
    },
    File {
        mode: u32,
        uid: u32,
        gid: u32,
        size: u64,
        sha256: String,
    },
}

fn materialized_manifest(root: &Path) -> Result<BTreeMap<PathBuf, MaterializedEntry>, DriverError> {
    let mut manifest = BTreeMap::new();
    let metadata = root.symlink_metadata().map_err(DriverError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(DriverError::UnsafeCache);
    }
    #[cfg(unix)]
    let (mode, uid, gid) = (metadata.mode() & 0o777, metadata.uid(), metadata.gid());
    #[cfg(not(unix))]
    let (mode, uid, gid) = (0, 0, 0);
    manifest.insert(
        PathBuf::from("."),
        MaterializedEntry::Directory { mode, uid, gid },
    );
    collect_materialized_manifest(root, root, &mut manifest)?;
    Ok(manifest)
}

fn collect_materialized_manifest(
    root: &Path,
    directory: &Path,
    manifest: &mut BTreeMap<PathBuf, MaterializedEntry>,
) -> Result<(), DriverError> {
    for entry in fs::read_dir(directory).map_err(DriverError::Io)? {
        let entry = entry.map_err(DriverError::Io)?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| DriverError::UnsafeCache)?
            .to_owned();
        let metadata = path.symlink_metadata().map_err(DriverError::Io)?;
        if metadata.file_type().is_symlink() {
            return Err(DriverError::UnsafeCache);
        }
        #[cfg(unix)]
        let (mode, uid, gid) = (metadata.mode() & 0o777, metadata.uid(), metadata.gid());
        #[cfg(not(unix))]
        let (mode, uid, gid) = (0, 0, 0);
        if metadata.is_dir() {
            manifest.insert(relative, MaterializedEntry::Directory { mode, uid, gid });
            collect_materialized_manifest(root, &path, manifest)?;
        } else if metadata.is_file() {
            #[cfg(unix)]
            if metadata.nlink() != 1 {
                return Err(DriverError::UnsafeCache);
            }
            let mut file = File::open(&path).map_err(DriverError::Io)?;
            let mut hasher = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let count = file.read(&mut buffer).map_err(DriverError::Io)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            manifest.insert(
                relative,
                MaterializedEntry::File {
                    mode,
                    uid,
                    gid,
                    size: metadata.len(),
                    sha256: format!("{:x}", hasher.finalize()),
                },
            );
        } else {
            return Err(DriverError::UnsafeCache);
        }
    }
    Ok(())
}

struct PrebuiltIdentity {
    kernel_release: String,
    vermagic: String,
    bundle_manifest_sha256: String,
}

fn validate_prebuilt(
    root: &Path,
    expected_kernel: &str,
    expected_commit: &str,
    expected_vermagic: &str,
    expected_bundle_manifest_sha256: &str,
) -> Result<PrebuiltIdentity, DriverError> {
    let manifest_path = root.join("manifest.txt");
    let manifest_metadata = manifest_path.symlink_metadata().map_err(DriverError::Io)?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        return Err(DriverError::InvalidPrebuiltIdentity);
    }
    #[cfg(unix)]
    if manifest_metadata.nlink() != 1 {
        return Err(DriverError::InvalidPrebuiltIdentity);
    }
    let manifest_bytes = fs::read(&manifest_path).map_err(DriverError::Io)?;
    let bundle_manifest_sha256 = sha256_bytes(&manifest_bytes);
    if bundle_manifest_sha256 != expected_bundle_manifest_sha256 {
        return Err(DriverError::InvalidPrebuiltIdentity);
    }
    let contents =
        std::str::from_utf8(&manifest_bytes).map_err(|_| DriverError::InvalidPrebuiltIdentity)?;
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
        || *vermagic != expected_vermagic
        || validate_vermagic(vermagic, expected_kernel).is_err()
    {
        return Err(DriverError::InvalidPrebuiltIdentity);
    }
    let module = root.join(module_file);
    let size = module.symlink_metadata().map_err(DriverError::Io)?.len();
    validate_regular_digest(&module, size, module_sha256, false)?;
    Ok(PrebuiltIdentity {
        kernel_release: (*kernel_release).to_owned(),
        vermagic: (*vermagic).to_owned(),
        bundle_manifest_sha256,
    })
}

fn atomic_write_lock(path: &Path, lock: &DriverLock) -> Result<(), DriverError> {
    let parent = path.parent().ok_or(DriverError::InvalidPath)?;
    if !parent.is_absolute() {
        return Err(DriverError::InvalidPath);
    }
    let contents = format!(
        "repository = \"{}\"\nversion = \"{}\"\ncommit = \"{}\"\nmanifest_sha256 = \"{}\"\nlifecycle_protocol = \"{}\"\n",
        lock.repository,
        lock.version,
        lock.commit,
        lock.manifest_sha256,
        crate::config::DRIVER_LIFECYCLE_PROTOCOL
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
    RecordAccepted,
    PrepareAccepted,
    MarkCommittedAccepted,
    StageRetained,
    CommitRetained,
    RecoverAccepted,
    MarkVerifiedAccepted,
    FinalizeAccepted,
    UninstallAccepted,
    FinalizeUninstall,
    RetireInactive,
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
    plan: DriverPlan,
    context: DriverContext,
    driver_version: String,
    source_revision: String,
    expected_overlay_file: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriverPostconditions {
    pub driver_version: String,
    pub source_revision: String,
    pub source_tree: String,
    pub kernel_release: String,
    pub module_vermagic: String,
    pub manifest_sha256: String,
    pub module_file: String,
    pub module_sha256: String,
    pub overlay_file: String,
    pub overlay_sha256: String,
    pub applied_dtb_file: String,
    pub applied_dtb_sha256: String,
    pub replaced_overlay: String,
}

impl<R> DriverTool<R> {
    pub fn new(
        runner: R,
        source: PathBuf,
        plan: DriverPlan,
        context: DriverContext,
        driver_version: String,
        source_revision: String,
    ) -> Result<Self, DriverError> {
        if !source.is_absolute()
            || !context.kernel_export.is_absolute()
            || !context.artifacts.is_absolute()
            || context.target.is_empty()
            || context.target.contains(['\0', '\r', '\n'])
            || !crate::preflight::is_supported_hyperpixel_overlay(&context.replace_overlay)
            || Version::parse(&driver_version).ok().is_none_or(|version| {
                !version.pre.is_empty() || version.to_string() != driver_version
            })
            || !is_lower_hex(&source_revision, 40)
        {
            return Err(DriverError::InvalidContext);
        }
        match &plan {
            DriverPlan::Prebuilt { archive } => {
                if !archive.is_absolute() {
                    return Err(DriverError::InvalidContext);
                }
            }
            DriverPlan::CrossBuild { source: planned } => {
                if planned != &source {
                    return Err(DriverError::InvalidContext);
                }
            }
        }
        validate_kernel_release(&context.kernel_release)?;
        let expected_overlay_file = format!("hyperpixel2r-kms-{}.dtbo", &source_revision[..12]);
        Ok(Self {
            runner,
            source,
            plan,
            context,
            driver_version,
            source_revision,
            expected_overlay_file,
        })
    }

    pub fn driver_version(&self) -> &str {
        &self.driver_version
    }

    pub fn source_revision(&self) -> &str {
        &self.source_revision
    }

    pub fn kernel_release(&self) -> &str {
        &self.context.kernel_release
    }

    pub fn expected_overlay_file(&self) -> &str {
        &self.expected_overlay_file
    }

    pub fn postconditions(&self) -> Result<DriverPostconditions, DriverError> {
        let artifact_dir = match &self.plan {
            DriverPlan::Prebuilt { archive } => archive.clone(),
            DriverPlan::CrossBuild { .. } => {
                self.context.artifacts.join(&self.context.kernel_release)
            }
        };
        let manifest_path = artifact_dir.join("manifest.txt");
        let metadata = fs::symlink_metadata(&manifest_path).map_err(DriverError::Io)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(DriverError::InvalidPrebuiltIdentity);
        }
        let manifest = fs::read(&manifest_path).map_err(DriverError::Io)?;
        let text =
            std::str::from_utf8(&manifest).map_err(|_| DriverError::InvalidPrebuiltIdentity)?;
        let mut fields = BTreeMap::<String, String>::new();
        for line in text.lines() {
            let (key, value) = line
                .split_once('\t')
                .ok_or(DriverError::InvalidPrebuiltIdentity)?;
            if key.is_empty()
                || value.is_empty()
                || fields.insert(key.to_owned(), value.to_owned()).is_some()
            {
                return Err(DriverError::InvalidPrebuiltIdentity);
            }
        }
        const KEYS: [&str; 14] = [
            "schema_version",
            "driver_version",
            "source_revision",
            "source_tree",
            "kernel_release",
            "architecture",
            "base_dtb_sha256",
            "module_file",
            "module_sha256",
            "module_vermagic",
            "overlay_file",
            "overlay_sha256",
            "applied_dtb_file",
            "applied_dtb_sha256",
        ];
        if fields.len() != KEYS.len() || KEYS.iter().any(|key| !fields.contains_key(*key)) {
            return Err(DriverError::InvalidPrebuiltIdentity);
        }
        let get = |key: &str| {
            fields
                .get(key)
                .cloned()
                .ok_or(DriverError::InvalidPrebuiltIdentity)
        };
        let result = DriverPostconditions {
            driver_version: get("driver_version")?,
            source_revision: get("source_revision")?,
            source_tree: get("source_tree")?,
            kernel_release: get("kernel_release")?,
            module_vermagic: get("module_vermagic")?,
            manifest_sha256: sha256_bytes(&manifest),
            module_file: get("module_file")?,
            module_sha256: get("module_sha256")?,
            overlay_file: get("overlay_file")?,
            overlay_sha256: get("overlay_sha256")?,
            applied_dtb_file: get("applied_dtb_file")?,
            applied_dtb_sha256: get("applied_dtb_sha256")?,
            replaced_overlay: self.context.replace_overlay.clone(),
        };
        if fields.get("schema_version").map(String::as_str) != Some("1")
            || fields.get("architecture").map(String::as_str) != Some("aarch64")
            || result.driver_version != self.driver_version
            || result.source_revision != self.source_revision
            || result.kernel_release != self.context.kernel_release
            || !is_lower_hex(&result.source_tree, 40)
            || !is_lower_hex(
                fields
                    .get("base_dtb_sha256")
                    .ok_or(DriverError::InvalidPrebuiltIdentity)?,
                64,
            )
            || result.module_file != "hyperpixel2r_kms.ko"
            || result.overlay_file != self.expected_overlay_file
            || result.applied_dtb_file != "hyperpixel2r-kms-applied.dtb"
            || validate_vermagic(&result.module_vermagic, &result.kernel_release).is_err()
        {
            return Err(DriverError::InvalidPrebuiltIdentity);
        }
        for (name, digest) in [
            (&result.module_file, &result.module_sha256),
            (&result.overlay_file, &result.overlay_sha256),
            (&result.applied_dtb_file, &result.applied_dtb_sha256),
        ] {
            if !safe_name(name) || !is_lower_hex(digest, 64) {
                return Err(DriverError::InvalidPrebuiltIdentity);
            }
            let path = artifact_dir.join(name);
            let leaf = fs::symlink_metadata(&path).map_err(DriverError::Io)?;
            if !leaf.file_type().is_file()
                || leaf.file_type().is_symlink()
                || sha256_bytes(&fs::read(path).map_err(DriverError::Io)?) != *digest
            {
                return Err(DriverError::InvalidPrebuiltIdentity);
            }
        }
        Ok(result)
    }
}

impl<R: CommandRunner> DriverTool<R> {
    pub fn run_accepted_protocol(
        &self,
        action: DriverAction,
        source_revision: Option<&str>,
    ) -> Result<(), DriverError> {
        if !matches!(
            action,
            DriverAction::RecordAccepted
                | DriverAction::MarkCommittedAccepted
                | DriverAction::StageRetained
                | DriverAction::CommitRetained
                | DriverAction::RecoverAccepted
                | DriverAction::MarkVerifiedAccepted
                | DriverAction::FinalizeAccepted
                | DriverAction::UninstallAccepted
                | DriverAction::FinalizeUninstall
                | DriverAction::RetireInactive
        ) {
            return Err(DriverError::InvalidContext);
        }
        let requires_identity = matches!(
            action,
            DriverAction::RecordAccepted
                | DriverAction::StageRetained
                | DriverAction::UninstallAccepted
                | DriverAction::RetireInactive
        );
        if requires_identity != source_revision.is_some()
            || source_revision.is_some_and(|revision| !is_lower_hex(revision, 40))
        {
            return Err(DriverError::InvalidContext);
        }
        let action_name = match action {
            DriverAction::RecordAccepted => "record",
            DriverAction::MarkCommittedAccepted => "mark-committed",
            DriverAction::StageRetained => "stage-retained",
            DriverAction::CommitRetained => "commit-retained",
            DriverAction::RecoverAccepted => "recover",
            DriverAction::MarkVerifiedAccepted => "mark-verified",
            DriverAction::FinalizeAccepted => "finalize",
            DriverAction::UninstallAccepted => "uninstall",
            DriverAction::FinalizeUninstall => "finalize-uninstall",
            DriverAction::RetireInactive => "retire-inactive",
            _ => unreachable!("accepted protocol action was validated"),
        };
        let mut arguments = vec![
            self.source
                .join("scripts/accepted-lifecycle.sh")
                .to_string_lossy()
                .into_owned(),
            "--target".into(),
            self.context.target.clone(),
            "--action".into(),
            action_name.into(),
        ];
        if let Some(revision) = source_revision {
            arguments.extend([
                "--driver-version".into(),
                self.driver_version.clone(),
                "--source-revision".into(),
                revision.to_owned(),
                "--kernel-release".into(),
                self.context.kernel_release.clone(),
            ]);
        }
        self.execute(action, arguments)?;
        Ok(())
    }

    pub fn prepare_accepted_protocol(
        &self,
        candidate: &DriverPostconditions,
    ) -> Result<(), DriverError> {
        if candidate.kernel_release != self.context.kernel_release
            || candidate.module_file != "hyperpixel2r_kms.ko"
            || !is_lower_hex(&candidate.source_revision, 40)
            || !is_lower_hex(&candidate.manifest_sha256, 64)
            || !is_lower_hex(&candidate.module_sha256, 64)
            || !is_lower_hex(&candidate.overlay_sha256, 64)
            || candidate.overlay_file
                != format!("hyperpixel2r-kms-{}.dtbo", &candidate.source_revision[..12])
        {
            return Err(DriverError::InvalidContext);
        }
        let arguments = vec![
            self.source
                .join("scripts/accepted-lifecycle.sh")
                .to_string_lossy()
                .into_owned(),
            "--target".into(),
            self.context.target.clone(),
            "--action".into(),
            "prepare-new".into(),
            "--driver-version".into(),
            candidate.driver_version.clone(),
            "--source-revision".into(),
            candidate.source_revision.clone(),
            "--kernel-release".into(),
            candidate.kernel_release.clone(),
            "--manifest-sha256".into(),
            candidate.manifest_sha256.clone(),
            "--module-file".into(),
            candidate.module_file.clone(),
            "--module-sha256".into(),
            candidate.module_sha256.clone(),
            "--overlay-file".into(),
            candidate.overlay_file.clone(),
            "--overlay-sha256".into(),
            candidate.overlay_sha256.clone(),
        ];
        self.execute(DriverAction::PrepareAccepted, arguments)?;
        Ok(())
    }

    pub fn prepare_artifacts(&self) -> Result<(), DriverError> {
        match self.plan {
            DriverPlan::Prebuilt { .. } => {
                self.run(DriverAction::ExportKernel)?;
            }
            DriverPlan::CrossBuild { .. } => {
                self.run(DriverAction::ExportKernel)?;
                self.run(DriverAction::Build)?;
            }
        }
        Ok(())
    }

    pub fn stage_prepared(&self) -> Result<(), DriverError> {
        self.run(DriverAction::StageTryboot)?;
        Ok(())
    }

    pub fn prepare_and_stage(&self) -> Result<(), DriverError> {
        self.prepare_artifacts()?;
        self.stage_prepared()
    }

    pub fn cleanup_legacy_planeradar(&self) -> Result<(), DriverError> {
        let arguments = vec![
            self.source
                .join("scripts/uninstall.sh")
                .to_string_lossy()
                .into_owned(),
            "--target".into(),
            self.context.target.clone(),
            "--cleanup-legacy-planeradar".into(),
            "--expect-overlay-file".into(),
            self.expected_overlay_file.clone(),
        ];
        self.execute(DriverAction::Uninstall, arguments)?;
        Ok(())
    }

    pub fn verify_normal_boot(&self) -> Result<DriverVerification, DriverError> {
        let arguments = vec![
            self.source
                .join("scripts/verify-boot.sh")
                .to_string_lossy()
                .into_owned(),
            "--target".into(),
            self.context.target.clone(),
            "--expect-normal".into(),
            "--expect-driver-version".into(),
            self.driver_version.clone(),
            "--expect-overlay-file".into(),
            self.expected_overlay_file.clone(),
            "--json".into(),
        ];
        let output = self.execute(DriverAction::VerifyBoot, arguments)?;
        self.parse_verification(output.stdout())
    }

    pub fn run(&self, action: DriverAction) -> Result<Option<DriverVerification>, DriverError> {
        let script = match action {
            DriverAction::ExportKernel => "export-target-kbuild.sh",
            DriverAction::Build => "build-driver.sh",
            DriverAction::StageTryboot => "stage-tryboot.sh",
            DriverAction::VerifyBoot => "verify-boot.sh",
            DriverAction::CommitBoot => "commit-boot.sh",
            DriverAction::RollbackBoot => "rollback-boot.sh",
            DriverAction::RecordAccepted
            | DriverAction::PrepareAccepted
            | DriverAction::MarkCommittedAccepted
            | DriverAction::StageRetained
            | DriverAction::CommitRetained
            | DriverAction::RecoverAccepted
            | DriverAction::MarkVerifiedAccepted
            | DriverAction::FinalizeAccepted
            | DriverAction::UninstallAccepted => "accepted-lifecycle.sh",
            DriverAction::FinalizeUninstall | DriverAction::RetireInactive => {
                "accepted-lifecycle.sh"
            }
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
                    "--source-revision".into(),
                    self.source_revision.clone(),
                    "--output".into(),
                    self.context.artifacts.to_string_lossy().into_owned(),
                ]);
            }
            DriverAction::StageTryboot => {
                arguments.extend([
                    "--artifact-dir".into(),
                    match &self.plan {
                        DriverPlan::Prebuilt { archive } => archive.to_string_lossy().into_owned(),
                        DriverPlan::CrossBuild { .. } => self
                            .context
                            .artifacts
                            .join(&self.context.kernel_release)
                            .to_string_lossy()
                            .into_owned(),
                    },
                    "--kernel-target".into(),
                    self.context.kernel_export.to_string_lossy().into_owned(),
                    "--replace-overlay".into(),
                    self.context.replace_overlay.clone(),
                ]);
            }
            DriverAction::VerifyBoot => {
                arguments.extend([
                    "--expect-tryboot".into(),
                    "--expect-driver-version".into(),
                    self.driver_version.clone(),
                    "--expect-overlay-file".into(),
                    self.expected_overlay_file.clone(),
                    "--json".into(),
                ]);
            }
            DriverAction::RecordAccepted
            | DriverAction::PrepareAccepted
            | DriverAction::MarkCommittedAccepted
            | DriverAction::StageRetained
            | DriverAction::CommitRetained
            | DriverAction::RecoverAccepted
            | DriverAction::MarkVerifiedAccepted
            | DriverAction::FinalizeAccepted
            | DriverAction::UninstallAccepted
            | DriverAction::FinalizeUninstall
            | DriverAction::RetireInactive => {
                let action_name = match action {
                    DriverAction::RecordAccepted => "record",
                    DriverAction::PrepareAccepted => "prepare-new",
                    DriverAction::MarkCommittedAccepted => "mark-committed",
                    DriverAction::StageRetained => "stage-retained",
                    DriverAction::CommitRetained => "commit-retained",
                    DriverAction::RecoverAccepted => "recover",
                    DriverAction::MarkVerifiedAccepted => "mark-verified",
                    DriverAction::FinalizeAccepted => "finalize",
                    DriverAction::UninstallAccepted => "uninstall",
                    DriverAction::FinalizeUninstall => "finalize-uninstall",
                    DriverAction::RetireInactive => "retire-inactive",
                    _ => unreachable!("accepted driver action classification"),
                };
                arguments.extend(["--action".into(), action_name.into()]);
                if matches!(
                    action,
                    DriverAction::RecordAccepted
                        | DriverAction::PrepareAccepted
                        | DriverAction::StageRetained
                        | DriverAction::UninstallAccepted
                        | DriverAction::RetireInactive
                ) {
                    arguments.extend([
                        "--driver-version".into(),
                        self.driver_version.clone(),
                        "--source-revision".into(),
                        self.source_revision.clone(),
                        "--kernel-release".into(),
                        self.context.kernel_release.clone(),
                    ]);
                }
            }
            DriverAction::CommitBoot | DriverAction::RollbackBoot | DriverAction::Uninstall => {}
        }
        let output = self.execute(action, arguments)?;
        if action != DriverAction::VerifyBoot {
            return Ok(None);
        }
        Ok(Some(self.parse_verification(output.stdout())?))
    }

    fn parse_verification(&self, output: &[u8]) -> Result<DriverVerification, DriverError> {
        let verification: DriverVerification =
            serde_json::from_slice(output).map_err(|_| DriverError::InvalidVerification)?;
        verification.validate(&self.context.kernel_release, &self.driver_version)?;
        Ok(verification)
    }

    fn execute(
        &self,
        action: DriverAction,
        arguments: Vec<String>,
    ) -> Result<CommandOutput, DriverError> {
        let output = self
            .runner
            .run(
                Invocation::new("bash", arguments)
                    .with_timeout(Duration::from_secs(15 * 60))
                    .with_stdout_limit(64 * 1024),
            )
            .map_err(|source| DriverError::ToolCommandSpawn {
                action,
                program: "bash".into(),
                source,
            })?;
        if output.status() != 0 {
            return Err(DriverError::ToolCommandFailed {
                action,
                program: "bash".into(),
                status: output.status(),
                stderr: bounded_diagnostic(output.stderr()),
            });
        }
        Ok(output)
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
    fn validate(
        &self,
        expected_kernel: &str,
        expected_driver_version: &str,
    ) -> Result<(), DriverError> {
        if self.schema_version != 1
            || self.driver_version != expected_driver_version
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

fn validate_vermagic(value: &str, expected_kernel_release: &str) -> Result<(), DriverError> {
    if value.len() > 512
        || value
            .strip_prefix(expected_kernel_release)
            .is_none_or(|suffix| !suffix.starts_with(' '))
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() && byte != b' ')
    {
        return Err(DriverError::InvalidPrebuiltIdentity);
    }
    Ok(())
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bounded_diagnostic(bytes: &[u8]) -> String {
    const MAX_DIAGNOSTIC_BYTES: usize = 4096;
    let mut diagnostic = String::new();
    for character in String::from_utf8_lossy(bytes).chars() {
        let character = if character == '\n' || character == '\t' || !character.is_control() {
            character
        } else {
            '\u{fffd}'
        };
        if diagnostic.len() + character.len_utf8() > MAX_DIAGNOSTIC_BYTES {
            break;
        }
        diagnostic.push(character);
    }
    diagnostic
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
    #[error("driver {action:?} command {program} could not be started")]
    ToolCommandSpawn {
        action: DriverAction,
        program: String,
        #[source]
        source: RunnerError,
    },
    #[error("driver {action:?} command {program} exited {status}: {stderr}")]
    ToolCommandFailed {
        action: DriverAction,
        program: String,
        status: i32,
        stderr: String,
    },
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
    #[error("driver release tag identity is invalid")]
    InvalidReleaseTag,
    #[error("driver release verification command {program} could not be started")]
    ReleaseCommandSpawn {
        program: String,
        #[source]
        source: RunnerError,
    },
    #[error("driver release verification command {program} exited {status}: {stderr}")]
    ReleaseCommandFailed {
        program: String,
        status: i32,
        stderr: String,
    },
    #[error("driver release artifact does not match its manifest")]
    ArtifactMismatch,
    #[error("driver release cache is unsafe")]
    UnsafeCache,
    #[error("driver release archive is unsafe")]
    UnsafeArchive,
    #[error("driver filesystem operation failed")]
    Io(#[source] io::Error),
}
