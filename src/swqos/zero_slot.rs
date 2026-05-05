use reqwest::{Client, RequestBuilder};
use serde_json::json;
use solana_program::{pubkey, pubkey::Pubkey};

pub const ZERO_SLOT_ENDPOINTS: [&str; 5] = [
    "http://de1.0slot.trade",
    "http://ny.0slot.trade",
    "http://ams.0slot.trade",
    "http://jp.0slot.trade",
    "http://la.0slot.trade",
];

pub const ZERO_SLOT_TIP_ACCS: &[Pubkey] = &[
    pubkey!("Eb2KpSC8uMt9GmzyAEm5Eb1AAAgTjRaXWFjKyFXHZxF3"),
    pubkey!("FCjUJZ1qozm1e8romw216qyfQMaaWKxWsuySnumVCCNe"),
    pubkey!("ENxTEjSQ1YabmUpXAdCgevnHQ9MHdLv8tzFiuiYJqa13"),
    pubkey!("6rYLG55Q9RpsPGvqdPNJs4z5WTxJVatMB8zV3WJhs5EK"),
    pubkey!("Cix2bHfqPcKcM233mzxbLk14kSggUUiz2A87fJtGivXr"),
];

pub struct ZeroSlot {
    client: Client,
    pub endpoint: String,
    pub auth_token: String,
}

impl ZeroSlot {
    pub fn new(endpoint: impl Into<String>, auth_token: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            endpoint: endpoint.into(),
            auth_token: auth_token.into(),
        }
    }

    pub fn get_tip_account(&self) -> Pubkey {
        ZERO_SLOT_TIP_ACCS[rand::random_range(0..ZERO_SLOT_TIP_ACCS.len())]
    }

    pub fn multiple_new(endpoints: &[&str], auth_token: impl Into<String>) -> Vec<Self> {
        let tok = auth_token.into();
        endpoints
            .iter()
            .map(|e| Self::new(*e, tok.clone()))
            .collect()
    }

    pub async fn warmup(&self) -> anyhow::Result<()> {
        let path = format!("{}?api-key={}", self.endpoint, self.auth_token);
        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getHealth",
        });
        let req = self.client.post(path).json(&request_body).send().await?;
        req.error_for_status()?.json::<serde_json::Value>().await?;
        Ok(())
    }

    pub fn send_transaction(&self, b64_tx: &str) -> RequestBuilder {
        let path = format!("{}?api-key={}", self.endpoint, self.auth_token);
        let request_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [
                b64_tx,
                {
                    "encoding": "base64",
                    "skipPreflight": true,
                }
            ]
        });

        self.client.post(path).json(&request_body)
    }
}
