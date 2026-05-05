use reqwest::{Client, RequestBuilder};
use serde_json::json;
use solana_program::{pubkey, pubkey::Pubkey};

pub const JITO_TIP_ACCS: &[Pubkey] = &[
    pubkey!("DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL"),
    pubkey!("3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT"),
    pubkey!("DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh"),
    pubkey!("ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49"),
    pubkey!("Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY"),
    pubkey!("ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt"),
    pubkey!("HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe"),
    pubkey!("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5"),
];

pub const BLOCK_ENGINES: [&str; 8] = [
    "https://frankfurt.mainnet.block-engine.jito.wtf/api/v1",
    "https://london.mainnet.block-engine.jito.wtf/api/v1",
    "https://amsterdam.mainnet.block-engine.jito.wtf/api/v1",
    "https://ny.mainnet.block-engine.jito.wtf/api/v1",
    "https://tokyo.mainnet.block-engine.jito.wtf/api/v1",
    "https://singapore.mainnet.block-engine.jito.wtf/api/v1",
    "https://slc.mainnet.block-engine.jito.wtf/api/v1",
    "https://mainnet.block-engine.jito.wtf/api/v1",
];

pub fn get_jito_clients() -> Vec<JitoClient> {
    BLOCK_ENGINES.iter().map(|e| JitoClient::new(*e)).collect()
}

pub struct JitoClient {
    client: Client,
    pub endpoint: String,
    pub auth: Option<String>,
}

impl JitoClient {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            endpoint: endpoint.into(),
            auth: None,
        }
    }

    pub fn with_auth(endpoint: impl Into<String>, uuid: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            endpoint: endpoint.into(),
            auth: Some(uuid.into()),
        }
    }

    pub fn multiple_new(endpoints: &[&str]) -> Vec<Self> {
        endpoints.iter().map(|e| Self::new(*e)).collect()
    }

    pub fn get_tip_account(&self) -> Pubkey {
        JITO_TIP_ACCS[rand::random_range(0..JITO_TIP_ACCS.len())]
    }

    pub fn send_transaction(&self, b64_tx: &str, skip_preflight: bool) -> RequestBuilder {
        let body = json!({
          "jsonrpc": "2.0",
          "id": 1,
          "method": "sendTransaction",
          "params": [ b64_tx, { "encoding": "base64", "skipPreflight": skip_preflight } ]
        });
        let url = format!("{}/transactions", self.endpoint);
        self.apply_auth(
            self.client
                .post(url)
                .header("Content-Type", "application/json")
                .json(&body),
        )
    }

    pub fn send_transaction_bundle_only(&self, b64_tx: &str) -> RequestBuilder {
        let body = json!({
          "jsonrpc": "2.0",
          "id": 1,
          "method": "sendTransaction",
          "params": [ b64_tx, { "encoding": "base64" } ]
        });
        let url = format!("{}/transactions?bundleOnly=true", self.endpoint);
        self.apply_auth(
            self.client
                .post(url)
                .header("Content-Type", "application/json")
                .json(&body),
        )
    }

    pub fn send_bundle<S: AsRef<str>>(&self, txs_b64: &[S]) -> RequestBuilder {
        let arr: Vec<&str> = txs_b64.iter().map(|s| s.as_ref()).collect();
        let body = json!({
          "jsonrpc": "2.0",
          "id": 1,
          "method": "sendBundle",
          "params": [ arr, { "encoding": "base64" } ]
        });
        let url = format!("{}/bundles", self.endpoint);
        self.apply_auth(
            self.client
                .post(url)
                .header("Content-Type", "application/json")
                .json(&body),
        )
    }

    pub fn get_bundle_statuses<S: AsRef<str>>(&self, bundle_ids: &[S]) -> RequestBuilder {
        let arr: Vec<&str> = bundle_ids.iter().map(|s| s.as_ref()).collect();
        let body = json!({
          "jsonrpc": "2.0",
          "id": 1,
          "method": "getBundleStatuses",
          "params": [ arr ]
        });
        let url = format!("{}/getBundleStatuses", self.endpoint);
        self.apply_auth(
            self.client
                .post(url)
                .header("Content-Type", "application/json")
                .json(&body),
        )
    }

    pub fn get_inflight_bundle_statuses<S: AsRef<str>>(&self, bundle_ids: &[S]) -> RequestBuilder {
        let arr: Vec<&str> = bundle_ids.iter().map(|s| s.as_ref()).collect();
        let body = json!({
          "jsonrpc": "2.0",
          "id": 1,
          "method": "getInflightBundleStatuses",
          "params": [ arr ]
        });
        let url = format!("{}/getInflightBundleStatuses", self.endpoint);
        self.apply_auth(
            self.client
                .post(url)
                .header("Content-Type", "application/json")
                .json(&body),
        )
    }

    pub fn get_tip_accounts(&self) -> RequestBuilder {
        let body = json!({
          "jsonrpc": "2.0",
          "id": 1,
          "method": "getTipAccounts",
          "params": []
        });
        let url = format!("{}/getTipAccounts", self.endpoint);
        self.apply_auth(
            self.client
                .post(url)
                .header("Content-Type", "application/json")
                .json(&body),
        )
    }

    fn apply_auth(&self, rb: RequestBuilder) -> RequestBuilder {
        if let Some(ref uuid) = self.auth {
            rb.header("x-jito-auth", uuid)
        } else {
            rb
        }
    }
}
