use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

pub const STOCK_HYPERPIXEL_DECLARATION: &str = "dtoverlay=vc4-kms-dpi-hyperpixel2r";
pub const DEFAULT_HYPERPIXEL_DECLARATION: &str = STOCK_HYPERPIXEL_DECLARATION;
pub const PLANERADAR_HYPERPIXEL_PREFIX: &str = "planeradar-hyperpixel2r-";
pub const MAX_BOOT_CONFIG_LINE_BYTES: usize = 98;

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
    #[error("failed to update boot configuration: {0}")]
    Io(#[from] io::Error),
    #[error("failed to persist boot configuration: {0}")]
    Persist(#[from] tempfile::PersistError),
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
    let lock_path = lock_path(path);
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

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .expect("boot configuration path has a file name")
        .to_os_string();
    name.push(".planeradar-lock");
    path.with_file_name(name)
}
