//! Path sandbox tests: parent escape, symlink escape, absolute paths, and
//! attachment prechecks.

use agently_cli_mcp::guard::{AttachmentLimits, Sandbox};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

/// Create a tempdir containing a `workdir` subdirectory used as the sandbox
/// root. The returned `TempDir` must stay alive for the duration of the test.
fn setup() -> (tempfile::TempDir, PathBuf, Sandbox) {
    let dir = tempdir().unwrap();
    let root = dir.path().join("workdir");
    std::fs::create_dir_all(&root).unwrap();
    let sandbox = Sandbox::new(root.clone()).unwrap();
    (dir, root, sandbox)
}

#[test]
fn rejects_parent_escape() {
    let (dir, _root, sandbox) = setup();
    std::fs::write(dir.path().join("secret"), "x").unwrap();

    let err = sandbox.resolve_input(Path::new("../secret")).unwrap_err();
    assert_eq!(err.code(), "PATH_OUTSIDE_WORKDIR");
}

#[cfg(unix)]
#[test]
fn rejects_symlink_escape() {
    let (dir, root, sandbox) = setup();
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "x").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link")).unwrap();

    let err = sandbox.resolve_input(Path::new("link")).unwrap_err();
    assert_eq!(err.code(), "PATH_OUTSIDE_WORKDIR");
}

#[test]
fn accepts_absolute_path_inside_root() {
    let (_dir, root, sandbox) = setup();
    std::fs::create_dir_all(root.join("inner")).unwrap();
    let file = root.join("inner").join("file.txt");
    std::fs::write(&file, "x").unwrap();

    let rel = sandbox.resolve_input(&file).unwrap();
    assert_eq!(rel, PathBuf::from("inner").join("file.txt"));
}

#[test]
fn rejects_oversized_attachment() {
    let (_dir, root, sandbox) = setup();
    let big = root.join("big.bin");
    let file = std::fs::File::create(&big).unwrap();
    // 21 MiB exceeds the 20 MiB per-file default.
    file.set_len(21 * 1024 * 1024).unwrap();

    let err = sandbox
        .validate_attachments(&[big], &AttachmentLimits::default())
        .unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
}

#[test]
fn resolves_output_dir_lexically_when_missing() {
    let (_dir, _root, sandbox) = setup();

    let rel = sandbox.resolve_output_dir(Path::new("new/sub")).unwrap();
    assert_eq!(rel, PathBuf::from("new").join("sub"));
}

#[test]
fn rejects_output_dir_escape() {
    let (_dir, _root, sandbox) = setup();

    let err = sandbox
        .resolve_output_dir(Path::new("../outside"))
        .unwrap_err();
    assert_eq!(err.code(), "PATH_OUTSIDE_WORKDIR");
}

#[test]
fn relative_converts_absolute_inside_root() {
    let (_dir, root, sandbox) = setup();

    let rel = sandbox.relative(&root.join("a").join("b")).unwrap();
    assert_eq!(rel, PathBuf::from("a").join("b"));
}

#[test]
fn rejects_non_regular_file_attachment() {
    let (_dir, root, sandbox) = setup();
    let sub = root.join("subdir");
    std::fs::create_dir_all(&sub).unwrap();

    let err = sandbox
        .validate_attachments(&[sub], &AttachmentLimits::default())
        .unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
}

#[test]
fn validate_attachments_returns_relative_paths() {
    let (_dir, root, sandbox) = setup();
    let file = root.join("att.txt");
    std::fs::write(&file, "data").unwrap();

    let rel = sandbox
        .validate_attachments(&[file], &AttachmentLimits::default())
        .unwrap();
    assert_eq!(rel, vec![PathBuf::from("att.txt")]);
}
