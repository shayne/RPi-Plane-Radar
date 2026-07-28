use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use serde::de::{DeserializeOwned, Error as DeError};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::target::TargetIdentity;

pub const STATE_SCHEMA_VERSION: u32 = 1;
pub const TARGET_STATE_PATH: &str = "/var/lib/planeradar/installer/state.json";
pub const TARGET_STATE_OWNER: &str = "root";
pub const TARGET_STATE_FILE_MODE: u32 = 0o600;

/// The non-negotiable persistence rules for the target-side installer record.
///
/// A future target adapter must serialize before publishing, create its
/// temporary file in the destination directory, set the file root-owned and
/// mode `0600`, fsync the file, atomically rename it over the final path, and
/// then fsync the containing directory. It must reject a symlink or
/// non-regular final path rather than read through or replace it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetStateStoreContract {
    pub path: &'static str,
    pub owner: &'static str,
    pub file_mode: u32,
    pub serializes_before_publish: bool,
    pub uses_same_directory_temporary_file: bool,
    pub syncs_file_before_publish: bool,
    pub atomically_replaces_final_path: bool,
    pub syncs_parent_directory_after_publish: bool,
    pub refuses_unsafe_final_path: bool,
}

impl TargetStateStoreContract {
    pub const fn required() -> Self {
        Self {
            path: TARGET_STATE_PATH,
            owner: TARGET_STATE_OWNER,
            file_mode: TARGET_STATE_FILE_MODE,
            serializes_before_publish: true,
            uses_same_directory_temporary_file: true,
            syncs_file_before_publish: true,
            atomically_replaces_final_path: true,
            syncs_parent_directory_after_publish: true,
            refuses_unsafe_final_path: true,
        }
    }
}

/// The ordered, verified milestones of an installation transaction.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
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

impl InstallPhase {
    pub const ALL: [Self; 12] = [
        Self::Discovered,
        Self::PreflightPassed,
        Self::ApplicationAcquired,
        Self::DriverReady,
        Self::TrybootStaged,
        Self::TrybootVerified,
        Self::DriverAccepted,
        Self::ApplicationInstalled,
        Self::HostnameChanged,
        Self::FinalRebooted,
        Self::FinalVerified,
        Self::Complete,
    ];
}

/// An immutable application or driver release identity persisted for resumption.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactIdentity {
    pub version: String,
    pub source_commit: String,
    pub sha256: String,
}

/// Mac-side durable state for one target installation transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InstallState {
    pub schema_version: u32,
    pub target: TargetIdentity,
    pub phase: InstallPhase,
    pub application: Option<ArtifactIdentity>,
    pub driver: Option<ArtifactIdentity>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInstallState {
    schema_version: u32,
    target: TargetIdentity,
    phase: InstallPhase,
    application: Option<ArtifactIdentity>,
    driver: Option<ArtifactIdentity>,
}

impl<'de> Deserialize<'de> for InstallState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawInstallState::deserialize(deserializer)?;
        let state = Self {
            schema_version: raw.schema_version,
            target: raw.target,
            phase: raw.phase,
            application: raw.application,
            driver: raw.driver,
        };
        state.validate().map_err(D::Error::custom)?;
        Ok(state)
    }
}

impl InstallState {
    pub fn to_json(&self) -> Result<String, StateError> {
        self.validate().map_err(StateError::InvalidState)?;
        serde_json::to_string(self).map_err(StateError::Serialize)
    }

    pub fn from_json(contents: &str) -> Result<Self, StateError> {
        parse_json(contents.as_bytes()).map_err(StateError::Parse)
    }

    fn validate(&self) -> Result<(), &'static str> {
        validate_schema_version(self.schema_version)?;
        validate_target_identity(&self.target)?;
        self.application
            .as_ref()
            .map(validate_artifact)
            .transpose()?;
        self.driver.as_ref().map(validate_artifact).transpose()?;
        Ok(())
    }
}

/// Target hardware identity.  The target record deliberately does not retain
/// its SSH host key because it is written by the target rather than OpenSSH.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetHardwareIdentity {
    pub model: String,
    pub serial: String,
}

/// A file owned by the target installer and therefore eligible for controlled
/// rollback or removal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedFile {
    pub target_path: String,
    pub sha256: String,
}

/// The target-side durable installer record contract.
///
/// Task 9 defines this record and its strict JSON representation only.  It
/// does not open an SSH connection or write the target-side path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetInstallState {
    pub schema_version: u32,
    pub hardware: TargetHardwareIdentity,
    pub application: Option<ArtifactIdentity>,
    pub driver: Option<ArtifactIdentity>,
    pub owned_files: Vec<OwnedFile>,
    pub last_verified_phase: InstallPhase,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTargetInstallState {
    schema_version: u32,
    hardware: TargetHardwareIdentity,
    application: Option<ArtifactIdentity>,
    driver: Option<ArtifactIdentity>,
    owned_files: Vec<OwnedFile>,
    last_verified_phase: InstallPhase,
}

