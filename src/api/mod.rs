use {
    crate::core::{
        sol::{
            DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, METADATA_PROGRAM_ID, PriorityFeeLevel,
            PriorityFeeOverride, SolHook, WSOL_MINT,
        },
        wallet::{ManagedWalletRuntime, wallet_store_path},
    },
    crate::dex::{
        pump_fun::PumpFun,
        pump_swap::PumpSwap,
        swaps::{
            CreatorResolutionSource, DEFAULT_MARKET_PRIORITY, Market, MintCreatorRoute,
            MintCreatorRouteSelection, RouteLiquiditySnapshot, Swaps,
        },
    },
    crate::handlers::ws::{MigrationConfidence, MigrationEvent, Mint, WsHandler},
    crate::swqos::{SWQoSettings, SwqosProvider},
    crate::warn,
    anyhow::Context,
    axum::{
        Json, Router,
        body::Body,
        extract::{
            Path, Query, State,
            connect_info::ConnectInfo,
            ws::{Message, WebSocket, WebSocketUpgrade},
        },
        http::{Request, StatusCode},
        middleware::{self, Next},
        response::{IntoResponse, Response},
        routing::{get, post},
    },
    base64::Engine,
    mpl_token_metadata::accounts::Metadata as MplMetadata,
    serde::{Deserialize, Serialize},
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_keypair::Keypair,
    solana_program::pubkey::Pubkey,
    solana_signature::Signature,
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    sqlx::{PgPool, Row},
    std::{
        cmp::Ordering,
        collections::{HashMap, HashSet},
        net::{IpAddr, SocketAddr},
        path::PathBuf,
        str::FromStr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
        },
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
    tokio::sync::Mutex,
};

use chrono::{DateTime, Utc};

mod create;
mod pool;
mod wallet;

const DEFAULT_API_BIND_ADDR: &str = "127.0.0.1:8787";
const DEFAULT_ROUTE_BASE: &str = "/mamba-api";
const DEFAULT_MIN_LIQUIDITY_RAW: u64 = 1;
const DEFAULT_MINT_LIMIT: usize = 100;
const MAX_MINT_LIMIT: usize = 500;
const DEFAULT_WS_STREAM_INTERVAL_MS: u64 = 700;
const DEFAULT_WS_STREAM_LIMIT: usize = 150;
const MAX_WS_STREAM_LIMIT: usize = 500;
const MAX_MINT_METADATA_BATCH: usize = 100;
const SCORE_WEIGHT_MINTS: f64 = 0.45;
const SCORE_WEIGHT_MARKET_CAP: f64 = 0.4;
const SCORE_WEIGHT_VOLUME: f64 = 0.15;

struct MarketSubscription {
    handler: Arc<WsHandler>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub struct ApiState {
    swaps: Arc<Swaps>,
    ws_urls: Arc<Vec<String>>,
    rpc_urls: Arc<Vec<String>>,
    rpc_clients: Arc<Vec<Arc<RpcClient>>>,
    rpc_cluster_cache: Arc<Mutex<HashMap<String, crate::core::cluster::SolanaCluster>>>,
    endpoint_cursor: Arc<AtomicUsize>,
    cluster: crate::core::cluster::SolanaCluster,
    store: Option<Arc<ApiStore>>,
    subscriptions: Arc<Mutex<HashMap<Market, MarketSubscription>>>,
    api_key: String,
    allow_live_sends: bool,
    allow_private_network_clients: bool,
    signer_configured: bool,
    wallet_store_path: Option<PathBuf>,
}

#[derive(Clone)]
struct ApiConfig {
    bind_addr: SocketAddr,
    route_base: String,
    api_key: String,
    ws_urls: Vec<String>,
    rpc_urls: Vec<String>,
    allow_live_sends: bool,
    allow_private_network_clients: bool,
    store_mode: bool,
    database_url: Option<String>,
    signer_raw: Option<String>,
}

impl ApiConfig {
    fn from_env() -> anyhow::Result<Self> {
        dotenv::dotenv().ok();

        let bind_addr = std::env::var("MAMBA_API_BIND_ADDR")
            .ok()
            .unwrap_or_else(|| DEFAULT_API_BIND_ADDR.to_string());
        let bind_addr = SocketAddr::from_str(bind_addr.trim())
            .with_context(|| "MAMBA_API_BIND_ADDR must be a valid socket address")?;
        let allow_private_network_clients =
            parse_env_bool("MAMBA_API_ALLOW_PRIVATE_NETWORK_CLIENTS", false);
        if !allow_private_network_clients {
            anyhow::ensure!(
                bind_addr.ip().is_loopback(),
                "MAMBA_API_BIND_ADDR must be loopback-only unless MAMBA_API_ALLOW_PRIVATE_NETWORK_CLIENTS=true"
            );
        }

        let route_base = std::env::var("MAMBA_API_ROUTE_BASE")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_ROUTE_BASE.to_string());

        let api_key = std::env::var("MAMBA_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .context("missing MAMBA_API_KEY; required for secure local API authentication")?;

        let mut ws_urls = parse_env_url_list(&["MAMBA_API_WS_URLS"]);
        if ws_urls.is_empty() {
            ws_urls = vec![crate::core::cluster::DEFAULT_DEVNET_WS_URL.to_string()];
        }

        let mut rpc_urls = parse_env_url_list(&["MAMBA_API_HTTP_URLS"]);
        if rpc_urls.is_empty() {
            rpc_urls = vec![crate::core::cluster::DEFAULT_DEVNET_HTTP_URL.to_string()];
        }

        let allow_live_sends = parse_env_bool("MAMBA_API_ENABLE_LIVE_SENDS", false);
        let store_mode = parse_env_bool("MAMBA_API_STORE_MODE", false);
        let database_url = std::env::var("MAMBA_API_DATABASE_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if store_mode {
            anyhow::ensure!(
                database_url.is_some(),
                "MAMBA_API_STORE_MODE=true requires MAMBA_API_DATABASE_URL"
            );
        }

        let signer_raw = std::env::var("MAMBA_PRIVATE_KEY")
            .ok()
            .or_else(|| std::env::var("MAMBA_API_PRIVATE_KEY").ok())
            .or_else(|| std::env::var("PRIVATE_KEY").ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        Ok(Self {
            bind_addr,
            route_base,
            api_key,
            ws_urls,
            rpc_urls,
            allow_live_sends,
            allow_private_network_clients,
            store_mode,
            database_url,
            signer_raw,
        })
    }
}

pub async fn run_from_env() -> anyhow::Result<()> {
    let config = ApiConfig::from_env()?;
    let state = Arc::new(build_state(&config).await?);

    let api_routes = Router::new()
        .route("/health", get(get_health))
        .route("/docs", get(get_docs))
        .route("/markets", get(get_markets))
        .route("/swap", post(post_swap))
        .route("/create/methods", get(create::get_methods))
        .route("/create/build", post(create::post_build))
        .route("/create/execute", post(create::post_execute))
        .route(
            "/wallets",
            get(wallet::get_wallets).post(wallet::post_create_wallet),
        )
        .route("/wallets/active", get(wallet::get_active_wallet))
        .route("/wallets/{wallet}/balance", get(wallet::get_wallet_balance))
        .route("/wallets/select", post(wallet::post_select_wallets))
        .route("/wallets/transfer/build", post(wallet::post_transfer_build))
        .route(
            "/wallets/transfer/execute",
            post(wallet::post_transfer_execute),
        )
        .route("/wallets/clean/preview", get(wallet::get_clean_preview))
        .route("/wallets/clean/build", post(wallet::post_clean_build))
        .route("/wallets/clean/execute", post(wallet::post_clean_execute))
        .route("/pool/methods", get(pool::get_methods))
        .route("/pool/build", post(pool::post_build))
        .route("/pool/execute", post(pool::post_execute))
        .route("/pool/positions", get(pool::get_positions))
        .route("/pool/manage/build", post(pool::post_manage_build))
        .route("/pool/manage/execute", post(pool::post_manage_execute))
        .route(
            "/create/raydium_launchpad/global-configs",
            get(create::get_raydium_launchpad_global_configs),
        )
        .route(
            "/create/raydium_launchpad/platform-configs",
            get(create::get_raydium_launchpad_platform_configs),
        )
        .route(
            "/create/raydium_launchpad/platform-configs/{platform_config}/curve-params",
            get(create::get_raydium_launchpad_platform_curve_params),
        )
        .route("/mints", get(list_cached_mints))
        .route("/mints/{mint}/creator", get(get_mint_creator))
        .route("/mints/{mint}/metadata", get(get_mint_metadata))
        .route("/mints/metadata-batch", post(post_mint_metadata_batch))
        .route("/mints/{mint}/route", get(get_mint_route))
        .route("/ws/subscribe", post(post_ws_subscribe))
        .route("/ws/unsubscribe", post(post_ws_unsubscribe))
        .route("/ws/subscriptions", get(get_ws_subscriptions))
        .route("/ws/stream", get(get_ws_stream))
        .route("/creators", get(get_creators))
        .route("/creator-mints", get(get_creator_mints))
        .route("/transactions", get(get_transactions));

    let secure_app = Router::new()
        .nest(&format!("{}/v1", config.route_base), api_routes.clone())
        .nest(&config.route_base, api_routes)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            secure_local_middleware,
        ));

    let app = Router::new().merge(secure_app).with_state(state);

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    println!(
        "mamba api (authenticated) listening on http://{}{} (x-api-key required)",
        config.bind_addr, config.route_base
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

async fn build_state(config: &ApiConfig) -> anyhow::Result<ApiState> {
    let (signer, signer_configured) = load_keypair(config.signer_raw.as_deref())?;
    let rpc_clients = Arc::new(
        config
            .rpc_urls
            .iter()
            .map(|url| Arc::new(RpcClient::new(url.clone())))
            .collect::<Vec<_>>(),
    );
    let primary_rpc_client = rpc_clients
        .first()
        .cloned()
        .context("missing primary RPC URL for SolHook")?;
    let mut genesis_hash = None;
    let mut genesis_source = None;
    let mut last_error: Option<anyhow::Error> = None;
    for (idx, rpc) in rpc_clients.iter().enumerate() {
        let url = config
            .rpc_urls
            .get(idx)
            .map(|v| v.as_str())
            .unwrap_or("<unknown>");
        let response = tokio::time::timeout(Duration::from_secs(6), rpc.get_genesis_hash()).await;
        match response {
            Ok(Ok(hash)) => {
                genesis_hash = Some(hash);
                genesis_source = Some(url.to_string());
                break;
            }
            Ok(Err(error)) => {
                let detail = describe_genesis_hash_rpc_failure(url).await;
                let detail = detail
                    .as_deref()
                    .map(|value| format!(" ({value})"))
                    .unwrap_or_default();
                last_error = Some(anyhow::anyhow!("getGenesisHash {url}: {error}{detail}"));
            }
            Err(_) => {
                let detail = describe_genesis_hash_rpc_failure(url).await;
                let detail = detail
                    .as_deref()
                    .map(|value| format!(" ({value})"))
                    .unwrap_or_default();
                last_error = Some(anyhow::anyhow!(
                    "rpc getGenesisHash timed out for {url}{detail}"
                ));
            }
        }
    }

    let genesis_hash = genesis_hash.ok_or_else(|| {
        anyhow::anyhow!(
            "rpc getGenesisHash failed: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown error".to_string())
        )
    })?;
    let cluster = crate::core::cluster::SolanaCluster::from_genesis_hash(&genesis_hash.to_string());
    if let Some(source) = genesis_source {
        println!("rpc cluster detected via getGenesisHash: {cluster:?} ({source})");
    }
    let mut rpc_cluster_cache = HashMap::new();
    for url in &config.rpc_urls {
        if let Some(key) = normalize_rpc_url_key(url) {
            rpc_cluster_cache.insert(key, cluster);
        }
    }
    let sol_hook = SolHook::from_rpc_client_with_cluster(primary_rpc_client.clone(), cluster);

    if signer_configured {
        crate::core::wallet::ensure_wallet_store_has_signer(signer.as_ref(), "main")?;
    }

    let arc_sol_hook = Arc::new(sol_hook.clone());
    let pump_fun = PumpFun::new(signer.clone(), arc_sol_hook.clone());
    let pump_swap = PumpSwap::new(signer, arc_sol_hook);
    let swaps = Arc::new(Swaps::new(sol_hook.clone(), pump_swap, pump_fun));

    let store = if config.store_mode {
        let database_url = config
            .database_url
            .clone()
            .context("store mode enabled without database url")?;
        let store = ApiStore::new(&database_url).await?;
        Some(Arc::new(store))
    } else {
        None
    };

    Ok(ApiState {
        swaps,
        ws_urls: Arc::new(config.ws_urls.clone()),
        rpc_urls: Arc::new(config.rpc_urls.clone()),
        rpc_clients: rpc_clients.clone(),
        rpc_cluster_cache: Arc::new(Mutex::new(rpc_cluster_cache)),
        endpoint_cursor: Arc::new(AtomicUsize::new(0)),
        cluster,
        store,
        subscriptions: Arc::new(Mutex::new(HashMap::new())),
        api_key: config.api_key.clone(),
        allow_live_sends: config.allow_live_sends,
        allow_private_network_clients: config.allow_private_network_clients,
        signer_configured,
        wallet_store_path: wallet_store_path(),
    })
}

#[derive(Debug, Deserialize)]
struct GenesisHashRpcErrorBody {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct GenesisHashRpcResponseBody {
    error: Option<GenesisHashRpcErrorBody>,
    result: Option<String>,
}

async fn describe_genesis_hash_rpc_failure(url: &str) -> Option<String> {
    if url.trim().is_empty() {
        return None;
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .ok()?;
    let response = client
        .post(url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": "mamba-genesis-hash-probe",
            "method": "getGenesisHash",
            "params": [],
        }))
        .send()
        .await
        .ok()?;
    let status = response.status();
    let body = response.text().await.ok()?;
    let body_trimmed = body.trim();
    if body_trimmed.is_empty() {
        return Some(format!("http {status} with empty body"));
    }

    if let Ok(parsed) = serde_json::from_str::<GenesisHashRpcResponseBody>(body_trimmed) {
        if let Some(error) = parsed.error {
            return Some(format!("rpc error {}: {}", error.code, error.message));
        }
        if parsed.result.is_some() {
            return Some("raw JSON-RPC probe succeeded unexpectedly".to_string());
        }
    }

    let truncated = if body_trimmed.chars().count() > 240 {
        format!("{}...", body_trimmed.chars().take(240).collect::<String>())
    } else {
        body_trimmed.to_string()
    };

    Some(format!("http {status}: {truncated}"))
}

fn normalize_rpc_url_key(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.trim_end_matches('/').to_string())
}

fn configured_rpc_index_for_url(state: &ApiState, url: &str) -> Option<usize> {
    let needle = normalize_rpc_url_key(url)?;
    state
        .rpc_urls
        .iter()
        .position(|candidate| normalize_rpc_url_key(candidate).as_deref() == Some(needle.as_str()))
}

async fn cached_cluster_for_rpc_url(
    state: &ApiState,
    url: &str,
) -> Option<crate::core::cluster::SolanaCluster> {
    let key = normalize_rpc_url_key(url)?;
    state.rpc_cluster_cache.lock().await.get(&key).copied()
}

async fn cache_cluster_for_rpc_url(
    state: &ApiState,
    url: &str,
    cluster: crate::core::cluster::SolanaCluster,
) {
    let Some(key) = normalize_rpc_url_key(url) else {
        return;
    };
    state.rpc_cluster_cache.lock().await.insert(key, cluster);
}

fn parse_env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

