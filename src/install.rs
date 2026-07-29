use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use nix::fcntl::{OFlag, openat, renameat};
use nix::sys::stat::{Mode, mkdirat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const STOCK_HYPERPIXEL_DECLARATION: &str = "dtoverlay=vc4-kms-dpi-hyperpixel2r";
pub const DEFAULT_HYPERPIXEL_DECLARATION: &str = STOCK_HYPERPIXEL_DECLARATION;
pub const PLANERADAR_HYPERPIXEL_PREFIX: &str = "planeradar-hyperpixel2r-";
pub const MAX_BOOT_CONFIG_LINE_BYTES: usize = 98;
pub const PLANERADAR_SERVICE: &str = include_str!("../packaging/planeradar.service");

const INSTALL_BINARY: &str = "opt/planeradar/bin/planeradar";
const INSTALL_REVISION: &str = "opt/planeradar/REVISION";
const INSTALL_CHECKSUM: &str = "opt/planeradar/SHA256";
const INSTALL_SERVICE: &str = "etc/systemd/system/planeradar.service";
const INSTALL_STATE: &str = "var/lib/planeradar";
const LIFECYCLE_STATE: &str = "var/lib/planeradar-installer/lifecycle.json";
const RUNTIME_PACKAGES: &[&str] = &[
    "libsdl2-2.0-0",
    "libegl1",
    "libgles2",
    "libgl1-mesa-dri",
    "ca-certificates",
    "avahi-daemon",
    "dkms",
    "kmod",
    "device-tree-compiler",
    "linux-headers-rpi-v8",
    "build-essential",
    "evtest",
    "pngcheck",
];

const SUPPORTED_DISPLAY_PARAMETERS: &[&str] = &[
    "rotate=0",
    "rotate=90",
    "rotate=180",
    "rotate=270",
    "touchscreen-inverted-x",
    "touchscreen-inverted-y",
    "touchscreen-swapped-x-y",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplaySelection<'a> {
    Stock,
    Candidate {
        overlay: &'a str,
        parameters: &'a [&'a str],
    },
}

struct ConfigLine {
    body: String,
    ending: String,
}

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("boot configuration path has no parent: {0}")]
    MissingParent(PathBuf),
    #[error("boot configuration changed after preview: {0}")]
    SourceChanged(PathBuf),
    #[error("invalid versioned HyperPixel overlay name: {0}")]
    InvalidOverlayName(String),
    #[error("unsupported HyperPixel overlay parameter: {0}")]
    InvalidDisplayParameter(String),
    #[error("duplicate HyperPixel overlay parameter: {0}")]
    DuplicateDisplayParameter(String),
    #[error("boot configuration line {line} is {bytes} bytes; maximum is 98")]
    BootLineTooLong { line: usize, bytes: usize },
    #[error("normal and tryboot configuration paths resolve to the same destination: {0}")]
    ConflictingConfigPath(PathBuf),
    #[error("refusing unsafe non-regular configuration file: {0}")]
    UnsafeFileType(PathBuf),
    #[error("unsupported installation root: {0}")]
    InvalidRoot(PathBuf),
    #[error("unsupported operating system: {0}")]
    UnsupportedOperatingSystem(String),
    #[error("unsupported hardware model: {0}")]
    UnsupportedHardware(String),
    #[error("installer must run as an AArch64 binary on the live target")]
    UnsupportedArchitecture,
    #[error("invalid Plane Radar artifact: {0}")]
    InvalidArtifact(String),
    #[error("artifact checksum does not match the verified sidecar")]
    ChecksumMismatch,
    #[error("artifact revision does not match this installer: {0}")]
    RevisionMismatch(String),
    #[error("target installer state is invalid")]
    InvalidInstallerState,
    #[error("application release identity is invalid")]
    InvalidReleaseIdentity,
    #[error("owned-file manifest is invalid")]
    InvalidOwnershipManifest,
    #[error("settings are not proven installer-owned")]
    SettingsNotOwned,
    #[error("an installer-owned path has drifted: {0}")]
    OwnedPathDrift(PathBuf),
    #[error("boot configuration has {0} active HyperPixel declarations; expected at most one")]
    AmbiguousDisplay(usize),
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("command failed: {program} exited with {status}")]
    CommandFailed { program: String, status: String },
    #[error("failed to update boot configuration: {0}")]
    Io(#[from] io::Error),
    #[error("failed to persist boot configuration: {0}")]
    Persist(#[from] tempfile::PersistError),
}

const MAX_INSTALLER_STATE_BYTES: usize = 64 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallerStateDocument {
    schema_version: u32,
    hardware: InstallerHardwareIdentity,
    application: Option<InstallerArtifactIdentity>,
    driver: Option<InstallerArtifactIdentity>,
    owned_files: Vec<InstallerOwnedFile>,
    last_verified_phase: InstallerPhase,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallerHardwareIdentity {
    model: String,
    serial: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallerArtifactIdentity {
    version: String,
    source_commit: String,
    sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallerOwnedFile {
    target_path: String,
    sha256: String,
}

#[derive(Clone, Copy, Deserialize, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum InstallerPhase {
    Discovered,
    PreflightPassed,
    ApplicationAcquired,
    DriverReady,
    TrybootStaged,
    TrybootVerified,
    DriverAccepted,
    ApplicationInstalled,
    HostnameChanged,
    FinalRebooted,
    FinalVerified,
    Complete,
}

pub fn write_installer_state_json(path: &Path, contents: &[u8]) -> Result<(), InstallError> {
    let state = parse_installer_state(contents)?;
    let serialized = serde_json::to_vec(&state).map_err(|_| InstallError::InvalidInstallerState)?;
    let parent_path = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.into()))?;
    let parent = open_private_installer_directory(parent_path, true)?
        .ok_or_else(|| InstallError::MissingParent(parent_path.into()))?;
    let state_name = path
        .file_name()
        .ok_or_else(|| InstallError::UnsafeFileType(path.into()))?;
    reject_unsafe_installer_state_at(&parent, state_name)?;

    static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let temporary_name = format!(
        ".state.json.{}.{}",
        std::process::id(),
        TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let temporary_fd = openat(
        &parent,
        temporary_name.as_str(),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(nix_io)?;
    let mut temporary = File::from(temporary_fd);
    temporary.set_permissions(fs::Permissions::from_mode(0o600))?;
    let write_result = (|| {
        temporary.write_all(&serialized)?;
        temporary.flush()?;
        temporary.sync_all()?;
        renameat(&parent, temporary_name.as_str(), &parent, state_name).map_err(nix_io)?;
        parent.sync_all()
    })();
    if write_result.is_err() {
        let _ = nix::unistd::unlinkat(
            &parent,
            temporary_name.as_str(),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        );
    }
    write_result?;
    parent.sync_all()?;
    Ok(())
}

fn open_directory_at(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(nix_io)
}

fn open_private_installer_directory(
    path: &Path,
    create: bool,
) -> Result<Option<File>, InstallError> {
    if !path.is_absolute() {
        return Err(InstallError::UnsafeFileType(path.into()));
    }
    let mut current = {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        options.open("/")?
    };
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_owned()),
            Component::RootDir => None,
            _ => Some(std::ffi::OsString::new()),
        })
        .collect::<Vec<_>>();
    if components.iter().any(|component| component.is_empty()) {
        return Err(InstallError::UnsafeFileType(path.into()));
    }
    for (index, component) in components.iter().enumerate() {
        let is_private_directory = index + 1 == components.len();
        let parent_owner = current.metadata()?.uid();
        match open_directory_at(&current, component) {
            Ok(next) => current = next,
            Err(error)
                if create && is_private_directory && error.kind() == io::ErrorKind::NotFound =>
            {
                mkdirat(
                    &current,
                    component.as_os_str(),
                    Mode::from_bits_truncate(0o700),
                )
                .map_err(nix_io)?;
                current.sync_all()?;
                current = open_directory_at(&current, component)?;
            }
            Err(error)
                if !create && is_private_directory && error.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        }
        if is_private_directory {
            let metadata = current.metadata()?;
            if !metadata.is_dir()
                || metadata.permissions().mode() & 0o777 != 0o700
                || metadata.uid() != parent_owner
            {
                return Err(InstallError::UnsafeFileType(path.into()));
            }
        }
    }
    Ok(Some(current))
}

