# Mamba MCP Server

`mamba_mcp` is a stdio-based MCP bridge that sits in front of Mamba's authenticated local API. Agents talk MCP over stdin/stdout, `mamba_mcp` forwards requests as authenticated HTTP calls to `mamba_api`, and Mamba handles all signing locally. Private keys never leave the process boundary.

The MCP layer is self-sufficient. Agents do not need the `solana` CLI or any host-installed packages for balance checks, metadata lookups, Launchpad config discovery, or transaction planning.

## Architecture

```text
Agent (MCP client)
  -> stdio
    -> mamba_mcp
      -> authenticated HTTP
        -> mamba_api
          -> local signer / managed wallet store
            -> Solana RPC
```

RPC behavior is owned by `mamba_api`. The API-configured multi-RPC pool is inherited by all MCP tools for reads, simulations, wallet operations, create flows, and execution.

### Contracts

| Contract | Detail |
|---|---|
| Response envelope | All MCP tool responses are wrapped as `{ "data": ... }` |
| Key isolation | `mamba_mcp` never returns private keys |
| Execute gating | Transactions are submitted only when the backing API has live sends unlocked and a local signer available |
| Decision resource | Built-in playbook at `mamba://tool-playbook` with canonical tool-selection rules |

## Setup

### 1. Start `mamba_api`

The API must be running before `mamba_mcp` can connect.

```bash
export MAMBA_API_KEY='mamba_test_key'
export MAMBA_PRIVATE_KEY='<base58-64-byte-keypair-or-json-[u8;64]>'
cargo run --bin mamba_api
```

For mainnet websocket throughput, set `MAMBA_API_HTTP_URLS` and `MAMBA_API_WS_URLS` before starting the API. Use comma-separated same-cluster lists and prefer at least 3 HTTP RPCs across 2 providers.

### 2. Build `mamba_mcp`

```bash
cargo build --bin mamba_mcp
```

### Environment Variables

| Variable | Default | Notes |
|---|---|---|
| `MAMBA_MCP_API_URL` | `http://127.0.0.1:8787/mamba-api/v1` | Also accepts `MAMBA_API_BASE_URL` as fallback |
| `MAMBA_MCP_API_KEY` | Falls back to `MAMBA_API_KEY` | Must match the key used by `mamba_api` |
| `MAMBA_MCP_TIMEOUT_SECS` | `30` | Per-request timeout for HTTP calls to the API |
| `AUTO_ACCEPT_LOW_LQ_POOLS` | `false` | When `false`, execute calls default to `skip_low_lq_pools=true` unless the caller explicitly overrides |

## Client Configuration

GUI MCP launchers (Codex, Claude Desktop, etc.) are not interactive shells. They may lack your Rust toolchain PATH or `rustup` environment. Point them at the **built binary** rather than `cargo run`.

Using the binary with no arguments removes the most common failure mode. Rebuild after code changes with `cargo build --bin mamba_mcp`, then reconnect.

### Generic stdio config (JSON)

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

### GUI client field mapping

| Field | Value |
|---|---|
| Command | Absolute path to built binary, e.g. `/absolute/path/to/mamba/target/debug/mamba_mcp` |
| Arguments | Leave empty |
| Environment | `MAMBA_MCP_API_URL=http://127.0.0.1:8787/mamba-api/v1` and `MAMBA_MCP_API_KEY=<same as MAMBA_API_KEY>` |
| Working directory | Absolute repo root |

### Codex note

If using `cargo run` in Codex, each argument must be a separate argv entry: `run`, `--bin`, `mamba_mcp`. A single string `run --bin mamba_mcp` fails because Cargo treats it as an unknown subcommand.

### Local development

```bash
export MAMBA_MCP_API_KEY="$MAMBA_API_KEY"
cargo run --bin mamba_mcp
```

Client-specific setup recipes for Codex, Claude Code, Claude Desktop, Gemini CLI, and OpenClaw are in `docs/MCP_CLIENT_SETUP.md`.

## Tool Reference

### Discovery

| Tool | Purpose |
|---|---|
| `api_docs` | Returns API documentation |
| `health` | Health check for the backing API |
| `list_supported_markets` | Lists all supported DEX markets |

### Market Data

| Tool | Purpose |
|---|---|
| `list_tokens` | Token discovery from the live websocket cache |
| `get_token_details` | Single-mint route, creator, metadata, and liquidity info |
| `batch_get_token_metadata` | Canonical metadata for multiple mints in one call |
| `list_creators` | Creator discovery across tracked markets |
| `list_creator_mints` | All mints launched by a specific creator |
| `list_transactions` | Transaction history for a mint or wallet |

### Trade

| Tool | Purpose |
|---|---|
| `buy_token` | Build or execute a buy transaction |
| `sell_token` | Build or execute a sell transaction |

### Wallet

| Tool | Purpose |
|---|---|
| `list_wallets` | Managed wallet list with labels and selection state |
| `get_active_wallet` | Active wallet pubkey and live SOL balance |
| `get_wallet_balance` | Balance for any wallet pubkey |
| `create_wallet` | Generate a new managed wallet |
| `select_wallets` | Change the active wallet selection |
| `transfer_asset` | SOL or SPL token transfer (signed locally) |
| `preview_wallet_clean` | Inspect reclaimable accounts before cleanup |
| `clean_wallet` | Build or execute account cleanup |

### Create and Pool