fn parse_env_url_list(names: &[&str]) -> Vec<String> {
    for name in names {
        let Ok(raw) = std::env::var(name) else {
            continue;
        };

        let mut out = Vec::<String>::new();
        let mut seen = HashSet::<String>::new();

        for token in raw
            .split([',', ';', '\n', '\r', ' ', '\t'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let value = token
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            if value.is_empty() {
                continue;
            }
            if seen.insert(value.clone()) {
                out.push(value);
            }
        }

        if !out.is_empty() {
            return out;
        }
    }

    Vec::new()
}

fn market_endpoint_slot(market: Market, endpoint_count: usize) -> usize {
    if endpoint_count <= 1 {
        return 0;
    }

    let market_slot = match market {
        Market::PumpSwap => 0,
        Market::PumpFun => 1,
        Market::RaydiumAmmV4 => 2,
        Market::RaydiumLaunchpad => 3,
        Market::RaydiumClmm => 4,
        Market::RaydiumCpmm => 5,
        Market::MeteoraDlmm => 6,
        Market::MeteoraDammV1 => 7,
        Market::MeteoraDammV2 => 8,
        Market::MeteoraDbc => 9,
    };

    market_slot % endpoint_count
}

async fn rpc_client_for_attempt(
    state: &ApiState,
    base_offset: usize,
    attempt: usize,
) -> anyhow::Result<Arc<RpcClient>> {
    let rpc_count = state.rpc_clients.len();
    anyhow::ensure!(rpc_count > 0, "no configured RPC clients");
    let index = (base_offset + attempt) % rpc_count;
    Ok(state.rpc_clients[index].clone())
}

async fn resolve_request_rpc(
    state: &ApiState,
    base_offset: usize,
    rpc_url_override: Option<&str>,
) -> Result<(Arc<RpcClient>, crate::core::cluster::SolanaCluster), ApiError> {
    let rpc_url_override = rpc_url_override
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let rpc = if let Some(url) = rpc_url_override {
        if let Some(index) = configured_rpc_index_for_url(state, url) {
            return Ok((state.rpc_clients[index].clone(), state.cluster));
        }
        Arc::new(RpcClient::new(url.to_string()))
    } else {
        rpc_client_for_attempt(state, base_offset, 0)
            .await
            .map_err(|error| ApiError::internal(format!("rpc client selection failed: {error}")))?
    };

    let cluster = if rpc_url_override.is_some() {
        if let Some(url) = rpc_url_override
            && let Some(cluster) = cached_cluster_for_rpc_url(state, url).await
        {
            cluster
        } else {
            match tokio::time::timeout(Duration::from_secs(4), rpc.get_genesis_hash()).await {
                Ok(Ok(genesis_hash)) => {
                    let cluster = crate::core::cluster::SolanaCluster::from_genesis_hash(
                        &genesis_hash.to_string(),
                    );
                    if let Some(url) = rpc_url_override {
                        cache_cluster_for_rpc_url(state, url, cluster).await;
                    }
                    cluster
                }
                Ok(Err(error)) => {
                    if let Some(url) = rpc_url_override
                        && let Some(cluster) = cached_cluster_for_rpc_url(state, url).await
                    {
                        warn!(
                            "reusing cached rpc cluster for override {} after getGenesisHash error: {}",
                            url, error
                        );
                        return Ok((rpc, cluster));
                    }
                    return Err(ApiError::bad_request(format!(
                        "rpc getGenesisHash failed for override rpc_url: {error}"
                    )));
                }
                Err(_) => {
                    if let Some(url) = rpc_url_override
                        && let Some(cluster) = cached_cluster_for_rpc_url(state, url).await
                    {
                        warn!(
                            "reusing cached rpc cluster for override {} after getGenesisHash timeout",
                            url
                        );
                        return Ok((rpc, cluster));
                    }
                    return Err(ApiError::bad_request(
                        "rpc getGenesisHash timed out for override rpc_url",
                    ));
                }
            }
        }
    } else {
        state.cluster
    };

    Ok((rpc, cluster))
}

fn enforce_live_send_cluster_match(
    state: &ApiState,
    rpc_url_override: Option<&str>,
    request_cluster: crate::core::cluster::SolanaCluster,
) -> Result<(), ApiError> {
    let Some(_) = non_empty_optional_str(rpc_url_override) else {
        return Ok(());
    };

    if request_cluster == state.cluster {
        return Ok(());
    }

    Err(ApiError::conflict(format!(
        "live execution rpc_url resolved to {:?}, but this API is configured for {:?}; cross-cluster live sends are not allowed",
        request_cluster, state.cluster
    )))
}

fn load_keypair(raw: Option<&str>) -> anyhow::Result<(Arc<Keypair>, bool)> {
    let Some(raw) = raw else {
        return Ok((Arc::new(Keypair::new()), false));
    };

    let parsed = if raw.starts_with('[') {
        let bytes: Vec<u8> = serde_json::from_str(raw)
            .context("failed to parse API signer key as JSON byte array")?;
        Keypair::try_from(bytes.as_slice())
            .context("API signer JSON must encode 64 keypair bytes")?
    } else {
        let bytes = bs58::decode(raw)
            .into_vec()
            .context("failed to parse API signer key as base58")?;
        Keypair::try_from(bytes.as_slice())
            .context("API signer key base58 must decode to 64 keypair bytes")?
    };

    Ok((Arc::new(parsed), true))
}

fn load_managed_wallet_runtime(state: &ApiState) -> Option<ManagedWalletRuntime> {
    let path = state.wallet_store_path.as_deref()?;
    ManagedWalletRuntime::load(path).ok()
}

fn api_signer_available(state: &ApiState) -> bool {
    state.signer_configured
        || load_managed_wallet_runtime(state)
            .as_ref()
            .and_then(ManagedWalletRuntime::active_signer)
            .is_some()
}

fn configured_api_signer_pubkey(state: &ApiState) -> Option<String> {
    state
        .signer_configured
        .then(|| state.swaps.pump_fun.keypair.pubkey().to_string())
}

fn resolve_local_signer(state: &ApiState, pubkey: &Pubkey) -> Result<Arc<Keypair>, ApiError> {
    if state.signer_configured && state.swaps.pump_fun.keypair.pubkey() == *pubkey {
        return Ok(state.swaps.pump_fun.keypair.clone());
    }

    load_managed_wallet_runtime(state)
        .and_then(|runtime| runtime.signer_for(pubkey))
        .ok_or_else(|| ApiError::bad_request(format!("wallet signer not found locally: {pubkey}")))
}

fn resolve_required_signers(
    state: &ApiState,
    required_signers: &[String],
    generated_signers: &HashMap<Pubkey, Arc<Keypair>>,
) -> Result<Vec<Arc<Keypair>>, ApiError> {
    let mut resolved = Vec::with_capacity(required_signers.len());
    let mut seen = HashSet::new();

    for raw in required_signers {
        let pubkey = Pubkey::from_str(raw.trim())
            .map_err(|_| ApiError::bad_request(format!("invalid required signer pubkey: {raw}")))?;
        if !seen.insert(pubkey) {
            continue;
        }
        if let Some(signer) = generated_signers.get(&pubkey) {
            resolved.push(signer.clone());
            continue;
        }
        resolved.push(resolve_local_signer(state, &pubkey)?);
    }

    Ok(resolved)
}

fn decode_versioned_transaction_base64(encoded: &str) -> Result<VersionedTransaction, ApiError> {
    let wire = base64::prelude::BASE64_STANDARD
        .decode(encoded.trim())
        .map_err(|error| ApiError::internal(format!("decode tx base64 failed: {error}")))?;
    bincode::deserialize(&wire)
        .map_err(|error| ApiError::internal(format!("deserialize tx failed: {error}")))
}

fn sign_versioned_transaction(
    unsigned: &VersionedTransaction,
    signers: &[Arc<Keypair>],
) -> Result<VersionedTransaction, ApiError> {
    let signer_refs = signers
        .iter()
        .map(|signer| signer.as_ref())
        .collect::<Vec<_>>();
    VersionedTransaction::try_new(unsigned.message.clone(), &signer_refs)
        .map_err(|error| ApiError::internal(format!("sign transaction failed: {error}")))
}

async fn submit_signed_transaction(
    rpc: Arc<RpcClient>,
    cluster: crate::core::cluster::SolanaCluster,
    tx: &VersionedTransaction,
    use_swqos: bool,
    swqos_settings: Option<SWQoSettings>,
) -> Result<Signature, ApiError> {
    let sol = SolHook::from_rpc_client_with_cluster(rpc.clone(), cluster);
    if use_swqos {
        let settings = swqos_settings.ok_or_else(|| {
            ApiError::bad_request("swqos_settings is required when use_swqos=true")
        })?;
        return sol
            .submit_signed_via_swqos(tx, &settings)
            .await
            .map_err(ApiError::from);
    }

    sol.submit_signed(tx).await.map_err(ApiError::from)
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left_bytes = left.as_bytes();
    let right_bytes = right.as_bytes();
    let mut diff = left_bytes.len() ^ right_bytes.len();
    let max_len = left_bytes.len().max(right_bytes.len());
    for idx in 0..max_len {
        let l = *left_bytes.get(idx).unwrap_or(&0);
        let r = *right_bytes.get(idx).unwrap_or(&0);
        diff |= (l ^ r) as usize;
    }
    diff == 0
}

fn is_private_network_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => addr.is_private() || addr.is_link_local(),
        IpAddr::V6(addr) => addr.is_unique_local() || addr.is_unicast_link_local(),
    }
}

async fn secure_local_middleware(
    State(state): State<Arc<ApiState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let remote_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect| connect.0.ip());
    let remote_is_allowed = remote_ip
        .map(|ip| {
            ip.is_loopback() || (state.allow_private_network_clients && is_private_network_ip(ip))
        })
        .unwrap_or(false);

    if !remote_is_allowed {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "client IP is not allowed (loopback only by default; set MAMBA_API_ALLOW_PRIVATE_NETWORK_CLIENTS=true for docker/private networks)",
        )
        .into_response();
    }

    let provided_api_key = request
        .headers()
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    if !constant_time_eq(provided_api_key, &state.api_key) {
        return ApiError::new(StatusCode::UNAUTHORIZED, "invalid x-api-key").into_response();
    }

    next.run(request).await
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    cluster: String,
    live_sends_enabled: bool,
    signer_configured: bool,
    api_signer_pubkey: Option<String>,
    wallet_count: usize,
    selected_wallet_count: usize,
    active_wallet_pubkey: Option<String>,
    active_ws_subscriptions: Vec<String>,
    timestamp_unix_ms: u128,
}

async fn get_health(State(state): State<Arc<ApiState>>) -> Result<Json<HealthResponse>, ApiError> {
    let subscriptions = state.subscriptions.lock().await;
    let active_ws_subscriptions = subscriptions
        .iter()
        .filter(|(_, subscription)| !subscription.task.is_finished())
        .map(|(market, _)| market.as_str().to_string())
        .collect::<Vec<_>>();

    let timestamp_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis();
    let wallet_runtime = load_managed_wallet_runtime(state.as_ref());

    Ok(Json(HealthResponse {
        status: "ok",
        cluster: format!("{:?}", state.cluster),
        live_sends_enabled: state.allow_live_sends,
        signer_configured: api_signer_available(state.as_ref()),
        api_signer_pubkey: configured_api_signer_pubkey(state.as_ref()),
        wallet_count: wallet_runtime
            .as_ref()
            .map(ManagedWalletRuntime::wallet_count)
            .unwrap_or(0),
        selected_wallet_count: wallet_runtime
            .as_ref()
            .map(ManagedWalletRuntime::selected_count)
            .unwrap_or(0),
        active_wallet_pubkey: wallet_runtime
            .as_ref()
            .and_then(ManagedWalletRuntime::active_pubkey)
            .map(|value| value.to_string()),
        active_ws_subscriptions,
        timestamp_unix_ms,
    }))
}

#[derive(Serialize)]
struct DocsResponse {
    auth_header: &'static str,
    local_only: bool,
    base_paths: Vec<&'static str>,
    endpoints: Vec<DocsEndpoint>,
}

#[derive(Serialize)]
struct DocsEndpoint {
    method: &'static str,
    path: &'static str,
    description: &'static str,
}

async fn get_docs() -> Json<DocsResponse> {
    Json(DocsResponse {
        auth_header: "x-api-key: <MAMBA_API_KEY>",
        local_only: true,
        base_paths: vec!["/mamba-api", "/mamba-api/v1"],
        endpoints: vec![
            DocsEndpoint {
                method: "GET",
                path: "/health",
                description: "Service health + security mode",
            },
            DocsEndpoint {
                method: "GET",
                path: "/create/methods",
                description: "List token creation builder methods (build-only, no send)",
            },
            DocsEndpoint {
                method: "POST",
                path: "/create/build",
                description: "Build token creation transaction (unsigned base64)",
            },
            DocsEndpoint {
                method: "POST",
                path: "/create/execute",
                description: "Build, locally sign, and submit a token creation transaction using the configured Mamba signer or managed wallet",
            },
            DocsEndpoint {
                method: "GET",
                path: "/wallets",
                description: "List locally stored wallet metadata (never returns private keys)",
            },
            DocsEndpoint {
                method: "GET",
                path: "/wallets/active",
                description: "Return the active managed wallet with its live SOL balance",
            },
            DocsEndpoint {
                method: "GET",
                path: "/wallets/{wallet}/balance",
                description: "Return the live SOL balance for any wallet pubkey, plus managed-wallet flags when known",
            },
            DocsEndpoint {
                method: "POST",
                path: "/wallets",
                description: "Generate a new locally stored wallet and return its public metadata",
            },
            DocsEndpoint {
                method: "POST",
                path: "/wallets/select",
                description: "Update the managed-wallet active set (active_wallet kept as a compatibility alias)",
            },
            DocsEndpoint {
                method: "POST",
                path: "/wallets/transfer/build",
                description: "Build an unsigned SOL or token transfer from a stored wallet to any destination pubkey",
            },
            DocsEndpoint {
                method: "POST",
                path: "/wallets/transfer/execute",
                description: "Build, locally sign, and submit a SOL or token transfer from a stored wallet",
            },
            DocsEndpoint {
                method: "GET",
                path: "/wallets/clean/preview",
                description: "Preview cleanable token accounts for one wallet: close empty ATAs, unwrap WSOL, optionally burn+close non-zero balances",
            },
            DocsEndpoint {
                method: "POST",
                path: "/wallets/clean/build",
                description: "Build one or more unsigned wallet-cleaner transactions for the requested wallet/selection",
            },
            DocsEndpoint {
                method: "POST",
                path: "/wallets/clean/execute",
                description: "Build, locally sign, and submit wallet-cleaner transactions for the requested wallet/selection",
            },
            DocsEndpoint {
                method: "GET",
                path: "/pool/methods",
                description: "List pool creation builder markets (build-only, no send)",
            },
            DocsEndpoint {
                method: "POST",
                path: "/pool/build",
                description: "Build pool creation transaction (unsigned base64)",
            },
            DocsEndpoint {
                method: "POST",
                path: "/pool/execute",
                description: "Build, locally sign, and submit a pool creation transaction using the configured Mamba signer or managed wallet",
            },
            DocsEndpoint {
                method: "GET",
                path: "/pool/positions",
                description: "List wallet-owned pool positions with withdraw support and estimates",
            },
            DocsEndpoint {
                method: "POST",
                path: "/pool/manage/build",
                description: "Build pool withdrawal transaction for supported markets",
            },
            DocsEndpoint {
                method: "POST",
                path: "/pool/manage/execute",
                description: "Build, locally sign, and submit a pool withdrawal transaction for supported markets",
            },
            DocsEndpoint {
                method: "GET",
                path: "/create/raydium_launchpad/global-configs",
                description: "List Raydium Launchpad global configs (required for create)",
            },
            DocsEndpoint {
                method: "GET",
                path: "/create/raydium_launchpad/platform-configs",
                description: "List Raydium Launchpad platform configs (required for create)",
            },
            DocsEndpoint {
                method: "GET",
                path: "/create/raydium_launchpad/platform-configs/{platform_config}/curve-params",
                description: "List Raydium Launchpad curve params for one platform config, optionally filtered by global config",
            },
            DocsEndpoint {
                method: "POST",
                path: "/swap",
                description: "Dry-run swap planning and optional live execution",
            },
            DocsEndpoint {
                method: "GET",
                path: "/mints",
                description: "List cached websocket mints (supports market filters)",
            },
            DocsEndpoint {
                method: "GET",
                path: "/mints/{mint}/route",
                description: "Resolve market/pool/price route for mint",
            },
            DocsEndpoint {
                method: "GET",
                path: "/mints/{mint}/creator",
                description: "Resolve true first creator (metadata-first)",
            },
            DocsEndpoint {
                method: "GET",
                path: "/mints/{mint}/metadata",
                description: "Resolve canonical on-chain metadata (name/symbol/uri)",
            },
            DocsEndpoint {
                method: "POST",
                path: "/mints/metadata-batch",
                description: "Batch resolve canonical on-chain metadata for up to 100 mints",
            },
            DocsEndpoint {
                method: "POST",
                path: "/ws/subscribe",
                description: "Start websocket market subscription",
            },
            DocsEndpoint {
                method: "POST",
                path: "/ws/unsubscribe",
                description: "Stop websocket market subscription",
            },
            DocsEndpoint {
                method: "GET",
                path: "/ws/subscriptions",
                description: "List active websocket subscriptions",
            },
            DocsEndpoint {
                method: "GET",
                path: "/ws/stream",
                description: "Websocket stream of filtered mint snapshots",
            },
            DocsEndpoint {
                method: "GET",
                path: "/transactions",
                description: "List stored API swap transactions (store mode)",
            },
            DocsEndpoint {
                method: "GET",
                path: "/creators",
                description: "Creator leaderboard with normalized score (store mode)",
            },
            DocsEndpoint {
                method: "GET",
                path: "/creator-mints",
                description: "List mints for a creator with per-mint activity stats",
            },
        ],
    })
}

#[derive(Serialize)]
struct MarketsResponse {
    markets: Vec<&'static str>,
}

async fn get_markets() -> Json<MarketsResponse> {
    Json(MarketsResponse {
        markets: Swaps::default_market_priority()
            .iter()
            .map(Market::as_str)
            .collect(),
    })
}

