use anyhow::Context;
use solana_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_program::pubkey::Pubkey;
use solana_rpc_client_types::config::{RpcSimulateTransactionConfig, RpcTransactionLogsFilter};
use solana_signature::Signature;
use solana_signer::Signer;
use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use crate::core::cluster::{SolanaCluster, raydium_launchpad_program_id};
use crate::core::create::compile_unsigned_v0_transaction;
use crate::core::sol::{SolHook, WSOL_MINT};
use crate::dex::meteora_damm_v1::{METEORA_DAMM_V1_ID, MeteoraDammV1, MeteoraDammV1Event};
use crate::dex::meteora_damm_v2::{METEORA_DAMM_V2_ID, MeteoraDammV2, MeteoraDammV2Event};
use crate::dex::meteora_dbc::{METEORA_DBC_ID, MeteoraDbc, MeteoraDbcEvent};
use crate::dex::meteora_dlmm::{METEORA_DLMM_ID, MeteoraDlmm, MeteoraDlmmEvent};
use crate::dex::pump_fun::{PUMP_FUN_ID, PumpFun, PumpFunEvent};
use crate::dex::pump_swap::{PUMP_SWAP_ID, PumpSwap, PumpSwapEvent};
use crate::dex::raydium_amm_v4::{RAYDIUM_AMM_V4_ID, RaydiumAmmV4, RaydiumAmmV4Event};
use crate::dex::raydium_clmm::{RAYDIUM_CLMM_ID, RaydiumClmm, RaydiumClmmEvent};
use crate::dex::raydium_cpmm::{RAYDIUM_CPMM_ID, RaydiumCpmm, RaydiumCpmmEvent};
use crate::dex::raydium_launchpad::{
    RAYDIUM_LAUNCHPAD_DEVNET_ID, RAYDIUM_LAUNCHPAD_ID, RaydiumLaunchpad, RaydiumLaunchpadEvent,
};
use crate::dex::swaps::{Market, Swaps};

fn required_env(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name).with_context(|| format!("missing env var {name}"))?;
    let trimmed = value.trim().to_string();
    anyhow::ensure!(!trimmed.is_empty(), "env var {name} is empty");
    Ok(trimmed)
}

fn required_first_url_env(name: &str) -> anyhow::Result<String> {
    let raw = required_env(name)?;
    raw.split([',', ';', '\n', '\r', ' ', '\t'])
        .map(str::trim)
        .map(|value| value.trim_matches('"').trim_matches('\''))
        .find(|value| !value.is_empty())
        .map(str::to_string)
        .with_context(|| format!("env var {name} did not contain any URLs"))
}

fn required_http_url() -> anyhow::Result<String> {
    required_first_url_env("MAMBA_API_HTTP_URLS")
}

fn required_ws_url() -> anyhow::Result<String> {
    required_first_url_env("MAMBA_API_WS_URLS")
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_slippage_percent(name: &str, default: f64) -> anyhow::Result<f64> {
    let value = env_f64(name, default);
    anyhow::ensure!(
        (1.0..=99.0).contains(&value),
        "{name} must be within 1..=99 (percent), got {value}"
    );
    Ok(value)
}

fn resolve_lookup_mint(primary_name: &str) -> anyhow::Result<Pubkey> {
    let value = std::env::var(primary_name)
        .ok()
        .or_else(|| std::env::var("LOOKUP_MINT").ok())
        .or_else(|| std::env::var("TEST_MINT").ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .with_context(|| {
            format!("missing env var {primary_name} (or LOOKUP_MINT / TEST_MINT fallback)")
        })?;
    Pubkey::from_str(&value)
        .with_context(|| format!("invalid pubkey in env var {primary_name}: {value}"))
}

fn normalize_market_token(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn parse_market_token(value: &str) -> Option<Market> {
    match normalize_market_token(value).as_str() {
        "pumpswap" => Some(Market::PumpSwap),
        "pumpfun" => Some(Market::PumpFun),
        "raydiumammv4" => Some(Market::RaydiumAmmV4),
        "raydiumlaunchpad" => Some(Market::RaydiumLaunchpad),
        "raydiumclmm" => Some(Market::RaydiumClmm),
        "raydiumcpmm" => Some(Market::RaydiumCpmm),
        "meteoradlmm" => Some(Market::MeteoraDlmm),
        "meteoradammv1" => Some(Market::MeteoraDammV1),
        "meteoradammv2" => Some(Market::MeteoraDammV2),
        "meteoradbc" => Some(Market::MeteoraDbc),
        _ => None,
    }
}

fn parse_swaps_router_market_priority_from_env() -> anyhow::Result<Option<Vec<Market>>> {
    let Some(raw) = std::env::var("SWAPS_ROUTER_MARKET_PRIORITY").ok() else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }

    let mut ordered = Vec::new();

    for token in raw
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let market = parse_market_token(token).with_context(|| {
            format!(
                "invalid market '{token}' in SWAPS_ROUTER_MARKET_PRIORITY; \
supported values include pump_swap,pump_fun,raydium_amm_v4,raydium_launchpad,raydium_clmm,\
raydium_cpmm,meteora_dlmm,meteora_damm_v1,meteora_damm_v2,meteora_dbc"
            )
        })?;
        if !ordered.contains(&market) {
            ordered.push(market);
        }
    }

    anyhow::ensure!(
        !ordered.is_empty(),
        "SWAPS_ROUTER_MARKET_PRIORITY is set but no valid markets were parsed"
    );
    Ok(Some(ordered))
}

async fn collect_recent_raydium_launchpad_activity(
    raydium_launchpad: &RaydiumLaunchpad,
    launchpad_program_id: &Pubkey,
    max_signature_lookups: usize,
    signature_lookups: &mut usize,
    rate_limit_hits: &mut usize,
    extracted_pools: &mut BTreeSet<Pubkey>,
    extracted_mints: &mut BTreeSet<Pubkey>,
) -> anyhow::Result<usize> {
    let entries = raydium_launchpad
        .sol
        .rpc_client
        .get_signatures_for_address_with_config(
            launchpad_program_id,
            GetConfirmedSignaturesForAddress2Config {
                before: None,
                until: None,
                limit: Some(max_signature_lookups),
                commitment: Some(CommitmentConfig::confirmed()),
            },
        )
        .await?;

    let mut matches = 0usize;
    for entry in entries {
        if *signature_lookups >= max_signature_lookups {
            break;
        }
        let Ok(signature) = Signature::from_str(&entry.signature) else {
            continue;
        };
        *signature_lookups += 1;

        let pool = match raydium_launchpad.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            Ok(None) => continue,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    *rate_limit_hits += 1;
                }
                continue;
            }
        };

        let state = match raydium_launchpad.fetch_state(&pool).await {
            Ok(state) => state,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    *rate_limit_hits += 1;
                }
                continue;
            }
        };

        let attributable_mint = if state.base_mint == WSOL_MINT && state.quote_mint != WSOL_MINT {
            Some(state.quote_mint)
        } else if state.quote_mint == WSOL_MINT && state.base_mint != WSOL_MINT {
            Some(state.base_mint)
        } else if state.base_mint != Pubkey::default() && state.base_mint != WSOL_MINT {
            Some(state.base_mint)
        } else if state.quote_mint != Pubkey::default() && state.quote_mint != WSOL_MINT {
            Some(state.quote_mint)
        } else {
            None
        };

        if let Some(mint) = attributable_mint {
            matches += 1;
            extracted_pools.insert(pool);
            extracted_mints.insert(mint);
        }
    }

    Ok(matches)
}

async fn assert_swaps_router_route_matches_lookup_mint(
    swaps: &Swaps,
    route: &crate::dex::swaps::MintPoolRoute,
    lookup_mint: &Pubkey,
) -> anyhow::Result<()> {
    match route.market {
        Market::PumpSwap => {
            let state = swaps
                .pump_swap
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch pump.swap state for routed pool {}",
                        route.pool
                    )
                })?;
            let base_mint = Pubkey::new_from_array(state.base_mint.to_bytes());
            let quote_mint = Pubkey::new_from_array(state.quote_mint.to_bytes());
            println!("swaps router pool base_mint: {base_mint}");
            println!("swaps router pool quote_mint: {quote_mint}");
            anyhow::ensure!(
                base_mint == *lookup_mint || quote_mint == *lookup_mint,
                "pump.swap route pool {} does not contain lookup mint {}",
                route.pool,
                lookup_mint
            );
        }
        Market::PumpFun => {
            let expected_pool = PumpFun::derive_bonding_curve(lookup_mint)
                .await
                .with_context(|| {
                    format!(
                        "failed to derive pump.fun bonding curve for lookup mint {}",
                        lookup_mint
                    )
                })?;
            let state = swaps
                .pump_fun
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch pump.fun state for routed pool {}",
                        route.pool
                    )
                })?;
            println!("swaps router pump.fun creator: {}", state.creator);
            println!("swaps router pump.fun complete: {}", state.complete);
            println!(
                "swaps router pump.fun is_mayhem_mode: {}",
                state.is_mayhem_mode
            );
            anyhow::ensure!(
                route.pool == expected_pool,
                "pump.fun route pool {} does not match derived bonding curve {} for lookup mint {}",
                route.pool,
                expected_pool,
                lookup_mint
            );
        }
        Market::RaydiumAmmV4 => {
            let state = swaps
                .raydium_amm_v4
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch raydium amm v4 state for routed pool {}",
                        route.pool
                    )
                })?;
            println!("swaps router pool base_mint: {}", state.base_mint);
            println!("swaps router pool quote_mint: {}", state.quote_mint);
            anyhow::ensure!(
                state.base_mint == *lookup_mint || state.quote_mint == *lookup_mint,
                "raydium amm v4 route pool {} does not contain lookup mint {}",
                route.pool,
                lookup_mint
            );
        }
        Market::RaydiumLaunchpad => {
            let state = swaps
                .raydium_launchpad
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch raydium launchpad state for routed pool {}",
                        route.pool
                    )
                })?;
            println!("swaps router pool base_mint: {}", state.base_mint);
            println!("swaps router pool quote_mint: {}", state.quote_mint);
            anyhow::ensure!(
                state.base_mint == *lookup_mint || state.quote_mint == *lookup_mint,
                "raydium launchpad route pool {} does not contain lookup mint {}",
                route.pool,
                lookup_mint
            );
        }
        Market::RaydiumClmm => {
            let state = swaps
                .raydium_clmm
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch raydium clmm state for routed pool {}",
                        route.pool
                    )
                })?;
            println!("swaps router pool mint_a: {}", state.mint_a);
            println!("swaps router pool mint_b: {}", state.mint_b);
            anyhow::ensure!(
                state.mint_a == *lookup_mint || state.mint_b == *lookup_mint,
                "raydium clmm route pool {} does not contain lookup mint {}",
                route.pool,
                lookup_mint
            );
        }
        Market::RaydiumCpmm => {
            let state = swaps
                .raydium_cpmm
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch raydium cpmm state for routed pool {}",
                        route.pool
                    )
                })?;
            println!("swaps router pool token_0_mint: {}", state.token_0_mint);
            println!("swaps router pool token_1_mint: {}", state.token_1_mint);
            anyhow::ensure!(
                state.token_0_mint == *lookup_mint || state.token_1_mint == *lookup_mint,
                "raydium cpmm route pool {} does not contain lookup mint {}",
                route.pool,
                lookup_mint
            );
        }
        Market::MeteoraDlmm => {
            let state = swaps
                .meteora_dlmm
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch meteora dlmm state for routed pool {}",
                        route.pool
                    )
                })?;
            println!("swaps router pool token_x_mint: {}", state.token_x_mint);
            println!("swaps router pool token_y_mint: {}", state.token_y_mint);
            anyhow::ensure!(
                state.token_x_mint == *lookup_mint || state.token_y_mint == *lookup_mint,
                "meteora dlmm route pool {} does not contain lookup mint {}",
                route.pool,
                lookup_mint
            );
        }
        Market::MeteoraDammV1 => {
            let state = swaps
                .meteora_damm_v1
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch meteora damm v1 state for routed pool {}",
                        route.pool
                    )
                })?;
            println!("swaps router pool token_a_mint: {}", state.token_a_mint);
            println!("swaps router pool token_b_mint: {}", state.token_b_mint);
            anyhow::ensure!(
                state.token_a_mint == *lookup_mint || state.token_b_mint == *lookup_mint,
                "meteora damm v1 route pool {} does not contain lookup mint {}",
                route.pool,
                lookup_mint
            );
        }
        Market::MeteoraDammV2 => {
            let state = swaps
                .meteora_damm_v2
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch meteora damm v2 state for routed pool {}",
                        route.pool
                    )
                })?;
            println!("swaps router pool token_a_mint: {}", state.token_a_mint);
            println!("swaps router pool token_b_mint: {}", state.token_b_mint);
            anyhow::ensure!(
                state.token_a_mint == *lookup_mint || state.token_b_mint == *lookup_mint,
                "meteora damm v2 route pool {} does not contain lookup mint {}",
                route.pool,
                lookup_mint
            );
        }
        Market::MeteoraDbc => {
            let state = swaps
                .meteora_dbc
                .fetch_state(&route.pool)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch meteora dbc state for routed pool {}",
                        route.pool
                    )
                })?;
            println!(
                "swaps router pool base_mint: {}",
                state.virtual_pool.base_mint
            );
            println!("swaps router pool quote_mint: {}", state.config.quote_mint);
            anyhow::ensure!(
                state.virtual_pool.base_mint == *lookup_mint,
                "meteora dbc route pool {} does not contain lookup mint {}",
                route.pool,
                lookup_mint
            );
        }
    }

    Ok(())
}

fn load_operator_keypair() -> anyhow::Result<Arc<Keypair>> {
    let (key, raw) = if let Ok(raw) = std::env::var("MAMBA_PRIVATE_KEY") {
        ("MAMBA_PRIVATE_KEY", raw)
    } else if let Ok(raw) = std::env::var("PRIVATE_KEY") {
        ("PRIVATE_KEY", raw)
    } else {
        anyhow::bail!("missing MAMBA_PRIVATE_KEY (or PRIVATE_KEY fallback)");
    };
    let raw = raw.trim().to_string();
    anyhow::ensure!(!raw.is_empty(), "{key} is empty");
    let keypair = if raw.starts_with('[') {
        let bytes: Vec<u8> = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {key} as JSON byte array"))?;
        Keypair::try_from(bytes.as_slice())
            .with_context(|| format!("{key} JSON must encode 64 keypair bytes"))?
    } else {
        let bytes = bs58::decode(raw)
            .into_vec()
            .with_context(|| format!("failed to parse {key} as base58"))?;
        Keypair::try_from(bytes.as_slice())
            .with_context(|| format!("{key} base58 must decode to 64 keypair bytes"))?
    };
    Ok(Arc::new(keypair))
}

