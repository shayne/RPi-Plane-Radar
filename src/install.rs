use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub const DEFAULT_HYPERPIXEL_DECLARATION: &str = "dtoverlay=vc4-kms-dpi-hyperpixel2r";

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("boot configuration path has no parent: {0}")]
    MissingParent(PathBuf),
    #[error("failed to update boot configuration: {0}")]
    Io(#[from] io::Error),
    #[error("failed to persist boot configuration: {0}")]
    Persist(#[from] tempfile::PersistError),
}

pub fn ensure_overlay(input: &str, declaration: &str) -> (String, bool) {
    let newline = if input.contains("\r\n") { "\r\n" } else { "\n" };
    let had_final_newline = input.ends_with('\n');
    let mut lines: Vec<String> = input
        .lines()
        .filter(|line| line.trim() != declaration)
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();

    let insertion = if let Some(last_all) = lines.iter().rposition(|line| line.trim() == "[all]") {
        last_all + 1
    } else {
        lines.push("[all]".to_owned());
        lines.len()
    };
    lines.insert(insertion, declaration.to_owned());

    let mut updated = lines.join(newline);
    if had_final_newline {
        updated.push_str(newline);
    }

    if updated == input {
        (input.to_owned(), false)
    } else {
        (updated, true)
    }
}

pub fn edit_boot_config(path: &Path, declaration: &str) -> Result<bool, InstallError> {
    let source = fs::read_to_string(path)?;
    let (updated, changed) = ensure_overlay(&source, declaration);
    if !changed {
        return Ok(false);
    }

    let metadata = fs::metadata(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::MissingParent(path.to_owned()))?;
    let backup = backup_path(path);

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
    {
        Ok(mut file) => {
            file.write_all(source.as_bytes())?;
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
    temporary.persist(path)?;
    File::open(parent)?.sync_all()?;

    Ok(true)
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .expect("boot configuration path has a file name")
        .to_os_string();
    name.push(".planeradar-backup");
    path.with_file_name(name)
}
