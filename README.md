# Mamba

![Mamba icon](docs/images/mamba_icon.png)

Rust-based Solana market kit: multi-market adapters, decoding, routing, a local authenticated API, an MCP server, and a degen-friendly CLI/TUI.

## What it is

- A reusable Solana Rust integration kit (library + binaries), not a single-purpose bot.
- `mamba_api`: an authenticated local HTTP API with websocket-backed market ingestion plus build-first transaction builders and execute routes that keep signing inside Mamba.
- `mamba_mcp`: a stdio MCP server that mirrors Mamba API functionality so an agent can buy, sell, list tokens, transfer assets, clean wallets, create tokens, and manage pools without direct key access.
- `mamba`: a CLI/TUI for live websocket validation, Create (token launch), Trade UX, and deterministic snapshot “screenshots”.

## Quickstart (local)

```bash
# Linux bootstrap
./scripts/install_mamba_linux.sh

# macOS bootstrap
./scripts/install_mamba_macos.sh

# Windows PowerShell bootstrap
powershell -ExecutionPolicy Bypass -File .\scripts\install_mamba_windows.ps1

cp .env.example .env
export MAMBA_API_KEY='change_me'

# API (authenticated; default bind 127.0.0.1:8787, base /mamba-api)
cargo run --bin mamba_api

# MCP bridge binary (recommended for Codex/GUI MCP clients)
cargo build --bin mamba_mcp
./target/debug/mamba_mcp

# CLI/TUI (devnet defaults when URLs are unset)
cargo run --bin mamba --
```

The bootstrap scripts read the pinned Rust toolchain from `rust-toolchain.toml`, install host build dependencies, refresh `external/upstreams/` via `scripts/sync_sources.sh`, and finish with `cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp` by default.

`./scripts/print_mamba_mcp_configs.sh` emits MCP client snippets for the current checkout. Client-specific setup lives in `docs/MCP_CLIENT_SETUP.md`.

## Platform support

- Linux: first-class path. The repo is actively validated on a GUI Linux VM.
- macOS: supported through `scripts/install_mamba_macos.sh` plus the normal Cargo workflow.
- Windows: supported through `scripts/install_mamba_windows.ps1` and PowerShell.

The primary runtime is host-native Rust binaries. Docker and a frontend are not required.

`mamba_api` maintains an in-memory websocket mint cache for live market views. `MAMBA_API_STORE_MODE=true` optionally enables a Postgres-backed API store for `/transactions`, `/creators`, and `/creator-mints`.

## Required environment

Minimum setup:

- `MAMBA_API_KEY` for authenticated API routes
- `MAMBA_PRIVATE_KEY` for signed build or execute flows
- `MAMBA_API_HTTP_URLS` and `MAMBA_API_WS_URLS` for explicit RPC selection

Optional store mode:

- `MAMBA_API_STORE_MODE=true`
- `MAMBA_API_DATABASE_URL=<postgres connection string>`

`MAMBA_API_HTTP_URLS` and `MAMBA_API_WS_URLS` accept comma-separated lists. `mamba_api`, `mamba`, and `mamba_mcp` all use that pool end-to-end: read-heavy paths rotate across it automatically and temporarily cool down rate-limited endpoints instead of hammering the same RPC.

Useful cluster examples:

```bash
# Devnet
export MAMBA_API_HTTP_URLS=https://api.devnet.solana.com
export MAMBA_API_WS_URLS=wss://api.devnet.solana.com

# Mainnet
export MAMBA_API_HTTP_URLS=https://api.mainnet-beta.solana.com
export MAMBA_API_WS_URLS=wss://api.mainnet-beta.solana.com
```

Mainnet recommendation:

- Use at least 3 HTTP RPCs across 2 providers for sustained websocket + parsed-transaction workloads.
- Do not count multiple API keys on the same host as real diversity; `https://mainnet.helius-rpc.com/?api-key=A` and `...?api-key=B` can still 429 together.
- Keep every URL in the list on the same Solana cluster.

