#!/usr/bin/env bash
#
# Cross-compile agently-cli-mcp for all supported platforms with cargo-zigbuild.
#
# Targets (default: all):
#   linux-x86_64    x86_64-unknown-linux-musl
#   linux-arm64     aarch64-unknown-linux-musl
#   darwin-x86_64   x86_64-apple-darwin
#   darwin-arm64    aarch64-apple-darwin
#   windows-x86_64  x86_64-pc-windows-msvc
#
# Prerequisites:
#   - Rust toolchain with the target std libraries installed
#   - cargo-zigbuild + zig for Linux/macOS targets
#     (cargo install cargo-zigbuild --locked; zig on PATH)
#   - cargo-xwin for the Windows MSVC target
#     (cargo install cargo-xwin --locked)
#   - macOS SDK for darwin targets: SDKROOT / DARWIN_SDK, or ~/MacOSX.sdk
#     (zig does not bundle Apple's SDK)
#
# Usage:
#   ./scripts/build.sh                          # build all five targets
#   ./scripts/build.sh --targets linux-x86_64,darwin-arm64
#   TARGETS=host ./scripts/build.sh             # only the host's target
#   ./scripts/build.sh --no-archive             # binaries only, no dist/
#   ./scripts/build.sh --no-strip
#   ./scripts/build.sh --output dist --jobs 4
#
# Output:
#   dist/agently-cli-mcp-<triple>.tar.gz|.zip
#   dist/checksums.txt
#
# Linux/macOS targets are linked with cargo-zigbuild. The Windows target is
# MSVC and is linked with cargo-xwin, matching the official release target.
# The MSVC build uses the static CRT (`+crt-static`), so no VC++ Redistributable
# is required.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ALL_TARGETS="linux-x86_64 linux-arm64 darwin-x86_64 darwin-arm64 windows-x86_64"
TARGETS="${TARGETS:-all}"
OUT_DIR="${OUT_DIR:-dist}"
DO_ARCHIVE=1
DO_STRIP=1
JOBS=""

usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "${BASH_SOURCE[0]}"
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

warn() {
  printf 'warning: %s\n' "$*" >&2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --targets)
      [ "$#" -ge 2 ] || die "--targets requires a value"
      TARGETS="$2"
      shift 2
      ;;
    --targets=*)
      TARGETS="${1#*=}"
      shift
      ;;
    --output)
      [ "$#" -ge 2 ] || die "--output requires a value"
      OUT_DIR="$2"
      shift 2
      ;;
    --output=*)
      OUT_DIR="${1#*=}"
      shift
      ;;
    --jobs)
      [ "$#" -ge 2 ] || die "--jobs requires a value"
      JOBS="$2"
      shift 2
      ;;
    --jobs=*)
      JOBS="${1#*=}"
      shift
      ;;
    --no-archive)
      DO_ARCHIVE=0
      shift
      ;;
    --no-strip)
      DO_STRIP=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1 (try --help)"
      ;;
  esac
done

target_triple() {
  case "$1" in
    linux-x86_64)   printf '%s' "x86_64-unknown-linux-musl" ;;
    linux-arm64)    printf '%s' "aarch64-unknown-linux-musl" ;;
    darwin-x86_64)  printf '%s' "x86_64-apple-darwin" ;;
    darwin-arm64)   printf '%s' "aarch64-apple-darwin" ;;
    windows-x86_64) printf '%s' "x86_64-pc-windows-msvc" ;;
    *) return 1 ;;
  esac
}

target_ext() {
  case "$1" in
    windows-*) printf '%s' ".exe" ;;
    *) printf '%s' "" ;;
  esac
}

target_archive_ext() {
  case "$1" in
    windows-*) printf '%s' "zip" ;;
    *) printf '%s' "tar.gz" ;;
  esac
}

host_name() {
  local host
  host="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
  case "$host" in
    x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl) printf '%s' "linux-x86_64" ;;
    aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl) printf '%s' "linux-arm64" ;;
    x86_64-apple-darwin) printf '%s' "darwin-x86_64" ;;
    aarch64-apple-darwin) printf '%s' "darwin-arm64" ;;
    x86_64-pc-windows-msvc|x86_64-pc-windows-gnu) printf '%s' "windows-x86_64" ;;
    *) printf '%s' "" ;;
  esac
}

