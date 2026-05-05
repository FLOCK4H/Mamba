# Mamba Local API

## Scope

`mamba_api` is Mamba's authenticated local backend. It exposes:

- websocket-backed market data (cached mint snapshots + creator/metadata enrichment),
- builder-style routes for **token creation**, **pool creation/management**, **wallet transfers**, and **wallet cleanup**,
- a swap surface (`POST /swap`) that does **route+price planning** and can optionally **execute** swaps when live sends are enabled.

Defaults:

- Bind: `127.0.0.1:8787`
- Route base: `/mamba-api`
- Versioned base path: `/mamba-api/v1`
- Required auth header: `x-api-key: <MAMBA_API_KEY>`

## Start it

```bash
cp .env.example .env
cargo run --bin mamba_api
```

Release/manual path:

```bash
cargo run --bin mamba_api --release
```

## Environment

Required:

- `MAMBA_API_KEY`

RPC configuration:

- `MAMBA_API_HTTP_URLS`
- `MAMBA_API_WS_URLS`

Both variables accept comma-separated endpoint lists. Keep every URL on the same Solana cluster.

Multi-RPC behavior:

- `mamba_api` uses the full HTTP list for reads, simulation, confirmation, and local builder verification.
- Read-heavy paths rotate across the pool and temporarily cool down endpoints that return retryable transport errors or `429` responses.
- High-volume websocket enrichment (`getAccount`, `getTransactionParsed`, `getMultipleAccounts`) prefers non-Helius endpoints when both Helius and non-Helius URLs are present, which cuts down parsed-transaction/account throttling on mainnet.
- The first URL remains the default cluster anchor for startup detection and the usual primary for non-readonly paths.
- `rpc_url` overrides still use the same resilient read helpers, but live-send routes require the override to resolve to the same cluster as the API runtime.

Mainnet recommendation:

- Busy websocket markets like `pump_fun` and `pump_swap` generally need at least 3 HTTP RPCs across 2 providers.
- Two URLs on the same host are not enough diversity for sustained parsed-transaction loads.

Important option flags:

- `MAMBA_API_BIND_ADDR`
- `MAMBA_API_ROUTE_BASE`
- `MAMBA_API_ALLOW_PRIVATE_NETWORK_CLIENTS`
- `MAMBA_API_ENABLE_LIVE_SENDS`
- `MAMBA_API_STORE_MODE`
- `MAMBA_API_DATABASE_URL` when store mode is enabled
- `MAMBA_PRIVATE_KEY` only when live execution is intentionally enabled (`MAMBA_API_PRIVATE_KEY` remains a legacy fallback)

Store mode:

- `MAMBA_API_STORE_MODE=true` enables a Postgres-backed store for `/transactions`, `/creators`, and `/creator-mints`.
- Without store mode, those routes fall back to live websocket cache views and may return less historical detail.

## Conventions

### Base URL used in examples

All examples below assume:

```bash
export MAMBA_API_KEY="..."
export MAMBA_API_BASE="http://127.0.0.1:8787/mamba-api/v1"
```

Every endpoint is also available under the unversioned base (`/mamba-api/...`) for compatibility.

### Error format

Non-2xx responses return a JSON body:

```json
{ "error": "human-readable message" }
```

## Route groups

| Group | Endpoints |
| --- | --- |
| Health and docs | `GET /health`, `GET /docs`, `GET /markets` |
| Websocket control | `POST /ws/subscribe`, `POST /ws/unsubscribe`, `GET /ws/subscriptions`, `GET /ws/stream` |
| Market data | `GET /mints`, `GET /mints/{mint}/route`, `GET /mints/{mint}/creator`, `GET /mints/{mint}/metadata`, `POST /mints/metadata-batch`, `GET /creators`, `GET /creator-mints`, `GET /transactions` |
| Swaps | `POST /swap` |
| Create | `GET /create/methods`, `POST /create/build`, `POST /create/execute`, `GET /create/raydium_launchpad/global-configs`, `GET /create/raydium_launchpad/platform-configs`, `GET /create/raydium_launchpad/platform-configs/{platform_config}/curve-params` |
| Pools | `GET /pool/methods`, `POST /pool/build`, `POST /pool/execute`, `GET /pool/positions`, `POST /pool/manage/build`, `POST /pool/manage/execute` |
| Wallets | `GET /wallets`, `GET /wallets/active`, `GET /wallets/{wallet}/balance`, `POST /wallets`, `POST /wallets/select`, `POST /wallets/transfer/build`, `POST /wallets/transfer/execute`, `GET /wallets/clean/preview`, `POST /wallets/clean/build`, `POST /wallets/clean/execute` |

