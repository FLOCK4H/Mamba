// blox.rs
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use solana_program::{pubkey, pubkey::Pubkey};

pub const BLOX_ENDPOINTS: &[&str] = &[
    "http://germany.solana.dex.blxrbdn.com",
    "http://uk.solana.dex.blxrbdn.com",
    "http://amsterdam.solana.dex.blxrbdn.com",
    "http://global.solana.dex.blxrbdn.com",
    "http://tokyo.solana.dex.blxrbdn.com",
    "http://la.solana.dex.blxrbdn.com",
    "http://ny.solana.dex.blxrbdn.com",
];

pub const BLOX_TIP_ACCS: &[Pubkey] = &[
    pubkey!("HWEoBxYs7ssKuudEjzjmpfJVX7Dvi7wescFsVx2L5yoY"),
    pubkey!("95cfoy472fcQHaw4tPGBTKpn6ZQnfEPfBgDQx6gcRmRg"),
    pubkey!("3UQUKjhMKaY2S6bjcQD6yHB7utcZt5bfarRCmctpRtUd"),
    pubkey!("FogxVNs6Mm2w9rnGL1vkARSwJxvLE8mujTv3LK8RnUhF"),
];

pub struct Bloxroute {
    client: Client,
    pub endpoint: String,
    pub auth_header: String,
}

impl Bloxroute {
    pub fn new(endpoint: impl Into<String>, auth_header: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            endpoint: endpoint.into(),
            auth_header: auth_header.into(),
        }
    }

    pub fn multiple_new(endpoints: &[&str], auth_header: impl Into<String>) -> Vec<Self> {
        let tok = auth_header.into();
        endpoints
            .iter()
            .map(|e| Self::new(*e, tok.clone()))
            .collect()
    }

    pub fn get_tip_acc(&self) -> Pubkey {
        let idx = rand::random_range(0..BLOX_TIP_ACCS.len());
        BLOX_TIP_ACCS[idx]
    }

    fn post_json(&self, path: &str, body: Value) -> RequestBuilder {
        let url = format!("{}/api/v2/{}", self.endpoint, path);
        self.client
            .post(url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .json(&body)
    }

    pub fn submit(&self, b64_tx: &str, opts: &SubmitOpts) -> RequestBuilder {
        let mut payload = json!({
            "transaction": { "content": b64_tx },
        });

        if let Some(v) = opts.skip_preflight {
            payload["skipPreFlight"] = json!(v);
        }
        if let Some(v) = opts.front_running_protection {
            payload["frontRunningProtection"] = json!(v);
        }
        if let Some(p) = opts.submit_protection {
            payload["submitProtection"] = json!(p.as_str());
        }
        if let Some(v) = opts.fast_best_effort {
            payload["fastBestEffort"] = json!(v);
        }
        if let Some(v) = opts.use_staked_rpcs {
            payload["useStakedRPCs"] = json!(v);
        }
        if let Some(v) = opts.allow_back_run {
            payload["allowBackRun"] = json!(v);
        }
        if let Some(ref addr) = opts.revenue_address {
            payload["revenueAddress"] = json!(addr);
        }

        self.post_json("submit", payload)
    }

    /// Unused.
    pub fn submit_snipe(
        &self,
        first_b64: &str,
        second_b64: &str,
        use_staked_rpcs: Option<bool>,
    ) -> RequestBuilder {
        let mut payload = json!({
            "entries": [
                { "transaction": { "content": first_b64 } },
                { "transaction": { "content": second_b64 } }
            ]
        });
        if let Some(v) = use_staked_rpcs {
            payload["useStakedRPCs"] = json!(v);
        }
        self.post_json("submit-snipe", payload)
    }

    pub fn send_transaction(&self, b64_tx: &str) -> RequestBuilder {
        self.submit(
            b64_tx,
            &SubmitOpts {
                skip_preflight: Some(true),
                front_running_protection: Some(false),
                submit_protection: None,
                fast_best_effort: None,
                use_staked_rpcs: None,
                allow_back_run: None,
                revenue_address: None,
            },
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SubmitProtection {
    Low,
    Medium,
    High,
}
impl SubmitProtection {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubmitProtection::Low => "SP_LOW",
            SubmitProtection::Medium => "SP_MEDIUM",
            SubmitProtection::High => "SP_HIGH",
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct SubmitOpts {
    pub skip_preflight: Option<bool>,
    pub front_running_protection: Option<bool>,
    pub submit_protection: Option<SubmitProtection>,
    pub fast_best_effort: Option<bool>,
    pub use_staked_rpcs: Option<bool>,
    pub allow_back_run: Option<bool>,
    pub revenue_address: Option<String>,
}