| Tool | Purpose |
|---|---|
| `list_create_methods` | Available token creation methods with `execute_generated_fields` |
| `list_raydium_launchpad_global_configs` | Raydium Launchpad global configuration |
| `list_raydium_launchpad_platform_configs` | Raydium Launchpad platform configurations |
| `list_raydium_launchpad_platform_curve_params` | Raydium Launchpad curve parameters |
| `create_token` | Build or execute token creation |
| `list_pool_methods` | Available pool creation methods with `execute_generated_fields` |
| `create_pool` | Build or execute pool creation |
| `list_pool_positions` | Existing pool positions |
| `manage_pool_position` | Withdraw from or manage an existing pool position |

### Subscription

| Tool | Purpose |
|---|---|
| `subscribe_market` | Start a websocket subscription for a market |
| `unsubscribe_market` | Stop a market subscription |
| `list_subscriptions` | List active subscriptions |

### Escape Hatch

| Tool | Purpose |
|---|---|
| `call_mamba_api` | Call any authenticated API route directly. Use only when no dedicated tool exists. |

## Tool Selection Rules

When an agent needs to perform an action, use this table to pick the correct tool. Prefer dedicated tools over `call_mamba_api` in all cases.

| Intent | Tool |
|---|---|
| Active wallet pubkey + SOL balance | `get_active_wallet` |
| Balance for an arbitrary pubkey | `get_wallet_balance` |
| Managed wallet list, labels, selection | `list_wallets` |
| Broad token discovery | `subscribe_market`, then `list_tokens` |
| Single-mint route, creator, metadata | `get_token_details` |
| Metadata for many mints at once | `batch_get_token_metadata` |
| Creator discovery | `list_creators` |
| Mints by a specific creator | `list_creator_mints` |
| Buy or sell (plan or execute) | `buy_token` / `sell_token` |
| SOL or SPL transfer | `transfer_asset` |
| Wallet cleanup inspection | `preview_wallet_clean` |
| Wallet cleanup execution | `clean_wallet` |
| Available token creation methods | `list_create_methods` |
| Raydium Launchpad configs and curves | `list_raydium_launchpad_global_configs`, `list_raydium_launchpad_platform_configs`, `list_raydium_launchpad_platform_curve_params` |
| Token creation | `create_token` |
| Available pool creation methods | `list_pool_methods` |
| View pool positions | `list_pool_positions` |
| Pool withdrawal or management | `manage_pool_position` |
| Raw HTTP fallback (no dedicated tool) | `call_mamba_api` |

### Safety Defaults

- Default `execute` to `false` for buy, sell, transfer, cleaner, create, and pool operations unless the user explicitly requests a live send.
- Prefer read-only tools first, then build mode, then execute mode.
- If `get_token_details` reports `route.low_lq=true`, confirm with the user before a live buy/sell unless `AUTO_ACCEPT_LOW_LQ_POOLS=true`.
- Check `route.low_lq` from `get_token_details` before trading any unfamiliar mint.
- Do not fall back to `solana` CLI or host packages for operations that Mamba already supports.

## Common Patterns

### List tokens from websocket cache

```json
{
  "tool": "list_tokens",
  "arguments": {
    "markets": ["pump_fun", "pump_swap"],
    "limit": 25
  }
}
```

### Check active wallet

```json
{
  "tool": "get_active_wallet",
  "arguments": {}
}
```

### Batch metadata lookup

```json
{
  "tool": "batch_get_token_metadata",
  "arguments": {
    "mints": ["<MINT_A>", "<MINT_B>"]
  }
}
```

### Dry-run a buy (no signing)

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

### Execute a sell with custom priority fee

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

### Transfer SOL

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

### Create a token

Mamba generates the mint signer internally.

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

## Priority Fee Overrides

Both `buy_token` and `sell_token` accept fee override arguments.

| Parameter | Values | Notes |
|---|---|---|
| `priority_fee_level` | `env`, `low`, `medium`, `high`, `turbo`, `max`, `custom` | Controls compute-unit price tier |
| `priority_fee_sol` | Decimal SOL amount | Used only when `priority_fee_level` is `custom` |

## Low-Liquidity Pool Handling

- `get_token_details` exposes `route.low_lq` and route liquidity fields.
- `buy_token` and `sell_token` accept `skip_low_lq_pools`.
- When `AUTO_ACCEPT_LOW_LQ_POOLS=false` (the default), execute calls set `skip_low_lq_pools=true` automatically. The caller must explicitly pass `skip_low_lq_pools=false` after user confirmation to trade on low-LQ routes.

## Devnet and Mainnet

| Scenario | Supported | Notes |
|---|---|---|
| Devnet live execution | Yes | Recommended for testing execute flows |
| Mainnet dry runs | Yes | Use `execute=false` on any tool |
| Mainnet live sends | Yes | Controlled by `mamba_api` safety gates, not by MCP |
| Cross-cluster redirect | No | Execute calls cannot redirect a devnet API to mainnet via `rpc_url` |

Enabling the MCP server alone does not unlock mainnet live sends. The backing API must have `MAMBA_API_ENABLE_LIVE_SENDS=true` independently.

## Maintenance Notes

- MCP tools are thin wrappers around the HTTP API. Behavior stays centralized in `mamba_api`.
- `call_mamba_api` prevents MCP drift when new API routes land before dedicated wrappers are added.
- Most execute tools map 1:1 to existing API request/response bodies, keeping docs, tests, and clients aligned.
- `list_create_methods` and `list_pool_methods` expose `execute_generated_fields` so MCP clients can determine which signer inputs Mamba generates internally during execute mode.
