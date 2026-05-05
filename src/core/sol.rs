#![allow(deprecated)]
use {
    crate::compute_budget::compute_budget::{ix_set_compute_unit_limit, ix_set_compute_unit_price},
    crate::core::cluster::{DEFAULT_DEVNET_HTTP_URL, DEFAULT_MAINNET_HTTP_URL, SolanaCluster},
    crate::swqos::{
        SWQoSettings, SwqosProvider,
        blox::{BLOX_ENDPOINTS, Bloxroute, SubmitOpts, SubmitProtection},
        helius::{HELIUS_SENDER_ENDPOINTS, HeliusSender},
        jito::{BLOCK_ENGINES as JITO_BLOCK_ENGINES, JitoClient},
        nextblock::{NB_ENDPOINTS, NextBlock},
        temporal::{TEMPORAL_HTTP_ENDPOINTS, TemporalSender},
        tip_account_for_provider,
        zero_slot::ZERO_SLOT_ENDPOINTS,
        zero_slot::ZeroSlot,
    },
    crate::utils::writing::cc,
    crate::{log, warn},
    anyhow::Context,
    base64::{Engine as _, engine::general_purpose::STANDARD as B64},
    mpl_token_metadata::accounts::Metadata as MplMetadata,
    serde::{Deserialize, Serialize},
    solana_account::Account,
    solana_account_decoder_client_types::{UiAccount, UiAccountEncoding},
    solana_address::Address,
    solana_client::{
        nonblocking::rpc_client::RpcClient,
        rpc_config::{RpcProgramAccountsConfig, RpcSendTransactionConfig, RpcTransactionConfig},
        rpc_response::SlotInfo,
    },
    solana_commitment_config::CommitmentConfig,
    solana_keypair::Keypair,
    solana_message::{VersionedMessage, v0::Message as V0Message},
    solana_nonce::{state::State as NonceState, versions::Versions as NonceVersions},
    solana_program::{hash::Hash, instruction::Instruction, program_pack::Pack, pubkey::Pubkey},
    solana_pubsub_client::nonblocking::pubsub_client::PubsubClient,
    solana_rpc_client_types::{
        config::{RpcAccountInfoConfig, RpcTransactionLogsConfig, RpcTransactionLogsFilter},
        response::{Response as RpcResponse, RpcLogsResponse, RpcSimulateTransactionResult},
    },
    solana_signature::Signature,
    solana_signer::Signer,
    solana_system_interface::instruction as system_instruction_if,
    solana_transaction::versioned::VersionedTransaction,
    solana_transaction_status::{EncodedConfirmedTransactionWithStatusMeta, UiTransactionEncoding},
    spl_token::state::Account as SplTokenAccount,
    spl_token::state::Mint as SplMint,
    spl_token_2022::state::Account as SplToken2022Account,
    spl_token_2022::state::Mint as SplMint2022,
    std::{
        collections::{HashMap, HashSet},
        future::Future,
        str::FromStr,
        sync::{Arc, Mutex, OnceLock},
        time::{Duration, Instant},
    },
    tokio::sync::mpsc::{self, Receiver},
    tokio_stream::StreamExt,
};

#[cfg(not(windows))]
use std::pin::Pin;
#[cfg(not(windows))]
use tokio_stream::Stream;
#[cfg(not(windows))]
use tonic::Status;
#[cfg(not(windows))]
use yellowstone_grpc_client::{
    GeyserGrpcBuilder, GeyserGrpcClient, Interceptor, InterceptorXToken,
};
#[cfg(not(windows))]
use yellowstone_grpc_proto::geyser::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots,
    SubscribeRequestFilterTransactions, SubscribeUpdate,
};

#[cfg(not(windows))]
pub type YellowstoneClient = GeyserGrpcClient<InterceptorXToken>;

pub fn pubkey_to_address(pk: &Pubkey) -> Address {
    Address::from(pk.to_bytes())
}

pub const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
pub const ATA_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const WSOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
pub const USDC_MINT: Pubkey =
    Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
pub const METADATA_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
pub const SYSTEM_PROGRAM: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
const RPC_ENDPOINT_COOLDOWN_ON_RETRYABLE_ERROR: Duration = Duration::from_secs(60);

fn trim_nul(s: &str) -> &str {
    s.trim_matches('\0')
}

