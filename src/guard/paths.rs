//! Path sandbox: every file argument must resolve inside a single working
//! directory. Existing paths are canonicalized (which also resolves symlinks)
//! before the prefix check; output paths that do not exist yet are normalized
//! lexically. Inside-root absolute paths are converted back to relative paths
//! for the CLI arguments.

use std::path::{Component, Path, PathBuf};

use crate::error::AppError;

/// One mebibyte, the unit used by the attachment size limits.
const MIB: u64 = 1024 * 1024;

/// Attachment set limits, mirroring the `+me` constraints. Defaults are
/// 50 files, 20 MiB per file, and 20 MiB total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentLimits {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for AttachmentLimits {
    fn default() -> Self {
        Self {
            max_files: 50,
            max_file_bytes: 20 * MIB,
            max_total_bytes: 20 * MIB,
        }
    }
}

/// A path sandbox rooted at a single canonicalized directory.
#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// Build a sandbox rooted at `root`. The root must already exist; it is
    /// canonicalized so later prefix checks compare canonical paths.
    pub fn new(root: PathBuf) -> Result<Self, AppError> {
        let root = std::fs::canonicalize(&root).map_err(|e| {
            AppError::InvalidArgument(format!(
                "cannot resolve working directory `{}`: {e}",
                root.display()
            ))
        })?;
        Ok(Self { root })
    }

    /// The canonicalized sandbox root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve an existing input file (e.g. an attachment) and return its path
    /// relative to the root. Rejects parent-directory escapes and symlink
    /// escapes with [`AppError::PathOutsideWorkdir`].
    pub fn resolve_input(&self, p: &Path) -> Result<PathBuf, AppError> {
        let abs = self.absolutize(p);
        let lex = lexical_normalize(&abs);
        self.ensure_within(&lex)?;
        let canon = std::fs::canonicalize(&lex).map_err(|e| {
            AppError::InvalidArgument(format!("cannot resolve input `{}`: {e}", p.display()))
        })?;
        self.ensure_within(&canon)?;
        Ok(self.relative_unchecked(&canon))
    }

    /// Resolve an output directory that may not exist yet, returning its path
    /// relative to the root. Existing directories are canonicalized (rejecting
    /// symlink escapes); missing directories are checked lexically.
    pub fn resolve_output_dir(&self, p: &Path) -> Result<PathBuf, AppError> {
        let abs = self.absolutize(p);
        if let Ok(canon) = std::fs::canonicalize(&abs) {
            self.ensure_within(&canon)?;
            return Ok(self.relative_unchecked(&canon));
        }
        let lex = lexical_normalize(&abs);
        self.ensure_within(&lex)?;
        Ok(self.relative_unchecked(&lex))
    }

    /// Convert an absolute path that lies inside the root into a path relative
    /// to the root. Rejects any path that would escape the root.
    pub fn relative(&self, p: &Path) -> Result<PathBuf, AppError> {
        let abs = self.absolutize(p);
        let lex = lexical_normalize(&abs);
        self.ensure_within(&lex)?;
        Ok(self.relative_unchecked(&lex))
    }

    /// Validate a set of attachments before spawning the CLI: every path must
    /// resolve to a regular file inside the root, and the set must satisfy
    /// `limits`. Returns the resolved relative paths in input order.
    pub fn validate_attachments(
        &self,
        paths: &[PathBuf],
        limits: &AttachmentLimits,
    ) -> Result<Vec<PathBuf>, AppError> {
        if paths.len() > limits.max_files {
            return Err(AppError::InvalidArgument(format!(
                "too many attachments: {} (maximum {})",
                paths.len(),
                limits.max_files
            )));
        }

        let mut resolved = Vec::with_capacity(paths.len());
        let mut total: u64 = 0;
        for p in paths {
            let canon = self.resolve_input_abs(p)?;
            let meta = std::fs::metadata(&canon).map_err(|e| {
                AppError::InvalidArgument(format!("cannot stat attachment `{}`: {e}", p.display()))
            })?;
            if !meta.is_file() {
                return Err(AppError::InvalidArgument(format!(
                    "attachment is not a regular file: `{}`",
                    p.display()
                )));
            }
            let size = meta.len();
            if size > limits.max_file_bytes {
                return Err(AppError::InvalidArgument(format!(
                    "attachment `{}` is {size} bytes, exceeding the {}-byte per-file limit",
                    p.display(),
                    limits.max_file_bytes
                )));
            }
            total += size;
            if total > limits.max_total_bytes {
                return Err(AppError::InvalidArgument(format!(
                    "attachments total {total} bytes, exceeding the {}-byte total limit",
                    limits.max_total_bytes
                )));
            }
            resolved.push(self.relative_unchecked(&canon));
        }
        Ok(resolved)
    }

    /// Resolve an existing input path to a canonical absolute path inside the
    /// root (no relativization).
    fn resolve_input_abs(&self, p: &Path) -> Result<PathBuf, AppError> {
        let abs = self.absolutize(p);
        let lex = lexical_normalize(&abs);
        self.ensure_within(&lex)?;
        let canon = std::fs::canonicalize(&lex).map_err(|e| {
            AppError::InvalidArgument(format!("cannot resolve input `{}`: {e}", p.display()))
        })?;
        self.ensure_within(&canon)?;
        Ok(canon)
    }

    /// Join a relative path onto the root; pass absolute paths through.
    fn absolutize(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }

    /// Reject a path that does not lexically live inside the root.
    fn ensure_within(&self, path: &Path) -> Result<(), AppError> {
        if path.strip_prefix(&self.root).is_ok() {
            Ok(())
        } else {
            Err(AppError::PathOutsideWorkdir(format!(
                "path `{}` is outside the working directory `{}`",
                path.display(),
                self.root.display()
            )))
        }
    }

    /// Strip the root prefix. Callers must have already verified the path is
    /// inside the root.
    fn relative_unchecked(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.root)
            .map(PathBuf::from)
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Normalize a path lexically, resolving `.` and collapsing `..` without
/// touching the filesystem. The input is expected to be absolute.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pop past the root; a leading `..` has no effect.
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}
