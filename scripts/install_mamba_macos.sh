#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/install_mamba_macos.sh [options]

Installs the host dependencies needed to build Mamba binaries on macOS,
installs the Rust toolchain pinned in `rust-toolchain.toml`, refreshes upstream
mirrors, and optionally runs a locked build.

Options:
  --rust-toolchain <version>  Override the Rust toolchain channel/version.
  --skip-homebrew-packages    Do not install Homebrew packages.
  --skip-sync                 Do not run scripts/sync_sources.sh.
  --skip-build                Do not run cargo build --locked for mamba, mamba_api, and mamba_mcp.
  --dry-run                   Print the actions without executing them.
  -h, --help                  Show this help text.
EOF
}

DRY_RUN=0
SKIP_HOMEBREW_PACKAGES=0
SKIP_SYNC=0
SKIP_BUILD=0
RUST_TOOLCHAIN_OVERRIDE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rust-toolchain)
      if [[ $# -lt 2 ]]; then
        echo "missing value for --rust-toolchain" >&2
        exit 2
      fi
      RUST_TOOLCHAIN_OVERRIDE="$2"
      shift 2
      ;;
    --skip-homebrew-packages)
      SKIP_HOMEBREW_PACKAGES=1
      shift
      ;;
    --skip-sync)
      SKIP_SYNC=1
      shift
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

run() {
  printf '+'
  printf ' %q' "$@"
  printf '\n'
  if [[ "${DRY_RUN}" -eq 0 ]]; then
    "$@"
  fi
}

require_file() {
  local path="$1"
  if [[ ! -f "${path}" ]]; then
    echo "required file not found: ${path}" >&2
    exit 1
  fi
}

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"

require_file "${repo_root}/Cargo.toml"
require_file "${repo_root}/rust-toolchain.toml"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "this bootstrap script is only for macOS" >&2
  exit 2
fi

toolchain="${RUST_TOOLCHAIN_OVERRIDE}"
if [[ -z "${toolchain}" ]]; then
  toolchain="$(awk -F'"' '/^channel = / { print $2; exit }' "${repo_root}/rust-toolchain.toml")"
fi
if [[ -z "${toolchain}" ]]; then
  echo "failed to determine Rust toolchain from rust-toolchain.toml" >&2
  exit 1
fi

if [[ "${SKIP_HOMEBREW_PACKAGES}" -eq 0 ]]; then
  if ! command -v brew >/dev/null 2>&1; then
    echo "Homebrew is required for macOS bootstrap: https://brew.sh" >&2
    exit 1
  fi
  run brew update
  run brew install git pkg-config python
fi

if ! command -v rustup >/dev/null 2>&1; then
  if ! command -v curl >/dev/null 2>&1; then
    echo "curl is required to install rustup on macOS" >&2
    exit 1
  fi
  if [[ "${DRY_RUN}" -eq 0 ]]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "${toolchain}"
  else
    echo "+ curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${toolchain}"
  fi
fi

export PATH="${HOME}/.cargo/bin:${PATH}"

run rustup toolchain install "${toolchain}" --profile minimal
run rustup default "${toolchain}"

if [[ "${SKIP_SYNC}" -eq 0 ]]; then
  run "${repo_root}/scripts/sync_sources.sh"
fi

if [[ "${SKIP_BUILD}" -eq 0 ]]; then
  run cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp
fi
