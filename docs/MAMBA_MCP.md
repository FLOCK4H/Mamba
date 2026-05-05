# Mamba MCP Server

`mamba_mcp` is a stdio MCP bridge on top of Mamba's authenticated local API. It is designed for agent control without exposing private keys: the agent talks MCP, MCP talks HTTP to `mamba_api`, and Mamba performs any signing locally.

The MCP layer is intended to be self-sufficient. Agents should not need `solana` CLI or any extra host packages for wallet balance checks, metadata lookup, Launchpad config discovery, or transaction planning that the local API already supports.

RPC behavior is owned by `mamba_api`, not by the MCP bridge. Whatever multi-RPC pool you configure for the API is the same pool MCP tools inherit for reads, simulations, wallet routes, create flows, and execute flows.

## Why it exists

- Agents can discover tokens, creators, subscriptions, and routes through normal MCP tools.
- Agents can schedule buys, sells, transfers, cleanup, token creation, and pool actions without direct key access.
- The same safety controls used by the local API still apply:
  - authenticated access through `MAMBA_API_KEY`
  - build-first behavior by default
  - live sends only when `MAMBA_API_ENABLE_LIVE_SENDS=true`
  - signer material stays inside Mamba through `MAMBA_PRIVATE_KEY` (legacy `MAMBA_API_PRIVATE_KEY` still accepted) or the managed wallet store

## Architecture

```text
Agent MCP client
  -> stdio
    -> mamba_mcp
      -> authenticated HTTP
        -> mamba_api
          -> local signer / managed wallet store
            -> Solana RPC
```

Important contract:

- MCP tool responses are wrapped as `{ "data": ... }`.
- `mamba_mcp` never returns private keys.
- Execute tools only submit transactions when the backing API is already unlocked and has a local signer available.
- The MCP server exposes a built-in decision resource at `mamba://tool-playbook` with canonical tool-selection rules for common intents.

## Start it

Start the authenticated API first:

```bash
export MAMBA_API_KEY='mamba_test_key'
export MAMBA_PRIVATE_KEY='<base58-64-byte-keypair-or-json-[u8;64]>'
cargo run --bin mamba_api
```

If you care about mainnet websocket throughput, set `MAMBA_API_HTTP_URLS` and `MAMBA_API_WS_URLS` before starting the API. Use comma-separated same-cluster lists and prefer at least 3 HTTP RPCs across 2 providers for sustained MCP/API/TUI activity.

Build the MCP binary first:

```bash
cargo build --bin mamba_mcp
```

Optional MCP-specific environment variables:

- `MAMBA_MCP_API_URL`
  - default: `http://127.0.0.1:8787/mamba-api/v1`
  - `MAMBA_API_BASE_URL` is also accepted as a fallback
- `MAMBA_MCP_API_KEY`
  - falls back to `MAMBA_API_KEY`
- `MAMBA_MCP_TIMEOUT_SECS`
  - default: `30`
- `AUTO_ACCEPT_LOW_LQ_POOLS`
  - default: `false`
  - when `false`, MCP execute calls default to `skip_low_lq_pools=true` unless the caller explicitly overrides after user confirmation

Client-specific installation recipes for Codex, Claude Code, Claude Desktop, Gemini CLI, and OpenClaw live in `docs/MCP_CLIENT_SETUP.md`.

## Client config example

Codex / GUI MCP configuration to use:

| Field | Recommended value |
| --- | --- |
| Command to launch | Absolute path to the built binary, for example `/absolute/path/to/mamba/target/debug/mamba_mcp` |
| Arguments | Leave empty |
| Environment variables | `MAMBA_MCP_API_URL=http://127.0.0.1:8787/mamba-api/v1` and `MAMBA_MCP_API_KEY=<same value as MAMBA_API_KEY>` |
| Working directory | Absolute repo root, for example `/absolute/path/to/mamba` |

Your working Codex settings screenshot follows that shape exactly:

- command points straight at the built `mamba_mcp` binary
- arguments are empty
- MCP API URL and key are passed explicitly
- working directory stays at the repository root

Why the binary path should be used for GUI client launch:

