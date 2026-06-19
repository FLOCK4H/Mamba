# Repository Inventory

Repository paths indexed from `git ls-files --cached --others --exclude-standard`.

## Coverage

- Indexed paths: 112
- Generated at: 2026-06-19 19:18:20 UTC
- Inventory source: `git ls-files --cached --others --exclude-standard`
- Ignored paths excluded through `.gitignore` (for example `target/`, `.venv-docs/`, `.env`, `docs-site/`, `PACKAGING.md`, and synced `external/upstreams/` mirrors)
- Additional local-only exclusion: `Capture.PNG`

## Top-level groups

| Group | Indexed paths |
| --- | ---: |
| repository root | 10 |
| `.github/` | 1 |
| `docs/` | 17 |
| `external/` | 14 |
| `scripts/` | 7 |
| `src/` | 62 |
| `tests/` | 1 |

## Path listing

The sections below expand to the indexed paths currently present in the checkout.

### Repository root

<details><summary>Show indexed paths (10)</summary>

```text
.env.example
.gitignore
Cargo.lock
Cargo.toml
README.md
UPSTREAM_SOURCES.lock
market_check_19_06_2026.md
mkdocs.yml
requirements-docs.txt
rust-toolchain.toml
```

</details>

### `.github/`

<details><summary>Show indexed paths (1)</summary>

```text
.github/workflows/docs.yml
```

</details>

### `docs/`

<details><summary>Show indexed paths (17)</summary>

```text
docs/MAMBA_API.md
docs/MAMBA_CLI.md
docs/MAMBA_CREATE.md
docs/MAMBA_MCP.md
docs/MCP_CLIENT_SETUP.md
docs/architecture.md
docs/features/wallet-cleaner.md
docs/images/mamba_500_500.png
docs/images/mamba_text_1280_640.png
docs/index.md
docs/markets.md
docs/quickstart.md
docs/reference/module-map.md
docs/reference/repository-inventory.md
docs/reference/scripts-and-artifacts.md
docs/repository-workflow.md
docs/stylesheets/extra.css
```

</details>

### `external/`

<details><summary>Show indexed paths (14)</summary>

```text
external/patches/anchor-idl/.cargo-ok
external/patches/anchor-idl/.cargo_vcs_info.json
external/patches/anchor-idl/Cargo.lock
external/patches/anchor-idl/Cargo.toml
external/patches/anchor-idl/Cargo.toml.orig
external/patches/anchor-idl/README.md
external/patches/anchor-idl/src/account.rs
external/patches/anchor-idl/src/event.rs
external/patches/anchor-idl/src/fields.rs
external/patches/anchor-idl/src/instruction.rs
external/patches/anchor-idl/src/lib.rs
external/patches/anchor-idl/src/program.rs
external/patches/anchor-idl/src/state.rs
external/patches/anchor-idl/src/typedef.rs
```

</details>

### `scripts/`

<details><summary>Show indexed paths (7)</summary>

```text
scripts/generate_docs_inventory.sh
scripts/install_mamba_linux.sh
scripts/install_mamba_macos.sh
scripts/install_mamba_windows.ps1
scripts/print_mamba_mcp_configs.sh
scripts/snapshot_to_svg.sh
scripts/sync_sources.sh
```

</details>

### `src/`

<details><summary>Show indexed paths (62)</summary>

```text
src/api/create.rs
src/api/mod.rs
src/api/pool.rs
src/api/wallet.rs
src/bin/mamba.rs
src/bin/mamba_api.rs
src/bin/mamba_mcp.rs
src/bin/mamba_tx_inspect.rs
src/compute_budget/compute_budget.rs
src/compute_budget/mod.rs
src/constants.rs
src/core/cluster.rs
src/core/create.rs
src/core/ipfs.rs
src/core/mod.rs
src/core/pool.rs
src/core/sol.rs
src/core/wallet.rs
src/dex/meteora_damm_v1.rs
src/dex/meteora_damm_v2.rs
src/dex/meteora_dbc.rs
src/dex/meteora_dlmm.rs
src/dex/mod.rs
src/dex/operator_live_tests.rs
src/dex/pump_fun.rs
src/dex/pump_swap.rs
src/dex/raydium_amm_v4.rs
src/dex/raydium_clmm.rs
src/dex/raydium_cpmm.rs
src/dex/raydium_launchpad.rs
src/dex/swaps.rs
src/gate/mod.rs
src/gate/squeeze.rs
src/handlers/mod.rs
src/handlers/ws.rs
src/idls/meteora_dlmm_types/Cargo.toml
src/idls/meteora_dlmm_types/src/lib.rs
src/idls/pump_fun_types/Cargo.lock
src/idls/pump_fun_types/Cargo.toml
src/idls/pump_fun_types/pump.json
src/idls/pump_fun_types/src/lib.rs
src/idls/pump_swap_types/Cargo.lock
src/idls/pump_swap_types/Cargo.toml
src/idls/pump_swap_types/pump_swap.json
src/idls/pump_swap_types/src/lib.rs
src/idls/raydium_launchpad_types/Cargo.toml
src/idls/raydium_launchpad_types/src/lib.rs
src/lib.rs
src/mcp/mod.rs
src/swqos/blox.rs
src/swqos/helius.rs
src/swqos/jito.rs
src/swqos/mod.rs
src/swqos/nextblock.rs
src/swqos/temporal.rs
src/swqos/zero_slot.rs
src/transfers/cex.rs
src/transfers/mod.rs
src/transfers/wsol.rs
src/utils/mod.rs
src/utils/utils.rs
src/utils/writing.rs
```

</details>

### `tests/`

<details><summary>Show indexed paths (1)</summary>

```text
tests/mamba_mcp.rs
```

</details>

