# MCP Client Setup

This page covers the current install path for Mamba's stdio MCP server across common agent clients:

- Codex
- Claude Code
- Claude Desktop
- Gemini CLI
- OpenClaw
- any generic stdio MCP client

## Prerequisites

Start the authenticated API first:

```bash
cp .env.example .env
cargo run --bin mamba_api
```

Build the MCP binary:

```bash
cargo build --bin mamba_mcp
```

Canonical MCP environment values:

- `MAMBA_MCP_API_URL=http://127.0.0.1:8787/mamba-api/v1`
- `MAMBA_MCP_API_KEY=<same value as MAMBA_API_KEY>`

Snippet generator for the current checkout:

```bash
./scripts/print_mamba_mcp_configs.sh
```

## Codex

Codex CLI supports local stdio MCP servers directly. Use the built binary path:

```bash
codex mcp add mamba \
  --env MAMBA_MCP_API_URL=http://127.0.0.1:8787/mamba-api/v1 \
  --env MAMBA_MCP_API_KEY="$MAMBA_API_KEY" \
  -- /absolute/path/to/mamba/target/debug/mamba_mcp
```

Verify it:

```bash
codex mcp list
```

## Claude Code

Use Claude Code's JSON-based stdio registration:

```bash
claude mcp add-json mamba \
  '{"type":"stdio","command":"/absolute/path/to/mamba/target/debug/mamba_mcp","args":[],"env":{"MAMBA_MCP_API_URL":"http://127.0.0.1:8787/mamba-api/v1","MAMBA_MCP_API_KEY":"'"$MAMBA_API_KEY"'"}}'
```

Project-file variant: `.mcp.json` with the same `mcpServers.mamba` object.

## Claude Desktop

There are two viable paths:

1. Current direct local setup: add a local stdio server entry to `claude_desktop_config.json`.
2. Future one-click setup: ship a `.mcpb` bundle and install it through `Settings > Extensions > Install Extension...`.

Current local stdio config fragment:

```json
{
  "mcpServers": {
    "mamba": {
      "command": "/absolute/path/to/mamba/target/debug/mamba_mcp",
      "args": [],
      "env": {
        "MAMBA_MCP_API_URL": "http://127.0.0.1:8787/mamba-api/v1",
        "MAMBA_MCP_API_KEY": "change_me"
      }
    }
  }
}
```

## Gemini CLI

Gemini CLI can add stdio servers from the shell:

```bash
gemini mcp add \
  -e MAMBA_MCP_API_URL=http://127.0.0.1:8787/mamba-api/v1 \
  -e MAMBA_MCP_API_KEY="$MAMBA_API_KEY" \
  mamba /absolute/path/to/mamba/target/debug/mamba_mcp
```

Verify it:

```bash
gemini mcp list
```

## OpenClaw

OpenClaw stores MCP server definitions in its own config registry:

```bash
openclaw mcp set mamba \
  '{"command":"/absolute/path/to/mamba/target/debug/mamba_mcp","args":[],"env":{"MAMBA_MCP_API_URL":"http://127.0.0.1:8787/mamba-api/v1","MAMBA_MCP_API_KEY":"change_me"}}'
```

Inspect it:

```bash
openclaw mcp show mamba --json
```

## Generic stdio MCP clients

Clients that accept a plain stdio server object can use this shape:

```json
{
  "mcpServers": {
    "mamba": {
      "command": "/absolute/path/to/mamba/target/debug/mamba_mcp",
      "args": [],
      "env": {
        "MAMBA_MCP_API_URL": "http://127.0.0.1:8787/mamba-api/v1",
        "MAMBA_MCP_API_KEY": "change_me"
      }
    }
  }
}
```

## Notes

- Use the built binary path, not `cargo run --bin mamba_mcp`, for client integrations.
- Keep signing in `mamba_api`; the MCP server never returns private keys.
- The local MCP bridge is stdio-based today. Clients that only support remote MCP require a future hosted surface rather than the local binary.
