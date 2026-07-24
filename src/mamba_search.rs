//! Concurrent, read-only discovery of every WSOL pool for a mint.
//!
//! `MambaSearch` exposes both an aggregate response and a progressive event
//! stream. Discovery is fanned out across all supported markets, while price,
//! liquidity, creator, and timing fields race configured RPCs independently so
//! one failed field does not discard the values other providers returned.

use {
    crate::{
        core::sol::WSOL_MINT,
        dex::swaps::{
            CreatorResolutionSource, DEFAULT_MARKET_PRIORITY, Market, RouteLiquiditySnapshot, Swaps,
        },
    },
    anyhow::Context,
    futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered},
    serde::Serialize,
    solana_client::rpc_client::GetConfirmedSignaturesForAddress2Config,
    solana_commitment_config::CommitmentConfig,
    solana_program::pubkey::Pubkey,
    std::{
        collections::{HashMap, HashSet, VecDeque},
        str::FromStr,
        sync::{Arc, OnceLock},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
    tokio::sync::{Semaphore, mpsc},
};

const DEFAULT_DISCOVERY_TIMEOUT_SECS: u64 = 15;
const DEFAULT_INSPECTION_TIMEOUT_SECS: u64 = 8;
const DEFAULT_HISTORY_SIGNATURE_LIMIT: usize = 25;
const DEFAULT_POOL_CONCURRENCY: usize = 4;
const DEFAULT_GLOBAL_POOL_CONCURRENCY: usize = 8;
const DEFAULT_RPC_CONCURRENCY: usize = 3;
const SEARCH_EVENT_BUFFER: usize = 128;

