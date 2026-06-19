# API Reference

`mamba_api` is the authenticated local backend. It runs on your machine, handles signing internally, and exposes HTTP endpoints for market data, swaps, token creation, pool management, wallet operations, and account cleanup.

**Base URL:** `http://127.0.0.1:8787/mamba-api/v1`
**Auth:** `x-api-key: <MAMBA_API_KEY>` header on every request

All examples below assume:

```bash
export MAMBA_API_KEY="..."
export MAMBA_API_BASE="http://127.0.0.1:8787/mamba-api/v1"
```

Every endpoint is also available under the unversioned base (`/mamba-api/...`) for backward compatibility.

## Start

```bash
cp .env.example .env
cargo run --bin mamba_api

# or release build
cargo run --bin mamba_api --release
```

## Configuration

### Required

| Variable | Purpose |
|----------|---------|
| `MAMBA_API_KEY` | Auth key for all API requests |

### RPC endpoints

| Variable | Purpose |
|----------|---------|
| `MAMBA_API_HTTP_URLS` | Comma-separated HTTP RPC endpoints (same cluster) |
| `MAMBA_API_WS_URLS` | Comma-separated WebSocket RPC endpoints (same cluster) |

The API rotates reads across the full HTTP pool and temporarily cools down endpoints that return `429` or transport errors. High-volume websocket enrichment prefers non-Helius endpoints when both Helius and non-Helius URLs are present, reducing parsed-transaction throttling on mainnet.

For busy mainnet markets like `pump_fun` and `pump_swap`, use at least 3 HTTP RPCs across 2 providers. Two URLs on the same host are not enough diversity for sustained loads.

### Options

| Variable | Default | Purpose |
|----------|---------|---------|
| `MAMBA_API_BIND_ADDR` | `127.0.0.1:8787` | Listen address |
| `MAMBA_API_ROUTE_BASE` | `/mamba-api` | Route prefix |
| `MAMBA_API_ENABLE_LIVE_SENDS` | `false` | Enable transaction submission |
| `MAMBA_API_ALLOW_PRIVATE_NETWORK_CLIENTS` | `false` | Accept requests from LAN clients |
| `MAMBA_PRIVATE_KEY` | | Base58 or JSON keypair for signing (`MAMBA_API_PRIVATE_KEY` is a legacy fallback) |
| `MAMBA_API_STORE_MODE` | `false` | Enable Postgres-backed history |
| `MAMBA_API_DATABASE_URL` | | Postgres connection string (required when store mode is on) |
| `FEE_LEVEL` | `medium` | Default priority fee level |
| `MAX_FEE` | | Optional cap on computed/custom priority fee (in SOL) |
| `MAMBA_ROUTE_LOOKUP_TIMEOUT_SECS` | `15` | Route discovery timeout |
| `MAMBA_SWAP_CONFIRM_TIMEOUT_SECS` | `60` | Transaction confirmation timeout |
| `AUTO_ACCEPT_LOW_LQ_POOLS` | `false` | Skip low-liquidity confirmation prompts |

### Error format

Non-2xx responses return:

```json
{ "error": "human-readable message" }
```

---

## Endpoint overview

| Group | Endpoints |
|-------|-----------|
| Health | `GET /health`, `GET /docs`, `GET /markets` |
| Websocket | `POST /ws/subscribe`, `POST /ws/unsubscribe`, `GET /ws/subscriptions`, `GET /ws/stream` |
| Market data | `GET /mints`, `GET /mints/{mint}/route`, `GET /mints/{mint}/creator`, `GET /mints/{mint}/metadata`, `POST /mints/metadata-batch` |
| Swaps | `POST /swap` |
| Create | `GET /create/methods`, `POST /create/build`, `POST /create/execute`, Raydium Launchpad config routes |
| Pools | `GET /pool/methods`, `POST /pool/build`, `POST /pool/execute`, `GET /pool/positions`, `POST /pool/manage/build`, `POST /pool/manage/execute` |
| Wallets | `GET /wallets`, `POST /wallets`, `POST /wallets/select`, `GET /wallets/active`, `GET /wallets/{wallet}/balance` |
| Transfers | `POST /wallets/transfer/build`, `POST /wallets/transfer/execute` |
| Cleaner | `GET /wallets/clean/preview`, `POST /wallets/clean/build`, `POST /wallets/clean/execute` |
| History | `GET /transactions`, `GET /creators`, `GET /creator-mints` |