For LAN or host-visible API access, bind to a non-loopback address and explicitly allow private-network clients:

```bash
export MAMBA_API_BIND_ADDR=0.0.0.0:8787
export MAMBA_API_ALLOW_PRIVATE_NETWORK_CLIENTS=true
```

## Documentation

MkDocs Material site sourced from `docs/` plus a generated repository inventory page.

Local preview:

```bash
python3 -m venv .venv-docs
. .venv-docs/bin/activate
pip install -r requirements-docs.txt
./scripts/generate_docs_inventory.sh
mkdocs serve -a 0.0.0.0:8000
```

GitHub Pages publication is defined in [`.github/workflows/docs.yml`](.github/workflows/docs.yml) and uses the `GitHub Actions` source.

Primary source pages:

- Overview: `docs/index.md`
- API: `docs/MAMBA_API.md`
- MCP: `docs/MAMBA_MCP.md`
- MCP client setup: `docs/MCP_CLIENT_SETUP.md`
- CLI/TUI: `docs/MAMBA_CLI.md`
- Create runbook: `docs/MAMBA_CREATE.md`
- Wallet cleaner: `docs/features/wallet-cleaner.md`

## Runtime checks

API health:

```bash
curl -sS -H "x-api-key: $MAMBA_API_KEY" \
  http://127.0.0.1:8787/mamba-api/v1/health
```

Deterministic TUI snapshots:

```bash
cargo run --bin mamba -- --snapshot
```

Snapshots are written to `artifacts/cli-screenshots/`.

## Supported markets (10)

- Raydium CLMM
- Meteora DLMM
- Pump.fun
- PumpSwap AMM
- Meteora DAMM v1
- Meteora DAMM v2
- Meteora DBC (Believe path)
- Raydium CPMM
- Raydium AMM v4
- Raydium Launchpad

## Safety model (high level)

- Build-first by default. Live execution routes exist, but signing stays inside Mamba through the configured API signer or managed wallet store.
- Treat `.env` and any private keys as sensitive.
- Devnet is the expected automated validation cluster.
- Mainnet execution stays manual by default unless explicitly unlocked for a session.

## Devnet validation

Smoke-test the authenticated API and local create/pool coverage:

```bash
# Start the API on devnet
MAMBA_API_HTTP_URLS=https://api.devnet.solana.com \
MAMBA_API_WS_URLS=wss://api.devnet.solana.com \
cargo run --bin mamba_api

# In another shell
cargo run --bin mamba -- --pool-suite --http-url https://api.devnet.solana.com
```

When the suite asks for cached setup state, run the one-time setup path:

```bash
cargo run --bin mamba -- --pool-suite --pool-suite-setup-send --http-url https://api.devnet.solana.com
```

## Troubleshooting

### API startup fails before `/health` appears

Mamba surfaces the upstream `getGenesisHash` reason instead of only returning a generic RPC failure. A common case is an exhausted hosted provider, for example:

```text
rpc error -32429: max usage reached
```

Check the latest autostart or direct API logs under `artifacts/logs/`, then either refresh the provider key or override to a healthy RPC endpoint:

```bash
MAMBA_API_HTTP_URLS=https://api.mainnet-beta.solana.com \
MAMBA_API_WS_URLS=wss://api.mainnet-beta.solana.com \
cargo run --bin mamba_api
```

### Raydium Launchpad create simulation fails with `InvalidInput`

Launchpad supply limits are enforced against token decimals. Increasing decimals without scaling raw supply causes the program to reject the create request during `InitializeV2`. The pool suite uses `decimals=6` for the Launchpad smoke path because that matches Raydium's documented default and current devnet-compatible presets.

### Websocket-backed views look empty

Start `mamba_api`, verify `/health`, then leave subscriptions active for at least 15 seconds. The validation harnesses and monitor surfaces are websocket-first and intentionally avoid tight HTTP polling loops.