#[derive(Debug, Clone, Serialize)]
pub struct MambaSearchMint {
    pub address: String,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub uri: Option<String>,
    pub supply_ui: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MambaSearchPool {
    pub market: String,
    pub pool: String,
    pub creator: Option<String>,
    pub creator_source: String,
    pub price_sol: Option<f64>,
    pub mint_balance_raw: Option<u64>,
    pub mint_decimals: Option<u8>,
    pub mint_balance_ui: Option<f64>,
    pub sol_balance_raw: Option<u64>,
    pub sol_balance: Option<f64>,
    pub max_safe_buy_sol: Option<f64>,
    pub market_cap_sol: Option<f64>,
    pub low_liquidity: Option<bool>,
    pub created_time: Option<f64>,
    pub created_time_source: Option<String>,
    pub created_time_approximate: bool,
    pub last_activity_time: Option<f64>,
    pub inspection_status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct MambaSearchMarketReport {
    pub market: String,
    pub pools_found: usize,
    pub rpc_attempts: usize,
    pub rpc_successes: usize,
    pub rpc_failures: usize,
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct MambaSearchResponse {
    pub mint: MambaSearchMint,
    pub quote_mint: String,
    pub pools: Vec<MambaSearchPool>,
    pub markets: Vec<MambaSearchMarketReport>,
    pub rpc_count: usize,
    pub complete: bool,
    pub duration_ms: u128,
    pub searched_at_unix_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MambaSearchEvent {
    Started {
        mint: String,
        quote_mint: String,
        rpc_count: usize,
        market_total: usize,
        searched_at_unix_ms: u128,
    },
    Market {
        report: MambaSearchMarketReport,
        markets_completed: usize,
        market_total: usize,
        pools_discovered: usize,
    },
    Pool {
        pool: MambaSearchPool,
        pools_discovered: usize,
        pools_inspected: usize,
    },
    Mint {
        mint: MambaSearchMint,
    },
    Complete {
        result: MambaSearchResponse,
    },
    Error {
        message: String,
    },
}

#[derive(Debug)]
struct MarketDiscovery {
    market: Market,
    pools: Vec<Pubkey>,
    rpc_successes: usize,
    rpc_failures: usize,
}

#[derive(Debug, Clone, Default)]
struct PoolInspection {
    price_sol: Option<f64>,
    liquidity: Option<RouteLiquiditySnapshot>,
    creator: Option<String>,
    creator_source: String,
    direct_created_time: Option<f64>,
    history: Option<PoolHistory>,
}

enum PoolFieldUpdate {
    Price(Option<f64>),
    Liquidity(Option<RouteLiquiditySnapshot>),
    Creator(Option<(Option<String>, String)>),
    DirectCreatedTime(Option<f64>),
    History(Option<PoolHistory>),
}

#[derive(Debug, Clone, Copy)]
struct PoolHistory {
    oldest_time: f64,
    newest_time: f64,
    window_full: bool,
}

#[derive(Clone)]
pub struct MambaSearch {
    rpc_swaps: Arc<Vec<Arc<Swaps>>>,
    discovery_timeout: Duration,
    inspection_timeout: Duration,
    history_signature_limit: usize,
    pool_concurrency: usize,
    global_pool_semaphore: Arc<Semaphore>,
    market_observations: Arc<HashMap<Market, (Option<f64>, Option<f64>)>>,
}

impl MambaSearch {
    pub fn new(mut rpc_swaps: Vec<Arc<Swaps>>) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !rpc_swaps.is_empty(),
            "mamba_search requires at least one RPC client"
        );
        let rpc_concurrency = usize_from_env(
            "MAMBA_SEARCH_RPC_CONCURRENCY",
            DEFAULT_RPC_CONCURRENCY,
            1,
            rpc_swaps.len(),
        );
        rpc_swaps.truncate(rpc_concurrency);
        Ok(Self {
            rpc_swaps: Arc::new(rpc_swaps),
            discovery_timeout: duration_from_env(
                "MAMBA_SEARCH_DISCOVERY_TIMEOUT_SECS",
                DEFAULT_DISCOVERY_TIMEOUT_SECS,
            ),
            inspection_timeout: duration_from_env(
                "MAMBA_SEARCH_INSPECTION_TIMEOUT_SECS",
                DEFAULT_INSPECTION_TIMEOUT_SECS,
            ),
            history_signature_limit: usize_from_env(
                "MAMBA_SEARCH_HISTORY_SIGNATURE_LIMIT",
                DEFAULT_HISTORY_SIGNATURE_LIMIT,
                1,
                1_000,
            ),
            pool_concurrency: usize_from_env(
                "MAMBA_SEARCH_POOL_CONCURRENCY",
                DEFAULT_POOL_CONCURRENCY,
                1,
                32,
            ),
            global_pool_semaphore: global_pool_semaphore(),
            market_observations: Arc::new(HashMap::new()),
        })
    }

    pub fn with_market_observations(
        mut self,
        observations: HashMap<Market, (Option<f64>, Option<f64>)>,
    ) -> Self {
        self.market_observations = Arc::new(observations);
        self
    }

    pub async fn search(
        &self,
        mint: &str,
        quote_mint: Option<&str>,
    ) -> anyhow::Result<MambaSearchResponse> {
        self.search_inner(mint, quote_mint, None).await
    }

    pub fn search_stream(
        &self,
        mint: String,
        quote_mint: Option<String>,
    ) -> mpsc::Receiver<MambaSearchEvent> {
        let (sender, receiver) = mpsc::channel(SEARCH_EVENT_BUFFER);
        let search = self.clone();
        tokio::spawn(async move {
            match search
                .search_inner(&mint, quote_mint.as_deref(), Some(sender.clone()))
                .await
            {
                Ok(result) => {
                    let _ = sender.send(MambaSearchEvent::Complete { result }).await;
                }
                Err(error) => {
                    let _ = sender
                        .send(MambaSearchEvent::Error {
                            message: crate::core::sol::SolHook::redacted_error(&error),
                        })
                        .await;
                }
            }
        });
        receiver
    }

    async fn search_inner(
        &self,
        mint: &str,
        quote_mint: Option<&str>,
        events: Option<mpsc::Sender<MambaSearchEvent>>,
    ) -> anyhow::Result<MambaSearchResponse> {
        let started = Instant::now();
        let searched_at_unix_ms = unix_time_ms();
        let mint = Pubkey::from_str(mint.trim()).context("invalid mint pubkey")?;
        let quote_mint = quote_mint
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(Pubkey::from_str)
            .transpose()
            .context("invalid quote mint pubkey")?
            .unwrap_or(WSOL_MINT);
        anyhow::ensure!(
            quote_mint == WSOL_MINT,
            "mamba_search currently reports SOL balances and requires the WSOL quote mint"
        );

        send_event(
            &events,
            MambaSearchEvent::Started {
                mint: mint.to_string(),
                quote_mint: quote_mint.to_string(),
                rpc_count: self.rpc_swaps.len(),
                market_total: DEFAULT_MARKET_PRIORITY.len(),
                searched_at_unix_ms,
            },
        )
        .await;

        let supply_search = self.clone();
        let metadata_search = self.clone();
        let supply_task = tokio::spawn(async move { supply_search.first_supply(mint).await });
        let metadata_task = tokio::spawn(async move { metadata_search.first_metadata(mint).await });

        let mut reports = DEFAULT_MARKET_PRIORITY
            .iter()
            .map(|market| MambaSearchMarketReport {
                market: market.as_str().to_string(),
                pools_found: 0,
                rpc_attempts: self.rpc_swaps.len(),
                rpc_successes: 0,
                rpc_failures: 0,
                status: "unavailable",
            })
            .collect::<Vec<_>>();
        let mut discovered = HashSet::<(Market, Pubkey)>::new();
        let mut pending_inspections = VecDeque::<(Market, Pubkey, usize)>::new();
        let mut discovery_attempts = FuturesUnordered::new();
        let mut inspections = FuturesUnordered::new();
        for market in DEFAULT_MARKET_PRIORITY {
            discovery_attempts.push(self.discover_market(mint, quote_mint, market));
        }

        let mut markets_completed = 0usize;
        let mut pools = Vec::with_capacity(discovered.len());
        let mut pools_inspected = 0usize;
        while markets_completed < DEFAULT_MARKET_PRIORITY.len()
            || !pending_inspections.is_empty()
            || !inspections.is_empty()
        {
            if events.as_ref().is_some_and(mpsc::Sender::is_closed) {
                anyhow::bail!("search client disconnected");
            }
            while inspections.len() < self.pool_concurrency {
                let Some((market, pool, pools_discovered)) = pending_inspections.pop_front() else {
                    break;
                };
                let search = self.clone();
                let pool_events = events.clone();
                inspections.push(async move {
                    let inspection = search
                        .inspect_pool_progressive(mint, market, pool, pool_events, pools_discovered)
                        .await;
                    (market, pool, inspection)
                });
            }
            tokio::select! {
                market_discovery = discovery_attempts.next(), if markets_completed < DEFAULT_MARKET_PRIORITY.len() => {
                    let Some(market_discovery) = market_discovery else { continue };
                    markets_completed += 1;
                    let report = MambaSearchMarketReport {
                        market: market_discovery.market.as_str().to_string(),
                        pools_found: market_discovery.pools.len(),
                        rpc_attempts: self.rpc_swaps.len(),
                        rpc_successes: market_discovery.rpc_successes,
                        rpc_failures: market_discovery.rpc_failures,
                        status: if market_discovery.rpc_successes > 0 {
                            "complete"
                        } else {
                            "unavailable"
                        },
                    };
                    reports[market_index(market_discovery.market)] = report.clone();

                    let market = market_discovery.market;
                    let new_pools = market_discovery
                        .pools
                        .into_iter()
                        .filter(|pool| discovered.insert((market, *pool)))
                        .collect::<Vec<_>>();
                    send_event(
                        &events,
                        MambaSearchEvent::Market {
                            report,
                            markets_completed,
                            market_total: DEFAULT_MARKET_PRIORITY.len(),
                            pools_discovered: discovered.len(),
                        },
                    )
                    .await;
                    for pool in new_pools {
                        send_event(
                            &events,
                            MambaSearchEvent::Pool {
                                pool: self.pending_pool_response(market, pool),
                                pools_discovered: discovered.len(),
                                pools_inspected,
                            },
                        )
                        .await;
                        pending_inspections.push_back((market, pool, discovered.len()));
                    }
                }
                inspection = inspections.next(), if !inspections.is_empty() => {
                    let Some((market, pool, inspection)) = inspection else { continue };
                    pools_inspected += 1;
                    let response = self.pool_response(market, pool, inspection, None);
                    pools.push(response.clone());
                    send_event(
                        &events,
                        MambaSearchEvent::Pool {
                            pool: response,
                            pools_discovered: discovered.len(),
                            pools_inspected,
                        },
                    )
                    .await;
                }
            }
        }

        let supply_ui = supply_task.await.ok().and_then(Result::ok);
        if let Some(supply_ui) = supply_ui {
            for pool in &mut pools {
                pool.market_cap_sol = pool.price_sol.map(|price| price * supply_ui);
                send_event(
                    &events,
                    MambaSearchEvent::Pool {
                        pool: pool.clone(),
                        pools_discovered: discovered.len(),
                        pools_inspected,
                    },
                )
                .await;
            }
        }

        let metadata = metadata_task
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or((None, None, None));
        let mint_response = MambaSearchMint {
            address: mint.to_string(),
            name: metadata.0,
            symbol: metadata.1,
            uri: metadata.2,
            supply_ui,
        };
        send_event(
            &events,
            MambaSearchEvent::Mint {
                mint: mint_response.clone(),
            },
        )
        .await;

        pools.sort_by(|left, right| {
            right
                .sol_balance_raw
                .unwrap_or(0)
                .cmp(&left.sol_balance_raw.unwrap_or(0))
                .then_with(|| left.market.cmp(&right.market))
                .then_with(|| left.pool.cmp(&right.pool))
        });

        let complete = reports.iter().all(|report| report.rpc_successes > 0)
            && pools
                .iter()
                .all(|pool| pool.inspection_status == "complete");
        Ok(MambaSearchResponse {
            mint: mint_response,
            quote_mint: quote_mint.to_string(),
            pools,
            markets: reports,
            rpc_count: self.rpc_swaps.len(),
            complete,
            duration_ms: started.elapsed().as_millis(),
            searched_at_unix_ms,
        })
    }

    async fn discover_market(
        &self,
        mint: Pubkey,
        quote_mint: Pubkey,
        market: Market,
    ) -> MarketDiscovery {
        let mut attempts = FuturesUnordered::new();
        for swaps in self.rpc_swaps.iter().cloned() {
            let timeout = self.discovery_timeout;
            attempts.push(async move {
                tokio::time::timeout(
                    timeout,
                    swaps.find_pools_for_market(market, &mint, Some(&quote_mint)),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "{} discovery timed out after {}s",
                        market.as_str(),
                        timeout.as_secs()
                    )
                })?
            });
        }

        let mut rpc_failures = 0;
        while let Some(result) = attempts.next().await {
            match result {
                Ok(pools) => {
                    return MarketDiscovery {
                        market,
                        pools,
                        rpc_successes: 1,
                        rpc_failures,
                    };
                }
                Err(_) => rpc_failures += 1,
            }
        }
        MarketDiscovery {
            market,
            pools: Vec::new(),
            rpc_successes: 0,
            rpc_failures,
        }
    }