## Health and introspection

### `GET /health`

Reports:

- active cluster
- signer presence
- live-send mode
- websocket subscription visibility

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/health"
```

Example response:

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

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/docs"
```

Example response:

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

Returns the supported market tokens (also used as `market`/`markets` filters across the API).

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/markets"
```

Example response:

```json
{
  "markets": [
    "pump_swap",
    "pump_fun",
    "raydium_amm_v4",
    "raydium_launchpad",
    "raydium_clmm",
    "raydium_cpmm",
    "meteora_dlmm",
    "meteora_damm_v1",
    "meteora_damm_v2",
    "meteora_dbc"
  ]
}
```

## Websocket control

The mint cache is websocket-backed. `GET /mints` and `GET /ws/stream` return rows only after at least one market subscription is active.

### `POST /ws/subscribe`

Starts a market websocket subscription and begins filling the mint cache.

Request body:

- `market` one of: `pump_swap`, `pump_fun`, `raydium_amm_v4`, `raydium_launchpad`, `raydium_clmm`, `raydium_cpmm`, `meteora_dlmm`, `meteora_damm_v1`, `meteora_damm_v2`, `meteora_dbc`

Example request:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "market": "pump_fun" }' \
  "$MAMBA_API_BASE/ws/subscribe"
```

Example response:

```json
{ "market": "pump_fun", "subscribed": true }
```

### `POST /ws/unsubscribe`

Stops a market websocket subscription.

Example request:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "market": "pump_fun" }' \
  "$MAMBA_API_BASE/ws/unsubscribe"
```

Example response:

```json
{ "market": "pump_fun", "unsubscribed": true }
```

### `GET /ws/subscriptions`

Lists the currently tracked subscriptions and whether they are still active.

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/ws/subscriptions"
```

Example response:

```json
[
  { "market": "pump_fun", "active": true },
  { "market": "pump_swap", "active": true }
]
```

## Live market stream

### `GET /ws/stream` (websocket upgrade)

Upgrades to a websocket and pushes filtered mint snapshots using the same filtering model as `GET /mints`.

Common query params:

- `market` single market token
- `markets` comma-separated list of markets
- `q` search string (name/symbol/mint)
- `min_liquidity` minimum cached liquidity
- `min_volume` minimum cached volume
- `limit` max rows per payload
- `interval_ms` push interval (clamped to `150..30000`)

Example Node client:

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

Example message payload:

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

## Market data (cache-backed)

### `GET /mints`

Returns cached websocket-backed mint snapshots. Without active market subscriptions, the response is an empty list.

Query params:

- `market` single market token
- `markets` comma-separated list
- `q` search string
- `min_liquidity` minimum liquidity
- `min_volume` minimum volume
- `limit` max rows (clamped to `1..=500`)

