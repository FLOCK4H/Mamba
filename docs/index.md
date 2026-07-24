# Mamba

<div class="home-hero">
  <img class="home-hero-brand" src="images/mamba_text_1280_640.png" alt="Mamba">
  <h1>A Solana adapter for traders and developers.</h1>
  <p>Read markets, inspect pools, build swaps, create tokens, and run wallet operations across 10 protocols. Mamba provides these capabilities through a local API, terminal app, and MCP bridge. Mamba charges no software fee, and signing stays on your machine.</p>
  <div class="hero-actions">
    <a class="md-button md-button--primary" href="quickstart/">Start Building</a>
    <a class="md-button" href="MAMBA_API/">API Reference</a>
    <a class="md-button" href="MAMBA_MCP/">MCP Integration</a>
    <a class="md-button" href="MAMBA_CLI/">TUI Guide</a>
  </div>
</div>

<div class="home-grid">
  <a href="MAMBA_API/" class="feature-card">
    <div class="feature-icon">
      <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"></path><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"></path></svg>
    </div>
    <strong>Local HTTP API</strong>
    <p>Construct swaps, pools, and token launches locally. Mamba handles the routing and metadata, returning unsigned transactions ready for execution.</p>
  </a>
  <a href="MAMBA_CLI/" class="feature-card">
    <div class="feature-icon">
      <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"></polyline><line x1="12" y1="19" x2="20" y2="19"></line></svg>
    </div>
    <strong>Terminal Interface</strong>
    <p>A terminal interface for traders and operators. Review trades, manage wallets, clean up dust, and capture deterministic snapshots for UI evidence.</p>
  </a>
  <a href="MAMBA_MCP/" class="feature-card">
    <div class="feature-icon">
      <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="11" width="18" height="10" rx="2"></rect><circle cx="12" cy="5" r="2"></circle><path d="M12 7v4"></path><line x1="8" y1="16" x2="8" y2="16"></line><line x1="16" y1="16" x2="16" y2="16"></line></svg>
    </div>
    <strong>MCP Server</strong>
    <p>Connect your AI agents. The stdio bridge exposes the entire API surface without ever leaking private keys to the language model.</p>
  </a>
  <a href="markets/" class="feature-card">
    <div class="feature-icon">
      <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 12h-4l-3 9L9 3l-3 9H2"></path></svg>
    </div>
    <strong>10 DEX Adapters</strong>
    <p>Deep integration with Raydium, Meteora, Pump.fun, and PumpSwap. Shared routing logic keeps market adapters isolated and safe.</p>
  </a>
  <a href="MAMBA_SEARCH/" class="feature-card">
    <div class="feature-icon">
      <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"></circle><path d="m21 21-4.3-4.3"></path></svg>
    </div>
    <strong>Mamba Search</strong>
    <p>Inspect every WSOL pool for a mint across all supported markets, with concurrent RPC discovery and exact pool reserves.</p>
  </a>
</div>

## Site contents

This site covers the Rust crate layout under `src/` (binaries, shared modules, market adapters), the runtime surfaces exposed by `mamba_api` and `mamba`, and the public repo files that matter: `README.md`, `.env.example`, `UPSTREAM_SOURCES.lock`, `docs/`, `scripts/`, `src/`, `tests/`.

A generated repository inventory page is sourced from the tracked tree. If it is not in the tree, it is not documented.

## Runtime rules

| Rule | Detail |
|---|---|
| **Build first** | Transactions are constructed but not sent by default. Live sends require `MAMBA_API_ENABLE_LIVE_SENDS=true`. |
| **Devnet rehearsal** | Mainnet sends stay manual unless runtime flags unlock them. Test on devnet first. |
| **Adapter isolation** | Market adapters are protocol-specific. Shared infrastructure lives in `src/core`, `src/api`, `src/handlers`, `src/transfers`, `src/swqos`. |
| **Local output ignored** | Docs builds, screenshots, logs, and secrets are gitignored so nothing sensitive gets pushed by accident. |

## Current surfaces

| Surface | Status |
|---|---|
| Wallet cleaner | Available in both the local API and the TUI main menu. Preview and build flows classify SPL Token and Token-2022 accounts into unwrap, burn-and-close, close-empty, or skip. |
| Mamba Search | Available at `GET /mamba-search/{mint}` with all-market discovery, multi-RPC fan-out, and partial-result reporting. |
| Documentation site | GitHub Pages MkDocs Material site with a custom visual layer and a repo inventory. |

## Entry points

| Looking for… | Go to |
|---|---|
| First-time setup | [Quickstart](quickstart.md) |
| Runtime shape and crate map | [Architecture](architecture.md) |
| HTTP routes and builders | [Local API](MAMBA_API.md) |
| All-market mint pool discovery | [Mamba Search](MAMBA_SEARCH.md) |
| Agent integration | [MCP guide](MAMBA_MCP.md) |
| Client-specific MCP setup | [MCP client setup](MCP_CLIENT_SETUP.md) |
| Terminal app operation | [CLI and TUI](MAMBA_CLI.md) |
| Wallet cleanup | [Wallet Cleaner](features/wallet-cleaner.md) |