    async fn inspect_pool_progressive(
        &self,
        mint: Pubkey,
        market: Market,
        pool: Pubkey,
        events: Option<mpsc::Sender<MambaSearchEvent>>,
        pools_discovered: usize,
    ) -> PoolInspection {
        let Ok(_permit) = self.global_pool_semaphore.clone().acquire_owned().await else {
            return PoolInspection::default();
        };
        let mut inspection = PoolInspection {
            creator_source: CreatorResolutionSource::Unresolved.as_str().to_string(),
            ..PoolInspection::default()
        };
        let mut fields = FuturesUnordered::<BoxFuture<'static, PoolFieldUpdate>>::new();

        let search = self.clone();
        fields.push(
            async move { PoolFieldUpdate::Price(search.first_price(market, pool).await.ok()) }
                .boxed(),
        );
        let search = self.clone();
        fields.push(
            async move {
                PoolFieldUpdate::Liquidity(search.first_liquidity(mint, market, pool).await.ok())
            }
            .boxed(),
        );
        let search = self.clone();
        fields.push(
            async move {
                PoolFieldUpdate::Creator(search.first_creator(mint, market, pool).await.ok())
            }
            .boxed(),
        );
        let search = self.clone();
        fields.push(
            async move {
                PoolFieldUpdate::DirectCreatedTime(
                    search
                        .first_direct_created_time(market, pool)
                        .await
                        .ok()
                        .flatten(),
                )
            }
            .boxed(),
        );
        let search = self.clone();
        fields.push(
            async move { PoolFieldUpdate::History(search.first_history(pool).await.ok()) }.boxed(),
        );

        while let Some(field) = fields.next().await {
            if events.as_ref().is_some_and(mpsc::Sender::is_closed) {
                break;
            }
            match field {
                PoolFieldUpdate::Price(value) => inspection.price_sol = value,
                PoolFieldUpdate::Liquidity(value) => inspection.liquidity = value,
                PoolFieldUpdate::Creator(Some((creator, source))) => {
                    inspection.creator = creator;
                    inspection.creator_source = source;
                }
                PoolFieldUpdate::Creator(None) => {}
                PoolFieldUpdate::DirectCreatedTime(value) => {
                    inspection.direct_created_time = value;
                }
                PoolFieldUpdate::History(value) => inspection.history = value,
            }
            let mut pool_response = self.pool_response(market, pool, inspection.clone(), None);
            if !fields.is_empty() {
                pool_response.inspection_status =
                    if pool_response.inspection_status == "unavailable" {
                        "pending"
                    } else {
                        "partial"
                    };
            }
            send_event(
                &events,
                MambaSearchEvent::Pool {
                    pool: pool_response,
                    pools_discovered,
                    pools_inspected: 0,
                },
            )
            .await;
        }
        inspection
    }