#[derive(Debug, Deserialize)]
struct MintRouteQuery {
    quote_mint: Option<String>,
    market_priority: Option<String>,
    min_liquidity_raw: Option<u64>,
    rpc_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MintMetadataQuery {
    rpc_url: Option<String>,
}

fn parse_market_priority(query: Option<&str>) -> Result<Option<Vec<Market>>, ApiError> {
    let Some(raw) = query else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed =
        Market::parse_csv(trimmed).map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(Some(parsed))
}

fn parse_market_filters(
    market: Option<&str>,
    markets: Option<&str>,
) -> Result<Option<Vec<Market>>, ApiError> {
    let mut out = Vec::new();

    if let Some(raw_market) = market {
        let trimmed = raw_market.trim();
        if !trimmed.is_empty() {
            out.push(parse_market_or_bad_request(trimmed)?);
        }
    }

    if let Some(raw_markets) = markets {
        let trimmed = raw_markets.trim();
        if !trimmed.is_empty() {
            let parsed = Market::parse_csv(trimmed)
                .map_err(|error| ApiError::bad_request(error.to_string()))?;
            for market in parsed {
                if !out.contains(&market) {
                    out.push(market);
                }
            }
        }
    }

    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

fn non_empty_optional(value: Option<String>) -> Option<String> {
    value
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

fn non_empty_optional_str(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|raw| !raw.is_empty())
}

#[derive(Debug, Clone, Copy, Serialize)]
struct RouteLiquidityResponse {
    wsol_liquidity_raw: u64,
    wsol_liquidity_sol: f64,
    max_safe_buy_sol_raw: u64,
    max_safe_buy_sol: f64,
}

impl From<RouteLiquiditySnapshot> for RouteLiquidityResponse {
    fn from(value: RouteLiquiditySnapshot) -> Self {
        Self {
            wsol_liquidity_raw: value.wsol_liquidity_raw,
            wsol_liquidity_sol: Swaps::lamports_to_sol(value.wsol_liquidity_raw),
            max_safe_buy_sol_raw: value.max_safe_buy_sol_raw,
            max_safe_buy_sol: Swaps::lamports_to_sol(value.max_safe_buy_sol_raw),
        }
    }
}

#[derive(Serialize)]
struct MintCreatorResponse {
    mint: String,
    market: String,
    pool: String,
    creator: String,
    creator_source: String,
    low_lq: bool,
    #[serde(flatten)]
    liquidity: RouteLiquidityResponse,
    liquidity_warning: Option<String>,
}

async fn get_mint_creator(
    State(state): State<Arc<ApiState>>,
    Path(mint): Path<String>,
    Query(query): Query<MintRouteQuery>,
) -> Result<Json<MintCreatorResponse>, ApiError> {
    let quote_mint = non_empty_optional(query.quote_mint);
    let market_priority = parse_market_priority(query.market_priority.as_deref())?;
    let min_liquidity_raw = query.min_liquidity_raw.unwrap_or(DEFAULT_MIN_LIQUIDITY_RAW);
    let rpc_url_override = non_empty_optional(query.rpc_url);

    let selection = resolve_mint_route_selection_with_rpc_fallback(
        state.as_ref(),
        &mint,
        quote_mint.as_ref(),
        min_liquidity_raw,
        market_priority.as_deref(),
        rpc_url_override.as_deref(),
        true,
    )
    .await?
    .ok_or_else(|| ApiError::not_found("no eligible route found for mint"))?;
    let route = selection.route;

    Ok(Json(MintCreatorResponse {
        mint,
        market: route.market.as_str().to_string(),
        pool: route.pool.to_string(),
        creator: route.creator.to_string(),
        creator_source: route.source.as_str().to_string(),
        low_lq: selection.low_lq,
        liquidity: RouteLiquidityResponse::from(selection.liquidity),
        liquidity_warning: selection.warning,
    }))
}

#[derive(Serialize)]
struct MintRouteResponse {
    mint: String,
    market: String,
    pool: String,
    creator: String,
    creator_source: String,
    price: f64,
    low_lq: bool,
    #[serde(flatten)]
    liquidity: RouteLiquidityResponse,
    liquidity_warning: Option<String>,
}

async fn get_mint_route(
    State(state): State<Arc<ApiState>>,
    Path(mint): Path<String>,
    Query(query): Query<MintRouteQuery>,
) -> Result<Json<MintRouteResponse>, ApiError> {
    let quote_mint = non_empty_optional(query.quote_mint);
    let market_priority = parse_market_priority(query.market_priority.as_deref())?;
    let min_liquidity_raw = query.min_liquidity_raw.unwrap_or(DEFAULT_MIN_LIQUIDITY_RAW);
    let rpc_url_override = non_empty_optional(query.rpc_url);

    let selection = resolve_mint_route_selection_with_rpc_fallback(
        state.as_ref(),
        &mint,
        quote_mint.as_ref(),
        min_liquidity_raw,
        market_priority.as_deref(),
        rpc_url_override.as_deref(),
        false,
    )
    .await?
    .ok_or_else(|| ApiError::not_found("no eligible route found for mint"))?;
    let route = selection.route;

    let price = match fetch_price_for_route_with_fallback(
        state.as_ref(),
        &mint,
        &route,
        rpc_url_override.as_deref(),
    )
    .await
    {
        Ok(price) => price,
        Err(error) if route.market == Market::PumpSwap => {
            warn!(
                "pump.swap route price lookup failed for mint {} pool {}: {:?}; returning route without live price",
                mint, route.pool, error
            );
            resolve_cached_price_for_route(state.as_ref(), &mint, &route)
                .await
                .unwrap_or(0.0)
        }
        Err(error) => return Err(error),
    };

    Ok(Json(MintRouteResponse {
        mint,
        market: route.market.as_str().to_string(),
        pool: route.pool.to_string(),
        creator: route.creator.to_string(),
        creator_source: route.source.as_str().to_string(),
        price,
        low_lq: selection.low_lq,
        liquidity: RouteLiquidityResponse::from(selection.liquidity),
        liquidity_warning: selection.warning,
    }))
}

#[derive(Serialize)]
struct MintMetadataResponse {
    mint: String,
    name: String,
    symbol: String,
    uri: String,
    creator: Option<String>,
    authority: String,
}

fn is_retryable_mint_metadata_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("error sending request")
        || message.contains("timed out")
        || message.contains("too many requests")
        || message.contains("429")
        || message.contains("connection")
        || message.contains("temporarily unavailable")
}

fn is_rate_limited_mint_metadata_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("429") || message.contains("too many requests")
}

fn is_not_found_mint_metadata_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("accountnotfound") || message.contains("account not found")
}

async fn get_mint_metadata(
    State(state): State<Arc<ApiState>>,
    Path(mint): Path<String>,
    Query(query): Query<MintMetadataQuery>,
) -> Result<Json<MintMetadataResponse>, ApiError> {
    let mint_trimmed = mint.trim().to_string();
    if mint_trimmed.is_empty() {
        return Err(ApiError::bad_request("mint is required"));
    }

    let mint_pubkey = Pubkey::from_str(&mint_trimmed)
        .with_context(|| format!("invalid mint pubkey: {}", mint_trimmed))
        .map_err(|error| ApiError::bad_request(error.to_string()))?;

    let mut last_error: Option<anyhow::Error> = None;
    let mut info = None;
    let rpc_url_override = non_empty_optional(query.rpc_url);
    let base_offset = state.endpoint_cursor.fetch_add(1, AtomicOrdering::Relaxed);
    let attempt_count = if rpc_url_override.is_some() {
        1
    } else {
        state.rpc_clients.len().max(3)
    };
    for attempt in 0..attempt_count {
        let (rpc_client, cluster) = if let Some(url) = rpc_url_override.as_deref() {
            resolve_request_rpc(state.as_ref(), base_offset, Some(url)).await?
        } else {
            (
                rpc_client_for_attempt(state.as_ref(), base_offset, attempt).await?,
                state.cluster,
            )
        };
        let sol_hook = SolHook::from_rpc_client_with_cluster(rpc_client, cluster);
        match sol_hook.get_token_info(&mint_pubkey).await {
            Ok(found) => {
                info = Some(found);
                break;
            }
            Err(error) => {
                let retryable = is_retryable_mint_metadata_error(&error);
                last_error = Some(error);
                if !retryable || attempt + 1 >= attempt_count {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(120 * (attempt as u64 + 1))).await;
            }
        }
    }

    let info = if let Some(info) = info {
        info
    } else {
        let error = last_error.unwrap_or_else(|| anyhow::anyhow!("mint metadata lookup failed"));
        if is_rate_limited_mint_metadata_error(&error) {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                format!("mint metadata lookup rate-limited: {}", error),
            ));
        }
        if is_not_found_mint_metadata_error(&error) {
            return Err(ApiError::not_found(format!(
                "mint metadata not found for {}: {}",
                mint_trimmed, error
            )));
        }
        return Err(error.into());
    };

    Ok(Json(MintMetadataResponse {
        mint: mint_trimmed,
        name: info.name,
        symbol: info.symbol,
        uri: info.uri,
        creator: info.creator.map(|creator| creator.to_string()),
        authority: info.authority.to_string(),
    }))
}

#[derive(Deserialize)]
struct MintMetadataBatchRequest {
    mints: Vec<String>,
}

#[derive(Serialize)]
struct MintMetadataBatchResponse {
    results: Vec<MintMetadataResponse>,
}

fn trim_nul_metadata(value: &str) -> String {
    value.trim_matches('\0').trim().to_string()
}

async fn post_mint_metadata_batch(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<MintMetadataBatchRequest>,
) -> Result<Json<MintMetadataBatchResponse>, ApiError> {
    let mut parsed = Vec::<(String, Pubkey)>::new();
    let mut seen = HashSet::<String>::new();
    for raw in payload.mints {
        if parsed.len() >= MAX_MINT_METADATA_BATCH {
            break;
        }

        let mint = raw.trim().to_string();
        if mint.is_empty() || !seen.insert(mint.clone()) {
            continue;
        }

        let mint_pubkey = match Pubkey::from_str(&mint) {
            Ok(pubkey) => pubkey,
            Err(_) => continue,
        };
        parsed.push((mint, mint_pubkey));
    }

    if parsed.is_empty() {
        return Ok(Json(MintMetadataBatchResponse {
            results: Vec::new(),
        }));
    }

    let mut results = Vec::new();
    let mut rate_limited_subchunks = 0usize;
    let mut failed_subchunks = 0usize;
    for chunk in parsed.chunks(10) {
        let pdas = chunk
            .iter()
            .map(|(_, mint)| {
                Pubkey::find_program_address(
                    &[b"metadata", METADATA_PROGRAM_ID.as_ref(), mint.as_ref()],
                    &METADATA_PROGRAM_ID,
                )
                .0
            })
            .collect::<Vec<_>>();

        let base_offset = state.endpoint_cursor.fetch_add(1, AtomicOrdering::Relaxed);
        let rpc_client = rpc_client_for_attempt(state.as_ref(), base_offset, 0).await?;
        let sol_hook = SolHook::from_rpc_client_with_cluster(rpc_client, state.cluster);
        let accounts = match sol_hook.get_multiple_accounts_resilient(&pdas).await {
            Ok(accounts) => accounts,
            Err(error) => {
                if is_rate_limited_mint_metadata_error(&error) {
                    rate_limited_subchunks = rate_limited_subchunks.saturating_add(1);
                }
                failed_subchunks = failed_subchunks.saturating_add(1);
                warn!(
                    "mint metadata batch subchunk lookup failed ({} mints): {}",
                    chunk.len(),
                    error
                );
                continue;
            }
        };

        for ((mint, _mint_pubkey), account_opt) in chunk.iter().zip(accounts.into_iter()) {
            let Some(account) = account_opt else {
                continue;
            };

            let Ok(metadata) = MplMetadata::safe_deserialize(&account.data) else {
                continue;
            };

            let name = trim_nul_metadata(&metadata.name);
            let symbol = trim_nul_metadata(&metadata.symbol);
            let uri = trim_nul_metadata(&metadata.uri);
            if name.is_empty() && symbol.is_empty() && uri.is_empty() {
                continue;
            }

            let creator = metadata
                .creators
                .as_ref()
                .and_then(|entries| entries.first().map(|entry| entry.address.to_string()));

            results.push(MintMetadataResponse {
                mint: mint.clone(),
                name,
                symbol,
                uri,
                creator,
                authority: metadata.update_authority.to_string(),
            });
        }
    }

    if results.is_empty() && rate_limited_subchunks > 0 && failed_subchunks > 0 {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "mint metadata batch lookup rate-limited across {} subchunks",
                rate_limited_subchunks
            ),
        ));
    }

    Ok(Json(MintMetadataBatchResponse { results }))
}

#[derive(Deserialize)]
struct MintListQuery {
    q: Option<String>,
    limit: Option<usize>,
    min_liquidity: Option<f64>,
    min_volume: Option<f64>,
    market: Option<String>,
    markets: Option<String>,
}

#[derive(Serialize)]
struct CachedMintView {
    market: String,
    mint: String,
    pool: String,
    creator: String,
    name: String,
    symbol: String,
    uri: String,
    price: f64,
    highest_price: f64,
    volume: f64,
    liquidity: f64,
    buys: i64,
    sells: i64,
    tx_count: i64,
    is_migrated: bool,
    migration_source_market: Option<String>,
    migration_target_market: Option<String>,
    migration_signature: Option<String>,
    migration_slot: Option<u64>,
    migration_time: Option<f64>,
    migration_confidence: Option<String>,
    holder_count: i64,
    holder_debug_reason: Option<String>,
    created_time: f64,
    last_activity_time: f64,
    market_cap: f64,
}

async fn collect_active_market_handlers(
    state: &ApiState,
    market_filters: Option<&[Market]>,
) -> Vec<(Market, Arc<WsHandler>)> {
    let subscriptions = state.subscriptions.lock().await;
    let mut handlers = subscriptions
        .iter()
        .filter_map(|(market, subscription)| {
            if subscription.task.is_finished() {
                return None;
            }
            if let Some(filters) = market_filters
                && !filters.contains(market)
            {
                return None;
            }
            Some((*market, subscription.handler.clone()))
        })
        .collect::<Vec<_>>();
    if let Some(filters) = market_filters {
        handlers.sort_by_key(|(market, _)| {
            filters
                .iter()
                .position(|candidate| candidate == market)
                .unwrap_or(filters.len())
        });
    } else {
        handlers.sort_by_key(|(market, _)| {
            DEFAULT_MARKET_PRIORITY
                .iter()
                .position(|candidate| candidate == market)
                .unwrap_or(DEFAULT_MARKET_PRIORITY.len())
        });
    }
    handlers
}

fn collect_cached_mints_from_handlers(
    handlers: &[(Market, Arc<WsHandler>)],
) -> Vec<(Market, Mint)> {
    let mut deduped = HashMap::<String, (Market, Mint)>::new();
    for (market, handler) in handlers {
        for (_, mint) in handler.mints.iter() {
            if mint.mint == WSOL_MINT || mint.mint == Pubkey::default() {
                continue;
            }

            let key = mint.mint.to_string();
            if let Some((existing_market, existing)) = deduped.get_mut(&key) {
                merge_cached_mint_snapshot(existing_market, existing, *market, &mint);
            } else {
                deduped.insert(key, (*market, mint.clone()));
            }
        }
    }
    deduped.into_values().collect()
}

fn migration_confidence_rank(confidence: MigrationConfidence) -> u8 {
    match confidence {
        MigrationConfidence::Confirmed => 2,
        MigrationConfidence::Suspected => 1,
    }
}

fn should_replace_migration_event(current: &MigrationEvent, next: &MigrationEvent) -> bool {
    let current_rank = migration_confidence_rank(current.migration_confidence);
    let next_rank = migration_confidence_rank(next.migration_confidence);
    if next_rank != current_rank {
        return next_rank > current_rank;
    }
    if next.migration_slot != current.migration_slot {
        return next.migration_slot > current.migration_slot;
    }
    if (next.migration_time - current.migration_time).abs() > f64::EPSILON {
        return next.migration_time > current.migration_time;
    }
    false
}

fn merge_non_decreasing_mint_fields(primary: &mut Mint, secondary: &Mint) {
    primary.highest_price = primary
        .highest_price
        .max(secondary.highest_price)
        .max(primary.price.max(secondary.price));
    if primary.price <= 0.0 && secondary.price > 0.0 {
        primary.price = secondary.price;
    }
    primary.buys = primary.buys.max(secondary.buys);
    primary.sells = primary.sells.max(secondary.sells);
    primary.tx_count = primary
        .tx_count
        .max(secondary.tx_count)
        .max(primary.buys.saturating_add(primary.sells));
    primary.volume = primary.volume.max(secondary.volume);
    if primary.liquidity <= 0.0 && secondary.liquidity > 0.0 {
        primary.liquidity = secondary.liquidity;
    }
    primary.holder_count = primary.holder_count.max(secondary.holder_count);

    if secondary.created_time > 0.0
        && (primary.created_time <= 0.0 || secondary.created_time < primary.created_time)
    {
        primary.created_time = secondary.created_time;
    }
    primary.last_activity_time = primary.last_activity_time.max(secondary.last_activity_time);
}

