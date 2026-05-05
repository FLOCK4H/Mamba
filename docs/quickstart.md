# Quickstart

## Prerequisites

- Linux bootstrap script:

```bash
./scripts/install_mamba_linux.sh
```

- Windows PowerShell bootstrap:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install_mamba_windows.ps1
```

- Both scripts install the Rust toolchain pinned in `rust-toolchain.toml`, install host build dependencies, refresh `external/upstreams/`, and run `cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp` by default.
- Network access for Solana RPC endpoints and upstream source sync
- A private `.env` copied from `.env.example`

## Local runtime

Copy the example environment first:

```bash
cp .env.example .env
```

Start the authenticated local API:

```bash
cargo run --bin mamba_api
```

Build the MCP binary once for agent clients:

```bash
cargo build --bin mamba_mcp
```

Run the built MCP bridge locally:

```bash
./target/debug/mamba_mcp
```

!!! note
    MCP client snippets for the current checkout come from `./scripts/print_mamba_mcp_configs.sh`. Client-specific setup lives in `docs/MCP_CLIENT_SETUP.md`.

Start the CLI/TUI:

```bash
cargo run --bin mamba --
```

Release/manual path for the backend:

```bash
cargo run --bin mamba_api --release
```

## Defaults and overrides

- Default API bind: `127.0.0.1:8787`
- Default API route base: `/mamba-api`
- Default cluster intent in checked-in examples: devnet
- Route base is configurable via `MAMBA_API_ROUTE_BASE`

## Cleaner snapshot and docs validation

Generate deterministic TUI snapshots:

```bash
cargo run --bin mamba -- --snapshot
```

Build the documentation site locally:

```bash
python3 -m venv .venv-docs
. .venv-docs/bin/activate
pip install -r requirements-docs.txt
./scripts/generate_docs_inventory.sh
.venv-docs/bin/mkdocs serve -a 0.0.0.0:8000
```

Debian and Ubuntu require `python3-venv` when `python3 -m venv` is unavailable. The Linux bootstrap script installs it automatically.

Strict local build:

```bash
.venv-docs/bin/mkdocs build --strict
```

## Local-only maintainer files

These paths are intentionally ignored by default so a simple `git add .` does not publish them:

- `.env`
- `.venv-docs/`
- `docs-site/`
- `artifacts/`
- `PACKAGING.md`