---

## Health and introspection

### `GET /health`

Reports cluster, signer status, live-send mode, and active subscriptions.

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/health"
```

```json
{
  "status": "ok",
  "cluster": "Devnet",
  "live_sends_enabled": false,
  "signer_configured": true,
  "api_signer_pubkey": "9v1Y...Xg8T",
  "wallet_count": 2,
  "selected_wallet_count": 1,
  "active_wallet_pubkey": "7wY9...rK3p",
  "active_ws_subscriptions": ["pump_fun", "pump_swap"],
  "timestamp_unix_ms": 1760872800123
}
```

### `GET /docs`

Machine-readable endpoint index.

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/docs"
```

```json
{
  "auth_header": "x-api-key: <MAMBA_API_KEY>",
  "local_only": true,
  "base_paths": ["/mamba-api", "/mamba-api/v1"],
  "endpoints": [
    { "method": "GET", "path": "/health", "description": "Service health + security mode" },
    { "method": "POST", "path": "/swap", "description": "Dry-run swap planning and optional live execution" }
  ]
}
```

### `GET /markets`

Returns the supported market identifiers used as filters across the API.

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/markets"
```

```json
{
  "markets": [
    "pump_swap", "pump_fun", "raydium_amm_v4", "raydium_launchpad",
    "raydium_clmm", "raydium_cpmm", "meteora_dlmm",
    "meteora_damm_v1", "meteora_damm_v2", "meteora_dbc"
  ]
}
```

---

## Websocket control

The mint cache is websocket-backed. `GET /mints` and `GET /ws/stream` return data only after at least one market subscription is active.

### `POST /ws/subscribe`

Start a market websocket subscription and begin filling the mint cache.

**Body:** `{ "market": "<market_id>" }`

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "market": "pump_fun" }' \
  "$MAMBA_API_BASE/ws/subscribe"
```

```json
{ "market": "pump_fun", "subscribed": true }
```

### `POST /ws/unsubscribe`

Stop a market websocket subscription.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "market": "pump_fun" }' \
  "$MAMBA_API_BASE/ws/unsubscribe"
```

```json
{ "market": "pump_fun", "unsubscribed": true }
```

### `GET /ws/subscriptions`

List active subscriptions and their status.

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/ws/subscriptions"
```

```json
[
  { "market": "pump_fun", "active": true },
  { "market": "pump_swap", "active": true }
]
```

### `GET /ws/stream` (websocket upgrade)

Upgrades to a websocket and pushes filtered mint snapshots on an interval.

**Query params:**

| Param | Type | Description |
|-------|------|-------------|
| `market` | string | Single market filter |
| `markets` | string | Comma-separated market list |
| `q` | string | Search by name, symbol, or mint |
| `min_liquidity` | number | Minimum cached liquidity |
| `min_volume` | number | Minimum cached volume |
| `limit` | integer | Max rows per payload |
| `interval_ms` | integer | Push interval, clamped to 150..30000 |

**Node.js example:**

```js
import WebSocket from "ws";

const ws = new WebSocket(
  "ws://127.0.0.1:8787/mamba-api/v1/ws/stream?markets=pump_fun,pump_swap&limit=50",
  { headers: { "x-api-key": process.env.MAMBA_API_KEY } }
);

ws.on("message", (data) => {
  const payload = JSON.parse(data.toString());
  console.log(payload.sent_unix_ms, payload.mints.length);
});
```

**Message payload:**

```json
{
  "sent_unix_ms": 1760872800789,
  "mints": [
    {
      "market": "pump_fun",
      "mint": "EC89...oPy4",
      "pool": "58j5...fRSa",
      "creator": "4g2s...b7Qx",
      "name": "Example",
      "symbol": "EX",
      "uri": "https://example.com/meta.json",
      "price": 0.0000000123,
      "highest_price": 0.000000014,
      "volume": 12.3,
      "liquidity": 8.9,
      "buys": 14,
      "sells": 8,
      "tx_count": 22,
      "is_migrated": false,
      "migration_source_market": null,
      "migration_target_market": null,
      "migration_signature": null,
      "migration_slot": null,
      "migration_time": null,
      "migration_confidence": null,
      "holder_count": 120,
      "holder_debug_reason": null,
      "created_time": 1760872000.1,
      "last_activity_time": 1760872799.7,
      "market_cap": 12.3
    }
  ]
}
```