Example request:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  "$MAMBA_API_BASE/mints?markets=pump_fun,pump_swap&limit=2&min_liquidity=1"
```

Example response:

```json
[
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
```

### `GET /mints/{mint}/route`

Resolves a best-effort swap route (market + pool + creator) and also returns a price snapshot.

Query params:

- `quote_mint` optional quote mint
- `market_priority` optional comma-separated market priority (overrides default)
- `min_liquidity_raw` optional advanced raw-liquidity filter for route discovery
- `rpc_url` optional RPC override (read-only; used for route/price lookups)

Route selection notes:

- Mamba now prefers non-low-LQ pools automatically before falling back to low-LQ pools.
- `low_lq` means the route's WSOL quote reserve is below `10 SOL`.
- Low-LQ warnings are intentionally suppressed for `pump_fun`, `raydium_launchpad`, and `meteora_dbc`.

Example request:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  "$MAMBA_API_BASE/mints/EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4/route?market_priority=pump_fun,pump_swap"
```

Example response:

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

Resolves the creator with “metadata-first” preference (when possible) and keeps the resolution source explicit.

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/mints/EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4/creator"
```

Example response:

```json
{
  "mint": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
  "market": "pump_fun",
  "pool": "58j5fTSL5W3LrwiRhBMStsjMdjj1rpGopcNpLkaHfRSa",
  "creator": "4g2s...b7Qx",
  "creator_source": "metadata_first_creator",
  "low_lq": false,
  "wsol_liquidity_raw": 12500000000,
  "wsol_liquidity_sol": 12.5,
  "max_safe_buy_sol_raw": 11000000000,
  "max_safe_buy_sol": 11.0,
  "liquidity_warning": null
}
```

### `GET /mints/{mint}/metadata`

Resolves canonical token metadata (name/symbol/uri), including authority and (when available) the first creator.

Query params:

- `rpc_url` optional RPC override

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/mints/EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4/metadata"
```

Example response:

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

Batch metadata resolution for up to 100 mints. Invalid pubkeys are ignored and omitted from results.

Example request:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "mints": ["EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4", "So11111111111111111111111111111111111111112"] }' \
  "$MAMBA_API_BASE/mints/metadata-batch"
```

Example response:

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

## Swaps

### `POST /swap`

This is the swap surface for all supported markets. It always returns:

- the resolved route (`market`, `pool`, `creator`, `creator_source`)
- a price snapshot (`price`)
- execution status fields (`dry_run`, `executed`, `success`, `signature`, `error`)

**Important:** There is no separate `POST /swap/build` endpoint today. `execute=false` performs planning only (route + price), and `execute=true` performs a live swap when allowed.

Request fields:

- `side` **required**: `buy` or `sell`
- `mint` **required**: base mint pubkey (string)
- `execute` optional (default `false`)
- `buy_sol` required when `side=buy` and `execute=true`
- `sell_pct` optional when `side=sell` (default `100`), range `1..=100`
- `slippage_pct` optional (default `15`), range `1..=99`
- `market` optional: force a single market (e.g. `pump_fun`)
- `market_priority` optional: comma-separated priority list (e.g. `pump_fun,pump_swap,raydium_clmm`)
- `pool` optional + `market` optional: force a specific pool address
- `creator` optional: override creator when forcing `market+pool`
- `quote_mint` optional: quote mint used for route discovery
- `min_liquidity_raw` optional: minimum raw liquidity threshold used in route discovery
- `skip_low_lq_pools` optional: when `true`, reject low-LQ fallback routes instead of planning/executing them
- `use_idempotent` optional: when true, uses idempotent ATA creation where supported
- `retries` optional (sell only): retry count for retryable transport failures
- `priority_fee_level` optional: `env`, `low`, `medium`, `high`, `turbo`, `max`, or `custom`
- `priority_fee_sol` optional: decimal SOL amount used only with `priority_fee_level=custom`
- `wallet` optional (execute only): pubkey to execute from (API signer or managed wallet must be available)
- `rpc_url` optional override (planning is allowed, but live sends enforce same-cluster matching)
- `use_swqos` optional, `swqos_settings` required when `use_swqos=true`

Notes:

- `execute=false` still performs planning only. Fee overrides matter when the request is actually sent (`execute=true`).
- `priority_fee_level=env` keeps the global `.env` default (`FEE_LEVEL`).
- `priority_fee_sol` is converted from SOL into the swap path's shared 300,000 compute-unit budget and still respects `MAX_FEE` when that env cap is set.
- When no healthy route exists, `/swap` can still return a low-LQ route with `low_lq=true` and a warning unless the caller sets `skip_low_lq_pools=true`.

#### Example: dry-run planning (no live send)

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

Example response:

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

#### Example: execute BUY (live send)

Requires `MAMBA_API_ENABLE_LIVE_SENDS=true` and a signer configured.

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

Example response:

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

#### Example: execute SELL with a custom priority fee amount

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

## Create endpoints

### `GET /create/methods`

Returns method specs with required/optional fields and `execute_generated_fields` (signers that Mamba can generate only in execute mode).

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/create/methods"
```

Example response:

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

Builds an unsigned token-creation transaction and returns:

- `transaction` (unsigned base64)
- `required_signers` (pubkeys that must sign before submission)
- `derived_addresses` (method-dependent, helps clients avoid recalculating PDAs)
- optional `simulation`

