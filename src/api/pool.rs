use {
    super::{
        ApiError, ApiState, decode_versioned_transaction_base64, enforce_live_send_cluster_match,
        resolve_request_rpc, resolve_required_signers, rpc_client_for_attempt,
        sign_versioned_transaction, submit_signed_transaction,
    },
    crate::{
        core::{
            cluster::SolanaCluster,
            create::{
                ComputeBudgetPlan, DerivedAddresses, SOLANA_MAX_TX_WIRE_BYTES,
                compile_unsigned_v0_transaction, encode_transaction_base64,
            },
            pool::{
                MeteoraDammV1CreatePoolPlan, MeteoraDammV2CreatePoolPlan, MeteoraDbcCreatePoolPlan,
                MeteoraDlmmCreatePoolPlan, MeteoraDlmmSeedLiquidityPlan, PlannedPoolTx,
                PumpSwapCreatePoolPlan, RaydiumAmmV4CreatePoolPlan, RaydiumClmmCreatePoolPlan,
                RaydiumClmmSeedLiquidityPlan, RaydiumCpmmCreatePoolPlan,
                plan_meteora_damm_v1_create_pool, plan_meteora_damm_v2_create_pool,
                plan_meteora_dbc_create_pool, plan_meteora_dlmm_create_pool,
                plan_meteora_dlmm_seed_liquidity, plan_pump_swap_create_pool,
                plan_raydium_amm_v4_create_pool, plan_raydium_clmm_create_pool,
                plan_raydium_clmm_seed_liquidity, plan_raydium_cpmm_create_pool,
            },
            sol::{PriorityFeeLevel, SolHook, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT},
        },
        dex::{
            meteora_dbc::{
                DBC_VIRTUAL_POOL_ACCOUNT_LEN, DBC_VIRTUAL_POOL_CREATOR_OFFSET, METEORA_DBC_ID,
                MeteoraDbc, VIRTUAL_POOL_DISCRIM as DBC_VIRTUAL_POOL_DISCRIM,
            },
            meteora_dlmm::{
                LB_PAIR_ACCOUNT_LEN, LB_PAIR_CREATOR_OFFSET,
                LB_PAIR_DISCRIM as METEORA_DLMM_LB_PAIR_DISCRIM, METEORA_DLMM_ID, MeteoraDlmm,
            },
            pump_fun::PumpFun,
            pump_swap::{CREATOR_OFFSET as PUMP_SWAP_CREATOR_OFFSET, PUMP_SWAP_ID, PumpSwap},
            raydium_amm_v4::{
                AMM_V4_POOL_ACCOUNT_LEN, AMM_V4_POOL_OWNER_OFFSET, RAYDIUM_AMM_V4_DEVNET_ID,
                RAYDIUM_AMM_V4_ID, RaydiumAmmV4,
            },
            raydium_clmm::{
                CLMM_POOL_ACCOUNT_LEN, CLMM_POOL_OWNER_OFFSET, RAYDIUM_CLMM_DEVNET_ID,
                RAYDIUM_CLMM_ID, RaydiumClmm,
            },
            raydium_cpmm::{
                AMM_CONFIG_DISCRIM as RAYDIUM_CPMM_AMM_CONFIG_DISCRIM, CPMM_POOL_ACCOUNT_LEN,
                CPMM_POOL_POOL_CREATOR_OFFSET,
                POOL_STATE_DISCRIM as RAYDIUM_CPMM_POOL_STATE_DISCRIM, RAYDIUM_CPMM_DEVNET_ID,
                RAYDIUM_CPMM_ID, RaydiumCpmm,
            },
            raydium_launchpad::{
                LAUNCHPAD_POOL_ACCOUNT_LEN, LAUNCHPAD_POOL_CREATOR_OFFSET,
                POOL_STATE_DISCRIM as RAYDIUM_LAUNCHPAD_POOL_STATE_DISCRIM,
                RAYDIUM_LAUNCHPAD_DEVNET_ID, RAYDIUM_LAUNCHPAD_ID, RaydiumLaunchpad,
            },
        },
        swqos::{SWQoSettings, SwqosProvider, tip_account_for_provider},
        warn,
    },
    axum::{
        Json,
        extract::{Query, State},
        http::StatusCode,
    },
    meteora_dlmm_types as dlmm_idl,
    ruint::aliases::{U256, U512},
    serde::{Deserialize, Serialize},
    serde_json::{Value, json},
    solana_account_decoder_client_types::{UiAccountEncoding, UiDataSliceConfig},
    solana_client::{
        rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
        rpc_filter::{Memcmp, RpcFilterType},
    },
    solana_commitment_config::CommitmentConfig,
    solana_keypair::Keypair,
    solana_program::pubkey::Pubkey,
    solana_rpc_client_types::config::RpcSimulateTransactionConfig,
    solana_signer::Signer,
    solana_system_interface::instruction as system_instruction_if,
    std::{
        collections::{BTreeMap, BTreeSet, HashMap},
        str::FromStr,
        sync::Arc,
        time::Duration,
    },
};

const SWQOS_MAINNET_ONLY_ERROR: &str =
    "use_swqos is only supported on mainnet-beta sender infrastructure";

const HELIUS_PROGRAM_ACCOUNTS_PAGE_LIMIT: u64 = 1_000;

async fn resolve_pump_swap_coin_creator(
    sol: &SolHook,
    base_mint: &Pubkey,
    payer: Pubkey,
) -> Pubkey {
    if let Ok(Some(creator)) = sol.get_mint_first_creator(base_mint).await
        && creator != Pubkey::default()
    {
        return creator;
    }

    let pump_fun = PumpFun::new(Arc::new(Keypair::new()), Arc::new(sol.clone()));
    if let Ok(bonding_curve) = PumpFun::derive_bonding_curve(base_mint).await
        && let Ok(creator) = pump_fun.get_creator(&bonding_curve).await
        && creator != Pubkey::default()
    {
        return creator;
    }

    payer
}

fn is_helius_rpc_url(rpc_url: &str) -> bool {
    rpc_url
        .split('?')
        .next()
        .is_some_and(|base| base.contains("helius-rpc.com"))
}

fn should_retry_helius_paginated_program_accounts(error: &anyhow::Error, rpc_url: &str) -> bool {
    if !is_helius_rpc_url(rpc_url) {
        return false;
    }
    let message = error.to_string().to_ascii_lowercase();
    message.contains("too many accounts requested")
        || message.contains("request deprioritized")
        || message.contains("getprogramaccountsv2")
        || message.contains("pagination")
}

fn data_size_filter(len: usize) -> Value {
    json!({ "dataSize": len })
}

fn memcmp_pubkey_filter(offset: usize, pubkey: &Pubkey) -> Value {
    json!({
        "memcmp": {
            "offset": offset,
            "bytes": pubkey.to_string()
        }
    })
}

fn memcmp_bytes_filter(offset: usize, bytes: &[u8]) -> Value {
    json!({
        "memcmp": {
            "offset": offset,
            "bytes": bs58::encode(bytes).into_string()
        }
    })
}

async fn helius_get_program_account_pubkeys_v2(
    rpc_url: &str,
    program_id: Pubkey,
    filters: Vec<Value>,
) -> Result<Vec<Pubkey>, ApiError> {
    #[derive(Debug, Deserialize)]
    struct ProgramAccountsV2Response {
        result: Option<ProgramAccountsV2Result>,
        error: Option<ProgramAccountsV2Error>,
    }

    #[derive(Debug, Deserialize)]
    struct ProgramAccountsV2Result {
        accounts: Vec<ProgramAccountsV2Account>,
        #[serde(rename = "paginationKey")]
        pagination_key: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct ProgramAccountsV2Account {
        pubkey: String,
    }

    #[derive(Debug, Deserialize)]
    struct ProgramAccountsV2Error {
        code: i64,
        message: String,
    }

    let client = reqwest::Client::new();
    let mut pagination_key = None::<String>;
    let mut accounts = BTreeSet::new();

    loop {
        let mut options = json!({
            "commitment": "confirmed",
            "encoding": "base64",
            "dataSlice": {
                "offset": 0,
                "length": 0
            },
            "filters": filters,
            "limit": HELIUS_PROGRAM_ACCOUNTS_PAGE_LIMIT
        });
        if let Some(cursor) = pagination_key.as_ref() {
            options["paginationKey"] = json!(cursor);
        }

        let response = client
            .post(rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": "pool-discovery",
                "method": "getProgramAccountsV2",
                "params": [program_id.to_string(), options]
            }))
            .send()
            .await
            .map_err(|e| {
                ApiError::internal(format!("helius getProgramAccountsV2 request failed: {e}"))
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|e| {
            ApiError::internal(format!("helius getProgramAccountsV2 read body failed: {e}"))
        })?;
        if !status.is_success() {
            return Err(ApiError::internal(format!(
                "helius getProgramAccountsV2 http {status}: {body}"
            )));
        }

        let parsed: ProgramAccountsV2Response = serde_json::from_str(&body).map_err(|e| {
            ApiError::internal(format!(
                "helius getProgramAccountsV2 response decode failed: {e}"
            ))
        })?;
        if let Some(error) = parsed.error {
            return Err(ApiError::internal(format!(
                "helius getProgramAccountsV2 rpc error {}: {}",
                error.code, error.message
            )));
        }
        let result = parsed
            .result
            .ok_or_else(|| ApiError::internal("helius getProgramAccountsV2 missing result"))?;

        for account in result.accounts.iter() {
            let pubkey = account.pubkey.parse::<Pubkey>().map_err(|e| {
                ApiError::internal(format!(
                    "helius getProgramAccountsV2 returned invalid pubkey {}: {e}",
                    account.pubkey
                ))
            })?;
            accounts.insert(pubkey);
        }

        if result.accounts.is_empty() || result.pagination_key.is_none() {
            break;
        }
        pagination_key = result.pagination_key;
    }

    Ok(accounts.into_iter().collect())
}

async fn discover_program_accounts_resilient(
    market_label: &str,
    standard_result: Result<Vec<Pubkey>, anyhow::Error>,
    rpc_url: &str,
    program_id: Pubkey,
    filters: Vec<Value>,
) -> Result<Vec<Pubkey>, ApiError> {
    match standard_result {
        Ok(accounts) => Ok(accounts),
        Err(error) if should_retry_helius_paginated_program_accounts(&error, rpc_url) => {
            warn!(
                "{market_label} discovery hit RPC account limits; retrying with Helius getProgramAccountsV2 pagination"
            );
            helius_get_program_account_pubkeys_v2(rpc_url, program_id, filters).await
        }
        Err(error) => Err(ApiError::internal(format!(
            "{market_label} pool discovery failed: {error}"
        ))),
    }
}

fn is_retryable_pool_positions_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("429")
        || lower.contains("too many requests")
        || lower.contains("rate limit")
        || lower.contains("rate limited")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("temporarily unavailable")
        || lower.contains("connection")
}

fn is_rate_limited_pool_positions_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("429")
        || lower.contains("too many requests")
        || lower.contains("rate limit")
        || lower.contains("rate limited")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PoolMarket {
    PumpSwap,
    PumpFun,
    RaydiumAmmV4,
    RaydiumLaunchpad,
    RaydiumClmm,
    RaydiumCpmm,
    MeteoraDlmm,
    MeteoraDammV1,
    MeteoraDammV2,
    MeteoraDbc,
}

#[derive(Debug, Deserialize)]
pub(super) struct PoolBuildRequest {
    market: PoolMarket,
    payer: String,
    base_mint: Option<String>,
    quote_mint: Option<String>,

    // Optional UI strings. Some markets will require these.
    base_amount: Option<String>,
    quote_amount: Option<String>,
    initial_price: Option<String>,
    name: Option<String>,
    symbol: Option<String>,
    uri: Option<String>,

    // Market-specific knobs.
    pump_swap_index: Option<u16>,
    pump_swap_coin_creator: Option<String>,
    pump_swap_is_mayhem_mode: Option<bool>,

    raydium_cpmm_amm_config: Option<String>,
    raydium_cpmm_amm_config_index: Option<u16>,

    raydium_amm_v4_market: Option<String>,

    raydium_clmm_amm_config: Option<String>,
    raydium_clmm_tick_spacing: Option<u16>,
    raydium_clmm_position_nft_mint: Option<String>,

    meteora_dlmm_bin_step: Option<u16>,
    meteora_dlmm_base_fee_bps: Option<u16>,
    meteora_dlmm_activation_type: Option<u8>,
    meteora_dlmm_activation_point: Option<u64>,
    meteora_dlmm_has_alpha_vault: Option<bool>,
    meteora_dlmm_creator_pool_on_off_control: Option<bool>,
    meteora_dlmm_rounding: Option<String>,

    meteora_damm_v1_trade_fee_bps: Option<u64>,

    meteora_damm_v2_config: Option<String>,
    meteora_damm_v2_config_index: Option<u64>,
    meteora_damm_v2_position_nft_mint: Option<String>,

    meteora_dbc_config: Option<String>,
    meteora_dbc_config_index: Option<u64>,

