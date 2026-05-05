use crate::warn;
use reqwest::{Client, RequestBuilder};
use serde_json::json;
use solana_program::{pubkey, pubkey::Pubkey};

pub const TEMPORAL_HTTP_ENDPOINTS: [&str; 6] = [
    "http://fra2.nozomi.temporal.xyz/?c=",
    "http://ams1.nozomi.temporal.xyz/?c=",
    "http://pit1.nozomi.temporal.xyz/?c=",
    "http://tyo1.nozomi.temporal.xyz/?c=",
    "http://sgp1.nozomi.temporal.xyz/?c=",
    "http://ewr1.nozomi.temporal.xyz/?c=",
];

pub const TEMPORAL_TIP_ACCS: &[Pubkey] = &[
    pubkey!("TEMPaMeCRFAS9EKF53Jd6KpHxgL47uWLcpFArU1Fanq"),
    pubkey!("noz3jAjPiHuBPqiSPkkugaJDkJscPuRhYnSpbi8UvC4"),
    pubkey!("noz3str9KXfpKknefHji8L1mPgimezaiUyCHYMDv1GE"),
    pubkey!("noz6uoYCDijhu1V7cutCpwxNiSovEwLdRHPwmgCGDNo"),
    pubkey!("noz9EPNcT7WH6Sou3sr3GGjHQYVkN3DNirpbvDkv9YJ"),
    pubkey!("nozc5yT15LazbLTFVZzoNZCwjh3yUtW86LoUyqsBu4L"),
    pubkey!("nozFrhfnNGoyqwVuwPAW4aaGqempx4PU6g6D9CJMv7Z"),
    pubkey!("nozievPk7HyK1Rqy1MPJwVQ7qQg2QoJGyP71oeDwbsu"),
    pubkey!("noznbgwYnBLDHu8wcQVCEw6kDrXkPdKkydGJGNXGvL7"),
    pubkey!("nozNVWs5N8mgzuD3qigrCG2UoKxZttxzZ85pvAQVrbP"),
    pubkey!("nozpEGbwx4BcGp6pvEdAh1JoC2CQGZdU6HbNP1v2p6P"),
    pubkey!("nozrhjhkCr3zXT3BiT4WCodYCUFeQvcdUkM7MqhKqge"),
    pubkey!("nozrwQtWhEdrA6W8dkbt9gnUaMs52PdAv5byipnadq3"),
    pubkey!("nozUacTVWub3cL4mJmGCYjKZTnE9RbdY5AP46iQgbPJ"),
    pubkey!("nozWCyTPppJjRuw2fpzDhhWbW355fzosWSzrrMYB1Qk"),
    pubkey!("nozWNju6dY353eMkMqURqwQEoM3SFgEKC6psLCSfUne"),
    pubkey!("nozxNBgWohjR75vdspfxR5H9ceC7XXH99xpxhVGt3Bb"),
];

const PING_URL: &str = "http://nozomi.temporal.xyz/ping";

/// e.g. "https://ewr1.secure.nozomi.temporal.xyz/?c="
pub struct TemporalSender {
    client: Client,
    pub endpoint_prefix: String,
    pub api_key: String,
}

impl TemporalSender {
    pub fn new(endpoint_prefix: impl Into<String>, api_key: impl Into<String>) -> Self {
        // server closes idle >65s, so we ping every 60s anyway
        let client = Client::builder()
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .pool_idle_timeout(Some(std::time::Duration::from_secs(70)))
            .pool_max_idle_per_host(32)
            .build()
            .unwrap_or_else(|error| {
                warn!(
                    "temporal reqwest client build failed, falling back to default client: {}",
                    error
                );
                Client::new()
            });

        Self {
            client,
            endpoint_prefix: endpoint_prefix.into(),
            api_key: api_key.into(),
        }
    }

    pub fn multiple_new(endpoints: &[&str], api_key: impl Into<String>) -> Vec<Self> {
        let key = api_key.into();
        endpoints
            .iter()
            .map(|e| Self::new(*e, key.clone()))
            .collect()
    }

    pub fn get_tip_account(&self) -> Pubkey {
        TEMPORAL_TIP_ACCS[rand::random_range(0..TEMPORAL_TIP_ACCS.len())]
    }

    pub fn send_transaction(&self, b64_tx: &str) -> RequestBuilder {
        let url = format!("{}{}", self.endpoint_prefix, self.api_key);
        self.client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "sendTransaction",
                "params": [
                    b64_tx,
                    { "encoding": "base64" }
                ]
            }))
    }

    pub fn ping(&self) -> RequestBuilder {
        self.client.get(PING_URL)
    }

    #[allow(dead_code)]
    pub fn spawn_keepalive(&self) -> tokio::task::JoinHandle<()> {
        let client = self.client.clone();
        tokio::spawn(async move {
            loop {
                let _ = client.get(PING_URL).send().await;
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        })
    }
}
