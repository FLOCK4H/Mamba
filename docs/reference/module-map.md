# Module Map

## Binaries

| Path | Purpose |
| --- | --- |
| `src/bin/mamba.rs` | Main CLI/TUI, snapshots, wallet operations, create and swap UX |
| `src/bin/mamba_api.rs` | Launches the authenticated local API |
| `src/bin/mamba_tx_inspect.rs` | Transaction inspection helper for signer validation |

## API modules

| Path | Purpose |
| --- | --- |
| `src/api/mod.rs` | Router composition, auth, docs endpoint, shared state |
| `src/api/create.rs` | Create-method discovery and create-builder routes |
| `src/api/pool.rs` | Pool creation and pool management routes |
| `src/api/wallet.rs` | Managed wallet list/create/select, transfer builds, cleaner preview/build |

## Core modules

| Path | Purpose |
| --- | --- |
| `src/core/cluster.rs` | Cluster detection and network intent helpers |
| `src/core/create.rs` | Create transaction composition and helpers |
| `src/core/ipfs.rs` | IPFS upload support used by create flows |
| `src/core/pool.rs` | Pool builder and position helpers |
| `src/core/sol.rs` | Solana constants, metadata helpers, WSOL support |
| `src/core/wallet.rs` | Wallet store, transfer builders, cleaner preview/build batching |

## Market adapters

| Path | Purpose |
| --- | --- |
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
| `src/dex/swaps.rs` | Shared market enum, route selection, swap orchestration |

## Support modules

| Path | Purpose |
| --- | --- |
| `src/handlers/ws.rs` | Websocket worker state and mint cache population |
| `src/swqos/` | Provider-specific relay and transaction-delivery support |
| `src/transfers/` | SOL, WSOL, and transfer utilities |
| `src/compute_budget/` | Compute-budget helpers |
| `src/utils/` | General helper and file-writing routines |
| `src/idls/` | Typed protocol crates and checked-in JSON IDLs |
