use {
    super::{
        ApiError, ApiState, decode_versioned_transaction_base64, enforce_live_send_cluster_match,
        resolve_request_rpc, resolve_required_signers, rpc_client_for_attempt,
        sign_versioned_transaction, submit_signed_transaction,
    },
    crate::compute_budget::compute_budget::ix_set_compute_unit_price,
    crate::core::{
        cluster::SolanaCluster,
        create::{
            ComputeBudgetPlan, PumpFunAutoBuyExactSolInPlan, PumpFunCreatePlan,
            RaydiumLaunchpadAmmFeeOn, RaydiumLaunchpadAutoBuyExactSolInPlan,
            RaydiumLaunchpadBaseTokenProgram, RaydiumLaunchpadCreatePlan,
            RaydiumLaunchpadCurveParams, RaydiumLaunchpadTransferFeeExtensionParams,
            RaydiumLaunchpadVestingParams, SOLANA_MAX_TX_WIRE_BYTES, SplToken2022CreatePlan,
            SplTokenCreatePlan, compile_unsigned_v0_transaction, encode_transaction_base64,
            plan_pump_fun_create, plan_pump_fun_create_and_buy_exact_sol_in,
            plan_raydium_launchpad_create, plan_raydium_launchpad_create_and_buy_exact_sol_in,
            plan_spl_token_2022_create, plan_spl_token_create,
            spl_token_2022_inline_metadata_required_space,
        },
        sol::{PriorityFeeLevel, SolHook, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT},
    },
    crate::dex::pump_fun::{MAYHEM_FEE_RECIPIENT, PUMP_FUN_ID},
    crate::dex::raydium_launchpad::{
        GLOBAL_CONFIG_DISCRIM, LAUNCHPAD_GLOBAL_CONFIG_ACCOUNT_LEN, PLATFORM_CONFIG_DISCRIM,
        RaydiumLaunchpad, RaydiumLaunchpadGlobalConfigState,
    },
    crate::swqos::{SWQoSettings, SwqosProvider, tip_account_for_provider},
    axum::{
        Json,
        extract::{Path, Query, State},
    },
    borsh::BorshDeserialize,
    serde::{Deserialize, Serialize},
    solana_account_decoder_client_types::UiAccountEncoding,
    solana_client::{
        rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
        rpc_filter::{Memcmp, RpcFilterType},
    },
    solana_commitment_config::CommitmentConfig,
    solana_keypair::Keypair,
    solana_program::{hash::Hash, program_pack::Pack, pubkey::Pubkey},
    solana_rpc_client_types::config::RpcSimulateTransactionConfig,
    solana_signer::Signer,
    solana_system_interface::instruction as system_instruction_if,
    std::{
        collections::{BTreeMap, BTreeSet, HashMap},
        io::Cursor,
        str::FromStr,
        sync::Arc,
    },
};

const MAX_NAME_LEN: usize = 32;
const MAX_SYMBOL_LEN: usize = 10;
const MAX_URI_LEN: usize = 200;
const SWQOS_MAINNET_ONLY_ERROR: &str =
    "use_swqos is only supported on mainnet-beta sender infrastructure";