---

## Market data

### `GET /mints`

Returns cached mint snapshots from active websocket subscriptions.

**Query params:**

| Param | Type | Description |
|-------|------|-------------|
| `market` | string | Single market filter |
| `markets` | string | Comma-separated list |
| `q` | string | Search string |
| `min_liquidity` | number | Minimum liquidity |
| `min_volume` | number | Minimum volume |
| `limit` | integer | Max rows, clamped to 1..500 |

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  "$MAMBA_API_BASE/mints?markets=pump_fun,pump_swap&limit=2&min_liquidity=1"
```

Response uses the same mint snapshot format as `/ws/stream` messages.

### `GET /mints/{mint}/route`

Resolves a swap route (market, pool, creator) with a price snapshot.

**Query params:**

| Param | Type | Description |
|-------|------|-------------|
| `quote_mint` | string | Optional quote mint |
| `market_priority` | string | Comma-separated market priority override |
| `min_liquidity_raw` | integer | Raw liquidity filter for route discovery |
| `rpc_url` | string | RPC override (read-only, used for route/price lookups) |

Mamba prefers non-low-LQ pools before falling back. `low_lq` means the route's WSOL quote reserve is below 10 SOL. Low-LQ warnings are suppressed for `pump_fun`, `raydium_launchpad`, and `meteora_dbc`.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  "$MAMBA_API_BASE/mints/EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4/route?market_priority=pump_fun,pump_swap"
```

```json
{
  "mint": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
  "market": "pump_fun",
  "pool": "58j5fTSL5W3LrwiRhBMStsjMdjj1rpGopcNpLkaHfRSa",
  "creator": "4g2s...b7Qx",
  "creator_source": "market_state_fallback",
  "price": 0.0000000123,
  "low_lq": false,
  "wsol_liquidity_raw": 12500000000,
  "wsol_liquidity_sol": 12.5,
  "max_safe_buy_sol_raw": 11000000000,
  "max_safe_buy_sol": 11.0,
  "liquidity_warning": null
}
```

### `GET /mints/{mint}/creator`

Resolves the creator with metadata-first preference when possible.

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/mints/EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4/creator"
```

Response matches the route response format (includes `creator`, `creator_source`, liquidity fields).

### `GET /mints/{mint}/metadata`

Resolves canonical token metadata (name, symbol, URI) with authority and first creator when available.

**Query params:** `rpc_url` (optional RPC override)

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/mints/EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4/metadata"
```

```json
{
  "mint": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
  "name": "Example",
  "symbol": "EX",
  "uri": "https://example.com/meta.json",
  "creator": "4g2s...b7Qx",
  "authority": "8iQp...2oMZ"
}
```

### `POST /mints/metadata-batch`

Batch metadata for up to 100 mints. Invalid pubkeys are silently omitted.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "mints": ["EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4", "So11111111111111111111111111111111111111112"] }' \
  "$MAMBA_API_BASE/mints/metadata-batch"
