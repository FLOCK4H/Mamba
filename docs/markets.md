# Markets

## Trading markets

Mamba implements these 10 markets in the trading stack:

1. Raydium CLMM
2. Meteora DLMM
3. Pump.fun
4. PumpSwap AMM
5. Meteora DAMM v1
6. Meteora DAMM v2
7. Meteora DBC
8. Raydium CPMM
9. Raydium AMM v4
10. Raydium Launchpad

In code, those 10 appear in `src/dex/swaps.rs` as the `Market` enum, the default market priority, the websocket route labels, and the market dispatch for route lookup, price lookup, buy, and sell.

## What is implemented

Across those 10 markets, Mamba has code paths for:

- websocket subscription and live mint ingestion
- mint-to-pool route lookup
- price lookup
- creator lookup
- buy execution
- sell execution

The local API exposes those markets through `/markets`, `/ws/subscribe`, `/mints`, `/mints/{mint}/route`, and `/swap`.

The TUI and MCP layer both sit on top of the same routing and swap surface.

## Code evidence

The clearest code-level checks are:

- `src/dex/swaps.rs`
  - `Market` includes all 10 markets
  - `DEFAULT_MARKET_PRIORITY` includes all 10 markets
  - buy and sell dispatch handle all 10 markets
- `src/api/mod.rs`
  - `/ws/subscribe` accepts and dispatches all 10 markets
  - `/swap` resolves routes and executes against the selected market
- `src/dex/operator_live_tests.rs`
  - contains a `first_ws_mint_buy_sell_confirm` live operator test for each of the 10 markets

## Create and pool notes

Trading support and pool creation support are not the same thing.

- `pump_fun` and `raydium_launchpad` are supported trading markets.
- `pump_fun` and `raydium_launchpad` are not standalone pool-create methods because their pool step is part of the launch flow.
- Standalone pool creation is exposed for `pump_swap`, `raydium_cpmm`, `raydium_clmm`, `meteora_dlmm`, `meteora_damm_v1`, `raydium_amm_v4`, `meteora_damm_v2`, and `meteora_dbc`.

## Upstream drift control

Protocol-facing work is tied to the upstream sync process:

- refresh sources with `scripts/sync_sources.sh`
- verify the result in `UPSTREAM_SOURCES.lock`
- keep evidence in `STATUS.md`
- add findings or workarounds to `FINDINGS.md`