impl<'de> Deserialize<'de> for TargetInstallState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawTargetInstallState::deserialize(deserializer)?;
        let state = Self {
            schema_version: raw.schema_version,
            hardware: raw.hardware,
            application: raw.application,
            driver: raw.driver,
            owned_files: raw.owned_files,
            last_verified_phase: raw.last_verified_phase,
        };
        state.validate().map_err(D::Error::custom)?;
        Ok(state)
    }
}

impl TargetInstallState {
    pub fn to_json(&self) -> Result<String, StateError> {
        self.validate().map_err(StateError::InvalidState)?;
        serde_json::to_string(self).map_err(StateError::Serialize)
    }

    pub fn from_json(contents: &str) -> Result<Self, StateError> {
        parse_json(contents.as_bytes()).map_err(StateError::Parse)
    }

    fn validate(&self) -> Result<(), &'static str> {
        validate_schema_version(self.schema_version)?;
        validate_hardware_identity(&self.hardware)?;
        self.application
            .as_ref()
            .map(validate_artifact)
            .transpose()?;
        self.driver.as_ref().map(validate_artifact).transpose()?;
        for owned_file in &self.owned_files {
            validate_owned_file(owned_file)?;
        }
        Ok(())
    }
}

/// A durable state store bound to one expected target identity.
pub trait StateStore {
    fn load(&self) -> Result<Option<InstallState>, StateError>;
    fn save(&self, state: &InstallState) -> Result<(), StateError>;
}

/// The target-side persistence boundary to be implemented by the internal
/// target installer. Task 9 intentionally defines this contract without
/// opening a target connection or writing target state from the Mac.
pub trait TargetStateStore {
    fn load_target_state(&self) -> Result<Option<TargetInstallState>, StateError>;
    fn save_target_state(&self, state: &TargetInstallState) -> Result<(), StateError>;

    fn contract(&self) -> TargetStateStoreContract {
        TargetStateStoreContract::required()
    }
}

/// A local, per-user, file-backed state store.
///
/// Its state root is `${XDG_STATE_HOME}/planeradar/installer` when the caller
/// supplies an absolute `XDG_STATE_HOME`; otherwise it is
/// `${home}/.local/state/planeradar/installer`.  Relative values are rejected
/// so neither durable state nor untrusted target text can resolve from the
/// repository or current directory.
#[derive(Clone, Debug)]
pub struct LocalStateStore {
    state_root: PathBuf,
    state_path: PathBuf,
    target_key: String,
    expected_target: TargetIdentity,
}

impl LocalStateStore {
    pub fn new(
        home: &Path,
        xdg_state_home: Option<&Path>,
        expected_target: TargetIdentity,
    ) -> Result<Self, StateError> {
        let state_root = Self::resolve_state_root(home, xdg_state_home)?;
        let target_key = Self::target_key_for(&expected_target.host_key_sha256);
        let state_path = state_root
            .join("planeradar")
            .join("installer")
            .join(&target_key)
            .join("state.json");
        Ok(Self {
            state_root,
            state_path,
            target_key,
            expected_target,
        })
    }

    /// Resolves `XDG_STATE_HOME` from the process environment using the same
    /// absolute-only policy as [`Self::new`].
    pub fn from_environment(
        home: &Path,
        expected_target: TargetIdentity,
    ) -> Result<Self, StateError> {
        let xdg_state_home = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        if xdg_state_home
            .as_deref()
            .is_some_and(|path| path.to_str().is_none())
        {
            return Err(StateError::NonUnicodeXdgStateHome);
        }
        Self::new(home, xdg_state_home.as_deref(), expected_target)
    }

    pub fn resolve_state_root(
        home: &Path,
        xdg_state_home: Option<&Path>,
    ) -> Result<PathBuf, StateError> {
        if !home.is_absolute() {
            return Err(StateError::RelativeHome {
                path: home.to_owned(),
            });
        }
        match xdg_state_home {
            Some(path) if !path.is_absolute() => Err(StateError::RelativeXdgStateHome {
                path: path.to_owned(),
            }),
            Some(path) => Ok(path.to_owned()),
            None => Ok(home.join(".local").join("state")),
        }
    }

