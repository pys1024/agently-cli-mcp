//! Temporary body files for tool input/output.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::AppError;

/// Content format, which determines the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFormat {
    Plain,
    Html,
    Markdown,
}

impl BodyFormat {
    fn extension(self) -> &'static str {
        match self {
            BodyFormat::Plain => "txt",
            BodyFormat::Html => "html",
            BodyFormat::Markdown => "md",
        }
    }
}

/// A temporary body file under `<workdir>/.agently-cli-mcp/tmp/`.
#[derive(Debug)]
pub struct BodyFile {
    path: PathBuf,
    relative: String,
    sha256: [u8; 32],
}

pub(crate) const TMP_DIR: &str = ".agently-cli-mcp/tmp";

impl BodyFile {
    /// Write `content` to a new temp file named with a UUID.
    pub fn create(root: &Path, content: &[u8], format: BodyFormat) -> Result<BodyFile, AppError> {
        let dir = root.join(TMP_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| {
            AppError::CliError(format!(
                "failed to create temp dir `{}`: {e}",
                dir.display()
            ))
        })?;

        let name = format!("{}.{}", Uuid::new_v4(), format.extension());
        let path = dir.join(&name);

        write_private(&path, content).map_err(|e| {
            AppError::CliError(format!(
                "failed to write temp file `{}`: {e}",
                path.display()
            ))
        })?;

        let relative = format!("{TMP_DIR}/{name}");

        Ok(BodyFile {
            path,
            relative,
            sha256: Sha256::digest(content).into(),
        })
    }

    /// Path of the file relative to the working-directory root.
    pub fn relative_path(&self) -> &str {
        &self.relative
    }

    /// The SHA-256 digest of the content written at creation time.
    pub fn digest(&self) -> [u8; 32] {
        self.sha256
    }

    /// Recompute the file's SHA-256 and compare it against the content written
    /// at creation time.
    pub fn verify(&self) -> Result<(), AppError> {
        verify_file(&self.path, &self.sha256)
    }

    /// Delete the file. The caller decides when removal happens.
    pub fn remove(self) -> std::io::Result<()> {
        std::fs::remove_file(&self.path)
    }
}

/// Verify that the file at `path` still hashes to `expected`, returning
/// [`AppError::InvalidArgument`] when the content has been tampered with (or the
/// file can no longer be read). This is the integrity check the confirmed-replay
/// path performs on a retained body file before re-invoking the CLI.
pub fn verify_file(path: &Path, expected: &[u8; 32]) -> Result<(), AppError> {
    let data = std::fs::read(path).map_err(|e| {
        AppError::InvalidArgument(format!("cannot read body file `{}`: {e}", path.display()))
    })?;
    let digest: [u8; 32] = Sha256::digest(&data).into();
    if &digest != expected {
        return Err(AppError::InvalidArgument(format!(
            "body file `{}` content changed on disk",
            path.display()
        )));
    }
    Ok(())
}

/// Remove body temp files under `<root>/.agently-cli-mcp/tmp` whose modified
/// time is older than `max_age`. Returns the number of files removed. Used as a
/// startup sweep so stale files from a previous run do not accumulate forever.
pub fn sweep_stale_body_files(root: &Path, max_age: Duration) -> usize {
    let dir = root.join(TMP_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let cutoff = match std::time::SystemTime::now().checked_sub(max_age) {
        Some(t) => t,
        None => return 0,
    };

    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        if meta.modified().is_ok_and(|m| m < cutoff) && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(unix)]
fn write_private(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(content)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(content)?;
    Ok(())
}
