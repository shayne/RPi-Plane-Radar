use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub const DEFAULT_HYPERPIXEL_DECLARATION: &str = "dtoverlay=vc4-kms-dpi-hyperpixel2r";

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
        ensure_source_unchanged(&self.path, approved_source)?;
        if !changed {
            return Ok(false);
        }

        let metadata = fs::metadata(&self.path)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| InstallError::MissingParent(self.path.clone()))?;
        let backup = backup_path(&self.path);

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(mut file) => {
                file.write_all(approved_source.as_bytes())?;
                file.set_permissions(fs::Permissions::from_mode(metadata.permissions().mode()))?;
                file.sync_all()?;
                File::open(parent)?.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }

        let mut temporary = tempfile::Builder::new()
            .prefix(".planeradar-config-")
            .tempfile_in(parent)?;
        temporary.write_all(updated.as_bytes())?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(metadata.permissions().mode()))?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path)?;
        File::open(parent)?.sync_all()?;

        Ok(true)
    }
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

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .expect("boot configuration path has a file name")
        .to_os_string();
    name.push(".planeradar-backup");
    path.with_file_name(name)
}

fn open_lock_file(path: &Path) -> Result<File, InstallError> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(path))?)
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .expect("boot configuration path has a file name")
        .to_os_string();
    name.push(".planeradar-lock");
    path.with_file_name(name)
}
