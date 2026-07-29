use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const DEBUG_FRAME_PATH: &str = "/var/lib/planeradar/debug.png";
pub const CAPTURE_DIRECTORY_PATH: &str = "/var/lib/planeradar-installer/captures";
pub const PUBLISHED_FRAME_PATH: &str = "/var/lib/planeradar-installer/captures/current.png";
pub const MAX_CAPTURE_BYTES: u64 = 8 * 1024 * 1024;
const PROTOCOL_SCHEMA_VERSION: u32 = 1;
const MAX_PROTOCOL_JSON_BYTES: usize = 2 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureMetadata {
    pub inode: u64,
    pub modified_ns: u64,
    pub size: u64,
    pub sha256: String,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub links: u64,
    pub regular: bool,
    pub symlink: bool,
}

#[derive(Clone, Debug)]
pub struct CapturePaths {
    debug_frame: PathBuf,
    capture_directory: PathBuf,
    published_frame: PathBuf,
}

impl CapturePaths {
    pub fn new(
        debug_frame: PathBuf,
        capture_directory: PathBuf,
        published_frame: PathBuf,
    ) -> Result<Self, CaptureError> {
        if !debug_frame.is_absolute()
            || !capture_directory.is_absolute()
            || !published_frame.is_absolute()
            || debug_frame.file_name().and_then(|name| name.to_str()) != Some("debug.png")
            || published_frame.file_name().and_then(|name| name.to_str()) != Some("current.png")
            || published_frame.parent() != Some(capture_directory.as_path())
            || debug_frame
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
            || capture_directory
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(CaptureError::UnsafePath);
        }
        Ok(Self {
            debug_frame,
            capture_directory,
            published_frame,
        })
    }

    pub fn production() -> Self {
        Self {
            debug_frame: PathBuf::from(DEBUG_FRAME_PATH),
            capture_directory: PathBuf::from(CAPTURE_DIRECTORY_PATH),
            published_frame: PathBuf::from(PUBLISHED_FRAME_PATH),
        }
    }

    pub fn debug_frame(&self) -> &Path {
        &self.debug_frame
    }

    pub fn published_frame(&self) -> &Path {
        &self.published_frame
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureSnapshot {
    pub source: CaptureMetadata,
    pub published: CaptureMetadata,
    pub rechecked: CaptureMetadata,
    pub bytes: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProtocolHeader {
    schema_version: u32,
    source: CaptureMetadata,
    published: CaptureMetadata,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProtocolFooter {
    schema_version: u32,
    rechecked: CaptureMetadata,
}

pub fn capture_metadata(path: &Path) -> Result<Option<CaptureMetadata>, CaptureError> {
    let parent = safe_parent_metadata(path)?;
    let mut file = match open_source(path) {
        Ok(file) => file,
        Err(CaptureError::Absent) => return Ok(None),
        Err(error) => return Err(error),
    };
    let (metadata, _) = read_stable_source(&mut file, parent.uid(), parent.gid())?;
    Ok(Some(metadata))
}

pub fn capture_snapshot_protocol(
    paths: &CapturePaths,
    before: Option<&CaptureMetadata>,
    timeout: Duration,
) -> Result<Vec<u8>, CaptureError> {
    if timeout.is_zero() {
        return Err(CaptureError::TimedOut);
    }
    let service_parent = safe_parent_metadata(paths.debug_frame())?;
    let started_at = Instant::now();
    let (source, source_bytes) = loop {
        match open_source(paths.debug_frame()) {
            Ok(mut file) => {
                let candidate =
                    read_stable_source(&mut file, service_parent.uid(), service_parent.gid())?;
                if fresh_after(before, &candidate.0) {
                    break candidate;
                }
            }
            Err(CaptureError::Absent) => {}
            Err(error) => return Err(error),
        }
        let remaining = timeout
            .checked_sub(started_at.elapsed())
            .ok_or(CaptureError::TimedOut)?;
        if remaining.is_zero() {
            return Err(CaptureError::TimedOut);
        }
        thread::sleep(POLL_INTERVAL.min(remaining));
    };
    if started_at.elapsed() >= timeout {
        return Err(CaptureError::TimedOut);
    }

    publish_snapshot(paths, &source_bytes)?;
    if started_at.elapsed() >= timeout {
        return Err(CaptureError::TimedOut);
    }
    let (published, published_bytes) = read_published(paths)?;
    if published.sha256 != source.sha256
        || published.size != source.size
        || published_bytes != source_bytes
    {
        return Err(CaptureError::Changed);
    }
    let (rechecked, rechecked_bytes) = read_published(paths)?;
    if rechecked != published || rechecked_bytes != published_bytes {
        return Err(CaptureError::Changed);
    }
    if started_at.elapsed() >= timeout {
        return Err(CaptureError::TimedOut);
    }
    encode_protocol(&source, &published, &rechecked, &published_bytes)
}

pub fn parse_snapshot_protocol(input: &[u8]) -> Result<CaptureSnapshot, CaptureError> {
    let (header_bytes, rest) = take_framed_json(input)?;
    let header: ProtocolHeader = parse_strict_json(header_bytes)?;
    if header.schema_version != PROTOCOL_SCHEMA_VERSION {
        return Err(CaptureError::InvalidProtocol);
    }
    validate_published_metadata(&header.published)?;
    validate_source_metadata(&header.source, header.source.uid, header.source.gid)?;
    let payload_size =
        usize::try_from(header.published.size).map_err(|_| CaptureError::InvalidProtocol)?;
    if payload_size == 0 || payload_size as u64 > MAX_CAPTURE_BYTES || rest.len() < payload_size {
        return Err(CaptureError::InvalidProtocol);
    }
    let (bytes, footer_input) = rest.split_at(payload_size);
    let (footer_bytes, trailing) = take_framed_json(footer_input)?;
    if !trailing.is_empty() {
        return Err(CaptureError::InvalidProtocol);
    }
    let footer: ProtocolFooter = parse_strict_json(footer_bytes)?;
    if footer.schema_version != PROTOCOL_SCHEMA_VERSION {
        return Err(CaptureError::InvalidProtocol);
    }
    validate_published_metadata(&footer.rechecked)?;
    if header.published != footer.rechecked
        || header.source.sha256 != header.published.sha256
        || header.source.size != header.published.size
        || sha256(bytes) != header.published.sha256
    {
        return Err(CaptureError::Changed);
    }
    Ok(CaptureSnapshot {
        source: header.source,
        published: header.published,
        rechecked: footer.rechecked,
        bytes: bytes.to_vec(),
    })
}

pub fn metadata_json(metadata: &CaptureMetadata) -> Result<String, CaptureError> {
    serde_json::to_string(metadata).map_err(|_| CaptureError::InvalidProtocol)
}

pub fn parse_metadata_json(input: &str) -> Result<CaptureMetadata, CaptureError> {
    if input.len() > MAX_PROTOCOL_JSON_BYTES {
        return Err(CaptureError::InvalidProtocol);
    }
    let metadata: CaptureMetadata = parse_strict_json(input.as_bytes())?;
    validate_source_metadata(&metadata, metadata.uid, metadata.gid)?;
    Ok(metadata)
}

fn open_source(path: &Path) -> Result<File, CaptureError> {
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(CaptureError::Absent),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => Err(CaptureError::UnsafeSource),
        Err(_) => Err(CaptureError::UnsafeSource),
    }
}

fn safe_parent_metadata(path: &Path) -> Result<fs::Metadata, CaptureError> {
    let parent = path.parent().ok_or(CaptureError::UnsafePath)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| CaptureError::UnsafePath)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CaptureError::UnsafePath);
    }
    Ok(metadata)
}