pub fn read_installer_state_json(path: &Path) -> Result<String, InstallError> {
    let parent_path = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.into()))?;
    let parent = open_private_installer_directory(parent_path, false)?
        .ok_or_else(|| InstallError::MissingParent(parent_path.into()))?;
    let state_name = path
        .file_name()
        .ok_or_else(|| InstallError::UnsafeFileType(path.into()))?;
    reject_unsafe_installer_state_at(&parent, state_name)?;
    let state_fd = openat(
        &parent,
        state_name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(nix_io)?;
    let mut file = File::from(state_fd);
    if !file.metadata()?.is_file() {
        return Err(InstallError::UnsafeFileType(path.into()));
    }
    let mut contents = Vec::new();
    Read::by_ref(&mut file)
        .take((MAX_INSTALLER_STATE_BYTES + 1) as u64)
        .read_to_end(&mut contents)?;
    let state = parse_installer_state(&contents)?;
    serde_json::to_string(&state).map_err(|_| InstallError::InvalidInstallerState)
}

pub fn read_optional_installer_state_json(path: &Path) -> Result<String, InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.into()))?;
    if open_private_installer_directory(parent, false)?.is_none() {
        return Ok("null".into());
    }
    match read_installer_state_json(path) {
        Err(InstallError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            Ok("null".into())
        }
        result => result,
    }
}

fn parse_installer_state(contents: &[u8]) -> Result<InstallerStateDocument, InstallError> {
    if contents.len() > MAX_INSTALLER_STATE_BYTES {
        return Err(InstallError::InvalidInstallerState);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(contents);
    let state = InstallerStateDocument::deserialize(&mut deserializer)
        .map_err(|_| InstallError::InvalidInstallerState)?;
    deserializer
        .end()
        .map_err(|_| InstallError::InvalidInstallerState)?;
    if state.schema_version != 1
        || !valid_installer_hardware(&state.hardware)
        || state
            .application
            .as_ref()
            .is_some_and(|artifact| !valid_installer_artifact(artifact))
        || state
            .driver
            .as_ref()
            .is_some_and(|artifact| !valid_installer_artifact(artifact))
        || state.owned_files.len() > 1024
        || state
            .owned_files
            .iter()
            .any(|file| !valid_installer_owned_file(file))
        || (state.last_verified_phase >= InstallerPhase::ApplicationAcquired)
            != state.application.is_some()
        || (state.last_verified_phase >= InstallerPhase::DriverReady) != state.driver.is_some()
        || (state.last_verified_phase >= InstallerPhase::ApplicationInstalled)
            != !state.owned_files.is_empty()
    {
        return Err(InstallError::InvalidInstallerState);
    }
    Ok(state)
}

fn valid_installer_hardware(hardware: &InstallerHardwareIdentity) -> bool {
    (hardware.model == "Raspberry Pi Zero 2 W"
        || hardware
            .model
            .strip_prefix("Raspberry Pi Zero 2 W Rev ")
            .is_some_and(|revision| {
                !revision.is_empty()
                    && revision
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'.')
            }))
        && is_lower_hex(&hardware.serial, 16)
}

fn valid_installer_artifact(artifact: &InstallerArtifactIdentity) -> bool {
    semver::Version::parse(&artifact.version).is_ok()
        && is_lower_hex(&artifact.source_commit, 40)
        && is_lower_hex(&artifact.sha256, 64)
}

fn valid_installer_owned_file(file: &InstallerOwnedFile) -> bool {
    file.target_path.starts_with('/')
        && file.target_path != "/"
        && !file.target_path.contains("//")
        && !file
            .target_path
            .split('/')
            .any(|part| part == "." || part == "..")
        && !file.target_path.bytes().any(|byte| byte.is_ascii_control())
        && is_lower_hex(&file.sha256, 64)
}

fn reject_unsafe_installer_state_at(
    parent: &File,
    name: &std::ffi::OsStr,
) -> Result<(), InstallError> {
    match openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => {
            if File::from(file).metadata()?.is_file() {
                Ok(())
            } else {
                Err(InstallError::UnsafeFileType(PathBuf::from(name)))
            }
        }
        Err(nix::errno::Errno::ENOENT) => Ok(()),
        Err(error) => Err(nix_io(error).into()),
    }
}

