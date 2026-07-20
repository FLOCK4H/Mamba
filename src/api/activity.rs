use {
    super::{ApiError, ApiState},
    crate::dex::{
        pump_fun::{PumpFun, PumpFunEvent},
        pump_swap::{PumpSwap, PumpSwapEvent},
    },
    axum::{
        Json,
        extract::{Path, Query, State},
    },
    moka::sync::Cache,
    serde::{Deserialize, Serialize},
    serde_json::{Value, json},
    solana_program::pubkey::Pubkey,
    std::{
        cmp::Ordering,
        collections::{HashMap, HashSet},
        str::FromStr,
        sync::{Arc, OnceLock},
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
};

const DEFAULT_TRADE_LIMIT: usize = 100;
const MAX_TRADE_LIMIT: usize = 250;
const DEFAULT_HOLDER_LIMIT: usize = 1_000;
const MAX_HOLDER_LIMIT: usize = 10_000;
const DAS_PAGE_SIZE: usize = 1_000;
const MAX_TOKEN_ACCOUNTS: usize = 50_000;

#[derive(Debug, Deserialize)]
pub(super) struct MintActivityQuery {
    market: Option<String>,
    pool: Option<String>,
    trade_limit: Option<usize>,
    holder_limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct MintTrade {
    signature: String,
    side: &'static str,
    signer: String,
    token_amount: f64,
    sol_amount: Option<f64>,
    price_sol: Option<f64>,
    market_cap_sol: Option<f64>,
    timestamp: i64,
    holding_pct: Option<f64>,
    source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct MintHolder {
    rank: usize,
    owner: String,
    token_amount: f64,
    holding_pct: f64,
    token_accounts: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct MintActivityResponse {
    mint: String,
    market: Option<String>,
    fetched_at: i64,
    trades: Vec<MintTrade>,
    holders: Vec<MintHolder>,
    trade_count: usize,
    buy_count: usize,
    sell_count: usize,
    volume_sol: f64,
    holder_count: usize,
    supply: Option<f64>,
    holders_complete: bool,
    holders_source: &'static str,
    trades_complete: bool,
    trades_source: &'static str,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct HolderIndex {
    rows: Vec<MintHolder>,
    percentages: HashMap<String, f64>,
    holder_count: usize,
    supply: Option<f64>,
    complete: bool,
    source: &'static str,
    warning: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DasTokenAccountPage {
    #[serde(default)]
    total: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    token_accounts: Vec<DasTokenAccount>,
}

#[derive(Debug, Deserialize)]
struct DasTokenAccount {
    owner: String,
    amount: Value,
}

fn activity_cache() -> &'static Cache<String, MintActivityResponse> {
    static CACHE: OnceLock<Cache<String, MintActivityResponse>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Cache::builder()
            .max_capacity(2_000)
            .time_to_live(Duration::from_secs(8))
            .build()
    })
}

fn helius_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
            .expect("mint activity HTTP client configuration must be valid")
    })
}