fn stabilize_market_from_migration(current_market: &mut Market, mint: &Mint) {
    if let Some(target_market) = mint
        .migration_event
        .as_ref()
        .and_then(|event| Market::from_token(event.target_market))
    {
        *current_market = target_market;
    }
}

fn merge_cached_mint_snapshot(
    existing_market: &mut Market,
    existing: &mut Mint,
    candidate_market: Market,
    candidate: &Mint,
) {
    let allow_market_replacement = *existing_market == candidate_market
        || existing.migration_event.is_some()
        || candidate.migration_event.is_some()
        || existing.is_migrated
        || candidate.is_migrated;
    if allow_market_replacement && should_replace_cached_mint(existing, candidate) {
        let mut replacement = candidate.clone();
        if let Some(existing_event) = existing.migration_event.clone()
            && replacement.migration_event.is_none()
        {
            replacement.migration_event = Some(existing_event);
        }
        replacement.is_migrated = replacement.is_migrated
            || replacement.migration_event.is_some()
            || existing.is_migrated;
        replacement.holder_count = replacement.holder_count.max(existing.holder_count);
        if replacement.name.is_empty() && !existing.name.is_empty() {
            replacement.name = existing.name.clone();
        }
        if replacement.symbol.is_empty() && !existing.symbol.is_empty() {
            replacement.symbol = existing.symbol.clone();
        }
        if replacement.uri.is_empty() && !existing.uri.is_empty() {
            replacement.uri = existing.uri.clone();
        }
        merge_non_decreasing_mint_fields(&mut replacement, existing);
        *existing = replacement;
        *existing_market = candidate_market;
        stabilize_market_from_migration(existing_market, existing);
        return;
    }

    if existing.name.is_empty() && !candidate.name.is_empty() {
        existing.name = candidate.name.clone();
    }
    if existing.symbol.is_empty() && !candidate.symbol.is_empty() {
        existing.symbol = candidate.symbol.clone();
    }
    if existing.uri.is_empty() && !candidate.uri.is_empty() {
        existing.uri = candidate.uri.clone();
    }
    existing.holder_count = existing.holder_count.max(candidate.holder_count);

    match (
        existing.migration_event.as_ref(),
        candidate.migration_event.as_ref(),
    ) {
        (None, Some(event)) => existing.migration_event = Some(event.clone()),
        (Some(current), Some(next)) if should_replace_migration_event(current, next) => {
            existing.migration_event = Some(next.clone());
        }
        _ => {}
    }

    if existing.migration_event.is_some() {
        existing.is_migrated = true;
    } else {
        existing.is_migrated = existing.is_migrated || candidate.is_migrated;
    }

    merge_non_decreasing_mint_fields(existing, candidate);
    stabilize_market_from_migration(existing_market, existing);
}

fn should_replace_cached_mint(current: &Mint, next: &Mint) -> bool {
    let current_has_migration = current.migration_event.is_some() || current.is_migrated;
    let next_has_migration = next.migration_event.is_some() || next.is_migrated;
    if !current_has_migration && next_has_migration {
        return true;
    }
    if current_has_migration && !next_has_migration {
        return false;
    }

    match (
        current.migration_event.as_ref(),
        next.migration_event.as_ref(),
    ) {
        (Some(current_event), Some(next_event)) => {
            if should_replace_migration_event(current_event, next_event) {
                return true;
            }
            if should_replace_migration_event(next_event, current_event) {
                return false;
            }
        }
        (None, Some(_)) => return true,
        (Some(_), None) => return false,
        (None, None) => {}
    }

    if next.last_activity_time > current.last_activity_time {
        return true;
    }
    if next.last_activity_time < current.last_activity_time {
        return false;
    }

    if next.created_time > current.created_time {
        return true;
    }
    if next.created_time < current.created_time {
        return false;
    }

    if current.name.is_empty() && !next.name.is_empty() {
        return true;
    }
    if current.symbol.is_empty() && !next.symbol.is_empty() {
        return true;
    }

    false
}

fn holder_debug_reason(tx_count: i64, holder_count: i64) -> Option<String> {
    if tx_count >= 10 && holder_count <= 1 {
        Some("holder_count_low_vs_tx_count".to_string())
    } else {
        None
    }
}

fn effective_price_for_market_cap(mint: &Mint) -> f64 {
    if mint.price > 0.0 {
        mint.price
    } else {
        mint.highest_price.max(0.0)
    }
}

fn cached_mint_view_from_snapshot(market: Market, mint: Mint) -> CachedMintView {
    let tx_count = mint
        .tx_count
        .max(mint.buys.saturating_add(mint.sells))
        .max(0);

    let migration_source_market = mint
        .migration_event
        .as_ref()
        .map(|event| event.source_market.to_string());
    let migration_target_market = mint
        .migration_event
        .as_ref()
        .map(|event| event.target_market.to_string());
    let migration_signature = mint
        .migration_event
        .as_ref()
        .map(|event| event.migration_signature.clone());
    let migration_slot = mint
        .migration_event
        .as_ref()
        .map(|event| event.migration_slot);
    let migration_time = mint
        .migration_event
        .as_ref()
        .map(|event| event.migration_time);
    let migration_confidence = mint
        .migration_event
        .as_ref()
        .map(|event| event.migration_confidence.as_str().to_string());
    let holder_debug_reason = holder_debug_reason(tx_count, mint.holder_count);

    let market_cap = default_market_cap_from_price(effective_price_for_market_cap(&mint));

    CachedMintView {
        market: market.as_str().to_string(),
        mint: mint.mint.to_string(),
        pool: mint.bonding_curve.to_string(),
        creator: mint.creator.to_string(),
        name: mint.name,
        symbol: mint.symbol,
        uri: mint.uri,
        price: mint.price,
        highest_price: mint.highest_price,
        volume: mint.volume,
        liquidity: mint.liquidity,
        buys: mint.buys,
        sells: mint.sells,
        tx_count,
        is_migrated: mint.is_migrated || mint.migration_event.is_some(),
        migration_source_market,
        migration_target_market,
        migration_signature,
        migration_slot,
        migration_time,
        migration_confidence,
        holder_count: mint.holder_count,
        holder_debug_reason,
        created_time: mint.created_time,
        last_activity_time: mint.last_activity_time,
        market_cap,
    }
}

fn compare_f64_desc(left: f64, right: f64) -> Ordering {
    right.partial_cmp(&left).unwrap_or(Ordering::Equal)
}

fn sort_cached_mint_rows_freshest_first(rows: &mut [(Market, Mint)]) {
    rows.sort_by(|left, right| {
        compare_f64_desc(left.1.last_activity_time, right.1.last_activity_time)
            .then_with(|| compare_f64_desc(left.1.created_time, right.1.created_time))
    });
}

async fn build_cached_mint_views(
    _state: &ApiState,
    rows: Vec<(Market, Mint)>,
    limit: usize,
) -> Vec<CachedMintView> {
    rows.into_iter()
        .take(limit)
        .map(|(market, mint)| cached_mint_view_from_snapshot(market, mint))
        .collect()
}

fn mint_matches_query(mint: &Mint, query: &str) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }
    mint.name.to_ascii_lowercase().contains(&q)
        || mint.symbol.to_ascii_lowercase().contains(&q)
        || mint.mint.to_string().to_ascii_lowercase().contains(&q)
}

async fn list_cached_mints(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MintListQuery>,
) -> Result<Json<Vec<CachedMintView>>, ApiError> {
    let market_filters = parse_market_filters(query.market.as_deref(), query.markets.as_deref())?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_MINT_LIMIT)
        .clamp(1, MAX_MINT_LIMIT);
    let min_liquidity = query.min_liquidity.unwrap_or(0.0).max(0.0);
    let min_volume = query.min_volume.unwrap_or(0.0).max(0.0);
    let q = query.q.unwrap_or_default();

    let handlers = collect_active_market_handlers(state.as_ref(), market_filters.as_deref()).await;
    let mut rows = collect_cached_mints_from_handlers(&handlers)
        .into_iter()
        .filter(|(_, mint)| {
            mint_matches_query(mint, &q)
                && mint.liquidity >= min_liquidity
                && mint.volume >= min_volume
        })
        .collect::<Vec<_>>();

    sort_cached_mint_rows_freshest_first(&mut rows);

    let out = build_cached_mint_views(state.as_ref(), rows, limit).await;

    Ok(Json(out))
}

#[derive(Deserialize)]
struct WsSubscriptionRequest {
    market: String,
}

#[derive(Serialize)]
struct WsSubscriptionResponse {
    market: String,
    subscribed: bool,
}

fn parse_market_or_bad_request(raw: &str) -> Result<Market, ApiError> {
    Market::from_token(raw).ok_or_else(|| {
        ApiError::bad_request(
            "unsupported market; use pump_swap,pump_fun,raydium_amm_v4,raydium_launchpad,raydium_clmm,raydium_cpmm,meteora_dlmm,meteora_damm_v1,meteora_damm_v2,meteora_dbc",
        )
    })
}

fn spawn_ws_market_subscription(
    ws_handler: Arc<WsHandler>,
    market: Market,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let result = match market {
            Market::PumpSwap => ws_handler.subscribe_ws_pump_swap().await,
            Market::PumpFun => ws_handler.subscribe_ws_pump_fun().await,
            Market::RaydiumAmmV4 => ws_handler.subscribe_ws_raydium_amm_v4().await,
            Market::RaydiumLaunchpad => ws_handler.subscribe_ws_raydium_launchpad().await,
            Market::RaydiumClmm => ws_handler.subscribe_ws_raydium_clmm().await,
            Market::RaydiumCpmm => ws_handler.subscribe_ws_raydium_cpmm().await,
            Market::MeteoraDlmm => ws_handler.subscribe_ws_meteora_dlmm().await,
            Market::MeteoraDammV1 => ws_handler.subscribe_ws_meteora_damm_v1().await,
            Market::MeteoraDammV2 => ws_handler.subscribe_ws_meteora_damm_v2().await,
            Market::MeteoraDbc => ws_handler.subscribe_ws_meteora_dbc().await,
        };

        if let Err(error) = result {
            warn!(
                "api websocket subscription {} terminated with error: {}",
                market.as_str(),
                error
            );
        }
    })
}

async fn post_ws_subscribe(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<WsSubscriptionRequest>,
) -> Result<Json<WsSubscriptionResponse>, ApiError> {
    let market = parse_market_or_bad_request(&payload.market)?;

    let mut subscriptions = state.subscriptions.lock().await;
    if let Some(subscription) = subscriptions.get(&market)
        && !subscription.task.is_finished()
    {
        return Ok(Json(WsSubscriptionResponse {
            market: market.as_str().to_string(),
            subscribed: true,
        }));
    }

    let ws_slot = market_endpoint_slot(market, state.ws_urls.len());
    let ws_url = state
        .ws_urls
        .get(ws_slot)
        .cloned()
        .ok_or_else(|| ApiError::internal("missing configured websocket endpoint"))?;
    let rpc_slot = market_endpoint_slot(market, state.rpc_clients.len());
    let rpc_client = state
        .rpc_clients
        .get(rpc_slot)
        .cloned()
        .ok_or_else(|| ApiError::internal("missing configured RPC endpoint"))?;
    let sol_hook = SolHook::from_rpc_client_with_cluster(rpc_client, state.cluster);

    let handler = Arc::new(WsHandler::new(sol_hook, ws_url.clone()));
    let task = spawn_ws_market_subscription(handler.clone(), market);
    subscriptions.insert(market, MarketSubscription { handler, task });

    Ok(Json(WsSubscriptionResponse {
        market: market.as_str().to_string(),
        subscribed: true,
    }))
}

#[derive(Serialize)]
struct WsUnsubscribeResponse {
    market: String,
    unsubscribed: bool,
}

async fn post_ws_unsubscribe(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<WsSubscriptionRequest>,
) -> Result<Json<WsUnsubscribeResponse>, ApiError> {
    let market = parse_market_or_bad_request(&payload.market)?;
    let mut subscriptions = state.subscriptions.lock().await;

    if let Some((_, subscription)) = subscriptions.remove_entry(&market) {
        subscription.task.abort();
        return Ok(Json(WsUnsubscribeResponse {
            market: market.as_str().to_string(),
            unsubscribed: true,
        }));
    }

    Ok(Json(WsUnsubscribeResponse {
        market: market.as_str().to_string(),
        unsubscribed: false,
    }))
}

#[derive(Serialize)]
struct WsSubscriptionStatus {
    market: String,
    active: bool,
}

async fn get_ws_subscriptions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<WsSubscriptionStatus>>, ApiError> {
    let subscriptions = state.subscriptions.lock().await;
    let mut out = subscriptions
        .iter()
        .map(|(market, subscription)| WsSubscriptionStatus {
            market: market.as_str().to_string(),
            active: !subscription.task.is_finished(),
        })
        .collect::<Vec<_>>();
    out.sort_by(|left, right| left.market.cmp(&right.market));
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
struct WsStreamQuery {
    q: Option<String>,
    limit: Option<usize>,
    min_liquidity: Option<f64>,
    min_volume: Option<f64>,
    market: Option<String>,
    markets: Option<String>,
    interval_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct WsStreamOptions {
    q: String,
    limit: usize,
    min_liquidity: f64,
    min_volume: f64,
    market_filters: Option<Vec<Market>>,
    interval_ms: u64,
}

#[derive(Serialize)]
struct WsStreamEnvelope {
    sent_unix_ms: u128,
    mints: Vec<CachedMintView>,
}

async fn get_ws_stream(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WsStreamQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let market_filters = parse_market_filters(query.market.as_deref(), query.markets.as_deref())?;
    let options = WsStreamOptions {
        q: query.q.unwrap_or_default(),
        limit: query
            .limit
            .unwrap_or(DEFAULT_WS_STREAM_LIMIT)
            .clamp(1, MAX_WS_STREAM_LIMIT),
        min_liquidity: query.min_liquidity.unwrap_or(0.0).max(0.0),
        min_volume: query.min_volume.unwrap_or(0.0).max(0.0),
        market_filters,
        interval_ms: query
            .interval_ms
            .unwrap_or(DEFAULT_WS_STREAM_INTERVAL_MS)
            .clamp(150, 30_000),
    };

    Ok(upgrade.on_upgrade(move |socket| async move {
        stream_mint_snapshots(socket, state, options).await;
    }))
}

async fn stream_mint_snapshots(socket: WebSocket, state: Arc<ApiState>, options: WsStreamOptions) {
    let mut socket = socket;
    let mut interval = tokio::time::interval(Duration::from_millis(options.interval_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_push = Instant::now();

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let handlers = collect_active_market_handlers(state.as_ref(), options.market_filters.as_deref()).await;
                let mut rows = collect_cached_mints_from_handlers(&handlers)
                    .into_iter()
                    .filter(|(_, mint)| {
                        mint_matches_query(mint, &options.q)
                            && mint.liquidity >= options.min_liquidity
                            && mint.volume >= options.min_volume
                    })
                    .collect::<Vec<_>>();

                sort_cached_mint_rows_freshest_first(&mut rows);

                let mints = build_cached_mint_views(state.as_ref(), rows, options.limit).await;

                let payload = WsStreamEnvelope {
                    sent_unix_ms: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or(Duration::from_secs(0))
                        .as_millis(),
                    mints,
                };

                let text = match serde_json::to_string(&payload) {
                    Ok(text) => text,
                    Err(error) => {
                        warn!("failed to encode websocket mint stream payload: {}", error);
                        break;
                    }
                };

                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
                last_push = Instant::now();
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Text(_))) => {}
                    Some(Ok(Message::Binary(_))) => {}
                    Some(Err(error)) => {
                        warn!("websocket stream receive error: {}", error);
                        break;
                    }
                }
            }
        }

        // If a client becomes one-way idle and never reads, close eventually.
        if last_push.elapsed() > Duration::from_secs(600) {
            break;
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SwapSide {
    Buy,
    Sell,
}

#[derive(Debug, Deserialize)]
struct SwapRequest {
    side: SwapSide,
    mint: String,
    market: Option<String>,
    pool: Option<String>,
    creator: Option<String>,
    quote_mint: Option<String>,
    market_priority: Option<String>,
    min_liquidity_raw: Option<u64>,
    skip_low_lq_pools: Option<bool>,
    buy_sol: Option<f64>,
    sell_pct: Option<u64>,
    slippage_pct: Option<f64>,
    retries: Option<u32>,
    use_idempotent: Option<bool>,
    priority_fee_level: Option<String>,
    priority_fee_sol: Option<f64>,
    use_swqos: Option<bool>,
    swqos_settings: Option<ApiSwqosSettings>,
    execute: Option<bool>,
    market_cap: Option<f64>,
    wallet: Option<String>,
    rpc_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiSwqosSettings {
    provider: SwqosProvider,
    jito_key: Option<String>,
    nextblock_key: String,
    zero_slot_key: String,
    temporal_key: String,
    blox_key: String,
    tip_lamports: u64,
    nonce_account: Option<String>,
}

impl From<ApiSwqosSettings> for SWQoSettings {
    fn from(value: ApiSwqosSettings) -> Self {
        Self {
            provider: value.provider,
            jito_key: value.jito_key,
            nextblock_key: value.nextblock_key,
            zero_slot_key: value.zero_slot_key,
            temporal_key: value.temporal_key,
            blox_key: value.blox_key,
            tip_lamports: value.tip_lamports,
            nonce_account: value.nonce_account,
        }
    }
}

#[derive(Serialize)]
struct SwapResponse {
    dry_run: bool,
    executed: bool,
    success: bool,
    market: String,
    pool: String,
    mint: String,
    creator: String,
    creator_source: String,
    price: f64,
    low_lq: bool,
    #[serde(flatten)]
    liquidity: RouteLiquidityResponse,
    signature: Option<String>,
    error: Option<String>,
    warning: Option<String>,
}

fn default_market_cap_from_price(price: f64) -> f64 {
    // Common memecoin assumption (1B supply), used when caller doesn't provide explicit market cap.
    (price.max(0.0)) * 1_000_000_000.0
}

fn append_warning(existing: Option<String>, warning: impl Into<String>) -> Option<String> {
    let warning = warning.into();
    if warning.trim().is_empty() {
        return existing;
    }

    match existing {
        Some(existing) if !existing.trim().is_empty() => Some(format!("{existing}; {warning}")),
        _ => Some(warning),
    }
}

fn buy_sol_capacity_threshold_raw(
    buy_sol: Option<f64>,
    slippage_pct: f64,
) -> Result<Option<u64>, ApiError> {
    let Some(buy_sol) = buy_sol else {
        return Ok(None);
    };
    if !buy_sol.is_finite() || buy_sol <= 0.0 {
        return Ok(None);
    }

    let lamports = buy_sol * 1_000_000_000.0 * (1.0 + (slippage_pct / 100.0));
    if !lamports.is_finite() || lamports <= 0.0 || lamports > (u64::MAX as f64) {
        return Err(ApiError::bad_request(
            "buy_sol is too large to convert into a liquidity threshold",
        ));
    }

    Ok(Some(lamports.ceil() as u64))
}

fn parse_swap_priority_fee_override(
    raw_level: Option<&str>,
    priority_fee_sol: Option<f64>,
) -> Result<Option<PriorityFeeOverride>, ApiError> {
    let level = raw_level
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());

    if let Some(priority_fee_sol) = priority_fee_sol {
        if let Some(level) = level.as_deref()
            && level != "custom"
        {
            return Err(ApiError::bad_request(
                "priority_fee_sol cannot be combined with priority_fee_level unless the level is custom",
            ));
        }
        let fee = SolHook::custom_priority_fee_micro_lamports_from_sol_amount(
            priority_fee_sol,
            DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
        )
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
        return Ok(Some(PriorityFeeOverride::ExactMicroLamports(fee)));
    }

    let Some(level) = level.as_deref() else {
        return Ok(None);
    };

    if level == "env" {
        return Ok(None);
    }

    if level == "custom" {
        return Err(ApiError::bad_request(
            "priority_fee_level=custom requires priority_fee_sol",
        ));
    }

    let parsed = PriorityFeeLevel::parse(level).ok_or_else(|| {
        ApiError::bad_request(
            "priority_fee_level must be one of env, low, medium, high, turbo, max, custom",
        )
    })?;
    Ok(Some(PriorityFeeOverride::Level(parsed)))
}

async fn resolve_mint_route_selection_with_swaps(
    swaps: &Swaps,
    mint: &str,
    quote_mint: Option<&String>,
    min_liquidity_raw: u64,
    market_priority: Option<&[Market]>,
    prefer_metadata_creator_first: bool,
) -> Result<Option<MintCreatorRouteSelection>, ApiError> {
    let timeout = Swaps::route_lookup_timeout_duration(market_priority);
    match tokio::time::timeout(
        timeout,
        swaps.get_mint_creator_selection_with_market_priority(
            &mint.to_string(),
            quote_mint,
            min_liquidity_raw,
            market_priority,
            prefer_metadata_creator_first,
        ),
    )
    .await
    {
        Ok(result) => result.map_err(ApiError::from),
        Err(_) => Err(ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            format!(
                "mint route lookup timed out after {}s for {mint}",
                timeout.as_secs()
            ),
        )),
    }
}

