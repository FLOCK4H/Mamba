use {
    anyhow::{Context, bail},
    reqwest::Method,
    rmcp::{
        Json, RoleServer, ServerHandler, ServiceExt,
        handler::server::{router::tool::ToolRouter, wrapper::Parameters},
        model::{
            AnnotateAble, ErrorData as McpError, ListResourcesResult, PaginatedRequestParams,
            RawResource, ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
            ServerCapabilities, ServerInfo,
        },
        schemars::JsonSchema,
        service::RequestContext,
        tool, tool_handler, tool_router,
    },
    serde::{Deserialize, Serialize},
    serde_json::{Map, Value, json},
    std::time::Duration,
    tokio::{
        io::{
            AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
            BufReader,
        },
        sync::watch,
    },
};

#[derive(Debug, Clone)]
pub struct MambaMcpConfig {
    api_base_url: String,
    api_key: String,
    timeout_secs: u64,
    auto_accept_low_lq_pools: bool,
}

impl MambaMcpConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenv::dotenv().ok();

        let api_base_url = std::env::var("MAMBA_MCP_API_URL")
            .ok()
            .or_else(|| std::env::var("MAMBA_API_BASE_URL").ok())
            .unwrap_or_else(|| "http://127.0.0.1:8787/mamba-api/v1".to_string());
        let api_key = std::env::var("MAMBA_MCP_API_KEY")
            .ok()
            .or_else(|| std::env::var("MAMBA_API_KEY").ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .context("missing MAMBA_MCP_API_KEY or MAMBA_API_KEY")?;
        let timeout_secs = std::env::var("MAMBA_MCP_TIMEOUT_SECS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .unwrap_or(30)
            .max(5);
        let auto_accept_low_lq_pools = env_truthy("AUTO_ACCEPT_LOW_LQ_POOLS");

        Ok(Self {
            api_base_url: normalize_api_base_url(&api_base_url)?,
            api_key,
            timeout_secs,
            auto_accept_low_lq_pools,
        })
    }
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[derive(Debug, Clone)]
struct MambaApiClient {
    http: reqwest::Client,
    api_base_url: String,
    api_key: String,
}

impl MambaApiClient {
    fn new(config: &MambaMcpConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .context("build MCP HTTP client")?;
        Ok(Self {
            http,
            api_base_url: config.api_base_url.clone(),
            api_key: config.api_key.clone(),
        })
    }

    async fn get(&self, path: &str, query: Option<Value>) -> anyhow::Result<Value> {
        self.request(Method::GET, path, query, None).await
    }

    async fn post(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        self.request(Method::POST, path, None, Some(body)).await
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: Option<Value>,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let normalized_path = normalize_api_path(path);
        let url = format!("{}{}", self.api_base_url, normalized_path);
        let mut request = self
            .http
            .request(method.clone(), &url)
            .header("x-api-key", &self.api_key);

        if let Some(query) = prune_nulls(query)
            && let Some(pairs) = query_pairs_from_value(&query)?
        {
            request = request.query(&pairs);
        }

        if let Some(body) = prune_nulls(body) {
            request = request.json(&body);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("request failed for {} {}", method, normalized_path))?;
        let status = response.status();
        let text = response.text().await.with_context(|| {
            format!(
                "read response body failed for {} {}",
                method, normalized_path
            )
        })?;
        if !status.is_success() {
            bail!(
                "{} {} returned {}: {}",
                method,
                normalized_path,
                status,
                text
            );
        }

        serde_json::from_str(&text).with_context(|| {
            format!(
                "decode JSON response failed for {} {}: {}",
                method, normalized_path, text
            )
        })
    }
}

#[derive(Debug, Clone)]
pub struct MambaMcpServer {
    client: MambaApiClient,
    auto_accept_low_lq_pools: bool,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MambaToolResponse {
    pub data: Value,
}

type ToolResult = Result<Json<MambaToolResponse>, String>;

const MCP_RESOURCE_HEALTH_URI: &str = "mamba://health";
const MCP_RESOURCE_MARKETS_URI: &str = "mamba://markets";
const MCP_RESOURCE_DOCS_URI: &str = "mamba://docs";
const MCP_RESOURCE_TOOL_PLAYBOOK_URI: &str = "mamba://tool-playbook";
const MCP_RESOURCE_ACTIVE_WALLET_URI: &str = "mamba://wallets/active";
const MCP_RESOURCE_SUBSCRIPTIONS_URI: &str = "mamba://ws/subscriptions";

impl MambaMcpServer {
    pub fn new(config: MambaMcpConfig) -> anyhow::Result<Self> {
        Ok(Self {
            client: MambaApiClient::new(&config)?,
            auto_accept_low_lq_pools: config.auto_accept_low_lq_pools,
            tool_router: Self::tool_router(),
        })
    }

    fn skip_low_lq_pools_for_execute(
        &self,
        execute: Option<bool>,
        requested: Option<bool>,
    ) -> Option<bool> {
        if execute.unwrap_or(false) && requested.is_none() && !self.auto_accept_low_lq_pools {
            Some(true)
        } else {
            requested
        }
    }

    async fn get_json(&self, path: &str, query: Option<Value>) -> ToolResult {
        self.client
            .get(path, query)
            .await
            .map(wrap_tool_response)
            .map_err(render_tool_error)
    }

    async fn post_json(&self, path: &str, body: Value) -> ToolResult {
        self.client
            .post(path, body)
            .await
            .map(wrap_tool_response)
            .map_err(render_tool_error)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MambaMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_instructions(mamba_server_instructions())
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListResourcesResult::with_all_items(
            mamba_static_resources(),
        )))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, McpError>> + Send + '_ {
        async move {
            if request.uri == MCP_RESOURCE_TOOL_PLAYBOOK_URI {
                return Ok(ReadResourceResult::new(vec![json_resource_contents(
                    MCP_RESOURCE_TOOL_PLAYBOOK_URI,
                    mamba_tool_playbook(),
                )?]));
            }
            let Some((resource_uri, path)) = mamba_resource_route(&request.uri) else {
                return Err(McpError::resource_not_found(
                    format!("unknown Mamba MCP resource URI: {}", request.uri),
                    None,
                ));
            };
            let payload = self
                .client
                .get(path, None)
                .await
                .map_err(|error| McpError::internal_error(format!("{error:#}"), None))?;
            Ok(ReadResourceResult::new(vec![json_resource_contents(
                resource_uri,
                payload,
            )?]))
        }
    }
}

