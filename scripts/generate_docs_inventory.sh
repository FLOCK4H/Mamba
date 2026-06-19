#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${ROOT}/docs/reference/repository-inventory.md"

if command -v rg >/dev/null 2>&1; then
  exclude_capture_cmd=(rg -v '^Capture\.PNG$')
else
  exclude_capture_cmd=(grep -v '^Capture\.PNG$')
fi

mapfile -t REPO_PATHS < <(
  cd "${ROOT}" &&
    git ls-files --cached --others --exclude-standard \
      | "${exclude_capture_cmd[@]}" \
      | while IFS= read -r path; do
          [[ -e "${path}" ]] && printf '%s\n' "${path}"
        done
) || true

mapfile -t REPO_GROUPS < <(
  printf '%s\n' "${REPO_PATHS[@]}" \
    | awk -F/ '{print (NF == 1 ? "." : $1)}' \
    | sort -u
) || true

indexed_total="${#REPO_PATHS[@]}"
generated_at="$(date -u +"%Y-%m-%d %H:%M:%S UTC")"

{
  echo "# Repository Inventory"
  echo
  echo "Repository paths indexed from \`git ls-files --cached --others --exclude-standard\`."
  echo
  echo "## Coverage"
  echo
  echo "- Indexed paths: ${indexed_total}"
  echo "- Generated at: ${generated_at}"
  echo "- Inventory source: \`git ls-files --cached --others --exclude-standard\`"
  echo "- Ignored paths excluded through \`.gitignore\` (for example \`target/\`, \`.venv-docs/\`, \`.env\`, \`docs-site/\`, \`PACKAGING.md\`, and synced \`external/upstreams/\` mirrors)"
  echo "- Additional local-only exclusion: \`Capture.PNG\`"
  echo
  echo "## Top-level groups"
  echo
  echo "| Group | Indexed paths |"
  echo "| --- | ---: |"

  for group in "${REPO_GROUPS[@]}"; do
    if [[ "${group}" == "." ]]; then
      label="repository root"
      count="$(
        printf '%s\n' "${REPO_PATHS[@]}" \
          | awk 'index($0, "/") == 0' \
          | wc -l \
          | tr -d ' '
      )"
    else
      label="\`${group}/\`"
      count="$(
        printf '%s\n' "${REPO_PATHS[@]}" \
          | awk -v prefix="${group}/" 'index($0, prefix) == 1' \
          | wc -l \
          | tr -d ' '
      )"
    fi
    echo "| ${label} | ${count} |"
  done

  echo
  echo "## Path listing"
  echo
  echo "The sections below expand to the indexed paths currently present in the checkout."
  echo

  for group in "${REPO_GROUPS[@]}"; do
    if [[ "${group}" == "." ]]; then
      heading="Repository root"
      files="$(
        printf '%s\n' "${REPO_PATHS[@]}" \
          | awk 'index($0, "/") == 0'
      )"
      count="$(printf '%s\n' "${files}" | sed '/^$/d' | wc -l | tr -d ' ')"
    else
      heading="\`${group}/\`"
      files="$(
        printf '%s\n' "${REPO_PATHS[@]}" \
          | awk -v prefix="${group}/" 'index($0, prefix) == 1'
      )"
      count="$(printf '%s\n' "${files}" | sed '/^$/d' | wc -l | tr -d ' ')"
    fi

    echo "### ${heading}"
    echo
    echo "<details><summary>Show indexed paths (${count})</summary>"
    echo
    echo '```text'
    printf '%s\n' "${files}"
    echo '```'
    echo
    echo "</details>"
    echo
  done
} > "${OUT}"

echo "wrote ${OUT}"