async fn fetch_price_for_route_with_swaps(
    swaps: &Swaps,
    route: &MintCreatorRoute,
) -> anyhow::Result<f64> {
    let timeout = Swaps::price_lookup_timeout_duration();
    match tokio::time::timeout(
        timeout,
        swaps.fetch_price_for_market_pool(route.market, &route.pool),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "price lookup timed out after {}s for market {} pool {}",
            timeout.as_secs(),
            route.market.as_str(),
            route.pool
        )),
    }
}

async fn resolve_swap_route(
    state: &ApiState,
    request: &SwapRequest,
) -> Result<MintCreatorRouteSelection, ApiError> {
    let mint = request.mint.trim().to_string();
    let rpc_url_override = non_empty_optional_str(request.rpc_url.as_deref());
    if mint.is_empty() {
        return Err(ApiError::bad_request("mint is required"));
    }

    if let (Some(market_raw), Some(pool_raw)) = (&request.market, &request.pool) {
        let market = parse_market_or_bad_request(market_raw)?;
        let pool = Pubkey::from_str(pool_raw.trim())
            .with_context(|| format!("invalid pool pubkey: {}", pool_raw.trim()))?;
        let mint_pubkey =
            Pubkey::from_str(&mint).with_context(|| format!("invalid mint pubkey: {}", mint))?;

        let creator = if let Some(raw_creator) = &request.creator {
            let creator = Pubkey::from_str(raw_creator.trim())
                .with_context(|| format!("invalid creator pubkey: {}", raw_creator.trim()))?;
            (creator, CreatorResolutionSource::MarketStateFallback)
        } else {
            let resolved = if let Some(rpc_url) = rpc_url_override {
                let base_offset = state.endpoint_cursor.fetch_add(1, AtomicOrdering::Relaxed);
                let (rpc, cluster) = resolve_request_rpc(state, base_offset, Some(rpc_url)).await?;
                let swaps = build_swaps_for_rpc_with_signer(
                    rpc,
                    cluster,
                    Arc::new(Keypair::new()),
                    Some(state.swaps.pump_swap.shared_pool_state_cache()),
                );
                swaps
                    .get_route_creator_for_market_pool(&mint_pubkey, market, &pool)
                    .await?
            } else {
                state
                    .swaps
                    .get_route_creator_for_market_pool(&mint_pubkey, market, &pool)
                    .await?
            };
            (resolved.creator, resolved.source)
        };

        let route = MintCreatorRoute {
            market,
            pool,
            creator: creator.0,
            source: creator.1,
        };
        let (liquidity, warning) = if let Some(rpc_url) = rpc_url_override {
            let base_offset = state.endpoint_cursor.fetch_add(1, AtomicOrdering::Relaxed);
            let (rpc, cluster) = resolve_request_rpc(state, base_offset, Some(rpc_url)).await?;
            let swaps = build_swaps_for_rpc_with_signer(
                rpc,
                cluster,
                Arc::new(Keypair::new()),
                Some(state.swaps.pump_swap.shared_pool_state_cache()),
            );
            match swaps
                .measure_route_liquidity_for_market_pool(&mint, market, pool_raw)
                .await
            {
                Ok(liquidity) => (liquidity, None),
                Err(error) => {
                    warn!(
                        "explicit swap route liquidity estimate failed for mint {} market {} pool {}: {}",
                        mint,
                        market.as_str(),
                        pool,
                        error
                    );
                    (
                        RouteLiquiditySnapshot::default(),
                        Some(format!(
                            "route liquidity estimate unavailable for explicit {} pool {}",
                            market.as_str(),
                            pool
                        )),
                    )
                }
            }
        } else {
            match state
                .swaps
                .measure_route_liquidity_for_market_pool(&mint, market, pool_raw)
                .await
            {
                Ok(liquidity) => (liquidity, None),
                Err(error) => {
                    warn!(
                        "explicit swap route liquidity estimate failed for mint {} market {} pool {}: {}",
                        mint,
                        market.as_str(),
                        pool,
                        error
                    );
                    (
                        RouteLiquiditySnapshot::default(),
                        Some(format!(
                            "route liquidity estimate unavailable for explicit {} pool {}",
                            market.as_str(),
                            pool
                        )),
                    )
                }
            }
        };
        let low_lq = Swaps::route_is_low_lq(market, &liquidity);
        let warning = append_warning(
            warning,
            Swaps::low_lq_warning_for_market_pool(market, &pool, &liquidity).unwrap_or_default(),
        );
        return Ok(MintCreatorRouteSelection {
            route,
            liquidity,
            low_lq,
            warning,
        });
    }

    let quote_mint = non_empty_optional(request.quote_mint.clone());
    let market_priority = if let Some(market_raw) = &request.market {
        Some(vec![parse_market_or_bad_request(market_raw)?])
    } else {
        parse_market_priority(request.market_priority.as_deref())?
    };
    let min_liquidity_raw = request
        .min_liquidity_raw
        .unwrap_or(DEFAULT_MIN_LIQUIDITY_RAW);

    resolve_mint_route_selection_with_rpc_fallback(
        state,
        &mint,
        quote_mint.as_ref(),
        min_liquidity_raw,
        market_priority.as_deref(),
        rpc_url_override,
        false,
    )
    .await?
    .ok_or_else(|| ApiError::not_found("no eligible route found for mint"))
}

fn build_swaps_for_rpc_with_signer(
    rpc: Arc<RpcClient>,
    cluster: crate::core::cluster::SolanaCluster,
    signer: Arc<Keypair>,
    pump_swap_pool_state_cache: Option<Arc<Mutex<HashMap<Pubkey, Vec<u8>>>>>,
) -> Swaps {
    let sol_hook = SolHook::from_rpc_client_with_cluster(rpc, cluster);
    let arc_sol_hook = Arc::new(sol_hook.clone());
    let pump_fun = PumpFun::new(signer.clone(), arc_sol_hook.clone());
    let pump_swap = if let Some(cache) = pump_swap_pool_state_cache {
        PumpSwap::new_with_pool_state_cache(signer, arc_sol_hook, cache)
    } else {
        PumpSwap::new(signer, arc_sol_hook)
    };
    Swaps::new(sol_hook, pump_swap, pump_fun)
}

async fn resolve_mint_route_selection_from_active_cache(
    state: &ApiState,
    mint: &str,
    market_priority: Option<&[Market]>,
) -> Option<MintCreatorRouteSelection> {
    let handlers = collect_active_market_handlers(state, market_priority).await;
    let (market, snapshot) = collect_cached_mints_from_handlers(&handlers)
        .into_iter()
        .find(|(_, snapshot)| snapshot.mint.to_string() == mint)?;
    let route = MintCreatorRoute {
        market,
        pool: snapshot.bonding_curve,
        creator: snapshot.creator,
        source: if snapshot.creator == Pubkey::default() {
            CreatorResolutionSource::Unresolved
        } else {
            CreatorResolutionSource::MarketStateFallback
        },
    };
    let liquidity = state
        .swaps
        .measure_route_liquidity_for_market_pool(
            &mint.to_string(),
            route.market,
            &route.pool.to_string(),
        )
        .await
        .unwrap_or_default();
    Some(MintCreatorRouteSelection {
        route,
        liquidity,
        low_lq: Swaps::route_is_low_lq(route.market, &liquidity),
        warning: append_warning(
            Some("using active cache fallback for route resolution".to_string()),
            Swaps::low_lq_warning_for_market_pool(route.market, &route.pool, &liquidity)
                .unwrap_or_default(),
        ),
    })
}

async fn resolve_cached_price_for_route(
    state: &ApiState,
    mint: &str,
    route: &MintCreatorRoute,
) -> Option<f64> {
    let handlers = collect_active_market_handlers(state, Some(&[route.market])).await;
    collect_cached_mints_from_handlers(&handlers)
        .into_iter()
        .find(|(market, snapshot)| {
            *market == route.market
                && (snapshot.bonding_curve == route.pool || snapshot.mint.to_string() == mint)
        })
        .and_then(|(_, snapshot)| (snapshot.price > 0.0).then_some(snapshot.price))
}

async fn resolve_mint_route_selection_with_rpc_fallback(
    state: &ApiState,
    mint: &str,
    quote_mint: Option<&String>,
    min_liquidity_raw: u64,
    market_priority: Option<&[Market]>,
    rpc_url_override: Option<&str>,
    prefer_metadata_creator_first: bool,
) -> Result<Option<MintCreatorRouteSelection>, ApiError> {
    if let Some(rpc_url) = non_empty_optional_str(rpc_url_override) {
        let base_offset = state.endpoint_cursor.fetch_add(1, AtomicOrdering::Relaxed);
        let (rpc, cluster) = resolve_request_rpc(state, base_offset, Some(rpc_url)).await?;
        let swaps = build_swaps_for_rpc_with_signer(
            rpc,
            cluster,
            Arc::new(Keypair::new()),
            Some(state.swaps.pump_swap.shared_pool_state_cache()),
        );
        match resolve_mint_route_selection_with_swaps(
            &swaps,
            mint,
            quote_mint,
            min_liquidity_raw,
            market_priority,
            prefer_metadata_creator_first,
        )
        .await
        {
            Ok(Some(route)) => return Ok(Some(route)),
            Ok(None) if cluster == state.cluster => {
                warn!(
                    "override rpc route lookup returned no result for {}; falling back to configured rpc pool on matching cluster {:?}",
                    mint, cluster
                );
            }
            Ok(None) => return Ok(None),
            Err(error) if cluster == state.cluster => {
                warn!(
                    "override rpc route lookup failed for {} on matching cluster {:?}: {}; falling back to configured rpc pool",
                    mint, cluster, error.message
                );
            }
            Err(error) => return Err(error),
        }
    }

    let primary = resolve_mint_route_selection_with_swaps(
        state.swaps.as_ref(),
        mint,
        quote_mint,
        min_liquidity_raw,
        market_priority,
        prefer_metadata_creator_first,
    )
    .await?;
    if primary.is_some() || state.rpc_clients.len() <= 1 {
        return Ok(primary);
    }

    for (idx, rpc) in state.rpc_clients.iter().enumerate().skip(1) {
        let swaps = build_swaps_for_rpc_with_signer(
            rpc.clone(),
            state.cluster,
            Arc::new(Keypair::new()),
            Some(state.swaps.pump_swap.shared_pool_state_cache()),
        );
        match resolve_mint_route_selection_with_swaps(
            &swaps,
            mint,
            quote_mint,
            min_liquidity_raw,
            market_priority,
            prefer_metadata_creator_first,
        )
        .await
        {
            Ok(Some(route)) => {
                println!(
                    "mint route fallback resolved {} via rpc slot {} ({})",
                    mint,
                    idx,
                    state
                        .rpc_urls
                        .get(idx)
                        .map(|value| value.as_str())
                        .unwrap_or("<unknown>")
                );
                return Ok(Some(route));
            }
            Ok(None) => {}
            Err(error) => warn!(
                "mint route fallback failed for {} on rpc slot {} ({}): {}",
                mint,
                idx,
                state
                    .rpc_urls
                    .get(idx)
                    .map(|value| value.as_str())
                    .unwrap_or("<unknown>"),
                error.message
            ),
        }
    }

    Ok(resolve_mint_route_selection_from_active_cache(state, mint, market_priority).await)
}

async fn fetch_price_for_route_with_fallback(
    state: &ApiState,
    mint: &str,
    route: &MintCreatorRoute,
    rpc_url_override: Option<&str>,
) -> Result<f64, ApiError> {
    if let Some(rpc_url) = non_empty_optional_str(rpc_url_override) {
        let base_offset = state.endpoint_cursor.fetch_add(1, AtomicOrdering::Relaxed);
        let (rpc, cluster) = resolve_request_rpc(state, base_offset, Some(rpc_url)).await?;
        let swaps = build_swaps_for_rpc_with_signer(
            rpc,
            cluster,
            Arc::new(Keypair::new()),
            Some(state.swaps.pump_swap.shared_pool_state_cache()),
        );
        match fetch_price_for_route_with_swaps(&swaps, route).await {
            Ok(price) => return Ok(price),
            Err(error) if cluster == state.cluster => {
                warn!(
                    "override rpc price lookup failed for mint {} market {} pool {} on matching cluster {:?}: {}; falling back to configured rpc pool",
                    mint,
                    route.market.as_str(),
                    route.pool,
                    cluster,
                    error
                );
            }
            Err(error) => {
                return Err(ApiError::new(StatusCode::BAD_GATEWAY, error.to_string()));
            }
        }
    }

    let last_error = match fetch_price_for_route_with_swaps(state.swaps.as_ref(), route).await {
        Ok(price) if price > 0.0 => return Ok(price),
        Ok(_) => Some(anyhow::anyhow!(
            "non-positive price returned for market {} pool {}",
            route.market.as_str(),
            route.pool
        )),
        Err(error) => Some(error),
    };

    for (idx, rpc) in state.rpc_clients.iter().enumerate().skip(1) {
        let swaps = build_swaps_for_rpc_with_signer(
            rpc.clone(),
            state.cluster,
            Arc::new(Keypair::new()),
            Some(state.swaps.pump_swap.shared_pool_state_cache()),
        );
        match fetch_price_for_route_with_swaps(&swaps, route).await {
            Ok(price) if price > 0.0 => {
                println!(
                    "mint price fallback resolved {} via rpc slot {} ({})",
                    mint,
                    idx,
                    state
                        .rpc_urls
                        .get(idx)
                        .map(|value| value.as_str())
                        .unwrap_or("<unknown>")
                );
                return Ok(price);
            }
            Ok(_) => {}
            Err(error) => warn!(
                "mint price fallback failed for {} on rpc slot {} ({}): {}",
                mint,
                idx,
                state
                    .rpc_urls
                    .get(idx)
                    .map(|value| value.as_str())
                    .unwrap_or("<unknown>"),
                error
            ),
        }
    }

    if let Some(price) = resolve_cached_price_for_route(state, mint, route).await {
        return Ok(price);
    }

    let message = last_error
        .map(|error| {
            format!(
                "failed to fetch price for market {} pool {} mint {}: {}",
                route.market.as_str(),
                route.pool,
                mint,
                error
            )
        })
        .unwrap_or_else(|| {
            format!(
                "failed to fetch price for market {} pool {} mint {}",
                route.market.as_str(),
                route.pool,
                mint
            )
        });
    Err(ApiError::new(StatusCode::BAD_GATEWAY, message))
}