    async fn first_price(&self, market: Market, pool: Pubkey) -> anyhow::Result<f64> {
        let attempts = FuturesUnordered::new();
        for swaps in self.rpc_swaps.iter().cloned() {
            let timeout = self.inspection_timeout;
            attempts.push(async move {
                tokio::time::timeout(timeout, swaps.fetch_price_for_market_pool(market, &pool))
                    .await
                    .map_err(|_| anyhow::anyhow!("pool price lookup timed out"))?
            });
        }
        first_success(attempts).await
    }

    async fn first_liquidity(
        &self,
        mint: Pubkey,
        market: Market,
        pool: Pubkey,
    ) -> anyhow::Result<RouteLiquiditySnapshot> {
        let attempts = FuturesUnordered::new();
        for swaps in self.rpc_swaps.iter().cloned() {
            let timeout = self.inspection_timeout;
            let mint_literal = mint.to_string();
            let pool_literal = pool.to_string();
            attempts.push(async move {
                tokio::time::timeout(
                    timeout,
                    swaps.measure_route_liquidity_for_market_pool(
                        &mint_literal,
                        market,
                        &pool_literal,
                    ),
                )
                .await
                .map_err(|_| anyhow::anyhow!("pool liquidity lookup timed out"))?
            });
        }
        first_success(attempts).await
    }

