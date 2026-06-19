use {
    crate::core::cluster::{DEFAULT_DEVNET_HTTP_URL, DEFAULT_MAINNET_HTTP_URL, SolanaCluster},
    crate::core::sol::{PriorityFeeOverride, SolHook},
    crate::dex::{
        meteora_damm_v1::MeteoraDammV1, meteora_damm_v2::MeteoraDammV2, meteora_dbc::MeteoraDbc,
        meteora_dlmm::MeteoraDlmm, pump_fun::PumpFun, pump_swap::PumpSwap,
        raydium_amm_v4::RaydiumAmmV4, raydium_clmm::RaydiumClmm, raydium_cpmm::RaydiumCpmm,
        raydium_launchpad::RaydiumLaunchpad,
    },
    crate::swqos::SWQoSettings,
    crate::utils::writing::cc,
    crate::{log, warn},
    anyhow::Context,
    futures::future::join_all,
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_commitment_config::CommitmentConfig,
    solana_program::{instruction::Instruction, pubkey::Pubkey},
    solana_signature::Signature,
    solana_signer::Signer,
    std::str::FromStr,
    std::sync::Arc,
    std::time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Market {
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

impl Market {
    pub fn as_str(&self) -> &'static str {
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

    fn normalize_token(value: &str) -> String {
        value
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase()
    }

    pub fn from_token(value: &str) -> Option<Self> {
        match Self::normalize_token(value).as_str() {
            "pumpswap" => Some(Self::PumpSwap),
            "pumpfun" => Some(Self::PumpFun),
            "raydiumammv4" => Some(Self::RaydiumAmmV4),
            "raydiumlaunchpad" => Some(Self::RaydiumLaunchpad),
            "raydiumclmm" => Some(Self::RaydiumClmm),
            "raydiumcpmm" => Some(Self::RaydiumCpmm),
            "meteoradlmm" => Some(Self::MeteoraDlmm),
            "meteoradammv1" => Some(Self::MeteoraDammV1),
            "meteoradammv2" => Some(Self::MeteoraDammV2),
            "meteoradbc" => Some(Self::MeteoraDbc),
            _ => None,
        }
    }

    pub fn parse_csv(raw: &str) -> anyhow::Result<Vec<Self>> {
        let mut markets = Vec::new();
        for token in raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let market = Self::from_token(token).with_context(|| {
                format!(
                    "unsupported market '{token}'; supported values: pump_swap,pump_fun,raydium_amm_v4,\
raydium_launchpad,raydium_clmm,raydium_cpmm,meteora_dlmm,meteora_damm_v1,meteora_damm_v2,meteora_dbc"
                )
            })?;
            if !markets.contains(&market) {
                markets.push(market);
            }
        }
        anyhow::ensure!(!markets.is_empty(), "market list is empty");
        Ok(markets)
    }
}

pub const DEFAULT_MARKET_PRIORITY: [Market; 10] = [
    Market::PumpSwap,
    Market::PumpFun,
    Market::RaydiumAmmV4,
    Market::RaydiumLaunchpad,
    Market::RaydiumCpmm,
    Market::MeteoraDammV1,
    Market::MeteoraDammV2,
    Market::MeteoraDbc,
    Market::RaydiumClmm,
    Market::MeteoraDlmm,
];

const DEFAULT_MARKET_PRIMARY_LOOKUP: [Market; 9] = [
    Market::PumpSwap,
    Market::PumpFun,
    Market::RaydiumAmmV4,
    Market::RaydiumLaunchpad,
    Market::RaydiumCpmm,
    Market::MeteoraDammV1,
    Market::MeteoraDammV2,
    Market::MeteoraDbc,
    Market::MeteoraDlmm,
];

const DEFAULT_MARKET_DEFERRED_LOOKUP: [Market; 1] = [Market::RaydiumClmm];
const LAST_RESORT_LOOKUP_MARKETS: [Market; 1] = [Market::MeteoraDlmm];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MintPoolRoute {
    pub market: Market,
    pub pool: Pubkey,
    pub creator: Pubkey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatorResolutionSource {
    MetadataFirstCreator,
    MarketStateFallback,
    Unresolved,
}

impl CreatorResolutionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MetadataFirstCreator => "metadata_first_creator",
            Self::MarketStateFallback => "market_state_fallback",
            Self::Unresolved => "unresolved",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MintCreatorResolution {
    pub creator: Pubkey,
    pub source: CreatorResolutionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MintCreatorRoute {
    pub market: Market,
    pub pool: Pubkey,
    pub creator: Pubkey,
    pub source: CreatorResolutionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RouteLiquiditySnapshot {
    pub wsol_liquidity_raw: u64,
    pub max_safe_buy_sol_raw: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintPoolRouteSelection {
    pub route: MintPoolRoute,
    pub liquidity: RouteLiquiditySnapshot,
    pub low_lq: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintCreatorRouteSelection {
    pub route: MintCreatorRoute,
    pub liquidity: RouteLiquiditySnapshot,
    pub low_lq: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MeasuredRouteCandidate {
    market: Market,
    pool: Pubkey,
    liquidity: RouteLiquiditySnapshot,
}

pub struct Swaps {
    pub sol_hook: SolHook,
    pub pump_swap: PumpSwap,
    pub pump_fun: PumpFun,
    pub raydium_amm_v4: RaydiumAmmV4,
    pub raydium_launchpad: RaydiumLaunchpad,
    pub raydium_clmm: RaydiumClmm,
    pub raydium_cpmm: RaydiumCpmm,
    pub meteora_dlmm: MeteoraDlmm,
    pub meteora_damm_v1: MeteoraDammV1,
    pub meteora_damm_v2: MeteoraDammV2,
    pub meteora_dbc: MeteoraDbc,
}

#[derive(Debug, Clone)]
pub struct SwapExecutionResult {
    pub success: bool,
    pub signature: Option<Signature>,
    pub error: Option<String>,
}

#[allow(unused)]
impl Swaps {
    const DEFAULT_ROUTE_LOOKUP_TIMEOUT_SECS: u64 = 15;
    const ROUTE_LOOKUP_TIMEOUT_ENV: &'static str = "MAMBA_ROUTE_LOOKUP_TIMEOUT_SECS";
    const METADATA_CREATOR_LOOKUP_TIMEOUT_MS: u64 = 500;
    const ROUTE_PREFERENCE_LOOKUP_TIMEOUT_CAP_SECS: u64 = 3;
    const ROUTE_LIQUIDITY_LOOKUP_TIMEOUT_CAP_SECS: u64 = 5;
    const BUY_CAPACITY_SAFETY_MARGIN_BPS: u64 = 9_000;
    const LOW_LQ_WSOL_THRESHOLD_RAW: u64 = 10_000_000_000;
    const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

    fn parse_pubkey_input(label: &str, value: &str) -> anyhow::Result<Pubkey> {
        let trimmed = value.trim();
        anyhow::ensure!(!trimmed.is_empty(), "{label} pubkey is empty");
        Pubkey::from_str(trimmed).with_context(|| format!("invalid {label} pubkey: {trimmed}"))
    }

    fn parse_optional_pubkey_input(
        label: &str,
        value: Option<&str>,
    ) -> anyhow::Result<Option<Pubkey>> {
        let Some(raw) = value else {
            return Ok(None);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let parsed = Pubkey::from_str(trimmed)
            .with_context(|| format!("invalid {label} pubkey: {trimmed}"))?;
        Ok(Some(parsed))
    }

    fn trade_send_error_is_retryable(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        if (lower.contains("transaction failed ")
            && !lower.contains("transaction failed to confirm "))
            || lower.contains("status err:")
            || lower.contains("meta err:")
            || lower.contains("custom program error")
            || lower.contains("anchorerror")
            || lower.contains("instructionerror(")
        {
            return false;
        }

        lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("didn't land on-chain")
            || lower.contains("did not land on-chain")
            || lower.contains("connection")
            || lower.contains("429")
            || lower.contains("rate limited")
            || lower.contains("too many requests")
            || lower.contains("blockhash not found")
    }

    pub fn new(sol_hook: SolHook, pump_swap: PumpSwap, pump_fun: PumpFun) -> Self {
        let raydium_amm_v4 = RaydiumAmmV4::new(pump_fun.keypair.clone(), pump_fun.sol.clone());
        let raydium_launchpad =
            RaydiumLaunchpad::new(pump_fun.keypair.clone(), pump_fun.sol.clone());
        let raydium_clmm = RaydiumClmm::new(pump_fun.keypair.clone(), pump_fun.sol.clone());
        let raydium_cpmm = RaydiumCpmm::new(pump_fun.keypair.clone(), pump_fun.sol.clone());
        let meteora_dlmm = MeteoraDlmm::new(pump_fun.keypair.clone(), pump_fun.sol.clone());
        let meteora_damm_v1 = MeteoraDammV1::new(pump_fun.keypair.clone(), pump_fun.sol.clone());
        let meteora_damm_v2 = MeteoraDammV2::new(pump_fun.keypair.clone(), pump_fun.sol.clone());
        let meteora_dbc = MeteoraDbc::new(pump_fun.keypair.clone(), pump_fun.sol.clone());
        Self {
            sol_hook,
            pump_swap,
            pump_fun,
            raydium_amm_v4,
            raydium_launchpad,
            raydium_clmm,
            raydium_cpmm,
            meteora_dlmm,
            meteora_damm_v1,
            meteora_damm_v2,
            meteora_dbc,
        }
    }

    pub fn default_market_priority() -> &'static [Market] {
        &DEFAULT_MARKET_PRIORITY
    }

    fn default_market_lookup_groups() -> [&'static [Market]; 2] {
        [
            &DEFAULT_MARKET_PRIMARY_LOOKUP,
            &DEFAULT_MARKET_DEFERRED_LOOKUP,
        ]
    }

    fn is_deferred_lookup_market(market: Market) -> bool {
        DEFAULT_MARKET_DEFERRED_LOOKUP.contains(&market)
    }

    fn is_last_resort_lookup_market(market: Market) -> bool {
        LAST_RESORT_LOOKUP_MARKETS.contains(&market)
    }

    fn buy_needs_shared_wsol_cleanup(market: Market) -> bool {
        !matches!(market, Market::PumpFun)
    }

    fn update_best_measured_candidate(
        best: &mut Option<MeasuredRouteCandidate>,
        candidate: MeasuredRouteCandidate,
    ) {
        if best
            .as_ref()
            .is_none_or(|current| Self::better_candidate(&candidate, current))
        {
            *best = Some(candidate);
        }
    }

    fn market_lookup_groups_for_priority(markets: &[Market]) -> Vec<Vec<Market>> {
        let mut primary = Vec::new();
        let mut deferred = Vec::new();
        for market in markets.iter().copied() {
            if Self::is_deferred_lookup_market(market) {
                deferred.push(market);
            } else {
                primary.push(market);
            }
        }

        let mut groups = Vec::new();
        if !primary.is_empty() {
            groups.push(primary);
        }
        if !deferred.is_empty() {
            groups.push(deferred);
        }
        groups
    }

    fn parse_route_lookup_timeout_secs(raw: Option<&str>) -> u64 {
        raw.and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(Self::DEFAULT_ROUTE_LOOKUP_TIMEOUT_SECS)
    }

    fn route_lookup_timeout_secs() -> u64 {
        Self::parse_route_lookup_timeout_secs(
            std::env::var(Self::ROUTE_LOOKUP_TIMEOUT_ENV)
                .ok()
                .as_deref(),
        )
    }

    fn allocate_group_route_lookup_timeout(deadline: Instant, groups_remaining: usize) -> Duration {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return remaining;
        }
        let groups_remaining = groups_remaining.max(1);
        let per_group_secs = (remaining.as_secs_f64() / groups_remaining as f64).ceil();
        Duration::from_secs_f64(per_group_secs.max(1.0)).min(remaining)
    }

    fn allocate_route_preference_lookup_timeout(deadline: Instant) -> Duration {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return remaining;
        }
        remaining.min(Duration::from_secs(
            Self::ROUTE_PREFERENCE_LOOKUP_TIMEOUT_CAP_SECS,
        ))
    }

    fn allocate_route_liquidity_lookup_timeout(deadline: Instant) -> Duration {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return remaining;
        }
        remaining.min(Duration::from_secs(
            Self::ROUTE_LIQUIDITY_LOOKUP_TIMEOUT_CAP_SECS,
        ))
    }

    fn prioritize_markets(markets: &[Market], preferred: &[Market]) -> Vec<Market> {
        let mut ordered = Vec::with_capacity(markets.len());
        for market in preferred.iter().copied() {
            if markets.contains(&market) && !ordered.contains(&market) {
                ordered.push(market);
            }
        }
        for market in markets.iter().copied() {
            if !ordered.contains(&market) {
                ordered.push(market);
            }
        }
        ordered
    }

    fn preferred_route_markets_from_mint_family(
        mint: &str,
        requested_markets: &[Market],
    ) -> Vec<Market> {
        let normalized = mint.trim().to_ascii_lowercase();
        let preferred = if normalized.ends_with("pump") {
            vec![
                Market::RaydiumAmmV4,
                Market::RaydiumCpmm,
                Market::PumpSwap,
                Market::PumpFun,
            ]
        } else if normalized.ends_with("bags") {
            vec![
                Market::MeteoraDammV2,
                Market::MeteoraDammV1,
                Market::MeteoraDbc,
            ]
        } else {
            Vec::new()
        };

        preferred
            .into_iter()
            .filter(|market| requested_markets.contains(market))
            .collect()
    }

    pub fn route_lookup_timeout_duration(_market_priority: Option<&[Market]>) -> Duration {
        Duration::from_secs(Self::route_lookup_timeout_secs())
    }

    pub fn price_lookup_timeout_duration() -> Duration {
        Duration::from_secs(Self::route_lookup_timeout_secs())
    }

    pub fn lamports_to_sol(raw: u64) -> f64 {
        raw as f64 / Self::LAMPORTS_PER_SOL
    }

    pub fn low_lq_wsol_threshold_raw() -> u64 {
        Self::LOW_LQ_WSOL_THRESHOLD_RAW
    }

    pub fn low_lq_wsol_threshold_sol() -> f64 {
        Self::lamports_to_sol(Self::LOW_LQ_WSOL_THRESHOLD_RAW)
    }

    pub fn market_is_low_lq_exempt(market: Market) -> bool {
        matches!(
            market,
            Market::PumpFun | Market::RaydiumLaunchpad | Market::MeteoraDbc
        )
    }

    pub fn route_is_low_lq(market: Market, liquidity: &RouteLiquiditySnapshot) -> bool {
        !Self::market_is_low_lq_exempt(market)
            && liquidity.wsol_liquidity_raw < Self::LOW_LQ_WSOL_THRESHOLD_RAW
    }

    pub fn low_lq_warning_for_market_pool(
        market: Market,
        pool: &Pubkey,
        liquidity: &RouteLiquiditySnapshot,
    ) -> Option<String> {
        if !Self::route_is_low_lq(market, liquidity) {
            return None;
        }

        Some(format!(
            "selected {} pool {} has low liquidity: WSOL quote {:.6} SOL is below the 10 SOL threshold",
            market.as_str(),
            pool,
            Self::lamports_to_sol(liquidity.wsol_liquidity_raw),
        ))
    }

    fn append_route_warning(existing: Option<String>, warning: Option<String>) -> Option<String> {
        match (
            existing
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            warning
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        ) {
            (Some(existing), Some(warning)) => Some(format!("{existing}; {warning}")),
            (Some(existing), None) => Some(existing),
            (None, Some(warning)) => Some(warning),
            (None, None) => None,
        }
    }

    async fn find_pool_for_mint_with_market_priority_timed(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        market_priority: Option<&[Market]>,
    ) -> anyhow::Result<Option<MintPoolRoute>> {
        let timeout = Self::route_lookup_timeout_duration(market_priority);
        match tokio::time::timeout(
            timeout,
            self.find_pool_for_mint_with_market_priority(
                mint,
                quote_mint,
                min_liquidity_raw,
                market_priority,
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "mint route lookup timed out after {}s for {}",
                timeout.as_secs(),
                mint
            )),
        }
    }

    fn apply_buy_capacity_safety_margin(raw: u64) -> u64 {
        ((u128::from(raw) * u128::from(Self::BUY_CAPACITY_SAFETY_MARGIN_BPS)) / 10_000u128)
            .min(u128::from(u64::MAX)) as u64
    }

    fn sol_capacity_from_token_reserve_raw(
        token_reserve_raw: u64,
        token_decimals: u8,
        price_sol: f64,
    ) -> u64 {
        if token_reserve_raw == 0 || !price_sol.is_finite() || price_sol <= 0.0 {
            return 0;
        }
        let reserve_ui = token_reserve_raw as f64 / 10_f64.powi(token_decimals as i32);
        let capacity_sol = reserve_ui * price_sol;
        if !capacity_sol.is_finite() || capacity_sol <= 0.0 {
            return 0;
        }
        ((capacity_sol * Self::LAMPORTS_PER_SOL)
            .floor()
            .clamp(0.0, u64::MAX as f64)) as u64
    }

    fn candidate_capacity_raw(liquidity: &RouteLiquiditySnapshot) -> u64 {
        if liquidity.max_safe_buy_sol_raw > 0 {
            liquidity.max_safe_buy_sol_raw
        } else {
            liquidity.wsol_liquidity_raw
        }
    }

    fn better_candidate(left: &MeasuredRouteCandidate, right: &MeasuredRouteCandidate) -> bool {
        let left_capacity = Self::candidate_capacity_raw(&left.liquidity);
        let right_capacity = Self::candidate_capacity_raw(&right.liquidity);
        left_capacity > right_capacity
            || (left_capacity == right_capacity
                && left.liquidity.wsol_liquidity_raw > right.liquidity.wsol_liquidity_raw)
    }

    fn choose_best_measured_candidate(
        candidates: &[MeasuredRouteCandidate],
    ) -> Option<MeasuredRouteCandidate> {
        let mut best = None;
        for candidate in candidates.iter().copied() {
            if best
                .as_ref()
                .is_none_or(|current| Self::better_candidate(&candidate, current))
            {
                best = Some(candidate);
            }
        }
        best
    }

    fn relaxed_liquidity_warning(
        mint: &Pubkey,
        candidate: &MeasuredRouteCandidate,
        min_liquidity_raw: u64,
    ) -> String {
        format!(
            "no route met min_liquidity_raw={} for mint {}; using best available {} pool {} (estimated safe max buy {:.6} SOL, WSOL liquidity {:.6} SOL)",
            min_liquidity_raw,
            mint,
            candidate.market.as_str(),
            candidate.pool,
            Self::lamports_to_sol(candidate.liquidity.max_safe_buy_sol_raw),
            Self::lamports_to_sol(candidate.liquidity.wsol_liquidity_raw),
        )
    }

    async fn fetch_price_for_market_pool_timed(
        &self,
        market: Market,
        pool: &Pubkey,
    ) -> anyhow::Result<f64> {
        let timeout = Self::price_lookup_timeout_duration();
        match tokio::time::timeout(timeout, self.fetch_price_for_market_pool(market, pool)).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "price lookup timed out after {}s for market {} pool {}",
                timeout.as_secs(),
                market.as_str(),
                pool
            )),
        }
    }

    fn readonly_fallback_rpc_url(&self) -> Option<&'static str> {
        let current_url = self.sol_hook.rpc_client.url();
        let fallback = match self.sol_hook.cluster {
            SolanaCluster::MainnetBeta => Some(DEFAULT_MAINNET_HTTP_URL),
            SolanaCluster::Devnet => Some(DEFAULT_DEVNET_HTTP_URL),
            SolanaCluster::Unknown if current_url.contains("devnet") => {
                Some(DEFAULT_DEVNET_HTTP_URL)
            }
            SolanaCluster::Unknown => Some(DEFAULT_MAINNET_HTTP_URL),
            _ => None,
        }?;
        (current_url != fallback).then_some(fallback)
    }

    fn readonly_fallback_sol_hook(&self) -> Option<Arc<SolHook>> {
        let rpc_url = self.readonly_fallback_rpc_url()?;
        let rpc_client = Arc::new(RpcClient::new_with_commitment(
            rpc_url.to_string(),
            CommitmentConfig::confirmed(),
        ));
        Some(Arc::new(SolHook::from_rpc_client_with_cluster(
            rpc_client,
            self.sol_hook.cluster,
        )))
    }

    async fn detect_meteora_migration_target_markets(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Option<Vec<Market>>> {
        let fallback_dbc = self
            .readonly_fallback_sol_hook()
            .map(|sol| MeteoraDbc::new(self.pump_fun.keypair.clone(), sol));
        let pools = match self.meteora_dbc.find_pools_by_mint(mint, quote_mint).await {
            Ok(pools) => pools,
            Err(primary_error) => {
                let Some(fallback_dbc) = fallback_dbc.as_ref() else {
                    return Err(primary_error);
                };
                warn!(
                    "meteora dbc migration hint lookup failed for mint {} on primary rpc: {}; retrying via {}",
                    mint,
                    primary_error,
                    self.readonly_fallback_rpc_url().unwrap_or("<none>")
                );
                fallback_dbc.find_pools_by_mint(mint, quote_mint).await?
            }
        };
        for pool in pools {
            let state = match self.meteora_dbc.fetch_state(&pool).await {
                Ok(state) => state,
                Err(error) => {
                    if let Some(fallback_dbc) = fallback_dbc.as_ref() {
                        match fallback_dbc.fetch_state(&pool).await {
                            Ok(state) => state,
                            Err(fallback_error) => {
                                warn!(
                                    "failed to inspect meteora dbc migration state for mint {} pool {} on primary rpc: {}; fallback rpc also failed: {}",
                                    mint, pool, error, fallback_error
                                );
                                continue;
                            }
                        }
                    } else {
                        warn!(
                            "failed to inspect meteora dbc migration state for mint {} pool {}: {}",
                            mint, pool, error
                        );
                        continue;
                    }
                }
            };
            if state.virtual_pool.base_mint != *mint || state.virtual_pool.is_migrated == 0 {
                continue;
            }
            if state.config.migration_option == 1 {
                return Ok(Some(vec![Market::MeteoraDammV2, Market::MeteoraDammV1]));
            }
            return Ok(Some(vec![Market::MeteoraDammV1, Market::MeteoraDammV2]));
        }
        Ok(None)
    }

    async fn detect_raydium_launchpad_target_markets(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Option<Vec<Market>>> {
        const LAUNCHPAD_POOL_STATUS_TRADE: u8 = 2;
        const LAUNCHPAD_MIGRATE_TYPE_CPMM: u8 = 1;

        let fallback_launchpad = self
            .readonly_fallback_sol_hook()
            .map(|sol| RaydiumLaunchpad::new(self.pump_fun.keypair.clone(), sol));
        let pools = match self
            .raydium_launchpad
            .find_pools_by_mint(mint, quote_mint)
            .await
        {
            Ok(pools) => pools,
            Err(primary_error) => {
                let Some(fallback_launchpad) = fallback_launchpad.as_ref() else {
                    return Err(primary_error);
                };
                warn!(
                    "raydium launchpad migration hint lookup failed for mint {} on primary rpc: {}; retrying via {}",
                    mint,
                    primary_error,
                    self.readonly_fallback_rpc_url().unwrap_or("<none>")
                );
                fallback_launchpad
                    .find_pools_by_mint(mint, quote_mint)
                    .await?
            }
        };
        for pool in pools {
            let state = match self.raydium_launchpad.fetch_state(&pool).await {
                Ok(state) => state,
                Err(error) => {
                    if let Some(fallback_launchpad) = fallback_launchpad.as_ref() {
                        match fallback_launchpad.fetch_state(&pool).await {
                            Ok(state) => state,
                            Err(fallback_error) => {
                                warn!(
                                    "failed to inspect raydium launchpad migration state for mint {} pool {} on primary rpc: {}; fallback rpc also failed: {}",
                                    mint, pool, error, fallback_error
                                );
                                continue;
                            }
                        }
                    } else {
                        warn!(
                            "failed to inspect raydium launchpad migration state for mint {} pool {}: {}",
                            mint, pool, error
                        );
                        continue;
                    }
                }
            };
            if state.base_mint != *mint && state.quote_mint != *mint {
                continue;
            }
            if state.status != LAUNCHPAD_POOL_STATUS_TRADE {
                continue;
            }
            if state.migrate_type == LAUNCHPAD_MIGRATE_TYPE_CPMM {
                return Ok(Some(vec![Market::RaydiumCpmm, Market::RaydiumAmmV4]));
            }
            return Ok(Some(vec![Market::RaydiumAmmV4, Market::RaydiumCpmm]));
        }
        Ok(None)
    }

    async fn detect_preferred_route_markets(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Vec<Market>> {
        if let Some(markets) = self
            .detect_meteora_migration_target_markets(mint, quote_mint)
            .await?
        {
            return Ok(markets);
        }
        if let Some(markets) = self
            .detect_raydium_launchpad_target_markets(mint, quote_mint)
            .await?
        {
            return Ok(markets);
        }
        Ok(Vec::new())
    }

    async fn detect_preferred_route_markets_before_deadline(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
        deadline: Instant,
    ) -> Vec<Market> {
        let timeout = Self::allocate_route_preference_lookup_timeout(deadline);
        if timeout.is_zero() {
            return Vec::new();
        }

        match tokio::time::timeout(
            timeout,
            self.detect_preferred_route_markets(mint, quote_mint),
        )
        .await
        {
            Ok(Ok(markets)) => markets,
            Ok(Err(error)) => {
                warn!(
                    "failed to detect preferred route markets for mint {}: {}",
                    mint, error
                );
                Vec::new()
            }
            Err(_) => {
                warn!(
                    "preferred route hint lookup timed out for mint {} after {}ms; continuing with requested market priority",
                    mint,
                    timeout.as_millis()
                );
                Vec::new()
            }
        }
    }

    async fn fetch_market_wsol_liquidity_raw(
        &self,
        market: Market,
        pool: &Pubkey,
    ) -> anyhow::Result<u64> {
        match market {
            Market::PumpSwap => {
                let state = self.pump_swap.fetch_state(pool).await?;
                self.pump_swap.fetch_wsol_liquidity_raw(&state).await
            }
            Market::PumpFun => {
                let state = self.pump_fun.fetch_state(pool).await?;
                Ok(state.real_sol_reserves)
            }
            Market::RaydiumAmmV4 => {
                let state = self.raydium_amm_v4.fetch_state(pool).await?;
                self.raydium_amm_v4.fetch_wsol_liquidity_raw(&state).await
            }
            Market::RaydiumLaunchpad => {
                let state = self.raydium_launchpad.fetch_state(pool).await?;
                self.raydium_launchpad
                    .fetch_wsol_liquidity_raw(&state)
                    .await
            }
            Market::RaydiumClmm => {
                let state = self.raydium_clmm.fetch_state(pool).await?;
                let wsol_vault = if state.mint_a == crate::core::sol::WSOL_MINT {
                    state.vault_a
                } else if state.mint_b == crate::core::sol::WSOL_MINT {
                    state.vault_b
                } else {
                    anyhow::bail!("raydium clmm pool is not WSOL-quoted");
                };
                let (raw_balance, _) = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&wsol_vault)
                    .await?;
                Ok(raw_balance)
            }
            Market::RaydiumCpmm => {
                let state = self.raydium_cpmm.fetch_state(pool).await?;
                self.raydium_cpmm.fetch_wsol_liquidity_raw(&state).await
            }
            Market::MeteoraDlmm => {
                let state = self.meteora_dlmm.fetch_state(pool).await?;
                self.meteora_dlmm.fetch_wsol_liquidity_raw(&state).await
            }
            Market::MeteoraDammV1 => {
                let state = self.meteora_damm_v1.fetch_state(pool).await?;
                self.meteora_damm_v1.fetch_wsol_liquidity_raw(&state).await
            }
            Market::MeteoraDammV2 => {
                let state = self.meteora_damm_v2.fetch_state(pool).await?;
                self.meteora_damm_v2.fetch_wsol_liquidity_raw(&state).await
            }
            Market::MeteoraDbc => {
                let state = self.meteora_dbc.fetch_state(pool).await?;
                self.meteora_dbc.fetch_wsol_liquidity_raw(&state).await
            }
        }
    }

    async fn fetch_market_liquidity_snapshot(
        &self,
        market: Market,
        pool: &Pubkey,
        mint: &Pubkey,
    ) -> anyhow::Result<RouteLiquiditySnapshot> {
        let snapshot = match market {
            Market::PumpSwap => {
                let state = self.pump_swap.fetch_state(pool).await?;
                let base_mint = Pubkey::new_from_array(state.base_mint.to_bytes());
                let quote_mint = Pubkey::new_from_array(state.quote_mint.to_bytes());
                let pool_base = Pubkey::new_from_array(state.pool_base_token_account.to_bytes());
                let pool_quote = Pubkey::new_from_array(state.pool_quote_token_account.to_bytes());
                let base_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&pool_base)
                    .await?
                    .0;
                let quote_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&pool_quote)
                    .await?
                    .0;
                let token_decimals = self.sol_hook.get_token_decimals(mint).await?;

                if base_mint == *mint && quote_mint == crate::core::sol::WSOL_MINT {
                    let token_ui = base_raw as f64 / 10_f64.powi(token_decimals as i32);
                    anyhow::ensure!(token_ui > 0.0, "pump.swap token reserve is zero");
                    let price = (quote_raw as f64 / Self::LAMPORTS_PER_SOL) / token_ui;
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(base_raw, token_decimals, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: quote_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else if quote_mint == *mint && base_mint == crate::core::sol::WSOL_MINT {
                    let price = {
                        let token_ui = quote_raw as f64 / 10_f64.powi(token_decimals as i32);
                        let sol_ui = base_raw as f64 / Self::LAMPORTS_PER_SOL;
                        anyhow::ensure!(token_ui > 0.0, "pump.swap token reserve is zero");
                        sol_ui / token_ui
                    };
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(quote_raw, token_decimals, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: base_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else {
                    anyhow::bail!(
                        "pump.swap pool does not contain mint {} as the non-WSOL side",
                        mint
                    );
                }
            }
            Market::PumpFun => {
                let state = self.pump_fun.fetch_state(pool).await?;
                let token_decimals = self.sol_hook.get_token_decimals(mint).await?;
                let price = self.pump_fun.fetch_price(pool).await?.1;
                let max_buy = Self::sol_capacity_from_token_reserve_raw(
                    state.real_token_reserves,
                    token_decimals,
                    price,
                );
                RouteLiquiditySnapshot {
                    wsol_liquidity_raw: state.real_sol_reserves,
                    max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                }
            }
            Market::RaydiumAmmV4 => {
                let state = self.raydium_amm_v4.fetch_state(pool).await?;
                let base_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&state.base_vault)
                    .await?
                    .0;
                let quote_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&state.quote_vault)
                    .await?
                    .0;
                let base_net = base_raw.saturating_sub(state.base_need_take_pnl);
                let quote_net = quote_raw.saturating_sub(state.quote_need_take_pnl);

                if state.base_mint == *mint && state.quote_mint == crate::core::sol::WSOL_MINT {
                    let token_decimals = u8::try_from(state.base_decimals)?;
                    let token_ui = base_net as f64 / 10_f64.powi(token_decimals as i32);
                    anyhow::ensure!(token_ui > 0.0, "raydium amm v4 token reserve is zero");
                    let price = (quote_net as f64 / Self::LAMPORTS_PER_SOL) / token_ui;
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(base_net, token_decimals, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: quote_net,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else if state.quote_mint == *mint
                    && state.base_mint == crate::core::sol::WSOL_MINT
                {
                    let token_decimals = u8::try_from(state.quote_decimals)?;
                    let token_ui = quote_net as f64 / 10_f64.powi(token_decimals as i32);
                    anyhow::ensure!(token_ui > 0.0, "raydium amm v4 token reserve is zero");
                    let price = (base_net as f64 / Self::LAMPORTS_PER_SOL) / token_ui;
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(quote_net, token_decimals, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: base_net,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else {
                    anyhow::bail!(
                        "raydium amm v4 pool does not contain mint {} as the non-WSOL side",
                        mint
                    );
                }
            }
            Market::RaydiumLaunchpad => {
                let state = self.raydium_launchpad.fetch_state(pool).await?;
                anyhow::ensure!(
                    state.base_mint == *mint && state.quote_mint == crate::core::sol::WSOL_MINT,
                    "raydium launchpad pool does not contain mint {} as the base mint with WSOL quote",
                    mint
                );
                let global = self
                    .raydium_launchpad
                    .fetch_global_config(&state.global_config)
                    .await?;
                let price = RaydiumLaunchpad::sol_price_from_pool_state(&state, global.curve_type)?;
                let remaining_base_raw = state.total_base_sell.saturating_sub(state.real_base);
                let max_buy = Self::sol_capacity_from_token_reserve_raw(
                    remaining_base_raw,
                    state.base_decimals,
                    price,
                );
                RouteLiquiditySnapshot {
                    wsol_liquidity_raw: state.real_quote,
                    max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                }
            }
            Market::RaydiumClmm => {
                let state = self.raydium_clmm.fetch_state(pool).await?;
                let price = RaydiumClmm::price_from_sqrt_price_x64(&state)?;
                if state.mint_a == *mint && state.mint_b == crate::core::sol::WSOL_MINT {
                    let token_raw = self
                        .sol_hook
                        .get_token_balance_raw_from_ata(&state.vault_a)
                        .await?
                        .0;
                    let wsol_raw = self
                        .sol_hook
                        .get_token_balance_raw_from_ata(&state.vault_b)
                        .await?
                        .0;
                    let max_buy = Self::sol_capacity_from_token_reserve_raw(
                        token_raw,
                        state.mint_decimals_a,
                        price,
                    );
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: wsol_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else if state.mint_b == *mint && state.mint_a == crate::core::sol::WSOL_MINT {
                    let token_raw = self
                        .sol_hook
                        .get_token_balance_raw_from_ata(&state.vault_b)
                        .await?
                        .0;
                    let wsol_raw = self
                        .sol_hook
                        .get_token_balance_raw_from_ata(&state.vault_a)
                        .await?
                        .0;
                    let max_buy = Self::sol_capacity_from_token_reserve_raw(
                        token_raw,
                        state.mint_decimals_b,
                        price,
                    );
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: wsol_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else {
                    anyhow::bail!(
                        "raydium clmm pool does not contain mint {} as the non-WSOL side",
                        mint
                    );
                }
            }
            Market::RaydiumCpmm => {
                let state = self.raydium_cpmm.fetch_state(pool).await?;
                let vault_0_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&state.token_0_vault)
                    .await?
                    .0;
                let vault_1_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&state.token_1_vault)
                    .await?
                    .0;
                let fees_0 = state
                    .protocol_fees_token_0
                    .saturating_add(state.fund_fees_token_0)
                    .saturating_add(state.creator_fees_token_0);
                let fees_1 = state
                    .protocol_fees_token_1
                    .saturating_add(state.fund_fees_token_1)
                    .saturating_add(state.creator_fees_token_1);
                let vault_0_net = vault_0_raw.saturating_sub(fees_0);
                let vault_1_net = vault_1_raw.saturating_sub(fees_1);

                if state.token_0_mint == *mint && state.token_1_mint == crate::core::sol::WSOL_MINT
                {
                    let token_ui = vault_0_net as f64 / 10_f64.powi(state.mint_0_decimals as i32);
                    anyhow::ensure!(token_ui > 0.0, "raydium cpmm token reserve is zero");
                    let price = (vault_1_net as f64 / Self::LAMPORTS_PER_SOL) / token_ui;
                    let max_buy = Self::sol_capacity_from_token_reserve_raw(
                        vault_0_net,
                        state.mint_0_decimals,
                        price,
                    );
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: vault_1_net,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else if state.token_1_mint == *mint
                    && state.token_0_mint == crate::core::sol::WSOL_MINT
                {
                    let token_ui = vault_1_net as f64 / 10_f64.powi(state.mint_1_decimals as i32);
                    anyhow::ensure!(token_ui > 0.0, "raydium cpmm token reserve is zero");
                    let price = (vault_0_net as f64 / Self::LAMPORTS_PER_SOL) / token_ui;
                    let max_buy = Self::sol_capacity_from_token_reserve_raw(
                        vault_1_net,
                        state.mint_1_decimals,
                        price,
                    );
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: vault_0_net,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else {
                    anyhow::bail!(
                        "raydium cpmm pool does not contain mint {} as the non-WSOL side",
                        mint
                    );
                }
            }
            Market::MeteoraDlmm => {
                let state = self.meteora_dlmm.fetch_state(pool).await?;
                let reserve_x_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&state.reserve_x)
                    .await?
                    .0;
                let reserve_y_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&state.reserve_y)
                    .await?
                    .0;
                let decimals_x = self
                    .sol_hook
                    .get_token_decimals(&state.token_x_mint)
                    .await?;
                let decimals_y = self
                    .sol_hook
                    .get_token_decimals(&state.token_y_mint)
                    .await?;
                let price =
                    MeteoraDlmm::price_from_state_with_decimals(&state, decimals_x, decimals_y)?;
                if state.token_x_mint == *mint && state.token_y_mint == crate::core::sol::WSOL_MINT
                {
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(reserve_x_raw, decimals_x, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: reserve_y_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else if state.token_y_mint == *mint
                    && state.token_x_mint == crate::core::sol::WSOL_MINT
                {
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(reserve_y_raw, decimals_y, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: reserve_x_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else {
                    anyhow::bail!(
                        "meteora dlmm pool does not contain mint {} as the non-WSOL side",
                        mint
                    );
                }
            }
            Market::MeteoraDammV1 => {
                let state = self.meteora_damm_v1.fetch_state(pool).await?;
                let a_token_vault = Pubkey::find_program_address(
                    &[
                        crate::dex::meteora_damm_v1::TOKEN_VAULT_SEED,
                        state.a_vault.as_ref(),
                    ],
                    &crate::dex::meteora_damm_v1::METEORA_DYNAMIC_VAULT_ID,
                )
                .0;
                let b_token_vault = Pubkey::find_program_address(
                    &[
                        crate::dex::meteora_damm_v1::TOKEN_VAULT_SEED,
                        state.b_vault.as_ref(),
                    ],
                    &crate::dex::meteora_damm_v1::METEORA_DYNAMIC_VAULT_ID,
                )
                .0;
                let reserve_a_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&a_token_vault)
                    .await?
                    .0;
                let reserve_b_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&b_token_vault)
                    .await?
                    .0;
                let decimals_a = self
                    .sol_hook
                    .get_token_decimals(&state.token_a_mint)
                    .await?;
                let decimals_b = self
                    .sol_hook
                    .get_token_decimals(&state.token_b_mint)
                    .await?;
                let price = MeteoraDammV1::price_from_state_with_reserves_and_decimals(
                    &state,
                    reserve_a_raw,
                    reserve_b_raw,
                    decimals_a,
                    decimals_b,
                )?;
                if state.token_a_mint == *mint && state.token_b_mint == crate::core::sol::WSOL_MINT
                {
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(reserve_a_raw, decimals_a, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: reserve_b_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else if state.token_b_mint == *mint
                    && state.token_a_mint == crate::core::sol::WSOL_MINT
                {
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(reserve_b_raw, decimals_b, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: reserve_a_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else {
                    anyhow::bail!(
                        "meteora damm v1 pool does not contain mint {} as the non-WSOL side",
                        mint
                    );
                }
            }
            Market::MeteoraDammV2 => {
                let state = self.meteora_damm_v2.fetch_state(pool).await?;
                let reserve_a_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&state.token_a_vault)
                    .await?
                    .0;
                let reserve_b_raw = self
                    .sol_hook
                    .get_token_balance_raw_from_ata(&state.token_b_vault)
                    .await?
                    .0;
                let (_, price) = self.meteora_damm_v2.fetch_price(pool).await?;
                let decimals_a = self
                    .sol_hook
                    .get_token_decimals(&state.token_a_mint)
                    .await?;
                let decimals_b = self
                    .sol_hook
                    .get_token_decimals(&state.token_b_mint)
                    .await?;
                if state.token_a_mint == *mint && state.token_b_mint == crate::core::sol::WSOL_MINT
                {
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(reserve_a_raw, decimals_a, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: reserve_b_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else if state.token_b_mint == *mint
                    && state.token_a_mint == crate::core::sol::WSOL_MINT
                {
                    let max_buy =
                        Self::sol_capacity_from_token_reserve_raw(reserve_b_raw, decimals_b, price);
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: reserve_a_raw,
                        max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                    }
                } else {
                    anyhow::bail!(
                        "meteora damm v2 pool does not contain mint {} as the non-WSOL side",
                        mint
                    );
                }
            }
            Market::MeteoraDbc => {
                let state = self.meteora_dbc.fetch_state(pool).await?;
                anyhow::ensure!(
                    state.virtual_pool.base_mint == *mint
                        && state.config.quote_mint == crate::core::sol::WSOL_MINT,
                    "meteora dbc pool does not contain mint {} as the base mint with WSOL quote",
                    mint
                );
                let (_, price) = self.meteora_dbc.fetch_price(pool).await?;
                let max_buy = Self::sol_capacity_from_token_reserve_raw(
                    state.virtual_pool.base_reserve,
                    self.sol_hook.get_token_decimals(mint).await?,
                    price,
                );
                RouteLiquiditySnapshot {
                    wsol_liquidity_raw: state.virtual_pool.quote_reserve,
                    max_safe_buy_sol_raw: Self::apply_buy_capacity_safety_margin(max_buy),
                }
            }
        };

        Ok(snapshot)
    }

    async fn measure_market_candidates(
        &self,
        mint: &Pubkey,
        candidates: &[(Market, Pubkey)],
        candidate_filter_markets: Option<&[Market]>,
        measurement_timeout: Duration,
    ) -> Vec<MeasuredRouteCandidate> {
        let eligible_candidates = if let Some(candidate_filter_markets) =
            candidate_filter_markets.filter(|markets| !markets.is_empty())
        {
            let preferred_only = candidates
                .iter()
                .copied()
                .filter(|(market, _)| candidate_filter_markets.contains(market))
                .collect::<Vec<_>>();
            if preferred_only.is_empty() {
                candidates.to_vec()
            } else {
                preferred_only
            }
        } else {
            candidates.to_vec()
        };

        let measured = join_all(eligible_candidates.iter().copied().map(
            |(market, pool)| async move {
                let liquidity = match tokio::time::timeout(
                    measurement_timeout,
                    self.fetch_market_liquidity_snapshot(market, &pool, mint),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err(anyhow::anyhow!(
                        "route liquidity measurement timed out after {}ms",
                        measurement_timeout.as_millis()
                    )),
                };
                (market, pool, liquidity)
            },
        ))
        .await;

        let mut out = Vec::with_capacity(measured.len());
        for (market, pool, liquidity) in measured {
            let liquidity = match liquidity {
                Ok(liquidity) => liquidity,
                Err(error) => {
                    let timed_out = error
                        .to_string()
                        .contains("route liquidity measurement timed out");
                    warn!(
                        "failed to measure route liquidity for market {} pool {} mint {}: {}",
                        market.as_str(),
                        pool,
                        mint,
                        error
                    );
                    RouteLiquiditySnapshot {
                        wsol_liquidity_raw: if timed_out {
                            0
                        } else {
                            match tokio::time::timeout(
                                measurement_timeout,
                                self.fetch_market_wsol_liquidity_raw(market, &pool),
                            )
                            .await
                            {
                                Ok(Ok(value)) => value,
                                _ => 0,
                            }
                        },
                        max_safe_buy_sol_raw: 0,
                    }
                }
            };
            out.push(MeasuredRouteCandidate {
                market,
                pool,
                liquidity,
            });
        }

        out
    }

    async fn find_pool_for_market(
        &self,
        market: Market,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
        min_liquidity_raw: u64,
    ) -> anyhow::Result<Option<Pubkey>> {
        match market {
            Market::PumpSwap => {
                self.pump_swap
                    .find_pool_by_mint_with_min_liquidity(mint, quote_mint, min_liquidity_raw)
                    .await
            }
            Market::PumpFun => {
                self.pump_fun
                    .find_pool_by_mint_with_min_liquidity(mint, quote_mint, min_liquidity_raw)
                    .await
            }
            Market::RaydiumAmmV4 => {
                self.raydium_amm_v4
                    .find_pool_by_mint_with_min_liquidity(mint, quote_mint, min_liquidity_raw)
                    .await
            }
            Market::RaydiumLaunchpad => {
                self.raydium_launchpad
                    .find_pool_by_mint_with_min_liquidity(mint, quote_mint, min_liquidity_raw)
                    .await
            }
            Market::RaydiumClmm => {
                self.raydium_clmm
                    .find_pool_by_mint_with_min_liquidity(
                        mint,
                        quote_mint,
                        min_liquidity_raw as u128,
                    )
                    .await
            }
            Market::RaydiumCpmm => {
                self.raydium_cpmm
                    .find_pool_by_mint_with_min_liquidity(mint, quote_mint, min_liquidity_raw)
                    .await
            }
            Market::MeteoraDlmm => {
                self.meteora_dlmm
                    .find_pool_by_mint_with_min_liquidity(mint, quote_mint, min_liquidity_raw)
                    .await
            }
            Market::MeteoraDammV1 => {
                self.meteora_damm_v1
                    .find_pool_by_mint_with_min_liquidity(mint, quote_mint, min_liquidity_raw)
                    .await
            }
            Market::MeteoraDammV2 => {
                self.meteora_damm_v2
                    .find_pool_by_mint_with_min_liquidity(mint, quote_mint, min_liquidity_raw)
                    .await
            }
            Market::MeteoraDbc => {
                self.meteora_dbc
                    .find_pool_by_mint_with_min_liquidity(mint, quote_mint, min_liquidity_raw)
                    .await
            }
        }
    }

    async fn resolve_market_fallback_creator(
        &self,
        market: Market,
        pool: &Pubkey,
    ) -> anyhow::Result<Option<Pubkey>> {
        let creator = match market {
            Market::PumpSwap => self
                .pump_swap
                .fetch_state(pool)
                .await
                .map(|state| Pubkey::new_from_array(state.coin_creator.to_bytes()))
                .ok(),
            Market::PumpFun => self.pump_fun.get_creator(pool).await.ok(),
            Market::RaydiumAmmV4 => self
                .raydium_amm_v4
                .fetch_state(pool)
                .await
                .map(|state| state.owner)
                .ok(),
            Market::RaydiumLaunchpad => self
                .raydium_launchpad
                .fetch_state(pool)
                .await
                .map(|state| state.creator)
                .ok(),
            Market::RaydiumClmm => self
                .raydium_clmm
                .fetch_state(pool)
                .await
                .map(|state| state.owner)
                .ok(),
            Market::RaydiumCpmm => self
                .raydium_cpmm
                .fetch_state(pool)
                .await
                .map(|state| state.pool_creator)
                .ok(),
            Market::MeteoraDlmm => self
                .meteora_dlmm
                .fetch_state(pool)
                .await
                .map(|state| state.creator)
                .ok(),
            Market::MeteoraDammV1 => None,
            Market::MeteoraDammV2 => None,
            Market::MeteoraDbc => self
                .meteora_dbc
                .fetch_state(pool)
                .await
                .map(|state| state.virtual_pool.creator)
                .ok(),
        };
        Ok(creator)
    }

    pub async fn get_mint_creator_for_market_pool(
        &self,
        mint: &Pubkey,
        market: Market,
        pool: &Pubkey,
    ) -> anyhow::Result<MintCreatorResolution> {
        match tokio::time::timeout(
            std::time::Duration::from_millis(Self::METADATA_CREATOR_LOOKUP_TIMEOUT_MS),
            self.sol_hook.get_mint_first_creator(mint),
        )
        .await
        {
            Ok(Ok(Some(creator))) => {
                return Ok(MintCreatorResolution {
                    creator,
                    source: CreatorResolutionSource::MetadataFirstCreator,
                });
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                warn!(
                    "metadata creator lookup failed for mint {}: {}",
                    mint, error
                );
            }
            Err(_) => {
                warn!(
                    "metadata creator lookup timed out for mint {} after {}ms",
                    mint,
                    Self::METADATA_CREATOR_LOOKUP_TIMEOUT_MS
                );
            }
        }

        if let Some(creator) = self.resolve_market_fallback_creator(market, pool).await? {
            return Ok(MintCreatorResolution {
                creator,
                source: CreatorResolutionSource::MarketStateFallback,
            });
        }

        Ok(MintCreatorResolution {
            creator: Pubkey::default(),
            source: CreatorResolutionSource::Unresolved,
        })
    }

    pub async fn get_route_creator_for_market_pool(
        &self,
        mint: &Pubkey,
        market: Market,
        pool: &Pubkey,
    ) -> anyhow::Result<MintCreatorResolution> {
        if let Some(creator) = self.resolve_market_fallback_creator(market, pool).await? {
            if market == Market::PumpSwap || creator != Pubkey::default() {
                return Ok(MintCreatorResolution {
                    creator,
                    source: CreatorResolutionSource::MarketStateFallback,
                });
            }
        }

        self.get_mint_creator_for_market_pool(mint, market, pool)
            .await
    }

    async fn build_creator_route_selection(
        &self,
        mint: &Pubkey,
        market: Market,
        pool: Pubkey,
        liquidity: RouteLiquiditySnapshot,
        warning: Option<String>,
        prefer_metadata_creator_first: bool,
    ) -> anyhow::Result<MintCreatorRouteSelection> {
        let creator = (if prefer_metadata_creator_first {
            let creator = self
                .get_route_creator_for_market_pool(mint, market, &pool)
                .await?;
            if creator.source == CreatorResolutionSource::MarketStateFallback {
                self.get_mint_creator_for_market_pool(mint, market, &pool)
                    .await
            } else {
                Ok(creator)
            }
        } else {
            self.get_route_creator_for_market_pool(mint, market, &pool)
                .await
        })
        .with_context(|| {
            format!(
                "failed to resolve mint creator for market {} mint {} pool {}",
                market.as_str(),
                mint,
                pool
            )
        })?;

        let low_lq = Self::route_is_low_lq(market, &liquidity);
        let warning = Self::append_route_warning(
            warning,
            Self::low_lq_warning_for_market_pool(market, &pool, &liquidity),
        );

        Ok(MintCreatorRouteSelection {
            route: MintCreatorRoute {
                market,
                pool,
                creator: creator.creator,
                source: creator.source,
            },
            liquidity,
            low_lq,
            warning,
        })
    }

    pub async fn get_mint_creator_selection_with_market_priority(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        market_priority: Option<&[Market]>,
        prefer_metadata_creator_first: bool,
    ) -> anyhow::Result<Option<MintCreatorRouteSelection>> {
        let mint_literal = mint.trim().to_string();
        let mint = Self::parse_pubkey_input("mint", mint)?;
        let quote_mint =
            Self::parse_optional_pubkey_input("quote_mint", quote_mint.map(|v| v.as_str()))?;
        let mut requested_markets = market_priority
            .map(|markets| markets.to_vec())
            .unwrap_or_else(|| Self::default_market_priority().to_vec());
        let explicit_single_market = market_priority.is_some_and(|markets| markets.len() <= 1);
        let family_markets = if explicit_single_market {
            Vec::new()
        } else {
            Self::preferred_route_markets_from_mint_family(&mint_literal, &requested_markets)
        };
        if !family_markets.is_empty() {
            let scoped_markets = requested_markets
                .iter()
                .copied()
                .filter(|market| family_markets.contains(market))
                .collect::<Vec<_>>();
            if !scoped_markets.is_empty() {
                requested_markets = scoped_markets;
            }
        }
        let deadline =
            Instant::now() + Self::route_lookup_timeout_duration(Some(&requested_markets));
        let mut restrict_candidates_to_preferred_markets = false;
        let preferred_markets = if explicit_single_market {
            Vec::new()
        } else if !family_markets.is_empty() {
            family_markets
        } else {
            let detected_markets = self
                .detect_preferred_route_markets_before_deadline(
                    &mint,
                    quote_mint.as_ref(),
                    deadline,
                )
                .await;
            restrict_candidates_to_preferred_markets = !detected_markets.is_empty();
            detected_markets
        };
        let ordered_markets = Self::prioritize_markets(&requested_markets, &preferred_markets);
        let market_groups = Self::market_lookup_groups_for_priority(&ordered_markets);
        let mut best_relaxed_candidate: Option<MeasuredRouteCandidate> = None;
        let mut best_last_resort_candidate: Option<MeasuredRouteCandidate> = None;
        let mut best_last_resort_low_lq_candidate: Option<MeasuredRouteCandidate> = None;
        let mut best_last_resort_relaxed_candidate: Option<MeasuredRouteCandidate> = None;
        let candidate_filter_markets =
            restrict_candidates_to_preferred_markets.then_some(preferred_markets.as_slice());

        for (group_idx, markets) in market_groups.iter().enumerate() {
            let group_timeout = Self::allocate_group_route_lookup_timeout(
                deadline,
                market_groups.len() - group_idx,
            );
            if group_timeout.is_zero() {
                break;
            }
            let pool_results = join_all(markets.iter().copied().map(|market| async move {
                let result = tokio::time::timeout(
                    group_timeout,
                    self.find_pool_for_market(market, &mint, quote_mint.as_ref(), 0),
                )
                .await;
                (market, result)
            }))
            .await;

            let mut candidates = Vec::new();
            for (market, lookup) in pool_results {
                match lookup {
                    Ok(Ok(Some(pool))) => candidates.push((market, pool)),
                    Ok(Ok(None)) => continue,
                    Ok(Err(error)) => {
                        warn!(
                            "{} mint route failed for {}: {}",
                            market.as_str(),
                            mint,
                            error
                        );
                        continue;
                    }
                    Err(_) => {
                        warn!(
                            "{} mint route timed out for {} after {}ms",
                            market.as_str(),
                            mint,
                            group_timeout.as_millis()
                        );
                        continue;
                    }
                }
            }

            if candidates.is_empty() {
                continue;
            }

            let measurement_timeout = Self::allocate_route_liquidity_lookup_timeout(deadline);
            if measurement_timeout.is_zero() {
                break;
            }
            let measured = self
                .measure_market_candidates(
                    &mint,
                    &candidates,
                    candidate_filter_markets,
                    measurement_timeout,
                )
                .await;
            if measured.is_empty() {
                continue;
            }

            let mut standard_measured = Vec::new();
            let mut last_resort_measured = Vec::new();
            for candidate in measured {
                if ordered_markets.len() > 1 && Self::is_last_resort_lookup_market(candidate.market)
                {
                    last_resort_measured.push(candidate);
                } else {
                    standard_measured.push(candidate);
                }
            }

            if !last_resort_measured.is_empty() {
                let matching = last_resort_measured
                    .iter()
                    .copied()
                    .filter(|candidate| {
                        Self::candidate_capacity_raw(&candidate.liquidity) >= min_liquidity_raw
                    })
                    .collect::<Vec<_>>();
                let matching_without_low_lq = matching
                    .iter()
                    .copied()
                    .filter(|candidate| {
                        !Self::route_is_low_lq(candidate.market, &candidate.liquidity)
                    })
                    .collect::<Vec<_>>();

                if let Some(best) = Self::choose_best_measured_candidate(&matching_without_low_lq) {
                    Self::update_best_measured_candidate(&mut best_last_resort_candidate, best);
                } else if let Some(low_lq) = Self::choose_best_measured_candidate(&matching) {
                    Self::update_best_measured_candidate(
                        &mut best_last_resort_low_lq_candidate,
                        low_lq,
                    );
                } else if let Some(relaxed) =
                    Self::choose_best_measured_candidate(&last_resort_measured)
                {
                    Self::update_best_measured_candidate(
                        &mut best_last_resort_relaxed_candidate,
                        relaxed,
                    );
                }
            }

            if standard_measured.is_empty() {
                continue;
            }

            let matching = standard_measured
                .iter()
                .copied()
                .filter(|candidate| {
                    Self::candidate_capacity_raw(&candidate.liquidity) >= min_liquidity_raw
                })
                .collect::<Vec<_>>();

            let matching_without_low_lq = matching
                .iter()
                .copied()
                .filter(|candidate| !Self::route_is_low_lq(candidate.market, &candidate.liquidity))
                .collect::<Vec<_>>();

            if let Some(best) = Self::choose_best_measured_candidate(&matching_without_low_lq) {
                return Ok(Some(
                    self.build_creator_route_selection(
                        &mint,
                        best.market,
                        best.pool,
                        best.liquidity,
                        None,
                        prefer_metadata_creator_first,
                    )
                    .await?,
                ));
            }

            if let Some(low_lq_candidate) = Self::choose_best_measured_candidate(&matching) {
                return Ok(Some(
                    self.build_creator_route_selection(
                        &mint,
                        low_lq_candidate.market,
                        low_lq_candidate.pool,
                        low_lq_candidate.liquidity,
                        None,
                        prefer_metadata_creator_first,
                    )
                    .await?,
                ));
            }

            let Some(relaxed) = Self::choose_best_measured_candidate(&standard_measured) else {
                continue;
            };
            Self::update_best_measured_candidate(&mut best_relaxed_candidate, relaxed);
        }

        if let Some(relaxed) = best_relaxed_candidate {
            return Ok(Some(
                self.build_creator_route_selection(
                    &mint,
                    relaxed.market,
                    relaxed.pool,
                    relaxed.liquidity,
                    Some(Self::relaxed_liquidity_warning(
                        &mint,
                        &relaxed,
                        min_liquidity_raw,
                    )),
                    prefer_metadata_creator_first,
                )
                .await?,
            ));
        }

        if let Some(last_resort) = best_last_resort_candidate {
            return Ok(Some(
                self.build_creator_route_selection(
                    &mint,
                    last_resort.market,
                    last_resort.pool,
                    last_resort.liquidity,
                    None,
                    prefer_metadata_creator_first,
                )
                .await?,
            ));
        }

        if let Some(last_resort_low_lq) = best_last_resort_low_lq_candidate {
            return Ok(Some(
                self.build_creator_route_selection(
                    &mint,
                    last_resort_low_lq.market,
                    last_resort_low_lq.pool,
                    last_resort_low_lq.liquidity,
                    None,
                    prefer_metadata_creator_first,
                )
                .await?,
            ));
        }

        if let Some(last_resort_relaxed) = best_last_resort_relaxed_candidate {
            return Ok(Some(
                self.build_creator_route_selection(
                    &mint,
                    last_resort_relaxed.market,
                    last_resort_relaxed.pool,
                    last_resort_relaxed.liquidity,
                    Some(Self::relaxed_liquidity_warning(
                        &mint,
                        &last_resort_relaxed,
                        min_liquidity_raw,
                    )),
                    prefer_metadata_creator_first,
                )
                .await?,
            ));
        }

        Ok(None)
    }

    pub async fn get_mint_creator_with_market_priority(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        market_priority: Option<&[Market]>,
        prefer_metadata_creator_first: bool,
    ) -> anyhow::Result<Option<MintCreatorRoute>> {
        Ok(self
            .get_mint_creator_selection_with_market_priority(
                mint,
                quote_mint,
                min_liquidity_raw,
                market_priority,
                prefer_metadata_creator_first,
            )
            .await?
            .map(|selection| selection.route))
    }

    pub async fn get_mint_creator(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
    ) -> anyhow::Result<Option<MintCreatorRoute>> {
        self.get_mint_creator_with_market_priority(mint, quote_mint, min_liquidity_raw, None, true)
            .await
    }

    pub async fn find_pool_for_mint_with_market_priority(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        market_priority: Option<&[Market]>,
    ) -> anyhow::Result<Option<MintPoolRoute>> {
        let Some(selection) = self
            .find_pool_selection_for_mint_with_market_priority(
                mint,
                quote_mint,
                min_liquidity_raw,
                market_priority,
            )
            .await?
        else {
            return Ok(None);
        };

        Ok(Some(selection.route))
    }

    pub async fn find_pool_selection_for_mint_with_market_priority(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        market_priority: Option<&[Market]>,
    ) -> anyhow::Result<Option<MintPoolRouteSelection>> {
        let Some(selection) = self
            .get_mint_creator_selection_with_market_priority(
                mint,
                quote_mint,
                min_liquidity_raw,
                market_priority,
                false,
            )
            .await?
        else {
            return Ok(None);
        };

        Ok(Some(MintPoolRouteSelection {
            route: MintPoolRoute {
                market: selection.route.market,
                pool: selection.route.pool,
                creator: selection.route.creator,
            },
            liquidity: selection.liquidity,
            low_lq: selection.low_lq,
            warning: selection.warning,
        }))
    }

    pub async fn fetch_price_for_market_pool(
        &self,
        market: Market,
        pool: &Pubkey,
    ) -> anyhow::Result<f64> {
        let price = match market {
            Market::PumpSwap => self.pump_swap.fetch_price(pool).await?.1,
            Market::PumpFun => self.pump_fun.fetch_price(pool).await?.1,
            Market::RaydiumAmmV4 => self.raydium_amm_v4.fetch_price(pool).await?.1,
            Market::RaydiumLaunchpad => self.raydium_launchpad.fetch_price(pool).await?.1,
            Market::RaydiumClmm => self.raydium_clmm.fetch_price(pool).await?.1,
            Market::RaydiumCpmm => self.raydium_cpmm.fetch_price(pool).await?.1,
            Market::MeteoraDlmm => self.meteora_dlmm.fetch_price(pool).await?.1,
            Market::MeteoraDammV1 => self.meteora_damm_v1.fetch_price(pool).await?.1,
            Market::MeteoraDammV2 => self.meteora_damm_v2.fetch_price(pool).await?.1,
            Market::MeteoraDbc => self.meteora_dbc.fetch_price(pool).await?.1,
        };
        anyhow::ensure!(
            price.is_finite() && price > 0.0,
            "invalid {:?} price for pool {}: {}",
            market,
            pool,
            price
        );
        Ok(price)
    }

    pub async fn measure_route_liquidity_for_market_pool(
        &self,
        mint: &String,
        market: Market,
        pool: &String,
    ) -> anyhow::Result<RouteLiquiditySnapshot> {
        let mint = Self::parse_pubkey_input("mint", mint)?;
        let pool = Self::parse_pubkey_input("pool", pool)?;
        self.fetch_market_liquidity_snapshot(market, &pool, &mint)
            .await
    }

    pub async fn find_pool_and_price_for_mint_with_market_priority(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        market_priority: Option<&[Market]>,
    ) -> anyhow::Result<Option<(MintPoolRoute, f64)>> {
        let Some(route) = self
            .find_pool_for_mint_with_market_priority_timed(
                mint,
                quote_mint,
                min_liquidity_raw,
                market_priority,
            )
            .await?
        else {
            return Ok(None);
        };

        let price = self
            .fetch_price_for_market_pool_timed(route.market, &route.pool)
            .await
            .with_context(|| {
                format!(
                    "failed to fetch price for mint route market {:?} pool {}",
                    route.market, route.pool
                )
            })?;

        Ok(Some((route, price)))
    }

    pub async fn buy_by_mint_with_market_priority(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        sol_amount_in: f64,
        slippage: f64,
        use_idempotent: Option<bool>,
        market_priority: Option<&[Market]>,
        use_swqos: bool,
        swqos_settings: Option<SWQoSettings>,
    ) -> anyhow::Result<(SwapExecutionResult, MintPoolRoute, f64)> {
        self.buy_by_mint_with_priority_fee_override(
            mint,
            quote_mint,
            min_liquidity_raw,
            sol_amount_in,
            slippage,
            use_idempotent,
            market_priority,
            None,
            use_swqos,
            swqos_settings,
        )
        .await
    }

    pub async fn buy_by_mint_with_priority_fee_override(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        sol_amount_in: f64,
        slippage: f64,
        use_idempotent: Option<bool>,
        market_priority: Option<&[Market]>,
        priority_fee_override: Option<PriorityFeeOverride>,
        use_swqos: bool,
        swqos_settings: Option<SWQoSettings>,
    ) -> anyhow::Result<(SwapExecutionResult, MintPoolRoute, f64)> {
        let required_capacity_raw = min_liquidity_raw.max(
            ((sol_amount_in.max(0.0) * Self::LAMPORTS_PER_SOL)
                * (1.0
                    + if slippage > 1.0 {
                        slippage / 100.0
                    } else {
                        slippage
                    }))
            .ceil()
            .clamp(0.0, u64::MAX as f64) as u64,
        );
        let route = self
            .find_pool_for_mint_with_market_priority_timed(
                mint,
                quote_mint,
                required_capacity_raw,
                market_priority,
            )
            .await?
            .context("no eligible pool route found for mint")?;
        let price = match self
            .fetch_price_for_market_pool_timed(route.market, &route.pool)
            .await
        {
            Ok(price) => price,
            Err(error) if route.market == Market::PumpSwap => {
                warn!(
                    "pump.swap buy price lookup failed for pool {}: {}; continuing with exact-quote buy builder",
                    route.pool, error
                );
                0.0
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to fetch price for mint route market {:?} pool {}",
                        route.market, route.pool
                    )
                });
            }
        };

        let execution = self
            .buy_with_priority_fee_override(
                mint,
                &route.pool.to_string(),
                &route.creator.to_string(),
                sol_amount_in,
                slippage,
                price,
                use_idempotent,
                route.market,
                priority_fee_override,
                use_swqos,
                swqos_settings,
            )
            .await?;

        Ok((execution, route, price))
    }

    pub async fn sell_by_mint_with_market_priority(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        sell_pct: u64,
        slippage: f64,
        market_priority: Option<&[Market]>,
        retries: u32,
        use_swqos: bool,
        swqos_settings: Option<SWQoSettings>,
    ) -> anyhow::Result<(SwapExecutionResult, MintPoolRoute, f64)> {
        self.sell_by_mint_with_priority_fee_override(
            mint,
            quote_mint,
            min_liquidity_raw,
            sell_pct,
            slippage,
            market_priority,
            retries,
            None,
            use_swqos,
            swqos_settings,
        )
        .await
    }

    pub async fn sell_by_mint_with_priority_fee_override(
        &self,
        mint: &String,
        quote_mint: Option<&String>,
        min_liquidity_raw: u64,
        sell_pct: u64,
        slippage: f64,
        market_priority: Option<&[Market]>,
        retries: u32,
        priority_fee_override: Option<PriorityFeeOverride>,
        use_swqos: bool,
        swqos_settings: Option<SWQoSettings>,
    ) -> anyhow::Result<(SwapExecutionResult, MintPoolRoute, f64)> {
        let (route, price) = self
            .find_pool_and_price_for_mint_with_market_priority(
                mint,
                quote_mint,
                min_liquidity_raw,
                market_priority,
            )
            .await?
            .context("no eligible pool route found for mint")?;

        let execution = self
            .sell_with_priority_fee_override(
                mint,
                &route.pool.to_string(),
                &route.creator.to_string(),
                sell_pct,
                slippage,
                price,
                route.market,
                retries,
                priority_fee_override,
                use_swqos,
                swqos_settings,
            )
            .await?;

        Ok((execution, route, price))
    }

    pub async fn build_buy_instructions_for_user(
        &self,
        buyer: &Pubkey,
        mint: &Pubkey,
        current_pool: &Pubkey,
        creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        price: f64,
        use_idempotent: Option<bool>,
        market: Market,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        match market {
            Market::PumpSwap => {
                self.pump_swap
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
            Market::PumpFun => {
                self.pump_fun
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
            Market::RaydiumAmmV4 => {
                self.raydium_amm_v4
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
            Market::RaydiumLaunchpad => {
                self.raydium_launchpad
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
            Market::RaydiumClmm => {
                self.raydium_clmm
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
            Market::RaydiumCpmm => {
                self.raydium_cpmm
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
            Market::MeteoraDlmm => {
                self.meteora_dlmm
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
            Market::MeteoraDammV1 => {
                self.meteora_damm_v1
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
            Market::MeteoraDammV2 => {
                self.meteora_damm_v2
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
            Market::MeteoraDbc => {
                self.meteora_dbc
                    .buy_for_user(
                        buyer,
                        mint,
                        current_pool,
                        creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                    )
                    .await
            }
        }
    }

    pub async fn build_sell_instructions_for_user(
        &self,
        seller: &Pubkey,
        mint: &Pubkey,
        current_pool: &Pubkey,
        creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        price: f64,
        market: Market,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        match market {
            Market::PumpSwap => {
                self.pump_swap
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
            Market::PumpFun => {
                self.pump_fun
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
            Market::RaydiumAmmV4 => {
                self.raydium_amm_v4
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
            Market::RaydiumLaunchpad => {
                self.raydium_launchpad
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
            Market::RaydiumClmm => {
                self.raydium_clmm
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
            Market::RaydiumCpmm => {
                self.raydium_cpmm
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
            Market::MeteoraDlmm => {
                self.meteora_dlmm
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
            Market::MeteoraDammV1 => {
                self.meteora_damm_v1
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
            Market::MeteoraDammV2 => {
                self.meteora_damm_v2
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
            Market::MeteoraDbc => {
                self.meteora_dbc
                    .sell_for_user(
                        seller,
                        mint,
                        current_pool,
                        creator,
                        sell_pct,
                        slippage,
                        price,
                    )
                    .await
            }
        }
    }

    pub async fn buy(
        &self,
        mint: &String,
        current_pool: &String,
        creator: &String,
        sol_amount_in: f64,
        slippage: f64,
        price: f64,
        use_idempotent: Option<bool>,
        market: Market,
        use_swqos: bool,
        swqos_settings: Option<SWQoSettings>,
    ) -> anyhow::Result<SwapExecutionResult> {
        self.buy_with_priority_fee_override(
            mint,
            current_pool,
            creator,
            sol_amount_in,
            slippage,
            price,
            use_idempotent,
            market,
            None,
            use_swqos,
            swqos_settings,
        )
        .await
    }

    pub async fn buy_with_priority_fee_override(
        &self,
        mint: &String,
        current_pool: &String,
        creator: &String,
        sol_amount_in: f64,
        slippage: f64,
        price: f64,
        use_idempotent: Option<bool>,
        market: Market,
        priority_fee_override: Option<PriorityFeeOverride>,
        use_swqos: bool,
        swqos_settings: Option<SWQoSettings>,
    ) -> anyhow::Result<SwapExecutionResult> {
        let mint = Self::parse_pubkey_input("mint", mint)?;
        let current_pool = Self::parse_pubkey_input("current_pool", current_pool)?;
        let creator = Self::parse_pubkey_input("creator", creator)?;

        let (mut ixs, fee) = match market {
            Market::PumpSwap => {
                self.pump_swap
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::PumpFun => {
                self.pump_fun
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::RaydiumAmmV4 => {
                self.raydium_amm_v4
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::RaydiumLaunchpad => {
                self.raydium_launchpad
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::RaydiumClmm => {
                self.raydium_clmm
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::RaydiumCpmm => {
                self.raydium_cpmm
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::MeteoraDlmm => {
                self.meteora_dlmm
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::MeteoraDammV1 => {
                self.meteora_damm_v1
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::MeteoraDammV2 => {
                self.meteora_damm_v2
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::MeteoraDbc => {
                self.meteora_dbc
                    .buy_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sol_amount_in,
                        slippage,
                        price,
                        use_idempotent,
                        priority_fee_override,
                    )
                    .await?
            }
        };

        if Self::buy_needs_shared_wsol_cleanup(market) {
            let buyer = self.pump_fun.keypair.pubkey();
            let wsol_program = self
                .sol_hook
                .get_token_program_id(&crate::core::sol::WSOL_MINT)
                .await
                .context("failed to resolve WSOL token program for buy cleanup")?;
            let wsol_ata = if wsol_program == crate::core::sol::TOKEN_PROGRAM_ID {
                self.sol_hook
                    .get_ata_for_token(&buyer, &crate::core::sol::WSOL_MINT)
            } else if wsol_program == crate::core::sol::TOKEN_2022_PROGRAM_ID {
                self.sol_hook
                    .get_ata_for_token2022(&buyer, &crate::core::sol::WSOL_MINT)
            } else {
                anyhow::bail!(
                    "unsupported token program for WSOL buy cleanup: {}",
                    wsol_program
                );
            };
            let close_wsol_ix = self
                .sol_hook
                .close_token_account_ix(&wsol_program, &wsol_ata, &buyer, &buyer)
                .context("failed to build WSOL close instruction for buy cleanup")?;
            ixs.push(close_wsol_ix);
        }

        let tx = if use_swqos {
            let settings = swqos_settings
                .as_ref()
                .context("swqos_settings is required when use_swqos=true")?;
            self.sol_hook
                .send_with_swqos(ixs, &self.pump_fun.keypair, fee, None, settings)
                .await
        } else {
            self.sol_hook
                .send(ixs, &self.pump_fun.keypair, fee, None)
                .await
        };
        match tx {
            Ok(sig) => {
                log!(cc::LIGHT_WHITE, "Sig: https://solscan.io/tx/{:?}", sig);
                Ok(SwapExecutionResult {
                    success: true,
                    signature: Some(sig),
                    error: None,
                })
            }
            Err(e) => {
                warn!("Error sending tx: {e}");
                Ok(SwapExecutionResult {
                    success: false,
                    signature: None,
                    error: Some(format!("{e:#}")),
                })
            }
        }
    }

    pub async fn sell(
        &self,
        mint: &String,
        current_pool: &String,
        creator: &String,
        sell_pct: u64,
        slippage: f64,
        price: f64,
        market: Market,
        retries: u32,
        use_swqos: bool,
        swqos_settings: Option<SWQoSettings>,
    ) -> anyhow::Result<SwapExecutionResult> {
        self.sell_with_priority_fee_override(
            mint,
            current_pool,
            creator,
            sell_pct,
            slippage,
            price,
            market,
            retries,
            None,
            use_swqos,
            swqos_settings,
        )
        .await
    }

    pub async fn sell_with_priority_fee_override(
        &self,
        mint: &String,
        current_pool: &String,
        creator: &String,
        sell_pct: u64,
        slippage: f64,
        price: f64,
        market: Market,
        retries: u32,
        priority_fee_override: Option<PriorityFeeOverride>,
        use_swqos: bool,
        swqos_settings: Option<SWQoSettings>,
    ) -> anyhow::Result<SwapExecutionResult> {
        let mint = Self::parse_pubkey_input("mint", mint)?;
        let current_pool = Self::parse_pubkey_input("current_pool", current_pool)?;
        let creator = Self::parse_pubkey_input("creator", creator)?;

        let (ixs, fee) = match market {
            Market::PumpSwap => {
                self.pump_swap
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::PumpFun => {
                self.pump_fun
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::RaydiumAmmV4 => {
                self.raydium_amm_v4
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::RaydiumLaunchpad => {
                self.raydium_launchpad
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::RaydiumClmm => {
                self.raydium_clmm
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::RaydiumCpmm => {
                self.raydium_cpmm
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::MeteoraDlmm => {
                self.meteora_dlmm
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::MeteoraDammV1 => {
                self.meteora_damm_v1
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::MeteoraDammV2 => {
                self.meteora_damm_v2
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
            Market::MeteoraDbc => {
                self.meteora_dbc
                    .sell_with_priority_fee_override(
                        &mint,
                        &current_pool,
                        &creator,
                        sell_pct,
                        slippage,
                        price,
                        priority_fee_override,
                    )
                    .await?
            }
        };
        let mut last_error: Option<String> = None;
        for attempt in 0..=retries {
            let tx = if use_swqos {
                let settings = swqos_settings
                    .as_ref()
                    .context("swqos_settings is required when use_swqos=true")?;
                self.sol_hook
                    .send_with_swqos(ixs.clone(), &self.pump_fun.keypair, fee, None, settings)
                    .await
            } else {
                self.sol_hook
                    .send(ixs.clone(), &self.pump_fun.keypair, fee, None)
                    .await
            };
            match tx {
                Ok(sig) => {
                    log!(cc::LIGHT_WHITE, "Sig: https://solscan.io/tx/{:?}", sig);
                    return Ok(SwapExecutionResult {
                        success: true,
                        signature: Some(sig),
                        error: None,
                    });
                }
                Err(e) => {
                    let rendered = format!("{e:#}");
                    let retryable = Self::trade_send_error_is_retryable(&rendered);
                    last_error = Some(rendered);
                    warn!(
                        "Error sending tx (attempt {}/{}): {e}",
                        attempt + 1,
                        retries + 1
                    );
                    if !retryable {
                        break;
                    }
                }
            }
        }
        Ok(SwapExecutionResult {
            success: false,
            signature: None,
            error: Some(last_error.unwrap_or_else(|| {
                "swap execution failed without a send/confirm error message".to_string()
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_parse_pubkey_input_valid() {
        let pk = Swaps::parse_pubkey_input("mint", "So11111111111111111111111111111111111111112")
            .expect("valid pubkey must parse");
        assert_eq!(pk, crate::core::sol::WSOL_MINT);
    }

    #[test]
    fn test_parse_pubkey_input_invalid() {
        let err = Swaps::parse_pubkey_input("mint", "not-a-pubkey").unwrap_err();
        assert!(err.to_string().contains("invalid mint pubkey"));
    }

    #[test]
    fn test_parse_optional_pubkey_input_handles_none_and_empty() {
        let none =
            Swaps::parse_optional_pubkey_input("nonce_account", None).expect("None is accepted");
        assert!(none.is_none());

        let empty = Swaps::parse_optional_pubkey_input("nonce_account", Some("   "))
            .expect("empty string is accepted as none");
        assert!(empty.is_none());
    }

    #[test]
    fn test_parse_optional_pubkey_input_invalid() {
        let err = Swaps::parse_optional_pubkey_input("nonce_account", Some("nope")).unwrap_err();
        assert!(err.to_string().contains("invalid nonce_account pubkey"));
    }

    #[test]
    fn test_default_market_priority_contains_all_markets_once() {
        let mut seen = HashSet::new();
        for market in Swaps::default_market_priority() {
            assert!(
                seen.insert(*market),
                "duplicate market in default route priority"
            );
        }

        assert_eq!(seen.len(), 10);
        assert!(seen.contains(&Market::PumpSwap));
        assert!(seen.contains(&Market::PumpFun));
        assert!(seen.contains(&Market::RaydiumAmmV4));
        assert!(seen.contains(&Market::RaydiumLaunchpad));
        assert!(seen.contains(&Market::RaydiumClmm));
        assert!(seen.contains(&Market::RaydiumCpmm));
        assert!(seen.contains(&Market::MeteoraDlmm));
        assert!(seen.contains(&Market::MeteoraDammV1));
        assert!(seen.contains(&Market::MeteoraDammV2));
        assert!(seen.contains(&Market::MeteoraDbc));
    }

    #[test]
    fn test_trade_send_error_is_retryable_distinguishes_transport_and_program_failures() {
        assert!(Swaps::trade_send_error_is_retryable(
            "trade send/confirm timed out after 60s"
        ));
        assert!(Swaps::trade_send_error_is_retryable(
            "Transaction failed to confirm sig | didn't land on-chain in 60s"
        ));
        assert!(!Swaps::trade_send_error_is_retryable(
            "Transaction failed sig | status err: InstructionError(1, Custom(6024))"
        ));
    }

    #[test]
    fn test_route_is_low_lq_uses_10_sol_threshold_for_non_exempt_markets() {
        assert!(Swaps::route_is_low_lq(
            Market::PumpSwap,
            &RouteLiquiditySnapshot {
                wsol_liquidity_raw: 9_999_999_999,
                max_safe_buy_sol_raw: 0,
            }
        ));
        assert!(!Swaps::route_is_low_lq(
            Market::PumpSwap,
            &RouteLiquiditySnapshot {
                wsol_liquidity_raw: Swaps::low_lq_wsol_threshold_raw(),
                max_safe_buy_sol_raw: 0,
            }
        ));
    }

    #[test]
    fn test_route_is_low_lq_skips_exempt_markets() {
        let liquidity = RouteLiquiditySnapshot {
            wsol_liquidity_raw: 1,
            max_safe_buy_sol_raw: 0,
        };

        assert!(!Swaps::route_is_low_lq(Market::PumpFun, &liquidity));
        assert!(!Swaps::route_is_low_lq(
            Market::RaydiumLaunchpad,
            &liquidity
        ));
        assert!(!Swaps::route_is_low_lq(Market::MeteoraDbc, &liquidity));
    }

    #[test]
    fn test_default_market_priority_order_is_stable() {
        assert_eq!(Swaps::default_market_priority(), &DEFAULT_MARKET_PRIORITY);
        assert_eq!(
            Swaps::default_market_priority(),
            &[
                Market::PumpSwap,
                Market::PumpFun,
                Market::RaydiumAmmV4,
                Market::RaydiumLaunchpad,
                Market::RaydiumCpmm,
                Market::MeteoraDammV1,
                Market::MeteoraDammV2,
                Market::MeteoraDbc,
                Market::RaydiumClmm,
                Market::MeteoraDlmm,
            ]
        );
    }

    #[test]
    fn test_default_market_lookup_groups_search_dlmm_primary_and_defer_clmm() {
        let groups = Swaps::default_market_lookup_groups();
        assert_eq!(groups[0], &DEFAULT_MARKET_PRIMARY_LOOKUP);
        assert_eq!(groups[1], &DEFAULT_MARKET_DEFERRED_LOOKUP);
        assert!(!groups[0].contains(&Market::RaydiumClmm));
        assert!(groups[0].contains(&Market::MeteoraDlmm));
        assert_eq!(groups[1], &[Market::RaydiumClmm]);
        assert!(Swaps::is_last_resort_lookup_market(Market::MeteoraDlmm));
    }

    #[test]
    fn test_explicit_market_priority_searches_dlmm_with_primary_and_defers_clmm() {
        let groups = Swaps::market_lookup_groups_for_priority(&[
            Market::RaydiumClmm,
            Market::PumpSwap,
            Market::MeteoraDlmm,
            Market::PumpFun,
        ]);
        assert_eq!(
            groups,
            vec![
                vec![Market::PumpSwap, Market::MeteoraDlmm, Market::PumpFun],
                vec![Market::RaydiumClmm],
            ]
        );
    }

    #[test]
    fn test_single_deferred_market_priority_remains_eligible() {
        let groups = Swaps::market_lookup_groups_for_priority(&[Market::RaydiumClmm]);
        assert_eq!(groups, vec![vec![Market::RaydiumClmm]]);
    }

    #[test]
    fn test_buy_shared_wsol_cleanup_skips_native_sol_pump_fun() {
        assert!(!Swaps::buy_needs_shared_wsol_cleanup(Market::PumpFun));
        assert!(Swaps::buy_needs_shared_wsol_cleanup(Market::PumpSwap));
        assert!(Swaps::buy_needs_shared_wsol_cleanup(Market::MeteoraDammV2));
    }

    #[test]
    fn test_route_lookup_timeout_duration_scales_with_deferred_groups() {
        assert_eq!(
            Swaps::route_lookup_timeout_duration(None),
            Duration::from_secs(Swaps::DEFAULT_ROUTE_LOOKUP_TIMEOUT_SECS)
        );
        assert_eq!(
            Swaps::route_lookup_timeout_duration(Some(&[Market::PumpSwap, Market::RaydiumClmm,])),
            Duration::from_secs(Swaps::DEFAULT_ROUTE_LOOKUP_TIMEOUT_SECS)
        );
        assert_eq!(
            Swaps::route_lookup_timeout_duration(Some(&[Market::PumpSwap])),
            Duration::from_secs(Swaps::DEFAULT_ROUTE_LOOKUP_TIMEOUT_SECS)
        );
        assert_eq!(
            Swaps::price_lookup_timeout_duration(),
            Duration::from_secs(Swaps::DEFAULT_ROUTE_LOOKUP_TIMEOUT_SECS)
        );
    }

    #[test]
    fn test_preferred_route_markets_from_mint_family_prefers_raydium_migration_family() {
        assert_eq!(
            Swaps::preferred_route_markets_from_mint_family(
                "21EZ83KVV3YqhXiAuEwsWsUF8C2EkNVZC3ejV29Hpump",
                Swaps::default_market_priority(),
            ),
            vec![
                Market::RaydiumAmmV4,
                Market::RaydiumCpmm,
                Market::PumpSwap,
                Market::PumpFun,
            ]
        );
    }

    #[test]
    fn test_preferred_route_markets_from_mint_family_prefers_bags_family() {
        assert_eq!(
            Swaps::preferred_route_markets_from_mint_family(
                "4UeLCRqARmfb6e6KQijtiktqqXUxbfk6jZng7DhuBAGS",
                Swaps::default_market_priority(),
            ),
            vec![
                Market::MeteoraDammV2,
                Market::MeteoraDammV1,
                Market::MeteoraDbc,
            ]
        );
    }

    #[test]
    fn test_parse_route_lookup_timeout_secs_defaults_and_accepts_positive_values() {
        assert_eq!(
            Swaps::parse_route_lookup_timeout_secs(None),
            Swaps::DEFAULT_ROUTE_LOOKUP_TIMEOUT_SECS
        );
        assert_eq!(
            Swaps::parse_route_lookup_timeout_secs(Some("0")),
            Swaps::DEFAULT_ROUTE_LOOKUP_TIMEOUT_SECS
        );
        assert_eq!(Swaps::parse_route_lookup_timeout_secs(Some("15")), 15);
        assert_eq!(Swaps::parse_route_lookup_timeout_secs(Some(" 21 ")), 21);
    }

    #[test]
    fn test_prioritize_markets_moves_preferred_markets_to_front_without_duplicates() {
        let ordered = Swaps::prioritize_markets(
            &[
                Market::PumpSwap,
                Market::RaydiumCpmm,
                Market::MeteoraDammV2,
                Market::MeteoraDammV1,
            ],
            &[
                Market::MeteoraDammV2,
                Market::MeteoraDammV1,
                Market::PumpSwap,
            ],
        );
        assert_eq!(
            ordered,
            vec![
                Market::MeteoraDammV2,
                Market::MeteoraDammV1,
                Market::PumpSwap,
                Market::RaydiumCpmm,
            ]
        );
    }

    #[test]
    fn test_market_from_token_variants() {
        assert_eq!(Market::from_token("pump_swap"), Some(Market::PumpSwap));
        assert_eq!(Market::from_token("PumpFun"), Some(Market::PumpFun));
        assert_eq!(
            Market::from_token("raydium-amm-v4"),
            Some(Market::RaydiumAmmV4)
        );
        assert_eq!(
            Market::from_token("meteora_damm_v2"),
            Some(Market::MeteoraDammV2)
        );
        assert_eq!(Market::from_token("unknown"), None);
    }

    #[test]
    fn test_market_parse_csv_deduplicates_and_preserves_order() {
        let markets = Market::parse_csv("pump_swap, raydium_clmm, pump_swap, meteora_dbc")
            .expect("must parse market list");
        assert_eq!(
            markets,
            vec![Market::PumpSwap, Market::RaydiumClmm, Market::MeteoraDbc]
        );
    }

    #[test]
    fn test_market_parse_csv_rejects_invalid_token() {
        let err = Market::parse_csv("pump_swap,nope").unwrap_err();
        assert!(err.to_string().contains("unsupported market"));
    }
}