async fn build_swaps_for_signer(
    state: &ApiState,
    signer: Arc<Keypair>,
    rpc_url_override: Option<&str>,
) -> Result<Swaps, ApiError> {
    if let Some(rpc_url) = non_empty_optional_str(rpc_url_override) {
        let base_offset = state.endpoint_cursor.fetch_add(1, AtomicOrdering::Relaxed);
        let (rpc, cluster) = resolve_request_rpc(state, base_offset, Some(rpc_url)).await?;
        return Ok(build_swaps_for_rpc_with_signer(
            rpc,
            cluster,
            signer,
            Some(state.swaps.pump_swap.shared_pool_state_cache()),
        ));
    }

    let rpc = state
        .rpc_clients
        .first()
        .cloned()
        .ok_or_else(|| ApiError::internal("missing primary rpc client"))?;
    Ok(build_swaps_for_rpc_with_signer(
        rpc,
        state.cluster,
        signer,
        Some(state.swaps.pump_swap.shared_pool_state_cache()),
    ))
}

fn resolve_swap_request_signer(
    state: &ApiState,
    wallet: Option<&str>,
) -> Result<Option<Arc<Keypair>>, ApiError> {
    if let Some(wallet) = wallet.map(str::trim).filter(|value| !value.is_empty()) {
        let pubkey =
            Pubkey::from_str(wallet).with_context(|| format!("invalid wallet pubkey: {wallet}"))?;
        if state.signer_configured
            && solana_signer::Signer::pubkey(state.swaps.pump_fun.keypair.as_ref()) == pubkey
        {
            return Ok(Some(state.swaps.pump_fun.keypair.clone()));
        }

        let signer = load_managed_wallet_runtime(state)
            .and_then(|runtime| runtime.signer_for(&pubkey))
            .ok_or_else(|| ApiError::bad_request(format!("wallet not found: {pubkey}")))?;
        return Ok(Some(signer));
    }

    if state.signer_configured {
        return Ok(None);
    }

    Ok(load_managed_wallet_runtime(state).and_then(|runtime| runtime.active_signer()))
}

async fn post_swap(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SwapRequest>,
) -> Result<Json<SwapResponse>, ApiError> {
    let execute = request.execute.unwrap_or(false);
    let slippage_pct = request.slippage_pct.unwrap_or(15.0);
    if !(1.0..=99.0).contains(&slippage_pct) {
        return Err(ApiError::bad_request(
            "slippage_pct must be in range 1..=99",
        ));
    }
    let priority_fee_override = parse_swap_priority_fee_override(
        request.priority_fee_level.as_deref(),
        request.priority_fee_sol,
    )?;

    let route_selection = resolve_swap_route(state.as_ref(), &request).await?;
    let route = route_selection.route;
    if request.skip_low_lq_pools.unwrap_or(false) && route_selection.low_lq {
        return Err(ApiError::conflict(format!(
            "selected {} pool {} is low liquidity ({:.6} SOL WSOL quote < {:.0} SOL threshold); rerun with skip_low_lq_pools=false to allow it",
            route.market.as_str(),
            route.pool,
            Swaps::lamports_to_sol(route_selection.liquidity.wsol_liquidity_raw),
            Swaps::low_lq_wsol_threshold_sol(),
        )));
    }
    let mut warning = route_selection.warning.clone();
    if matches!(&request.side, SwapSide::Buy)
        && let Some(requested_capacity_raw) =
            buy_sol_capacity_threshold_raw(request.buy_sol, slippage_pct)?
        && requested_capacity_raw > route_selection.liquidity.max_safe_buy_sol_raw
    {
        warning = append_warning(
            warning,
            format!(
                "requested buy {:.6} SOL exceeds estimated safe max {:.6} SOL for {} pool {}; build/send may still fail with low-liquidity errors",
                request.buy_sol.unwrap_or(0.0),
                Swaps::lamports_to_sol(route_selection.liquidity.max_safe_buy_sol_raw),
                route.market.as_str(),
                route.pool
            ),
        );
    }
    let price = match fetch_price_for_route_with_fallback(
        state.as_ref(),
        &request.mint,
        &route,
        request.rpc_url.as_deref(),
    )
    .await
    {
        Ok(price) => price,
        Err(error) if route.market == Market::PumpSwap => {
            warn!(
                "pump.swap price lookup failed for mint {} pool {}: {:?}; continuing without live price",
                request.mint, route.pool, error
            );
            resolve_cached_price_for_route(state.as_ref(), &request.mint, &route)
                .await
                .unwrap_or(0.0)
        }
        Err(error) => return Err(error),
    };
    let request_signer = if execute {
        resolve_swap_request_signer(state.as_ref(), request.wallet.as_deref())?
    } else {
        None
    };
    let request_swaps = match request_signer.clone() {
        Some(signer) => {
            Some(build_swaps_for_signer(state.as_ref(), signer, request.rpc_url.as_deref()).await?)
        }
        None => None,
    };
    let swap_client = request_swaps.as_ref().unwrap_or(state.swaps.as_ref());

    if execute {
        if !state.allow_live_sends {
            return Err(ApiError::conflict(
                "live sends are disabled (set MAMBA_API_ENABLE_LIVE_SENDS=true to unlock)",
            ));
        }
        if !api_signer_available(state.as_ref()) {
            return Err(ApiError::bad_request(
                "no signer is available for live execution (configure MAMBA_PRIVATE_KEY or create/select a managed wallet)",
            ));
        }
        if let Some(rpc_url) = non_empty_optional_str(request.rpc_url.as_deref()) {
            let base_offset = state.endpoint_cursor.fetch_add(1, AtomicOrdering::Relaxed);
            let (_, request_cluster) =
                resolve_request_rpc(state.as_ref(), base_offset, Some(rpc_url)).await?;
            enforce_live_send_cluster_match(state.as_ref(), Some(rpc_url), request_cluster)?;
        }
    }

    let mut success = true;
    let mut signature: Option<Signature> = None;
    let mut execution_error: Option<String> = None;
    let swqos_settings = request.swqos_settings.map(SWQoSettings::from);
    let use_swqos = request.use_swqos.unwrap_or(false);
    if use_swqos && swqos_settings.is_none() {
        return Err(ApiError::bad_request(
            "swqos_settings is required when use_swqos=true",
        ));
    }
    if let Some(settings) = swqos_settings.as_ref() {
        if use_swqos && settings.tip_lamports == 0 {
            return Err(ApiError::bad_request("swqos tip_lamports must be > 0"));
        }
        if use_swqos
            && settings.provider.requires_api_key()
            && settings.active_provider_key().is_none()
        {
            return Err(ApiError::bad_request(format!(
                "missing API key for {}",
                settings.provider.label()
            )));
        }
    }

    if execute {
        match request.side {
            SwapSide::Buy => {
                let buy_sol = request.buy_sol.unwrap_or(0.0);
                if buy_sol <= 0.0 {
                    return Err(ApiError::bad_request("buy_sol must be > 0 for buy side"));
                }

                let execution = swap_client
                    .buy_with_priority_fee_override(
                        &request.mint,
                        &route.pool.to_string(),
                        &route.creator.to_string(),
                        buy_sol,
                        slippage_pct,
                        price,
                        request.use_idempotent,
                        route.market,
                        priority_fee_override,
                        use_swqos,
                        swqos_settings.clone(),
                    )
                    .await?;

                success = execution.success;
                signature = execution.signature;
                execution_error = execution.error;
            }
            SwapSide::Sell => {
                let sell_pct = request.sell_pct.unwrap_or(100);
                if !(1..=100).contains(&sell_pct) {
                    return Err(ApiError::bad_request("sell_pct must be in range 1..=100"));
                }

                let retries = request.retries.unwrap_or(0);
                let execution = swap_client
                    .sell_with_priority_fee_override(
                        &request.mint,
                        &route.pool.to_string(),
                        &route.creator.to_string(),
                        sell_pct,
                        slippage_pct,
                        price,
                        route.market,
                        retries,
                        priority_fee_override,
                        use_swqos,
                        swqos_settings.clone(),
                    )
                    .await?;

                success = execution.success;
                signature = execution.signature;
                execution_error = execution.error;
            }
        }
    }

    if execute && !success && execution_error.is_none() {
        execution_error = Some("swap execution reported unsuccessful result".to_string());
    }

    if let Some(store) = &state.store {
        let market_cap = request
            .market_cap
            .unwrap_or_else(|| default_market_cap_from_price(price));
        store
            .record_transaction(&StoreTransaction {
                market: route.market.as_str().to_string(),
                mint: request.mint.trim().to_string(),
                pool: route.pool.to_string(),
                creator: route.creator.to_string(),
                creator_source: route.source.as_str().to_string(),
                side: match request.side {
                    SwapSide::Buy => "buy".to_string(),
                    SwapSide::Sell => "sell".to_string(),
                },
                slippage_pct,
                sol_amount: request.buy_sol,
                sell_pct: request.sell_pct,
                price,
                market_cap,
                executed: execute,
                success,
                signature: signature.map(|sig| sig.to_string()),
            })
            .await?;
    }

    Ok(Json(SwapResponse {
        dry_run: !execute,
        executed: execute,
        success,
        market: route.market.as_str().to_string(),
        pool: route.pool.to_string(),
        mint: request.mint.trim().to_string(),
        creator: route.creator.to_string(),
        creator_source: route.source.as_str().to_string(),
        price,
        low_lq: route_selection.low_lq,
        liquidity: RouteLiquidityResponse::from(route_selection.liquidity),
        signature: signature.map(|sig| sig.to_string()),
        error: execution_error,
        warning,
    }))
}

#[derive(Deserialize)]
struct CreatorQuery {
    min_mint_count: Option<i64>,
    min_avg_market_cap: Option<f64>,
    min_score: Option<f64>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct CreatorResponse {
    creator: String,
    mint_count: i64,
    avg_market_cap: f64,
    tx_count: i64,
    total_volume_sol: f64,
    score_raw: f64,
    score_normalized: f64,
    updated_at: String,
}

async fn get_creators(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CreatorQuery>,
) -> Result<Json<Vec<CreatorResponse>>, ApiError> {
    let min_mint_count = query.min_mint_count.unwrap_or(1).max(0);
    let min_avg_market_cap = query.min_avg_market_cap.unwrap_or(0.0).max(0.0);
    let min_score = query.min_score.unwrap_or(0.0).max(0.0);
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);

    if let Some(store) = state.store.as_ref() {
        match store
            .list_creators(min_mint_count, min_avg_market_cap, min_score, limit, offset)
            .await
        {
            Ok(store_rows) if !store_rows.is_empty() => {
                return Ok(Json(store_rows));
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    "creator store query failed, falling back to live cache: {}",
                    error
                );
            }
        }
    }

    let live_rows = list_live_creators(
        state.as_ref(),
        min_mint_count,
        min_avg_market_cap,
        min_score,
        limit,
        offset,
    )
    .await?;

    Ok(Json(live_rows))
}

#[derive(Default)]
struct LiveCreatorAccumulator {
    mints: HashSet<String>,
    market_cap_sum: f64,
    tx_count: i64,
    total_volume_sol: f64,
    last_activity_time: f64,
}

fn normalize_creator_scores(rows: &mut [CreatorResponse]) {
    let (min_raw, max_raw) = rows
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min_v, max_v), row| {
            (min_v.min(row.score_raw), max_v.max(row.score_raw))
        });

    for row in rows {
        row.score_normalized = if !min_raw.is_finite()
            || !max_raw.is_finite()
            || (max_raw - min_raw).abs() < f64::EPSILON
        {
            if row.score_raw > 0.0 { 100.0 } else { 0.0 }
        } else {
            ((row.score_raw - min_raw) / (max_raw - min_raw)) * 100.0
        };
    }
}

fn unix_seconds_to_rfc3339(seconds: f64) -> String {
    let millis_f64 = (seconds * 1000.0).round();
    if !millis_f64.is_finite() {
        return Utc::now().to_rfc3339();
    }

    if millis_f64 > i64::MAX as f64 || millis_f64 < i64::MIN as f64 {
        return Utc::now().to_rfc3339();
    }

    let millis = millis_f64 as i64;
    DateTime::<Utc>::from_timestamp_millis(millis)
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

async fn list_live_creators(
    state: &ApiState,
    min_mint_count: i64,
    min_avg_market_cap: f64,
    min_score: f64,
    limit: i64,
    offset: i64,
) -> Result<Vec<CreatorResponse>, ApiError> {
    let handlers = collect_active_market_handlers(state, None).await;
    let rows = collect_cached_mints_from_handlers(&handlers);
    let mut grouped: HashMap<String, LiveCreatorAccumulator> = HashMap::new();

    for (_, mint) in rows {
        let creator = mint.creator.to_string();
        if creator == Pubkey::default().to_string() {
            continue;
        }

        let entry = grouped.entry(creator).or_default();
        if entry.mints.insert(mint.mint.to_string()) {
            let market_cap = default_market_cap_from_price(effective_price_for_market_cap(&mint));
            entry.market_cap_sum += market_cap.max(0.0);
        }
        let mint_tx_count = mint
            .tx_count
            .max(mint.buys.saturating_add(mint.sells))
            .max(0);
        entry.tx_count = entry.tx_count.saturating_add(mint_tx_count);
        entry.total_volume_sol += mint.volume.max(0.0);
        entry.last_activity_time = entry.last_activity_time.max(mint.last_activity_time);
    }

    let mut creators = grouped
        .into_iter()
        .map(|(creator, agg)| {
            let mint_count = agg.mints.len() as i64;
            let avg_market_cap = if mint_count > 0 {
                agg.market_cap_sum / mint_count as f64
            } else {
                0.0
            };
            let score_raw = SCORE_WEIGHT_MINTS * (mint_count.max(0) as f64).ln_1p()
                + SCORE_WEIGHT_MARKET_CAP * avg_market_cap.max(0.0).ln_1p()
                + SCORE_WEIGHT_VOLUME * agg.total_volume_sol.max(0.0).ln_1p();

            CreatorResponse {
                creator,
                mint_count,
                avg_market_cap,
                tx_count: agg.tx_count.max(0),
                total_volume_sol: agg.total_volume_sol,
                score_raw,
                score_normalized: 0.0,
                updated_at: unix_seconds_to_rfc3339(agg.last_activity_time),
            }
        })
        .collect::<Vec<_>>();

    normalize_creator_scores(&mut creators);

    creators.retain(|row| {
        row.mint_count >= min_mint_count
            && row.avg_market_cap >= min_avg_market_cap
            && row.score_normalized >= min_score
    });

    creators.sort_by(|left, right| {
        compare_f64_desc(left.score_normalized, right.score_normalized)
            .then_with(|| right.mint_count.cmp(&left.mint_count))
            .then_with(|| compare_f64_desc(left.total_volume_sol, right.total_volume_sol))
    });

    let offset = offset.max(0) as usize;
    let limit = limit.max(1) as usize;

    Ok(creators.into_iter().skip(offset).take(limit).collect())
}

#[derive(Deserialize)]
struct TransactionQuery {
    creator: Option<String>,
    market: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct TransactionResponse {
    signature: Option<String>,
    market: String,
    mint: String,
    pool: String,
    creator: String,
    creator_source: String,
    side: String,
    slippage_pct: f64,
    sol_amount: Option<f64>,
    sell_pct: Option<i64>,
    price: f64,
    market_cap: f64,
    executed: bool,
    success: bool,
    created_at: String,
}

async fn get_transactions(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TransactionQuery>,
) -> Result<Json<Vec<TransactionResponse>>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let creator_filter = non_empty_optional(query.creator.clone());
    let market_filter = non_empty_optional(query.market.clone());

    if let Some(store) = state.store.as_ref() {
        match store
            .list_transactions(creator_filter.clone(), market_filter.clone(), limit, offset)
            .await
        {
            Ok(rows) if !rows.is_empty() => {
                return Ok(Json(rows));
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    "transaction store query failed, falling back to live cache: {}",
                    error
                );
            }
        }
    }

    let live_rows = list_live_transactions(
        state.as_ref(),
        creator_filter.as_deref(),
        market_filter.as_deref(),
        limit,
        offset,
    )
    .await?;

    Ok(Json(live_rows))
}