```

```json
{
  "results": [
    {
      "mint": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
      "name": "Example",
      "symbol": "EX",
      "uri": "https://example.com/meta.json",
      "creator": "4g2s...b7Qx",
      "authority": "8iQp...2oMZ"
    }
  ]
}
```

---

## Swaps

### `POST /swap`

Unified swap surface for all 10 markets. Plans a route by default; executes only when `execute=true` and live sends are enabled.

**Request fields:**

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `side` | yes | | `buy` or `sell` |
| `mint` | yes | | Base mint pubkey |
| `execute` | no | `false` | Submit the transaction |
| `buy_sol` | when buying + executing | | SOL amount to spend |
| `sell_pct` | no | `100` | Percentage of holdings to sell (1..100) |
| `slippage_pct` | no | `15` | Slippage tolerance (1..99) |
| `market` | no | | Force a single market |
| `market_priority` | no | | Comma-separated priority list |
| `pool` | no | | Force a specific pool address |
| `creator` | no | | Override creator when forcing market+pool |
| `quote_mint` | no | | Quote mint for route discovery |
| `min_liquidity_raw` | no | | Minimum raw liquidity threshold |
| `skip_low_lq_pools` | no | | Reject low-LQ fallback routes |
| `use_idempotent` | no | | Use idempotent ATA creation where supported |
| `retries` | no | | Retry count for sell transport failures |
| `priority_fee_level` | no | | `env`, `low`, `medium`, `high`, `turbo`, `max`, or `custom` |
| `priority_fee_sol` | no | | Decimal SOL amount (only with `custom` level) |
| `wallet` | no | | Pubkey to execute from (execute only) |
| `rpc_url` | no | | RPC override (planning OK, live sends enforce same-cluster) |
| `use_swqos` | no | | Enable stake-weighted QoS |
| `swqos_settings` | when swqos=true | | SWQoS provider configuration |

**Notes on fee overrides:**
- `priority_fee_level=env` keeps the default from `.env` (`FEE_LEVEL`)
- `priority_fee_sol` is converted into the swap path's shared 300,000 compute-unit budget and respects `MAX_FEE` when set
- Fee overrides only matter when `execute=true`

#### Dry-run (planning only)

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "side": "buy",
    "mint": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
    "market_priority": "pump_fun,pump_swap",
    "min_liquidity_raw": 1000,
    "slippage_pct": 25
  }' \
  "$MAMBA_API_BASE/swap"
```

```json
{
  "dry_run": true,
  "executed": false,
  "success": true,
  "market": "pump_fun",
  "pool": "58j5fTSL5W3LrwiRhBMStsjMdjj1rpGopcNpLkaHfRSa",
  "mint": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
  "creator": "4g2s...b7Qx",
  "creator_source": "market_state_fallback",
  "price": 0.0000000123,
  "low_lq": false,
  "wsol_liquidity_raw": 12500000000,
  "wsol_liquidity_sol": 12.5,
  "max_safe_buy_sol_raw": 11000000000,
  "max_safe_buy_sol": 11.0,
  "signature": null,
  "error": null,
  "warning": null
}
```

#### Live buy

Requires `MAMBA_API_ENABLE_LIVE_SENDS=true` and a signer.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "side": "buy",
    "mint": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
    "buy_sol": 0.01,
    "slippage_pct": 25,
    "priority_fee_level": "high",
    "execute": true
  }' \
  "$MAMBA_API_BASE/swap"
```

```json
{
  "dry_run": false,
  "executed": true,
  "success": true,
  "market": "pump_fun",
  "pool": "58j5fTSL5W3LrwiRhBMStsjMdjj1rpGopcNpLkaHfRSa",
  "mint": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
  "creator": "4g2s...b7Qx",
  "creator_source": "market_state_fallback",
  "price": 0.0000000123,
  "low_lq": false,
  "wsol_liquidity_raw": 12500000000,
  "wsol_liquidity_sol": 12.5,
  "max_safe_buy_sol_raw": 11000000000,
  "max_safe_buy_sol": 11.0,
  "signature": "5nq2...zvYx",
  "error": null,
  "warning": null
}
```

#### Live sell with custom fee

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "side": "sell",
    "mint": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
    "sell_pct": 100,
    "slippage_pct": 15,
    "priority_fee_level": "custom",
    "priority_fee_sol": 0.00003,
    "execute": true
  }' \
  "$MAMBA_API_BASE/swap"
```

---

## Token creation

### `GET /create/methods`

Returns method specs with required/optional fields and `execute_generated_fields` (signers that Mamba generates internally in execute mode).

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/create/methods"
```

```json
[
  {
    "method": "pump_fun",
    "required_fields": ["method", "payer", "mint", "name", "symbol", "uri"],
    "optional_fields": ["auto_buy.buy_sol", "auto_buy.slippage_pct", "simulate", "rpc_url"],
    "execute_generated_fields": ["mint"]
  }
]
```

### `POST /create/build`

Builds an unsigned token-creation transaction.

**Response fields:** `transaction` (base64), `required_signers`, `derived_addresses`, `mint_token_program`, optional `simulation`.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "method": "pump_fun",
    "payer": "<PAYER_PUBKEY>",
    "mint": "<NEW_MINT_PUBKEY>",
    "name": "Example",
    "symbol": "EX",
    "uri": "https://example.com/token.json",
    "simulate": true,
    "rpc_url": "https://api.devnet.solana.com"
  }' \
  "$MAMBA_API_BASE/create/build"
```