    async fn first_creator(
        &self,
        mint: Pubkey,
        market: Market,
        pool: Pubkey,
    ) -> anyhow::Result<(Option<String>, String)> {
        let mut attempts = FuturesUnordered::new();
        for swaps in self.rpc_swaps.iter().cloned() {
            let timeout = self.inspection_timeout;
            attempts.push(async move {
                tokio::time::timeout(
                    timeout,
                    swaps.get_route_creator_for_market_pool(&mint, market, &pool),
                )
                .await
                .map_err(|_| anyhow::anyhow!("pool creator lookup timed out"))?
            });
        }
        let mut last_error = None;
        while let Some(result) = attempts.next().await {
            match result {
                Ok(value) if value.source != CreatorResolutionSource::Unresolved => {
                    return Ok((
                        Some(value.creator.to_string()),
                        value.source.as_str().to_string(),
                    ));
                }
                Ok(_) => {}
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("pool creator unresolved")))
    }

    async fn first_direct_created_time(
        &self,
        market: Market,
        pool: Pubkey,
    ) -> anyhow::Result<Option<f64>> {
        let mut attempts = FuturesUnordered::new();
        for swaps in self.rpc_swaps.iter().cloned() {
            let timeout = self.inspection_timeout;
            attempts.push(async move {
                tokio::time::timeout(
                    timeout,
                    swaps.get_pool_created_time_for_market_pool(market, &pool),
                )
                .await
                .map_err(|_| anyhow::anyhow!("pool creation-time lookup timed out"))?
            });
        }
        let mut last_error = None;
        while let Some(result) = attempts.next().await {
            match result {
                Ok(Some(value)) => return Ok(Some(value)),
                Ok(None) => {}
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(error) = last_error {
            Err(error)
        } else {
            Ok(None)
        }
    }

    async fn first_history(&self, pool: Pubkey) -> anyhow::Result<PoolHistory> {
        let attempts = FuturesUnordered::new();
        for swaps in self.rpc_swaps.iter().cloned() {
            let timeout = self.inspection_timeout;
            let limit = self.history_signature_limit;
            attempts.push(async move {
                let entries = tokio::time::timeout(
                    timeout,
                    swaps
                        .sol_hook
                        .rpc_client
                        .get_signatures_for_address_with_config(
                            &pool,
                            GetConfirmedSignaturesForAddress2Config {
                                before: None,
                                until: None,
                                limit: Some(limit),
                                commitment: Some(CommitmentConfig::confirmed()),
                            },
                        ),
                )
                .await
                .map_err(|_| anyhow::anyhow!("pool history lookup timed out"))??;
                let newest_time = entries.iter().find_map(|entry| entry.block_time);
                let oldest_time = entries.iter().rev().find_map(|entry| entry.block_time);
                let (Some(newest_time), Some(oldest_time)) = (newest_time, oldest_time) else {
                    anyhow::bail!("pool history has no block timestamps");
                };
                Ok(PoolHistory {
                    oldest_time: oldest_time as f64,
                    newest_time: newest_time as f64,
                    window_full: entries.len() >= limit,
                })
            });
        }
        first_success(attempts).await
    }

    async fn first_supply(&self, mint: Pubkey) -> anyhow::Result<f64> {
        let attempts = FuturesUnordered::new();
        for swaps in self.rpc_swaps.iter().cloned() {
            let timeout = self.inspection_timeout;
            attempts.push(async move {
                tokio::time::timeout(timeout, swaps.sol_hook.get_token_supply_ui(&mint))
                    .await
                    .map_err(|_| anyhow::anyhow!("mint supply lookup timed out"))?
            });
        }
        first_success(attempts).await
    }

    async fn first_metadata(
        &self,
        mint: Pubkey,
    ) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
        let attempts = FuturesUnordered::new();
        for swaps in self.rpc_swaps.iter().cloned() {
            let timeout = self.inspection_timeout;
            attempts.push(async move {
                let (metadata, _) =
                    tokio::time::timeout(timeout, swaps.sol_hook.get_token_metadata(&mint))
                        .await
                        .map_err(|_| anyhow::anyhow!("mint metadata lookup timed out"))??;
                Ok::<_, anyhow::Error>((
                    non_empty(metadata.name),
                    non_empty(metadata.symbol),
                    non_empty(metadata.uri),
                ))
            });
        }
        first_success(attempts).await
    }

    fn pending_pool_response(&self, market: Market, pool: Pubkey) -> MambaSearchPool {
        let observation = self.market_observations.get(&market).copied();
        MambaSearchPool {
            market: market.as_str().to_string(),
            pool: pool.to_string(),
            creator: None,
            creator_source: CreatorResolutionSource::Unresolved.as_str().to_string(),
            price_sol: None,
            mint_balance_raw: None,
            mint_decimals: None,
            mint_balance_ui: None,
            sol_balance_raw: None,
            sol_balance: None,
            max_safe_buy_sol: None,
            market_cap_sol: None,
            low_liquidity: None,
            created_time: observation.and_then(|value| value.0),
            created_time_source: observation
                .and_then(|value| value.0)
                .map(|_| "market_observation".to_string()),
            created_time_approximate: observation.and_then(|value| value.0).is_some(),
            last_activity_time: observation.and_then(|value| value.1),
            inspection_status: "pending",
        }
    }

    fn pool_response(
        &self,
        market: Market,
        pool: Pubkey,
        inspection: PoolInspection,
        supply_ui: Option<f64>,
    ) -> MambaSearchPool {
        let observation = self.market_observations.get(&market).copied();
        let created_time = inspection
            .direct_created_time
            .or_else(|| inspection.history.map(|history| history.oldest_time))
            .or_else(|| observation.and_then(|value| value.0));
        let (created_time_source, created_time_approximate) =
            if inspection.direct_created_time.is_some() {
                (Some("market_state".to_string()), false)
            } else if let Some(history) = inspection.history {
                (
                    Some(
                        if history.window_full {
                            "account_history_lower_bound"
                        } else {
                            "account_history"
                        }
                        .to_string(),
                    ),
                    history.window_full,
                )
            } else if observation.and_then(|value| value.0).is_some() {
                (Some("market_observation".to_string()), true)
            } else {
                (None, false)
            };
        let last_activity_time = max_optional(
            inspection.history.map(|history| history.newest_time),
            observation.and_then(|value| value.1),
        );
        let mint_balance_ui = inspection.liquidity.as_ref().map(|liquidity| {
            liquidity.mint_balance_raw as f64 / 10_f64.powi(liquidity.mint_decimals as i32)
        });
        let market_cap_sol = inspection
            .price_sol
            .zip(supply_ui)
            .map(|(price, supply)| price * supply);
        let resolved_fields = usize::from(inspection.price_sol.is_some())
            + usize::from(inspection.liquidity.is_some())
            + usize::from(inspection.creator.is_some())
            + usize::from(created_time.is_some());
        let inspection_status = if resolved_fields == 4 {
            "complete"
        } else if resolved_fields > 0 {
            "partial"
        } else {
            "unavailable"
        };

        MambaSearchPool {
            market: market.as_str().to_string(),
            pool: pool.to_string(),
            creator: inspection.creator,
            creator_source: inspection.creator_source,
            price_sol: inspection.price_sol,
            mint_balance_raw: inspection
                .liquidity
                .as_ref()
                .map(|liquidity| liquidity.mint_balance_raw),
            mint_decimals: inspection
                .liquidity
                .as_ref()
                .map(|liquidity| liquidity.mint_decimals),
            mint_balance_ui,
            sol_balance_raw: inspection
                .liquidity
                .as_ref()
                .map(|liquidity| liquidity.wsol_liquidity_raw),
            sol_balance: inspection
                .liquidity
                .as_ref()
                .map(|liquidity| liquidity.wsol_liquidity_raw as f64 / 1_000_000_000.0),
            max_safe_buy_sol: inspection
                .liquidity
                .as_ref()
                .map(|liquidity| liquidity.max_safe_buy_sol_raw as f64 / 1_000_000_000.0),
            market_cap_sol,
            low_liquidity: inspection
                .liquidity
                .as_ref()
                .map(|liquidity| Swaps::route_is_low_lq(market, liquidity)),
            created_time,
            created_time_source,
            created_time_approximate,
            last_activity_time,
            inspection_status,
        }
    }
}

async fn send_event(events: &Option<mpsc::Sender<MambaSearchEvent>>, event: MambaSearchEvent) {
    if let Some(events) = events {
        let _ = events.send(event).await;
    }
}

async fn first_success<T, F>(mut attempts: FuturesUnordered<F>) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut last_error = None;
    while let Some(result) = attempts.next().await {
        match result {
            Ok(value) => return Ok(value),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("RPC lookup unavailable")))
}