- GUI MCP launchers are not interactive shells. They may not inherit the same Rust toolchain PATH or `rustup` environment as your terminal.
- `cargo run --bin mamba_mcp` adds an extra layer of command parsing and toolchain resolution that the client does not need.
- Some GUI clients serialize arguments differently from a shell; using the binary with no args removes the most common failure mode completely.
- Rebuilding after code changes is explicit: run `cargo build --bin mamba_mcp`, then reconnect the client to the same binary path.

Codex-specific note:

- The binary path is the preferred configuration even if `cargo run --bin mamba_mcp` works in a manual terminal.
- If you insist on using Cargo in Codex, each argv entry must be separate:
  - `run`
  - `--bin`
  - `mamba_mcp`
- A single row containing `run --bin mamba_mcp` is wrong and Cargo treats it as an unknown subcommand.

Generic stdio MCP client configuration with the recommended binary launch:

```json
{
  "mcpServers": {
    "mamba": {
      "command": "/absolute/path/to/mamba/target/debug/mamba_mcp",
      "args": [],
      "cwd": "/absolute/path/to/mamba",
      "env": {
        "MAMBA_MCP_API_URL": "http://127.0.0.1:8787/mamba-api/v1",
        "MAMBA_MCP_API_KEY": "mamba_test_key"
      }
    }
  }
}
```

Local terminal dev-only launch:

```bash
export MAMBA_MCP_API_KEY="$MAMBA_API_KEY"
./target/debug/mamba_mcp
```

If you are actively editing MCP code and want Cargo to rebuild and run it in one shot from a shell, this also works:

```bash
export MAMBA_MCP_API_KEY="$MAMBA_API_KEY"
cargo run --bin mamba_mcp
```

That Cargo form is convenient for local development, but the built binary is still the preferred client launcher for Codex and other GUI MCP tools.

## Tool groups

Discovery and health:

- `api_docs`
- `health`
- `list_supported_markets`

Market data and provenance:

- `list_tokens`
- `get_token_details`
- `batch_get_token_metadata`
- `list_creators`
- `list_creator_mints`
- `list_transactions`

Trade control:

- `buy_token`
- `sell_token`

Wallet control:

- `list_wallets`
- `get_active_wallet`
- `get_wallet_balance`
- `create_wallet`
- `select_wallets`
- `transfer_asset`
- `preview_wallet_clean`
- `clean_wallet`

Create and pool control:

- `list_create_methods`
- `list_raydium_launchpad_global_configs`
- `list_raydium_launchpad_platform_configs`
- `list_raydium_launchpad_platform_curve_params`
- `create_token`
- `list_pool_methods`
- `create_pool`
- `list_pool_positions`
- `manage_pool_position`

Subscription control:

- `subscribe_market`
- `unsubscribe_market`
- `list_subscriptions`

Escape hatch:

- `call_mamba_api`
  - lets MCP clients call any authenticated Mamba API route before a dedicated MCP tool exists
  - this keeps the MCP layer maintainable while the HTTP API evolves

## Selection rules

Use these defaults so the agent picks one canonical tool per intent instead of guessing:

- Current wallet or live SOL balance: `get_active_wallet`
- Arbitrary wallet pubkey balance: `get_wallet_balance`
- Managed wallet list/labels/selection state: `list_wallets`
- Broad token discovery from websocket cache: `subscribe_market`, then `list_tokens`
- Single mint route/creator/metadata: `get_token_details`
- Metadata for many mints: `batch_get_token_metadata`
- Creator discovery: `list_creators`
- Mints for one creator: `list_creator_mints`
- Buy or sell planning/execution: `buy_token`, `sell_token`
  - read `route.low_lq` from `get_token_details` first when the mint is unknown or route quality matters
- Direct SOL or SPL transfer: `transfer_asset`
- Cleanup inspection first: `preview_wallet_clean`
- Cleanup build/execute after inspection: `clean_wallet`
- Discover create methods first: `list_create_methods`
- Raydium Launchpad create planning: `list_raydium_launchpad_global_configs`, `list_raydium_launchpad_platform_configs`, `list_raydium_launchpad_platform_curve_params`
- Token create build/execute: `create_token`
- Discover pool methods first: `list_pool_methods`
- Existing pool positions: `list_pool_positions`
- Pool withdrawal/management: `manage_pool_position`
- Raw authenticated HTTP fallback only when no dedicated tool exists: `call_mamba_api`