pub(super) async fn get_mint_activity(
    State(state): State<Arc<ApiState>>,
    Path(mint): Path<String>,
    Query(query): Query<MintActivityQuery>,
) -> Result<Json<MintActivityResponse>, ApiError> {
    Pubkey::from_str(mint.trim())
        .map_err(|_| ApiError::bad_request("mint must be a valid Solana public key"))?;

    let trade_limit = query
        .trade_limit
        .unwrap_or(DEFAULT_TRADE_LIMIT)
        .clamp(1, MAX_TRADE_LIMIT);
    let holder_limit = query
        .holder_limit
        .unwrap_or(DEFAULT_HOLDER_LIMIT)
        .clamp(1, MAX_HOLDER_LIMIT);
    let market = query
        .market
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let pool = query
        .pool
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(pool) = pool {
        Pubkey::from_str(pool)
            .map_err(|_| ApiError::bad_request("pool must be a valid Solana public key"))?;
    }
    let cache_key = format!(
        "{}:{}:{}:{}:{}",
        mint,
        market.unwrap_or_default(),
        pool.unwrap_or_default(),
        trade_limit,
        holder_limit
    );
    if let Some(cached) = activity_cache().get(&cache_key) {
        return Ok(Json(cached));
    }

    let (holders, trades_result) = tokio::join!(
        fetch_holder_index(&state, &mint, pool, holder_limit),
        fetch_recent_trades(&state, &mint, trade_limit)
    );

    let mut warnings = Vec::new();
    if let Some(warning) = holders.warning.clone() {
        warnings.push(warning);
    }

    let (mut trades, trades_complete, trades_source) = match trades_result {
        Ok(value) => value,
        Err(error) => {
            warnings.push(format!("Recent on-chain trades are unavailable: {error}"));
            (Vec::new(), false, "unavailable")
        }
    };
    for trade in &mut trades {
        trade.holding_pct = holders
            .percentages
            .get(&trade.signer)
            .copied()
            .or_else(|| holders.complete.then_some(0.0));
        trade.market_cap_sol = trade_market_cap_sol(trade.price_sol, holders.supply);
    }
    trades.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    trades.truncate(trade_limit);

    let buy_count = trades.iter().filter(|trade| trade.side == "buy").count();
    let sell_count = trades.iter().filter(|trade| trade.side == "sell").count();
    let volume_sol = trades
        .iter()
        .filter_map(|trade| trade.sol_amount)
        .sum::<f64>()
        .abs();
    let response = MintActivityResponse {
        mint: mint.clone(),
        market: market.map(ToOwned::to_owned),
        fetched_at: unix_timestamp(),
        trade_count: trades.len(),
        trades,
        holders: holders.rows,
        buy_count,
        sell_count,
        volume_sol,
        holder_count: holders.holder_count,
        supply: holders.supply,
        holders_complete: holders.complete,
        holders_source: holders.source,
        trades_complete,
        trades_source,
        warnings,
    };
    activity_cache().insert(cache_key, response.clone());
    Ok(Json(response))
}

async fn fetch_holder_index(
    state: &ApiState,
    mint: &str,
    excluded_owner: Option<&str>,
    holder_limit: usize,
) -> HolderIndex {
    match fetch_das_holder_index(state, mint, excluded_owner, holder_limit).await {
        Ok(index) => index,
        Err(error) => HolderIndex {
            rows: Vec::new(),
            percentages: HashMap::new(),
            holder_count: 0,
            supply: None,
            complete: false,
            source: "unavailable",
            warning: Some(format!("Full holder index is unavailable: {error}")),
        },
    }
}

async fn fetch_das_holder_index(
    state: &ApiState,
    mint: &str,
    excluded_owner: Option<&str>,
    holder_limit: usize,
) -> anyhow::Result<HolderIndex> {
    let urls = helius_rpc_urls(state);
    anyhow::ensure!(!urls.is_empty(), "no Helius RPC endpoint is configured");

    let mut cursor: Option<String> = None;
    let mut accounts = Vec::new();
    let mut reported_total = 0usize;
    let mut reached_end = false;

    while accounts.len() < MAX_TOKEN_ACCOUNTS {
        let params = json!({
            "mint": mint,
            "limit": DAS_PAGE_SIZE,
            "cursor": cursor,
            "options": { "showZeroBalance": false }
        });
        let page: DasTokenAccountPage =
            helius_rpc_request(&urls, "getTokenAccounts", params).await?;
        reported_total = reported_total.max(page.total);
        let received = page.token_accounts.len();
        accounts.extend(page.token_accounts);
        cursor = page.cursor.filter(|value| !value.is_empty());
        if received == 0 || cursor.is_none() || accounts.len() >= reported_total {
            reached_end = true;
            break;
        }
    }

    let supply_result: anyhow::Result<Value> =
        helius_rpc_request(&urls, "getTokenSupply", json!([mint])).await;
    let decimals = supply_result
        .as_ref()
        .ok()
        .and_then(|value| value.get("value"))
        .and_then(|value| value.get("decimals"))
        .and_then(Value::as_u64)
        .unwrap_or(6) as u32;
    let rpc_supply_raw = supply_result
        .as_ref()
        .ok()
        .and_then(|value| value.get("value"))
        .and_then(|value| value.get("amount"))
        .and_then(value_as_u128);

    let mut by_owner: HashMap<String, (u128, usize)> = HashMap::new();
    for account in accounts.iter() {
        let amount = value_as_u128(&account.amount).unwrap_or_default();
        if amount == 0
            || account.owner.trim().is_empty()
            || excluded_owner == Some(account.owner.as_str())
        {
            continue;
        }
        let entry = by_owner.entry(account.owner.clone()).or_default();
        entry.0 = entry.0.saturating_add(amount);
        entry.1 += 1;
    }

    let summed_supply_raw: u128 = by_owner.values().map(|(amount, _)| *amount).sum();
    let supply_raw = rpc_supply_raw
        .filter(|value| *value > 0)
        .unwrap_or(summed_supply_raw);
    let divisor = 10_f64.powi(decimals as i32);
    let mut ranked = by_owner.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.0.cmp(&left.1.0));
    let holder_count = ranked.len();
    let percentages = ranked
        .iter()
        .map(|(owner, (amount, _))| {
            let pct = if supply_raw > 0 {
                (*amount as f64 / supply_raw as f64) * 100.0
            } else {
                0.0
            };
            (owner.clone(), pct)
        })
        .collect::<HashMap<_, _>>();
    let rows = ranked
        .into_iter()
        .take(holder_limit)
        .enumerate()
        .map(|(index, (owner, (amount, token_accounts)))| MintHolder {
            rank: index + 1,
            owner,
            token_amount: amount as f64 / divisor,
            holding_pct: if supply_raw > 0 {
                (amount as f64 / supply_raw as f64) * 100.0
            } else {
                0.0
            },
            token_accounts,
        })
        .collect::<Vec<_>>();

    let fetched_all_accounts = reached_end && accounts.len() >= reported_total;
    let returned_all_holders = holder_count <= holder_limit;
    let complete = fetched_all_accounts && returned_all_holders;
    let warning = if accounts.len() >= MAX_TOKEN_ACCOUNTS && !reached_end {
        Some(format!(
            "Holder indexing stopped at the safety limit of {MAX_TOKEN_ACCOUNTS} token accounts"
        ))
    } else if !returned_all_holders {
        Some(format!(
            "Showing the top {holder_limit} of {holder_count} holders"
        ))
    } else if !fetched_all_accounts {
        Some("The holder provider returned a partial token-account index".to_string())
    } else {
        None
    };

    Ok(HolderIndex {
        rows,
        percentages,
        holder_count,
        supply: (supply_raw > 0).then_some(supply_raw as f64 / divisor),
        complete,
        source: "helius_das",
        warning,
    })
}

