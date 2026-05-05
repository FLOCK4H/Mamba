# Mamba CLI and TUI

`mamba` is the degen-facing terminal application for Create, Swap, Dash, wallet management, and wallet cleaning.

## Main menu

The current main menu is:

1. Create
2. Swap
3. Dash
4. Cleaner
5. Help
6. Quit

![Mamba menu](images/mamba_menu.svg){ .screenshot-frame }

## Cleaner screen

The cleaner is part of the main menu rather than a hidden workflow. It previews reclaimable token accounts, reports recoverable SOL, and supports build or send cleanup batches for selected wallets.

![Mamba cleaner](images/mamba_cleaner.svg){ .screenshot-frame }

## Quickstart

Start the backend first:

```bash
cp .env.example .env
export MAMBA_API_KEY="change_me"
cargo run --bin mamba_api
```

Then start the TUI:

```bash
cargo run --bin mamba --
```

## Headless modes

Websocket validation suite:

```bash
cargo run --bin mamba -- --suite --suite-secs 15
```

Pool-create smoke suite:

```bash
cargo run --bin mamba -- --pool-suite
```

Top-holders lookup:

```bash
cargo run --bin mamba -- --holders --mint <MINT> --holders-limit 20
```

Create build/sign flow:

```bash
cargo run --bin mamba -- --create --create-method spl_token --create-name "Example" --create-symbol "EX" --create-uri "https://example.com/token.json" --create-decimals 6
```

Deterministic snapshots:

```bash
cargo run --bin mamba -- --snapshot
```

Convert a snapshot to SVG:

```bash
scripts/snapshot_to_svg.sh artifacts/cli-screenshots/<snapshot>.txt docs/images/<name>.svg
```

## Runtime notes

- `.env` is loaded automatically.
- `MAMBA_API_KEY` is required for API-backed features.
- `MAMBA_API_HTTP_URLS` / `MAMBA_API_WS_URLS` can be comma-separated same-cluster lists; the TUI and headless flows inherit the API multi-RPC pool end-to-end, and local fallback reads reuse the same HTTP list.
- Live sends require both a signer and explicit allow flags.
- `AUTO_ACCEPT_LOW_LQ_POOLS=false` makes Quick Trade require an extra confirmation before live-sending a low-LQ route.
- Mainnet live sends remain locked unless runtime flags enable them.

Mainnet note:

- For busy websocket markets, prefer at least 3 HTTP RPCs across 2 providers. Two API keys on the same host can still rate-limit together.

## Swap and Dash fee controls

- Swap and Dash both expose `priority_fee_level` in the trade controls.
- Levels are `env`, `low`, `medium`, `high`, `turbo`, `max`, and `custom`.
- `env` keeps the default from `.env` (`FEE_LEVEL`).
- Choosing `custom` reveals `priority_fee_sol`, entered as a decimal SOL amount instead of lamports.

## Keybinds to remember

| Context | Keys |
| --- | --- |
| Global | `Ctrl-C` quit, `F1` help, `F4` provider setup, `Esc` back |
| Menu | `Up/Down`, `Enter`, `1..6` |
| Cleaner | `Up/Down` move, `Left/Right` toggle, `r` refresh, `Enter` run `BUILD` or `SEND` |
| Create | `Tab` or arrows to move, typing to edit, `Enter` build/sign |
| Swap | paste mint, route lookup, low-LQ warning review, trade controls, per-swap priority fee override |
| Dash | market navigation, subscriptions, live mint inspection, trading controls with per-swap priority fee override |

## Utilities

Inspect a base64 transaction and assert the expected signer:

```bash
echo "<TX_BASE64>" | cargo run --quiet --bin mamba_tx_inspect -- --expected-signer <WALLET_PUBKEY>
```
