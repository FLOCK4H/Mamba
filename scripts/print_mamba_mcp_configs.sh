#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/print_mamba_mcp_configs.sh [options]

Print ready-to-paste MCP client snippets for this Mamba checkout.

Options:
  --name <server-name>   MCP server name to emit (default: mamba)
  --bin <path>           Absolute or relative path to mamba_mcp binary
  --api-url <url>        MAMBA_MCP_API_URL value to embed
  --api-key <key>        MAMBA_MCP_API_KEY value to embed
  -h, --help             Show this help text
EOF
}

json_escape() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\t'/\\t}"
  printf '%s' "${value}"
}

shell_quote() {
  printf '%q' "$1"
}

abspath() {
  local path="$1"
  if command -v realpath >/dev/null 2>&1; then
    realpath "${path}"
    return
  fi
  python3 - "$path" <<'PY'
import os
import sys

print(os.path.abspath(sys.argv[1]))
PY
}

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"

server_name="mamba"
bin_path="${repo_root}/target/debug/mamba_mcp"
api_url="${MAMBA_MCP_API_URL:-http://127.0.0.1:8787/mamba-api/v1}"
api_key="${MAMBA_MCP_API_KEY:-${MAMBA_API_KEY:-change_me}}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --name)
      [[ $# -ge 2 ]] || { echo "missing value for --name" >&2; exit 2; }
      server_name="$2"
      shift 2
      ;;
    --bin)
      [[ $# -ge 2 ]] || { echo "missing value for --bin" >&2; exit 2; }
      bin_path="$2"
      shift 2
      ;;
    --api-url)
      [[ $# -ge 2 ]] || { echo "missing value for --api-url" >&2; exit 2; }
      api_url="$2"
      shift 2
      ;;
    --api-key)
      [[ $# -ge 2 ]] || { echo "missing value for --api-key" >&2; exit 2; }
      api_key="$2"
      shift 2
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

if [[ "${bin_path}" != /* ]]; then
  bin_path="$(cd -- "${repo_root}" && abspath "${bin_path}")"
fi

json_bin="$(json_escape "${bin_path}")"
json_api_url="$(json_escape "${api_url}")"
json_api_key="$(json_escape "${api_key}")"

cat <<EOF
Mamba MCP client snippets
=========================

Repo root: ${repo_root}
Binary:    ${bin_path}
API URL:   ${api_url}
API key:   ${api_key}

Codex
-----
$(shell_quote codex) $(shell_quote mcp) $(shell_quote add) $(shell_quote "${server_name}") \\
  --env $(shell_quote "MAMBA_MCP_API_URL=${api_url}") \\
  --env $(shell_quote "MAMBA_MCP_API_KEY=${api_key}") \\
  -- $(shell_quote "${bin_path}")

Claude Code
-----------
claude mcp add-json ${server_name} \\
  '{"type":"stdio","command":"${json_bin}","args":[],"env":{"MAMBA_MCP_API_URL":"${json_api_url}","MAMBA_MCP_API_KEY":"${json_api_key}"}}'

Claude Desktop (claude_desktop_config.json fragment)
----------------------------------------------------
{
  "mcpServers": {
    "${server_name}": {
      "command": "${json_bin}",
      "args": [],
      "env": {
        "MAMBA_MCP_API_URL": "${json_api_url}",
        "MAMBA_MCP_API_KEY": "${json_api_key}"
      }
    }
  }
}

Gemini CLI
----------
gemini mcp add \\
  -e MAMBA_MCP_API_URL=$(shell_quote "${api_url}") \\
  -e MAMBA_MCP_API_KEY=$(shell_quote "${api_key}") \\
  ${server_name} $(shell_quote "${bin_path}")

OpenClaw
--------
openclaw mcp set ${server_name} \\
  '{"command":"${json_bin}","args":[],"env":{"MAMBA_MCP_API_URL":"${json_api_url}","MAMBA_MCP_API_KEY":"${json_api_key}"}}'

Generic stdio client JSON
-------------------------
{
  "mcpServers": {
    "${server_name}": {
      "command": "${json_bin}",
      "args": [],
      "env": {
        "MAMBA_MCP_API_URL": "${json_api_url}",
        "MAMBA_MCP_API_KEY": "${json_api_key}"
      }
    }
  }
}
EOF
