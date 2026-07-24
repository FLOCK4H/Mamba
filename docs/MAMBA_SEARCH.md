# Mamba Search

Mamba Search is the read-only pool discovery surface for a complete mint address. It asks every supported market for every WSOL-quoted pool, inspects every pool it finds, and returns one response with comparable route details.

It is implemented in `src/mamba_search.rs` and exposed by the authenticated local API:

```text
GET /mamba-search/{mint}
GET /mamba-search/{mint}/stream
```

## What it returns

Each pool result can include:

- market and pool address;
- current SOL price;
- exact mint-side reserve in raw and display units;
- exact WSOL-side reserve in lamports and SOL;
- estimated safe buy capacity;
- token creator and the creator-resolution source;
- token supply and SOL-denominated market capitalization;
- pool creation and last-activity timestamps from market state, bounded account history, or the local websocket cache;
- a low-liquidity signal using Mamba's normal route policy.

Mamba does not invent timestamps. `created_time_source` identifies `market_state`, exact `account_history`, an `account_history_lower_bound`, or a `market_observation`. `created_time_approximate` is true for lower bounds and observations. If no authoritative or bounded timestamp can be established, the field remains `null`.

The streaming route uses newline-delimited JSON (`application/x-ndjson`). It emits `started`, `market`, `pool`, `mint`, `complete`, or `error` records. A pool is emitted as soon as its market discovers it, then emitted again whenever price, reserves, creator, or timing resolves. Clients should upsert pools by `(market, pool)` and never wait for `complete` before rendering.

## Concurrency model

Discovery and inspection overlap rather than running as two blocking phases:

1. Pool discovery starts for all 10 markets on every configured HTTP RPC at the same time. The first successful RPC result wins for each market and slower attempts are cancelled.
2. Each deduplicated pool is emitted immediately and starts inspection while the remaining markets are still searching.
3. Price, exact reserves, creator, direct creation time, and account history resolve independently. Each field races every configured RPC and produces another pool event as soon as it resolves.

Token supply and metadata are also raced across the configured search subset. By default Mamba uses the first three configured HTTP RPCs, keeps four pool inspections in flight per search, and permits eight across simultaneous searches. This avoids turning a mint with hundreds of pools into thousands of simultaneous requests. A market is considered available when at least one search RPC completes its discovery request. `rpc_failures` counts failed attempts observed before that market's winning response; requests still in flight are cancelled and are not misreported as failures.

Configure several independent RPC providers for useful redundancy:

```bash
MAMBA_API_HTTP_URLS=https://rpc-a.example,https://rpc-b.example,https://rpc-c.example
```

The API never returns configured RPC URLs or provider credentials.

## Request

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  "$MAMBA_API_BASE/mamba-search/EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4"
```

`quote_mint` is optional and currently must be WSOL because the response reports SOL-side balances.

## Response shape

```json
{
  "mint": {
    "address": "EC89C9SJscnDsteimgg6cShCGBVzNvcey8wNEhm3oPy4",
    "name": "Example",
    "symbol": "EX",
    "uri": "https://example.com/metadata.json",
    "supply_ui": 1000000000.0
  },
  "quote_mint": "So11111111111111111111111111111111111111112",
  "pools": [
    {
      "market": "pump_swap",
      "pool": "9xu...wB2",
      "creator": "4g2...b7Q",
      "creator_source": "market_state_fallback",
      "price_sol": 1.23e-8,
      "mint_balance_raw": 530000000000000,
      "mint_decimals": 6,
      "mint_balance_ui": 530000000.0,
      "sol_balance_raw": 12500000000,
      "sol_balance": 12.5,
      "max_safe_buy_sol": 5.87,
      "market_cap_sol": 12.3,
      "low_liquidity": false,
      "created_time": null,
      "created_time_source": null,
      "created_time_approximate": false,
      "last_activity_time": null,
      "inspection_status": "complete"
    }
  ],
  "markets": [
    {
      "market": "pump_swap",
      "pools_found": 1,
      "rpc_attempts": 3,
      "rpc_successes": 1,
      "rpc_failures": 0,
      "status": "complete"
    }
  ],
  "rpc_count": 3,
  "complete": true,
  "duration_ms": 842,
  "searched_at_unix_ms": 1784592000000
}
```

`complete: false` means at least one market had no successful discovery response or at least one discovered pool could not be fully inspected. Partial responses keep successfully inspected pools instead of failing the whole search.

For progressive clients, `inspection_status` moves through `pending`, `partial`, and `complete` (or `unavailable`). Missing price or SOL values are unresolved facts, not placeholders: later pool events can fill them without replacing creator or age values that already arrived.

## Timeouts

| Variable | Default | Meaning |
|---|---:|---|
| `MAMBA_SEARCH_DISCOVERY_TIMEOUT_SECS` | `15` | Maximum duration for one market discovery attempt on one RPC |
| `MAMBA_SEARCH_INSPECTION_TIMEOUT_SECS` | `8` | Maximum duration for one pool field, metadata, or supply attempt on one RPC |
| `MAMBA_SEARCH_RPC_CONCURRENCY` | `3` | Ordered prefix of configured HTTP RPCs used by one search |
| `MAMBA_SEARCH_POOL_CONCURRENCY` | `4` | Maximum pools inspected concurrently per search |
| `MAMBA_SEARCH_GLOBAL_POOL_CONCURRENCY` | `8` | Maximum pool inspections across simultaneous searches |
| `MAMBA_SEARCH_HISTORY_SIGNATURE_LIMIT` | `25` | Bounded signature window used for age/activity evidence |

These are read-only operations. Mamba Search does not build, sign, or submit transactions. A client can use a selected `market` and `pool` as explicit inputs to the normal review-first swap flow.
