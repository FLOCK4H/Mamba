# Mamba

<div class="home-hero">
  <div class="eyebrow">SOLANA RUST DEV KIT</div>
  <h1>Find the pool. Build the tx. Send with intent.</h1>
  <p>Mamba is a Solana Rust kit for sniffing markets, streaming websockets, building swaps and launches, and running wallet ops. Runtime surfaces: Local API, terminal app, and MCP bridge. Signing stays local; receipts stay inspectable.</p>
  <div class="hero-actions">
    <a class="md-button md-button--primary" href="quickstart/">Quickstart</a>
    <a class="md-button" href="MAMBA_API/">Local API</a>
    <a class="md-button" href="MAMBA_MCP/">MCP guide</a>
    <a class="md-button" href="features/wallet-cleaner/">Cleaner playbook</a>
  </div>
</div>

<div class="home-grid">
  <div class="surface-card">
    <span class="surface-label">Build surface</span>
    <strong>Authenticated local API</strong>
    <p>Swaps, creates, pool ops, transfers, and wallet cleaning. Build first. Execute after an explicit mode flip. Signing stays inside Mamba.</p>
  </div>
  <div class="surface-card">
    <span class="surface-label">Degen surface</span>
    <strong>CLI and TUI</strong>
    <p>One terminal app for create, swap, dashboard, wallet ops, and the cleaner. Deterministic snapshots keep UI evidence reproducible.</p>
  </div>
  <div class="surface-card">
    <span class="surface-label">Agent surface</span>
    <strong>MCP server</strong>
    <p>`mamba_mcp` mirrors the authenticated API so agent clients can queue trades, mints, transfers, cleanup, and pool actions without key exposure.</p>
  </div>
  <div class="surface-card">
    <span class="surface-label">Protocol surface</span>
    <strong>10 target markets</strong>
    <p>Market adapters stay isolated. Shared routing, metadata, token logic, and validation live in common modules, so one adapter does not brick the rest.</p>
  </div>
</div>

## Site contents

- The Rust crate map under `src/`: binaries, shared modules, and market adapters. No mystery meat.
- Runtime surfaces in `mamba_api` and `mamba`: authenticated HTTP routes plus the TUI main menu.
- The public repo files that matter most: `README.md`, `.env.example`, `UPSTREAM_SOURCES.lock`, `docs/`, `scripts/`, `src/`, and `tests/`.
- A generated repository inventory page sourced from the tracked tree. If it is not in the tree, it did not happen.

## Runtime rules

- Build first by default. Execute routes exist, but signing stays inside Mamba through local signer material the agent never receives.
- Devnet rehearsal is expected. Mainnet sends stay manual unless runtime flags unlock them.
- Adapters stay protocol specific. Shared infrastructure lives in `src/core`, `src/api`, `src/handlers`, `src/transfers`, and `src/swqos`.
- Local validation output stays ignored by default so docs builds, screenshots, logs, and secrets do not get pushed by accident.

## Current surfaces

- Wallet cleaner support in both the local API and the TUI main menu.
- Cleaner preview and build flows that classify SPL Token and Token 2022 accounts into unwrap, burn and close, close empty, or skip.
- A GitHub Pages ready MkDocs Material site with a custom visual layer and a repo inventory.

## Entry points

- New to the repo: [Quickstart](quickstart.md)
- Need the runtime shape: [Architecture](architecture.md)
- Working on routes or builders: [Local API](MAMBA_API.md)
- Wiring an agent to Mamba: [MCP guide](MAMBA_MCP.md)
- Need client-specific MCP setup: [MCP client setup](MCP_CLIENT_SETUP.md)
- Operating the terminal app: [CLI and TUI](MAMBA_CLI.md)
- Cleaning wallets safely: [Wallet Cleaner](features/wallet-cleaner.md)