#[derive(Debug, Deserialize)]
pub(super) struct RpcUrlQuery {
    rpc_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RaydiumPlatformCurveParamsQuery {
    rpc_url: Option<String>,
    global_config: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CreateMethod {
    PumpFun,
    RaydiumLaunchpad,
    SplToken,
    #[serde(rename = "spl_token_2022", alias = "spl_token2022")]
    SplToken2022,
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateBuildRequest {
    method: CreateMethod,
    payer: String,
    mint: Option<String>,
    name: String,
    symbol: String,
    uri: String,
    auto_buy: Option<AutoBuyParams>,
    decimals: Option<u8>,
    initial_supply: Option<u64>,
    freeze_authority: Option<bool>,
    revoke_mint_authority: Option<bool>,
    revoke_freeze_authority: Option<bool>,
    metadata_is_mutable: Option<bool>,
    simulate: Option<bool>,
    rpc_url: Option<String>,
    priority_fee_level: Option<String>,
    compute_unit_limit: Option<u32>,
    use_swqos: Option<bool>,
    swqos_settings: Option<ApiBuildSwqosSettings>,
    raydium_launchpad: Option<RaydiumLaunchpadBuildParams>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiBuildSwqosSettings {
    provider: SwqosProvider,
    tip_lamports: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct AutoBuyParams {
    buy_sol: f64,
    slippage_pct: f64,
}

#[allow(dead_code)]
#[derive(Debug, BorshDeserialize)]
#[borsh(crate = "borsh")]
struct PumpFunGlobalPrefix {
    initialized: bool,
    authority: Pubkey,
    fee_recipient: Pubkey,
    initial_virtual_token_reserves: u64,
    initial_virtual_sol_reserves: u64,
    initial_real_token_reserves: u64,
    token_total_supply: u64,
    fee_basis_points: u64,
    withdraw_authority: Pubkey,
    enable_migrate: bool,
    pool_migration_fee: u64,
    creator_fee_basis_points: u64,
    fee_recipients: [Pubkey; 7],
    set_creator_authority: Pubkey,
    admin_set_creator_authority: Pubkey,
    create_v2_enabled: bool,
    whitelist_pda: Pubkey,
    reserved_fee_recipient: Pubkey,
    mayhem_mode_enabled: bool,
    reserved_fee_recipients: [Pubkey; 7],
    is_cashback_enabled: bool,
}

#[derive(Debug, BorshDeserialize)]
#[borsh(crate = "borsh")]
struct PumpFeesFees {
    #[allow(dead_code)]
    lp_fee_bps: u64,
    protocol_fee_bps: u64,
    creator_fee_bps: u64,
}

#[derive(Debug, BorshDeserialize)]
#[borsh(crate = "borsh")]
struct PumpFeesFeeTier {
    market_cap_lamports_threshold: u128,
    fees: PumpFeesFees,
}

#[derive(Debug, BorshDeserialize)]
#[borsh(crate = "borsh")]
struct PumpFeesFeeConfig {
    #[allow(dead_code)]
    bump: u8,
    #[allow(dead_code)]
    admin: Pubkey,
    #[allow(dead_code)]
    flat_fees: PumpFeesFees,
    fee_tiers: Vec<PumpFeesFeeTier>,
}

#[derive(Debug, Clone, Deserialize)]
struct RaydiumLaunchpadBuildParams {
    global_config: String,
    platform_config: String,
    base_token_program: Option<String>,
    curve: RaydiumLaunchpadCurveBuildParams,
    vesting: RaydiumLaunchpadVestingBuildParams,
    amm_fee_on: RaydiumLaunchpadAmmFeeOnBuildParam,
    transfer_fee_extension: Option<RaydiumLaunchpadTransferFeeExtensionBuildParams>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum RaydiumLaunchpadCurveBuildParams {
    Constant {
        supply: u64,
        total_base_sell: u64,
        total_quote_fund_raising: u64,
        migrate_type: u8,
    },
    Fixed {
        supply: u64,
        total_quote_fund_raising: u64,
        migrate_type: u8,
    },
    Linear {
        supply: u64,
        total_quote_fund_raising: u64,
        migrate_type: u8,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct RaydiumLaunchpadVestingBuildParams {
    total_locked_amount: u64,
    cliff_period: u64,
    unlock_period: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RaydiumLaunchpadAmmFeeOnBuildParam {
    QuoteToken,
    BothToken,
}

#[derive(Debug, Clone, Deserialize)]
struct RaydiumLaunchpadTransferFeeExtensionBuildParams {
    transfer_fee_basis_points: u16,
    maximum_fee: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct CreateMethodSpec {
    method: &'static str,
    required_fields: Vec<&'static str>,
    optional_fields: Vec<&'static str>,
    execute_generated_fields: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct CreateBuildResponse {
    transaction: String,
    required_signers: Vec<String>,
    derived_addresses: BTreeMap<String, String>,
    mint_token_program: String,
    simulation: Option<CreateSimulationResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct CreateSimulationResponse {
    ok: bool,
    err: Option<String>,
    units_consumed: Option<u64>,
    logs: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct CreateExecuteResponse {
    submitted: bool,
    success: bool,
    signature: Option<String>,
    error: Option<String>,
    cluster: String,
    generated_signers: BTreeMap<String, String>,
    build: CreateBuildResponse,
}

#[derive(Debug, Serialize)]
pub(super) struct RaydiumLaunchpadGlobalConfigResponse {
    pubkey: String,
    curve_type: u8,
    trade_fee_rate: u64,
    max_share_fee_rate: u64,
    quote_mint: String,
}

#[derive(Debug, Serialize)]
pub(super) struct RaydiumLaunchpadPlatformConfigResponse {
    pubkey: String,
    platform_fee_wallet: String,
    fee_rate: u64,
    creator_fee_rate: u64,
    name: String,
    web: String,
    img: String,
    curve_params_len: u32,
    curve_params_global_configs: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct RaydiumLaunchpadPlatformCurveParamResponse {
    epoch: u64,
    index: u8,
    global_config: String,
    migrate_type: u8,
    amm_fee_on: String,
    supply: u64,
    total_base_sell: u64,
    total_quote_fund_raising: u64,
    vesting_total_locked_amount: u64,
    vesting_cliff_period: u64,
    vesting_unlock_period: u64,
}

pub(super) async fn get_methods() -> Result<Json<Vec<CreateMethodSpec>>, ApiError> {
    Ok(Json(vec![
        CreateMethodSpec {
            method: "pump_fun",
            required_fields: vec!["method", "payer", "mint", "name", "symbol", "uri"],
            optional_fields: vec![
                "auto_buy.buy_sol",
                "auto_buy.slippage_pct",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec!["mint"],
        },
        CreateMethodSpec {
            method: "spl_token",
            required_fields: vec![
                "method", "payer", "mint", "name", "symbol", "uri", "decimals",
            ],
            optional_fields: vec![
                "initial_supply",
                "freeze_authority",
                "revoke_mint_authority",
                "revoke_freeze_authority",
                "metadata_is_mutable",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec!["mint"],
        },
        CreateMethodSpec {
            method: "spl_token_2022",
            required_fields: vec![
                "method", "payer", "mint", "name", "symbol", "uri", "decimals",
            ],
            optional_fields: vec![
                "initial_supply",
                "freeze_authority",
                "revoke_mint_authority",
                "revoke_freeze_authority",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec!["mint"],
        },
        CreateMethodSpec {
            method: "raydium_launchpad",
            required_fields: vec![
                "method",
                "payer",
                "mint",
                "name",
                "symbol",
                "uri",
                "decimals",
                "raydium_launchpad.global_config",
                "raydium_launchpad.platform_config",
                "raydium_launchpad.curve",
                "raydium_launchpad.vesting",
                "raydium_launchpad.amm_fee_on",
            ],
            optional_fields: vec![
                "auto_buy.buy_sol",
                "auto_buy.slippage_pct",
                "raydium_launchpad.base_token_program",
                "raydium_launchpad.transfer_fee_extension",
                "simulate",
                "rpc_url",
                "priority_fee_level",
                "compute_unit_limit",
            ],
            execute_generated_fields: vec!["mint"],
        },
    ]))
}

pub(super) async fn get_raydium_launchpad_global_configs(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RpcUrlQuery>,
) -> Result<Json<Vec<RaydiumLaunchpadGlobalConfigResponse>>, ApiError> {
    let base_offset = state
        .endpoint_cursor
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let rpc_url_override = query
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
    let program_id = crate::core::cluster::raydium_launchpad_program_id(cluster_for_programs);

    let accounts = sol
        .get_program_ui_accounts_with_config_resilient(
            &program_id,
            RpcProgramAccountsConfig {
                filters: Some(vec![
                    RpcFilterType::DataSize(LAUNCHPAD_GLOBAL_CONFIG_ACCOUNT_LEN as u64),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        0,
                        GLOBAL_CONFIG_DISCRIM.as_ref(),
                    )),
                ]),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    commitment: Some(CommitmentConfig::confirmed()),
                    ..Default::default()
                },
                with_context: None,
                sort_results: None,
            },
        )
        .await
        .map_err(|e| {
            ApiError::internal(format!("rpc getProgramAccounts(global_config) failed: {e}"))
        })?;

    let mut out = Vec::with_capacity(accounts.len());
    let mut decode_errors: Vec<String> = Vec::new();
    for (pubkey, ui_account) in accounts {
        let bytes = ui_account.data.decode().ok_or_else(|| {
            ApiError::internal(format!("failed to decode account data for {pubkey}"))
        })?;
        let state = match RaydiumLaunchpad::decode_global_config_account_data(&bytes) {
            Ok(state) => state,
            Err(e) => {
                decode_errors.push(format!("{pubkey}: {e:#}"));
                continue;
            }
        };
        out.push(RaydiumLaunchpadGlobalConfigResponse {
            pubkey: pubkey.to_string(),
            curve_type: state.curve_type,
            trade_fee_rate: state.trade_fee_rate,
            max_share_fee_rate: state.max_share_fee_rate,
            quote_mint: state.quote_mint.to_string(),
        });
    }
    if out.is_empty() && !decode_errors.is_empty() {
        return Err(ApiError::internal(format!(
            "raydium launchpad global_config decode failed (first error): {}",
            decode_errors[0]
        )));
    }
    out.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));

    Ok(Json(out))
}

pub(super) async fn get_raydium_launchpad_platform_configs(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RpcUrlQuery>,
) -> Result<Json<Vec<RaydiumLaunchpadPlatformConfigResponse>>, ApiError> {
    let base_offset = state
        .endpoint_cursor
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let rpc_url_override = query
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
    let program_id = crate::core::cluster::raydium_launchpad_program_id(cluster_for_programs);

    let accounts = sol
        .get_program_ui_accounts_with_config_resilient(
            &program_id,
            RpcProgramAccountsConfig {
                filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    0,
                    PLATFORM_CONFIG_DISCRIM.as_ref(),
                ))]),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    commitment: Some(CommitmentConfig::confirmed()),
                    ..Default::default()
                },
                with_context: None,
                sort_results: None,
            },
        )
        .await
        .map_err(|e| {
            ApiError::internal(format!(
                "rpc getProgramAccounts(platform_config) failed: {e}"
            ))
        })?;

    let mut out = Vec::with_capacity(accounts.len());
    let mut decode_errors: Vec<String> = Vec::new();
    for (pubkey, ui_account) in accounts {
        let bytes = ui_account.data.decode().ok_or_else(|| {
            ApiError::internal(format!("failed to decode account data for {pubkey}"))
        })?;
        let state = match RaydiumLaunchpad::decode_platform_config_account_data(&bytes) {
            Ok(state) => state,
            Err(e) => {
                decode_errors.push(format!("{pubkey}: {e:#}"));
                continue;
            }
        };
        let info = match RaydiumLaunchpad::decode_platform_config_info(&bytes) {
            Ok(info) => info,
            Err(e) => {
                decode_errors.push(format!("{pubkey}: {e:#}"));
                continue;
            }
        };
        let curve_params = match RaydiumLaunchpad::decode_platform_curve_params(&bytes) {
            Ok(params) => params,
            Err(e) => {
                decode_errors.push(format!("{pubkey}: {e:#}"));
                continue;
            }
        };

        let mut globals = BTreeSet::new();
        let mut curve_params_len: u32 = 0;
        for param in &curve_params {
            let bonding = &param.bonding_curve_param;
            if bonding.supply == 0 || bonding.total_quote_fund_raising == 0 {
                continue;
            }
            if !matches!(bonding.migrate_type, 0 | 1) {
                continue;
            }
            globals.insert(param.global_config.to_string());
            curve_params_len = curve_params_len.saturating_add(1);
        }
        let curve_params_global_configs = globals.into_iter().collect::<Vec<_>>();
        out.push(RaydiumLaunchpadPlatformConfigResponse {
            pubkey: pubkey.to_string(),
            platform_fee_wallet: state.platform_fee_wallet.to_string(),
            fee_rate: state.fee_rate,
            creator_fee_rate: state.creator_fee_rate,
            name: info.name,
            web: info.web,
            img: info.img,
            curve_params_len,
            curve_params_global_configs,
        });
    }
    if out.is_empty() && !decode_errors.is_empty() {
        return Err(ApiError::internal(format!(
            "raydium launchpad platform_config decode failed (first error): {}",
            decode_errors[0]
        )));
    }
    out.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));

    Ok(Json(out))
}

pub(super) async fn get_raydium_launchpad_platform_curve_params(
    State(state): State<Arc<ApiState>>,
    Path(platform_config): Path<String>,
    Query(query): Query<RaydiumPlatformCurveParamsQuery>,
) -> Result<Json<Vec<RaydiumLaunchpadPlatformCurveParamResponse>>, ApiError> {
    let platform_config = Pubkey::from_str(platform_config.trim())
        .map_err(|_| ApiError::bad_request("invalid platform_config pubkey"))?;

    let global_config_filter = query
        .global_config
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(Pubkey::from_str)
        .transpose()
        .map_err(|_| ApiError::bad_request("invalid global_config pubkey"))?;

    let base_offset = state
        .endpoint_cursor
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let rpc_url_override = query
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
    let program_id = crate::core::cluster::raydium_launchpad_program_id(cluster_for_programs);

    let acc = sol
        .get_account_with_commitment_resilient(&platform_config, CommitmentConfig::processed())
        .await
        .map_err(|e| ApiError::internal(format!("rpc getAccount(platform_config) failed: {e}")))?;
    if acc.owner != program_id {
        return Err(ApiError::bad_request(format!(
            "raydium launchpad platform_config owner mismatch: {}",
            acc.owner
        )));
    }

    let curve_params = RaydiumLaunchpad::decode_platform_curve_params(&acc.data).map_err(|e| {
        ApiError::internal(format!(
            "raydium launchpad platform_config decode curve params failed for {platform_config}: {e:#}"
        ))
    })?;

    let mut out = curve_params
        .into_iter()
        .filter(|param| {
            global_config_filter
                .map(|filter| param.global_config == filter)
                .unwrap_or(true)
        })
        .filter(|param| {
            let bonding = &param.bonding_curve_param;
            if bonding.supply == 0 || bonding.total_quote_fund_raising == 0 {
                return false;
            }
            matches!(bonding.migrate_type, 0 | 1)
        })
        .map(|param| {
            let migrate_type = param.bonding_curve_param.migrate_type;
            let amm_fee_on = match param.bonding_curve_param.amm_fee_on {
                2 => "both_token".to_string(),
                _ => "quote_token".to_string(),
            };
            RaydiumLaunchpadPlatformCurveParamResponse {
                epoch: param.epoch,
                index: param.index,
                global_config: param.global_config.to_string(),
                migrate_type,
                amm_fee_on,
                supply: param.bonding_curve_param.supply,
                total_base_sell: param.bonding_curve_param.total_base_sell,
                total_quote_fund_raising: param.bonding_curve_param.total_quote_fund_raising,
                vesting_total_locked_amount: param.bonding_curve_param.total_locked_amount,
                vesting_cliff_period: param.bonding_curve_param.cliff_period,
                vesting_unlock_period: param.bonding_curve_param.unlock_period,
            }
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| a.index.cmp(&b.index).then(a.epoch.cmp(&b.epoch)));

    Ok(Json(out))
}

async fn build_create_response(
    state: &ApiState,
    mut request: CreateBuildRequest,
) -> Result<
    (
        CreateBuildResponse,
        Arc<solana_client::nonblocking::rpc_client::RpcClient>,
        SolanaCluster,
    ),
    ApiError,
> {
    let payer = Pubkey::from_str(request.payer.trim())
        .map_err(|_| ApiError::bad_request("invalid payer pubkey"))?;
    let mint_raw = request
        .mint
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("mint is required"))?;
    let mint = Pubkey::from_str(mint_raw.trim())
        .map_err(|_| ApiError::bad_request("invalid mint pubkey"))?;

    let name = request.name.trim().to_string();
    let symbol = request.symbol.trim().to_string();
    let uri = request.uri.trim().to_string();

    validate_token_fields(&name, &symbol, &uri)?;

    let simulate = request.simulate.unwrap_or(true);
    if request.auto_buy.is_some()
        && !matches!(
            request.method,
            CreateMethod::PumpFun | CreateMethod::RaydiumLaunchpad
        )
    {
        return Err(ApiError::bad_request(
            "auto_buy is only supported for method=pump_fun or method=raydium_launchpad",
        ));
    }

    let base_offset = state
        .endpoint_cursor
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let rpc_url_override = request
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
    let raydium_launchpad_program_id =
        crate::core::cluster::raydium_launchpad_program_id(cluster_for_programs);
    let use_swqos = request.use_swqos.unwrap_or(false);

    let priority_fee_level = request
        .priority_fee_level
        .as_deref()
        .and_then(parse_priority_fee_level);
    let mut compute_budget = ComputeBudgetPlan {
        compute_unit_price_micro_lamports: None,
        compute_unit_limit: request.compute_unit_limit,
    };
    if request.auto_buy.is_some() && compute_budget.compute_unit_limit.is_none() {
        match request.method {
            CreateMethod::PumpFun => {
                // Combined pump.fun create+buy is heavier than create-only. Default high enough to
                // avoid unexpected compute failures while keeping the flow atomic.
                compute_budget.compute_unit_limit = Some(800_000);
            }
            CreateMethod::RaydiumLaunchpad => {
                // Raydium Launchpad initialize+buy is heavier than initialize-only.
                compute_budget.compute_unit_limit = Some(1_000_000);
            }
            _ => {}
        }
    }

    let (mut planned, mint_token_program) = match request.method {
        CreateMethod::PumpFun => {
            let global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
            let global_acc = sol
                .get_account_with_commitment_resilient(&global, CommitmentConfig::processed())
                .await
                .map_err(|e| ApiError::internal(format!("rpc getAccount failed: {e}")))?;
            if global_acc.data.len() < 8 {
                return Err(ApiError::internal("pump.fun global account too short"));
            }
            let mut cur = Cursor::new(&global_acc.data[8..]);
            let global_state = PumpFunGlobalPrefix::deserialize_reader(&mut cur)
                .map_err(|e| ApiError::internal(format!("decode pump.fun global failed: {e}")))?;
            let is_mayhem_mode = global_state.mayhem_mode_enabled;
            let fee_recipient = if is_mayhem_mode {
                if global_state.reserved_fee_recipient != Pubkey::default() {
                    global_state.reserved_fee_recipient
                } else {
                    MAYHEM_FEE_RECIPIENT
                }
            } else {
                global_state.fee_recipient
            };

            if let Some(auto_buy) = request.auto_buy.take() {
                if !auto_buy.buy_sol.is_finite() || auto_buy.buy_sol <= 0.0 {
                    return Err(ApiError::bad_request("auto_buy.buy_sol must be > 0"));
                }
                if !auto_buy.slippage_pct.is_finite() || auto_buy.slippage_pct < 0.0 {
                    return Err(ApiError::bad_request("auto_buy.slippage_pct must be >= 0"));
                }

                let slippage_pct = if auto_buy.slippage_pct <= 1.0 {
                    auto_buy.slippage_pct * 100.0
                } else {
                    auto_buy.slippage_pct
                };
                if slippage_pct > 99.0 {
                    return Err(ApiError::bad_request("auto_buy.slippage_pct must be <= 99"));
                }

                let buy_sol_lamports = (auto_buy.buy_sol * 1_000_000_000.0).ceil() as u64;
                if buy_sol_lamports == 0 {
                    return Err(ApiError::bad_request("auto_buy.buy_sol must be > 0"));
                }

                let slippage_bps = (slippage_pct * 100.0).ceil() as u128;
                let scale = 10_000u128;
                let spendable_sol_in = buy_sol_lamports;

                let fee_config_acc = sol
                    .get_account_with_commitment_resilient(
                        &crate::dex::pump_fun::FEE_CONFIG,
                        CommitmentConfig::processed(),
                    )
                    .await
                    .ok();

                let (protocol_fee_bps, creator_fee_bps) = match fee_config_acc {
                    Some(acc) if acc.data.len() >= 8 => {
                        let mut cur = Cursor::new(&acc.data[8..]);
                        match PumpFeesFeeConfig::deserialize_reader(&mut cur) {
                            Ok(cfg) if !cfg.fee_tiers.is_empty() => {
                                let virtual_sol_reserves =
                                    global_state.initial_virtual_sol_reserves as u128;
                                let virtual_token_reserves =
                                    global_state.initial_virtual_token_reserves as u128;
                                let mint_supply = global_state.token_total_supply as u128;
                                if virtual_token_reserves == 0 {
                                    return Err(ApiError::internal(
                                        "pump.fun virtual_token_reserves is zero",
                                    ));
                                }
                                let market_cap_lamports = virtual_sol_reserves
                                    .saturating_mul(mint_supply)
                                    / virtual_token_reserves;
                                let first = cfg.fee_tiers.first().ok_or_else(|| {
                                    ApiError::internal("pump.fun fee tiers empty")
                                })?;
                                let selected =
                                    if market_cap_lamports < first.market_cap_lamports_threshold {
                                        &first.fees
                                    } else {
                                        cfg.fee_tiers
                                            .iter()
                                            .rev()
                                            .find(|tier| {
                                                market_cap_lamports
                                                    >= tier.market_cap_lamports_threshold
                                            })
                                            .map(|tier| &tier.fees)
                                            .unwrap_or(&first.fees)
                                    };
                                (selected.protocol_fee_bps, selected.creator_fee_bps)
                            }
                            _ => (
                                global_state.fee_basis_points,
                                global_state.creator_fee_basis_points,
                            ),
                        }
                    }
                    _ => (
                        global_state.fee_basis_points,
                        global_state.creator_fee_basis_points,
                    ),
                };

                let total_fee_bps =
                    protocol_fee_bps.saturating_add(creator_fee_bps).min(10_000) as u128;

                let spendable_sol_in_u128 = spendable_sol_in as u128;
                let net_sol = spendable_sol_in_u128.saturating_mul(scale)
                    / scale.saturating_add(total_fee_bps);

                let ceil_div = |num: u128, denom: u128| -> u128 {
                    if denom == 0 {
                        return 0;
                    }
                    num.saturating_add(denom.saturating_sub(1)) / denom
                };
                let protocol_fee =
                    ceil_div(net_sol.saturating_mul(protocol_fee_bps as u128), scale);
                let creator_fee = ceil_div(net_sol.saturating_mul(creator_fee_bps as u128), scale);
                let fees_total = protocol_fee.saturating_add(creator_fee);

                let mut net_sol = net_sol;
                if net_sol.saturating_add(fees_total) > spendable_sol_in_u128 {
                    let delta = net_sol
                        .saturating_add(fees_total)
                        .saturating_sub(spendable_sol_in_u128);
                    net_sol = net_sol.saturating_sub(delta);
                }

                if net_sol <= 1 {
                    return Err(ApiError::bad_request(
                        "auto_buy.buy_sol too small after fees",
                    ));
                }

                let virtual_sol_reserves = global_state.initial_virtual_sol_reserves as u128;
                let virtual_token_reserves = global_state.initial_virtual_token_reserves as u128;
                let denom = virtual_sol_reserves.saturating_add(net_sol.saturating_sub(1));
                if denom == 0 || virtual_token_reserves == 0 {
                    return Err(ApiError::internal("pump.fun global reserves invalid"));
                }

                let mut expected_tokens_out = net_sol
                    .saturating_sub(1)
                    .saturating_mul(virtual_token_reserves)
                    / denom;
                expected_tokens_out =
                    expected_tokens_out.min(global_state.initial_real_token_reserves as u128);

                let token_amount_out = expected_tokens_out;
                if token_amount_out == 0 {
                    return Err(ApiError::bad_request(
                        "auto_buy.buy_sol too small for any token amount",
                    ));
                }

                let min_tokens_out =
                    token_amount_out.saturating_mul(scale.saturating_sub(slippage_bps)) / scale;
                if min_tokens_out == 0 {
                    return Err(ApiError::bad_request(
                        "auto_buy.buy_sol too small for any token amount after slippage",
                    ));
                }

                let create_plan = PumpFunCreatePlan {
                    payer,
                    mint,
                    name,
                    symbol,
                    uri,
                    is_mayhem_mode,
                };
                let buy_plan = PumpFunAutoBuyExactSolInPlan {
                    spendable_sol_in,
                    min_tokens_out: min_tokens_out.min(u64::MAX as u128) as u64,
                    fee_recipient,
                    track_volume: false,
                };

                // Use the "exact SOL in" buy path: user specifies a SOL budget + slippage, and the
                // program computes tokens_out using the forward quote formula.
                (
                    plan_pump_fun_create_and_buy_exact_sol_in(
                        create_plan,
                        buy_plan,
                        compute_budget,
                    )?,
                    if is_mayhem_mode {
                        TOKEN_2022_PROGRAM_ID
                    } else {
                        TOKEN_PROGRAM_ID
                    },
                )
            } else {
                (
                    plan_pump_fun_create(
                        PumpFunCreatePlan {
                            payer,
                            mint,
                            name,
                            symbol,
                            uri,
                            is_mayhem_mode,
                        },
                        compute_budget,
                    )?,
                    if is_mayhem_mode {
                        TOKEN_2022_PROGRAM_ID
                    } else {
                        TOKEN_PROGRAM_ID
                    },
                )
            }
        }
        CreateMethod::SplToken => {
            let decimals = request
                .decimals
                .ok_or_else(|| ApiError::bad_request("decimals is required for spl_token"))?;
            let mint_rent_lamports = sol
                .get_minimum_balance_for_rent_exemption_resilient(
                    <spl_token::state::Mint as Pack>::LEN,
                )
                .await
                .map_err(|e| ApiError::internal(format!("rpc rent-exempt lookup failed: {e}")))?;
            (
                plan_spl_token_create(
                    SplTokenCreatePlan {
                        payer,
                        mint,
                        name,
                        symbol,
                        uri,
                        decimals,
                        initial_supply: request.initial_supply.unwrap_or(0),
                        freeze_authority: request.freeze_authority.unwrap_or(false),
                        revoke_mint_authority: request.revoke_mint_authority.unwrap_or(false),
                        revoke_freeze_authority: request.revoke_freeze_authority.unwrap_or(false),
                        metadata_is_mutable: request.metadata_is_mutable.unwrap_or(true),
                    },
                    compute_budget,
                    mint_rent_lamports,
                )?,
                TOKEN_PROGRAM_ID,
            )
        }
        CreateMethod::SplToken2022 => {
            let decimals = request
                .decimals
                .ok_or_else(|| ApiError::bad_request("decimals is required for spl_token_2022"))?;
            let mint_space =
                spl_token_2022_inline_metadata_required_space(payer, mint, &name, &symbol, &uri)?;
            let mint_rent_lamports = sol
                .get_minimum_balance_for_rent_exemption_resilient(mint_space)
                .await
                .map_err(|e| ApiError::internal(format!("rpc rent-exempt lookup failed: {e}")))?;
            (
                plan_spl_token_2022_create(
                    SplToken2022CreatePlan {
                        payer,
                        mint,
                        name,
                        symbol,
                        uri,
                        decimals,
                        initial_supply: request.initial_supply.unwrap_or(0),
                        freeze_authority: request.freeze_authority.unwrap_or(false),
                        revoke_mint_authority: request.revoke_mint_authority.unwrap_or(false),
                        revoke_freeze_authority: request.revoke_freeze_authority.unwrap_or(false),
                    },
                    compute_budget,
                    mint_rent_lamports,
                )?,
                TOKEN_2022_PROGRAM_ID,
            )
        }
        CreateMethod::RaydiumLaunchpad => {
            let decimals = request.decimals.ok_or_else(|| {
                ApiError::bad_request("decimals is required for raydium_launchpad")
            })?;
            let params = request.raydium_launchpad.ok_or_else(|| {
                ApiError::bad_request("raydium_launchpad params are required for raydium_launchpad")
            })?;

            let global_config = Pubkey::from_str(params.global_config.trim()).map_err(|_| {
                ApiError::bad_request("invalid raydium_launchpad.global_config pubkey")
            })?;
            let platform_config =
                Pubkey::from_str(params.platform_config.trim()).map_err(|_| {
                    ApiError::bad_request("invalid raydium_launchpad.platform_config pubkey")
                })?;

            let base_token_program =
                parse_raydium_base_token_program(params.base_token_program.as_deref())?;

            let curve = match params.curve {
                RaydiumLaunchpadCurveBuildParams::Constant {
                    supply,
                    total_base_sell,
                    total_quote_fund_raising,
                    migrate_type,
                } => RaydiumLaunchpadCurveParams::Constant {
                    supply,
                    total_base_sell,
                    total_quote_fund_raising,
                    migrate_type,
                },
                RaydiumLaunchpadCurveBuildParams::Fixed {
                    supply,
                    total_quote_fund_raising,
                    migrate_type,
                } => RaydiumLaunchpadCurveParams::Fixed {
                    supply,
                    total_quote_fund_raising,
                    migrate_type,
                },
                RaydiumLaunchpadCurveBuildParams::Linear {
                    supply,
                    total_quote_fund_raising,
                    migrate_type,
                } => RaydiumLaunchpadCurveParams::Linear {
                    supply,
                    total_quote_fund_raising,
                    migrate_type,
                },
            };

            if matches!(
                base_token_program,
                RaydiumLaunchpadBaseTokenProgram::Token2022
            ) && curve.migrate_type() != 1
            {
                return Err(ApiError::bad_request(
                    "raydium_launchpad curve.migrate_type must be 1 when base_token_program=spl_token_2022",
                ));
            }

            let vesting = RaydiumLaunchpadVestingParams {
                total_locked_amount: params.vesting.total_locked_amount,
                cliff_period: params.vesting.cliff_period,
                unlock_period: params.vesting.unlock_period,
            };

            let amm_fee_on = match params.amm_fee_on {
                RaydiumLaunchpadAmmFeeOnBuildParam::QuoteToken => {
                    RaydiumLaunchpadAmmFeeOn::QuoteToken
                }
                RaydiumLaunchpadAmmFeeOnBuildParam::BothToken => {
                    RaydiumLaunchpadAmmFeeOn::BothToken
                }
            };

            let transfer_fee_extension =
                params
                    .transfer_fee_extension
                    .map(|v| RaydiumLaunchpadTransferFeeExtensionParams {
                        transfer_fee_basis_points: v.transfer_fee_basis_points,
                        maximum_fee: v.maximum_fee,
                    });

            if matches!(base_token_program, RaydiumLaunchpadBaseTokenProgram::Token)
                && transfer_fee_extension.is_some()
            {
                return Err(ApiError::bad_request(
                    "raydium_launchpad.transfer_fee_extension is only supported when base_token_program=spl_token_2022",
                ));
            }
            if let Some(params) = transfer_fee_extension.as_ref() {
                if params.transfer_fee_basis_points > 500 {
                    return Err(ApiError::bad_request(
                        "raydium_launchpad.transfer_fee_extension.transfer_fee_basis_points must be <= 500",
                    ));
                }
                let supply = match &curve {
                    RaydiumLaunchpadCurveParams::Constant { supply, .. }
                    | RaydiumLaunchpadCurveParams::Fixed { supply, .. }
                    | RaydiumLaunchpadCurveParams::Linear { supply, .. } => *supply,
                };
                let min_max_fee =
                    ((supply as u128) * (params.transfer_fee_basis_points as u128) / 10_000) as u64;
                if params.maximum_fee <= min_max_fee {
                    return Err(ApiError::bad_request(format!(
                        "raydium_launchpad.transfer_fee_extension.maximum_fee must be > supply*bps/10000 (min={min_max_fee})",
                    )));
                }
            }

            let global_acc = sol
                .get_account_with_commitment_resilient(
                    &global_config,
                    CommitmentConfig::processed(),
                )
                .await
                .map_err(|e| {
                    ApiError::internal(format!("rpc getAccount(global_config) failed: {e}"))
                })?;
            if global_acc.owner != raydium_launchpad_program_id {
                return Err(ApiError::bad_request(format!(
                    "raydium launchpad global_config owner mismatch: {}",
                    global_acc.owner
                )));
            }
            let global_state = RaydiumLaunchpad::decode_global_config_account_data(
                &global_acc.data,
            )
            .map_err(|e| {
                ApiError::internal(format!(
                    "raydium launchpad global_config decode failed: {e:#}"
                ))
            })?;

            let platform_acc = sol
                .get_account_with_commitment_resilient(
                    &platform_config,
                    CommitmentConfig::processed(),
                )
                .await
                .map_err(|e| {
                    ApiError::internal(format!("rpc getAccount(platform_config) failed: {e}"))
                })?;
            if platform_acc.owner != raydium_launchpad_program_id {
                return Err(ApiError::bad_request(format!(
                    "raydium launchpad platform_config owner mismatch: {}",
                    platform_acc.owner
                )));
            }

            validate_raydium_curve_type(global_state.curve_type, &curve)?;
            validate_raydium_launchpad_global_constraints(
                &global_state,
                &curve,
                &vesting,
                decimals,
            )?;

            let create_plan = RaydiumLaunchpadCreatePlan {
                launchpad_program_id: raydium_launchpad_program_id,
                payer,
                creator: payer,
                global_config,
                platform_config,
                base_mint: mint,
                quote_mint: global_state.quote_mint,
                name,
                symbol,
                uri,
                decimals,
                curve,
                vesting,
                amm_fee_on,
                base_token_program,
                transfer_fee_extension,
            };

            if let Some(auto_buy) = request.auto_buy.take() {
                if !simulate {
                    return Err(ApiError::bad_request(
                        "auto_buy requires simulate=true for method=raydium_launchpad",
                    ));
                }
                if create_plan.quote_mint != WSOL_MINT {
                    return Err(ApiError::bad_request(format!(
                        "auto_buy is only supported when quote_mint=WSOL (got {})",
                        create_plan.quote_mint
                    )));
                }

                if !auto_buy.buy_sol.is_finite() || auto_buy.buy_sol <= 0.0 {
                    return Err(ApiError::bad_request("auto_buy.buy_sol must be > 0"));
                }
                if !auto_buy.slippage_pct.is_finite() || auto_buy.slippage_pct < 0.0 {
                    return Err(ApiError::bad_request("auto_buy.slippage_pct must be >= 0"));
                }

                let slippage_pct = if auto_buy.slippage_pct <= 1.0 {
                    auto_buy.slippage_pct * 100.0
                } else {
                    auto_buy.slippage_pct
                };
                if slippage_pct > 99.0 {
                    return Err(ApiError::bad_request("auto_buy.slippage_pct must be <= 99"));
                }
                let slippage_bps = (slippage_pct * 100.0).round() as u64;

                let buy_sol_lamports = (auto_buy.buy_sol * 1_000_000_000.0).ceil() as u64;
                if buy_sol_lamports == 0 {
                    return Err(ApiError::bad_request("auto_buy.buy_sol must be > 0"));
                }

                let provisional = plan_raydium_launchpad_create_and_buy_exact_sol_in(
                    create_plan.clone(),
                    RaydiumLaunchpadAutoBuyExactSolInPlan {
                        amount_in_quote_lamports: buy_sol_lamports,
                        min_base_amount_out: 1,
                        share_fee_rate: 0,
                    },
                    compute_budget,
                )?;
                let provisional_tx = compile_unsigned_v0_transaction(
                    &provisional.payer,
                    &provisional.instructions,
                    Hash::default(),
                )?;
                let sim = sol
                    .simulate_transaction_with_config_resilient(
                        &provisional_tx,
                        RpcSimulateTransactionConfig {
                            sig_verify: false,
                            replace_recent_blockhash: true,
                            commitment: Some(CommitmentConfig::processed()),
                            ..RpcSimulateTransactionConfig::default()
                        },
                    )
                    .await
                    .map_err(|e| ApiError::internal(format!("rpc simulation failed: {e}")))?;
                if let Some(err) = sim.value.err {
                    return Err(ApiError::bad_request(format!(
                        "raydium launchpad auto_buy simulation failed: {err:?}"
                    )));
                }
                let logs = sim.value.logs.unwrap_or_default();
                let amount_out = RaydiumLaunchpad::parse_logs(logs.iter(), None)
                    .into_iter()
                    .find_map(|event| match event {
                        crate::dex::raydium_launchpad::RaydiumLaunchpadEvent::Trade(Some(
                            trade,
                        )) => Some(trade.amount_out),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        ApiError::internal(
                            "raydium launchpad auto_buy simulation missing trade event",
                        )
                    })?;
                if amount_out == 0 {
                    return Err(ApiError::bad_request(
                        "auto_buy.buy_sol too small for any token amount",
                    ));
                }

                let scale = 10_000u128;
                let min_out = ((amount_out as u128)
                    .saturating_mul(scale.saturating_sub(slippage_bps as u128)))
                    / scale;
                let min_out = u64::try_from(min_out).unwrap_or(1).max(1);

                (
                    plan_raydium_launchpad_create_and_buy_exact_sol_in(
                        create_plan,
                        RaydiumLaunchpadAutoBuyExactSolInPlan {
                            amount_in_quote_lamports: buy_sol_lamports,
                            min_base_amount_out: min_out,
                            share_fee_rate: 0,
                        },
                        compute_budget,
                    )?,
                    match base_token_program {
                        RaydiumLaunchpadBaseTokenProgram::Token => TOKEN_PROGRAM_ID,
                        RaydiumLaunchpadBaseTokenProgram::Token2022 => TOKEN_2022_PROGRAM_ID,
                    },
                )
            } else {
                (
                    plan_raydium_launchpad_create(create_plan, compute_budget)?,
                    match base_token_program {
                        RaydiumLaunchpadBaseTokenProgram::Token => TOKEN_PROGRAM_ID,
                        RaydiumLaunchpadBaseTokenProgram::Token2022 => TOKEN_2022_PROGRAM_ID,
                    },
                )
            }
        }
    };

    if let Some(level) = priority_fee_level {
        let price = sol
            .fetch_priority_fee(&level, &planned.priority_fee_addresses)
            .await
            .map_err(ApiError::from)?;
        planned
            .instructions
            .insert(0, ix_set_compute_unit_price(price));
    }

    if use_swqos && cluster_for_programs != SolanaCluster::MainnetBeta {
        return Err(ApiError::bad_request(SWQOS_MAINNET_ONLY_ERROR));
    }

    if use_swqos {
        let settings = request.swqos_settings.as_ref().ok_or_else(|| {
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
        Some(CreateSimulationResponse {
            ok: sim.value.err.is_none(),
            err: sim.value.err.map(|e| format!("{e:?}")),
            units_consumed: sim.value.units_consumed,
            logs: sim.value.logs.unwrap_or_default(),
        })
    } else {
        None
    };

    Ok((
        CreateBuildResponse {
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
            mint_token_program: mint_token_program.to_string(),
            simulation,
        },
        rpc,
        cluster_for_programs,
    ))
}

pub(super) async fn post_build(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateBuildRequest>,
) -> Result<Json<CreateBuildResponse>, ApiError> {
    let (response, _, _) = build_create_response(state.as_ref(), request).await?;
    Ok(Json(response))
}

pub(super) async fn post_execute(
    State(state): State<Arc<ApiState>>,
    Json(mut request): Json<CreateBuildRequest>,
) -> Result<Json<CreateExecuteResponse>, ApiError> {
    if !state.allow_live_sends {
        return Err(ApiError::conflict(
            "live sends are disabled (set MAMBA_API_ENABLE_LIVE_SENDS=true to unlock)",
        ));
    }

    let mut generated_signers = BTreeMap::new();
    let mut generated_signer_map = HashMap::new();
    if request
        .mint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        let mint_signer = Arc::new(Keypair::new());
        let mint_pubkey = mint_signer.pubkey().to_string();
        request.mint = Some(mint_pubkey.clone());
        generated_signers.insert("mint".to_string(), mint_pubkey.clone());
        generated_signer_map.insert(mint_signer.pubkey(), mint_signer);
    }

    let use_swqos = request.use_swqos.unwrap_or(false);
    let swqos_settings = request
        .swqos_settings
        .as_ref()
        .map(|settings| SWQoSettings {
            provider: settings.provider,
            tip_lamports: settings.tip_lamports,
            jito_key: None,
            nextblock_key: String::new(),
            zero_slot_key: String::new(),
            temporal_key: String::new(),
            blox_key: String::new(),
            nonce_account: None,
        });

    let rpc_url_override = request.rpc_url.clone();
    let (build, rpc, cluster) = build_create_response(state.as_ref(), request).await?;
    enforce_live_send_cluster_match(state.as_ref(), rpc_url_override.as_deref(), cluster)?;

    if let Some(simulation) = build.simulation.as_ref()
        && !simulation.ok
    {
        return Ok(Json(CreateExecuteResponse {
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

    let required_signers = resolve_required_signers(
        state.as_ref(),
        &build.required_signers,
        &generated_signer_map,
    )?;
    let unsigned = decode_versioned_transaction_base64(&build.transaction)?;
    let signed = sign_versioned_transaction(&unsigned, &required_signers)?;

    match submit_signed_transaction(rpc, cluster, &signed, use_swqos, swqos_settings).await {
        Ok(signature) => Ok(Json(CreateExecuteResponse {
            submitted: true,
            success: true,
            signature: Some(signature.to_string()),
            error: None,
            cluster: format!("{cluster:?}"),
            generated_signers,
            build,
        })),
        Err(error) => Ok(Json(CreateExecuteResponse {
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

fn parse_raydium_base_token_program(
    raw: Option<&str>,
) -> Result<RaydiumLaunchpadBaseTokenProgram, ApiError> {
    match raw.map(|v| v.trim().to_ascii_lowercase()) {
        None => Ok(RaydiumLaunchpadBaseTokenProgram::Token),
        Some(value)
            if value == "spl_token"
                || value == "token"
                || value == "tokenkeg"
                || value == "token_program" =>
        {
            Ok(RaydiumLaunchpadBaseTokenProgram::Token)
        }
        Some(value)
            if value == "spl_token_2022"
                || value == "token_2022"
                || value == "token2022"
                || value == "token_2022_program" =>
        {
            Ok(RaydiumLaunchpadBaseTokenProgram::Token2022)
        }
        Some(other) => Err(ApiError::bad_request(format!(
            "invalid raydium_launchpad.base_token_program: {other}"
        ))),
    }
}

fn validate_raydium_curve_type(
    curve_type: u8,
    curve: &RaydiumLaunchpadCurveParams,
) -> Result<(), ApiError> {
    let ok = matches!(
        (curve_type, curve),
        (0, RaydiumLaunchpadCurveParams::Constant { .. })
            | (1, RaydiumLaunchpadCurveParams::Fixed { .. })
            | (2, RaydiumLaunchpadCurveParams::Linear { .. })
    );
    if ok {
        return Ok(());
    }
    Err(ApiError::bad_request(format!(
        "raydium launchpad curve mismatch: global curve_type={curve_type} does not match provided curve variant"
    )))
}

fn validate_raydium_launchpad_global_constraints(
    global: &RaydiumLaunchpadGlobalConfigState,
    curve: &RaydiumLaunchpadCurveParams,
    vesting: &RaydiumLaunchpadVestingParams,
    decimals: u8,
) -> Result<(), ApiError> {
    let raw_supply = match curve {
        RaydiumLaunchpadCurveParams::Constant { supply, .. }
        | RaydiumLaunchpadCurveParams::Fixed { supply, .. }
        | RaydiumLaunchpadCurveParams::Linear { supply, .. } => *supply as u128,
    };
    let scale = 10u128.checked_pow(decimals.into()).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unsupported decimals for raydium_launchpad: {decimals}"
        ))
    })?;
    let min_raw_supply = (global.min_base_supply as u128)
        .checked_mul(scale)
        .ok_or_else(|| ApiError::bad_request("raydium launchpad min_base_supply overflow"))?;
    if raw_supply < min_raw_supply {
        return Err(ApiError::bad_request(format!(
            "raydium launchpad supply is too small for decimals={decimals}: raw supply {raw_supply} < min raw supply {min_raw_supply} (global min_base_supply={} before decimals)",
            global.min_base_supply,
        )));
    }

    let locked = vesting.total_locked_amount as u128;
    let max_locked = raw_supply.saturating_mul(global.max_lock_rate as u128) / 1_000_000u128;
    if locked > max_locked {
        return Err(ApiError::bad_request(format!(
            "raydium launchpad total_locked_amount {locked} exceeds max allowed {max_locked} for supply {raw_supply} and max_lock_rate={}",
            global.max_lock_rate,
        )));
    }

    let quote_fund_raising = match curve {
        RaydiumLaunchpadCurveParams::Constant {
            total_quote_fund_raising,
            ..
        }
        | RaydiumLaunchpadCurveParams::Fixed {
            total_quote_fund_raising,
            ..
        }
        | RaydiumLaunchpadCurveParams::Linear {
            total_quote_fund_raising,
            ..
        } => *total_quote_fund_raising,
    };
    if quote_fund_raising < global.min_quote_fund_raising {
        return Err(ApiError::bad_request(format!(
            "raydium launchpad total_quote_fund_raising {quote_fund_raising} is below global minimum {}",
            global.min_quote_fund_raising,
        )));
    }

    if let RaydiumLaunchpadCurveParams::Constant {
        total_base_sell, ..
    } = curve
    {
        let total_base_sell = *total_base_sell as u128;
        let min_base_sell =
            raw_supply.saturating_mul(global.min_base_sell_rate as u128) / 1_000_000u128;
        if total_base_sell < min_base_sell {
            return Err(ApiError::bad_request(format!(
                "raydium launchpad total_base_sell {total_base_sell} is below minimum {min_base_sell} for supply {raw_supply} and min_base_sell_rate={}",
                global.min_base_sell_rate,
            )));
        }

        let locked_and_sold = total_base_sell.saturating_add(locked);
        if locked_and_sold > raw_supply {
            return Err(ApiError::bad_request(format!(
                "raydium launchpad total_base_sell + total_locked_amount exceeds supply: {} + {} > {}",
                total_base_sell, locked, raw_supply,
            )));
        }

        let migrate_amount = raw_supply.saturating_sub(locked_and_sold);
        let min_migrate_amount =
            raw_supply.saturating_mul(global.min_base_migrate_rate as u128) / 1_000_000u128;
        if migrate_amount < min_migrate_amount {
            return Err(ApiError::bad_request(format!(
                "raydium launchpad migrate amount {migrate_amount} is below minimum {min_migrate_amount} for supply {raw_supply} and min_base_migrate_rate={}",
                global.min_base_migrate_rate,
            )));
        }
    }

    Ok(())
}

fn validate_token_fields(name: &str, symbol: &str, uri: &str) -> Result<(), ApiError> {
    if name.is_empty() {
        return Err(ApiError::bad_request("name is required"));
    }
    if symbol.is_empty() {
        return Err(ApiError::bad_request("symbol is required"));
    }
    if uri.is_empty() {
        return Err(ApiError::bad_request("uri is required"));
    }

    if name.len() > MAX_NAME_LEN {
        return Err(ApiError::bad_request(format!(
            "name must be <= {MAX_NAME_LEN} characters"
        )));
    }
    if symbol.len() > MAX_SYMBOL_LEN {
        return Err(ApiError::bad_request(format!(
            "symbol must be <= {MAX_SYMBOL_LEN} characters"
        )));
    }
    if uri.len() > MAX_URI_LEN {
        return Err(ApiError::bad_request(format!(
            "uri must be <= {MAX_URI_LEN} characters"
        )));
    }

    Ok(())
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
    use super::{get_methods, validate_raydium_launchpad_global_constraints};
    use crate::core::create::{RaydiumLaunchpadCurveParams, RaydiumLaunchpadVestingParams};
    use crate::core::sol::WSOL_MINT;
    use crate::dex::raydium_launchpad::RaydiumLaunchpadGlobalConfigState;

    #[tokio::test]
    async fn test_create_methods_advertise_execute_generated_mint() {
        let methods = get_methods().await.expect("create methods should build").0;
        assert!(
            methods
                .iter()
                .all(|method| method.execute_generated_fields == vec!["mint"]),
            "every create method should advertise execute-time mint generation"
        );
    }

    #[test]
    fn test_raydium_launchpad_constraints_reject_supply_too_small_for_decimals() {
        let global = RaydiumLaunchpadGlobalConfigState {
            curve_type: 0,
            trade_fee_rate: 2_500,
            max_share_fee_rate: 10_000,
            min_base_supply: 10_000_000,
            max_lock_rate: 999_999,
            min_base_sell_rate: 1,
            min_base_migrate_rate: 1,
            min_quote_fund_raising: 1,
            quote_mint: WSOL_MINT,
        };
        let curve = RaydiumLaunchpadCurveParams::Constant {
            supply: 69_000_000_000_000,
            total_base_sell: 54_723_900_000_000,
            total_quote_fund_raising: 500_000_000,
            migrate_type: 1,
        };
        let vesting = RaydiumLaunchpadVestingParams {
            total_locked_amount: 0,
            cliff_period: 0,
            unlock_period: 0,
        };

        let err = validate_raydium_launchpad_global_constraints(&global, &curve, &vesting, 9)
            .expect_err("decimals=9 should violate min base supply");
        assert!(
            err.message.contains("supply is too small for decimals=9"),
            "unexpected error: {:?}",
            err,
        );

        validate_raydium_launchpad_global_constraints(&global, &curve, &vesting, 6)
            .expect("decimals=6 should satisfy the same global min supply");
    }
}
