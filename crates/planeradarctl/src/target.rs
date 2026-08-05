use std::net::Ipv4Addr;
use std::str::FromStr;
use std::{borrow::Cow, fmt};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// A stable target identity collected from OpenSSH and target hardware probes.
///
/// Host names and addresses may change during installation, so later operations
/// must compare every field rather than treating a connection address as an
/// identity.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetIdentity {
    pub host_key_sha256: String,
    pub model: String,
    pub serial: String,
}

impl TargetIdentity {
    /// A domain-specific, length-delimited binding for driver lifecycle state.
    pub fn driver_binding_sha256(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"planeradarctl.target-identity.v1\\0");
        for value in [&self.host_key_sha256, &self.model, &self.serial] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    /// Returns true only when the host key and both hardware identifiers agree.
    pub fn matches(&self, observed: &Self) -> bool {
        self == observed
    }
}

impl fmt::Debug for TargetIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TargetIdentity(<redacted>)")
    }
}

/// A Linux login name that passed the conservative public control-tool grammar.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct SshUsername(String);

impl SshUsername {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SshUsername {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshUsername(<redacted>)")
    }
}

/// A host component accepted by [`SshTarget`].
#[derive(Clone, Eq, Hash, PartialEq)]
pub enum SshHost {
    Hostname(String),
    Ipv4(Ipv4Addr),
}

impl fmt::Debug for SshHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshHost(<redacted>)")
    }
}

impl SshHost {
    pub fn as_str(&self) -> Cow<'_, str> {
        match self {
            Self::Hostname(hostname) => Cow::Borrowed(hostname),
            Self::Ipv4(address) => Cow::Owned(address.to_string()),
        }
    }
}

/// A parsed OpenSSH destination.
///
/// The username and host are retained as separate typed values.  Consumers use
/// [`Self::ssh_arguments`] to append destination arguments to `Command`, never
/// to construct a local shell command.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct SshTarget {
    username: SshUsername,
    host: SshHost,
}

impl fmt::Debug for SshTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshTarget(<redacted>)")
    }
}

impl SshTarget {
    pub fn username(&self) -> &SshUsername {
        &self.username
    }

    pub fn host(&self) -> &SshHost {
        &self.host
    }

    /// One OpenSSH destination argument, not a shell command.
    pub fn ssh_destination(&self) -> String {
        format!("{}@{}", self.username.as_str(), self.host.as_str())
    }

    /// The separator and destination values for an OpenSSH argument vector.
    pub fn ssh_arguments(&self) -> [String; 2] {
        ["--".into(), self.ssh_destination()]
    }
}

impl FromStr for SshTarget {
    type Err = TargetError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        reject_unsafe_input(value)?;

        let mut parts = value.split('@');
        let username = parts.next().expect("split always yields one component");
        let host = parts.next().ok_or(TargetError::MissingHost)?;
        if parts.next().is_some() {
            return Err(TargetError::MultipleAtSigns);
        }

        Ok(Self {
            username: SshUsername(validate_username(username)?.to_owned()),
            host: validate_host(host)?,
        })
    }
}

fn reject_unsafe_input(value: &str) -> Result<(), TargetError> {
    if value.is_empty() {
        return Err(TargetError::Empty);
    }
    if value
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(TargetError::WhitespaceOrControl);
    }
    if value.bytes().any(|byte| {
        matches!(
            byte,
            b'/' | b'\\'
                | b':'
                | b';'
                | b'|'
                | b'&'
                | b'<'
                | b'>'
                | b'$'
                | b'('
                | b')'
                | b'{'
                | b'}'
                | b'['
                | b']'
                | b'!'
                | b'*'
                | b'?'
                | b'~'
                | b'\''
                | b'"'
                | b'`'
        )
    }) {
        return Err(TargetError::UnsafeCharacter);
    }
    Ok(())
}

fn validate_username(value: &str) -> Result<&str, TargetError> {
    if value.is_empty() {
        return Err(TargetError::MissingUsername);
    }
    if value == "root" {
        return Err(TargetError::RootLogin);
    }
    if value.len() > 32
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'_')
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return Err(TargetError::InvalidUsername);
    }
    Ok(value)
}

fn validate_host(value: &str) -> Result<SshHost, TargetError> {
    if value.is_empty() {
        return Err(TargetError::MissingHost);
    }
    if value.starts_with('-') {
        return Err(TargetError::OptionLikeHost);
    }

    if value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return value
            .parse::<Ipv4Addr>()
            .map(SshHost::Ipv4)
            .map_err(|_| TargetError::InvalidIpv4);
    }

    if value.len() > 253 || !value.is_ascii() {
        return Err(TargetError::InvalidHostname);
    }
    for label in value.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(TargetError::InvalidHostname);
        }
    }
    Ok(SshHost::Hostname(value.to_owned()))
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TargetError {
    #[error("SSH target must not be empty")]
    Empty,
    #[error("SSH target must use exactly one user@host separator")]
    MultipleAtSigns,
    #[error("SSH target is missing its username")]
    MissingUsername,
    #[error("SSH target is missing its host")]
    MissingHost,
    #[error("SSH target must not contain whitespace or control characters")]
    WhitespaceOrControl,
    #[error("SSH target contains a character that is unsafe in the public target grammar")]
    UnsafeCharacter,
    #[error("SSH target user must use the conservative Linux username grammar")]
    InvalidUsername,
    #[error("SSH root login is not supported")]
    RootLogin,
    #[error("SSH target host must not be option-like")]
    OptionLikeHost,
    #[error("SSH target host must be a valid lowercase DNS hostname")]
    InvalidHostname,
    #[error("SSH target host looks like IPv4 but is not valid IPv4 text")]
    InvalidIpv4,
}
