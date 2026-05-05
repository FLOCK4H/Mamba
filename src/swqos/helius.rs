use reqwest::{Client, RequestBuilder};
use serde_json::json;
use solana_program::{pubkey, pubkey::Pubkey};

pub const HELIUS_SENDER_ENDPOINTS: [&str; 7] = [
    "http://fra-sender.helius-rpc.com",
    "http://ams-sender.helius-rpc.com",
    "http://lon-sender.helius-rpc.com",
    "http://slc-sender.helius-rpc.com",
    "http://ewr-sender.helius-rpc.com",
    "http://sg-sender.helius-rpc.com",
    "http://tyo-sender.helius-rpc.com",
];

pub const HELIUS_TIP_ACCS: &[Pubkey] = &[
    pubkey!("4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE"),
    pubkey!("D2L6yPZ2FmmmTKPgzaMKdhu6EWZcTpLy1Vhx8uvZe7NZ"),
    pubkey!("9bnz4RShgq1hAnLnZbP8kbgBg1kEmcJBYQq3gQbmnSta"),
    pubkey!("5VY91ws6B2hMmBFRsXkoAAdsPHBJwRfBht4DXox3xkwn"),
    pubkey!("2nyhqdwKcJZR2vcqCyrYsaPVdAnFoJjiksCXJ7hfEYgD"),
    pubkey!("2q5pghRs6arqVjRvT5gfgWfWcHWmw1ZuCzphgd5KfWGJ"),
    pubkey!("wyvPkWjVZz1M8fHQnMMCDTQDbkManefNNhweYk5WkcF"),
    pubkey!("3KCKozbAaF75qEU33jtzozcJ29yJuaLJTy2jFdzUY8bT"),
    pubkey!("4vieeGHPYPG2MmyPRcYjdiDmmhN3ww7hsFNap8pVN3Ey"),
    pubkey!("4TQLFNWK8AovT1gFvda5jfw2oJeRMKEmw7aH6MGBJ3or"),
];

pub struct HeliusSender {
    client: Client,
    pub endpoint: String,
}

impl HeliusSender {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            endpoint: endpoint.into(),
        }
    }

    pub fn multiple_new(endpoints: &[&str]) -> Vec<Self> {
        endpoints.iter().map(|e| Self::new(*e)).collect()
    }

    pub fn get_tip_account(&self) -> Pubkey {
        HELIUS_TIP_ACCS[rand::random_range(0..HELIUS_TIP_ACCS.len())]
    }

    /// If `swqos_only` is true, routes via SWQOS infra with lower min tip (0.0005 SOL).
    pub fn send_transaction(&self, b64_tx: &str, swqos_only: bool) -> RequestBuilder {
        let mut url = format!("{}/fast", self.endpoint);
        if swqos_only {
            url.push_str("?swqos_only=true");
        }
        self.client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "sendTransaction",
                "params": [
                    b64_tx,
                    {
                        "encoding": "base64",
                        "skipPreflight": true,
                        "maxRetries": 0
                    }
                ]
            }))
    }

    pub fn ping(&self) -> RequestBuilder {
        let url = format!("{}/ping", self.endpoint);
        self.client.get(url)
    }
}
