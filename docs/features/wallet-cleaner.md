# Wallet Cleaner

The wallet cleaner adds Cobra-style cleanup primitives to Mamba's local API, MCP bridge, and terminal app without weakening the repo's build-first safety model.

## What it does

- previews SPL Token and Token-2022 accounts owned by a selected wallet
- classifies each token account as `unwrap_wsol`, `burn_and_close`, `close_empty`, or `skip`
- builds unsigned cleanup transactions in size-safe batches
- simulates each batch through the API before the CLI or execute route signs or sends it

## Cleaner actions

| Action | Meaning |
| --- | --- |
| `unwrap_wsol` | Close a native WSOL account and reclaim its lamports |
| `burn_and_close` | Burn a non-zero token balance, then close the token account |
| `close_empty` | Close a zero-balance token account |
| `skip` | Leave the account alone because it is not cleanable under current rules |

## API surface

Preview one wallet:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  --get \
  --data-urlencode "owner=<WALLET_PUBKEY>" \
  --data-urlencode "rpc_url=https://api.devnet.solana.com" \
  http://127.0.0.1:8787/mamba-api/v1/wallets/clean/preview
```

Build cleanup batches:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "owner": "<WALLET_PUBKEY>",
    "burn_nonzero": true,
    "close_empty": true,
    "close_wsol": true,
    "simulate": true,
    "rpc_url": "https://api.devnet.solana.com"
  }' \
  http://127.0.0.1:8787/mamba-api/v1/wallets/clean/build
```

The build response includes:

- the original preview
- selected reclaim totals
- one or more unsigned transaction batches
- per-batch simulation result when `simulate` is enabled

Execute cleanup batches:

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  -H "content-type: application/json" \
  -d '{
    "owner": "<WALLET_PUBKEY>",
    "burn_nonzero": true,
    "close_empty": true,
    "close_wsol": true,
    "simulate": true,
    "rpc_url": "https://api.devnet.solana.com"
  }' \
  http://127.0.0.1:8787/mamba-api/v1/wallets/clean/execute
```

## TUI flow

- Open `Cleaner` from the main menu.
- Select the target network.
- Toggle `burn_nonzero`, `close_empty`, and `unwrap_wsol`.
- Press `r` to refresh previews.
- `Enter` on `BUILD` produces bundles. `SEND` is available only when live sending is intentionally enabled for the local wallet flow.

![Cleaner screen](../images/mamba_cleaner.svg){ .screenshot-frame }

## Safety notes

- Non-zero token burns are opt-in through `burn_nonzero`.
- `/wallets/clean/build` is unsigned and deterministic. `/wallets/clean/execute` signs locally only when live sends are intentionally enabled.
- The owner wallet must be locally signable through the configured API signer or managed wallet store before execute requests are accepted.
- Batch building enforces Solana transaction wire-size limits before returning unsigned transactions.