    simulate: Option<bool>,
    rpc_url: Option<String>,
    priority_fee_level: Option<String>,
    compute_unit_limit: Option<u32>,
    use_swqos: Option<bool>,
    swqos_settings: Option<ApiBuildSwqosSettings>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiBuildSwqosSettings {
    provider: SwqosProvider,
    tip_lamports: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct PoolMethodSpec {
    market: &'static str,
    supported: bool,
    required_fields: Vec<&'static str>,
    optional_fields: Vec<&'static str>,
    execute_generated_fields: Vec<&'static str>,
    notes: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PoolBuildResponse {
    transaction: String,
    required_signers: Vec<String>,
    derived_addresses: BTreeMap<String, String>,
    simulation: Option<PoolSimulationResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PoolSimulationResponse {
    ok: bool,
    err: Option<String>,
    units_consumed: Option<u64>,
    logs: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PoolExecuteResponse {
    submitted: bool,
    success: bool,
    signature: Option<String>,
    error: Option<String>,
    cluster: String,
    generated_signers: BTreeMap<String, String>,
    build: PoolBuildResponse,
}

impl PoolMarket {
    fn as_str(self) -> &'static str {
        match self {
            Self::PumpSwap => "pump_swap",
            Self::PumpFun => "pump_fun",
            Self::RaydiumAmmV4 => "raydium_amm_v4",
            Self::RaydiumLaunchpad => "raydium_launchpad",
            Self::RaydiumClmm => "raydium_clmm",
            Self::RaydiumCpmm => "raydium_cpmm",
            Self::MeteoraDlmm => "meteora_dlmm",
            Self::MeteoraDammV1 => "meteora_damm_v1",
            Self::MeteoraDammV2 => "meteora_damm_v2",
            Self::MeteoraDbc => "meteora_dbc",
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct PoolPositionsQuery {
    owner: String,
    rpc_url: Option<String>,
    include_unsupported: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PoolManageBuildRequest {
    market: PoolMarket,
    owner: String,
    pool: String,
    withdraw_pct: Option<f64>,
    slippage_pct: Option<f64>,
    simulate: Option<bool>,
    rpc_url: Option<String>,
    priority_fee_level: Option<String>,
    compute_unit_limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(super) struct PoolPositionView {
    market: &'static str,
    pool: String,
    base_mint: String,
    quote_mint: String,
    lp_mint: Option<String>,
    owner_role: &'static str,
    owner_lp_balance_raw: Option<String>,
    owner_lp_balance_ui: Option<f64>,
    lp_decimals: Option<u8>,
    estimated_base_out_ui: Option<f64>,
    estimated_quote_out_ui: Option<f64>,
    withdraw_supported: bool,
    close_supported: bool,
    note: Option<String>,
}

pub(super) async fn get_methods() -> Result<Json<Vec<PoolMethodSpec>>, ApiError> {
    Ok(Json(vec![
        PoolMethodSpec {
            market: "pump_swap",
            supported: true,
            required_fields: vec![
                "market",
                "payer",
                "base_mint",
                "base_amount",
                "quote_amount",
            ],
            optional_fields: vec![
                "quote_mint",
                "pump_swap_index",
                "pump_swap_coin_creator",
                "pump_swap_is_mayhem_mode",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec![],
            notes: Some("Creates a PumpSwap pool and seeds initial liquidity from payer."),
        },
        PoolMethodSpec {
            market: "raydium_cpmm",
            supported: true,
            required_fields: vec![
                "market",
                "payer",
                "base_mint",
                "base_amount",
                "quote_amount",
            ],
            optional_fields: vec![
                "quote_mint",
                "raydium_cpmm_amm_config",
                "raydium_cpmm_amm_config_index",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec![],
            notes: Some("Initializes a Raydium CPMM pool (permissionless) with initial liquidity."),
        },
        PoolMethodSpec {
            market: "raydium_clmm",
            supported: true,
            required_fields: vec!["market", "payer", "base_mint"],
            optional_fields: vec![
                "quote_mint",
                "base_amount",
                "quote_amount",
                "initial_price",
                "raydium_clmm_amm_config",
                "raydium_clmm_tick_spacing",
                "raydium_clmm_position_nft_mint",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec!["raydium_clmm_position_nft_mint"],
            notes: Some(
                "Creates a Raydium CLMM pool. If `base_amount` or `quote_amount` is provided, also opens a position and seeds initial liquidity. Execute mode can generate `raydium_clmm_position_nft_mint` when omitted. If one side is missing, it is computed from `initial_price`.",
            ),
        },
        PoolMethodSpec {
            market: "meteora_dlmm",
            supported: true,
            required_fields: vec!["market", "payer", "base_mint"],
            optional_fields: vec![
                "quote_mint",
                "base_amount",
                "quote_amount",
                "initial_price",
                "meteora_dlmm_bin_step",
                "meteora_dlmm_base_fee_bps",
                "meteora_dlmm_activation_type",
                "meteora_dlmm_activation_point",
                "meteora_dlmm_has_alpha_vault",
                "meteora_dlmm_creator_pool_on_off_control",
                "meteora_dlmm_rounding",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec![],
            notes: Some(
                "Initializes a Meteora DLMM LB pair. If `base_amount`+`quote_amount` are provided, also seeds initial liquidity into the active bin.",
            ),
        },
        PoolMethodSpec {
            market: "meteora_damm_v1",
            supported: true,
            required_fields: vec![
                "market",
                "payer",
                "base_mint",
                "base_amount",
                "quote_amount",
            ],
            optional_fields: vec![
                "quote_mint",
                "meteora_damm_v1_trade_fee_bps",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec![],
            notes: Some(
                "Initializes a Meteora DAMM v1 pool with fee tier and bootstraps initial liquidity from payer.",
            ),
        },
        PoolMethodSpec {
            market: "pump_fun",
            supported: false,
            required_fields: vec!["market"],
            optional_fields: vec![],
            execute_generated_fields: vec![],
            notes: Some(
                "Pump.fun pool creation is coupled to token launch; use Create → Token (pump_fun).",
            ),
        },
        PoolMethodSpec {
            market: "raydium_launchpad",
            supported: false,
            required_fields: vec!["market"],
            optional_fields: vec![],
            execute_generated_fields: vec![],
            notes: Some(
                "Raydium Launchpad pool creation is coupled to token launch; use Create → Token (raydium_launchpad).",
            ),
        },
        PoolMethodSpec {
            market: "raydium_amm_v4",
            supported: true,
            required_fields: vec![
                "market",
                "payer",
                "raydium_amm_v4_market",
                "base_mint",
                "base_amount",
                "quote_amount",
            ],
            optional_fields: vec![
                "quote_mint",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec![],
            notes: Some(
                "Initializes a Raydium AMM v4 pool against an existing OpenBook market and seeds initial liquidity from payer.",
            ),
        },
        PoolMethodSpec {
            market: "meteora_damm_v2",
            supported: true,
            required_fields: vec![
                "market",
                "payer",
                "base_mint",
                "base_amount",
                "quote_amount",
                "initial_price",
                "meteora_damm_v2_position_nft_mint",
            ],
            optional_fields: vec![
                "quote_mint",
                "meteora_damm_v2_config",
                "meteora_damm_v2_config_index",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec!["meteora_damm_v2_position_nft_mint"],
            notes: Some(
                "Initializes a Meteora DAMM v2 pool with static config and seeds initial liquidity from payer. Execute mode can generate the extra position NFT mint signer when omitted.",
            ),
        },
        PoolMethodSpec {
            market: "meteora_dbc",
            supported: true,
            required_fields: vec!["market", "payer", "base_mint", "name", "symbol", "uri"],
            optional_fields: vec![
                "quote_mint",
                "meteora_dbc_config",
                "meteora_dbc_config_index",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec!["base_mint"],
            notes: Some(
                "Initializes a Meteora DBC virtual pool and base token mint. Build mode requires a fresh `base_mint` signer; execute mode can generate it internally when omitted.",
            ),
        },
    ]))
}

pub(super) async fn get_positions(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<PoolPositionsQuery>,
) -> Result<Json<Vec<PoolPositionView>>, ApiError> {
    let base_offset = state
        .endpoint_cursor
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let owner = parse_pubkey(&query.owner, "owner")?;
    let include_unsupported = query.include_unsupported.unwrap_or(false);
    let rpc_url_override = query
        .rpc_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let attempt_count = if rpc_url_override.is_some() {
        1
    } else {
        state.rpc_clients.len().max(3)
    };
    let mut last_error: Option<ApiError> = None;

    for attempt in 0..attempt_count {
        let (_rpc, sol, cluster, selected_rpc_url) =
            resolve_pool_rpc(state.as_ref(), base_offset, attempt, rpc_url_override).await?;
        match collect_pool_positions(sol, cluster, &selected_rpc_url, owner, include_unsupported)
            .await
        {
            Ok(rows) => return Ok(Json(rows)),
            Err(error) => {
                let message = error.message.clone();
                let retryable = is_retryable_pool_positions_error(&message);
                last_error = Some(error);
                if !retryable || attempt + 1 >= attempt_count {
                    break;
                }
                warn!(
                    "pool positions lookup retrying after transient rpc error on {selected_rpc_url} (attempt {}/{}): {}",
                    attempt + 1,
                    attempt_count,
                    message
                );
                tokio::time::sleep(Duration::from_millis(150 * (attempt as u64 + 1))).await;
            }
        }
    }

    let error = last_error.unwrap_or_else(|| ApiError::internal("pool positions lookup failed"));
    if is_rate_limited_pool_positions_error(&error.message) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("pool positions lookup rate-limited: {}", error.message),
        ));
    }
    Err(error)
}

async fn collect_pool_positions(
    sol: SolHook,
    cluster: SolanaCluster,
    selected_rpc_url: &str,
    owner: Pubkey,
    include_unsupported: bool,
) -> Result<Vec<PoolPositionView>, ApiError> {
    let sol_arc = Arc::new(sol.clone());
    let keypair = Arc::new(Keypair::new());
    let pump_swap = PumpSwap::new(keypair.clone(), sol_arc.clone());
    let raydium_amm_v4 = RaydiumAmmV4::new(keypair.clone(), sol_arc.clone());
    let raydium_clmm = RaydiumClmm::new(keypair.clone(), sol_arc.clone());
    let raydium_cpmm = RaydiumCpmm::new(keypair.clone(), sol_arc.clone());
    let meteora_dlmm = MeteoraDlmm::new(keypair.clone(), sol_arc.clone());
    let meteora_dbc = MeteoraDbc::new(keypair.clone(), sol_arc.clone());
    let raydium_launchpad = RaydiumLaunchpad::new(keypair, sol_arc);

    let mut rows = Vec::<PoolPositionView>::new();
    let mut seen = BTreeSet::<(String, String)>::new();
    let mut push_row = |row: PoolPositionView| {
        let key = (row.market.to_string(), row.pool.clone());
        if seen.insert(key) {
            rows.push(row);
        }
    };

    let pump_swap_pools = discover_program_accounts_resilient(
        "pump.swap",
        pump_swap.find_pools_by_creator(&owner).await,
        &selected_rpc_url,
        PUMP_SWAP_ID,
        vec![memcmp_pubkey_filter(PUMP_SWAP_CREATOR_OFFSET, &owner)],
    )
    .await?;
    for pool in pump_swap_pools {
        let Ok(pool_state) = pump_swap.fetch_state(&pool).await else {
            continue;
        };
        let lp_mint = Pubkey::new_from_array(pool_state.lp_mint.to_bytes());
        let base_mint = Pubkey::new_from_array(pool_state.base_mint.to_bytes());
        let quote_mint = Pubkey::new_from_array(pool_state.quote_mint.to_bytes());
        let lp_decimals = sol.get_token_decimals(&lp_mint).await.ok();
        let base_decimals = sol.get_token_decimals(&base_mint).await.ok();
        let quote_decimals = sol.get_token_decimals(&quote_mint).await.ok();
        let owner_balance_raw =
            owner_lp_balance_raw(&sol, &owner, &lp_mint, TOKEN_2022_PROGRAM_ID).await?;
        let (estimated_base_out_ui, estimated_quote_out_ui) = if owner_balance_raw > 0 {
            match pump_swap
                .estimate_withdraw_amounts_raw(&pool, owner_balance_raw)
                .await
            {
                Ok((_state, base_out, quote_out)) => (
                    base_decimals.and_then(|d| raw_amount_to_ui(base_out, d)),
                    quote_decimals.and_then(|d| raw_amount_to_ui(quote_out, d)),
                ),
                Err(_) => (None, None),
            }
        } else {
            (None, None)
        };
        push_row(PoolPositionView {
            market: PoolMarket::PumpSwap.as_str(),
            pool: pool.to_string(),
            base_mint: base_mint.to_string(),
            quote_mint: quote_mint.to_string(),
            lp_mint: Some(lp_mint.to_string()),
            owner_role: "creator",
            owner_lp_balance_raw: Some(owner_balance_raw.to_string()),
            owner_lp_balance_ui: lp_decimals.and_then(|d| raw_amount_to_ui(owner_balance_raw, d)),
            lp_decimals,
            estimated_base_out_ui,
            estimated_quote_out_ui,
            withdraw_supported: owner_balance_raw > 0,
            close_supported: false,
            note: Some(if owner_balance_raw > 0 {
                "full withdraw exits the LP position; the pool account remains open".to_string()
            } else {
                "wallet LP balance is zero".to_string()
            }),
        });
    }

    let raydium_cpmm_program_id = match cluster {
        SolanaCluster::Devnet => RAYDIUM_CPMM_DEVNET_ID,
        _ => RAYDIUM_CPMM_ID,
    };
    let raydium_cpmm_pools = discover_program_accounts_resilient(
        "raydium cpmm",
        raydium_cpmm.find_pools_by_creator(&owner).await,
        &selected_rpc_url,
        raydium_cpmm_program_id,
        vec![
            data_size_filter(CPMM_POOL_ACCOUNT_LEN),
            memcmp_bytes_filter(0, &RAYDIUM_CPMM_POOL_STATE_DISCRIM),
            memcmp_pubkey_filter(CPMM_POOL_POOL_CREATOR_OFFSET, &owner),
        ],
    )
    .await?;
    for pool in raydium_cpmm_pools {
        let Ok(pool_state) = raydium_cpmm.fetch_state(&pool).await else {
            continue;
        };
        let lp_mint = pool_state.lp_mint;
        let lp_decimals = Some(pool_state.lp_mint_decimals);
        let owner_balance_raw =
            owner_lp_balance_raw(&sol, &owner, &lp_mint, TOKEN_PROGRAM_ID).await?;
        let (estimated_base_out_ui, estimated_quote_out_ui) = if owner_balance_raw > 0 {
            match raydium_cpmm
                .estimate_withdraw_amounts_raw(&pool, owner_balance_raw)
                .await
            {
                Ok((_state, base_out, quote_out)) => (
                    raw_amount_to_ui(base_out, pool_state.mint_0_decimals),
                    raw_amount_to_ui(quote_out, pool_state.mint_1_decimals),
                ),
                Err(_) => (None, None),
            }
        } else {
            (None, None)
        };
        push_row(PoolPositionView {
            market: PoolMarket::RaydiumCpmm.as_str(),
            pool: pool.to_string(),
            base_mint: pool_state.token_0_mint.to_string(),
            quote_mint: pool_state.token_1_mint.to_string(),
            lp_mint: Some(lp_mint.to_string()),
            owner_role: "creator",
            owner_lp_balance_raw: Some(owner_balance_raw.to_string()),
            owner_lp_balance_ui: lp_decimals.and_then(|d| raw_amount_to_ui(owner_balance_raw, d)),
            lp_decimals,
            estimated_base_out_ui,
            estimated_quote_out_ui,
            withdraw_supported: owner_balance_raw > 0,
            close_supported: false,
            note: Some(if owner_balance_raw > 0 {
                "full withdraw exits the LP position; the pool account remains open".to_string()
            } else {
                "wallet LP balance is zero".to_string()
            }),
        });
    }

    let raydium_amm_v4_program_id = match cluster {
        SolanaCluster::Devnet => RAYDIUM_AMM_V4_DEVNET_ID,
        _ => RAYDIUM_AMM_V4_ID,
    };
    let raydium_amm_v4_pools = discover_program_accounts_resilient(
        "raydium amm v4",
        raydium_amm_v4.find_pools_by_owner(&owner).await,
        &selected_rpc_url,
        raydium_amm_v4_program_id,
        vec![
            data_size_filter(AMM_V4_POOL_ACCOUNT_LEN),
            memcmp_pubkey_filter(AMM_V4_POOL_OWNER_OFFSET, &owner),
        ],
    )
    .await?;
    for pool in raydium_amm_v4_pools {
        let Ok(pool_state) = raydium_amm_v4.fetch_state(&pool).await else {
            continue;
        };
        let lp_mint = pool_state.lp_mint;
        let lp_decimals = sol.get_token_decimals(&lp_mint).await.ok();
        let owner_balance_raw =
            owner_lp_balance_raw(&sol, &owner, &lp_mint, TOKEN_PROGRAM_ID).await?;
        let (estimated_base_out_ui, estimated_quote_out_ui, note_suffix) = if owner_balance_raw > 0
        {
            match raydium_amm_v4
                .estimate_withdraw_amounts_raw(&pool, owner_balance_raw)
                .await
            {
                Ok((_state, effective_lp_amount, base_out, quote_out)) => (
                    raw_amount_to_ui(
                        base_out,
                        u8::try_from(pool_state.base_decimals).unwrap_or_default(),
                    ),
                    raw_amount_to_ui(
                        quote_out,
                        u8::try_from(pool_state.quote_decimals).unwrap_or_default(),
                    ),
                    (effective_lp_amount < owner_balance_raw).then_some(
                        "raydium amm v4 keeps 1 LP unit locked for the pool".to_string(),
                    ),
                ),
                Err(_) => (None, None, None),
            }
        } else {
            (None, None, None)
        };
        push_row(PoolPositionView {
            market: PoolMarket::RaydiumAmmV4.as_str(),
            pool: pool.to_string(),
            base_mint: pool_state.base_mint.to_string(),
            quote_mint: pool_state.quote_mint.to_string(),
            lp_mint: Some(lp_mint.to_string()),
            owner_role: "creator",
            owner_lp_balance_raw: Some(owner_balance_raw.to_string()),
            owner_lp_balance_ui: lp_decimals.and_then(|d| raw_amount_to_ui(owner_balance_raw, d)),
            lp_decimals,
            estimated_base_out_ui,
            estimated_quote_out_ui,
            withdraw_supported: owner_balance_raw > 0,
            close_supported: false,
            note: Some(if owner_balance_raw > 0 {
                note_suffix.unwrap_or_else(|| {
                    "full withdraw exits the LP position; the pool account remains open".to_string()
                })
            } else {
                "wallet LP balance is zero".to_string()
            }),
        });
    }

    if include_unsupported {
        let raydium_clmm_program_id = match cluster {
            SolanaCluster::Devnet => RAYDIUM_CLMM_DEVNET_ID,
            _ => RAYDIUM_CLMM_ID,
        };
        let raydium_clmm_pools = discover_program_accounts_resilient(
            "raydium clmm",
            raydium_clmm.find_pools_by_owner(&owner).await,
            &selected_rpc_url,
            raydium_clmm_program_id,
            vec![
                data_size_filter(CLMM_POOL_ACCOUNT_LEN),
                memcmp_pubkey_filter(CLMM_POOL_OWNER_OFFSET, &owner),
            ],
        )
        .await?;
        for pool in raydium_clmm_pools {
            let Ok(pool_state) = raydium_clmm.fetch_state(&pool).await else {
                continue;
            };
            push_row(PoolPositionView {
                market: PoolMarket::RaydiumClmm.as_str(),
                pool: pool.to_string(),
                base_mint: pool_state.mint_a.to_string(),
                quote_mint: pool_state.mint_b.to_string(),
                lp_mint: None,
                owner_role: "creator",
                owner_lp_balance_raw: None,
                owner_lp_balance_ui: None,
                lp_decimals: None,
                estimated_base_out_ui: None,
                estimated_quote_out_ui: None,
                withdraw_supported: false,
                close_supported: false,
                note: Some(
                    "concentrated-position management is not wired into the builder yet"
                        .to_string(),
                ),
            });
        }

        let raydium_launchpad_program_id = match cluster {
            SolanaCluster::Devnet => RAYDIUM_LAUNCHPAD_DEVNET_ID,
            _ => RAYDIUM_LAUNCHPAD_ID,
        };
        let raydium_launchpad_pools = discover_program_accounts_resilient(
            "raydium launchpad",
            raydium_launchpad.find_pools_by_creator(&owner).await,
            &selected_rpc_url,
            raydium_launchpad_program_id,
            vec![
                data_size_filter(LAUNCHPAD_POOL_ACCOUNT_LEN),
                memcmp_bytes_filter(0, &RAYDIUM_LAUNCHPAD_POOL_STATE_DISCRIM),
                memcmp_pubkey_filter(LAUNCHPAD_POOL_CREATOR_OFFSET, &owner),
            ],
        )
        .await?;
        for pool in raydium_launchpad_pools {
            let Ok(pool_state) = raydium_launchpad.fetch_state(&pool).await else {
                continue;
            };
            push_row(PoolPositionView {
                market: PoolMarket::RaydiumLaunchpad.as_str(),
                pool: pool.to_string(),
                base_mint: pool_state.base_mint.to_string(),
                quote_mint: pool_state.quote_mint.to_string(),
                lp_mint: None,
                owner_role: "creator",
                owner_lp_balance_raw: None,
                owner_lp_balance_ui: None,
                lp_decimals: None,
                estimated_base_out_ui: None,
                estimated_quote_out_ui: None,
                withdraw_supported: false,
                close_supported: false,
                note: Some(
                    "launchpad pool management stays coupled to launchpad-specific trade flows"
                        .to_string(),
                ),
            });
        }

        let meteora_dlmm_pools = discover_program_accounts_resilient(
            "meteora dlmm",
            meteora_dlmm.find_pools_by_creator(&owner).await,
            &selected_rpc_url,
            METEORA_DLMM_ID,
            vec![
                data_size_filter(LB_PAIR_ACCOUNT_LEN),
                memcmp_bytes_filter(0, &METEORA_DLMM_LB_PAIR_DISCRIM),
                memcmp_pubkey_filter(LB_PAIR_CREATOR_OFFSET, &owner),
            ],
        )
        .await?;
        for pool in meteora_dlmm_pools {
            let Ok(pool_state) = meteora_dlmm.fetch_state(&pool).await else {
                continue;
            };
            push_row(PoolPositionView {
                market: PoolMarket::MeteoraDlmm.as_str(),
                pool: pool.to_string(),
                base_mint: pool_state.token_x_mint.to_string(),
                quote_mint: pool_state.token_y_mint.to_string(),
                lp_mint: None,
                owner_role: "creator",
                owner_lp_balance_raw: None,
                owner_lp_balance_ui: None,
                lp_decimals: None,
                estimated_base_out_ui: None,
                estimated_quote_out_ui: None,
                withdraw_supported: false,
                close_supported: false,
                note: Some(
                    "dlmm position close/remove-liquidity builders are not wired into mamba_api yet"
                        .to_string(),
                ),
            });
        }

        let meteora_dbc_pools = discover_program_accounts_resilient(
            "meteora dbc",
            meteora_dbc.find_pools_by_creator(&owner).await,
            &selected_rpc_url,
            METEORA_DBC_ID,
            vec![
                data_size_filter(DBC_VIRTUAL_POOL_ACCOUNT_LEN),
                memcmp_bytes_filter(0, &DBC_VIRTUAL_POOL_DISCRIM),
                memcmp_pubkey_filter(DBC_VIRTUAL_POOL_CREATOR_OFFSET, &owner),
            ],
        )
        .await?;
        for pool in meteora_dbc_pools {
            let Ok(pool_state) = meteora_dbc.fetch_state(&pool).await else {
                continue;
            };
            push_row(PoolPositionView {
                market: PoolMarket::MeteoraDbc.as_str(),
                pool: pool.to_string(),
                base_mint: pool_state.virtual_pool.base_mint.to_string(),
                quote_mint: pool_state.config.quote_mint.to_string(),
                lp_mint: None,
                owner_role: "creator",
                owner_lp_balance_raw: None,
                owner_lp_balance_ui: None,
                lp_decimals: None,
                estimated_base_out_ui: None,
                estimated_quote_out_ui: None,
                withdraw_supported: false,
                close_supported: false,
                note: Some(
                    "dbc liquidity management remains gated by vesting/migration rules in this pass"
                        .to_string(),
                ),
            });
        }
    }

    rows.sort_by(|left, right| {
        right
            .withdraw_supported
            .cmp(&left.withdraw_supported)
            .then_with(|| left.market.cmp(right.market))
            .then_with(|| left.pool.cmp(&right.pool))
    });
    Ok(rows)
}

async fn build_manage_response(
    state: &ApiState,
    req: PoolManageBuildRequest,
) -> Result<
    (
        PoolBuildResponse,
        Arc<solana_client::nonblocking::rpc_client::RpcClient>,
        SolanaCluster,
    ),
    ApiError,
> {
    let base_offset = state
        .endpoint_cursor
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let owner = parse_pubkey(&req.owner, "owner")?;
    let pool = parse_pubkey(&req.pool, "pool")?;
    let simulate = req.simulate.unwrap_or(true);
    let withdraw_pct = req.withdraw_pct.unwrap_or(100.0);
    let slippage_pct = req.slippage_pct.unwrap_or(5.0);
    let rpc_url_override = req
        .rpc_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let (rpc, sol, cluster, _selected_rpc_url) =
        resolve_pool_rpc(state, base_offset, 0, rpc_url_override).await?;

    let sol_arc = Arc::new(sol.clone());
    let keypair = Arc::new(Keypair::new());
    let pump_swap = PumpSwap::new(keypair.clone(), sol_arc.clone());
    let raydium_amm_v4 = RaydiumAmmV4::new(keypair.clone(), sol_arc.clone());
    let raydium_cpmm = RaydiumCpmm::new(keypair, sol_arc);

    let mut planned = match req.market {
        PoolMarket::PumpSwap => {
            let pool_state = pump_swap
                .fetch_state(&pool)
                .await
                .map_err(|e| ApiError::internal(format!("pump.swap fetch_state failed: {e}")))?;
            let lp_mint = Pubkey::new_from_array(pool_state.lp_mint.to_bytes());
            let owner_balance_raw =
                owner_lp_balance_raw(&sol, &owner, &lp_mint, TOKEN_2022_PROGRAM_ID).await?;
            let lp_token_amount = withdraw_amount_from_balance(owner_balance_raw, withdraw_pct)?;
            let (_state, base_out, quote_out) = pump_swap
                .estimate_withdraw_amounts_raw(&pool, lp_token_amount)
                .await
                .map_err(|e| ApiError::internal(format!("pump.swap withdraw quote failed: {e}")))?;
            let (instructions, pool_state, _base_out, _quote_out) = pump_swap
                .withdraw_for_user(
                    &owner,
                    &pool,
                    lp_token_amount,
                    min_after_slippage(base_out, slippage_pct),
                    min_after_slippage(quote_out, slippage_pct),
                )
                .await
                .map_err(|e| ApiError::internal(format!("pump.swap withdraw build failed: {e}")))?;
            PlannedPoolTx {
                payer: owner,
                required_signers: vec![owner],
                derived: DerivedAddresses::new().insert("pool", pool).insert(
                    "lp_mint",
                    Pubkey::new_from_array(pool_state.lp_mint.to_bytes()),
                ),
                priority_fee_addresses: vec![PUMP_SWAP_ID, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID],
                instructions,
            }
        }
        PoolMarket::RaydiumCpmm => {
            let pool_state = raydium_cpmm
                .fetch_state(&pool)
                .await
                .map_err(|e| ApiError::internal(format!("raydium cpmm fetch_state failed: {e}")))?;
            let owner_balance_raw =
                owner_lp_balance_raw(&sol, &owner, &pool_state.lp_mint, TOKEN_PROGRAM_ID).await?;
            let lp_token_amount = withdraw_amount_from_balance(owner_balance_raw, withdraw_pct)?;
            let (_state, token_0_out, token_1_out) = raydium_cpmm
                .estimate_withdraw_amounts_raw(&pool, lp_token_amount)
                .await
                .map_err(|e| {
                    ApiError::internal(format!("raydium cpmm withdraw quote failed: {e}"))
                })?;
            let (instructions, pool_state, _token_0_out, _token_1_out) = raydium_cpmm
                .withdraw_for_user(
                    &owner,
                    &pool,
                    lp_token_amount,
                    min_after_slippage(token_0_out, slippage_pct),
                    min_after_slippage(token_1_out, slippage_pct),
                )
                .await
                .map_err(|e| {
                    ApiError::internal(format!("raydium cpmm withdraw build failed: {e}"))
                })?;
            PlannedPoolTx {
                payer: owner,
                required_signers: vec![owner],
                derived: DerivedAddresses::new()
                    .insert("pool", pool)
                    .insert("lp_mint", pool_state.lp_mint),
                priority_fee_addresses: vec![
                    pool_state.lp_mint,
                    pool_state.token_0_mint,
                    pool_state.token_1_mint,
                ],
                instructions,
            }
        }
        PoolMarket::RaydiumAmmV4 => {
            let pool_state = raydium_amm_v4.fetch_state(&pool).await.map_err(|e| {
                ApiError::internal(format!("raydium amm v4 fetch_state failed: {e}"))
            })?;
            let owner_balance_raw =
                owner_lp_balance_raw(&sol, &owner, &pool_state.lp_mint, TOKEN_PROGRAM_ID).await?;
            let requested_lp_amount =
                withdraw_amount_from_balance(owner_balance_raw, withdraw_pct)?;
            let (_state, effective_lp_amount, base_out, quote_out) = raydium_amm_v4
                .estimate_withdraw_amounts_raw(&pool, requested_lp_amount)
                .await
                .map_err(|e| {
                    ApiError::internal(format!("raydium amm v4 withdraw quote failed: {e}"))
                })?;
            let (instructions, pool_state, _effective_lp_amount, _base_out, _quote_out) =
                raydium_amm_v4
                    .withdraw_for_user(
                        &owner,
                        &pool,
                        requested_lp_amount,
                        min_after_slippage(base_out, slippage_pct),
                        min_after_slippage(quote_out, slippage_pct),
                    )
                    .await
                    .map_err(|e| {
                        ApiError::internal(format!("raydium amm v4 withdraw build failed: {e}"))
                    })?;
            let _ = effective_lp_amount;
            PlannedPoolTx {
                payer: owner,
                required_signers: vec![owner],
                derived: DerivedAddresses::new()
                    .insert("pool", pool)
                    .insert("lp_mint", pool_state.lp_mint),
                priority_fee_addresses: vec![
                    pool_state.lp_mint,
                    pool_state.base_mint,
                    pool_state.quote_mint,
                ],
                instructions,
            }
        }
        _ => {
            return Err(ApiError::bad_request(format!(
                "pool management build is not supported yet for {}",
                req.market.as_str()
            )));
        }
    };

    let compute_budget = ComputeBudgetPlan {
        compute_unit_limit: req.compute_unit_limit,
        compute_unit_price_micro_lamports: None,
    };
    compute_budget.prepend_instructions(&mut planned.instructions);
    let priority_fee_level = req
        .priority_fee_level
        .as_deref()
        .and_then(parse_priority_fee_level);
    apply_priority_fee_if_requested(&sol, &mut planned, priority_fee_level).await?;

    Ok((
        build_pool_tx_response(&sol, planned, simulate).await?,
        rpc,
        cluster,
    ))
}

pub(super) async fn post_manage_build(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<PoolManageBuildRequest>,
) -> Result<Json<PoolBuildResponse>, ApiError> {
    let (response, _, _) = build_manage_response(state.as_ref(), req).await?;
    Ok(Json(response))
}

pub(super) async fn post_manage_execute(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<PoolManageBuildRequest>,
) -> Result<Json<PoolExecuteResponse>, ApiError> {
    if !state.allow_live_sends {
        return Err(ApiError::conflict(
            "live sends are disabled (set MAMBA_API_ENABLE_LIVE_SENDS=true to unlock)",
        ));
    }

    let rpc_url_override = req.rpc_url.clone();
    let (build, rpc, cluster) = build_manage_response(state.as_ref(), req).await?;
    enforce_live_send_cluster_match(state.as_ref(), rpc_url_override.as_deref(), cluster)?;
    if let Some(simulation) = build.simulation.as_ref()
        && !simulation.ok
    {
        return Ok(Json(PoolExecuteResponse {
            submitted: false,
            success: false,
            signature: None,
            error: Some(
                simulation
                    .err
                    .clone()
                    .unwrap_or_else(|| "simulation failed".to_string()),
            ),
            cluster: format!("{cluster:?}"),
            generated_signers: BTreeMap::new(),
            build,
        }));
    }

    let signers =
        resolve_required_signers(state.as_ref(), &build.required_signers, &HashMap::new())?;
    let unsigned = decode_versioned_transaction_base64(&build.transaction)?;
    let signed = sign_versioned_transaction(&unsigned, &signers)?;

    match submit_signed_transaction(rpc, cluster, &signed, false, None).await {
        Ok(signature) => Ok(Json(PoolExecuteResponse {
            submitted: true,
            success: true,
            signature: Some(signature.to_string()),
            error: None,
            cluster: format!("{cluster:?}"),
            generated_signers: BTreeMap::new(),
            build,
        })),
        Err(error) => Ok(Json(PoolExecuteResponse {
            submitted: false,
            success: false,
            signature: None,
            error: Some(error.message),
            cluster: format!("{cluster:?}"),
            generated_signers: BTreeMap::new(),
            build,
        })),
    }
}

fn parse_pubkey(raw: &str, label: &str) -> Result<Pubkey, ApiError> {
    Pubkey::from_str(raw.trim())
        .map_err(|e| ApiError::bad_request(format!("invalid {label} pubkey: {e}")))
}

fn parse_ui_amount_to_raw(raw: &str, decimals: u8, label: &str) -> Result<u64, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::bad_request(format!("{label} is empty")));
    }
    if text.starts_with('-') {
        return Err(ApiError::bad_request(format!("{label} must be >= 0")));
    }

    let (whole, frac) = match text.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (text, None),
    };
    let whole = if whole.is_empty() { "0" } else { whole };
    if !whole.chars().all(|c| c.is_ascii_digit())
        || frac.is_some_and(|f| !f.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(ApiError::bad_request(format!(
            "{label} must be a decimal string"
        )));
    }

    let whole_val: u64 = whole
        .parse()
        .map_err(|_| ApiError::bad_request(format!("invalid {label}")))?;
    let scale = 10u64
        .checked_pow(decimals as u32)
        .ok_or_else(|| ApiError::bad_request(format!("{label} decimals overflow")))?;

    let mut raw_val = whole_val
        .checked_mul(scale)
        .ok_or_else(|| ApiError::bad_request(format!("{label} overflow")))?;

    if let Some(frac) = frac {
        if frac.len() > decimals as usize {
            return Err(ApiError::bad_request(format!(
                "{label} has too many decimal places (max {decimals})"
            )));
        }
        let mut frac_digits = frac.to_string();
        while frac_digits.len() < decimals as usize {
            frac_digits.push('0');
        }
        let frac_val: u64 = if frac_digits.is_empty() {
            0
        } else {
            frac_digits
                .parse()
                .map_err(|_| ApiError::bad_request(format!("invalid {label} fractional part")))?
        };
        raw_val = raw_val
            .checked_add(frac_val)
            .ok_or_else(|| ApiError::bad_request(format!("{label} overflow")))?;
    }

    Ok(raw_val)
}

fn build_meteora_dlmm_customizable_params(
    active_id: i32,
    bin_step: u16,
    base_factor: u16,
    activation_type: u8,
    has_alpha_vault: bool,
    activation_point: Option<u64>,
    creator_pool_on_off_control: bool,
    base_fee_power_factor: u8,
) -> Result<dlmm_idl::CustomizableParams, ApiError> {
    let mut data = Vec::with_capacity(82);
    data.extend_from_slice(&active_id.to_le_bytes());
    data.extend_from_slice(&bin_step.to_le_bytes());
    data.extend_from_slice(&base_factor.to_le_bytes());
    data.push(activation_type);
    data.push(u8::from(has_alpha_vault));
    match activation_point {
        Some(value) => {
            data.push(1);
            data.extend_from_slice(&value.to_le_bytes());
        }
        None => data.push(0),
    }
    data.push(u8::from(creator_pool_on_off_control));
    data.push(base_fee_power_factor);
    data.extend_from_slice(&[0u8; 62]);

    <dlmm_idl::CustomizableParams as anchor_lang::AnchorDeserialize>::try_from_slice(&data).map_err(
        |error| {
            ApiError::internal(format!(
                "failed to build meteora dlmm customizable params: {error}"
            ))
        },
    )
}

async fn resolve_pool_rpc(
    state: &ApiState,
    base_offset: usize,
    attempt: usize,
    rpc_url_override: Option<&str>,
) -> Result<
    (
        Arc<solana_client::nonblocking::rpc_client::RpcClient>,
        SolHook,
        crate::core::cluster::SolanaCluster,
        String,
    ),
    ApiError,
> {
    let (rpc, selected_rpc_url) = if let Some(url) = rpc_url_override {
        let url = url.to_string();
        (
            Arc::new(solana_client::nonblocking::rpc_client::RpcClient::new(
                url.clone(),
            )),
            url,
        )
    } else {
        let rpc_count = state.rpc_clients.len();
        if rpc_count == 0 {
            return Err(ApiError::internal(
                "rpc client selection failed: no configured RPC clients",
            ));
        }
        let index = (base_offset + attempt) % rpc_count;
        let selected_rpc_url = state.rpc_urls.get(index).cloned().ok_or_else(|| {
            ApiError::internal("rpc client selection failed: missing configured RPC url")
        })?;
        (state.rpc_clients[index].clone(), selected_rpc_url)
    };

    let seed_cluster = if rpc_url_override.is_some() {
        crate::core::cluster::SolanaCluster::Unknown
    } else {
        state.cluster
    };
    let seed_sol = SolHook::from_rpc_client_with_cluster(rpc.clone(), seed_cluster);
    let cluster = if rpc_url_override.is_some() {
        seed_sol
            .detect_cluster()
            .await
            .map_err(|e| ApiError::internal(format!("rpc getGenesisHash failed: {e}")))?
    } else {
        state.cluster
    };
    let sol = SolHook::from_rpc_client_with_cluster(rpc.clone(), cluster);
    Ok((rpc, sol, cluster, selected_rpc_url))
}

fn raw_amount_to_ui(raw: u64, decimals: u8) -> Option<f64> {
    let value = (raw as f64) / 10_f64.powi(decimals as i32);
    if value.is_finite() { Some(value) } else { None }
}

fn min_after_slippage(expected_raw: u64, slippage_pct: f64) -> u64 {
    let pct = if slippage_pct > 1.0 {
        slippage_pct / 100.0
    } else {
        slippage_pct
    }
    .clamp(0.0, 0.99);
    ((expected_raw as f64) * (1.0 - pct)).floor().max(0.0) as u64
}

fn withdraw_amount_from_balance(balance_raw: u64, withdraw_pct: f64) -> Result<u64, ApiError> {
    if balance_raw == 0 {
        return Err(ApiError::bad_request("owner LP balance is zero"));
    }
    if !withdraw_pct.is_finite() || withdraw_pct <= 0.0 || withdraw_pct > 100.0 {
        return Err(ApiError::bad_request("withdraw_pct must be > 0 and <= 100"));
    }
    let amount = ((balance_raw as f64) * (withdraw_pct / 100.0)).floor() as u64;
    Ok(amount.max(1).min(balance_raw))
}

async fn owner_lp_balance_raw(
    sol: &SolHook,
    owner: &Pubkey,
    lp_mint: &Pubkey,
    token_program: Pubkey,
) -> Result<u64, ApiError> {
    let ata = if token_program == TOKEN_2022_PROGRAM_ID {
        sol.get_ata_for_token2022(owner, lp_mint)
    } else {
        sol.get_ata_for_token(owner, lp_mint)
    };
    match sol.get_token_balance_raw_from_ata(&ata).await {
        Ok((raw, _decimals)) => Ok(raw),
        Err(_) => Ok(0),
    }
}

async fn build_pool_tx_response(
    sol: &SolHook,
    planned: PlannedPoolTx,
    simulate: bool,
) -> Result<PoolBuildResponse, ApiError> {
    let blockhash = sol
        .get_latest_blockhash_with_commitment_resilient(CommitmentConfig::processed())
        .await
        .map_err(|e| ApiError::internal(format!("rpc blockhash fetch failed: {e}")))?
        .0;
    let tx = compile_unsigned_v0_transaction(&planned.payer, &planned.instructions, blockhash)
        .map_err(|e| ApiError::internal(format!("compile transaction failed: {e}")))?;
    let b64 = encode_transaction_base64(&tx)
        .map_err(|e| ApiError::internal(format!("encode transaction failed: {e}")))?;

    let simulation = if simulate {
        let sim = sol
            .simulate_transaction_with_config_resilient(
                &tx,
                RpcSimulateTransactionConfig {
                    sig_verify: false,
                    replace_recent_blockhash: true,
                    commitment: Some(CommitmentConfig::processed()),
                    ..RpcSimulateTransactionConfig::default()
                },
            )
            .await
            .map_err(|e| ApiError::internal(format!("rpc simulation failed: {e}")))?;
        Some(PoolSimulationResponse {
            ok: sim.value.err.is_none(),
            err: sim.value.err.map(|e| format!("{e:?}")),
            units_consumed: sim.value.units_consumed,
            logs: sim.value.logs.unwrap_or_default(),
        })
    } else {
        None
    };

    Ok(PoolBuildResponse {
        transaction: b64,
        required_signers: planned
            .required_signers
            .iter()
            .map(|pk| pk.to_string())
            .collect(),
        derived_addresses: planned
            .derived
            .map
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect(),
        simulation,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DlmmRounding {
    Down,
    Up,
    None,
}

fn parse_dlmm_rounding(raw: Option<&str>) -> Result<DlmmRounding, ApiError> {
    match raw.map(|v| v.trim().to_ascii_lowercase()) {
        None => Ok(DlmmRounding::Down),
        Some(v) if v == "down" => Ok(DlmmRounding::Down),
        Some(v) if v == "up" => Ok(DlmmRounding::Up),
        Some(v) if v == "none" || v == "exact" => Ok(DlmmRounding::None),
        Some(v) => Err(ApiError::bad_request(format!(
            "invalid meteora_dlmm_rounding: {v} (expected down|up|none)"
        ))),
    }
}

fn compute_dlmm_base_factor(bin_step: u16, fee_bps: u16) -> Result<(u16, u8), ApiError> {
    if bin_step == 0 {
        return Err(ApiError::bad_request("meteora_dlmm_bin_step must be > 0"));
    }
    let computed = (fee_bps as f64) * 10_000.0 / (bin_step as f64);
    if !computed.is_finite() || computed <= 0.0 {
        return Err(ApiError::bad_request(
            "invalid meteora dlmm base fee parameters",
        ));
    }
    if computed > (u16::MAX as f64) {
        let mut truncated = computed;
        let mut power = 0u8;
        loop {
            if truncated < (u16::MAX as f64) {
                break;
            }
            let remainder = truncated % 10.0;
            if remainder != 0.0 {
                return Err(ApiError::bad_request(
                    "meteora dlmm base_fee_bps/bin_step produced decimals",
                ));
            }
            power = power.saturating_add(1);
            truncated /= 10.0;
        }
        Ok((truncated as u16, power))
    } else {
        let casted = (computed as u16) as f64;
        if casted != computed {
            return Err(ApiError::bad_request(
                "meteora dlmm base_fee_bps/bin_step produced decimals",
            ));
        }
        Ok((computed as u16, 0))
    }
}

fn compute_dlmm_active_id(
    bin_step: u16,
    price_per_lamport: f64,
    rounding: DlmmRounding,
) -> Result<i32, ApiError> {
    if bin_step == 0 {
        return Err(ApiError::bad_request("meteora_dlmm_bin_step must be > 0"));
    }
    if !price_per_lamport.is_finite() || price_per_lamport <= 0.0 {
        return Err(ApiError::bad_request(
            "meteora_dlmm initial_price must be > 0",
        ));
    }
    let bps = (bin_step as f64) / 10_000.0;
    let base = 1.0 + bps;
    let exact = price_per_lamport.log10() / base.log10();
    if !exact.is_finite() {
        return Err(ApiError::bad_request(
            "meteora dlmm price produced invalid bin id",
        ));
    }
    let id = match rounding {
        DlmmRounding::Down => exact.floor(),
        DlmmRounding::Up => exact.ceil(),
        DlmmRounding::None => {
            let rounded = exact.round();
            if (exact - rounded).abs() > 1e-9 {
                return Err(ApiError::bad_request(
                    "meteora dlmm rounding=none requires exact bin id for price",
                ));
            }
            rounded
        }
    };
    if id < (i32::MIN as f64) || id > (i32::MAX as f64) {
        return Err(ApiError::bad_request("meteora dlmm active_id out of range"));
    }
    Ok(id as i32)
}

async fn pick_raydium_cpmm_amm_config(
    sol: &SolHook,
    program_id: &Pubkey,
    maybe_pubkey: Option<&Pubkey>,
    maybe_index: Option<u16>,
) -> Result<Pubkey, ApiError> {
    if let Some(pk) = maybe_pubkey {
        let acc = sol
            .get_account_with_commitment_resilient(pk, CommitmentConfig::processed())
            .await
            .map_err(|e| ApiError::internal(format!("rpc getAccount(amm_config) failed: {e}")))?;
        if acc.owner != *program_id {
            return Err(ApiError::bad_request(format!(
                "raydium cpmm amm_config owner mismatch: {}",
                acc.owner
            )));
        }
        if acc.data.len() < 8 || acc.data[..8] != RAYDIUM_CPMM_AMM_CONFIG_DISCRIM {
            return Err(ApiError::bad_request(
                "raydium cpmm amm_config discriminator mismatch",
            ));
        }
        return Ok(*pk);
    }

    let accounts = sol
        .get_program_ui_accounts_with_config_resilient(
            program_id,
            RpcProgramAccountsConfig {
                filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    0,
                    RAYDIUM_CPMM_AMM_CONFIG_DISCRIM.as_ref(),
                ))]),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    commitment: Some(CommitmentConfig::processed()),
                    ..Default::default()
                },
                with_context: None,
                sort_results: None,
            },
        )
        .await
        .map_err(|e| {
            ApiError::internal(format!("rpc getProgramAccounts(amm_config) failed: {e}"))
        })?;

    let mut candidates: Vec<(u16, Pubkey, bool)> = Vec::new(); // (index, pubkey, disable_create_pool)
    for (pk, ui_acc) in accounts {
        let bytes = ui_acc
            .data
            .decode()
            .ok_or_else(|| ApiError::internal(format!("failed to decode account data for {pk}")))?;
        if bytes.len() < 12 {
            continue;
        }
        let disable_create_pool = bytes.get(9).copied().unwrap_or(1) != 0;
        let index = u16::from_le_bytes([bytes[10], bytes[11]]);
        candidates.push((index, pk, disable_create_pool));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    if let Some(index) = maybe_index {
        let pk = candidates
            .iter()
            .find(|(idx, _pk, disabled)| *idx == index && !*disabled)
            .map(|(_idx, pk, _)| *pk)
            .ok_or_else(|| {
                ApiError::not_found("raydium cpmm amm_config not found for requested index")
            })?;
        return Ok(pk);
    }

    candidates
        .iter()
        .find(|(_idx, _pk, disabled)| !*disabled)
        .map(|(_idx, pk, _)| *pk)
        .ok_or_else(|| ApiError::not_found("raydium cpmm no permissionless amm_config found"))
}

async fn pick_raydium_clmm_amm_config(
    sol: &SolHook,
    program_id: &Pubkey,
    maybe_pubkey: Option<&Pubkey>,
    tick_spacing: u16,
    strict_tick_spacing: bool,
) -> Result<Pubkey, ApiError> {
    if let Some(pk) = maybe_pubkey {
        let acc = sol
            .get_account_with_commitment_resilient(pk, CommitmentConfig::processed())
            .await
            .map_err(|e| ApiError::internal(format!("rpc getAccount(amm_config) failed: {e}")))?;
        if acc.owner != *program_id {
            return Err(ApiError::bad_request(format!(
                "raydium clmm amm_config owner mismatch: {}",
                acc.owner
            )));
        }
        return Ok(*pk);
    }

    // Prefer the curated Raydium CLMM config list on mainnet (fast and RPC-friendly). Fall back to
    // on-chain discovery if the list is unavailable/empty (e.g. different cluster).
    if *program_id == crate::dex::raydium_clmm::RAYDIUM_CLMM_ID
        && let Ok(accounts) = sol
            .get_multiple_accounts_resilient(&crate::dex::raydium_clmm::AMM_CONFIGS)
            .await
    {
        let mut candidates: Vec<(u16, Pubkey)> = Vec::new();
        for (pk, acc_opt) in crate::dex::raydium_clmm::AMM_CONFIGS
            .iter()
            .copied()
            .zip(accounts.into_iter())
        {
            let Some(acc) = acc_opt else {
                continue;
            };
            if acc.owner != *program_id {
                continue;
            }
            if let Ok(ts) = crate::dex::raydium_clmm::decode_amm_config_tick_spacing(&acc.data) {
                candidates.push((ts, pk));
            }
        }

        if !candidates.is_empty() {
            let mut matches = candidates
                .iter()
                .filter(|(ts, _)| *ts == tick_spacing)
                .map(|(_ts, pk)| *pk)
                .collect::<Vec<_>>();
            if !matches.is_empty() {
                matches.sort_by_key(|pk| pk.to_bytes());
                return Ok(matches[0]);
            }

            let mut spacings = candidates.iter().map(|(ts, _)| *ts).collect::<Vec<_>>();
            spacings.sort_unstable();
            spacings.dedup();
            let available = if spacings.is_empty() {
                "none".to_string()
            } else {
                spacings
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            if strict_tick_spacing {
                return Err(ApiError::not_found(format!(
                    "raydium clmm amm_config not found for requested tick_spacing={tick_spacing} (available: {available})"
                )));
            }

            candidates.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then_with(|| a.1.to_bytes().cmp(&b.1.to_bytes()))
            });
            return Ok(candidates[0].1);
        }
    }

    let discrim = crate::dex::raydium_clmm::AMM_CONFIG_DISCRIM;
    let tick_spacing_offset = crate::dex::raydium_clmm::AMM_CONFIG_TICK_SPACING_OFFSET;
    let slice_len = tick_spacing_offset + 2;

    let account_config = RpcAccountInfoConfig {
        encoding: Some(UiAccountEncoding::Base64),
        commitment: Some(CommitmentConfig::processed()),
        min_context_slot: None,
        data_slice: Some(UiDataSliceConfig {
            offset: 0,
            length: slice_len,
        }),
    };
    let discrim_filter = RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, discrim.as_ref()));

    // Fast-path: query only configs that match the requested tick spacing.
    let tick_bytes = tick_spacing.to_le_bytes();
    let tick_filter =
        RpcFilterType::Memcmp(Memcmp::new_base58_encoded(tick_spacing_offset, &tick_bytes));
    let exact_accounts = sol
        .get_program_ui_accounts_with_config_resilient(
            program_id,
            RpcProgramAccountsConfig {
                filters: Some(vec![discrim_filter.clone(), tick_filter]),
                account_config: account_config.clone(),
                with_context: None,
                sort_results: None,
            },
        )
        .await
        .map_err(|e| {
            ApiError::internal(format!("rpc getProgramAccounts(amm_config) failed: {e}"))
        })?;

    if !exact_accounts.is_empty() {
        let mut matches = exact_accounts
            .into_iter()
            .map(|(pk, _)| pk)
            .collect::<Vec<_>>();
        matches.sort_by_key(|pk| pk.to_bytes());
        return Ok(matches[0]);
    }

    if strict_tick_spacing {
        // Nothing matched the requested tick spacing; list available spacings to help the user.
        let accounts = sol
            .get_program_ui_accounts_with_config_resilient(
                program_id,
                RpcProgramAccountsConfig {
                    filters: Some(vec![discrim_filter]),
                    account_config,
                    with_context: None,
                    sort_results: None,
                },
            )
            .await
            .map_err(|e| {
                ApiError::internal(format!("rpc getProgramAccounts(amm_config) failed: {e}"))
            })?;

        let mut spacings = Vec::<u16>::new();
        for (pk, ui_acc) in accounts {
            let bytes = ui_acc.data.decode().ok_or_else(|| {
                ApiError::internal(format!("failed to decode account data for {pk}"))
            })?;
            if let Ok(ts) = crate::dex::raydium_clmm::decode_amm_config_tick_spacing(&bytes) {
                spacings.push(ts);
            }
        }
        spacings.sort_unstable();
        spacings.dedup();
        let available = if spacings.is_empty() {
            "none".to_string()
        } else {
            spacings
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(ApiError::not_found(format!(
            "raydium clmm amm_config not found for requested tick_spacing={tick_spacing} (available: {available})"
        )));
    }

    // Non-strict: pick the first matching tick spacing (deterministic by pubkey), or fall back to
    // the lowest available tick spacing.
    let accounts = sol
        .get_program_ui_accounts_with_config_resilient(
            program_id,
            RpcProgramAccountsConfig {
                filters: Some(vec![discrim_filter]),
                account_config,
                with_context: None,
                sort_results: None,
            },
        )
        .await
        .map_err(|e| {
            ApiError::internal(format!("rpc getProgramAccounts(amm_config) failed: {e}"))
        })?;

    let mut matches: Vec<Pubkey> = Vec::new();
    let mut candidates: Vec<(u16, Pubkey)> = Vec::new();
    for (pk, ui_acc) in accounts {
        let bytes = ui_acc
            .data
            .decode()
            .ok_or_else(|| ApiError::internal(format!("failed to decode account data for {pk}")))?;
        if let Ok(ts) = crate::dex::raydium_clmm::decode_amm_config_tick_spacing(&bytes) {
            if ts == tick_spacing {
                matches.push(pk);
            }
            candidates.push((ts, pk));
        }
    }

    if !matches.is_empty() {
        matches.sort_by_key(|pk| pk.to_bytes());
        return Ok(matches[0]);
    }

    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.to_bytes().cmp(&b.1.to_bytes()))
    });
    candidates
        .first()
        .map(|(_ts, pk)| *pk)
        .ok_or_else(|| ApiError::not_found("raydium clmm no amm_config accounts found"))
}

fn read_pubkey_at(data: &[u8], offset: usize, label: &str) -> Result<Pubkey, ApiError> {
    let slice = data.get(offset..offset + 32).ok_or_else(|| {
        ApiError::bad_request(format!("missing {label} bytes at offset {offset}"))
    })?;
    let arr: [u8; 32] = slice
        .try_into()
        .map_err(|_| ApiError::bad_request(format!("invalid {label} bytes")))?;
    Ok(Pubkey::new_from_array(arr))
}

fn read_u8_at(data: &[u8], offset: usize, label: &str) -> Result<u8, ApiError> {
    data.get(offset)
        .copied()
        .ok_or_else(|| ApiError::bad_request(format!("missing {label} byte at offset {offset}")))
}

fn read_u64_at(data: &[u8], offset: usize, label: &str) -> Result<u64, ApiError> {
    let slice = data.get(offset..offset + 8).ok_or_else(|| {
        ApiError::bad_request(format!("missing {label} bytes at offset {offset}"))
    })?;
    let arr: [u8; 8] = slice
        .try_into()
        .map_err(|_| ApiError::bad_request(format!("invalid {label} bytes")))?;
    Ok(u64::from_le_bytes(arr))
}

fn read_u128_at(data: &[u8], offset: usize, label: &str) -> Result<u128, ApiError> {
    let slice = data.get(offset..offset + 16).ok_or_else(|| {
        ApiError::bad_request(format!("missing {label} bytes at offset {offset}"))
    })?;
    let arr: [u8; 16] = slice
        .try_into()
        .map_err(|_| ApiError::bad_request(format!("invalid {label} bytes")))?;
    Ok(u128::from_le_bytes(arr))
}

#[derive(Debug, Clone, Copy)]
struct MeteoraDammV2ConfigInfo {
    pool_creator_authority: Pubkey,
    config_type: u8,
    index: u64,
    sqrt_min_price: u128,
    sqrt_max_price: u128,
}

fn parse_meteora_damm_v2_config_info(data: &[u8]) -> Result<MeteoraDammV2ConfigInfo, ApiError> {
    let discrim = crate::dex::meteora_damm_v2::CONFIG_DISCRIM;
    if data.len() < 8 + 320 {
        return Err(ApiError::bad_request(format!(
            "meteora damm v2 config too short: {}",
            data.len()
        )));
    }
    if data[..8] != discrim {
        return Err(ApiError::bad_request(
            "meteora damm v2 config discriminator mismatch",
        ));
    }

    // Layout matches upstream `state::Config` (zero-copy) fields order.
    // Offsets below are relative to account start (including 8-byte anchor discriminator).
    let pool_creator_authority = read_pubkey_at(data, 8 + 32, "pool_creator_authority")?;
    let config_type = read_u8_at(data, 8 + 192 + 1 + 1, "config_type")?;
    let index = read_u64_at(data, 8 + 192 + 1 + 1 + 1 + 5, "index")?;
    let sqrt_min_price = read_u128_at(data, 8 + 192 + 1 + 1 + 1 + 5 + 8, "sqrt_min_price")?;
    let sqrt_max_price = read_u128_at(data, 8 + 192 + 1 + 1 + 1 + 5 + 8 + 16, "sqrt_max_price")?;

    Ok(MeteoraDammV2ConfigInfo {
        pool_creator_authority,
        config_type,
        index,
        sqrt_min_price,
        sqrt_max_price,
    })
}

async fn pick_meteora_damm_v2_config(
    sol: &SolHook,
    maybe_pubkey: Option<&Pubkey>,
    maybe_index: Option<u64>,
    sqrt_price: u128,
) -> Result<(Pubkey, MeteoraDammV2ConfigInfo), ApiError> {
    let program_id = crate::dex::meteora_damm_v2::METEORA_DAMM_V2_ID;

    if let Some(pk) = maybe_pubkey {
        let acc = sol
            .get_account_with_commitment_resilient(pk, CommitmentConfig::processed())
            .await
            .map_err(|e| ApiError::internal(format!("rpc getAccount(config) failed: {e}")))?;
        if acc.owner != program_id {
            return Err(ApiError::bad_request(format!(
                "meteora damm v2 config owner mismatch: {}",
                acc.owner
            )));
        }
        let info = parse_meteora_damm_v2_config_info(&acc.data)?;
        if info.config_type != 0 {
            return Err(ApiError::bad_request(
                "meteora damm v2 config_type must be static (0)",
            ));
        }
        if info.pool_creator_authority != Pubkey::default() {
            return Err(ApiError::bad_request(
                "meteora damm v2 config is not permissionless (pool_creator_authority set)",
            ));
        }
        if sqrt_price < info.sqrt_min_price || sqrt_price > info.sqrt_max_price {
            return Err(ApiError::bad_request(
                "meteora damm v2 initial_price out of config sqrt price range",
            ));
        }
        return Ok((*pk, info));
    }

    let discrim = crate::dex::meteora_damm_v2::CONFIG_DISCRIM;
    let accounts = sol
        .get_program_ui_accounts_with_config_resilient(
            &program_id,
            RpcProgramAccountsConfig {
                filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    0,
                    discrim.as_ref(),
                ))]),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    commitment: Some(CommitmentConfig::processed()),
                    ..Default::default()
                },
                with_context: None,
                sort_results: None,
            },
        )
        .await
        .map_err(|e| ApiError::internal(format!("rpc getProgramAccounts(config) failed: {e}")))?;

    let mut candidates: Vec<(u64, Pubkey, MeteoraDammV2ConfigInfo)> = Vec::new();
    for (pk, ui_acc) in accounts {
        let bytes = ui_acc
            .data
            .decode()
            .ok_or_else(|| ApiError::internal(format!("failed to decode account data for {pk}")))?;
        let info = match parse_meteora_damm_v2_config_info(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only static, permissionless configs.
        if info.config_type != 0 || info.pool_creator_authority != Pubkey::default() {
            continue;
        }
        if sqrt_price < info.sqrt_min_price || sqrt_price > info.sqrt_max_price {
            continue;
        }
        candidates.push((info.index, pk, info));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    if let Some(index) = maybe_index {
        return candidates
            .into_iter()
            .find(|(idx, _pk, _info)| *idx == index)
            .map(|(_idx, pk, info)| (pk, info))
            .ok_or_else(|| {
                ApiError::not_found(
                    "meteora damm v2 config not found for requested index + price range",
                )
            });
    }

    candidates
        .into_iter()
        .next()
        .map(|(_idx, pk, info)| (pk, info))
        .ok_or_else(|| ApiError::not_found("meteora damm v2 no permissionless config found"))
}

async fn pick_meteora_dbc_config(
    sol: &SolHook,
    maybe_pubkey: Option<&Pubkey>,
    maybe_index: Option<u64>,
    desired_quote_mint: Pubkey,
) -> Result<(Pubkey, crate::dex::meteora_dbc::MeteoraDbcConfigState), ApiError> {
    let program_id = crate::dex::meteora_dbc::METEORA_DBC_ID;

    if let Some(pk) = maybe_pubkey {
        let acc = sol
            .get_account_with_commitment_resilient(pk, CommitmentConfig::processed())
            .await
            .map_err(|e| ApiError::internal(format!("rpc getAccount(config) failed: {e}")))?;
        if acc.owner != program_id {
            return Err(ApiError::bad_request(format!(
                "meteora dbc config owner mismatch: {}",
                acc.owner
            )));
        }
        let state = MeteoraDbc::decode_pool_config_account_data(&acc.data)?;
        let locked_bps = state.total_locked_liquidity_bps_after_n_seconds(
            crate::dex::meteora_dbc::DBC_LOCKED_LIQUIDITY_CHECK_SECONDS,
        )?;
        if locked_bps < crate::dex::meteora_dbc::DBC_MIN_LOCKED_LIQUIDITY_BPS {
            return Err(ApiError::bad_request(format!(
                "meteora dbc config migration locked liquidity too low: {} bps (min {})",
                locked_bps,
                crate::dex::meteora_dbc::DBC_MIN_LOCKED_LIQUIDITY_BPS
            )));
        }
        if state.quote_mint != desired_quote_mint {
            return Err(ApiError::bad_request(format!(
                "meteora dbc config quote_mint mismatch (config={}, requested={})",
                state.quote_mint, desired_quote_mint
            )));
        }
        return Ok((*pk, state));
    }

    let discrim = crate::dex::meteora_dbc::POOL_CONFIG_DISCRIM;
    let quote_offset = crate::dex::meteora_dbc::DBC_POOL_CONFIG_QUOTE_MINT_OFFSET;
    let accounts = sol
        .get_program_ui_accounts_with_config_resilient(
            &program_id,
            RpcProgramAccountsConfig {
                filters: Some(vec![
                    RpcFilterType::DataSize(
                        crate::dex::meteora_dbc::DBC_POOL_CONFIG_ACCOUNT_LEN as u64,
                    ),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, discrim.as_ref())),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        quote_offset,
                        desired_quote_mint.as_ref(),
                    )),
                ]),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    commitment: Some(CommitmentConfig::processed()),
                    ..Default::default()
                },
                with_context: None,
                sort_results: None,
            },
        )
        .await
        .map_err(|e| ApiError::internal(format!("rpc getProgramAccounts(config) failed: {e}")))?;

    let mut candidates: Vec<(Pubkey, crate::dex::meteora_dbc::MeteoraDbcConfigState)> = Vec::new();
    for (pk, ui_acc) in accounts {
        let bytes = ui_acc
            .data
            .decode()
            .ok_or_else(|| ApiError::internal(format!("failed to decode account data for {pk}")))?;
        let state = match MeteoraDbc::decode_pool_config_account_data(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let locked_bps = match state.total_locked_liquidity_bps_after_n_seconds(
            crate::dex::meteora_dbc::DBC_LOCKED_LIQUIDITY_CHECK_SECONDS,
        ) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if locked_bps < crate::dex::meteora_dbc::DBC_MIN_LOCKED_LIQUIDITY_BPS {
            continue;
        }
        candidates.push((pk, state));
    }

    // Deterministic ordering for config_index selection.
    candidates.sort_by(|a, b| {
        a.1.token_type
            .cmp(&b.1.token_type)
            .then_with(|| a.0.to_bytes().cmp(&b.0.to_bytes()))
    });

    if let Some(index) = maybe_index {
        let idx = usize::try_from(index)
            .map_err(|_| ApiError::bad_request("meteora_dbc_config_index must fit in usize"))?;
        return candidates.get(idx).cloned().ok_or_else(|| {
            ApiError::not_found("meteora dbc config not found for requested index")
        });
    }

    candidates
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::not_found("meteora dbc no config found for quote_mint"))
}