async fn confirm_signature(sol: &SolHook, sig: &Signature, label: &str) -> anyhow::Result<()> {
    let confirmed = sol
        .rpc_client
        .confirm_transaction_with_commitment(sig, CommitmentConfig::confirmed())
        .await?
        .value;
    if !confirmed {
        if let Some(details) = sol.fetch_signature_failure_details(sig).await? {
            anyhow::bail!("{label} transaction failed: {sig} | {details}");
        }
        anyhow::bail!("{label} transaction was not confirmed: {sig}");
    }
    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only cluster check"]
async fn test_operator_inspect_rpc_cluster_genesis_hash() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let sol = Arc::new(SolHook::new(rpc_url));
    let genesis_hash = sol.rpc_client.get_genesis_hash().await?;
    let cluster = SolanaCluster::from_genesis_hash(&genesis_hash.to_string());

    println!("rpc genesis hash: {genesis_hash}");
    println!("rpc cluster: {:?}", cluster);

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_pump_fun_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;
    let sell_slippage_fraction = sell_slippage_pct / 100.0;

    let sol = Arc::new(SolHook::new(rpc_url));
    let pump_fun = PumpFun::new(keypair.clone(), sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![PUMP_FUN_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe pump.fun websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, f64)> = None;
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        for event in PumpFun::parse_logs(msg.logs.iter(), Some(&msg.signature)) {
            match event {
                PumpFunEvent::Trade(Some(trade)) => {
                    let mint = Pubkey::new_from_array(trade.mint.to_bytes());
                    if mint == Pubkey::default() {
                        continue;
                    }
                    let creator = Pubkey::new_from_array(trade.creator.to_bytes());
                    let price = PumpFun::get_price(&trade);
                    if price > 0.0 {
                        selected = Some((mint, creator, price));
                        break;
                    }
                }
                PumpFunEvent::Create(Some(create)) => {
                    let mint = Pubkey::new_from_array(create.mint.to_bytes());
                    if mint == Pubkey::default() {
                        continue;
                    }
                    let creator = Pubkey::new_from_array(create.creator.to_bytes());
                    let price = PumpFun::get_open_price(&create);
                    if price > 0.0 {
                        selected = Some((mint, creator, price));
                        break;
                    }
                }
                _ => {}
            }
        }

        if selected.is_some() {
            break;
        }
    }

    let (mint, mut creator, mut price) =
        selected.context("did not observe an eligible pump.fun mint within timeout")?;
    let bonding_curve = PumpFun::derive_bonding_curve(&mint).await?;

    if creator == Pubkey::default() {
        creator = pump_fun.get_creator(&bonding_curve).await?;
    }
    if price <= 0.0 {
        price = pump_fun.fetch_price(&bonding_curve).await?.1;
    }
    anyhow::ensure!(price > 0.0, "invalid pump.fun price for selected mint");

    let (buy_ixs, buy_fee) = pump_fun
        .buy(
            &mint,
            &bonding_curve,
            &creator,
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("pump_fun buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "pump_fun buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = pump_fun
        .sell(
            &mint,
            &bonding_curve,
            &creator,
            100,
            sell_slippage_fraction,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("pump_fun sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "pump_fun sell").await?;

    Ok(())
}

async fn resolve_pump_swap_lookup_route(
    swaps: &Swaps,
    lookup_mint: Pubkey,
) -> anyhow::Result<crate::dex::swaps::MintPoolRoute> {
    let mint = lookup_mint.to_string();
    if let Ok(raw_pool) = std::env::var("PUMP_SWAP_BUY_LOOKUP_POOL") {
        let pool = Pubkey::from_str(raw_pool.trim())
            .with_context(|| format!("invalid PUMP_SWAP_BUY_LOOKUP_POOL: {}", raw_pool.trim()))?;
        let creator = swaps
            .get_route_creator_for_market_pool(&lookup_mint, Market::PumpSwap, &pool)
            .await
            .with_context(|| {
                format!(
                    "failed to resolve pump.swap creator for lookup mint {} pool {}",
                    lookup_mint, pool
                )
            })?;
        return Ok(crate::dex::swaps::MintPoolRoute {
            market: Market::PumpSwap,
            pool,
            creator: creator.creator,
        });
    }

    let quote_mint = WSOL_MINT.to_string();
    if let Some(route) = swaps
        .find_pool_for_mint_with_market_priority(
            &mint,
            Some(&quote_mint),
            1,
            Some(&[Market::PumpSwap]),
        )
        .await
        .with_context(|| format!("failed to route pump.swap lookup mint {}", lookup_mint))?
    {
        return Ok(route);
    }

    swaps
        .find_pool_for_mint_with_market_priority(&mint, None, 1, Some(&[Market::PumpSwap]))
        .await
        .with_context(|| {
            format!(
                "failed to route pump.swap lookup mint {} without quote filter",
                lookup_mint
            )
        })?
        .context("no pump.swap route found for lookup mint")
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_pump_fun_mayhem_token_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;
    let sell_slippage_fraction = sell_slippage_pct / 100.0;

    let sol = Arc::new(SolHook::new(rpc_url));
    let pump_fun = PumpFun::new(keypair.clone(), sol.clone());

    let mint = Pubkey::from_str("FFC6Jn8KJRZWRV8pwXLWfAPFPMnJ59DiMg4tuEh7pump")
        .context("invalid mayhem mint pubkey")?;
    let bonding_curve = PumpFun::derive_bonding_curve(&mint).await?;
    let creator = pump_fun
        .get_creator(&bonding_curve)
        .await
        .context("failed to resolve creator for mayhem bonding curve")?;
    let price = pump_fun.fetch_price(&bonding_curve).await?.1;
    anyhow::ensure!(price > 0.0, "invalid pump.fun price for mayhem mint");

    let (buy_ixs, buy_fee) = pump_fun
        .buy(
            &mint,
            &bonding_curve,
            &creator,
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("pump_fun mayhem buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "pump_fun mayhem buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = pump_fun
        .sell(
            &mint,
            &bonding_curve,
            &creator,
            100,
            sell_slippage_fraction,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("pump_fun mayhem sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "pump_fun mayhem sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_pump_swap_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let pump_swap = PumpSwap::new(keypair.clone(), sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![PUMP_SWAP_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe pump.swap websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, Pubkey, f64)> = None;
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        for event in PumpSwap::parse_logs(msg.logs.iter(), Some(&msg.signature)) {
            match event {
                PumpSwapEvent::CreatePool(Some(create)) => {
                    let mint = Pubkey::new_from_array(create.base_mint.to_bytes());
                    if mint == WSOL_MINT || mint == Pubkey::default() {
                        continue;
                    }
                    let pool = Pubkey::new_from_array(create.pool.to_bytes());
                    let creator = Pubkey::new_from_array(create.coin_creator.to_bytes());
                    let price = PumpSwap::price_from_create(&create);
                    if price > 0.0 {
                        selected = Some((mint, pool, creator, price));
                        break;
                    }
                }
                PumpSwapEvent::Buy(Some(buy)) => {
                    let pool = Pubkey::new_from_array(buy.pool.to_bytes());
                    let mint = pump_swap
                        .get_mint_from_pool(&pool)
                        .await
                        .context("failed to resolve mint from pool for pump.swap buy event")?;
                    if mint == WSOL_MINT || mint == Pubkey::default() {
                        continue;
                    }
                    let creator = Pubkey::new_from_array(buy.coin_creator.to_bytes());
                    let price = PumpSwap::price_from_buy(&buy);
                    if price > 0.0 {
                        selected = Some((mint, pool, creator, price));
                        break;
                    }
                }
                PumpSwapEvent::Sell(Some(sell)) => {
                    let pool = Pubkey::new_from_array(sell.pool.to_bytes());
                    let mint = pump_swap
                        .get_mint_from_pool(&pool)
                        .await
                        .context("failed to resolve mint from pool for pump.swap sell event")?;
                    if mint == WSOL_MINT || mint == Pubkey::default() {
                        continue;
                    }
                    let creator = Pubkey::new_from_array(sell.coin_creator.to_bytes());
                    let price = PumpSwap::price_from_sell(&sell);
                    if price > 0.0 {
                        selected = Some((mint, pool, creator, price));
                        break;
                    }
                }
                _ => {}
            }
        }

        if selected.is_some() {
            break;
        }
    }

    let (mint, pool, creator, price) =
        selected.context("did not observe an eligible pump.swap mint within timeout")?;

    let (buy_ixs, buy_fee) = pump_swap
        .buy(
            &mint,
            &pool,
            &creator,
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("pump_swap buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "pump_swap buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = pump_swap
        .sell(&mint, &pool, &creator, 100, sell_slippage_pct, price)
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("pump_swap sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "pump_swap sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only build simulation"]
async fn test_operator_build_simulate_pump_swap_min_buy_without_send() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let lookup_mint = resolve_lookup_mint("PUMP_SWAP_BUY_LOOKUP_MINT")?;
    anyhow::ensure!(
        lookup_mint != WSOL_MINT,
        "PUMP_SWAP_BUY_LOOKUP_MINT (or LOOKUP_MINT / TEST_MINT fallback) must be a non-WSOL mint"
    );

    let buy_sol = env_f64("BUY_SOL", 0.00001);
    anyhow::ensure!(buy_sol > 0.0, "BUY_SOL must be > 0");
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;

    let keypair = load_operator_keypair()?;
    let buyer = keypair.pubkey();
    let sol_hook = SolHook::new(rpc_url);
    let sol = Arc::new(sol_hook.clone());
    let pump_swap = PumpSwap::new(keypair.clone(), sol.clone());
    let pump_fun = PumpFun::new(keypair, sol);
    let swaps = Swaps::new(sol_hook, pump_swap, pump_fun);

    let route = resolve_pump_swap_lookup_route(&swaps, lookup_mint).await?;
    anyhow::ensure!(
        route.market == Market::PumpSwap,
        "expected pump.swap route, got {:?}",
        route.market
    );
    assert_swaps_router_route_matches_lookup_mint(&swaps, &route, &lookup_mint).await?;

    let (instructions, recent_fee) = swaps
        .build_buy_instructions_for_user(
            &buyer,
            &lookup_mint,
            &route.pool,
            &route.creator,
            buy_sol,
            buy_slippage_pct,
            0.0,
            Some(true),
            Market::PumpSwap,
        )
        .await
        .with_context(|| {
            format!(
                "failed to build pump.swap min-buy instructions for mint {} pool {}",
                lookup_mint, route.pool
            )
        })?;
    anyhow::ensure!(
        !instructions.is_empty(),
        "pump.swap min-buy build returned no instructions"
    );

    let (blockhash, _) = swaps
        .sol_hook
        .rpc_client
        .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
        .await
        .context("failed to fetch blockhash for pump.swap min-buy simulation")?;
    let tx = compile_unsigned_v0_transaction(&buyer, &instructions, blockhash)
        .context("failed to compile unsigned tx for pump.swap min-buy simulation")?;
    let simulation = swaps
        .sol_hook
        .rpc_client
        .simulate_transaction_with_config(
            &tx,
            RpcSimulateTransactionConfig {
                sig_verify: false,
                replace_recent_blockhash: true,
                commitment: Some(CommitmentConfig::processed()),
                ..RpcSimulateTransactionConfig::default()
            },
        )
        .await
        .context("pump.swap min-buy simulation rpc call failed")?;
    let logs = simulation.value.logs.unwrap_or_default();
    if let Some(err) = simulation.value.err {
        anyhow::bail!(
            "pump.swap min-buy simulation failed for mint {} pool {}: {:?}; logs: {}",
            lookup_mint,
            route.pool,
            err,
            logs.join(" | ")
        );
    }

    println!("pump_swap min-buy lookup mint: {lookup_mint}");
    println!("pump_swap min-buy pool: {}", route.pool);
    println!("pump_swap min-buy creator: {}", route.creator);
    println!("pump_swap min-buy buyer: {buyer}");
    println!("pump_swap min-buy buy_sol: {buy_sol}");
    println!("pump_swap min-buy recent_fee: {recent_fee}");
    println!(
        "pump_swap min-buy instruction_count: {}",
        instructions.len()
    );
    println!("pump_swap min-buy simulation log_count: {}", logs.len());

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_pump_swap_lookup_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let lookup_mint = resolve_lookup_mint("PUMP_SWAP_BUY_LOOKUP_MINT")?;
    anyhow::ensure!(
        lookup_mint != WSOL_MINT,
        "PUMP_SWAP_BUY_LOOKUP_MINT (or LOOKUP_MINT / TEST_MINT fallback) must be a non-WSOL mint"
    );

    let buy_sol = env_f64("BUY_SOL", 0.00001);
    anyhow::ensure!(buy_sol > 0.0, "BUY_SOL must be > 0");
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let keypair = load_operator_keypair()?;
    let sol_hook = SolHook::new(rpc_url);
    let sol = Arc::new(sol_hook.clone());
    let pump_swap = PumpSwap::new(keypair.clone(), sol.clone());
    let pump_fun = PumpFun::new(keypair.clone(), sol.clone());
    let swaps = Swaps::new(sol_hook, pump_swap.clone(), pump_fun);

    let route = resolve_pump_swap_lookup_route(&swaps, lookup_mint).await?;
    anyhow::ensure!(
        route.market == Market::PumpSwap,
        "expected pump.swap route, got {:?}",
        route.market
    );
    assert_swaps_router_route_matches_lookup_mint(&swaps, &route, &lookup_mint).await?;

    let (buy_ixs, buy_fee) = pump_swap
        .buy(
            &lookup_mint,
            &route.pool,
            &route.creator,
            buy_sol,
            buy_slippage_pct,
            0.0,
            Some(true),
        )
        .await
        .with_context(|| {
            format!(
                "failed to build pump.swap live lookup buy for mint {} pool {}",
                lookup_mint, route.pool
            )
        })?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("pump_swap lookup buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "pump_swap lookup buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = pump_swap
        .sell(
            &lookup_mint,
            &route.pool,
            &route.creator,
            100,
            sell_slippage_pct,
            0.0,
        )
        .await
        .with_context(|| {
            format!(
                "failed to build pump.swap live lookup sell for mint {} pool {}",
                lookup_mint, route.pool
            )
        })?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("pump_swap lookup sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "pump_swap lookup sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_pump_swap_lookup_mint_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let lookup_mint = resolve_lookup_mint("PUMP_SWAP_BUY_LOOKUP_MINT")?;
    anyhow::ensure!(
        lookup_mint != WSOL_MINT,
        "PUMP_SWAP_BUY_LOOKUP_MINT (or LOOKUP_MINT / TEST_MINT fallback) must be a non-WSOL mint"
    );

    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let keypair = load_operator_keypair()?;
    let sol_hook = SolHook::new(rpc_url);
    let sol = Arc::new(sol_hook.clone());
    let pump_swap = PumpSwap::new(keypair.clone(), sol.clone());
    let pump_fun = PumpFun::new(keypair.clone(), sol.clone());
    let swaps = Swaps::new(sol_hook, pump_swap.clone(), pump_fun);

    let route = resolve_pump_swap_lookup_route(&swaps, lookup_mint).await?;
    anyhow::ensure!(
        route.market == Market::PumpSwap,
        "expected pump.swap route, got {:?}",
        route.market
    );
    assert_swaps_router_route_matches_lookup_mint(&swaps, &route, &lookup_mint).await?;

    let (sell_ixs, sell_fee) = pump_swap
        .sell(
            &lookup_mint,
            &route.pool,
            &route.creator,
            100,
            sell_slippage_pct,
            0.0,
        )
        .await
        .with_context(|| {
            format!(
                "failed to build pump.swap live lookup sell for mint {} pool {}",
                lookup_mint, route.pool
            )
        })?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("pump_swap lookup sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "pump_swap lookup sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_raydium_clmm_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let raydium_clmm = RaydiumClmm::new(keypair.clone(), sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![RAYDIUM_CLMM_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe raydium clmm websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, f64)> = None;
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        for event in RaydiumClmm::parse_logs(msg.logs.iter(), Some(&msg.signature)) {
            match event {
                RaydiumClmmEvent::PoolCreated(Some(create)) => {
                    let pool = Pubkey::new_from_array(create.pool_state.to_bytes());
                    let mint_a = Pubkey::new_from_array(create.token_mint_0.to_bytes());
                    let mint_b = Pubkey::new_from_array(create.token_mint_1.to_bytes());
                    let mint = if mint_a == WSOL_MINT && mint_b != WSOL_MINT {
                        mint_b
                    } else if mint_b == WSOL_MINT && mint_a != WSOL_MINT {
                        mint_a
                    } else {
                        continue;
                    };
                    let price = match raydium_clmm.fetch_price(&pool).await {
                        Ok((_, price)) if price > 0.0 => price,
                        _ => continue,
                    };
                    selected = Some((mint, pool, price));
                    break;
                }
                RaydiumClmmEvent::Swap(Some(swap)) => {
                    let pool = Pubkey::new_from_array(swap.pool_state.to_bytes());
                    let mint = match raydium_clmm.get_mint_from_pool(&pool).await {
                        Ok(mint) if mint != WSOL_MINT && mint != Pubkey::default() => mint,
                        _ => continue,
                    };
                    let price = match raydium_clmm.fetch_price(&pool).await {
                        Ok((_, price)) if price > 0.0 => price,
                        _ => continue,
                    };
                    selected = Some((mint, pool, price));
                    break;
                }
                _ => {}
            }
        }

        if selected.is_some() {
            break;
        }
    }

    let (mint, pool, price) =
        selected.context("did not observe an eligible raydium clmm mint within timeout")?;

    let (buy_ixs, buy_fee) = raydium_clmm
        .buy(
            &mint,
            &pool,
            &Pubkey::default(),
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("raydium_clmm buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "raydium_clmm buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = raydium_clmm
        .sell(
            &mint,
            &pool,
            &Pubkey::default(),
            100,
            sell_slippage_pct,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("raydium_clmm sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "raydium_clmm sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_raydium_cpmm_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let raydium_cpmm = RaydiumCpmm::new(keypair.clone(), sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![RAYDIUM_CPMM_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe raydium cpmm websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, f64)> = None;
    let cpmm_invoke_prefix = format!("Program {} invoke", RAYDIUM_CPMM_ID);
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        let events = RaydiumCpmm::parse_logs(msg.logs.iter(), Some(&msg.signature));
        if !events.iter().any(|event| {
            matches!(
                event,
                RaydiumCpmmEvent::LpChange(_) | RaydiumCpmmEvent::Swap(_)
            )
        }) && !msg
            .logs
            .iter()
            .any(|log| log.starts_with(&cpmm_invoke_prefix))
        {
            continue;
        }

        let mut pool_from_event: Option<Pubkey> = None;
        for event in &events {
            match event {
                RaydiumCpmmEvent::Swap(Some(swap)) => {
                    pool_from_event = Some(swap.pool_id);
                    break;
                }
                RaydiumCpmmEvent::LpChange(Some(lp_change)) => {
                    pool_from_event = Some(lp_change.pool_id);
                    break;
                }
                _ => {}
            }
        }

        let pool = if let Some(pool) = pool_from_event {
            pool
        } else {
            let signature = match Signature::from_str(&msg.signature) {
                Ok(signature) => signature,
                Err(_) => continue,
            };
            match raydium_cpmm.find_pool_from_signature(&signature).await {
                Ok(Some(pool)) => pool,
                _ => continue,
            }
        };
        let mint = match raydium_cpmm.get_mint_from_pool(&pool).await {
            Ok(mint) if mint != WSOL_MINT && mint != Pubkey::default() => mint,
            _ => continue,
        };
        let price = match raydium_cpmm.fetch_price(&pool).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => continue,
        };
        selected = Some((mint, pool, price));
        break;
    }

    let (mint, pool, price) =
        selected.context("did not observe an eligible raydium cpmm mint within timeout")?;

    let (buy_ixs, buy_fee) = raydium_cpmm
        .buy(
            &mint,
            &pool,
            &Pubkey::default(),
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("raydium_cpmm buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "raydium_cpmm buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = raydium_cpmm
        .sell(
            &mint,
            &pool,
            &Pubkey::default(),
            100,
            sell_slippage_pct,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("raydium_cpmm sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "raydium_cpmm sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_raydium_amm_v4_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let raydium_amm_v4 = RaydiumAmmV4::new(keypair.clone(), sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![RAYDIUM_AMM_V4_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe raydium amm v4 websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, f64)> = None;
    let amm_v4_invoke_prefix = format!("Program {} invoke", RAYDIUM_AMM_V4_ID);
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        let events = RaydiumAmmV4::parse_logs(msg.logs.iter(), Some(&msg.signature));
        if !events.iter().any(|event| {
            matches!(
                event,
                RaydiumAmmV4Event::SwapBaseIn(_) | RaydiumAmmV4Event::SwapBaseOut(_)
            )
        }) && !msg
            .logs
            .iter()
            .any(|log| log.starts_with(&amm_v4_invoke_prefix))
        {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        let pool = match raydium_amm_v4.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            _ => continue,
        };
        let mint = match raydium_amm_v4.get_mint_from_pool(&pool).await {
            Ok(mint) if mint != WSOL_MINT && mint != Pubkey::default() => mint,
            _ => continue,
        };
        let price = match raydium_amm_v4.fetch_price(&pool).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => continue,
        };
        selected = Some((mint, pool, price));
        break;
    }

    let (mint, pool, price) =
        selected.context("did not observe an eligible raydium amm v4 mint within timeout")?;

    let (buy_ixs, buy_fee) = raydium_amm_v4
        .buy(
            &mint,
            &pool,
            &Pubkey::default(),
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("raydium_amm_v4 buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "raydium_amm_v4 buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = raydium_amm_v4
        .sell(
            &mint,
            &pool,
            &Pubkey::default(),
            100,
            sell_slippage_pct,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("raydium_amm_v4 sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "raydium_amm_v4 sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_raydium_launchpad_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()>
{
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let raydium_launchpad = RaydiumLaunchpad::new(keypair.clone(), sol.clone());

    let genesis_hash = sol.rpc_client.get_genesis_hash().await?;
    let cluster = SolanaCluster::from_genesis_hash(&genesis_hash.to_string());
    let launchpad_program_id = raydium_launchpad_program_id(cluster);

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![launchpad_program_id.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe raydium launchpad websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, f64)> = None;
    let launchpad_invoke_prefixes = [
        format!("Program {} invoke", RAYDIUM_LAUNCHPAD_ID),
        format!("Program {} invoke", RAYDIUM_LAUNCHPAD_DEVNET_ID),
    ];
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        let events = RaydiumLaunchpad::parse_logs(msg.logs.iter(), Some(&msg.signature));
        if !events.iter().any(|event| {
            matches!(
                event,
                RaydiumLaunchpadEvent::Trade(_) | RaydiumLaunchpadEvent::PoolCreate(_)
            )
        }) && !msg.logs.iter().any(|log| {
            launchpad_invoke_prefixes
                .iter()
                .any(|prefix| log.starts_with(prefix))
        }) {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };

        let mut pool_from_event = None;
        for event in &events {
            match event {
                RaydiumLaunchpadEvent::Trade(Some(trade)) => {
                    pool_from_event = Some(trade.pool_state);
                    break;
                }
                RaydiumLaunchpadEvent::PoolCreate(Some(create)) => {
                    pool_from_event = Some(create.pool_state);
                    break;
                }
                _ => {}
            }
        }

        let mut pool = pool_from_event;
        let mut state = if let Some(candidate) = pool {
            raydium_launchpad.fetch_state(&candidate).await.ok()
        } else {
            None
        };
        if state.is_none() {
            pool = match raydium_launchpad.find_pool_from_signature(&signature).await {
                Ok(Some(pool)) => Some(pool),
                _ => None,
            };
            if let Some(found_pool) = pool {
                state = raydium_launchpad.fetch_state(&found_pool).await.ok();
            }
        }
        let (pool, state) = match (pool, state) {
            (Some(pool), Some(state)) => (pool, state),
            _ => continue,
        };

        let mint = if state.base_mint == WSOL_MINT && state.quote_mint != WSOL_MINT {
            state.quote_mint
        } else if state.quote_mint == WSOL_MINT && state.base_mint != WSOL_MINT {
            state.base_mint
        } else {
            continue;
        };
        if mint == Pubkey::default() {
            continue;
        }

        let price = match raydium_launchpad.fetch_price(&pool).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => continue,
        };
        selected = Some((mint, pool, price));
        break;
    }

    let (mint, pool, price) =
        selected.context("did not observe an eligible raydium launchpad mint within timeout")?;

    let (buy_ixs, buy_fee) = raydium_launchpad
        .buy(
            &mint,
            &pool,
            &Pubkey::default(),
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("raydium_launchpad buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "raydium_launchpad buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = raydium_launchpad
        .sell(
            &mint,
            &pool,
            &Pubkey::default(),
            100,
            sell_slippage_pct,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("raydium_launchpad sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "raydium_launchpad sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_meteora_dlmm_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.000001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let meteora_dlmm = MeteoraDlmm::new(keypair.clone(), sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![METEORA_DLMM_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe meteora dlmm websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, f64)> = None;
    let dlmm_invoke_prefix = format!("Program {} invoke", METEORA_DLMM_ID);
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        let events = MeteoraDlmm::parse_logs(msg.logs.iter(), Some(&msg.signature));
        if !events.iter().any(|event| {
            matches!(
                event,
                MeteoraDlmmEvent::LbPairCreate(_) | MeteoraDlmmEvent::Swap(_)
            )
        }) && !msg
            .logs
            .iter()
            .any(|log| log.starts_with(&dlmm_invoke_prefix))
        {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        let pool = match meteora_dlmm.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            _ => continue,
        };
        let mint = match meteora_dlmm.get_mint_from_pool(&pool).await {
            Ok(mint) if mint != WSOL_MINT && mint != Pubkey::default() => mint,
            _ => continue,
        };
        let price = match meteora_dlmm.fetch_price(&pool).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => continue,
        };
        selected = Some((mint, pool, price));
        break;
    }

    let (mint, pool, price) =
        selected.context("did not observe an eligible meteora dlmm mint within timeout")?;

    let (buy_ixs, buy_fee) = meteora_dlmm
        .buy(
            &mint,
            &pool,
            &Pubkey::default(),
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("meteora_dlmm buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "meteora_dlmm buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = meteora_dlmm
        .sell(
            &mint,
            &pool,
            &Pubkey::default(),
            100,
            sell_slippage_pct,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("meteora_dlmm sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "meteora_dlmm sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_meteora_damm_v1_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let meteora_damm_v1 = MeteoraDammV1::new(keypair.clone(), sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![METEORA_DAMM_V1_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe meteora damm v1 websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, f64)> = None;
    let damm_invoke_prefix = format!("Program {} invoke", METEORA_DAMM_V1_ID);
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        let events = MeteoraDammV1::parse_logs(msg.logs.iter(), Some(&msg.signature));
        if !events.iter().any(|event| {
            matches!(
                event,
                MeteoraDammV1Event::PoolCreated(_) | MeteoraDammV1Event::Swap(_)
            )
        }) && !msg
            .logs
            .iter()
            .any(|log| log.starts_with(&damm_invoke_prefix))
        {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        let pool = match meteora_damm_v1.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            _ => continue,
        };
        let mint = match meteora_damm_v1.get_mint_from_pool(&pool).await {
            Ok(mint) if mint != WSOL_MINT && mint != Pubkey::default() => mint,
            _ => continue,
        };
        let price = match meteora_damm_v1.fetch_price(&pool).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => continue,
        };
        selected = Some((mint, pool, price));
        break;
    }

    let (mint, pool, price) =
        selected.context("did not observe an eligible meteora damm v1 mint within timeout")?;

    let (buy_ixs, buy_fee) = meteora_damm_v1
        .buy(
            &mint,
            &pool,
            &Pubkey::default(),
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("meteora_damm_v1 buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "meteora_damm_v1 buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = meteora_damm_v1
        .sell(
            &mint,
            &pool,
            &Pubkey::default(),
            100,
            sell_slippage_pct,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("meteora_damm_v1 sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "meteora_damm_v1 sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_meteora_damm_v2_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let meteora_damm_v2 = MeteoraDammV2::new(keypair.clone(), sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![METEORA_DAMM_V2_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe meteora damm v2 websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, f64)> = None;
    let damm_invoke_prefix = format!("Program {} invoke", METEORA_DAMM_V2_ID);
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        let events = MeteoraDammV2::parse_logs(msg.logs.iter(), Some(&msg.signature));
        if !events.iter().any(|event| {
            matches!(
                event,
                MeteoraDammV2Event::InitializePool(_) | MeteoraDammV2Event::Swap2(_)
            )
        }) && !msg
            .logs
            .iter()
            .any(|log| log.starts_with(&damm_invoke_prefix))
        {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        let pool = match meteora_damm_v2.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            _ => continue,
        };
        let mint = match meteora_damm_v2.get_mint_from_pool(&pool).await {
            Ok(mint) if mint != WSOL_MINT && mint != Pubkey::default() => mint,
            _ => continue,
        };
        let price = match meteora_damm_v2.fetch_price(&pool).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => continue,
        };
        selected = Some((mint, pool, price));
        break;
    }

    let (mint, pool, price) =
        selected.context("did not observe an eligible meteora damm v2 mint within timeout")?;

    let (buy_ixs, buy_fee) = meteora_damm_v2
        .buy(
            &mint,
            &pool,
            &Pubkey::default(),
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("meteora_damm_v2 buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "meteora_damm_v2 buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = meteora_damm_v2
        .sell(
            &mint,
            &pool,
            &Pubkey::default(),
            100,
            sell_slippage_pct,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("meteora_damm_v2 sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "meteora_damm_v2 sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_meteora_dbc_first_ws_mint_buy_sell_confirm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;
    let keypair = load_operator_keypair()?;

    let buy_sol = env_f64("BUY_SOL", 0.001);
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let meteora_dbc = MeteoraDbc::new(keypair.clone(), sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![METEORA_DBC_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe meteora dbc websocket logs")?;

    let mut selected: Option<(Pubkey, Pubkey, f64)> = None;
    let dbc_invoke_prefix = format!("Program {} invoke", METEORA_DBC_ID);
    let started_at = Instant::now();
    while started_at.elapsed() < Duration::from_secs(120) {
        let msg = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };

        let events = MeteoraDbc::parse_logs(msg.logs.iter(), Some(&msg.signature));
        if !events.iter().any(|event| {
            matches!(
                event,
                MeteoraDbcEvent::InitializePool(_) | MeteoraDbcEvent::Swap2(_)
            )
        }) && !msg
            .logs
            .iter()
            .any(|log| log.starts_with(&dbc_invoke_prefix))
        {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        let pool = match meteora_dbc.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            _ => continue,
        };
        let state = match meteora_dbc.fetch_state(&pool).await {
            Ok(state) => state,
            Err(_) => continue,
        };
        if state.config.quote_mint != WSOL_MINT || state.virtual_pool.is_migrated != 0 {
            continue;
        }
        let mint = match meteora_dbc.get_mint_from_pool(&pool).await {
            Ok(mint) if mint != WSOL_MINT && mint != Pubkey::default() => mint,
            _ => continue,
        };
        let price = match meteora_dbc.fetch_price(&pool).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => continue,
        };
        selected = Some((mint, pool, price));
        break;
    }

    let (mint, pool, price) =
        selected.context("did not observe an eligible meteora dbc mint within timeout")?;

    let (buy_ixs, buy_fee) = meteora_dbc
        .buy(
            &mint,
            &pool,
            &Pubkey::default(),
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
        )
        .await?;
    let buy_sig = sol
        .send(buy_ixs, keypair.as_ref(), buy_fee, Some(300_000))
        .await?;
    println!("meteora_dbc buy signature: {buy_sig}");
    confirm_signature(&sol, &buy_sig, "meteora_dbc buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let (sell_ixs, sell_fee) = meteora_dbc
        .sell(
            &mint,
            &pool,
            &Pubkey::default(),
            100,
            sell_slippage_pct,
            price,
        )
        .await?;
    let sell_sig = sol
        .send(sell_ixs, keypair.as_ref(), sell_fee, Some(300_000))
        .await?;
    println!("meteora_dbc sell signature: {sell_sig}");
    confirm_signature(&sol, &sell_sig, "meteora_dbc sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_pump_swap_and_pump_fun() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let pump_swap_mint = resolve_lookup_mint("PUMP_SWAP_LOOKUP_MINT")?;
    let pump_fun_mint = resolve_lookup_mint("PUMP_FUN_LOOKUP_MINT")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let pump_swap = PumpSwap::new(keypair.clone(), sol.clone());
    let pump_fun = PumpFun::new(keypair, sol);

    let mut pools = pump_swap
        .find_pools_by_mint(&pump_swap_mint, Some(&WSOL_MINT))
        .await
        .with_context(|| {
            format!(
                "failed to search pump.swap pools for mint {}",
                pump_swap_mint
            )
        })?;
    if pools.is_empty() {
        pools = pump_swap
            .find_pools_by_mint(&pump_swap_mint, None)
            .await
            .with_context(|| {
                format!(
                    "failed to search pump.swap pools (any quote mint) for mint {}",
                    pump_swap_mint
                )
            })?;
    }
    let pool = pools
        .first()
        .copied()
        .with_context(|| format!("no pump.swap pool found for mint {}", pump_swap_mint))?;
    let pool_state = pump_swap
        .fetch_state(&pool)
        .await
        .with_context(|| format!("failed to fetch pump.swap pool state for {}", pool))?;

    let pool_base_mint = Pubkey::new_from_array(pool_state.base_mint.to_bytes());
    let pool_quote_mint = Pubkey::new_from_array(pool_state.quote_mint.to_bytes());
    let pool_base_ata = Pubkey::new_from_array(pool_state.pool_base_token_account.to_bytes());
    let pool_quote_ata = Pubkey::new_from_array(pool_state.pool_quote_token_account.to_bytes());
    let pool_coin_creator = Pubkey::new_from_array(pool_state.coin_creator.to_bytes());

    println!("lookup mint (pump_swap): {pump_swap_mint}");
    println!("pump_swap pool: {pool}");
    println!("pump_swap base_mint: {pool_base_mint}");
    println!("pump_swap quote_mint: {pool_quote_mint}");
    println!("pump_swap pool_base_token_account: {pool_base_ata}");
    println!("pump_swap pool_quote_token_account: {pool_quote_ata}");
    println!("pump_swap coin_creator: {pool_coin_creator}");
    println!("pump_swap is_mayhem_mode: {}", pool_state.is_mayhem_mode);
    anyhow::ensure!(
        pool_base_mint == pump_swap_mint || pool_quote_mint == pump_swap_mint,
        "pump.swap pool {} does not contain lookup mint {}",
        pool,
        pump_swap_mint
    );

    let bonding_curve = PumpFun::derive_bonding_curve(&pump_fun_mint)
        .await
        .with_context(|| {
            format!(
                "failed to derive pump.fun bonding curve for {}",
                pump_fun_mint
            )
        })?;
    let state = pump_fun
        .fetch_state(&bonding_curve)
        .await
        .with_context(|| format!("failed to fetch pump.fun state for {}", bonding_curve))?;
    let (_, price) = pump_fun
        .fetch_price(&bonding_curve)
        .await
        .with_context(|| {
            format!(
                "failed to fetch pump.fun price/state for bonding curve {}",
                bonding_curve
            )
        })?;

    println!("lookup mint (pump_fun): {pump_fun_mint}");
    println!("pump_fun bonding_curve: {bonding_curve}");
    println!("pump_fun creator: {}", state.creator);
    println!("pump_fun complete: {}", state.complete);
    println!("pump_fun is_mayhem_mode: {}", state.is_mayhem_mode);
    println!(
        "pump_fun virtual_reserves: token={} sol={}",
        state.virtual_token_reserves, state.virtual_sol_reserves
    );
    println!(
        "pump_fun real_reserves: token={} sol={}",
        state.real_token_reserves, state.real_sol_reserves
    );
    println!("pump_fun token_total_supply: {}", state.token_total_supply);
    println!("pump_fun implied_price_sol: {price}");

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_swaps_router() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let lookup_mint = resolve_lookup_mint("SWAPS_ROUTER_LOOKUP_MINT")?;
    anyhow::ensure!(
        lookup_mint != WSOL_MINT,
        "SWAPS_ROUTER_LOOKUP_MINT (or LOOKUP_MINT/TEST_MINT fallback) must be a non-WSOL token mint"
    );
    let market_priority = parse_swaps_router_market_priority_from_env()?;

    let sol_hook = SolHook::new(rpc_url);
    let sol = Arc::new(sol_hook.clone());
    let keypair = Arc::new(Keypair::new());
    let pump_swap = PumpSwap::new(keypair.clone(), sol.clone());
    let pump_fun = PumpFun::new(keypair, sol);
    let swaps = Swaps::new(sol_hook, pump_swap, pump_fun);

    let mint = lookup_mint.to_string();
    let quote_mint = WSOL_MINT.to_string();
    let configured_priority = market_priority.as_deref();
    let routed = swaps
        .find_pool_and_price_for_mint_with_market_priority(
            &mint,
            Some(&quote_mint),
            1,
            configured_priority,
        )
        .await
        .with_context(|| {
            format!(
                "failed to route lookup mint {} with WSOL quote",
                lookup_mint
            )
        })?;
    let (route, price) = if let Some(routed) = routed {
        routed
    } else {
        swaps
            .find_pool_and_price_for_mint_with_market_priority(&mint, None, 1, configured_priority)
            .await
            .with_context(|| {
                format!(
                    "failed to route lookup mint {} without quote filter after WSOL-quote miss",
                    lookup_mint
                )
            })?
            .context("no route found by shared swaps router for lookup mint")?
    };

    println!("lookup mint (swaps router): {lookup_mint}");
    println!("swaps router market: {:?}", route.market);
    println!("swaps router pool: {}", route.pool);
    println!("swaps router creator: {}", route.creator);
    println!("swaps router implied_price_sol: {price}");

    assert_swaps_router_route_matches_lookup_mint(&swaps, &route, &lookup_mint).await?;
    anyhow::ensure!(
        price.is_finite() && price > 0.0,
        "shared swaps router returned invalid price for route {}: {}",
        route.pool,
        price
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only funded flow"]
async fn test_operator_live_lookup_mint_buy_sell_confirm_swaps_router() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let lookup_mint = resolve_lookup_mint("SWAPS_ROUTER_LOOKUP_MINT")?;
    anyhow::ensure!(
        lookup_mint != WSOL_MINT,
        "SWAPS_ROUTER_LOOKUP_MINT (or LOOKUP_MINT/TEST_MINT fallback) must be a non-WSOL token mint"
    );
    let buy_sol = env_f64("BUY_SOL", 0.000001);
    anyhow::ensure!(buy_sol > 0.0, "BUY_SOL must be > 0");
    let buy_slippage_pct = env_slippage_percent("BUY_SLIPPAGE", 15.0)?;
    let sell_slippage_pct = env_slippage_percent("SELL_SLIPPAGE", buy_slippage_pct)?;
    let market_priority = parse_swaps_router_market_priority_from_env()?;
    let keypair = load_operator_keypair()?;
    let forced_market = std::env::var("SWAPS_ROUTER_FORCE_MARKET")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            parse_market_token(&value)
                .with_context(|| format!("invalid SWAPS_ROUTER_FORCE_MARKET value: {value}"))
        })
        .transpose()?;
    let forced_pool = std::env::var("SWAPS_ROUTER_FORCE_POOL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            Pubkey::from_str(&value)
                .with_context(|| format!("invalid SWAPS_ROUTER_FORCE_POOL value: {value}"))
        })
        .transpose()?;
    let forced_creator = std::env::var("SWAPS_ROUTER_FORCE_CREATOR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            Pubkey::from_str(&value)
                .with_context(|| format!("invalid SWAPS_ROUTER_FORCE_CREATOR value: {value}"))
        })
        .transpose()?;
    let forced_price = std::env::var("SWAPS_ROUTER_FORCE_PRICE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<f64>()
                .with_context(|| format!("invalid SWAPS_ROUTER_FORCE_PRICE value: {value}"))
        })
        .transpose()?;

    let sol_hook = SolHook::new(rpc_url);
    let sol = Arc::new(sol_hook.clone());
    let pump_swap = PumpSwap::new(keypair.clone(), sol.clone());
    let pump_fun = PumpFun::new(keypair, sol);
    let swaps = Swaps::new(sol_hook, pump_swap, pump_fun);

    let mint = lookup_mint.to_string();
    let (route, price) = if let (Some(market), Some(pool), Some(price)) =
        (forced_market, forced_pool, forced_price)
    {
        (
            crate::dex::swaps::MintPoolRoute {
                market,
                pool,
                creator: forced_creator.unwrap_or_default(),
            },
            price,
        )
    } else {
        let quote_mint = WSOL_MINT.to_string();
        let configured_priority = market_priority.as_deref();
        let routed = swaps
            .find_pool_and_price_for_mint_with_market_priority(
                &mint,
                Some(&quote_mint),
                1,
                configured_priority,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to route lookup mint {} with WSOL quote",
                    lookup_mint
                )
            })?;
        if let Some(routed) = routed {
            routed
        } else {
            swaps
                .find_pool_and_price_for_mint_with_market_priority(
                    &mint,
                    None,
                    1,
                    configured_priority,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to route lookup mint {} without quote filter after WSOL-quote miss",
                        lookup_mint
                    )
                })?
                .context("no route found by shared swaps router for lookup mint")?
        }
    };

    println!("lookup mint (swaps router live): {lookup_mint}");
    println!("swaps router live market: {:?}", route.market);
    println!("swaps router live pool: {}", route.pool);
    println!("swaps router live creator: {}", route.creator);
    println!("swaps router live implied_price_sol: {price}");

    assert_swaps_router_route_matches_lookup_mint(&swaps, &route, &lookup_mint).await?;
    anyhow::ensure!(
        price.is_finite() && price > 0.0,
        "shared swaps router returned invalid live price for route {}: {}",
        route.pool,
        price
    );

    let buy = swaps
        .buy(
            &mint,
            &route.pool.to_string(),
            &route.creator.to_string(),
            buy_sol,
            buy_slippage_pct,
            price,
            Some(true),
            route.market,
            false,
            None,
        )
        .await?;
    anyhow::ensure!(
        buy.success,
        "swaps router live buy failed: {}",
        buy.error
            .unwrap_or_else(|| "swap execution returned unsuccessful result".to_string())
    );
    let buy_sig = buy
        .signature
        .context("swaps router live buy missing signature")?;
    println!("swaps router live buy signature: {buy_sig}");
    confirm_signature(&swaps.sol_hook, &buy_sig, "swaps router live buy").await?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let sell = swaps
        .sell(
            &mint,
            &route.pool.to_string(),
            &route.creator.to_string(),
            100,
            sell_slippage_pct,
            price,
            route.market,
            0,
            false,
            None,
        )
        .await?;
    anyhow::ensure!(
        sell.success,
        "swaps router live sell failed: {}",
        sell.error
            .unwrap_or_else(|| "swap execution returned unsuccessful result".to_string())
    );
    let sell_sig = sell
        .signature
        .context("swaps router live sell missing signature")?;
    println!("swaps router live sell signature: {sell_sig}");
    confirm_signature(&swaps.sol_hook, &sell_sig, "swaps router live sell").await?;

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_meteora_dlmm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let dlmm_mint = resolve_lookup_mint("METEORA_DLMM_LOOKUP_MINT")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let meteora_dlmm = MeteoraDlmm::new(keypair, sol);

    let pool = if let Some(pool) = meteora_dlmm
        .find_pool_by_mint_with_min_liquidity(&dlmm_mint, Some(&WSOL_MINT), 1)
        .await
        .with_context(|| format!("failed to search meteora dlmm pools for mint {}", dlmm_mint))?
    {
        pool
    } else {
        meteora_dlmm
            .find_pool_by_mint_with_min_liquidity(&dlmm_mint, None, 1)
            .await
            .with_context(|| {
                format!(
                    "failed to search meteora dlmm pools (any quote mint) for mint {}",
                    dlmm_mint
                )
            })?
            .with_context(|| format!("no meteora dlmm pool found for mint {}", dlmm_mint))?
    };

    let state = meteora_dlmm
        .fetch_state(&pool)
        .await
        .with_context(|| format!("failed to fetch meteora dlmm state for {}", pool))?;
    let (_, price) = meteora_dlmm
        .fetch_price(&pool)
        .await
        .with_context(|| format!("failed to fetch meteora dlmm price for {}", pool))?;

    println!("lookup mint (meteora_dlmm): {dlmm_mint}");
    println!("meteora_dlmm pool: {pool}");
    println!("meteora_dlmm token_x_mint: {}", state.token_x_mint);
    println!("meteora_dlmm token_y_mint: {}", state.token_y_mint);
    println!("meteora_dlmm reserve_x: {}", state.reserve_x);
    println!("meteora_dlmm reserve_y: {}", state.reserve_y);
    println!("meteora_dlmm oracle: {}", state.oracle);
    println!("meteora_dlmm active_id: {}", state.active_id);
    println!("meteora_dlmm bin_step: {}", state.bin_step);
    println!("meteora_dlmm status: {}", state.status);
    println!("meteora_dlmm creator: {}", state.creator);
    println!(
        "meteora_dlmm token_mint_x_program_flag: {}",
        state.token_mint_x_program_flag
    );
    println!(
        "meteora_dlmm token_mint_y_program_flag: {}",
        state.token_mint_y_program_flag
    );
    println!("meteora_dlmm implied_price_sol: {price}");

    anyhow::ensure!(
        state.token_x_mint == dlmm_mint || state.token_y_mint == dlmm_mint,
        "meteora dlmm pool {} does not contain lookup mint {}",
        pool,
        dlmm_mint
    );
    anyhow::ensure!(
        state.token_x_mint == WSOL_MINT || state.token_y_mint == WSOL_MINT,
        "meteora dlmm pool {} is not WSOL-quoted",
        pool
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_meteora_damm_v1() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let damm_mint = resolve_lookup_mint("METEORA_DAMM_V1_LOOKUP_MINT")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let meteora_damm_v1 = MeteoraDammV1::new(keypair, sol);

    let pool = if let Some(pool) = meteora_damm_v1
        .find_pool_by_mint_with_min_liquidity(&damm_mint, Some(&WSOL_MINT), 1)
        .await
        .with_context(|| {
            format!(
                "failed to search meteora damm v1 pools for mint {}",
                damm_mint
            )
        })? {
        pool
    } else {
        meteora_damm_v1
            .find_pool_by_mint_with_min_liquidity(&damm_mint, None, 1)
            .await
            .with_context(|| {
                format!(
                    "failed to search meteora damm v1 pools (any quote mint) for mint {}",
                    damm_mint
                )
            })?
            .with_context(|| format!("no meteora damm v1 pool found for mint {}", damm_mint))?
    };

    let state = meteora_damm_v1
        .fetch_state(&pool)
        .await
        .with_context(|| format!("failed to fetch meteora damm v1 state for {}", pool))?;
    let (_, price) = meteora_damm_v1
        .fetch_price(&pool)
        .await
        .with_context(|| format!("failed to fetch meteora damm v1 price for {}", pool))?;
    let wsol_liquidity_raw = meteora_damm_v1
        .fetch_wsol_liquidity_raw(&state)
        .await
        .with_context(|| format!("failed to fetch meteora damm v1 liquidity for {}", pool))?;

    println!("lookup mint (meteora_damm_v1): {damm_mint}");
    println!("meteora_damm_v1 pool: {pool}");
    println!("meteora_damm_v1 lp_mint: {}", state.lp_mint);
    println!("meteora_damm_v1 token_a_mint: {}", state.token_a_mint);
    println!("meteora_damm_v1 token_b_mint: {}", state.token_b_mint);
    println!("meteora_damm_v1 a_vault: {}", state.a_vault);
    println!("meteora_damm_v1 b_vault: {}", state.b_vault);
    println!("meteora_damm_v1 a_vault_lp: {}", state.a_vault_lp);
    println!("meteora_damm_v1 b_vault_lp: {}", state.b_vault_lp);
    println!("meteora_damm_v1 enabled: {}", state.enabled);
    println!(
        "meteora_damm_v1 protocol_token_a_fee: {}",
        state.protocol_token_a_fee
    );
    println!(
        "meteora_damm_v1 protocol_token_b_fee: {}",
        state.protocol_token_b_fee
    );
    println!(
        "meteora_damm_v1 fee_last_updated_at: {}",
        state.fee_last_updated_at
    );
    println!("meteora_damm_v1 wsol_liquidity_raw: {wsol_liquidity_raw}");
    println!("meteora_damm_v1 implied_price_sol: {price}");

    anyhow::ensure!(
        state.token_a_mint == damm_mint || state.token_b_mint == damm_mint,
        "meteora damm v1 pool {} does not contain lookup mint {}",
        pool,
        damm_mint
    );
    anyhow::ensure!(
        state.token_a_mint == WSOL_MINT || state.token_b_mint == WSOL_MINT,
        "meteora damm v1 pool {} is not WSOL-quoted",
        pool
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_known_pool_data_by_mint_meteora_damm_v1() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let damm_mint = Pubkey::from_str("FN7JqcckLCYGGp1gnhYE7j5qrmWHFxmHWhd6X38VDRU4")?;
    let expected_pool = Pubkey::from_str("HmeVhpb8zYTyLrSzCZ6qfdQn5TCmpYpRLzsMHNYkpkQ6")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let meteora_damm_v1 = MeteoraDammV1::new(keypair, sol);

    let discovered = meteora_damm_v1
        .find_pool_by_mint_with_min_liquidity(&damm_mint, Some(&WSOL_MINT), 1)
        .await?
        .context("expected WSOL-quoted meteora damm v1 pool for sample mint")?;

    anyhow::ensure!(
        discovered == expected_pool,
        "sample meteora damm v1 lookup returned unexpected pool {} (expected {})",
        discovered,
        expected_pool
    );

    let state = meteora_damm_v1.fetch_state(&discovered).await?;
    anyhow::ensure!(
        state.token_a_mint == damm_mint || state.token_b_mint == damm_mint,
        "discovered meteora damm v1 pool {} does not contain sample mint {}",
        discovered,
        damm_mint
    );
    anyhow::ensure!(
        state.token_a_mint == WSOL_MINT || state.token_b_mint == WSOL_MINT,
        "discovered meteora damm v1 pool {} is not WSOL-quoted",
        discovered
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_meteora_damm_v2() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let damm_mint = resolve_lookup_mint("METEORA_DAMM_V2_LOOKUP_MINT")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let meteora_damm_v2 = MeteoraDammV2::new(keypair, sol);

    let pool = if let Some(pool) = meteora_damm_v2
        .find_pool_by_mint_with_min_liquidity(&damm_mint, Some(&WSOL_MINT), 1)
        .await
        .with_context(|| {
            format!(
                "failed to search meteora damm v2 pools for mint {}",
                damm_mint
            )
        })? {
        pool
    } else {
        meteora_damm_v2
            .find_pool_by_mint_with_min_liquidity(&damm_mint, None, 1)
            .await
            .with_context(|| {
                format!(
                    "failed to search meteora damm v2 pools (any quote mint) for mint {}",
                    damm_mint
                )
            })?
            .with_context(|| format!("no meteora damm v2 pool found for mint {}", damm_mint))?
    };

    let state = meteora_damm_v2
        .fetch_state(&pool)
        .await
        .with_context(|| format!("failed to fetch meteora damm v2 state for {}", pool))?;
    let (_, price) = meteora_damm_v2
        .fetch_price(&pool)
        .await
        .with_context(|| format!("failed to fetch meteora damm v2 price for {}", pool))?;
    let wsol_liquidity_raw = meteora_damm_v2
        .fetch_wsol_liquidity_raw(&state)
        .await
        .with_context(|| format!("failed to fetch meteora damm v2 liquidity for {}", pool))?;

    println!("lookup mint (meteora_damm_v2): {damm_mint}");
    println!("meteora_damm_v2 pool: {pool}");
    println!("meteora_damm_v2 token_a_mint: {}", state.token_a_mint);
    println!("meteora_damm_v2 token_b_mint: {}", state.token_b_mint);
    println!("meteora_damm_v2 token_a_vault: {}", state.token_a_vault);
    println!("meteora_damm_v2 token_b_vault: {}", state.token_b_vault);
    println!(
        "meteora_damm_v2 whitelisted_vault: {}",
        state.whitelisted_vault
    );
    println!("meteora_damm_v2 partner: {}", state.partner);
    println!("meteora_damm_v2 liquidity: {}", state.liquidity);
    println!("meteora_damm_v2 protocol_a_fee: {}", state.protocol_a_fee);
    println!("meteora_damm_v2 protocol_b_fee: {}", state.protocol_b_fee);
    println!("meteora_damm_v2 partner_a_fee: {}", state.partner_a_fee);
    println!("meteora_damm_v2 partner_b_fee: {}", state.partner_b_fee);
    println!("meteora_damm_v2 sqrt_min_price: {}", state.sqrt_min_price);
    println!("meteora_damm_v2 sqrt_max_price: {}", state.sqrt_max_price);
    println!("meteora_damm_v2 sqrt_price: {}", state.sqrt_price);
    println!(
        "meteora_damm_v2 activation_point: {}",
        state.activation_point
    );
    println!("meteora_damm_v2 activation_type: {}", state.activation_type);
    println!("meteora_damm_v2 pool_status: {}", state.pool_status);
    println!("meteora_damm_v2 token_a_flag: {}", state.token_a_flag);
    println!("meteora_damm_v2 token_b_flag: {}", state.token_b_flag);
    println!(
        "meteora_damm_v2 collect_fee_mode: {}",
        state.collect_fee_mode
    );
    println!("meteora_damm_v2 pool_type: {}", state.pool_type);
    println!("meteora_damm_v2 version: {}", state.version);
    println!("meteora_damm_v2 wsol_liquidity_raw: {wsol_liquidity_raw}");
    println!("meteora_damm_v2 implied_price_sol: {price}");

    anyhow::ensure!(
        state.token_a_mint == damm_mint || state.token_b_mint == damm_mint,
        "meteora damm v2 pool {} does not contain lookup mint {}",
        pool,
        damm_mint
    );
    anyhow::ensure!(
        state.token_a_mint == WSOL_MINT || state.token_b_mint == WSOL_MINT,
        "meteora damm v2 pool {} is not WSOL-quoted",
        pool
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_meteora_dbc() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let dbc_mint = resolve_lookup_mint("METEORA_DBC_LOOKUP_MINT")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let meteora_dbc = MeteoraDbc::new(keypair, sol);

    let pool = if let Some(pool) = meteora_dbc
        .find_pool_by_mint_with_min_liquidity(&dbc_mint, Some(&WSOL_MINT), 1)
        .await
        .with_context(|| format!("failed to search meteora dbc pools for mint {}", dbc_mint))?
    {
        pool
    } else {
        meteora_dbc
            .find_pool_by_mint_with_min_liquidity(&dbc_mint, None, 1)
            .await
            .with_context(|| {
                format!(
                    "failed to search meteora dbc pools (any quote mint) for mint {}",
                    dbc_mint
                )
            })?
            .with_context(|| format!("no meteora dbc pool found for mint {}", dbc_mint))?
    };

    let state = meteora_dbc
        .fetch_state(&pool)
        .await
        .with_context(|| format!("failed to fetch meteora dbc state for {}", pool))?;
    let (_, price) = meteora_dbc
        .fetch_price(&pool)
        .await
        .with_context(|| format!("failed to fetch meteora dbc price for {}", pool))?;
    let wsol_liquidity_raw = meteora_dbc
        .fetch_wsol_liquidity_raw(&state)
        .await
        .with_context(|| format!("failed to fetch meteora dbc liquidity for {}", pool))?;

    println!("lookup mint (meteora_dbc): {dbc_mint}");
    println!("meteora_dbc pool: {pool}");
    println!("meteora_dbc config: {}", state.virtual_pool.config);
    println!("meteora_dbc creator: {}", state.virtual_pool.creator);
    println!("meteora_dbc base_mint: {}", state.virtual_pool.base_mint);
    println!("meteora_dbc quote_mint: {}", state.config.quote_mint);
    println!("meteora_dbc base_vault: {}", state.virtual_pool.base_vault);
    println!(
        "meteora_dbc quote_vault: {}",
        state.virtual_pool.quote_vault
    );
    println!(
        "meteora_dbc base_reserve: {}",
        state.virtual_pool.base_reserve
    );
    println!(
        "meteora_dbc quote_reserve: {}",
        state.virtual_pool.quote_reserve
    );
    println!("meteora_dbc sqrt_price: {}", state.virtual_pool.sqrt_price);
    println!("meteora_dbc pool_type: {}", state.virtual_pool.pool_type);
    println!(
        "meteora_dbc migration_progress: {}",
        state.virtual_pool.migration_progress
    );
    println!(
        "meteora_dbc is_migrated: {}",
        state.virtual_pool.is_migrated
    );
    println!(
        "meteora_dbc collect_fee_mode: {}",
        state.config.collect_fee_mode
    );
    println!("meteora_dbc token_decimal: {}", state.config.token_decimal);
    println!("meteora_dbc token_type: {}", state.config.token_type);
    println!("meteora_dbc wsol_liquidity_raw: {wsol_liquidity_raw}");
    println!("meteora_dbc implied_price_sol: {price}");

    anyhow::ensure!(
        state.virtual_pool.base_mint == dbc_mint,
        "meteora dbc pool {} does not contain lookup mint {}",
        pool,
        dbc_mint
    );
    anyhow::ensure!(
        state.config.quote_mint == WSOL_MINT,
        "meteora dbc pool {} is not WSOL-quoted",
        pool
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_pump_fun() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let pump_fun = PumpFun::new(keypair, sol);

    let (mut rx, _handle) = pump_fun
        .sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![PUMP_FUN_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe pump.fun websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let pump_fun_invoke_prefix = format!("Program {} invoke", PUMP_FUN_ID);
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_create_events = 0usize;
    let mut parsed_trade_events = 0usize;
    let mut state_lookups = 0usize;
    let max_state_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();
    let mut verified_pools = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = PumpFun::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg
            .logs
            .iter()
            .any(|log| log.starts_with(&pump_fun_invoke_prefix));
        let has_relevant_event = events
            .iter()
            .any(|event| matches!(event, PumpFunEvent::Create(_) | PumpFunEvent::Trade(_)));
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        let mut pools_to_verify = Vec::new();
        for event in &events {
            match event {
                PumpFunEvent::Create(Some(create)) => {
                    parsed_create_events += 1;
                    let mint = Pubkey::new_from_array(create.mint.to_bytes());
                    if mint == Pubkey::default() || mint == WSOL_MINT {
                        continue;
                    }
                    let pool = PumpFun::derive_bonding_curve(&mint).await?;
                    extracted_pools.insert(pool);
                    extracted_mints.insert(mint);
                    if verified_pools.insert(pool) {
                        pools_to_verify.push(pool);
                    }
                }
                PumpFunEvent::Trade(Some(trade)) => {
                    parsed_trade_events += 1;
                    let mint = Pubkey::new_from_array(trade.mint.to_bytes());
                    if mint == Pubkey::default() || mint == WSOL_MINT {
                        continue;
                    }
                    let pool = PumpFun::derive_bonding_curve(&mint).await?;
                    extracted_pools.insert(pool);
                    extracted_mints.insert(mint);
                    if verified_pools.insert(pool) {
                        pools_to_verify.push(pool);
                    }
                }
                _ => {}
            }
        }

        for pool in pools_to_verify {
            if state_lookups >= max_state_lookups {
                break;
            }
            state_lookups += 1;
            if let Err(err) = pump_fun.fetch_state(&pool).await {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
            }
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    let streamed_for = started_at.elapsed();
    println!(
        "pump_fun ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_create_events={} parsed_trade_events={} state_lookups={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_create_events,
        parsed_trade_events,
        state_lookups,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("pump_fun extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("pump_fun extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        state_lookups <= max_state_lookups,
        "state lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0,
        "did not observe pump.fun candidate websocket messages in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for pump.fun (no attributable pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_pump_swap() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let pump_swap = PumpSwap::new(keypair, sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![PUMP_SWAP_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe pump.swap websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let pump_swap_invoke_prefix = format!("Program {} invoke", PUMP_SWAP_ID);
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_create_pool_events = 0usize;
    let mut parsed_buy_events = 0usize;
    let mut parsed_sell_events = 0usize;
    let mut state_lookups = 0usize;
    let max_state_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();
    let mut checked_pools = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = PumpSwap::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg
            .logs
            .iter()
            .any(|log| log.starts_with(&pump_swap_invoke_prefix));
        let has_relevant_event = events.iter().any(|event| {
            matches!(
                event,
                PumpSwapEvent::CreatePool(_) | PumpSwapEvent::Buy(_) | PumpSwapEvent::Sell(_)
            )
        });
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        let mut pools_to_check = Vec::new();
        for event in &events {
            match event {
                PumpSwapEvent::CreatePool(Some(create)) => {
                    parsed_create_pool_events += 1;
                    let pool = Pubkey::new_from_array(create.pool.to_bytes());
                    if pool != Pubkey::default() && checked_pools.insert(pool) {
                        pools_to_check.push(pool);
                    }
                    let base_mint = Pubkey::new_from_array(create.base_mint.to_bytes());
                    let quote_mint = Pubkey::new_from_array(create.quote_mint.to_bytes());
                    if quote_mint == WSOL_MINT
                        && base_mint != WSOL_MINT
                        && base_mint != Pubkey::default()
                    {
                        extracted_pools.insert(pool);
                        extracted_mints.insert(base_mint);
                    } else if base_mint == WSOL_MINT
                        && quote_mint != WSOL_MINT
                        && quote_mint != Pubkey::default()
                    {
                        extracted_pools.insert(pool);
                        extracted_mints.insert(quote_mint);
                    }
                }
                PumpSwapEvent::Buy(Some(buy)) => {
                    parsed_buy_events += 1;
                    let pool = Pubkey::new_from_array(buy.pool.to_bytes());
                    if pool != Pubkey::default() && checked_pools.insert(pool) {
                        pools_to_check.push(pool);
                    }
                }
                PumpSwapEvent::Sell(Some(sell)) => {
                    parsed_sell_events += 1;
                    let pool = Pubkey::new_from_array(sell.pool.to_bytes());
                    if pool != Pubkey::default() && checked_pools.insert(pool) {
                        pools_to_check.push(pool);
                    }
                }
                _ => {}
            }
        }

        for pool in pools_to_check {
            if state_lookups >= max_state_lookups {
                break;
            }
            state_lookups += 1;
            let state = match pump_swap.fetch_state(&pool).await {
                Ok(state) => state,
                Err(err) => {
                    let err_text = err.to_string().to_ascii_lowercase();
                    if err_text.contains("429")
                        || err_text.contains("too many requests")
                        || err_text.contains("rate limit")
                    {
                        rate_limit_hits += 1;
                    }
                    continue;
                }
            };
            let base_mint = Pubkey::new_from_array(state.base_mint.to_bytes());
            let quote_mint = Pubkey::new_from_array(state.quote_mint.to_bytes());
            if quote_mint == WSOL_MINT && base_mint != WSOL_MINT && base_mint != Pubkey::default() {
                extracted_pools.insert(pool);
                extracted_mints.insert(base_mint);
            } else if base_mint == WSOL_MINT
                && quote_mint != WSOL_MINT
                && quote_mint != Pubkey::default()
            {
                extracted_pools.insert(pool);
                extracted_mints.insert(quote_mint);
            }
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    let streamed_for = started_at.elapsed();
    println!(
        "pump_swap ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_create_pool_events={} parsed_buy_events={} parsed_sell_events={} state_lookups={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_create_pool_events,
        parsed_buy_events,
        parsed_sell_events,
        state_lookups,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("pump_swap extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("pump_swap extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        state_lookups <= max_state_lookups,
        "state lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0,
        "did not observe pump.swap candidate websocket messages in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for pump.swap (no WSOL-quoted pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_raydium_clmm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let raydium_clmm = RaydiumClmm::new(keypair, sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![RAYDIUM_CLMM_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe raydium clmm websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let clmm_invoke_prefix = format!("Program {} invoke", RAYDIUM_CLMM_ID);
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_pool_created_events = 0usize;
    let mut parsed_swap_events = 0usize;
    let mut state_lookups = 0usize;
    let max_state_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();
    let mut checked_pools = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = RaydiumClmm::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg
            .logs
            .iter()
            .any(|log| log.starts_with(&clmm_invoke_prefix));
        let has_relevant_event = events.iter().any(|event| {
            matches!(
                event,
                RaydiumClmmEvent::PoolCreated(_) | RaydiumClmmEvent::Swap(_)
            )
        });
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        let mut pools_to_check = Vec::new();
        for event in &events {
            match event {
                RaydiumClmmEvent::PoolCreated(Some(pool_created)) => {
                    parsed_pool_created_events += 1;
                    let pool = pool_created.pool_state;
                    if pool == Pubkey::default() {
                        continue;
                    }
                    extracted_pools.insert(pool);

                    let mint_a = pool_created.token_mint_0;
                    let mint_b = pool_created.token_mint_1;
                    if mint_a != Pubkey::default()
                        && mint_b != Pubkey::default()
                        && mint_a != mint_b
                    {
                        let extracted_mint = if mint_a == WSOL_MINT && mint_b != WSOL_MINT {
                            mint_b
                        } else if mint_b == WSOL_MINT && mint_a != WSOL_MINT {
                            mint_a
                        } else {
                            mint_a
                        };
                        extracted_mints.insert(extracted_mint);
                    }
                }
                RaydiumClmmEvent::Swap(Some(swap)) => {
                    parsed_swap_events += 1;
                    let pool = swap.pool_state;
                    if pool != Pubkey::default() && checked_pools.insert(pool) {
                        pools_to_check.push(pool);
                    }
                }
                _ => {}
            }
        }

        for pool in pools_to_check {
            if state_lookups >= max_state_lookups {
                break;
            }
            state_lookups += 1;
            let state = match raydium_clmm.fetch_state(&pool).await {
                Ok(state) => state,
                Err(err) => {
                    let err_text = err.to_string().to_ascii_lowercase();
                    if err_text.contains("429")
                        || err_text.contains("too many requests")
                        || err_text.contains("rate limit")
                    {
                        rate_limit_hits += 1;
                    }
                    continue;
                }
            };
            if state.mint_a != Pubkey::default()
                && state.mint_b != Pubkey::default()
                && state.mint_a != state.mint_b
            {
                extracted_pools.insert(pool);
                let extracted_mint = if state.mint_a == WSOL_MINT && state.mint_b != WSOL_MINT {
                    state.mint_b
                } else if state.mint_b == WSOL_MINT && state.mint_a != WSOL_MINT {
                    state.mint_a
                } else {
                    state.mint_a
                };
                extracted_mints.insert(extracted_mint);
            }
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    let streamed_for = started_at.elapsed();
    println!(
        "raydium_clmm ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_pool_created_events={} parsed_swap_events={} state_lookups={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_pool_created_events,
        parsed_swap_events,
        state_lookups,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("raydium_clmm extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("raydium_clmm extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        state_lookups <= max_state_lookups,
        "state lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0,
        "did not observe raydium clmm candidate websocket messages in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for raydium clmm (no attributable pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_meteora_dlmm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let meteora_dlmm = MeteoraDlmm::new(keypair, sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![METEORA_DLMM_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe meteora dlmm websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let dlmm_invoke_prefix = format!("Program {} invoke", METEORA_DLMM_ID);
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_lb_pair_create_events = 0usize;
    let mut parsed_swap_events = 0usize;
    let mut signature_lookups = 0usize;
    let max_signature_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = MeteoraDlmm::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg
            .logs
            .iter()
            .any(|log| log.starts_with(&dlmm_invoke_prefix));
        let has_relevant_event = events.iter().any(|event| {
            matches!(
                event,
                MeteoraDlmmEvent::LbPairCreate(_) | MeteoraDlmmEvent::Swap(_)
            )
        });
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        for event in &events {
            match event {
                MeteoraDlmmEvent::LbPairCreate(Some(_)) => {
                    parsed_lb_pair_create_events += 1;
                }
                MeteoraDlmmEvent::Swap(Some(_)) => {
                    parsed_swap_events += 1;
                }
                _ => {}
            }
        }

        if signature_lookups >= max_signature_lookups {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        signature_lookups += 1;

        let pool = match meteora_dlmm.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            Ok(None) => continue,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };
        let state = match meteora_dlmm.fetch_state(&pool).await {
            Ok(state) => state,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };

        if state.token_x_mint != Pubkey::default()
            && state.token_y_mint != Pubkey::default()
            && state.token_x_mint != state.token_y_mint
        {
            extracted_pools.insert(pool);
            let extracted_mint =
                if state.token_x_mint == WSOL_MINT && state.token_y_mint != WSOL_MINT {
                    state.token_y_mint
                } else if state.token_y_mint == WSOL_MINT && state.token_x_mint != WSOL_MINT {
                    state.token_x_mint
                } else {
                    state.token_x_mint
                };
            extracted_mints.insert(extracted_mint);
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    let streamed_for = started_at.elapsed();
    println!(
        "meteora_dlmm ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_lb_pair_create_events={} parsed_swap_events={} signature_lookups={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_lb_pair_create_events,
        parsed_swap_events,
        signature_lookups,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("meteora_dlmm extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("meteora_dlmm extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        signature_lookups <= max_signature_lookups,
        "signature lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0,
        "did not observe meteora dlmm candidate websocket messages in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for meteora dlmm (no attributable pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_meteora_damm_v1() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let meteora_damm_v1 = MeteoraDammV1::new(keypair, sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![METEORA_DAMM_V1_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe meteora damm v1 websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let damm_invoke_prefix = format!("Program {} invoke", METEORA_DAMM_V1_ID);
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_pool_created_events = 0usize;
    let mut parsed_swap_events = 0usize;
    let mut signature_lookups = 0usize;
    let max_signature_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = MeteoraDammV1::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg
            .logs
            .iter()
            .any(|log| log.starts_with(&damm_invoke_prefix));
        let has_relevant_event = events.iter().any(|event| {
            matches!(
                event,
                MeteoraDammV1Event::PoolCreated(_) | MeteoraDammV1Event::Swap(_)
            )
        });
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        for event in &events {
            match event {
                MeteoraDammV1Event::PoolCreated(Some(pool_created)) => {
                    parsed_pool_created_events += 1;
                    if pool_created.token_a_mint == WSOL_MINT
                        && pool_created.token_b_mint != WSOL_MINT
                    {
                        extracted_pools.insert(pool_created.pool);
                        extracted_mints.insert(pool_created.token_b_mint);
                    } else if pool_created.token_b_mint == WSOL_MINT
                        && pool_created.token_a_mint != WSOL_MINT
                    {
                        extracted_pools.insert(pool_created.pool);
                        extracted_mints.insert(pool_created.token_a_mint);
                    }
                }
                MeteoraDammV1Event::Swap(Some(_)) => {
                    parsed_swap_events += 1;
                }
                _ => {}
            }
        }

        if signature_lookups >= max_signature_lookups {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        signature_lookups += 1;

        let pool = match meteora_damm_v1.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            Ok(None) => continue,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };
        let state = match meteora_damm_v1.fetch_state(&pool).await {
            Ok(state) => state,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };

        if state.token_a_mint == WSOL_MINT && state.token_b_mint != WSOL_MINT {
            extracted_pools.insert(pool);
            extracted_mints.insert(state.token_b_mint);
        } else if state.token_b_mint == WSOL_MINT && state.token_a_mint != WSOL_MINT {
            extracted_pools.insert(pool);
            extracted_mints.insert(state.token_a_mint);
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    let streamed_for = started_at.elapsed();
    println!(
        "meteora_damm_v1 ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_pool_created_events={} parsed_swap_events={} signature_lookups={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_pool_created_events,
        parsed_swap_events,
        signature_lookups,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("meteora_damm_v1 extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("meteora_damm_v1 extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        signature_lookups <= max_signature_lookups,
        "signature lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0,
        "did not observe meteora damm v1 candidate websocket messages in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for meteora damm v1 (no WSOL-quoted pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_meteora_damm_v2() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let meteora_damm_v2 = MeteoraDammV2::new(keypair, sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![METEORA_DAMM_V2_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe meteora damm v2 websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let damm_invoke_prefix = format!("Program {} invoke", METEORA_DAMM_V2_ID);
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_initialize_pool_events = 0usize;
    let mut parsed_swap2_events = 0usize;
    let mut signature_lookups = 0usize;
    let max_signature_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = MeteoraDammV2::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg
            .logs
            .iter()
            .any(|log| log.starts_with(&damm_invoke_prefix));
        let has_relevant_event = events.iter().any(|event| {
            matches!(
                event,
                MeteoraDammV2Event::InitializePool(_) | MeteoraDammV2Event::Swap2(_)
            )
        });
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        for event in &events {
            match event {
                MeteoraDammV2Event::InitializePool(Some(pool_created)) => {
                    parsed_initialize_pool_events += 1;
                    if pool_created.token_a_mint == WSOL_MINT
                        && pool_created.token_b_mint != WSOL_MINT
                    {
                        extracted_pools.insert(pool_created.pool);
                        extracted_mints.insert(pool_created.token_b_mint);
                    } else if pool_created.token_b_mint == WSOL_MINT
                        && pool_created.token_a_mint != WSOL_MINT
                    {
                        extracted_pools.insert(pool_created.pool);
                        extracted_mints.insert(pool_created.token_a_mint);
                    }
                }
                MeteoraDammV2Event::Swap2(Some(_)) => {
                    parsed_swap2_events += 1;
                }
                _ => {}
            }
        }

        if signature_lookups >= max_signature_lookups {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        signature_lookups += 1;

        let pool = match meteora_damm_v2.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            Ok(None) => continue,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };
        let state = match meteora_damm_v2.fetch_state(&pool).await {
            Ok(state) => state,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };

        if state.token_a_mint == WSOL_MINT && state.token_b_mint != WSOL_MINT {
            extracted_pools.insert(pool);
            extracted_mints.insert(state.token_b_mint);
        } else if state.token_b_mint == WSOL_MINT && state.token_a_mint != WSOL_MINT {
            extracted_pools.insert(pool);
            extracted_mints.insert(state.token_a_mint);
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    let streamed_for = started_at.elapsed();
    println!(
        "meteora_damm_v2 ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_initialize_pool_events={} parsed_swap2_events={} signature_lookups={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_initialize_pool_events,
        parsed_swap2_events,
        signature_lookups,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("meteora_damm_v2 extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("meteora_damm_v2 extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        signature_lookups <= max_signature_lookups,
        "signature lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0,
        "did not observe meteora damm v2 candidate websocket messages in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for meteora damm v2 (no WSOL-quoted pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_meteora_dbc() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let meteora_dbc = MeteoraDbc::new(keypair, sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![METEORA_DBC_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe meteora dbc websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let dbc_invoke_prefix = format!("Program {} invoke", METEORA_DBC_ID);
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_initialize_pool_events = 0usize;
    let mut parsed_swap2_events = 0usize;
    let mut signature_lookups = 0usize;
    let max_signature_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = MeteoraDbc::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg
            .logs
            .iter()
            .any(|log| log.starts_with(&dbc_invoke_prefix));
        let has_relevant_event = events.iter().any(|event| {
            matches!(
                event,
                MeteoraDbcEvent::InitializePool(_) | MeteoraDbcEvent::Swap2(_)
            )
        });
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        for event in &events {
            match event {
                MeteoraDbcEvent::InitializePool(Some(pool_created)) => {
                    parsed_initialize_pool_events += 1;
                    if pool_created.base_mint != WSOL_MINT {
                        extracted_pools.insert(pool_created.pool);
                        extracted_mints.insert(pool_created.base_mint);
                    }
                }
                MeteoraDbcEvent::Swap2(Some(_)) => {
                    parsed_swap2_events += 1;
                }
                _ => {}
            }
        }

        if signature_lookups >= max_signature_lookups {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        signature_lookups += 1;

        let pool = match meteora_dbc.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            Ok(None) => continue,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };
        let state = match meteora_dbc.fetch_state(&pool).await {
            Ok(state) => state,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };

        if state.config.quote_mint == WSOL_MINT && state.virtual_pool.base_mint != WSOL_MINT {
            extracted_pools.insert(pool);
            extracted_mints.insert(state.virtual_pool.base_mint);
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    let streamed_for = started_at.elapsed();
    println!(
        "meteora_dbc ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_initialize_pool_events={} parsed_swap2_events={} signature_lookups={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_initialize_pool_events,
        parsed_swap2_events,
        signature_lookups,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("meteora_dbc extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("meteora_dbc extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        signature_lookups <= max_signature_lookups,
        "signature lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0,
        "did not observe meteora dbc candidate websocket messages in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for meteora dbc (no WSOL-quoted pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_raydium_cpmm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let raydium_cpmm = RaydiumCpmm::new(keypair, sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![RAYDIUM_CPMM_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe raydium cpmm websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let cpmm_invoke_prefix = format!("Program {} invoke", RAYDIUM_CPMM_ID);
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_lp_change_events = 0usize;
    let mut parsed_swap_events = 0usize;
    let mut signature_lookups = 0usize;
    let max_signature_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = RaydiumCpmm::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg
            .logs
            .iter()
            .any(|log| log.starts_with(&cpmm_invoke_prefix));
        let has_relevant_event = events.iter().any(|event| {
            matches!(
                event,
                RaydiumCpmmEvent::LpChange(_) | RaydiumCpmmEvent::Swap(_)
            )
        });
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        for event in &events {
            match event {
                RaydiumCpmmEvent::LpChange(Some(_)) => {
                    parsed_lp_change_events += 1;
                }
                RaydiumCpmmEvent::Swap(Some(swap)) => {
                    parsed_swap_events += 1;
                    if swap.input_mint == WSOL_MINT && swap.output_mint != WSOL_MINT {
                        extracted_pools.insert(swap.pool_id);
                        extracted_mints.insert(swap.output_mint);
                    } else if swap.output_mint == WSOL_MINT && swap.input_mint != WSOL_MINT {
                        extracted_pools.insert(swap.pool_id);
                        extracted_mints.insert(swap.input_mint);
                    }
                }
                _ => {}
            }
        }

        if signature_lookups >= max_signature_lookups {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        signature_lookups += 1;

        let pool = match raydium_cpmm.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            Ok(None) => continue,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };
        let state = match raydium_cpmm.fetch_state(&pool).await {
            Ok(state) => state,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };

        if state.token_0_mint == WSOL_MINT && state.token_1_mint != WSOL_MINT {
            extracted_pools.insert(pool);
            extracted_mints.insert(state.token_1_mint);
        } else if state.token_1_mint == WSOL_MINT && state.token_0_mint != WSOL_MINT {
            extracted_pools.insert(pool);
            extracted_mints.insert(state.token_0_mint);
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    let streamed_for = started_at.elapsed();
    println!(
        "raydium_cpmm ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_lp_change_events={} parsed_swap_events={} signature_lookups={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_lp_change_events,
        parsed_swap_events,
        signature_lookups,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("raydium_cpmm extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("raydium_cpmm extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        signature_lookups <= max_signature_lookups,
        "signature lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0,
        "did not observe raydium cpmm candidate websocket messages in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for raydium cpmm (no WSOL-quoted pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_raydium_amm_v4() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let raydium_amm_v4 = RaydiumAmmV4::new(keypair, sol.clone());

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![RAYDIUM_AMM_V4_ID.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe raydium amm v4 websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let amm_v4_invoke_prefix = format!("Program {} invoke", RAYDIUM_AMM_V4_ID);
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_swap_base_in_events = 0usize;
    let mut parsed_swap_base_out_events = 0usize;
    let mut signature_lookups = 0usize;
    let max_signature_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = RaydiumAmmV4::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg
            .logs
            .iter()
            .any(|log| log.starts_with(&amm_v4_invoke_prefix));
        let has_relevant_event = events.iter().any(|event| {
            matches!(
                event,
                RaydiumAmmV4Event::SwapBaseIn(_) | RaydiumAmmV4Event::SwapBaseOut(_)
            )
        });
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        for event in &events {
            match event {
                RaydiumAmmV4Event::SwapBaseIn(Some(_)) => {
                    parsed_swap_base_in_events += 1;
                }
                RaydiumAmmV4Event::SwapBaseOut(Some(_)) => {
                    parsed_swap_base_out_events += 1;
                }
                _ => {}
            }
        }

        if signature_lookups >= max_signature_lookups {
            continue;
        }

        let signature = match Signature::from_str(&msg.signature) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        signature_lookups += 1;

        let pool = match raydium_amm_v4.find_pool_from_signature(&signature).await {
            Ok(Some(pool)) => pool,
            Ok(None) => continue,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };
        let state = match raydium_amm_v4.fetch_state(&pool).await {
            Ok(state) => state,
            Err(err) => {
                let err_text = err.to_string().to_ascii_lowercase();
                if err_text.contains("429")
                    || err_text.contains("too many requests")
                    || err_text.contains("rate limit")
                {
                    rate_limit_hits += 1;
                }
                continue;
            }
        };

        if state.base_mint != Pubkey::default()
            && state.quote_mint != Pubkey::default()
            && state.base_mint != state.quote_mint
        {
            extracted_pools.insert(pool);
            let extracted_mint = if state.base_mint == WSOL_MINT && state.quote_mint != WSOL_MINT {
                state.quote_mint
            } else if state.quote_mint == WSOL_MINT && state.base_mint != WSOL_MINT {
                state.base_mint
            } else {
                state.base_mint
            };
            extracted_mints.insert(extracted_mint);
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    let streamed_for = started_at.elapsed();
    println!(
        "raydium_amm_v4 ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_swap_base_in_events={} parsed_swap_base_out_events={} signature_lookups={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_swap_base_in_events,
        parsed_swap_base_out_events,
        signature_lookups,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("raydium_amm_v4 extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("raydium_amm_v4 extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        signature_lookups <= max_signature_lookups,
        "signature lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0,
        "did not observe raydium amm v4 candidate websocket messages in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for raydium amm v4 (no WSOL-quoted pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual read-only websocket parser validation"]
async fn test_operator_ws_parser_validation_raydium_launchpad() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let ws_url = required_ws_url()?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let raydium_launchpad = RaydiumLaunchpad::new(keypair, sol.clone());

    let genesis_hash = sol.rpc_client.get_genesis_hash().await?;
    let cluster = SolanaCluster::from_genesis_hash(&genesis_hash.to_string());
    let launchpad_program_id = raydium_launchpad_program_id(cluster);

    let (mut rx, _handle) = sol
        .subscribe_logs_channel(
            &ws_url,
            RpcTransactionLogsFilter::Mentions(vec![launchpad_program_id.to_string()]),
            CommitmentConfig::processed(),
        )
        .await
        .context("failed to subscribe raydium launchpad websocket logs")?;

    let min_stream_duration = Duration::from_secs(15);
    let max_stream_duration = Duration::from_secs(35);
    let launchpad_invoke_prefixes = [
        format!("Program {} invoke", RAYDIUM_LAUNCHPAD_ID),
        format!("Program {} invoke", RAYDIUM_LAUNCHPAD_DEVNET_ID),
    ];
    let started_at = Instant::now();

    let mut observed_messages = 0usize;
    let mut candidate_messages = 0usize;
    let mut parsed_pool_create_events = 0usize;
    let mut parsed_trade_events = 0usize;
    let mut signature_lookups = 0usize;
    let max_signature_lookups = 30usize;
    let mut rate_limit_hits = 0usize;
    let mut recent_activity_matches = 0usize;
    let mut extracted_pools = BTreeSet::new();
    let mut extracted_mints = BTreeSet::new();

    while started_at.elapsed() < max_stream_duration {
        let msg = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(_) => continue,
        };
        observed_messages += 1;

        let events = RaydiumLaunchpad::parse_logs(msg.logs.iter(), Some(&msg.signature));
        let has_invoke = msg.logs.iter().any(|log| {
            launchpad_invoke_prefixes
                .iter()
                .any(|prefix| log.starts_with(prefix))
        });
        let has_relevant_event = events.iter().any(|event| {
            matches!(
                event,
                RaydiumLaunchpadEvent::Trade(_) | RaydiumLaunchpadEvent::PoolCreate(_)
            )
        });
        if !has_invoke && !has_relevant_event {
            continue;
        }
        candidate_messages += 1;

        let mut pool_from_event: Option<Pubkey> = None;
        for event in &events {
            match event {
                RaydiumLaunchpadEvent::Trade(Some(trade)) => {
                    parsed_trade_events += 1;
                    if pool_from_event.is_none() {
                        pool_from_event = Some(trade.pool_state);
                    }
                }
                RaydiumLaunchpadEvent::PoolCreate(Some(create)) => {
                    parsed_pool_create_events += 1;
                    if pool_from_event.is_none() {
                        pool_from_event = Some(create.pool_state);
                    }
                }
                _ => {}
            }
        }

        let signature = Signature::from_str(&msg.signature).ok();
        let mut pool = pool_from_event;
        let mut state = if let Some(candidate) = pool {
            match raydium_launchpad.fetch_state(&candidate).await {
                Ok(state) => Some(state),
                Err(err) => {
                    let err_text = err.to_string().to_ascii_lowercase();
                    if err_text.contains("429")
                        || err_text.contains("too many requests")
                        || err_text.contains("rate limit")
                    {
                        rate_limit_hits += 1;
                    }
                    None
                }
            }
        } else {
            None
        };

        if state.is_none() {
            if signature_lookups >= max_signature_lookups {
                continue;
            }
            let Some(signature) = signature else {
                continue;
            };
            signature_lookups += 1;
            match raydium_launchpad.find_pool_from_signature(&signature).await {
                Ok(Some(found_pool)) => {
                    pool = Some(found_pool);
                    state = match raydium_launchpad.fetch_state(&found_pool).await {
                        Ok(state) => Some(state),
                        Err(err) => {
                            let err_text = err.to_string().to_ascii_lowercase();
                            if err_text.contains("429")
                                || err_text.contains("too many requests")
                                || err_text.contains("rate limit")
                            {
                                rate_limit_hits += 1;
                            }
                            None
                        }
                    };
                }
                Ok(None) => continue,
                Err(err) => {
                    let err_text = err.to_string().to_ascii_lowercase();
                    if err_text.contains("429")
                        || err_text.contains("too many requests")
                        || err_text.contains("rate limit")
                    {
                        rate_limit_hits += 1;
                    }
                    continue;
                }
            }
        }

        let (pool, state) = match (pool, state) {
            (Some(pool), Some(state)) => (pool, state),
            _ => continue,
        };

        let attributable_mint = if state.base_mint == WSOL_MINT && state.quote_mint != WSOL_MINT {
            Some(state.quote_mint)
        } else if state.quote_mint == WSOL_MINT && state.base_mint != WSOL_MINT {
            Some(state.base_mint)
        } else if state.base_mint != Pubkey::default() && state.base_mint != WSOL_MINT {
            Some(state.base_mint)
        } else if state.quote_mint != Pubkey::default() && state.quote_mint != WSOL_MINT {
            Some(state.quote_mint)
        } else {
            None
        };

        if let Some(mint) = attributable_mint {
            extracted_pools.insert(pool);
            extracted_mints.insert(mint);
        }

        if started_at.elapsed() >= min_stream_duration
            && !extracted_pools.is_empty()
            && !extracted_mints.is_empty()
            && rate_limit_hits == 0
        {
            break;
        }
    }

    if candidate_messages == 0 || extracted_pools.is_empty() || extracted_mints.is_empty() {
        recent_activity_matches = collect_recent_raydium_launchpad_activity(
            &raydium_launchpad,
            &launchpad_program_id,
            max_signature_lookups,
            &mut signature_lookups,
            &mut rate_limit_hits,
            &mut extracted_pools,
            &mut extracted_mints,
        )
        .await?;
    }

    let streamed_for = started_at.elapsed();
    println!(
        "raydium_launchpad ws parser validation: streamed_for={:.2}s observed_messages={} candidate_messages={} parsed_pool_create_events={} parsed_trade_events={} signature_lookups={} recent_activity_matches={} extracted_pools={} extracted_mints={} rate_limit_hits={}",
        streamed_for.as_secs_f64(),
        observed_messages,
        candidate_messages,
        parsed_pool_create_events,
        parsed_trade_events,
        signature_lookups,
        recent_activity_matches,
        extracted_pools.len(),
        extracted_mints.len(),
        rate_limit_hits
    );
    if !extracted_pools.is_empty() {
        println!("raydium_launchpad extracted pools: {:?}", extracted_pools);
    }
    if !extracted_mints.is_empty() {
        println!("raydium_launchpad extracted mints: {:?}", extracted_mints);
    }

    anyhow::ensure!(
        streamed_for >= min_stream_duration,
        "websocket stream ended before required {}s validation window",
        min_stream_duration.as_secs()
    );
    anyhow::ensure!(
        rate_limit_hits == 0,
        "observed rate-limit errors during websocket/parser validation: {rate_limit_hits}"
    );
    anyhow::ensure!(
        signature_lookups <= max_signature_lookups,
        "signature lookup count exceeded anti-spam budget"
    );
    anyhow::ensure!(
        candidate_messages > 0 || recent_activity_matches > 0,
        "did not observe raydium launchpad websocket candidates or attributable recent activity in validation window"
    );
    anyhow::ensure!(
        !extracted_pools.is_empty() && !extracted_mints.is_empty(),
        "parser extractability/provenance check failed for raydium launchpad (no attributable pool+mint extracted)"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_raydium_clmm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let clmm_mint = resolve_lookup_mint("RAYDIUM_CLMM_LOOKUP_MINT")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let raydium_clmm = RaydiumClmm::new(keypair, sol);

    let pool = if let Some(pool) = raydium_clmm
        .find_pool_by_mint_with_min_liquidity(&clmm_mint, Some(&WSOL_MINT), 1)
        .await
        .with_context(|| format!("failed to search raydium clmm pools for mint {}", clmm_mint))?
    {
        pool
    } else {
        raydium_clmm
            .find_pool_by_mint_with_min_liquidity(&clmm_mint, None, 1)
            .await
            .with_context(|| {
                format!(
                    "failed to search raydium clmm pools (any quote mint) for mint {}",
                    clmm_mint
                )
            })?
            .with_context(|| format!("no raydium clmm pool found for mint {}", clmm_mint))?
    };

    let state = raydium_clmm
        .fetch_state(&pool)
        .await
        .with_context(|| format!("failed to fetch raydium clmm state for {}", pool))?;
    let (_, price) = raydium_clmm
        .fetch_price(&pool)
        .await
        .with_context(|| format!("failed to fetch raydium clmm price for {}", pool))?;

    println!("lookup mint (raydium_clmm): {clmm_mint}");
    println!("raydium_clmm pool: {pool}");
    println!("raydium_clmm amm_config: {}", state.amm_config);
    println!("raydium_clmm mint_a: {}", state.mint_a);
    println!("raydium_clmm mint_b: {}", state.mint_b);
    println!("raydium_clmm vault_a: {}", state.vault_a);
    println!("raydium_clmm vault_b: {}", state.vault_b);
    println!("raydium_clmm observation_id: {}", state.observation_id);
    println!("raydium_clmm tick_spacing: {}", state.tick_spacing);
    println!("raydium_clmm tick_current: {}", state.tick_current);
    println!("raydium_clmm sqrt_price_x64: {}", state.sqrt_price_x64);
    println!("raydium_clmm liquidity: {}", state.liquidity);
    println!("raydium_clmm status: {}", state.status);
    println!("raydium_clmm implied_price_sol: {price}");

    anyhow::ensure!(
        state.mint_a == clmm_mint || state.mint_b == clmm_mint,
        "raydium clmm pool {} does not contain lookup mint {}",
        pool,
        clmm_mint
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_raydium_cpmm() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let cpmm_mint = resolve_lookup_mint("RAYDIUM_CPMM_LOOKUP_MINT")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let raydium_cpmm = RaydiumCpmm::new(keypair, sol);

    let pool = if let Some(pool) = raydium_cpmm
        .find_pool_by_mint_with_min_liquidity(&cpmm_mint, Some(&WSOL_MINT), 1)
        .await
        .with_context(|| format!("failed to search raydium cpmm pools for mint {}", cpmm_mint))?
    {
        pool
    } else {
        raydium_cpmm
            .find_pool_by_mint_with_min_liquidity(&cpmm_mint, None, 1)
            .await
            .with_context(|| {
                format!(
                    "failed to search raydium cpmm pools (any quote mint) for mint {}",
                    cpmm_mint
                )
            })?
            .with_context(|| format!("no raydium cpmm pool found for mint {}", cpmm_mint))?
    };

    let state = raydium_cpmm
        .fetch_state(&pool)
        .await
        .with_context(|| format!("failed to fetch raydium cpmm state for {}", pool))?;
    let (_, price) = raydium_cpmm
        .fetch_price(&pool)
        .await
        .with_context(|| format!("failed to fetch raydium cpmm price for {}", pool))?;
    let wsol_liquidity_raw = raydium_cpmm
        .fetch_wsol_liquidity_raw(&state)
        .await
        .with_context(|| format!("failed to fetch raydium cpmm liquidity for {}", pool))?;

    println!("lookup mint (raydium_cpmm): {cpmm_mint}");
    println!("raydium_cpmm pool: {pool}");
    println!("raydium_cpmm amm_config: {}", state.amm_config);
    println!("raydium_cpmm pool_creator: {}", state.pool_creator);
    println!("raydium_cpmm token_0_mint: {}", state.token_0_mint);
    println!("raydium_cpmm token_1_mint: {}", state.token_1_mint);
    println!("raydium_cpmm token_0_vault: {}", state.token_0_vault);
    println!("raydium_cpmm token_1_vault: {}", state.token_1_vault);
    println!("raydium_cpmm token_0_program: {}", state.token_0_program);
    println!("raydium_cpmm token_1_program: {}", state.token_1_program);
    println!("raydium_cpmm lp_mint: {}", state.lp_mint);
    println!("raydium_cpmm observation_key: {}", state.observation_key);
    println!("raydium_cpmm status: {}", state.status);
    println!("raydium_cpmm open_time: {}", state.open_time);
    println!("raydium_cpmm lp_supply: {}", state.lp_supply);
    println!(
        "raydium_cpmm protocol_fees_token_0: {}",
        state.protocol_fees_token_0
    );
    println!(
        "raydium_cpmm protocol_fees_token_1: {}",
        state.protocol_fees_token_1
    );
    println!(
        "raydium_cpmm fund_fees_token_0: {}",
        state.fund_fees_token_0
    );
    println!(
        "raydium_cpmm fund_fees_token_1: {}",
        state.fund_fees_token_1
    );
    println!(
        "raydium_cpmm creator_fees_token_0: {}",
        state.creator_fees_token_0
    );
    println!(
        "raydium_cpmm creator_fees_token_1: {}",
        state.creator_fees_token_1
    );
    println!("raydium_cpmm wsol_liquidity_raw: {wsol_liquidity_raw}");
    println!("raydium_cpmm implied_price_sol: {price}");

    anyhow::ensure!(
        state.token_0_mint == cpmm_mint || state.token_1_mint == cpmm_mint,
        "raydium cpmm pool {} does not contain lookup mint {}",
        pool,
        cpmm_mint
    );
    anyhow::ensure!(
        state.token_0_mint == WSOL_MINT || state.token_1_mint == WSOL_MINT,
        "raydium cpmm pool {} is not WSOL-quoted",
        pool
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_raydium_amm_v4() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let amm_v4_mint = resolve_lookup_mint("RAYDIUM_AMM_V4_LOOKUP_MINT")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let raydium_amm_v4 = RaydiumAmmV4::new(keypair, sol);

    let pool = if let Some(pool) = raydium_amm_v4
        .find_pool_by_mint_with_min_liquidity(&amm_v4_mint, Some(&WSOL_MINT), 1)
        .await
        .with_context(|| {
            format!(
                "failed to search raydium amm v4 pools for mint {}",
                amm_v4_mint
            )
        })? {
        pool
    } else {
        raydium_amm_v4
            .find_pool_by_mint_with_min_liquidity(&amm_v4_mint, None, 1)
            .await
            .with_context(|| {
                format!(
                    "failed to search raydium amm v4 pools (any quote mint) for mint {}",
                    amm_v4_mint
                )
            })?
            .with_context(|| format!("no raydium amm v4 pool found for mint {}", amm_v4_mint))?
    };

    let state = raydium_amm_v4
        .fetch_state(&pool)
        .await
        .with_context(|| format!("failed to fetch raydium amm v4 state for {}", pool))?;
    let (_, price) = raydium_amm_v4
        .fetch_price(&pool)
        .await
        .with_context(|| format!("failed to fetch raydium amm v4 price for {}", pool))?;
    let wsol_liquidity_raw = raydium_amm_v4
        .fetch_wsol_liquidity_raw(&state)
        .await
        .with_context(|| format!("failed to fetch raydium amm v4 liquidity for {}", pool))?;

    println!("lookup mint (raydium_amm_v4): {amm_v4_mint}");
    println!("raydium_amm_v4 pool: {pool}");
    println!("raydium_amm_v4 status: {}", state.status);
    println!("raydium_amm_v4 nonce: {}", state.nonce);
    println!("raydium_amm_v4 base_mint: {}", state.base_mint);
    println!("raydium_amm_v4 quote_mint: {}", state.quote_mint);
    println!("raydium_amm_v4 base_vault: {}", state.base_vault);
    println!("raydium_amm_v4 quote_vault: {}", state.quote_vault);
    println!("raydium_amm_v4 open_orders: {}", state.open_orders);
    println!("raydium_amm_v4 target_orders: {}", state.target_orders);
    println!("raydium_amm_v4 market_id: {}", state.market_id);
    println!(
        "raydium_amm_v4 market_program_id: {}",
        state.market_program_id
    );
    println!("raydium_amm_v4 owner: {}", state.owner);
    println!("raydium_amm_v4 base_decimals: {}", state.base_decimals);
    println!("raydium_amm_v4 quote_decimals: {}", state.quote_decimals);
    println!(
        "raydium_amm_v4 trade_fee_numerator: {}",
        state.trade_fee_numerator
    );
    println!(
        "raydium_amm_v4 trade_fee_denominator: {}",
        state.trade_fee_denominator
    );
    println!(
        "raydium_amm_v4 swap_fee_numerator: {}",
        state.swap_fee_numerator
    );
    println!(
        "raydium_amm_v4 swap_fee_denominator: {}",
        state.swap_fee_denominator
    );
    println!(
        "raydium_amm_v4 base_need_take_pnl: {}",
        state.base_need_take_pnl
    );
    println!(
        "raydium_amm_v4 quote_need_take_pnl: {}",
        state.quote_need_take_pnl
    );
    println!("raydium_amm_v4 pool_open_time: {}", state.pool_open_time);
    println!(
        "raydium_amm_v4 orderbook_to_init_time: {}",
        state.orderbook_to_init_time
    );
    println!("raydium_amm_v4 wsol_liquidity_raw: {wsol_liquidity_raw}");
    println!("raydium_amm_v4 implied_price_sol: {price}");

    anyhow::ensure!(
        state.base_mint == amm_v4_mint || state.quote_mint == amm_v4_mint,
        "raydium amm v4 pool {} does not contain lookup mint {}",
        pool,
        amm_v4_mint
    );
    anyhow::ensure!(
        state.base_mint == WSOL_MINT || state.quote_mint == WSOL_MINT,
        "raydium amm v4 pool {} is not WSOL-quoted",
        pool
    );

    Ok(())
}

#[tokio::test]
#[ignore = "manual operator-only read-only lookup flow"]
async fn test_operator_lookup_pool_data_by_mint_raydium_launchpad() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = required_http_url()?;
    let launchpad_mint = resolve_lookup_mint("RAYDIUM_LAUNCHPAD_LOOKUP_MINT")?;

    let sol = Arc::new(SolHook::new(rpc_url));
    let keypair = Arc::new(Keypair::new());
    let raydium_launchpad = RaydiumLaunchpad::new(keypair, sol);

    let pool = if let Some(pool) = raydium_launchpad
        .find_pool_by_mint_with_min_liquidity(&launchpad_mint, Some(&WSOL_MINT), 1)
        .await
        .with_context(|| {
            format!(
                "failed to search raydium launchpad pools for mint {}",
                launchpad_mint
            )
        })? {
        pool
    } else {
        raydium_launchpad
            .find_pool_by_mint_with_min_liquidity(&launchpad_mint, None, 1)
            .await
            .with_context(|| {
                format!(
                    "failed to search raydium launchpad pools (any quote mint) for mint {}",
                    launchpad_mint
                )
            })?
            .with_context(|| {
                format!(
                    "no raydium launchpad pool found for mint {}",
                    launchpad_mint
                )
            })?
    };

    let state = raydium_launchpad
        .fetch_state(&pool)
        .await
        .with_context(|| format!("failed to fetch raydium launchpad state for {}", pool))?;
    let global_config = raydium_launchpad
        .fetch_global_config(&state.global_config)
        .await
        .with_context(|| {
            format!(
                "failed to fetch raydium launchpad global config {}",
                state.global_config
            )
        })?;
    let platform_config = raydium_launchpad
        .fetch_platform_config(&state.platform_config)
        .await
        .with_context(|| {
            format!(
                "failed to fetch raydium launchpad platform config {}",
                state.platform_config
            )
        })?;
    let (_, price) = raydium_launchpad
        .fetch_price(&pool)
        .await
        .with_context(|| format!("failed to fetch raydium launchpad price for {}", pool))?;
    let wsol_liquidity_raw = raydium_launchpad
        .fetch_wsol_liquidity_raw(&state)
        .await
        .with_context(|| format!("failed to fetch raydium launchpad liquidity for {}", pool))?;

    println!("lookup mint (raydium_launchpad): {launchpad_mint}");
    println!("raydium_launchpad pool: {pool}");
    println!("raydium_launchpad epoch: {}", state.epoch);
    println!("raydium_launchpad auth_bump: {}", state.auth_bump);
    println!("raydium_launchpad status: {}", state.status);
    println!("raydium_launchpad migrate_type: {}", state.migrate_type);
    println!("raydium_launchpad base_mint: {}", state.base_mint);
    println!("raydium_launchpad quote_mint: {}", state.quote_mint);
    println!("raydium_launchpad base_vault: {}", state.base_vault);
    println!("raydium_launchpad quote_vault: {}", state.quote_vault);
    println!("raydium_launchpad creator: {}", state.creator);
    println!("raydium_launchpad global_config: {}", state.global_config);
    println!(
        "raydium_launchpad platform_config: {}",
        state.platform_config
    );
    println!("raydium_launchpad supply: {}", state.supply);
    println!(
        "raydium_launchpad total_base_sell: {}",
        state.total_base_sell
    );
    println!(
        "raydium_launchpad total_quote_fund_raising: {}",
        state.total_quote_fund_raising
    );
    println!("raydium_launchpad virtual_base: {}", state.virtual_base);
    println!("raydium_launchpad virtual_quote: {}", state.virtual_quote);
    println!("raydium_launchpad real_base: {}", state.real_base);
    println!("raydium_launchpad real_quote: {}", state.real_quote);
    println!(
        "raydium_launchpad quote_protocol_fee: {}",
        state.quote_protocol_fee
    );
    println!("raydium_launchpad platform_fee: {}", state.platform_fee);
    println!("raydium_launchpad migrate_fee: {}", state.migrate_fee);
    println!(
        "raydium_launchpad vesting_total_locked_amount: {}",
        state.vesting_schedule.total_locked_amount
    );
    println!(
        "raydium_launchpad vesting_cliff_period: {}",
        state.vesting_schedule.cliff_period
    );
    println!(
        "raydium_launchpad vesting_unlock_period: {}",
        state.vesting_schedule.unlock_period
    );
    println!(
        "raydium_launchpad vesting_start_time: {}",
        state.vesting_schedule.start_time
    );
    println!(
        "raydium_launchpad vesting_allocated_share_amount: {}",
        state.vesting_schedule.allocated_share_amount
    );
    println!(
        "raydium_launchpad token_program_flag: {}",
        state.token_program_flag
    );
    println!("raydium_launchpad amm_fee_on: {}", state.amm_fee_on);
    println!(
        "raydium_launchpad global_curve_type: {}",
        global_config.curve_type
    );
    println!(
        "raydium_launchpad global_trade_fee_rate: {}",
        global_config.trade_fee_rate
    );
    println!(
        "raydium_launchpad global_max_share_fee_rate: {}",
        global_config.max_share_fee_rate
    );
    println!(
        "raydium_launchpad platform_fee_wallet: {}",
        platform_config.platform_fee_wallet
    );
    println!(
        "raydium_launchpad platform_fee_rate: {}",
        platform_config.fee_rate
    );
    println!(
        "raydium_launchpad platform_creator_fee_rate: {}",
        platform_config.creator_fee_rate
    );
    println!("raydium_launchpad wsol_liquidity_raw: {wsol_liquidity_raw}");
    println!("raydium_launchpad implied_price_sol: {price}");

    anyhow::ensure!(
        state.base_mint == launchpad_mint || state.quote_mint == launchpad_mint,
        "raydium launchpad pool {} does not contain lookup mint {}",
        pool,
        launchpad_mint
    );
    anyhow::ensure!(
        state.base_mint == WSOL_MINT || state.quote_mint == WSOL_MINT,
        "raydium launchpad pool {} is not WSOL-quoted",
        pool
    );

    Ok(())
}
