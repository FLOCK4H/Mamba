# MCP Client Setup

Connect any MCP-capable agent to Mamba's local Solana DEX toolkit. The `mamba_mcp` binary is a stdio-based bridge that forwards tool calls to `mamba_api` over HTTP on your machine. Private keys never leave `mamba_api`.

## Prerequisites

**1. Start the API**

```bash
cp .env.example .env   # set MAMBA_API_KEY and other values
cargo run --bin mamba_api
```

**2. Build the MCP binary**

```bash
cargo build --bin mamba_mcp
```

The built binary lives at `target/debug/mamba_mcp` (or `target/release/mamba_mcp` with `--release`). Use the binary path in all client configs below, not `cargo run`.

**3. Note the environment variables**

| Variable | Value |
|---|---|
| `MAMBA_MCP_API_URL` | `http://127.0.0.1:8787/mamba-api/v1` |
| `MAMBA_MCP_API_KEY` | Same value as `MAMBA_API_KEY` in your `.env` |

To auto-generate config snippets for your current checkout:

```bash
./scripts/print_mamba_mcp_configs.sh
```

---

## Codex

Register a local stdio server with `codex mcp add`:

```bash
codex mcp add mamba \
  --env MAMBA_MCP_API_URL=http://127.0.0.1:8787/mamba-api/v1 \
  --env MAMBA_MCP_API_KEY="$MAMBA_API_KEY" \
  -- /absolute/path/to/mamba/target/debug/mamba_mcp
```

Verify the registration:

```bash
codex mcp list
```

---

## Claude Code

Claude Code uses JSON-based stdio registration:

```bash
claude mcp add-json mamba \
  '{"type":"stdio","command":"/absolute/path/to/mamba/target/debug/mamba_mcp","args":[],"env":{"MAMBA_MCP_API_URL":"http://127.0.0.1:8787/mamba-api/v1","MAMBA_MCP_API_KEY":"'"$MAMBA_API_KEY"'"}}'
```

You can also add a `.mcp.json` file at the project root with the same `mcpServers.mamba` object for per-project configuration.

---

## Claude Desktop

Add a local stdio server entry to `claude_desktop_config.json`:

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

A future option is one-click install via a `.mcpb` bundle through **Settings > Extensions > Install Extension**.

---

## Gemini CLI

Add the server from your shell:

```bash
gemini mcp add \
  -e MAMBA_MCP_API_URL=http://127.0.0.1:8787/mamba-api/v1 \
  -e MAMBA_MCP_API_KEY="$MAMBA_API_KEY" \
  mamba /absolute/path/to/mamba/target/debug/mamba_mcp
```

Verify the registration:

```bash
gemini mcp list
```

---

## OpenClaw

OpenClaw stores server definitions in its own config registry:

```bash
openclaw mcp set mamba \
  '{"command":"/absolute/path/to/mamba/target/debug/mamba_mcp","args":[],"env":{"MAMBA_MCP_API_URL":"http://127.0.0.1:8787/mamba-api/v1","MAMBA_MCP_API_KEY":"change_me"}}'
```

Inspect the stored config:

```bash
openclaw mcp show mamba --json
```

---

## Generic stdio Clients

Any client that accepts a standard stdio server object can use this shape:

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

---

## Notes

- Always point clients at the **built binary** (`target/debug/mamba_mcp`), not at `cargo run --bin mamba_mcp`.
- All signing stays in `mamba_api`. The MCP bridge never handles or returns private keys.
- The bridge is stdio-based. Clients that require a remote HTTP transport will need a future hosted surface instead of the local binary.
