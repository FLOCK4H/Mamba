use reqwest::{Client, RequestBuilder};
use serde_json::json;
use solana_program::{pubkey, pubkey::Pubkey};

pub const NB_TIP_ACCS: &[Pubkey] = &[
    pubkey!("NextbLoCkVtMGcV47JzewQdvBpLqT9TxQFozQkN98pE"),
    pubkey!("NexTbLoCkWykbLuB1NkjXgFWkX9oAtcoagQegygXXA2"),
    pubkey!("NeXTBLoCKs9F1y5PJS9CKrFNNLU1keHW71rfh7KgA1X"),
    pubkey!("NexTBLockJYZ7QD7p2byrUa6df8ndV2WSd8GkbWqfbb"),
    pubkey!("neXtBLock1LeC67jYd1QdAa32kbVeubsfPNTJC1V5At"),
    pubkey!("nEXTBLockYgngeRmRrjDV31mGSekVPqZoMGhQEZtPVG"),
    pubkey!("NEXTbLoCkB51HpLBLojQfpyVAMorm3zzKg7w9NFdqid"),
    pubkey!("nextBLoCkPMgmG8ZgJtABeScP35qLa2AMCNKntAP7Xc"),
];

pub const NB_ENDPOINTS: [&str; 5] = [
    "http://fra.nextblock.io",
    "http://london.nextblock.io",
    "http://ny.nextblock.io",
    "http://slc.nextblock.io",
    "http://tokyo.nextblock.io",
];

pub struct NextBlock {
    client: Client,
    pub endpoint: String,
    pub auth_token: String,
}

impl NextBlock {
    pub fn new(endpoint: impl Into<String>, auth_token: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            endpoint: endpoint.into(),
            auth_token: auth_token.into(),
        }
    }

    pub fn multiple_new(endpoints: &[&str], auth_token: impl Into<String>) -> Vec<Self> {
        let tok = auth_token.into();
        endpoints
            .iter()
            .map(|e| Self::new(*e, tok.clone()))
            .collect()
    }

    pub fn get_tip_account(&self) -> Pubkey {
        NB_TIP_ACCS[rand::random_range(0..NB_TIP_ACCS.len())]
    }

    /// bild the request; call `send` on the returned builder to get a futur
    pub fn send_transaction(&self, b64_tx: &str) -> RequestBuilder {
        let path = format!("{}/api/v2/submit", self.endpoint);
        self.client
            .post(path)
            .header("Authorization", &self.auth_token)
            .header("Content-Type", "application/json")
            .json(&json!({
                "transaction": { "content": b64_tx },
                "frontRunningProtection": false
            }))
    }
}