fn compute_meteora_damm_v2_sqrt_price_x64(
    initial_price: f64,
    base_decimals: u8,
    quote_decimals: u8,
) -> Result<u128, ApiError> {
    if !initial_price.is_finite() || initial_price <= 0.0 {
        return Err(ApiError::bad_request("initial_price must be > 0"));
    }
    let price_per_raw = initial_price * 10_f64.powi(quote_decimals as i32 - base_decimals as i32);
    if !price_per_raw.is_finite() || price_per_raw <= 0.0 {
        return Err(ApiError::bad_request(
            "initial_price produced invalid decimals-adjusted price",
        ));
    }

    let sqrt_ratio = price_per_raw.sqrt();
    let sqrt_price = (sqrt_ratio * 2_f64.powi(64)).round();
    if !sqrt_price.is_finite() || sqrt_price <= 0.0 {
        return Err(ApiError::bad_request(
            "initial_price produced invalid sqrt_price",
        ));
    }
    Ok(sqrt_price as u128)
}

fn ceil_div_u256(n: U256, d: U256) -> Result<U256, ApiError> {
    if d.is_zero() {
        return Err(ApiError::bad_request("division by zero"));
    }
    if n.is_zero() {
        return Ok(U256::ZERO);
    }
    Ok((n + d - U256::from(1u8)) / d)
}