```json
{
  "transaction": "<base64>",
  "required_signers": ["<PAYER_PUBKEY>", "<NEW_MINT_PUBKEY>"],
  "derived_addresses": { "metadata": "..." },
  "mint_token_program": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
  "simulation": { "ok": true, "err": null, "units_consumed": 123456, "logs": [] }
}
```

### `POST /create/execute`

Same body as `/create/build`, but signs and submits when live sends are enabled.

If `mint` is omitted, Mamba generates it internally and returns the public key in `generated_signers`. `rpc_url` overrides must resolve to the same cluster for live sends.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "method": "pump_fun",
    "payer": "<PAYER_PUBKEY>",
    "name": "Example",
    "symbol": "EX",
    "uri": "https://example.com/token.json",
    "simulate": true
  }' \
  "$MAMBA_API_BASE/create/execute"
```

```json
{
  "submitted": true,
  "success": true,
  "signature": "3YdL...WqgZ",
  "error": null,
  "cluster": "Devnet",
  "generated_signers": { "mint": "<GENERATED_MINT_PUBKEY>" },
  "build": {
    "transaction": "<base64>",
    "required_signers": ["<PAYER_PUBKEY>", "<GENERATED_MINT_PUBKEY>"],
    "derived_addresses": { "metadata": "..." },
    "mint_token_program": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
    "simulation": { "ok": true, "err": null, "units_consumed": 123456, "logs": [] }
  }
}
```

### Raydium Launchpad config discovery

These routes help plan Raydium Launchpad create flows by exposing on-chain config state.

#### `GET /create/raydium_launchpad/global-configs`

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/create/raydium_launchpad/global-configs?rpc_url=https://api.devnet.solana.com"
```

```json
[
  { "pubkey": "7pQk...GZkH", "curve_type": 1, "trade_fee_rate": 30, "max_share_fee_rate": 20, "quote_mint": "So11111111111111111111111111111111111111112" }
]
```

#### `GET /create/raydium_launchpad/platform-configs`

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/create/raydium_launchpad/platform-configs?rpc_url=https://api.devnet.solana.com"
```

```json
[
  {
    "pubkey": "9dYq...iJx1",
    "platform_fee_wallet": "3t2u...hQp1",
    "fee_rate": 30,
    "creator_fee_rate": 5,
    "name": "Example Platform",
    "web": "https://example.com",
    "img": "https://example.com/img.png",
    "curve_params_len": 4,
    "curve_params_global_configs": ["7pQk...GZkH"]
  }
]
```

#### `GET /create/raydium_launchpad/platform-configs/{platform_config}/curve-params`

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/create/raydium_launchpad/platform-configs/9dYq...iJx1/curve-params?rpc_url=https://api.devnet.solana.com"
```

```json
[
  {
    "epoch": 0,
    "index": 0,
    "global_config": "7pQk...GZkH",
    "migrate_type": 0,
    "amm_fee_on": "quote_token",
    "supply": 1000000000,
    "total_base_sell": 0,
    "total_quote_fund_raising": 100,
    "vesting_total_locked_amount": 0,
    "vesting_cliff_period": 0,
    "vesting_unlock_period": 0
  }
]
```

---

## Pool management

### `GET /pool/methods`

Returns pool build specs per market: required/optional fields and execute-generated signers.

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/pool/methods"
```

```json
[
  {
    "market": "pump_swap",
    "supported": true,
    "required_fields": ["market", "payer", "base_mint", "base_amount", "quote_mint", "quote_amount"],
    "optional_fields": ["simulate", "rpc_url", "pump_swap_index"],
    "execute_generated_fields": [],
    "notes": null
  }
]
```

### `POST /pool/build`

Builds an unsigned pool-creation transaction.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "market": "pump_swap",
    "payer": "<PAYER_PUBKEY>",
    "base_mint": "<BASE_MINT_PUBKEY>",
    "quote_mint": "So11111111111111111111111111111111111111112",
    "base_amount": "1000",
    "quote_amount": "1",
    "simulate": true,
    "rpc_url": "https://api.devnet.solana.com"
  }' \
  "$MAMBA_API_BASE/pool/build"
```

