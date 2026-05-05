use {
    super::{
        ApiError, ApiState, decode_versioned_transaction_base64, enforce_live_send_cluster_match,
        resolve_local_signer, resolve_request_rpc, resolve_required_signers,
        rpc_client_for_attempt, sign_versioned_transaction, submit_signed_transaction,
    },
    crate::core::{
        sol::SolHook,
        wallet::{
            ManagedWalletRuntime, ManagedWalletStore, ManagedWalletSummary, WalletCleanBuild,
            WalletCleanBuildParams, WalletCleanPreview, WalletCleanSimulation,
            WalletTransferAssetKind, WalletTransferBuildParams, WalletTransferBuildResponse,
            WalletTransferSimulation,
        },
    },
    axum::{
        Json,
        extract::{Query, State},
    },
    serde::{Deserialize, Serialize},
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_client::rpc_config::RpcSimulateTransactionConfig,
    solana_commitment_config::CommitmentConfig,
    solana_program::pubkey::Pubkey,
    std::{
        collections::HashMap,
        str::FromStr,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
};

#[derive(Debug, Deserialize)]
pub(super) struct WalletCreateRequest {
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WalletSelectRequest {
    active_wallet: Option<String>,
    selected_wallets: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WalletTransferAssetKindRequest {
    Sol,
    Token,
}

#[derive(Debug, Deserialize)]
pub(super) struct WalletTransferBuildRequest {
    from_wallet: String,
    to_wallet: Option<String>,
    to_address: Option<String>,
    amount: String,
    asset_kind: WalletTransferAssetKindRequest,
    mint: Option<String>,
    simulate: Option<bool>,
    rpc_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WalletCleanPreviewQuery {
    owner: String,
    rpc_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WalletRpcUrlQuery {
    rpc_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WalletCleanBuildRequest {
    owner: String,
    token_accounts: Option<Vec<String>>,
    burn_nonzero: Option<bool>,
    close_empty: Option<bool>,
    close_wsol: Option<bool>,
    simulate: Option<bool>,
    rpc_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct WalletTransferExecuteResponse {
    submitted: bool,
    success: bool,
    signature: Option<String>,
    error: Option<String>,
    cluster: String,
    build: WalletTransferBuildResponse,
}

#[derive(Debug, Serialize)]
pub(super) struct WalletCleanBatchExecutionResponse {
    batch_index: usize,
    submitted: bool,
    success: bool,
    signature: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct WalletCleanExecuteResponse {
    submitted: bool,
    success: bool,
    cluster: String,
    error: Option<String>,
    build: WalletCleanBuild,
    batches: Vec<WalletCleanBatchExecutionResponse>,
}

#[derive(Debug, Serialize)]
pub(super) struct WalletBalanceResponse {
    pubkey: String,
    label: Option<String>,
    managed: bool,
    active: bool,
    selected: bool,
    balance_lamports: u64,
    balance_sol: f64,
    cluster: String,
    timestamp_unix_ms: u128,
}

pub(super) async fn get_wallets(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<ManagedWalletSummary>>, ApiError> {
    let runtime = load_runtime(state.as_ref())?;
    Ok(Json(runtime.public_wallets()))
}

pub(super) async fn get_active_wallet(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WalletRpcUrlQuery>,
) -> Result<Json<WalletBalanceResponse>, ApiError> {
    let runtime = load_runtime(state.as_ref())?;
    let active = runtime
        .active_pubkey()
        .ok_or_else(|| ApiError::not_found("no active managed wallet configured"))?;
    let (rpc, cluster) = rpc_for_request(state.as_ref(), query.rpc_url.as_deref()).await?;
    let response = build_wallet_balance_response(Some(&runtime), active, rpc, cluster).await?;
    Ok(Json(response))
}

pub(super) async fn get_wallet_balance(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(wallet): axum::extract::Path<String>,
    Query(query): Query<WalletRpcUrlQuery>,
) -> Result<Json<WalletBalanceResponse>, ApiError> {
    let wallet = parse_wallet_pubkey(&wallet)?;
    let runtime = load_runtime_optional(state.as_ref())?;
    let (rpc, cluster) = rpc_for_request(state.as_ref(), query.rpc_url.as_deref()).await?;
    let response = build_wallet_balance_response(runtime.as_ref(), wallet, rpc, cluster).await?;
    Ok(Json(response))
}

pub(super) async fn post_create_wallet(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WalletCreateRequest>,
) -> Result<Json<ManagedWalletSummary>, ApiError> {
    let path = wallet_store_path(state.as_ref())?;
    let mut store = ManagedWalletStore::load_or_default(path)?;
    let created = store.create_generated_wallet(request.label.as_deref())?;
    store.save(path)?;
    Ok(Json(created))
}

pub(super) async fn post_select_wallets(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WalletSelectRequest>,
) -> Result<Json<Vec<ManagedWalletSummary>>, ApiError> {
    let path = wallet_store_path(state.as_ref())?;
    let mut store = ManagedWalletStore::load_or_default(path)?;

    match (&request.selected_wallets, &request.active_wallet) {
        (Some(selected), active) => {
            let parsed_selected = parse_wallet_pubkeys(selected)?;
            let parsed_active = active.as_deref().map(parse_wallet_pubkey).transpose()?;
            store.set_selected_wallets(&parsed_selected, parsed_active)?;
        }
        (None, Some(active)) => {
            let parsed_active = parse_wallet_pubkey(active)?;
            store.set_active_wallet(&parsed_active)?;
        }
        (None, None) => {
            return Err(ApiError::bad_request(
                "active_wallet and/or selected_wallets is required",
            ));
        }
    }

    store.save(path)?;
    Ok(Json(store.public_wallets()))
}

async fn build_transfer_response(
    state: &ApiState,
    request: WalletTransferBuildRequest,
) -> Result<
    (
        WalletTransferBuildResponse,
        Arc<RpcClient>,
        crate::core::cluster::SolanaCluster,
    ),
    ApiError,
> {
    let from_wallet = parse_wallet_pubkey(&request.from_wallet)?;
    let to_wallet = request
        .to_address
        .as_deref()
        .or(request.to_wallet.as_deref())
        .ok_or_else(|| ApiError::bad_request("to_address or to_wallet is required"))
        .and_then(parse_wallet_pubkey)?;

    resolve_local_signer(state, &from_wallet)?;

    let mint = request
        .mint
        .as_deref()
        .map(parse_wallet_pubkey)
        .transpose()?;

    let (rpc, cluster) = rpc_for_request(state, request.rpc_url.as_deref()).await?;
    let sol = SolHook::from_rpc_client_with_cluster(rpc.clone(), cluster);
    let transfer_params = WalletTransferBuildParams {
        from: from_wallet,
        to: to_wallet,
        amount: request.amount,
        asset_kind: match request.asset_kind {
            WalletTransferAssetKindRequest::Sol => WalletTransferAssetKind::Sol,
            WalletTransferAssetKindRequest::Token => WalletTransferAssetKind::Token,
        },
        mint,
    };
    let mut response = sol
        .run_rpc_attempts("walletTransferBuild", |rpc| {
            let transfer_params = transfer_params.clone();
            async move {
                crate::core::wallet::build_wallet_transfer(rpc.as_ref(), &transfer_params).await
            }
        })
        .await
        .map_err(ApiError::from)?;

    if request.simulate.unwrap_or(true) {
        let tx = decode_versioned_transaction_base64(&response.transaction)?;
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
            .map_err(|error| ApiError::internal(format!("rpc simulation failed: {error}")))?;
        response.simulation = Some(WalletTransferSimulation {
            ok: sim.value.err.is_none(),
            err: sim.value.err.map(|value| format!("{value:?}")),
            units_consumed: sim.value.units_consumed,
            logs: sim.value.logs.unwrap_or_default(),
        });
    }

    Ok((response, rpc, cluster))
}

pub(super) async fn post_transfer_build(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WalletTransferBuildRequest>,
) -> Result<Json<WalletTransferBuildResponse>, ApiError> {
    let (response, _, _) = build_transfer_response(state.as_ref(), request).await?;
    Ok(Json(response))
}

pub(super) async fn post_transfer_execute(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WalletTransferBuildRequest>,
) -> Result<Json<WalletTransferExecuteResponse>, ApiError> {
    if !state.allow_live_sends {
        return Err(ApiError::conflict(
            "live sends are disabled (set MAMBA_API_ENABLE_LIVE_SENDS=true to unlock)",
        ));
    }

    let rpc_url_override = request.rpc_url.clone();
    let (build, rpc, cluster) = build_transfer_response(state.as_ref(), request).await?;
    enforce_live_send_cluster_match(state.as_ref(), rpc_url_override.as_deref(), cluster)?;
    if let Some(simulation) = build.simulation.as_ref()
        && !simulation.ok
    {
        return Ok(Json(WalletTransferExecuteResponse {
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
            build,
        }));
    }

    let signers =
        resolve_required_signers(state.as_ref(), &build.required_signers, &HashMap::new())?;
    let unsigned = decode_versioned_transaction_base64(&build.transaction)?;
    let signed = sign_versioned_transaction(&unsigned, &signers)?;
    match submit_signed_transaction(rpc, cluster, &signed, false, None).await {
        Ok(signature) => Ok(Json(WalletTransferExecuteResponse {
            submitted: true,
            success: true,
            signature: Some(signature.to_string()),
            error: None,
            cluster: format!("{cluster:?}"),
            build,
        })),
        Err(error) => Ok(Json(WalletTransferExecuteResponse {
            submitted: false,
            success: false,
            signature: None,
            error: Some(error.message),
            cluster: format!("{cluster:?}"),
            build,
        })),
    }
}

pub(super) async fn get_clean_preview(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WalletCleanPreviewQuery>,
) -> Result<Json<WalletCleanPreview>, ApiError> {
    let owner = parse_wallet_pubkey(&query.owner)?;
    let (rpc, cluster) = rpc_for_request(state.as_ref(), query.rpc_url.as_deref()).await?;
    let sol = SolHook::from_rpc_client_with_cluster(rpc, cluster);
    let preview = sol
        .run_rpc_attempts("walletCleanPreview", |rpc| async move {
            crate::core::wallet::preview_wallet_clean(rpc.as_ref(), owner).await
        })
        .await
        .map_err(ApiError::from)?;
    Ok(Json(preview))
}

async fn build_clean_response(
    state: &ApiState,
    request: WalletCleanBuildRequest,
) -> Result<
    (
        WalletCleanBuild,
        Arc<RpcClient>,
        crate::core::cluster::SolanaCluster,
    ),
    ApiError,
> {
    let owner = parse_wallet_pubkey(&request.owner)?;
    resolve_local_signer(state, &owner)?;

    let token_accounts = request
        .token_accounts
        .as_deref()
        .map(parse_wallet_pubkeys)
        .transpose()?
        .unwrap_or_default();

    let (rpc, cluster) = rpc_for_request(state, request.rpc_url.as_deref()).await?;
    let sol = SolHook::from_rpc_client_with_cluster(rpc.clone(), cluster);
    let clean_params = WalletCleanBuildParams {
        owner,
        token_accounts,
        burn_nonzero: request.burn_nonzero.unwrap_or(false),
        close_empty: request.close_empty.unwrap_or(true),
        close_wsol: request.close_wsol.unwrap_or(true),
    };
    let mut response = sol
        .run_rpc_attempts("walletCleanBuild", |rpc| {
            let clean_params = clean_params.clone();
            async move { crate::core::wallet::build_wallet_clean(rpc.as_ref(), &clean_params).await }
        })
        .await
        .map_err(ApiError::from)?;

    if request.simulate.unwrap_or(true) {
        for batch in response.batches.iter_mut() {
            let tx = decode_versioned_transaction_base64(&batch.transaction)?;
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
                .map_err(|error| ApiError::internal(format!("rpc simulation failed: {error}")))?;
            batch.simulation = Some(WalletCleanSimulation {
                ok: sim.value.err.is_none(),
                err: sim.value.err.map(|value| format!("{value:?}")),
                units_consumed: sim.value.units_consumed,
                logs: sim.value.logs.unwrap_or_default(),
            });
        }
    }

    Ok((response, rpc, cluster))
}

pub(super) async fn post_clean_build(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WalletCleanBuildRequest>,
) -> Result<Json<WalletCleanBuild>, ApiError> {
    let (response, _, _) = build_clean_response(state.as_ref(), request).await?;
    Ok(Json(response))
}

pub(super) async fn post_clean_execute(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WalletCleanBuildRequest>,
) -> Result<Json<WalletCleanExecuteResponse>, ApiError> {
    if !state.allow_live_sends {
        return Err(ApiError::conflict(
            "live sends are disabled (set MAMBA_API_ENABLE_LIVE_SENDS=true to unlock)",
        ));
    }

    let rpc_url_override = request.rpc_url.clone();
    let (build, rpc, cluster) = build_clean_response(state.as_ref(), request).await?;
    enforce_live_send_cluster_match(state.as_ref(), rpc_url_override.as_deref(), cluster)?;
    let owner = parse_wallet_pubkey(&build.owner)?;
    let owner_signer =
        resolve_required_signers(state.as_ref(), &[owner.to_string()], &HashMap::new())?
            .into_iter()
            .next()
            .ok_or_else(|| ApiError::bad_request("missing owner signer"))?;

    if let Some(failed_batch) = build.batches.iter().find(|batch| {
        batch
            .simulation
            .as_ref()
            .is_some_and(|simulation| !simulation.ok)
    }) {
        return Ok(Json(WalletCleanExecuteResponse {
            submitted: false,
            success: false,
            cluster: format!("{cluster:?}"),
            error: Some(format!(
                "wallet clean simulation failed for batch {}",
                failed_batch.batch_index
            )),
            build,
            batches: Vec::new(),
        }));
    }

    let mut batch_responses = Vec::with_capacity(build.batches.len());
    let mut overall_success = true;
    let mut overall_error = None;

    for batch in build.batches.iter() {
        let unsigned = decode_versioned_transaction_base64(&batch.transaction)?;
        let signed = sign_versioned_transaction(&unsigned, std::slice::from_ref(&owner_signer))?;
        match submit_signed_transaction(rpc.clone(), cluster, &signed, false, None).await {
            Ok(signature) => batch_responses.push(WalletCleanBatchExecutionResponse {
                batch_index: batch.batch_index,
                submitted: true,
                success: true,
                signature: Some(signature.to_string()),
                error: None,
            }),
            Err(error) => {
                overall_success = false;
                overall_error = Some(error.message.clone());
                batch_responses.push(WalletCleanBatchExecutionResponse {
                    batch_index: batch.batch_index,
                    submitted: false,
                    success: false,
                    signature: None,
                    error: Some(error.message),
                });
                break;
            }
        }
    }

    Ok(Json(WalletCleanExecuteResponse {
        submitted: !batch_responses.is_empty(),
        success: overall_success,
        cluster: format!("{cluster:?}"),
        error: overall_error,
        build,
        batches: batch_responses,
    }))
}

async fn build_wallet_balance_response(
    runtime: Option<&ManagedWalletRuntime>,
    wallet: Pubkey,
    rpc: Arc<RpcClient>,
    cluster: crate::core::cluster::SolanaCluster,
) -> Result<WalletBalanceResponse, ApiError> {
    let sol = SolHook::from_rpc_client_with_cluster(rpc, cluster);
    let summary = runtime.and_then(|runtime| {
        runtime
            .public_wallets()
            .into_iter()
            .find(|entry| entry.pubkey == wallet.to_string())
    });
    let balance_lamports = sol
        .get_balance_lamports(&wallet)
        .await
        .map_err(|error| ApiError::internal(format!("rpc getBalance failed: {error}")))?;
    let timestamp_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis();

    Ok(WalletBalanceResponse {
        pubkey: wallet.to_string(),
        label: summary.as_ref().map(|entry| entry.label.clone()),
        managed: summary.is_some(),
        active: summary.as_ref().is_some_and(|entry| entry.active),
        selected: summary.as_ref().is_some_and(|entry| entry.selected),
        balance_lamports,
        balance_sol: balance_lamports as f64 / 1_000_000_000.0,
        cluster: format!("{cluster:?}"),
        timestamp_unix_ms,
    })
}

fn load_runtime(state: &ApiState) -> Result<ManagedWalletRuntime, ApiError> {
    let path = wallet_store_path(state)?;
    ManagedWalletRuntime::load(path).map_err(ApiError::from)
}

fn load_runtime_optional(state: &ApiState) -> Result<Option<ManagedWalletRuntime>, ApiError> {
    let Some(path) = state.wallet_store_path.as_deref() else {
        return Ok(None);
    };
    ManagedWalletRuntime::load(path)
        .map(Some)
        .map_err(ApiError::from)
}

fn wallet_store_path(state: &ApiState) -> Result<&std::path::Path, ApiError> {
    state
        .wallet_store_path
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("wallet store path unavailable in this environment"))
}

fn parse_wallet_pubkey(raw: &str) -> Result<Pubkey, ApiError> {
    Pubkey::from_str(raw.trim())
        .map_err(|_| ApiError::bad_request(format!("invalid pubkey: {}", raw.trim())))
}

fn parse_wallet_pubkeys(raw: &[String]) -> Result<Vec<Pubkey>, ApiError> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    raw.iter()
        .map(|value| parse_wallet_pubkey(value))
        .collect::<Result<Vec<_>, _>>()
}

async fn rpc_for_request(
    state: &ApiState,
    rpc_url: Option<&str>,
) -> Result<(Arc<RpcClient>, crate::core::cluster::SolanaCluster), ApiError> {
    let base_offset = state
        .endpoint_cursor
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Some(url) = rpc_url.map(str::trim).filter(|value| !value.is_empty()) {
        return resolve_request_rpc(state, base_offset, Some(url)).await;
    }
    let rpc = rpc_client_for_attempt(state, base_offset, 0)
        .await
        .map_err(ApiError::from)?;
    Ok((rpc, state.cluster))
}