fn compute_meteora_damm_v2_liquidity_and_amounts(
    cfg: &MeteoraDammV2ConfigInfo,
    sqrt_price: u128,
    max_amount_a: u64,
    max_amount_b: u64,
) -> Result<(u128, u64, u64), ApiError> {
    if sqrt_price < cfg.sqrt_min_price || sqrt_price > cfg.sqrt_max_price {
        return Err(ApiError::bad_request(
            "initial_price out of selected config range",
        ));
    }

    let delta_a = cfg
        .sqrt_max_price
        .checked_sub(sqrt_price)
        .ok_or_else(|| ApiError::bad_request("sqrt_max_price < sqrt_price"))?;
    let delta_b = sqrt_price
        .checked_sub(cfg.sqrt_min_price)
        .ok_or_else(|| ApiError::bad_request("sqrt_price < sqrt_min_price"))?;

    let denom_a = U256::from(sqrt_price) * U256::from(cfg.sqrt_max_price);

    let liq_a: Option<U512> = if delta_a == 0 {
        None
    } else {
        let num = U512::from(max_amount_a) * U512::from(denom_a);
        Some(num / U512::from(delta_a))
    };
    let liq_b: Option<U512> = if delta_b == 0 {
        None
    } else {
        let denom = U256::from(1u8) << 128;
        let num = U256::from(max_amount_b) * denom;
        Some(U512::from(num / U256::from(delta_b)))
    };

    let liq_u512 = match (liq_a, liq_b) {
        (Some(a), Some(b)) => std::cmp::min(a, b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => {
            return Err(ApiError::bad_request(
                "invalid config range (sqrt_min/sqrt_max)",
            ));
        }
    };
    if liq_u512.is_zero() {
        return Err(ApiError::bad_request(
            "initial amounts are too small for selected config range",
        ));
    }
    let liquidity: u128 = u128::try_from(liq_u512).unwrap_or(u128::MAX);

    let liq = U256::from(liquidity);

    let amount_a_u256 = if delta_a == 0 {
        U256::ZERO
    } else {
        let num = liq * U256::from(delta_a);
        ceil_div_u256(num, denom_a)?
    };
    let amount_b_u256 = if delta_b == 0 {
        U256::ZERO
    } else {
        let num = liq * U256::from(delta_b);
        let denom = U256::from(1u8) << 128;
        // ceil(num/2^128) = (num + 2^128 - 1) >> 128
        (num + denom - U256::from(1u8)) >> 128
    };

    if amount_a_u256.is_zero() && amount_b_u256.is_zero() {
        return Err(ApiError::bad_request(
            "computed initialize amounts are zero",
        ));
    }

    if amount_a_u256 > U256::from(u64::MAX) || amount_b_u256 > U256::from(u64::MAX) {
        return Err(ApiError::bad_request(
            "computed initialize amounts overflow u64",
        ));
    }
    let amount_a: u64 = amount_a_u256
        .try_into()
        .map_err(|_| ApiError::bad_request("computed token_a amount overflow"))?;
    let amount_b: u64 = amount_b_u256
        .try_into()
        .map_err(|_| ApiError::bad_request("computed token_b amount overflow"))?;

    if amount_a > max_amount_a || amount_b > max_amount_b {
        return Err(ApiError::bad_request(
            "computed initialize amounts exceed provided max amounts",
        ));
    }

    Ok((liquidity, amount_a, amount_b))
}

fn meteora_damm_v2_is_supported_mint(
    mint: &Pubkey,
    token_program: &Pubkey,
    mint_data: &[u8],
) -> Result<bool, ApiError> {
    if *token_program == TOKEN_PROGRAM_ID {
        return Ok(true);
    }
    if *token_program != TOKEN_2022_PROGRAM_ID {
        return Err(ApiError::bad_request(format!(
            "unsupported token program: {}",
            token_program
        )));
    }
    if spl_token_2022::native_mint::check_id(mint) {
        return Err(ApiError::bad_request(
            "token-2022 native mint is not supported for meteora damm v2 pool create",
        ));
    }

    use spl_token_2022::extension::{
        BaseStateWithExtensions, ExtensionType, StateWithExtensions, transfer_hook::TransferHook,
    };
    let mint_state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(mint_data)
        .map_err(|e| ApiError::bad_request(format!("failed to unpack token-2022 mint: {e}")))?;
    let extensions = mint_state.get_extension_types().map_err(|e| {
        ApiError::bad_request(format!("failed to read token-2022 mint extensions: {e}"))
    })?;

    for ext in extensions {
        match ext {
            ExtensionType::TransferFeeConfig
            | ExtensionType::MetadataPointer
            | ExtensionType::TokenMetadata => {}
            ExtensionType::TransferHook => {
                let hook = mint_state.get_extension::<TransferHook>().map_err(|_| {
                    ApiError::bad_request("failed to decode token-2022 transfer hook extension")
                })?;
                let program_id: Option<Pubkey> = hook.program_id.into();
                let authority: Option<Pubkey> = hook.authority.into();
                if program_id.is_some() || authority.is_some() {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }

    Ok(true)
}

async fn apply_priority_fee_if_requested(
    sol: &SolHook,
    planned: &mut PlannedPoolTx,
    priority_fee_level: Option<PriorityFeeLevel>,
) -> Result<(), ApiError> {
    if let Some(level) = priority_fee_level {
        let price = sol
            .fetch_priority_fee(&level, &planned.priority_fee_addresses)
            .await
            .map_err(ApiError::from)?;
        planned.instructions.insert(
            0,
            crate::compute_budget::compute_budget::ix_set_compute_unit_price(price),
        );
    }
    Ok(())
}

async fn build_pool_response(
    state: &ApiState,
    req: PoolBuildRequest,
) -> Result<
    (
        PoolBuildResponse,
        Arc<solana_client::nonblocking::rpc_client::RpcClient>,
        SolanaCluster,
    ),
    ApiError,
> {
    let base_offset = state
        .endpoint_cursor
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let payer = parse_pubkey(&req.payer, "payer")?;
    let base_mint_raw = req
        .base_mint
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("base_mint is required"))?;
    let base_mint = parse_pubkey(base_mint_raw, "base_mint")?;
    let quote_mint = req
        .quote_mint
        .as_deref()
        .map(|s| parse_pubkey(s, "quote_mint"))
        .transpose()?
        .unwrap_or(WSOL_MINT);

    let simulate = req.simulate.unwrap_or(true);
    let use_swqos = req.use_swqos.unwrap_or(false);
    let rpc_url_override = req
        .rpc_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let (rpc, cluster_for_programs) = if let Some(url) = rpc_url_override {
        resolve_request_rpc(&state, base_offset, Some(url)).await?
    } else {
        (
            rpc_client_for_attempt(&state, base_offset, 0)
                .await
                .map_err(|e| ApiError::internal(format!("rpc client selection failed: {e}")))?,
            state.cluster,
        )
    };
    let sol = SolHook::from_rpc_client_with_cluster(rpc.clone(), cluster_for_programs);

    let (base_program, quote_program) = match req.market {
        PoolMarket::MeteoraDbc => (TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID),
        _ => {
            let base_program = sol.get_token_program_id(&base_mint).await.map_err(|e| {
                ApiError::internal(format!("rpc getAccount(base_mint) failed: {e}"))
            })?;
            let quote_program = sol.get_token_program_id(&quote_mint).await.map_err(|e| {
                ApiError::internal(format!("rpc getAccount(quote_mint) failed: {e}"))
            })?;
            (base_program, quote_program)
        }
    };

    let compute_budget = ComputeBudgetPlan {
        compute_unit_price_micro_lamports: None,
        compute_unit_limit: req.compute_unit_limit,
    };
    let priority_fee_level = req
        .priority_fee_level
        .as_deref()
        .and_then(parse_priority_fee_level);

    let mut planned: PlannedPoolTx = match req.market {
        PoolMarket::PumpSwap => {
            let base_decimals = sol
                .get_token_decimals(&base_mint)
                .await
                .map_err(ApiError::from)?;
            let quote_decimals = sol
                .get_token_decimals(&quote_mint)
                .await
                .map_err(ApiError::from)?;
            let base_amount_ui = req
                .base_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("base_amount is required"))?;
            let quote_amount_ui = req
                .quote_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("quote_amount is required"))?;
            let base_amount_in =
                parse_ui_amount_to_raw(base_amount_ui, base_decimals, "base_amount")?;
            let quote_amount_in =
                parse_ui_amount_to_raw(quote_amount_ui, quote_decimals, "quote_amount")?;

            let coin_creator = match req
                .pump_swap_coin_creator
                .as_deref()
                .map(|s| parse_pubkey(s, "pump_swap_coin_creator"))
                .transpose()?
            {
                Some(creator) => creator,
                None => resolve_pump_swap_coin_creator(&sol, &base_mint, payer).await,
            };
            let is_mayhem_mode = req.pump_swap_is_mayhem_mode.unwrap_or(false);

            let mut index = req.pump_swap_index;
            if index.is_none() {
                // Choose the first unused pool index for (payer, base, quote).
                for i in 0u16..500u16 {
                    let (pool, _bump) = Pubkey::find_program_address(
                        &[
                            b"pool",
                            &i.to_le_bytes(),
                            payer.as_ref(),
                            base_mint.as_ref(),
                            quote_mint.as_ref(),
                        ],
                        &PUMP_SWAP_ID,
                    );
                    if !sol.exists(&pool).await.map_err(ApiError::from)? {
                        index = Some(i);
                        break;
                    }
                }
            }
            let index =
                index.ok_or_else(|| ApiError::internal("failed to find unused pump_swap index"))?;

            plan_pump_swap_create_pool(
                PumpSwapCreatePoolPlan {
                    payer,
                    base_mint,
                    quote_mint,
                    base_token_program: base_program,
                    quote_token_program: quote_program,
                    index,
                    base_amount_in,
                    quote_amount_in,
                    coin_creator,
                    is_mayhem_mode,
                },
                compute_budget,
            )?
        }
        PoolMarket::RaydiumCpmm => {
            let program_id = crate::core::cluster::raydium_cpmm_program_id(cluster_for_programs);
            let create_pool_fee =
                crate::core::cluster::raydium_cpmm_create_pool_fee_receiver(cluster_for_programs);

            let base_decimals = sol
                .get_token_decimals(&base_mint)
                .await
                .map_err(ApiError::from)?;
            let quote_decimals = sol
                .get_token_decimals(&quote_mint)
                .await
                .map_err(ApiError::from)?;
            let base_amount_ui = req
                .base_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("base_amount is required"))?;
            let quote_amount_ui = req
                .quote_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("quote_amount is required"))?;
            let base_amount_raw =
                parse_ui_amount_to_raw(base_amount_ui, base_decimals, "base_amount")?;
            let quote_amount_raw =
                parse_ui_amount_to_raw(quote_amount_ui, quote_decimals, "quote_amount")?;

            let (
                token_0_mint,
                token_1_mint,
                init_amount_0,
                init_amount_1,
                token_0_program,
                token_1_program,
            ) = if base_mint < quote_mint {
                (
                    base_mint,
                    quote_mint,
                    base_amount_raw,
                    quote_amount_raw,
                    base_program,
                    quote_program,
                )
            } else {
                (
                    quote_mint,
                    base_mint,
                    quote_amount_raw,
                    base_amount_raw,
                    quote_program,
                    base_program,
                )
            };

            let amm_config_pk = req
                .raydium_cpmm_amm_config
                .as_deref()
                .map(|s| parse_pubkey(s, "raydium_cpmm_amm_config"))
                .transpose()?;
            let amm_config = pick_raydium_cpmm_amm_config(
                &sol,
                &program_id,
                amm_config_pk.as_ref(),
                req.raydium_cpmm_amm_config_index,
            )
            .await?;

            plan_raydium_cpmm_create_pool(
                RaydiumCpmmCreatePoolPlan {
                    payer,
                    program_id,
                    amm_config,
                    create_pool_fee,
                    token_0_mint,
                    token_1_mint,
                    token_0_program,
                    token_1_program,
                    init_amount_0,
                    init_amount_1,
                    open_time: 0,
                },
                compute_budget,
            )?
        }
        PoolMarket::RaydiumClmm => {
            let program_id = crate::core::cluster::raydium_clmm_program_id(cluster_for_programs);

            let (token_mint_0, token_mint_1, token_program_0, token_program_1) =
                if base_mint < quote_mint {
                    (base_mint, quote_mint, base_program, quote_program)
                } else {
                    (quote_mint, base_mint, quote_program, base_program)
                };

            let tick_spacing_opt = req.raydium_clmm_tick_spacing;
            let tick_spacing = tick_spacing_opt.unwrap_or(60);
            let amm_config_pk = req
                .raydium_clmm_amm_config
                .as_deref()
                .map(|s| parse_pubkey(s, "raydium_clmm_amm_config"))
                .transpose()?;
            let amm_config = pick_raydium_clmm_amm_config(
                &sol,
                &program_id,
                amm_config_pk.as_ref(),
                tick_spacing,
                tick_spacing_opt.is_some(),
            )
            .await?;

            let price_ui: f64 = req
                .initial_price
                .as_deref()
                .unwrap_or("1")
                .trim()
                .parse()
                .map_err(|_| ApiError::bad_request("initial_price must be a number"))?;
            if !price_ui.is_finite() || price_ui <= 0.0 {
                return Err(ApiError::bad_request("initial_price must be > 0"));
            }

            let seed_base_amount_ui = req
                .base_amount
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let seed_quote_amount_ui = req
                .quote_amount
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let seed_requested = seed_base_amount_ui.is_some() || seed_quote_amount_ui.is_some();

            let mut compute_budget = compute_budget;
            if seed_requested && compute_budget.compute_unit_limit.is_none() {
                compute_budget.compute_unit_limit = Some(1_000_000);
            }

            let decimals_0 = sol
                .get_token_decimals(&token_mint_0)
                .await
                .map_err(ApiError::from)?;
            let decimals_1 = sol
                .get_token_decimals(&token_mint_1)
                .await
                .map_err(ApiError::from)?;
            let desired_token1_per_token0 = if base_mint == token_mint_0 {
                // token_0 is base, token_1 is quote
                price_ui
            } else {
                // token_1 is base, token_0 is quote
                1.0 / price_ui
            };
            let decimal_adjust = 10_f64.powi(decimals_0 as i32 - decimals_1 as i32);
            let sqrt_ratio = (desired_token1_per_token0 / decimal_adjust).sqrt();
            let sqrt_price_x64 = (sqrt_ratio * 2_f64.powi(64)).round();
            if !sqrt_price_x64.is_finite() || sqrt_price_x64 <= 0.0 {
                return Err(ApiError::bad_request(
                    "initial_price produced invalid sqrt_price_x64",
                ));
            }
            let sqrt_price_x64 = sqrt_price_x64 as u128;

            let mut planned = plan_raydium_clmm_create_pool(
                RaydiumClmmCreatePoolPlan {
                    payer,
                    program_id,
                    amm_config,
                    token_mint_0,
                    token_mint_1,
                    token_program_0,
                    token_program_1,
                    sqrt_price_x64,
                    open_time: 0,
                },
                compute_budget,
            )?;

            if seed_requested {
                if base_program != TOKEN_PROGRAM_ID || quote_program != TOKEN_PROGRAM_ID {
                    return Err(ApiError::bad_request(
                        "raydium_clmm seed liquidity only supports legacy spl_token mints",
                    ));
                }

                let base_decimals = if base_mint == token_mint_0 {
                    decimals_0
                } else {
                    decimals_1
                };
                let quote_decimals = if quote_mint == token_mint_0 {
                    decimals_0
                } else {
                    decimals_1
                };
                let to_ui =
                    |raw: u64, decimals: u8| -> f64 { raw as f64 / 10_f64.powi(decimals as i32) };
                let raw_from_ui_ceil =
                    |ui: f64, decimals: u8, label: &str| -> Result<u64, ApiError> {
                        if !ui.is_finite() || ui <= 0.0 {
                            return Err(ApiError::bad_request(format!("{label} must be > 0")));
                        }
                        let scale = 10_f64.powi(decimals as i32);
                        let raw_f = (ui * scale).ceil();
                        if !raw_f.is_finite() || raw_f <= 0.0 {
                            return Err(ApiError::bad_request(format!("{label} is too small")));
                        }
                        if raw_f > (u64::MAX as f64) {
                            return Err(ApiError::bad_request(format!("{label} overflow")));
                        }
                        Ok(raw_f as u64)
                    };

                let (base_amount_raw, quote_amount_raw) = match (
                    seed_base_amount_ui,
                    seed_quote_amount_ui,
                ) {
                    (Some(base_ui), Some(quote_ui)) => {
                        let base_amount_raw =
                            parse_ui_amount_to_raw(base_ui, base_decimals, "base_amount")?;
                        let quote_amount_raw =
                            parse_ui_amount_to_raw(quote_ui, quote_decimals, "quote_amount")?;
                        if base_amount_raw == 0 || quote_amount_raw == 0 {
                            return Err(ApiError::bad_request(
                                "base_amount and quote_amount must both be > 0 when seeding raydium_clmm liquidity",
                            ));
                        }
                        (base_amount_raw, quote_amount_raw)
                    }
                    (Some(base_ui), None) => {
                        let base_amount_raw =
                            parse_ui_amount_to_raw(base_ui, base_decimals, "base_amount")?;
                        if base_amount_raw == 0 {
                            return Err(ApiError::bad_request(
                                "base_amount must be > 0 when seeding raydium_clmm liquidity",
                            ));
                        }
                        let quote_amount_ui = to_ui(base_amount_raw, base_decimals) * price_ui;
                        let quote_amount_raw = raw_from_ui_ceil(
                            quote_amount_ui,
                            quote_decimals,
                            "quote_amount (computed)",
                        )?;
                        (base_amount_raw, quote_amount_raw)
                    }
                    (None, Some(quote_ui)) => {
                        let quote_amount_raw =
                            parse_ui_amount_to_raw(quote_ui, quote_decimals, "quote_amount")?;
                        if quote_amount_raw == 0 {
                            return Err(ApiError::bad_request(
                                "quote_amount must be > 0 when seeding raydium_clmm liquidity",
                            ));
                        }
                        let base_amount_ui = to_ui(quote_amount_raw, quote_decimals) / price_ui;
                        let base_amount_raw = raw_from_ui_ceil(
                            base_amount_ui,
                            base_decimals,
                            "base_amount (computed)",
                        )?;
                        (base_amount_raw, quote_amount_raw)
                    }
                    (None, None) => {
                        return Err(ApiError::internal(
                            "raydium_clmm seed requested but base_amount/quote_amount missing",
                        ));
                    }
                };

                let position_nft_mint_raw = req.raydium_clmm_position_nft_mint.as_deref().ok_or_else(|| {
                    ApiError::bad_request(
                        "raydium_clmm_position_nft_mint is required when seeding raydium_clmm initial liquidity",
                    )
                })?;
                let position_nft_mint =
                    parse_pubkey(position_nft_mint_raw, "raydium_clmm_position_nft_mint")?;

                let pool_state = planned
                    .derived
                    .map
                    .get("pool_state")
                    .copied()
                    .ok_or_else(|| ApiError::internal("missing derived pool_state"))?;
                let (seed_sqrt_price_x64, seed_tick_spacing) =
                    if sol.exists(&pool_state).await.map_err(ApiError::from)? {
                        planned.instructions.pop().ok_or_else(|| {
                            ApiError::internal("raydium_clmm: missing create ix to drop")
                        })?;

                        let acc = sol
                            .get_account_with_commitment_resilient(
                                &pool_state,
                                CommitmentConfig::processed(),
                            )
                            .await
                            .map_err(|e| {
                                ApiError::internal(format!(
                                    "rpc getAccount(pool_state) failed: {e}"
                                ))
                            })?;
                        let state =
                            crate::dex::raydium_clmm::RaydiumClmm::decode_pool_state_account_data(
                                &acc.data,
                            )
                            .map_err(|e| {
                                ApiError::internal(format!(
                                    "failed to decode raydium_clmm pool_state: {e:#}"
                                ))
                            })?;
                        (state.sqrt_price_x64, state.tick_spacing)
                    } else {
                        (sqrt_price_x64, tick_spacing)
                    };
                let token_vault_0 = planned
                    .derived
                    .map
                    .get("token_vault_0")
                    .copied()
                    .ok_or_else(|| ApiError::internal("missing derived token_vault_0"))?;
                let token_vault_1 = planned
                    .derived
                    .map
                    .get("token_vault_1")
                    .copied()
                    .ok_or_else(|| ApiError::internal("missing derived token_vault_1"))?;

                // If the user only provides one side, prefer using that side as the `open_position`
                // "base" so the on-chain deposit matches the user input (within rounding).
                let open_position_base_is_mint0_override =
                    match (seed_base_amount_ui, seed_quote_amount_ui) {
                        (Some(_), None) => Some(base_mint == token_mint_0),
                        (None, Some(_)) => Some(quote_mint == token_mint_0),
                        _ => None,
                    };
                let seed = plan_raydium_clmm_seed_liquidity(RaydiumClmmSeedLiquidityPlan {
                    payer,
                    program_id,
                    pool_state,
                    token_mint_0,
                    token_mint_1,
                    token_program_0,
                    token_program_1,
                    token_vault_0,
                    token_vault_1,
                    position_nft_mint,
                    base_mint,
                    quote_mint,
                    base_amount_in: base_amount_raw,
                    quote_amount_in: quote_amount_raw,
                    sqrt_price_x64: seed_sqrt_price_x64,
                    tick_spacing: seed_tick_spacing,
                    with_metadata: false,
                    open_position_base_is_mint0_override,
                })?;

                planned.instructions.extend(seed.instructions);
                planned
                    .priority_fee_addresses
                    .extend(seed.priority_fee_addresses);
                for signer in seed.required_signers {
                    if !planned.required_signers.contains(&signer) {
                        planned.required_signers.push(signer);
                    }
                }
                planned.derived.map.extend(seed.derived.map);
            }

            planned
        }
        PoolMarket::MeteoraDlmm => {
            let token_mint_x = base_mint;
            let token_mint_y = quote_mint;

            let initial_price: f64 = req
                .initial_price
                .as_deref()
                .unwrap_or("1")
                .trim()
                .parse()
                .map_err(|_| ApiError::bad_request("initial_price must be a number"))?;
            if !initial_price.is_finite() || initial_price <= 0.0 {
                return Err(ApiError::bad_request("initial_price must be > 0"));
            }

            let bin_step = req.meteora_dlmm_bin_step.unwrap_or(25);
            let base_fee_bps = req.meteora_dlmm_base_fee_bps.unwrap_or(25);
            let activation_type = req.meteora_dlmm_activation_type.unwrap_or(0);
            let activation_point = req.meteora_dlmm_activation_point;
            let has_alpha_vault = req.meteora_dlmm_has_alpha_vault.unwrap_or(false);
            let creator_pool_on_off_control = req
                .meteora_dlmm_creator_pool_on_off_control
                .unwrap_or(false);

            let rounding = parse_dlmm_rounding(req.meteora_dlmm_rounding.as_deref())?;
            let base_decimals = sol
                .get_token_decimals(&token_mint_x)
                .await
                .map_err(ApiError::from)?;
            let quote_decimals = sol
                .get_token_decimals(&token_mint_y)
                .await
                .map_err(ApiError::from)?;

            let seed_base_amount_ui = req
                .base_amount
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let seed_quote_amount_ui = req
                .quote_amount
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let seed_requested = seed_base_amount_ui.is_some() || seed_quote_amount_ui.is_some();
            let (seed_amount_x, seed_amount_y) = if seed_requested {
                let base_amount_ui = seed_base_amount_ui.ok_or_else(|| {
                    ApiError::bad_request(
                        "base_amount is required when seeding meteora_dlmm initial liquidity",
                    )
                })?;
                let quote_amount_ui = seed_quote_amount_ui.ok_or_else(|| {
                    ApiError::bad_request(
                        "quote_amount is required when seeding meteora_dlmm initial liquidity",
                    )
                })?;
                let x = parse_ui_amount_to_raw(base_amount_ui, base_decimals, "base_amount")?;
                let y = parse_ui_amount_to_raw(quote_amount_ui, quote_decimals, "quote_amount")?;
                if x == 0 || y == 0 {
                    return Err(ApiError::bad_request(
                        "base_amount and quote_amount must both be > 0 when seeding meteora_dlmm liquidity",
                    ));
                }
                (x, y)
            } else {
                (0u64, 0u64)
            };

            let mut compute_budget = compute_budget;
            if seed_requested && compute_budget.compute_unit_limit.is_none() {
                compute_budget.compute_unit_limit = Some(800_000);
            }

            const DLMM_LAUNCH_PROOF_LAMPORTS: u64 = 1_000;
            let wsol_wrap_lamports_x = if token_mint_x == WSOL_MINT {
                if seed_requested {
                    seed_amount_x.max(DLMM_LAUNCH_PROOF_LAMPORTS)
                } else {
                    DLMM_LAUNCH_PROOF_LAMPORTS
                }
            } else {
                0
            };
            let wsol_wrap_lamports_y = if token_mint_y == WSOL_MINT {
                if seed_requested {
                    seed_amount_y.max(DLMM_LAUNCH_PROOF_LAMPORTS)
                } else {
                    DLMM_LAUNCH_PROOF_LAMPORTS
                }
            } else {
                0
            };

            let price_per_lamport =
                initial_price * 10_f64.powi(quote_decimals as i32 - base_decimals as i32);
            let active_id = compute_dlmm_active_id(bin_step, price_per_lamport, rounding)?;
            let (base_factor, base_fee_power_factor) =
                compute_dlmm_base_factor(bin_step, base_fee_bps)?;

            let (lb_pair, _bump) = Pubkey::find_program_address(
                &[
                    crate::dex::meteora_dlmm::ILM_BASE_KEY.as_ref(),
                    std::cmp::min(token_mint_x, token_mint_y).as_ref(),
                    std::cmp::max(token_mint_x, token_mint_y).as_ref(),
                ],
                &METEORA_DLMM_ID,
            );
            let lb_pair_exists = if seed_requested {
                sol.exists(&lb_pair).await.map_err(ApiError::from)?
            } else {
                false
            };

            let reserve_x = Pubkey::find_program_address(
                &[lb_pair.as_ref(), token_mint_x.as_ref()],
                &METEORA_DLMM_ID,
            )
            .0;
            let reserve_y = Pubkey::find_program_address(
                &[lb_pair.as_ref(), token_mint_y.as_ref()],
                &METEORA_DLMM_ID,
            )
            .0;
            let oracle = Pubkey::find_program_address(
                &[crate::dex::meteora_dlmm::ORACLE_SEED, lb_pair.as_ref()],
                &METEORA_DLMM_ID,
            )
            .0;
            let user_token_x = sol
                .get_ata_auto(&payer, &token_mint_x)
                .await
                .map_err(ApiError::from)?;
            let user_token_y = sol
                .get_ata_auto(&payer, &token_mint_y)
                .await
                .map_err(ApiError::from)?;

            let token_badge_x_pda = Pubkey::find_program_address(
                &[
                    crate::dex::meteora_dlmm::TOKEN_BADGE_SEED,
                    token_mint_x.as_ref(),
                ],
                &METEORA_DLMM_ID,
            )
            .0;
            let token_badge_y_pda = Pubkey::find_program_address(
                &[
                    crate::dex::meteora_dlmm::TOKEN_BADGE_SEED,
                    token_mint_y.as_ref(),
                ],
                &METEORA_DLMM_ID,
            )
            .0;
            let token_badge_x = if sol
                .exists(&token_badge_x_pda)
                .await
                .map_err(ApiError::from)?
            {
                token_badge_x_pda
            } else {
                METEORA_DLMM_ID
            };
            let token_badge_y = if sol
                .exists(&token_badge_y_pda)
                .await
                .map_err(ApiError::from)?
            {
                token_badge_y_pda
            } else {
                METEORA_DLMM_ID
            };

            let bin_array_bitmap_extension = METEORA_DLMM_ID; // SDK uses program-id placeholder

            let params = build_meteora_dlmm_customizable_params(
                active_id,
                bin_step,
                base_factor,
                activation_type,
                has_alpha_vault,
                activation_point,
                creator_pool_on_off_control,
                base_fee_power_factor,
            )?;

            let mut planned = plan_meteora_dlmm_create_pool(
                MeteoraDlmmCreatePoolPlan {
                    payer,
                    lb_pair,
                    token_mint_x,
                    token_mint_y,
                    reserve_x,
                    reserve_y,
                    oracle,
                    user_token_x,
                    user_token_y,
                    token_program_x: base_program,
                    token_program_y: quote_program,
                    token_badge_x,
                    token_badge_y,
                    bin_array_bitmap_extension,
                    params,
                    wsol_wrap_lamports_x,
                    wsol_wrap_lamports_y,
                },
                compute_budget,
            )?;

            if seed_requested {
                let active_id = if lb_pair_exists {
                    let acc = sol
                        .get_account_with_commitment_resilient(
                            &lb_pair,
                            CommitmentConfig::processed(),
                        )
                        .await
                        .map_err(|e| {
                            ApiError::internal(format!("rpc getAccount(lb_pair) failed: {e}"))
                        })?;
                    crate::dex::meteora_dlmm::MeteoraDlmm::decode_lb_pair_account_data(&acc.data)
                        .map_err(|e| {
                            ApiError::internal(format!("failed to decode lb_pair state: {e:#}"))
                        })?
                        .active_id
                } else {
                    active_id
                };

                if lb_pair_exists {
                    planned.instructions.pop().ok_or_else(|| {
                        ApiError::internal("meteora_dlmm: missing initialize ix to drop")
                    })?;
                }

                let seed = plan_meteora_dlmm_seed_liquidity(MeteoraDlmmSeedLiquidityPlan {
                    payer,
                    lb_pair,
                    reserve_x,
                    reserve_y,
                    user_token_x,
                    user_token_y,
                    token_mint_x,
                    token_mint_y,
                    token_program_x: base_program,
                    token_program_y: quote_program,
                    bin_array_bitmap_extension,
                    active_id,
                    amount_x: seed_amount_x,
                    amount_y: seed_amount_y,
                    width: 1,
                })?;

                planned.instructions.extend(seed.instructions);
                planned
                    .priority_fee_addresses
                    .extend(seed.priority_fee_addresses);
                for signer in seed.required_signers {
                    if !planned.required_signers.contains(&signer) {
                        planned.required_signers.push(signer);
                    }
                }
                planned.derived.map.extend(seed.derived.map);
            }

            planned
        }
        PoolMarket::MeteoraDammV1 => {
            // Dynamic AMM v1 only supports legacy token program.
            if base_program != TOKEN_PROGRAM_ID || quote_program != TOKEN_PROGRAM_ID {
                return Err(ApiError::bad_request(
                    "meteora_damm_v1 create pool only supports legacy spl_token mints",
                ));
            }
            let trade_fee_bps = req.meteora_damm_v1_trade_fee_bps.unwrap_or(25);
            let base_decimals = sol
                .get_token_decimals(&base_mint)
                .await
                .map_err(ApiError::from)?;
            let quote_decimals = sol
                .get_token_decimals(&quote_mint)
                .await
                .map_err(ApiError::from)?;
            let base_amount_ui = req
                .base_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("base_amount is required"))?;
            let quote_amount_ui = req
                .quote_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("quote_amount is required"))?;
            let base_amount_raw =
                parse_ui_amount_to_raw(base_amount_ui, base_decimals, "base_amount")?;
            let quote_amount_raw =
                parse_ui_amount_to_raw(quote_amount_ui, quote_decimals, "quote_amount")?;

            let vault_program_id = crate::dex::meteora_damm_v1::METEORA_DYNAMIC_VAULT_ID;
            let vault_base = crate::dex::meteora_damm_v1::METEORA_DYNAMIC_VAULT_BASE_ID;
            let vault_key = |mint: &Pubkey| -> Pubkey {
                Pubkey::find_program_address(
                    &[b"vault", mint.as_ref(), vault_base.as_ref()],
                    &vault_program_id,
                )
                .0
            };
            let a_vault = vault_key(&base_mint);
            let b_vault = vault_key(&quote_mint);
            let init_vault_a = !sol.exists(&a_vault).await.map_err(ApiError::from)?;
            let init_vault_b = !sol.exists(&b_vault).await.map_err(ApiError::from)?;

            let vault_lp_mint_pda = |vault: &Pubkey| -> Pubkey {
                Pubkey::find_program_address(
                    &[crate::dex::meteora_damm_v1::LP_MINT_SEED, vault.as_ref()],
                    &vault_program_id,
                )
                .0
            };

            let a_vault_lp_mint = if init_vault_a {
                vault_lp_mint_pda(&a_vault)
            } else {
                let acc = sol
                    .run_rpc_attempts_optional("getAccount(dynamic vault)", |rpc| async move {
                        Ok(rpc
                            .get_account_with_commitment(&a_vault, CommitmentConfig::processed())
                            .await?
                            .value)
                    })
                    .await
                    .map_err(|e| {
                        ApiError::internal(format!("rpc getAccount(dynamic vault) failed: {e}"))
                    })?;
                match acc {
                    None => vault_lp_mint_pda(&a_vault),
                    Some(acc)
                        if acc.owner == vault_program_id
                            && acc.data.len()
                                >= crate::dex::meteora_damm_v1::DYNAMIC_VAULT_LP_MINT_REQUIRED_LEN =>
                    {
                        read_pubkey_at(
                            &acc.data,
                            crate::dex::meteora_damm_v1::DYNAMIC_VAULT_LP_MINT_OFFSET,
                            "dynamic vault lp_mint",
                        )?
                    }
                    Some(_) => vault_lp_mint_pda(&a_vault),
                }
            };
            let b_vault_lp_mint = if init_vault_b {
                vault_lp_mint_pda(&b_vault)
            } else {
                let acc = sol
                    .run_rpc_attempts_optional("getAccount(dynamic vault)", |rpc| async move {
                        Ok(rpc
                            .get_account_with_commitment(&b_vault, CommitmentConfig::processed())
                            .await?
                            .value)
                    })
                    .await
                    .map_err(|e| {
                        ApiError::internal(format!("rpc getAccount(dynamic vault) failed: {e}"))
                    })?;
                match acc {
                    None => vault_lp_mint_pda(&b_vault),
                    Some(acc)
                        if acc.owner == vault_program_id
                            && acc.data.len()
                                >= crate::dex::meteora_damm_v1::DYNAMIC_VAULT_LP_MINT_REQUIRED_LEN =>
                    {
                        read_pubkey_at(
                            &acc.data,
                            crate::dex::meteora_damm_v1::DYNAMIC_VAULT_LP_MINT_OFFSET,
                            "dynamic vault lp_mint",
                        )?
                    }
                    Some(_) => vault_lp_mint_pda(&b_vault),
                }
            };

            plan_meteora_damm_v1_create_pool(
                MeteoraDammV1CreatePoolPlan {
                    payer,
                    token_a_mint: base_mint,
                    token_b_mint: quote_mint,
                    trade_fee_bps,
                    token_a_amount: base_amount_raw,
                    token_b_amount: quote_amount_raw,
                    init_vault_a,
                    init_vault_b,
                    a_vault_lp_mint,
                    b_vault_lp_mint,
                },
                compute_budget,
            )?
        }
        PoolMarket::RaydiumAmmV4 => {
            // Raydium AMM v4 only supports legacy SPL Token.
            if base_program != TOKEN_PROGRAM_ID || quote_program != TOKEN_PROGRAM_ID {
                return Err(ApiError::bad_request(
                    "raydium_amm_v4 create pool only supports legacy spl_token mints",
                ));
            }

            let market_raw = req
                .raydium_amm_v4_market
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("raydium_amm_v4_market is required"))?;
            let market = parse_pubkey(market_raw, "raydium_amm_v4_market")?;

            let program_id = crate::core::cluster::raydium_amm_v4_program_id(cluster_for_programs);
            let openbook_program_id =
                crate::core::cluster::raydium_amm_v4_openbook_program_id(cluster_for_programs);
            let create_fee_destination =
                crate::core::cluster::raydium_amm_v4_create_pool_fee_destination(
                    cluster_for_programs,
                );

            let market_acc = sol
                .get_account_with_commitment_resilient(&market, CommitmentConfig::processed())
                .await
                .map_err(|e| {
                    ApiError::internal(format!("rpc getAccount(openbook_market) failed: {e}"))
                })?;
            if market_acc.owner != openbook_program_id {
                return Err(ApiError::bad_request(format!(
                    "openbook market owner mismatch: {}",
                    market_acc.owner
                )));
            }
            if market_acc.data.len() < crate::dex::raydium_amm_v4::OPENBOOK_V3_MARKET_MIN_LEN {
                return Err(ApiError::bad_request("openbook market account too short"));
            }

            let market_coin_mint = read_pubkey_at(
                &market_acc.data,
                crate::dex::raydium_amm_v4::OPENBOOK_V3_MARKET_BASE_MINT_OFFSET,
                "openbook coin_mint",
            )?;
            let market_pc_mint = read_pubkey_at(
                &market_acc.data,
                crate::dex::raydium_amm_v4::OPENBOOK_V3_MARKET_QUOTE_MINT_OFFSET,
                "openbook pc_mint",
            )?;
            if market_coin_mint != base_mint || market_pc_mint != quote_mint {
                return Err(ApiError::bad_request(format!(
                    "openbook market mint mismatch (market coin={}, pc={})",
                    market_coin_mint, market_pc_mint
                )));
            }

            let base_decimals = sol
                .get_token_decimals(&base_mint)
                .await
                .map_err(ApiError::from)?;
            let quote_decimals = sol
                .get_token_decimals(&quote_mint)
                .await
                .map_err(ApiError::from)?;
            let base_amount_ui = req
                .base_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("base_amount is required"))?;
            let quote_amount_ui = req
                .quote_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("quote_amount is required"))?;
            let init_coin_amount =
                parse_ui_amount_to_raw(base_amount_ui, base_decimals, "base_amount")?;
            let init_pc_amount =
                parse_ui_amount_to_raw(quote_amount_ui, quote_decimals, "quote_amount")?;

            plan_raydium_amm_v4_create_pool(
                RaydiumAmmV4CreatePoolPlan {
                    payer,
                    program_id,
                    openbook_program_id,
                    market,
                    coin_mint: base_mint,
                    pc_mint: quote_mint,
                    init_coin_amount,
                    init_pc_amount,
                    create_fee_destination,
                },
                compute_budget,
            )?
        }
        PoolMarket::MeteoraDammV2 => {
            let base_decimals = sol
                .get_token_decimals(&base_mint)
                .await
                .map_err(ApiError::from)?;
            let quote_decimals = sol
                .get_token_decimals(&quote_mint)
                .await
                .map_err(ApiError::from)?;

            let base_amount_ui = req
                .base_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("base_amount is required"))?;
            let quote_amount_ui = req
                .quote_amount
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("quote_amount is required"))?;
            let max_amount_a =
                parse_ui_amount_to_raw(base_amount_ui, base_decimals, "base_amount")?;
            let max_amount_b =
                parse_ui_amount_to_raw(quote_amount_ui, quote_decimals, "quote_amount")?;

            let initial_price: f64 = req
                .initial_price
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("initial_price is required"))?
                .trim()
                .parse()
                .map_err(|_| ApiError::bad_request("initial_price must be a number"))?;

            let sqrt_price = compute_meteora_damm_v2_sqrt_price_x64(
                initial_price,
                base_decimals,
                quote_decimals,
            )?;

            let config_pk = req
                .meteora_damm_v2_config
                .as_deref()
                .map(|s| parse_pubkey(s, "meteora_damm_v2_config"))
                .transpose()?;
            let (config, cfg_info) = pick_meteora_damm_v2_config(
                &sol,
                config_pk.as_ref(),
                req.meteora_damm_v2_config_index,
                sqrt_price,
            )
            .await?;

            let position_nft_mint_raw = req
                .meteora_damm_v2_position_nft_mint
                .as_deref()
                .ok_or_else(|| {
                    ApiError::bad_request("meteora_damm_v2_position_nft_mint is required")
                })?;
            let position_nft_mint =
                parse_pubkey(position_nft_mint_raw, "meteora_damm_v2_position_nft_mint")?;

            let (liquidity, token_a_amount_in, token_b_amount_in) =
                compute_meteora_damm_v2_liquidity_and_amounts(
                    &cfg_info,
                    sqrt_price,
                    max_amount_a,
                    max_amount_b,
                )?;

            let program_id = crate::dex::meteora_damm_v2::METEORA_DAMM_V2_ID;
            let token_badge_a_pda = Pubkey::find_program_address(
                &[
                    crate::dex::meteora_damm_v2::TOKEN_BADGE_PREFIX,
                    base_mint.as_ref(),
                ],
                &program_id,
            )
            .0;
            let token_badge_b_pda = Pubkey::find_program_address(
                &[
                    crate::dex::meteora_damm_v2::TOKEN_BADGE_PREFIX,
                    quote_mint.as_ref(),
                ],
                &program_id,
            )
            .0;

            let token_badge_a = if base_program == TOKEN_2022_PROGRAM_ID {
                let acc = sol
                    .get_account_with_commitment_resilient(
                        &base_mint,
                        CommitmentConfig::processed(),
                    )
                    .await
                    .map_err(|e| {
                        ApiError::internal(format!("rpc getAccount(base_mint) failed: {e}"))
                    })?;
                let supported =
                    meteora_damm_v2_is_supported_mint(&base_mint, &base_program, &acc.data)?;
                if supported {
                    None
                } else if sol
                    .exists(&token_badge_a_pda)
                    .await
                    .map_err(ApiError::from)?
                {
                    Some(token_badge_a_pda)
                } else {
                    return Err(ApiError::bad_request(
                        "meteora_damm_v2 token_a requires token badge (not found)",
                    ));
                }
            } else {
                None
            };

            let token_badge_b = if quote_program == TOKEN_2022_PROGRAM_ID {
                let acc = sol
                    .get_account_with_commitment_resilient(
                        &quote_mint,
                        CommitmentConfig::processed(),
                    )
                    .await
                    .map_err(|e| {
                        ApiError::internal(format!("rpc getAccount(quote_mint) failed: {e}"))
                    })?;
                let supported =
                    meteora_damm_v2_is_supported_mint(&quote_mint, &quote_program, &acc.data)?;
                if supported {
                    None
                } else if sol
                    .exists(&token_badge_b_pda)
                    .await
                    .map_err(ApiError::from)?
                {
                    Some(token_badge_b_pda)
                } else {
                    return Err(ApiError::bad_request(
                        "meteora_damm_v2 token_b requires token badge (not found)",
                    ));
                }
            } else {
                None
            };

            // If token_b needs a badge but token_a doesn't, occupy remaining_accounts[0].
            let token_badge_a = if token_badge_a.is_none() && token_badge_b.is_some() {
                Some(program_id)
            } else {
                token_badge_a
            };

            plan_meteora_damm_v2_create_pool(
                MeteoraDammV2CreatePoolPlan {
                    payer,
                    config,
                    token_a_mint: base_mint,
                    token_b_mint: quote_mint,
                    token_a_program: base_program,
                    token_b_program: quote_program,
                    position_nft_mint,
                    liquidity,
                    sqrt_price,
                    activation_point: None,
                    token_a_amount_in,
                    token_b_amount_in,
                    token_badge_a,
                    token_badge_b,
                },
                compute_budget,
            )?
        }
        PoolMarket::MeteoraDbc => {
            let desired_quote_mint = quote_mint;

            let name = req
                .name
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("name is required"))?
                .trim();
            let symbol = req
                .symbol
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("symbol is required"))?
                .trim();
            let uri = req
                .uri
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("uri is required"))?
                .trim();
            if name.is_empty() || symbol.is_empty() || uri.is_empty() {
                return Err(ApiError::bad_request(
                    "name, symbol, and uri must be non-empty",
                ));
            }
            if name.chars().count() > 32 {
                return Err(ApiError::bad_request("name max length is 32"));
            }
            if symbol.chars().count() > 10 {
                return Err(ApiError::bad_request("symbol max length is 10"));
            }
            if uri.chars().count() > 200 {
                return Err(ApiError::bad_request("uri max length is 200"));
            }

            // Base mint must be new (initialized in-program).
            if sol.exists(&base_mint).await.map_err(ApiError::from)? {
                return Err(ApiError::bad_request(
                    "meteora_dbc base_mint already exists (must be a new mint keypair)",
                ));
            }

            let config_pk = req
                .meteora_dbc_config
                .as_deref()
                .map(|s| parse_pubkey(s, "meteora_dbc_config"))
                .transpose()?;
            let (config, cfg_state) = pick_meteora_dbc_config(
                &sol,
                config_pk.as_ref(),
                req.meteora_dbc_config_index,
                desired_quote_mint,
            )
            .await?;

            let quote_mint = cfg_state.quote_mint;
            if quote_mint != desired_quote_mint {
                return Err(ApiError::bad_request(format!(
                    "meteora_dbc config quote_mint mismatch (config={}, requested={})",
                    quote_mint, desired_quote_mint
                )));
            }

            let base_token_program = match cfg_state.token_type {
                0 => TOKEN_PROGRAM_ID,
                1 => TOKEN_2022_PROGRAM_ID,
                other => {
                    return Err(ApiError::bad_request(format!(
                        "unsupported meteora_dbc token_type: {other}"
                    )));
                }
            };
            let quote_token_program = sol.get_token_program_id(&quote_mint).await.map_err(|e| {
                ApiError::internal(format!("rpc getAccount(quote_mint) failed: {e}"))
            })?;

            plan_meteora_dbc_create_pool(
                MeteoraDbcCreatePoolPlan {
                    payer,
                    creator: payer,
                    config,
                    base_mint,
                    quote_mint,
                    quote_token_program,
                    base_token_program,
                    name: name.to_string(),
                    symbol: symbol.to_string(),
                    uri: uri.to_string(),
                },
                compute_budget,
            )?
        }
        _ => {
            return Err(ApiError::bad_request(
                "pool creation not supported for requested market",
            ));
        }
    };

    apply_priority_fee_if_requested(&sol, &mut planned, priority_fee_level).await?;

    if use_swqos && cluster_for_programs != SolanaCluster::MainnetBeta {
        return Err(ApiError::bad_request(SWQOS_MAINNET_ONLY_ERROR));
    }

    if use_swqos {
        let settings = req.swqos_settings.as_ref().ok_or_else(|| {
            ApiError::bad_request("swqos_settings is required when use_swqos=true")
        })?;
        if settings.tip_lamports == 0 {
            return Err(ApiError::bad_request("swqos tip_lamports must be > 0"));
        }
        planned.instructions.push(system_instruction_if::transfer(
            &payer,
            &tip_account_for_provider(settings.provider),
            settings.tip_lamports,
        ));
    }

    let blockhash = sol
        .get_latest_blockhash_with_commitment_resilient(CommitmentConfig::processed())
        .await
        .map_err(|e| ApiError::internal(format!("rpc blockhash fetch failed: {e}")))?
        .0;
    let tx = compile_unsigned_v0_transaction(&planned.payer, &planned.instructions, blockhash)?;
    let wire_len = bincode::serialize(&tx)
        .map_err(|e| ApiError::internal(format!("transaction serialization failed: {e}")))?
        .len();
    if wire_len > SOLANA_MAX_TX_WIRE_BYTES {
        let msg = if use_swqos {
            format!(
                "use_swqos tip would exceed Solana max transaction size (len={wire_len}, max={SOLANA_MAX_TX_WIRE_BYTES}); retry without use_swqos"
            )
        } else {
            format!(
                "built transaction exceeds Solana max transaction size (len={wire_len}, max={SOLANA_MAX_TX_WIRE_BYTES})"
            )
        };
        return Err(ApiError::bad_request(msg));
    }
    let b64 = encode_transaction_base64(&tx)?;

    let simulation = if simulate {
        let sim = sol
            .simulate_transaction_with_config_resilient(
                &tx,
                RpcSimulateTransactionConfig {
                    sig_verify: false,
                    replace_recent_blockhash: true,
                    commitment: Some(CommitmentConfig::processed()),
                    ..RpcSimulateTransactionConfig::default()
                },
            )
            .await
            .map_err(|e| ApiError::internal(format!("rpc simulation failed: {e}")))?;
        Some(PoolSimulationResponse {
            ok: sim.value.err.is_none(),
            err: sim.value.err.map(|e| format!("{e:?}")),
            units_consumed: sim.value.units_consumed,
            logs: sim.value.logs.unwrap_or_default(),
        })
    } else {
        None
    };

    Ok((
        PoolBuildResponse {
            transaction: b64,
            required_signers: planned
                .required_signers
                .iter()
                .map(|pk| pk.to_string())
                .collect(),
            derived_addresses: planned
                .derived
                .map
                .iter()
                .map(|(k, v)| (k.clone(), v.to_string()))
                .collect(),
            simulation,
        },
        rpc,
        cluster_for_programs,
    ))
}

