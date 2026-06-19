# Markets

Mamba implements 10 Solana DEX markets with full websocket ingestion, route discovery, pricing, and buy/sell execution.

## Coverage matrix

| Market | Trade | Websocket | Route lookup | Price | Create token | Create pool |
|--------|:-----:|:---------:|:------------:|:-----:|:------------:|:-----------:|
| PumpSwap AMM | ✓ | ✓ | ✓ | ✓ | | ✓ |
| Pump.fun | ✓ | ✓ | ✓ | ✓ | ✓ | |
| Raydium AMM v4 | ✓ | ✓ | ✓ | ✓ | | ✓ |
| Raydium Launchpad | ✓ | ✓ | ✓ | ✓ | ✓ | |
| Raydium CLMM | ✓ | ✓ | ✓ | ✓ | | ✓ |
| Raydium CPMM | ✓ | ✓ | ✓ | ✓ | | ✓ |
| Meteora DLMM | ✓ | ✓ | ✓ | ✓ | | ✓ |
| Meteora DAMM v1 | ✓ | ✓ | ✓ | ✓ | | ✓ |
| Meteora DAMM v2 | ✓ | ✓ | ✓ | ✓ | | ✓ |
| Meteora DBC | ✓ | ✓ | ✓ | ✓ | | ✓ |

**Token creation** is also supported via `spl_token` and `spl_token_2022` outside of market-specific launch flows.

**Pool creation** is not available for `pump_fun` or `raydium_launchpad` because their pool step is part of the token launch flow, not a standalone operation.

## API surface

The local API exposes these markets through:

| Endpoint | Purpose |
|----------|---------|
| `GET /markets` | List all supported market identifiers |
| `POST /ws/subscribe` | Start a market websocket subscription |
| `GET /mints` | Query the live mint cache |
| `GET /mints/{mint}/route` | Resolve a swap route for a mint |
| `POST /swap` | Plan or execute a swap against any market |

The TUI and MCP bridge both sit on top of the same routing and swap surface.

## Code paths

The definitive code-level checks for market support:

| File | What to look for |
|------|-----------------|
| `src/dex/swaps.rs` | `Market` enum includes all 10. `DEFAULT_MARKET_PRIORITY` includes all 10. Buy and sell dispatch handle all 10. |
| `src/api/mod.rs` | `/ws/subscribe` accepts all 10. `/swap` resolves routes and executes against the selected market. |
| `src/dex/operator_live_tests.rs` | Contains a `first_ws_mint_buy_sell_confirm` live test for each of the 10 markets. |

Each market has its own adapter file in `src/dex/`:

| Adapter | File |
|---------|------|
| Pump.fun | `src/dex/pump_fun.rs` |
| PumpSwap | `src/dex/pump_swap.rs` |
| Raydium CLMM | `src/dex/raydium_clmm.rs` |
| Raydium CPMM | `src/dex/raydium_cpmm.rs` |
| Raydium AMM v4 | `src/dex/raydium_amm_v4.rs` |
| Raydium Launchpad | `src/dex/raydium_launchpad.rs` |
| Meteora DLMM | `src/dex/meteora_dlmm.rs` |
| Meteora DAMM v1 | `src/dex/meteora_damm_v1.rs` |
| Meteora DAMM v2 | `src/dex/meteora_damm_v2.rs` |
| Meteora DBC | `src/dex/meteora_dbc.rs` |

## Upstream drift control

Protocol-facing work follows the upstream sync process:

1. Refresh sources with `scripts/sync_sources.sh`
2. Verify the result in `UPSTREAM_SOURCES.lock`
3. Record evidence in `STATUS.md`
4. Append findings or workarounds to `FINDINGS.md`