# Numeric major.minor.patch comparison: ver_ge A B is true when A >= B.
ver_ge() {
  awk -v a="$1" -v b="$2" 'BEGIN {
    split(a, x, "."); split(b, y, ".");
    for (i = 1; i <= 3; i++) {
      xv = (x[i] == "" ? 0 : x[i]) + 0;
      yv = (y[i] == "" ? 0 : y[i]) + 0;
      if (xv > yv) exit 0;
      if (xv < yv) exit 1;
    }
    exit 0;
  }'
}

resolve_targets() {
  local raw="$1" item
  SELECTED=""

  case "$raw" in
    all)
      SELECTED="$ALL_TARGETS"
      ;;
    host)
      item="$(host_name)"
      [ -n "$item" ] || die "cannot determine a supported host target (host: $(rustc -vV 2>/dev/null | sed -n 's/^host: //p'))"
      SELECTED="$item"
      ;;
    *)
      SELECTED="$(printf '%s' "$raw" | tr ',' ' ')"
      ;;
  esac

  SELECTED="$(printf '%s\n' $SELECTED | awk 'NF && !seen[$0]++' | tr '\n' ' ' | sed 's/ *$//')"
  [ -n "$SELECTED" ] || die "no targets selected"

  for item in $SELECTED; do
    target_triple "$item" >/dev/null 2>&1 || die "unknown target '$item' (valid: $ALL_TARGETS)"
  done
}

resolve_targets "$TARGETS"

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

command -v rustc >/dev/null 2>&1 || die "rustc is not on PATH"
command -v cargo >/dev/null 2>&1 || die "cargo is not on PATH"

NEEDS_WINDOWS=0
NEEDS_ZIGBUILD=0
for item in $SELECTED; do
  case "$item" in
    windows-*) NEEDS_WINDOWS=1 ;;
    *) NEEDS_ZIGBUILD=1 ;;
  esac
done

if [ "$NEEDS_ZIGBUILD" = 1 ]; then
  command -v zig >/dev/null 2>&1 || die "zig is not on PATH (install it and re-run)"
  if ! cargo zigbuild --help >/dev/null 2>&1; then
    die "cargo-zigbuild is not installed (run: cargo install cargo-zigbuild --locked)"
  fi
fi

if [ "$NEEDS_WINDOWS" = 1 ] && ! cargo xwin build --help >/dev/null 2>&1; then
  die "cargo-xwin is not installed (run: cargo install cargo-xwin --locked)"
fi

if [ "$NEEDS_ZIGBUILD" = 1 ]; then
  ZB_VERSION="$(cargo install --list 2>/dev/null | awk '/^cargo-zigbuild / {print $2; exit}' | sed 's/^v//')"
  ZIG_VERSION="$(zig version 2>/dev/null || true)"
  if [ "$ZB_VERSION" = "0.23.4" ] && [ -n "$ZIG_VERSION" ] && ver_ge "$ZIG_VERSION" "0.16.0"; then
    die "cargo-zigbuild 0.23.4 has a known Darwin linking regression with zig >= 0.16.0.
         Install a fixed cargo-zigbuild first:
           cargo install cargo-zigbuild --version 0.23.3 --locked
         or the upstream fix:
           cargo install --git https://github.com/rust-cross/cargo-zigbuild --locked cargo-zigbuild"
  fi
fi

if command -v rustup >/dev/null 2>&1; then
  INSTALLED_TARGETS="$(rustup target list --installed 2>/dev/null || true)"
  MISSING=""
  for item in $SELECTED; do
    triple="$(target_triple "$item")"
    if ! printf '%s\n' "$INSTALLED_TARGETS" | grep -qx "$triple"; then
      MISSING="$MISSING $triple"
    fi
  done
  if [ -n "$MISSING" ]; then
    die "missing Rust target(s):$MISSING
       Install them first:
         rustup target add$MISSING"
  fi
fi

NEEDS_DARWIN=0
for item in $SELECTED; do
  case "$item" in darwin-*) NEEDS_DARWIN=1 ;; esac
done

if [ "$NEEDS_DARWIN" = 1 ]; then
  DARWIN_SDK_PATH="${SDKROOT:-${DARWIN_SDK:-}}"
  if [ -z "$DARWIN_SDK_PATH" ] && [ "$(uname -s 2>/dev/null || true)" = "Darwin" ]; then
    DARWIN_SDK_PATH="$(xcrun --show-sdk-path 2>/dev/null || true)"
  fi
  if [ -z "$DARWIN_SDK_PATH" ]; then
    DARWIN_SDK_PATH="$HOME/MacOSX.sdk"
  fi
  if [ ! -d "$DARWIN_SDK_PATH" ]; then
    die "macOS SDK not found at '$DARWIN_SDK_PATH'.
       Darwin targets need Apple's SDK (zig does not bundle it).
       Set SDKROOT (or DARWIN_SDK) to a macOS SDK, or place one at ~/MacOSX.sdk."
  fi
  export SDKROOT="$DARWIN_SDK_PATH"
  printf 'Using macOS SDK: %s\n' "$SDKROOT"
