# Markets

## Target market set

The repository target is full parity across these 10 markets:

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

## Current split to keep explicit

Per `AGENTS.md`, the main-menu mint-first Buy/Sell flow is not yet considered market-complete. The repo should keep this split explicit until the remaining markets are fully stable.

Coverage snapshot from `AGENTS.md`, dated **March 14, 2026**:

| Mint-first Buy/Sell flow | Markets |
| --- | --- |
| Enabled now | Raydium CLMM, Meteora DLMM, Pump.fun, PumpSwap AMM, Meteora DAMM v1 |
| Still pending or hanging | Meteora DAMM v2, Meteora DBC, Raydium CPMM, Raydium AMM v4, Raydium Launchpad |

## Integration contract

Each market integration is expected to include:

- protocol-specific constants and discriminators kept inside the market module
- websocket subscription coverage comparable to Pump.fun and PumpSwap patterns
- decoder and instruction-contract fixtures
- actionable diagnostics for send and confirm failures

## Upstream drift control

Protocol-facing work is tied to the upstream sync process:

- refresh sources with `scripts/sync_sources.sh`
- verify the result in `UPSTREAM_SOURCES.lock`
- keep evidence in `STATUS.md`
- add findings or workarounds to `FINDINGS.md`