fn nix_io(error: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub root: PathBuf,
    pub boot_config: PathBuf,
    pub artifact: PathBuf,
    pub checksum_file: PathBuf,
    pub revision_file: PathBuf,
    pub reboot: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallResult {
    pub files_changed: bool,
    pub boot_config_changed: bool,
    pub reboot_required: bool,
    pub revision: String,
    pub sha256: String,
    pub owned_files: Vec<InstalledFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledFile {
    pub target_path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleStateDocument {
    schema_version: u32,
    hardware: InstallerHardwareIdentity,
    accepted: Vec<LifecycleAcceptedPair>,
    transaction: Option<LifecycleTransactionDocument>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleAcceptedPair {
    pair: LifecycleReleasePair,
    sequence: u64,
    owned_files: Vec<InstalledFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleReleasePair {
    application: InstallerArtifactIdentity,
    driver: InstallerArtifactIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleTransactionDocument {
    prior: LifecycleAcceptedPair,
    candidate: LifecycleReleasePair,
    phase: LifecyclePhaseDocument,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LifecyclePhaseDocument {
    Prepared,
    ApplicationStaged,
    DriverStaged,
    TrybootVerified,
    DriverCommitted,
    NormalBootVerified,
    ApplicationActivated,
    ApplicationRestarted,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ApplicationOwnershipDocument {
    schema_version: u32,
    owned_files: Vec<InstalledFile>,
}

pub fn application_release_ownership_json(
    owned_files: &[InstalledFile],
) -> Result<String, InstallError> {
    validate_application_ownership(owned_files)?;
    serde_json::to_string(&ApplicationOwnershipDocument {
        schema_version: 1,
        owned_files: owned_files.to_vec(),
    })
    .map_err(|_| InstallError::InvalidOwnershipManifest)
}

pub fn parse_application_ownership_json(
    contents: &[u8],
) -> Result<Vec<InstalledFile>, InstallError> {
    if contents.len() > MAX_INSTALLER_STATE_BYTES {
        return Err(InstallError::InvalidOwnershipManifest);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(contents);
    let document = ApplicationOwnershipDocument::deserialize(&mut deserializer)
        .map_err(|_| InstallError::InvalidOwnershipManifest)?;
    deserializer
        .end()
        .map_err(|_| InstallError::InvalidOwnershipManifest)?;
    if document.schema_version != 1 {
        return Err(InstallError::InvalidOwnershipManifest);
    }
    validate_application_ownership(&document.owned_files)?;
    Ok(document.owned_files)
}

fn validate_application_ownership(owned_files: &[InstalledFile]) -> Result<(), InstallError> {
    if owned_files.is_empty()
        || owned_files.len() > 64
        || owned_files.iter().enumerate().any(|(index, file)| {
            !valid_installer_owned_file(&InstallerOwnedFile {
                target_path: file.target_path.clone(),
                sha256: file.sha256.clone(),
            }) || owned_files[..index]
                .iter()
                .any(|prior| prior.target_path == file.target_path)
        })
    {
        return Err(InstallError::InvalidOwnershipManifest);
    }
    Ok(())
}

pub fn write_lifecycle_state_json(path: &Path, contents: &[u8]) -> Result<(), InstallError> {
    let state = parse_lifecycle_state(contents)?;
    let serialized = serde_json::to_vec(&state).map_err(|_| InstallError::InvalidInstallerState)?;
    let parent_path = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.into()))?;
    let parent = open_private_installer_directory(parent_path, true)?
        .ok_or_else(|| InstallError::MissingParent(parent_path.into()))?;
    let state_name = path
        .file_name()
        .ok_or_else(|| InstallError::UnsafeFileType(path.into()))?;
    reject_unsafe_installer_state_at(&parent, state_name)?;

    static LIFECYCLE_TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let temporary_name = format!(
        ".lifecycle.json.{}.{}",
        std::process::id(),
        LIFECYCLE_TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let temporary_fd = openat(
        &parent,
        temporary_name.as_str(),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(nix_io)?;
    let mut temporary = File::from(temporary_fd);
    temporary.write_all(&serialized)?;
    temporary.flush()?;
    temporary.sync_all()?;
    if let Err(error) = renameat(&parent, temporary_name.as_str(), &parent, state_name) {
        let _ = nix::unistd::unlinkat(
            &parent,
            temporary_name.as_str(),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        );
        return Err(nix_io(error).into());
    }
    parent.sync_all()?;
    Ok(())
}

pub fn read_lifecycle_state_json(path: &Path) -> Result<String, InstallError> {
    let contents = read_bounded_private_state(path)?;
    let state = parse_lifecycle_state(&contents)?;
    serde_json::to_string(&state).map_err(|_| InstallError::InvalidInstallerState)
}

pub fn read_optional_lifecycle_state_json(path: &Path) -> Result<String, InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.into()))?;
    if open_private_installer_directory(parent, false)?.is_none() {
        return Ok("null".into());
    }
    match read_lifecycle_state_json(path) {
        Err(InstallError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            Ok("null".into())
        }
        result => result,
    }
}

fn read_bounded_private_state(path: &Path) -> Result<Vec<u8>, InstallError> {
    let parent_path = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.into()))?;
    let parent = open_private_installer_directory(parent_path, false)?
        .ok_or_else(|| InstallError::MissingParent(parent_path.into()))?;
    let state_name = path
        .file_name()
        .ok_or_else(|| InstallError::UnsafeFileType(path.into()))?;
    reject_unsafe_installer_state_at(&parent, state_name)?;
    let state_fd = openat(
        &parent,
        state_name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(nix_io)?;
    let mut file = File::from(state_fd);
    let mut contents = Vec::new();
    Read::by_ref(&mut file)
        .take((MAX_INSTALLER_STATE_BYTES + 1) as u64)
        .read_to_end(&mut contents)?;
    Ok(contents)
}

fn parse_lifecycle_state(contents: &[u8]) -> Result<LifecycleStateDocument, InstallError> {
    if contents.len() > MAX_INSTALLER_STATE_BYTES {
        return Err(InstallError::InvalidInstallerState);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(contents);
    let state = LifecycleStateDocument::deserialize(&mut deserializer)
        .map_err(|_| InstallError::InvalidInstallerState)?;
    deserializer
        .end()
        .map_err(|_| InstallError::InvalidInstallerState)?;
    let accepted_valid = state.accepted.len() <= 3
        && state.accepted.iter().enumerate().all(|(index, accepted)| {
            accepted.sequence > 0
                && valid_installer_artifact(&accepted.pair.application)
                && valid_installer_artifact(&accepted.pair.driver)
                && validate_application_ownership(&accepted.owned_files).is_ok()
                && state.accepted[..index]
                    .iter()
                    .all(|prior| prior.sequence > accepted.sequence && prior.pair != accepted.pair)
        });
    let transaction_valid = state.transaction.as_ref().is_none_or(|transaction| {
        valid_installer_artifact(&transaction.candidate.application)
            && valid_installer_artifact(&transaction.candidate.driver)
            && state
                .accepted
                .first()
                .is_some_and(|current| current == &transaction.prior)
    });
    if state.schema_version != 1
        || !valid_installer_hardware(&state.hardware)
        || !accepted_valid
        || !transaction_valid
    {
        return Err(InstallError::InvalidInstallerState);
    }
    Ok(state)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationReleaseIdentity {
    pub version: String,
    pub revision: String,
    pub sha256: String,
}

#[derive(Serialize)]
struct MachineInstallResult<'a> {
    schema_version: u32,
    files_changed: bool,
    boot_config_changed: bool,
    reboot_required: bool,
    revision: &'a str,
    sha256: &'a str,
}

impl InstallResult {
    pub fn to_json(&self) -> Result<String, InstallMachineOutputError> {
        if !is_lower_hex(&self.revision, 40) || !is_lower_hex(&self.sha256, 64) {
            return Err(InstallMachineOutputError::InvalidIdentity);
        }
        serde_json::to_string(&MachineInstallResult {
            schema_version: 1,
            files_changed: self.files_changed,
            boot_config_changed: self.boot_config_changed,
            reboot_required: self.reboot_required,
            revision: &self.revision,
            sha256: &self.sha256,
        })
        .map_err(InstallMachineOutputError::Serialize)
    }
}

#[derive(Serialize)]
struct MachineInstallOwnership {
    schema_version: u32,
    owned_files: Vec<InstalledFile>,
}

pub fn installer_ownership_json(root: &Path) -> Result<String, InstallError> {
    let owned_files = [
        INSTALL_BINARY,
        INSTALL_REVISION,
        INSTALL_CHECKSUM,
        INSTALL_SERVICE,
    ]
    .into_iter()
    .map(|relative| {
        let path = install_path(root, relative);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(InstallError::UnsafeFileType(path));
        }
        Ok(InstalledFile {
            target_path: format!("/{relative}"),
            sha256: format!("{:x}", Sha256::digest(fs::read(&path)?)),
        })
    })
    .collect::<Result<Vec<_>, InstallError>>()?;
    serde_json::to_string(&MachineInstallOwnership {
        schema_version: 1,
        owned_files,
    })
    .map_err(|error| InstallError::Io(io::Error::other(error)))
}

pub fn activate_application_release(
    root: &Path,
    artifact: &Path,
    identity: &ApplicationReleaseIdentity,
    current_owned_files: &[InstalledFile],
) -> Result<Vec<InstalledFile>, InstallError> {
    let root = canonical_installation_root(root)?;
    validate_application_release_identity(identity)?;
    verify_application_switch_precondition(&root, current_owned_files, identity)?;
    require_single_link_regular(artifact, None)?;
    let artifact_bytes = fs::read(artifact)?;
    validate_aarch64_elf(&artifact_bytes)?;
    if format!("{:x}", Sha256::digest(&artifact_bytes)) != identity.sha256 {
        return Err(InstallError::ChecksumMismatch);
    }

    for relative in [
        Path::new("opt/planeradar/releases"),
        Path::new("opt/planeradar/releases")
            .join(&identity.version)
            .as_path(),
    ] {
        ensure_install_directory(&root, relative, 0o755, true)?;
    }
    let digest_relative = Path::new("opt/planeradar/releases")
        .join(&identity.version)
        .join(&identity.sha256);
    ensure_install_directory(&root, &digest_relative, 0o755, true)?;
    let release_relative = digest_relative.join("planeradar");
    let release_path = install_path(&root, &release_relative);
    durable_atomic_write_bytes(&release_path, &artifact_bytes, 0o755)?;
    require_single_link_regular(&release_path, Some(&root))?;

    durable_atomic_write_bytes(&install_path(&root, INSTALL_BINARY), &artifact_bytes, 0o755)?;
    let revision = format!("{}\n", identity.revision);
    durable_atomic_write_bytes(
        &install_path(&root, INSTALL_REVISION),
        revision.as_bytes(),
        0o644,
    )?;
    let checksum = format!("{}  planeradar\n", identity.sha256);
    durable_atomic_write_bytes(
        &install_path(&root, INSTALL_CHECKSUM),
        checksum.as_bytes(),
        0o644,
    )?;

    let mut owned = current_owned_files
        .iter()
        .filter(|file| {
            !matches!(
                file.target_path.as_str(),
                "/opt/planeradar/bin/planeradar"
                    | "/opt/planeradar/REVISION"
                    | "/opt/planeradar/SHA256"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    owned.splice(
        0..0,
        [
            InstalledFile {
                target_path: "/opt/planeradar/bin/planeradar".into(),
                sha256: identity.sha256.clone(),
            },
            InstalledFile {
                target_path: "/opt/planeradar/REVISION".into(),
                sha256: format!("{:x}", Sha256::digest(revision.as_bytes())),
            },
            InstalledFile {
                target_path: "/opt/planeradar/SHA256".into(),
                sha256: format!("{:x}", Sha256::digest(checksum.as_bytes())),
            },
        ],
    );
    owned.push(InstalledFile {
        target_path: format!("/{}", release_relative.display()),
        sha256: identity.sha256.clone(),
    });
    Ok(owned)
}

pub fn uninstall_owned_installation(
    root: &Path,
    owned_files: &[InstalledFile],
    purge_settings: bool,
    commands: &dyn CommandRunner,
) -> Result<(), InstallError> {
    let root = canonical_installation_root(root)?;
    if purge_settings
        && !owned_files
            .iter()
            .any(|file| file.target_path == "/var/lib/planeradar/settings.json")
    {
        return Err(InstallError::SettingsNotOwned);
    }
    verify_owned_manifest(&root, owned_files, purge_settings)?;
    commands.run("systemctl", &["disable", "--now", "planeradar.service"])?;
    for file in owned_files {
        if !purge_settings && file.target_path == "/var/lib/planeradar/settings.json" {
            continue;
        }
        let path = owned_target_path(&root, &file.target_path)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                fs::remove_file(&path)?;
                if let Some(parent) = path.parent() {
                    File::open(parent)?.sync_all()?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    commands.run("systemctl", &["daemon-reload"])?;
    Ok(())
}

fn canonical_installation_root(root: &Path) -> Result<PathBuf, InstallError> {
    if !root.is_absolute() {
        return Err(InstallError::InvalidRoot(root.to_owned()));
    }
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(InstallError::InvalidRoot(root.to_owned()));
    }
    fs::canonicalize(root).map_err(InstallError::Io)
}

fn validate_application_release_identity(
    identity: &ApplicationReleaseIdentity,
) -> Result<(), InstallError> {
    if semver::Version::parse(&identity.version)
        .ok()
        .is_none_or(|version| version.to_string() != identity.version)
        || !is_lower_hex(&identity.revision, 40)
        || !is_lower_hex(&identity.sha256, 64)
    {
        return Err(InstallError::InvalidReleaseIdentity);
    }
    Ok(())
}

fn verify_application_switch_precondition(
    root: &Path,
    current_owned_files: &[InstalledFile],
    identity: &ApplicationReleaseIdentity,
) -> Result<(), InstallError> {
    let drift = match verify_owned_manifest(root, current_owned_files, false) {
        Ok(()) => return Ok(()),
        Err(error @ InstallError::OwnedPathDrift(_)) => error,
        Err(error) => return Err(error),
    };
    if transaction_proves_interrupted_application_switch(root, current_owned_files, identity) {
        Ok(())
    } else {
        Err(drift)
    }
}

fn transaction_proves_interrupted_application_switch(
    root: &Path,
    current_owned_files: &[InstalledFile],
    identity: &ApplicationReleaseIdentity,
) -> bool {
    let state_path = install_path(root, LIFECYCLE_STATE);
    let Ok(contents) = read_bounded_private_state(&state_path) else {
        return false;
    };
    let Ok(state) = parse_lifecycle_state(&contents) else {
        return false;
    };
    let Some(transaction) = state.transaction else {
        return false;
    };
    if transaction.prior.owned_files != current_owned_files
        || (!application_identity_matches(&transaction.prior.pair.application, identity)
            && !application_identity_matches(&transaction.candidate.application, identity))
    {
        return false;
    }

    current_owned_files.iter().all(|file| {
        let Ok(path) = owned_target_path(root, &file.target_path) else {
            return false;
        };
        if require_single_link_regular(&path, Some(root)).is_err() {
            return false;
        }
        let Ok(bytes) = fs::read(&path) else {
            return false;
        };
        let digest = format!("{:x}", Sha256::digest(&bytes));
        digest == file.sha256
            || interrupted_candidate_digest(&file.target_path, &transaction.candidate.application)
                .is_some_and(|candidate| digest == candidate)
    })
}

fn application_identity_matches(
    artifact: &InstallerArtifactIdentity,
    identity: &ApplicationReleaseIdentity,
) -> bool {
    artifact.version == identity.version
        && artifact.source_commit == identity.revision
        && artifact.sha256 == identity.sha256
}

fn interrupted_candidate_digest(
    target_path: &str,
    candidate: &InstallerArtifactIdentity,
) -> Option<String> {
    match target_path {
        "/opt/planeradar/bin/planeradar" => Some(candidate.sha256.clone()),
        "/opt/planeradar/REVISION" => Some(format!(
            "{:x}",
            Sha256::digest(format!("{}\n", candidate.source_commit))
        )),
        "/opt/planeradar/SHA256" => Some(format!(
            "{:x}",
            Sha256::digest(format!("{}  planeradar\n", candidate.sha256))
        )),
        _ => None,
    }
}

fn verify_owned_manifest(
    root: &Path,
    owned_files: &[InstalledFile],
    include_settings: bool,
) -> Result<(), InstallError> {
    if owned_files.is_empty() || owned_files.len() > 64 {
        return Err(InstallError::InvalidOwnershipManifest);
    }
    for (index, file) in owned_files.iter().enumerate() {
        if owned_files[..index]
            .iter()
            .any(|prior| prior.target_path == file.target_path)
            || !is_lower_hex(&file.sha256, 64)
            || !allowed_owned_path(&file.target_path, include_settings)
        {
            return Err(InstallError::InvalidOwnershipManifest);
        }
        let path = owned_target_path(root, &file.target_path)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                require_single_link_regular(&path, Some(root))?;
                let digest = format!("{:x}", Sha256::digest(fs::read(&path)?));
                if digest != file.sha256 {
                    return Err(InstallError::OwnedPathDrift(path));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn allowed_owned_path(path: &str, include_settings: bool) -> bool {
    if matches!(
        path,
        "/opt/planeradar/bin/planeradar"
            | "/opt/planeradar/REVISION"
            | "/opt/planeradar/SHA256"
            | "/etc/systemd/system/planeradar.service"
    ) {
        return true;
    }
    if include_settings && path == "/var/lib/planeradar/settings.json" {
        return true;
    }
    let Some(release) = path.strip_prefix("/opt/planeradar/releases/") else {
        return false;
    };
    let mut components = release.split('/');
    let (Some(version), Some(digest), Some("planeradar"), None) = (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    ) else {
        return false;
    };
    semver::Version::parse(version).is_ok_and(|parsed| parsed.to_string() == version)
        && is_lower_hex(digest, 64)
}

fn owned_target_path(root: &Path, target_path: &str) -> Result<PathBuf, InstallError> {
    if !target_path.starts_with('/')
        || target_path == "/"
        || target_path.contains("//")
        || target_path
            .split('/')
            .any(|component| component == "." || component == "..")
        || target_path.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(InstallError::InvalidOwnershipManifest);
    }
    let relative = target_path.trim_start_matches('/');
    let mut current = root.to_owned();
    let components = Path::new(relative).components().collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(component) = component else {
            return Err(InstallError::InvalidOwnershipManifest);
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            }
            Ok(_) => return Err(InstallError::OwnedPathDrift(current)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(root.join(relative))
}

fn require_single_link_regular(path: &Path, root: Option<&Path>) -> Result<(), InstallError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() || metadata.nlink() != 1
    {
        return Err(InstallError::OwnedPathDrift(path.to_owned()));
    }
    if let Some(root) = root {
        let root_metadata = fs::symlink_metadata(root)?;
        if metadata.uid() != root_metadata.uid() || metadata.gid() != root_metadata.gid() {
            return Err(InstallError::OwnedPathDrift(path.to_owned()));
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum InstallMachineOutputError {
    #[error("install result artifact identity is invalid")]
    InvalidIdentity,
    #[error("install result JSON serialization failed")]
    Serialize(#[source] serde_json::Error),
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> Result<(), InstallError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<(), InstallError> {
        let status = Command::new(program).args(args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(InstallError::CommandFailed {
                program: program.to_owned(),
                status: status.to_string(),
            })
        }
    }
}

pub struct Installer<'a> {
    commands: &'a dyn CommandRunner,
}

impl<'a> Installer<'a> {
    pub fn new(commands: &'a dyn CommandRunner) -> Self {
        Self { commands }
    }

    pub fn install(&self, options: &InstallOptions) -> Result<InstallResult, InstallError> {
        let ValidatedInstallation {
            root,
            artifact,
            checksum,
            revision,
            revision_identity,
            sha256_identity,
        } = validate_installation(options)?;

        self.commands.run("apt-get", &["update"])?;
        let mut install_args = vec!["install", "--yes", "--no-install-recommends"];
        install_args.extend_from_slice(RUNTIME_PACKAGES);
        self.commands.run("apt-get", &install_args)?;
        let kernel_reboot_required = installed_kernel_reboot_required(&root)?;
        self.ensure_service_account(&root)?;

        let mut files_changed = false;
        files_changed |= ensure_install_directory(&root, Path::new("opt"), 0o755, false)?;
        files_changed |= ensure_install_directory(&root, Path::new("opt/planeradar"), 0o755, true)?;
        files_changed |=
            ensure_install_directory(&root, Path::new("opt/planeradar/bin"), 0o755, true)?;
        files_changed |= ensure_install_directory(&root, Path::new("var"), 0o755, false)?;
        files_changed |= ensure_install_directory(&root, Path::new("var/lib"), 0o755, false)?;
        files_changed |= ensure_install_directory(&root, Path::new(INSTALL_STATE), 0o750, true)?;
        ensure_existing_directory(&root, Path::new("etc"))?;
        files_changed |= ensure_install_directory(&root, Path::new("etc/systemd"), 0o755, false)?;
        files_changed |=
            ensure_install_directory(&root, Path::new("etc/systemd/system"), 0o755, false)?;

        files_changed |=
            durable_atomic_write_bytes(&install_path(&root, INSTALL_BINARY), &artifact, 0o755)?;
        files_changed |=
            durable_atomic_write_bytes(&install_path(&root, INSTALL_REVISION), &revision, 0o644)?;
        files_changed |=
            durable_atomic_write_bytes(&install_path(&root, INSTALL_CHECKSUM), &checksum, 0o644)?;
        let service_changed = durable_atomic_write_bytes(
            &install_path(&root, INSTALL_SERVICE),
            PLANERADAR_SERVICE.as_bytes(),
            0o644,
        )?;
        files_changed |= service_changed;

        let backup_existed = backup_path(&options.boot_config).exists();
        let boot_config_changed = ensure_calibrated_boot_config(&options.boot_config)?;
        files_changed |= !backup_existed && backup_path(&options.boot_config).exists();

        let state_path = install_path(&root, INSTALL_STATE);
        let state = path_as_str(&state_path)?;
        self.commands
            .run("chown", &["--recursive", "planeradar:planeradar", state])?;
        if service_changed {
            self.commands.run("systemctl", &["daemon-reload"])?;
        }
        self.commands
            .run("systemctl", &["enable", "planeradar.service"])?;
        if files_changed {
            self.commands
                .run("systemctl", &["restart", "planeradar.service"])?;
        } else {
            self.commands
                .run("systemctl", &["start", "planeradar.service"])?;
        }

        let reboot_required = boot_config_changed || kernel_reboot_required;
        if reboot_required && options.reboot {
            self.commands.run("systemctl", &["reboot"])?;
        }

        let owned_files = [
            (INSTALL_BINARY, artifact.as_slice()),
            (INSTALL_REVISION, revision.as_slice()),
            (INSTALL_CHECKSUM, checksum.as_slice()),
            (INSTALL_SERVICE, PLANERADAR_SERVICE.as_bytes()),
        ]
        .into_iter()
        .map(|(path, contents)| InstalledFile {
            target_path: format!("/{path}"),
            sha256: format!("{:x}", Sha256::digest(contents)),
        })
        .collect();

        Ok(InstallResult {
            files_changed,
            boot_config_changed,
            reboot_required,
            revision: revision_identity,
            sha256: sha256_identity,
            owned_files,
        })
    }

    fn ensure_service_account(&self, root: &Path) -> Result<(), InstallError> {
        let passwd = fs::read_to_string(install_path(root, "etc/passwd"))?;
        if !passwd
            .lines()
            .any(|line| line.split(':').next() == Some("planeradar"))
        {
            self.commands.run(
                "useradd",
                &[
                    "--system",
                    "--user-group",
                    "--home-dir",
                    "/var/lib/planeradar",
                    "--no-create-home",
                    "--shell",
                    "/usr/sbin/nologin",
                    "planeradar",
                ],
            )?;
        }
        self.commands.run(
            "usermod",
            &["--append", "--groups", "video,render,input", "planeradar"],
        )
    }
}

struct ValidatedInstallation {
    root: PathBuf,
    artifact: Vec<u8>,
    checksum: Vec<u8>,
    revision: Vec<u8>,
    revision_identity: String,
    sha256_identity: String,
}

fn validate_installation(options: &InstallOptions) -> Result<ValidatedInstallation, InstallError> {
    if !options.root.is_absolute() {
        return Err(InstallError::InvalidRoot(options.root.clone()));
    }
    let root_metadata = fs::symlink_metadata(&options.root)?;
    if !root_metadata.file_type().is_dir() {
        return Err(InstallError::InvalidRoot(options.root.clone()));
    }
    let root = fs::canonicalize(&options.root)?;

    if root == Path::new("/") && std::env::consts::ARCH != "aarch64" {
        return Err(InstallError::UnsupportedArchitecture);
    }

    let os_release = read_os_release(&root)?;
    let operating_system = parse_os_release(&os_release);
    let id = operating_system
        .iter()
        .find_map(|(key, value)| (key == "ID").then_some(value.as_str()))
        .unwrap_or_default();
    let version = operating_system
        .iter()
        .find_map(|(key, value)| (key == "VERSION_ID").then_some(value.as_str()))
        .unwrap_or_default();
    if !matches!(id, "debian" | "raspbian") || version != "13" {
        return Err(InstallError::UnsupportedOperatingSystem(format!(
            "{id} {version}"
        )));
    }

    let model = read_regular_utf8(&install_path(&root, "proc/device-tree/model"))?;
    let model = model.trim_matches(['\0', '\r', '\n']);
    if !model.starts_with("Raspberry Pi Zero 2 W") {
        return Err(InstallError::UnsupportedHardware(model.to_owned()));
    }

    for input in [
        &options.artifact,
        &options.checksum_file,
        &options.revision_file,
        &options.boot_config,
    ] {
        require_regular_file(input)?;
    }
    let boot_parent = fs::canonicalize(
        options
            .boot_config
            .parent()
            .ok_or_else(|| InstallError::MissingParent(options.boot_config.clone()))?,
    )?;
    if !boot_parent.starts_with(&root) {
        return Err(InstallError::InvalidRoot(options.boot_config.clone()));
    }
    let boot_source = fs::read_to_string(&options.boot_config)?;
    validate_boot_config(&boot_source)?;
    validate_display_preflight(&boot_source)?;
    let backup = backup_path(&options.boot_config);
    if let Ok(metadata) = fs::symlink_metadata(&backup)
        && !metadata.file_type().is_file()
    {
        return Err(InstallError::UnsafeFileType(backup));
    }

    let artifact = fs::read(&options.artifact)?;
    validate_aarch64_elf(&artifact)?;
    let checksum = fs::read(&options.checksum_file)?;
    let checksum_text = std::str::from_utf8(&checksum).map_err(|_| {
        InstallError::InvalidArtifact(format!("{} is not UTF-8", options.checksum_file.display()))
    })?;
    let expected = parse_checksum_sidecar(checksum_text, &options.artifact)?;
    if format!("{:x}", Sha256::digest(&artifact)) != expected {
        return Err(InstallError::ChecksumMismatch);
    }
    let revision = fs::read(&options.revision_file)?;
    let expected_revision = format!("{}\n", env!("PLANERADAR_REVISION"));
    if revision != expected_revision.as_bytes() {
        return Err(InstallError::RevisionMismatch(
            String::from_utf8_lossy(&revision).trim().to_owned(),
        ));
    }

    Ok(ValidatedInstallation {
        root,
        artifact,
        checksum,
        revision,
        revision_identity: env!("PLANERADAR_REVISION").to_owned(),
        sha256_identity: expected,
    })
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn installed_kernel_reboot_required(root: &Path) -> Result<bool, InstallError> {
    let running_path = install_path(root, "proc/sys/kernel/osrelease");
    let running = match fs::read_to_string(&running_path) {
        Ok(value) => value.trim().to_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !valid_kernel_release(&running) {
        return Ok(false);
    }
    if header_release(root, &running).as_deref() == Some(running.as_str()) {
        return Ok(false);
    }

    let modules = install_path(root, "lib/modules");
    let entries = match fs::read_dir(&modules) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut safe_alternates = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(release) = entry.file_name().to_str().map(str::to_owned) else {
            return Ok(false);
        };
        if release == running || !valid_kernel_release(&release) {
            continue;
        }
        if header_release(root, &release).as_deref() == Some(release.as_str()) {
            safe_alternates.push(release);
        }
    }
    Ok(safe_alternates.len() == 1 && boot_selects_kernel(root, &safe_alternates[0]))
}

fn boot_selects_kernel(root: &Path, release: &str) -> bool {
    let kernel8 = install_path(root, "boot/firmware/kernel8.img");
    let Some(kernel8_sha) = owned_regular_sha256(root, &kernel8) else {
        return false;
    };
    let boot_config = install_path(root, "boot/firmware/config.txt");
    let Ok(config) = read_owned_regular(root, &boot_config) else {
        return false;
    };
    if config.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        line.split_once('=')
            .is_some_and(|(key, value)| key.trim() == "kernel" && value.trim() != "kernel8.img")
    }) {
        return false;
    }

    let boot = install_path(root, "boot");
    let Ok(entries) = fs::read_dir(boot) else {
        return false;
    };
    let mut selected = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return false;
        };
        let Some(candidate) = name.strip_prefix("vmlinuz-") else {
            continue;
        };
        if !valid_kernel_release(candidate) {
            continue;
        }
        if owned_regular_sha256(root, &entry.path()).as_ref() == Some(&kernel8_sha) {
            selected.push(candidate.to_owned());
        }
    }
    selected.as_slice() == [release]
}

fn read_owned_regular(root: &Path, path: &Path) -> io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    let root_metadata = fs::symlink_metadata(root)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != root_metadata.uid()
        || metadata.gid() != root_metadata.gid()
    {
        return Err(io::Error::other("file ownership or type is unsafe"));
    }
    fs::read_to_string(path)
}

fn owned_regular_sha256(root: &Path, path: &Path) -> Option<[u8; 32]> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let root_metadata = fs::symlink_metadata(root).ok()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != root_metadata.uid()
        || metadata.gid() != root_metadata.gid()
    {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(hasher.finalize().into())
}

fn header_release(root: &Path, kernel_release: &str) -> Option<String> {
    let path = install_path(root, "lib/modules")
        .join(kernel_release)
        .join("build/include/config/kernel.release");
    let canonical = fs::canonicalize(&path).ok()?;
    if !canonical.starts_with(root) || !fs::metadata(&canonical).ok()?.is_file() {
        return None;
    }
    let release = fs::read_to_string(canonical).ok()?;
    let release = release.trim();
    valid_kernel_release(release).then(|| release.to_owned())
}

fn valid_kernel_release(release: &str) -> bool {
    !release.is_empty()
        && release.len() <= 128
        && release
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-' | b'_'))
}

fn parse_os_release(input: &str) -> Vec<(String, String)> {
    input
        .lines()
        .filter_map(|line| {
            let (key, raw_value) = line.split_once('=')?;
            let value = raw_value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(raw_value);
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn validate_aarch64_elf(artifact: &[u8]) -> Result<(), InstallError> {
    let valid = artifact.len() >= 20
        && artifact.starts_with(b"\x7fELF")
        && artifact[4] == 2
        && artifact[5] == 1
        && artifact[18] == 0xb7
        && artifact[19] == 0;
    if valid {
        Ok(())
    } else {
        Err(InstallError::InvalidArtifact(
            "expected a little-endian ELF64 AArch64 executable".to_owned(),
        ))
    }
}

fn parse_checksum_sidecar(sidecar: &str, artifact: &Path) -> Result<String, InstallError> {
    let mut lines = sidecar.lines();
    let line = lines
        .next()
        .ok_or_else(|| InstallError::InvalidArtifact("checksum sidecar is empty".to_owned()))?;
    if lines.next().is_some() {
        return Err(InstallError::InvalidArtifact(
            "checksum sidecar must contain exactly one row".to_owned(),
        ));
    }
    let mut fields = line.split_whitespace();
    let checksum = fields.next().unwrap_or_default();
    let filename = fields.next().unwrap_or_default();
    if fields.next().is_some()
        || checksum.len() != 64
        || !checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || artifact.file_name().and_then(|name| name.to_str()) != Some(filename)
    {
        return Err(InstallError::InvalidArtifact(
            "checksum sidecar has an invalid digest or filename".to_owned(),
        ));
    }
    Ok(checksum.to_owned())
}

fn validate_display_preflight(source: &str) -> Result<(), InstallError> {
    let declarations = active_hyperpixel_declarations(source);
    if declarations.len() > 1 {
        return Err(InstallError::AmbiguousDisplay(declarations.len()));
    }
    if let Some(declaration) = declarations.first() {
        let selection = declaration
            .strip_prefix("dtoverlay=")
            .ok_or_else(|| InstallError::InvalidOverlayName((*declaration).to_owned()))?;
        let mut fields = selection.split(',');
        let overlay = fields.next().unwrap_or_default();
        if overlay != STOCK_HYPERPIXEL_DECLARATION.trim_start_matches("dtoverlay=") {
            validate_overlay_name(overlay)?;
        }
        let mut parameters = Vec::new();
        for parameter in fields {
            if !SUPPORTED_DISPLAY_PARAMETERS.contains(&parameter) {
                return Err(InstallError::InvalidDisplayParameter(parameter.to_owned()));
            }
            if parameters.contains(&parameter) {
                return Err(InstallError::DuplicateDisplayParameter(
                    parameter.to_owned(),
                ));
            }
            parameters.push(parameter);
        }
    }
    Ok(())
}

fn ensure_calibrated_boot_config(path: &Path) -> Result<bool, InstallError> {
    let editor = BootConfigEditor::acquire(path)?;
    let source = editor.read_source()?;
    validate_display_preflight(&source)?;
    if active_hyperpixel_declarations(&source).is_empty() {
        return editor.edit_from_source(&source, STOCK_HYPERPIXEL_DECLARATION);
    }

    let mode = regular_file_mode(path)?;
    preserve_backup(path, &source, mode)?;
    Ok(false)
}

fn active_hyperpixel_declarations(source: &str) -> Vec<&str> {
    source
        .lines()
        .map(str::trim)
        .filter(|line| is_install_hyperpixel_declaration(line))
        .collect()
}

fn is_install_hyperpixel_declaration(line: &str) -> bool {
    let Some(selection) = line.strip_prefix("dtoverlay=") else {
        return false;
    };
    let overlay = selection.split(',').next().unwrap_or_default();
    overlay == STOCK_HYPERPIXEL_DECLARATION.trim_start_matches("dtoverlay=")
        || overlay.starts_with(PLANERADAR_HYPERPIXEL_PREFIX)
}

fn require_regular_file(path: &Path) -> Result<(), InstallError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(InstallError::UnsafeFileType(path.to_owned()))
    }
}

fn read_regular_utf8(path: &Path) -> Result<String, InstallError> {
    require_regular_file(path)?;
    String::from_utf8(fs::read(path)?)
        .map_err(|_| InstallError::InvalidArtifact(format!("{} is not UTF-8", path.display())))
}

fn read_os_release(root: &Path) -> Result<String, InstallError> {
    let path = install_path(root, "etc/os-release");
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_file() {
        return read_regular_utf8(&path);
    }
    if metadata.file_type().is_symlink()
        && fs::read_link(&path)? == Path::new("../usr/lib/os-release")
    {
        return read_regular_utf8(&install_path(root, "usr/lib/os-release"));
    }
    Err(InstallError::UnsafeFileType(path))
}

fn install_path(root: &Path, relative: impl AsRef<Path>) -> PathBuf {
    root.join(relative)
}

fn ensure_existing_directory(root: &Path, relative: &Path) -> Result<(), InstallError> {
    let path = install_path(root, relative);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_dir() {
        Ok(())
    } else {
        Err(InstallError::UnsafeFileType(path))
    }
}

fn ensure_install_directory(
    root: &Path,
    relative: &Path,
    mode: u32,
    enforce_mode: bool,
) -> Result<bool, InstallError> {
    let path = install_path(root, relative);
    let mut changed = false;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            if enforce_mode && metadata.permissions().mode() & 0o777 != mode {
                fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
                changed = true;
            }
        }
        Ok(_) => return Err(InstallError::UnsafeFileType(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| InstallError::MissingParent(path.clone()))?;
            if !fs::symlink_metadata(parent)?.file_type().is_dir() {
                return Err(InstallError::UnsafeFileType(parent.to_owned()));
            }
            fs::create_dir(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
            changed = true;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(changed)
}

fn durable_atomic_write_bytes(
    path: &Path,
    contents: &[u8],
    mode: u32,
) -> Result<bool, InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.to_owned()))?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let bytes_match = fs::read(path)? == contents;
            let mode_matches = metadata.permissions().mode() & 0o777 == mode;
            if bytes_match && mode_matches {
                return Ok(false);
            }
            if bytes_match {
                fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
                File::open(path)?.sync_all()?;
                File::open(parent)?.sync_all()?;
                return Ok(true);
            }
        }
        Ok(_) => return Err(InstallError::UnsafeFileType(path.to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut temporary = tempfile::Builder::new()
        .prefix(".planeradar-install-")
        .tempfile_in(parent)?;
    temporary.write_all(contents)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(true)
}

fn path_as_str(path: &Path) -> Result<&str, InstallError> {
    path.to_str()
        .ok_or_else(|| InstallError::NonUtf8Path(path.to_owned()))
}

pub struct BootConfigEditor {
    path: PathBuf,
    _lock: File,
}

impl BootConfigEditor {
    pub fn acquire(path: &Path) -> Result<Self, InstallError> {
        let lock = open_lock_file(path)?;
        lock.lock()?;
        Ok(Self {
            path: path.to_owned(),
            _lock: lock,
        })
    }

    pub fn try_acquire(path: &Path) -> Result<Option<Self>, InstallError> {
        let lock = open_lock_file(path)?;
        match lock.try_lock() {
            Ok(()) => Ok(Some(Self {
                path: path.to_owned(),
                _lock: lock,
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    pub fn read_source(&self) -> Result<String, InstallError> {
        Ok(fs::read_to_string(&self.path)?)
    }

    pub fn edit_from_source(
        &self,
        approved_source: &str,
        declaration: &str,
    ) -> Result<bool, InstallError> {
        let (updated, changed) = ensure_overlay(approved_source, declaration);
        self.commit_from_source(approved_source, &updated, changed)
    }

    fn commit_from_source(
        &self,
        approved_source: &str,
        updated: &str,
        changed: bool,
    ) -> Result<bool, InstallError> {
        ensure_source_unchanged(&self.path, approved_source)?;
        if !changed {
            return Ok(false);
        }

        let mode = regular_file_mode(&self.path)?;
        preserve_backup(&self.path, approved_source, mode)?;
        durable_atomic_write(&self.path, updated, mode)
    }
}

pub fn select_hyperpixel_overlay(
    input: &str,
    selection: DisplaySelection<'_>,
) -> Result<(String, bool), InstallError> {
    let inserted = selection_lines(selection)?;
    let had_final_newline = input.ends_with('\n');
    let original_lines = split_lines_preserving_endings(input);
    let fallback_ending = original_lines
        .iter()
        .find_map(|line| (!line.ending.is_empty()).then(|| line.ending.clone()))
        .unwrap_or_else(|| "\n".to_owned());
    let mut remove_owned_parameters = false;
    let mut lines = Vec::with_capacity(original_lines.len() + inserted.len() + 1);

    for line in original_lines {
        let trimmed = line.body.trim();
        if is_hyperpixel_declaration(trimmed) {
            remove_owned_parameters = true;
            continue;
        }
        if remove_owned_parameters && is_supported_parameter_line(trimmed) {
            continue;
        }
        remove_owned_parameters = false;
        lines.push(line);
    }

    if let Some(last_all) = lines.iter().rposition(|line| line.body.trim() == "[all]") {
        let insertion = last_all + 1;
        let insertion_ending = if lines[last_all].ending.is_empty() {
            lines[last_all].ending = fallback_ending.clone();
            fallback_ending
        } else {
            lines[last_all].ending.clone()
        };
        let has_following_line = insertion < lines.len();
        for (offset, body) in inserted.into_iter().enumerate() {
            let is_last_inserted = offset + 1
                == match selection {
                    DisplaySelection::Stock => 1,
                    DisplaySelection::Candidate { parameters, .. } => parameters.len() + 1,
                };
            let ending = if has_following_line || had_final_newline || !is_last_inserted {
                insertion_ending.clone()
            } else {
                String::new()
            };
            lines.insert(insertion + offset, ConfigLine { body, ending });
        }
    } else {
        if let Some(last) = lines.last_mut()
            && last.ending.is_empty()
        {
            last.ending = fallback_ending.clone();
        }
        lines.push(ConfigLine {
            body: "[all]".to_owned(),
            ending: fallback_ending.clone(),
        });
        let inserted_count = inserted.len();
        for (offset, body) in inserted.into_iter().enumerate() {
            lines.push(ConfigLine {
                body,
                ending: if had_final_newline || offset + 1 < inserted_count {
                    fallback_ending.clone()
                } else {
                    String::new()
                },
            });
        }
    }

    let updated: String = lines
        .into_iter()
        .map(|line| line.body + &line.ending)
        .collect();
    validate_boot_config(&updated)?;
    Ok(if updated == input {
        (input.to_owned(), false)
    } else {
        (updated, true)
    })
}

pub fn validate_boot_config(input: &str) -> Result<(), InstallError> {
    for (index, line) in split_lines_preserving_endings(input).iter().enumerate() {
        let bytes = line.body.len();
        if bytes > MAX_BOOT_CONFIG_LINE_BYTES {
            return Err(InstallError::BootLineTooLong {
                line: index + 1,
                bytes,
            });
        }
    }
    Ok(())
}

pub fn stage_tryboot_config(
    boot_config: &Path,
    tryboot_config: &Path,
    selection: DisplaySelection<'_>,
) -> Result<bool, InstallError> {
    stage_tryboot_config_inner(boot_config, tryboot_config, None, selection)
}

pub fn stage_tryboot_config_if_source_matches(
    boot_config: &Path,
    tryboot_config: &Path,
    expected_boot_config_sha256: &str,
    selection: DisplaySelection<'_>,
) -> Result<bool, InstallError> {
    stage_tryboot_config_inner(
        boot_config,
        tryboot_config,
        Some(expected_boot_config_sha256),
        selection,
    )
}

fn stage_tryboot_config_inner(
    boot_config: &Path,
    tryboot_config: &Path,
    expected_boot_config_sha256: Option<&str>,
    selection: DisplaySelection<'_>,
) -> Result<bool, InstallError> {
    if normalized_destination(boot_config)? == normalized_destination(tryboot_config)? {
        return Err(InstallError::ConflictingConfigPath(
            tryboot_config.to_owned(),
        ));
    }
    let editor = BootConfigEditor::acquire(boot_config)?;
    let source = editor.read_source()?;
    if expected_boot_config_sha256
        .is_some_and(|expected| format!("{:x}", Sha256::digest(source.as_bytes())) != expected)
    {
        return Err(InstallError::SourceChanged(boot_config.to_owned()));
    }
    let (updated, _) = select_hyperpixel_overlay(&source, selection)?;
    ensure_source_unchanged(boot_config, &source)?;
    durable_atomic_write(tryboot_config, &updated, 0o644)
}

pub fn commit_display_config(
    boot_config: &Path,
    selection: DisplaySelection<'_>,
) -> Result<bool, InstallError> {
    let editor = BootConfigEditor::acquire(boot_config)?;
    let source = editor.read_source()?;
    let (updated, changed) = select_hyperpixel_overlay(&source, selection)?;
    editor.commit_from_source(&source, &updated, changed)
}

pub fn rollback_display_config(boot_config: &Path) -> Result<bool, InstallError> {
    commit_display_config(boot_config, DisplaySelection::Stock)
}

pub fn ensure_overlay(input: &str, declaration: &str) -> (String, bool) {
    let had_final_newline = input.ends_with('\n');
    let original_lines = split_lines_preserving_endings(input);
    let fallback_ending = original_lines
        .iter()
        .find_map(|line| (!line.ending.is_empty()).then(|| line.ending.clone()))
        .unwrap_or_else(|| "\n".to_owned());
    let mut lines: Vec<ConfigLine> = original_lines
        .into_iter()
        .filter(|line| line.body.trim() != declaration)
        .collect();

    if let Some(last_all) = lines.iter().rposition(|line| line.body.trim() == "[all]") {
        let insertion = last_all + 1;
        let insertion_ending = if lines[last_all].ending.is_empty() {
            lines[last_all].ending = fallback_ending.clone();
            fallback_ending
        } else {
            lines[last_all].ending.clone()
        };
        let ending = if insertion < lines.len() || had_final_newline {
            insertion_ending
        } else {
            String::new()
        };
        lines.insert(
            insertion,
            ConfigLine {
                body: declaration.to_owned(),
                ending,
            },
        );
    } else {
        if let Some(last) = lines.last_mut()
            && last.ending.is_empty()
        {
            last.ending = fallback_ending.clone();
        }
        lines.push(ConfigLine {
            body: "[all]".to_owned(),
            ending: fallback_ending.clone(),
        });
        lines.push(ConfigLine {
            body: declaration.to_owned(),
            ending: if had_final_newline {
                fallback_ending
            } else {
                String::new()
            },
        });
    }

    let updated = lines
        .into_iter()
        .map(|line| line.body + &line.ending)
        .collect();
    if updated == input {
        (input.to_owned(), false)
    } else {
        (updated, true)
    }
}

fn split_lines_preserving_endings(input: &str) -> Vec<ConfigLine> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (newline, _) in input.match_indices('\n') {
        let (body_end, ending) =
            if newline > start && input.as_bytes().get(newline - 1) == Some(&b'\r') {
                (newline - 1, "\r\n")
            } else {
                (newline, "\n")
            };
        lines.push(ConfigLine {
            body: input[start..body_end].to_owned(),
            ending: ending.to_owned(),
        });
        start = newline + 1;
    }
    if start < input.len() {
        lines.push(ConfigLine {
            body: input[start..].to_owned(),
            ending: String::new(),
        });
    }
    lines
}

fn selection_lines(selection: DisplaySelection<'_>) -> Result<Vec<String>, InstallError> {
    match selection {
        DisplaySelection::Stock => Ok(vec![STOCK_HYPERPIXEL_DECLARATION.to_owned()]),
        DisplaySelection::Candidate {
            overlay,
            parameters,
        } => {
            validate_overlay_name(overlay)?;
            for (index, parameter) in parameters.iter().enumerate() {
                if !SUPPORTED_DISPLAY_PARAMETERS.contains(parameter) {
                    return Err(InstallError::InvalidDisplayParameter(
                        (*parameter).to_owned(),
                    ));
                }
                if parameters[..index].contains(parameter) {
                    return Err(InstallError::DuplicateDisplayParameter(
                        (*parameter).to_owned(),
                    ));
                }
            }

            let mut lines = Vec::with_capacity(parameters.len() + 1);
            lines.push(format!("dtoverlay={overlay}"));
            lines.extend(
                parameters
                    .iter()
                    .map(|parameter| format!("dtparam={parameter}")),
            );
            Ok(lines)
        }
    }
}

fn validate_overlay_name(overlay: &str) -> Result<(), InstallError> {
    let Some(revision) = overlay.strip_prefix(PLANERADAR_HYPERPIXEL_PREFIX) else {
        return Err(InstallError::InvalidOverlayName(overlay.to_owned()));
    };
    if revision.len() != 12
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(InstallError::InvalidOverlayName(overlay.to_owned()));
    }
    Ok(())
}

fn is_hyperpixel_declaration(trimmed: &str) -> bool {
    trimmed.starts_with(STOCK_HYPERPIXEL_DECLARATION)
        || trimmed.starts_with(&format!("dtoverlay={PLANERADAR_HYPERPIXEL_PREFIX}"))
}

fn is_supported_parameter_line(trimmed: &str) -> bool {
    trimmed
        .strip_prefix("dtparam=")
        .is_some_and(|parameter| SUPPORTED_DISPLAY_PARAMETERS.contains(&parameter))
}

pub fn edit_boot_config(path: &Path, declaration: &str) -> Result<bool, InstallError> {
    let editor = BootConfigEditor::acquire(path)?;
    let source = editor.read_source()?;
    editor.edit_from_source(&source, declaration)
}

pub fn edit_boot_config_from_source(
    path: &Path,
    approved_source: &str,
    declaration: &str,
) -> Result<bool, InstallError> {
    let editor = BootConfigEditor::acquire(path)?;
    editor.edit_from_source(approved_source, declaration)
}

fn ensure_source_unchanged(path: &Path, approved_source: &str) -> Result<(), InstallError> {
    if fs::read_to_string(path)? == approved_source {
        Ok(())
    } else {
        Err(InstallError::SourceChanged(path.to_owned()))
    }
}

fn durable_atomic_write(path: &Path, contents: &str, new_mode: u32) -> Result<bool, InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.to_owned()))?;
    let mode = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(InstallError::UnsafeFileType(path.to_owned()));
            }
            if fs::read(path)? == contents.as_bytes() {
                return Ok(false);
            }
            metadata.permissions().mode() & 0o7777
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => new_mode,
        Err(error) => return Err(error.into()),
    };
    let mut temporary = tempfile::Builder::new()
        .prefix(".planeradar-config-")
        .tempfile_in(parent)?;
    temporary.write_all(contents.as_bytes())?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(true)
}

fn preserve_backup(path: &Path, contents: &str, mode: u32) -> Result<(), InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.to_owned()))?;
    let backup = backup_path(path);
    match fs::symlink_metadata(&backup) {
        Ok(metadata) if metadata.file_type().is_file() => return Ok(()),
        Ok(_) => return Err(InstallError::UnsafeFileType(backup)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut temporary = tempfile::Builder::new()
        .prefix(".planeradar-backup-")
        .tempfile_in(parent)?;
    temporary.write_all(contents.as_bytes())?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&backup) {
        Ok(_) => {
            File::open(parent)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(&backup)?.file_type().is_file() {
                Ok(())
            } else {
                Err(InstallError::UnsafeFileType(backup))
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn regular_file_mode(path: &Path) -> Result<u32, InstallError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(InstallError::UnsafeFileType(path.to_owned()));
    }
    Ok(metadata.permissions().mode() & 0o7777)
}

fn normalized_destination(path: &Path) -> Result<PathBuf, InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.to_owned()))?;
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::MissingParent(path.to_owned()))?;
    Ok(fs::canonicalize(parent)?.join(name))
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .expect("boot configuration path has a file name")
        .to_os_string();
    name.push(".planeradar-backup");
    path.with_file_name(name)
}

fn open_lock_file(path: &Path) -> Result<File, InstallError> {
    let lock_path = lock_path(path)?;
    if let Ok(metadata) = fs::symlink_metadata(&lock_path)
        && !metadata.file_type().is_file()
    {
        return Err(InstallError::UnsafeFileType(lock_path));
    }
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?)
}

fn lock_path(path: &Path) -> Result<PathBuf, InstallError> {
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::MissingParent(path.to_owned()))?;
    let mut name = name.to_os_string();
    name.push(".planeradar-lock");
    Ok(path.with_file_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_rejects_a_path_without_a_file_name() {
        assert!(matches!(
            lock_path(Path::new("/")),
            Err(InstallError::MissingParent(path)) if path == Path::new("/")
        ));
    }
}