async fn fetch_recent_trades(
    state: &ApiState,
    mint: &str,
    limit: usize,
) -> anyhow::Result<(Vec<MintTrade>, bool, &'static str)> {
    let urls = helius_rpc_urls(state);
    anyhow::ensure!(!urls.is_empty(), "no Helius RPC endpoint is configured");
    let result: Value = helius_rpc_request(
        &urls,
        "getTransactionsForAddress",
        json!([
            mint,
            {
                "transactionDetails": "full",
                "encoding": "jsonParsed",
                "sortOrder": "desc",
                "limit": limit,
                "filters": { "status": "succeeded" }
            }
        ]),
    )
    .await?;

    let data = result
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut trades = Vec::new();
    let mut seen = HashSet::new();

    for entry in data {
        let Some(signature) = entry
            .pointer("/transaction/signatures/0")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        let fallback_timestamp = entry
            .get("blockTime")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let logs = entry
            .pointer("/meta/logMessages")
            .and_then(Value::as_array)
            .map(|logs| {
                logs.iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut decoded = false;
        for event in PumpFun::parse_logs(logs.iter(), Some(&signature)) {
            let PumpFunEvent::Trade(Some(event)) = event else {
                continue;
            };
            if event.mint.to_string() != mint {
                continue;
            }
            let key = format!("{}:{}:{}", signature, event.user, event.is_buy);
            if !seen.insert(key) {
                continue;
            }
            let token_amount = event.token_amount as f64 / 1_000_000.0;
            let sol_amount = event.sol_amount as f64 / 1_000_000_000.0;
            trades.push(MintTrade {
                signature: signature.clone(),
                side: if event.is_buy { "buy" } else { "sell" },
                signer: event.user.to_string(),
                token_amount,
                sol_amount: Some(sol_amount),
                price_sol: trade_price_sol(token_amount, Some(sol_amount)),
                market_cap_sol: None,
                timestamp: positive_timestamp(event.timestamp, fallback_timestamp),
                holding_pct: None,
                source: "pump_fun_event",
            });
            decoded = true;
        }

        if !decoded {
            for event in PumpSwap::parse_logs(logs.iter(), Some(&signature)) {
                match event {
                    PumpSwapEvent::Buy(Some(event)) => {
                        let key = format!("{}:{}:buy", signature, event.user);
                        if seen.insert(key) {
                            let token_amount = event.base_amount_out as f64 / 1_000_000.0;
                            let sol_amount = event.quote_amount_in as f64 / 1_000_000_000.0;
                            trades.push(MintTrade {
                                signature: signature.clone(),
                                side: "buy",
                                signer: event.user.to_string(),
                                token_amount,
                                sol_amount: Some(sol_amount),
                                price_sol: trade_price_sol(token_amount, Some(sol_amount)),
                                market_cap_sol: None,
                                timestamp: positive_timestamp(event.timestamp, fallback_timestamp),
                                holding_pct: None,
                                source: "pump_swap_event",
                            });
                            decoded = true;
                        }
                    }
                    PumpSwapEvent::Sell(Some(event)) => {
                        let key = format!("{}:{}:sell", signature, event.user);
                        if seen.insert(key) {
                            let token_amount = event.base_amount_in as f64 / 1_000_000.0;
                            let sol_amount = event.quote_amount_out as f64 / 1_000_000_000.0;
                            trades.push(MintTrade {
                                signature: signature.clone(),
                                side: "sell",
                                signer: event.user.to_string(),
                                token_amount,
                                sol_amount: Some(sol_amount),
                                price_sol: trade_price_sol(token_amount, Some(sol_amount)),
                                market_cap_sol: None,
                                timestamp: positive_timestamp(event.timestamp, fallback_timestamp),
                                holding_pct: None,
                                source: "pump_swap_event",
                            });
                            decoded = true;
                        }
                    }
                    _ => {}
                }
            }
        }

        if !decoded {
            if let Some(trade) =
                infer_trade_from_balances(&entry, mint, &signature, fallback_timestamp)
            {
                let key = format!("{}:{}:{}", signature, trade.signer, trade.side);
                if seen.insert(key) {
                    trades.push(trade);
                }
            }
        }
    }

    trades.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    Ok((trades, true, "helius_rpc"))
}

fn infer_trade_from_balances(
    entry: &Value,
    mint: &str,
    signature: &str,
    timestamp: i64,
) -> Option<MintTrade> {
    let account_keys = entry
        .pointer("/transaction/message/accountKeys")?
        .as_array()?;
    let signer = account_keys.iter().find_map(|key| match key {
        Value::String(value) => Some(value.clone()),
        Value::Object(value) if value.get("signer").and_then(Value::as_bool) == Some(true) => value
            .get("pubkey")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    })?;

    let mut before = HashMap::<String, f64>::new();
    let mut after = HashMap::<String, f64>::new();
    collect_token_balances(entry.pointer("/meta/preTokenBalances"), mint, &mut before);
    collect_token_balances(entry.pointer("/meta/postTokenBalances"), mint, &mut after);
    let owners = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<HashSet<_>>();
    let (owner, delta) = owners
        .into_iter()
        .map(|owner| {
            let delta = after.get(&owner).copied().unwrap_or_default()
                - before.get(&owner).copied().unwrap_or_default();
            (owner, delta)
        })
        .max_by(|left, right| {
            left.1
                .abs()
                .partial_cmp(&right.1.abs())
                .unwrap_or(Ordering::Equal)
        })?;
    if delta.abs() <= f64::EPSILON {
        return None;
    }
    let signer = if owner.is_empty() { signer } else { owner };
    let token_amount = delta.abs();
    let sol_amount = signer_sol_delta(entry, account_keys, &signer)
        .map(f64::abs)
        .filter(|value| *value > 0.000_001);
    Some(MintTrade {
        signature: signature.to_string(),
        side: if delta > 0.0 { "buy" } else { "sell" },
        signer,
        token_amount,
        sol_amount,
        price_sol: trade_price_sol(token_amount, sol_amount),
        market_cap_sol: None,
        timestamp,
        holding_pct: None,
        source: "token_balance_delta",
    })
}

fn collect_token_balances(value: Option<&Value>, mint: &str, target: &mut HashMap<String, f64>) {
    for balance in value.and_then(Value::as_array).into_iter().flatten() {
        if balance.get("mint").and_then(Value::as_str) != Some(mint) {
            continue;
        }
        let owner = balance
            .get("owner")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let amount = balance
            .pointer("/uiTokenAmount/uiAmountString")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<f64>().ok())
            .or_else(|| {
                balance
                    .pointer("/uiTokenAmount/uiAmount")
                    .and_then(Value::as_f64)
            })
            .unwrap_or_default();
        target
            .entry(owner)
            .and_modify(|value| *value += amount)
            .or_insert(amount);
    }
}

fn signer_sol_delta(entry: &Value, account_keys: &[Value], signer: &str) -> Option<f64> {
    let index = account_keys.iter().position(|key| match key {
        Value::String(value) => value == signer,
        Value::Object(value) => value.get("pubkey").and_then(Value::as_str) == Some(signer),
        _ => false,
    })?;
    let before = entry.pointer("/meta/preBalances")?.get(index)?.as_u64()?;
    let after = entry.pointer("/meta/postBalances")?.get(index)?.as_u64()?;
    Some((after as f64 - before as f64) / 1_000_000_000.0)
}

fn helius_rpc_urls(state: &ApiState) -> Vec<String> {
    state
        .rpc_urls
        .iter()
        .filter(|url| url.contains("helius"))
        .cloned()
        .collect()
}

async fn helius_rpc_request<T: for<'de> Deserialize<'de>>(
    urls: &[String],
    method: &str,
    params: Value,
) -> anyhow::Result<T> {
    let client = helius_client();
    let mut last_error = None;
    for url in urls {
        for attempt in 0..4 {
            let response = client
                .post(url)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": "mamba-mint-activity",
                    "method": method,
                    "params": params
                }))
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    last_error = Some(anyhow::anyhow!("provider request failed: {error}"));
                    continue;
                }
            };
            let status = response.status();
            let payload: Value = match response.json().await {
                Ok(payload) => payload,
                Err(error) => {
                    last_error = Some(anyhow::anyhow!("provider returned invalid JSON: {error}"));
                    continue;
                }
            };
            let rpc_message = payload
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str);
            let rate_limited = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || rpc_message
                    .is_some_and(|message| message.to_ascii_lowercase().contains("rate limit"));
            if rate_limited && attempt < 3 {
                tokio::time::sleep(Duration::from_millis(400_u64 << attempt)).await;
                continue;
            }
            if !status.is_success() {
                last_error = Some(anyhow::anyhow!("provider returned HTTP {status}"));
                break;
            }
            if let Some(message) = rpc_message {
                last_error = Some(anyhow::anyhow!("provider RPC error: {message}"));
                break;
            }
            return serde_json::from_value(payload.get("result").cloned().unwrap_or(Value::Null))
                .map_err(Into::into);
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no Helius RPC endpoint succeeded")))
}

