# Module Map

## Binaries

| Path | Purpose |
|------|---------|
| `src/bin/mamba.rs` | Terminal app: trading, wallet ops, token creation, pool management, snapshots |
| `src/bin/mamba_api.rs` | Launches the authenticated local API backend |
| `src/bin/mamba_mcp.rs` | Stdio MCP bridge for agent clients |
| `src/bin/mamba_tx_inspect.rs` | Transaction inspection tool for signer validation |

## API modules

| Path | Purpose |
|------|---------|
| `src/api/mod.rs` | Router composition, auth middleware, docs endpoint, websocket cache views, store-backed endpoints, shared state |
| `src/api/create.rs` | Create-method discovery and builder routes |
| `src/api/pool.rs` | Pool creation and management routes |
| `src/api/wallet.rs` | Wallet list/create/select, transfer builds, cleaner preview/build/execute |

## Core modules

| Path | Purpose |
|------|---------|
| `src/core/mod.rs` | Core module root |
| `src/core/cluster.rs` | Cluster detection and network intent helpers |
| `src/core/create.rs` | Create transaction composition and helpers |
| `src/core/ipfs.rs` | IPFS upload support for create flows |
| `src/core/pool.rs` | Pool builder and position helpers |
| `src/core/sol.rs` | Solana constants, metadata helpers, WSOL support |
| `src/core/wallet.rs` | Wallet store, transfer builders, cleaner preview/build batching |

## MCP module

| Path | Purpose |
|------|---------|
| `src/mcp/mod.rs` | Stdio MCP server with all tool definitions wrapping the HTTP API |

## Market adapters

| Path | Purpose |
|------|---------|
| `src/dex/mod.rs` | DEX module root |
| `src/dex/swaps.rs` | Shared market enum, route selection, swap orchestration |
| `src/dex/pump_fun.rs` | Pump.fun parsing and routing |
| `src/dex/pump_swap.rs` | PumpSwap parsing and routing |
| `src/dex/raydium_clmm.rs` | Raydium CLMM integration |
| `src/dex/raydium_cpmm.rs` | Raydium CPMM integration |
| `src/dex/raydium_amm_v4.rs` | Raydium AMM v4 integration |
| `src/dex/raydium_launchpad.rs` | Raydium Launchpad integration |
| `src/dex/meteora_dlmm.rs` | Meteora DLMM integration |
| `src/dex/meteora_damm_v1.rs` | Meteora DAMM v1 integration |
| `src/dex/meteora_damm_v2.rs` | Meteora DAMM v2 integration |
| `src/dex/meteora_dbc.rs` | Meteora DBC integration |
| `src/dex/operator_live_tests.rs` | Per-market live operator test suite |

## Support modules

| Path | Purpose |
|------|---------|
| `src/handlers/mod.rs` | Handler module root |
| `src/handlers/ws.rs` | Websocket worker state and mint cache population |
| `src/gate/mod.rs` | Auth middleware module root |
| `src/gate/squeeze.rs` | Rate limiting logic |
| `src/swqos/mod.rs` | SWQoS module root |
| `src/swqos/jito.rs` | Jito relay support |
| `src/swqos/helius.rs` | Helius relay support |
| `src/swqos/blox.rs` | BloXroute relay support |
| `src/swqos/nextblock.rs` | NextBlock relay support |
| `src/swqos/temporal.rs` | Temporal relay support |
| `src/swqos/zero_slot.rs` | ZeroSlot relay support |
| `src/transfers/mod.rs` | Transfer module root |
| `src/transfers/cex.rs` | CEX transfer utilities |
| `src/transfers/wsol.rs` | WSOL wrap/unwrap helpers |
| `src/compute_budget/mod.rs` | Compute budget module root |
| `src/compute_budget/compute_budget.rs` | Compute-unit policy utilities |
| `src/utils/mod.rs` | Utils module root |
| `src/utils/utils.rs` | General helpers |
| `src/utils/writing.rs` | File-writing routines |
| `src/constants.rs` | Shared constants |
| `src/lib.rs` | Crate root |

## IDL crates

| Path | Purpose |
|------|---------|
| `src/idls/pump_fun_types/` | Typed Pump.fun protocol crate with checked-in IDL |
| `src/idls/pump_swap_types/` | Typed PumpSwap protocol crate with checked-in IDL |
| `src/idls/raydium_launchpad_types/` | Typed Raydium Launchpad protocol crate |
| `src/idls/meteora_dlmm_types/` | Typed Meteora DLMM protocol crate |