    /// The SHA-256 key for the entire OpenSSH host-key fingerprint string.
    pub fn target_key_for(host_key_fingerprint: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(host_key_fingerprint.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    pub fn target_key(&self) -> &str {
        &self.target_key
    }

    fn state_parent(&self) -> Result<&Path, StateError> {
        self.state_path
            .parent()
            .ok_or_else(|| StateError::MissingStateParent {
                path: self.state_path.clone(),
            })
    }

    fn existing_state_parent_is_safe(&self) -> Result<bool, StateError> {
        match fs::metadata(&self.state_root) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(StateError::UnsafeParentPath {
                    path: self.state_root.clone(),
                });
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(StateError::Io {
                    path: self.state_root.clone(),
                    source,
                });
            }
        }

        let mut directory = self.state_root.clone();
        for component in ["planeradar", "installer"] {
            directory.push(component);
            if !existing_owned_state_directory_is_safe(&directory)? {
                return Ok(false);
            }
        }
        directory.push(&self.target_key);
        existing_owned_state_directory_is_safe(&directory)
    }

    fn create_state_parent_safely(&self) -> Result<(), StateError> {
        fs::create_dir_all(&self.state_root).map_err(|source| StateError::Io {
            path: self.state_root.clone(),
            source,
        })?;
        let metadata = fs::metadata(&self.state_root).map_err(|source| StateError::Io {
            path: self.state_root.clone(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(StateError::UnsafeParentPath {
                path: self.state_root.clone(),
            });
        }

        let mut directory = self.state_root.clone();
        for component in ["planeradar", "installer"] {
            directory.push(component);
            create_owned_state_directory_safely(&directory)?;
        }
        directory.push(&self.target_key);
        create_owned_state_directory_safely(&directory)
    }
}

impl StateStore for LocalStateStore {
    fn load(&self) -> Result<Option<InstallState>, StateError> {
        if !self.existing_state_parent_is_safe()? {
            return Ok(None);
        }
        let Some(file) = open_existing_regular_file(&self.state_path)? else {
            return Ok(None);
        };
        let state: InstallState =
            parse_json_reader(BufReader::new(file)).map_err(|source| StateError::StateParse {
                path: self.state_path.clone(),
                source,
            })?;
        if !self.expected_target.matches(&state.target) {
            return Err(StateError::TargetIdentityMismatch);
        }
        Ok(Some(state))
    }

    fn save(&self, state: &InstallState) -> Result<(), StateError> {
        state.validate().map_err(StateError::InvalidState)?;
        if !self.expected_target.matches(&state.target) {
            return Err(StateError::TargetIdentityMismatch);
        }

        // Serialize and validate before creating a temporary file, so any
        // serialization/validation failure leaves a previously valid record intact.
        let encoded = state.to_json()?;
        self.create_state_parent_safely()?;
        let parent = self.state_parent()?;
        reject_unsafe_final_path(&self.state_path)?;

        let mut temporary = NamedTempFile::new_in(parent).map_err(|source| StateError::Io {
            path: parent.to_owned(),
            source,
        })?;
        set_private_mode(&temporary)?;
        temporary
            .as_file_mut()
            .write_all(encoded.as_bytes())
            .map_err(|source| StateError::Io {
                path: temporary.path().to_owned(),
                source,
            })?;
        temporary
            .as_file_mut()
            .flush()
            .map_err(|source| StateError::Io {
                path: temporary.path().to_owned(),
                source,
            })?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|source| StateError::Io {
                path: temporary.path().to_owned(),
                source,
            })?;
        temporary
            .persist(&self.state_path)
            .map_err(|error| StateError::Io {
                path: self.state_path.clone(),
                source: error.error,
            })?;
        sync_directory(parent)?;
        Ok(())
    }
}

fn validate_schema_version(schema_version: u32) -> Result<(), &'static str> {
    if schema_version != STATE_SCHEMA_VERSION {
        return Err("unknown state schema version");
    }
    Ok(())
}

fn validate_target_identity(identity: &TargetIdentity) -> Result<(), &'static str> {
    if !is_openssh_sha256_fingerprint(&identity.host_key_sha256) {
        return Err("target host key must be an OpenSSH SHA256 fingerprint");
    }
    validate_hardware_identity_fields(&identity.model, &identity.serial)
}

fn is_openssh_sha256_fingerprint(value: &str) -> bool {
    let Some(fingerprint) = value.strip_prefix("SHA256:") else {
        return false;
    };
    fingerprint.len() == 43
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
}

fn validate_hardware_identity_fields(model: &str, serial: &str) -> Result<(), &'static str> {
    let valid_model = model == "Raspberry Pi Zero 2 W"
        || model
            .strip_prefix("Raspberry Pi Zero 2 W Rev ")
            .is_some_and(|revision| {
                !revision.is_empty()
                    && revision
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'.')
            });
    if !valid_model {
        return Err("target model must identify a Raspberry Pi Zero 2 W");
    }
    if !is_lower_hex(serial, 16) {
        return Err("target serial must be 16 lowercase hexadecimal characters");
    }
    Ok(())
}