fn matches_optional_filter(value: &str, filter: Option<&str>) -> bool {
    match filter.map(str::trim).filter(|token| !token.is_empty()) {
        Some(token) => value.eq_ignore_ascii_case(token),
        None => true,
    }
}

async fn list_live_transactions(
    state: &ApiState,
    creator_filter: Option<&str>,
    market_filter: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<TransactionResponse>, ApiError> {
    let handlers = collect_active_market_handlers(state, None).await;
    let mut rows = collect_cached_mints_from_handlers(&handlers)
        .into_iter()
        .filter_map(|(market, mint)| {
            let creator = mint.creator.to_string();
            if creator == Pubkey::default().to_string() {
                return None;
            }

            if !matches_optional_filter(&creator, creator_filter) {
                return None;
            }

            let market_text = market.as_str().to_string();
            if !matches_optional_filter(&market_text, market_filter) {
                return None;
            }

            let side = if mint.buys > mint.sells {
                "buy".to_string()
            } else if mint.sells > mint.buys {
                "sell".to_string()
            } else if mint.buys > 0 || mint.sells > 0 {
                "mixed".to_string()
            } else {
                "unknown".to_string()
            };

            Some((
                mint.last_activity_time,
                TransactionResponse {
                    signature: None,
                    market: market_text,
                    mint: mint.mint.to_string(),
                    pool: mint.bonding_curve.to_string(),
                    creator,
                    creator_source: "live_cache".to_string(),
                    side,
                    slippage_pct: 0.0,
                    sol_amount: if mint.volume > 0.0 {
                        Some(mint.volume)
                    } else {
                        None
                    },
                    sell_pct: None,
                    price: mint.price,
                    market_cap: default_market_cap_from_price(effective_price_for_market_cap(
                        &mint,
                    )),
                    executed: false,
                    success: mint.buys > 0 || mint.sells > 0,
                    created_at: unix_seconds_to_rfc3339(mint.last_activity_time),
                },
            ))
        })
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| compare_f64_desc(left.0, right.0));
    let offset = offset.max(0) as usize;
    let limit = limit.max(1) as usize;

    Ok(rows
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(_, row)| row)
        .collect())
}

#[derive(Debug, Deserialize)]
struct CreatorMintQuery {
    creator: Option<String>,
    market: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct CreatorMintResponse {
    creator: String,
    market: String,
    mint: String,
    pool: String,
    name: String,
    symbol: String,
    uri: String,
    price: f64,
    market_cap: f64,
    liquidity: f64,
    volume: f64,
    buys: i64,
    sells: i64,
    tx_count: i64,
    holder_count: i64,
    created_time: f64,
    last_activity_time: f64,
    source: String,
}

async fn get_creator_mints(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CreatorMintQuery>,
) -> Result<Json<Vec<CreatorMintResponse>>, ApiError> {
    let creator = non_empty_optional(query.creator.clone())
        .ok_or_else(|| ApiError::bad_request("creator query parameter is required"))?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let market_filter = query
        .market
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(parse_market_or_bad_request)
        .transpose()?
        .map(|market| market.as_str().to_string());

    if let Some(store) = state.store.as_ref() {
        match store
            .list_creator_mints(creator.clone(), market_filter.clone(), limit, offset)
            .await
        {
            Ok(mut rows) if !rows.is_empty() => {
                enrich_creator_mints_from_live_cache(state.as_ref(), &mut rows).await;
                return Ok(Json(rows));
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    "creator-mints store query failed, falling back to live cache: {}",
                    error
                );
            }
        }
    }

    let live_rows = list_live_creator_mints(
        state.as_ref(),
        &creator,
        market_filter.as_deref(),
        limit,
        offset,
    )
    .await?;

    Ok(Json(live_rows))
}

fn creator_mint_from_live(market: Market, mint: Mint) -> CreatorMintResponse {
    let market_text = market.as_str().to_string();
    let creator = mint.creator.to_string();
    let mint_key = mint.mint.to_string();
    let market_cap = default_market_cap_from_price(effective_price_for_market_cap(&mint));
    let tx_count = mint
        .tx_count
        .max(mint.buys.saturating_add(mint.sells))
        .max(0);

    CreatorMintResponse {
        creator,
        market: market_text,
        mint: mint_key,
        pool: mint.bonding_curve.to_string(),
        name: mint.name,
        symbol: mint.symbol,
        uri: mint.uri,
        price: mint.price,
        market_cap,
        liquidity: mint.liquidity,
        volume: mint.volume.max(0.0),
        buys: mint.buys.max(0),
        sells: mint.sells.max(0),
        tx_count,
        holder_count: mint.holder_count.max(0),
        created_time: mint.created_time,
        last_activity_time: mint.last_activity_time,
        source: "live_cache".to_string(),
    }
}

async fn list_live_creator_mints(
    state: &ApiState,
    creator: &str,
    market_filter: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<CreatorMintResponse>, ApiError> {
    let market_filters = match market_filter {
        Some(market) => Some(vec![parse_market_or_bad_request(market)?]),
        None => None,
    };
    let handlers = collect_active_market_handlers(state, market_filters.as_deref()).await;
    let mut rows = collect_cached_mints_from_handlers(&handlers)
        .into_iter()
        .filter_map(|(market, mint)| {
            let row = creator_mint_from_live(market, mint);
            if !row.creator.eq_ignore_ascii_case(creator) {
                return None;
            }

            if !matches_optional_filter(&row.market, market_filter) {
                return None;
            }
            Some(row)
        })
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        compare_f64_desc(left.last_activity_time, right.last_activity_time)
            .then_with(|| right.tx_count.cmp(&left.tx_count))
    });
    let offset = offset.max(0) as usize;
    let limit = limit.max(1) as usize;

    Ok(rows.into_iter().skip(offset).take(limit).collect())
}

async fn enrich_creator_mints_from_live_cache(state: &ApiState, rows: &mut [CreatorMintResponse]) {
    if rows.is_empty() {
        return;
    }

    let creator_filters = rows
        .iter()
        .map(|row| row.creator.to_ascii_lowercase())
        .collect::<HashSet<_>>();

    let market_filters = rows
        .iter()
        .filter_map(|row| Market::from_token(&row.market))
        .collect::<Vec<_>>();

    let handlers = collect_active_market_handlers(
        state,
        if market_filters.is_empty() {
            None
        } else {
            Some(market_filters.as_slice())
        },
    )
    .await;

    if handlers.is_empty() {
        return;
    }

    let mut live_by_key = HashMap::<(String, String), CreatorMintResponse>::new();
    for (market, mint) in collect_cached_mints_from_handlers(&handlers) {
        let row = creator_mint_from_live(market, mint);
        if !creator_filters.contains(&row.creator.to_ascii_lowercase()) {
            continue;
        }

        let key = (row.market.clone(), row.mint.clone());
        if let Some(existing) = live_by_key.get_mut(&key) {
            if row.last_activity_time > existing.last_activity_time {
                *existing = row;
            }
        } else {
            live_by_key.insert(key, row);
        }
    }

    for row in rows.iter_mut() {
        let key = (row.market.clone(), row.mint.clone());
        let Some(live) = live_by_key.get(&key) else {
            continue;
        };

        if row.pool.trim().is_empty() {
            row.pool = live.pool.clone();
        }
        if row.name.trim().is_empty() {
            row.name = live.name.clone();
        }
        if row.symbol.trim().is_empty() {
            row.symbol = live.symbol.clone();
        }
        if row.uri.trim().is_empty() {
            row.uri = live.uri.clone();
        }
        if live.price > 0.0 {
            row.price = live.price;
        }
        row.market_cap = row.market_cap.max(live.market_cap);
        row.liquidity = row.liquidity.max(live.liquidity);
        row.volume = row.volume.max(live.volume);
        row.buys = row.buys.max(live.buys);
        row.sells = row.sells.max(live.sells);
        row.tx_count = row.tx_count.max(live.tx_count);
        row.holder_count = row.holder_count.max(live.holder_count);
        if row.created_time <= 0.0 {
            row.created_time = live.created_time;
        }
        row.last_activity_time = row.last_activity_time.max(live.last_activity_time);
        if row.source == "store" {
            row.source = "store+live_cache".to_string();
        }
    }

    rows.sort_by(|left, right| {
        compare_f64_desc(left.last_activity_time, right.last_activity_time)
            .then_with(|| right.tx_count.cmp(&left.tx_count))
    });
}

#[derive(Clone)]
struct ApiStore {
    pool: PgPool,
}

#[derive(Debug)]
struct StoreTransaction {
    signature: Option<String>,
    market: String,
    mint: String,
    pool: String,
    creator: String,
    creator_source: String,
    side: String,
    slippage_pct: f64,
    sol_amount: Option<f64>,
    sell_pct: Option<u64>,
    price: f64,
    market_cap: f64,
    executed: bool,
    success: bool,
}

#[derive(Debug)]
struct CreatorAggregate {
    creator: String,
    mint_count: i64,
    avg_market_cap: f64,
    tx_count: i64,
    total_volume_sol: f64,
    score_raw: f64,
    score_normalized: f64,
}

impl ApiStore {
    async fn new(database_url: &str) -> anyhow::Result<Self> {
        let pool = PgPool::connect(database_url).await?;
        let store = Self { pool };
        store.init_schema().await?;
        Ok(store)
    }