Example request (pump.fun):

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

Example response:

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

Uses the same request body as `/create/build`, but signs and submits locally when live sends are enabled.

Notes:

- If `mint` is omitted, Mamba generates it internally and returns the public key in `generated_signers`.
- `rpc_url` overrides must resolve to the same cluster as the API runtime for live sends.

Example request:

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

Example response:

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

These routes support Raydium Launchpad create flows.

#### `GET /create/raydium_launchpad/global-configs`

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/create/raydium_launchpad/global-configs?rpc_url=https://api.devnet.solana.com"
```

Example response:

```json
[
  { "pubkey": "7pQk...GZkH", "curve_type": 1, "trade_fee_rate": 30, "max_share_fee_rate": 20, "quote_mint": "So11111111111111111111111111111111111111112" }
]
```

#### `GET /create/raydium_launchpad/platform-configs`

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/create/raydium_launchpad/platform-configs?rpc_url=https://api.devnet.solana.com"
```

Example response:

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

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/create/raydium_launchpad/platform-configs/9dYq...iJx1/curve-params?rpc_url=https://api.devnet.solana.com"
```

Example response:

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

## Pool endpoints

### `GET /pool/methods`

Returns pool build specs per market (supported/unsupported, required/optional fields, and execute-time generated signer fields).

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/pool/methods"
```

Example response:

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

Builds an unsigned pool-creation transaction for the selected market.

Example request (pump.swap):

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

Example response:

```json
{
  "transaction": "<base64>",
  "required_signers": ["<PAYER_PUBKEY>"],
  "derived_addresses": { "pool": "..." },
  "simulation": { "ok": true, "err": null, "units_consumed": 123456, "logs": [] }
}
```

### `POST /pool/execute`

Uses the same request body as `/pool/build`, but signs and submits locally when live sends are enabled.

Example request:

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

Example response:

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

Lists wallet-owned positions (for supported markets) with withdraw support flags and estimates.

Query params:

- `owner` required wallet pubkey
- `rpc_url` optional override
- `include_unsupported` optional (default `false`)

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/pool/positions?owner=<WALLET_PUBKEY>&include_unsupported=false"
```

Example response:

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

Builds a supported pool withdrawal/management transaction.

Example request:

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

Example response:

```json
{
  "transaction": "<base64>",
  "required_signers": ["<WALLET_PUBKEY>"],
  "derived_addresses": {},
  "simulation": { "ok": true, "err": null, "units_consumed": 123456, "logs": [] }
}
```

### `POST /pool/manage/execute`

Uses the same request body as `/pool/manage/build`, but signs and submits locally when live sends are enabled.

Example request:

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

Example response:

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

## Wallet endpoints

### `GET /wallets`

Lists locally stored wallet metadata (never returns private keys).

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/wallets"
```

Example response:

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

Generates a new locally stored wallet.

Example request:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "label": "test-wallet" }' \
  "$MAMBA_API_BASE/wallets"
```

Example response:

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

Updates the active and/or selected managed-wallet set.

Example request (set active only):

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "active_wallet": "7wY9...rK3p" }' \
  "$MAMBA_API_BASE/wallets/select"
```

Example response:

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

### `GET /wallets/active`

Returns the active managed wallet with its live SOL balance.

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/wallets/active?rpc_url=https://api.devnet.solana.com"
```

Example response:

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

Returns the live SOL balance for any wallet pubkey (managed-wallet flags included when known).

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/wallets/7wY9...rK3p/balance"
```

Example response:

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
  "timestamp_unix_ms": 1760872801001
}
```

## Wallet transfer endpoints

### `POST /wallets/transfer/build`

Builds an unsigned SOL or token transfer (and optionally simulates it).

Example request (SOL transfer):

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

Example response:

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

Uses the same request body as `/wallets/transfer/build`, but signs and submits locally when live sends are enabled.

Example request:

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

Example response:

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

## Wallet cleaner endpoints

The wallet cleaner is exposed in preview, build, and execute form.

### `GET /wallets/clean/preview`

Query params:

- `owner` required wallet pubkey to inspect
- `rpc_url` optional RPC override

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/wallets/clean/preview?owner=<WALLET_PUBKEY>&rpc_url=https://api.devnet.solana.com"
```

