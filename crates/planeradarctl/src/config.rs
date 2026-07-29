use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::Deserialize;
use thiserror::Error;

use crate::cli::{Cli, Command, MutatingOptions, UninstallOptions};

pub const DEFAULT_HOSTNAME: &str = "planeradar";
pub const DRIVER_REPOSITORY: &str = "https://github.com/shayne/hyperpixel2r-kms";
pub const DRIVER_LIFECYCLE_PROTOCOL: &str = "accepted-driver-v2";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub target: Option<String>,
    pub hostname: Option<String>,
    pub docker_context: Option<String>,
}

impl Environment {
    pub fn from_dotenv_path(path: &Path) -> Result<Self, ConfigError> {
        let contents = match fs::read(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(ConfigError::DotenvRead {
                    path: path.to_owned(),
                    error,
                });
            }
        };
        let contents =
            std::str::from_utf8(&contents).map_err(|_| ConfigError::DotenvNonUnicode {
                path: path.to_owned(),
            })?;

        let mut environment = Self::default();
        let mut keys = HashSet::new();
        for (line_index, line) in contents.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line_number = line_index + 1;
            let (key, value) = line
                .split_once('=')
                .ok_or(ConfigError::MalformedDotenvLine { line: line_number })?;
            if key.is_empty() || key.trim() != key || value.trim() != value {
                return Err(ConfigError::MalformedDotenvLine { line: line_number });
            }
            if !keys.insert(key) {
                return Err(ConfigError::DuplicateDotenvKey {
                    key: key.to_owned(),
                });
            }

            match key {
                "PLANERADAR_PI_TARGET" => {
                    environment.target = Some(required_dotenv_value(key, value)?);
                }
                "PLANERADAR_HOSTNAME" => {
                    environment.hostname = Some(required_dotenv_value(key, value)?);
                }
                "PLANERADAR_DOCKER_CONTEXT" => {
                    environment.docker_context = optional_value(value);
                }
                _ if key.contains("PASSWORD") || key.contains("TOKEN") => {
                    return Err(ConfigError::SecretDotenvKey {
                        key: key.to_owned(),
                    });
                }
                _ => {
                    return Err(ConfigError::UnknownDotenvKey {
                        key: key.to_owned(),
                    });
                }
            }
        }
        Ok(environment)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallConfig {
    pub target: Option<String>,
    pub hostname: String,
    pub version: Option<Version>,
    pub release_dir: Option<PathBuf>,
    pub docker_context: Option<String>,
    pub non_interactive: bool,
    pub purge_settings: bool,
}

impl InstallConfig {
    pub fn resolve(cli: Cli, environment: Environment) -> Result<Self, ConfigError> {
        match cli.command {
            Command::Install(options) | Command::Upgrade(options) | Command::Rollback(options) => {
                Self::from_options(options, environment)
            }
            Command::Uninstall(options) => Self::from_uninstall_options(options, environment),
            _ => Err(ConfigError::NonMutatingCommand),
        }
    }

    fn from_options(
        options: MutatingOptions,
        environment: Environment,
    ) -> Result<Self, ConfigError> {
        if options.version.is_some() && options.release_dir.is_some() {
            return Err(ConfigError::ConflictingReleaseInputs);
        }

        let target = select_cli_value(options.target, environment.target, "target")?;
        let hostname = select_cli_value(options.hostname, environment.hostname, "hostname")?
            .unwrap_or_else(|| DEFAULT_HOSTNAME.to_owned());
        let docker_context = select_cli_value(
            options.docker_context,
            environment.docker_context,
            "docker context",
        )?;
        let version = options
            .version
            .map(|value| Version::parse(&value).map_err(|_| ConfigError::InvalidVersion { value }))
            .transpose()?;
        let release_dir = options
            .release_dir
            .map(|directory| {
                if directory.as_os_str().is_empty() {
                    Err(ConfigError::EmptyValue {
                        field: "release directory",
                    })
                } else {
                    Ok(directory)
                }
            })
            .transpose()?;

        Ok(Self {
            target,
            hostname,
            version,
            release_dir,
            docker_context,
            non_interactive: options.non_interactive,
            purge_settings: false,
        })
    }

