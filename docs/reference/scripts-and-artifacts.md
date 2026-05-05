# Scripts and Artifacts

## Repository scripts

| Script | Purpose |
| --- | --- |
| `scripts/install_mamba_linux.sh` | Installs Linux host build dependencies, Rust, upstream mirrors, and validates `cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp` |
| `scripts/install_mamba_macos.sh` | Installs macOS host build dependencies, Rust, upstream mirrors, and validates `cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp` |
| `scripts/install_mamba_windows.ps1` | Installs Windows host build dependencies, Rust, upstream mirrors, and validates `cargo build --locked --bin mamba --bin mamba_api --bin mamba_mcp` |
| `scripts/sync_sources.sh` | Refreshes protocol upstream mirrors and rewrites `UPSTREAM_SOURCES.lock` |
| `scripts/snapshot_to_svg.sh` | Converts deterministic text snapshots into embeddable SVG screenshots |
| `scripts/print_mamba_mcp_configs.sh` | Prints ready-to-paste MCP client config snippets for the current checkout |
| `scripts/generate_docs_inventory.sh` | Generates the tracked repository inventory page for this docs site |

## Local-only output directories

| Path | Role |
| --- | --- |
| `artifacts/api-validation/` | Health, route, subscribe, and swap validation captures |
| `artifacts/api-wallet-validation/` | Wallet API validation outputs |
| `artifacts/cli-screenshots/` | Deterministic TUI snapshots used for screenshots and review evidence |
| `artifacts/create-pool/` | Pool-create bundle outputs |
| `artifacts/pool-manage/` | Pool-management bundle outputs |
| `artifacts/validation/` | Focused fix-validation logs and supporting outputs |
| `docs-site/` | Generated MkDocs site output for local preview or static hosting |

## External source directories

| Path | Role |
| --- | --- |
| `external/patches/anchor-idl/` | Checked-in local patch for the `anchor-idl` dependency |
| `external/upstreams/` | Synced upstream protocol references used for drift-resistant integration work |

## Notes

- The generated repository inventory page is based on `git ls-files --cached --others --exclude-standard` plus a small local-only exclusion for `Capture.PNG`, so it reflects the checkout without publishing ignored toolchains, secrets, or build output line-by-line.
