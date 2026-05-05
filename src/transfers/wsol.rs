use {
    crate::{
        core::sol::{SYSTEM_PROGRAM, SolHook},
        log,
        utils::writing::cc,
        warn,
    },
    moka::sync::Cache,
    serde::{Deserialize, Serialize},
    solana_commitment_config::CommitmentConfig,
    solana_rpc_client_types::{config::RpcTransactionLogsFilter, response::RpcLogsResponse},
    solana_signature::Signature,
    solana_transaction_status::{
        EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiInstruction, UiMessage,
        UiParsedInstruction,
    },
    std::{str::FromStr, sync::Arc},
    tokio::sync::mpsc::Receiver,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemTransfer {
    pub source: String,
    pub destination: String,
    pub lamports: u64,
}

#[derive(Clone)]
pub struct Watchtower {
    pub sol_hook: SolHook,
}

impl Watchtower {
    pub fn new(sol_hook: SolHook) -> Arc<Self> {
        Arc::new(Self {
            sol_hook: sol_hook.clone(),
        })
    }

    pub async fn find_system_transfers(
        &self,
        tx: &EncodedConfirmedTransactionWithStatusMeta,
    ) -> anyhow::Result<Vec<SystemTransfer>> {
        let mut out = Vec::new();

        let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
            return Ok(out);
        };
        let UiMessage::Parsed(msg) = &ui_tx.message else {
            return Ok(out);
        };

        for ix in &msg.instructions {
            let UiInstruction::Parsed(UiParsedInstruction::Parsed(pi)) = ix else {
                continue;
            };

            let parsed = &pi.parsed;
            let ty = parsed.get("type").and_then(|v| v.as_str());
            if !matches!(
                ty,
                Some("transfer" | "transferWithSeed" | "transferChecked")
            ) {
                continue;
            }

            if pi.program.as_str() != "system" {
                continue;
            }

            let info = &parsed["info"];
            if let (Some(src), Some(dst), Some(lamports)) = (
                info.get("source").and_then(|v| v.as_str()),
                info.get("destination").and_then(|v| v.as_str()),
                info.get("lamports").and_then(|v| v.as_u64()),
            ) {
                out.push(SystemTransfer {
                    source: src.to_string(),
                    destination: dst.to_string(),
                    lamports,
                });
            }
        }
        Ok(out)
    }

    async fn proc_st_task(
        self: Arc<Self>,
        mut stream: Receiver<RpcLogsResponse>,
        channel: tokio::sync::mpsc::Sender<Vec<SystemTransfer>>,
        cache: Cache<Signature, Vec<SystemTransfer>>,
    ) {
        while let Some(msg) = stream.recv().await {
            let Ok(sig) = Signature::from_str(&msg.signature) else {
                continue;
            };
            match self.sol_hook.get_transaction_parsed(&sig).await {
                Ok(tx) => {
                    if cache.contains_key(&sig) {
                        continue;
                    }
                    cache.insert(sig, Vec::new());
                    let transfers = self.find_system_transfers(&tx).await.unwrap_or_default();
                    if !transfers.is_empty() {
                        log!(cc::LIGHT_MAGENTA, "Found system transfers: {:?}", transfers);
                        log!(cc::LIGHT_MAGENTA, "Sig: {}", sig);
                        let _ = channel.send(transfers).await;
                    }
                }
                Err(e) => warn!("get_transaction_parsed error: {e} for {}", sig),
            }
        }
    }

    /// Subscribe to the system transfer logs and return the handles and the channel to receive the transfers
    /// # Arguments
    /// * `ws_url` - The URL of the websocket server
    /// # Returns
    /// * `(tokio::task::JoinHandle<()>, Receiver<Vec<SystemTransfer>>)` - The handle and the channel to receive the transfers
    pub async fn sub(
        self: Arc<Self>,
        ws_url: &str,
    ) -> anyhow::Result<(tokio::task::JoinHandle<()>, Receiver<Vec<SystemTransfer>>)> {
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<SystemTransfer>>(1024);
        let cache: Cache<Signature, _> = Cache::new(4096);

        let (rx_sysprog, _h7) = self
            .sol_hook
            .subscribe_logs_channel(
                ws_url,
                RpcTransactionLogsFilter::Mentions(vec![SYSTEM_PROGRAM.to_string()]),
                CommitmentConfig::confirmed(),
            )
            .await?;

        let me: Arc<Watchtower> = Arc::clone(&self.clone());
        let tx = tx.clone();
        let cache = cache.clone();
        let handle = tokio::spawn(async move { me.proc_st_task(rx_sysprog, tx, cache).await });

        Ok((handle, rx))
    }
}