#[tool_router]
impl MambaMcpServer {
    #[tool(
        description = "Read-only. Use when you need the raw authenticated Mamba API route catalog or exact HTTP endpoint names. Prefer dedicated MCP tools for normal tasks; use this for route discovery and debugging."
    )]
    async fn api_docs(&self) -> ToolResult {
        self.get_json("/docs", None).await
    }

    #[tool(
        description = "Read-only. Use first when you need runtime readiness: cluster, signer availability, active wallet pubkey, and websocket subscription state. Do not use this for balances or token details."
    )]
    async fn health(&self) -> ToolResult {
        self.get_json("/health", None).await
    }

    #[tool(
        description = "Read-only. Use to validate or discover canonical market labels before subscribe, swap, create-pool, or manage-pool calls."
    )]
    async fn list_supported_markets(&self) -> ToolResult {
        self.get_json("/markets", None).await
    }

    #[tool(
        description = "Read-only. Canonical tool for 'my wallet', 'current wallet', 'active wallet', or 'what is my SOL balance'. Prefer this over list_wallets when the user is asking about the current wallet state."
    )]
    async fn get_active_wallet(&self, Parameters(req): Parameters<RpcUrlRequest>) -> ToolResult {
        self.get_json("/wallets/active", Some(json!({ "rpc_url": req.rpc_url })))
            .await
    }

    #[tool(
        description = "Read-only. Canonical tool for live SOL balance of an arbitrary wallet pubkey. Prefer get_active_wallet for the current managed wallet; use this when the wallet is explicitly named or not active."
    )]
    async fn get_wallet_balance(
        &self,
        Parameters(req): Parameters<GetWalletBalanceRequest>,
    ) -> ToolResult {
        self.get_json(
            &format!("/wallets/{}/balance", req.wallet),
            Some(json!({ "rpc_url": req.rpc_url })),
        )
        .await
    }

    #[tool(
        description = "Read-only. Use for broad token discovery from Mamba's websocket cache, such as 'show recent pump.fun tokens' or 'find tokens matching X'. Prefer get_token_details for one known mint."
    )]
    async fn list_tokens(&self, Parameters(req): Parameters<ListTokensRequest>) -> ToolResult {
        self.get_json(
            "/mints",
            Some(json!({
                "market": req.market,
                "markets": join_csv(req.markets),
                "q": req.q,
                "min_liquidity": req.min_liquidity,
                "min_volume": req.min_volume,
                "limit": req.limit,
            })),
        )
        .await
    }

    #[tool(
        description = "Read-only. Canonical single-mint resolver. Use this when the user gives one mint and needs route, creator provenance, or metadata. Prefer this over list_tokens for known mints and over batch_get_token_metadata when route data matters."
    )]
    async fn get_token_details(
        &self,
        Parameters(req): Parameters<GetTokenDetailsRequest>,
    ) -> ToolResult {
        let mut out = json!({ "mint": req.mint });
        let route_query = Some(json!({
            "quote_mint": req.quote_mint,
            "market_priority": join_csv(req.market_priority),
            "min_liquidity_raw": req.min_liquidity_raw,
            "rpc_url": req.rpc_url,
        }));

        if req.include_route.unwrap_or(true) {
            out["route"] = self
                .client
                .get(&format!("/mints/{}/route", req.mint), route_query.clone())
                .await
                .map_err(render_tool_error)?;
        }
        if req.include_creator.unwrap_or(true) {
            out["creator"] = self
                .client
                .get(&format!("/mints/{}/creator", req.mint), route_query.clone())
                .await
                .map_err(render_tool_error)?;
        }
        if req.include_metadata.unwrap_or(true) {
            out["metadata"] = self
                .client
                .get(
                    &format!("/mints/{}/metadata", req.mint),
                    Some(json!({ "rpc_url": req.rpc_url })),
                )
                .await
                .map_err(render_tool_error)?;
        }

        Ok(wrap_tool_response(out))
    }

    #[tool(
        description = "Read-only. Use only when you need metadata for many mints at once. Prefer get_token_details for a single mint because it can also return route and creator data."
    )]
    async fn batch_get_token_metadata(
        &self,
        Parameters(req): Parameters<BatchGetTokenMetadataRequest>,
    ) -> ToolResult {
        self.post_json("/mints/metadata-batch", json!({ "mints": req.mints }))
            .await
    }

    #[tool(
        description = "Read-only. Use during Raydium Launchpad token-create planning to discover valid global_config values. Prefer this only for Raydium Launchpad create flows."
    )]
    async fn list_raydium_launchpad_global_configs(
        &self,
        Parameters(req): Parameters<RpcUrlRequest>,
    ) -> ToolResult {
        self.get_json(
            "/create/raydium_launchpad/global-configs",
            Some(json!({ "rpc_url": req.rpc_url })),
        )
        .await
    }

    #[tool(
        description = "Read-only. Use during Raydium Launchpad token-create planning to discover valid platform_config values and their metadata. Prefer this only for Raydium Launchpad create flows."
    )]
    async fn list_raydium_launchpad_platform_configs(
        &self,
        Parameters(req): Parameters<RpcUrlRequest>,
    ) -> ToolResult {
        self.get_json(
            "/create/raydium_launchpad/platform-configs",
            Some(json!({ "rpc_url": req.rpc_url })),
        )
        .await
    }

    #[tool(
        description = "Read-only. Use during Raydium Launchpad token-create planning after you already know a platform_config and need valid curve parameters. Prefer this only for Raydium Launchpad create flows."
    )]
    async fn list_raydium_launchpad_platform_curve_params(
        &self,
        Parameters(req): Parameters<RaydiumLaunchpadPlatformCurveParamsRequest>,
    ) -> ToolResult {
        self.get_json(
            &format!(
                "/create/raydium_launchpad/platform-configs/{}/curve-params",
                req.platform_config
            ),
            Some(json!({
                "global_config": req.global_config,
                "rpc_url": req.rpc_url,
            })),
        )
        .await
    }

    #[tool(
        description = "Build or execute a buy. Use for mint-first token purchases. Default to execute=false unless the user clearly wants submission. Prefer this over call_mamba_api for buy workflows."
    )]
    async fn buy_token(&self, Parameters(req): Parameters<BuyTokenRequest>) -> ToolResult {
        let skip_low_lq_pools =
            self.skip_low_lq_pools_for_execute(req.execute, req.skip_low_lq_pools);
        self.post_json(
            "/swap",
            json!({
                "side": "buy",
                "mint": req.mint,
                "market": req.market,
                "pool": req.pool,
                "creator": req.creator,
                "quote_mint": req.quote_mint,
                "market_priority": join_csv(req.market_priority),
                "min_liquidity_raw": req.min_liquidity_raw,
                "skip_low_lq_pools": skip_low_lq_pools,
                "buy_sol": req.buy_sol,
                "slippage_pct": req.slippage_pct,
                "use_idempotent": req.use_idempotent,
                "priority_fee_level": req.priority_fee_level,
                "priority_fee_sol": req.priority_fee_sol,
                "use_swqos": req.use_swqos,
                "swqos_settings": req.swqos_settings,
                "execute": req.execute,
                "wallet": req.wallet,
                "rpc_url": req.rpc_url,
            }),
        )
        .await
    }

    #[tool(
        description = "Build or execute a sell. Use for mint-first token exits. Default to execute=false unless the user clearly wants submission. Prefer this over call_mamba_api for sell workflows."
    )]
    async fn sell_token(&self, Parameters(req): Parameters<SellTokenRequest>) -> ToolResult {
        let skip_low_lq_pools =
            self.skip_low_lq_pools_for_execute(req.execute, req.skip_low_lq_pools);
        self.post_json(
            "/swap",
            json!({
                "side": "sell",
                "mint": req.mint,
                "market": req.market,
                "pool": req.pool,
                "creator": req.creator,
                "quote_mint": req.quote_mint,
                "market_priority": join_csv(req.market_priority),
                "min_liquidity_raw": req.min_liquidity_raw,
                "skip_low_lq_pools": skip_low_lq_pools,
                "sell_pct": req.sell_pct,
                "slippage_pct": req.slippage_pct,
                "retries": req.retries,
                "priority_fee_level": req.priority_fee_level,
                "priority_fee_sol": req.priority_fee_sol,
                "use_swqos": req.use_swqos,
                "swqos_settings": req.swqos_settings,
                "execute": req.execute,
                "wallet": req.wallet,
                "rpc_url": req.rpc_url,
            }),
        )
        .await
    }

    #[tool(
        description = "Read-only. Use to inspect the managed wallet registry, labels, and selected flags. Prefer get_active_wallet when the user wants the current wallet balance or status."
    )]
    async fn list_wallets(&self) -> ToolResult {
        self.get_json("/wallets", None).await
    }

    #[tool(
        description = "State-changing. Use only when the user explicitly wants a new managed wallet created inside Mamba. Returns public metadata only; private keys never leave Mamba."
    )]
    async fn create_wallet(&self, Parameters(req): Parameters<CreateWalletRequest>) -> ToolResult {
        self.post_json("/wallets", json!({ "label": req.label }))
            .await
    }

    #[tool(
        description = "State-changing. Canonical tool for switching the active managed wallet or updating the selected wallet set. Use this before buy, sell, transfer, create, or cleaner calls when wallet context must change."
    )]
    async fn select_wallets(
        &self,
        Parameters(req): Parameters<SelectWalletsRequest>,
    ) -> ToolResult {
        self.post_json(
            "/wallets/select",
            json!({
                "active_wallet": req.active_wallet,
                "selected_wallets": req.selected_wallets,
            }),
        )
        .await
    }

    #[tool(
        description = "Build or execute a SOL or SPL transfer. Use this for wallet-to-wallet or wallet-to-address sends, not swaps. Default to execute=false unless the user clearly wants submission."
    )]
    async fn transfer_asset(
        &self,
        Parameters(req): Parameters<TransferAssetRequest>,
    ) -> ToolResult {
        let path = if req.execute.unwrap_or(false) {
            "/wallets/transfer/execute"
        } else {
            "/wallets/transfer/build"
        };
        self.post_json(
            path,
            json!({
                "from_wallet": req.from_wallet,
                "to_address": req.to_address,
                "to_wallet": req.to_wallet,
                "amount": req.amount,
                "asset_kind": req.asset_kind,
                "mint": req.mint,
                "simulate": req.simulate,
                "rpc_url": req.rpc_url,
            }),
        )
        .await
    }

    #[tool(
        description = "Read-only. Use before wallet cleanup to inspect which token accounts can be closed, unwrapped, or burned-and-closed. Prefer this before clean_wallet when the user wants to review reclaimable SOL first."
    )]
    async fn preview_wallet_clean(
        &self,
        Parameters(req): Parameters<PreviewWalletCleanRequest>,
    ) -> ToolResult {
        self.get_json(
            "/wallets/clean/preview",
            Some(json!({
                "owner": req.owner,
                "rpc_url": req.rpc_url,
            })),
        )
        .await
    }

    #[tool(
        description = "Build or execute wallet cleanup. Use after preview_wallet_clean when the user wants the cleanup transaction plan or submission. Default to execute=false unless the user clearly wants submission."
    )]
    async fn clean_wallet(&self, Parameters(req): Parameters<CleanWalletRequest>) -> ToolResult {
        let path = if req.execute.unwrap_or(false) {
            "/wallets/clean/execute"
        } else {
            "/wallets/clean/build"
        };
        self.post_json(
            path,
            json!({
                "owner": req.owner,
                "token_accounts": req.token_accounts,
                "burn_nonzero": req.burn_nonzero,
                "close_empty": req.close_empty,
                "close_wsol": req.close_wsol,
                "simulate": req.simulate,
                "rpc_url": req.rpc_url,
            }),
        )
        .await
    }

    #[tool(
        description = "Read-only. Use before create_token to discover supported create methods and their required fields. Prefer this when the user wants to know what create flows are available."
    )]
    async fn list_create_methods(&self) -> ToolResult {
        self.get_json("/create/methods", None).await
    }

    #[tool(
        description = "Build or execute token creation. Use for pump_fun, spl_token, spl_token_2022, or raydium_launchpad create flows. Default to execute=false unless the user clearly wants submission. Use the Raydium Launchpad discovery tools first when method=raydium_launchpad."
    )]
    async fn create_token(&self, Parameters(req): Parameters<CreateTokenRequest>) -> ToolResult {
        let path = if req.execute.unwrap_or(false) {
            "/create/execute"
        } else {
            "/create/build"
        };

        let body = merge_object(
            json!({
                "method": req.method,
                "payer": req.payer,
                "mint": req.mint,
                "name": req.name,
                "symbol": req.symbol,
                "uri": req.uri,
                "decimals": req.decimals,
                "initial_supply": req.initial_supply,
                "freeze_authority": req.freeze_authority,
                "revoke_mint_authority": req.revoke_mint_authority,
                "revoke_freeze_authority": req.revoke_freeze_authority,
                "metadata_is_mutable": req.metadata_is_mutable,
                "simulate": req.simulate,
                "rpc_url": req.rpc_url,
                "priority_fee_level": req.priority_fee_level,
                "compute_unit_limit": req.compute_unit_limit,
                "use_swqos": req.use_swqos,
                "swqos_settings": req.swqos_settings,
                "auto_buy": req.auto_buy,
                "raydium_launchpad": req.raydium_launchpad,
            }),
            req.extra,
        )?;
        self.post_json(path, body).await
    }

    #[tool(
        description = "Read-only. Use before create_pool to discover supported pool-build markets and required fields."
    )]
    async fn list_pool_methods(&self) -> ToolResult {
        self.get_json("/pool/methods", None).await
    }

    #[tool(
        description = "Build or execute pool creation. Use when the user explicitly wants to create a liquidity pool, not for token creation or pool withdrawal. Default to execute=false unless the user clearly wants submission."
    )]
    async fn create_pool(&self, Parameters(req): Parameters<CreatePoolRequest>) -> ToolResult {
        let path = if req.execute.unwrap_or(false) {
            "/pool/execute"
        } else {
            "/pool/build"
        };
        self.post_json(path, req.request).await
    }

    #[tool(
        description = "Read-only. Use to inspect wallet-owned pool positions and withdraw support before manage_pool_position. Prefer this when the user asks about existing pools or LP positions."
    )]
    async fn list_pool_positions(
        &self,
        Parameters(req): Parameters<ListPoolPositionsRequest>,
    ) -> ToolResult {
        self.get_json(
            "/pool/positions",
            Some(json!({
                "owner": req.owner,
                "rpc_url": req.rpc_url,
                "include_unsupported": req.include_unsupported,
            })),
        )
        .await
    }

    #[tool(
        description = "Build or execute supported pool withdrawals or pool-position management. Use after list_pool_positions when the user wants to withdraw or manage an existing supported position. Default to execute=false unless the user clearly wants submission."
    )]
    async fn manage_pool_position(
        &self,
        Parameters(req): Parameters<ManagePoolPositionRequest>,
    ) -> ToolResult {
        let path = if req.execute.unwrap_or(false) {
            "/pool/manage/execute"
        } else {
            "/pool/manage/build"
        };
        self.post_json(
            path,
            json!({
                "market": req.market,
                "owner": req.owner,
                "pool": req.pool,
                "withdraw_pct": req.withdraw_pct,
                "slippage_pct": req.slippage_pct,
                "simulate": req.simulate,
                "rpc_url": req.rpc_url,
                "priority_fee_level": req.priority_fee_level,
                "compute_unit_limit": req.compute_unit_limit,
            }),
        )
        .await
    }

    #[tool(
        description = "Read-only. Use for creator-level discovery, rankings, or score-based filtering. Prefer list_creator_mints when you already know the creator and need the mints."
    )]
    async fn list_creators(&self, Parameters(req): Parameters<ListCreatorsRequest>) -> ToolResult {
        self.get_json(
            "/creators",
            Some(json!({
                "min_mint_count": req.min_mint_count,
                "min_avg_market_cap": req.min_avg_market_cap,
                "min_score": req.min_score,
                "limit": req.limit,
                "offset": req.offset,
            })),
        )
        .await
    }

    #[tool(
        description = "Read-only. Use when you know a creator pubkey and need that creator's mints. Prefer list_creators for creator discovery and get_token_details for one mint."
    )]
    async fn list_creator_mints(
        &self,
        Parameters(req): Parameters<ListCreatorMintsRequest>,
    ) -> ToolResult {
        self.get_json(
            "/creator-mints",
            Some(json!({
                "creator": req.creator,
                "market": req.market,
                "limit": req.limit,
                "offset": req.offset,
            })),
        )
        .await
    }

    #[tool(
        description = "Read-only. Use for transaction history or observed transaction rows, optionally filtered by creator or market. Do not use this for current balances or route resolution."
    )]
    async fn list_transactions(
        &self,
        Parameters(req): Parameters<ListTransactionsRequest>,
    ) -> ToolResult {
        self.get_json(
            "/transactions",
            Some(json!({
                "creator": req.creator,
                "market": req.market,
                "limit": req.limit,
                "offset": req.offset,
            })),
        )
        .await
    }

    #[tool(
        description = "State-changing. Use to start a websocket-backed market subscription so token cache data can populate. Prefer this before list_tokens when the cache for a market may be empty."
    )]
    async fn subscribe_market(&self, Parameters(req): Parameters<MarketRequest>) -> ToolResult {
        self.post_json("/ws/subscribe", json!({ "market": req.market }))
            .await
    }

    #[tool(
        description = "State-changing. Use to stop an existing websocket-backed market subscription when the user explicitly wants it disabled."
    )]
    async fn unsubscribe_market(&self, Parameters(req): Parameters<MarketRequest>) -> ToolResult {
        self.post_json("/ws/unsubscribe", json!({ "market": req.market }))
            .await
    }

    #[tool(
        description = "Read-only. Use to confirm which websocket market subscriptions are active when you only need current subscription state."
    )]
    async fn list_subscriptions(&self) -> ToolResult {
        self.get_json("/ws/subscriptions", None).await
    }

    #[tool(
        description = "Fallback escape hatch. Use only when no dedicated MCP tool exists for the needed authenticated HTTP route. Prefer dedicated tools first because they encode intent and safer defaults more clearly."
    )]
    async fn call_mamba_api(&self, Parameters(req): Parameters<CallMambaApiRequest>) -> ToolResult {
        let method = Method::from_bytes(req.method.trim().to_ascii_uppercase().as_bytes())
            .map_err(|error| format!("invalid HTTP method: {error}"))?;
        self.client
            .request(method, &req.path, req.query, req.body)
            .await
            .map(wrap_tool_response)
            .map_err(render_tool_error)
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListTokensRequest {
    pub market: Option<String>,
    pub markets: Option<Vec<String>>,
    pub q: Option<String>,
    pub min_liquidity: Option<f64>,
    pub min_volume: Option<f64>,
    pub limit: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RpcUrlRequest {
    pub rpc_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetWalletBalanceRequest {
    pub wallet: String,
    pub rpc_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetTokenDetailsRequest {
    pub mint: String,
    pub include_route: Option<bool>,
    pub include_metadata: Option<bool>,
    pub include_creator: Option<bool>,
    pub quote_mint: Option<String>,
    pub market_priority: Option<Vec<String>>,
    pub min_liquidity_raw: Option<u64>,
    pub rpc_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BatchGetTokenMetadataRequest {
    pub mints: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RaydiumLaunchpadPlatformCurveParamsRequest {
    pub platform_config: String,
    pub global_config: Option<String>,
    pub rpc_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuyTokenRequest {
    pub mint: String,
    pub buy_sol: f64,
    pub market: Option<String>,
    pub pool: Option<String>,
    pub creator: Option<String>,
    pub quote_mint: Option<String>,
    pub market_priority: Option<Vec<String>>,
    pub min_liquidity_raw: Option<u64>,
    pub skip_low_lq_pools: Option<bool>,
    pub slippage_pct: Option<f64>,
    pub use_idempotent: Option<bool>,
    pub priority_fee_level: Option<String>,
    pub priority_fee_sol: Option<f64>,
    pub use_swqos: Option<bool>,
    pub swqos_settings: Option<Value>,
    pub execute: Option<bool>,
    pub wallet: Option<String>,
    pub rpc_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SellTokenRequest {
    pub mint: String,
    pub sell_pct: Option<u64>,
    pub market: Option<String>,
    pub pool: Option<String>,
    pub creator: Option<String>,
    pub quote_mint: Option<String>,
    pub market_priority: Option<Vec<String>>,
    pub min_liquidity_raw: Option<u64>,
    pub skip_low_lq_pools: Option<bool>,
    pub slippage_pct: Option<f64>,
    pub retries: Option<u32>,
    pub priority_fee_level: Option<String>,
    pub priority_fee_sol: Option<f64>,
    pub use_swqos: Option<bool>,
    pub swqos_settings: Option<Value>,
    pub execute: Option<bool>,
    pub wallet: Option<String>,
    pub rpc_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CreateWalletRequest {
    pub label: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SelectWalletsRequest {
    pub active_wallet: Option<String>,
    pub selected_wallets: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TransferAssetRequest {
    pub from_wallet: String,
    pub to_address: Option<String>,
    pub to_wallet: Option<String>,
    pub amount: String,
    pub asset_kind: String,
    pub mint: Option<String>,
    pub execute: Option<bool>,
    pub simulate: Option<bool>,
    pub rpc_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PreviewWalletCleanRequest {
    pub owner: String,
    pub rpc_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CleanWalletRequest {
    pub owner: String,
    pub token_accounts: Option<Vec<String>>,
    pub burn_nonzero: Option<bool>,
    pub close_empty: Option<bool>,
    pub close_wsol: Option<bool>,
    pub execute: Option<bool>,
    pub simulate: Option<bool>,
    pub rpc_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CreateTokenRequest {
    pub execute: Option<bool>,
    pub method: String,
    pub payer: String,
    pub mint: Option<String>,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub decimals: Option<u8>,
    pub initial_supply: Option<u64>,
    pub freeze_authority: Option<bool>,
    pub revoke_mint_authority: Option<bool>,
    pub revoke_freeze_authority: Option<bool>,
    pub metadata_is_mutable: Option<bool>,
    pub auto_buy: Option<Value>,
    pub simulate: Option<bool>,
    pub rpc_url: Option<String>,
    pub priority_fee_level: Option<String>,
    pub compute_unit_limit: Option<u32>,
    pub use_swqos: Option<bool>,
    pub swqos_settings: Option<Value>,
    pub raydium_launchpad: Option<Value>,
    pub extra: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CreatePoolRequest {
    pub execute: Option<bool>,
    pub request: Value,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListPoolPositionsRequest {
    pub owner: String,
    pub rpc_url: Option<String>,
    pub include_unsupported: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ManagePoolPositionRequest {
    pub execute: Option<bool>,
    pub market: String,
    pub owner: String,
    pub pool: String,
    pub withdraw_pct: Option<f64>,
    pub slippage_pct: Option<f64>,
    pub simulate: Option<bool>,
    pub rpc_url: Option<String>,
    pub priority_fee_level: Option<String>,
    pub compute_unit_limit: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListCreatorsRequest {
    pub min_mint_count: Option<i64>,
    pub min_avg_market_cap: Option<f64>,
    pub min_score: Option<f64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListCreatorMintsRequest {
    pub creator: String,
    pub market: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListTransactionsRequest {
    pub creator: Option<String>,
    pub market: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MarketRequest {
    pub market: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CallMambaApiRequest {
    pub method: String,
    pub path: String,
    pub query: Option<Value>,
    pub body: Option<Value>,
}

fn render_tool_error(error: anyhow::Error) -> String {
    format!("{error:#}")
}

fn wrap_tool_response(data: Value) -> Json<MambaToolResponse> {
    Json(MambaToolResponse { data })
}

fn mamba_server_instructions() -> &'static str {
    concat!(
        "Authenticated MCP bridge for Mamba's local API. ",
        "Never rely on solana CLI or host-installed packages when a Mamba MCP tool can answer the request. ",
        "Use dedicated tools before call_mamba_api. ",
        "Selection rules: health for readiness, get_active_wallet for the current wallet and live SOL balance, get_wallet_balance for arbitrary pubkeys, ",
        "list_wallets for the managed wallet registry, list_tokens for broad websocket-cache discovery, get_token_details for one known mint, ",
        "batch_get_token_metadata for many mints, subscribe_market to warm cache before list_tokens if needed, list_subscriptions to confirm websocket state, ",
        "buy_token and sell_token for swaps, transfer_asset for wallet-to-wallet sends, preview_wallet_clean before clean_wallet, ",
        "list_create_methods before create_token, Raydium Launchpad discovery tools before Raydium Launchpad create_token calls, ",
        "list_pool_methods before create_pool, list_pool_positions before manage_pool_position. ",
        "Build-first is the default safety model: prefer execute=false unless the user clearly wants submission. ",
        "Mamba signs locally during execute mode; private keys never leave Mamba."
    )
}

fn mamba_tool_playbook() -> Value {
    json!({
        "principles": [
            "Prefer dedicated MCP tools over call_mamba_api whenever a dedicated tool exists.",
            "Do not fall back to solana CLI or host packages for wallet balances, token metadata, route discovery, or transaction planning that Mamba already supports.",
            "Prefer read-only inspection tools first, then build tools, then execute mode only when the user clearly wants submission.",
            "Use execute=false by default for buy_token, sell_token, transfer_asset, clean_wallet, create_token, create_pool, and manage_pool_position unless the user clearly wants a live send."
        ],
        "canonical_tools_by_intent": {
            "runtime_readiness": ["health"],
            "supported_market_labels": ["list_supported_markets"],
            "current_wallet_status_or_balance": ["get_active_wallet"],
            "arbitrary_wallet_balance": ["get_wallet_balance"],
            "managed_wallet_registry": ["list_wallets"],
            "create_managed_wallet": ["create_wallet"],
            "switch_active_wallet_or_selection": ["select_wallets"],
            "broad_token_discovery": ["subscribe_market", "list_tokens"],
            "single_mint_route_creator_metadata": ["get_token_details"],
            "multi_mint_metadata_lookup": ["batch_get_token_metadata"],
            "creator_discovery": ["list_creators"],
            "creator_mints": ["list_creator_mints"],
            "observed_transactions": ["list_transactions"],
            "buy_token": ["buy_token"],
            "sell_token": ["sell_token"],
            "wallet_transfer": ["transfer_asset"],
            "wallet_cleanup_preview": ["preview_wallet_clean"],
            "wallet_cleanup_build_or_execute": ["clean_wallet"],
            "discover_create_methods": ["list_create_methods"],
            "raydium_launchpad_create_discovery": [
                "list_raydium_launchpad_global_configs",
                "list_raydium_launchpad_platform_configs",
                "list_raydium_launchpad_platform_curve_params"
            ],
            "token_create": ["create_token"],
            "discover_pool_methods": ["list_pool_methods"],
            "pool_create": ["create_pool"],
            "inspect_pool_positions": ["list_pool_positions"],
            "pool_withdraw_or_manage": ["manage_pool_position"],
            "subscription_start": ["subscribe_market"],
            "subscription_stop": ["unsubscribe_market"],
            "subscription_state": ["list_subscriptions"],
            "raw_http_escape_hatch": ["call_mamba_api"]
        },
        "prefer_over": [
            {
                "prefer": "get_active_wallet",
                "over": "list_wallets",
                "when": "the user wants the current wallet, active wallet, or live SOL balance"
            },
            {
                "prefer": "get_wallet_balance",
                "over": "get_active_wallet",
                "when": "the wallet pubkey is explicitly named and may not be the active wallet"
            },
            {
                "prefer": "get_token_details",
                "over": "list_tokens",
                "when": "the user already knows the mint and wants route, creator, or metadata"
            },
            {
                "prefer": "batch_get_token_metadata",
                "over": "get_token_details",
                "when": "the user needs metadata for many mints and does not need route resolution"
            },
            {
                "prefer": "transfer_asset",
                "over": "buy_token or sell_token",
                "when": "the task is a direct SOL or SPL transfer rather than a market swap"
            },
            {
                "prefer": "preview_wallet_clean",
                "over": "clean_wallet",
                "when": "the user wants to inspect reclaimable SOL or cleanup actions before building or sending"
            },
            {
                "prefer": "call_mamba_api",
                "over": "all other tools",
                "when": "and only when no dedicated MCP tool exists for the needed authenticated HTTP route"
            }
        ],
        "state_changing_tools": [
            "create_wallet",
            "select_wallets",
            "buy_token",
            "sell_token",
            "transfer_asset",
            "clean_wallet",
            "create_token",
            "create_pool",
            "manage_pool_position",
            "subscribe_market",
            "unsubscribe_market"
        ]
    })
}

fn mamba_static_resources() -> Vec<Resource> {
    [
        (
            MCP_RESOURCE_HEALTH_URI,
            "health",
            "Authenticated API health snapshot.",
        ),
        (
            MCP_RESOURCE_MARKETS_URI,
            "markets",
            "Supported Mamba market labels.",
        ),
        (
            MCP_RESOURCE_DOCS_URI,
            "docs",
            "Authenticated Mamba API docs index and route catalog.",
        ),
        (
            MCP_RESOURCE_TOOL_PLAYBOOK_URI,
            "tool_playbook",
            "Canonical MCP tool-selection playbook and intent mapping.",
        ),
        (
            MCP_RESOURCE_ACTIVE_WALLET_URI,
            "active_wallet",
            "Active managed wallet state with live SOL balance.",
        ),
        (
            MCP_RESOURCE_SUBSCRIPTIONS_URI,
            "subscriptions",
            "Active websocket market subscriptions inside Mamba.",
        ),
    ]
    .into_iter()
    .map(|(uri, name, description)| {
        RawResource::new(uri, format!("mamba_{name}"))
            .with_description(description)
            .with_mime_type("application/json")
            .no_annotation()
    })
    .collect()
}

fn mamba_resource_route(uri: &str) -> Option<(&'static str, &'static str)> {
    match uri {
        MCP_RESOURCE_HEALTH_URI => Some((MCP_RESOURCE_HEALTH_URI, "/health")),
        MCP_RESOURCE_MARKETS_URI => Some((MCP_RESOURCE_MARKETS_URI, "/markets")),
        MCP_RESOURCE_DOCS_URI => Some((MCP_RESOURCE_DOCS_URI, "/docs")),
        MCP_RESOURCE_ACTIVE_WALLET_URI => Some((MCP_RESOURCE_ACTIVE_WALLET_URI, "/wallets/active")),
        MCP_RESOURCE_SUBSCRIPTIONS_URI => {
            Some((MCP_RESOURCE_SUBSCRIPTIONS_URI, "/ws/subscriptions"))
        }
        _ => None,
    }
}

fn json_resource_contents(uri: &str, value: Value) -> Result<ResourceContents, McpError> {
    let text = serde_json::to_string_pretty(&value).map_err(|error| {
        McpError::internal_error(format!("serialize resource JSON: {error}"), None)
    })?;
    Ok(ResourceContents::text(text, uri).with_mime_type("application/json"))
}

fn normalize_api_base_url(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("Mamba MCP API base URL cannot be empty");
    }

    if trimmed.ends_with("/mamba-api/v1") {
        return Ok(trimmed.to_string());
    }
    if trimmed.ends_with("/mamba-api") {
        return Ok(format!("{trimmed}/v1"));
    }
    Ok(trimmed.to_string())
}

fn normalize_api_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("/mamba-api/v1")
        .or_else(|| trimmed.strip_prefix("/mamba-api"))
        .unwrap_or(trimmed);
    if stripped.starts_with('/') {
        stripped.to_string()
    } else {
        format!("/{stripped}")
    }
}

fn prune_nulls(value: Option<Value>) -> Option<Value> {
    value.and_then(|value| prune_nulls_inner(value))
}

fn prune_nulls_inner(value: Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                if let Some(value) = prune_nulls_inner(value) {
                    out.insert(key, value);
                }
            }
            Some(Value::Object(out))
        }
        Value::Array(values) => {
            let values = values
                .into_iter()
                .filter_map(prune_nulls_inner)
                .collect::<Vec<_>>();
            Some(Value::Array(values))
        }
        other => Some(other),
    }
}

fn query_pairs_from_value(value: &Value) -> anyhow::Result<Option<Vec<(String, String)>>> {
    let object = match value {
        Value::Object(object) => object,
        _ => bail!("query must be a JSON object"),
    };
    let mut out = Vec::new();
    for (key, value) in object {
        if value.is_null() {
            continue;
        }
        let rendered = match value {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            Value::Bool(value) => value.to_string(),
            Value::Array(values) => values
                .iter()
                .map(render_query_value)
                .collect::<anyhow::Result<Vec<_>>>()?
                .join(","),
            Value::Object(_) => serde_json::to_string(value).context("serialize query object")?,
            Value::Null => continue,
        };
        out.push((key.clone(), rendered));
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

fn render_query_value(value: &Value) -> anyhow::Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Object(_) | Value::Array(_) => {
            serde_json::to_string(value).context("serialize nested query value")
        }
        Value::Null => Ok(String::new()),
    }
}

fn join_csv(values: Option<Vec<String>>) -> Option<String> {
    values.and_then(|values| {
        let values = values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if values.is_empty() {
            None
        } else {
            Some(values.join(","))
        }
    })
}

fn merge_object(base: Value, extra: Option<Value>) -> Result<Value, String> {
    let Some(extra) = extra else {
        return Ok(base);
    };
    let mut base = match base {
        Value::Object(object) => object,
        _ => return Err("base request must be an object".to_string()),
    };
    let extra = match extra {
        Value::Object(object) => object,
        _ => return Err("extra request fields must be a JSON object".to_string()),
    };
    for (key, value) in extra {
        base.insert(key, value);
    }
    Ok(Value::Object(base))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdioFraming {
    ContentLength,
    JsonLine,
}

const STDIO_BRIDGE_BUFFER_BYTES: usize = 64 * 1024;

fn trim_line_endings(value: &str) -> &str {
    value.trim_end_matches(['\r', '\n'])
}

fn parse_content_length_header(line: &str) -> anyhow::Result<Option<usize>> {
    let Some((name, value)) = line.split_once(':') else {
        return Ok(None);
    };
    if !name.trim().eq_ignore_ascii_case("content-length") {
        return Ok(None);
    }
    let length = value
        .trim()
        .parse::<usize>()
        .with_context(|| format!("invalid MCP Content-Length header: {line}"))?;
    Ok(Some(length))
}

async fn read_client_stdio_message<R>(
    reader: &mut R,
) -> anyhow::Result<Option<(StdioFraming, Vec<u8>)>>
where
    R: AsyncBufRead + Unpin,
{
    let mut first_line = String::new();
    loop {
        first_line.clear();
        let bytes = reader
            .read_line(&mut first_line)
            .await
            .context("read MCP stdio input line")?;
        if bytes == 0 {
            return Ok(None);
        }
        if !trim_line_endings(&first_line).is_empty() {
            break;
        }
    }

    let first_line = trim_line_endings(&first_line);
    if first_line.starts_with('{') || first_line.starts_with('[') {
        return Ok(Some((
            StdioFraming::JsonLine,
            first_line.as_bytes().to_vec(),
        )));
    }

    let mut content_length = parse_content_length_header(first_line)?;
    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .await
            .context("read MCP stdio header line")?;
        if bytes == 0 {
            bail!("unexpected EOF while reading MCP stdio headers");
        }
        let line = trim_line_endings(&line);
        if line.is_empty() {
            break;
        }
        if let Some(length) = parse_content_length_header(line)? {
            content_length = Some(length);
        }
    }

    let content_length = content_length.context("missing MCP Content-Length header")?;
    let mut payload = vec![0_u8; content_length];
    reader
        .read_exact(&mut payload)
        .await
        .context("read MCP stdio payload")?;
    Ok(Some((StdioFraming::ContentLength, payload)))
}

async fn wait_for_stdio_framing(
    framing_rx: &mut watch::Receiver<Option<StdioFraming>>,
) -> anyhow::Result<StdioFraming> {
    loop {
        if let Some(framing) = *framing_rx.borrow() {
            return Ok(framing);
        }
        framing_rx
            .changed()
            .await
            .context("wait for MCP stdio framing")?;
    }
}

async fn bridge_client_stdin<R, W>(
    input: R,
    mut bridge_write: W,
    framing_tx: watch::Sender<Option<StdioFraming>>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(input);
    while let Some((framing, payload)) = read_client_stdio_message(&mut reader).await? {
        if framing_tx.borrow().is_none() {
            let _ = framing_tx.send(Some(framing));
        }
        bridge_write
            .write_all(&payload)
            .await
            .context("write MCP payload into rmcp bridge")?;
        bridge_write
            .write_all(b"\n")
            .await
            .context("write MCP newline into rmcp bridge")?;
        bridge_write
            .flush()
            .await
            .context("flush MCP rmcp bridge input")?;
    }
    bridge_write
        .shutdown()
        .await
        .context("shutdown MCP rmcp bridge input")?;
    Ok(())
}

async fn bridge_client_stdout<R, W>(
    output: R,
    mut stdout: W,
    mut framing_rx: watch::Receiver<Option<StdioFraming>>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(output);
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .await
            .context("read rmcp bridge output")?;
        if bytes == 0 {
            stdout.flush().await.context("flush MCP stdout on EOF")?;
            return Ok(());
        }
        let payload = trim_line_endings(&line);
        if payload.is_empty() {
            continue;
        }
        match wait_for_stdio_framing(&mut framing_rx).await? {
            StdioFraming::JsonLine => {
                stdout
                    .write_all(payload.as_bytes())
                    .await
                    .context("write MCP json-line payload to stdout")?;
                stdout
                    .write_all(b"\n")
                    .await
                    .context("write MCP json-line delimiter to stdout")?;
            }
            StdioFraming::ContentLength => {
                let payload = payload.as_bytes();
                let header = format!("Content-Length: {}\r\n\r\n", payload.len());
                stdout
                    .write_all(header.as_bytes())
                    .await
                    .context("write MCP Content-Length header to stdout")?;
                stdout
                    .write_all(payload)
                    .await
                    .context("write MCP framed payload to stdout")?;
            }
        }
        stdout.flush().await.context("flush MCP stdout")?;
    }
}

pub async fn run_from_env() -> anyhow::Result<()> {
    if let Some(raw) = std::env::var("MAMBA_PRIVATE_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        if let Err(error) = seed_managed_wallet_store_from_env(&raw) {
            eprintln!("mamba_mcp wallet-store seed skipped: {error:#}");
        }
    }

    let server = MambaMcpServer::new(MambaMcpConfig::from_env()?)?;
    eprintln!("mamba_mcp connected to {}", server.client.api_base_url);
    let (service_stream, bridge_stream) = tokio::io::duplex(STDIO_BRIDGE_BUFFER_BYTES);
    let (bridge_read, bridge_write) = tokio::io::split(bridge_stream);
    let (framing_tx, framing_rx) = watch::channel(None::<StdioFraming>);

    let stdin_task = tokio::spawn(async move {
        bridge_client_stdin(tokio::io::stdin(), bridge_write, framing_tx).await
    });
    let stdout_task = tokio::spawn(async move {
        bridge_client_stdout(bridge_read, tokio::io::stdout(), framing_rx).await
    });

    let service = server.serve(service_stream).await?;
    tokio::try_join!(
        async { service.waiting().await.map_err(anyhow::Error::from) },
        async {
            stdin_task
                .await
                .context("MCP stdin bridge task join failure")?
        },
        async {
            stdout_task
                .await
                .context("MCP stdout bridge task join failure")?
        },
    )?;
    Ok(())
}

fn seed_managed_wallet_store_from_env(raw: &str) -> anyhow::Result<()> {
    let signer = if raw.starts_with('[') {
        let bytes: Vec<u8> = serde_json::from_str(raw)
            .context("failed to parse MAMBA_PRIVATE_KEY as JSON byte array")?;
        solana_keypair::Keypair::try_from(bytes.as_slice())
            .context("MAMBA_PRIVATE_KEY JSON must encode 64 keypair bytes")?
    } else {
        let bytes = bs58::decode(raw)
            .into_vec()
            .context("failed to parse MAMBA_PRIVATE_KEY as base58")?;
        solana_keypair::Keypair::try_from(bytes.as_slice())
            .context("MAMBA_PRIVATE_KEY base58 must decode to 64 keypair bytes")?
    };

    crate::core::wallet::ensure_wallet_store_has_signer(&signer, "main")?;
    Ok(())
}