    async fn init_schema(&self) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS api_transactions (
                id BIGSERIAL PRIMARY KEY,
                signature TEXT,
                market TEXT NOT NULL,
                mint TEXT NOT NULL,
                pool TEXT NOT NULL,
                creator TEXT NOT NULL,
                creator_source TEXT NOT NULL,
                side TEXT NOT NULL,
                slippage_pct DOUBLE PRECISION NOT NULL,
                sol_amount DOUBLE PRECISION,
                sell_pct BIGINT,
                price DOUBLE PRECISION NOT NULL,
                market_cap DOUBLE PRECISION NOT NULL,
                executed BOOLEAN NOT NULL,
                success BOOLEAN NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_api_transactions_creator_created_at
             ON api_transactions (creator, created_at DESC)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_api_transactions_market_created_at
             ON api_transactions (market, created_at DESC)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS creator_mints (
                creator TEXT NOT NULL,
                market TEXT NOT NULL,
                mint TEXT NOT NULL,
                latest_market_cap DOUBLE PRECISION NOT NULL,
                first_seen TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                last_seen TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                PRIMARY KEY (creator, market, mint)
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS creators (
                creator TEXT PRIMARY KEY,
                mint_count BIGINT NOT NULL,
                avg_market_cap DOUBLE PRECISION NOT NULL,
                tx_count BIGINT NOT NULL,
                total_volume_sol DOUBLE PRECISION NOT NULL,
                score_raw DOUBLE PRECISION NOT NULL,
                score_normalized DOUBLE PRECISION NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn record_transaction(&self, transaction: &StoreTransaction) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO api_transactions (
                signature, market, mint, pool, creator, creator_source, side,
                slippage_pct, sol_amount, sell_pct, price, market_cap,
                executed, success
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7,
                $8, $9, $10, $11, $12,
                $13, $14
            )",
        )
        .bind(transaction.signature.as_deref())
        .bind(&transaction.market)
        .bind(&transaction.mint)
        .bind(&transaction.pool)
        .bind(&transaction.creator)
        .bind(&transaction.creator_source)
        .bind(&transaction.side)
        .bind(transaction.slippage_pct)
        .bind(transaction.sol_amount)
        .bind(transaction.sell_pct.map(|value| value as i64))
        .bind(transaction.price)
        .bind(transaction.market_cap)
        .bind(transaction.executed)
        .bind(transaction.success)
        .execute(&self.pool)
        .await?;

        if transaction.creator != Pubkey::default().to_string() {
            sqlx::query(
                "INSERT INTO creator_mints (creator, market, mint, latest_market_cap)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (creator, market, mint)
                 DO UPDATE SET
                    latest_market_cap = EXCLUDED.latest_market_cap,
                    last_seen = NOW()",
            )
            .bind(&transaction.creator)
            .bind(&transaction.market)
            .bind(&transaction.mint)
            .bind(transaction.market_cap)
            .execute(&self.pool)
            .await?;
        }

        self.refresh_creator_scores().await?;
        Ok(())
    }

    async fn refresh_creator_scores(&self) -> anyhow::Result<()> {
        let rows = sqlx::query(
            "SELECT
                cm.creator AS creator,
                COUNT(DISTINCT cm.mint)::BIGINT AS mint_count,
                COALESCE(AVG(cm.latest_market_cap), 0)::DOUBLE PRECISION AS avg_market_cap,
                COALESCE(tx.tx_count, 0)::BIGINT AS tx_count,
                COALESCE(tx.total_volume_sol, 0)::DOUBLE PRECISION AS total_volume_sol
             FROM creator_mints cm
             LEFT JOIN (
                SELECT
                    creator,
                    COUNT(*)::BIGINT AS tx_count,
                    COALESCE(SUM(COALESCE(sol_amount, 0)), 0)::DOUBLE PRECISION AS total_volume_sol
                FROM api_transactions
                WHERE success = TRUE
                GROUP BY creator
             ) tx ON tx.creator = cm.creator
             GROUP BY cm.creator, tx.tx_count, tx.total_volume_sol",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut aggregates = rows
            .into_iter()
            .map(|row| {
                let mint_count = row.try_get::<i64, _>("mint_count")?;
                let avg_market_cap = row.try_get::<f64, _>("avg_market_cap")?;
                let total_volume_sol = row.try_get::<f64, _>("total_volume_sol")?;
                let score_raw = SCORE_WEIGHT_MINTS * (mint_count.max(0) as f64).ln_1p()
                    + SCORE_WEIGHT_MARKET_CAP * avg_market_cap.max(0.0).ln_1p()
                    + SCORE_WEIGHT_VOLUME * total_volume_sol.max(0.0).ln_1p();

                Ok::<CreatorAggregate, sqlx::Error>(CreatorAggregate {
                    creator: row.try_get::<String, _>("creator")?,
                    mint_count,
                    avg_market_cap,
                    tx_count: row.try_get::<i64, _>("tx_count")?,
                    total_volume_sol,
                    score_raw,
                    score_normalized: 0.0,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let (min_raw, max_raw) = aggregates
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min_v, max_v), row| {
                (min_v.min(row.score_raw), max_v.max(row.score_raw))
            });

        for aggregate in &mut aggregates {
            aggregate.score_normalized = if !min_raw.is_finite()
                || !max_raw.is_finite()
                || (max_raw - min_raw).abs() < f64::EPSILON
            {
                if aggregate.score_raw > 0.0 {
                    100.0
                } else {
                    0.0
                }
            } else {
                ((aggregate.score_raw - min_raw) / (max_raw - min_raw)) * 100.0
            };
        }

        for aggregate in aggregates {
            sqlx::query(
                "INSERT INTO creators (
                    creator, mint_count, avg_market_cap, tx_count, total_volume_sol,
                    score_raw, score_normalized, updated_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
                ON CONFLICT (creator)
                DO UPDATE SET
                    mint_count = EXCLUDED.mint_count,
                    avg_market_cap = EXCLUDED.avg_market_cap,
                    tx_count = EXCLUDED.tx_count,
                    total_volume_sol = EXCLUDED.total_volume_sol,
                    score_raw = EXCLUDED.score_raw,
                    score_normalized = EXCLUDED.score_normalized,
                    updated_at = NOW()",
            )
            .bind(&aggregate.creator)
            .bind(aggregate.mint_count)
            .bind(aggregate.avg_market_cap)
            .bind(aggregate.tx_count)
            .bind(aggregate.total_volume_sol)
            .bind(aggregate.score_raw)
            .bind(aggregate.score_normalized)
            .execute(&self.pool)
            .await?;
        }

        sqlx::query(
            "DELETE FROM creators
             WHERE creator NOT IN (SELECT DISTINCT creator FROM creator_mints)",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn list_creators(
        &self,
        min_mint_count: i64,
        min_avg_market_cap: f64,
        min_score: f64,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<CreatorResponse>> {
        let rows = sqlx::query(
            "SELECT
                creator,
                mint_count,
                avg_market_cap,
                tx_count,
                total_volume_sol,
                score_raw,
                score_normalized,
                updated_at::TEXT AS updated_at
             FROM creators
             WHERE mint_count >= $1
               AND avg_market_cap >= $2
               AND score_normalized >= $3
             ORDER BY score_normalized DESC, mint_count DESC, avg_market_cap DESC
             LIMIT $4 OFFSET $5",
        )
        .bind(min_mint_count)
        .bind(min_avg_market_cap)
        .bind(min_score)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(CreatorResponse {
                    creator: row.try_get("creator")?,
                    mint_count: row.try_get("mint_count")?,
                    avg_market_cap: row.try_get("avg_market_cap")?,
                    tx_count: row.try_get("tx_count")?,
                    total_volume_sol: row.try_get("total_volume_sol")?,
                    score_raw: row.try_get("score_raw")?,
                    score_normalized: row.try_get("score_normalized")?,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(Into::into)
    }

    async fn list_transactions(
        &self,
        creator: Option<String>,
        market: Option<String>,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<TransactionResponse>> {
        let rows = sqlx::query(
            "SELECT
                signature,
                market,
                mint,
                pool,
                creator,
                creator_source,
                side,
                slippage_pct,
                sol_amount,
                sell_pct,
                price,
                market_cap,
                executed,
                success,
                created_at::TEXT AS created_at
             FROM api_transactions
             WHERE ($1::TEXT IS NULL OR creator = $1)
               AND ($2::TEXT IS NULL OR market = $2)
             ORDER BY api_transactions.created_at DESC
             LIMIT $3 OFFSET $4",
        )
        .bind(creator)
        .bind(market)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(TransactionResponse {
                    signature: row.try_get("signature")?,
                    market: row.try_get("market")?,
                    mint: row.try_get("mint")?,
                    pool: row.try_get("pool")?,
                    creator: row.try_get("creator")?,
                    creator_source: row.try_get("creator_source")?,
                    side: row.try_get("side")?,
                    slippage_pct: row.try_get("slippage_pct")?,
                    sol_amount: row.try_get("sol_amount")?,
                    sell_pct: row.try_get("sell_pct")?,
                    price: row.try_get("price")?,
                    market_cap: row.try_get("market_cap")?,
                    executed: row.try_get("executed")?,
                    success: row.try_get("success")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(Into::into)
    }

    async fn list_creator_mints(
        &self,
        creator: String,
        market: Option<String>,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<CreatorMintResponse>> {
        let rows = sqlx::query(
            "WITH tx_agg AS (
                SELECT
                    market,
                    mint,
                    COUNT(*)::BIGINT AS tx_count,
                    COALESCE(SUM(COALESCE(sol_amount, 0)), 0)::DOUBLE PRECISION AS volume,
                    COALESCE(SUM(CASE WHEN LOWER(side) = 'buy' THEN 1 ELSE 0 END), 0)::BIGINT AS buys,
                    COALESCE(SUM(CASE WHEN LOWER(side) = 'sell' THEN 1 ELSE 0 END), 0)::BIGINT AS sells,
                    COALESCE(MAX(EXTRACT(EPOCH FROM created_at)), 0)::DOUBLE PRECISION AS last_activity_time
                FROM api_transactions
                WHERE creator = $1
                  AND ($2::TEXT IS NULL OR market = $2)
                GROUP BY market, mint
            ),
            tx_latest AS (
                SELECT DISTINCT ON (market, mint)
                    market,
                    mint,
                    pool,
                    COALESCE(price, 0)::DOUBLE PRECISION AS price,
                    COALESCE(market_cap, 0)::DOUBLE PRECISION AS market_cap
                FROM api_transactions
                WHERE creator = $1
                  AND ($2::TEXT IS NULL OR market = $2)
                ORDER BY market, mint, created_at DESC
            )
            SELECT
                cm.creator AS creator,
                cm.market AS market,
                cm.mint AS mint,
                COALESCE(tl.pool, '') AS pool,
                COALESCE(tl.price, 0)::DOUBLE PRECISION AS price,
                CASE
                    WHEN COALESCE(tl.market_cap, 0) > 0 THEN tl.market_cap
                    ELSE COALESCE(cm.latest_market_cap, 0)
                END::DOUBLE PRECISION AS market_cap,
                COALESCE(ta.volume, 0)::DOUBLE PRECISION AS volume,
                COALESCE(ta.buys, 0)::BIGINT AS buys,
                COALESCE(ta.sells, 0)::BIGINT AS sells,
                COALESCE(ta.tx_count, 0)::BIGINT AS tx_count,
                EXTRACT(EPOCH FROM cm.first_seen)::DOUBLE PRECISION AS created_time,
                GREATEST(
                    EXTRACT(EPOCH FROM cm.last_seen)::DOUBLE PRECISION,
                    COALESCE(ta.last_activity_time, 0)
                )::DOUBLE PRECISION AS last_activity_time
             FROM creator_mints cm
             LEFT JOIN tx_agg ta
                ON ta.market = cm.market
               AND ta.mint = cm.mint
             LEFT JOIN tx_latest tl
                ON tl.market = cm.market
               AND tl.mint = cm.mint
             WHERE cm.creator = $1
               AND ($2::TEXT IS NULL OR cm.market = $2)
             ORDER BY last_activity_time DESC, cm.market ASC, cm.mint ASC
             LIMIT $3 OFFSET $4",
        )
        .bind(creator)
        .bind(market)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(CreatorMintResponse {
                    creator: row.try_get("creator")?,
                    market: row.try_get("market")?,
                    mint: row.try_get("mint")?,
                    pool: row.try_get("pool")?,
                    name: String::new(),
                    symbol: String::new(),
                    uri: String::new(),
                    price: row.try_get("price")?,
                    market_cap: row.try_get("market_cap")?,
                    liquidity: 0.0,
                    volume: row.try_get("volume")?,
                    buys: row.try_get("buys")?,
                    sells: row.try_get("sells")?,
                    tx_count: row.try_get("tx_count")?,
                    holder_count: 0,
                    created_time: row.try_get("created_time")?,
                    last_activity_time: row.try_get("last_activity_time")?,
                    source: "store".to_string(),
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_address::Address;
    use std::sync::Arc;

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }

    #[test]
    fn test_default_market_cap_from_price() {
        assert_eq!(default_market_cap_from_price(0.002), 2_000_000.0);
        assert_eq!(default_market_cap_from_price(-10.0), 0.0);
    }

    #[test]
    fn test_buy_sol_capacity_threshold_raw_applies_slippage() {
        assert_eq!(
            buy_sol_capacity_threshold_raw(Some(0.01), 15.0).expect("threshold"),
            Some(11_500_000)
        );
    }

    #[test]
    fn test_parse_market_filters_handles_single_and_csv() {
        let filters = parse_market_filters(Some("pump_swap"), Some("raydium_clmm,pump_swap"))
            .expect("market filters should parse")
            .expect("non-empty filter list expected");
        assert_eq!(filters, vec![Market::PumpSwap, Market::RaydiumClmm]);
    }

    #[test]
    fn test_parse_market_filters_rejects_unknown_market() {
        let err = parse_market_filters(Some("nope"), None).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_swap_response_serializes_error_field() {
        let body = serde_json::to_value(SwapResponse {
            dry_run: false,
            executed: true,
            success: false,
            market: "pump_fun".to_string(),
            pool: Pubkey::new_unique().to_string(),
            mint: Pubkey::new_unique().to_string(),
            creator: Pubkey::new_unique().to_string(),
            creator_source: CreatorResolutionSource::MarketStateFallback
                .as_str()
                .to_string(),
            price: 0.00000003,
            low_lq: false,
            liquidity: RouteLiquidityResponse::from(RouteLiquiditySnapshot::default()),
            signature: None,
            error: Some("send failed".to_string()),
            warning: None,
        })
        .expect("serialize swap response");
        assert_eq!(
            body.get("error").and_then(|value| value.as_str()),
            Some("send failed")
        );
    }

    #[test]
    fn test_resolve_swap_request_signer_accepts_configured_signer_wallet() {
        let signer = Arc::new(Keypair::new());
        let rpc = Arc::new(solana_client::nonblocking::rpc_client::RpcClient::new(
            "http://127.0.0.1:8899".to_string(),
        ));
        let sol_hook = crate::core::sol::SolHook::from_rpc_client_with_cluster(
            rpc.clone(),
            crate::core::cluster::SolanaCluster::Devnet,
        );
        let arc_sol_hook = Arc::new(sol_hook.clone());
        let swaps = Arc::new(Swaps::new(
            sol_hook,
            crate::dex::pump_swap::PumpSwap::new(signer.clone(), arc_sol_hook.clone()),
            crate::dex::pump_fun::PumpFun::new(signer.clone(), arc_sol_hook),
        ));
        let state = ApiState {
            swaps,
            ws_urls: Arc::new(Vec::new()),
            rpc_urls: Arc::new(vec!["http://127.0.0.1:8899".to_string()]),
            rpc_clients: Arc::new(vec![rpc]),
            rpc_cluster_cache: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::from(
                [(
                    "http://127.0.0.1:8899".to_string(),
                    crate::core::cluster::SolanaCluster::Devnet,
                )],
            ))),
            endpoint_cursor: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            cluster: crate::core::cluster::SolanaCluster::Devnet,
            store: None,
            subscriptions: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            api_key: "test-api-key".to_string(),
            allow_live_sends: false,
            allow_private_network_clients: false,
            signer_configured: true,
            wallet_store_path: None,
        };

        let signer_pubkey = solana_signer::Signer::pubkey(signer.as_ref());
        let resolved = resolve_swap_request_signer(&state, Some(&signer_pubkey.to_string()))
            .expect("configured signer should resolve")
            .expect("signer should be returned");

        assert_eq!(
            solana_signer::Signer::pubkey(resolved.as_ref()),
            signer_pubkey
        );
    }

    #[test]
    fn test_enforce_live_send_cluster_match_rejects_cross_cluster_override() {
        let signer = Arc::new(Keypair::new());
        let rpc = Arc::new(solana_client::nonblocking::rpc_client::RpcClient::new(
            "http://127.0.0.1:8899".to_string(),
        ));
        let sol_hook = crate::core::sol::SolHook::from_rpc_client_with_cluster(
            rpc.clone(),
            crate::core::cluster::SolanaCluster::Devnet,
        );
        let arc_sol_hook = Arc::new(sol_hook.clone());
        let swaps = Arc::new(Swaps::new(
            sol_hook,
            crate::dex::pump_swap::PumpSwap::new(signer.clone(), arc_sol_hook.clone()),
            crate::dex::pump_fun::PumpFun::new(signer, arc_sol_hook),
        ));
        let state = ApiState {
            swaps,
            ws_urls: Arc::new(Vec::new()),
            rpc_urls: Arc::new(vec!["http://127.0.0.1:8899".to_string()]),
            rpc_clients: Arc::new(vec![rpc]),
            rpc_cluster_cache: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::from(
                [(
                    "http://127.0.0.1:8899".to_string(),
                    crate::core::cluster::SolanaCluster::Devnet,
                )],
            ))),
            endpoint_cursor: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            cluster: crate::core::cluster::SolanaCluster::Devnet,
            store: None,
            subscriptions: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            api_key: "test-api-key".to_string(),
            allow_live_sends: true,
            allow_private_network_clients: false,
            signer_configured: true,
            wallet_store_path: None,
        };

        let err = enforce_live_send_cluster_match(
            &state,
            Some("https://api.mainnet-beta.solana.com"),
            crate::core::cluster::SolanaCluster::MainnetBeta,
        )
        .expect_err("cross-cluster override must be rejected");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(
            err.message
                .contains("cross-cluster live sends are not allowed"),
            "unexpected error: {}",
            err.message
        );
    }

    fn build_test_mint(key: Pubkey, created_time: f64, last_activity_time: f64) -> Mint {
        Mint {
            mint: Address::from(key.to_bytes()),
            bonding_curve: Pubkey::new_unique(),
            price: 0.0,
            highest_price: 0.0,
            name: String::new(),
            symbol: String::new(),
            uri: String::new(),
            creator: Pubkey::new_unique(),
            creator_sold: false,
            creator_token_amount: 0.0,
            buys: 0,
            sells: 0,
            tx_count: 0,
            volume: 0.0,
            liquidity: 0.0,
            is_migrated: false,
            migration_event: None,
            holder_count: 0,
            created_time,
            last_activity_time,
        }
    }

    #[test]
    fn test_sort_cached_mint_rows_freshest_first_prefers_last_activity_time() {
        let stale_key = Pubkey::new_unique();
        let fresh_key = Pubkey::new_unique();
        let mut rows = vec![
            (Market::PumpSwap, build_test_mint(stale_key, 100.0, 100.0)),
            (Market::PumpSwap, build_test_mint(fresh_key, 50.0, 999.0)),
        ];

        sort_cached_mint_rows_freshest_first(&mut rows);

        assert_eq!(rows[0].1.mint, Address::from(fresh_key.to_bytes()));
        assert_eq!(rows[1].1.mint, Address::from(stale_key.to_bytes()));
    }

    #[test]
    fn test_should_replace_cached_mint_prefers_migration_event() {
        let key = Pubkey::new_unique();
        let plain = build_test_mint(key, 500.0, 500.0);
        let mut migrated = build_test_mint(key, 10.0, 10.0);
        migrated.is_migrated = true;
        migrated.migration_event = Some(MigrationEvent {
            source_market: "pump_fun",
            target_market: "pump_swap",
            migration_signature: "sig-migrate".to_string(),
            migration_slot: 10,
            migration_time: 10.0,
            migration_confidence: MigrationConfidence::Confirmed,
        });

        assert!(should_replace_cached_mint(&plain, &migrated));
        assert!(!should_replace_cached_mint(&migrated, &plain));
    }

    #[test]
    fn test_merge_cached_mint_snapshot_prefers_confirmed_migration() {
        let key = Pubkey::new_unique();
        let mut existing_market = Market::RaydiumLaunchpad;
        let mut existing = build_test_mint(key, 100.0, 100.0);
        existing.is_migrated = true;
        existing.migration_event = Some(MigrationEvent {
            source_market: "raydium_launchpad",
            target_market: "raydium_cpmm",
            migration_signature: "sig-suspected".to_string(),
            migration_slot: 100,
            migration_time: 100.0,
            migration_confidence: MigrationConfidence::Suspected,
        });

        let mut candidate = build_test_mint(key, 20.0, 20.0);
        candidate.is_migrated = true;
        candidate.migration_event = Some(MigrationEvent {
            source_market: "raydium_launchpad",
            target_market: "raydium_cpmm",
            migration_signature: "sig-confirmed".to_string(),
            migration_slot: 50,
            migration_time: 50.0,
            migration_confidence: MigrationConfidence::Confirmed,
        });

        merge_cached_mint_snapshot(
            &mut existing_market,
            &mut existing,
            Market::RaydiumCpmm,
            &candidate,
        );

        assert_eq!(existing_market, Market::RaydiumCpmm);
        let migration = existing
            .migration_event
            .expect("merged mint should preserve migration event");
        assert_eq!(migration.migration_signature, "sig-confirmed");
        assert_eq!(
            migration.migration_confidence,
            MigrationConfidence::Confirmed
        );
    }

    #[test]
    fn test_merge_cached_mint_snapshot_keeps_non_decreasing_counters() {
        let key = Pubkey::new_unique();
        let mut existing_market = Market::PumpSwap;
        let mut existing = build_test_mint(key, 10.0, 10.0);
        existing.buys = 7;
        existing.sells = 5;
        existing.volume = 20.0;
        existing.holder_count = 4;

        let mut candidate = build_test_mint(key, 11.0, 11.0);
        candidate.buys = 1;
        candidate.sells = 0;
        candidate.volume = 1.0;
        candidate.holder_count = 0;

        merge_cached_mint_snapshot(
            &mut existing_market,
            &mut existing,
            Market::PumpFun,
            &candidate,
        );

        assert_eq!(existing.buys, 7);
        assert_eq!(existing.sells, 5);
        assert_eq!(existing.volume, 20.0);
        assert_eq!(existing.holder_count, 4);
    }

    #[test]
    fn test_merge_cached_mint_snapshot_keeps_first_market_without_migration() {
        let key = Pubkey::new_unique();
        let mut existing_market = Market::PumpSwap;
        let mut existing = build_test_mint(key, 10.0, 10.0);
        existing.name = "Pump".to_string();

        let mut candidate = build_test_mint(key, 20.0, 20.0);
        candidate.name = "Clmm".to_string();

        merge_cached_mint_snapshot(
            &mut existing_market,
            &mut existing,
            Market::RaydiumClmm,
            &candidate,
        );

        assert_eq!(existing_market, Market::PumpSwap);
        assert_eq!(existing.name, "Pump");
        assert_eq!(existing.last_activity_time, 20.0);
    }

    #[test]
    fn test_merge_cached_mint_snapshot_stabilizes_market_to_migration_target() {
        let key = Pubkey::new_unique();
        let mut existing_market = Market::PumpFun;
        let mut existing = build_test_mint(key, 10.0, 10.0);
        existing.is_migrated = true;
        existing.migration_event = Some(MigrationEvent {
            source_market: "pump_fun",
            target_market: "pump_swap",
            migration_signature: "sig-confirmed".to_string(),
            migration_slot: 10,
            migration_time: 10.0,
            migration_confidence: MigrationConfidence::Confirmed,
        });

        let candidate = build_test_mint(key, 12.0, 12.0);
        merge_cached_mint_snapshot(
            &mut existing_market,
            &mut existing,
            Market::PumpFun,
            &candidate,
        );

        assert_eq!(existing_market, Market::PumpSwap);
    }
}