fn read_stable_source(
    file: &mut File,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<(CaptureMetadata, Vec<u8>), CaptureError> {
    let before = file.metadata().map_err(|_| CaptureError::UnsafeSource)?;
    validate_open_source(&before, expected_uid, expected_gid)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    Read::by_ref(file)
        .take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CaptureError::UnsafeSource)?;
    if bytes.len() as u64 > MAX_CAPTURE_BYTES {
        return Err(CaptureError::TooLarge);
    }
    let after = file.metadata().map_err(|_| CaptureError::UnsafeSource)?;
    if !same_open_identity(&before, &after) || bytes.len() as u64 != before.len() {
        return Err(CaptureError::Changed);
    }
    Ok((metadata_from(&after, sha256(&bytes)), bytes))
}

fn validate_open_source(
    metadata: &fs::Metadata,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<(), CaptureError> {
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != expected_uid
        || metadata.gid() != expected_gid
        || !matches!(metadata.mode() & 0o777, 0o600 | 0o640)
        || metadata.len() == 0
        || metadata.len() > MAX_CAPTURE_BYTES
    {
        return Err(CaptureError::UnsafeSource);
    }
    Ok(())
}

fn publish_snapshot(paths: &CapturePaths, bytes: &[u8]) -> Result<(), CaptureError> {
    let installer = paths
        .capture_directory
        .parent()
        .ok_or(CaptureError::UnsafePath)?;
    let installer_metadata =
        fs::symlink_metadata(installer).map_err(|_| CaptureError::UnsafePath)?;
    if installer_metadata.file_type().is_symlink()
        || !installer_metadata.is_dir()
        || installer_metadata.mode() & 0o777 != 0o700
    {
        return Err(CaptureError::UnsafePath);
    }
    match fs::symlink_metadata(&paths.capture_directory) {
        Ok(metadata) => validate_capture_directory(&metadata, &installer_metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(&paths.capture_directory)
                .map_err(|_| CaptureError::UnsafePath)?;
            let metadata = fs::symlink_metadata(&paths.capture_directory)
                .map_err(|_| CaptureError::UnsafePath)?;
            validate_capture_directory(&metadata, &installer_metadata)?;
        }
        Err(_) => return Err(CaptureError::UnsafePath),
    }
    if let Ok(metadata) = fs::symlink_metadata(&paths.published_frame) {
        validate_published_file_metadata(&metadata, &installer_metadata)?;
    }

    let mut random = rand::rng();
    let mut temporary = None;
    for _ in 0..16 {
        let name = format!(".capture-{:016x}", random.next_u64());
        let path = paths.capture_directory.join(name);
        match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => {
                temporary = Some((path, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(CaptureError::UnsafePath),
        }
    }
    let (temporary_path, mut temporary_file) = temporary.ok_or(CaptureError::UnsafePath)?;
    let result = (|| {
        temporary_file
            .write_all(bytes)
            .and_then(|()| temporary_file.sync_all())
            .map_err(|_| CaptureError::UnsafePath)?;
        temporary_file
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| CaptureError::UnsafePath)?;
        let metadata = temporary_file
            .metadata()
            .map_err(|_| CaptureError::UnsafePath)?;
        validate_published_file_metadata(&metadata, &installer_metadata)?;
        fs::rename(&temporary_path, &paths.published_frame)
            .map_err(|_| CaptureError::UnsafePath)?;
        File::open(&paths.capture_directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| CaptureError::UnsafePath)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn read_published(paths: &CapturePaths) -> Result<(CaptureMetadata, Vec<u8>), CaptureError> {
    let installer = paths
        .capture_directory
        .parent()
        .ok_or(CaptureError::UnsafePath)?;
    let owner = fs::symlink_metadata(installer).map_err(|_| CaptureError::UnsafePath)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&paths.published_frame)
        .map_err(|_| CaptureError::UnsafePath)?;
    let metadata = file.metadata().map_err(|_| CaptureError::UnsafePath)?;
    validate_published_file_metadata(&metadata, &owner)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CaptureError::Changed)?;
    let after = file.metadata().map_err(|_| CaptureError::Changed)?;
    if !same_open_identity(&metadata, &after) || bytes.len() as u64 != metadata.len() {
        return Err(CaptureError::Changed);
    }
    Ok((metadata_from(&after, sha256(&bytes)), bytes))
}

fn validate_capture_directory(
    metadata: &fs::Metadata,
    owner: &fs::Metadata,
) -> Result<(), CaptureError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != owner.uid()
        || metadata.gid() != owner.gid()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(CaptureError::UnsafePath);
    }
    Ok(())
}

fn validate_published_file_metadata(
    metadata: &fs::Metadata,
    owner: &fs::Metadata,
) -> Result<(), CaptureError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != owner.uid()
        || metadata.gid() != owner.gid()
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > MAX_CAPTURE_BYTES
    {
        return Err(CaptureError::UnsafePath);
    }
    Ok(())
}