pub(super) async fn post_build(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<PoolBuildRequest>,
) -> Result<Json<PoolBuildResponse>, ApiError> {
    let (response, _, _) = build_pool_response(state.as_ref(), req).await?;
    Ok(Json(response))
}

pub(super) async fn post_execute(
    State(state): State<Arc<ApiState>>,
    Json(mut req): Json<PoolBuildRequest>,
) -> Result<Json<PoolExecuteResponse>, ApiError> {
    if !state.allow_live_sends {
        return Err(ApiError::conflict(
            "live sends are disabled (set MAMBA_API_ENABLE_LIVE_SENDS=true to unlock)",
        ));
    }

    let mut generated_signers = BTreeMap::new();
    let mut generated_signer_map = HashMap::new();

    if req
        .base_mint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        if req.market != PoolMarket::MeteoraDbc {
            return Err(ApiError::bad_request(
                "base_mint is required for this pool market",
            ));
        }
        let base_mint_signer = Arc::new(Keypair::new());
        let base_mint_pubkey = base_mint_signer.pubkey().to_string();
        req.base_mint = Some(base_mint_pubkey.clone());
        generated_signers.insert("base_mint".to_string(), base_mint_pubkey.clone());
        generated_signer_map.insert(base_mint_signer.pubkey(), base_mint_signer);
    }

    let raydium_clmm_seed_requested = req.market == PoolMarket::RaydiumClmm
        && (req
            .base_amount
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
            || req
                .quote_amount
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some());
    if raydium_clmm_seed_requested
        && req
            .raydium_clmm_position_nft_mint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        let nft_signer = Arc::new(Keypair::new());
        let nft_pubkey = nft_signer.pubkey().to_string();
        req.raydium_clmm_position_nft_mint = Some(nft_pubkey.clone());
        generated_signers.insert("raydium_clmm_position_nft_mint".to_string(), nft_pubkey);
        generated_signer_map.insert(nft_signer.pubkey(), nft_signer);
    }

    if req.market == PoolMarket::MeteoraDammV2
        && req
            .meteora_damm_v2_position_nft_mint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        let nft_signer = Arc::new(Keypair::new());
        let nft_pubkey = nft_signer.pubkey().to_string();
        req.meteora_damm_v2_position_nft_mint = Some(nft_pubkey.clone());
        generated_signers.insert("meteora_damm_v2_position_nft_mint".to_string(), nft_pubkey);
        generated_signer_map.insert(nft_signer.pubkey(), nft_signer);
    }

    let use_swqos = req.use_swqos.unwrap_or(false);
    let swqos_settings = req.swqos_settings.as_ref().map(|settings| SWQoSettings {
        provider: settings.provider,
        tip_lamports: settings.tip_lamports,
        jito_key: None,
        nextblock_key: String::new(),
        zero_slot_key: String::new(),
        temporal_key: String::new(),
        blox_key: String::new(),
        nonce_account: None,
    });

    let rpc_url_override = req.rpc_url.clone();
    let (build, rpc, cluster) = build_pool_response(state.as_ref(), req).await?;
    enforce_live_send_cluster_match(state.as_ref(), rpc_url_override.as_deref(), cluster)?;
    if let Some(simulation) = build.simulation.as_ref()
        && !simulation.ok
    {
        return Ok(Json(PoolExecuteResponse {
            submitted: false,
            success: false,
            signature: None,
            error: Some(
                simulation
                    .err
                    .clone()
                    .unwrap_or_else(|| "simulation failed".to_string()),
            ),
            cluster: format!("{cluster:?}"),
            generated_signers,
            build,
        }));
    }

    let signers = resolve_required_signers(
        state.as_ref(),
        &build.required_signers,
        &generated_signer_map,
    )?;
    let unsigned = decode_versioned_transaction_base64(&build.transaction)?;
    let signed = sign_versioned_transaction(&unsigned, &signers)?;

    match submit_signed_transaction(rpc, cluster, &signed, use_swqos, swqos_settings).await {
        Ok(signature) => Ok(Json(PoolExecuteResponse {
            submitted: true,
            success: true,
            signature: Some(signature.to_string()),
            error: None,
            cluster: format!("{cluster:?}"),
            generated_signers,
            build,
        })),
        Err(error) => Ok(Json(PoolExecuteResponse {
            submitted: false,
            success: false,
            signature: None,
            error: Some(error.message),
            cluster: format!("{cluster:?}"),
            generated_signers,
            build,
        })),
    }
}

