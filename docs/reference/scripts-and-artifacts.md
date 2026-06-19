# Scripts and Artifacts

## Repository scripts

| Script | What it does |
|--------|-------------|
| `scripts/install_mamba_linux.sh` | Installs Linux build deps, Rust toolchain, upstream mirrors, validates `cargo build --locked` |
| `scripts/install_mamba_macos.sh` | Same for macOS |
| `scripts/install_mamba_windows.ps1` | Same for Windows (PowerShell) |
| `scripts/sync_sources.sh` | Refreshes protocol upstream mirrors and rewrites `UPSTREAM_SOURCES.lock` |
| `scripts/print_mamba_mcp_configs.sh` | Prints MCP client config snippets for the current checkout |
| `scripts/snapshot_to_svg.sh` | Converts deterministic text snapshots into embeddable SVG screenshots |
| `scripts/generate_docs_inventory.sh` | Generates the tracked repository inventory page for this docs site |

## Local output directories

These directories are gitignored and used for local validation work:

| Path | Contents |
|------|----------|
| `artifacts/api-validation/` | Health, route, subscribe, and swap validation captures |
| `artifacts/api-wallet-validation/` | Wallet API validation outputs |
| `artifacts/cli-screenshots/` | Deterministic TUI snapshots for review evidence |
| `artifacts/create-pool/` | Pool-create bundle outputs |
| `artifacts/pool-manage/` | Pool-management bundle outputs |
| `artifacts/validation/` | Focused fix-validation logs |
| `docs-site/` | Generated MkDocs site output |

## External sources

| Path | Contents |
|------|----------|
| `external/patches/anchor-idl/` | Checked-in local patch for the `anchor-idl` dependency |
| `external/upstreams/` | Synced upstream protocol references for drift-resistant integration |

## Notes

The generated repository inventory page is based on `git ls-files --cached --others --exclude-standard`. Ignored toolchains, secrets, and build output stay out of the published listing.