    fn from_uninstall_options(
        options: UninstallOptions,
        environment: Environment,
    ) -> Result<Self, ConfigError> {
        Ok(Self {
            target: select_cli_value(options.target, environment.target, "target")?,
            hostname: environment
                .hostname
                .unwrap_or_else(|| DEFAULT_HOSTNAME.to_owned()),
            version: None,
            release_dir: None,
            docker_context: environment.docker_context,
            non_interactive: options.non_interactive,
            purge_settings: options.purge_settings,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverLock {
    pub repository: String,
    pub version: Version,
    pub commit: String,
    pub manifest_sha256: String,
}

impl DriverLock {
    pub fn parse(contents: &str) -> Result<Self, DriverLockError> {
        let raw: RawDriverLock = toml::from_str(contents).map_err(DriverLockError::Toml)?;
        let version =
            Version::parse(&raw.version).map_err(|_| DriverLockError::InvalidVersion {
                version: raw.version,
            })?;
        if raw.lifecycle_protocol != DRIVER_LIFECYCLE_PROTOCOL {
            return Err(DriverLockError::InvalidLifecycleProtocol);
        }
        let lock = Self {
            repository: raw.repository,
            version,
            commit: raw.commit,
            manifest_sha256: raw.manifest_sha256,
        };
        lock.validate()?;
        Ok(lock)
    }

    pub(crate) fn validate(&self) -> Result<(), DriverLockError> {
        if self.repository != DRIVER_REPOSITORY {
            return Err(if self.repository.starts_with("https://") {
                DriverLockError::WrongRepository {
                    repository: self.repository.clone(),
                }
            } else {
                DriverLockError::NonHttpsRepository {
                    repository: self.repository.clone(),
                }
            });
        }
        if !is_lower_hex(&self.commit, 40) {
            return Err(DriverLockError::InvalidCommit {
                commit: self.commit.clone(),
            });
        }
        if !is_lower_hex(&self.manifest_sha256, 64) {
            return Err(DriverLockError::InvalidManifestSha256 {
                digest: self.manifest_sha256.clone(),
            });
        }
        Ok(())
    }

    pub fn checked_in() -> Result<Self, DriverLockError> {
        Self::parse(include_str!("../../../driver.lock.toml"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDriverLock {
    repository: String,
    version: String,
    commit: String,
    manifest_sha256: String,
    lifecycle_protocol: String,
}

fn required_dotenv_value(key: &str, value: &str) -> Result<String, ConfigError> {
    if value.is_empty() {
        return Err(ConfigError::EmptyDotenvValue {
            key: key.to_owned(),
        });
    }
    Ok(value.to_owned())
}

fn optional_value(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn select_cli_value(
    cli_value: Option<String>,
    environment_value: Option<String>,
    field: &'static str,
) -> Result<Option<String>, ConfigError> {
    let value = cli_value.or(environment_value);
    if value.as_deref().is_some_and(|value| value.is_empty()) {
        return Err(ConfigError::EmptyValue { field });
    }
    Ok(value)
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read dotenv file {path}")]
    DotenvRead {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("dotenv file {path} is not valid Unicode")]
    DotenvNonUnicode { path: PathBuf },
    #[error("dotenv line {line} must be a strict KEY=value assignment")]
    MalformedDotenvLine { line: usize },
    #[error("dotenv key {key} is declared more than once")]
    DuplicateDotenvKey { key: String },
    #[error("dotenv key {key} is not supported")]
    UnknownDotenvKey { key: String },
    #[error("dotenv key {key} is not allowed")]
    SecretDotenvKey { key: String },
    #[error("dotenv key {key} must not be empty")]
    EmptyDotenvValue { key: String },
    #[error("{field} must not be empty")]
    EmptyValue { field: &'static str },
    #[error("--version and --release-dir cannot be used together")]
    ConflictingReleaseInputs,
    #[error("version {value:?} is not an exact semantic version")]
    InvalidVersion { value: String },
    #[error("this command does not use install configuration")]
    NonMutatingCommand,
}

#[derive(Debug, Error)]
pub enum DriverLockError {
    #[error("driver lock TOML is invalid")]
    Toml(#[source] toml::de::Error),
    #[error("driver lock repository must use HTTPS: {repository}")]
    NonHttpsRepository { repository: String },
    #[error("driver lock repository is not the supported driver repository: {repository}")]
    WrongRepository { repository: String },
    #[error("driver lock version is not an exact semantic version: {version}")]
    InvalidVersion { version: String },
    #[error("driver lock commit must be a full lowercase SHA-1: {commit}")]
    InvalidCommit { commit: String },
    #[error("driver lock manifest SHA-256 must be lowercase hexadecimal: {digest}")]
    InvalidManifestSha256 { digest: String },
    #[error("driver lock does not name the accepted driver lifecycle protocol")]
    InvalidLifecycleProtocol,
}