fn validate_source_metadata(
    metadata: &CaptureMetadata,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<(), CaptureError> {
    if !metadata.regular
        || metadata.symlink
        || metadata.links != 1
        || metadata.uid != expected_uid
        || metadata.gid != expected_gid
        || !matches!(metadata.mode, 0o600 | 0o640)
        || metadata.size == 0
        || metadata.size > MAX_CAPTURE_BYTES
        || !is_lower_hex(&metadata.sha256, 64)
    {
        return Err(CaptureError::InvalidProtocol);
    }
    Ok(())
}

fn validate_published_metadata(metadata: &CaptureMetadata) -> Result<(), CaptureError> {
    if !metadata.regular
        || metadata.symlink
        || metadata.links != 1
        || metadata.mode != 0o600
        || metadata.size == 0
        || metadata.size > MAX_CAPTURE_BYTES
        || !is_lower_hex(&metadata.sha256, 64)
    {
        return Err(CaptureError::InvalidProtocol);
    }
    Ok(())
}

fn fresh_after(before: Option<&CaptureMetadata>, candidate: &CaptureMetadata) -> bool {
    before.is_none_or(|before| {
        candidate.inode != before.inode && candidate.modified_ns >= before.modified_ns
    })
}

fn same_open_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.len() == right.len()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
}