```json
{
  "transaction": "<base64>",
  "required_signers": ["<PAYER_PUBKEY>"],
  "derived_addresses": { "pool": "..." },
  "simulation": { "ok": true, "err": null, "units_consumed": 123456, "logs": [] }
}
```

### `POST /pool/execute`

Same body as `/pool/build`, signs and submits when live sends are enabled.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "market": "pump_swap",
    "payer": "<PAYER_PUBKEY>",
    "base_mint": "<BASE_MINT_PUBKEY>",
    "quote_mint": "So11111111111111111111111111111111111111112",
    "base_amount": "1000",
    "quote_amount": "1",
    "simulate": true
  }' \
  "$MAMBA_API_BASE/pool/execute"
```

```json
{
  "submitted": true,
  "success": true,
  "signature": "4JxH...F8dQ",
  "error": null,
  "cluster": "Devnet",
  "generated_signers": {},
  "build": {
    "transaction": "<base64>",
    "required_signers": ["<PAYER_PUBKEY>"],
    "derived_addresses": { "pool": "..." },
    "simulation": { "ok": true, "err": null, "units_consumed": 123456, "logs": [] }
  }
}
```

### `GET /pool/positions`

Lists wallet-owned pool positions with withdraw support flags and value estimates.

**Query params:**

| Param | Required | Description |
|-------|----------|-------------|
| `owner` | yes | Wallet pubkey |
| `rpc_url` | no | RPC override |
| `include_unsupported` | no | Include unsupported markets (default `false`) |

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/pool/positions?owner=<WALLET_PUBKEY>&include_unsupported=false"
```

```json
[
  {
    "market": "raydium_clmm",
    "pool": "6wVn...mF1b",
    "base_mint": "EC89...oPy4",
    "quote_mint": "So11111111111111111111111111111111111111112",
    "lp_mint": null,
    "owner_role": "position_owner",
    "owner_lp_balance_raw": null,
    "owner_lp_balance_ui": null,
    "lp_decimals": null,
    "estimated_base_out_ui": 123.45,
    "estimated_quote_out_ui": 0.67,
    "withdraw_supported": true,
    "close_supported": false,
    "note": null
  }
]
```

### `POST /pool/manage/build`

Builds a pool withdrawal or management transaction.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "market": "raydium_clmm",
    "owner": "<WALLET_PUBKEY>",
    "pool": "6wVn...mF1b",
    "withdraw_pct": 100,
    "slippage_pct": 10,
    "simulate": true
  }' \
  "$MAMBA_API_BASE/pool/manage/build"
```

```json
{
  "transaction": "<base64>",
  "required_signers": ["<WALLET_PUBKEY>"],
  "derived_addresses": {},
  "simulation": { "ok": true, "err": null, "units_consumed": 123456, "logs": [] }
}
```

### `POST /pool/manage/execute`

Same body as `/pool/manage/build`, signs and submits when live sends are enabled.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "market": "raydium_clmm",
    "owner": "<WALLET_PUBKEY>",
    "pool": "6wVn...mF1b",
    "withdraw_pct": 50,
    "slippage_pct": 10,
    "simulate": true
  }' \
  "$MAMBA_API_BASE/pool/manage/execute"
```

```json
{
  "submitted": true,
  "success": true,
  "signature": "2V3c...uW1p",
  "error": null,
  "cluster": "Devnet",
  "generated_signers": {},
  "build": {
    "transaction": "<base64>",
    "required_signers": ["<WALLET_PUBKEY>"],
    "derived_addresses": {},
    "simulation": { "ok": true, "err": null, "units_consumed": 123456, "logs": [] }
  }
}
```

---

## Wallet management

### `GET /wallets`

Lists locally stored wallets. Never returns private keys.

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/wallets"
```

```json
[
  {
    "pubkey": "7wY9...rK3p",
    "label": "devnet",
    "created_at_utc": "2026-04-19T12:00:00Z",
    "active": true,
    "selected": true
  }
]
```

### `POST /wallets`

Generate a new locally stored wallet.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "label": "test-wallet" }' \
  "$MAMBA_API_BASE/wallets"
```

```json
{
  "pubkey": "3cQx...p9zK",
  "label": "test-wallet",
  "created_at_utc": "2026-04-19T12:05:00Z",
  "active": false,
  "selected": false
}
```

### `POST /wallets/select`

Update the active and/or selected wallet set.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "active_wallet": "7wY9...rK3p" }' \
  "$MAMBA_API_BASE/wallets/select"
