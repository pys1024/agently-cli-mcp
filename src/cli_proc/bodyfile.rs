//! Temporary body files for tool input/output.

use std::io::Write;
use std::path::{Path, PathBuf};

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

const TMP_DIR: &str = ".agently-cli-mcp/tmp";

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

    /// Recompute the file's SHA-256 and compare it against the content written
    /// at creation time.
    pub fn verify(&self) -> Result<(), AppError> {
        let data = std::fs::read(&self.path).map_err(|e| {
            AppError::CliError(format!(
                "failed to read temp file `{}`: {e}",
                self.path.display()
            ))
        })?;
        let digest: [u8; 32] = Sha256::digest(&data).into();
        if digest != self.sha256 {
            return Err(AppError::CliError(format!(
                "temp file `{}` content changed on disk",
                self.path.display()
            )));
        }
        Ok(())
    }

    /// Delete the file. The caller decides when removal happens.
    pub fn remove(self) -> std::io::Result<()> {
        std::fs::remove_file(&self.path)
    }
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
