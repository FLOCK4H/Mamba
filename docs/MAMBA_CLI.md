# Mamba TUI

`mamba` is the terminal interface for trading, token creation, pool management, wallet operations, and MEV setup. It connects to a running `mamba_api` backend over HTTP.

## Quickstart

Start the API backend first, then launch the TUI.

```bash
# 1. Backend
cp .env.example .env
export MAMBA_API_KEY="change_me"
cargo run --bin mamba_api

# 2. TUI (in a second terminal)
cargo run --bin mamba --
```

## Main Menu

| # | Item | Purpose |
|---|------|---------|
| 1 | Create | Build and sign token-creation transactions |
| 2 | Swap | Paste a mint, fetch a route, review warnings, and trade |
| 3 | Dash | Live market subscriptions with mint inspection and trading controls |
| 4 | Pools | Browse and create liquidity pools |
| 5 | Wallet | View balances and manage accounts |
| 6 | Cleaner | Preview reclaimable token accounts, report recoverable SOL, build or send cleanup batches |
| 7 | MEV / SWQoS | Configure MEV protection and stake-weighted QoS |
| 8 | Help | In-app reference |
| 9 | Quit | Exit the TUI |

## Fee Controls (Swap and Dash)

Both Swap and Dash expose a `priority_fee_level` selector in their trade controls.

| Level | Behavior |
|-------|----------|
| `env` | Uses the default from `.env` (`FEE_LEVEL`) |
| `low` | Low priority fee |
| `medium` | Medium priority fee |
| `high` | High priority fee |
| `turbo` | Aggressive priority fee |
| `max` | Maximum priority fee |
| `custom` | Reveals a `priority_fee_sol` input, entered as decimal SOL (not lamports) |

## Keybinds

| Context | Keys |
|---------|------|
| Global | `Ctrl-C` quit, `F1` help, `F4` provider setup, `Esc` back |
| Menu | `Up`/`Down` navigate, `Enter` select, `1`..`9` jump |
| Cleaner | `Up`/`Down` move, `Left`/`Right` toggle, `r` refresh, `Enter` run BUILD or SEND |
| Create | `Tab` or arrows to move between fields, type to edit, `Enter` build/sign |
| Swap | Paste mint, review route and low-LQ warnings, adjust trade controls, override priority fee per swap |
| Dash | Navigate markets, manage subscriptions, inspect live mints, trade with per-swap fee override |

## Headless Modes

Run `mamba` with flags to skip the interactive TUI and execute a specific task.

### Websocket Validation Suite

Runs market websocket streams and validates parser extraction for a configurable duration.

```bash
cargo run --bin mamba -- --suite --suite-secs 15
```

### Pool-Create Smoke Suite

Builds pool-creation transactions across supported markets to verify instruction construction.

```bash
cargo run --bin mamba -- --pool-suite
```

### Top Holders Lookup

Fetches the largest token holders for a given mint.

```bash
cargo run --bin mamba -- --holders --mint <MINT> --holders-limit 20
```

### Token Creation (Build/Sign)

Builds and signs a token-creation transaction without the interactive UI.

```bash
cargo run --bin mamba -- --create \
  --create-method spl_token \
  --create-name "Example" \
  --create-symbol "EX" \
  --create-uri "https://example.com/token.json" \
  --create-decimals 6
```

### Deterministic Snapshot

Captures a text snapshot of the TUI for CI or visual diffing.

```bash
cargo run --bin mamba -- --snapshot
```

Convert a snapshot to SVG:

```bash
scripts/snapshot_to_svg.sh artifacts/cli-screenshots/<snapshot>.txt docs/images/<name>.svg
```

## Runtime Configuration

**Environment file.** `.env` is loaded automatically at startup.

**API key.** `MAMBA_API_KEY` is required for all API-backed features.

**Multi-RPC.** `MAMBA_API_HTTP_URLS` and `MAMBA_API_WS_URLS` accept comma-separated lists of same-cluster endpoints. The TUI and headless flows inherit the full RPC pool from the API, and local fallback reads reuse the same HTTP list. For busy websocket markets, use at least 3 HTTP RPCs across 2 providers. Two API keys on the same host can still share rate limits.

**Live sends.** Mainnet transactions require both a configured signer and explicit runtime flags (`MAMBA_API_ENABLE_LIVE_SENDS=true`). Without these, all transactions are build-only.

**Low-liquidity confirmation.** Set `AUTO_ACCEPT_LOW_LQ_POOLS=false` to require an extra confirmation before live-sending through a low-liquidity route in Quick Trade.

## Transaction Inspector

`mamba_tx_inspect` decodes a base64-encoded transaction and verifies the expected signer.

```bash
echo "<TX_BASE64>" | cargo run --quiet --bin mamba_tx_inspect -- --expected-signer <WALLET_PUBKEY>
```

Pipe any transaction blob through this utility to confirm signer identity before broadcast.
