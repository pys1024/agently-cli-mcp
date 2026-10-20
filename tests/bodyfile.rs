//! Tests for the `BodyFile` temp-file abstraction used by the write tools.

use agently_cli_mcp::cli_proc::bodyfile::{BodyFile, BodyFormat};
use tempfile::tempdir;

#[test]
fn extension_matches_format() {
    let dir = tempdir().unwrap();

    let plain = BodyFile::create(dir.path(), b"x", BodyFormat::Plain).unwrap();
    let html = BodyFile::create(dir.path(), b"x", BodyFormat::Html).unwrap();
    let markdown = BodyFile::create(dir.path(), b"x", BodyFormat::Markdown).unwrap();

    assert!(plain.relative_path().ends_with(".txt"));
    assert!(html.relative_path().ends_with(".html"));
    assert!(markdown.relative_path().ends_with(".md"));
}

#[test]
fn relative_path_is_under_tmp_dir() {
    let dir = tempdir().unwrap();

    let body = BodyFile::create(dir.path(), b"hello", BodyFormat::Plain).unwrap();

    assert!(body.relative_path().starts_with(".agently-cli-mcp/tmp/"));
    assert!(dir.path().join(body.relative_path()).is_file());
}

#[test]
fn content_is_written_and_verifiable() {
    let dir = tempdir().unwrap();

    let body = BodyFile::create(dir.path(), b"hello body", BodyFormat::Plain).unwrap();

    let on_disk = std::fs::read(dir.path().join(body.relative_path())).unwrap();
    assert_eq!(on_disk, b"hello body");

    body.verify().expect("fresh body file must verify");
}

#[test]
fn verify_detects_tampering() {
    let dir = tempdir().unwrap();

    let body = BodyFile::create(dir.path(), b"original", BodyFormat::Plain).unwrap();
    body.verify().expect("fresh body file must verify");

    std::fs::write(dir.path().join(body.relative_path()), b"tampered").unwrap();
    assert!(body.verify().is_err(), "tampering must fail verification");
}

#[test]
fn remove_deletes_file() {
    let dir = tempdir().unwrap();

    let body = BodyFile::create(dir.path(), b"x", BodyFormat::Plain).unwrap();
    let relative = body.relative_path().to_string();
    let full = dir.path().join(&relative);
    assert!(full.is_file());

    body.remove().expect("remove must succeed");
    assert!(!full.exists(), "file must be gone after remove");
}

#[cfg(unix)]
#[test]
fn body_file_is_private_on_unix() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();

    let body = BodyFile::create(dir.path(), b"secret", BodyFormat::Plain).unwrap();

    let meta = std::fs::metadata(dir.path().join(body.relative_path())).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}