#[cfg(not(windows))]
pub type SubStream = Pin<Box<dyn Stream<Item = Result<SubscribeUpdate, Status>> + Send>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityFeeLevel {
    Low,
    Medium,
    High,
    Turbo,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityFeeOverride {
    Level(PriorityFeeLevel),
    ExactMicroLamports(u64),
}

pub const DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS: u32 = 300_000;
const SEND_CONFIRM_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SEND_FAILURE_DETAILS_POLL_EVERY: u32 = 4;
const DEFAULT_SEND_CONFIRM_TIMEOUT_SECS: u64 = 60;
const SEND_CONFIRM_TIMEOUT_ENV: &str = "MAMBA_SWAP_CONFIRM_TIMEOUT_SECS";

impl PriorityFeeLevel {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "turbo" => Some(Self::Turbo),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    pub fn from_env() -> Self {
        let raw = std::env::var("FEE_LEVEL").unwrap_or_else(|_| "medium".to_string());
        Self::parse(&raw).unwrap_or(Self::Medium)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenInfo {
    pub mint: Pubkey,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub creator: Option<Pubkey>,
    pub authority: Pubkey,
}

#[derive(Clone)]
pub struct SolHook {
    pub rpc_client: Arc<RpcClient>,
    pub cluster: SolanaCluster,
    read_rpc_clients: Arc<Vec<Arc<RpcClient>>>,
}

impl SolHook {
    fn rpc_endpoint_cooldowns() -> &'static Mutex<HashMap<String, Instant>> {
        static COOLDOWNS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
        COOLDOWNS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn max_priority_fee_micro_lamports_from_env(compute_units: u32) -> Option<u64> {
        let max_fee_sol = std::env::var("MAX_FEE")
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| *value > 0.0)?;
        let compute_units = u128::from(compute_units.max(1));
        let max_fee_lamports = (max_fee_sol * 1_000_000_000.0).floor();
        if max_fee_lamports <= 0.0 {
            return Some(0);
        }

        let max_fee_lamports = max_fee_lamports as u128;
        Some(
            max_fee_lamports
                .saturating_mul(1_000_000)
                .checked_div(compute_units)
                .unwrap_or(0) as u64,
        )
    }

    pub fn priority_fee_micro_lamports_from_sol_amount(
        priority_fee_sol: f64,
        compute_units: u32,
    ) -> anyhow::Result<u64> {
        anyhow::ensure!(
            priority_fee_sol.is_finite() && priority_fee_sol > 0.0,
            "priority_fee_sol must be a finite number > 0"
        );
        let compute_units = u128::from(compute_units.max(1));
        let lamports = (priority_fee_sol * 1_000_000_000.0).floor();
        anyhow::ensure!(
            lamports.is_finite() && lamports > 0.0,
            "priority_fee_sol is too small to convert into lamports"
        );
        let fee = (lamports as u128)
            .saturating_mul(1_000_000)
            .checked_div(compute_units)
            .unwrap_or(0);
        anyhow::ensure!(
            fee > 0 && fee <= u128::from(u64::MAX),
            "priority_fee_sol is out of range for compute-unit pricing"
        );
        Ok(fee as u64)
    }

    pub fn custom_priority_fee_micro_lamports_from_sol_amount(
        priority_fee_sol: f64,
        compute_units: u32,
    ) -> anyhow::Result<u64> {
        let fee =
            Self::priority_fee_micro_lamports_from_sol_amount(priority_fee_sol, compute_units)?;
        if let Some(max_fee) = Self::max_priority_fee_micro_lamports_from_env(compute_units) {
            anyhow::ensure!(
                fee <= max_fee,
                "priority_fee_sol exceeds MAX_FEE for the current swap budget"
            );
        }
        Ok(fee)
    }

    fn parse_send_confirm_timeout_secs(raw: Option<&str>) -> u64 {
        raw.and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_SEND_CONFIRM_TIMEOUT_SECS)
    }

    pub fn send_confirm_timeout_duration() -> Duration {
        Duration::from_secs(Self::parse_send_confirm_timeout_secs(
            std::env::var(SEND_CONFIRM_TIMEOUT_ENV).ok().as_deref(),
        ))
    }

    fn default_readonly_rpc_url_for_cluster(
        cluster: SolanaCluster,
        current_url: &str,
    ) -> Option<&'static str> {
        match cluster {
            SolanaCluster::MainnetBeta => Some(DEFAULT_MAINNET_HTTP_URL),
            SolanaCluster::Devnet => Some(DEFAULT_DEVNET_HTTP_URL),
            SolanaCluster::Unknown if current_url.contains("devnet") => {
                Some(DEFAULT_DEVNET_HTTP_URL)
            }
            SolanaCluster::Unknown => Some(DEFAULT_MAINNET_HTTP_URL),
            _ => None,
        }
    }

    fn configured_read_rpc_urls_from_env() -> Vec<String> {
        std::env::var("MAMBA_API_HTTP_URLS")
            .ok()
            .map(|raw| {
                raw.split([',', ';', '\n', '\r', ' ', '\t'])
                    .map(str::trim)
                    .map(|value| value.trim_matches('"').trim_matches('\''))
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    fn build_read_rpc_clients(
        rpc_client: Arc<RpcClient>,
        cluster: SolanaCluster,
        extra_clients: Vec<Arc<RpcClient>>,
    ) -> Vec<Arc<RpcClient>> {
        let mut ordered = Vec::<Arc<RpcClient>>::new();
        let mut seen = HashSet::<String>::new();

        let push_client = |ordered: &mut Vec<Arc<RpcClient>>,
                           seen: &mut HashSet<String>,
                           client: Arc<RpcClient>| {
            let url = client.url();
            let label = Self::rpc_url_label(&url).to_string();
            if seen.insert(label) {
                ordered.push(client);
            }
        };

        let primary_url = rpc_client.url();
        push_client(&mut ordered, &mut seen, rpc_client.clone());
        for client in extra_clients {
            push_client(&mut ordered, &mut seen, client);
        }
        for url in Self::configured_read_rpc_urls_from_env() {
            let label = Self::rpc_url_label(&url).to_string();
            if seen.insert(label) {
                ordered.push(Arc::new(RpcClient::new(url)));
            }
        }

        if let Some(fallback) = Self::default_readonly_rpc_url_for_cluster(cluster, &primary_url) {
            let label = Self::rpc_url_label(fallback).to_string();
            if seen.insert(label) {
                ordered.push(Arc::new(RpcClient::new(fallback.to_string())));
            }
        }

        ordered
    }

    pub fn new(rpc_client: String) -> Self {
        Self::from_rpc_client(Arc::new(RpcClient::new(rpc_client)))
    }

    pub fn from_rpc_client(rpc_client: Arc<RpcClient>) -> Self {
        Self::from_rpc_pool_with_cluster(
            rpc_client.clone(),
            vec![rpc_client],
            SolanaCluster::Unknown,
        )
    }

    pub fn from_rpc_client_with_cluster(
        rpc_client: Arc<RpcClient>,
        cluster: SolanaCluster,
    ) -> Self {
        Self::from_rpc_pool_with_cluster(rpc_client.clone(), vec![rpc_client], cluster)
    }

    pub fn from_rpc_pool_with_cluster(
        rpc_client: Arc<RpcClient>,
        rpc_clients: Vec<Arc<RpcClient>>,
        cluster: SolanaCluster,
    ) -> Self {
        Self {
            rpc_client: rpc_client.clone(),
            cluster,
            read_rpc_clients: Arc::new(Self::build_read_rpc_clients(
                rpc_client,
                cluster,
                rpc_clients,
            )),
        }
    }

    pub async fn get_nonce_data(&self, nonce_account: &Pubkey) -> anyhow::Result<(Hash, Pubkey)> {
        let acc = self
            .get_account_with_commitment_resilient(nonce_account, CommitmentConfig::processed())
            .await
            .map_err(|_| anyhow::anyhow!("nonce account does not exist"))?;
        let versions: NonceVersions = bincode::deserialize(&acc.data)?;
        match versions.state() {
            NonceState::Initialized(d) => Ok((*d.durable_nonce.as_hash(), d.authority)),
            _ => anyhow::bail!("nonce account is not initialized"),
        }
    }

    pub async fn create_nonce_account(
        &self,
        payer: &Keypair,
        nonce_account: &Keypair,
        authority: &Pubkey,
    ) -> anyhow::Result<Signature> {
        let space = NonceState::size();
        let lamports = self
            .get_minimum_balance_for_rent_exemption_resilient(space)
            .await?;

        let ixs = system_instruction_if::create_nonce_account(
            &payer.pubkey(),
            &nonce_account.pubkey(),
            authority,
            lamports,
        );

        let (blockhash, _) = self
            .get_latest_blockhash_with_commitment_resilient(CommitmentConfig::processed())
            .await?;

        let msg = V0Message::try_compile(&payer.pubkey(), &ixs, &[], blockhash)?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[payer, nonce_account])?;

        let sig = self.submit_signed_transaction_resilient(&tx).await?;

        Ok(sig)
    }

    pub async fn subscribe_logs_channel(
        &self,
        ws_url: &str,
        filter: RpcTransactionLogsFilter,
        commitment: CommitmentConfig,
    ) -> anyhow::Result<(Receiver<RpcLogsResponse>, tokio::task::JoinHandle<()>)> {
        let ws = ws_url.to_string();
        let (tx, rx) = mpsc::channel::<RpcLogsResponse>(1024);

        let handle = tokio::spawn(async move {
            let ws_label = ws
                .trim()
                .split('?')
                .next()
                .unwrap_or(ws.as_str())
                .to_string();
            let cfg = RpcTransactionLogsConfig {
                commitment: Some(commitment),
            };

            let mut backoff = Duration::from_millis(250);
            loop {
                let client = match PubsubClient::new(&ws).await {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("ws connect failed ({ws_label}): {e}");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(5));
                        continue;
                    }
                };

                let (mut stream, _unsub) =
                    match client.logs_subscribe(filter.clone(), cfg.clone()).await {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("subscribe failed ({ws_label}): {e}");
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(Duration::from_secs(5));
                            continue;
                        }
                    };

                backoff = Duration::from_millis(250);
                while let Some(msg) = stream.next().await {
                    if tx.send(msg.value).await.is_err() {
                        return;
                    }
                }
            }
        });

        Ok((rx, handle))
    }

    pub async fn subscribe_slot_channel(
        &self,
        ws_url: &str,
    ) -> anyhow::Result<(Receiver<SlotInfo>, tokio::task::JoinHandle<()>)> {
        let ws = ws_url.to_string();
        let (tx, rx) = mpsc::channel::<SlotInfo>(1024);

        let handle = tokio::spawn(async move {
            let client = match PubsubClient::new(&ws).await {
                Ok(c) => c,
                Err(e) => {
                    warn!("ws connect failed: {e}");
                    return;
                }
            };
            let (mut stream, _unsub) = match client.slot_subscribe().await {
                Ok(p) => p,
                Err(e) => {
                    warn!("subscribe failed: {e}");
                    return;
                }
            };
            while let Some(msg) = stream.next().await {
                let _ = tx.send(msg).await;
            }
        });
        Ok((rx, handle))
    }

    #[cfg(not(windows))]
    pub async fn grpc_connect(
        url: impl Into<String>,
        token: Option<impl Into<String>>,
    ) -> anyhow::Result<GeyserGrpcClient<impl Interceptor>> {
        let token = token.map(Into::into);
        let mut b = GeyserGrpcBuilder::from_shared(url.into())?;
        if let Some(t) = token {
            b = b.x_token(Some(t))?;
        }
        let grpc = b.connect().await?;
        Ok(grpc)
    }

    #[cfg(not(windows))]
    pub async fn grpc_subscribe_slot_channel(
        grpc_client: &mut GeyserGrpcClient<impl Interceptor>,
    ) -> anyhow::Result<impl Stream<Item = Result<SubscribeUpdate, Status>>> {
        let request = SubscribeRequest {
            slots: HashMap::from([(
                "all".into(),
                SubscribeRequestFilterSlots {
                    filter_by_commitment: Some(true),
                    interslot_updates: Some(true),
                },
            )]),
            ..Default::default()
        };

        let (_, stream) = grpc_client.subscribe_with_request(Some(request)).await?;
        Ok(stream)
    }

    #[cfg(not(windows))]
    pub async fn grpc_subscribe_accounts(
        grpc_client: &mut GeyserGrpcClient<impl Interceptor>,
        account: &Pubkey,
    ) -> anyhow::Result<impl Stream<Item = Result<SubscribeUpdate, Status>>> {
        let request = SubscribeRequest {
            accounts: HashMap::from([(
                "all".into(),
                SubscribeRequestFilterAccounts {
                    account: vec![account.to_string()],
                    owner: vec![],
                    filters: vec![],
                    nonempty_txn_signature: Some(false),
                },
            )]),
            ..Default::default()
        };
        let (_, stream) = grpc_client.subscribe_with_request(Some(request)).await?;
        Ok(stream)
    }

    #[cfg(not(windows))]
    pub async fn grpc_subscribe_transactions(
        grpc_client: &mut GeyserGrpcClient<impl Interceptor>,
        accounts: &[Pubkey],
    ) -> anyhow::Result<impl Stream<Item = Result<SubscribeUpdate, Status>>> {
        let request = SubscribeRequest {
            transactions: HashMap::from([(
                "all".into(),
                SubscribeRequestFilterTransactions {
                    vote: Some(false),
                    failed: Some(false),
                    signature: None,
                    account_include: vec![],
                    account_exclude: vec![],
                    account_required: accounts.iter().map(|a| a.to_string()).collect(),
                },
            )]),
            ..Default::default()
        };
        let (_, stream) = grpc_client.subscribe_with_request(Some(request)).await?;
        Ok(stream)
    }

    #[cfg(not(windows))]
    pub async fn grpc_latest_blockhash(
        &self,
        client: &mut GeyserGrpcClient<impl Interceptor>,
    ) -> anyhow::Result<Hash> {
        let resp = client
            .get_latest_blockhash(Some(CommitmentLevel::Processed))
            .await?;
        Ok(Hash::from_str(&resp.blockhash)?)
    }

    pub fn parse_signature(signature: &[u8]) -> anyhow::Result<Signature> {
        if signature.len() != 64 {
            anyhow::bail!("Signature must be exactly 64 bytes");
        }
        let raw_signature_array: [u8; 64] = signature.try_into()?;
        Ok(Signature::from(raw_signature_array))
    }

    pub async fn exists(&self, address: &Pubkey) -> anyhow::Result<bool> {
        Ok(self
            .run_rpc_attempts_optional("getAccount", |rpc| async move {
                Ok(rpc.get_account(address).await.ok())
            })
            .await?
            .is_some())
    }

    fn rpc_url_label(url: &str) -> &str {
        url.split('?').next().unwrap_or(url)
    }

    #[cfg(test)]
    fn readonly_fallback_rpc_url(&self) -> Option<String> {
        self.read_rpc_clients.get(1).map(|rpc| rpc.url())
    }

    fn read_rpc_clients(&self) -> &[Arc<RpcClient>] {
        self.read_rpc_clients.as_ref()
    }

    fn record_retryable_rpc_cooldown(url: &str) {
        let until = Instant::now() + RPC_ENDPOINT_COOLDOWN_ON_RETRYABLE_ERROR;
        if let Ok(mut cooldowns) = Self::rpc_endpoint_cooldowns().lock() {
            cooldowns.insert(Self::rpc_url_label(url).to_string(), until);
        }
    }

    fn read_rpc_preference_score(operation: &str, url: &str) -> u8 {
        let label = Self::rpc_url_label(url).to_ascii_lowercase();
        let high_volume_read = matches!(
            operation,
            "getAccount"
                | "getAccountWithConfig"
                | "getMultipleAccounts"
                | "getTransaction"
                | "getTransactionParsed"
        );
        if high_volume_read && label.contains("helius-rpc.com") {
            1
        } else {
            0
        }
    }

    fn read_rpc_clients_for_attempts(&self, operation: &str) -> Vec<Arc<RpcClient>> {
        let clients = self.read_rpc_clients();
        if clients.len() <= 1 {
            return clients.to_vec();
        }

        let now = Instant::now();
        let mut ready = Vec::with_capacity(clients.len());
        let mut cooling = Vec::new();

        if let Ok(mut cooldowns) = Self::rpc_endpoint_cooldowns().lock() {
            cooldowns.retain(|_, until| *until > now);
            for client in clients.iter().cloned() {
                let label = Self::rpc_url_label(&client.url()).to_string();
                if cooldowns.get(&label).is_some_and(|until| *until > now) {
                    cooling.push(client);
                } else {
                    ready.push(client);
                }
            }
        } else {
            return clients.to_vec();
        }

        ready.sort_by_key(|client| Self::read_rpc_preference_score(operation, &client.url()));
        cooling.sort_by_key(|client| Self::read_rpc_preference_score(operation, &client.url()));

        if ready.is_empty() {
            clients.to_vec()
        } else {
            ready.extend(cooling);
            ready
        }
    }

    pub async fn run_rpc_attempts<T, Op, Fut>(&self, operation: &str, op: Op) -> anyhow::Result<T>
    where
        Op: Fn(Arc<RpcClient>) -> Fut,
        Fut: Future<Output = anyhow::Result<T>>,
    {
        let clients = self.read_rpc_clients_for_attempts(operation);
        let mut last_retryable_error: Option<anyhow::Error> = None;

        for (idx, client) in clients.iter().cloned().enumerate() {
            match op(client.clone()).await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    let retryable = idx + 1 < clients.len()
                        && Self::rpc_transport_error_is_retryable(&error.to_string());
                    if retryable {
                        Self::record_retryable_rpc_cooldown(&client.url());
                        let next_url = clients
                            .get(idx + 1)
                            .map(|rpc| rpc.url())
                            .unwrap_or_else(|| "<none>".to_string());
                        warn!(
                            "rpc {} failed on {}: {}; retrying via {}",
                            operation,
                            Self::rpc_url_label(&client.url()),
                            error,
                            Self::rpc_url_label(&next_url)
                        );
                        last_retryable_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
            }
        }

        Err(last_retryable_error.unwrap_or_else(|| {
            anyhow::anyhow!("rpc {} failed with no available endpoints", operation)
        }))
    }

    pub async fn run_rpc_attempts_optional<T, Op, Fut>(
        &self,
        operation: &str,
        op: Op,
    ) -> anyhow::Result<Option<T>>
    where
        Op: Fn(Arc<RpcClient>) -> Fut,
        Fut: Future<Output = anyhow::Result<Option<T>>>,
    {
        let clients = self.read_rpc_clients_for_attempts(operation);
        let mut last_retryable_error: Option<anyhow::Error> = None;

        for (idx, client) in clients.iter().cloned().enumerate() {
            match op(client.clone()).await {
                Ok(Some(value)) => return Ok(Some(value)),
                Ok(None) => return Ok(None),
                Err(error) => {
                    let retryable = idx + 1 < clients.len()
                        && Self::rpc_transport_error_is_retryable(&error.to_string());
                    if retryable {
                        Self::record_retryable_rpc_cooldown(&client.url());
                        let next_url = clients
                            .get(idx + 1)
                            .map(|rpc| rpc.url())
                            .unwrap_or_else(|| "<none>".to_string());
                        warn!(
                            "rpc {} failed on {}: {}; retrying via {}",
                            operation,
                            Self::rpc_url_label(&client.url()),
                            error,
                            Self::rpc_url_label(&next_url)
                        );
                        last_retryable_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
            }
        }

        if let Some(error) = last_retryable_error {
            Err(error)
        } else {
            Ok(None)
        }
    }

    pub(crate) async fn get_account_with_commitment_resilient(
        &self,
        address: &Pubkey,
        commitment: CommitmentConfig,
    ) -> anyhow::Result<Account> {
        self.run_rpc_attempts_optional("getAccount", |rpc| async move {
            Ok(rpc
                .get_account_with_commitment(address, commitment)
                .await?
                .value)
        })
        .await?
        .ok_or_else(|| anyhow::anyhow!("account {} not found", address))
    }

    pub(crate) async fn get_latest_blockhash_with_commitment_resilient(
        &self,
        commitment: CommitmentConfig,
    ) -> anyhow::Result<(Hash, u64)> {
        self.run_rpc_attempts("getLatestBlockhash", |rpc| async move {
            Ok(rpc.get_latest_blockhash_with_commitment(commitment).await?)
        })
        .await
    }

    pub(crate) async fn get_slot_with_commitment_resilient(
        &self,
        commitment: CommitmentConfig,
    ) -> anyhow::Result<u64> {
        self.run_rpc_attempts("getSlot", |rpc| async move {
            Ok(rpc.get_slot_with_commitment(commitment).await?)
        })
        .await
    }

    pub async fn detect_cluster(&self) -> anyhow::Result<SolanaCluster> {
        if self.cluster != SolanaCluster::Unknown {
            return Ok(self.cluster);
        }

        let genesis_hash = self
            .run_rpc_attempts("getGenesisHash", |rpc| async move {
                Ok(rpc.get_genesis_hash().await?)
            })
            .await?;
        Ok(SolanaCluster::from_genesis_hash(&genesis_hash.to_string()))
    }

    pub async fn get_balance_lamports(&self, address: &Pubkey) -> anyhow::Result<u64> {
        self.run_rpc_attempts("getBalance", |rpc| async move {
            Ok(rpc.get_balance(address).await?)
        })
        .await
    }

    pub async fn get_minimum_balance_for_rent_exemption_resilient(
        &self,
        data_len: usize,
    ) -> anyhow::Result<u64> {
        self.run_rpc_attempts("getMinimumBalanceForRentExemption", |rpc| async move {
            Ok(rpc.get_minimum_balance_for_rent_exemption(data_len).await?)
        })
        .await
    }

    pub async fn get_multiple_accounts_resilient(
        &self,
        addresses: &[Pubkey],
    ) -> anyhow::Result<Vec<Option<Account>>> {
        self.run_rpc_attempts("getMultipleAccounts", |rpc| async move {
            Ok(rpc.get_multiple_accounts(addresses).await?)
        })
        .await
    }

    pub async fn get_program_ui_accounts_with_config_resilient(
        &self,
        program_id: &Pubkey,
        config: RpcProgramAccountsConfig,
    ) -> anyhow::Result<Vec<(Pubkey, UiAccount)>> {
        self.run_rpc_attempts("getProgramAccounts", |rpc| {
            let config = config.clone();
            async move {
                Ok(rpc
                    .get_program_ui_accounts_with_config(program_id, config)
                    .await?)
            }
        })
        .await
    }

    pub async fn simulate_transaction_with_config_resilient(
        &self,
        tx: &VersionedTransaction,
        config: solana_rpc_client_types::config::RpcSimulateTransactionConfig,
    ) -> anyhow::Result<RpcResponse<RpcSimulateTransactionResult>> {
        self.run_rpc_attempts("simulateTransaction", |rpc| {
            let config = config.clone();
            async move { Ok(rpc.simulate_transaction_with_config(tx, config).await?) }
        })
        .await
    }

    fn rpc_transport_error_is_retryable(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("429")
            || lower.contains("too many requests")
            || lower.contains("rate limit")
            || lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("connection")
            || lower.contains("unavailable")
            || lower.contains("overloaded")
    }

    fn default_send_transaction_config() -> RpcSendTransactionConfig {
        RpcSendTransactionConfig {
            skip_preflight: true,
            preflight_commitment: None,
            encoding: None,
            max_retries: Some(0),
            min_context_slot: None,
        }
    }

    async fn submit_signed_transaction_resilient(
        &self,
        tx: &VersionedTransaction,
    ) -> anyhow::Result<Signature> {
        let send_config = Self::default_send_transaction_config();
        self.run_rpc_attempts("sendTransaction", |rpc| async move {
            Ok(rpc.send_transaction_with_config(tx, send_config).await?)
        })
        .await
    }

    async fn confirm_transaction_with_commitment_resilient(
        &self,
        sig: &Signature,
        commitment: CommitmentConfig,
    ) -> anyhow::Result<bool> {
        self.run_rpc_attempts("confirmTransaction", |rpc| async move {
            Ok(rpc
                .confirm_transaction_with_commitment(sig, commitment)
                .await?
                .value)
        })
        .await
    }

    pub async fn get_token_decimals(&self, mint: &Pubkey) -> anyhow::Result<u8> {
        if *mint == WSOL_MINT {
            return Ok(9);
        }

        let acc = self
            .get_account_with_commitment_resilient(mint, CommitmentConfig::processed())
            .await
            .map_err(|e| anyhow::anyhow!("Error getting account: {:?}", e))?;
        if acc.owner == TOKEN_PROGRAM_ID {
            let mint_state = SplMint::unpack(&acc.data)
                .map_err(|e| anyhow::anyhow!("not an SPL Token mint: {e}"))?;
            Ok(mint_state.decimals)
        } else if acc.owner == TOKEN_2022_PROGRAM_ID {
            use spl_token_2022::extension::StateWithExtensions;

            let mint_state = StateWithExtensions::<SplMint2022>::unpack(&acc.data)
                .map_err(|e| anyhow::anyhow!("not a Token-2022 mint: {e}"))?;
            Ok(mint_state.base.decimals)
        } else {
            anyhow::bail!(
                "account {} not owned by SPL Token program(s): {}",
                mint,
                acc.owner
            );
        }
    }

    pub async fn get_token_metadata(&self, mint: &Pubkey) -> anyhow::Result<(MplMetadata, Pubkey)> {
        let (pda, _) = Pubkey::find_program_address(
            &[b"metadata", METADATA_PROGRAM_ID.as_ref(), mint.as_ref()],
            &METADATA_PROGRAM_ID,
        );
        let acc = self
            .get_account_with_commitment_resilient(&pda, CommitmentConfig::processed())
            .await?;
        let md = MplMetadata::safe_deserialize(&acc.data)
            .with_context(|| format!("failed to deserialize token metadata account {}", pda))?;
        Ok((md, pda))
    }

    fn extract_first_metadata_creator(metadata: &MplMetadata) -> Option<Pubkey> {
        metadata
            .creators
            .as_ref()
            .and_then(|creators| creators.first().map(|entry| entry.address))
            .map(|address| pubkey_to_address(&Address::from(address.to_bytes())))
    }

    /// Resolve the canonical "first creator" from Metaplex metadata when available.
    pub async fn get_mint_first_creator(&self, mint: &Pubkey) -> anyhow::Result<Option<Pubkey>> {
        Ok(self
            .get_token_metadata(mint)
            .await
            .ok()
            .and_then(|(metadata, _)| Self::extract_first_metadata_creator(&metadata)))
    }

    pub async fn get_token_info(&self, mint: &Pubkey) -> anyhow::Result<TokenInfo> {
        let acc = self
            .get_account_with_commitment_resilient(mint, CommitmentConfig::processed())
            .await?;

        if acc.owner == TOKEN_PROGRAM_ID {
            // Classic Metaplex metadata account (mpl-token-metadata)
            let (md, _pda) = self.get_token_metadata(mint).await?;
            let creator = Self::extract_first_metadata_creator(&md);
            let name = trim_nul(&md.name).to_string();
            let symbol = trim_nul(&md.symbol).to_string();
            let uri = trim_nul(&md.uri).to_string();

            if name.is_empty() && symbol.is_empty() && uri.is_empty() {
                log!(
                    cc::LIGHT_RED,
                    "Token metadata is empty for mint: {:?}",
                    mint
                );
                return Err(anyhow::anyhow!("Token metadata is empty"));
            }

            return Ok(TokenInfo {
                mint: *mint,
                name,
                symbol,
                uri,
                creator,
                authority: pubkey_to_address(&Address::from(md.update_authority.to_bytes())),
            });
        }

        if acc.owner != TOKEN_2022_PROGRAM_ID {
            anyhow::bail!("Unsupported token program: {}", acc.owner);
        }

        use spl_token_2022::extension::{
            BaseStateWithExtensions, StateWithExtensions, metadata_pointer::MetadataPointer,
        };
        use spl_token_metadata_interface::state::TokenMetadata as T22Metadata;
        use spl_type_length_value::state::{TlvState, TlvStateBorrowed};

        let mint_state = StateWithExtensions::<SplMint2022>::unpack(&acc.data).map_err(|e2| {
            anyhow::anyhow!("failed to unpack token-2022 mint with extensions: {e2}")
        })?;

        // First try the inline token-2022 metadata extension.
        let md22_inline = mint_state.get_variable_len_extension::<T22Metadata>().ok();

        // If no inline metadata or empty values, try resolving via MetadataPointer.
        let mut md22 = md22_inline;
        if md22.as_ref().is_none_or(|m| {
            m.name.trim_matches('\0').is_empty() || m.symbol.trim_matches('\0').is_empty()
        }) && let Ok(pointer) = mint_state.get_extension::<MetadataPointer>()
            && let Some(meta_address) = pointer.metadata_address.into()
            && let Ok(meta_acc) = self
                .get_account_with_commitment_resilient(&meta_address, CommitmentConfig::processed())
                .await
            && let Ok(state) = TlvStateBorrowed::unpack(&meta_acc.data)
            && let Ok(metadata) = state.get_first_variable_len_value::<T22Metadata>()
        {
            md22 = Some(metadata);
        }

        let md22 =
            md22.ok_or_else(|| anyhow::anyhow!("failed to decode token-2022 metadata extension"))?;

        let name = trim_nul(&md22.name).to_string();
        let symbol = trim_nul(&md22.symbol).to_string();
        let uri = trim_nul(&md22.uri).to_string();

        if name.is_empty() && symbol.is_empty() && uri.is_empty() {
            anyhow::bail!("token-2022 metadata missing name/symbol/uri");
        }

        let authority = Pubkey::default();
        let creator = self.get_mint_first_creator(mint).await.unwrap_or(None);

        Ok(TokenInfo {
            mint: *mint,
            name,
            symbol,
            uri,
            creator,
            authority,
        })
    }

    pub async fn get_token_program_id(&self, token_address: &Pubkey) -> anyhow::Result<Pubkey> {
        Ok(self
            .get_account_with_commitment_resilient(token_address, CommitmentConfig::processed())
            .await?
            .owner)
    }

    pub async fn get_token_program_id_for_token_account(
        &self,
        token_account: &Pubkey,
    ) -> anyhow::Result<Pubkey> {
        let owner = self
            .get_account_with_commitment_resilient(token_account, CommitmentConfig::processed())
            .await?
            .owner;
        anyhow::ensure!(
            owner == TOKEN_PROGRAM_ID || owner == TOKEN_2022_PROGRAM_ID,
            "account {} not owned by SPL Token programs, owner = {}",
            token_account,
            owner
        );
        Ok(owner)
    }

    pub async fn get_ata_auto(&self, owner: &Pubkey, mint: &Pubkey) -> anyhow::Result<Pubkey> {
        let token_program_id = self.get_token_program_id(mint).await?;
        let ata = match token_program_id {
            TOKEN_PROGRAM_ID => self.get_ata_for_token(owner, mint),
            TOKEN_2022_PROGRAM_ID => self.get_ata_for_token2022(owner, mint),
            _ => return Err(anyhow::anyhow!("Invalid token program id")),
        };
        Ok(ata)
    }

    pub fn get_ata_for_token(&self, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
        let (ata, _) = Pubkey::find_program_address(
            &[owner.as_ref(), TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
            &ATA_PROGRAM_ID,
        );
        ata
    }

    pub fn get_ata_for_token2022(&self, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
        let (ata, _) = Pubkey::find_program_address(
            &[
                owner.as_ref(),
                TOKEN_2022_PROGRAM_ID.as_ref(),
                mint.as_ref(),
            ],
            &ATA_PROGRAM_ID,
        );
        ata
    }

    pub async fn get_account(&self, address: &Pubkey) -> anyhow::Result<Account> {
        use anyhow::Context;

        let account = self
            .run_rpc_attempts_optional("getAccountWithConfig", |rpc| async move {
                Ok(rpc
                    .get_account_with_config(
                        address,
                        RpcAccountInfoConfig {
                            encoding: Some(UiAccountEncoding::JsonParsed),
                            commitment: Some(CommitmentConfig::finalized()),
                            min_context_slot: None,
                            data_slice: None,
                        },
                    )
                    .await?
                    .value)
            })
            .await?;
        account.context(format!("account {} not found", address))
    }

    pub async fn get_mint_from_token_account(
        &self,
        token_account: Address,
    ) -> anyhow::Result<Address> {
        use anyhow::Context;

        let acc = self
            .get_account_with_commitment_resilient(&token_account, CommitmentConfig::processed())
            .await
            .context("token account not found")?;

        if acc.owner == TOKEN_PROGRAM_ID {
            let ta = SplTokenAccount::unpack(&acc.data).map_err(|e| {
                anyhow::anyhow!("not an SPL Token account: {e} for {token_account}")
            })?;
            Ok(Address::from(ta.mint.to_bytes()))
        } else if acc.owner == TOKEN_2022_PROGRAM_ID {
            let ta = SplToken2022Account::unpack(&acc.data).map_err(|e| {
                anyhow::anyhow!("not a Token-2022 account: {e} for {token_account}")
            })?;
            Ok(Address::from(ta.mint.to_bytes()))
        } else {
            anyhow::bail!(
                "account {} not owned by SPL Token programs, owner = {}",
                token_account,
                acc.owner
            );
        }
    }

    pub async fn get_balance(&self, address: &Pubkey) -> anyhow::Result<f64> {
        let balance = self.get_balance_lamports(address).await?;
        Ok(balance as f64 / 1e9)
    }

    /// Returns the balance of a token in the ui amount format
    ///
    /// # Arguments
    ///
    /// * `address` - The address of the account to get the balance of
    /// * `token_address` - The address of the token to get the balance of
    ///
    /// # Returns
    ///
    /// The balance of the token in the ui_amount format
    ///
    /// # Errors
    ///
    pub async fn get_token_balance(
        &self,
        address: &Pubkey,
        token_address: &Pubkey,
    ) -> anyhow::Result<f64> {
        let token_ata = match self.get_ata_auto(address, token_address).await {
            Ok(ata) => ata,
            Err(e) => return Err(anyhow::anyhow!(format!("Error getting token ata: {:?}", e))),
        };
        self.get_token_balance_from_ata(&token_ata).await
    }

    pub async fn get_token_balance_raw(
        &self,
        address: &Pubkey,
        token_address: &Pubkey,
    ) -> anyhow::Result<(u64, u8)> {
        let token_ata = self.get_ata_auto(address, token_address).await?;
        self.get_token_balance_raw_from_ata(&token_ata).await
    }

    pub async fn get_token_balance_raw_from_ata(&self, ata: &Pubkey) -> anyhow::Result<(u64, u8)> {
        let token_balance = self
            .run_rpc_attempts("getTokenAccountBalance", |rpc| async move {
                Ok(rpc
                    .get_token_account_balance_with_commitment(ata, CommitmentConfig::confirmed())
                    .await?)
            })
            .await?;
        let raw = token_balance
            .value
            .amount
            .parse::<u64>()
            .map_err(|e| anyhow::anyhow!("failed to parse token balance raw amount: {e}"))?;
        Ok((raw, token_balance.value.decimals))
    }

    pub async fn get_token_balance_from_ata(&self, ata: &Pubkey) -> anyhow::Result<f64> {
        let (raw, decimals) = self.get_token_balance_raw_from_ata(ata).await?;
        Ok((raw as f64) / 10_f64.powi(decimals as i32))
    }

    pub async fn get_transaction(
        &self,
        signature: &Signature,
    ) -> anyhow::Result<EncodedConfirmedTransactionWithStatusMeta> {
        let config = RpcTransactionConfig {
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(0),
            ..Default::default()
        };
        self.run_rpc_attempts("getTransaction", |rpc| async move {
            Ok(rpc.get_transaction_with_config(signature, config).await?)
        })
        .await
    }

    pub async fn get_transaction_parsed(
        &self,
        sig: &Signature,
    ) -> anyhow::Result<EncodedConfirmedTransactionWithStatusMeta> {
        let cfg = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::JsonParsed),
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(0),
        };
        self.run_rpc_attempts("getTransactionParsed", |rpc| async move {
            Ok(rpc.get_transaction_with_config(sig, cfg).await?)
        })
        .await
    }

    pub async fn fetch_signature_failure_details(
        &self,
        sig: &Signature,
    ) -> anyhow::Result<Option<String>> {
        let statuses = self
            .run_rpc_attempts("getSignatureStatuses", |rpc| async move {
                Ok(rpc.get_signature_statuses(&[*sig]).await?)
            })
            .await?;
        let Some(status) = statuses.value.into_iter().next().flatten() else {
            return Ok(None);
        };
        let Some(err) = status.err else {
            return Ok(None);
        };

        let mut details = format!("status err: {:?}", err);

        if let Ok(tx) = self.get_transaction(sig).await
            && let Some(meta) = tx.transaction.meta
        {
            if let Some(meta_err) = meta.err {
                details.push_str(&format!(" | meta err: {:?}", meta_err));
            }
            if let solana_transaction_status::option_serializer::OptionSerializer::Some(logs) =
                meta.log_messages
                && let Some(line) = logs.iter().rev().find(|line| {
                    line.contains("Error Code:")
                        || line.contains("custom program error")
                        || line.contains("AnchorError")
                })
            {
                details.push_str(&format!(" | log: {}", line));
            }
        }

        Ok(Some(details))
    }

    pub fn clamp_priority_fee_from_env(
        priority_fee_micro_lamports: u64,
        compute_units: u32,
    ) -> u64 {
        match Self::max_priority_fee_micro_lamports_from_env(compute_units) {
            Some(max_price) => priority_fee_micro_lamports.min(max_price),
            None => priority_fee_micro_lamports,
        }
    }

    pub fn close_token_account_ix(
        &self,
        token_program_id: &Pubkey,
        account_pubkey: &Pubkey,
        destination_pubkey: &Pubkey,
        owner_pubkey: &Pubkey,
    ) -> anyhow::Result<Instruction> {
        spl_token_2022::instruction::close_account(
            token_program_id,
            account_pubkey,
            destination_pubkey,
            owner_pubkey,
            &[],
        )
        .map_err(|e| anyhow::anyhow!("failed to build close account instruction: {:?}", e))
    }

    pub async fn fetch_priority_fee(
        &self,
        level: &PriorityFeeLevel,
        addresses: &[Pubkey],
    ) -> anyhow::Result<u64> {
        fn default_fee(level: &PriorityFeeLevel) -> u64 {
            match level {
                PriorityFeeLevel::Low => 50_000,
                PriorityFeeLevel::Medium => 100_000,
                PriorityFeeLevel::High => 500_000,
                PriorityFeeLevel::Turbo => 1_000_000,
                PriorityFeeLevel::Max => 30_000_000,
            }
        }

        let mut set = HashSet::with_capacity(addresses.len());
        let mut addrs: Vec<Pubkey> = addresses
            .iter()
            .copied()
            .filter(|k| set.insert(*k))
            .take(128)
            .collect();

        if addrs.is_empty() {
            addrs = vec![SYSTEM_PROGRAM, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID];
        }

        let fees = match self
            .run_rpc_attempts("getRecentPrioritizationFees", |rpc| {
                let addrs = addrs.clone();
                async move {
                    tokio::time::timeout(
                        Duration::from_secs(4),
                        rpc.get_recent_prioritization_fees(&addrs),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!("priority fee estimate rpc timed out"))?
                    .map_err(anyhow::Error::from)
                }
            })
            .await
        {
            Ok(fees) => fees,
            Err(error) => {
                warn!("priority fee estimate rpc error: {error:?} (using default for {level:?})");
                return Ok(default_fee(level));
            }
        };

        let mut samples = fees
            .into_iter()
            .map(|r| r.prioritization_fee)
            .filter(|&v| v > 0)
            .collect::<Vec<u64>>();

        if samples.is_empty() {
            return Ok(default_fee(level));
        }

        samples.sort_unstable();
        let p = match level {
            PriorityFeeLevel::Low => 0.50,
            PriorityFeeLevel::Medium => 0.75,
            PriorityFeeLevel::High => 0.90,
            PriorityFeeLevel::Turbo => 0.95,
            PriorityFeeLevel::Max => 0.99,
        };

        let idx = ((samples.len() - 1) as f64 * p).ceil() as usize;
        let mut price = samples[idx.min(samples.len() - 1)];

        if price < 10_000 {
            price = 10_000;
        }
        if price > 30_000_000 {
            price = 30_000_000;
        }

        Ok(price)
    }

    pub async fn resolve_priority_fee(
        &self,
        override_fee: Option<PriorityFeeOverride>,
        addresses: &[Pubkey],
        compute_units: u32,
    ) -> anyhow::Result<u64> {
        match override_fee {
            Some(PriorityFeeOverride::ExactMicroLamports(fee)) => Ok(fee),
            Some(PriorityFeeOverride::Level(level)) => {
                let fee = self.fetch_priority_fee(&level, addresses).await?;
                Ok(Self::clamp_priority_fee_from_env(fee, compute_units))
            }
            None => {
                let level = PriorityFeeLevel::from_env();
                let fee = self.fetch_priority_fee(&level, addresses).await?;
                Ok(Self::clamp_priority_fee_from_env(fee, compute_units))
            }
        }
    }

    pub async fn fetch_sol_price(&self) -> anyhow::Result<f64> {
        use anyhow::Context;

        let response = reqwest::get(
            "https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd",
        )
        .await
        .context("failed to fetch SOL price from CoinGecko")?;
        let data: serde_json::Value = response
            .error_for_status()
            .context("CoinGecko returned an error status for SOL price lookup")?
            .json()
            .await
            .context("failed to decode CoinGecko SOL price response")?;
        data["solana"]["usd"]
            .as_f64()
            .context("CoinGecko response missing solana.usd price")
    }

    pub async fn close_ata(
        &self,
        mint: &Pubkey,
        owner: &Keypair,
        return_ix: bool,
    ) -> anyhow::Result<(Instruction, Signature)> {
        let blockhash = self
            .get_latest_blockhash_with_commitment_resilient(CommitmentConfig::processed())
            .await?
            .0;
        let ata = self.get_ata_auto(&owner.pubkey(), mint).await?;
        let token_program_id = self.get_token_program_id(mint).await.map_err(|e| {
            anyhow::anyhow!("failed to resolve token program for close_ata: {:?}", e)
        })?;
        let ix =
            self.close_token_account_ix(&token_program_id, &ata, &owner.pubkey(), &owner.pubkey())?;
        if return_ix {
            return Ok((ix, Signature::default()));
        }
        let msg = VersionedMessage::V0(V0Message::try_compile(
            &owner.pubkey(),
            &[ix.clone()],
            &[],
            blockhash,
        )?);
        let tx = VersionedTransaction::try_new(msg, &[&owner])?;
        let sig = self.submit_signed_transaction_resilient(&tx).await?;
        self.wait_for_confirmed_signature(&sig).await?;
        Ok((ix, sig))
    }

    pub async fn send(
        &self,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: Option<u32>,
    ) -> anyhow::Result<Signature> {
        let time_start = Instant::now();
        ixs.insert(0, ix_set_compute_unit_price(fee));
        if let Some(compute_budget) = compute_budget {
            ixs.insert(1, ix_set_compute_unit_limit(compute_budget));
        }
        let blockhash = self
            .get_latest_blockhash_with_commitment_resilient(CommitmentConfig::processed())
            .await?
            .0;
        let block_1 = match self
            .get_slot_with_commitment_resilient(CommitmentConfig::processed())
            .await
        {
            Ok(slot) => Some(slot),
            Err(error) => {
                warn!("failed to fetch pre-send slot telemetry: {error}");
                None
            }
        };
        let msg = VersionedMessage::V0(V0Message::try_compile(
            &payer.pubkey(),
            &ixs,
            &[],
            blockhash,
        )?);
        let tx = VersionedTransaction::try_new(msg, &[payer])?;
        let sig = self.submit_signed_transaction_resilient(&tx).await?;
        log!("Time to send: {:?}", time_start.elapsed());
        self.wait_for_confirmed_signature(&sig).await?;
        if let Some(block_1) = block_1 {
            match self
                .get_slot_with_commitment_resilient(CommitmentConfig::processed())
                .await
            {
                Ok(block_2) => log!("Blocks diff: n+{:?}", block_2.saturating_sub(block_1)),
                Err(error) => warn!(
                    "failed to fetch post-confirm slot telemetry for {}: {error}",
                    sig
                ),
            }
        }
        log!("Time taken: {:?}", time_start.elapsed());
        Ok(sig)
    }

    async fn wait_for_confirmed_signature(&self, sig: &Signature) -> anyhow::Result<()> {
        let started = Instant::now();
        let confirm_timeout = Self::send_confirm_timeout_duration();
        let mut polls = 0u32;
        loop {
            if self
                .confirm_transaction_with_commitment_resilient(sig, CommitmentConfig::confirmed())
                .await?
            {
                return Ok(());
            }

            if polls.is_multiple_of(SEND_FAILURE_DETAILS_POLL_EVERY)
                && let Some(details) = self.fetch_signature_failure_details(sig).await?
            {
                anyhow::bail!("Transaction failed {} | {}", sig, details);
            }

            if started.elapsed() >= confirm_timeout {
                if let Some(details) = self.fetch_signature_failure_details(sig).await? {
                    anyhow::bail!("Transaction failed {} | {}", sig, details);
                }
                if let Ok(tx) = self.get_transaction(sig).await
                    && tx
                        .transaction
                        .meta
                        .as_ref()
                        .is_some_and(|meta| meta.err.is_none())
                {
                    return Ok(());
                }
                anyhow::bail!(
                    "Transaction failed to confirm {} | didn't land on-chain in {}s",
                    sig,
                    confirm_timeout.as_secs()
                );
            }

            tokio::time::sleep(SEND_CONFIRM_POLL_INTERVAL).await;
            polls += 1;
        }
    }

    fn jito_clients_for_settings(settings: &SWQoSettings) -> Vec<JitoClient> {
        let auth = settings
            .jito_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        JITO_BLOCK_ENGINES
            .iter()
            .map(|endpoint| match auth {
                Some(uuid) => JitoClient::with_auth(*endpoint, uuid),
                None => JitoClient::new(*endpoint),
            })
            .collect()
    }

    async fn ensure_swqos_cluster_supported(&self) -> anyhow::Result<()> {
        let cluster = self
            .detect_cluster()
            .await
            .unwrap_or(SolanaCluster::Unknown);
        anyhow::ensure!(
            cluster == SolanaCluster::MainnetBeta,
            "SWQoS senders are only supported on mainnet-beta (got {:?})",
            cluster
        );
        Ok(())
    }

    async fn confirm_submitted_signature(&self, sig: &Signature) -> anyhow::Result<Signature> {
        self.wait_for_confirmed_signature(sig).await?;
        Ok(*sig)
    }

    pub async fn submit_signed(&self, tx: &VersionedTransaction) -> anyhow::Result<Signature> {
        let sig = self.submit_signed_transaction_resilient(tx).await?;
        self.confirm_submitted_signature(&sig).await
    }

    pub async fn submit_signed_via_swqos(
        &self,
        tx: &VersionedTransaction,
        settings: &SWQoSettings,
    ) -> anyhow::Result<Signature> {
        self.ensure_swqos_cluster_supported().await?;
        if settings.provider.requires_api_key() && settings.active_provider_key().is_none() {
            anyhow::bail!("missing API key for {}", settings.provider.label());
        }

        let sig = tx
            .signatures
            .first()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("signed transaction is missing a signature"))?;
        let b64 = {
            let wire = bincode::serialize(tx)?;
            B64.encode(wire)
        };

        let reqs = match settings.provider {
            SwqosProvider::Jito => Self::jito_clients_for_settings(settings)
                .into_iter()
                .map(|client| client.send_bundle(&[&b64]))
                .collect(),
            SwqosProvider::Helius => HeliusSender::multiple_new(&HELIUS_SENDER_ENDPOINTS)
                .into_iter()
                .map(|client| client.send_transaction(&b64, true))
                .collect(),
            SwqosProvider::NextBlock => {
                let auth = settings
                    .active_provider_key()
                    .ok_or_else(|| anyhow::anyhow!("missing API key for nextblock"))?;
                NextBlock::multiple_new(&NB_ENDPOINTS, auth)
                    .into_iter()
                    .map(|client| client.send_transaction(&b64))
                    .collect()
            }
            SwqosProvider::ZeroSlot => {
                let auth = settings
                    .active_provider_key()
                    .ok_or_else(|| anyhow::anyhow!("missing API key for zero_slot"))?;
                ZeroSlot::multiple_new(&ZERO_SLOT_ENDPOINTS, auth)
                    .into_iter()
                    .map(|client| client.send_transaction(&b64))
                    .collect()
            }
            SwqosProvider::Temporal => {
                let auth = settings
                    .active_provider_key()
                    .ok_or_else(|| anyhow::anyhow!("missing API key for temporal"))?;
                TemporalSender::multiple_new(&TEMPORAL_HTTP_ENDPOINTS, auth)
                    .into_iter()
                    .map(|client| client.send_transaction(&b64))
                    .collect()
            }
            SwqosProvider::Bloxroute => {
                let auth = settings
                    .active_provider_key()
                    .ok_or_else(|| anyhow::anyhow!("missing API key for bloxroute"))?;
                Bloxroute::multiple_new(BLOX_ENDPOINTS, auth)
                    .into_iter()
                    .map(|client| {
                        client.submit(
                            &b64,
                            &SubmitOpts {
                                skip_preflight: Some(true),
                                front_running_protection: Some(false),
                                submit_protection: Some(SubmitProtection::Low),
                                fast_best_effort: Some(false),
                                use_staked_rpcs: Some(true),
                                allow_back_run: Some(false),
                                revenue_address: Some(String::new()),
                            },
                        )
                    })
                    .collect()
            }
        };

        self.spawn_forget(reqs).await;
        self.confirm_submitted_signature(&sig).await
    }

    pub async fn send_with_swqos(
        &self,
        ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: Option<u32>,
        settings: &SWQoSettings,
    ) -> anyhow::Result<Signature> {
        self.ensure_swqos_cluster_supported().await?;
        let mut prepared = Vec::new();
        let blockhash = if let Some(nonce_account) = settings
            .nonce_account
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let nonce_pubkey = Pubkey::from_str(nonce_account)
                .map_err(|_| anyhow::anyhow!("invalid nonce_account pubkey: {nonce_account}"))?;
            let (nonce_hash, _) = self.get_nonce_data(&nonce_pubkey).await?;
            prepared.push(system_instruction_if::advance_nonce_account(
                &nonce_pubkey,
                &payer.pubkey(),
            ));
            nonce_hash
        } else {
            self.get_latest_blockhash_with_commitment_resilient(CommitmentConfig::processed())
                .await?
                .0
        };

        prepared.push(ix_set_compute_unit_price(fee));
        if let Some(limit) = compute_budget {
            prepared.push(ix_set_compute_unit_limit(limit));
        }
        if settings.tip_lamports > 0 {
            prepared.push(system_instruction_if::transfer(
                &payer.pubkey(),
                &tip_account_for_provider(settings.provider),
                settings.tip_lamports,
            ));
        }
        prepared.extend(ixs);

        let msg = VersionedMessage::V0(V0Message::try_compile(
            &payer.pubkey(),
            &prepared,
            &[],
            blockhash,
        )?);
        let tx = VersionedTransaction::try_new(msg, &[payer])?;
        self.submit_signed_via_swqos(&tx, settings).await
    }

    pub async fn send_with_jito(
        &self,
        jito_client: &JitoClient,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: u32,
    ) -> anyhow::Result<Signature> {
        let time_start = Instant::now();
        ixs.insert(0, ix_set_compute_unit_price(fee));
        ixs.insert(1, ix_set_compute_unit_limit(compute_budget));

        let rta = jito_client.get_tip_account();
        let jito_tip_ix = system_instruction_if::transfer(&payer.pubkey(), &rta, 1_000_000);

        ixs.insert(2, jito_tip_ix);
        let blockhash = self
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?
            .0;
        let block_1 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        eprintln!("Block 0: {:?}", block_1);
        let msg = VersionedMessage::V0(V0Message::try_compile(
            &payer.pubkey(),
            &ixs,
            &[],
            blockhash,
        )?);

        let tx = VersionedTransaction::try_new(msg, &[payer])?;

        let wire = bincode::serialize(&tx)?;
        let b64 = B64.encode(wire.clone());

        let resp = jito_client.send_transaction(&b64, true).send().await?;
        let v: serde_json::Value = resp.json().await?;
        let sig_str = v
            .get("result")
            .and_then(|r| r.as_str())
            .ok_or_else(|| anyhow::anyhow!(format!("no result in response: {}", v)))?;
        let sig = Signature::from_str(sig_str)?;
        let mut retry_count = 0;
        while !self
            .rpc_client
            .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
            .await?
            .value
        {
            tokio::time::sleep(std::time::Duration::from_secs_f32(0.1)).await;
            retry_count += 1;
            if retry_count > 50 {
                anyhow::bail!(
                    "Transaction failed to confirm https://solscan.io/tx/{}",
                    sig
                );
            }
        }
        let block_2 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        println!("Blocks diff: n+{:?}", block_2 - block_1);
        println!("Time taken: {:?}", time_start.elapsed());
        Ok(sig)
    }

    pub async fn send_with_jito_bundle(
        &self,
        jito_client: &JitoClient,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: u32,
        tip_lamports: u64,
    ) -> anyhow::Result<Signature> {
        ixs.insert(0, ix_set_compute_unit_price(fee));
        ixs.insert(1, ix_set_compute_unit_limit(compute_budget));

        let tip_acc = jito_client.get_tip_account();
        ixs.insert(
            2,
            system_instruction_if::transfer(&payer.pubkey(), &tip_acc, tip_lamports),
        );

        let (blockhash, _) = self
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?;

        let msg = V0Message::try_compile(&payer.pubkey(), &ixs, &[], blockhash)?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[payer])?;
        let sig = tx.signatures[0];

        let wire = bincode::serialize(&tx)?;
        let b64 = B64.encode(wire);

        let resp = jito_client.send_bundle(&[b64]).send().await?;
        let v: serde_json::Value = resp.json().await?;
        let bundle_id = v
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("<no-bundle-id>");
        eprintln!("Jito bundle_id: {}", bundle_id);

        let mut tries = 0u32;
        while !self
            .rpc_client
            .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
            .await?
            .value
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tries += 1;
            if tries > 300 {
                anyhow::bail!("Jito bundle timed out https://solscan.io/tx/{}", sig);
            }
        }
        Ok(sig)
    }

    pub async fn spray_with_jito(
        &self,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: u32,
        clients: &Vec<JitoClient>,
    ) -> anyhow::Result<Signature> {
        let time_start = Instant::now();
        ixs.insert(0, ix_set_compute_unit_price(fee));
        ixs.insert(1, ix_set_compute_unit_limit(compute_budget));

        let rta = clients[0].get_tip_account();
        let jito_tip_ix = system_instruction_if::transfer(&payer.pubkey(), &rta, 1_000_000);

        ixs.insert(2, jito_tip_ix);
        let blockhash = self
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?
            .0;
        let block_1 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        println!(
            "Block 0: {:?} | Elapsed: {:?}",
            block_1,
            time_start.elapsed()
        );
        let msg = VersionedMessage::V0(V0Message::try_compile(
            &payer.pubkey(),
            &ixs,
            &[],
            blockhash,
        )?);

        let tx = VersionedTransaction::try_new(msg, &[payer])?;
        let sig = tx.signatures[0];
        let wire = bincode::serialize(&tx)?;
        let b64 = B64.encode(wire.clone());

        let be_fut = async {
            let reqs: Vec<_> = clients
                .iter()
                .map(|c| c.send_transaction(&b64, true))
                .collect();
            self.spawn_forget(reqs).await;
        };

        let _ = tokio::join!(be_fut);
        let mut retry_count = 0;
        while !self
            .rpc_client
            .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
            .await?
            .value
        {
            tokio::time::sleep(std::time::Duration::from_secs_f32(0.1)).await;
            retry_count += 1;
            if retry_count > 50 {
                anyhow::bail!(
                    "Transaction failed to confirm https://solscan.io/tx/{}",
                    sig
                );
            }
        }
        let block_2 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        println!("Blocks diff: n+{:?}", block_2 - block_1);
        println!("Time taken: {:?}", time_start.elapsed());
        Ok(sig)
    }

    pub async fn spray_with_jito_bundle(
        &self,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: u32,
        clients: &[JitoClient],
        tip_lamports: u64,
    ) -> anyhow::Result<Signature> {
        let start = std::time::Instant::now();

        ixs.insert(0, ix_set_compute_unit_price(fee));
        ixs.insert(1, ix_set_compute_unit_limit(compute_budget));

        let tip_acc = clients[0].get_tip_account();
        ixs.insert(
            2,
            system_instruction_if::transfer(&payer.pubkey(), &tip_acc, tip_lamports),
        );

        let (blockhash, _) = self
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?;
        let slot0 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        let msg = V0Message::try_compile(&payer.pubkey(), &ixs, &[], blockhash)?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[payer])?;
        let sig = tx.signatures[0];

        let b64 = {
            let wire = bincode::serialize(&tx)?;
            B64.encode(wire)
        };

        // Fire-and-forget to all regions.
        let reqs: Vec<_> = clients.iter().map(|c| c.send_bundle(&[&b64])).collect();
        self.spawn_forget(reqs).await;

        // Confirm fast.
        let mut tries = 0u32;
        while !self
            .rpc_client
            .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
            .await?
            .value
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tries += 1;
            if tries > 300 {
                anyhow::bail!("Jito bundle spray: timeout https://solscan.io/tx/{}", sig);
            }
        }
        let slot1 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        println!("Blocks diff: n+{}", slot1.saturating_sub(slot0));
        let end = std::time::Instant::now();
        println!("Time taken: {:?}", end.duration_since(start));
        Ok(sig)
    }

    /// NextBlock.io
    /// Accuracy: n+1/n+2
    pub async fn spray_with_nextblock(
        &self,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: u32,
        clients: &Vec<NextBlock>,
    ) -> anyhow::Result<Signature> {
        let start = std::time::Instant::now();

        // CU budget + price
        ixs.insert(0, ix_set_compute_unit_price(fee));
        ixs.insert(1, ix_set_compute_unit_limit(compute_budget));

        let tip_acc = clients[0].get_tip_account();
        let tip_ix = system_instruction_if::transfer(&payer.pubkey(), &tip_acc, 1_000_000);
        ixs.insert(2, tip_ix);

        let blockhash = self
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?
            .0;
        let slot0 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        let msg = V0Message::try_compile(&payer.pubkey(), &ixs, &[], blockhash)?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[payer])?;
        let sig = tx.signatures[0];

        let wire = bincode::serialize(&tx)?;
        let b64 = B64.encode(wire);

        let per_call = std::time::Duration::from_millis(250);
        let overall_nb = std::time::Duration::from_millis(350);

        let reqs: Vec<_> = clients.iter().map(|c| c.send_transaction(&b64)).collect();

        let nb_fut = async {
            let futures = reqs
                .into_iter()
                .map(|rb| tokio::time::timeout(per_call, rb.send()));
            let all = futures::future::join_all(futures);
            let _ = tokio::time::timeout(overall_nb, all).await;
        };

        let rpc_fut = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            self.rpc_client.send_transaction_with_config(
                &tx,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    preflight_commitment: None,
                    encoding: None,
                    max_retries: Some(0),
                    min_context_slot: None,
                },
            ),
        );

        let _ = tokio::join!(nb_fut, rpc_fut);

        // Fast confirm loop
        let mut tries = 0u32;
        while !self
            .rpc_client
            .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
            .await?
            .value
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tries += 1;
            if tries > 300 {
                anyhow::bail!(
                    "NB spray: transaction timed out https://solscan.io/tx/{}",
                    sig
                );
            }
        }

        let slot1 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        println!("Blocks diff: n+{}", slot1.saturating_sub(slot0));
        println!("Time taken: {:?}", start.elapsed());
        Ok(sig)
    }

    pub async fn spray_with_helius(
        &self,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: u32,
        clients: &Vec<HeliusSender>,
    ) -> anyhow::Result<Signature> {
        let start = std::time::Instant::now();

        ixs.insert(0, ix_set_compute_unit_price(fee));
        ixs.insert(1, ix_set_compute_unit_limit(compute_budget));

        let tip_acc = clients[0].get_tip_account();
        let tip_ix = system_instruction_if::transfer(&payer.pubkey(), &tip_acc, 500_000);
        ixs.insert(2, tip_ix);
        let blockhash = self
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?
            .0;
        let slot0 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        let msg = V0Message::try_compile(&payer.pubkey(), &ixs, &[], blockhash)?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[payer])?;
        let sig = tx.signatures[0];

        let wire = bincode::serialize(&tx)?;
        let b64 = B64.encode(wire);

        let per_call = std::time::Duration::from_millis(250);
        let overall_nb = std::time::Duration::from_millis(350);

        let reqs: Vec<_> = clients
            .iter()
            .map(|c| c.send_transaction(&b64, true))
            .collect();

        let hl_fut = async {
            let futures = reqs
                .into_iter()
                .map(|rb| tokio::time::timeout(per_call, rb.send()));
            let all = futures::future::join_all(futures);
            let _ = tokio::time::timeout(overall_nb, all).await;
        };

        let rpc_fut = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            self.rpc_client.send_transaction_with_config(
                &tx,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    preflight_commitment: None,
                    encoding: None,
                    max_retries: Some(0),
                    min_context_slot: None,
                },
            ),
        );

        let _ = tokio::join!(hl_fut, rpc_fut);

        let mut tries = 0u32;
        while !self
            .rpc_client
            .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
            .await?
            .value
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tries += 1;
            if tries > 300 {
                anyhow::bail!(
                    "NB spray: transaction timed out https://solscan.io/tx/{}",
                    sig
                );
            }
        }

        let slot1 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        println!("Blocks diff: n+{}", slot1.saturating_sub(slot0));
        println!("Time taken: {:?}", start.elapsed());
        Ok(sig)
    }

    pub async fn spray_with_zero_slot(
        &self,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: u32,
        clients: &Vec<ZeroSlot>,
    ) -> anyhow::Result<Signature> {
        let start = std::time::Instant::now();

        let tip_acc = clients[0].get_tip_account();
        let tip_ix = system_instruction_if::transfer(&payer.pubkey(), &tip_acc, 3_200_000);
        ixs.insert(0, tip_ix);

        ixs.insert(1, ix_set_compute_unit_price(fee));
        ixs.insert(2, ix_set_compute_unit_limit(compute_budget));

        let blockhash = self
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?
            .0;
        let slot0 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        let msg = V0Message::try_compile(&payer.pubkey(), &ixs, &[], blockhash)?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[payer])?;
        let sig = tx.signatures[0];

        let wire = bincode::serialize(&tx)?;
        let b64 = B64.encode(wire);

        let per_call = std::time::Duration::from_millis(250);
        let overall_nb = std::time::Duration::from_millis(350);

        let reqs: Vec<_> = clients.iter().map(|c| c.send_transaction(&b64)).collect();

        let zs_fut = async {
            let futures = reqs
                .into_iter()
                .map(|rb| tokio::time::timeout(per_call, rb.send()));
            let all = futures::future::join_all(futures);
            let _ = tokio::time::timeout(overall_nb, all).await;
        };

        let _ = tokio::join!(zs_fut);

        let mut tries = 0u32;
        while !self
            .rpc_client
            .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
            .await?
            .value
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tries += 1;
            if tries > 300 {
                anyhow::bail!(
                    "ZeroSlot spray: transaction timed out https://solscan.io/tx/{}",
                    sig
                );
            }
        }

        let slot1 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        println!("Blocks diff: n+{}", slot1.saturating_sub(slot0));
        println!("Time taken: {:?}", start.elapsed());
        Ok(sig)
    }

    pub async fn spray_with_temporal(
        &self,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: u32,
        clients: &Vec<TemporalSender>,
        tip_lamports: u64,
    ) -> anyhow::Result<Signature> {
        let start = std::time::Instant::now();
        ixs.insert(1, ix_set_compute_unit_price(fee));
        ixs.insert(2, ix_set_compute_unit_limit(compute_budget));

        let tip_acc = clients[0].get_tip_account();
        let tip_ix = system_instruction_if::transfer(&payer.pubkey(), &tip_acc, tip_lamports);
        ixs.insert(0, tip_ix);

        let blockhash = self
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?
            .0;

        let slot0 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;

        let msg = V0Message::try_compile(&payer.pubkey(), &ixs, &[], blockhash)?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[payer])?;
        let sig = tx.signatures[0];

        let wire = bincode::serialize(&tx)?;
        let b64 = B64.encode(wire);
        let per_call = std::time::Duration::from_millis(250);
        let overall_to = std::time::Duration::from_millis(350);

        let reqs: Vec<_> = clients.iter().map(|c| c.send_transaction(&b64)).collect();
        let tmp_fut = async {
            let futures = reqs
                .into_iter()
                .map(|rb| tokio::time::timeout(per_call, rb.send()));
            let all = futures::future::join_all(futures);
            let _ = tokio::time::timeout(overall_to, all).await;
        };

        let _ = tokio::join!(tmp_fut);

        let mut tries = 0u32;
        while !self
            .rpc_client
            .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
            .await?
            .value
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tries += 1;
            if tries > 300 {
                anyhow::bail!(
                    "Temporal spray: transaction timed out https://solscan.io/tx/{}",
                    sig
                );
            }
        }

        let slot1 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        println!("Blocks diff: n+{}", slot1.saturating_sub(slot0));
        println!("Time taken: {:?}", start.elapsed());
        Ok(sig)
    }

    pub async fn spray_with_blox(
        &self,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        compute_budget: u32,
        clients: &Vec<Bloxroute>,
        tip_lamports: u64,
    ) -> anyhow::Result<Signature> {
        let start = std::time::Instant::now();
        ixs.insert(1, ix_set_compute_unit_price(fee));
        ixs.insert(2, ix_set_compute_unit_limit(compute_budget));

        let tip_acc = clients[0].get_tip_acc();
        let tip_ix = system_instruction_if::transfer(&payer.pubkey(), &tip_acc, tip_lamports);
        ixs.insert(0, tip_ix);

        let blockhash = self
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?
            .0;

        let slot0 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;

        let msg = V0Message::try_compile(&payer.pubkey(), &ixs, &[], blockhash)?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[payer])?;
        let sig = tx.signatures[0];

        let wire = bincode::serialize(&tx)?;
        let b64 = B64.encode(wire);

        let reqs: Vec<_> = clients
            .iter()
            .map(|c| {
                c.submit(
                    &b64,
                    &SubmitOpts {
                        skip_preflight: Some(true),
                        front_running_protection: Some(false),
                        submit_protection: Some(SubmitProtection::Low),
                        fast_best_effort: Some(false),
                        use_staked_rpcs: Some(true),
                        allow_back_run: Some(false),
                        revenue_address: Some(String::new()),
                    },
                )
            })
            .collect();
        self.spawn_forget(reqs).await;

        let mut tries = 0u32;
        while !self
            .rpc_client
            .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
            .await?
            .value
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tries += 1;
            if tries > 300 {
                anyhow::bail!(
                    "Blox spray: transaction timed out https://solscan.io/tx/{}",
                    sig
                );
            }
        }

        let slot1 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        println!("Blocks diff: n+{}", slot1.saturating_sub(slot0));
        println!("Time taken: {:?}", start.elapsed());
        Ok(sig)
    }

    pub async fn spray_with_all(
        &self,
        mut ixs: Vec<Instruction>,
        payer: &Keypair,
        fee: u64,
        nb_clients: &[NextBlock],
        jito_clients: &[JitoClient],
        helius_clients: &[HeliusSender],
        zs_clients: &[ZeroSlot],
        temporal_clients: &[TemporalSender],
        blox_clients: &[Bloxroute],
        tip_lamports: u64,
        slot0: Option<u64>,
        nonce_account: Option<Pubkey>,
    ) -> anyhow::Result<Signature> {
        let start = std::time::Instant::now();

        let (nonce_hash, _) = match nonce_account {
            Some(nonce_account) => self.get_nonce_data(&nonce_account).await?,
            None => (
                self.rpc_client
                    .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
                    .await?
                    .0,
                Pubkey::default(),
            ),
        };
        if let Some(nonce_addr) = nonce_account {
            ixs.insert(
                0,
                system_instruction_if::advance_nonce_account(&nonce_addr, &payer.pubkey()),
            );
        }

        let blockhash = nonce_hash;
        let slot0 = match slot0 {
            Some(slot) => slot,
            None => {
                self.rpc_client
                    .get_slot_with_commitment(CommitmentConfig::processed())
                    .await?
            }
        };

        let mut ixs_hl = ixs.clone();
        ixs_hl.insert(
            1,
            ix_set_compute_unit_price(fee * rand::random_range(0.7..1.3) as u64),
        );
        let hl_tip_acc =
            helius_clients[rand::random_range(0..helius_clients.len())].get_tip_account();
        ixs_hl.push(system_instruction_if::transfer(
            &payer.pubkey(),
            &hl_tip_acc,
            tip_lamports,
        ));
        let hl_msg = V0Message::try_compile(&payer.pubkey(), &ixs_hl, &[], blockhash)?;
        let hl_tx = VersionedTransaction::try_new(VersionedMessage::V0(hl_msg), &[payer])?;
        let hl_sig = hl_tx.signatures[0];
        let hl_b64 = {
            let wire = bincode::serialize(&hl_tx)?;
            B64.encode(wire)
        };

        let mut ixs_zs = ixs.clone();
        ixs_zs.insert(
            1,
            ix_set_compute_unit_price(fee * rand::random_range(0.7..1.3) as u64),
        );
        let zs_tip_acc = zs_clients[rand::random_range(0..zs_clients.len())].get_tip_account();
        ixs_zs.insert(
            2,
            system_instruction_if::transfer(&payer.pubkey(), &zs_tip_acc, tip_lamports),
        );
        let zs_msg = V0Message::try_compile(&payer.pubkey(), &ixs_zs, &[], blockhash)?;
        let zs_tx = VersionedTransaction::try_new(VersionedMessage::V0(zs_msg), &[payer])?;
        let zs_sig = zs_tx.signatures[0];
        let zs_b64 = {
            let wire = bincode::serialize(&zs_tx)?;
            B64.encode(wire)
        };

        let mut ixs_jito = ixs.clone();
        ixs_jito.insert(
            1,
            ix_set_compute_unit_price(fee * rand::random_range(0.7..1.3) as u64),
        );
        let jito_rta = jito_clients[0].get_tip_account();
        ixs_jito.push(system_instruction_if::transfer(
            &payer.pubkey(),
            &jito_rta,
            tip_lamports,
        ));
        let jito_msg = V0Message::try_compile(&payer.pubkey(), &ixs_jito, &[], blockhash)?;
        let jito_tx = VersionedTransaction::try_new(VersionedMessage::V0(jito_msg), &[payer])?;
        let jito_sig = jito_tx.signatures[0];
        let jito_b64 = {
            let wire = bincode::serialize(&jito_tx)?;
            B64.encode(wire)
        };

        let mut ixs_nb = ixs.clone();
        ixs_nb.insert(
            1,
            ix_set_compute_unit_price(fee * rand::random_range(0.7..1.3) as u64),
        );
        let nb_tip_acc = nb_clients[rand::random_range(0..nb_clients.len())].get_tip_account();
        ixs_nb.push(system_instruction_if::transfer(
            &payer.pubkey(),
            &nb_tip_acc,
            tip_lamports,
        ));
        let nb_msg = V0Message::try_compile(&payer.pubkey(), &ixs_nb, &[], blockhash)?;
        let nb_tx = VersionedTransaction::try_new(VersionedMessage::V0(nb_msg), &[payer])?;
        let nb_sig = nb_tx.signatures[0];
        let nb_b64 = {
            let wire = bincode::serialize(&nb_tx)?;
            B64.encode(wire)
        };

        let mut ixs_tmp = ixs.clone();
        ixs_tmp.insert(
            1,
            ix_set_compute_unit_price(fee * rand::random_range(0.7..1.3) as u64),
        );
        let tmp_tip_acc =
            temporal_clients[rand::random_range(0..temporal_clients.len())].get_tip_account();
        ixs_tmp.push(system_instruction_if::transfer(
            &payer.pubkey(),
            &tmp_tip_acc,
            tip_lamports,
        ));
        let tmp_msg = V0Message::try_compile(&payer.pubkey(), &ixs_tmp, &[], blockhash)?;
        let tmp_tx = VersionedTransaction::try_new(VersionedMessage::V0(tmp_msg), &[payer])?;
        let tmp_sig = tmp_tx.signatures[0];
        let tmp_b64 = {
            let wire = bincode::serialize(&tmp_tx)?;
            B64.encode(wire)
        };

        let mut ixs_bx = ixs.clone();
        ixs_bx.insert(
            1,
            ix_set_compute_unit_price(fee * rand::random_range(0.7..1.3) as u64),
        );
        let bx_tip_acc = blox_clients[rand::random_range(0..blox_clients.len())].get_tip_acc();
        ixs_bx.push(system_instruction_if::transfer(
            &payer.pubkey(),
            &bx_tip_acc,
            tip_lamports,
        ));
        let bx_msg = V0Message::try_compile(&payer.pubkey(), &ixs_bx, &[], blockhash)?;
        let bx_tx = VersionedTransaction::try_new(VersionedMessage::V0(bx_msg), &[payer])?;
        let bx_sig = bx_tx.signatures[0];
        let bx_b64 = {
            let wire = bincode::serialize(&bx_tx)?;
            B64.encode(wire)
        };

        let bx_fut = async {
            let reqs: Vec<_> = blox_clients
                .iter()
                .map(|c| {
                    c.submit(
                        &bx_b64,
                        &SubmitOpts {
                            skip_preflight: Some(true),
                            front_running_protection: Some(false),
                            submit_protection: Some(SubmitProtection::Low),
                            fast_best_effort: Some(false),
                            use_staked_rpcs: Some(true),
                            allow_back_run: Some(false),
                            revenue_address: Some(String::new()),
                        },
                    )
                })
                .collect();
            self.spawn_forget(reqs).await;
        };

        let hl_fut = async {
            let reqs: Vec<_> = helius_clients
                .iter()
                .map(|c| c.send_transaction(&hl_b64, false))
                .collect();
            self.spawn_forget(reqs).await;
        };

        let zs_fut = async {
            let reqs: Vec<_> = zs_clients
                .iter()
                .map(|c| c.send_transaction(&zs_b64))
                .collect();
            self.spawn_forget(reqs).await;
        };

        let be_fut = async {
            let reqs: Vec<_> = jito_clients
                .iter()
                .map(|c| c.send_transaction(&jito_b64, true))
                .collect();
            self.spawn_forget(reqs).await;
        };

        let nb_fut = async {
            let reqs: Vec<_> = nb_clients
                .iter()
                .map(|c| c.send_transaction(&nb_b64))
                .collect();
            self.spawn_forget(reqs).await;
        };

        let tmp_fut = async {
            let reqs: Vec<_> = temporal_clients
                .iter()
                .map(|c| c.send_transaction(&tmp_b64))
                .collect();
            self.spawn_forget(reqs).await;
        };

        let _ = tokio::join!(zs_fut, tmp_fut, hl_fut, nb_fut, be_fut, bx_fut);
        log!(cc::LIGHT_WHITE, "Time to send: {:?}", start.elapsed());
        log!(
            cc::LIGHT_WHITE,
            "ZeroSlot sig:  https://solscan.io/tx/{}",
            zs_sig
        );
        log!(
            cc::LIGHT_WHITE,
            "Jito sig:      https://solscan.io/tx/{}",
            jito_sig
        );
        log!(
            cc::LIGHT_WHITE,
            "NextBlock sig: https://solscan.io/tx/{}",
            nb_sig
        );
        log!(
            cc::LIGHT_WHITE,
            "Helius sig:    https://solscan.io/tx/{}",
            hl_sig
        );
        log!(
            cc::LIGHT_WHITE,
            "Temporal sig:  https://solscan.io/tx/{}",
            tmp_sig
        );
        log!(
            cc::LIGHT_WHITE,
            "Blox sig:      https://solscan.io/tx/{}",
            bx_sig
        );
        let mut tries = 0u32;
        let winner = loop {
            if self
                .rpc_client
                .confirm_transaction_with_commitment(&jito_sig, CommitmentConfig::confirmed())
                .await?
                .value
            {
                break jito_sig;
            }
            if self
                .rpc_client
                .confirm_transaction_with_commitment(&nb_sig, CommitmentConfig::confirmed())
                .await?
                .value
            {
                break nb_sig;
            }
            if self
                .rpc_client
                .confirm_transaction_with_commitment(&zs_sig, CommitmentConfig::confirmed())
                .await?
                .value
            {
                break zs_sig;
            }
            if self
                .rpc_client
                .confirm_transaction_with_commitment(&hl_sig, CommitmentConfig::confirmed())
                .await?
                .value
            {
                break hl_sig;
            }
            if self
                .rpc_client
                .confirm_transaction_with_commitment(&tmp_sig, CommitmentConfig::confirmed())
                .await?
                .value
            {
                break tmp_sig;
            }
            if self
                .rpc_client
                .confirm_transaction_with_commitment(&bx_sig, CommitmentConfig::confirmed())
                .await?
                .value
            {
                break bx_sig;
            }
            tries += 1;
            if tries > 300 {
                anyhow::bail!(
                    "spray_with_all timeout; jito={}, nb={}, zs={}",
                    jito_sig,
                    nb_sig,
                    zs_sig
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        let slot1 = self
            .rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;
        log!("Blocks diff: n+{}", slot1.saturating_sub(slot0));
        log!("Time taken: {:?}", start.elapsed());

        Ok(winner)
    }

    pub async fn spawn_forget(&self, reqs: Vec<reqwest::RequestBuilder>) {
        for rb in reqs {
            tokio::spawn(async move {
                let _ = rb.send().await;
            });
        }
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn restore_env_var(name: &str, value: Option<String>) {
        if let Some(value) = value {
            unsafe { std::env::set_var(name, value) };
        } else {
            unsafe { std::env::remove_var(name) };
        }
    }

    #[test]
    fn test_readonly_fallback_rpc_url_prefers_second_configured_http_url() {
        let _guard = env_lock().lock().expect("env lock poisoned");
        let previous_urls = std::env::var("MAMBA_API_HTTP_URLS").ok();

        unsafe {
            std::env::set_var(
                "MAMBA_API_HTTP_URLS",
                "https://primary.example/?api-key=secret, https://secondary.example/rpc",
            );
        }
        let sol = SolHook::new("https://primary.example/?api-key=secret".to_string());
        assert_eq!(
            sol.readonly_fallback_rpc_url().as_deref(),
            Some("https://secondary.example/rpc")
        );

        restore_env_var("MAMBA_API_HTTP_URLS", previous_urls);
    }

    #[test]
    fn test_readonly_fallback_rpc_url_skips_same_host_variants() {
        let _guard = env_lock().lock().expect("env lock poisoned");
        let previous_urls = std::env::var("MAMBA_API_HTTP_URLS").ok();

        unsafe {
            std::env::set_var(
                "MAMBA_API_HTTP_URLS",
                "https://mainnet.helius-rpc.com/?api-key=one, https://mainnet.helius-rpc.com/?api-key=two",
            );
        }
        let sol = SolHook::new("https://mainnet.helius-rpc.com/?api-key=one".to_string());
        assert_eq!(
            sol.readonly_fallback_rpc_url().as_deref(),
            Some(DEFAULT_MAINNET_HTTP_URL)
        );

        restore_env_var("MAMBA_API_HTTP_URLS", previous_urls);
    }

    #[test]
    fn test_rpc_url_label_strips_query_string() {
        assert_eq!(
            SolHook::rpc_url_label("https://mainnet.helius-rpc.com/?api-key=secret"),
            "https://mainnet.helius-rpc.com/"
        );
    }

    #[test]
    fn test_send_confirm_timeout_duration_uses_env_override() {
        let _guard = env_lock().lock().expect("env lock poisoned");
        let previous = std::env::var(SEND_CONFIRM_TIMEOUT_ENV).ok();

        unsafe { std::env::set_var(SEND_CONFIRM_TIMEOUT_ENV, "17") };
        assert_eq!(
            SolHook::send_confirm_timeout_duration(),
            Duration::from_secs(17)
        );

        restore_env_var(SEND_CONFIRM_TIMEOUT_ENV, previous);
    }
}
