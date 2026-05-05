#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/install_mamba_linux.sh [options]

Installs the host dependencies needed to build Mamba binaries on Linux,
installs the Rust toolchain pinned in `rust-toolchain.toml`, refreshes upstream
mirrors, and optionally runs a locked build.

Options:
  --rust-toolchain <version>  Override the Rust toolchain channel/version.
  --skip-system-packages      Do not install distro packages.
  --skip-sync                 Do not run scripts/sync_sources.sh.
  --skip-build                Do not run cargo build --locked for mamba, mamba_api, and mamba_mcp.
  --dry-run                   Print the actions without executing them.
  -h, --help                  Show this help text.

Run this script as a normal user. It uses sudo for system package installs.
EOF
}

DRY_RUN=0
SKIP_SYSTEM_PACKAGES=0
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
    --skip-system-packages)
      SKIP_SYSTEM_PACKAGES=1
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

if [[ "${EUID}" -eq 0 ]]; then
  echo "run this script as a normal user; it will use sudo for package installs" >&2
  exit 2
fi

require_file "${repo_root}/Cargo.toml"
require_file "${repo_root}/rust-toolchain.toml"

toolchain="${RUST_TOOLCHAIN_OVERRIDE}"
if [[ -z "${toolchain}" ]]; then
  toolchain="$(awk -F'"' '/^channel = / { print $2; exit }' "${repo_root}/rust-toolchain.toml")"
fi
if [[ -z "${toolchain}" ]]; then
  echo "failed to determine Rust toolchain from rust-toolchain.toml" >&2
  exit 1
fi

if [[ "${SKIP_SYSTEM_PACKAGES}" -eq 0 ]]; then
  if [[ ! -r /etc/os-release ]]; then
    echo "unsupported Linux distribution: missing /etc/os-release" >&2
    exit 1
  fi

  # shellcheck disable=SC1091
  . /etc/os-release

  if ! command -v sudo >/dev/null 2>&1; then
    echo "sudo is required to install system packages" >&2
    exit 1
  fi
  if ! sudo -n true >/dev/null 2>&1; then
    echo "passwordless sudo is required for non-interactive package installation" >&2
    exit 1
  fi

  sudo_cmd=(sudo -n)
  packages=()
  package_manager=""

  case "${ID}" in
    ubuntu|debian|linuxmint|pop)
      package_manager="apt"
      packages=(
        build-essential
        ca-certificates
        curl
        git
        libssl-dev
        libwayland-dev
        libxcb-render0-dev
        libxcb-shape0-dev
        libxcb-xfixes0-dev
        libxcb1-dev
        libxkbcommon-dev
        perl
        python3
        python3-venv
        pkg-config
      )
      ;;
    fedora|rhel|centos|rocky|almalinux)
      package_manager="dnf"
      packages=(
        ca-certificates
        curl
        gcc
        gcc-c++
        git
        libxkbcommon-devel
        libxcb-devel
        make
        openssl-devel
        perl-core
        python3
        pkgconf-pkg-config
        wayland-devel
      )
      ;;
    arch|manjaro)
      package_manager="pacman"
      packages=(
        base-devel
        ca-certificates
        curl
        git
        libxcb
        libxkbcommon
        openssl
        perl
        python
        pkgconf
        wayland
      )
      ;;
    opensuse*|sles)
      package_manager="zypper"
      packages=(
        ca-certificates
        curl
        gcc
        gcc-c++
        git
        libopenssl-devel
        libwayland-devel
        libxcb-devel
        libxkbcommon-devel
        make
        perl
        python3
        pkg-config
      )
      ;;
    *)
      case "${ID_LIKE:-}" in
        *debian*)
          package_manager="apt"
          packages=(
            build-essential
            ca-certificates
            curl
            git
            libssl-dev
            libwayland-dev
            libxcb-render0-dev
            libxcb-shape0-dev
            libxcb-xfixes0-dev
            libxcb1-dev
            libxkbcommon-dev
            perl
            python3
            python3-venv
            pkg-config
          )
          ;;
        *rhel*|*fedora*)
          package_manager="dnf"
          packages=(
            ca-certificates
            curl
            gcc
            gcc-c++
            git
            libxkbcommon-devel
            libxcb-devel
            make
            openssl-devel
            perl-core
            python3
            pkgconf-pkg-config
            wayland-devel
          )
          ;;
        *arch*)
          package_manager="pacman"
          packages=(
            base-devel
            ca-certificates
            curl
            git
            libxcb
            libxkbcommon
            openssl
            perl
            python
            pkgconf
            wayland
          )
          ;;
        *suse*)
          package_manager="zypper"
          packages=(
            ca-certificates
            curl
            gcc
            gcc-c++
            git
            libopenssl-devel
            libwayland-devel
            libxcb-devel
            libxkbcommon-devel
            make
            perl
            python3
            pkg-config
          )
          ;;
        *)
          echo "unsupported Linux distribution: ID=${ID} ID_LIKE=${ID_LIKE:-}" >&2
          exit 1
          ;;
      esac
      ;;
  esac

  case "${package_manager}" in
    apt)
      run "${sudo_cmd[@]}" apt-get update
      run "${sudo_cmd[@]}" env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${packages[@]}"
      ;;
    dnf)
      run "${sudo_cmd[@]}" dnf install -y "${packages[@]}"
      ;;
    pacman)
      run "${sudo_cmd[@]}" pacman -Sy --needed --noconfirm "${packages[@]}"
      ;;
    zypper)
      run "${sudo_cmd[@]}" zypper --non-interactive install --no-recommends "${packages[@]}"
      ;;
    *)
      echo "internal error: unsupported package manager ${package_manager}" >&2
      exit 1
      ;;
  esac
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to install rustup" >&2
  exit 1
fi

if [[ ! -x "${HOME}/.cargo/bin/rustup" ]] && ! command -v rustup >/dev/null 2>&1; then
  run bash -lc "curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal --default-toolchain '${toolchain}'"
fi

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck disable=SC1090
  . "${HOME}/.cargo/env"
fi
export PATH="${HOME}/.cargo/bin:${PATH}"

if ! command -v rustup >/dev/null 2>&1; then
  echo "rustup is not available after installation" >&2
  exit 1
fi

run rustup toolchain install "${toolchain}" --profile minimal
run rustup default "${toolchain}"

if [[ "${SKIP_SYNC}" -eq 0 ]]; then
  run bash -lc "cd '${repo_root}' && scripts/sync_sources.sh"
fi

if [[ "${SKIP_BUILD}" -eq 0 ]]; then
  run bash -lc "cd '${repo_root}' && cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp"
fi

echo
echo "Linux host setup complete."
echo "Rust toolchain: ${toolchain}"
echo "Repo root: ${repo_root}"
if [[ "${SKIP_BUILD}" -eq 0 ]]; then
  echo "Validated with: cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp"
else
  echo "Build validation skipped."
fi