fn market_index(market: Market) -> usize {
    DEFAULT_MARKET_PRIORITY
        .iter()
        .position(|candidate| *candidate == market)
        .unwrap_or(0)
}

fn duration_from_env(name: &str, default_seconds: u64) -> Duration {
    let seconds = std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default_seconds);
    Duration::from_secs(seconds)
}

fn usize_from_env(name: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn non_empty(value: String) -> Option<String> {
    let value = value
        .trim_matches(|character: char| character == char::from(0) || character.is_whitespace())
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn max_optional(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn global_pool_semaphore() -> Arc<Semaphore> {
    static GLOBAL: OnceLock<Arc<Semaphore>> = OnceLock::new();
    GLOBAL
        .get_or_init(|| {
            Arc::new(Semaphore::new(usize_from_env(
                "MAMBA_SEARCH_GLOBAL_POOL_CONCURRENCY",
                DEFAULT_GLOBAL_POOL_CONCURRENCY,
                1,
                64,
            )))
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_trims_metadata_padding() {
        assert_eq!(
            non_empty(" Mamba\0\0 ".to_string()).as_deref(),
            Some("Mamba")
        );
        assert_eq!(non_empty("\0 \0".to_string()), None);
    }

    #[test]
    fn market_index_follows_route_priority() {
        for (index, market) in DEFAULT_MARKET_PRIORITY.iter().copied().enumerate() {
            assert_eq!(market_index(market), index);
        }
    }

    #[test]
    fn max_optional_preserves_latest_observation() {
        assert_eq!(max_optional(Some(20.0), Some(30.0)), Some(30.0));
        assert_eq!(max_optional(None, Some(30.0)), Some(30.0));
    }
}