fn validate_hardware_identity(identity: &TargetHardwareIdentity) -> Result<(), &'static str> {
    validate_hardware_identity_fields(&identity.model, &identity.serial)
}

fn validate_artifact(artifact: &ArtifactIdentity) -> Result<(), &'static str> {
    if semver::Version::parse(&artifact.version).is_err() {
        return Err("artifact version must be semantic version text");
    }
    if !is_lower_hex(&artifact.source_commit, 40) {
        return Err("artifact source commit must be 40 lowercase hexadecimal characters");
    }
    if !is_lower_hex(&artifact.sha256, 64) {
        return Err("artifact SHA-256 must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn validate_owned_file(file: &OwnedFile) -> Result<(), &'static str> {
    if !file.target_path.starts_with('/')
        || file.target_path == "/"
        || file.target_path.contains("//")
        || file
            .target_path
            .split('/')
            .any(|part| part == "." || part == "..")
        || file.target_path.starts_with("/Users/")
        || file.target_path.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err("owned file must use a safe absolute target path");
    }
    if !is_lower_hex(&file.sha256, 64) {
        return Err("owned-file SHA-256 must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_json<T>(contents: &[u8]) -> Result<T, serde_json::Error>
where
    T: DeserializeOwned,
{
    parse_json_reader(contents)
}

fn parse_json_reader<T, R>(reader: R) -> Result<T, serde_json::Error>
where
    T: DeserializeOwned,
    R: io::Read,
{
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let value = T::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

fn existing_owned_state_directory_is_safe(path: &Path) -> Result<bool, StateError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(StateError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(StateError::UnsafeParentPath {
            path: path.to_owned(),
        });
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(StateError::UnsafeParentPath {
            path: path.to_owned(),
        });
    }
    Ok(true)
}

fn create_owned_state_directory_safely(path: &Path) -> Result<(), StateError> {
    match create_directory_with_private_mode(path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(StateError::Io {
                path: path.to_owned(),
                source,
            });
        }
    }
    if !existing_owned_state_directory_is_safe(path)? {
        return Err(StateError::UnsafeParentPath {
            path: path.to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn create_directory_with_private_mode(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_directory_with_private_mode(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

fn reject_unsafe_final_path(path: &Path) -> Result<(), StateError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(StateError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(StateError::SymlinkFinalPath {
            path: path.to_owned(),
        });
    }
    if !metadata.file_type().is_file() {
        return Err(StateError::NonRegularFinalPath {
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn open_existing_regular_file(path: &Path) -> Result<Option<File>, StateError> {
    reject_unsafe_final_path(path)?;
    let file = {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        match options.open(path) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StateError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        }
    };
    let metadata = file.metadata().map_err(|source| StateError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(StateError::NonRegularFinalPath {
            path: path.to_owned(),
        });
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(StateError::InsecurePermissions {
            path: path.to_owned(),
            mode: metadata.permissions().mode() & 0o777,
        });
    }
    Ok(Some(file))
}

fn set_private_mode(file: &NamedTempFile) -> Result<(), StateError> {
    #[cfg(unix)]
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| StateError::Io {
            path: file.path().to_owned(),
            source,
        })?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), StateError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| StateError::Io {
            path: path.to_owned(),
            source,
        })
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("state home must be an absolute path: {path}")]
    RelativeHome { path: PathBuf },
    #[error("XDG_STATE_HOME must be an absolute path: {path}")]
    RelativeXdgStateHome { path: PathBuf },
    #[error("XDG_STATE_HOME must be Unicode")]
    NonUnicodeXdgStateHome,
    #[error("state path has no containing directory: {path}")]
    MissingStateParent { path: PathBuf },
    #[error("installer-owned state parent is unsafe: {path}")]
    UnsafeParentPath { path: PathBuf },
    #[error("state file is a symlink and will not be followed: {path}")]
    SymlinkFinalPath { path: PathBuf },
    #[error("state file is not a regular file: {path}")]
    NonRegularFinalPath { path: PathBuf },
    #[error("state file permissions are too permissive ({mode:o}): {path}")]
    InsecurePermissions { path: PathBuf, mode: u32 },
    #[error("state does not match the expected SSH host key, model, and serial")]
    TargetIdentityMismatch,
    #[error("invalid state: {0}")]
    InvalidState(&'static str),
    #[error("could not serialize state")]
    Serialize(#[source] serde_json::Error),
    #[error("could not parse state JSON")]
    Parse(#[source] serde_json::Error),
    #[error("could not parse state JSON from {path}")]
    StateParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("filesystem operation failed for {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
