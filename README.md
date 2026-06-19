# Mamba

<p align="center">
  <img src="docs/images/mamba_500_500.png" width="96" alt="Mamba icon">
  <br>
  <img src="docs/images/mamba_text_1280_640.png" width="640" alt="Mamba">
</p>

[![Docs](https://github.com/FLOCK4H/Mamba/actions/workflows/docs.yml/badge.svg?event=push)](https://github.com/FLOCK4H/Mamba/actions/workflows/docs.yml)
[![Stars](https://img.shields.io/github/stars/FLOCK4H/Mamba?style=flat-square)](https://github.com/FLOCK4H/Mamba/stargazers)
[![Last commit](https://img.shields.io/github/last-commit/FLOCK4H/Mamba?style=flat-square)](https://github.com/FLOCK4H/Mamba/commits)

Mamba is a local Solana trading and market toolkit written in Rust. It watches live markets over websocket, keeps a mint cache in memory, finds routes, plans trades, sends them when live mode is unlocked, and exposes the same core through a local API, an MCP server, a terminal app, and a transaction inspector.

[Quickstart](docs/quickstart.md) | [API](docs/MAMBA_API.md) | [MCP](docs/MAMBA_MCP.md) | [CLI](docs/MAMBA_CLI.md) | [Markets](docs/markets.md)

## What traders can do right now

- Subscribe to live markets and keep a local mint feed running in memory.
- Filter that feed by market, search, liquidity, and volume through `/mints` or `/ws/stream`.
- Check route, pool, creator, metadata, price, and low-liquidity warnings before a trade.
- Dry-run or send buys and sells with slippage, retries, priority fee control, and optional SWQoS on mainnet.
- Create wallets, pick the active wallet, check balances, move SOL or SPL tokens, and clean old token accounts.
- Launch tokens with `pump_fun`, `spl_token`, `spl_token_2022`, or `raydium_launchpad`.
- Create pools on supported markets and manage positions that support withdrawal.
- Keep live history in memory, or turn on Postgres store mode for `/transactions`, `/creators`, and `/creator-mints`.

## Choose the surface

| Surface | Use it for | Notes |
| --- | --- | --- |
| `mamba_api` | Local bots, dashboards, backend products, direct HTTP control | Authenticated local API for market data, swaps, wallets, create, pools, and optional stored history |
| `mamba_mcp` | Codex, Claude, and other MCP clients | Same local power through tools. Signing stays inside Mamba |
| `mamba` | Manual trading in a terminal | Quick trade, live monitor, create, pool, wallet, cleaner, holder lookup, snapshots |
| `mamba_tx_inspect` | Checking a built transaction before send | Reads base64 from an argument or `stdin` and checks signer assumptions |

Teams can build on top of `mamba_api` or `mamba_mcp` instead of redoing websocket ingestion, route discovery, wallet flows, token launch flows, and pool builders from scratch.

## Market coverage

Mamba implements 10 markets for websocket ingestion, route lookup, price lookup, and buy or sell flows.

- Trading markets: `pump_swap`, `pump_fun`, `raydium_amm_v4`, `raydium_launchpad`, `raydium_clmm`, `raydium_cpmm`, `meteora_dlmm`, `meteora_damm_v1`, `meteora_damm_v2`, `meteora_dbc`.
- Token create methods: `pump_fun`, `spl_token`, `spl_token_2022`, `raydium_launchpad`.
- Pool create methods: `pump_swap`, `raydium_cpmm`, `raydium_clmm`, `meteora_dlmm`, `meteora_damm_v1`, `raydium_amm_v4`, `meteora_damm_v2`, `meteora_dbc`.
- `pump_fun` and `raydium_launchpad` are not standalone pool-create paths because those pool steps are tied to the launch flow.

See [docs/markets.md](docs/markets.md) for the code-backed market breakdown.

## Quickstart

```bash
# Linux
./scripts/install_mamba_linux.sh

# macOS
./scripts/install_mamba_macos.sh

# Windows PowerShell
powershell -ExecutionPolicy Bypass -File .\scripts\install_mamba_windows.ps1

cp .env.example .env

# start the local API
cargo run --bin mamba_api

# start the terminal app
cargo run --bin mamba --

# build the MCP bridge for GUI clients
cargo build --bin mamba_mcp
```

Set these in `.env` for the first run:

```bash
MAMBA_API_KEY=change_me
MAMBA_API_HTTP_URLS=https://api.devnet.solana.com
MAMBA_API_WS_URLS=wss://api.devnet.solana.com
```

The bootstrap scripts install the pinned Rust toolchain from `rust-toolchain.toml`, install host build deps, refresh `external/upstreams/`, and finish with `cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp`.

`MAMBA_PRIVATE_KEY` is only needed for sign or send flows. Read-only market discovery works without it.

## First calls

Fill the live mint cache for a market:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{ "market": "pump_fun" }' \
  http://127.0.0.1:8787/mamba-api/v1/ws/subscribe
```

Read the current mint feed:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  "http://127.0.0.1:8787/mamba-api/v1/mints?markets=pump_fun,pump_swap&limit=20&min_liquidity=1"
```

Use `/swap` in dry-run mode:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "side": "buy",
    "mint": "<MINT>",
    "buy_sol": 0.01,
    "market_priority": "pump_fun,pump_swap,raydium_clmm",
    "slippage_pct": 25,
    "execute": false
  }' \
  http://127.0.0.1:8787/mamba-api/v1/swap
```

## Runtime

- Host-native Rust binaries on Linux, macOS, and Windows.
- No Docker is required.
- No bundled web frontend ships with the repo.
- Websocket subscriptions fill the in-memory mint cache used by `/mints` and `/ws/stream`.
- `MAMBA_API_HTTP_URLS` and `MAMBA_API_WS_URLS` accept same-cluster comma-separated lists.
- `MAMBA_API_STORE_MODE=true` plus `MAMBA_API_DATABASE_URL` turns on the optional Postgres store.

For busy mainnet websocket markets, use at least 3 HTTP RPCs across 2 providers. Two URLs on the same host can still throttle together.

## Live send model

- Create, pool, wallet transfer, and wallet clean routes are build-first by default.
- `/swap` plans by default. It only sends when `execute=true`.
- Live sends require `MAMBA_API_ENABLE_LIVE_SENDS=true`.
- Signing stays inside Mamba through the configured API signer or the managed wallet store.
- Cross-cluster live sends are blocked. A devnet API runtime cannot be turned into a mainnet sender with `rpc_url`.

## Docs

- [docs/quickstart.md](docs/quickstart.md)
- [docs/MAMBA_API.md](docs/MAMBA_API.md)
- [docs/MAMBA_MCP.md](docs/MAMBA_MCP.md)
- [docs/MAMBA_CLI.md](docs/MAMBA_CLI.md)
- [docs/MAMBA_CREATE.md](docs/MAMBA_CREATE.md)
- [docs/features/wallet-cleaner.md](docs/features/wallet-cleaner.md)
- [docs/architecture.md](docs/architecture.md)
- [docs/markets.md](docs/markets.md)
- [docs/MCP_CLIENT_SETUP.md](docs/MCP_CLIENT_SETUP.md)
