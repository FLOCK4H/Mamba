# Wallet Cleaner

Mamba's wallet cleaner scans SPL Token and Token-2022 accounts, classifies each one for cleanup, and builds unsigned transactions in wire-safe batches. Every batch is simulated before anything is signed or sent.

## How It Works

1. **Preview** the wallet's token accounts and see what the cleaner would do with each one.
2. **Build** unsigned cleanup transactions grouped into size-safe batches, with optional simulation.
3. **Execute** to sign and send. This step requires `MAMBA_API_ENABLE_LIVE_SENDS=true`.

The cleaner respects Mamba's build-first safety model throughout. No keys leave the local signer, and burning non-zero balances is always opt-in.

## Account Actions

Each token account is classified into one of four actions:

| Action | What Happens |
| --- | --- |
| `unwrap_wsol` | Closes a native WSOL account and reclaims its lamports. |
| `burn_and_close` | Burns the remaining token balance, then closes the account. |
| `close_empty` | Closes a zero-balance token account. |
| `skip` | Leaves the account untouched (not cleanable under current rules). |

## API Endpoints

All endpoints require the `x-api-key` header.

### Preview Accounts

`GET /wallets/clean/preview`

Returns classified token accounts for the given wallet.

| Parameter | Type | Description |
| --- | --- | --- |
| `owner` | string | Wallet public key to scan. |
| `rpc_url` | string | Solana RPC endpoint. |

```bash
curl -sS \
  -H "x-api-key: $MAMBA_API_KEY" \
  --get \
  --data-urlencode "owner=<WALLET_PUBKEY>" \
  --data-urlencode "rpc_url=https://api.devnet.solana.com" \
  "$MAMBA_API_BASE/wallets/clean/preview"
```

### Build Cleanup Batches

`POST /wallets/clean/build`

Produces unsigned transactions grouped into batches that respect Solana's transaction wire-size limit. The response includes the full preview, reclaim totals, unsigned transaction batches, and per-batch simulation results (when `simulate` is enabled).

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `owner` | string | required | Wallet public key. |
| `burn_nonzero` | bool | `false` | Include non-zero-balance accounts for burn-and-close. |
| `close_empty` | bool | `false` | Include zero-balance accounts for closing. |
| `close_wsol` | bool | `false` | Include native WSOL accounts for unwrapping. |
| `simulate` | bool | `false` | Simulate each batch before returning. |
| `rpc_url` | string | required | Solana RPC endpoint. |

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
  "$MAMBA_API_BASE/wallets/clean/build"
```

### Execute Cleanup

`POST /wallets/clean/execute`

Accepts the same body as `/build`. Signs each batch locally and submits it on-chain. The `owner` wallet must be locally signable through the configured API signer or managed wallet store.

Requires `MAMBA_API_ENABLE_LIVE_SENDS=true`.

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
  "$MAMBA_API_BASE/wallets/clean/execute"
```

## TUI Usage

Open **Cleaner** from the main menu. Select the target network, then toggle the cleanup options you want:

| Key / Control | Effect |
| --- | --- |
| Toggle `burn_nonzero` | Include non-zero-balance accounts for burn-and-close. |
| Toggle `close_empty` | Include zero-balance accounts for closing. |
| Toggle `unwrap_wsol` | Include WSOL accounts for unwrapping. |
| `r` | Refresh the preview. |
| `Enter` on **BUILD** | Produce unsigned transaction batches. |
| `Enter` on **SEND** | Sign and send. Only available when live sends are enabled. |

## Safety

**Burn is opt-in.** Non-zero token balances are never burned unless `burn_nonzero` is explicitly set.

**Build is deterministic.** The `/build` endpoint returns unsigned transactions. Nothing is signed or sent. Batch sizing enforces Solana's wire-size limit before returning results.

**Execute requires live sends.** The `/execute` endpoint signs locally and submits only when `MAMBA_API_ENABLE_LIVE_SENDS=true` is set. The `owner` wallet must be available in the local signer.

**Keys stay local.** Private keys never leave Mamba during any stage of the cleanup flow.