```

Returns the updated wallet list.

### `GET /wallets/active`

Returns the active wallet with its live SOL balance.

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/wallets/active?rpc_url=https://api.devnet.solana.com"
```

```json
{
  "pubkey": "7wY9...rK3p",
  "label": "devnet",
  "managed": true,
  "active": true,
  "selected": true,
  "balance_lamports": 3820000000,
  "balance_sol": 3.82,
  "cluster": "Devnet",
  "timestamp_unix_ms": 1760872800999
}
```

### `GET /wallets/{wallet}/balance`

Live SOL balance for any wallet pubkey. Includes managed-wallet flags when known.

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/wallets/7wY9...rK3p/balance"
```

Response matches the `/wallets/active` format.

---

## Transfers

### `POST /wallets/transfer/build`

Builds an unsigned SOL or SPL token transfer with optional simulation.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "from_wallet": "7wY9...rK3p",
    "to_address": "9v1Y...Xg8T",
    "amount": "0.01",
    "asset_kind": "sol",
    "simulate": true,
    "rpc_url": "https://api.devnet.solana.com"
  }' \
  "$MAMBA_API_BASE/wallets/transfer/build"
```

```json
{
  "transaction": "<base64>",
  "required_signers": ["7wY9...rK3p"],
  "derived_addresses": {},
  "kind": "sol",
  "amount_input": "0.01",
  "amount_raw": "10000000",
  "decimals": 9,
  "mint": null,
  "token_program": null,
  "simulation": { "ok": true, "err": null, "units_consumed": 5000, "logs": [] }
}
```

### `POST /wallets/transfer/execute`

Same body as `/wallets/transfer/build`, signs and submits when live sends are enabled.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "from_wallet": "7wY9...rK3p",
    "to_address": "9v1Y...Xg8T",
    "amount": "0.01",
    "asset_kind": "sol",
    "simulate": true
  }' \
  "$MAMBA_API_BASE/wallets/transfer/execute"
```

```json
{
  "submitted": true,
  "success": true,
  "signature": "5xJp...rT2a",
  "error": null,
  "cluster": "Devnet",
  "build": {
    "transaction": "<base64>",
    "required_signers": ["7wY9...rK3p"],
    "derived_addresses": {},
    "kind": "sol",
    "amount_input": "0.01",
    "amount_raw": "10000000",
    "decimals": 9,
    "mint": null,
    "token_program": null,
    "simulation": { "ok": true, "err": null, "units_consumed": 5000, "logs": [] }
  }
}
```

---

## Wallet cleaner

The cleaner previews token accounts, classifies them (unwrap WSOL, burn and close, close empty, or skip), and builds batched cleanup transactions.

### `GET /wallets/clean/preview`

**Query params:** `owner` (required), `rpc_url` (optional)

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/wallets/clean/preview?owner=<WALLET_PUBKEY>&rpc_url=https://api.devnet.solana.com"
```

```json
{
  "owner": "<WALLET_PUBKEY>",
  "total_token_accounts": 12,
  "cleanable_accounts": 9,
  "burn_accounts": 1,
  "close_only_accounts": 8,
  "unwrap_accounts": 0,
  "blocked_accounts": 3,
  "total_reclaim_lamports": 2039280,
  "total_reclaim_sol": 0.00203928,
  "entries": [
    {
      "token_account": "9dYq...iJx1",
      "mint": "EC89...oPy4",
      "token_program": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
      "owner": "<WALLET_PUBKEY>",
      "action": "close_empty",
      "amount_raw": 0,
      "amount_ui": 0.0,
      "decimals": 6,
      "reclaim_lamports": 2039280,
      "reclaim_sol": 0.00203928,
      "burn_required": false,
      "is_associated": true,
      "is_native_wsol": false,
      "state": null,
      "skip_reason": null
    }
  ]
}
```

### `POST /wallets/clean/build`

**Request fields:**

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `owner` | yes | | Wallet pubkey |
| `token_accounts` | no | | Explicit account subset |
| `burn_nonzero` | no | `false` | Burn non-zero balances before closing |
| `close_empty` | no | `true` | Close zero-balance accounts |
| `close_wsol` | no | `true` | Unwrap and close WSOL accounts |
| `simulate` | no | `true` | Simulate each batch |
| `rpc_url` | no | | RPC override |