fn value_as_u128(value: &Value) -> Option<u128> {
    value
        .as_u64()
        .map(u128::from)
        .or_else(|| value.as_str().and_then(|value| value.parse::<u128>().ok()))
}

fn positive_timestamp(value: i64, fallback: i64) -> i64 {
    if value > 0 { value } else { fallback }
}

fn trade_price_sol(token_amount: f64, sol_amount: Option<f64>) -> Option<f64> {
    let sol_amount = sol_amount?;
    if !token_amount.is_finite()
        || token_amount <= 0.0
        || !sol_amount.is_finite()
        || sol_amount <= 0.0
    {
        return None;
    }

    let price = sol_amount / token_amount;
    (price.is_finite() && price > 0.0).then_some(price)
}

fn trade_market_cap_sol(price_sol: Option<f64>, supply: Option<f64>) -> Option<f64> {
    let price_sol = price_sol?;
    let supply = supply?;
    if !price_sol.is_finite() || price_sol <= 0.0 || !supply.is_finite() || supply <= 0.0 {
        return None;
    }

    let market_cap = price_sol * supply;
    (market_cap.is_finite() && market_cap > 0.0).then_some(market_cap)
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::{trade_market_cap_sol, trade_price_sol};

    #[test]
    fn derives_finite_trade_price_and_market_cap() {
        let price = trade_price_sol(500_000.0, Some(0.1)).expect("price");
        assert!((price - 0.000_000_2).abs() < f64::EPSILON);

        let market_cap =
            trade_market_cap_sol(Some(price), Some(1_000_000_000.0)).expect("market cap");
        assert!((market_cap - 200.0).abs() < 0.000_001);
    }

    #[test]
    fn rejects_zero_or_non_finite_trade_values() {
        assert_eq!(trade_price_sol(0.0, Some(0.1)), None);
        assert_eq!(trade_price_sol(100.0, Some(f64::NAN)), None);
        assert_eq!(trade_market_cap_sol(Some(0.1), Some(0.0)), None);
    }
}
