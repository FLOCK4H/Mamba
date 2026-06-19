# Quickstart

## Install

Run the bootstrap script for your platform. It installs the Rust toolchain pinned in `rust-toolchain.toml`, pulls host build dependencies, refreshes `external/upstreams/`, and runs `cargo build --locked` for all binaries.

=== "Linux / macOS"

    ```bash
    ./scripts/install_mamba_linux.sh
    ```

=== "Windows"

    ```powershell
    powershell -ExecutionPolicy Bypass -File .\scripts\install_mamba_windows.ps1
    ```

You will also need network access to Solana RPC endpoints for runtime.

## Run

Copy the example environment file, then start whichever binaries you need.

```bash
cp .env.example .env
```

| Binary | Command | Purpose |
|---|---|---|
| **API server** | `cargo run --bin mamba_api` | Local authenticated API (dev build) |
| **API server (release)** | `cargo run --bin mamba_api --release` | Optimized build for production use |
| **TUI** | `cargo run --bin mamba --` | Interactive terminal interface |
| **MCP bridge (build)** | `cargo build --bin mamba_mcp` | Build the MCP binary for agent clients |
| **MCP bridge (run)** | `./target/debug/mamba_mcp` | Start the stdio-based MCP bridge |

!!! note
    Run `./scripts/print_mamba_mcp_configs.sh` to generate MCP client config snippets for your checkout. Full client setup details are in [MCP Client Setup](MCP_CLIENT_SETUP.md).

## Defaults

| Setting | Default | Override |
|---|---|---|
| API bind address | `127.0.0.1:8787` | `MAMBA_API_HOST` / `MAMBA_API_PORT` |
| Route base | `/mamba-api` | `MAMBA_API_ROUTE_BASE` |
| Cluster | devnet | RPC URL in `.env` |

## Optional: TUI Snapshots

Generate a deterministic TUI snapshot (useful for CI or visual verification):

```bash
cargo run --bin mamba -- --snapshot
```

## Optional: Local Docs Site

```bash
python3 -m venv .venv-docs
. .venv-docs/bin/activate
pip install -r requirements-docs.txt
./scripts/generate_docs_inventory.sh
.venv-docs/bin/mkdocs serve -a 0.0.0.0:8000
```

On Debian/Ubuntu, install `python3-venv` if `python3 -m venv` fails. The Linux bootstrap script handles this automatically.

For a strict build (catches warnings as errors):

```bash
.venv-docs/bin/mkdocs build --strict
```

## Gitignored Maintainer Files

These paths are gitignored so `git add .` stays safe:

| Path | Contents |
|---|---|
| `.env` | Local secrets and RPC config |
| `.venv-docs/` | Docs virtualenv |
| `docs-site/` | MkDocs build output |
| `artifacts/` | Runtime artifacts, screenshots |
| `PACKAGING.md` | Release packaging notes |