fi

if [ -n "$JOBS" ]; then
  case "$JOBS" in
    ''|*[!0-9]*) die "--jobs must be a positive integer" ;;
  esac
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in
  /*) : ;;
  *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac

if [ "$DO_ARCHIVE" = 1 ]; then
  mkdir -p "$OUT_DIR"
fi

printf 'Building targets: %s\n' "$SELECTED"
printf 'Version: %s\n' "$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
printf 'Output: %s\n' "$OUT_DIR"

BUILT=""
FAILED=""
ARCHIVES=()

# ---------------------------------------------------------------------------
# Build, strip, archive
# ---------------------------------------------------------------------------

for item in $SELECTED; do
  triple="$(target_triple "$item")"
  ext="$(target_ext "$item")"
  bin="$TARGET_DIR/$triple/release/agently-cli-mcp$ext"

  printf '\n==> %s (%s)\n' "$item" "$triple"

  build_args="--release --locked --target $triple"
  if [ -n "$JOBS" ]; then
    build_args="$build_args --jobs $JOBS"
  fi

  # Linux/macOS use cargo-zigbuild; Windows MSVC uses cargo-xwin and the
  # static CRT, matching the official release configuration.
  # /ignore:4099 silences LNK4099: the MSVC static libs (libcmt/libvcruntime)
  # carry debug-info references to PDBs that are not shipped with the SDK.
  # shellcheck disable=SC2086
  if [ "$NEEDS_WINDOWS" = 1 ] && case "$item" in windows-*) true ;; *) false ;; esac; then
    if ! RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+crt-static -C link-arg=/ignore:4099" cargo xwin build $build_args; then
      FAILED="$FAILED $item"
      warn "build failed for $item; continuing with remaining targets"
      continue
    fi
  elif ! cargo zigbuild $build_args; then
    FAILED="$FAILED $item"
    warn "build failed for $item; continuing with remaining targets"
    continue
  fi

  [ -f "$bin" ] || {
    FAILED="$FAILED $item"
    warn "expected binary not found: $bin"
    continue
  }

  if [ "$DO_STRIP" = 1 ]; then
    if command -v llvm-strip >/dev/null 2>&1; then
      llvm-strip "$bin" 2>/dev/null || warn "llvm-strip failed for $item; leaving unstripped"
    elif [ "$item" = "$(host_name)" ] && command -v strip >/dev/null 2>&1; then
      strip "$bin" 2>/dev/null || warn "strip failed for $item; leaving unstripped"
    else
      warn "no llvm-strip on PATH; $item will be unstripped"
    fi
  fi

  printf '    %s (%s)\n' "$bin" "$(du -h "$bin" | cut -f1)"

  if [ "$DO_ARCHIVE" = 1 ]; then
    archive="$OUT_DIR/agently-cli-mcp-$triple.$(target_archive_ext "$item")"
    rm -f "$archive"
    case "$(target_archive_ext "$item")" in
      zip)
        if command -v zip >/dev/null 2>&1; then
          zip -q -j "$archive" "$bin"
        elif command -v python3 >/dev/null 2>&1; then
          python3 -m zipfile -c "$archive" "$bin"
        else
          die "zip is required to archive $item (install zip or python3)"
        fi
        ;;
      *)
        tar -czf "$archive" -C "$(dirname "$bin")" "$(basename "$bin")"
        ;;
    esac
    ARCHIVES+=("$archive")
    printf '    archived: %s\n' "$archive"
  fi

  BUILT="$BUILT $item"
done

if [ "$DO_ARCHIVE" = 1 ] && [ -n "$BUILT" ]; then
  printf '\n==> checksums\n'
  (
    cd "$OUT_DIR"
    : > checksums.txt
    for archive in "${ARCHIVES[@]}"; do
      base="$(basename "$archive")"
      if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$base" >> checksums.txt
      else
        shasum -a 256 "$base" >> checksums.txt
      fi
    done
  )
  printf '    %s/checksums.txt\n' "$OUT_DIR"
fi

printf '\n==> summary\n'
for item in $BUILT; do
  printf '  built   %s\n' "$item"
done
for item in $FAILED; do
  printf '  FAILED  %s\n' "$item"
done

if [ -n "$FAILED" ]; then
  exit 1
fi
