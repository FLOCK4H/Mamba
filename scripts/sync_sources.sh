#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UPSTREAM_DIR="${ROOT_DIR}/external/upstreams"
LOCK_FILE="${ROOT_DIR}/UPSTREAM_SOURCES.lock"

mkdir -p "${UPSTREAM_DIR}"

SOURCES=(
  "pump_public_docs|https://github.com/pump-fun/pump-public-docs.git"
  "meteora_docs|https://github.com/MeteoraAg/docs.git"
  "meteora_dlmm_sdk|https://github.com/MeteoraAg/dlmm-sdk.git"
  "damm-v1-sdk|https://github.com/MeteoraAg/damm-v1-sdk.git"
  "meteora_damm_v2|https://github.com/MeteoraAg/damm-v2.git"
  "meteora_damm_v2_sdk|https://github.com/MeteoraAg/damm-v2-sdk.git"
  "meteora_dbc|https://github.com/MeteoraAg/dynamic-bonding-curve.git"
  "raydium_docs|https://github.com/raydium-io/raydium-docs.git"
  "raydium_sdk_v2|https://github.com/raydium-io/raydium-sdk-V2.git"
  "raydium_cp_swap|https://github.com/raydium-io/raydium-cp-swap.git"
  "raydium_amm|https://github.com/raydium-io/raydium-amm.git"
  "raydium_clmm|https://github.com/raydium-io/raydium-clmm.git"
)

tmp_rows="$(mktemp)"

for entry in "${SOURCES[@]}"; do
  name="${entry%%|*}"
  url="${entry#*|}"
  repo_dir="${UPSTREAM_DIR}/${name}"

  if [[ -d "${repo_dir}/.git" ]]; then
    git -C "${repo_dir}" fetch --prune --tags origin

    default_branch="$(git -C "${repo_dir}" symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's#^origin/##' || true)"
    if [[ -z "${default_branch}" ]]; then
      default_branch="$(git -C "${repo_dir}" remote show origin | sed -n '/HEAD branch/s/.*: //p' || true)"
    fi
    if [[ -z "${default_branch}" ]]; then
      default_branch="main"
    fi

    if git -C "${repo_dir}" show-ref --verify --quiet "refs/remotes/origin/${default_branch}"; then
      git -C "${repo_dir}" checkout -B "${default_branch}" "origin/${default_branch}" >/dev/null 2>&1
    fi
  else
    git clone --depth 1 "${url}" "${repo_dir}"
  fi

  branch="$(git -C "${repo_dir}" rev-parse --abbrev-ref HEAD)"
  commit="$(git -C "${repo_dir}" rev-parse HEAD)"
  printf "%s|%s|%s|%s\n" "${name}" "${url}" "${branch}" "${commit}" >> "${tmp_rows}"
done

generated_at="$(date -u +"%Y-%m-%d %H:%M:%S UTC")"

{
  echo "# Upstream Sources Lock"
  echo
  echo "Generated: ${generated_at}"
  echo "Script: scripts/sync_sources.sh"
  echo
  echo "| Source | URL | Branch | Commit |"
  echo "| --- | --- | --- | --- |"
  while IFS='|' read -r name url branch commit; do
    echo "| ${name} | ${url} | ${branch} | ${commit} |"
  done < "${tmp_rows}"
  echo
  echo "This file is generated. Re-run scripts/sync_sources.sh to refresh."
} > "${LOCK_FILE}"

rm -f "${tmp_rows}"
echo "Updated upstream sources and wrote ${LOCK_FILE}"
