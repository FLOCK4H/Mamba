# Token Creation Runbook

This runbook covers the full lifecycle of launching a token through Mamba: building, signing, simulating, and optionally sending.

For the terminal app overview, see [CLI and TUI](MAMBA_CLI.md). For the HTTP endpoints, see [API Reference](MAMBA_API.md).

## Safety

- `/create/build` returns an unsigned transaction. It never signs.
- `/create/execute` signs and submits only when `MAMBA_API_ENABLE_LIVE_SENDS=true`.
- Headless `mamba --create` is build-only by default. It broadcasts only when `--create-send` is passed.
- In the TUI Create screen, set `execution=send` and press Enter to broadcast (devnet by default; mainnet requires explicit unlock).
- Prefer devnet for rehearsal. Mainnet creation is permanent and costs SOL.

## Prerequisites

- `mamba_api` running locally (authenticated)
- `mamba` available (`cargo run --bin mamba -- ...`)
- A locally controlled payer keypair (recommended: `--create-payer-keypair <path>`)
- RPC HTTP URL for the target cluster (defaults to devnet if omitted)

## 1. Start the backend

```bash
export MAMBA_API_KEY='mamba_test_key'
export MAMBA_API_HTTP_URLS='https://api.devnet.solana.com'
export MAMBA_API_WS_URLS='wss://api.devnet.solana.com'
cargo run --bin mamba_api
```

## 2. Discover available methods

List supported Create methods:

```bash
cargo run --bin mamba -- --create-list-methods
```

For Raydium Launchpad, you also need config pubkeys:

```bash
cargo run --bin mamba -- --raydium-list-global-configs
cargo run --bin mamba -- --raydium-list-platform-configs
```

Supported methods: `pump_fun`, `spl_token`, `spl_token_2022`, `raydium_launchpad`.

API-managed signing is available through `/create/execute` or `mamba_mcp`. In execute mode, omitting `mint` lets Mamba generate the mint signer internally and return only the public key in the response.

## 3. Build and sign (no send)

Use a file path for the payer keypair:

```bash
cargo run --bin mamba -- \
  --http-url 'https://api.devnet.solana.com' \
  --create \
  --create-method spl_token \
  --create-name 'Example Token' \
  --create-symbol 'EXMPL' \
  --create-uri 'https://example.com/token.json' \
  --create-decimals 6 \
  --create-payer-keypair ~/.config/solana/id.json
```

### CLI flags

| Flag | Required | Description |
|------|----------|-------------|
| `--create` | yes | Enable create mode |
| `--create-method` | yes | One of: `pump_fun`, `spl_token`, `spl_token_2022`, `raydium_launchpad` |
| `--create-name` | yes | Token name |
| `--create-symbol` | yes | Token symbol |
| `--create-uri` | when no `--create-image` | Metadata URI (cannot combine with `--create-image`) |
| `--create-decimals` | yes | Token decimals |
| `--create-payer-keypair` | recommended | Path to payer keypair file |
| `--create-image` | no | Upload image + metadata to IPFS, derive URI automatically |
| `--create-description` | no | Used only with `--create-image` |
| `--create-twitter` | no | Used only with `--create-image` |
| `--create-telegram` | no | Used only with `--create-image` |
| `--create-website` | no | Used only with `--create-image` |
| `--create-send` | no | Broadcast the signed transaction |
| `--create-no-verify` | no | Skip post-send on-chain verification |
| `--create-no-confirm` | no | Skip confirmation wait (combine with `--create-no-verify`) |
| `--http-url` | no | Target cluster RPC (defaults to devnet) |

### IPFS upload

`--create-image` uploads the image and metadata JSON to IPFS via `https://pump.fun/api/ipfs` (no API key needed) and uses the returned `metadataUri` as the on-chain URI.

Override the upload endpoint with `--create-ipfs-upload-url` or `MAMBA_IPFS_UPLOAD_URL`.

```bash
cargo run --bin mamba -- \
  --create \
  --create-method spl_token \
  --create-name 'Example Token' \
  --create-symbol 'EXMPL' \
  --create-decimals 6 \
  --create-image ./token.png \
  --create-description 'memecoin' \
  --create-payer-keypair ~/.config/solana/id.json
```

### Bundle output

Outputs are written to a single bundle file (chmod `0600`):

```
artifacts/create/<timestamp>_<method>_<mint>.json
```

The bundle contains (when available):

| Field | Description |
|-------|-------------|
| Build request/response | The exact payloads sent to and received from the API |
| `signed_tx_b64` | The signed transaction (treat as sensitive) |
| Simulation result | Present when an HTTP RPC URL was provided |
| Live-send/confirm/verify results | Present when `--create-send` was used |
| IPFS upload response | Present when `--create-image` was used |

## 4. Send, confirm, and verify

This broadcasts to the target cluster. Recommended: use `--create-send`, which sends the signed transaction and then confirms and verifies on-chain state.

```bash
cargo run --bin mamba -- \
  --http-url 'https://api.devnet.solana.com' \
  --create \
  --create-method spl_token \
  --create-name 'Example Token' \
  --create-symbol 'EXMPL' \
  --create-uri 'https://example.com/token.json' \
  --create-decimals 6 \
  --create-payer-keypair ~/.config/solana/id.json \
  --create-send
```

On success, `mamba` prints the signature and a Solscan link.

### Manual send (alternative)

Extract `signed_tx_b64` from the bundle and use RPC directly:

```bash
BUNDLE='artifacts/create/<timestamp>_<method>_<mint>.json'
TX_B64="$(jq -r .signed_tx_b64 "$BUNDLE")"
RPC_URL='https://api.devnet.solana.com'

# simulate with signature verification (no send)
curl -sS -H 'content-type: application/json' -X POST "$RPC_URL" -d "{
  \"jsonrpc\":\"2.0\",
  \"id\":1,
  \"method\":\"simulateTransaction\",
  \"params\":[
    \"$TX_B64\",
    {\"encoding\":\"base64\",\"sigVerify\":true,\"commitment\":\"processed\"}
  ]
}"

# send
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
