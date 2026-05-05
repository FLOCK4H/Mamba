# Architecture

## Runtime model

Mamba is one crate with three user-facing binaries and a set of protocol adapters:

| Surface | Path | Responsibility |
| --- | --- | --- |
| Local API | `src/bin/mamba_api.rs` | Starts the authenticated Axum backend through `mamba::api::run_from_env()` |
| CLI/TUI | `src/bin/mamba.rs` | Interactive trading terminal, snapshots, wallet actions, create flows, swap flows, dashboards |
| Inspect tool | `src/bin/mamba_tx_inspect.rs` | Reads a base64 transaction and verifies expected signer assumptions |

## Crate layout

| Module | Path | Role |
| --- | --- | --- |
| API | `src/api/` | Axum routes, auth, docs, websocket cache views, optional store-backed endpoints, wallet/create/pool handlers |
| Core | `src/core/` | Shared Solana utilities, create/pool/wallet builders, signer helpers, cluster handling |
| DEX | `src/dex/` | Market-specific integrations and route logic |
| Handlers | `src/handlers/` | Websocket ingestion and live mint cache plumbing |
| SWQoS | `src/swqos/` | Provider-specific transport and relay settings |
| Transfers | `src/transfers/` | SOL, WSOL, and transfer helpers |
| Compute budget | `src/compute_budget/` | Compute-unit policy utilities |
| Utils | `src/utils/` | File-writing and helper utilities |

## Data flow

1. `mamba_api` loads environment and starts authenticated routes.
2. Websocket subscriptions populate runtime market caches through `src/handlers/ws.rs`.
3. Read endpoints resolve cached mints, routes, creators, and metadata from the live websocket cache, with optional store-backed overlays when `MAMBA_API_STORE_MODE=true`.
4. Builder endpoints return unsigned transactions and optional simulations.
5. `mamba` consumes those builders, signs locally when required, and can render deterministic snapshots for UI evidence.

## Wallet surfaces

Wallet operations now split into two tracks:

- Transfer builder: move SOL or SPL assets between locally managed wallets.
- Cleaner builder: inspect token accounts, unwrap WSOL, close empty accounts, and optionally burn non-zero token balances before closing accounts.

The cleaner logic lives in `src/core/wallet.rs`, the HTTP routes live in `src/api/wallet.rs`, and the main-menu screen lives in `src/bin/mamba.rs`.

## External source strategy

- `scripts/sync_sources.sh` refreshes protocol references into `external/upstreams/`.
- `UPSTREAM_SOURCES.lock` records branch and commit state for those references.
- `external/patches/anchor-idl/` contains the checked-in local patch for `anchor-idl`.

## Evidence strategy

This public repository keeps the code and docs in-tree while leaving local validation output ignored by default:

- `docs-site/` is the generated MkDocs output directory.
- `artifacts/` is the local workspace for API checks, CLI snapshots, and validation runs.
- `.env`, `PACKAGING.md`, and local tool virtualenvs stay ignored so a simple `git add .` does not publish secrets or maintainer-only notes.