Example response (trimmed):

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

Request body fields:

- `owner`
- `token_accounts` optional explicit subset
- `burn_nonzero` optional, default `false`
- `close_empty` optional, default `true`
- `close_wsol` optional, default `true`
- `simulate` optional, default `true`
- `rpc_url` optional override

Build behavior:

- owner must be locally signable through the configured API signer or managed wallet store
- transactions are returned unsigned as base64
- batches are split before exceeding Solana wire-size limits
- simulations use `sig_verify=false` and `replace_recent_blockhash=true`

Example request:

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

Example response (trimmed):

```json
{
  "owner": "<WALLET_PUBKEY>",
  "burn_nonzero": true,
  "close_empty": true,
  "close_wsol": true,
  "selected_account_count": 9,
  "selected_reclaim_lamports": 2039280,
  "selected_reclaim_sol": 0.00203928,
  "preview": { "owner": "<WALLET_PUBKEY>", "total_token_accounts": 12, "cleanable_accounts": 9, "burn_accounts": 1, "close_only_accounts": 8, "unwrap_accounts": 0, "blocked_accounts": 3, "total_reclaim_lamports": 2039280, "total_reclaim_sol": 0.00203928, "entries": [] },
  "batches": [
    {
      "batch_index": 0,
      "transaction": "<base64>",
      "required_signers": ["<WALLET_PUBKEY>"],
      "action_count": 8,
      "token_account_count": 8,
      "reclaim_lamports": 2039280,
      "reclaim_sol": 0.00203928,
      "actions": [],
      "simulation": { "ok": true, "err": null, "units_consumed": 120000, "logs": [] }
    }
  ]
}
```

### `POST /wallets/clean/execute`

Uses the same request body as `/wallets/clean/build`, but signs each batch locally and submits it when:

- `MAMBA_API_ENABLE_LIVE_SENDS=true`
- the `owner` signer is available through the API signer or managed wallet store
- any `rpc_url` override resolves to the same cluster as the API's configured runtime cluster
- every simulated batch succeeded

Example request:

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

Example response (trimmed):

```json
{
  "submitted": true,
  "success": true,
  "cluster": "Devnet",
  "error": null,
  "build": { "owner": "<WALLET_PUBKEY>", "burn_nonzero": false, "close_empty": true, "close_wsol": true, "selected_account_count": 9, "selected_reclaim_lamports": 2039280, "selected_reclaim_sol": 0.00203928, "preview": { "owner": "<WALLET_PUBKEY>", "total_token_accounts": 12, "cleanable_accounts": 9, "burn_accounts": 0, "close_only_accounts": 9, "unwrap_accounts": 0, "blocked_accounts": 3, "total_reclaim_lamports": 2039280, "total_reclaim_sol": 0.00203928, "entries": [] }, "batches": [] },
  "batches": [
    { "batch_index": 0, "submitted": true, "success": true, "signature": "4n4B...zJ6o", "error": null }
  ]
}
```

## Store-mode endpoints

These routes read from Postgres when `MAMBA_API_STORE_MODE=true` and otherwise fall back to live websocket cache views that may omit stored history.

### `GET /transactions`

Query params:

- `creator` optional filter
- `market` optional filter
- `limit` optional (clamped `1..=500`)
- `offset` optional

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/transactions?market=pump_fun&limit=5"
```

Example response:

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

Query params:

- `min_mint_count` optional
- `min_avg_market_cap` optional
- `min_score` optional
- `limit` optional (clamped `1..=500`)
- `offset` optional

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/creators?min_mint_count=2&limit=10"
```

Example response:

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

Query params:

- `creator` **required**
- `market` optional
- `limit` optional (clamped `1..=500`)
- `offset` optional

Example request:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" "$MAMBA_API_BASE/creator-mints?creator=4g2s...b7Qx&limit=5"
```

Example response:

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

## Builder and safety model

- Build-first by default for create/pool/transfer/cleaner routes.
- `MAMBA_API_ENABLE_LIVE_SENDS=true` is required before any live submission happens.
- The server never returns private keys or partially signed transactions.
- Execute flows refuse cross-cluster `rpc_url` overrides (devnet runtime cannot be redirected to mainnet for live sends).
