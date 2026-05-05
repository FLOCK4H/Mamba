# Mamba Create (Token Creation) Runbook

This page is the focused Create runbook for launch flows. For the overall terminal app, see [CLI and TUI](MAMBA_CLI.md). For the authenticated builder surface, see [Local API](MAMBA_API.md).

This runbook covers token creation ("Create") flows:

- build an **unsigned** Create transaction via `mamba_api`,
- optionally let `mamba_api` sign and submit it through `/create/execute`,
- sign locally via `mamba`,
- optionally simulate (recommended),
- optionally **send** (manual-only) via `mamba --create-send` or manual RPC `sendTransaction`.

## Safety notes

- `mamba_api` Create flows are **build-first**. `/create/build` never signs, while `/create/execute` can sign and submit only when live sends are intentionally enabled.
- Headless `mamba --create` is **build-only by default**; it will only broadcast when `--create-send` is passed.
- In the `mamba` TUI Create screen, set `execution=send` and press `Enter` to broadcast (devnet by default; mainnet send remains locked unless explicitly enabled).
- Treat any sending steps as **live execution** and only run them when you intend to create a token on-chain.
- Prefer **devnet** for rehearsal; mainnet creation is permanent and costs SOL for rent + fees.

## Prerequisites

- `mamba_api` running locally (authenticated).
- `mamba` available (`cargo run --bin mamba -- ...`).
- A payer keypair you control (recommended: `--create-payer-keypair <path>`).
- RPC HTTP URL for the target cluster (send/confirm/verify). If omitted, Mamba defaults to devnet.

## 1) Start `mamba_api` (local, authenticated)

Example (devnet; safe until you call an execute route):

```bash
export MAMBA_API_KEY='mamba_test_key'
export MAMBA_API_HTTP_URLS='https://api.devnet.solana.com'
export MAMBA_API_WS_URLS='wss://api.devnet.solana.com'
cargo run --bin mamba_api
```

Note: if `MAMBA_API_HTTP_URLS` / `MAMBA_API_WS_URLS` are not set, `mamba_api` falls back to devnet endpoints.

## 2) Discover Create methods and Raydium configs

List supported Create methods:

```bash
cargo run --bin mamba -- --create-list-methods
```

If you plan to use `raydium_launchpad`, you must provide config pubkeys:

```bash
cargo run --bin mamba -- --raydium-list-global-configs
cargo run --bin mamba -- --raydium-list-platform-configs
```

If you want API-managed signing instead of CLI signing, you can also use `/create/execute` directly or through `mamba_mcp`. In execute mode, omitting `mint` is allowed and Mamba will generate the mint signer internally, returning only the public key in the response.

## 3) Build + sign a Create transaction (no send)

Use a payer keypair you control (file path is recommended over env vars).

Example (`spl_token`), with signed simulation enabled:

```bash
cargo run --bin mamba -- \\
  --http-url 'https://api.devnet.solana.com' \\
  --create \\
  --create-method spl_token \\
  --create-name 'Example Token' \\
  --create-symbol 'EXMPL' \\
  --create-uri 'https://example.com/token.json' \\
  --create-decimals 6 \\
  --create-payer-keypair ~/.config/solana/id.json
```

Notes:

- `--create-uri` is required unless `--create-image` is provided.
- `--create-uri` cannot be combined with `--create-image` (the `uri` is derived from the upload).
- `--create-description/--create-twitter/--create-telegram/--create-website` are only used when `--create-image` is set (metadata upload).

### Optional: free IPFS upload (image + metadata)

If you provide `--create-image`, Mamba uploads your image and metadata JSON to IPFS via `https://pump.fun/api/ipfs` (no API key required) and uses the returned `metadataUri` as the on-chain `uri`.

Override the upload endpoint via `--create-ipfs-upload-url` or `MAMBA_IPFS_UPLOAD_URL`.

Example (build + sign, no send):

```bash
cargo run --bin mamba -- \\
  --create \\
  --create-method spl_token \\
  --create-name 'Example Token' \\
  --create-symbol 'EXMPL' \\
  --create-decimals 6 \\
  --create-image ./token.png \\
  --create-description 'memecoin' \\
  --create-payer-keypair ~/.config/solana/id.json
```

Outputs are written to a **single bundle file** (chmod `0600`):

- `artifacts/create/<timestamp>_<method>_<mint>.json`

The bundle contains (when available):

- the exact build request/response payloads,
- `signed_tx_b64` (the signed transaction; treat as sensitive),
- simulation result (if an HTTP RPC URL was provided),
- live-send/confirm/verify results (if `--create-send` was used),
- IPFS upload response (if `--create-image` was used).

## 4) Optional live send + confirm + verify (manual-only)

Only do this when you are ready to create the token on the target cluster.

Recommended: `--create-send` broadcasts the signed transaction, then (by default) confirms and verifies on-chain state.

```bash
cargo run --bin mamba -- \\
  --http-url 'https://api.devnet.solana.com' \\
  --create \\
  --create-method spl_token \\
  --create-name 'Example Token' \\
  --create-symbol 'EXMPL' \\
  --create-uri 'https://example.com/token.json' \\
  --create-decimals 6 \\
  --create-payer-keypair ~/.config/solana/id.json \\
  --create-send
```

On success, `mamba` prints:

- `signature=<sig>`
- `solscan_tx=<url>`

Optional knobs:

- `--create-no-verify` skip post-send verification
- `--create-no-confirm --create-no-verify` send without waiting for confirmation

Alternative (manual): extract `signed_tx_b64` from the bundle and use RPC directly:

```bash
BUNDLE='artifacts/create/<timestamp>_<method>_<mint>.json'
TX_B64="$(jq -r .signed_tx_b64 "$BUNDLE")"
RPC_URL='https://api.devnet.solana.com'

# (Recommended) simulate with signature verification (no send)
curl -sS -H 'content-type: application/json' -X POST "$RPC_URL" -d "{
  \"jsonrpc\":\"2.0\",
  \"id\":1,
  \"method\":\"simulateTransaction\",
  \"params\":[
    \"$TX_B64\",
    {\"encoding\":\"base64\",\"sigVerify\":true,\"commitment\":\"processed\"}
  ]
}"

# (Live) send
curl -sS -H 'content-type: application/json' -X POST "$RPC_URL" -d "{
  \"jsonrpc\":\"2.0\",
  \"id\":1,
  \"method\":\"sendTransaction\",
  \"params\":[
    \"$TX_B64\",
    {\"encoding\":\"base64\",\"skipPreflight\":false,\"preflightCommitment\":\"processed\"}
  ]
}"
```