fn metadata_from(metadata: &fs::Metadata, sha256: String) -> CaptureMetadata {
    let seconds = u64::try_from(metadata.mtime()).unwrap_or(0);
    let nanoseconds = u64::try_from(metadata.mtime_nsec()).unwrap_or(0);
    CaptureMetadata {
        inode: metadata.ino(),
        modified_ns: seconds
            .saturating_mul(1_000_000_000)
            .saturating_add(nanoseconds),
        size: metadata.len(),
        sha256,
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.mode() & 0o777,
        links: metadata.nlink(),
        regular: metadata.is_file(),
        symlink: metadata.file_type().is_symlink(),
    }
}

fn encode_protocol(
    source: &CaptureMetadata,
    published: &CaptureMetadata,
    rechecked: &CaptureMetadata,
    bytes: &[u8],
) -> Result<Vec<u8>, CaptureError> {
    let header = serde_json::to_vec(&ProtocolHeader {
        schema_version: PROTOCOL_SCHEMA_VERSION,
        source: source.clone(),
        published: published.clone(),
    })
    .map_err(|_| CaptureError::InvalidProtocol)?;
    let footer = serde_json::to_vec(&ProtocolFooter {
        schema_version: PROTOCOL_SCHEMA_VERSION,
        rechecked: rechecked.clone(),
    })
    .map_err(|_| CaptureError::InvalidProtocol)?;
    if header.len() > MAX_PROTOCOL_JSON_BYTES || footer.len() > MAX_PROTOCOL_JSON_BYTES {
        return Err(CaptureError::InvalidProtocol);
    }
    let mut output = Vec::with_capacity(8 + header.len() + bytes.len() + footer.len());
    output.extend_from_slice(&(header.len() as u32).to_be_bytes());
    output.extend_from_slice(&header);
    output.extend_from_slice(bytes);
    output.extend_from_slice(&(footer.len() as u32).to_be_bytes());
    output.extend_from_slice(&footer);
    Ok(output)
}

fn take_framed_json(input: &[u8]) -> Result<(&[u8], &[u8]), CaptureError> {
    let length_bytes: [u8; 4] = input
        .get(..4)
        .ok_or(CaptureError::InvalidProtocol)?
        .try_into()
        .map_err(|_| CaptureError::InvalidProtocol)?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 || length > MAX_PROTOCOL_JSON_BYTES || input.len() < 4 + length {
        return Err(CaptureError::InvalidProtocol);
    }
    Ok((&input[4..4 + length], &input[4 + length..]))
}

fn parse_strict_json<T: for<'de> Deserialize<'de>>(input: &[u8]) -> Result<T, CaptureError> {
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = T::deserialize(&mut deserializer).map_err(|_| CaptureError::InvalidProtocol)?;
    deserializer
        .end()
        .map_err(|_| CaptureError::InvalidProtocol)?;
    Ok(value)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CaptureError {
    #[error("capture path is unsafe")]
    UnsafePath,
    #[error("debug source is absent")]
    Absent,
    #[error("debug source is unsafe")]
    UnsafeSource,
    #[error("debug source is too large")]
    TooLarge,
    #[error("debug source changed while captured")]
    Changed,
    #[error("fresh debug source did not arrive before the deadline")]
    TimedOut,
    #[error("capture protocol is invalid")]
    InvalidProtocol,
}