General safety rules:

- Prefer dedicated tools over `call_mamba_api`.
- Prefer read-only tools first, then build mode, then execute mode only when the user clearly wants submission.
- Default `execute` to `false` for buy, sell, transfer, cleaner, create, pool-create, and pool-manage tasks unless the user clearly wants a live send.
- If `get_token_details` reports `route.low_lq=true`, ask the user before a live buy/sell unless `AUTO_ACCEPT_LOW_LQ_POOLS=true`.
- Do not fall back to `solana` CLI or host-installed packages for balances, route discovery, metadata lookup, or planning that Mamba already supports.

## Common patterns

List tokens from live websocket cache:

```json
{
  "tool": "list_tokens",
  "arguments": {
    "markets": ["pump_fun", "pump_swap"],
    "limit": 25
  }
}
```

Read the active wallet and its live SOL balance directly from Mamba:

```json
{
  "tool": "get_active_wallet",
  "arguments": {}
}
```

Resolve canonical metadata for multiple mints without reimplementing lookup logic client-side:

```json
{
  "tool": "batch_get_token_metadata",
  "arguments": {
    "mints": ["<MINT_A>", "<MINT_B>"]
  }
}
```

Dry-run a buy without any signing:

```json
{
  "tool": "buy_token",
  "arguments": {
    "mint": "<MINT>",
    "buy_sol": 0.01,
    "priority_fee_level": "medium",
    "slippage_pct": 15,
    "execute": false
  }
}
```

Low-LQ execution behavior:

- `get_token_details` now exposes `route.low_lq` and route liquidity fields directly from the API.
- `buy_token` and `sell_token` accept `skip_low_lq_pools`.
- When `AUTO_ACCEPT_LOW_LQ_POOLS=false`, execute calls default to `skip_low_lq_pools=true` unless the caller explicitly sets `skip_low_lq_pools=false` after the user confirms they want the low-LQ route anyway.

Swap fee override arguments accepted by both `buy_token` and `sell_token`:

- `priority_fee_level`: `env`, `low`, `medium`, `high`, `turbo`, `max`, or `custom`
- `priority_fee_sol`: decimal SOL amount, used only with `priority_fee_level="custom"`

Example custom-fee sell:

```json
{
  "tool": "sell_token",
  "arguments": {
    "mint": "<MINT>",
    "sell_pct": 100,
    "slippage_pct": 15,
    "priority_fee_level": "custom",
    "priority_fee_sol": 0.00003,
    "execute": true
  }
}
```

Execute a transfer where Mamba signs locally:

```json
{
  "tool": "transfer_asset",
  "arguments": {
    "from_wallet": "<SOURCE_PUBKEY>",
    "to_address": "<DESTINATION_PUBKEY>",
    "amount": "0.01",
    "asset_kind": "sol",
    "execute": true
  }
}
```

Create a token while letting Mamba generate the mint signer internally:

```json
{
  "tool": "create_token",
  "arguments": {
    "method": "spl_token",
    "payer": "<PAYER_PUBKEY>",
    "name": "Example Token",
    "symbol": "EXMPL",
    "uri": "https://example.com/token.json",
    "decimals": 6,
    "initial_supply": 1000000,
    "execute": true
  }
}
```

## Devnet and mainnet

- Devnet live validation is supported and is the recommended place to test execute flows.
- Mainnet dry runs are supported through the same tools with `execute=false`.
- Mainnet live sends stay controlled by the underlying API safety gates; enabling the MCP server alone does not unlock them.
- Execute calls cannot redirect a devnet-configured API into mainnet by passing a different `rpc_url`; the backing API rejects cross-cluster live sends.

## Maintenance notes

- Dedicated MCP tools are thin wrappers around the HTTP API so behavior stays centralized in `mamba_api`.
- The raw `call_mamba_api` tool prevents MCP drift when a new API route is added before a dedicated wrapper lands.
- Most execute tools map 1:1 to existing API request and response bodies, which keeps docs, tests, and clients aligned.
- `list_create_methods` and `list_pool_methods` expose `execute_generated_fields` so MCP clients can tell which signer inputs Mamba may generate internally during execute mode.