The owner must be locally signable. Batches are split before exceeding Solana wire-size limits. Simulations use `sig_verify=false` and `replace_recent_blockhash=true`.

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "owner": "<WALLET_PUBKEY>",
    "burn_nonzero": true,
    "close_empty": true,
    "close_wsol": true,
    "simulate": true,
    "rpc_url": "https://api.devnet.solana.com"
  }' \
  "$MAMBA_API_BASE/wallets/clean/build"
```

Response includes: `preview`, `selected_account_count`, `selected_reclaim_lamports`, `selected_reclaim_sol`, and `batches[]` with per-batch `transaction` (base64), `required_signers`, `action_count`, `reclaim_lamports`, `reclaim_sol`, and `simulation`.

### `POST /wallets/clean/execute`

Same body as `/wallets/clean/build`. Signs and submits each batch when:

- `MAMBA_API_ENABLE_LIVE_SENDS=true`
- Owner signer is available
- `rpc_url` override resolves to the same cluster
- All simulated batches succeeded

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "owner": "<WALLET_PUBKEY>",
    "burn_nonzero": false,
    "close_empty": true,
    "close_wsol": true,
    "simulate": true
  }' \
  "$MAMBA_API_BASE/wallets/clean/execute"
```

```json
{
  "submitted": true,
  "success": true,
  "cluster": "Devnet",
  "error": null,
  "build": { "..." : "..." },
  "batches": [
    { "batch_index": 0, "submitted": true, "success": true, "signature": "4n4B...zJ6o", "error": null }
  ]
}
```

---

## History (store-mode)

These routes read from Postgres when `MAMBA_API_STORE_MODE=true`. Without store mode, they fall back to live websocket cache views with limited historical depth.

### `GET /transactions`

**Query params:** `creator` (optional), `market` (optional), `limit` (1..500), `offset` (optional)

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/transactions?market=pump_fun&limit=5"
```

```json
[
  {
    "signature": "5nq2...zvYx",
    "market": "pump_fun",
    "mint": "EC89...oPy4",
    "pool": "58j5...fRSa",
    "creator": "4g2s...b7Qx",
    "creator_source": "market_state_fallback",
    "side": "buy",
    "slippage_pct": 25,
    "sol_amount": 0.01,
    "sell_pct": null,
    "price": 0.0000000123,
    "market_cap": 12.3,
    "executed": true,
    "success": true,
    "created_at": "2026-04-19T12:20:00Z"
  }
]
```

### `GET /creators`

**Query params:** `min_mint_count`, `min_avg_market_cap`, `min_score`, `limit` (1..500), `offset`

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/creators?min_mint_count=2&limit=10"
```

```json
[
  {
    "creator": "4g2s...b7Qx",
    "mint_count": 3,
    "avg_market_cap": 12345.67,
    "tx_count": 120,
    "total_volume_sol": 56.78,
    "score_raw": 9.1,
    "score_normalized": 100.0,
    "updated_at": "2026-04-19T12:20:00Z"
  }
]
```

### `GET /creator-mints`

**Query params:** `creator` (required), `market` (optional), `limit` (1..500), `offset`

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/creator-mints?creator=4g2s...b7Qx&limit=5"
```

```json
[
  {
    "creator": "4g2s...b7Qx",
    "market": "pump_fun",
    "mint": "EC89...oPy4",
    "pool": "58j5...fRSa",
    "name": "Example",
    "symbol": "EX",
    "uri": "https://example.com/meta.json",
    "price": 0.0000000123,
    "market_cap": 12.3,
    "liquidity": 8.9,
    "volume": 12.3,
    "buys": 14,
    "sells": 8,
    "tx_count": 22,
    "holder_count": 120,
    "created_time": 1760872000.1,
    "last_activity_time": 1760872799.7,
    "source": "store"
  }
]
```

---

## Safety model

- **Build-first by default.** Create, pool, transfer, and cleaner routes return unsigned transactions.
- **Live sends are opt-in.** Set `MAMBA_API_ENABLE_LIVE_SENDS=true` to allow submission.
- **No key leakage.** The API never returns private keys or partially signed transactions.
- **Cross-cluster protection.** Execute flows reject `rpc_url` overrides that point to a different cluster than the API runtime.