fn parse_priority_fee_level(raw: &str) -> Option<PriorityFeeLevel> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "low" => Some(PriorityFeeLevel::Low),
        "medium" | "med" => Some(PriorityFeeLevel::Medium),
        "high" => Some(PriorityFeeLevel::High),
        "turbo" => Some(PriorityFeeLevel::Turbo),
        "max" => Some(PriorityFeeLevel::Max),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_meteora_dlmm_customizable_params, get_methods, is_rate_limited_pool_positions_error,
        is_retryable_pool_positions_error, parse_ui_amount_to_raw,
    };

    #[test]
    fn test_parse_ui_amount_to_raw_scales_whole_and_fractional_ui_amounts() {
        assert_eq!(
            parse_ui_amount_to_raw("100000", 6, "base_amount").expect("whole UI amount"),
            100_000_000_000
        );
        assert_eq!(
            parse_ui_amount_to_raw("0.01", 9, "quote_amount").expect("fractional UI amount"),
            10_000_000
        );
        assert_eq!(
            parse_ui_amount_to_raw(".5", 6, "base_amount").expect("leading decimal point"),
            500_000
        );
    }

    #[test]
    fn test_parse_ui_amount_to_raw_rejects_extra_precision() {
        let err = parse_ui_amount_to_raw("1.234", 2, "base_amount")
            .expect_err("extra precision must fail");
        let err_text = format!("{err:?}");
        assert!(
            err_text.contains("too many decimal places"),
            "unexpected error: {err_text}"
        );
    }

    #[test]
    fn test_pool_positions_rate_limit_errors_are_retryable() {
        assert!(is_retryable_pool_positions_error(
            "pool positions lookup failed: http 429 too many requests"
        ));
        assert!(is_rate_limited_pool_positions_error(
            "pool positions lookup failed: http 429 too many requests"
        ));
    }

    #[test]
    fn test_pool_positions_validation_errors_are_not_retryable() {
        assert!(!is_retryable_pool_positions_error(
            "invalid owner pubkey: bad base58"
        ));
        assert!(!is_rate_limited_pool_positions_error(
            "invalid owner pubkey: bad base58"
        ));
    }

    #[test]
    fn test_build_meteora_dlmm_customizable_params_serializes_canonical_layout() {
        let params = build_meteora_dlmm_customizable_params(
            -42,
            25,
            10_000,
            1,
            true,
            Some(123_456),
            false,
            0,
        )
        .expect("customizable params");
        let mut serialized = Vec::new();
        anchor_lang::AnchorSerialize::serialize(&params, &mut serialized)
            .expect("customizable params should serialize");

        let mut expected_prefix = Vec::new();
        expected_prefix.extend_from_slice(&(-42i32).to_le_bytes());
        expected_prefix.extend_from_slice(&25u16.to_le_bytes());
        expected_prefix.extend_from_slice(&10_000u16.to_le_bytes());
        expected_prefix.push(1);
        expected_prefix.push(1);
        expected_prefix.push(1);
        expected_prefix.extend_from_slice(&123_456u64.to_le_bytes());
        expected_prefix.push(0);
        expected_prefix.push(0);

        assert_eq!(serialized.len(), 83);
        assert_eq!(
            &serialized[..expected_prefix.len()],
            expected_prefix.as_slice()
        );
        assert!(
            serialized[expected_prefix.len()..]
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    #[tokio::test]
    async fn test_pool_methods_advertise_execute_generated_signers() {
        let methods = get_methods().await.expect("pool methods should build").0;
        let meteora_damm_v2 = methods
            .iter()
            .find(|method| method.market == "meteora_damm_v2")
            .expect("meteora_damm_v2 spec");
        assert_eq!(
            meteora_damm_v2.execute_generated_fields,
            vec!["meteora_damm_v2_position_nft_mint"]
        );

        let meteora_dbc = methods
            .iter()
            .find(|method| method.market == "meteora_dbc")
            .expect("meteora_dbc spec");
        assert_eq!(meteora_dbc.execute_generated_fields, vec!["base_mint"]);
    }
}
