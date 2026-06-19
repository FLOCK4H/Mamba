<p align="center">
  <img src="docs/images/mamba_text_1280_640.png" width="640" alt="Mamba">
</p>

<p align="center">
  <a href="https://github.com/FLOCK4H/Mamba/actions/workflows/docs.yml"><img src="https://github.com/FLOCK4H/Mamba/actions/workflows/docs.yml/badge.svg?event=push" alt="Docs"></a>
  <a href="https://github.com/FLOCK4H/Mamba/stargazers"><img src="https://img.shields.io/github/stars/FLOCK4H/Mamba?style=flat-square" alt="Stars"></a>
  <a href="https://github.com/FLOCK4H/Mamba/commits"><img src="https://img.shields.io/github/last-commit/FLOCK4H/Mamba?style=flat-square" alt="Last commit"></a>
</p>

<p align="center">
  Local Solana DEX toolkit in Rust. Live market feeds, route discovery, swaps, token launches, and pool management across 10 markets.<br>
  Ships as a local API, an MCP server, and a terminal app. All signing stays on your machine.
</p>

<p align="center">
  <a href="docs/quickstart.md">Quickstart</a> · <a href="docs/MAMBA_API.md">API</a> · <a href="docs/MAMBA_MCP.md">MCP</a> · <a href="docs/MAMBA_CLI.md">CLI</a> · <a href="docs/markets.md">Markets</a> · <a href="docs/MCP_CLIENT_SETUP.md">MCP Client Setup</a>
</p>

---

## Install

```bash
# Linux
./scripts/install_mamba_linux.sh

# macOS
./scripts/install_mamba_macos.sh

# Windows PowerShell
powershell -ExecutionPolicy Bypass -File .\scripts\install_mamba_windows.ps1
```

The bootstrap script installs the pinned Rust toolchain, pulls upstream dependencies, and builds all binaries.

```bash
cp .env.example .env
```

Set these in `.env`:

```bash
MAMBA_API_KEY=change_me
MAMBA_API_HTTP_URLS=https://api.devnet.solana.com
MAMBA_API_WS_URLS=wss://api.devnet.solana.com
```

`MAMBA_PRIVATE_KEY` is only needed for signing. Read-only market discovery works without it.

## Pick your surface

**Use MCP with your AI agent?** Build once, connect to Codex, Claude, Gemini CLI, or any stdio MCP client:

```bash
cargo run --bin mamba_api     # start the backend
cargo build --bin mamba_mcp   # build the MCP bridge

# then register with your client (see docs/MCP_CLIENT_SETUP.md)
./scripts/print_mamba_mcp_configs.sh
```

**Build on the API?** `mamba_api` is an authenticated local HTTP server for market data, swaps, wallets, token launches, pools, and stored history:

```bash
cargo run --bin mamba_api

# subscribe to a market
curl -sS -H "x-api-key: $MAMBA_API_KEY" -H "content-type: application/json" \
  -d '{ "market": "pump_fun" }' \
  http://127.0.0.1:8787/mamba-api/v1/ws/subscribe

# read the mint feed
curl -sS -H "x-api-key: $MAMBA_API_KEY" \
  "http://127.0.0.1:8787/mamba-api/v1/mints?markets=pump_fun,pump_swap&limit=20"
```

**Trade from the terminal?** The TUI gives you live monitoring, trading, token creation, pool management, wallet ops, and holder lookups in one screen:

```bash
cargo run --bin mamba
```

## Market coverage

10 markets with websocket ingestion, route lookup, pricing, and buy/sell flows:

| Market | Trade | Create token | Create pool |
|--------|:-----:|:------------:|:-----------:|
| PumpSwap | ✓ | | ✓ |
| Pump.fun | ✓ | ✓ | |
| Raydium AMM v4 | ✓ | | ✓ |
| Raydium Launchpad | ✓ | ✓ | |
| Raydium CLMM | ✓ | | ✓ |
| Raydium CPMM | ✓ | | ✓ |
| Meteora DLMM | ✓ | | ✓ |
| Meteora DAMM v1 | ✓ | | ✓ |
| Meteora DAMM v2 | ✓ | | ✓ |
| Meteora DBC | ✓ | | ✓ |

Token creation also supports `spl_token` and `spl_token_2022` outside of market-specific launch flows.

Full breakdown in [docs/markets.md](docs/markets.md).

## How it works

Mamba connects to Solana websocket streams and fills an in-memory mint cache. Routes, pools, metadata, and pricing are resolved on demand. When you issue a swap, it builds the transaction locally, optionally dry-runs it, and only sends when you explicitly opt in.

- Live sends require `MAMBA_API_ENABLE_LIVE_SENDS=true` in `.env`
- `/swap` plans by default; it sends only when `execute=true`
- Cross-cluster sends are blocked (a devnet runtime can't send to mainnet)
- Signing uses the configured API signer or the managed wallet store

For busy mainnet feeds, use at least 3 HTTP RPCs across 2 providers. Two URLs on the same host can still throttle together.

Optional Postgres storage is available with `MAMBA_API_STORE_MODE=true` and `MAMBA_API_DATABASE_URL`.

## Project layout

```
src/
  api/          HTTP API server (mamba_api)
  mcp/          MCP stdio bridge (mamba_mcp)
  bin/          Binary entrypoints (mamba, mamba_api, mamba_mcp, mamba_tx_inspect)
  dex/          Market implementations and swap router
  core/         Shared types, config, wallet management
  gate/         Auth and rate limiting
  handlers/     Request handlers
  transfers/    SOL and SPL token transfer logic
  swqos/        Stake-weighted QoS for transaction sending
  compute_budget/  Priority fee computation
  utils/        Helpers
```

## Docs

- [Quickstart](docs/quickstart.md)
- [API reference](docs/MAMBA_API.md)
- [MCP server](docs/MAMBA_MCP.md)
- [MCP client setup](docs/MCP_CLIENT_SETUP.md)
- [CLI/TUI](docs/MAMBA_CLI.md)
- [Token creation](docs/MAMBA_CREATE.md)
- [Market breakdown](docs/markets.md)
- [Architecture](docs/architecture.md)
- [Wallet cleaner](docs/features/wallet-cleaner.md)
